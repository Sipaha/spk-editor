//! Tool registry: holds boxed registration callbacks until `start_server`
//! drains them and applies to the live `McpServer`.
use gpui::App;

pub fn init(_cx: &mut App) {
    // Stub — implemented in Task 1.3.
}

pub fn register_tool<F>(_cx: &mut App, _registration: F)
where
    F: FnOnce(&mut context_server::listener::McpServer) + 'static,
{
    // Stub — implemented in Task 1.3.
}
