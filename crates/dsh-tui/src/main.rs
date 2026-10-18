//! Standalone `dsh-tui` entry point.
//!
//! The `dsh` launcher runs this crate in process. The binary exists so the TUI
//! can also be started directly and so its own tests can drive a real process.

use std::path::PathBuf;
use std::process::ExitCode;

use dsh_tui::{HELP, Parsed, parse_args, run};

fn home() -> PathBuf {
    if let Some(configured) = std::env::var_os("DSH_HOME") {
        return PathBuf::from(configured);
    }
    let os_home = std::env::var("HOME").map_or_else(|_| PathBuf::from("."), PathBuf::from);
    os_home.join(".dsh")
}

fn main() -> ExitCode {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    let workspace = match std::env::current_dir() {
        Ok(workspace) => workspace,
        Err(error) => {
            eprintln!("dsh-tui: cannot read the current directory: {error}");
            return ExitCode::from(1);
        }
    };
    match parse_args(&arguments, home(), workspace) {
        Ok(Parsed::Help) => {
            print!("{HELP}");
            ExitCode::SUCCESS
        }
        Ok(Parsed::Run(options)) => ExitCode::from(u8::try_from(run(&options)).unwrap_or(1)),
        Err(error) => {
            eprintln!("dsh-tui: {error}");
            ExitCode::from(2)
        }
    }
}
