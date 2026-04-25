//! MCP server lifecycle: lock acquisition, server bind, graceful shutdown.
use anyhow::Result;
use gpui::App;

pub fn start_server(_cx: &mut App) -> Result<()> {
    // Stub — implemented in Task 1.4.
    Ok(())
}

#[cfg(test)]
pub fn start_server_for_test(_cx: &mut App) -> Result<()> {
    Ok(())
}
