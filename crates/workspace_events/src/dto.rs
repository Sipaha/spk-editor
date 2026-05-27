//! Wire DTOs for the workspace.* MCP namespace. Mirror the structure
//! described in `docs/superpowers/specs/2026-05-27-unified-open-workspace-design.md` §3.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use solutions::mcp::SolutionSummary;
use solution_agent::mcp::SessionSummary;

//  Output-only types: SolutionSummary / SessionSummary only derive Serialize,
//  so these container types cannot derive Deserialize either.
#[allow(dead_code)] // consumed by C2 onward
#[derive(Serialize, JsonSchema, Debug, Clone)]
pub struct WorkspaceSolution {
    #[serde(flatten)]
    pub solution: SolutionSummary,
    pub sessions: Vec<SessionSummary>,
}

#[allow(dead_code)] // consumed by C2 onward
#[derive(Serialize, JsonSchema, Debug, Clone)]
pub struct WorkspaceSnapshot {
    pub seq: u64,
    pub solutions: Vec<WorkspaceSolution>,
}

#[allow(dead_code)] // consumed by C2 onward
#[derive(Serialize, Deserialize, JsonSchema, Debug, Default, Clone)]
pub struct SnapshotParams {
    /// Reserved for future use; ignored today.
    #[serde(default)]
    pub _placeholder: Option<()>,
}

#[allow(dead_code)] // consumed by C2 onward
#[derive(Serialize, Deserialize, JsonSchema, Debug, Clone)]
pub struct ListSolutionsParams {
    /// None = both. Some(true) = only open. Some(false) = only closed.
    #[serde(default)]
    pub open: Option<bool>,
}

//  Output-only: SolutionSummary is Serialize-only, so no Deserialize here.
#[allow(dead_code)] // consumed by C2 onward
#[derive(Serialize, JsonSchema, Debug, Clone)]
pub struct ListSolutionsResult {
    pub solutions: Vec<SolutionSummary>,
}

#[allow(dead_code)] // consumed by C2 onward
#[derive(Serialize, Deserialize, JsonSchema, Debug, Clone)]
pub struct SolutionIdParam {
    pub solution_id: String,
}

#[allow(dead_code)] // consumed by C2 onward
#[derive(Serialize, Deserialize, JsonSchema, Debug, Clone)]
pub struct SessionIdParam {
    pub session_id: String,
}

#[allow(dead_code)] // consumed by C2 onward
#[derive(Serialize, Deserialize, JsonSchema, Debug, Clone)]
pub struct SeqAck {
    pub seq: u64,
}
