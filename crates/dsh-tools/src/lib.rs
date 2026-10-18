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
//! Workspace tool execution.
//!
//! Every path resolves inside the workspace root. Failures return text the
//! model can read, never a panic. Shell output is bounded and labelled with
//! its exit status so a failing command is not mistaken for a successful one.

use std::fmt::Write as _;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use dsh_agent::{ToolInvocation, truncate_output};
use regex::Regex;
use walkdir::WalkDir;

/// Maximum characters returned from any one tool.
pub const MAX_OUTPUT_CHARS: usize = 30_000;

/// Directories a search never descends into.
pub const SEARCH_SKIPPED_DIRS: &[&str] = &[".git", "target", "node_modules"];

/// One tool result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolResult {
    /// Text the model receives.
    pub content: String,
    /// Whether the tool failed.
    pub is_error: bool,
}

impl ToolResult {
    fn ok(content: impl Into<String>) -> Self {
        Self {
            content: truncate_output(&content.into(), MAX_OUTPUT_CHARS),
            is_error: false,
        }
    }

    fn error(message: impl Into<String>) -> Self {
        Self {
            content: truncate_output(&message.into(), MAX_OUTPUT_CHARS),
            is_error: true,
        }
    }
}

/// A workspace root and the tools that operate inside it.
#[derive(Debug, Clone)]
pub struct Workspace {
    root: PathBuf,
}

impl Workspace {
    /// Open a workspace.
    ///
    /// # Errors
    /// Returns when the root does not exist or cannot be canonicalized.
    pub fn new(root: impl AsRef<Path>) -> Result<Self, String> {
        let root = root.as_ref().canonicalize().map_err(|error| {
            format!("cannot open workspace {}: {error}", root.as_ref().display())
        })?;
        if !root.is_dir() {
            return Err(format!("workspace {} is not a directory", root.display()));
        }
        Ok(Self { root })
    }

    /// The canonical workspace root.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Resolve a tool path and prove it stays inside the workspace.
    ///
    /// # Errors
    /// Returns when the path escapes the workspace or its parent does not
    /// exist.
    pub fn resolve(&self, path: &str) -> Result<PathBuf, String> {
        let candidate = if Path::new(path).is_absolute() {
            PathBuf::from(path)
        } else {
            self.root.join(path)
        };
        let resolved = if candidate.exists() {
            candidate
                .canonicalize()
                .map_err(|error| format!("cannot resolve {path}: {error}"))?
        } else {
            let parent = candidate
                .parent()
                .ok_or_else(|| format!("{path} has no parent directory"))?;
            let canonical_parent = parent
                .canonicalize()
                .map_err(|error| format!("cannot resolve {}: {error}", parent.display()))?;
            let name = candidate
                .file_name()
                .ok_or_else(|| format!("{path} has no file name"))?;
            canonical_parent.join(name)
        };
        if !resolved.starts_with(&self.root) {
            return Err(format!("{path} escapes the workspace"));
        }
        Ok(resolved)
    }

    /// Resolve a path that may not exist yet, proving the result stays inside
    /// the workspace.
    ///
    /// # Errors
    /// Returns when the path escapes the workspace or no ancestor exists.
    pub fn resolve_for_write(&self, path: &str) -> Result<PathBuf, String> {
        let candidate = if Path::new(path).is_absolute() {
            PathBuf::from(path)
        } else {
            self.root.join(path)
        };
        let mut suffix: Vec<std::ffi::OsString> = Vec::new();
        let mut current = candidate.as_path();
        let resolved = loop {
            if current.exists() {
                break current
                    .canonicalize()
                    .map_err(|error| format!("cannot resolve {path}: {error}"))?;
            }
            let Some(name) = current.file_name() else {
                return Err(format!("cannot resolve {path}"));
            };
            suffix.push(name.to_os_string());
            current = current
                .parent()
                .ok_or_else(|| format!("cannot resolve {path}"))?;
        };
        let mut resolved = resolved;
        for name in suffix.into_iter().rev() {
            resolved.push(name);
        }
        if !resolved.starts_with(&self.root) {
            return Err(format!("{path} escapes the workspace"));
        }
        Ok(resolved)
    }

    /// Execute one validated invocation.
    #[must_use]
    pub fn execute(&self, invocation: &ToolInvocation) -> ToolResult {
        match invocation {
            ToolInvocation::Bash {
                command,
                timeout_ms,
            } => self.bash(command, Duration::from_millis(*timeout_ms)),
            ToolInvocation::ReadFile {
                path,
                offset,
                limit,
            } => self.read_file(path, *offset, *limit),
            ToolInvocation::WriteFile { path, content } => self.write_file(path, content),
            ToolInvocation::EditFile { path, old, new } => self.edit_file(path, old, new),
            ToolInvocation::ListDir { path } => self.list_dir(path),
            ToolInvocation::Search {
                pattern,
                path,
                max_results,
            } => self.search(pattern, path, *max_results),
        }
    }

    fn bash(&self, command: &str, timeout: Duration) -> ToolResult {
        let mut child = match Command::new("sh")
            .arg("-c")
            .arg(command)
            .current_dir(&self.root)
            .env_remove("DEEPSEEK_API_KEY")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
        {
            Ok(child) => child,
            Err(error) => return ToolResult::error(format!("cannot start the command: {error}")),
        };
        let stdout = child.stdout.take().map(spawn_reader);
        let stderr = child.stderr.take().map(spawn_reader);
        let deadline = Instant::now() + timeout;
        let mut timed_out = false;
        let status = loop {
            match child.try_wait() {
                Ok(Some(status)) => break Some(status),
                Ok(None) if Instant::now() >= deadline => {
                    timed_out = true;
                    let _ = child.kill();
                    break child.wait().ok();
                }
                Ok(None) => std::thread::sleep(Duration::from_millis(20)),
                Err(error) => {
                    let _ = child.kill();
                    return ToolResult::error(format!("cannot wait for the command: {error}"));
                }
            }
        };
        let out = stdout
            .map(|reader| reader.join().unwrap_or_default())
            .unwrap_or_default();
        let err = stderr
            .map(|reader| reader.join().unwrap_or_default())
            .unwrap_or_default();
        let mut content = String::new();
        content.push_str(&String::from_utf8_lossy(&out));
        if !err.is_empty() {
            if !content.is_empty() && !content.ends_with('\n') {
                content.push('\n');
            }
            content.push_str("[stderr]\n");
            content.push_str(&String::from_utf8_lossy(&err));
        }
        let code = status.and_then(|status| status.code());
        if timed_out {
            let _ = write!(
                content,
                "\n[command timed out after {} ms]",
                timeout.as_millis()
            );
        } else {
            let _ = write!(content, "\n[exit {}]", code.unwrap_or(-1));
        }
        let is_error = timed_out || code != Some(0);
        ToolResult {
            content: truncate_output(&content, MAX_OUTPUT_CHARS),
            is_error,
        }
    }

    fn read_file(&self, path: &str, offset: u64, limit: u64) -> ToolResult {
        let resolved = match self.resolve(path) {
            Ok(resolved) => resolved,
            Err(error) => return ToolResult::error(error),
        };
        let content = match std::fs::read_to_string(&resolved) {
            Ok(content) => content,
            Err(error) => return ToolResult::error(format!("cannot read {path}: {error}")),
        };
        let start = usize::try_from(offset.saturating_sub(1)).unwrap_or(usize::MAX);
        let take = usize::try_from(limit).unwrap_or(usize::MAX);
        let lines: Vec<&str> = content.lines().collect();
        if start >= lines.len() {
            return ToolResult::ok(format!(
                "[offset {offset} is past the end; the file has {} lines]",
                lines.len()
            ));
        }
        let end = start.saturating_add(take).min(lines.len());
        let mut selected = lines[start..end].join("\n");
        if content.ends_with('\n') && end == lines.len() {
            selected.push('\n');
        }
        ToolResult::ok(selected)
    }

    fn write_file(&self, path: &str, content: &str) -> ToolResult {
        let resolved = match self.resolve_for_write(path) {
            Ok(resolved) => resolved,
            Err(error) => return ToolResult::error(error),
        };
        if let Some(parent) = resolved.parent()
            && let Err(error) = std::fs::create_dir_all(parent)
        {
            return ToolResult::error(format!("cannot create {}: {error}", parent.display()));
        }
        match std::fs::write(&resolved, content) {
            Ok(()) => ToolResult::ok(format!("wrote {} bytes to {path}", content.len())),
            Err(error) => ToolResult::error(format!("cannot write {path}: {error}")),
        }
    }

    fn edit_file(&self, path: &str, old: &str, new: &str) -> ToolResult {
        let resolved = match self.resolve(path) {
            Ok(resolved) => resolved,
            Err(error) => return ToolResult::error(error),
        };
        let content = match std::fs::read_to_string(&resolved) {
            Ok(content) => content,
            Err(error) => return ToolResult::error(format!("cannot read {path}: {error}")),
        };
        let matches = content.matches(old).count();
        if matches == 0 {
            return ToolResult::error(format!("{path} does not contain the old text"));
        }
        if matches > 1 {
            return ToolResult::error(format!(
                "{path} contains the old text {matches} times; the edit must match exactly once"
            ));
        }
        let replaced = content.replacen(old, new, 1);
        match std::fs::write(&resolved, replaced) {
            Ok(()) => ToolResult::ok(format!("edited {path}")),
            Err(error) => ToolResult::error(format!("cannot write {path}: {error}")),
        }
    }

    fn list_dir(&self, path: &str) -> ToolResult {
        let resolved = match self.resolve(path) {
            Ok(resolved) => resolved,
            Err(error) => return ToolResult::error(error),
        };
        let entries = match std::fs::read_dir(&resolved) {
            Ok(entries) => entries,
            Err(error) => return ToolResult::error(format!("cannot list {path}: {error}")),
        };
        let mut names: Vec<String> = Vec::new();
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if entry.file_type().is_ok_and(|kind| kind.is_dir()) {
                names.push(format!("{name}/"));
            } else {
                names.push(name);
            }
        }
        names.sort();
        if names.is_empty() {
            return ToolResult::ok("(empty)");
        }
        ToolResult::ok(names.join("\n"))
    }

    fn search(&self, pattern: &str, path: &str, max_results: u64) -> ToolResult {
        let resolved = match self.resolve(path) {
            Ok(resolved) => resolved,
            Err(error) => return ToolResult::error(error),
        };
        let regex = match Regex::new(pattern) {
            Ok(regex) => regex,
            Err(error) => return ToolResult::error(format!("invalid pattern: {error}")),
        };
        let limit = usize::try_from(max_results).unwrap_or(usize::MAX);
        let mut matches = Vec::new();
        let mut truncated = false;
        for entry in WalkDir::new(&resolved)
            .follow_links(false)
            .into_iter()
            .filter_entry(|entry| {
                !SEARCH_SKIPPED_DIRS.iter().any(|skipped| {
                    entry.file_type().is_dir() && entry.file_name().to_string_lossy() == *skipped
                })
            })
            .flatten()
        {
            if !entry.file_type().is_file() {
                continue;
            }
            let Ok(content) = std::fs::read_to_string(entry.path()) else {
                continue;
            };
            for (index, line) in content.lines().enumerate() {
                if !regex.is_match(line) {
                    continue;
                }
                if matches.len() >= limit {
                    truncated = true;
                    break;
                }
                let display = entry
                    .path()
                    .strip_prefix(&self.root)
                    .unwrap_or(entry.path())
                    .display();
                matches.push(format!("{display}:{}: {}", index + 1, line.trim_end()));
            }
            if truncated {
                break;
            }
        }
        if matches.is_empty() {
            return ToolResult::ok("(no matches)");
        }
        if truncated {
            matches.push(format!("[stopped after {max_results} matches]"));
        }
        ToolResult::ok(matches.join("\n"))
    }
}

fn spawn_reader(mut stream: impl Read + Send + 'static) -> std::thread::JoinHandle<Vec<u8>> {
    std::thread::spawn(move || {
        let mut output = Vec::new();
        let _ = stream.read_to_end(&mut output);
        output
    })
}

#[cfg(test)]
mod tests {
    use super::{MAX_OUTPUT_CHARS, SEARCH_SKIPPED_DIRS, Workspace};
    use dsh_agent::ToolInvocation;
    use std::fs;

    fn workspace() -> (tempfile::TempDir, Workspace) {
        let directory = tempfile::tempdir().expect("tempdir");
        let workspace = Workspace::new(directory.path()).expect("workspace");
        (directory, workspace)
    }

    #[test]
    fn paths_cannot_escape_the_workspace() {
        let (_directory, workspace) = workspace();
        assert!(workspace.resolve("a.txt").is_ok());
        assert!(workspace.resolve("../outside.txt").is_err());
        assert!(workspace.resolve("/etc/passwd").is_err());
        assert!(workspace.resolve_for_write("new/dir/a.txt").is_ok());
        assert!(workspace.resolve_for_write("../outside.txt").is_err());
        assert!(workspace.resolve_for_write("/etc/passwd").is_err());
    }

    #[test]
    fn write_read_and_edit_round_trip() {
        let (_directory, workspace) = workspace();
        let write = workspace.execute(&ToolInvocation::WriteFile {
            path: "src/a.txt".to_string(),
            content: "one\ntwo\nthree\n".to_string(),
        });
        assert!(!write.is_error, "{}", write.content);
        let read = workspace.execute(&ToolInvocation::ReadFile {
            path: "src/a.txt".to_string(),
            offset: 2,
            limit: 1,
        });
        assert_eq!(read.content, "two");
        let edit = workspace.execute(&ToolInvocation::EditFile {
            path: "src/a.txt".to_string(),
            old: "two".to_string(),
            new: "TWO".to_string(),
        });
        assert!(!edit.is_error, "{}", edit.content);
        let read = workspace.execute(&ToolInvocation::ReadFile {
            path: "src/a.txt".to_string(),
            offset: 1,
            limit: 10,
        });
        assert_eq!(read.content, "one\nTWO\nthree\n");
    }

    #[test]
    fn an_edit_must_match_exactly_once() {
        let (_directory, workspace) = workspace();
        fs::write(workspace.root().join("a.txt"), "x x").expect("write");
        let repeated = workspace.execute(&ToolInvocation::EditFile {
            path: "a.txt".to_string(),
            old: "x".to_string(),
            new: "y".to_string(),
        });
        assert!(repeated.is_error);
        assert!(repeated.content.contains("2 times"));
        let missing = workspace.execute(&ToolInvocation::EditFile {
            path: "a.txt".to_string(),
            old: "z".to_string(),
            new: "y".to_string(),
        });
        assert!(missing.is_error);
        assert!(missing.content.contains("does not contain"));
    }

    #[test]
    fn read_past_the_end_reports_the_line_count() {
        let (_directory, workspace) = workspace();
        fs::write(workspace.root().join("a.txt"), "one\n").expect("write");
        let read = workspace.execute(&ToolInvocation::ReadFile {
            path: "a.txt".to_string(),
            offset: 5,
            limit: 1,
        });
        assert!(read.content.contains("past the end"));
        assert!(read.content.contains("1 lines"));
    }

    #[test]
    fn listing_sorts_and_marks_directories() {
        let (_directory, workspace) = workspace();
        fs::create_dir(workspace.root().join("zdir")).expect("dir");
        fs::write(workspace.root().join("afile"), "x").expect("file");
        let listed = workspace.execute(&ToolInvocation::ListDir {
            path: ".".to_string(),
        });
        assert_eq!(listed.content, "afile\nzdir/");
        fs::remove_file(workspace.root().join("afile")).expect("remove");
        fs::remove_dir(workspace.root().join("zdir")).expect("remove");
        assert_eq!(
            workspace
                .execute(&ToolInvocation::ListDir {
                    path: ".".to_string()
                })
                .content,
            "(empty)"
        );
    }

    #[test]
    fn search_reports_paths_lines_and_skips_build_directories() {
        let (_directory, workspace) = workspace();
        fs::create_dir_all(workspace.root().join("src")).expect("src");
        fs::create_dir_all(workspace.root().join("target")).expect("target");
        fs::write(workspace.root().join("src/a.rs"), "fn main() {}\n").expect("file");
        fs::write(workspace.root().join("target/b.rs"), "fn hidden() {}\n").expect("file");
        let found = workspace.execute(&ToolInvocation::Search {
            pattern: "fn ".to_string(),
            path: ".".to_string(),
            max_results: 10,
        });
        assert!(found.content.contains("src/a.rs:1: fn main()"));
        assert!(!found.content.contains("target"));
        assert!(SEARCH_SKIPPED_DIRS.contains(&"target"));
        let none = workspace.execute(&ToolInvocation::Search {
            pattern: "missing".to_string(),
            path: ".".to_string(),
            max_results: 10,
        });
        assert_eq!(none.content, "(no matches)");
        let invalid = workspace.execute(&ToolInvocation::Search {
            pattern: "(".to_string(),
            path: ".".to_string(),
            max_results: 10,
        });
        assert!(invalid.is_error);
    }

    #[test]
    fn search_stops_at_the_result_limit() {
        let (_directory, workspace) = workspace();
        fs::write(workspace.root().join("a.txt"), "x\nx\nx\n").expect("file");
        let found = workspace.execute(&ToolInvocation::Search {
            pattern: "x".to_string(),
            path: ".".to_string(),
            max_results: 2,
        });
        assert_eq!(found.content.matches("a.txt").count(), 2);
        assert!(found.content.contains("stopped after 2 matches"));
    }

    #[test]
    fn bash_reports_output_and_exit_status() {
        let (_directory, workspace) = workspace();
        let ok = workspace.execute(&ToolInvocation::Bash {
            command: "printf hello".to_string(),
            timeout_ms: 5_000,
        });
        assert_eq!(ok.content, "hello\n[exit 0]");
        assert!(!ok.is_error);
        let failed = workspace.execute(&ToolInvocation::Bash {
            command: "echo bad >&2; exit 3".to_string(),
            timeout_ms: 5_000,
        });
        assert!(failed.is_error);
        assert!(failed.content.contains("[stderr]\nbad"));
        assert!(failed.content.contains("[exit 3]"));
    }

    #[test]
    fn bash_times_out_and_is_killed() {
        let (_directory, workspace) = workspace();
        let timed = workspace.execute(&ToolInvocation::Bash {
            command: "sleep 5".to_string(),
            timeout_ms: 100,
        });
        assert!(timed.is_error);
        assert!(timed.content.contains("timed out after 100 ms"));
    }

    #[test]
    fn output_is_truncated() {
        let (_directory, workspace) = workspace();
        let large = "x".repeat(MAX_OUTPUT_CHARS + 100);
        let result = workspace.execute(&ToolInvocation::WriteFile {
            path: "a.txt".to_string(),
            content: large,
        });
        assert!(!result.is_error);
        let read = workspace.execute(&ToolInvocation::ReadFile {
            path: "a.txt".to_string(),
            offset: 1,
            limit: 1,
        });
        assert!(read.content.len() <= MAX_OUTPUT_CHARS + 100);
        assert!(read.content.contains("...") || read.content.len() < MAX_OUTPUT_CHARS);
    }

    #[test]
    fn a_workspace_must_be_a_directory() {
        let directory = tempfile::tempdir().expect("tempdir");
        let file = directory.path().join("file");
        fs::write(&file, "x").expect("file");
        assert!(Workspace::new(&file).is_err());
        assert!(Workspace::new(directory.path().join("missing")).is_err());
    }
}

#[cfg(test)]
mod stderr_tests {
    use super::Workspace;
    use dsh_agent::ToolInvocation;

    #[test]
    fn stderr_separators_respect_existing_newlines() {
        let directory = tempfile::tempdir().expect("tempdir");
        let workspace = Workspace::new(directory.path()).expect("workspace");
        let inline = workspace.execute(&ToolInvocation::Bash {
            command: "printf hello; echo bad >&2; exit 1".to_string(),
            timeout_ms: 5_000,
        });
        assert_eq!(inline.content, "hello\n[stderr]\nbad\n\n[exit 1]");
        let trailing = workspace.execute(&ToolInvocation::Bash {
            command: "printf 'hello\\n'; echo bad >&2".to_string(),
            timeout_ms: 5_000,
        });
        assert!(
            trailing.content.starts_with("hello\n[stderr]"),
            "{}",
            trailing.content
        );
        let stderr_only = workspace.execute(&ToolInvocation::Bash {
            command: "echo bad >&2".to_string(),
            timeout_ms: 5_000,
        });
        assert!(
            stderr_only.content.starts_with("[stderr]\nbad"),
            "{}",
            stderr_only.content
        );
    }
}
