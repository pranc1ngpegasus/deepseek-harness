//! The coding agent's pure core.
//!
//! Message history, the tool contract, streamed tool-call assembly, and the
//! turn state machine live here with no filesystem or network access. The
//! runtime owns HTTP, tool execution, and the terminal, so every branch in
//! this crate is reachable from a direct test.
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
#![no_std]
extern crate alloc;

#[cfg(test)]
extern crate std;

pub mod message;
pub mod prompt;
pub mod stream;
pub mod tools;
pub mod truncate;
pub mod turn;

pub use message::{Message, Role, ToolCall};
pub use stream::{
    StreamChunk, StreamError, ToolCallDelta, TurnOutput, TurnStream, Usage, parse_chunk,
};
pub use tools::{
    TOOL_SPECS, ToolError, ToolInvocation, ToolSpec, parse_invocation, tool_definitions,
};
pub use truncate::truncate_output;
pub use turn::{Agent, DEFAULT_HISTORY_MESSAGES, Phase, Step};
