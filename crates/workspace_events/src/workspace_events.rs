//! Workspace-events crate.
//!
//! Owns the sequenced event protocol that backs the mobile `workspace.*` MCP
//! surface. Hosts:
//!   - `WorkspaceEventCoordinator` — an atomic `seq` counter + the sequenced
//!     emit helper used by mutation paths in `solutions` and `solution_agent`.
//!   - The `workspace.*` MCP tools: `snapshot`, `list_solutions`, `open_solution`,
//!     `close_solution`, `open_session`, `close_session`.
//!
//! Subsequent tasks fill these out. For now we expose `init` so `crates/zed`
//! can wire us up without further plumbing later.

use gpui::App;

mod coordinator;
mod dto;
mod mcp;

pub use coordinator::{WorkspaceEvent, WorkspaceEventCoordinator};
pub use dto::*;

/// Install the coordinator + register MCP tools. Idempotent.
pub fn init(cx: &mut App) {
    coordinator::install(cx);
    mcp::register(cx);
}
