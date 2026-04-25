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
    pub bounds: [i32; 4],
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
    // Prefer Z-ordered window stack for stable, meaningful ordering. SlotMap
    // iteration via `cx.windows()` is unstable across calls, which the
    // fallback compensates for with a deterministic sort by window id.
    let handles = cx.window_stack().unwrap_or_else(|| cx.windows());
    let mut out = Vec::new();
    for handle in handles {
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
    // Solution windows retain multiple workspaces; reading only the active
    // one would miss worktrees of non-active members. Walk every retained
    // workspace and dedupe paths so the response reflects the full window.
    let mut root_paths: Vec<String> = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    for workspace_entity in multi.workspaces() {
        let workspace = workspace_entity.read(cx);
        let project = workspace.project().read(cx);
        for tree in project.visible_worktrees(cx) {
            let path = tree.read(cx).abs_path().to_string_lossy().into_owned();
            if seen.insert(path.clone()) {
                root_paths.push(path);
            }
        }
    }

    let solution_id = solutions::SolutionStore::try_global(cx).and_then(|store| {
        store.read_with(cx, |store, _| {
            store.solutions().iter().find_map(|sol| {
                let matches = root_paths.iter().any(|p| {
                    let path = std::path::Path::new(p);
                    path.starts_with(&sol.root)
                        || sol.members.iter().any(|m| path.starts_with(&m.local_path))
                });
                if matches {
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
    // Window origin can be negative on multi-monitor / off-screen setups, so
    // use signed integers and route through `f32` (the only `From<Pixels>`
    // impl that preserves sign).
    let bounds_arr = [
        f32::from(bounds.origin.x) as i32,
        f32::from(bounds.origin.y) as i32,
        f32::from(bounds.size.width) as i32,
        f32::from(bounds.size.height) as i32,
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

/// Focus the editor window with the given window_id (raises it to front).
#[derive(Debug, Clone, Default, Serialize, JsonSchema)]
pub struct FocusWindowParams {
    pub window_id: String,
}

impl<'de> Deserialize<'de> for FocusWindowParams {
    fn deserialize<D: Deserializer<'de>>(de: D) -> Result<Self, D::Error> {
        #[derive(Deserialize, Default)]
        #[serde(default, deny_unknown_fields)]
        struct Inner {
            window_id: String,
        }
        let inner = Option::<Inner>::deserialize(de)?.unwrap_or_default();
        Ok(FocusWindowParams {
            window_id: inner.window_id,
        })
    }
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct FocusWindowResult {
    pub focused: bool,
}

#[derive(Clone)]
pub struct FocusWindowTool;

impl McpServerTool for FocusWindowTool {
    type Input = FocusWindowParams;
    type Output = FocusWindowResult;
    const NAME: &'static str = "windows.focus";

    async fn run(
        &self,
        input: Self::Input,
        cx: &mut AsyncApp,
    ) -> anyhow::Result<ToolResponse<Self::Output>> {
        let focused = cx.update(|cx| -> anyhow::Result<bool> {
            let handle = find_window_by_id(&input.window_id, cx)?;
            // `AnyWindowHandle::update` requires the window to still exist; if
            // it has been closed concurrently we surface that to the caller.
            handle
                .update(cx, |_view, window, _cx| window.activate_window())
                .map_err(|err| anyhow::anyhow!("activate_window failed: {err}"))?;
            Ok(true)
        })?;
        Ok(ToolResponse {
            content: vec![ToolResponseContent::Text {
                text: format!("focused: {focused}"),
            }],
            structured_content: FocusWindowResult { focused },
        })
    }
}

/// Close the editor window with the given window_id.
#[derive(Debug, Clone, Default, Serialize, JsonSchema)]
pub struct CloseWindowParams {
    pub window_id: String,
}

impl<'de> Deserialize<'de> for CloseWindowParams {
    fn deserialize<D: Deserializer<'de>>(de: D) -> Result<Self, D::Error> {
        #[derive(Deserialize, Default)]
        #[serde(default, deny_unknown_fields)]
        struct Inner {
            window_id: String,
        }
        let inner = Option::<Inner>::deserialize(de)?.unwrap_or_default();
        Ok(CloseWindowParams {
            window_id: inner.window_id,
        })
    }
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct CloseWindowResult {
    pub closed: bool,
}

#[derive(Clone)]
pub struct CloseWindowTool;

impl McpServerTool for CloseWindowTool {
    type Input = CloseWindowParams;
    type Output = CloseWindowResult;
    const NAME: &'static str = "windows.close";

    async fn run(
        &self,
        input: Self::Input,
        cx: &mut AsyncApp,
    ) -> anyhow::Result<ToolResponse<Self::Output>> {
        let closed = cx.update(|cx| -> anyhow::Result<bool> {
            let handle = find_window_by_id(&input.window_id, cx)?;
            // `Window::remove_window` flips the `removed` flag; the window is
            // actually torn down on the next platform tick. Failure here means
            // the handle is stale (window already gone).
            handle
                .update(cx, |_view, window, _cx| window.remove_window())
                .map_err(|err| anyhow::anyhow!("remove_window failed: {err}"))?;
            Ok(true)
        })?;
        Ok(ToolResponse {
            content: vec![ToolResponseContent::Text {
                text: format!("closed: {closed}"),
            }],
            structured_content: CloseWindowResult { closed },
        })
    }
}

/// Dispatch a registered action to the window with the given window_id.
///
/// Action name is the fully-qualified path like `workspace::ToggleLeftDock`.
/// Optional `args` are deserialized into the action's payload type.
///
/// Note: returns `dispatched: true` once the action was successfully built
/// and queued onto the window's dispatcher. The dispatch itself runs on a
/// later tick; this tool does NOT report whether a handler eventually
/// fired or refused the action.
#[derive(Debug, Clone, Default, Serialize, JsonSchema)]
pub struct DispatchActionParams {
    pub window_id: String,
    pub action_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub args: Option<serde_json::Value>,
}

impl<'de> Deserialize<'de> for DispatchActionParams {
    fn deserialize<D: Deserializer<'de>>(de: D) -> Result<Self, D::Error> {
        #[derive(Deserialize, Default)]
        #[serde(default, deny_unknown_fields)]
        struct Inner {
            window_id: String,
            action_name: String,
            #[serde(default)]
            args: Option<serde_json::Value>,
        }
        let inner = Option::<Inner>::deserialize(de)?.unwrap_or_default();
        Ok(DispatchActionParams {
            window_id: inner.window_id,
            action_name: inner.action_name,
            args: inner.args,
        })
    }
}

/// Result of `windows.dispatch_action`. `dispatched` indicates the action
/// was built and queued, NOT that a handler subsequently fired.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct DispatchActionResult {
    pub dispatched: bool,
}

#[derive(Clone)]
pub struct DispatchActionTool;

impl McpServerTool for DispatchActionTool {
    type Input = DispatchActionParams;
    type Output = DispatchActionResult;
    const NAME: &'static str = "windows.dispatch_action";

    async fn run(
        &self,
        input: Self::Input,
        cx: &mut AsyncApp,
    ) -> anyhow::Result<ToolResponse<Self::Output>> {
        let action_name = input.action_name.clone();
        let dispatched = cx.update(|cx| -> anyhow::Result<bool> {
            let handle = find_window_by_id(&input.window_id, cx)?;
            // Build the action up-front so a deserialization error surfaces
            // before we touch the window. Once built, dispatch is infallible
            // — the window itself routes the action through its keybinding /
            // focus tree.
            let action = cx
                .build_action(&input.action_name, input.args.clone())
                .map_err(|err| anyhow::anyhow!("build_action({}): {err}", input.action_name))?;
            handle
                .update(cx, |_view, window, cx| {
                    window.dispatch_action(action, cx);
                })
                .map_err(|err| anyhow::anyhow!("dispatch_action failed: {err}"))?;
            Ok(true)
        })?;
        Ok(ToolResponse {
            content: vec![ToolResponseContent::Text {
                text: format!("dispatched: {action_name} ({dispatched})"),
            }],
            structured_content: DispatchActionResult { dispatched },
        })
    }
}

fn find_window_by_id(
    window_id: &str,
    cx: &mut App,
) -> anyhow::Result<gpui::AnyWindowHandle> {
    // Mirror the iteration order used by `windows.list`: prefer Z-ordered
    // stack, fall back to the unstable slot-map iteration so both tools
    // observe the same set of handles.
    let candidates = cx.window_stack().unwrap_or_else(|| cx.windows());
    for handle in candidates {
        if crate::window_ids::format(handle.window_id()) == window_id {
            return Ok(handle);
        }
    }
    anyhow::bail!("window_not_found: {window_id}");
}
