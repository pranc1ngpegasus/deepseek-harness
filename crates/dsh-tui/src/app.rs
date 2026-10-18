//! TUI state and key handling.
//!
//! The app never performs I/O. It turns worker events and terminal keys into
//! the next command, so its behavior is testable without a terminal.

use dsh_agent::ToolCall;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

/// One conversation entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Item {
    /// A person's request.
    User(String),
    /// The model's reply.
    Assistant(String),
    /// A tool the agent is running.
    ToolCall {
        /// Tool name.
        name: String,
        /// One-line argument preview.
        preview: String,
    },
    /// A tool outcome.
    ToolResult {
        /// Result text.
        content: String,
        /// Whether the tool failed.
        is_error: bool,
    },
    /// Launcher feedback such as the session path.
    Notice(String),
    /// A failure.
    Error(String),
}

/// What the session is doing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    /// Waiting for a request.
    Ready,
    /// The request was sent and the first token has not arrived.
    Thinking,
    /// Assistant text is streaming.
    Streaming,
    /// A tool call waits for the operator.
    AwaitingApproval,
    /// A tool is running.
    RunningTool,
}

impl Status {
    /// The status-line label.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Ready => "ready",
            Self::Thinking => "thinking",
            Self::Streaming => "streaming",
            Self::AwaitingApproval => "approval needed",
            Self::RunningTool => "running tool",
        }
    }

    /// Whether a request is in flight.
    #[must_use]
    pub const fn is_busy(self) -> bool {
        !matches!(self, Self::Ready)
    }
}

/// The command the app wants the runtime to perform.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// Do nothing.
    None,
    /// Send a user request.
    Submit(String),
    /// Answer the pending approval.
    Approval(bool),
    /// Abandon the current turn.
    Cancel,
    /// Leave the application.
    Quit,
}

/// Events the worker sends to the app.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkerEvent {
    /// Assistant text fragment.
    Delta(String),
    /// The assistant finished its text and calls.
    AssistantDone,
    /// A call needs the operator's decision.
    Approval(ToolCall),
    /// A tool started.
    ToolStarted {
        /// Tool name.
        name: String,
        /// Argument preview.
        preview: String,
    },
    /// A tool finished.
    ToolResult {
        /// Result text.
        content: String,
        /// Whether it failed.
        is_error: bool,
    },
    /// The turn finished.
    TurnDone,
    /// The turn or session failed.
    Error(String),
    /// Launcher feedback.
    Notice(String),
    /// The operator cancelled the turn.
    Cancelled,
}

/// Everything the screen shows.
#[derive(Debug)]
pub struct App {
    items: Vec<Item>,
    input: String,
    status: Status,
    approval: Option<ToolCall>,
    session: String,
    model: String,
    scroll: u16,
    follow: bool,
    quit: bool,
}

impl App {
    /// A ready app.
    #[must_use]
    pub fn new(model: impl Into<String>, session: impl Into<String>) -> Self {
        Self {
            items: Vec::new(),
            input: String::new(),
            status: Status::Ready,
            approval: None,
            session: session.into(),
            model: model.into(),
            scroll: 0,
            follow: true,
            quit: false,
        }
    }

    /// The conversation.
    #[must_use]
    pub fn items(&self) -> &[Item] {
        &self.items
    }

    /// The current input.
    #[must_use]
    pub fn input(&self) -> &str {
        &self.input
    }

    /// The current status.
    #[must_use]
    pub const fn status(&self) -> Status {
        self.status
    }

    /// The pending approval, when one exists.
    #[must_use]
    pub const fn approval(&self) -> Option<&ToolCall> {
        self.approval.as_ref()
    }

    /// The model name.
    #[must_use]
    pub fn model(&self) -> &str {
        &self.model
    }

    /// The session id.
    #[must_use]
    pub fn session(&self) -> &str {
        &self.session
    }

    /// The top visible line.
    #[must_use]
    pub const fn scroll(&self) -> u16 {
        self.scroll
    }

    /// Whether the view follows new output.
    #[must_use]
    pub const fn follow(&self) -> bool {
        self.follow
    }

    /// Whether the app asked to leave.
    #[must_use]
    pub const fn should_quit(&self) -> bool {
        self.quit
    }

    /// Update the viewport-derived scroll offset during rendering.
    pub fn set_viewport(&mut self, total_lines: u16, viewport: u16) {
        let maximum = total_lines.saturating_sub(viewport);
        if self.follow {
            self.scroll = maximum;
        } else {
            self.scroll = self.scroll.min(maximum);
        }
    }

    /// Apply one worker event.
    pub fn apply(&mut self, event: WorkerEvent) -> Action {
        match event {
            WorkerEvent::Delta(text) => {
                self.status = Status::Streaming;
                match self.items.last_mut() {
                    Some(Item::Assistant(content)) => content.push_str(&text),
                    _ => self.items.push(Item::Assistant(text)),
                }
                self.follow = true;
                Action::None
            }
            WorkerEvent::AssistantDone => {
                if let Some(Item::Assistant(content)) = self.items.last_mut()
                    && content.is_empty()
                {
                    content.push_str("(empty reply)");
                }
                Action::None
            }
            WorkerEvent::Approval(call) => {
                self.status = Status::AwaitingApproval;
                self.approval = Some(call);
                self.follow = true;
                Action::None
            }
            WorkerEvent::ToolStarted { name, preview } => {
                self.status = Status::RunningTool;
                self.approval = None;
                self.items.push(Item::ToolCall { name, preview });
                self.follow = true;
                Action::None
            }
            WorkerEvent::ToolResult { content, is_error } => {
                self.items.push(Item::ToolResult { content, is_error });
                self.approval = None;
                self.follow = true;
                Action::None
            }
            WorkerEvent::TurnDone => {
                self.status = Status::Ready;
                self.approval = None;
                self.follow = true;
                Action::None
            }
            WorkerEvent::Error(message) => {
                self.status = Status::Ready;
                self.approval = None;
                self.items.push(Item::Error(message));
                self.follow = true;
                Action::None
            }
            WorkerEvent::Notice(message) => {
                self.items.push(Item::Notice(message));
                Action::None
            }
            WorkerEvent::Cancelled => {
                self.status = Status::Ready;
                self.approval = None;
                self.items.push(Item::Notice("turn cancelled".to_string()));
                Action::None
            }
        }
    }

    /// Submit a request from outside the key handler, such as a startup prompt.
    pub fn submit(&mut self, text: &str) -> Action {
        if self.status != Status::Ready {
            return Action::None;
        }
        let text = text.trim().to_string();
        if text.is_empty() {
            return Action::None;
        }
        self.input.clear();
        self.items.push(Item::User(text.clone()));
        self.status = Status::Thinking;
        self.follow = true;
        Action::Submit(text)
    }

    /// Handle one terminal key.
    pub fn handle_key(&mut self, key: KeyEvent) -> Action {
        if key.modifiers.contains(KeyModifiers::CONTROL)
            && matches!(key.code, KeyCode::Char('c' | 'C'))
        {
            return if self.status.is_busy() {
                Action::Cancel
            } else {
                self.quit = true;
                Action::Quit
            };
        }
        if key.modifiers.contains(KeyModifiers::CONTROL)
            && matches!(key.code, KeyCode::Char('d' | 'D'))
        {
            self.quit = true;
            return Action::Quit;
        }
        if let Some(_call) = &self.approval {
            match key.code {
                KeyCode::Char('y' | 'Y') => {
                    return Action::Approval(true);
                }
                KeyCode::Char('n' | 'N') | KeyCode::Esc => {
                    return Action::Approval(false);
                }
                _ => return Action::None,
            }
        }
        match key.code {
            KeyCode::Esc if self.status.is_busy() => Action::Cancel,
            KeyCode::Enter => self.submit(&self.input.clone()),
            KeyCode::Backspace if self.status == Status::Ready => {
                self.input.pop();
                Action::None
            }
            KeyCode::Char(character)
                if self.status == Status::Ready
                    && !key.modifiers.contains(KeyModifiers::CONTROL) =>
            {
                self.input.push(character);
                Action::None
            }
            KeyCode::Up => {
                self.follow = false;
                self.scroll = self.scroll.saturating_sub(1);
                Action::None
            }
            KeyCode::Down => {
                self.follow = false;
                self.scroll = self.scroll.saturating_add(1);
                Action::None
            }
            KeyCode::PageUp => {
                self.follow = false;
                self.scroll = self.scroll.saturating_sub(10);
                Action::None
            }
            KeyCode::PageDown => {
                self.follow = false;
                self.scroll = self.scroll.saturating_add(10);
                Action::None
            }
            KeyCode::Home => {
                self.follow = false;
                self.scroll = 0;
                Action::None
            }
            KeyCode::End => {
                self.follow = true;
                Action::None
            }
            _ => Action::None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Action, App, Item, Status, WorkerEvent};
    use dsh_agent::ToolCall;
    use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn ctrl(character: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(character), KeyModifiers::CONTROL)
    }

    fn app() -> App {
        App::new("model", "session")
    }

    fn call() -> ToolCall {
        ToolCall {
            id: "c".to_string(),
            name: "bash".to_string(),
            arguments: "{\"command\":\"ls\"}".to_string(),
        }
    }

    #[test]
    fn typing_then_enter_submits_and_clears_the_input() {
        let mut app = app();
        for character in "hello".chars() {
            assert_eq!(app.handle_key(key(KeyCode::Char(character))), Action::None);
        }
        assert_eq!(app.input(), "hello");
        assert_eq!(
            app.handle_key(key(KeyCode::Enter)),
            Action::Submit("hello".into())
        );
        assert_eq!(app.input(), "");
        assert_eq!(app.status(), Status::Thinking);
        assert_eq!(app.items().last(), Some(&Item::User("hello".into())));
    }

    #[test]
    fn blank_input_is_not_submitted() {
        let mut app = app();
        app.handle_key(key(KeyCode::Char(' ')));
        assert_eq!(app.handle_key(key(KeyCode::Enter)), Action::None);
        assert_eq!(app.status(), Status::Ready);
    }

    #[test]
    fn input_is_ignored_while_busy() {
        let mut app = app();
        app.handle_key(key(KeyCode::Char('a')));
        app.handle_key(key(KeyCode::Enter));
        assert_eq!(app.handle_key(key(KeyCode::Char('b'))), Action::None);
        assert_eq!(app.input(), "");
    }

    #[test]
    fn backspace_removes_one_character() {
        let mut app = app();
        app.handle_key(key(KeyCode::Char('a')));
        app.handle_key(key(KeyCode::Char('b')));
        app.handle_key(key(KeyCode::Backspace));
        assert_eq!(app.input(), "a");
        app.handle_key(key(KeyCode::Backspace));
        app.handle_key(key(KeyCode::Backspace));
        assert_eq!(app.input(), "");
    }

    #[test]
    fn worker_events_build_the_transcript() {
        let mut app = app();
        app.apply(WorkerEvent::Delta("Hel".into()));
        app.apply(WorkerEvent::Delta("lo".into()));
        assert_eq!(app.status(), Status::Streaming);
        assert_eq!(app.items(), &[Item::Assistant("Hello".into())]);
        app.apply(WorkerEvent::AssistantDone);
        app.apply(WorkerEvent::TurnDone);
        assert_eq!(app.status(), Status::Ready);
        app.apply(WorkerEvent::ToolStarted {
            name: "bash".into(),
            preview: "ls".into(),
        });
        app.apply(WorkerEvent::ToolResult {
            content: "ok".into(),
            is_error: false,
        });
        assert!(matches!(app.items()[1], Item::ToolCall { .. }));
        assert!(matches!(app.items()[2], Item::ToolResult { .. }));
    }

    #[test]
    fn an_empty_reply_gets_a_placeholder() {
        let mut app = app();
        app.apply(WorkerEvent::Delta("x".into()));
        app.apply(WorkerEvent::AssistantDone);
        assert_eq!(app.items().last(), Some(&Item::Assistant("x".into())));
    }

    #[test]
    fn approval_keys_answer_the_pending_call() {
        let mut app = app();
        app.apply(WorkerEvent::Approval(call()));
        assert_eq!(app.status(), Status::AwaitingApproval);
        assert_eq!(
            app.handle_key(key(KeyCode::Char('y'))),
            Action::Approval(true)
        );
        assert_eq!(app.handle_key(key(KeyCode::Char('x'))), Action::None);
        app.apply(WorkerEvent::Approval(call()));
        assert_eq!(
            app.handle_key(key(KeyCode::Char('n'))),
            Action::Approval(false)
        );
        app.apply(WorkerEvent::Approval(call()));
        assert_eq!(app.handle_key(key(KeyCode::Esc)), Action::Approval(false));
        app.apply(WorkerEvent::Approval(call()));
        assert_eq!(app.handle_key(key(KeyCode::Enter)), Action::None);
    }

    #[test]
    fn ctrl_c_cancels_a_busy_turn_and_quits_when_ready() {
        let mut app = app();
        assert_eq!(app.handle_key(ctrl('c')), Action::Quit);
        assert!(app.should_quit());
        let mut app = self::app();
        app.apply(WorkerEvent::Delta("x".into()));
        assert_eq!(app.handle_key(ctrl('c')), Action::Cancel);
        assert!(!app.should_quit());
        assert_eq!(app.handle_key(ctrl('d')), Action::Quit);
    }

    #[test]
    fn escape_cancels_only_a_busy_turn() {
        let mut app = app();
        assert_eq!(app.handle_key(key(KeyCode::Esc)), Action::None);
        app.apply(WorkerEvent::Delta("x".into()));
        assert_eq!(app.handle_key(key(KeyCode::Esc)), Action::Cancel);
    }

    #[test]
    fn scrolling_stops_following_and_end_resumes() {
        let mut app = app();
        assert!(app.follow());
        app.handle_key(key(KeyCode::Up));
        assert!(!app.follow());
        app.handle_key(key(KeyCode::PageUp));
        app.handle_key(key(KeyCode::Down));
        app.handle_key(key(KeyCode::PageDown));
        app.handle_key(key(KeyCode::Home));
        assert_eq!(app.scroll(), 0);
        app.handle_key(key(KeyCode::End));
        assert!(app.follow());
    }

    #[test]
    fn the_viewport_clamps_or_follows_the_scroll() {
        let mut app = app();
        app.set_viewport(100, 20);
        assert_eq!(app.scroll(), 80);
        app.handle_key(key(KeyCode::Home));
        app.set_viewport(100, 20);
        assert_eq!(app.scroll(), 0);
        app.handle_key(key(KeyCode::Down));
        app.set_viewport(10, 20);
        assert_eq!(app.scroll(), 0);
        app.apply(WorkerEvent::TurnDone);
        app.set_viewport(100, 20);
        assert_eq!(app.scroll(), 80);
    }

    #[test]
    fn errors_and_notices_are_recorded_and_reset_the_status() {
        let mut app = app();
        app.apply(WorkerEvent::Delta("x".into()));
        app.apply(WorkerEvent::Error("boom".into()));
        assert_eq!(app.status(), Status::Ready);
        assert_eq!(app.items().last(), Some(&Item::Error("boom".into())));
        app.apply(WorkerEvent::Notice("session saved".into()));
        assert_eq!(
            app.items().last(),
            Some(&Item::Notice("session saved".into()))
        );
        app.apply(WorkerEvent::Cancelled);
        assert_eq!(
            app.items().last(),
            Some(&Item::Notice("turn cancelled".into()))
        );
        assert_eq!(app.approval(), None);
    }
}

#[cfg(test)]
mod navigation_tests {
    use super::{Action, App, Status, WorkerEvent};
    use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    #[test]
    fn each_scroll_key_moves_by_its_documented_step() {
        let mut app = App::new("m", "s");
        app.set_viewport(100, 20);
        assert_eq!(app.scroll(), 80);
        app.handle_key(key(KeyCode::Up));
        assert_eq!(app.scroll(), 79);
        app.handle_key(key(KeyCode::Down));
        assert_eq!(app.scroll(), 80);
        app.handle_key(key(KeyCode::PageUp));
        assert_eq!(app.scroll(), 70);
        app.handle_key(key(KeyCode::PageDown));
        assert_eq!(app.scroll(), 80);
    }

    #[test]
    fn a_busy_app_ignores_editing_and_submission_even_with_text() {
        let mut app = App::new("m", "s");
        app.apply(WorkerEvent::Delta("x".into()));
        app.input.push('a');
        let items = app.items().len();
        assert_eq!(app.handle_key(key(KeyCode::Backspace)), Action::None);
        assert_eq!(app.input(), "a");
        assert_eq!(app.handle_key(key(KeyCode::Enter)), Action::None);
        assert_eq!(app.items().len(), items);
        assert_eq!(app.status(), Status::Streaming);
    }

    #[test]
    fn control_modified_characters_are_not_typed() {
        let mut app = App::new("m", "s");
        assert_eq!(
            app.handle_key(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::CONTROL)),
            Action::None
        );
        assert_eq!(app.input(), "");
    }
}
