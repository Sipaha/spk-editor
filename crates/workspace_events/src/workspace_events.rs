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
pub(crate) mod lifecycle;
mod list;
mod mcp;
mod snapshot;

pub use coordinator::{WorkspaceEvent, WorkspaceEventCoordinator};
pub use dto::*;
pub use list::ListSolutionsTool;

/// Install the coordinator + register MCP tools. Idempotent.
pub fn init(cx: &mut App) {
    coordinator::install(cx);
    mcp::register(cx);
}

/// Expose `build_snapshot` for integration tests that need to check the
/// snapshot filter logic without going through a live MCP socket.
///
/// This re-exports the internal `snapshot::build_snapshot` — it has no
/// side-effects and is safe to call in any context where the
/// `WorkspaceEventCoordinator` and `SolutionStore` globals are installed.
pub fn build_snapshot_for_test(cx: &App) -> WorkspaceSnapshot {
    snapshot::build_snapshot(cx)
}

/// Test-only direct invocation of the workspace.list_solutions logic,
/// bypassing the MCP socket. Used by integration tests.
pub fn list_solutions_for_test(cx: &App, open: Option<bool>) -> dto::ListSolutionsResult {
    list::build_list(cx, open)
}

/// Test-only direct invocation of `workspace.open_solution`.
/// Call from tests as: `cx.update(|cx| workspace_events::open_solution_for_test(cx, &id))`.
pub fn open_solution_for_test(cx: &mut App, id: &solutions::SolutionId) -> dto::SeqAck {
    let seq = lifecycle::open_solution_impl(cx, id).expect("open_solution");
    dto::SeqAck { seq }
}

/// Test-only direct invocation of `workspace.close_solution`.
/// Call from tests as: `cx.update(|cx| workspace_events::close_solution_for_test(cx, &id))`.
pub fn close_solution_for_test(cx: &mut App, id: &solutions::SolutionId) -> dto::SeqAck {
    let seq = lifecycle::close_solution_impl(cx, id).expect("close_solution");
    dto::SeqAck { seq }
}

/// Test-only accessor for the current event sequence number.
pub fn current_seq_for_test(cx: &App) -> u64 {
    coordinator::WorkspaceEventCoordinator::global(cx).current_seq()
}

/// Test-only direct invocation of `workspace.open_session`.
pub fn open_session_for_test(
    cx: &mut App,
    id: &solution_agent::SolutionSessionId,
) -> dto::SeqAck {
    let seq = lifecycle::open_session_impl(cx, id.as_str()).expect("open_session");
    dto::SeqAck { seq }
}

/// Test-only direct invocation of `workspace.close_session`.
pub fn close_session_for_test(
    cx: &mut App,
    id: &solution_agent::SolutionSessionId,
) -> dto::SeqAck {
    let seq = lifecycle::close_session_impl(cx, id.as_str()).expect("close_session");
    dto::SeqAck { seq }
}
