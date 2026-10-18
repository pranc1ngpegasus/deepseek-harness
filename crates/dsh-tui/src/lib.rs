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
//! `dsh tui`: a terminal coding agent.
//!
//! The launcher resolves the `tui` profile and hands this crate the Harness
//! home and inner arguments. This crate owns the provider client, workspace
//! tools, session log, and terminal loop. It never loads the TypeScript
//! harness.

#![forbid(unsafe_code)]

pub mod app;
mod render;
mod worker;

use std::path::PathBuf;
use std::sync::mpsc::channel;
use std::time::Duration;

use dsh_session::SessionLog;
use ratatui::crossterm::event::{self, Event, KeyEventKind};

use crate::app::{Action, App};
use crate::worker::Command;

/// Help text for `dsh tui --help`.
pub const HELP: &str = "\
Usage: dsh tui [options] [task]

A terminal coding agent. With no --print, the task opens the conversation.

Options:
  --print <task>    run one task, print the answer, and exit
  --model <name>    model name (default: deepseek-chat)
  --base-url <url>  OpenAI-compatible base URL (default: https://api.deepseek.com)
  --api-key <key>   API key (default: $DEEPSEEK_API_KEY)
  -y, --yes         approve every tool call without asking
  --max-turns <n>   model calls allowed per request (default: 32)
  -h, --help        show this help

Environment:
  DEEPSEEK_API_KEY, DEEPSEEK_BASE_URL, DEEPSEEK_MODEL or DSH_MODEL

Tools: bash, read_file, write_file, edit_file, list_dir, search.
Every tool path resolves inside the current workspace.
";

/// Resolved settings for one TUI run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Options {
    /// Harness home, for session logs.
    pub home: PathBuf,
    /// The workspace tools operate in.
    pub workspace: PathBuf,
    /// Model name.
    pub model: String,
    /// OpenAI-compatible base URL.
    pub base_url: String,
    /// Bearer token.
    pub api_key: String,
    /// Approve every tool call without asking.
    pub auto_approve: bool,
    /// One-shot task for `--print`.
    pub print: Option<String>,
    /// Model calls allowed per request.
    pub max_turns: u32,
    /// Prompt submitted when the TUI starts.
    pub initial_prompt: Option<String>,
}

/// The result of parsing `dsh tui` arguments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Parsed {
    /// Print the help and exit.
    Help,
    /// Run the agent.
    Run(Options),
}

fn next_value(iterator: &mut std::slice::Iter<'_, String>, option: &str) -> Result<String, String> {
    iterator
        .next()
        .cloned()
        .ok_or_else(|| format!("{option} needs a value"))
}

fn inline<'a>(argument: &'a str, option: &str) -> Option<&'a str> {
    argument.strip_prefix(option)?.strip_prefix('=')
}

/// Parse the launcher's inner arguments for `dsh tui`.
///
/// # Errors
/// Returns an unknown option, a missing option value, or a bad turn count.
pub fn parse_args(args: &[String], home: PathBuf, workspace: PathBuf) -> Result<Parsed, String> {
    let mut iterator = args.iter();
    let mut model: Option<String> = None;
    let mut base_url: Option<String> = None;
    let mut api_key: Option<String> = None;
    let mut print: Option<String> = None;
    let mut auto_approve = false;
    let mut max_turns: Option<u32> = None;
    let mut positionals: Vec<String> = Vec::new();
    while let Some(argument) = iterator.next() {
        match argument.as_str() {
            "-h" | "--help" => return Ok(Parsed::Help),
            "-y" | "--yes" => auto_approve = true,
            "--print" => print = Some(next_value(&mut iterator, "--print")?),
            "--model" => model = Some(next_value(&mut iterator, "--model")?),
            "--base-url" => base_url = Some(next_value(&mut iterator, "--base-url")?),
            "--api-key" => api_key = Some(next_value(&mut iterator, "--api-key")?),
            "--max-turns" => {
                let value = next_value(&mut iterator, "--max-turns")?;
                max_turns = Some(value.parse().map_err(|_| {
                    format!("--max-turns must be a positive integer, got {value:?}")
                })?);
            }
            other if other.starts_with('-') => {
                if let Some(value) = inline(other, "--print") {
                    print = Some(value.to_string());
                } else if let Some(value) = inline(other, "--model") {
                    model = Some(value.to_string());
                } else if let Some(value) = inline(other, "--base-url") {
                    base_url = Some(value.to_string());
                } else if let Some(value) = inline(other, "--api-key") {
                    api_key = Some(value.to_string());
                } else if let Some(value) = inline(other, "--max-turns") {
                    max_turns = Some(value.parse().map_err(|_| {
                        format!("--max-turns must be a positive integer, got {value:?}")
                    })?);
                } else {
                    return Err(format!("unknown option {other:?}"));
                }
            }
            other => positionals.push(other.to_string()),
        }
    }
    if print.is_some() && !positionals.is_empty() {
        return Err("give the task with --print or as an argument, not both".to_string());
    }
    let initial_prompt = if positionals.is_empty() {
        None
    } else {
        Some(positionals.join(" "))
    };
    let model = model
        .or_else(|| std::env::var("DEEPSEEK_MODEL").ok())
        .or_else(|| std::env::var("DSH_MODEL").ok())
        .unwrap_or_else(|| "deepseek-chat".to_string());
    let base_url = base_url
        .or_else(|| std::env::var("DEEPSEEK_BASE_URL").ok())
        .unwrap_or_else(|| "https://api.deepseek.com".to_string());
    let api_key = api_key
        .or_else(|| std::env::var("DEEPSEEK_API_KEY").ok())
        .unwrap_or_default();
    Ok(Parsed::Run(Options {
        home,
        workspace,
        model,
        base_url,
        api_key,
        auto_approve,
        print,
        max_turns: max_turns.unwrap_or(32).clamp(1, 1_000),
        initial_prompt,
    }))
}

/// Run the agent and return its exit status.
#[must_use]
pub fn run(options: &Options) -> i32 {
    if options.api_key.trim().is_empty() {
        eprintln!("dsh: DEEPSEEK_API_KEY is not set; export it or pass --api-key");
        return 1;
    }
    if let Some(task) = &options.print {
        return worker::run_print(options, task);
    }
    run_interactive(options)
}

/// Whether a terminal key event is a press this application should act on.
fn is_press(key: &ratatui::crossterm::event::KeyEvent) -> bool {
    key.kind == KeyEventKind::Press
}

fn run_interactive(options: &Options) -> i32 {
    let session = match SessionLog::create(&options.home) {
        Ok(session) => session,
        Err(error) => {
            eprintln!("dsh: cannot create a session log: {error}");
            return 1;
        }
    };
    let session_id = session.id().to_string();
    let (sender, receiver) = channel();
    let handle = match worker::spawn(options, sender, session) {
        Ok(handle) => handle,
        Err(error) => {
            eprintln!("dsh: {error}");
            return 1;
        }
    };
    let mut terminal = match ratatui::try_init() {
        Ok(terminal) => terminal,
        Err(error) => {
            eprintln!("dsh: cannot start the terminal: {error}");
            return 1;
        }
    };
    let mut app = App::new(&options.model, session_id);
    if let Some(prompt) = &options.initial_prompt
        && let Action::Submit(text) = app.submit(prompt)
    {
        handle.send(Command::Submit(text));
    }
    let mut result = Ok(());
    loop {
        if let Err(error) = terminal.draw(|frame| render::render(frame, &mut app)) {
            result = Err(format!("cannot draw: {error}"));
            break;
        }
        match event::poll(Duration::from_millis(50)) {
            Ok(true) => match event::read() {
                Ok(Event::Key(key)) => {
                    if is_press(&key) {
                        match app.handle_key(key) {
                            Action::Submit(text) => handle.send(Command::Submit(text)),
                            Action::Approval(approved) => handle.send(Command::Approval(approved)),
                            Action::Cancel => handle.send(Command::Cancel),
                            Action::Quit | Action::None => {}
                        }
                    }
                }
                Ok(_) => {}
                Err(error) => {
                    result = Err(format!("cannot read the terminal: {error}"));
                    break;
                }
            },
            Ok(false) => {}
            Err(error) => {
                result = Err(format!("cannot poll the terminal: {error}"));
                break;
            }
        }
        while let Ok(worker_event) = receiver.try_recv() {
            app.apply(worker_event);
        }
        if app.should_quit() {
            break;
        }
    }
    ratatui::restore();
    handle.send(Command::Quit);
    match result {
        Ok(()) => 0,
        Err(error) => {
            eprintln!("dsh: {error}");
            1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Parsed, parse_args};
    use std::path::PathBuf;

    fn parse(arguments: &[&str]) -> Parsed {
        parse_args(
            &arguments
                .iter()
                .map(|value| (*value).to_string())
                .collect::<Vec<_>>(),
            PathBuf::from("/home"),
            PathBuf::from("/work"),
        )
        .expect("parse")
    }

    fn resolve(arguments: &[&str]) -> super::Options {
        match parse(arguments) {
            Parsed::Run(options) => options,
            Parsed::Help => panic!("expected options"),
        }
    }

    #[test]
    fn help_is_recognized_before_anything_else() {
        assert_eq!(parse(&["--help"]), Parsed::Help);
        assert_eq!(parse(&["-h"]), Parsed::Help);
    }

    #[test]
    fn defaults_match_the_deepseek_provider() {
        let options = resolve(&[]);
        assert_eq!(
            options.model,
            std::env::var("DEEPSEEK_MODEL").unwrap_or_else(|_| {
                std::env::var("DSH_MODEL").unwrap_or_else(|_| "deepseek-chat".to_string())
            })
        );
        assert_eq!(options.home, PathBuf::from("/home"));
        assert_eq!(options.workspace, PathBuf::from("/work"));
        assert_eq!(options.max_turns, 32);
        assert!(!options.auto_approve);
        assert_eq!(options.print, None);
    }

    #[test]
    fn flags_accept_both_value_forms() {
        let options = resolve(&[
            "--model",
            "m1",
            "--base-url=http://x",
            "--api-key",
            "k",
            "--max-turns",
            "5",
            "--yes",
        ]);
        assert_eq!(options.model, "m1");
        assert_eq!(options.base_url, "http://x");
        assert_eq!(options.api_key, "k");
        assert_eq!(options.max_turns, 5);
        assert!(options.auto_approve);
        assert_eq!(resolve(&["--model=m2"]).model, "m2");
        assert_eq!(resolve(&["--api-key=k2"]).api_key, "k2");
        assert_eq!(resolve(&["--max-turns=0"]).max_turns, 1);
        assert_eq!(resolve(&["--max-turns=99999"]).max_turns, 1_000);
    }

    #[test]
    fn a_positional_task_becomes_the_initial_prompt() {
        let options = resolve(&["write", "a", "test"]);
        assert_eq!(options.initial_prompt.as_deref(), Some("write a test"));
        assert_eq!(options.print, None);
    }

    #[test]
    fn print_takes_its_task_and_rejects_a_second_one() {
        let options = resolve(&["--print", "do it"]);
        assert_eq!(options.print.as_deref(), Some("do it"));
        assert_eq!(options.initial_prompt, None);
        assert!(
            super::parse_args(
                &["--print".to_string(), "a".to_string(), "b".to_string()],
                PathBuf::from("/h"),
                PathBuf::from("/w"),
            )
            .is_err()
        );
        assert_eq!(
            resolve(&["--print=inline"]).print.as_deref(),
            Some("inline")
        );
    }

    #[test]
    fn only_key_presses_are_actions() {
        use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
        let press = KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE);
        assert!(super::is_press(&press));
        let release = KeyEvent::new_with_kind(
            KeyCode::Char('a'),
            KeyModifiers::NONE,
            KeyEventKind::Release,
        );
        assert!(!super::is_press(&release));
    }

    #[test]
    fn bad_options_are_rejected() {
        for arguments in [
            &["--bogus"][..],
            &["--model"],
            &["--print"],
            &["--max-turns", "x"],
        ] {
            assert!(
                parse_args(
                    &arguments
                        .iter()
                        .map(|value| (*value).to_string())
                        .collect::<Vec<_>>(),
                    PathBuf::from("/h"),
                    PathBuf::from("/w"),
                )
                .is_err(),
                "{arguments:?}"
            );
        }
    }
}
