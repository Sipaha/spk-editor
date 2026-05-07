//! `editor.capabilities` MCP tool — protocol probe for clients.
use context_server::listener::{McpServerTool, ToolResponse};
use context_server::types::ToolResponseContent;
use gpui::AsyncApp;
use schemars::JsonSchema;
use serde::{Deserialize, Deserializer, Serialize};

/// Editor MCP capability probe — returns protocol version, server version,
/// supported event kinds, and any experimental flags currently enabled.
#[derive(Debug, Clone, Default, JsonSchema)]
pub struct CapabilitiesParams {}

// Custom deserializer accepts JSON null, missing, or `{}` — all valid forms
// for a tool whose input schema declares no required fields. Without this,
// `serde_json::from_value(Value::Null)` rejects the unit-style struct, so
// MCP clients that omit `arguments` (the dispatcher routes that to `Null`)
// would fail before reaching `run`.
impl<'de> Deserialize<'de> for CapabilitiesParams {
    fn deserialize<D: Deserializer<'de>>(de: D) -> Result<Self, D::Error> {
        let _ = serde::de::IgnoredAny::deserialize(de)?;
        Ok(CapabilitiesParams {})
    }
}

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
    "solution_active_changed",
    "solution_panel_member_selection_changed",
    "window_focused",
    "lsp_started",
    "lsp_stopped",
    "cli_args_received",
    "server_shutting_down",
    "agent_session_created",
    "agent_session_closed",
    "agent_session_state_changed",
    "agent_session_title_changed",
    "agent_session_message_appended",
    "agent_session_notification_sent",
];
