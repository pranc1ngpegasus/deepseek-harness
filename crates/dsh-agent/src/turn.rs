//! The turn state machine.
//!
//! One user request runs model calls and tool calls in a loop. This machine
//! decides which step comes next and keeps the message history the next model
//! call needs. It never performs I/O, so the runtime can pause at any step for
//! approval or cancellation.

use alloc::collections::VecDeque;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use crate::message::{Message, ToolCall};
use crate::stream::TurnOutput;

/// How many model calls one user request may make by default.
pub const DEFAULT_MAX_MODEL_TURNS: u32 = 32;

/// How many history messages one model call keeps before dropping the oldest.
pub const DEFAULT_HISTORY_MESSAGES: usize = 200;

/// Where the agent is in the current turn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    /// Waiting for a user request.
    Idle,
    /// The model stream is running.
    Streaming,
    /// A tool call waits for the operator.
    AwaitingApproval,
    /// An approved tool is running.
    RunningTool,
    /// The reply is complete.
    Done,
    /// The turn stopped on an error or the turn limit.
    Failed,
}

/// The next thing the runtime must do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Step {
    /// Call the model with the current history.
    CallModel,
    /// Ask the operator to approve this call.
    Approve(ToolCall),
    /// Run this approved call.
    RunTool(ToolCall),
    /// The reply is complete.
    Done,
    /// Stop with this diagnostic.
    Fail(String),
}

/// The coding agent's conversation and turn state.
#[derive(Debug)]
pub struct Agent {
    messages: Vec<Message>,
    phase: Phase,
    calls: VecDeque<ToolCall>,
    current: Option<ToolCall>,
    model_turns: u32,
    max_model_turns: u32,
}

impl Agent {
    /// Start a conversation with one system message.
    #[must_use]
    pub fn new(system: String, max_model_turns: u32) -> Self {
        Self {
            messages: alloc::vec![Message::system(system)],
            phase: Phase::Idle,
            calls: VecDeque::new(),
            current: None,
            model_turns: 0,
            max_model_turns: max_model_turns.max(1),
        }
    }

    /// The history to send to the model.
    #[must_use]
    pub fn messages(&self) -> &[Message] {
        &self.messages
    }

    /// Where the turn stands.
    #[must_use]
    pub const fn phase(&self) -> Phase {
        self.phase
    }

    /// Begin a user request.
    pub fn begin(&mut self, user: &str) -> Step {
        if !matches!(self.phase, Phase::Idle | Phase::Done | Phase::Failed) {
            return self.fail("a turn is already in progress");
        }
        self.messages.push(Message::user(user));
        self.calls.clear();
        self.current = None;
        self.model_turns = 1;
        self.phase = Phase::Streaming;
        Step::CallModel
    }

    /// Record the completed model turn and decide the next step.
    pub fn stream_finished(&mut self, output: TurnOutput) -> Step {
        if self.phase != Phase::Streaming {
            return self.fail("no model stream is in progress");
        }
        self.messages.push(Message::assistant(
            output.content,
            output.tool_calls.clone(),
        ));
        if output.tool_calls.is_empty() {
            self.phase = Phase::Done;
            return Step::Done;
        }
        self.calls = output.tool_calls.into_iter().collect();
        self.next_call()
    }

    /// Record the operator's decision on the current call.
    pub fn approve(&mut self, approved: bool) -> Step {
        if self.phase != Phase::AwaitingApproval {
            return self.fail("no tool call is awaiting approval");
        }
        let Some(call) = self.current.take() else {
            return self.fail("the approved tool call is missing");
        };
        if !approved {
            self.messages
                .push(Message::tool(call.id, "Tool call denied by the user."));
            return self.next_call();
        }
        self.phase = Phase::RunningTool;
        self.current = Some(call.clone());
        Step::RunTool(call)
    }

    /// Record a finished tool call and decide the next step.
    pub fn tool_finished(&mut self, content: String) -> Step {
        if self.phase != Phase::RunningTool {
            return self.fail("no tool call is running");
        }
        let Some(call) = self.current.take() else {
            return self.fail("the running tool call is missing");
        };
        self.messages.push(Message::tool(call.id, content));
        self.next_call()
    }

    /// Abandon the current turn without recording a model reply.
    pub fn abort(&mut self) {
        self.phase = Phase::Idle;
        self.calls.clear();
        self.current = None;
    }

    /// Keep the system message and the newest `keep` messages.
    ///
    /// A window that starts on a tool result is advanced past it, because a
    /// result without its assistant call is rejected by the provider. The
    /// history is left untouched when it already fits.
    pub fn compact(&mut self, keep: usize) {
        if keep == 0 || self.messages.len() <= keep.saturating_add(1) {
            return;
        }
        let start = self.messages.len() - keep;
        let mut window: Vec<Message> = self.messages[start..].to_vec();
        while window
            .first()
            .is_some_and(|message| message.role == crate::message::Role::Tool)
        {
            window.remove(0);
        }
        let mut compacted = Vec::with_capacity(window.len() + 1);
        compacted.push(self.messages[0].clone());
        compacted.extend(window);
        self.messages = compacted;
    }

    fn next_call(&mut self) -> Step {
        if let Some(call) = self.calls.pop_front() {
            self.current = Some(call.clone());
            self.phase = Phase::AwaitingApproval;
            return Step::Approve(call);
        }
        self.model_turns += 1;
        if self.model_turns > self.max_model_turns {
            return self.fail(&format!(
                "agent stopped after {} model turns",
                self.max_model_turns
            ));
        }
        self.phase = Phase::Streaming;
        Step::CallModel
    }

    fn fail(&mut self, message: &str) -> Step {
        self.phase = Phase::Failed;
        Step::Fail(message.into())
    }
}

#[cfg(test)]
mod tests {
    use super::{Agent, Phase, Step};
    use crate::message::{Message, Role, ToolCall};
    use crate::stream::TurnOutput;
    use alloc::string::ToString;
    use alloc::vec;
    use alloc::vec::Vec;

    fn agent() -> Agent {
        Agent::new("system".to_string(), 4)
    }

    fn call(id: &str, name: &str) -> ToolCall {
        ToolCall {
            id: id.to_string(),
            name: name.to_string(),
            arguments: "{}".to_string(),
        }
    }

    fn output(content: &str, calls: Vec<ToolCall>) -> TurnOutput {
        TurnOutput {
            content: content.to_string(),
            tool_calls: calls,
            finish_reason: Some("stop".to_string()),
            usage: None,
        }
    }

    #[test]
    fn a_plain_reply_finishes_after_one_model_call() {
        let mut agent = agent();
        assert_eq!(agent.begin("hi"), Step::CallModel);
        assert_eq!(agent.phase(), Phase::Streaming);
        assert_eq!(agent.stream_finished(output("hello", vec![])), Step::Done);
        assert_eq!(agent.phase(), Phase::Done);
        assert_eq!(agent.messages().len(), 3);
        assert_eq!(agent.messages()[1].role, Role::User);
        assert_eq!(agent.messages()[2].content, "hello");
    }

    #[test]
    fn a_tool_call_pauses_for_approval_then_runs() {
        let mut agent = agent();
        agent.begin("write");
        assert_eq!(
            agent.stream_finished(output("", vec![call("c1", "write_file")])),
            Step::Approve(call("c1", "write_file"))
        );
        assert_eq!(agent.phase(), Phase::AwaitingApproval);
        assert_eq!(agent.approve(true), Step::RunTool(call("c1", "write_file")));
        assert_eq!(agent.phase(), Phase::RunningTool);
        assert_eq!(agent.tool_finished("written".to_string()), Step::CallModel);
        assert_eq!(agent.phase(), Phase::Streaming);
        assert_eq!(agent.messages().last().expect("tool").role, Role::Tool);
        assert_eq!(
            agent
                .messages()
                .last()
                .expect("tool")
                .tool_call_id
                .as_deref(),
            Some("c1")
        );
        assert_eq!(agent.stream_finished(output("done", vec![])), Step::Done);
    }

    #[test]
    fn several_tool_calls_run_in_index_order() {
        let mut agent = agent();
        agent.begin("work");
        assert_eq!(
            agent.stream_finished(output("", vec![call("a", "bash"), call("b", "read_file")])),
            Step::Approve(call("a", "bash"))
        );
        assert_eq!(agent.approve(true), Step::RunTool(call("a", "bash")));
        assert_eq!(
            agent.tool_finished("one".to_string()),
            Step::Approve(call("b", "read_file"))
        );
        assert_eq!(agent.approve(true), Step::RunTool(call("b", "read_file")));
        assert_eq!(agent.tool_finished("two".to_string()), Step::CallModel);
    }

    #[test]
    fn a_denied_call_is_reported_to_the_model_and_the_next_call_proceeds() {
        let mut agent = agent();
        agent.begin("danger");
        agent.stream_finished(output("", vec![call("a", "bash"), call("b", "bash")]));
        assert_eq!(agent.approve(false), Step::Approve(call("b", "bash")));
        let denial = agent.messages().last().expect("denial");
        assert_eq!(denial.content, "Tool call denied by the user.");
        assert_eq!(denial.tool_call_id.as_deref(), Some("a"));
        assert_eq!(agent.phase(), Phase::AwaitingApproval);
        assert_eq!(agent.approve(false), Step::CallModel);
    }

    #[test]
    fn the_model_turn_limit_fails_the_turn() {
        let mut agent = Agent::new("system".to_string(), 1);
        agent.begin("loop");
        agent.stream_finished(output("", vec![call("a", "bash")]));
        agent.approve(true);
        assert_eq!(
            agent.tool_finished("one".to_string()),
            Step::Fail("agent stopped after 1 model turns".to_string())
        );
        assert_eq!(agent.phase(), Phase::Failed);
    }

    #[test]
    fn a_turn_may_use_its_full_model_budget() {
        let mut agent = Agent::new("system".to_string(), 2);
        agent.begin("work");
        agent.stream_finished(output("", vec![call("a", "read_file")]));
        agent.approve(true);
        assert_eq!(agent.tool_finished("one".to_string()), Step::CallModel);
        assert_eq!(agent.stream_finished(output("done", vec![])), Step::Done);
    }

    #[test]
    fn unexpected_transitions_fail_instead_of_panicking() {
        let mut busy = agent();
        assert_eq!(busy.begin("one"), Step::CallModel);
        assert_eq!(
            busy.begin("second"),
            Step::Fail("a turn is already in progress".to_string())
        );

        let mut agent = agent();
        assert_eq!(
            agent.stream_finished(output("x", vec![])),
            Step::Fail("no model stream is in progress".to_string())
        );
        assert_eq!(
            agent.approve(true),
            Step::Fail("no tool call is awaiting approval".to_string())
        );
        assert_eq!(
            agent.tool_finished("x".to_string()),
            Step::Fail("no tool call is running".to_string())
        );
        assert_eq!(agent.begin("after failure"), Step::CallModel);
    }

    #[test]
    fn a_new_request_starts_after_completion_or_failure() {
        let mut agent = agent();
        agent.begin("one");
        agent.stream_finished(output("done", vec![]));
        assert_eq!(agent.begin("two"), Step::CallModel);
        assert_eq!(agent.messages()[1].content, "one");
        assert_eq!(agent.messages()[3].content, "two");
    }

    #[test]
    fn abort_returns_to_idle_and_drops_pending_calls() {
        let mut agent = agent();
        agent.begin("x");
        agent.stream_finished(output("", vec![call("a", "bash")]));
        agent.abort();
        assert_eq!(agent.phase(), Phase::Idle);
        assert_eq!(agent.begin("again"), Step::CallModel);
        assert_eq!(agent.messages().last().expect("user").role, Role::User);
    }

    #[test]
    fn the_system_message_is_always_first() {
        let agent = agent();
        assert_eq!(agent.messages().len(), 1);
        assert_eq!(agent.messages()[0].role, Role::System);
        assert_eq!(agent.messages()[0].content, "system");
        assert_eq!(Message::system("x").to_json()["role"], "system");
    }

    #[test]
    fn compaction_keeps_the_system_message_and_the_newest_window() {
        let mut agent = agent();
        agent.begin("first");
        agent.stream_finished(output("one", vec![]));
        agent.begin("second");
        agent.stream_finished(output("two", vec![]));
        let all = agent.messages().len();
        agent.compact(2);
        assert_eq!(agent.messages().len(), 3);
        assert_eq!(agent.messages()[0].content, "system");
        assert_eq!(agent.messages()[1].content, "second");
        assert_eq!(agent.messages()[2].content, "two");
        agent.compact(all);
        assert_eq!(agent.messages().len(), 3);
        agent.compact(0);
        assert_eq!(agent.messages().len(), 3);
    }

    #[test]
    fn compaction_never_starts_the_window_on_a_tool_result() {
        let mut agent = agent();
        agent.begin("work");
        agent.stream_finished(output("", vec![call("a", "bash")]));
        agent.approve(true);
        agent.tool_finished("result".to_string());
        agent.stream_finished(output("done", vec![]));
        assert_eq!(agent.messages().last().expect("last").role, Role::Assistant);
        agent.compact(3);
        assert_eq!(agent.messages()[0].role, Role::System);
        assert_ne!(agent.messages()[1].role, Role::Tool);
    }

    #[test]
    fn a_zero_turn_limit_is_lifted_to_one() {
        let mut agent = Agent::new("s".to_string(), 0);
        agent.begin("x");
        agent.stream_finished(output("", vec![call("a", "bash")]));
        agent.approve(true);
        assert_eq!(
            agent.tool_finished("done".to_string()),
            Step::Fail("agent stopped after 1 model turns".to_string())
        );
    }
}
