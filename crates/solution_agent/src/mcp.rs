//! MCP tools exposed by the `solution_agent` crate. Tools register with the
//! central `editor_mcp` registry from `solution_agent::init` so that
//! `start_server` (called later from `crates/zed/src/main.rs`) sees them
//! when binding the socket.
use anyhow::Result;
use context_server::listener::{McpServerTool, ToolResponse};
use context_server::types::ToolResponseContent;
use gpui::{App, AsyncApp};
use schemars::JsonSchema;
use serde::{Deserialize, Deserializer, Serialize};

use crate::store::SolutionAgentStore;
use solutions::SolutionId;

pub fn register(cx: &mut App) {
    editor_mcp::register_tool(cx, |server| {
        server.add_tool(ListSessionsTool);
    });
    // Tasks 5.2 / 5.3 add the other 7 tools and re-enable these lines:
    // editor_mcp::register_tool(cx, |server| server.add_tool(GetSessionTool));
    // editor_mcp::register_tool(cx, |server| server.add_tool(CreateSessionTool));
    // editor_mcp::register_tool(cx, |server| server.add_tool(SendMessageTool));
    // editor_mcp::register_tool(cx, |server| server.add_tool(CancelTurnTool));
    // editor_mcp::register_tool(cx, |server| server.add_tool(CloseSessionTool));
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
                    out.push(SessionSummary {
                        id: session.id.to_string(),
                        solution_id: session.solution_id.0.clone(),
                        agent_id: session.agent_id.to_string(),
                        title: session.title.to_string(),
                        state: format!("{:?}", session.state),
                        created_at: session.created_at.timestamp_millis(),
                        last_activity_at: session.last_activity_at.timestamp_millis(),
                    });
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
