#![cfg_attr(
    test,
    allow(
        clippy::expect_used,
        clippy::unwrap_used,
        clippy::panic,
        clippy::assert_is_empty,
        clippy::bool_assert_comparison,
        clippy::float_cmp
    )
)]
//! Append-only session logging.
//!
//! Every user message, assistant reply, tool call, and tool result is one JSON
//! line under the Harness home, so a session can be inspected after the
//! terminal is gone.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use dsh_agent::ToolCall;
use serde_json::{Value, json};

/// Directory under the Harness home holding session logs.
pub const SESSIONS_DIR: &str = "sessions";

/// One recorded event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionEvent {
    /// The session's fixed facts.
    Meta {
        /// Model name.
        model: String,
        /// Workspace root.
        workspace: String,
    },
    /// A person's request.
    User {
        /// Request text.
        text: String,
    },
    /// The model's reply.
    Assistant {
        /// Assistant text.
        content: String,
        /// Requested tool calls.
        tool_calls: Vec<ToolCall>,
    },
    /// One tool outcome.
    Tool {
        /// Correlation id.
        call_id: String,
        /// Tool name.
        name: String,
        /// Result text.
        content: String,
        /// Whether the tool failed.
        is_error: bool,
    },
    /// A failure that stopped the turn.
    Error {
        /// Diagnostic text.
        message: String,
    },
}

impl SessionEvent {
    /// The JSON line body.
    #[must_use]
    pub fn to_json(&self) -> Value {
        match self {
            Self::Meta { model, workspace } => json!({
                "type": "meta",
                "model": model,
                "workspace": workspace,
            }),
            Self::User { text } => json!({"type": "user", "text": text}),
            Self::Assistant {
                content,
                tool_calls,
            } => json!({
                "type": "assistant",
                "content": content,
                "tool_calls": tool_calls.iter().map(ToolCall::to_json).collect::<Vec<_>>(),
            }),
            Self::Tool {
                call_id,
                name,
                content,
                is_error,
            } => json!({
                "type": "tool",
                "call_id": call_id,
                "name": name,
                "content": content,
                "is_error": is_error,
            }),
            Self::Error { message } => json!({"type": "error", "message": message}),
        }
    }
}

/// One open session file.
#[derive(Debug)]
pub struct SessionLog {
    id: String,
    path: PathBuf,
    file: File,
}

impl SessionLog {
    /// Create a new session file under `home/sessions`.
    ///
    /// # Errors
    /// Returns when the directory or file cannot be created.
    pub fn create(home: &Path) -> std::io::Result<Self> {
        let directory = home.join(SESSIONS_DIR);
        std::fs::create_dir_all(&directory)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700));
        }
        let millis = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |duration| duration.as_millis());
        let id = format!("{millis}-{}", uuid::Uuid::new_v4());
        let path = directory.join(format!("{id}.jsonl"));
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let file = options.open(&path)?;
        Ok(Self { id, path, file })
    }

    /// The session id.
    #[must_use]
    pub fn id(&self) -> &str {
        &self.id
    }

    /// The session file path.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Append one event and flush it to disk.
    ///
    /// # Errors
    /// Returns when the event cannot be serialized or written.
    pub fn append(&mut self, event: &SessionEvent) -> std::io::Result<()> {
        let line = serde_json::to_string(&event.to_json())
            .map_err(|error| std::io::Error::other(error.to_string()))?;
        self.file.write_all(line.as_bytes())?;
        self.file.write_all(b"\n")?;
        self.file.flush()
    }
}

#[cfg(test)]
mod tests {
    use super::{SESSIONS_DIR, SessionEvent, SessionLog};
    use dsh_agent::ToolCall;
    use std::fs;

    #[test]
    fn every_event_becomes_one_json_line() {
        let home = tempfile::tempdir().expect("home");
        let mut log = SessionLog::create(home.path()).expect("log");
        let call = ToolCall {
            id: "c".to_string(),
            name: "bash".to_string(),
            arguments: "{}".to_string(),
        };
        for event in [
            SessionEvent::Meta {
                model: "m".to_string(),
                workspace: "/w".to_string(),
            },
            SessionEvent::User {
                text: "hi".to_string(),
            },
            SessionEvent::Assistant {
                content: "hello".to_string(),
                tool_calls: vec![call],
            },
            SessionEvent::Tool {
                call_id: "c".to_string(),
                name: "bash".to_string(),
                content: "ok".to_string(),
                is_error: false,
            },
            SessionEvent::Error {
                message: "bad".to_string(),
            },
        ] {
            log.append(&event).expect("append");
        }
        let text = fs::read_to_string(log.path()).expect("read");
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 5);
        let first: serde_json::Value = serde_json::from_str(lines[0]).expect("json");
        assert_eq!(first["type"], "meta");
        assert_eq!(first["model"], "m");
        let assistant: serde_json::Value = serde_json::from_str(lines[2]).expect("json");
        assert_eq!(assistant["type"], "assistant");
        assert_eq!(assistant["tool_calls"][0]["function"]["name"], "bash");
        assert_eq!(
            log.path().parent().expect("parent"),
            home.path().join(SESSIONS_DIR)
        );
        assert!(
            log.id()
                .rsplit_once('.')
                .is_none_or(|(_, extension)| extension != "jsonl")
        );
    }

    #[cfg(unix)]
    #[test]
    fn session_files_are_private() {
        use std::os::unix::fs::PermissionsExt;
        let home = tempfile::tempdir().expect("home");
        let log = SessionLog::create(home.path()).expect("log");
        assert_eq!(
            fs::metadata(log.path()).expect("meta").permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(
            fs::metadata(home.path().join(SESSIONS_DIR))
                .expect("meta")
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
    }

    #[test]
    fn event_json_covers_every_variant() {
        assert_eq!(
            SessionEvent::User {
                text: "x".to_string()
            }
            .to_json(),
            serde_json::json!({"type": "user", "text": "x"})
        );
        assert_eq!(
            SessionEvent::Error {
                message: "e".to_string()
            }
            .to_json(),
            serde_json::json!({"type": "error", "message": "e"})
        );
        assert_eq!(
            SessionEvent::Tool {
                call_id: "c".to_string(),
                name: "n".to_string(),
                content: "o".to_string(),
                is_error: true,
            }
            .to_json(),
            serde_json::json!({"type": "tool", "call_id": "c", "name": "n", "content": "o", "is_error": true})
        );
    }
}

#[cfg(test)]
mod id_tests {
    use super::SessionLog;

    #[test]
    fn session_ids_are_non_empty_and_distinct() {
        let home = tempfile::tempdir().expect("home");
        let first = SessionLog::create(home.path()).expect("first");
        let second = SessionLog::create(home.path()).expect("second");
        assert!(!first.id().is_empty());
        assert_ne!(first.id(), second.id());
        assert!(first.path().is_file());
        assert!(second.path().is_file());
    }
}
