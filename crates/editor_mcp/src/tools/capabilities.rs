//! `editor.capabilities` MCP tool — protocol probe for clients.
use context_server::listener::{McpServerTool, ToolResponse};
use context_server::types::ToolResponseContent;
use gpui::AsyncApp;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Editor MCP capability probe — returns protocol version, server version,
/// supported event kinds, and any experimental flags currently enabled.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct CapabilitiesParams {}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct Capabilities {
    pub protocol_version: String,
    pub editor_mcp_version: String,
    pub supported_event_kinds: Vec<String>,
    pub experiments: Vec<String>,
}

#[derive(Clone)]
pub struct CapabilitiesTool;

impl McpServerTool for CapabilitiesTool {
    type Input = CapabilitiesParams;
    type Output = Capabilities;
    const NAME: &'static str = "editor.capabilities";

    async fn run(
        &self,
        _input: Self::Input,
        _cx: &mut AsyncApp,
    ) -> anyhow::Result<ToolResponse<Self::Output>> {
        let caps = Capabilities {
            protocol_version: "2024-11-05".to_string(),
            editor_mcp_version: env!("CARGO_PKG_VERSION").to_string(),
            supported_event_kinds: SUPPORTED_EVENT_KINDS
                .iter()
                .map(|s| (*s).to_string())
                .collect(),
            experiments: vec![],
        };
        Ok(ToolResponse {
            content: vec![ToolResponseContent::Text {
                text: format!("editor_mcp v{}", caps.editor_mcp_version),
            }],
            structured_content: caps,
        })
    }
}

pub(crate) const SUPPORTED_EVENT_KINDS: &[&str] = &[
    "operation_progress",
    "operation_completed",
    "buffer_opened",
    "buffer_closed",
    "buffer_saved",
    "buffer_dirty_changed",
    "selection_changed",
    "diagnostic_updated",
    "solution_changed",
    "window_focused",
    "lsp_started",
    "lsp_stopped",
    "cli_args_received",
    "server_shutting_down",
];
