//! MCP tools exposed by the `solutions` crate. Tools register with the
//! central `editor_mcp` registry from `solutions::init` so that
//! `start_server` (called later from `crates/zed/src/main.rs`) sees them
//! when binding the socket.
use gpui::App;

pub fn register(_cx: &mut App) {
    // Tools land in Tasks 4.2-4.5.
}
