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
//! Startup failure reports for the `dsh` launcher.
//!
//! The terminal receives a short summary and the path of a private report.
//! The report keeps the launcher's own facts, the failed plugin rows, and the
//! loader messages, and warns that raw values may include configuration or
//! credentials before a person shares it. Formatting is pure; the caller owns
//! the clock, the UUID, and the filesystem.
#![no_std]
extern crate alloc;

#[cfg(test)]
extern crate std;

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::fmt::Write as _;

/// The warning that precedes every saved report.
pub const RAW_DIAGNOSTICS_WARNING: &str = "WARNING: Raw diagnostics may contain configuration or credential values from plugin errors. Review before sharing.";

/// Directory under the Harness home holding saved reports.
pub const LOGS_DIR: &str = "logs";

/// Launcher-owned context; no environment values or plugin configurations are collected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiagnosticContext {
    /// Resolved Harness home.
    pub home: String,
    /// Application version.
    pub version: String,
    /// Selected profile.
    pub profile: String,
}

/// One plugin row that did not activate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginFailure {
    /// The row id.
    pub id: String,
    /// The plugin module specifier.
    pub module: String,
    /// Whether the composition required this row.
    pub required: bool,
    /// The loader's fiber state.
    pub fiber_state: u8,
    /// Services the row is still waiting for.
    pub missing: Vec<String>,
    /// The failure's text, when the row reported one.
    pub error: Option<String>,
}

/// A startup audit failure with the metadata needed to diagnose it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StartupFailure {
    /// The summary printed to the terminal.
    pub message: String,
    /// The composed configuration file.
    pub configuration_path: Option<String>,
    /// Every failed or pending plugin row.
    pub plugins: Vec<PluginFailure>,
    /// Loader log lines captured during startup.
    pub messages: Vec<String>,
}

/// The terminal summary: the failure message and a newline.
#[must_use]
pub fn summary(failure: &StartupFailure) -> String {
    format!("{}\n", failure.message)
}

/// The file name for a saved report.
///
/// The timestamp's colons become hyphens so the name is valid on every
/// supported platform; the UUID keeps concurrent failures distinct.
#[must_use]
pub fn log_file_name(timestamp: &str, uuid: &str) -> String {
    let stamp = timestamp.replace(':', "-");
    format!("startup-{stamp}-{uuid}.log")
}

/// The absolute path of the saved report.
#[must_use]
pub fn log_path(home: &str, file_name: &str) -> String {
    format!("{home}/{LOGS_DIR}/{file_name}")
}

/// The line that names the saved report on the terminal.
#[must_use]
pub fn saved_path_line(path: &str) -> String {
    format!("\nFull diagnostics: {path}\n")
}

/// The warning printed when the report cannot be saved.
#[must_use]
pub fn write_failure_warning(error: &str) -> String {
    format!("\ndsh: warning: could not write startup diagnostics: {error}\nFull diagnostics:\n")
}

/// The complete private report.
#[must_use]
pub fn report(
    failure: &StartupFailure,
    context: &DiagnosticContext,
    timestamp: &str,
    platform: &str,
    arch: &str,
) -> String {
    let mut report = String::new();
    report.push_str(RAW_DIAGNOSTICS_WARNING);
    report.push_str("\n\n");
    report.push_str("{\n");
    let _ = writeln!(report, "  \"timestamp\": {},", json_string(timestamp));
    let _ = writeln!(
        report,
        "  \"dshVersion\": {},",
        json_string(&context.version)
    );
    let _ = writeln!(report, "  \"platform\": {},", json_string(platform));
    let _ = writeln!(report, "  \"arch\": {},", json_string(arch));
    let _ = writeln!(report, "  \"profile\": {},", json_string(&context.profile));
    report.push_str("  \"error\": {\n");
    let _ = writeln!(
        report,
        "    \"message\": {},",
        json_string(&failure.message)
    );
    if let Some(path) = &failure.configuration_path {
        let _ = writeln!(report, "    \"configurationPath\": {},", json_string(path));
    }
    report.push_str("    \"plugins\": [");
    for (index, plugin) in failure.plugins.iter().enumerate() {
        if index > 0 {
            report.push(',');
        }
        report.push_str("\n      {");
        let _ = write!(report, "\"id\": {}, ", json_string(&plugin.id));
        let _ = write!(report, "\"module\": {}, ", json_string(&plugin.module));
        let _ = write!(report, "\"required\": {}, ", plugin.required);
        let _ = write!(report, "\"fiberState\": {}, ", plugin.fiber_state);
        report.push_str("\"missing\": [");
        for (missing_index, service) in plugin.missing.iter().enumerate() {
            if missing_index > 0 {
                report.push_str(", ");
            }
            report.push_str(&json_string(service));
        }
        report.push(']');
        if let Some(error) = &plugin.error {
            let _ = write!(report, ", \"error\": {}", json_string(error));
        }
        report.push('}');
    }
    if failure.plugins.is_empty() {
        report.push(']');
    } else {
        report.push_str("\n    ]");
    }
    report.push_str(",\n    \"messages\": [");
    for (index, message) in failure.messages.iter().enumerate() {
        if index > 0 {
            report.push(',');
        }
        report.push_str("\n      ");
        report.push_str(&json_string(message));
    }
    if failure.messages.is_empty() {
        report.push(']');
    } else {
        report.push_str("\n    ]");
    }
    report.push_str("\n  }\n}\n");
    report
}

fn json_string(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len() + 2);
    escaped.push('"');
    for character in value.chars() {
        match character {
            '"' => escaped.push_str("\\\""),
            '\\' => escaped.push_str("\\\\"),
            '\n' => escaped.push_str("\\n"),
            '\r' => escaped.push_str("\\r"),
            '\t' => escaped.push_str("\\t"),
            control if control < ' ' => {
                let _ = write!(escaped, "\\u{:04x}", u32::from(control));
            }
            other => escaped.push(other),
        }
    }
    escaped.push('"');
    escaped
}

#[cfg(test)]
mod tests {
    use super::{
        DiagnosticContext, PluginFailure, RAW_DIAGNOSTICS_WARNING, StartupFailure, log_file_name,
        log_path, report, saved_path_line, summary, write_failure_warning,
    };
    use alloc::string::{String, ToString};
    use alloc::vec;

    fn failure() -> StartupFailure {
        StartupFailure {
            message: "dsh: startup failed: 1 required plugin did not activate".to_string(),
            configuration_path: Some("/example/cordis.yml".to_string()),
            plugins: vec![
                PluginFailure {
                    id: "webserver".to_string(),
                    module: "./webserver.mjs".to_string(),
                    required: true,
                    fiber_state: 3,
                    missing: vec![],
                    error: Some("failed".to_string()),
                },
                PluginFailure {
                    id: "waiting".to_string(),
                    module: "./waiting.mjs".to_string(),
                    required: false,
                    fiber_state: 0,
                    missing: vec!["webServer".to_string()],
                    error: None,
                },
            ],
            messages: vec!["loader error".to_string()],
        }
    }

    fn context() -> DiagnosticContext {
        DiagnosticContext {
            home: "/home/person/.dsh".to_string(),
            version: "1.2.3".to_string(),
            profile: "web".to_string(),
        }
    }

    #[test]
    fn summary_keeps_the_failure_message_on_its_own_line() {
        assert_eq!(
            summary(&failure()),
            "dsh: startup failed: 1 required plugin did not activate\n"
        );
    }

    #[test]
    fn report_names_are_unique_and_free_of_colons() {
        assert_eq!(
            log_file_name("2026-10-18T12:34:56.000Z", "0f8fad5b"),
            "startup-2026-10-18T12-34-56.000Z-0f8fad5b.log"
        );
        assert_eq!(
            log_path("/home/person/.dsh", "startup-a.log"),
            "/home/person/.dsh/logs/startup-a.log"
        );
    }

    #[test]
    fn terminal_lines_name_the_saved_report() {
        assert_eq!(
            saved_path_line("/home/person/.dsh/logs/startup-a.log"),
            "\nFull diagnostics: /home/person/.dsh/logs/startup-a.log\n"
        );
        assert_eq!(
            write_failure_warning("permission denied"),
            "\ndsh: warning: could not write startup diagnostics: permission denied\nFull diagnostics:\n"
        );
    }

    #[test]
    fn report_starts_with_the_sharing_warning_and_keeps_every_fact() {
        let rendered = report(
            &failure(),
            &context(),
            "2026-10-18T12:34:56.000Z",
            "linux",
            "x86_64",
        );
        assert!(rendered.starts_with(RAW_DIAGNOSTICS_WARNING));
        for expected in [
            "\"timestamp\": \"2026-10-18T12:34:56.000Z\"",
            "\"dshVersion\": \"1.2.3\"",
            "\"platform\": \"linux\"",
            "\"arch\": \"x86_64\"",
            "\"profile\": \"web\"",
            "\"configurationPath\": \"/example/cordis.yml\"",
            "\"module\": \"./waiting.mjs\"",
            "\"required\": false",
            "\"fiberState\": 0",
            "\"missing\": [\"webServer\"]",
            "\"error\": \"failed\"",
            "\"messages\": [\n      \"loader error\"\n    ]",
        ] {
            assert!(
                rendered.contains(expected),
                "missing {expected} in {rendered}"
            );
        }
    }

    #[test]
    fn report_escapes_strings_and_handles_empty_collections() {
        let empty = StartupFailure {
            message: "quote \" and slash \\".to_string(),
            configuration_path: None,
            plugins: vec![],
            messages: vec![],
        };
        let rendered = report(&empty, &context(), "t", "linux", "x86_64");
        assert!(rendered.contains("\"message\": \"quote \\\" and slash \\\\\""));
        assert!(rendered.contains("\"plugins\": [],"));
        assert!(rendered.contains("\"messages\": []"));
        assert!(!rendered.contains("configurationPath"));
    }

    #[test]
    fn report_has_a_stable_json_layout() {
        let failure = StartupFailure {
            message: "summary".to_string(),
            configuration_path: Some("/cfg.yml".to_string()),
            plugins: vec![
                PluginFailure {
                    id: "a".to_string(),
                    module: "./a.mjs".to_string(),
                    required: true,
                    fiber_state: 3,
                    missing: vec![],
                    error: Some("bad".to_string()),
                },
                PluginFailure {
                    id: "b".to_string(),
                    module: "./b.mjs".to_string(),
                    required: false,
                    fiber_state: 0,
                    missing: vec!["svc".to_string(), "other".to_string()],
                    error: None,
                },
            ],
            messages: vec!["first".to_string(), "second".to_string()],
        };
        let rendered = report(&failure, &context(), "t", "linux", "x86_64");
        assert_eq!(
            rendered,
            "WARNING: Raw diagnostics may contain configuration or credential values from plugin errors. Review before sharing.

{
  \"timestamp\": \"t\",
  \"dshVersion\": \"1.2.3\",
  \"platform\": \"linux\",
  \"arch\": \"x86_64\",
  \"profile\": \"web\",
  \"error\": {
    \"message\": \"summary\",
    \"configurationPath\": \"/cfg.yml\",
    \"plugins\": [
      {\"id\": \"a\", \"module\": \"./a.mjs\", \"required\": true, \"fiberState\": 3, \"missing\": [], \"error\": \"bad\"},
      {\"id\": \"b\", \"module\": \"./b.mjs\", \"required\": false, \"fiberState\": 0, \"missing\": [\"svc\", \"other\"]}
    ],
    \"messages\": [
      \"first\",
      \"second\"
    ]
  }
}
"
        );
    }

    #[test]
    fn control_characters_in_messages_are_escaped() {
        let failure = StartupFailure {
            message: "a\u{1f}b".to_string(),
            configuration_path: None,
            plugins: vec![],
            messages: vec![],
        };
        let rendered = report(&failure, &context(), "t", "linux", "x86_64");
        assert!(rendered.contains("a\\u001fb"));
        assert!(!rendered.contains('\u{1f}'));
    }

    #[test]
    fn report_contains_no_terminal_path_line() {
        let rendered = report(&failure(), &context(), "t", "linux", "x86_64");
        assert!(!rendered.contains("Full diagnostics: "));
        assert_eq!(
            String::from(RAW_DIAGNOSTICS_WARNING).len(),
            RAW_DIAGNOSTICS_WARNING.len()
        );
    }

    proptest::proptest! {
        #[test]
        fn file_names_never_contain_a_colon(
            stamp in "[0-9T:.Z-]{1,30}",
            id in "[a-f0-9-]{1,36}",
        ) {
            let name = log_file_name(&stamp, &id);
            proptest::prop_assert!(!name.contains(':'));
            proptest::prop_assert!(name.starts_with("startup-"));
            proptest::prop_assert_eq!(name.rsplit('.').next(), Some("log"));
        }

        #[test]
        fn report_escapes_any_message(message in ".{0,80}") {
            let failure = StartupFailure {
                message: message.clone(),
                configuration_path: None,
                plugins: vec![],
                messages: vec![message],
            };
            let rendered = report(&failure, &context(), "t", "linux", "x86_64");
            proptest::prop_assert!(rendered.contains("\"message\": "));
            proptest::prop_assert_eq!(rendered.as_bytes().last(), Some(&b'\n'));
        }
    }
}
