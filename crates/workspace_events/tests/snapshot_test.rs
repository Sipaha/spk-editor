//! End-to-end MCP smoke tests for workspace.snapshot.
//!
//! Isolation: pins the lock + socket to a tempdir via
//! `editor_mcp::set_runtime_dir_for_test` so it is safe to run alongside a
//! live `spk-editor` instance.

use std::sync::Arc;
use std::time::Duration;

use gpui::TestAppContext;
use serde_json::json;
use settings::SettingsStore;
use smol::net::unix::UnixStream;

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
