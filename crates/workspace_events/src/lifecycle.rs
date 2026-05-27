//! Lifecycle MCP tools for the workspace.* namespace:
//! `open_solution`, `close_solution`, `open_session`, `close_session`.
//! Each is idempotent at the store level — if the requested state
//! already holds, the tool returns the current seq with no emit.

use anyhow::{Result, anyhow};
use context_server::listener::{McpServerTool, ToolResponse};
use context_server::types::ToolResponseContent;
use gpui::{App, AsyncApp};
use serde_json::json;
use solutions::{SolutionId, SolutionStore};

use crate::coordinator::WorkspaceEventCoordinator;
use crate::dto::{SeqAck, SessionIdParam, SolutionIdParam};

pub(crate) fn open_solution_impl(cx: &mut App, id: &SolutionId) -> Result<u64> {
    let store = SolutionStore::try_global(cx)
        .ok_or_else(|| anyhow!("SolutionStore not initialised"))?;

    let was_open = store.read(cx).is_open(id);
    if was_open {
        // No-op: read seq as a plain u64 (borrow ends immediately).
        let seq = WorkspaceEventCoordinator::global(cx).current_seq();
        return Ok(seq);
    }

    // Reserve the next sequence number before any mutation so that consumers
    // can never observe a snapshot with a seq newer than the delta they just
    // received.
    let seq = WorkspaceEventCoordinator::global(cx).next_seq();

    // Mark open (emits Changed via SolutionStoreEvent::Changed).
    store.update(cx, |s, cx| s.mark_open(id.clone(), cx));

    // Hydrate restored sessions for this solution (idempotent if already hydrated).
    if let Some(agent) = solution_agent::store::SolutionAgentStore::try_global(cx) {
        let _ = agent.update(cx, |a, cx| a.hydrate_all_for_solution(id.clone(), cx));
        // The hydration is a Task<_> — we don't await here; the notification
        // reflects whatever state is in memory. The mobile client re-syncs on
        // reconnect via workspace.snapshot anyway.
    }

    // Build payload: solution summary + restored sessions array.
    let solution = store.read_with(cx, |store, cx| {
        store
            .solutions()
            .iter()
            .find(|s| &s.id == id)
            .map(|sol| solutions::mcp::build_summary(sol, cx))
    });
    let sessions = solution_agent::store::SolutionAgentStore::try_global(cx)
        .map(|agent| {
            agent.read_with(cx, |a, cx| {
                a.all_sessions()
                    .filter_map(|entity| {
                        let session = entity.read(cx);
                        if &session.solution_id == id && session.tab_order.is_some() {
                            Some(solution_agent::mcp::session_summary(session, cx))
                        } else {
                            None
                        }
                    })
                    .collect::<Vec<_>>()
            })
        })
        .unwrap_or_default();

    let payload = json!({
        "seq": seq,
        "solution": solution,
        "sessions": sessions,
    });
    // emit_notification is public from editor_mcp; bypasses the re-borrow
    // issue from holding &WorkspaceEventCoordinator while also needing &mut cx.
    editor_mcp::emit_notification(cx, "workspace.solution_opened", payload);
    Ok(seq)
}

pub(crate) fn close_solution_impl(cx: &mut App, id: &SolutionId) -> Result<u64> {
    let store = SolutionStore::try_global(cx)
        .ok_or_else(|| anyhow!("SolutionStore not initialised"))?;

    let was_open = store.read(cx).is_open(id);
    if !was_open {
        let seq = WorkspaceEventCoordinator::global(cx).current_seq();
        return Ok(seq);
    }

    // Reserve next seq before mutation.
    let seq = WorkspaceEventCoordinator::global(cx).next_seq();

    // Phase H will add agent + terminal termination here.

    store.update(cx, |s, cx| s.mark_closed(id, cx));

    let payload = json!({
        "seq": seq,
        "solution_id": id.as_str(),
    });
    editor_mcp::emit_notification(cx, "workspace.solution_closed", payload);
    Ok(seq)
}

#[derive(Clone)]
pub struct OpenSolutionTool;

impl McpServerTool for OpenSolutionTool {
    type Input = SolutionIdParam;
    type Output = SeqAck;
    const NAME: &'static str = "workspace.open_solution";

    async fn run(
        &self,
        input: Self::Input,
        cx: &mut AsyncApp,
    ) -> Result<ToolResponse<Self::Output>> {
        let id = SolutionId(input.solution_id.into());
        let seq = cx.update(|cx| open_solution_impl(cx, &id))?;
        Ok(ToolResponse {
            content: vec![ToolResponseContent::Text {
                text: format!("seq={seq}"),
            }],
            structured_content: SeqAck { seq },
        })
    }
}

#[derive(Clone)]
pub struct CloseSolutionTool;

impl McpServerTool for CloseSolutionTool {
    type Input = SolutionIdParam;
    type Output = SeqAck;
    const NAME: &'static str = "workspace.close_solution";

    async fn run(
        &self,
        input: Self::Input,
        cx: &mut AsyncApp,
    ) -> Result<ToolResponse<Self::Output>> {
        let id = SolutionId(input.solution_id.into());
        let seq = cx.update(|cx| close_solution_impl(cx, &id))?;
        Ok(ToolResponse {
            content: vec![ToolResponseContent::Text {
                text: format!("seq={seq}"),
            }],
            structured_content: SeqAck { seq },
        })
    }
}

// ── Session lifecycle ─────────────────────────────────────────────────────────

pub(crate) fn open_session_impl(cx: &mut App, session_id_str: &str) -> Result<u64> {
    let agent = solution_agent::store::SolutionAgentStore::try_global(cx)
        .ok_or_else(|| anyhow!("SolutionAgentStore not initialised"))?;
    let session_id = solution_agent::SolutionSessionId::parse(session_id_str)
        .map_err(|e| anyhow!("bad session_id: {e}"))?;

    // Read current state of the session.
    let (solution_id, already_in_strip) = agent.read_with(cx, |a, cx| {
        let entity = a
            .session(session_id)
            .ok_or_else(|| anyhow!("session not found"))?;
        let s = entity.read(cx);
        Ok::<_, anyhow::Error>((s.solution_id.clone(), s.tab_order.is_some()))
    })?;
    if already_in_strip {
        // No-op: return current seq without storing a borrow.
        return Ok(WorkspaceEventCoordinator::global(cx).current_seq());
    }

    // Build new ordered list = current tab-strip ids + this one (appended at end).
    let new_order: Vec<solution_agent::SolutionSessionId> = agent.read_with(cx, |a, cx| {
        let mut current: Vec<_> = a
            .all_sessions()
            .filter_map(|entity| {
                let s = entity.read(cx);
                if s.solution_id == solution_id && s.tab_order.is_some() {
                    Some((s.id, s.tab_order.unwrap()))
                } else {
                    None
                }
            })
            .collect();
        current.sort_by_key(|(_, ord)| *ord);
        let mut ids: Vec<_> = current.into_iter().map(|(id, _)| id).collect();
        ids.push(session_id);
        ids
    });

    // Persist (updates in-memory + DB).
    agent.update(cx, |a, cx| {
        a.persist_tab_order(solution_id.clone(), new_order, cx)
    });

    // Build payload.
    let summary = agent.read_with(cx, |a, cx| {
        let entity = a.session(session_id).expect("just added");
        let s = entity.read(cx);
        solution_agent::mcp::session_summary(s, cx)
    });
    // Reserve seq after all mutations so snapshot is consistent.
    let seq = WorkspaceEventCoordinator::global(cx).next_seq();
    let payload = json!({
        "seq": seq,
        "solution_id": solution_id.as_str(),
        "session": summary,
    });
    editor_mcp::emit_notification(cx, "workspace.session_opened", payload);
    Ok(seq)
}

pub(crate) fn close_session_impl(cx: &mut App, session_id_str: &str) -> Result<u64> {
    let agent = solution_agent::store::SolutionAgentStore::try_global(cx)
        .ok_or_else(|| anyhow!("SolutionAgentStore not initialised"))?;
    let session_id = solution_agent::SolutionSessionId::parse(session_id_str)
        .map_err(|e| anyhow!("bad session_id: {e}"))?;

    let (solution_id, was_in_strip) = agent.read_with(cx, |a, cx| {
        let entity = a
            .session(session_id)
            .ok_or_else(|| anyhow!("session not found"))?;
        let s = entity.read(cx);
        Ok::<_, anyhow::Error>((s.solution_id.clone(), s.tab_order.is_some()))
    })?;
    if !was_in_strip {
        return Ok(WorkspaceEventCoordinator::global(cx).current_seq());
    }

    // Build new ordered list = current minus this session.
    let new_order: Vec<solution_agent::SolutionSessionId> = agent.read_with(cx, |a, cx| {
        let mut current: Vec<_> = a
            .all_sessions()
            .filter_map(|entity| {
                let s = entity.read(cx);
                if s.solution_id == solution_id
                    && s.tab_order.is_some()
                    && s.id != session_id
                {
                    Some((s.id, s.tab_order.unwrap()))
                } else {
                    None
                }
            })
            .collect();
        current.sort_by_key(|(_, ord)| *ord);
        current.into_iter().map(|(id, _)| id).collect()
    });

    agent.update(cx, |a, cx| {
        a.persist_tab_order(solution_id.clone(), new_order, cx)
    });

    let seq = WorkspaceEventCoordinator::global(cx).next_seq();
    let payload = json!({
        "seq": seq,
        "solution_id": solution_id.as_str(),
        "session_id": session_id.to_string(),
    });
    editor_mcp::emit_notification(cx, "workspace.session_closed", payload);
    Ok(seq)
}

#[derive(Clone)]
pub struct OpenSessionTool;

impl McpServerTool for OpenSessionTool {
    type Input = SessionIdParam;
    type Output = SeqAck;
    const NAME: &'static str = "workspace.open_session";

    async fn run(
        &self,
        input: Self::Input,
        cx: &mut AsyncApp,
    ) -> Result<ToolResponse<Self::Output>> {
        let seq = cx.update(|cx| open_session_impl(cx, &input.session_id))?;
        Ok(ToolResponse {
            content: vec![ToolResponseContent::Text {
                text: format!("seq={seq}"),
            }],
            structured_content: SeqAck { seq },
        })
    }
}

#[derive(Clone)]
pub struct CloseSessionTool;

impl McpServerTool for CloseSessionTool {
    type Input = SessionIdParam;
    type Output = SeqAck;
    const NAME: &'static str = "workspace.close_session";

    async fn run(
        &self,
        input: Self::Input,
        cx: &mut AsyncApp,
    ) -> Result<ToolResponse<Self::Output>> {
        let seq = cx.update(|cx| close_session_impl(cx, &input.session_id))?;
        Ok(ToolResponse {
            content: vec![ToolResponseContent::Text {
                text: format!("seq={seq}"),
            }],
            structured_content: SeqAck { seq },
        })
    }
}
