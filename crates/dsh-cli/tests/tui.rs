//! Dispatch acceptance for `dsh tui`.
//!
//! The agent behavior itself is covered by `dsh-tui` tests. These checks prove
//! the launcher routes `tui` to the native runtime without a host binary.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

use std::process::Command;

const fn binary() -> &'static str {
    env!("CARGO_BIN_EXE_dsh")
}

fn run(arguments: &[&str], home: &std::path::Path) -> std::process::Output {
    Command::new(binary())
        .args(arguments)
        .env("DSH_HOME", home)
        .env_remove("DEEPSEEK_API_KEY")
        .output()
        .expect("run dsh")
}

#[test]
fn tui_help_is_served_by_the_native_runtime() {
    let home = tempfile::tempdir().expect("home");
    let output = run(&["tui", "--help"], home.path());
    assert_eq!(output.status.code(), Some(0));
    assert!(String::from_utf8_lossy(&output.stdout).contains("Usage: dsh tui"));
    assert!(home.path().join("profiles/tui/package.json").is_file());
}

#[test]
fn tui_reaches_the_runtime_without_a_key_or_host() {
    let home = tempfile::tempdir().expect("home");
    let output = run(&["tui", "--print", "hi"], home.path());
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("DEEPSEEK_API_KEY"), "{stderr}");
    assert!(!stderr.contains("harness host"), "{stderr}");
}
