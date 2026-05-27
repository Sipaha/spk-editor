//! Lifecycle MCP tools for the workspace.* namespace:
//! `open_solution`, `close_solution`, `open_session`, `close_session`.
//! Each is idempotent at the store level — if the requested state
//! already holds, the tool returns the current seq with no emit.

use anyhow::{Result, anyhow};
use context_server::listener::{McpServerTool, ToolResponse};
use context_server::types::ToolResponseContent;
use gpui::{App, AsyncApp};
use solutions::{SolutionId, SolutionStore};

use crate::coordinator::WorkspaceEventCoordinator;
use crate::dto::{SeqAck, SessionIdParam, SolutionIdParam};

pub(crate) fn open_solution_impl(cx: &mut App, id: &SolutionId) -> Result<u64> {
    let store = SolutionStore::try_global(cx)
        .ok_or_else(|| anyhow!("SolutionStore not initialised"))?;
    let coord = WorkspaceEventCoordinator::global(cx);

    let was_open = store.read(cx).is_open(id);
    if was_open {
        return Ok(coord.current_seq());
    }

    // Hydrate restored sessions for this solution (idempotent if already hydrated).
    // Done before mark_open so any sessions in memory are captured by the snapshot.
    if let Some(agent) = solution_agent::store::SolutionAgentStore::try_global(cx) {
        let _ = agent.update(cx, |a, cx| a.hydrate_all_for_solution(id.clone(), cx));
        // The hydration is a Task<_> — we don't await here; the notification
        // reflects whatever state is in memory. The mobile client re-syncs on
        // reconnect via workspace.snapshot anyway.
    }

    // mark_open itself emits the sequenced workspace.solution_opened event.
    store.update(cx, |s, cx| s.mark_open(id.clone(), cx));

    // Return the seq just reserved by mark_open's emit_sequenced call.
    Ok(WorkspaceEventCoordinator::global(cx).current_seq())
}

pub(crate) fn close_solution_impl(cx: &mut App, id: &SolutionId) -> Result<u64> {
    let store = SolutionStore::try_global(cx)
        .ok_or_else(|| anyhow!("SolutionStore not initialised"))?;
    let coord = WorkspaceEventCoordinator::global(cx);

    let was_open = store.read(cx).is_open(id);
    if !was_open {
        return Ok(coord.current_seq());
    }

    // Terminate the solution's agent threads + terminals first. Sessions stay
    // on disk (tab_order, transcripts preserved). Only running runtime state
    // is killed.
    crate::shutdown::shutdown_solution_runtime(id, cx);

    // mark_closed itself emits the sequenced workspace.solution_closed event.
    store.update(cx, |s, cx| s.mark_closed(id, cx));

    Ok(WorkspaceEventCoordinator::global(cx).current_seq())
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

    // persist_tab_order now emits workspace.session_opened internally
    // via WorkspaceEventCoordinator::emit_sequenced (F5). No manual
    // emit here — doing so would double-fire the notification.
    agent.update(cx, |a, cx| {
        a.persist_tab_order(solution_id.clone(), new_order, cx)
    });

    // Return the seq that persist_tab_order just reserved.
    Ok(WorkspaceEventCoordinator::global(cx).current_seq())
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

    // persist_tab_order now emits workspace.session_closed internally
    // via WorkspaceEventCoordinator::emit_sequenced (F5). No manual
    // emit here — doing so would double-fire the notification.
    agent.update(cx, |a, cx| {
        a.persist_tab_order(solution_id.clone(), new_order, cx)
    });

    // Return the seq that persist_tab_order just reserved.
    Ok(WorkspaceEventCoordinator::global(cx).current_seq())
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
