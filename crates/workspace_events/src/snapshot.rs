//! Build a `WorkspaceSnapshot` for the wire `workspace.snapshot` tool.

use crate::coordinator::WorkspaceEventCoordinator;
use crate::dto::{WorkspaceSnapshot, WorkspaceSolution};
use gpui::App;

pub(crate) fn build_snapshot(cx: &App) -> WorkspaceSnapshot {
    let coord = WorkspaceEventCoordinator::global(cx);
    let seq = coord.current_seq();

    // If either store is uninitialised (very early during boot or in tests
    // that didn't wire it), return an empty snapshot — never panic.
    let solution_store = match solutions::SolutionStore::try_global(cx) {
        Some(s) => s,
        None => return WorkspaceSnapshot { seq, solutions: Vec::new() },
    };

    let agent_store = solution_agent::store::SolutionAgentStore::try_global(cx);

    let solutions: Vec<WorkspaceSolution> = solution_store.read_with(cx, |store, cx| {
        let mut result = Vec::new();
        for sol in store.solutions() {
            let sol_id = sol.id.clone();
            let sessions = if let Some(agent_store_ref) = agent_store.as_ref() {
                agent_store_ref.read_with(cx, |agent, cx| {
                    agent
                        .all_sessions()
                        .filter_map(|entity| {
                            let session = entity.read(cx);
                            if session.solution_id == sol_id {
                                Some(solution_agent::mcp::session_summary(session, cx))
                            } else {
                                None
                            }
                        })
                        .collect()
                })
            } else {
                Vec::new()
            };
            result.push(WorkspaceSolution {
                solution: solutions::mcp::build_summary(sol, cx),
                sessions,
            });
        }
        result
    });

    WorkspaceSnapshot { seq, solutions }
}
