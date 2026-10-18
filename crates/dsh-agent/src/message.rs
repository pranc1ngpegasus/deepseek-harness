//! Chat messages in the shape a chat-completions endpoint accepts.

use alloc::string::{String, ToString};
use alloc::vec::Vec;
use serde_json::{Value, json};

use crate::tools::{ToolError, ToolInvocation, parse_invocation};

/// Who produced one message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    /// The agent's standing instructions.
    System,
    /// A person's request.
    User,
    /// The model's reply.
    Assistant,
    /// The result of a tool the model called.
    Tool,
}

impl Role {
    /// The wire spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::System => "system",
            Self::User => "user",
            Self::Assistant => "assistant",
            Self::Tool => "tool",
        }
    }
}

/// One model tool call: the id the result must reference, the tool name, and
/// the raw JSON argument text exactly as the model produced it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolCall {
    /// Correlation id echoed in the tool result message.
    pub id: String,
    /// The tool to run.
    pub name: String,
    /// The raw JSON arguments.
    pub arguments: String,
}

impl ToolCall {
    /// Parse and validate this call's arguments.
    ///
    /// # Errors
    /// Returns the tool name, malformed JSON, or the field that failed
    /// validation.
    pub fn invocation(&self) -> Result<ToolInvocation, ToolError> {
        parse_invocation(&self.name, &self.arguments)
    }

    /// The wire representation used in an assistant message.
    #[must_use]
    pub fn to_json(&self) -> Value {
        json!({
            "id": self.id,
            "type": "function",
            "function": {
                "name": self.name,
                "arguments": self.arguments,
            }
        })
    }
}

/// One chat message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Message {
    /// The producer.
    pub role: Role,
    /// Text content.
    pub content: String,
    /// Tool calls the assistant requested.
    pub tool_calls: Vec<ToolCall>,
    /// The call this result answers, for tool messages.
    pub tool_call_id: Option<String>,
}

impl Message {
    /// A system message.
    #[must_use]
    pub fn system(content: impl Into<String>) -> Self {
        Self {
            role: Role::System,
            content: content.into(),
            tool_calls: Vec::new(),
            tool_call_id: None,
        }
    }

    /// A user message.
    #[must_use]
    pub fn user(content: impl Into<String>) -> Self {
        Self {
            role: Role::User,
            content: content.into(),
            tool_calls: Vec::new(),
            tool_call_id: None,
        }
    }

    /// An assistant message, with or without tool calls.
    #[must_use]
    pub fn assistant(content: impl Into<String>, tool_calls: Vec<ToolCall>) -> Self {
        Self {
            role: Role::Assistant,
            content: content.into(),
            tool_calls,
            tool_call_id: None,
        }
    }

    /// A tool result.
    #[must_use]
    pub fn tool(call_id: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            role: Role::Tool,
            content: content.into(),
            tool_calls: Vec::new(),
            tool_call_id: Some(call_id.into()),
        }
    }

    /// The wire representation.
    #[must_use]
    pub fn to_json(&self) -> Value {
        let mut object = serde_json::Map::new();
        object.insert(
            "role".to_string(),
            Value::String(self.role.as_str().to_string()),
        );
        object.insert("content".to_string(), Value::String(self.content.clone()));
        if !self.tool_calls.is_empty() {
            object.insert(
                "tool_calls".to_string(),
                Value::Array(self.tool_calls.iter().map(ToolCall::to_json).collect()),
            );
        }
        if let Some(call_id) = &self.tool_call_id {
            object.insert("tool_call_id".to_string(), Value::String(call_id.clone()));
        }
        Value::Object(object)
    }
}

#[cfg(test)]
mod tests {
    use super::{Message, Role, ToolCall};
    use alloc::string::ToString;
    use alloc::vec;
    use serde_json::json;

    #[test]
    fn roles_use_the_wire_spelling() {
        assert_eq!(Role::System.as_str(), "system");
        assert_eq!(Role::User.as_str(), "user");
        assert_eq!(Role::Assistant.as_str(), "assistant");
        assert_eq!(Role::Tool.as_str(), "tool");
    }

    #[test]
    fn user_and_system_messages_carry_only_content() {
        assert_eq!(
            Message::user("hi").to_json(),
            json!({"role": "user", "content": "hi"})
        );
        assert_eq!(
            Message::system("rules").to_json(),
            json!({"role": "system", "content": "rules"})
        );
    }

    #[test]
    fn an_assistant_message_serializes_its_tool_calls() {
        let call = ToolCall {
            id: "call_1".to_string(),
            name: "bash".to_string(),
            arguments: "{\"command\":\"ls\"}".to_string(),
        };
        assert_eq!(
            Message::assistant("", vec![call.clone()]).to_json(),
            json!({
                "role": "assistant",
                "content": "",
                "tool_calls": [{
                    "id": "call_1",
                    "type": "function",
                    "function": {"name": "bash", "arguments": "{\"command\":\"ls\"}"}
                }]
            })
        );
        assert_eq!(call.to_json()["function"]["name"], "bash");
    }

    #[test]
    fn a_tool_message_names_the_call_it_answers() {
        assert_eq!(
            Message::tool("call_1", "ok").to_json(),
            json!({"role": "tool", "content": "ok", "tool_call_id": "call_1"})
        );
        assert_eq!(Message::tool("call_1", "ok").tool_calls.len(), 0);
    }

    #[test]
    fn tool_call_arguments_are_parsed_with_the_tool_contract() {
        let call = ToolCall {
            id: "c".to_string(),
            name: "bash".to_string(),
            arguments: "{\"command\":\"echo hi\"}".to_string(),
        };
        assert!(call.invocation().is_ok());
        let bad = ToolCall {
            id: "c".to_string(),
            name: "bash".to_string(),
            arguments: "{}".to_string(),
        };
        assert!(bad.invocation().is_err());
    }
}
