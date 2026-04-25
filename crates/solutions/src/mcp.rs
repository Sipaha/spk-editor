//! MCP tools exposed by the `solutions` crate. Tools register with the
//! central `editor_mcp` registry from `solutions::init` so that
//! `start_server` (called later from `crates/zed/src/main.rs`) sees them
//! when binding the socket.
use crate::{Solution, SolutionStore};
use anyhow::{Context as _, Result};
use context_server::listener::{McpServerTool, ToolResponse};
use context_server::types::ToolResponseContent;
use gpui::{App, AsyncApp};
use schemars::JsonSchema;
use serde::{Deserialize, Deserializer, Serialize};
use util::ResultExt as _;

pub fn register(cx: &mut App) {
    editor_mcp::register_tool(cx, |server| {
        server.add_tool(ListSolutionsTool);
    });
    editor_mcp::register_tool(cx, |server| {
        server.add_tool(GetSolutionTool);
    });
    editor_mcp::register_tool(cx, |server| {
        server.add_tool(CreateSolutionTool);
    });
    editor_mcp::register_tool(cx, |server| {
        server.add_tool(RenameSolutionTool);
    });
    editor_mcp::register_tool(cx, |server| {
        server.add_tool(DeleteSolutionTool);
    });
    editor_mcp::register_tool(cx, |server| {
        server.add_tool(OpenSolutionTool);
    });
    editor_mcp::register_tool(cx, |server| {
        server.add_tool(CloseSolutionTool);
    });
}

// =====================================================================
// solutions.list
// =====================================================================

/// List all configured Solutions with summary metadata.
#[derive(Debug, Clone, Default, Serialize, JsonSchema)]
pub struct ListSolutionsParams {}

impl<'de> Deserialize<'de> for ListSolutionsParams {
    fn deserialize<D: Deserializer<'de>>(de: D) -> Result<Self, D::Error> {
        let _ = serde::de::IgnoredAny::deserialize(de)?;
        Ok(ListSolutionsParams {})
    }
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct SolutionSummary {
    pub id: String,
    pub name: String,
    pub root: String,
    pub member_count: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_opened_at: Option<String>,
    pub window_open: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub main_window_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct ListSolutionsResult {
    pub solutions: Vec<SolutionSummary>,
}

#[derive(Clone)]
pub struct ListSolutionsTool;

impl McpServerTool for ListSolutionsTool {
    type Input = ListSolutionsParams;
    type Output = ListSolutionsResult;
    const NAME: &'static str = "solutions.list";

    async fn run(
        &self,
        _input: Self::Input,
        cx: &mut AsyncApp,
    ) -> anyhow::Result<ToolResponse<Self::Output>> {
        let summaries = cx.update(|cx| {
            let store = SolutionStore::global(cx);
            let solutions = store
                .read_with(cx, |store, _| store.solutions().to_vec());
            solutions
                .iter()
                .map(|sol| build_summary(sol, cx))
                .collect::<Vec<_>>()
        });
        Ok(ToolResponse {
            content: vec![ToolResponseContent::Text {
                text: format!("{} solution(s)", summaries.len()),
            }],
            structured_content: ListSolutionsResult {
                solutions: summaries,
            },
        })
    }
}

fn build_summary(sol: &Solution, cx: &App) -> SolutionSummary {
    let main_window_id = find_window_id_for_solution(&sol.root, cx);
    SolutionSummary {
        id: sol.id.as_str().to_string(),
        name: sol.name.clone(),
        root: sol.root.to_string_lossy().into_owned(),
        member_count: sol.members.len(),
        last_opened_at: sol.last_opened_at.map(|t| t.to_rfc3339()),
        window_open: main_window_id.is_some(),
        main_window_id,
    }
}

fn find_window_id_for_solution(solution_root: &std::path::Path, cx: &App) -> Option<String> {
    for handle in cx.windows() {
        let Some(window) = handle.downcast::<workspace::MultiWorkspace>() else {
            continue;
        };
        let matches = window
            .read_with(cx, |multi, cx| {
                multi.workspaces().any(|ws| {
                    ws.read(cx)
                        .project()
                        .read(cx)
                        .visible_worktrees(cx)
                        .any(|tree| tree.read(cx).abs_path().starts_with(solution_root))
                })
            })
            .ok()
            .unwrap_or(false);
        if matches {
            return Some(format_window_id(handle.window_id()));
        }
    }
    None
}

// Inline window-id formatting helper. Mirrors what `editor_mcp::window_ids::format`
// produces (`window:<u64>`); duplicated here because `solutions` cannot reach
// `editor_mcp`'s private modules. Both must agree on format — if
// `editor_mcp::window_ids` ever changes its format, this needs to track.
fn format_window_id(id: gpui::WindowId) -> String {
    format!("window:{}", id.as_u64())
}

// =====================================================================
// solutions.get
// =====================================================================

/// Get full details of a Solution by id, including any active window info.
#[derive(Debug, Clone, Default, Serialize, JsonSchema)]
pub struct GetSolutionParams {
    pub solution_id: String,
}

impl<'de> Deserialize<'de> for GetSolutionParams {
    fn deserialize<D: Deserializer<'de>>(de: D) -> Result<Self, D::Error> {
        #[derive(Deserialize, Default)]
        #[serde(default, deny_unknown_fields)]
        struct Inner {
            solution_id: String,
        }
        let inner = Option::<Inner>::deserialize(de)?.unwrap_or_default();
        Ok(Self {
            solution_id: inner.solution_id,
        })
    }
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct SolutionDetail {
    pub id: String,
    pub name: String,
    pub root: String,
    pub members: Vec<MemberDetail>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_opened_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct MemberDetail {
    pub catalog_id: String,
    pub local_path: String,
    pub status: String, // "ok" | "missing_on_disk"
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct WindowDetail {
    pub window_id: String,
    pub focused: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub active_buffer: Option<String>,
    pub worktree_paths: Vec<String>,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct GetSolutionResult {
    pub solution: SolutionDetail,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub window: Option<WindowDetail>,
}

#[derive(Clone)]
pub struct GetSolutionTool;

impl McpServerTool for GetSolutionTool {
    type Input = GetSolutionParams;
    type Output = GetSolutionResult;
    const NAME: &'static str = "solutions.get";

    async fn run(
        &self,
        input: Self::Input,
        cx: &mut AsyncApp,
    ) -> anyhow::Result<ToolResponse<Self::Output>> {
        let (detail, root) = cx.update(|cx| -> Result<(SolutionDetail, std::path::PathBuf)> {
            let store = SolutionStore::global(cx);
            store.read_with(cx, |s, _| {
                s.solutions()
                    .iter()
                    .find(|sol| sol.id.as_str() == input.solution_id)
                    .map(|sol| (build_detail(sol), sol.root.clone()))
                    .with_context(|| format!("solution_not_found: {}", input.solution_id))
            })
        })?;

        let window = cx.update(|cx| build_window_detail(&root, cx));

        Ok(ToolResponse {
            content: vec![ToolResponseContent::Text {
                text: detail.name.clone(),
            }],
            structured_content: GetSolutionResult {
                solution: detail,
                window,
            },
        })
    }
}

fn build_detail(sol: &Solution) -> SolutionDetail {
    SolutionDetail {
        id: sol.id.as_str().to_string(),
        name: sol.name.clone(),
        root: sol.root.to_string_lossy().into_owned(),
        members: sol
            .members
            .iter()
            .map(|m| {
                let exists = m.local_path.exists();
                MemberDetail {
                    catalog_id: m.catalog_id.as_str().to_string(),
                    local_path: m.local_path.to_string_lossy().into_owned(),
                    status: if exists { "ok" } else { "missing_on_disk" }.to_string(),
                }
            })
            .collect(),
        last_opened_at: sol.last_opened_at.map(|t| t.to_rfc3339()),
    }
}

fn build_window_detail(solution_root: &std::path::Path, cx: &mut App) -> Option<WindowDetail> {
    let active_window_id = cx.active_window().map(|h| h.window_id());
    for handle in cx.windows() {
        let Some(window) = handle.downcast::<workspace::MultiWorkspace>() else {
            continue;
        };
        let detail = window
            .update(cx, |multi, _window, cx| {
                let mut worktree_paths: Vec<String> = Vec::new();
                let mut active_buffer: Option<String> = None;
                let mut matches = false;

                for ws in multi.workspaces() {
                    let workspace = ws.read(cx);
                    let project = workspace.project().read(cx);
                    for tree in project.visible_worktrees(cx) {
                        let p = tree.read(cx).abs_path().to_string_lossy().into_owned();
                        if std::path::Path::new(&p).starts_with(solution_root) {
                            matches = true;
                        }
                        worktree_paths.push(p);
                    }
                    if active_buffer.is_none() {
                        active_buffer = workspace
                            .active_item(cx)
                            .and_then(|item| item.project_path(cx))
                            .map(|pp| pp.path.as_unix_str().to_string());
                    }
                }

                if !matches {
                    return None;
                }

                Some(WindowDetail {
                    window_id: format_window_id(handle.window_id()),
                    focused: active_window_id == Some(handle.window_id()),
                    active_buffer,
                    worktree_paths,
                })
            })
            .ok()
            .flatten();
        if detail.is_some() {
            return detail;
        }
    }
    None
}

// =====================================================================
// solutions.create
// =====================================================================

/// Create a new empty Solution. Generates a slug from `name`, creates the
/// on-disk root directory under `SolutionsSettings::root`, persists the new
/// entry. Returns the assigned id.
#[derive(Debug, Clone, Default, Serialize, JsonSchema)]
pub struct CreateSolutionParams {
    pub name: String,
}

impl<'de> Deserialize<'de> for CreateSolutionParams {
    fn deserialize<D: Deserializer<'de>>(de: D) -> Result<Self, D::Error> {
        #[derive(Deserialize, Default)]
        #[serde(default, deny_unknown_fields)]
        struct Inner {
            name: String,
        }
        Ok(Self {
            name: Option::<Inner>::deserialize(de)?
                .unwrap_or_default()
                .name,
        })
    }
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct CreateSolutionResult {
    pub solution_id: String,
}

#[derive(Clone)]
pub struct CreateSolutionTool;

impl McpServerTool for CreateSolutionTool {
    type Input = CreateSolutionParams;
    type Output = CreateSolutionResult;
    const NAME: &'static str = "solutions.create";

    async fn run(
        &self,
        input: Self::Input,
        cx: &mut AsyncApp,
    ) -> anyhow::Result<ToolResponse<Self::Output>> {
        anyhow::ensure!(
            !input.name.trim().is_empty(),
            "invalid_params: name is required"
        );
        let id = cx.update(|cx| -> Result<String> {
            use ::settings::Settings as _;
            let store = SolutionStore::global(cx);
            let root_base = crate::SolutionsSettings::get_global(cx).root.clone();
            let id = store.update(cx, |s, cx| s.create_solution(&input.name, root_base, cx))?;
            Ok(id.as_str().to_string())
        })?;
        Ok(ToolResponse {
            content: vec![ToolResponseContent::Text {
                text: format!("created: {id}"),
            }],
            structured_content: CreateSolutionResult { solution_id: id },
        })
    }
}

// =====================================================================
// solutions.rename
// =====================================================================

/// Rename an existing Solution. Mutates `name` only; `id` and on-disk paths
/// are unchanged.
#[derive(Debug, Clone, Default, Serialize, JsonSchema)]
pub struct RenameSolutionParams {
    pub solution_id: String,
    pub new_name: String,
}

impl<'de> Deserialize<'de> for RenameSolutionParams {
    fn deserialize<D: Deserializer<'de>>(de: D) -> Result<Self, D::Error> {
        #[derive(Deserialize, Default)]
        #[serde(default, deny_unknown_fields)]
        struct Inner {
            solution_id: String,
            new_name: String,
        }
        let inner = Option::<Inner>::deserialize(de)?.unwrap_or_default();
        Ok(Self {
            solution_id: inner.solution_id,
            new_name: inner.new_name,
        })
    }
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct RenameSolutionResult {
    pub solution_id: String,
}

#[derive(Clone)]
pub struct RenameSolutionTool;

impl McpServerTool for RenameSolutionTool {
    type Input = RenameSolutionParams;
    type Output = RenameSolutionResult;
    const NAME: &'static str = "solutions.rename";

    async fn run(
        &self,
        input: Self::Input,
        cx: &mut AsyncApp,
    ) -> anyhow::Result<ToolResponse<Self::Output>> {
        anyhow::ensure!(
            !input.solution_id.is_empty(),
            "invalid_params: solution_id is required"
        );
        anyhow::ensure!(
            !input.new_name.trim().is_empty(),
            "invalid_params: new_name is required"
        );
        let solution_id = input.solution_id.clone();
        cx.update(|cx| -> Result<()> {
            let store = SolutionStore::global(cx);
            let id = crate::SolutionId(input.solution_id);
            store.update(cx, |s, cx| s.rename_solution(&id, &input.new_name, cx))?;
            Ok(())
        })?;
        Ok(ToolResponse {
            content: vec![ToolResponseContent::Text {
                text: format!("renamed: {solution_id}"),
            }],
            structured_content: RenameSolutionResult { solution_id },
        })
    }
}

// =====================================================================
// solutions.delete
// =====================================================================

/// Delete a Solution from config. Does NOT touch on-disk directories
/// (Solutions are config-only entities; v1 deliberately leaves orphan
/// directories for the user to clean up).
#[derive(Debug, Clone, Default, Serialize, JsonSchema)]
pub struct DeleteSolutionParams {
    pub solution_id: String,
}

impl<'de> Deserialize<'de> for DeleteSolutionParams {
    fn deserialize<D: Deserializer<'de>>(de: D) -> Result<Self, D::Error> {
        #[derive(Deserialize, Default)]
        #[serde(default, deny_unknown_fields)]
        struct Inner {
            solution_id: String,
        }
        Ok(Self {
            solution_id: Option::<Inner>::deserialize(de)?
                .unwrap_or_default()
                .solution_id,
        })
    }
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct DeleteSolutionResult {
    pub deleted: bool,
}

#[derive(Clone)]
pub struct DeleteSolutionTool;

impl McpServerTool for DeleteSolutionTool {
    type Input = DeleteSolutionParams;
    type Output = DeleteSolutionResult;
    const NAME: &'static str = "solutions.delete";

    async fn run(
        &self,
        input: Self::Input,
        cx: &mut AsyncApp,
    ) -> anyhow::Result<ToolResponse<Self::Output>> {
        anyhow::ensure!(
            !input.solution_id.is_empty(),
            "invalid_params: solution_id is required"
        );
        cx.update(|cx| -> Result<()> {
            let store = SolutionStore::global(cx);
            let id = crate::SolutionId(input.solution_id);
            store.update(cx, |s, cx| s.delete_solution(&id, cx))?;
            Ok(())
        })?;
        Ok(ToolResponse {
            content: vec![ToolResponseContent::Text {
                text: "deleted".to_string(),
            }],
            structured_content: DeleteSolutionResult { deleted: true },
        })
    }
}

// =====================================================================
// solutions.open
// =====================================================================

/// Open a Solution: collects member paths, calls `workspace::open_paths`,
/// updates `last_opened_at` (only after a successful open), returns the
/// resulting window info. `focus` is plumbed into `OpenOptions.focus`:
/// `Some(true)` requests focus, `Some(false)` requests no focus, and
/// `None` leaves the workspace's default behaviour intact.
#[derive(Debug, Clone, Default, Serialize, JsonSchema)]
pub struct OpenSolutionParams {
    pub solution_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub focus: Option<bool>,
}

impl<'de> Deserialize<'de> for OpenSolutionParams {
    fn deserialize<D: Deserializer<'de>>(de: D) -> Result<Self, D::Error> {
        #[derive(Deserialize, Default)]
        #[serde(default, deny_unknown_fields)]
        struct Inner {
            solution_id: String,
            focus: Option<bool>,
        }
        let inner = Option::<Inner>::deserialize(de)?.unwrap_or_default();
        Ok(Self {
            solution_id: inner.solution_id,
            focus: inner.focus,
        })
    }
}

/// Result of `solutions.open`. `focused` reflects the FOCUS REQUEST sent to
/// the workspace (`input.focus.unwrap_or(true)`); the OS may not honor it on
/// all platforms, and we cannot synchronously observe the resulting OS
/// focus state, so the value is the request, not the post-condition.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct OpenSolutionResult {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub window_id: Option<String>,
    pub focused: bool,
    pub opened_paths: Vec<String>,
}

#[derive(Clone)]
pub struct OpenSolutionTool;

impl McpServerTool for OpenSolutionTool {
    type Input = OpenSolutionParams;
    type Output = OpenSolutionResult;
    const NAME: &'static str = "solutions.open";

    async fn run(
        &self,
        input: Self::Input,
        cx: &mut AsyncApp,
    ) -> anyhow::Result<ToolResponse<Self::Output>> {
        anyhow::ensure!(
            !input.solution_id.is_empty(),
            "invalid_params: solution_id is required"
        );
        let sol_id = crate::SolutionId(input.solution_id.clone());

        let paths = cx.update(|cx| -> Result<Vec<std::path::PathBuf>> {
            let store = SolutionStore::global(cx);
            store.read_with(cx, |s, _| s.paths_for_open(&sol_id))
        })?;

        anyhow::ensure!(
            !paths.is_empty(),
            "solution {} has no members",
            input.solution_id
        );

        // Open first; only stamp last_opened_at after the open actually
        // succeeds, so a failed open does not lie about recency.
        let task = cx.update(|cx| {
            let app_state = workspace::AppState::global(cx);
            let mut options = workspace::OpenOptions::default();
            options.focus = input.focus;
            workspace::open_paths(&paths, app_state, options, cx)
        });
        let open_result = task.await?;
        let window_id = format_window_id(open_result.window.window_id());

        // Persist failure here is non-fatal: the open already happened and the
        // user should see a window even if we lose the recency update.
        cx.update(|cx| {
            let store = SolutionStore::global(cx);
            store
                .update(cx, |s, cx| s.touch_last_opened(&sol_id, cx))
                .log_err();
        });

        let focused = input.focus.unwrap_or(true);

        let opened_paths: Vec<String> = paths
            .iter()
            .map(|p| p.to_string_lossy().into_owned())
            .collect();

        Ok(ToolResponse {
            content: vec![ToolResponseContent::Text {
                text: format!("opened: {}", input.solution_id),
            }],
            structured_content: OpenSolutionResult {
                window_id: Some(window_id),
                focused,
                opened_paths,
            },
        })
    }
}

// =====================================================================
// solutions.close
// =====================================================================

/// Close the editor window currently displaying the given Solution, if any.
/// Returns `closed: false` if no window matches (not an error).
///
/// **Warning**: forces close — does NOT prompt the user to save unsaved
/// buffers. Callers should ensure modifications are saved beforehand.
#[derive(Debug, Clone, Default, Serialize, JsonSchema)]
pub struct CloseSolutionParams {
    pub solution_id: String,
}

impl<'de> Deserialize<'de> for CloseSolutionParams {
    fn deserialize<D: Deserializer<'de>>(de: D) -> Result<Self, D::Error> {
        #[derive(Deserialize, Default)]
        #[serde(default, deny_unknown_fields)]
        struct Inner {
            solution_id: String,
        }
        Ok(Self {
            solution_id: Option::<Inner>::deserialize(de)?
                .unwrap_or_default()
                .solution_id,
        })
    }
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct CloseSolutionResult {
    pub closed: bool,
}

#[derive(Clone)]
pub struct CloseSolutionTool;

impl McpServerTool for CloseSolutionTool {
    type Input = CloseSolutionParams;
    type Output = CloseSolutionResult;
    const NAME: &'static str = "solutions.close";

    async fn run(
        &self,
        input: Self::Input,
        cx: &mut AsyncApp,
    ) -> anyhow::Result<ToolResponse<Self::Output>> {
        anyhow::ensure!(
            !input.solution_id.is_empty(),
            "invalid_params: solution_id is required"
        );
        let closed = cx.update(|cx| -> Result<bool> {
            let store = SolutionStore::global(cx);
            let root = store
                .read_with(cx, |s, _| {
                    s.solutions()
                        .iter()
                        .find(|sol| sol.id.as_str() == input.solution_id)
                        .map(|sol| sol.root.clone())
                })
                .with_context(|| format!("solution_not_found: {}", input.solution_id))?;
            for handle in cx.windows() {
                let Some(window) = handle.downcast::<workspace::MultiWorkspace>() else {
                    continue;
                };
                let matched = window
                    .read_with(cx, |multi, cx| {
                        multi.workspaces().any(|ws| {
                            ws.read(cx)
                                .project()
                                .read(cx)
                                .visible_worktrees(cx)
                                .any(|tree| tree.read(cx).abs_path().starts_with(&root))
                        })
                    })
                    .ok()
                    .unwrap_or(false);
                if matched {
                    window
                        .update(cx, |_view, window, _cx| window.remove_window())
                        .log_err();
                    return Ok(true);
                }
            }
            Ok(false)
        })?;
        Ok(ToolResponse {
            content: vec![ToolResponseContent::Text {
                text: format!("closed: {closed}"),
            }],
            structured_content: CloseSolutionResult { closed },
        })
    }
}

// =====================================================================
// Tests
// =====================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::SolutionStore;
    use gpui::TestAppContext;
    use tempfile::tempdir;

    #[gpui::test]
    async fn list_returns_empty_when_store_empty(cx: &mut TestAppContext) {
        let dir = tempdir().expect("tempdir");
        let store = cx.update(|cx| SolutionStore::for_test(dir.path().join("c.json"), cx));
        cx.update(|cx| crate::store::install_global_for_test(store, cx));

        let response = cx
            .update(|cx| {
                let tool = ListSolutionsTool;
                cx.spawn(async move |cx| tool.run(ListSolutionsParams {}, cx).await)
            })
            .await
            .expect("run task");

        assert_eq!(response.structured_content.solutions.len(), 0);
    }

    #[gpui::test]
    async fn list_returns_created_solutions(cx: &mut TestAppContext) {
        let dir = tempdir().expect("tempdir");
        let store = cx.update(|cx| SolutionStore::for_test(dir.path().join("c.json"), cx));
        cx.update(|cx| crate::store::install_global_for_test(store.clone(), cx));

        store
            .update(cx, |s, cx| {
                s.create_solution("Test Sol", dir.path().to_path_buf(), cx)
            })
            .expect("create");

        let response = cx
            .update(|cx| {
                let tool = ListSolutionsTool;
                cx.spawn(async move |cx| tool.run(ListSolutionsParams {}, cx).await)
            })
            .await
            .expect("run task");

        let arr = response.structured_content.solutions;
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0].name, "Test Sol");
        assert_eq!(arr[0].member_count, 0);
        assert!(!arr[0].window_open);
    }

    #[test]
    fn list_params_deserialize_from_null() {
        let _: ListSolutionsParams =
            serde_json::from_value(serde_json::Value::Null).expect("null");
    }

    #[test]
    fn get_params_round_trip() {
        let p: GetSolutionParams = serde_json::from_value(serde_json::json!({
            "solution_id": "demo"
        }))
        .expect("parse");
        assert_eq!(p.solution_id, "demo");
    }

    #[test]
    fn get_params_accepts_null() {
        let p: GetSolutionParams =
            serde_json::from_value(serde_json::Value::Null).expect("null");
        assert!(p.solution_id.is_empty());
    }

    #[test]
    fn create_params_round_trip() {
        let p: CreateSolutionParams =
            serde_json::from_value(serde_json::json!({"name": "Demo"})).expect("parse");
        assert_eq!(p.name, "Demo");
    }

    #[test]
    fn create_params_accepts_null() {
        let p: CreateSolutionParams =
            serde_json::from_value(serde_json::Value::Null).expect("null");
        assert!(p.name.is_empty());
    }

    #[test]
    fn rename_params_round_trip() {
        let p: RenameSolutionParams = serde_json::from_value(serde_json::json!({
            "solution_id": "demo",
            "new_name": "Renamed"
        }))
        .expect("parse");
        assert_eq!(p.solution_id, "demo");
        assert_eq!(p.new_name, "Renamed");
    }

    #[test]
    fn delete_params_round_trip() {
        let p: DeleteSolutionParams = serde_json::from_value(serde_json::json!({
            "solution_id": "demo"
        }))
        .expect("parse");
        assert_eq!(p.solution_id, "demo");
    }

    #[test]
    fn open_params_with_focus() {
        let p: OpenSolutionParams = serde_json::from_value(serde_json::json!({
            "solution_id": "demo",
            "focus": false
        }))
        .expect("parse");
        assert_eq!(p.solution_id, "demo");
        assert_eq!(p.focus, Some(false));
    }

    #[test]
    fn close_params_round_trip() {
        let p: CloseSolutionParams = serde_json::from_value(serde_json::json!({
            "solution_id": "demo"
        }))
        .expect("parse");
        assert_eq!(p.solution_id, "demo");
    }

    // NOTE: live-runner test for `solutions.create` requires a `SettingsStore`
    // (the tool reads `root` from `SolutionsSettings::get_global`). Setting
    // that up here is gnarly; the create path is exercised end-to-end in the
    // Phase 8 integration tests where a real editor `App` is available.
    // `rename` and `delete` go through the store directly and need no
    // settings, so we cover them here.

    #[gpui::test]
    async fn rename_solution_updates_store(cx: &mut TestAppContext) {
        let dir = tempdir().expect("tempdir");
        let store = cx.update(|cx| SolutionStore::for_test(dir.path().join("c.json"), cx));
        cx.update(|cx| crate::store::install_global_for_test(store.clone(), cx));

        let sol_id = store
            .update(cx, |s, cx| {
                s.create_solution("Original", dir.path().to_path_buf(), cx)
            })
            .expect("create");

        let response = cx
            .update(|cx| {
                let tool = RenameSolutionTool;
                let id = sol_id.as_str().to_string();
                cx.spawn(async move |cx| {
                    tool.run(
                        RenameSolutionParams {
                            solution_id: id,
                            new_name: "New Name".into(),
                        },
                        cx,
                    )
                    .await
                })
            })
            .await
            .expect("run task");

        assert_eq!(response.structured_content.solution_id, sol_id.as_str());

        let new_name = store.read_with(cx, |s, _| {
            s.solutions()
                .iter()
                .find(|sol| sol.id == sol_id)
                .map(|sol| sol.name.clone())
        });
        assert_eq!(new_name, Some("New Name".to_string()));
    }

    #[gpui::test]
    async fn delete_solution_removes_from_store(cx: &mut TestAppContext) {
        let dir = tempdir().expect("tempdir");
        let store = cx.update(|cx| SolutionStore::for_test(dir.path().join("c.json"), cx));
        cx.update(|cx| crate::store::install_global_for_test(store.clone(), cx));

        let sol_id = store
            .update(cx, |s, cx| {
                s.create_solution("Demo", dir.path().to_path_buf(), cx)
            })
            .expect("create");

        let response = cx
            .update(|cx| {
                let tool = DeleteSolutionTool;
                let id = sol_id.as_str().to_string();
                cx.spawn(async move |cx| {
                    tool.run(DeleteSolutionParams { solution_id: id }, cx).await
                })
            })
            .await
            .expect("run task");

        assert!(response.structured_content.deleted);
        let count = store.read_with(cx, |s, _| s.solutions().len());
        assert_eq!(count, 0);
    }
}
