//! Native Rust connection to the `claude` binary's stream-json protocol,
//! replacing the `@agentclientprotocol/claude-agent-acp` node wrapper.
//! Implements `acp_thread::AgentConnection`; selected via the
//! `solution_agent.claude_backend = "native"` setting.

pub mod command;
mod connection;
pub mod process;
pub mod protocol;
mod translate;
mod watchdog;

pub use connection::{ClaudeNativeAgentServer, ClaudeNativeConnection};
