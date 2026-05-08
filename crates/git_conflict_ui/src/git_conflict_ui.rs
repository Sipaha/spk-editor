//! Standalone 3-way merge conflict resolver.
//!
//! Skeleton crate. Real implementation lands in S-CFL (`docs/superpowers/plans/git-panel-plan.md`).
//! Owns its own MCP tools (`editor.git.list_conflicts`, `editor.git.resolve_conflict`,
//! `editor.git.mark_resolved`, `editor.git.continue_merge`, `editor.git.abort_merge`).
//!
//! Independent crate because of the size of the 3-way merge view and isolation of the
//! resolver UI surface — not for merge-friendliness.

use gpui::App;

pub fn init(_cx: &mut App) {
    // S-CFL implementation:
    // - register `editor.git.*` conflict tools via `editor_mcp::register_tool`
    // - install a workspace observer that opens `ResolverView` on conflict detection
}
