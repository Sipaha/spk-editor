//! MCP tools exposed by the `solution_agent` crate. Tools register with the
//! central `editor_mcp` registry from `solution_agent::init` so that
//! `start_server` (called later from `crates/zed/src/main.rs`) sees them
//! when binding the socket.
use anyhow::{Context as _, Result, anyhow};
use context_server::listener::{McpServerTool, ToolResponse};
use context_server::types::ToolResponseContent;
use gpui::{App, AsyncApp, Entity};
use schemars::JsonSchema;
use serde::{Deserialize, Deserializer, Serialize};

use crate::model::{SolutionSession, SolutionSessionId};
use crate::store::SolutionAgentStore;
use solutions::{SolutionId, SolutionStore};

pub fn register(cx: &mut App) {
    editor_mcp::register_tool(cx, |server| {
        server.add_tool(ListSessionsTool);
    });
    editor_mcp::register_tool(cx, |server| {
        server.add_tool(GetSessionTool);
    });
    editor_mcp::register_tool(cx, |server| {
        server.add_tool(CreateSessionTool);
    });
    editor_mcp::register_tool(cx, |server| {
        server.add_tool(SendMessageTool);
    });
    editor_mcp::register_tool(cx, |server| {
        server.add_tool(CloseSessionTool);
    });
    // Tasks 5.3 add the remaining 3 tools and re-enable these lines:
    // editor_mcp::register_tool(cx, |server| server.add_tool(CancelTurnTool));
    // editor_mcp::register_tool(cx, |server| server.add_tool(RenameSessionTool));
    // editor_mcp::register_tool(cx, |server| server.add_tool(RestartAgentTool));
}

// =====================================================================
// solution_agent.list_sessions
// =====================================================================

/// List Solution-scoped AI sessions, optionally filtered by `solution_id`.
#[derive(Debug, Clone, Default, Serialize, JsonSchema)]
pub struct ListSessionsParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub solution_id: Option<String>,
}

impl<'de> Deserialize<'de> for ListSessionsParams {
    fn deserialize<D: Deserializer<'de>>(de: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        struct Helper {
            #[serde(default)]
            solution_id: Option<String>,
        }
        let helper = Helper::deserialize(de).unwrap_or(Helper { solution_id: None });
        Ok(Self {
            solution_id: helper.solution_id,
        })
    }
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct SessionSummary {
    pub id: String,
    pub solution_id: String,
    pub agent_id: String,
    pub title: String,
    pub state: String,
    pub created_at: i64,
    pub last_activity_at: i64,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct ListSessionsResult {
    pub sessions: Vec<SessionSummary>,
}

#[derive(Clone)]
pub struct ListSessionsTool;

impl McpServerTool for ListSessionsTool {
    type Input = ListSessionsParams;
    type Output = ListSessionsResult;
    const NAME: &'static str = "solution_agent.list_sessions";

    async fn run(
        &self,
        input: Self::Input,
        cx: &mut AsyncApp,
    ) -> Result<ToolResponse<Self::Output>> {
        let summaries = cx.update(|cx| {
            let store = SolutionAgentStore::global(cx);
            store.read_with(cx, |store, cx| {
                let mut out = Vec::new();
                let want_solution = input.solution_id.as_ref().map(|s| SolutionId(s.clone()));
                for entity in store.all_sessions() {
                    let session = entity.read(cx);
                    if let Some(want) = &want_solution {
                        if &session.solution_id != want {
                            continue;
                        }
                    }
                    out.push(session_summary(session));
                }
                out
            })
        });
        Ok(ToolResponse {
            content: vec![ToolResponseContent::Text {
                text: format!("{} session(s)", summaries.len()),
            }],
            structured_content: ListSessionsResult {
                sessions: summaries,
            },
        })
    }
}

fn session_summary(session: &SolutionSession) -> SessionSummary {
    SessionSummary {
        id: session.id.to_string(),
        solution_id: session.solution_id.0.clone(),
        agent_id: session.agent_id.to_string(),
        title: session.title.to_string(),
        state: format!("{:?}", session.state),
        created_at: session.created_at.timestamp_millis(),
        last_activity_at: session.last_activity_at.timestamp_millis(),
    }
}

// =====================================================================
// solution_agent.get_session
// =====================================================================

/// Fetch a session's metadata plus a per-entry preview (first ~200 chars
/// of each entry's markdown rendering). When the session has no live
/// `acp_thread`, `entries` is empty and only the metadata is populated.
#[derive(Debug, Clone, Default, Serialize, JsonSchema)]
pub struct GetSessionParams {
    pub session_id: String,
}

impl<'de> Deserialize<'de> for GetSessionParams {
    fn deserialize<D: Deserializer<'de>>(de: D) -> Result<Self, D::Error> {
        #[derive(Deserialize, Default)]
        #[serde(default, deny_unknown_fields)]
        struct Inner {
            session_id: String,
        }
        Ok(Self {
            session_id: Option::<Inner>::deserialize(de)?
                .unwrap_or_default()
                .session_id,
        })
    }
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct EntrySummary {
    /// One of "user" | "assistant" | "tool_call" | "plan".
    pub role: String,
    /// Markdown rendering of the entry, truncated to roughly 200 chars.
    pub preview: String,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct GetSessionResult {
    pub id: String,
    pub solution_id: String,
    pub agent_id: String,
    pub title: String,
    pub state: String,
    pub created_at: i64,
    pub last_activity_at: i64,
    pub entries: Vec<EntrySummary>,
}

#[derive(Clone)]
pub struct GetSessionTool;

impl McpServerTool for GetSessionTool {
    type Input = GetSessionParams;
    type Output = GetSessionResult;
    const NAME: &'static str = "solution_agent.get_session";

    async fn run(
        &self,
        input: Self::Input,
        cx: &mut AsyncApp,
    ) -> Result<ToolResponse<Self::Output>> {
        anyhow::ensure!(
            !input.session_id.is_empty(),
            "invalid_params: session_id is required"
        );
        let session_id = SolutionSessionId::parse(&input.session_id)
            .map_err(|e| anyhow!("bad session id: {e}"))?;

        let result = cx.update(|cx| -> Result<GetSessionResult> {
            let store = SolutionAgentStore::global(cx);
            let entity = store
                .read_with(cx, |store, _| store.session(session_id))
                .with_context(|| format!("session_not_found: {}", session_id))?;
            let session = entity.read(cx);
            let entries = session
                .acp_thread
                .as_ref()
                .map(|thread| {
                    thread
                        .read(cx)
                        .entries()
                        .iter()
                        .map(|entry| EntrySummary {
                            role: entry_role(entry).to_string(),
                            preview: truncate_preview(&entry.to_markdown(cx), 200),
                        })
                        .collect()
                })
                .unwrap_or_default();
            let summary = session_summary(session);
            Ok(GetSessionResult {
                id: summary.id,
                solution_id: summary.solution_id,
                agent_id: summary.agent_id,
                title: summary.title,
                state: summary.state,
                created_at: summary.created_at,
                last_activity_at: summary.last_activity_at,
                entries,
            })
        })?;

        let title = result.title.clone();
        Ok(ToolResponse {
            content: vec![ToolResponseContent::Text { text: title }],
            structured_content: result,
        })
    }
}

fn entry_role(entry: &acp_thread::AgentThreadEntry) -> &'static str {
    match entry {
        acp_thread::AgentThreadEntry::UserMessage(_) => "user",
        acp_thread::AgentThreadEntry::AssistantMessage(_) => "assistant",
        acp_thread::AgentThreadEntry::ToolCall(_) => "tool_call",
        acp_thread::AgentThreadEntry::CompletedPlan(_) => "plan",
    }
}

fn truncate_preview(s: &str, max_chars: usize) -> String {
    let mut out = String::new();
    for (count, ch) in s.chars().enumerate() {
        if count >= max_chars {
            out.push('…');
            return out;
        }
        out.push(ch);
    }
    out
}

// =====================================================================
// solution_agent.create_session
// =====================================================================

/// Create a new ACP session for `(solution_id, agent_id)` on the active
/// workspace's project. `initial_message`, if present, is dispatched as a
/// detached `send_message` after the session is registered.
///
/// **Active project resolution**: the session needs an `Entity<Project>`
/// from a live workspace window whose worktrees back the named Solution.
/// MCP doesn't carry a workspace handle, so we walk every open
/// `MultiWorkspace` window and pick the first project whose visible
/// worktrees include the Solution's root. If no such window is open, the
/// tool errors with a clear message — the caller should open the Solution
/// first via `solutions.open`.
#[derive(Debug, Clone, Default, Serialize, JsonSchema)]
pub struct CreateSessionParams {
    pub solution_id: String,
    pub agent_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub initial_message: Option<String>,
}

impl<'de> Deserialize<'de> for CreateSessionParams {
    fn deserialize<D: Deserializer<'de>>(de: D) -> Result<Self, D::Error> {
        #[derive(Deserialize, Default)]
        #[serde(default, deny_unknown_fields)]
        struct Inner {
            solution_id: String,
            agent_id: String,
            initial_message: Option<String>,
        }
        let inner = Option::<Inner>::deserialize(de)?.unwrap_or_default();
        Ok(Self {
            solution_id: inner.solution_id,
            agent_id: inner.agent_id,
            initial_message: inner.initial_message,
        })
    }
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct CreateSessionResult {
    pub session_id: String,
}

#[derive(Clone)]
pub struct CreateSessionTool;

impl McpServerTool for CreateSessionTool {
    type Input = CreateSessionParams;
    type Output = CreateSessionResult;
    const NAME: &'static str = "solution_agent.create_session";

    async fn run(
        &self,
        input: Self::Input,
        cx: &mut AsyncApp,
    ) -> Result<ToolResponse<Self::Output>> {
        anyhow::ensure!(
            !input.solution_id.is_empty(),
            "invalid_params: solution_id is required"
        );
        anyhow::ensure!(
            !input.agent_id.is_empty(),
            "invalid_params: agent_id is required"
        );
        let solution_id = SolutionId(input.solution_id.clone());
        let agent_id: crate::model::AgentServerId = input.agent_id.clone().into();

        let project = cx
            .update(|cx| project_for_solution(&input.solution_id, cx))
            .ok_or_else(|| {
                anyhow!(
                    "no_active_workspace_for_solution: open Solution {} via solutions.open before \
                     creating a session",
                    input.solution_id
                )
            })?;

        let create_task = cx.update(|cx| {
            let store = SolutionAgentStore::global(cx);
            store.update(cx, |store, cx| {
                store.create_session(solution_id, agent_id, project, cx)
            })
        });
        let session_id = create_task.await?;

        if let Some(content) = input.initial_message {
            cx.update(|cx| {
                let store = SolutionAgentStore::global(cx);
                store.update(cx, |store, cx| {
                    store.send_message(session_id, content, cx).detach();
                });
            });
        }

        Ok(ToolResponse {
            content: vec![ToolResponseContent::Text {
                text: session_id.to_string(),
            }],
            structured_content: CreateSessionResult {
                session_id: session_id.to_string(),
            },
        })
    }
}

// Locate the `Project` whose worktrees back the named Solution. Mirrors
// the helper of the same name in `solutions::mcp` (kept private there);
// duplicated here to avoid widening the `solutions` crate's public API
// just for this MCP tool.
fn project_for_solution(
    solution_id: &str,
    cx: &mut App,
) -> Option<Entity<project::Project>> {
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

// =====================================================================
// solution_agent.send_message
// =====================================================================

/// Send a user message to an existing session. Fire-and-forget — the
/// returned `Task` is detached so the tool response returns immediately
/// once the prompt is enqueued. Use `solution_agent.get_session` to poll
/// for new entries, or subscribe to `solution_agent.*` events (deferred
/// to a later phase) for push notifications.
#[derive(Debug, Clone, Default, Serialize, JsonSchema)]
pub struct SendMessageParams {
    pub session_id: String,
    pub content: String,
}

impl<'de> Deserialize<'de> for SendMessageParams {
    fn deserialize<D: Deserializer<'de>>(de: D) -> Result<Self, D::Error> {
        #[derive(Deserialize, Default)]
        #[serde(default, deny_unknown_fields)]
        struct Inner {
            session_id: String,
            content: String,
        }
        let inner = Option::<Inner>::deserialize(de)?.unwrap_or_default();
        Ok(Self {
            session_id: inner.session_id,
            content: inner.content,
        })
    }
}

#[derive(Debug, Clone, Default, Serialize, JsonSchema)]
pub struct SendMessageResult {}

#[derive(Clone)]
pub struct SendMessageTool;

impl McpServerTool for SendMessageTool {
    type Input = SendMessageParams;
    type Output = SendMessageResult;
    const NAME: &'static str = "solution_agent.send_message";

    async fn run(
        &self,
        input: Self::Input,
        cx: &mut AsyncApp,
    ) -> Result<ToolResponse<Self::Output>> {
        anyhow::ensure!(
            !input.session_id.is_empty(),
            "invalid_params: session_id is required"
        );
        let session_id = SolutionSessionId::parse(&input.session_id)
            .map_err(|e| anyhow!("bad session id: {e}"))?;
        let content = input.content;

        cx.update(|cx| {
            let store = SolutionAgentStore::global(cx);
            store.update(cx, |store, cx| {
                store.send_message(session_id, content, cx).detach();
            });
        });

        Ok(ToolResponse {
            content: vec![ToolResponseContent::Text {
                text: "queued".to_string(),
            }],
            structured_content: SendMessageResult {},
        })
    }
}

// =====================================================================
// solution_agent.close_session
// =====================================================================

/// Close a session, dropping its `AcpThread` and removing it from the
/// store. Mirrors `SolutionAgentStore::close_session` directly — the
/// pool's per-pair `live_session_count` is not decremented here because
/// the store's own `close_session` doesn't either (the only production
/// `pool_release_session` call site is the failed-spawn rollback in
/// `create_session`). Pool leakage on close is a pre-existing store
/// concern, not MCP-specific.
#[derive(Debug, Clone, Default, Serialize, JsonSchema)]
pub struct CloseSessionParams {
    pub session_id: String,
}

impl<'de> Deserialize<'de> for CloseSessionParams {
    fn deserialize<D: Deserializer<'de>>(de: D) -> Result<Self, D::Error> {
        #[derive(Deserialize, Default)]
        #[serde(default, deny_unknown_fields)]
        struct Inner {
            session_id: String,
        }
        Ok(Self {
            session_id: Option::<Inner>::deserialize(de)?
                .unwrap_or_default()
                .session_id,
        })
    }
}

#[derive(Debug, Clone, Default, Serialize, JsonSchema)]
pub struct CloseSessionResult {}

#[derive(Clone)]
pub struct CloseSessionTool;

impl McpServerTool for CloseSessionTool {
    type Input = CloseSessionParams;
    type Output = CloseSessionResult;
    const NAME: &'static str = "solution_agent.close_session";

    async fn run(
        &self,
        input: Self::Input,
        cx: &mut AsyncApp,
    ) -> Result<ToolResponse<Self::Output>> {
        anyhow::ensure!(
            !input.session_id.is_empty(),
            "invalid_params: session_id is required"
        );
        let session_id = SolutionSessionId::parse(&input.session_id)
            .map_err(|e| anyhow!("bad session id: {e}"))?;

        cx.update(|cx| -> Result<()> {
            let store = SolutionAgentStore::global(cx);
            store.update(cx, |store, cx| store.close_session(session_id, cx))?;
            Ok(())
        })?;

        Ok(ToolResponse {
            content: vec![ToolResponseContent::Text {
                text: "closed".to_string(),
            }],
            structured_content: CloseSessionResult {},
        })
    }
}
