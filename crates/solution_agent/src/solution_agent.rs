//! Solution-scoped AI sessions: N parallel Claude Code-style chats per Solution,
//! multiplexed onto a shared subprocess per (solution, agent) pair.
//!
//! See `docs/superpowers/specs/2026-04-26-solution-scoped-ai-sessions-design.md`
//! for the design rationale.

use gpui::App;

pub fn init(_cx: &mut App) {
    // Phases 1+ register store, MCP, UI, actions here.
}
