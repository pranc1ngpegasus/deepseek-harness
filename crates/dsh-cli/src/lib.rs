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
//! The `dsh` launcher.
//!
//! Argument resolution, profile storage, config dumps, plugin forwarding, and
//! process lifecycle live here. The plugin mount itself is the harness host's
//! job, reached through one process seam so the launcher can be replaced
//! without changing the launcher contract.
#![forbid(unsafe_code)]

pub mod diagnostics_io;
pub mod dump;
pub mod home;
pub mod plugin;
pub mod profile_store;
pub mod runtime;
pub mod schema;
pub mod yaml;
