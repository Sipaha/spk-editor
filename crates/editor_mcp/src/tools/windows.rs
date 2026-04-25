//! `windows.*` MCP tools — list/focus/close/dispatch_action operations on open editor windows.
use context_server::listener::{McpServerTool, ToolResponse};
use context_server::types::ToolResponseContent;
use gpui::{App, AsyncApp};
use schemars::JsonSchema;
use serde::{Deserialize, Deserializer, Serialize};

/// List all open editor windows. Returns metadata (id, kind, root paths,
/// focused state, bounds, title) for each window currently managed by the
/// editor.
#[derive(Debug, Clone, Default, JsonSchema)]
pub struct ListWindowsParams {}

// Custom deserializer accepts JSON null, missing, or `{}` — matches the
// pattern used by other zero-field tool inputs (capabilities, etc.).
impl<'de> Deserialize<'de> for ListWindowsParams {
    fn deserialize<D: Deserializer<'de>>(de: D) -> Result<Self, D::Error> {
        let _ = serde::de::IgnoredAny::deserialize(de)?;
        Ok(ListWindowsParams {})
    }
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct WindowInfo {
    pub window_id: String,
    pub kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub solution_id: Option<String>,
    pub root_paths: Vec<String>,
    pub focused: bool,
    pub bounds: [u32; 4],
    pub title: String,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct ListWindowsResult {
    pub windows: Vec<WindowInfo>,
}

#[derive(Clone)]
pub struct ListWindowsTool;

impl McpServerTool for ListWindowsTool {
    type Input = ListWindowsParams;
    type Output = ListWindowsResult;
    const NAME: &'static str = "windows.list";

    async fn run(
        &self,
        _input: Self::Input,
        cx: &mut AsyncApp,
    ) -> anyhow::Result<ToolResponse<Self::Output>> {
        let windows: Vec<WindowInfo> = cx.update(collect_windows);
        Ok(ToolResponse {
            content: vec![ToolResponseContent::Text {
                text: format!("{} window(s) open", windows.len()),
            }],
            structured_content: ListWindowsResult { windows },
        })
    }
}

fn collect_windows(cx: &mut App) -> Vec<WindowInfo> {
    let active_window_id = cx.active_window().map(|h| h.window_id());
    let mut out = Vec::new();
    for handle in cx.windows() {
        let Some(window_handle) = handle.downcast::<workspace::MultiWorkspace>() else {
            continue;
        };
        let window_id = handle.window_id();
        let info = window_handle.update(cx, |multi, window, cx| {
            build_window_info(window_id, active_window_id, multi, window, cx)
        });
        if let Ok(info) = info {
            out.push(info);
        }
    }
    out
}

fn build_window_info(
    window_id: gpui::WindowId,
    active_window_id: Option<gpui::WindowId>,
    multi: &mut workspace::MultiWorkspace,
    window: &mut gpui::Window,
    cx: &mut gpui::Context<workspace::MultiWorkspace>,
) -> WindowInfo {
    let workspace = multi.workspace().read(cx);
    let project = workspace.project().read(cx);

    let root_paths: Vec<String> = project
        .visible_worktrees(cx)
        .map(|tree| tree.read(cx).abs_path().to_string_lossy().into_owned())
        .collect();

    let solution_id = solutions::SolutionStore::try_global(cx).and_then(|store| {
        store.read_with(cx, |store, _| {
            store.solutions().iter().find_map(|sol| {
                if root_paths
                    .iter()
                    .any(|p| std::path::Path::new(p).starts_with(&sol.root))
                {
                    Some(sol.id.as_str().to_string())
                } else {
                    None
                }
            })
        })
    });

    let kind = if solution_id.is_some() {
        "solution"
    } else if root_paths.is_empty() {
        "welcome"
    } else {
        "folder"
    }
    .to_string();

    let bounds = window.bounds();
    let bounds_arr = [
        u32::from(bounds.origin.x),
        u32::from(bounds.origin.y),
        u32::from(bounds.size.width),
        u32::from(bounds.size.height),
    ];

    let title = compute_title(&root_paths);

    WindowInfo {
        window_id: crate::window_ids::format(window_id),
        kind,
        solution_id,
        root_paths,
        focused: active_window_id == Some(window_id),
        bounds: bounds_arr,
        title,
    }
}

// `Workspace::update_window_title` is private and only writes to the OS
// window title; there is no public getter for the cached value. To keep the
// MCP response self-contained, derive a simple human-readable title from the
// known root paths, falling back to the product name when no folder is open.
fn compute_title(root_paths: &[String]) -> String {
    if root_paths.is_empty() {
        return String::from("SPK Editor");
    }
    let names: Vec<String> = root_paths
        .iter()
        .filter_map(|p| {
            std::path::Path::new(p)
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
        })
        .collect();
    if names.is_empty() {
        String::from("SPK Editor")
    } else {
        names.join(", ")
    }
}
