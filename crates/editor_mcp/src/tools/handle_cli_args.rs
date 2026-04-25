//! `editor.handle_cli_args` MCP tool — single-instance handoff endpoint.
//!
//! When a second `spk-editor` process launches, it connects to the existing
//! instance's socket and calls this tool with the CLI paths. The existing
//! instance opens them in (or as) a workspace and returns metadata.
use context_server::listener::{McpServerTool, ToolResponse};
use context_server::types::ToolResponseContent;
use gpui::AsyncApp;
use schemars::JsonSchema;
use serde::{Deserialize, Deserializer, Serialize};
use std::path::PathBuf;

/// Forward CLI args from a second editor process to the existing instance.
/// The existing instance opens any provided paths and focuses the relevant window.
#[derive(Debug, Clone, Default, JsonSchema)]
pub struct HandleCliArgsParams {
    pub paths: Vec<String>,
    pub cwd: Option<String>,
    pub new_window: Option<bool>,
    pub focus: Option<bool>,
}

// Custom deserializer accepts JSON null, missing, or `{}` — the dispatcher in
// `context_server::listener` converts a missing `arguments` field to
// `Value::Null`, which serde would otherwise reject for a struct. When a
// concrete object is provided, fields are populated normally; absent fields
// fall back to `Default`.
impl<'de> Deserialize<'de> for HandleCliArgsParams {
    fn deserialize<D: Deserializer<'de>>(de: D) -> Result<Self, D::Error> {
        #[derive(Deserialize, Default)]
        #[serde(default)]
        struct Inner {
            paths: Vec<String>,
            cwd: Option<String>,
            new_window: Option<bool>,
            focus: Option<bool>,
        }

        let opt = Option::<Inner>::deserialize(de)?;
        let inner = opt.unwrap_or_default();
        Ok(HandleCliArgsParams {
            paths: inner.paths,
            cwd: inner.cwd,
            new_window: inner.new_window,
            focus: inner.focus,
        })
    }
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct HandleCliArgsResult {
    pub handled: bool,
    pub opened_paths: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub focused_window_id: Option<String>,
}

#[derive(Clone)]
pub struct HandleCliArgsTool;

impl McpServerTool for HandleCliArgsTool {
    type Input = HandleCliArgsParams;
    type Output = HandleCliArgsResult;
    const NAME: &'static str = "editor.handle_cli_args";

    async fn run(
        &self,
        input: Self::Input,
        cx: &mut AsyncApp,
    ) -> anyhow::Result<ToolResponse<Self::Output>> {
        let resolved: Vec<PathBuf> = input
            .paths
            .iter()
            .map(|p| {
                let pb = PathBuf::from(p);
                if pb.is_absolute() {
                    pb
                } else if let Some(cwd) = input.cwd.as_ref() {
                    PathBuf::from(cwd).join(p)
                } else {
                    pb
                }
            })
            .collect();

        let opened_paths: Vec<String> = resolved
            .iter()
            .map(|p| p.to_string_lossy().into_owned())
            .collect();

        let mut focused_window_id: Option<String> = None;

        if !resolved.is_empty() {
            let task = cx.update(|cx| {
                let app_state = workspace::AppState::global(cx);
                workspace::open_paths(
                    &resolved,
                    app_state,
                    workspace::OpenOptions::default(),
                    cx,
                )
            });
            match task.await {
                Ok(open_result) => {
                    focused_window_id = Some(format!("{:?}", open_result.window.window_id()));
                }
                Err(err) => {
                    log::error!("editor_mcp: handle_cli_args open_paths failed: {err}");
                }
            }
        }

        // TODO Phase 7: emit `cli_args_received` notification with payload
        // { paths, source_pid, opened_window_id }.

        Ok(ToolResponse {
            content: vec![ToolResponseContent::Text {
                text: format!("opened {} path(s)", opened_paths.len()),
            }],
            structured_content: HandleCliArgsResult {
                handled: true,
                opened_paths,
                focused_window_id,
            },
        })
    }
}
