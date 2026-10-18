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
//! Streaming chat-completions client.
//!
//! The client owns HTTP and SSE framing. It hands each assistant text fragment
//! to a callback as it arrives and returns the completed turn with its tool
//! calls assembled. A non-success status returns the provider's body so the
//! operator sees the real cause.

use std::io::{BufRead, BufReader};
use std::time::Duration;

use dsh_agent::{Message, TurnOutput, TurnStream, parse_chunk, tool_definitions};
use serde_json::{Value, json};

/// Connection settings for one provider.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LlmConfig {
    /// Base URL without `/chat/completions`.
    pub base_url: String,
    /// Bearer token.
    pub api_key: String,
    /// Model name.
    pub model: String,
}

impl LlmConfig {
    /// Resolve from the environment with the provider defaults.
    ///
    /// # Errors
    /// Returns when no API key is set.
    pub fn from_environment() -> Result<Self, LlmError> {
        let api_key = resolve_api_key(std::env::var("DEEPSEEK_API_KEY").ok())?;
        Ok(Self {
            base_url: std::env::var("DEEPSEEK_BASE_URL")
                .unwrap_or_else(|_| "https://api.deepseek.com".to_string()),
            api_key,
            model: std::env::var("DEEPSEEK_MODEL")
                .or_else(|_| std::env::var("DSH_MODEL"))
                .unwrap_or_else(|_| "deepseek-chat".to_string()),
        })
    }

    /// The full completions URL.
    #[must_use]
    pub fn endpoint(&self) -> String {
        format!("{}/chat/completions", self.base_url.trim_end_matches('/'))
    }
}

/// A provider or transport failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LlmError {
    /// What to show the operator.
    pub message: String,
}

impl LlmError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl std::fmt::Display for LlmError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for LlmError {}

/// Build the request body for one model call.
#[must_use]
pub fn request_body(model: &str, messages: &[Message]) -> Value {
    json!({
        "model": model,
        "messages": messages.iter().map(Message::to_json).collect::<Vec<_>>(),
        "stream": true,
        "tools": tool_definitions(),
        "tool_choice": "auto",
    })
}

/// The message a cancelled stream reports.
pub const CANCELLED: &str = "cancelled";

/// Validate an API key supplied by the environment or a flag.
fn resolve_api_key(value: Option<String>) -> Result<String, LlmError> {
    value.filter(|key| !key.trim().is_empty()).ok_or_else(|| {
        LlmError::new("DEEPSEEK_API_KEY is not set; export it before starting dsh tui")
    })
}

/// A configured streaming client.
#[derive(Debug, Clone)]
pub struct LlmClient {
    config: LlmConfig,
    http: reqwest::blocking::Client,
}

impl LlmClient {
    /// Build a client for one provider configuration.
    ///
    /// # Errors
    /// Returns when the HTTP client cannot be constructed.
    pub fn new(config: LlmConfig) -> Result<Self, LlmError> {
        let http = reqwest::blocking::Client::builder()
            .connect_timeout(Duration::from_secs(15))
            .timeout(Duration::from_secs(600))
            .build()
            .map_err(|error| LlmError::new(format!("cannot build the HTTP client: {error}")))?;
        Ok(Self { config, http })
    }

    /// The provider configuration.
    #[must_use]
    pub const fn config(&self) -> &LlmConfig {
        &self.config
    }

    /// Run one streaming model call.
    ///
    /// `on_delta` receives each assistant text fragment as it arrives.
    ///
    /// # Errors
    /// Returns a transport failure, a non-success status with the provider's
    /// body, or a malformed stream chunk.
    pub fn stream(
        &self,
        messages: &[Message],
        on_delta: &mut dyn FnMut(&str),
    ) -> Result<TurnOutput, LlmError> {
        self.stream_with_cancel(messages, on_delta, &|| false)
    }

    /// Run one streaming model call that stops between chunks when asked.
    ///
    /// A stopped stream returns [`CANCELLED`] so the caller can distinguish a
    /// person's cancellation from a provider failure.
    ///
    /// # Errors
    /// Returns a transport failure, a non-success status with the provider's
    /// body, a malformed stream chunk, or [`CANCELLED`].
    pub fn stream_with_cancel(
        &self,
        messages: &[Message],
        on_delta: &mut dyn FnMut(&str),
        should_stop: &dyn Fn() -> bool,
    ) -> Result<TurnOutput, LlmError> {
        if should_stop() {
            return Err(LlmError::new(CANCELLED));
        }
        let body = request_body(&self.config.model, messages);
        let response = self
            .http
            .post(self.config.endpoint())
            .bearer_auth(&self.config.api_key)
            .json(&body)
            .send()
            .map_err(|error| LlmError::new(format!("request failed: {error}")))?;
        let status = response.status();
        if !status.is_success() {
            let body = response
                .text()
                .unwrap_or_else(|_| "the provider returned no body".to_string());
            return Err(LlmError::new(format!(
                "provider returned HTTP {status}: {}",
                body.trim()
            )));
        }
        let reader = BufReader::new(response);
        let mut stream = TurnStream::new();
        for line in reader.lines() {
            if should_stop() {
                return Err(LlmError::new(CANCELLED));
            }
            let line =
                line.map_err(|error| LlmError::new(format!("stream read failed: {error}")))?;
            let Some(data) = line.strip_prefix("data:") else {
                continue;
            };
            let data = data.trim();
            if data.is_empty() {
                continue;
            }
            if data == "[DONE]" {
                break;
            }
            let chunk = parse_chunk(data).map_err(|error| LlmError::new(error.message))?;
            if let Some(content) = chunk.content.as_deref() {
                on_delta(content);
            }
            stream.apply(chunk);
        }
        Ok(stream.finish())
    }
}

#[cfg(test)]
mod tests {
    use super::{LlmClient, LlmConfig, LlmError, request_body};
    use dsh_agent::{Message, ToolCall};
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::mpsc::{Receiver, channel};

    struct Server {
        base_url: String,
        bodies: Receiver<String>,
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

    fn serve(responses: Vec<(u16, String)>) -> Server {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let address = listener.local_addr().expect("addr");
        let (sender, bodies) = channel();
        std::thread::spawn(move || {
            for (status, body) in responses {
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
                    if let Some(index) = request.windows(4).position(|window| window == b"\r\n\r\n")
                    {
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
                let _ = sender.send(String::from_utf8_lossy(&request[header_end..]).into_owned());
                let response = format!(
                    "HTTP/1.1 {status} OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = stream.write_all(response.as_bytes());
            }
        });
        Server {
            base_url: format!("http://{address}"),
            bodies,
        }
    }

    fn config(base_url: &str) -> LlmConfig {
        LlmConfig {
            base_url: base_url.to_string(),
            api_key: "test-key".to_string(),
            model: "deepseek-chat".to_string(),
        }
    }

    #[test]
    fn the_endpoint_trims_a_trailing_slash() {
        assert_eq!(
            config("http://example/").endpoint(),
            "http://example/chat/completions"
        );
        assert_eq!(
            config("http://example").endpoint(),
            "http://example/chat/completions"
        );
    }

    #[test]
    fn the_request_body_carries_the_history_and_tools() {
        let body = request_body("model-x", &[Message::user("hello")]);
        assert_eq!(body["model"], "model-x");
        assert_eq!(body["messages"][0]["role"], "user");
        assert_eq!(body["stream"], true);
        assert_eq!(body["tool_choice"], "auto");
        assert!(body["tools"].as_array().expect("tools").len() >= 6);
    }

    #[test]
    fn a_streamed_answer_is_assembled_and_forwarded() {
        let server = serve(vec![(
            200,
            sse(&[
                r#"{"choices":[{"delta":{"content":"Hel"}}]}"#,
                r#"{"choices":[{"delta":{"content":"lo"}}]}"#,
                r#"{"choices":[{"delta":{},"finish_reason":"stop"}]}"#,
                "[DONE]",
            ]),
        )]);
        let client = LlmClient::new(config(&server.base_url)).expect("client");
        let mut deltas = String::new();
        let output = client
            .stream(&[Message::user("hi")], &mut |delta| deltas.push_str(delta))
            .expect("stream");
        assert_eq!(deltas, "Hello");
        assert_eq!(output.content, "Hello");
        assert!(output.tool_calls.is_empty());
        assert_eq!(output.finish_reason.as_deref(), Some("stop"));
        let body = server.bodies.recv().expect("request body");
        assert!(body.contains("\"model\":\"deepseek-chat\""), "{body}");
        assert!(body.contains("\"stream\":true"), "{body}");
    }

    #[test]
    fn streamed_tool_calls_are_assembled() {
        let server = serve(vec![(
            200,
            sse(&[
                r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"c","function":{"name":"bash","arguments":"{\"command\":"}}]}}]}"#,
                r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"\"ls\"}"}}]},"finish_reason":"tool_calls"}]}"#,
                "[DONE]",
            ]),
        )]);
        let client = LlmClient::new(config(&server.base_url)).expect("client");
        let output = client
            .stream(&[Message::user("list")], &mut |_| {})
            .expect("stream");
        assert_eq!(
            output.tool_calls,
            vec![ToolCall {
                id: "c".to_string(),
                name: "bash".to_string(),
                arguments: "{\"command\":\"ls\"}".to_string(),
            }]
        );
    }

    #[test]
    fn a_non_success_status_reports_the_provider_body() {
        let server = serve(vec![(401, "bad key".to_string())]);
        let client = LlmClient::new(config(&server.base_url)).expect("client");
        let error = client
            .stream(&[Message::user("hi")], &mut |_| {})
            .expect_err("status");
        assert!(error.message.contains("401"), "{}", error.message);
        assert!(error.message.contains("bad key"), "{}", error.message);
    }

    #[test]
    fn a_malformed_chunk_stops_the_stream() {
        let server = serve(vec![(200, sse(&["not json"]))]);
        let client = LlmClient::new(config(&server.base_url)).expect("client");
        assert!(
            client
                .stream(&[Message::user("hi")], &mut |_| {})
                .expect_err("chunk")
                .message
                .contains("invalid stream chunk")
        );
    }

    #[test]
    fn a_provider_error_chunk_stops_the_stream() {
        let server = serve(vec![(200, sse(&[r#"{"error":{"message":"overloaded"}}"#]))]);
        let client = LlmClient::new(config(&server.base_url)).expect("client");
        let error = client
            .stream(&[Message::user("hi")], &mut |_| {})
            .expect_err("error");
        assert_eq!(error.message, "overloaded");
    }

    #[test]
    fn a_cancelled_stream_stops_between_chunks() {
        let server = serve(vec![(
            200,
            sse(&[
                r#"{"choices":[{"delta":{"content":"one"}}]}"#,
                r#"{"choices":[{"delta":{"content":"two"}}]}"#,
                "[DONE]",
            ]),
        )]);
        let client = LlmClient::new(config(&server.base_url)).expect("client");
        let stop = std::cell::Cell::new(false);
        let error = client
            .stream_with_cancel(&[Message::user("hi")], &mut |_| stop.set(true), &|| {
                stop.get()
            })
            .expect_err("cancel");
        assert_eq!(error.message, super::CANCELLED);
    }

    #[test]
    fn an_already_cancelled_stream_sends_no_request() {
        let client = LlmClient::new(config("http://127.0.0.1:1")).expect("client");
        let error = client
            .stream_with_cancel(&[Message::user("hi")], &mut |_| {}, &|| true)
            .expect_err("cancel");
        assert_eq!(error.message, super::CANCELLED);
    }

    #[test]
    fn api_key_resolution_rejects_missing_blank_and_whitespace_values() {
        assert_eq!(
            super::resolve_api_key(Some("key".to_string())).expect("key"),
            "key"
        );
        for value in [None, Some(String::new()), Some("   ".to_string())] {
            assert!(
                super::resolve_api_key(value)
                    .expect_err("blank")
                    .message
                    .contains("DEEPSEEK_API_KEY")
            );
        }
    }

    #[test]
    fn a_missing_key_is_reported_before_any_request() {
        let previous = std::env::var("DEEPSEEK_API_KEY").ok();
        // The environment is process-global; this test only asserts the
        // constructor's message shape without mutating it when a key is set.
        if previous.is_none() {
            let error = LlmConfig::from_environment().expect_err("key");
            assert!(error.message.contains("DEEPSEEK_API_KEY"));
        }
        assert_eq!(LlmError::new("x").to_string(), "x");
    }
}
