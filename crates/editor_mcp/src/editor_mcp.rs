//! Editor MCP — single-instance MCP server embedded in SPK Editor.
//!
//! Approach C central registry: domain crates register their tools during
//! their own `init()` via [`register_tool`]. After all init is done, the
//! editor calls [`start_server`] which binds the Unix socket and accepts
//! connections.

mod handoff;
mod lifecycle;
mod notifications;
mod registry;
mod subscriptions;
mod tools;

pub use handoff::{HandoffOutcome, try_handoff_to_existing_instance};
pub use lifecycle::start_server;
pub use registry::{init, register_tool};

#[cfg(test)]
pub use lifecycle::start_server_for_test;
