//! The agent worker thread.
//!
//! The worker owns the model client, workspace tools, session log, and turn
//! state. The terminal thread only sends commands and renders events, so a
//! long HTTP stream or shell command never blocks the UI.

use std::io::Write as _;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};

use dsh_agent::{Agent, Step, prompt::system_prompt};
use dsh_llm::LlmClient;
use dsh_session::{SessionEvent, SessionLog};
use dsh_tools::{ToolResult, Workspace};

use crate::Options;
use crate::app::WorkerEvent;

/// Commands from the terminal thread.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    /// Run a user request.
    Submit(String),
    /// Answer the pending approval.
    Approval(bool),
    /// Abandon the current turn.
    Cancel,
    /// Leave the application.
    Quit,
}

/// The command channel for a running worker.
#[derive(Debug, Clone)]
pub struct WorkerHandle {
    commands: Sender<Command>,
}

impl WorkerHandle {
    /// Send one command, ignoring a worker that already stopped.
    pub fn send(&self, command: Command) {
        let _ = self.commands.send(command);
    }
}

/// Preview length for a tool call in the transcript.
const PREVIEW_CHARS: usize = 200;

fn preview(arguments: &str) -> String {
    let mut text: String = arguments.chars().take(PREVIEW_CHARS).collect();
    if arguments.chars().count() > PREVIEW_CHARS {
        text.push('…');
    }
    text
}

/// Handle cancellation and quit while a stream is running.
///
/// The terminal thread sends commands to one channel. A turn must poll that
/// channel itself, because it owns the worker thread while the stream is open.
fn drain_commands(commands: &Receiver<Command>, cancel: &AtomicBool, quit: &AtomicBool) -> bool {
    while let Ok(command) = commands.try_recv() {
        match command {
            Command::Cancel => cancel.store(true, Ordering::Relaxed),
            Command::Quit => quit.store(true, Ordering::Relaxed),
            Command::Submit(_) | Command::Approval(_) => {}
        }
    }
    cancel.load(Ordering::Relaxed) || quit.load(Ordering::Relaxed)
}

/// Start the worker for one session.
///
/// # Errors
/// Returns when the workspace, session log, or HTTP client cannot be opened.
pub fn spawn(
    options: &Options,
    events: Sender<WorkerEvent>,
    session: SessionLog,
) -> Result<WorkerHandle, String> {
    let workspace = Workspace::new(&options.workspace)?;
    let client = LlmClient::new(dsh_llm::LlmConfig {
        base_url: options.base_url.clone(),
        api_key: options.api_key.clone(),
        model: options.model.clone(),
    })
    .map_err(|error| error.message)?;
    let (command_sender, commands) = channel();
    let worker = Worker {
        agent: Agent::new(
            system_prompt(&workspace.root().display().to_string()),
            options.max_turns,
        ),
        client,
        workspace,
        session,
        events,
        commands,
        cancel: Arc::new(AtomicBool::new(false)),
        quit: Arc::new(AtomicBool::new(false)),
        auto_approve: options.auto_approve,
    };
    std::thread::spawn(move || worker.run());
    Ok(WorkerHandle {
        commands: command_sender,
    })
}

struct Worker {
    agent: Agent,
    client: LlmClient,
    workspace: Workspace,
    session: SessionLog,
    events: Sender<WorkerEvent>,
    commands: Receiver<Command>,
    cancel: Arc<AtomicBool>,
    quit: Arc<AtomicBool>,
    auto_approve: bool,
}

impl Worker {
    fn run(mut self) {
        let _ = self.events.send(WorkerEvent::Notice(format!(
            "session saved to {}",
            self.session.path().display()
        )));
        let _ = self.session.append(&SessionEvent::Meta {
            model: self.client.config().model.clone(),
            workspace: self.workspace.root().display().to_string(),
        });
        while let Ok(command) = self.commands.recv() {
            match command {
                Command::Submit(text) => self.turn(&text),
                Command::Cancel => self.cancel.store(true, Ordering::Relaxed),
                Command::Quit => return,
                Command::Approval(_) => {}
            }
        }
    }

    fn call_model(&mut self) -> Result<Step, ()> {
        self.agent.compact(dsh_agent::DEFAULT_HISTORY_MESSAGES);
        let commands = &self.commands;
        let cancel = Arc::clone(&self.cancel);
        let quit = Arc::clone(&self.quit);
        let mut on_delta = |delta: &str| {
            drain_commands(commands, &cancel, &quit);
            let _ = self.events.send(WorkerEvent::Delta(delta.to_string()));
        };
        let result = self
            .client
            .stream_with_cancel(self.agent.messages(), &mut on_delta, &|| {
                drain_commands(commands, &cancel, &quit)
            });
        match result {
            Ok(output) => {
                let _ = self.events.send(WorkerEvent::AssistantDone);
                let _ = self.session.append(&SessionEvent::Assistant {
                    content: output.content.clone(),
                    tool_calls: output.tool_calls.clone(),
                });
                Ok(self.agent.stream_finished(output))
            }
            Err(error) if error.message == dsh_llm::CANCELLED => {
                self.agent.abort();
                let _ = self.events.send(WorkerEvent::Cancelled);
                Err(())
            }
            Err(error) => {
                self.agent.abort();
                let _ = self.session.append(&SessionEvent::Error {
                    message: error.message.clone(),
                });
                let _ = self.events.send(WorkerEvent::Error(error.message));
                Err(())
            }
        }
    }

    fn turn(&mut self, text: &str) {
        self.cancel.store(false, Ordering::Relaxed);
        if self
            .session
            .append(&SessionEvent::User {
                text: text.to_string(),
            })
            .is_err()
        {
            let _ = self.events.send(WorkerEvent::Error(
                "cannot write the session log".to_string(),
            ));
            return;
        }
        let mut step = self.agent.begin(text);
        loop {
            match step {
                Step::CallModel => match self.call_model() {
                    Ok(next) => step = next,
                    Err(()) => return,
                },
                Step::Approve(call) => {
                    if self.cancel.load(Ordering::Relaxed) {
                        self.agent.abort();
                        let _ = self.events.send(WorkerEvent::Cancelled);
                        return;
                    }
                    if self.auto_approve {
                        step = self.agent.approve(true);
                        continue;
                    }
                    let _ = self.events.send(WorkerEvent::Approval(call));
                    let approved = loop {
                        match self.commands.recv() {
                            Ok(Command::Approval(approved)) => break Some(approved),
                            Ok(Command::Cancel) => break None,
                            Ok(Command::Quit) | Err(_) => return,
                            Ok(Command::Submit(_)) => {}
                        }
                    };
                    let Some(approved) = approved else {
                        self.agent.abort();
                        let _ = self.events.send(WorkerEvent::Cancelled);
                        return;
                    };
                    step = self.agent.approve(approved);
                }
                Step::RunTool(call) => {
                    let _ = self.events.send(WorkerEvent::ToolStarted {
                        name: call.name.clone(),
                        preview: preview(&call.arguments),
                    });
                    let result = match call.invocation() {
                        Ok(invocation) => self.workspace.execute(&invocation),
                        Err(error) => ToolResult {
                            content: format!("error: {}", error.message),
                            is_error: true,
                        },
                    };
                    let _ = self.session.append(&SessionEvent::Tool {
                        call_id: call.id.clone(),
                        name: call.name.clone(),
                        content: result.content.clone(),
                        is_error: result.is_error,
                    });
                    let _ = self.events.send(WorkerEvent::ToolResult {
                        content: result.content.clone(),
                        is_error: result.is_error,
                    });
                    step = self.agent.tool_finished(result.content);
                }
                Step::Done => {
                    let _ = self.events.send(WorkerEvent::TurnDone);
                    return;
                }
                Step::Fail(message) => {
                    let _ = self.session.append(&SessionEvent::Error {
                        message: message.clone(),
                    });
                    let _ = self.events.send(WorkerEvent::Error(message));
                    return;
                }
            }
        }
    }
}

/// Run one task without a terminal, printing the streamed answer to stdout.
pub fn run_print(options: &Options, task: &str) -> i32 {
    let workspace = match Workspace::new(&options.workspace) {
        Ok(workspace) => workspace,
        Err(error) => {
            eprintln!("dsh: {error}");
            return 1;
        }
    };
    let mut session = match SessionLog::create(&options.home) {
        Ok(session) => session,
        Err(error) => {
            eprintln!("dsh: cannot create a session log: {error}");
            return 1;
        }
    };
    let client = match LlmClient::new(dsh_llm::LlmConfig {
        base_url: options.base_url.clone(),
        api_key: options.api_key.clone(),
        model: options.model.clone(),
    }) {
        Ok(client) => client,
        Err(error) => {
            eprintln!("dsh: {}", error.message);
            return 1;
        }
    };
    let mut agent = Agent::new(
        system_prompt(&workspace.root().display().to_string()),
        options.max_turns,
    );
    let _ = session.append(&SessionEvent::Meta {
        model: options.model.clone(),
        workspace: workspace.root().display().to_string(),
    });
    let _ = session.append(&SessionEvent::User {
        text: task.to_string(),
    });
    let mut step = agent.begin(task);
    loop {
        match step {
            Step::CallModel => {
                let mut on_delta = |delta: &str| {
                    print!("{delta}");
                    let _ = std::io::stdout().flush();
                };
                match client.stream(agent.messages(), &mut on_delta) {
                    Ok(output) => {
                        let _ = session.append(&SessionEvent::Assistant {
                            content: output.content.clone(),
                            tool_calls: output.tool_calls.clone(),
                        });
                        step = agent.stream_finished(output);
                    }
                    Err(error) => {
                        eprintln!("\ndsh: {}", error.message);
                        return 1;
                    }
                }
            }
            Step::Approve(call) => {
                eprintln!("dsh: running {} {}", call.name, preview(&call.arguments));
                step = agent.approve(true);
            }
            Step::RunTool(call) => {
                let result = match call.invocation() {
                    Ok(invocation) => workspace.execute(&invocation),
                    Err(error) => ToolResult {
                        content: format!("error: {}", error.message),
                        is_error: true,
                    },
                };
                let _ = session.append(&SessionEvent::Tool {
                    call_id: call.id.clone(),
                    name: call.name.clone(),
                    content: result.content.clone(),
                    is_error: result.is_error,
                });
                if result.content.is_empty() {
                    eprintln!("dsh: {} finished", call.name);
                } else {
                    eprintln!("dsh: {} ->\n{}", call.name, result.content);
                }
                step = agent.tool_finished(result.content);
            }
            Step::Done => {
                println!();
                return 0;
            }
            Step::Fail(message) => {
                eprintln!("dsh: {message}");
                return 1;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::preview;

    #[test]
    fn a_preview_is_bounded() {
        assert_eq!(preview("abc"), "abc");
        let long = "x".repeat(500);
        let shortened = preview(&long);
        assert_eq!(shortened.chars().count(), super::PREVIEW_CHARS + 1);
        assert!(shortened.ends_with('…'));
    }
}

#[cfg(test)]
mod cancel_tests {
    use super::{Command, spawn};
    use crate::Options;
    use crate::app::WorkerEvent;
    use dsh_session::SessionLog;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::mpsc::channel;
    use std::time::{Duration, Instant};

    #[test]
    fn preview_boundaries_keep_exactly_the_limit() {
        assert_eq!(super::preview(&"x".repeat(200)), "x".repeat(200));
        assert_eq!(super::preview(&"x".repeat(201)).chars().count(), 201);
    }

    #[test]
    fn cancelling_a_live_stream_reports_cancelled() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let address = listener.local_addr().expect("addr");
        std::thread::spawn(move || {
            let Ok((mut stream, _)) = listener.accept() else {
                return;
            };
            let mut buffer = [0_u8; 4096];
            let mut request = Vec::new();
            loop {
                let Ok(read) = stream.read(&mut buffer) else {
                    return;
                };
                if read == 0 {
                    return;
                }
                request.extend_from_slice(&buffer[..read]);
                if request.windows(4).any(|window| window == b"\r\n\r\n") {
                    break;
                }
            }
            let body =
                "data: {\"choices\":[{\"delta\":{\"content\":\"one\"}}]}\n\ndata: [DONE]\n\n";
            let headers = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            let _ = stream.write_all(headers.as_bytes());
            let _ =
                stream.write_all(b"data: {\"choices\":[{\"delta\":{\"content\":\"one\"}}]}\n\n");
            let _ = stream.flush();
            std::thread::sleep(Duration::from_millis(300));
            let _ = stream.write_all(b"data: [DONE]\n\n");
        });
        let home = tempfile::tempdir().expect("home");
        let workspace = tempfile::tempdir().expect("workspace");
        let options = Options {
            home: home.path().to_path_buf(),
            workspace: workspace.path().to_path_buf(),
            model: "m".to_string(),
            base_url: format!("http://{address}"),
            api_key: "k".to_string(),
            auto_approve: false,
            print: None,
            max_turns: 4,
            initial_prompt: None,
        };
        let session = SessionLog::create(home.path()).expect("session");
        let (sender, receiver) = channel();
        let handle = spawn(&options, sender, session).expect("worker");
        handle.send(Command::Submit("hi".to_string()));
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut saw_delta = false;
        while Instant::now() < deadline {
            let Ok(event) = receiver.recv_timeout(Duration::from_millis(200)) else {
                continue;
            };
            match event {
                WorkerEvent::Delta(_) => {
                    saw_delta = true;
                    handle.send(Command::Cancel);
                }
                WorkerEvent::Cancelled => {
                    assert!(saw_delta, "cancel must follow streamed output");
                    handle.send(Command::Quit);
                    return;
                }
                WorkerEvent::Error(message) => panic!("expected cancel, got {message}"),
                _ => {}
            }
        }
        panic!("the worker never reported cancellation");
    }
}

#[cfg(test)]
mod provider_error_tests {
    use super::{Command, spawn};
    use crate::Options;
    use crate::app::WorkerEvent;
    use dsh_session::SessionLog;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::mpsc::channel;
    use std::time::{Duration, Instant};

    #[test]
    fn a_provider_error_is_an_error_not_a_cancellation() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let address = listener.local_addr().expect("addr");
        std::thread::spawn(move || {
            let Ok((mut stream, _)) = listener.accept() else {
                return;
            };
            let mut buffer = [0_u8; 4096];
            loop {
                let Ok(read) = stream.read(&mut buffer) else {
                    return;
                };
                if read == 0 {
                    return;
                }
                if buffer[..read]
                    .windows(4)
                    .any(|window| window == b"\r\n\r\n")
                {
                    break;
                }
            }
            let body = "denied";
            let response = format!(
                "HTTP/1.1 401 Unauthorized\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = stream.write_all(response.as_bytes());
        });
        let home = tempfile::tempdir().expect("home");
        let workspace = tempfile::tempdir().expect("workspace");
        let options = Options {
            home: home.path().to_path_buf(),
            workspace: workspace.path().to_path_buf(),
            model: "m".to_string(),
            base_url: format!("http://{address}"),
            api_key: "k".to_string(),
            auto_approve: false,
            print: None,
            max_turns: 4,
            initial_prompt: None,
        };
        let session = SessionLog::create(home.path()).expect("session");
        let (sender, receiver) = channel();
        let handle = spawn(&options, sender, session).expect("worker");
        handle.send(Command::Submit("hi".to_string()));
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            let Ok(event) = receiver.recv_timeout(Duration::from_millis(200)) else {
                continue;
            };
            match event {
                WorkerEvent::Error(message) => {
                    assert!(message.contains("401"), "{message}");
                    handle.send(Command::Quit);
                    return;
                }
                WorkerEvent::Cancelled => panic!("a provider error is not a cancellation"),
                _ => {}
            }
        }
        panic!("the worker never reported the provider error");
    }
}
