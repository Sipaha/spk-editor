//! End-to-end and unit MCP tests for workspace.snapshot.
//!
//! Integration test `snapshot_returns_seq_zero_and_empty_when_nothing_open`
//! goes through the live MCP socket; it pins the socket to a tempdir via
//! `editor_mcp::set_runtime_dir_for_test` (OnceLock — can only be set once
//! per process, so only one socket-level test can run).
//!
//! The two new tests (`snapshot_excludes_solutions_not_marked_open` and
//! `snapshot_includes_solution_marked_open`) bypass the socket entirely and
//! call `workspace_events::build_snapshot_for_test` directly, so they are
//! fully isolated from the OnceLock constraint and run in any order.

use std::sync::Arc;
use std::time::Duration;

use gpui::{Entity, TestAppContext};
use serde_json::json;
use settings::SettingsStore;
use smol::net::unix::UnixStream;
use tempfile::tempdir;

mod support;

#[gpui::test]
async fn snapshot_returns_seq_zero_and_empty_when_nothing_open(cx: &mut TestAppContext) {
    cx.executor().allow_parking();

    let runtime_dir = tempfile::tempdir().expect("runtime tempdir");
    let socket_path = runtime_dir.path().join("mcp.sock");
    editor_mcp::set_runtime_dir_for_test(runtime_dir.path().to_path_buf());

    let work_dir = tempfile::tempdir().expect("work tempdir");
    let cfg_path = work_dir.path().join("solutions.json");

    cx.update(|cx| {
        editor_mcp::init(cx);

        let settings_store = SettingsStore::test(cx);
        cx.set_global(settings_store);
        <solutions::SolutionsSettings as settings::Settings>::register(cx);
        let store = solutions::SolutionStore::for_test(cfg_path, cx);
        solutions::install_global_for_test(store, cx);

        let registry = Arc::new(solution_agent::adapter::AdapterRegistry::new());
        solution_agent::store::SolutionAgentStore::init_global(cx, registry);
        solution_agent::mcp::register(cx);
        solution_agent::event_sources::install(cx);

        workspace_events::init(cx);
    });

    let start_result = cx.update(|cx| editor_mcp::start_server(cx));
    assert!(
        start_result.is_ok(),
        "start_server: {:?}",
        start_result.err()
    );

    assert!(
        support::wait_for_socket(&socket_path, Duration::from_secs(10)).await,
        "mcp.sock did not appear within 10s at {}",
        socket_path.display()
    );

    let mut stream = UnixStream::connect(&socket_path)
        .await
        .expect("connect to socket");

    let resp = support::call_tool(&mut stream, 1, "workspace.snapshot", json!({})).await;

    let result = resp
        .pointer("/result/structuredContent")
        .unwrap_or_else(|| panic!("missing structuredContent; full response: {resp}"));

    assert_eq!(
        result["seq"].as_u64(),
        Some(0),
        "expected seq=0 at startup; full response: {resp}"
    );
    let solutions = result["solutions"]
        .as_array()
        .unwrap_or_else(|| panic!("expected solutions array; full response: {resp}"));
    assert!(
        solutions.is_empty(),
        "expected empty solutions at startup; full response: {resp}"
    );

    drop(runtime_dir);
    drop(work_dir);
}

/// A solution that has never had `mark_open` called must be excluded from the
/// snapshot — the `open` filter depends on stored runtime state, not live
/// window enumeration. Calls `build_snapshot_for_test` directly (no socket)
/// to stay isolated from the OnceLock runtime-dir constraint.
#[gpui::test]
async fn snapshot_excludes_solutions_not_marked_open(cx: &mut TestAppContext) {
    let work_dir = tempfile::tempdir().expect("work tempdir");

    cx.update(|cx| {
        editor_mcp::init(cx);

        let settings_store = SettingsStore::test(cx);
        cx.set_global(settings_store);
        <solutions::SolutionsSettings as settings::Settings>::register(cx);
        let store = solutions::SolutionStore::for_test(work_dir.path().join("s.json"), cx);
        solutions::install_global_for_test(store.clone(), cx);

        let registry = Arc::new(solution_agent::adapter::AdapterRegistry::new());
        solution_agent::store::SolutionAgentStore::init_global(cx, registry);

        workspace_events::init(cx);

        // Create a solution but do NOT call mark_open. Snapshot must exclude it.
        store.update(cx, |s, cx| {
            s.create_for_test_minimal("hidden", cx);
        });
    });
    cx.run_until_parked();

    let snap = cx.update(|cx| workspace_events::build_snapshot_for_test(cx));
    assert!(
        snap.solutions.is_empty(),
        "solution without mark_open must be filtered out; got {:?}",
        snap.solutions.iter().map(|s| &s.solution.name).collect::<Vec<_>>()
    );
}

/// A solution with `mark_open` called must appear in the snapshot.
/// Calls `build_snapshot_for_test` directly (no socket).
#[gpui::test]
async fn snapshot_includes_solution_marked_open(cx: &mut TestAppContext) {
    let work_dir = tempfile::tempdir().expect("work tempdir");

    cx.update(|cx| {
        editor_mcp::init(cx);

        let settings_store = SettingsStore::test(cx);
        cx.set_global(settings_store);
        <solutions::SolutionsSettings as settings::Settings>::register(cx);
        let store = solutions::SolutionStore::for_test(work_dir.path().join("s.json"), cx);
        solutions::install_global_for_test(store.clone(), cx);

        let registry = Arc::new(solution_agent::adapter::AdapterRegistry::new());
        solution_agent::store::SolutionAgentStore::init_global(cx, registry);

        workspace_events::init(cx);

        // Create a solution and mark it open.
        store.update(cx, |s, cx| {
            let id = s.create_for_test_minimal("visible", cx);
            s.mark_open(id, cx);
        });
    });
    cx.run_until_parked();

    let snap = cx.update(|cx| workspace_events::build_snapshot_for_test(cx));
    assert_eq!(
        snap.solutions.len(),
        1,
        "solution with mark_open must appear in snapshot; got {:?}",
        snap.solutions.iter().map(|s| &s.solution.name).collect::<Vec<_>>()
    );
    assert_eq!(snap.solutions[0].solution.name, "visible");
}

// ── list_solutions tests ──────────────────────────────────────────────────

#[gpui::test]
async fn list_solutions_with_open_true_returns_only_open(cx: &mut TestAppContext) {
    let work_dir = tempfile::tempdir().expect("work tempdir");

    let open_id = cx.update(|cx| {
        editor_mcp::init(cx);
        let settings_store = SettingsStore::test(cx);
        cx.set_global(settings_store);
        <solutions::SolutionsSettings as settings::Settings>::register(cx);
        let store = solutions::SolutionStore::for_test(work_dir.path().join("s.json"), cx);
        solutions::install_global_for_test(store.clone(), cx);

        let registry = Arc::new(solution_agent::adapter::AdapterRegistry::new());
        solution_agent::store::SolutionAgentStore::init_global(cx, registry);

        workspace_events::init(cx);

        let open_id = store.update(cx, |s, cx| s.create_for_test_minimal("open-one", cx));
        store.update(cx, |s, cx| s.create_for_test_minimal("closed-one", cx));
        store.update(cx, |s, cx| s.mark_open(open_id.clone(), cx));
        open_id
    });
    cx.run_until_parked();

    let result = cx.update(|cx| workspace_events::list_solutions_for_test(cx, Some(true)));
    assert_eq!(result.solutions.len(), 1, "expected 1 open solution");
    assert_eq!(result.solutions[0].id, open_id.as_str());
}

#[gpui::test]
async fn list_solutions_with_open_false_returns_only_closed(cx: &mut TestAppContext) {
    let work_dir = tempfile::tempdir().expect("work tempdir");

    let closed_id = cx.update(|cx| {
        editor_mcp::init(cx);
        let settings_store = SettingsStore::test(cx);
        cx.set_global(settings_store);
        <solutions::SolutionsSettings as settings::Settings>::register(cx);
        let store = solutions::SolutionStore::for_test(work_dir.path().join("s.json"), cx);
        solutions::install_global_for_test(store.clone(), cx);

        let registry = Arc::new(solution_agent::adapter::AdapterRegistry::new());
        solution_agent::store::SolutionAgentStore::init_global(cx, registry);

        workspace_events::init(cx);

        let open_id = store.update(cx, |s, cx| s.create_for_test_minimal("open-one", cx));
        let closed_id = store.update(cx, |s, cx| s.create_for_test_minimal("closed-one", cx));
        store.update(cx, |s, cx| s.mark_open(open_id, cx));
        closed_id
    });
    cx.run_until_parked();

    let result = cx.update(|cx| workspace_events::list_solutions_for_test(cx, Some(false)));
    assert_eq!(result.solutions.len(), 1, "expected 1 closed solution");
    assert_eq!(result.solutions[0].id, closed_id.as_str());
}

#[gpui::test]
async fn list_solutions_with_none_returns_both(cx: &mut TestAppContext) {
    let work_dir = tempfile::tempdir().expect("work tempdir");

    cx.update(|cx| {
        editor_mcp::init(cx);
        let settings_store = SettingsStore::test(cx);
        cx.set_global(settings_store);
        <solutions::SolutionsSettings as settings::Settings>::register(cx);
        let store = solutions::SolutionStore::for_test(work_dir.path().join("s.json"), cx);
        solutions::install_global_for_test(store.clone(), cx);

        let registry = Arc::new(solution_agent::adapter::AdapterRegistry::new());
        solution_agent::store::SolutionAgentStore::init_global(cx, registry);

        workspace_events::init(cx);

        let open_id = store.update(cx, |s, cx| s.create_for_test_minimal("a", cx));
        store.update(cx, |s, cx| s.create_for_test_minimal("b", cx));
        store.update(cx, |s, cx| s.mark_open(open_id, cx));
    });
    cx.run_until_parked();

    let result = cx.update(|cx| workspace_events::list_solutions_for_test(cx, None));
    assert_eq!(result.solutions.len(), 2, "expected both solutions");
}

// ── lifecycle: open_session / close_session tests ─────────────────────────────

fn setup_with_open_solution_and_one_session(
    cx: &mut TestAppContext,
    runtime_dir: &tempfile::TempDir,
) -> (solutions::SolutionId, solution_agent::SolutionSessionId) {
    cx.update(|cx| {
        let settings_store = SettingsStore::test(cx);
        cx.set_global(settings_store);
        <solutions::SolutionsSettings as settings::Settings>::register(cx);
        editor_mcp::init(cx);

        let store = solutions::SolutionStore::for_test(runtime_dir.path().join("s.json"), cx);
        solutions::install_global_for_test(store.clone(), cx);

        let registry = Arc::new(solution_agent::adapter::AdapterRegistry::new());
        solution_agent::store::SolutionAgentStore::init_global(cx, registry);
        solution_agent::mcp::register(cx);
        workspace_events::init(cx);

        let sol_id = store.update(cx, |s, cx| s.create_for_test_minimal("a", cx));
        store.update(cx, |s, cx| s.mark_open(sol_id.clone(), cx));

        let agent = solution_agent::store::SolutionAgentStore::global(cx);
        let sess_id = agent.update(cx, |a, cx| {
            a.create_for_test_minimal(&sol_id, "session-a", cx)
        });
        (sol_id, sess_id)
    })
}

#[gpui::test]
async fn open_session_adds_to_tab_strip(cx: &mut TestAppContext) {
    let runtime_dir = tempdir().expect("tempdir");
    let (_sol_id, sess_id) = setup_with_open_solution_and_one_session(cx, &runtime_dir);

    let pre_seq = cx.update(|cx| workspace_events::current_seq_for_test(cx));
    let ack = cx.update(|cx| workspace_events::open_session_for_test(cx, &sess_id));
    assert!(ack.seq > pre_seq);

    // Snapshot should now include this session.
    let snap = cx.update(|cx| workspace_events::build_snapshot_for_test(cx));
    assert_eq!(snap.solutions.len(), 1);
    assert_eq!(snap.solutions[0].sessions.len(), 1);
}

#[gpui::test]
async fn open_session_for_already_open_is_noop(cx: &mut TestAppContext) {
    let runtime_dir = tempdir().expect("tempdir");
    let (_sol_id, sess_id) = setup_with_open_solution_and_one_session(cx, &runtime_dir);

    // First open: real mutation.
    cx.update(|cx| workspace_events::open_session_for_test(cx, &sess_id));
    let mid_seq = cx.update(|cx| workspace_events::current_seq_for_test(cx));

    // Second open: no-op.
    let ack = cx.update(|cx| workspace_events::open_session_for_test(cx, &sess_id));
    assert_eq!(ack.seq, mid_seq);
}

#[gpui::test]
async fn close_session_removes_from_tab_strip(cx: &mut TestAppContext) {
    let runtime_dir = tempdir().expect("tempdir");
    let (_sol_id, sess_id) = setup_with_open_solution_and_one_session(cx, &runtime_dir);

    cx.update(|cx| workspace_events::open_session_for_test(cx, &sess_id));
    let mid_seq = cx.update(|cx| workspace_events::current_seq_for_test(cx));

    let ack = cx.update(|cx| workspace_events::close_session_for_test(cx, &sess_id));
    assert!(ack.seq > mid_seq);

    // Snapshot should NOT include this session anymore.
    let snap = cx.update(|cx| workspace_events::build_snapshot_for_test(cx));
    assert_eq!(snap.solutions[0].sessions.len(), 0);
}

#[gpui::test]
async fn close_session_for_already_closed_is_noop(cx: &mut TestAppContext) {
    let runtime_dir = tempdir().expect("tempdir");
    let (_sol_id, sess_id) = setup_with_open_solution_and_one_session(cx, &runtime_dir);

    // Never opened — close is no-op.
    let pre_seq = cx.update(|cx| workspace_events::current_seq_for_test(cx));
    let ack = cx.update(|cx| workspace_events::close_session_for_test(cx, &sess_id));
    assert_eq!(ack.seq, pre_seq);
}

// ── lifecycle: open_solution / close_solution tests ──────────────────────────

fn setup_lifecycle_test(
    work_dir: &std::path::Path,
    cx: &mut gpui::App,
) -> Entity<solutions::SolutionStore> {
    editor_mcp::init(cx);
    let settings_store = SettingsStore::test(cx);
    cx.set_global(settings_store);
    <solutions::SolutionsSettings as settings::Settings>::register(cx);
    let store = solutions::SolutionStore::for_test(work_dir.join("s.json"), cx);
    solutions::install_global_for_test(store.clone(), cx);
    let registry = Arc::new(solution_agent::adapter::AdapterRegistry::new());
    solution_agent::store::SolutionAgentStore::init_global(cx, registry);
    solution_agent::mcp::register(cx);
    workspace_events::init(cx);
    store
}

#[gpui::test]
async fn open_solution_for_already_open_is_noop(cx: &mut TestAppContext) {
    let work_dir = tempdir().expect("work tempdir");

    let sol_id = cx.update(|cx| {
        let store = setup_lifecycle_test(work_dir.path(), cx);
        let id = store.update(cx, |s, cx| s.create_for_test_minimal("a", cx));
        store.update(cx, |s, cx| s.mark_open(id.clone(), cx));
        id
    });

    let pre_seq = cx.update(|cx| workspace_events::current_seq_for_test(cx));
    let ack = cx.update(|cx| workspace_events::open_solution_for_test(cx, &sol_id));
    assert_eq!(ack.seq, pre_seq, "no-op must not advance seq");
}

#[gpui::test]
async fn open_solution_for_closed_marks_open_and_advances_seq(cx: &mut TestAppContext) {
    let work_dir = tempdir().expect("work tempdir");

    let sol_id = cx.update(|cx| {
        let store = setup_lifecycle_test(work_dir.path(), cx);
        store.update(cx, |s, cx| s.create_for_test_minimal("a", cx))
    });

    let pre_seq = cx.update(|cx| workspace_events::current_seq_for_test(cx));
    let ack = cx.update(|cx| workspace_events::open_solution_for_test(cx, &sol_id));
    assert!(ack.seq > pre_seq, "open must advance seq");

    let is_open = cx.update(|cx| {
        solutions::SolutionStore::global(cx).read(cx).is_open(&sol_id)
    });
    assert!(is_open, "solution must be marked open after open_solution");
}

#[gpui::test]
async fn close_solution_marks_closed_and_advances_seq(cx: &mut TestAppContext) {
    let work_dir = tempdir().expect("work tempdir");

    let sol_id = cx.update(|cx| {
        let store = setup_lifecycle_test(work_dir.path(), cx);
        let id = store.update(cx, |s, cx| s.create_for_test_minimal("a", cx));
        store.update(cx, |s, cx| s.mark_open(id.clone(), cx));
        id
    });

    let pre_seq = cx.update(|cx| workspace_events::current_seq_for_test(cx));
    let ack = cx.update(|cx| workspace_events::close_solution_for_test(cx, &sol_id));
    assert!(ack.seq > pre_seq, "close must advance seq");

    let is_open = cx.update(|cx| {
        solutions::SolutionStore::global(cx).read(cx).is_open(&sol_id)
    });
    assert!(!is_open, "solution must be marked closed after close_solution");
}

#[gpui::test]
async fn close_solution_for_already_closed_is_noop(cx: &mut TestAppContext) {
    let work_dir = tempdir().expect("work tempdir");

    let sol_id = cx.update(|cx| {
        let store = setup_lifecycle_test(work_dir.path(), cx);
        store.update(cx, |s, cx| s.create_for_test_minimal("a", cx))
    });

    let pre_seq = cx.update(|cx| workspace_events::current_seq_for_test(cx));
    let ack = cx.update(|cx| workspace_events::close_solution_for_test(cx, &sol_id));
    assert_eq!(ack.seq, pre_seq, "close on already-closed must be no-op");
}
