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
    editor_mcp::register_tool(cx, |server| {
        server.add_tool(ListCatalogTool);
    });
    editor_mcp::register_tool(cx, |server| {
        server.add_tool(AddCatalogProjectTool);
    });
    editor_mcp::register_tool(cx, |server| {
        server.add_tool(RemoveCatalogProjectTool);
    });
    editor_mcp::register_tool(cx, |server| {
        server.add_tool(EditCatalogProjectTool);
    });
    editor_mcp::register_tool(cx, |server| {
        server.add_tool(RefreshCacheTool);
    });
    editor_mcp::register_tool(cx, |server| {
        server.add_tool(AddMemberTool);
    });
    editor_mcp::register_tool(cx, |server| {
        server.add_tool(RemoveMemberTool);
    });
    editor_mcp::register_tool(cx, |server| {
        server.add_tool(ReorderMembersTool);
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
            return Some(editor_mcp::format_window_id(handle.window_id()));
        }
    }
    None
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
                    window_id: editor_mcp::format_window_id(handle.window_id()),
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
        let window_id = editor_mcp::format_window_id(open_result.window.window_id());

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
// catalog.list
// =====================================================================

/// List all catalog entries with their on-disk cache status.
#[derive(Debug, Clone, Default, Serialize, JsonSchema)]
pub struct ListCatalogParams {}

impl<'de> Deserialize<'de> for ListCatalogParams {
    fn deserialize<D: Deserializer<'de>>(de: D) -> Result<Self, D::Error> {
        let _ = serde::de::IgnoredAny::deserialize(de)?;
        Ok(ListCatalogParams {})
    }
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct CatalogProjectInfo {
    pub id: String,
    pub name: String,
    pub remote_url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_branch: Option<String>,
    /// `"absent"` when no cache directory exists, `"present"` when one does.
    pub cache_status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_last_fetched: Option<String>,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct ListCatalogResult {
    pub projects: Vec<CatalogProjectInfo>,
}

#[derive(Clone)]
pub struct ListCatalogTool;

impl McpServerTool for ListCatalogTool {
    type Input = ListCatalogParams;
    type Output = ListCatalogResult;
    const NAME: &'static str = "catalog.list";

    async fn run(
        &self,
        _input: Self::Input,
        cx: &mut AsyncApp,
    ) -> anyhow::Result<ToolResponse<Self::Output>> {
        let projects: Vec<CatalogProjectInfo> = cx.update(|cx| {
            let store = SolutionStore::global(cx);
            let cache_root = crate::default_cache_root();
            store.read_with(cx, |s, _| {
                s.catalog()
                    .iter()
                    .map(|p| build_catalog_info(p, &cache_root))
                    .collect()
            })
        });
        Ok(ToolResponse {
            content: vec![ToolResponseContent::Text {
                text: format!("{} project(s)", projects.len()),
            }],
            structured_content: ListCatalogResult { projects },
        })
    }
}

fn build_catalog_info(
    p: &crate::CatalogProject,
    cache_root: &std::path::Path,
) -> CatalogProjectInfo {
    let entry_path = crate::cache::cache_path(cache_root, &p.remote_url);
    let exists = entry_path.exists();
    let cache_last_fetched = if exists {
        std::fs::metadata(&entry_path)
            .and_then(|m| m.modified())
            .ok()
            .map(|t| chrono::DateTime::<chrono::Utc>::from(t).to_rfc3339())
    } else {
        None
    };
    CatalogProjectInfo {
        id: p.id.as_str().to_string(),
        name: p.name.clone(),
        remote_url: p.remote_url.clone(),
        default_branch: p.default_branch.clone(),
        cache_status: if exists { "present" } else { "absent" }.to_string(),
        cache_last_fetched,
    }
}

// =====================================================================
// catalog.add_project
// =====================================================================

/// Add a new catalog entry. The id is derived from `name` (slug) and is
/// returned in `catalog_id`. `remote_url` is immutable after creation.
#[derive(Debug, Clone, Default, Serialize, JsonSchema)]
pub struct AddCatalogProjectParams {
    pub name: String,
    pub remote_url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_branch: Option<String>,
}

impl<'de> Deserialize<'de> for AddCatalogProjectParams {
    fn deserialize<D: Deserializer<'de>>(de: D) -> Result<Self, D::Error> {
        #[derive(Deserialize, Default)]
        #[serde(default, deny_unknown_fields)]
        struct Inner {
            name: String,
            remote_url: String,
            default_branch: Option<String>,
        }
        let inner = Option::<Inner>::deserialize(de)?.unwrap_or_default();
        Ok(Self {
            name: inner.name,
            remote_url: inner.remote_url,
            default_branch: inner.default_branch,
        })
    }
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct AddCatalogProjectResult {
    pub catalog_id: String,
}

#[derive(Clone)]
pub struct AddCatalogProjectTool;

impl McpServerTool for AddCatalogProjectTool {
    type Input = AddCatalogProjectParams;
    type Output = AddCatalogProjectResult;
    const NAME: &'static str = "catalog.add_project";

    async fn run(
        &self,
        input: Self::Input,
        cx: &mut AsyncApp,
    ) -> anyhow::Result<ToolResponse<Self::Output>> {
        anyhow::ensure!(
            !input.name.trim().is_empty(),
            "invalid_params: name is required"
        );
        anyhow::ensure!(
            !input.remote_url.trim().is_empty(),
            "invalid_params: remote_url is required"
        );
        let id = cx.update(|cx| -> Result<String> {
            let store = SolutionStore::global(cx);
            let id = store.update(cx, |s, cx| {
                s.add_catalog_project(
                    &input.name,
                    &input.remote_url,
                    input.default_branch.clone(),
                    cx,
                )
            })?;
            Ok(id.as_str().to_string())
        })?;
        Ok(ToolResponse {
            content: vec![ToolResponseContent::Text {
                text: format!("added: {id}"),
            }],
            structured_content: AddCatalogProjectResult { catalog_id: id },
        })
    }
}

// =====================================================================
// catalog.remove_project
// =====================================================================

/// Remove a catalog entry. Refused (with an error) if any Solution still
/// references it; remove the member from the Solution(s) first.
#[derive(Debug, Clone, Default, Serialize, JsonSchema)]
pub struct RemoveCatalogProjectParams {
    pub catalog_id: String,
}

impl<'de> Deserialize<'de> for RemoveCatalogProjectParams {
    fn deserialize<D: Deserializer<'de>>(de: D) -> Result<Self, D::Error> {
        #[derive(Deserialize, Default)]
        #[serde(default, deny_unknown_fields)]
        struct Inner {
            catalog_id: String,
        }
        Ok(Self {
            catalog_id: Option::<Inner>::deserialize(de)?
                .unwrap_or_default()
                .catalog_id,
        })
    }
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct RemoveCatalogProjectResult {
    pub removed: bool,
}

#[derive(Clone)]
pub struct RemoveCatalogProjectTool;

impl McpServerTool for RemoveCatalogProjectTool {
    type Input = RemoveCatalogProjectParams;
    type Output = RemoveCatalogProjectResult;
    const NAME: &'static str = "catalog.remove_project";

    async fn run(
        &self,
        input: Self::Input,
        cx: &mut AsyncApp,
    ) -> anyhow::Result<ToolResponse<Self::Output>> {
        anyhow::ensure!(
            !input.catalog_id.is_empty(),
            "invalid_params: catalog_id is required"
        );
        cx.update(|cx| -> Result<()> {
            let store = SolutionStore::global(cx);
            let id = crate::CatalogId(input.catalog_id);
            store.update(cx, |s, cx| s.remove_catalog_project(&id, cx))?;
            Ok(())
        })?;
        Ok(ToolResponse {
            content: vec![ToolResponseContent::Text {
                text: "removed".to_string(),
            }],
            structured_content: RemoveCatalogProjectResult { removed: true },
        })
    }
}

// =====================================================================
// catalog.edit_project
// =====================================================================

/// Edit `name` and/or `default_branch` of a catalog entry. `remote_url` is
/// immutable in v1; to change it, remove and re-add (a new clone is required).
#[derive(Debug, Clone, Default, Serialize, JsonSchema)]
pub struct EditCatalogProjectParams {
    pub catalog_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_branch: Option<String>,
}

impl<'de> Deserialize<'de> for EditCatalogProjectParams {
    fn deserialize<D: Deserializer<'de>>(de: D) -> Result<Self, D::Error> {
        #[derive(Deserialize, Default)]
        #[serde(default, deny_unknown_fields)]
        struct Inner {
            catalog_id: String,
            name: Option<String>,
            default_branch: Option<String>,
        }
        let inner = Option::<Inner>::deserialize(de)?.unwrap_or_default();
        Ok(Self {
            catalog_id: inner.catalog_id,
            name: inner.name,
            default_branch: inner.default_branch,
        })
    }
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct EditCatalogProjectResult {
    pub catalog_id: String,
}

#[derive(Clone)]
pub struct EditCatalogProjectTool;

impl McpServerTool for EditCatalogProjectTool {
    type Input = EditCatalogProjectParams;
    type Output = EditCatalogProjectResult;
    const NAME: &'static str = "catalog.edit_project";

    async fn run(
        &self,
        input: Self::Input,
        cx: &mut AsyncApp,
    ) -> anyhow::Result<ToolResponse<Self::Output>> {
        anyhow::ensure!(
            !input.catalog_id.is_empty(),
            "invalid_params: catalog_id is required"
        );
        let catalog_id = input.catalog_id.clone();
        cx.update(|cx| -> Result<()> {
            let store = SolutionStore::global(cx);
            let id = crate::CatalogId(input.catalog_id);
            store.update(cx, |s, cx| {
                s.edit_catalog_project(&id, input.name, input.default_branch, cx)
            })?;
            Ok(())
        })?;
        Ok(ToolResponse {
            content: vec![ToolResponseContent::Text {
                text: format!("edited: {catalog_id}"),
            }],
            structured_content: EditCatalogProjectResult { catalog_id },
        })
    }
}

// =====================================================================
// catalog.refresh_cache
// =====================================================================

/// Refresh the on-disk cache for a catalog entry by running `git fetch`
/// (or cloning if the cache is absent). Returns an `operation_id`. Phase 7
/// will wire this into a real operation tracker; today the work runs inline
/// and the id is a deterministic placeholder derived from `catalog_id`.
#[derive(Debug, Clone, Default, Serialize, JsonSchema)]
pub struct RefreshCacheParams {
    pub catalog_id: String,
}

impl<'de> Deserialize<'de> for RefreshCacheParams {
    fn deserialize<D: Deserializer<'de>>(de: D) -> Result<Self, D::Error> {
        #[derive(Deserialize, Default)]
        #[serde(default, deny_unknown_fields)]
        struct Inner {
            catalog_id: String,
        }
        Ok(Self {
            catalog_id: Option::<Inner>::deserialize(de)?
                .unwrap_or_default()
                .catalog_id,
        })
    }
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct RefreshCacheResult {
    pub operation_id: String,
}

#[derive(Clone)]
pub struct RefreshCacheTool;

impl McpServerTool for RefreshCacheTool {
    type Input = RefreshCacheParams;
    type Output = RefreshCacheResult;
    const NAME: &'static str = "catalog.refresh_cache";

    async fn run(
        &self,
        input: Self::Input,
        cx: &mut AsyncApp,
    ) -> anyhow::Result<ToolResponse<Self::Output>> {
        anyhow::ensure!(
            !input.catalog_id.is_empty(),
            "invalid_params: catalog_id is required"
        );
        let remote_url = cx.update(|cx| -> Result<String> {
            let store = SolutionStore::global(cx);
            let url = store.read_with(cx, |s, _| {
                s.catalog()
                    .iter()
                    .find(|p| p.id.as_str() == input.catalog_id)
                    .map(|p| p.remote_url.clone())
            });
            url.with_context(|| format!("catalog_not_found: {}", input.catalog_id))
        })?;

        // Phase 7 placeholder: the real OperationTracker will replace this
        // with an async-tracked id; for now the fetch runs inline.
        let operation_id = format!("op-refresh-{}", input.catalog_id);
        let cache_root = crate::default_cache_root();
        crate::cache::refresh_cache(&cache_root, &remote_url, |_| {})
            .await
            .with_context(|| format!("refresh_cache failed for {}", input.catalog_id))?;

        Ok(ToolResponse {
            content: vec![ToolResponseContent::Text {
                text: format!("refreshed: {}", input.catalog_id),
            }],
            structured_content: RefreshCacheResult { operation_id },
        })
    }
}

// =====================================================================
// solutions.add_member
// =====================================================================

/// Add a catalog project as a member of a Solution. Clones the project into
/// the Solution's root (using cached source if available) and registers it.
/// Returns `operation_id`. Phase 7 will wire a real operation tracker; today
/// the work runs inline and the id is a deterministic placeholder derived
/// from `solution_id`/`catalog_id`.
///
/// **Slow**: cloning can take seconds-to-minutes for large repos.
#[derive(Debug, Clone, Default, Serialize, JsonSchema)]
pub struct AddMemberParams {
    pub solution_id: String,
    pub catalog_id: String,
}

impl<'de> Deserialize<'de> for AddMemberParams {
    fn deserialize<D: Deserializer<'de>>(de: D) -> Result<Self, D::Error> {
        #[derive(Deserialize, Default)]
        #[serde(default, deny_unknown_fields)]
        struct Inner {
            solution_id: String,
            catalog_id: String,
        }
        let inner = Option::<Inner>::deserialize(de)?.unwrap_or_default();
        Ok(Self {
            solution_id: inner.solution_id,
            catalog_id: inner.catalog_id,
        })
    }
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct AddMemberResult {
    pub operation_id: String,
}

#[derive(Clone)]
pub struct AddMemberTool;

impl McpServerTool for AddMemberTool {
    type Input = AddMemberParams;
    type Output = AddMemberResult;
    const NAME: &'static str = "solutions.add_member";

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
            !input.catalog_id.is_empty(),
            "invalid_params: catalog_id is required"
        );

        let sol_id = crate::SolutionId(input.solution_id.clone());
        let cat_id = crate::CatalogId(input.catalog_id.clone());
        let cache_root = crate::default_cache_root();

        // Phase 7 placeholder: the real OperationTracker will replace this
        // with an async-tracked id; for now the clone runs inline.
        let operation_id = format!(
            "op-add-member-{}-{}",
            input.solution_id, input.catalog_id
        );

        let task = cx.update(|cx| {
            let store = SolutionStore::global(cx);
            store.update(cx, |s, cx| s.add_member(sol_id, cat_id, cache_root, cx))
        });
        task.await?;

        Ok(ToolResponse {
            content: vec![ToolResponseContent::Text {
                text: format!("added member: {}/{}", input.solution_id, input.catalog_id),
            }],
            structured_content: AddMemberResult { operation_id },
        })
    }
}

// =====================================================================
// solutions.remove_member
// =====================================================================

/// Remove a member from a Solution. Config-only: the on-disk worktree
/// directory is NOT deleted; the user can re-add later by `add_member`
/// (the existing dir will be reused if origin matches).
#[derive(Debug, Clone, Default, Serialize, JsonSchema)]
pub struct RemoveMemberParams {
    pub solution_id: String,
    pub catalog_id: String,
}

impl<'de> Deserialize<'de> for RemoveMemberParams {
    fn deserialize<D: Deserializer<'de>>(de: D) -> Result<Self, D::Error> {
        #[derive(Deserialize, Default)]
        #[serde(default, deny_unknown_fields)]
        struct Inner {
            solution_id: String,
            catalog_id: String,
        }
        let inner = Option::<Inner>::deserialize(de)?.unwrap_or_default();
        Ok(Self {
            solution_id: inner.solution_id,
            catalog_id: inner.catalog_id,
        })
    }
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct RemoveMemberResult {
    pub removed: bool,
}

#[derive(Clone)]
pub struct RemoveMemberTool;

impl McpServerTool for RemoveMemberTool {
    type Input = RemoveMemberParams;
    type Output = RemoveMemberResult;
    const NAME: &'static str = "solutions.remove_member";

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
            !input.catalog_id.is_empty(),
            "invalid_params: catalog_id is required"
        );
        cx.update(|cx| -> Result<()> {
            let store = SolutionStore::global(cx);
            let sol_id = crate::SolutionId(input.solution_id);
            let cat_id = crate::CatalogId(input.catalog_id);
            store.update(cx, |s, cx| s.remove_member(&sol_id, &cat_id, cx))?;
            Ok(())
        })?;
        Ok(ToolResponse {
            content: vec![ToolResponseContent::Text {
                text: "removed".to_string(),
            }],
            structured_content: RemoveMemberResult { removed: true },
        })
    }
}

// =====================================================================
// solutions.reorder_members
// =====================================================================

/// Reorder Solution members. The new order MUST contain exactly the same
/// catalog_ids as the current member list (same set, different order).
/// Order matters — the first member becomes the agent CWD.
#[derive(Debug, Clone, Default, Serialize, JsonSchema)]
pub struct ReorderMembersParams {
    pub solution_id: String,
    pub ordered_catalog_ids: Vec<String>,
}

impl<'de> Deserialize<'de> for ReorderMembersParams {
    fn deserialize<D: Deserializer<'de>>(de: D) -> Result<Self, D::Error> {
        #[derive(Deserialize, Default)]
        #[serde(default, deny_unknown_fields)]
        struct Inner {
            solution_id: String,
            ordered_catalog_ids: Vec<String>,
        }
        let inner = Option::<Inner>::deserialize(de)?.unwrap_or_default();
        Ok(Self {
            solution_id: inner.solution_id,
            ordered_catalog_ids: inner.ordered_catalog_ids,
        })
    }
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct ReorderMembersResult {
    pub ok: bool,
}

#[derive(Clone)]
pub struct ReorderMembersTool;

impl McpServerTool for ReorderMembersTool {
    type Input = ReorderMembersParams;
    type Output = ReorderMembersResult;
    const NAME: &'static str = "solutions.reorder_members";

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
            let sol_id = crate::SolutionId(input.solution_id);
            let order: Vec<crate::CatalogId> = input
                .ordered_catalog_ids
                .into_iter()
                .map(crate::CatalogId)
                .collect();
            store.update(cx, |s, cx| s.reorder_members(&sol_id, order, cx))?;
            Ok(())
        })?;
        Ok(ToolResponse {
            content: vec![ToolResponseContent::Text {
                text: "reordered".to_string(),
            }],
            structured_content: ReorderMembersResult { ok: true },
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

    #[test]
    fn list_catalog_params_accepts_null() {
        let _: ListCatalogParams =
            serde_json::from_value(serde_json::Value::Null).expect("null");
    }

    #[test]
    fn add_catalog_params_round_trip() {
        let p: AddCatalogProjectParams = serde_json::from_value(serde_json::json!({
            "name": "Demo",
            "remote_url": "git@example.com:demo.git",
            "default_branch": "main"
        }))
        .expect("parse");
        assert_eq!(p.name, "Demo");
        assert_eq!(p.remote_url, "git@example.com:demo.git");
        assert_eq!(p.default_branch.as_deref(), Some("main"));
    }

    #[test]
    fn add_catalog_params_accepts_null() {
        let p: AddCatalogProjectParams =
            serde_json::from_value(serde_json::Value::Null).expect("null");
        assert!(p.name.is_empty());
        assert!(p.remote_url.is_empty());
        assert!(p.default_branch.is_none());
    }

    #[test]
    fn remove_catalog_params_round_trip() {
        let p: RemoveCatalogProjectParams = serde_json::from_value(serde_json::json!({
            "catalog_id": "demo"
        }))
        .expect("parse");
        assert_eq!(p.catalog_id, "demo");
    }

    #[test]
    fn edit_catalog_params_partial() {
        let p: EditCatalogProjectParams = serde_json::from_value(serde_json::json!({
            "catalog_id": "demo",
            "name": "Renamed"
        }))
        .expect("parse");
        assert_eq!(p.catalog_id, "demo");
        assert_eq!(p.name.as_deref(), Some("Renamed"));
        assert!(p.default_branch.is_none());
    }

    #[test]
    fn refresh_cache_params_round_trip() {
        let p: RefreshCacheParams = serde_json::from_value(serde_json::json!({
            "catalog_id": "demo"
        }))
        .expect("parse");
        assert_eq!(p.catalog_id, "demo");
    }

    #[gpui::test]
    async fn add_catalog_project_persists(cx: &mut TestAppContext) {
        let dir = tempdir().expect("tempdir");
        let store = cx.update(|cx| SolutionStore::for_test(dir.path().join("c.json"), cx));
        cx.update(|cx| crate::store::install_global_for_test(store.clone(), cx));

        let response = cx
            .update(|cx| {
                let tool = AddCatalogProjectTool;
                cx.spawn(async move |cx| {
                    tool.run(
                        AddCatalogProjectParams {
                            name: "Demo".into(),
                            remote_url: "git@example.com:demo.git".into(),
                            default_branch: Some("main".into()),
                        },
                        cx,
                    )
                    .await
                })
            })
            .await
            .expect("run task");

        assert_eq!(response.structured_content.catalog_id, "demo");
        let count = store.read_with(cx, |s, _| s.catalog().len());
        assert_eq!(count, 1);
    }

    #[test]
    fn add_member_params_round_trip() {
        let p: AddMemberParams = serde_json::from_value(serde_json::json!({
            "solution_id": "sol",
            "catalog_id": "cat"
        }))
        .expect("parse");
        assert_eq!(p.solution_id, "sol");
        assert_eq!(p.catalog_id, "cat");
    }

    #[test]
    fn remove_member_params_accepts_null() {
        let p: RemoveMemberParams =
            serde_json::from_value(serde_json::Value::Null).expect("null");
        assert!(p.solution_id.is_empty());
        assert!(p.catalog_id.is_empty());
    }

    #[test]
    fn reorder_members_params_round_trip() {
        let p: ReorderMembersParams = serde_json::from_value(serde_json::json!({
            "solution_id": "sol",
            "ordered_catalog_ids": ["a", "b", "c"]
        }))
        .expect("parse");
        assert_eq!(p.solution_id, "sol");
        assert_eq!(p.ordered_catalog_ids, vec!["a", "b", "c"]);
    }

    #[gpui::test]
    async fn remove_member_updates_store(cx: &mut TestAppContext) {
        let dir = tempdir().expect("tempdir");
        let store = cx.update(|cx| SolutionStore::for_test(dir.path().join("c.json"), cx));
        cx.update(|cx| crate::store::install_global_for_test(store.clone(), cx));

        let cat_id = store
            .update(cx, |s, cx| {
                s.add_catalog_project("Demo", "git@x:demo.git", None, cx)
            })
            .expect("add catalog");
        let sol_id = store
            .update(cx, |s, cx| {
                s.create_solution("Sol", dir.path().to_path_buf(), cx)
            })
            .expect("create");
        store.update(cx, |s, _| {
            s.test_force_add_member(&sol_id, &cat_id);
        });

        let count_before = store.read_with(cx, |s, _| {
            s.solutions()
                .iter()
                .find(|sol| sol.id == sol_id)
                .map(|sol| sol.members.len())
                .unwrap_or(0)
        });
        assert_eq!(count_before, 1);

        let response = cx
            .update(|cx| {
                let tool = RemoveMemberTool;
                let solution_id = sol_id.as_str().to_string();
                let catalog_id = cat_id.as_str().to_string();
                cx.spawn(async move |cx| {
                    tool.run(
                        RemoveMemberParams {
                            solution_id,
                            catalog_id,
                        },
                        cx,
                    )
                    .await
                })
            })
            .await
            .expect("run task");

        assert!(response.structured_content.removed);
        let count_after = store.read_with(cx, |s, _| {
            s.solutions()
                .iter()
                .find(|sol| sol.id == sol_id)
                .map(|sol| sol.members.len())
                .unwrap_or(0)
        });
        assert_eq!(count_after, 0);
    }
}
