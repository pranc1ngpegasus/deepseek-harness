//! Streaming chat-completions chunks and tool-call assembly.
//!
//! A provider splits one assistant turn across many SSE events. Content
//! fragments append, and tool-call fragments merge by index because the id,
//! name, and argument text can each arrive in a different event.

use alloc::collections::BTreeMap;
use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use serde_json::Value;

use crate::message::ToolCall;

/// Token counts reported by the provider.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Usage {
    /// Prompt tokens.
    pub prompt_tokens: u64,
    /// Completion tokens.
    pub completion_tokens: u64,
    /// Total tokens.
    pub total_tokens: u64,
}

/// One fragment of a tool call.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ToolCallDelta {
    /// Position in the assistant's call list.
    pub index: usize,
    /// Correlation id, usually sent once.
    pub id: Option<String>,
    /// Tool name, usually sent once.
    pub name: Option<String>,
    /// Argument text fragment.
    pub arguments: Option<String>,
}

/// One decoded SSE data payload.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct StreamChunk {
    /// Assistant text fragment.
    pub content: Option<String>,
    /// Tool-call fragments.
    pub tool_calls: Vec<ToolCallDelta>,
    /// Why the turn ended, on the last chunk.
    pub finish_reason: Option<String>,
    /// Token usage, on the last chunk when the provider reports it.
    pub usage: Option<Usage>,
}

/// A chunk that cannot be decoded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamError {
    /// The provider's message or the JSON failure.
    pub message: String,
}

impl core::fmt::Display for StreamError {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter.write_str(&self.message)
    }
}

fn number(value: Option<&Value>) -> u64 {
    value.and_then(Value::as_u64).unwrap_or(0)
}

fn text(value: Option<&Value>) -> Option<String> {
    value.and_then(Value::as_str).map(str::to_string)
}

/// Decode one SSE `data:` payload.
///
/// # Errors
/// Returns a provider error object, invalid JSON, or a payload that is not a
/// chat-completions chunk.
pub fn parse_chunk(data: &str) -> Result<StreamChunk, StreamError> {
    let value: Value = serde_json::from_str(data).map_err(|error| StreamError {
        message: format!("invalid stream chunk: {error}"),
    })?;
    if let Some(error) = value.get("error") {
        let message = error
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("provider reported an error");
        return Err(StreamError {
            message: message.to_string(),
        });
    }
    let Some(choice) = value
        .get("choices")
        .and_then(Value::as_array)
        .and_then(|choices| choices.first())
    else {
        return Ok(StreamChunk {
            usage: value.get("usage").map(|usage| Usage {
                prompt_tokens: number(usage.get("prompt_tokens")),
                completion_tokens: number(usage.get("completion_tokens")),
                total_tokens: number(usage.get("total_tokens")),
            }),
            ..StreamChunk::default()
        });
    };
    let delta = choice.get("delta");
    let tool_calls = delta
        .and_then(|delta| delta.get("tool_calls"))
        .and_then(Value::as_array)
        .map(|calls| {
            calls
                .iter()
                .map(|call| {
                    let function = call.get("function");
                    ToolCallDelta {
                        index: call
                            .get("index")
                            .and_then(Value::as_u64)
                            .and_then(|index| usize::try_from(index).ok())
                            .unwrap_or(0),
                        id: text(call.get("id")),
                        name: text(function.and_then(|function| function.get("name"))),
                        arguments: text(function.and_then(|function| function.get("arguments"))),
                    }
                })
                .collect()
        })
        .unwrap_or_default();
    Ok(StreamChunk {
        content: text(delta.and_then(|delta| delta.get("content"))),
        tool_calls,
        finish_reason: text(choice.get("finish_reason")),
        usage: value.get("usage").map(|usage| Usage {
            prompt_tokens: number(usage.get("prompt_tokens")),
            completion_tokens: number(usage.get("completion_tokens")),
            total_tokens: number(usage.get("total_tokens")),
        }),
    })
}

#[derive(Debug, Default)]
struct PartialCall {
    id: String,
    name: String,
    arguments: String,
}

/// The assistant turn assembled from streamed chunks.
#[derive(Debug, Default)]
pub struct TurnStream {
    content: String,
    finish_reason: Option<String>,
    usage: Option<Usage>,
    calls: BTreeMap<usize, PartialCall>,
}

impl TurnStream {
    /// An empty turn.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Merge one chunk.
    pub fn apply(&mut self, chunk: StreamChunk) {
        if let Some(content) = chunk.content {
            self.content.push_str(&content);
        }
        for delta in chunk.tool_calls {
            let call = self.calls.entry(delta.index).or_default();
            if let Some(id) = delta.id {
                call.id.push_str(&id);
            }
            if let Some(name) = delta.name {
                call.name.push_str(&name);
            }
            if let Some(arguments) = delta.arguments {
                call.arguments.push_str(&arguments);
            }
        }
        if chunk.finish_reason.is_some() {
            self.finish_reason = chunk.finish_reason;
        }
        if chunk.usage.is_some() {
            self.usage = chunk.usage;
        }
    }

    /// The assistant text collected so far.
    #[must_use]
    pub fn content(&self) -> &str {
        &self.content
    }

    /// Finish the turn, producing calls in the model's index order.
    #[must_use]
    pub fn finish(self) -> TurnOutput {
        let tool_calls = self
            .calls
            .into_iter()
            .map(|(index, call)| ToolCall {
                id: if call.id.is_empty() {
                    format!("call_{index}")
                } else {
                    call.id
                },
                name: call.name,
                arguments: call.arguments,
            })
            .collect();
        TurnOutput {
            content: self.content,
            tool_calls,
            finish_reason: self.finish_reason,
            usage: self.usage,
        }
    }
}

/// One completed assistant turn.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct TurnOutput {
    /// Assistant text.
    pub content: String,
    /// Tool calls in index order.
    pub tool_calls: Vec<ToolCall>,
    /// The provider's finish reason.
    pub finish_reason: Option<String>,
    /// Token usage.
    pub usage: Option<Usage>,
}

#[cfg(test)]
mod tests {
    use super::{StreamChunk, ToolCallDelta, TurnStream, Usage, parse_chunk};
    use alloc::string::ToString;
    use alloc::vec;

    #[test]
    fn decodes_a_content_delta() {
        let chunk =
            parse_chunk(r#"{"choices":[{"delta":{"content":"hel"},"finish_reason":null}]}"#)
                .expect("chunk");
        assert_eq!(chunk.content.as_deref(), Some("hel"));
        assert!(chunk.tool_calls.is_empty());
        assert_eq!(chunk.finish_reason, None);
    }

    #[test]
    fn decodes_tool_call_deltas_and_usage() {
        let chunk = parse_chunk(
            r#"{"choices":[{"delta":{"tool_calls":[{"index":2,"id":"c","function":{"name":"bash","arguments":"{\"command\""}}]},"finish_reason":"tool_calls"}],"usage":{"prompt_tokens":3,"completion_tokens":4,"total_tokens":7}}"#,
        )
        .expect("chunk");
        assert_eq!(
            chunk.tool_calls,
            vec![ToolCallDelta {
                index: 2,
                id: Some("c".to_string()),
                name: Some("bash".to_string()),
                arguments: Some("{\"command\"".to_string()),
            }]
        );
        assert_eq!(chunk.finish_reason.as_deref(), Some("tool_calls"));
        assert_eq!(
            chunk.usage,
            Some(Usage {
                prompt_tokens: 3,
                completion_tokens: 4,
                total_tokens: 7
            })
        );
    }

    #[test]
    fn a_usage_only_chunk_is_accepted() {
        let chunk = parse_chunk(r#"{"usage":{"prompt_tokens":1}}"#).expect("chunk");
        assert_eq!(
            chunk.usage,
            Some(Usage {
                prompt_tokens: 1,
                completion_tokens: 0,
                total_tokens: 0
            })
        );
        assert!(chunk.content.is_none());
    }

    #[test]
    fn provider_errors_and_bad_json_are_rejected() {
        assert_eq!(
            parse_chunk(r#"{"error":{"message":"rate limited"}}"#)
                .expect_err("error")
                .message,
            "rate limited"
        );
        assert!(
            parse_chunk(r#"{"error":{}}"#)
                .expect_err("error")
                .message
                .contains("provider reported")
        );
        assert!(parse_chunk("not json").is_err());
    }

    #[test]
    fn fragments_merge_by_index_and_sort_on_finish() {
        let mut stream = TurnStream::new();
        stream.apply(StreamChunk {
            tool_calls: vec![ToolCallDelta {
                index: 1,
                id: Some("second".to_string()),
                name: Some("read_file".to_string()),
                arguments: Some("{\"path\":".to_string()),
            }],
            ..StreamChunk::default()
        });
        stream.apply(StreamChunk {
            content: Some("thinking".to_string()),
            tool_calls: vec![ToolCallDelta {
                index: 1,
                arguments: Some("\"a\"}".to_string()),
                ..ToolCallDelta::default()
            }],
            ..StreamChunk::default()
        });
        stream.apply(StreamChunk {
            tool_calls: vec![ToolCallDelta {
                index: 0,
                id: Some("first".to_string()),
                name: Some("bash".to_string()),
                arguments: Some("{}".to_string()),
            }],
            finish_reason: Some("tool_calls".to_string()),
            usage: Some(Usage {
                prompt_tokens: 1,
                completion_tokens: 2,
                total_tokens: 3,
            }),
            ..StreamChunk::default()
        });
        assert_eq!(stream.content(), "thinking");
        let output = stream.finish();
        assert_eq!(output.content, "thinking");
        assert_eq!(output.tool_calls.len(), 2);
        assert_eq!(output.tool_calls[0].id, "first");
        assert_eq!(output.tool_calls[1].id, "second");
        assert_eq!(output.tool_calls[1].arguments, "{\"path\":\"a\"}");
        assert_eq!(output.finish_reason.as_deref(), Some("tool_calls"));
        assert_eq!(output.usage.expect("usage").total_tokens, 3);
    }

    #[test]
    fn a_call_without_an_id_gets_a_stable_one() {
        let mut stream = TurnStream::new();
        stream.apply(StreamChunk {
            tool_calls: vec![ToolCallDelta {
                index: 4,
                name: Some("list_dir".to_string()),
                ..ToolCallDelta::default()
            }],
            ..StreamChunk::default()
        });
        assert_eq!(stream.finish().tool_calls[0].id, "call_4");
    }

    #[test]
    fn an_empty_turn_finishes_with_no_calls() {
        let output = TurnStream::new().finish();
        assert!(output.content.is_empty());
        assert!(output.tool_calls.is_empty());
        assert_eq!(output.finish_reason, None);
        assert_eq!(output.usage, None);
    }

    #[test]
    fn a_stream_error_displays_its_message() {
        assert_eq!(
            super::StreamError {
                message: "bad stream".to_string()
            }
            .to_string(),
            "bad stream"
        );
    }
}
