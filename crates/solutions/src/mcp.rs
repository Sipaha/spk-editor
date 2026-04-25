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

pub fn register(cx: &mut App) {
    editor_mcp::register_tool(cx, |server| {
        server.add_tool(ListSolutionsTool);
    });
    editor_mcp::register_tool(cx, |server| {
        server.add_tool(GetSolutionTool);
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
                let workspace = multi.workspace().read(cx);
                workspace
                    .project()
                    .read(cx)
                    .visible_worktrees(cx)
                    .any(|tree| tree.read(cx).abs_path().starts_with(solution_root))
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
                let workspace = multi.workspace().read(cx);
                let project = workspace.project().read(cx);
                let worktree_paths: Vec<String> = project
                    .visible_worktrees(cx)
                    .map(|t| t.read(cx).abs_path().to_string_lossy().into_owned())
                    .collect();
                if !worktree_paths
                    .iter()
                    .any(|p| std::path::Path::new(p).starts_with(solution_root))
                {
                    return None;
                }
                let active_buffer = workspace
                    .active_item(cx)
                    .and_then(|item| item.project_path(cx))
                    .map(|pp| pp.path.as_unix_str().to_string());
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
}
