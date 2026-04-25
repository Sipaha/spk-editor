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
    editor_mcp::register_tool(cx, |server| {
        server.add_tool(ListBuffersTool);
    });
    editor_mcp::register_tool(cx, |server| {
        server.add_tool(GetEffectiveSettingsTool);
    });
    editor_mcp::register_tool(cx, |server| {
        server.add_tool(DispatchActionTool);
    });
    editor_mcp::register_tool(cx, |server| {
        server.add_tool(ScreenshotTool);
    });
    editor_mcp::register_tool(cx, |server| {
        server.add_tool(DumpVisualStructureTool);
    });
    editor_mcp::register_tool(cx, |server| {
        server.add_tool(GetDiagnosticsTool);
    });
    editor_mcp::register_tool(cx, |server| {
        server.add_tool(ListFilesTool);
    });
    editor_mcp::register_tool(cx, |server| {
        server.add_tool(ReadBufferTool);
    });
    editor_mcp::register_tool(cx, |server| {
        server.add_tool(ApplyEditTool);
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
// workspace.list_buffers
// =====================================================================

/// List open buffers in the editor window for a Solution. Each entry
/// reports the project-relative `path`, dirty/focused flags, and (when
/// available) the language name. Buffers from every pane in the window
/// are returned; a single buffer open in multiple panes appears once
/// per pane (matching the editor UI). Returns an empty list when no
/// window is currently open for the Solution.
#[derive(Debug, Clone, Default, Serialize, JsonSchema)]
pub struct ListBuffersParams {
    pub solution_id: String,
}

impl<'de> Deserialize<'de> for ListBuffersParams {
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
pub struct BufferInfo {
    pub path: String,
    pub dirty: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
    pub focused: bool,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct ListBuffersResult {
    pub buffers: Vec<BufferInfo>,
}

#[derive(Clone)]
pub struct ListBuffersTool;

impl McpServerTool for ListBuffersTool {
    type Input = ListBuffersParams;
    type Output = ListBuffersResult;
    const NAME: &'static str = "workspace.list_buffers";

    async fn run(
        &self,
        input: Self::Input,
        cx: &mut AsyncApp,
    ) -> anyhow::Result<ToolResponse<Self::Output>> {
        anyhow::ensure!(
            !input.solution_id.is_empty(),
            "invalid_params: solution_id is required"
        );
        let buffers = cx.update(|cx| collect_buffers(&input.solution_id, cx));
        Ok(ToolResponse {
            content: vec![ToolResponseContent::Text {
                text: format!("{} buffer(s)", buffers.len()),
            }],
            structured_content: ListBuffersResult { buffers },
        })
    }
}

fn collect_buffers(solution_id: &str, cx: &mut App) -> Vec<BufferInfo> {
    let Some(store) = SolutionStore::try_global(cx) else {
        return Vec::new();
    };
    let Some(root) = store.read_with(cx, |s, _| {
        s.solutions()
            .iter()
            .find(|sol| sol.id.as_str() == solution_id)
            .map(|sol| sol.root.clone())
    }) else {
        return Vec::new();
    };

    for handle in cx.windows() {
        let Some(window_handle) = handle.downcast::<workspace::MultiWorkspace>() else {
            continue;
        };
        let collected = window_handle
            .update(cx, |multi, _window, cx| {
                let workspace = multi.workspace().read(cx);
                let project = workspace.project().read(cx);
                let matches_solution = project
                    .visible_worktrees(cx)
                    .any(|tree| tree.read(cx).abs_path().starts_with(&root))
                    || multi.workspaces().any(|ws| {
                        ws.read(cx)
                            .project()
                            .read(cx)
                            .visible_worktrees(cx)
                            .any(|tree| tree.read(cx).abs_path().starts_with(&root))
                    });
                if !matches_solution {
                    return None;
                }

                // The active item resolves through the active pane; capture its
                // project_path so we can flag exactly the entry the user is
                // currently looking at, even if the same buffer is open in
                // another pane.
                let active_project_path =
                    workspace.active_item(cx).and_then(|item| item.project_path(cx));
                let active_pane_id = workspace.active_pane().entity_id();

                let mut buffers = Vec::new();
                for pane_entity in workspace.panes() {
                    let pane = pane_entity.read(cx);
                    let pane_is_active = pane_entity.entity_id() == active_pane_id;
                    let pane_active_item_id =
                        pane.active_item().map(|item| item.item_id());
                    for item in pane.items() {
                        let Some(project_path) = item.project_path(cx) else {
                            continue;
                        };
                        let is_active_in_pane =
                            pane_active_item_id == Some(item.item_id());
                        let focused = pane_is_active
                            && is_active_in_pane
                            && active_project_path
                                .as_ref()
                                .map(|p| p == &project_path)
                                .unwrap_or(true);
                        buffers.push(BufferInfo {
                            path: project_path.path.as_unix_str().to_string(),
                            dirty: item.is_dirty(cx),
                            // Language detection requires `Buffer` access via
                            // `act_as::<Editor>` and is left for a follow-up;
                            // the field is reserved in the schema so clients
                            // can rely on the shape today.
                            language: None,
                            focused,
                        });
                    }
                }
                Some(buffers)
            })
            .ok()
            .flatten();
        if let Some(buffers) = collected {
            return buffers;
        }
    }
    Vec::new()
}

// =====================================================================
// workspace.get_effective_settings
// =====================================================================

/// Get effective editor settings for a Solution as a JSON object. v1
/// returns the merged `SettingsContent` (default + user + profile)
/// without per-path scoping; the optional `path` argument is reserved
/// for a future revision that will resolve project-local + editorconfig
/// overrides via `SettingsLocation`. Today, supplying `path` is accepted
/// but does not change the response.
#[derive(Debug, Clone, Default, Serialize, JsonSchema)]
pub struct GetEffectiveSettingsParams {
    pub solution_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
}

impl<'de> Deserialize<'de> for GetEffectiveSettingsParams {
    fn deserialize<D: Deserializer<'de>>(de: D) -> Result<Self, D::Error> {
        #[derive(Deserialize, Default)]
        #[serde(default, deny_unknown_fields)]
        struct Inner {
            solution_id: String,
            path: Option<String>,
        }
        let inner = Option::<Inner>::deserialize(de)?.unwrap_or_default();
        Ok(Self {
            solution_id: inner.solution_id,
            path: inner.path,
        })
    }
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct GetEffectiveSettingsResult {
    pub settings: serde_json::Value,
}

#[derive(Clone)]
pub struct GetEffectiveSettingsTool;

impl McpServerTool for GetEffectiveSettingsTool {
    type Input = GetEffectiveSettingsParams;
    type Output = GetEffectiveSettingsResult;
    const NAME: &'static str = "workspace.get_effective_settings";

    async fn run(
        &self,
        input: Self::Input,
        cx: &mut AsyncApp,
    ) -> anyhow::Result<ToolResponse<Self::Output>> {
        anyhow::ensure!(
            !input.solution_id.is_empty(),
            "invalid_params: solution_id is required"
        );
        let settings = cx.update(|cx| -> serde_json::Value {
            // `merged_settings` returns the default+user+profile-resolved view.
            // Path-scoped resolution requires `SettingsLocation`, which we
            // don't have a clean surface for from the MCP layer yet; leaving
            // the `path` parameter as an explicit no-op is preferable to
            // silently returning the wrong scope.
            let store = cx.global::<::settings::SettingsStore>();
            serde_json::to_value(store.merged_settings()).unwrap_or(serde_json::Value::Null)
        });
        Ok(ToolResponse {
            content: vec![ToolResponseContent::Text {
                text: "settings".to_string(),
            }],
            structured_content: GetEffectiveSettingsResult { settings },
        })
    }
}

// =====================================================================
// workspace.dispatch_action
// =====================================================================

/// Dispatch a registered action to the editor window for a Solution.
/// Action name is the fully-qualified path like `workspace::ToggleLeftDock`.
/// Optional `args` are deserialized into the action's payload type.
///
/// Note: returns `dispatched: true` once the action was successfully
/// built and queued onto the window's dispatcher. The dispatch itself
/// runs on a later tick; this tool does NOT report whether a handler
/// eventually fired or refused the action. Returns `dispatched: false`
/// when no window is currently open for the Solution.
#[derive(Debug, Clone, Default, Serialize, JsonSchema)]
pub struct DispatchActionParams {
    pub solution_id: String,
    pub action_name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub args: Option<serde_json::Value>,
}

impl<'de> Deserialize<'de> for DispatchActionParams {
    fn deserialize<D: Deserializer<'de>>(de: D) -> Result<Self, D::Error> {
        #[derive(Deserialize, Default)]
        #[serde(default, deny_unknown_fields)]
        struct Inner {
            solution_id: String,
            action_name: String,
            args: Option<serde_json::Value>,
        }
        let inner = Option::<Inner>::deserialize(de)?.unwrap_or_default();
        Ok(Self {
            solution_id: inner.solution_id,
            action_name: inner.action_name,
            args: inner.args,
        })
    }
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct DispatchActionResult {
    pub dispatched: bool,
}

#[derive(Clone)]
pub struct DispatchActionTool;

impl McpServerTool for DispatchActionTool {
    type Input = DispatchActionParams;
    type Output = DispatchActionResult;
    const NAME: &'static str = "workspace.dispatch_action";

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
            !input.action_name.is_empty(),
            "invalid_params: action_name is required"
        );
        let action_name = input.action_name.clone();
        let dispatched = cx.update(|cx| -> Result<bool> {
            let Some(handle) = find_window_for_solution(&input.solution_id, cx) else {
                return Ok(false);
            };
            // Build the action up-front so a deserialization error surfaces
            // before we touch the window. Once built, dispatch is infallible
            // — the window itself routes the action through its keybinding
            // and focus tree on a later tick.
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

fn find_window_for_solution(
    solution_id: &str,
    cx: &mut App,
) -> Option<gpui::AnyWindowHandle> {
    let store = SolutionStore::try_global(cx)?;
    let root = store.read_with(cx, |s, _| {
        s.solutions()
            .iter()
            .find(|sol| sol.id.as_str() == solution_id)
            .map(|sol| sol.root.clone())
    })?;
    for handle in cx.windows() {
        let Some(window_handle) = handle.downcast::<workspace::MultiWorkspace>() else {
            continue;
        };
        let matches_solution = window_handle
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
        if matches_solution {
            return Some(handle);
        }
    }
    None
}

// =====================================================================
// workspace.screenshot
// =====================================================================

/// Capture a screenshot of the editor window for a Solution. Returns the
/// image as base64-encoded data, with default JPEG quality 80 for token
/// efficiency. Use `format: "png"` for pixel-perfect captures.
#[derive(Debug, Clone, Default, Serialize, JsonSchema)]
pub struct ScreenshotParams {
    pub solution_id: String,
    /// Image format: "jpeg" (default), "png", or "webp".
    #[serde(skip_serializing_if = "Option::is_none")]
    pub format: Option<String>,
    /// Quality 1..=100 for jpeg/webp (ignored for png). Default: 80.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub quality: Option<u8>,
    /// Optional max dimension; if either width or height exceeds this,
    /// the image is downscaled while preserving aspect ratio.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_dimension: Option<u32>,
}

impl<'de> Deserialize<'de> for ScreenshotParams {
    fn deserialize<D: Deserializer<'de>>(de: D) -> Result<Self, D::Error> {
        #[derive(Deserialize, Default)]
        #[serde(default, deny_unknown_fields)]
        struct Inner {
            solution_id: String,
            format: Option<String>,
            quality: Option<u8>,
            max_dimension: Option<u32>,
        }
        let inner = Option::<Inner>::deserialize(de)?.unwrap_or_default();
        Ok(Self {
            solution_id: inner.solution_id,
            format: inner.format,
            quality: inner.quality,
            max_dimension: inner.max_dimension,
        })
    }
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct ScreenshotResult {
    pub width: u32,
    pub height: u32,
    pub media_type: String,
    /// Base64-encoded image bytes.
    pub base64_data: String,
}

#[derive(Clone)]
pub struct ScreenshotTool;

impl McpServerTool for ScreenshotTool {
    type Input = ScreenshotParams;
    type Output = ScreenshotResult;
    const NAME: &'static str = "workspace.screenshot";

    async fn run(
        &self,
        input: Self::Input,
        cx: &mut AsyncApp,
    ) -> anyhow::Result<ToolResponse<Self::Output>> {
        anyhow::ensure!(
            !input.solution_id.is_empty(),
            "invalid_params: solution_id is required"
        );
        let format = input.format.as_deref().unwrap_or("jpeg").to_ascii_lowercase();
        let quality = input.quality.unwrap_or(80).clamp(1, 100);

        let rgba = cx.update(|cx| -> anyhow::Result<image::RgbaImage> {
            let handle = find_window_for_solution(&input.solution_id, cx)
                .ok_or_else(|| anyhow::anyhow!("solution_not_open: {}", input.solution_id))?;
            render_window_to_image(handle, cx)
        })?;

        let (orig_w, orig_h) = rgba.dimensions();
        let resized = if let Some(max_dim) = input.max_dimension {
            let max_side = orig_w.max(orig_h);
            if max_side > max_dim {
                let scale = max_dim as f32 / max_side as f32;
                let new_w = ((orig_w as f32 * scale).round() as u32).max(1);
                let new_h = ((orig_h as f32 * scale).round() as u32).max(1);
                image::imageops::resize(
                    &rgba,
                    new_w,
                    new_h,
                    image::imageops::FilterType::Lanczos3,
                )
            } else {
                rgba
            }
        } else {
            rgba
        };

        let mut buf: Vec<u8> = Vec::new();
        let mut cursor = std::io::Cursor::new(&mut buf);
        let media_type: &'static str = match format.as_str() {
            "png" => {
                resized
                    .write_to(&mut cursor, image::ImageFormat::Png)
                    .with_context(|| "encode png")?;
                "image/png"
            }
            "webp" => {
                resized
                    .write_to(&mut cursor, image::ImageFormat::WebP)
                    .with_context(|| "encode webp")?;
                "image/webp"
            }
            "jpeg" | "jpg" => {
                let dyn_image = image::DynamicImage::ImageRgba8(resized.clone());
                let rgb = dyn_image.to_rgb8();
                let mut encoder =
                    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut cursor, quality);
                encoder
                    .encode_image(&rgb)
                    .with_context(|| "encode jpeg")?;
                "image/jpeg"
            }
            other => anyhow::bail!("unsupported_format: {other}"),
        };

        use base64::Engine as _;
        let base64_data = base64::engine::general_purpose::STANDARD.encode(&buf);
        let width = resized.width();
        let height = resized.height();

        Ok(ToolResponse {
            content: vec![ToolResponseContent::Image {
                data: base64_data.clone(),
                mime_type: media_type.to_string(),
            }],
            structured_content: ScreenshotResult {
                width,
                height,
                media_type: media_type.to_string(),
                base64_data,
            },
        })
    }
}

// `Window::render_to_image` is gated behind gpui's `test-support` feature, so
// the production build cannot capture pixels. We surface a clear error in that
// configuration; the tool still parses params and validates state.
#[cfg(any(test, feature = "test-support"))]
fn render_window_to_image(
    handle: gpui::AnyWindowHandle,
    cx: &mut App,
) -> anyhow::Result<image::RgbaImage> {
    handle
        .update(cx, |_view, window, _cx| window.render_to_image())
        .map_err(|err| anyhow::anyhow!("render_to_image failed: {err}"))?
}

#[cfg(not(any(test, feature = "test-support")))]
fn render_window_to_image(
    _handle: gpui::AnyWindowHandle,
    _cx: &mut App,
) -> anyhow::Result<image::RgbaImage> {
    anyhow::bail!(
        "screenshot_unsupported: gpui::Window::render_to_image is only available in test/test-support builds of this fork"
    )
}

// =====================================================================
// workspace.dump_visual_structure
// =====================================================================

/// Dump a logical tree of the editor window for a Solution. Returns a
/// hierarchical view of `Workspace` -> `TitleBar` / `Dock(side)` /
/// `PaneArea` / `Pane` / `Tab` / `StatusBar` nodes with visibility and
/// focus state.
///
/// This is a logical structure (suitable for assertions like "is the
/// SolutionsPanel open"), NOT the full GPUI element tree.
#[derive(Debug, Clone, Default, Serialize, JsonSchema)]
pub struct DumpVisualStructureParams {
    pub solution_id: String,
}

impl<'de> Deserialize<'de> for DumpVisualStructureParams {
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
pub struct VisualNode {
    pub kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    pub visible: bool,
    pub focused: bool,
    pub children: Vec<VisualNode>,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct DumpVisualStructureResult {
    pub tree: VisualNode,
}

#[derive(Clone)]
pub struct DumpVisualStructureTool;

impl McpServerTool for DumpVisualStructureTool {
    type Input = DumpVisualStructureParams;
    type Output = DumpVisualStructureResult;
    const NAME: &'static str = "workspace.dump_visual_structure";

    async fn run(
        &self,
        input: Self::Input,
        cx: &mut AsyncApp,
    ) -> anyhow::Result<ToolResponse<Self::Output>> {
        anyhow::ensure!(
            !input.solution_id.is_empty(),
            "invalid_params: solution_id is required"
        );
        let tree = cx
            .update(|cx| build_visual_tree(&input.solution_id, cx))
            .ok_or_else(|| anyhow::anyhow!("solution_not_open: {}", input.solution_id))?;
        Ok(ToolResponse {
            content: vec![ToolResponseContent::Text {
                text: format!("structure for {}", input.solution_id),
            }],
            structured_content: DumpVisualStructureResult { tree },
        })
    }
}

fn build_visual_tree(solution_id: &str, cx: &mut App) -> Option<VisualNode> {
    let handle = find_window_for_solution(solution_id, cx)?;
    let window_handle = handle.downcast::<workspace::MultiWorkspace>()?;
    window_handle
        .read_with(cx, |multi, cx| build_workspace_node(multi, cx))
        .ok()
}

fn build_workspace_node(multi: &workspace::MultiWorkspace, cx: &App) -> VisualNode {
    let workspace = multi.workspace().read(cx);
    let mut children = vec![
        VisualNode {
            kind: "TitleBar".to_string(),
            label: None,
            visible: true,
            focused: false,
            children: Vec::new(),
        },
        build_dock_node("left", workspace.left_dock(), cx),
        build_pane_area_node(workspace, cx),
        build_dock_node("right", workspace.right_dock(), cx),
        build_dock_node("bottom", workspace.bottom_dock(), cx),
        VisualNode {
            kind: "StatusBar".to_string(),
            label: None,
            visible: workspace.status_bar_visible(cx),
            focused: false,
            children: Vec::new(),
        },
    ];

    if let Some(modal) = build_modal_node(workspace, cx) {
        children.push(modal);
    }

    VisualNode {
        kind: "Workspace".to_string(),
        label: None,
        visible: true,
        focused: false,
        children,
    }
}

fn build_dock_node(
    side: &str,
    dock: &gpui::Entity<workspace::dock::Dock>,
    cx: &App,
) -> VisualNode {
    let dock = dock.read(cx);
    let is_open = dock.is_open();
    let active_panel_label = dock
        .active_panel()
        .map(|panel| panel.persistent_name().to_string());

    let panel_node = active_panel_label.map(|name| VisualNode {
        kind: "Panel".to_string(),
        label: Some(name),
        visible: is_open,
        focused: false,
        children: Vec::new(),
    });

    VisualNode {
        kind: format!("Dock({side})"),
        label: None,
        visible: is_open,
        focused: false,
        children: panel_node.into_iter().collect(),
    }
}

fn build_pane_area_node(workspace: &workspace::Workspace, cx: &App) -> VisualNode {
    let active_pane_id = workspace.active_pane().entity_id();
    let pane_children: Vec<VisualNode> = workspace
        .panes()
        .iter()
        .map(|pane_entity| {
            let pane_is_active = pane_entity.entity_id() == active_pane_id;
            let pane = pane_entity.read(cx);
            let active_item_id = pane.active_item().map(|item| item.item_id());
            let tabs: Vec<VisualNode> = pane
                .items()
                .map(|item| {
                    let label = item
                        .project_path(cx)
                        .map(|p| p.path.as_unix_str().to_string())
                        .unwrap_or_else(|| {
                            item.tab_content_text(0, cx).to_string()
                        });
                    let is_active = active_item_id
                        .map(|id| id == item.item_id())
                        .unwrap_or(false);
                    VisualNode {
                        kind: format!("Tab({label})"),
                        label: Some(label),
                        visible: true,
                        focused: is_active,
                        children: Vec::new(),
                    }
                })
                .collect();

            VisualNode {
                kind: "Pane".to_string(),
                label: None,
                visible: true,
                focused: pane_is_active,
                children: tabs,
            }
        })
        .collect();

    VisualNode {
        kind: "PaneArea".to_string(),
        label: None,
        visible: true,
        focused: false,
        children: pane_children,
    }
}

// Modal layer access requires API discovery; skip for v1.
// Phase 7 / follow-up can enrich.
fn build_modal_node(_workspace: &workspace::Workspace, _cx: &App) -> Option<VisualNode> {
    None
}

// =====================================================================
// diagnostics.get
// =====================================================================

/// Get LSP diagnostic summary counts for files in a Solution. Returns
/// per-path `error_count` / `warning_count` aggregated across all language
/// servers reporting on that file. Optional `buffer_path` filters results
/// to a single project-relative path. Individual diagnostic items
/// (line / column / message / source) are not exposed in v1; that level
/// of detail requires per-buffer LSP queries and is deferred to Phase 7.
///
/// `info_count` / `hint_count` are intentionally absent from the schema:
/// the underlying `project::DiagnosticSummary` only tracks errors and
/// warnings today. Adding info/hint reporting requires upstream changes
/// and will land alongside the detailed-diagnostic surface.
#[derive(Debug, Clone, Default, Serialize, JsonSchema)]
pub struct GetDiagnosticsParams {
    pub solution_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub buffer_path: Option<String>,
}

impl<'de> Deserialize<'de> for GetDiagnosticsParams {
    fn deserialize<D: Deserializer<'de>>(de: D) -> Result<Self, D::Error> {
        #[derive(Deserialize, Default)]
        #[serde(default, deny_unknown_fields)]
        struct Inner {
            solution_id: String,
            buffer_path: Option<String>,
        }
        let inner = Option::<Inner>::deserialize(de)?.unwrap_or_default();
        Ok(Self {
            solution_id: inner.solution_id,
            buffer_path: inner.buffer_path,
        })
    }
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct DiagnosticPathSummary {
    pub path: String,
    pub error_count: usize,
    pub warning_count: usize,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct GetDiagnosticsResult {
    pub diagnostics: Vec<DiagnosticPathSummary>,
}

#[derive(Clone)]
pub struct GetDiagnosticsTool;

impl McpServerTool for GetDiagnosticsTool {
    type Input = GetDiagnosticsParams;
    type Output = GetDiagnosticsResult;
    const NAME: &'static str = "diagnostics.get";

    async fn run(
        &self,
        input: Self::Input,
        cx: &mut AsyncApp,
    ) -> anyhow::Result<ToolResponse<Self::Output>> {
        anyhow::ensure!(
            !input.solution_id.is_empty(),
            "invalid_params: solution_id is required"
        );
        let diagnostics = cx.update(|cx| collect_diagnostics(&input, cx));
        Ok(ToolResponse {
            content: vec![ToolResponseContent::Text {
                text: format!("{} file(s) with diagnostics", diagnostics.len()),
            }],
            structured_content: GetDiagnosticsResult { diagnostics },
        })
    }
}

fn collect_diagnostics(
    input: &GetDiagnosticsParams,
    cx: &mut App,
) -> Vec<DiagnosticPathSummary> {
    let Some(store) = SolutionStore::try_global(cx) else {
        return Vec::new();
    };
    let Some(root) = store.read_with(cx, |s, _| {
        s.solutions()
            .iter()
            .find(|sol| sol.id.as_str() == input.solution_id)
            .map(|sol| sol.root.clone())
    }) else {
        return Vec::new();
    };

    for handle in cx.windows() {
        let Some(window_handle) = handle.downcast::<workspace::MultiWorkspace>() else {
            continue;
        };
        let collected = window_handle
            .update(cx, |multi, _window, cx| {
                let workspace = multi.workspace().read(cx);
                let project = workspace.project().read(cx);
                let matches_solution = project
                    .visible_worktrees(cx)
                    .any(|tree| tree.read(cx).abs_path().starts_with(&root))
                    || multi.workspaces().any(|ws| {
                        ws.read(cx)
                            .project()
                            .read(cx)
                            .visible_worktrees(cx)
                            .any(|tree| tree.read(cx).abs_path().starts_with(&root))
                    });
                if !matches_solution {
                    return None;
                }

                // A path may have multiple language servers reporting on it
                // (e.g. rust-analyzer + clippy). Aggregate counts across all
                // servers for a single per-path summary, matching the rollup
                // shown in the editor's diagnostics panel.
                let mut by_path: std::collections::BTreeMap<String, DiagnosticPathSummary> =
                    std::collections::BTreeMap::new();

                for (project_path, _server_id, summary) in
                    project.diagnostic_summaries(false, cx)
                {
                    let path_str = project_path.path.as_unix_str().to_string();
                    if let Some(filter) = input.buffer_path.as_deref() {
                        if path_str != filter {
                            continue;
                        }
                    }
                    let entry = by_path.entry(path_str.clone()).or_insert(
                        DiagnosticPathSummary {
                            path: path_str,
                            error_count: 0,
                            warning_count: 0,
                        },
                    );
                    entry.error_count += summary.error_count;
                    entry.warning_count += summary.warning_count;
                }

                Some(by_path.into_values().collect::<Vec<_>>())
            })
            .ok()
            .flatten();

        if let Some(diagnostics) = collected {
            return diagnostics;
        }
    }
    Vec::new()
}

// =====================================================================
// project.list_files
// =====================================================================

/// List files across the worktrees of a Solution. Supports an optional
/// glob filter (matched against each file's path relative to its
/// worktree root), a `scope` (`all_worktrees` (default) or
/// `first_worktree`), and opaque cursor-based pagination. The cursor is
/// the `worktree_root|path` of the last entry returned in the previous
/// page; the next page begins strictly after that point in lexicographic
/// order.
#[derive(Debug, Clone, Default, Serialize, JsonSchema)]
pub struct ListFilesParams {
    pub solution_id: String,
    /// Optional glob pattern (e.g. `**/*.rs`). When omitted, all files
    /// are returned.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub glob: Option<String>,
    /// Scope: `"all_worktrees"` (default) or `"first_worktree"`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scope: Option<String>,
    /// Opaque pagination cursor returned from the previous response.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cursor: Option<String>,
    /// Maximum number of results in this page. Default 200, max 5000.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max: Option<usize>,
}

impl<'de> Deserialize<'de> for ListFilesParams {
    fn deserialize<D: Deserializer<'de>>(de: D) -> Result<Self, D::Error> {
        #[derive(Deserialize, Default)]
        #[serde(default, deny_unknown_fields)]
        struct Inner {
            solution_id: String,
            glob: Option<String>,
            scope: Option<String>,
            cursor: Option<String>,
            max: Option<usize>,
        }
        let inner = Option::<Inner>::deserialize(de)?.unwrap_or_default();
        Ok(Self {
            solution_id: inner.solution_id,
            glob: inner.glob,
            scope: inner.scope,
            cursor: inner.cursor,
            max: inner.max,
        })
    }
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct FileEntry {
    /// Path relative to `worktree_root`, in unix form.
    pub path: String,
    /// Absolute path of the worktree root containing this entry.
    pub worktree_root: String,
    /// File size in bytes, as reported by the worktree scan.
    pub size: u64,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct ListFilesResult {
    pub files: Vec<FileEntry>,
    /// Cursor for the next page, or absent when the list is exhausted.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
}

#[derive(Clone)]
pub struct ListFilesTool;

impl McpServerTool for ListFilesTool {
    type Input = ListFilesParams;
    type Output = ListFilesResult;
    const NAME: &'static str = "project.list_files";

    async fn run(
        &self,
        input: Self::Input,
        cx: &mut AsyncApp,
    ) -> anyhow::Result<ToolResponse<Self::Output>> {
        anyhow::ensure!(
            !input.solution_id.is_empty(),
            "invalid_params: solution_id is required"
        );
        let max = input.max.unwrap_or(200).clamp(1, 5000);
        let scope_first_only = match input.scope.as_deref() {
            None | Some("all_worktrees") => false,
            Some("first_worktree") => true,
            Some(other) => anyhow::bail!(
                "invalid_params: scope must be \"all_worktrees\" or \"first_worktree\", got {other:?}"
            ),
        };
        let glob_matcher = input
            .glob
            .as_deref()
            .map(globset::Glob::new)
            .transpose()
            .map_err(|err| anyhow::anyhow!("invalid_glob: {err}"))?
            .map(|g| g.compile_matcher());
        let start_after = input.cursor.clone().unwrap_or_default();

        let (files, next_cursor) = cx.update(|cx| {
            collect_files(
                &input.solution_id,
                scope_first_only,
                glob_matcher.as_ref(),
                &start_after,
                max,
                cx,
            )
        });

        Ok(ToolResponse {
            content: vec![ToolResponseContent::Text {
                text: format!("{} file(s)", files.len()),
            }],
            structured_content: ListFilesResult { files, next_cursor },
        })
    }
}

fn cursor_for(file: &FileEntry) -> String {
    format!("{}|{}", file.worktree_root, file.path)
}

fn collect_files(
    solution_id: &str,
    first_only: bool,
    glob: Option<&globset::GlobMatcher>,
    start_after: &str,
    max: usize,
    cx: &mut App,
) -> (Vec<FileEntry>, Option<String>) {
    let Some(store) = SolutionStore::try_global(cx) else {
        return (Vec::new(), None);
    };
    let Some(root) = store.read_with(cx, |s, _| {
        s.solutions()
            .iter()
            .find(|sol| sol.id.as_str() == solution_id)
            .map(|sol| sol.root.clone())
    }) else {
        return (Vec::new(), None);
    };

    for handle in cx.windows() {
        let Some(window_handle) = handle.downcast::<workspace::MultiWorkspace>() else {
            continue;
        };
        let collected = window_handle
            .update(cx, |multi, _window, cx| {
                let primary_matches = multi
                    .workspace()
                    .read(cx)
                    .project()
                    .read(cx)
                    .visible_worktrees(cx)
                    .any(|tree| tree.read(cx).abs_path().starts_with(&root));
                let any_matches = primary_matches
                    || multi.workspaces().any(|ws| {
                        ws.read(cx)
                            .project()
                            .read(cx)
                            .visible_worktrees(cx)
                            .any(|tree| tree.read(cx).abs_path().starts_with(&root))
                    });
                if !any_matches {
                    return None;
                }

                let mut files: Vec<FileEntry> = Vec::new();
                let mut reached_cap = false;
                'outer: for workspace_entity in multi.workspaces() {
                    let workspace = workspace_entity.read(cx);
                    let project = workspace.project().read(cx);
                    for tree_entity in project.visible_worktrees(cx) {
                        let tree = tree_entity.read(cx);
                        let abs_root = tree.abs_path();
                        if !abs_root.starts_with(&root) {
                            continue;
                        }
                        let worktree_root = abs_root.to_string_lossy().into_owned();
                        for entry in tree.entries(false, 0) {
                            if !entry.is_file() {
                                continue;
                            }
                            let path_str = entry.path.as_unix_str().to_string();
                            let candidate = FileEntry {
                                path: path_str,
                                worktree_root: worktree_root.clone(),
                                size: entry.size,
                            };
                            let key = cursor_for(&candidate);
                            if !start_after.is_empty() && key.as_str() <= start_after {
                                continue;
                            }
                            if let Some(matcher) = glob {
                                if !matcher.is_match(&candidate.path) {
                                    continue;
                                }
                            }
                            files.push(candidate);
                            if files.len() > max {
                                reached_cap = true;
                                break 'outer;
                            }
                        }
                        if first_only {
                            break 'outer;
                        }
                    }
                }

                let next_cursor = if reached_cap {
                    files.truncate(max);
                    files.last().map(cursor_for)
                } else {
                    None
                };
                Some((files, next_cursor))
            })
            .ok()
            .flatten();

        if let Some(result) = collected {
            return result;
        }
    }
    (Vec::new(), None)
}

// =====================================================================
// Path-validation helper (cross-cutting security primitive)
// =====================================================================

#[allow(dead_code)]
#[derive(Debug, Clone)]
pub enum PathValidationError {
    SolutionNotFound,
    PathOutsideSolution,
    InvalidPath,
}

impl std::fmt::Display for PathValidationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::SolutionNotFound => write!(f, "solution_not_found"),
            Self::PathOutsideSolution => write!(f, "path_outside_solution"),
            Self::InvalidPath => write!(f, "invalid_path"),
        }
    }
}

impl std::error::Error for PathValidationError {}

/// Verify that `path` lies under at least one worktree root of the
/// named Solution. Returns the canonicalized absolute path.
///
/// Used by every Phase 6 `project.*` tool to prevent agents from
/// escaping into arbitrary filesystem via `apply_edit("/etc/passwd", ...)`.
#[allow(dead_code)]
pub fn validate_path_in_solution(
    solution_id: &str,
    path: &str,
    cx: &App,
) -> Result<std::path::PathBuf, PathValidationError> {
    let absolute = std::path::PathBuf::from(path);
    if !absolute.is_absolute() {
        // Relative paths require a cwd that we don't have here. Reject.
        return Err(PathValidationError::InvalidPath);
    }

    // Best-effort canonicalization. If the path doesn't exist yet (e.g.
    // create_file), we accept the absolute non-canonical form provided
    // its prefix is under a Solution member.
    let canonical = absolute.canonicalize().unwrap_or_else(|_| absolute.clone());

    let store = SolutionStore::try_global(cx).ok_or(PathValidationError::SolutionNotFound)?;
    let valid = store.read_with(cx, |s, _| {
        s.solutions()
            .iter()
            .find(|sol| sol.id.as_str() == solution_id)
            .map(|sol| {
                sol.members.iter().any(|m| {
                    let canon_member = m
                        .local_path
                        .canonicalize()
                        .unwrap_or_else(|_| m.local_path.clone());
                    canonical.starts_with(&canon_member)
                }) || canonical.starts_with(&sol.root)
            })
    });

    match valid {
        Some(true) => Ok(canonical),
        Some(false) => Err(PathValidationError::PathOutsideSolution),
        None => Err(PathValidationError::SolutionNotFound),
    }
}

// =====================================================================
// project.read_buffer
// =====================================================================

/// Read the content of a file via the editor's Buffer system. If the
/// file is already open in any workspace of the Solution, returns the
/// live (potentially-dirty) content. Otherwise opens it as a Buffer
/// without creating a tab; calling again is idempotent.
#[derive(Debug, Clone, Default, Serialize, JsonSchema)]
pub struct ReadBufferParams {
    pub solution_id: String,
    /// Absolute path of the file to read. Must lie under one of the
    /// Solution's worktrees.
    pub path: String,
}

impl<'de> Deserialize<'de> for ReadBufferParams {
    fn deserialize<D: Deserializer<'de>>(de: D) -> Result<Self, D::Error> {
        #[derive(Deserialize, Default)]
        #[serde(default, deny_unknown_fields)]
        struct Inner {
            solution_id: String,
            path: String,
        }
        let inner = Option::<Inner>::deserialize(de)?.unwrap_or_default();
        Ok(Self {
            solution_id: inner.solution_id,
            path: inner.path,
        })
    }
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct ReadBufferResult {
    pub content: String,
    pub line_count: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
    pub dirty: bool,
}

#[derive(Clone)]
pub struct ReadBufferTool;

impl McpServerTool for ReadBufferTool {
    type Input = ReadBufferParams;
    type Output = ReadBufferResult;
    const NAME: &'static str = "project.read_buffer";

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
            !input.path.is_empty(),
            "invalid_params: path is required"
        );

        cx.update(|cx| validate_path_in_solution(&input.solution_id, &input.path, cx))
            .map_err(|err| anyhow::anyhow!("{err}"))?;

        let project = cx
            .update(|cx| project_for_solution(&input.solution_id, cx))
            .ok_or_else(|| anyhow::anyhow!("solution_not_open: {}", input.solution_id))?;

        let project_path = cx.update(|cx| resolve_project_path(&project, &input.path, cx))?;

        let buffer = project
            .update(cx, |project, cx| project.open_buffer(project_path, cx))
            .await?;

        let result = cx.update(|cx| {
            let buffer_ref = buffer.read(cx);
            ReadBufferResult {
                content: buffer_ref.text(),
                line_count: buffer_ref.max_point().row + 1,
                language: buffer_ref
                    .language()
                    .map(|language| language.name().as_ref().to_string()),
                dirty: buffer_ref.is_dirty(),
            }
        });

        Ok(ToolResponse {
            content: vec![ToolResponseContent::Text {
                text: format!("read {} ({} lines)", input.path, result.line_count),
            }],
            structured_content: result,
        })
    }
}

// =====================================================================
// project.apply_edit
// =====================================================================

/// Apply atomic edits to a file via a Buffer transaction. All edits in
/// the request are coalesced into a single edit call so the change is
/// applied as one undo/redo unit. The buffer is opened (without
/// creating a tab) if it is not already open. The edits become visible
/// to the user immediately and join the user's undo stack; saving is
/// not performed automatically.
#[derive(Debug, Clone, Default, Serialize, JsonSchema)]
pub struct ApplyEditParams {
    pub solution_id: String,
    /// Absolute path of the file to edit. Must lie under one of the
    /// Solution's worktrees.
    pub path: String,
    /// One or more edits to apply atomically.
    pub edits: Vec<EditSpec>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct EditSpec {
    pub range: EditRange,
    pub new_text: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct EditRange {
    pub start: EditPoint,
    pub end: EditPoint,
}

/// Zero-based `(line, col)` location. `col` is a UTF-8 byte offset
/// within the line, matching `language::Point`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct EditPoint {
    pub line: u32,
    pub col: u32,
}

impl<'de> Deserialize<'de> for ApplyEditParams {
    fn deserialize<D: Deserializer<'de>>(de: D) -> Result<Self, D::Error> {
        #[derive(Deserialize, Default)]
        #[serde(default, deny_unknown_fields)]
        struct Inner {
            solution_id: String,
            path: String,
            #[serde(default)]
            edits: Vec<EditSpec>,
        }
        let inner = Option::<Inner>::deserialize(de)?.unwrap_or_default();
        Ok(Self {
            solution_id: inner.solution_id,
            path: inner.path,
            edits: inner.edits,
        })
    }
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct AfterEditMeta {
    pub line_count: u32,
    pub dirty: bool,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct ApplyEditResult {
    pub applied: bool,
    pub after: AfterEditMeta,
}

#[derive(Clone)]
pub struct ApplyEditTool;

impl McpServerTool for ApplyEditTool {
    type Input = ApplyEditParams;
    type Output = ApplyEditResult;
    const NAME: &'static str = "project.apply_edit";

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
            !input.path.is_empty(),
            "invalid_params: path is required"
        );
        anyhow::ensure!(
            !input.edits.is_empty(),
            "invalid_params: at least one edit is required"
        );

        cx.update(|cx| validate_path_in_solution(&input.solution_id, &input.path, cx))
            .map_err(|err| anyhow::anyhow!("{err}"))?;

        let project = cx
            .update(|cx| project_for_solution(&input.solution_id, cx))
            .ok_or_else(|| anyhow::anyhow!("solution_not_open: {}", input.solution_id))?;

        let project_path = cx.update(|cx| resolve_project_path(&project, &input.path, cx))?;

        let buffer = project
            .update(cx, |project, cx| project.open_buffer(project_path, cx))
            .await?;

        let edit_count = input.edits.len();
        let after = buffer.update(cx, |buffer, cx| {
            let edits: Vec<(std::ops::Range<language::Point>, String)> = input
                .edits
                .iter()
                .map(|edit| {
                    let start = language::Point::new(edit.range.start.line, edit.range.start.col);
                    let end = language::Point::new(edit.range.end.line, edit.range.end.col);
                    (start..end, edit.new_text.clone())
                })
                .collect();
            buffer.edit(edits, None, cx);
            AfterEditMeta {
                line_count: buffer.max_point().row + 1,
                dirty: buffer.is_dirty(),
            }
        });

        Ok(ToolResponse {
            content: vec![ToolResponseContent::Text {
                text: format!("applied {} edit(s) to {}", edit_count, input.path),
            }],
            structured_content: ApplyEditResult {
                applied: true,
                after,
            },
        })
    }
}

// Locate the `Project` whose worktrees back the named Solution. We walk
// every open `MultiWorkspace` window and return the first project whose
// visible worktrees include the Solution's root (or a member directory
// underneath it).
fn project_for_solution(
    solution_id: &str,
    cx: &mut App,
) -> Option<gpui::Entity<project::Project>> {
    let store = SolutionStore::try_global(cx)?;
    let root = store.read_with(cx, |s, _| {
        s.solutions()
            .iter()
            .find(|sol| sol.id.as_str() == solution_id)
            .map(|sol| sol.root.clone())
    })?;

    for handle in cx.windows() {
        let Some(window_handle) = handle.downcast::<workspace::MultiWorkspace>() else {
            continue;
        };
        let result = window_handle
            .update(cx, |multi, _window, cx| {
                for workspace_entity in multi.workspaces() {
                    let workspace = workspace_entity.read(cx);
                    let project = workspace.project();
                    let matches = project
                        .read(cx)
                        .visible_worktrees(cx)
                        .any(|tree| tree.read(cx).abs_path().starts_with(&root));
                    if matches {
                        return Some(project.clone());
                    }
                }
                None
            })
            .ok()
            .flatten();
        if let Some(project) = result {
            return Some(project);
        }
    }
    None
}

// Map an absolute path to a `ProjectPath` within one of the project's
// visible worktrees. Returns `path_not_in_worktree` if no worktree
// contains it.
fn resolve_project_path(
    project: &gpui::Entity<project::Project>,
    abs_path: &str,
    cx: &App,
) -> anyhow::Result<project::ProjectPath> {
    let abs = std::path::PathBuf::from(abs_path);
    let project_ref = project.read(cx);
    for tree_entity in project_ref.visible_worktrees(cx) {
        let tree = tree_entity.read(cx);
        let root = tree.abs_path();
        if abs.starts_with(root.as_ref()) {
            let rel = abs
                .strip_prefix(root.as_ref())
                .map_err(|err| anyhow::anyhow!("strip_prefix: {err}"))?;
            let rel_path = util::rel_path::RelPath::new(rel, tree.path_style())
                .map_err(|err| anyhow::anyhow!("rel_path: {err}"))?
                .into_owned()
                .into();
            return Ok(project::ProjectPath {
                worktree_id: tree.id(),
                path: rel_path,
            });
        }
    }
    anyhow::bail!("path_not_in_worktree: {abs_path}")
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

    #[test]
    fn list_buffers_params_round_trip() {
        let p: ListBuffersParams = serde_json::from_value(serde_json::json!({
            "solution_id": "demo"
        }))
        .expect("parse");
        assert_eq!(p.solution_id, "demo");
    }

    #[test]
    fn list_buffers_params_accepts_null() {
        let p: ListBuffersParams =
            serde_json::from_value(serde_json::Value::Null).expect("null");
        assert!(p.solution_id.is_empty());
    }

    #[test]
    fn get_effective_settings_params_round_trip() {
        let p: GetEffectiveSettingsParams = serde_json::from_value(serde_json::json!({
            "solution_id": "demo",
            "path": "src/foo.rs"
        }))
        .expect("parse");
        assert_eq!(p.solution_id, "demo");
        assert_eq!(p.path.as_deref(), Some("src/foo.rs"));
    }

    #[test]
    fn get_effective_settings_params_accepts_null() {
        let p: GetEffectiveSettingsParams =
            serde_json::from_value(serde_json::Value::Null).expect("null");
        assert!(p.solution_id.is_empty());
        assert!(p.path.is_none());
    }

    #[test]
    fn dispatch_action_params_with_args() {
        let p: DispatchActionParams = serde_json::from_value(serde_json::json!({
            "solution_id": "demo",
            "action_name": "workspace::ToggleLeftDock",
            "args": null
        }))
        .expect("parse");
        assert_eq!(p.solution_id, "demo");
        assert_eq!(p.action_name, "workspace::ToggleLeftDock");
    }

    #[test]
    fn dispatch_action_params_accepts_null() {
        let p: DispatchActionParams =
            serde_json::from_value(serde_json::Value::Null).expect("null");
        assert!(p.solution_id.is_empty());
        assert!(p.action_name.is_empty());
    }

    #[test]
    fn screenshot_params_round_trip() {
        let p: ScreenshotParams = serde_json::from_value(serde_json::json!({
            "solution_id": "demo",
            "format": "jpeg",
            "quality": 75,
            "max_dimension": 1280
        }))
        .expect("parse");
        assert_eq!(p.solution_id, "demo");
        assert_eq!(p.format.as_deref(), Some("jpeg"));
        assert_eq!(p.quality, Some(75));
        assert_eq!(p.max_dimension, Some(1280));
    }

    #[test]
    fn screenshot_params_accepts_null() {
        let p: ScreenshotParams =
            serde_json::from_value(serde_json::Value::Null).expect("null");
        assert!(p.solution_id.is_empty());
        assert!(p.format.is_none());
        assert!(p.quality.is_none());
        assert!(p.max_dimension.is_none());
    }

    #[test]
    fn dump_visual_params_round_trip() {
        let p: DumpVisualStructureParams = serde_json::from_value(serde_json::json!({
            "solution_id": "demo"
        }))
        .expect("parse");
        assert_eq!(p.solution_id, "demo");
    }

    #[test]
    fn dump_visual_params_accepts_null() {
        let p: DumpVisualStructureParams =
            serde_json::from_value(serde_json::Value::Null).expect("null");
        assert!(p.solution_id.is_empty());
    }

    #[test]
    fn diagnostics_params_round_trip() {
        let p: GetDiagnosticsParams = serde_json::from_value(serde_json::json!({
            "solution_id": "demo",
            "buffer_path": "src/foo.rs"
        }))
        .expect("parse");
        assert_eq!(p.solution_id, "demo");
        assert_eq!(p.buffer_path.as_deref(), Some("src/foo.rs"));
    }

    #[test]
    fn diagnostics_params_accepts_null() {
        let p: GetDiagnosticsParams =
            serde_json::from_value(serde_json::Value::Null).expect("null");
        assert!(p.solution_id.is_empty());
        assert!(p.buffer_path.is_none());
    }

    #[test]
    fn list_files_params_round_trip() {
        let p: ListFilesParams = serde_json::from_value(serde_json::json!({
            "solution_id": "demo",
            "glob": "**/*.rs",
            "scope": "first_worktree",
            "max": 50
        }))
        .expect("parse");
        assert_eq!(p.solution_id, "demo");
        assert_eq!(p.glob.as_deref(), Some("**/*.rs"));
        assert_eq!(p.scope.as_deref(), Some("first_worktree"));
        assert_eq!(p.max, Some(50));
    }

    #[test]
    fn list_files_params_accepts_null() {
        let p: ListFilesParams =
            serde_json::from_value(serde_json::Value::Null).expect("null");
        assert!(p.solution_id.is_empty());
        assert!(p.glob.is_none());
        assert!(p.scope.is_none());
        assert!(p.cursor.is_none());
        assert!(p.max.is_none());
    }

    #[gpui::test]
    async fn validate_path_rejects_relative(cx: &mut TestAppContext) {
        let dir = tempdir().expect("tempdir");
        let store = cx.update(|cx| SolutionStore::for_test(dir.path().join("c.json"), cx));
        cx.update(|cx| crate::store::install_global_for_test(store, cx));
        let result = cx.update(|cx| validate_path_in_solution("any", "relative/path.rs", cx));
        assert!(matches!(result, Err(PathValidationError::InvalidPath)));
    }

    #[gpui::test]
    async fn validate_path_rejects_unknown_solution(cx: &mut TestAppContext) {
        let dir = tempdir().expect("tempdir");
        let store = cx.update(|cx| SolutionStore::for_test(dir.path().join("c.json"), cx));
        cx.update(|cx| crate::store::install_global_for_test(store, cx));
        let result = cx.update(|cx| validate_path_in_solution("nonexistent", "/tmp/foo", cx));
        assert!(matches!(result, Err(PathValidationError::SolutionNotFound)));
    }

    #[gpui::test]
    async fn validate_path_rejects_outside_solution(cx: &mut TestAppContext) {
        let dir = tempdir().expect("tempdir");
        let store = cx.update(|cx| SolutionStore::for_test(dir.path().join("c.json"), cx));
        cx.update(|cx| crate::store::install_global_for_test(store.clone(), cx));
        let _sol_id = store
            .update(cx, |s, cx| {
                s.create_solution("Sol", dir.path().to_path_buf(), cx)
            })
            .expect("create");
        let result = cx.update(|cx| validate_path_in_solution("sol", "/etc/passwd", cx));
        assert!(matches!(
            result,
            Err(PathValidationError::PathOutsideSolution)
        ));
    }

    #[test]
    fn read_buffer_params_round_trip() {
        let p: ReadBufferParams = serde_json::from_value(serde_json::json!({
            "solution_id": "demo",
            "path": "/abs/foo.rs"
        }))
        .expect("parse");
        assert_eq!(p.solution_id, "demo");
        assert_eq!(p.path, "/abs/foo.rs");
    }

    #[test]
    fn read_buffer_params_accepts_null() {
        let p: ReadBufferParams =
            serde_json::from_value(serde_json::Value::Null).expect("null");
        assert!(p.solution_id.is_empty());
        assert!(p.path.is_empty());
    }

    #[test]
    fn apply_edit_params_round_trip() {
        let p: ApplyEditParams = serde_json::from_value(serde_json::json!({
            "solution_id": "demo",
            "path": "/abs/foo.rs",
            "edits": [{
                "range": {"start": {"line": 0, "col": 0}, "end": {"line": 0, "col": 5}},
                "new_text": "hello"
            }]
        }))
        .expect("parse");
        assert_eq!(p.edits.len(), 1);
        assert_eq!(p.edits[0].new_text, "hello");
        assert_eq!(p.edits[0].range.start.line, 0);
        assert_eq!(p.edits[0].range.end.col, 5);
    }

    #[test]
    fn apply_edit_params_accepts_null() {
        let p: ApplyEditParams =
            serde_json::from_value(serde_json::Value::Null).expect("null");
        assert!(p.solution_id.is_empty());
        assert!(p.path.is_empty());
        assert!(p.edits.is_empty());
    }
}
