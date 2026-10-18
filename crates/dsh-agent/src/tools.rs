//! The tool contract the model sees, and validation of what it calls.
//!
//! Argument parsing happens before any tool touches the workspace, so a
//! malformed call becomes a tool error the model can read instead of a crash
//! in the executor.

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use serde_json::{Value, json};

/// Default shell-command timeout.
pub const DEFAULT_BASH_TIMEOUT_MS: u64 = 120_000;

/// Maximum shell-command timeout.
pub const MAX_BASH_TIMEOUT_MS: u64 = 600_000;

/// Default lines returned by `read_file`.
pub const DEFAULT_READ_LIMIT: u64 = 2_000;

/// Maximum lines returned by `read_file`.
pub const MAX_READ_LIMIT: u64 = 20_000;

/// Default matches returned by `search`.
pub const DEFAULT_SEARCH_RESULTS: u64 = 100;

/// Maximum matches returned by `search`.
pub const MAX_SEARCH_RESULTS: u64 = 1_000;

/// One tool the model may call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ToolSpec {
    /// Wire name.
    pub name: &'static str,
    /// One-line description shown to the model.
    pub description: &'static str,
}

/// Every tool this agent exposes.
pub const TOOL_SPECS: &[ToolSpec] = &[
    ToolSpec {
        name: "bash",
        description: "Run a shell command in the workspace and return its combined output.",
    },
    ToolSpec {
        name: "read_file",
        description: "Read a UTF-8 file, optionally a line range.",
    },
    ToolSpec {
        name: "write_file",
        description: "Create or replace a file with the given content.",
    },
    ToolSpec {
        name: "edit_file",
        description: "Replace one exact occurrence of old text with new text in a file.",
    },
    ToolSpec {
        name: "list_dir",
        description: "List directory entries, with a slash after directories.",
    },
    ToolSpec {
        name: "search",
        description: "Search files with a regular expression and return path:line matches.",
    },
];

/// A validated tool call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolInvocation {
    /// Run a shell command.
    Bash {
        /// The command text.
        command: String,
        /// Milliseconds before the process is killed.
        timeout_ms: u64,
    },
    /// Read a file.
    ReadFile {
        /// Path relative to the workspace.
        path: String,
        /// First line, one-based.
        offset: u64,
        /// Maximum lines.
        limit: u64,
    },
    /// Write a file.
    WriteFile {
        /// Path relative to the workspace.
        path: String,
        /// Complete new content.
        content: String,
    },
    /// Replace one exact occurrence.
    EditFile {
        /// Path relative to the workspace.
        path: String,
        /// Text that must occur exactly once.
        old: String,
        /// Replacement text.
        new: String,
    },
    /// List one directory.
    ListDir {
        /// Path relative to the workspace.
        path: String,
    },
    /// Search file contents.
    Search {
        /// Regular expression.
        pattern: String,
        /// Root path relative to the workspace.
        path: String,
        /// Maximum matches.
        max_results: u64,
    },
}

impl ToolInvocation {
    /// The tool name this invocation came from.
    #[must_use]
    pub const fn tool_name(&self) -> &'static str {
        match self {
            Self::Bash { .. } => "bash",
            Self::ReadFile { .. } => "read_file",
            Self::WriteFile { .. } => "write_file",
            Self::EditFile { .. } => "edit_file",
            Self::ListDir { .. } => "list_dir",
            Self::Search { .. } => "search",
        }
    }
}

/// A rejected tool call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolError {
    /// What the model must fix.
    pub message: String,
}

impl ToolError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl core::fmt::Display for ToolError {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter.write_str(&self.message)
    }
}

/// The JSON Schema for one tool's arguments.
fn parameters_for(name: &str) -> Value {
    match name {
        "bash" => json!({
            "type": "object",
            "properties": {
                "command": {"type": "string", "description": "Shell command to run."},
                "timeout_ms": {"type": "integer", "minimum": 1000, "maximum": MAX_BASH_TIMEOUT_MS}
            },
            "required": ["command"]
        }),
        "read_file" => json!({
            "type": "object",
            "properties": {
                "path": {"type": "string"},
                "offset": {"type": "integer", "minimum": 1},
                "limit": {"type": "integer", "minimum": 1, "maximum": MAX_READ_LIMIT}
            },
            "required": ["path"]
        }),
        "write_file" => json!({
            "type": "object",
            "properties": {
                "path": {"type": "string"},
                "content": {"type": "string"}
            },
            "required": ["path", "content"]
        }),
        "edit_file" => json!({
            "type": "object",
            "properties": {
                "path": {"type": "string"},
                "old": {"type": "string"},
                "new": {"type": "string"}
            },
            "required": ["path", "old", "new"]
        }),
        "list_dir" => json!({
            "type": "object",
            "properties": {
                "path": {"type": "string", "default": "."}
            }
        }),
        "search" => json!({
            "type": "object",
            "properties": {
                "pattern": {"type": "string"},
                "path": {"type": "string", "default": "."},
                "max_results": {"type": "integer", "minimum": 1, "maximum": MAX_SEARCH_RESULTS}
            },
            "required": ["pattern"]
        }),
        _ => json!({"type": "object"}),
    }
}

/// The `tools` array of a chat-completions request.
#[must_use]
pub fn tool_definitions() -> Value {
    Value::Array(
        TOOL_SPECS
            .iter()
            .map(|spec| {
                json!({
                    "type": "function",
                    "function": {
                        "name": spec.name,
                        "description": spec.description,
                        "parameters": parameters_for(spec.name),
                    }
                })
            })
            .collect(),
    )
}

fn object(arguments: &str) -> Result<serde_json::Map<String, Value>, ToolError> {
    let value: Value = serde_json::from_str(arguments)
        .map_err(|error| ToolError::new(format!("arguments are not valid JSON: {error}")))?;
    match value {
        Value::Object(map) => Ok(map),
        _ => Err(ToolError::new("arguments must be a JSON object")),
    }
}

fn required_string(
    object: &serde_json::Map<String, Value>,
    key: &str,
) -> Result<String, ToolError> {
    match object.get(key) {
        Some(Value::String(value)) if !value.trim().is_empty() => Ok(value.clone()),
        Some(Value::String(_)) => Err(ToolError::new(format!("{key} must not be empty"))),
        Some(_) => Err(ToolError::new(format!("{key} must be a string"))),
        None => Err(ToolError::new(format!("{key} is required"))),
    }
}

fn optional_string(
    object: &serde_json::Map<String, Value>,
    key: &str,
    default: &str,
) -> Result<String, ToolError> {
    match object.get(key) {
        None | Some(Value::Null) => Ok(default.to_string()),
        Some(Value::String(value)) if !value.trim().is_empty() => Ok(value.clone()),
        Some(Value::String(_)) => Err(ToolError::new(format!("{key} must not be empty"))),
        Some(_) => Err(ToolError::new(format!("{key} must be a string"))),
    }
}

fn optional_u64(
    object: &serde_json::Map<String, Value>,
    key: &str,
    default: u64,
    minimum: u64,
    maximum: u64,
) -> Result<u64, ToolError> {
    match object.get(key) {
        None | Some(Value::Null) => Ok(default),
        Some(Value::Number(number)) => {
            let Some(value) = number.as_u64() else {
                return Err(ToolError::new(format!("{key} must be a positive integer")));
            };
            Ok(value.clamp(minimum, maximum))
        }
        Some(_) => Err(ToolError::new(format!("{key} must be an integer"))),
    }
}

/// Parse and validate one tool call's name and JSON arguments.
///
/// # Errors
/// Returns malformed JSON, a missing or mistyped field, or a field value the
/// tool cannot use.
pub fn parse_invocation(name: &str, arguments: &str) -> Result<ToolInvocation, ToolError> {
    let object = object(arguments)?;
    match name {
        "bash" => Ok(ToolInvocation::Bash {
            command: required_string(&object, "command")?,
            timeout_ms: optional_u64(
                &object,
                "timeout_ms",
                DEFAULT_BASH_TIMEOUT_MS,
                1_000,
                MAX_BASH_TIMEOUT_MS,
            )?,
        }),
        "read_file" => Ok(ToolInvocation::ReadFile {
            path: required_string(&object, "path")?,
            offset: optional_u64(&object, "offset", 1, 1, u64::MAX)?,
            limit: optional_u64(&object, "limit", DEFAULT_READ_LIMIT, 1, MAX_READ_LIMIT)?,
        }),
        "write_file" => Ok(ToolInvocation::WriteFile {
            path: required_string(&object, "path")?,
            content: match object.get("content") {
                Some(Value::String(value)) => value.clone(),
                Some(_) => return Err(ToolError::new("content must be a string")),
                None => return Err(ToolError::new("content is required")),
            },
        }),
        "edit_file" => {
            let path = required_string(&object, "path")?;
            let old = required_string(&object, "old")?;
            let new = match object.get("new") {
                Some(Value::String(value)) => value.clone(),
                Some(_) => return Err(ToolError::new("new must be a string")),
                None => return Err(ToolError::new("new is required")),
            };
            if old == new {
                return Err(ToolError::new("old and new text are identical"));
            }
            Ok(ToolInvocation::EditFile { path, old, new })
        }
        "list_dir" => Ok(ToolInvocation::ListDir {
            path: optional_string(&object, "path", ".")?,
        }),
        "search" => Ok(ToolInvocation::Search {
            pattern: required_string(&object, "pattern")?,
            path: optional_string(&object, "path", ".")?,
            max_results: optional_u64(
                &object,
                "max_results",
                DEFAULT_SEARCH_RESULTS,
                1,
                MAX_SEARCH_RESULTS,
            )?,
        }),
        other => Err(ToolError::new(format!("unknown tool {other:?}"))),
    }
}

/// The tool names, in declaration order.
#[must_use]
pub fn tool_names() -> Vec<&'static str> {
    TOOL_SPECS.iter().map(|spec| spec.name).collect()
}

#[cfg(test)]
mod tests {
    use super::{
        DEFAULT_BASH_TIMEOUT_MS, DEFAULT_READ_LIMIT, DEFAULT_SEARCH_RESULTS, MAX_BASH_TIMEOUT_MS,
        MAX_READ_LIMIT, MAX_SEARCH_RESULTS, ToolInvocation, parse_invocation, tool_definitions,
        tool_names,
    };
    use alloc::string::{String, ToString};
    use alloc::vec;
    use serde_json::json;

    #[test]
    fn every_tool_has_a_definition_with_parameters() {
        let definitions = tool_definitions();
        let entries = definitions.as_array().expect("array");
        assert_eq!(entries.len(), 6);
        for entry in entries {
            assert_eq!(entry["type"], "function");
            assert_eq!(entry["function"]["parameters"]["type"], "object");
            assert!(
                !entry["function"]["description"]
                    .as_str()
                    .unwrap()
                    .is_empty()
            );
        }
        let by_name = |name: &str| {
            entries
                .iter()
                .find(|entry| entry["function"]["name"] == name)
                .expect(name)
                .clone()
        };
        assert_eq!(
            by_name("bash")["function"]["parameters"]["required"][0],
            "command"
        );
        assert_eq!(
            by_name("read_file")["function"]["parameters"]["required"][0],
            "path"
        );
        assert_eq!(
            by_name("write_file")["function"]["parameters"]["required"],
            json!(["path", "content"])
        );
        assert_eq!(
            by_name("edit_file")["function"]["parameters"]["required"],
            json!(["path", "old", "new"])
        );
        assert!(by_name("list_dir")["function"]["parameters"]["properties"]["path"].is_object());
        assert_eq!(
            by_name("search")["function"]["parameters"]["required"][0],
            "pattern"
        );
        assert_eq!(
            tool_names(),
            vec![
                "bash",
                "read_file",
                "write_file",
                "edit_file",
                "list_dir",
                "search"
            ]
        );
    }

    #[test]
    fn a_tool_error_displays_its_message() {
        assert_eq!(
            super::ToolError {
                message: "boom".to_string()
            }
            .to_string(),
            "boom"
        );
    }

    #[test]
    fn bash_defaults_and_clamps_its_timeout() {
        assert_eq!(
            parse_invocation("bash", r#"{"command":"ls"}"#).expect("bash"),
            ToolInvocation::Bash {
                command: "ls".to_string(),
                timeout_ms: DEFAULT_BASH_TIMEOUT_MS
            }
        );
        assert_eq!(
            parse_invocation("bash", r#"{"command":"ls","timeout_ms":1}"#)
                .expect("clamp low")
                .tool_name(),
            "bash"
        );
        assert!(matches!(
            parse_invocation("bash", r#"{"command":"ls","timeout_ms":1}"#),
            Ok(ToolInvocation::Bash {
                timeout_ms: 1_000,
                ..
            })
        ));
        assert!(matches!(
            parse_invocation("bash", r#"{"command":"ls","timeout_ms":9999999}"#),
            Ok(ToolInvocation::Bash {
                timeout_ms: MAX_BASH_TIMEOUT_MS,
                ..
            })
        ));
    }

    #[test]
    fn required_strings_reject_missing_empty_and_wrong_types() {
        for arguments in [
            "{}",
            r#"{"command":""}"#,
            r#"{"command":"  "}"#,
            r#"{"command":3}"#,
        ] {
            assert!(parse_invocation("bash", arguments).is_err(), "{arguments}");
        }
        assert!(parse_invocation("bash", "not json").is_err());
        assert!(parse_invocation("bash", "[]").is_err());
    }

    #[test]
    fn read_file_defaults_and_clamps_its_range() {
        assert_eq!(
            parse_invocation("read_file", r#"{"path":"a"}"#).expect("read"),
            ToolInvocation::ReadFile {
                path: "a".to_string(),
                offset: 1,
                limit: DEFAULT_READ_LIMIT
            }
        );
        assert!(matches!(
            parse_invocation("read_file", r#"{"path":"a","limit":0}"#),
            Ok(ToolInvocation::ReadFile { limit: 1, .. })
        ));
        assert!(matches!(
            parse_invocation("read_file", r#"{"path":"a","limit":999999}"#),
            Ok(ToolInvocation::ReadFile {
                limit: MAX_READ_LIMIT,
                ..
            })
        ));
        assert!(parse_invocation("read_file", r#"{"path":"a","offset":0}"#).is_ok());
        assert!(parse_invocation("read_file", r#"{"path":"a","offset":-1}"#).is_err());
        assert!(parse_invocation("read_file", r#"{"path":"a","limit":"x"}"#).is_err());
    }

    #[test]
    fn write_file_accepts_empty_content_but_requires_one() {
        assert_eq!(
            parse_invocation("write_file", r#"{"path":"a","content":""}"#).expect("write"),
            ToolInvocation::WriteFile {
                path: "a".to_string(),
                content: String::new()
            }
        );
        assert!(parse_invocation("write_file", r#"{"path":"a"}"#).is_err());
        assert!(parse_invocation("write_file", r#"{"path":"a","content":null}"#).is_err());
        assert!(parse_invocation("write_file", r#"{"path":"","content":"x"}"#).is_err());
    }

    #[test]
    fn edit_file_requires_distinct_non_empty_texts() {
        assert_eq!(
            parse_invocation("edit_file", r#"{"path":"a","old":"x","new":"y"}"#).expect("edit"),
            ToolInvocation::EditFile {
                path: "a".to_string(),
                old: "x".to_string(),
                new: "y".to_string()
            }
        );
        assert!(parse_invocation("edit_file", r#"{"path":"a","old":"x","new":"x"}"#).is_err());
        assert!(parse_invocation("edit_file", r#"{"path":"a","old":"","new":"y"}"#).is_err());
        assert!(parse_invocation("edit_file", r#"{"path":"a","old":"x"}"#).is_err());
        assert!(parse_invocation("edit_file", r#"{"path":"a","old":"x","new":1}"#).is_err());
    }

    #[test]
    fn list_dir_defaults_to_the_workspace_root() {
        assert_eq!(
            parse_invocation("list_dir", "{}").expect("list"),
            ToolInvocation::ListDir {
                path: ".".to_string()
            }
        );
        assert_eq!(
            parse_invocation("list_dir", r#"{"path":"src"}"#).expect("list"),
            ToolInvocation::ListDir {
                path: "src".to_string()
            }
        );
        assert!(parse_invocation("list_dir", r#"{"path":""}"#).is_err());
        assert!(parse_invocation("list_dir", r#"{"path":3}"#).is_err());
    }

    #[test]
    fn search_defaults_and_clamps_its_result_count() {
        assert_eq!(
            parse_invocation("search", r#"{"pattern":"fn "}"#).expect("search"),
            ToolInvocation::Search {
                pattern: "fn ".to_string(),
                path: ".".to_string(),
                max_results: DEFAULT_SEARCH_RESULTS
            }
        );
        assert!(matches!(
            parse_invocation("search", r#"{"pattern":"x","max_results":0}"#),
            Ok(ToolInvocation::Search { max_results: 1, .. })
        ));
        assert!(matches!(
            parse_invocation("search", r#"{"pattern":"x","max_results":99999}"#),
            Ok(ToolInvocation::Search {
                max_results: MAX_SEARCH_RESULTS,
                ..
            })
        ));
        assert!(parse_invocation("search", r#"{"pattern":""}"#).is_err());
    }

    #[test]
    fn an_unknown_tool_is_rejected_with_its_name() {
        let error = parse_invocation("nope", "{}").expect_err("unknown");
        assert!(error.message.contains("nope"));
    }

    #[test]
    fn every_tool_name_round_trips_through_its_invocation() {
        let cases = [
            ("bash", r#"{"command":"ls"}"#),
            ("read_file", r#"{"path":"a"}"#),
            ("write_file", r#"{"path":"a","content":"b"}"#),
            ("edit_file", r#"{"path":"a","old":"b","new":"c"}"#),
            ("list_dir", r#"{"path":"."}"#),
            ("search", r#"{"pattern":"x"}"#),
        ];
        for (name, arguments) in cases {
            let invocation = parse_invocation(name, arguments).expect(name);
            assert_eq!(invocation.tool_name(), name);
        }
    }
}
