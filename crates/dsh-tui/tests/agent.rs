//! End-to-end acceptance for `dsh tui`.
//!
//! A local SSE server stands in for the model. The print mode proves the full
//! agent loop, and tmux gives the interactive screen a real terminal.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    clippy::assert_is_empty
)]

use std::fs;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::PathBuf;
use std::process::{Command, Output};
use std::time::{Duration, Instant};

const fn binary() -> &'static str {
    env!("CARGO_BIN_EXE_dsh-tui")
}

struct Mock {
    base_url: String,
}

fn sse(events: &[&str]) -> String {
    let mut body = String::new();
    for event in events {
        body.push_str("data: ");
        body.push_str(event);
        body.push_str("\n\n");
    }
    body
}

fn serve(responses: Vec<String>) -> Mock {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let address = listener.local_addr().expect("addr");
    std::thread::spawn(move || {
        for body in responses {
            let Ok((mut stream, _)) = listener.accept() else {
                return;
            };
            let mut request = Vec::new();
            let mut buffer = [0_u8; 4096];
            let header_end = loop {
                let Ok(read) = stream.read(&mut buffer) else {
                    return;
                };
                if read == 0 {
                    return;
                }
                request.extend_from_slice(&buffer[..read]);
                if let Some(index) = request.windows(4).position(|window| window == b"\r\n\r\n") {
                    break index + 4;
                }
            };
            let headers = String::from_utf8_lossy(&request[..header_end]).to_ascii_lowercase();
            let content_length = headers
                .lines()
                .find_map(|line| line.strip_prefix("content-length:"))
                .and_then(|value| value.trim().parse::<usize>().ok())
                .unwrap_or(0);
            while request.len() < header_end + content_length {
                let Ok(read) = stream.read(&mut buffer) else {
                    return;
                };
                if read == 0 {
                    break;
                }
                request.extend_from_slice(&buffer[..read]);
            }
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = stream.write_all(response.as_bytes());
        }
    });
    Mock {
        base_url: format!("http://{address}"),
    }
}

struct Fixture {
    home: PathBuf,
    workspace: PathBuf,
    temp: tempfile::TempDir,
}

impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().expect("tempdir");
        let home = temp.path().join("home");
        let workspace = temp.path().join("workspace");
        fs::create_dir_all(&home).expect("home");
        fs::create_dir_all(&workspace).expect("workspace");
        Self {
            home,
            workspace,
            temp,
        }
    }

    fn command(&self, mock: &Mock, arguments: &[&str]) -> Command {
        let mut command = Command::new(binary());
        command
            .args(["--base-url", &mock.base_url, "--api-key", "test-key"])
            .args(arguments)
            .current_dir(&self.workspace)
            .env("DSH_HOME", &self.home)
            .env_remove("DEEPSEEK_API_KEY")
            .env_remove("DEEPSEEK_BASE_URL")
            .env_remove("DEEPSEEK_MODEL")
            .env_remove("DSH_MODEL");
        command
    }

    fn run(&self, mock: &Mock, arguments: &[&str]) -> Output {
        self.command(mock, arguments).output().expect("run dsh")
    }

    fn session_files(&self) -> Vec<PathBuf> {
        fs::read_dir(self.home.join("sessions"))
            .map(|entries| {
                entries
                    .flatten()
                    .map(|entry| entry.path())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default()
    }
}

fn text(output: &Output) -> (String, String) {
    (
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

#[test]
fn print_mode_runs_a_tool_and_prints_the_final_answer() {
    let fixture = Fixture::new();
    let mock = serve(vec![
        sse(&[
            r#"{"choices":[{"delta":{"content":"Writing the file."}}]}"#,
            r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_1","function":{"name":"write_file","arguments":"{\"path\":\"out.txt\",\"content\":\"hello from the agent\"}"}}]},"finish_reason":"tool_calls"}]}"#,
            "[DONE]",
        ]),
        sse(&[
            r#"{"choices":[{"delta":{"content":"Done. Wrote out.txt."}}]}"#,
            r#"{"choices":[{"delta":{},"finish_reason":"stop"}]}"#,
            "[DONE]",
        ]),
    ]);
    let output = fixture.run(&mock, &["--print", "write out.txt"]);
    let (stdout, stderr) = text(&output);
    assert_eq!(output.status.code(), Some(0), "{stderr}");
    assert!(stdout.contains("Writing the file."), "{stdout}");
    assert!(stdout.contains("Done. Wrote out.txt."), "{stdout}");
    assert!(
        stderr.contains("running write_file"),
        "tool trace on stderr: {stderr}"
    );
    assert_eq!(
        fs::read_to_string(fixture.workspace.join("out.txt")).expect("out.txt"),
        "hello from the agent"
    );
    let sessions = fixture.session_files();
    assert_eq!(sessions.len(), 1);
    let log = fs::read_to_string(&sessions[0]).expect("session log");
    assert!(log.contains("\"type\":\"user\""));
    assert!(log.contains("\"type\":\"assistant\""));
    assert!(log.contains("\"type\":\"tool\""));
    assert!(log.contains("write_file"));
}

#[test]
fn a_failed_tool_call_is_reported_and_the_model_can_recover() {
    let fixture = Fixture::new();
    let mock = serve(vec![
        sse(&[
            r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_1","function":{"name":"read_file","arguments":"{\"path\":\"missing.txt\"}"}}]},"finish_reason":"tool_calls"}]}"#,
            "[DONE]",
        ]),
        sse(&[
            r#"{"choices":[{"delta":{"content":"The file does not exist."}}]}"#,
            r#"{"choices":[{"delta":{},"finish_reason":"stop"}]}"#,
            "[DONE]",
        ]),
    ]);
    let output = fixture.run(&mock, &["--print", "read missing.txt"]);
    let (stdout, stderr) = text(&output);
    assert_eq!(output.status.code(), Some(0), "{stderr}");
    assert!(stdout.contains("The file does not exist."));
    assert!(stderr.contains("cannot read missing.txt"), "{stderr}");
}

#[test]
fn a_provider_failure_exits_nonzero_with_the_body() {
    let fixture = Fixture::new();
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let address = listener.local_addr().expect("addr");
    std::thread::spawn(move || {
        for _ in 0..2 {
            let Ok((mut stream, _)) = listener.accept() else {
                return;
            };
            let mut buffer = [0_u8; 4096];
            let _ = stream.read(&mut buffer);
            let body = "no key";
            let response = format!(
                "HTTP/1.1 401 Unauthorized\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = stream.write_all(response.as_bytes());
        }
    });
    let mock = Mock {
        base_url: format!("http://{address}"),
    };
    let output = fixture.run(&mock, &["--print", "hi"]);
    assert_eq!(output.status.code(), Some(1));
    let (_, stderr) = text(&output);
    assert!(stderr.contains("401"), "{stderr}");
    assert!(stderr.contains("no key"), "{stderr}");
}

#[test]
fn help_works_without_a_key_and_a_missing_key_is_reported() {
    let fixture = Fixture::new();
    let mock = Mock {
        base_url: "http://127.0.0.1:1".to_string(),
    };
    let help = fixture.run(&mock, &["--help"]);
    assert_eq!(help.status.code(), Some(0));
    assert!(text(&help).0.contains("Usage: dsh tui"));

    let mut command = Command::new(binary());
    let missing = command
        .args(["--print", "hi"])
        .current_dir(&fixture.workspace)
        .env("DSH_HOME", &fixture.home)
        .env_remove("DEEPSEEK_API_KEY")
        .output()
        .expect("run");
    assert_eq!(missing.status.code(), Some(1));
    assert!(text(&missing).1.contains("DEEPSEEK_API_KEY"));
    let _ = fixture.temp.path();
}

#[test]
fn the_tui_renders_and_answers_in_a_real_terminal() {
    if std::process::Command::new("tmux")
        .arg("-V")
        .output()
        .is_err()
    {
        return;
    }
    let fixture = Fixture::new();
    let mock = serve(vec![sse(&[
        r#"{"choices":[{"delta":{"content":"Hello from the mock."}}]}"#,
        r#"{"choices":[{"delta":{},"finish_reason":"stop"}]}"#,
        "[DONE]",
    ])]);
    let socket = format!("dsh-tui-{}", std::process::id());
    let session = "tui";
    let _ = Command::new("tmux")
        .args(["-L", &socket, "kill-server"])
        .output();
    let launch = format!(
        "DSH_HOME={} DEEPSEEK_API_KEY=test {} --base-url {} --api-key test-key",
        fixture.home.display(),
        binary(),
        mock.base_url
    );
    let start = Command::new("tmux")
        .args([
            "-L",
            &socket,
            "new-session",
            "-d",
            "-s",
            session,
            "-x",
            "100",
            "-y",
            "30",
            "-c",
            fixture.workspace.to_str().expect("workspace"),
            &launch,
        ])
        .output()
        .expect("tmux start");
    assert!(
        start.status.success(),
        "{}",
        String::from_utf8_lossy(&start.stderr)
    );
    let guard = TmuxGuard {
        socket: socket.clone(),
        session: session.to_string(),
    };
    let ready = wait_for_pane(&guard, "ready", Duration::from_secs(10));
    if !ready {
        let screen = capture(&guard);
        panic!("the TUI did not reach ready:\n{screen}");
    }
    let _ = Command::new("tmux")
        .args(["-L", &socket, "send-keys", "-t", session, "hello", "Enter"])
        .output();
    let answered = wait_for_pane(&guard, "Hello from the mock.", Duration::from_secs(10));
    let screen = capture(&guard);
    assert!(answered, "the answer did not render:\n{screen}");
    assert!(screen.contains("deepseek-chat"), "{screen}");
    assert!(screen.contains("session"), "{screen}");
    drop(guard);
}

struct TmuxGuard {
    socket: String,
    session: String,
}

impl Drop for TmuxGuard {
    fn drop(&mut self) {
        let _ = Command::new("tmux")
            .args(["-L", &self.socket, "send-keys", "-t", &self.session, "C-c"])
            .output();
        let _ = Command::new("tmux")
            .args(["-L", &self.socket, "kill-server"])
            .output();
    }
}

fn capture(guard: &TmuxGuard) -> String {
    let output = Command::new("tmux")
        .args([
            "-L",
            &guard.socket,
            "capture-pane",
            "-p",
            "-t",
            &guard.session,
            "-S",
            "-200",
        ])
        .output()
        .expect("capture");
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn wait_for_pane(guard: &TmuxGuard, needle: &str, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if capture(guard).contains(needle) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    false
}

#[test]
fn the_tui_asks_before_a_tool_and_runs_it_after_approval() {
    if std::process::Command::new("tmux")
        .arg("-V")
        .output()
        .is_err()
    {
        return;
    }
    let fixture = Fixture::new();
    let mock = serve(vec![
        sse(&[
            r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_1","function":{"name":"write_file","arguments":"{\"path\":\"approved.txt\",\"content\":\"yes\"}"}}]},"finish_reason":"tool_calls"}]}"#,
            "[DONE]",
        ]),
        sse(&[
            r#"{"choices":[{"delta":{"content":"Approved and written."}}]}"#,
            r#"{"choices":[{"delta":{},"finish_reason":"stop"}]}"#,
            "[DONE]",
        ]),
    ]);
    let socket = format!("dsh-tui-approval-{}", std::process::id());
    let session = "tui";
    let launch = format!(
        "DSH_HOME={} DEEPSEEK_API_KEY=test {} --base-url {} --api-key test-key",
        fixture.home.display(),
        binary(),
        mock.base_url
    );
    let start = Command::new("tmux")
        .args([
            "-L",
            &socket,
            "new-session",
            "-d",
            "-s",
            session,
            "-x",
            "100",
            "-y",
            "30",
            "-c",
            fixture.workspace.to_str().expect("workspace"),
            &launch,
        ])
        .output()
        .expect("tmux start");
    assert!(start.status.success());
    let guard = TmuxGuard {
        socket: socket.clone(),
        session: session.to_string(),
    };
    assert!(
        wait_for_pane(&guard, "ready", Duration::from_secs(10)),
        "{}",
        capture(&guard)
    );
    let _ = Command::new("tmux")
        .args([
            "-L",
            &socket,
            "send-keys",
            "-t",
            session,
            "write it",
            "Enter",
        ])
        .output();
    assert!(
        wait_for_pane(&guard, "approve tool call", Duration::from_secs(10)),
        "{}",
        capture(&guard)
    );
    let _ = Command::new("tmux")
        .args(["-L", &socket, "send-keys", "-t", session, "y"])
        .output();
    assert!(
        wait_for_pane(&guard, "Approved and written.", Duration::from_secs(10)),
        "{}",
        capture(&guard)
    );
    assert_eq!(
        fs::read_to_string(fixture.workspace.join("approved.txt")).expect("file"),
        "yes"
    );
}
