//! Integration tests driving a fake `claude` binary (a bash script) through
//! [`claude_native::process::ClaudeProcess`]. These exercise the real spawn +
//! stdio async tasks; the protocol/translate units are tested in-crate.

use std::path::PathBuf;
use std::time::Duration;

use claude_native::command::{ClaudeCommandSpec, SessionArg};
use claude_native::process::ClaudeProcess;
use claude_native::protocol::{InputMessage, OutputMessage, System};
use futures::{FutureExt as _, StreamExt as _};
use gpui::TestAppContext;

fn mock_binary() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("mock_claude.sh")
}

fn spec_for(binary: PathBuf, capture: Option<PathBuf>) -> ClaudeCommandSpec {
    let mut extra_env = Vec::new();
    if let Some(path) = capture {
        extra_env.push((
            "MOCK_CLAUDE_CAPTURE".to_string(),
            path.to_string_lossy().into_owned(),
        ));
    }
    ClaudeCommandSpec {
        binary,
        work_dir: std::env::temp_dir(),
        session: SessionArg::New("mock-session".into()),
        mcp_servers_json: r#"{"mcpServers":{}}"#.into(),
        append_system_prompt: None,
        extra_env,
    }
}

/// Pull the next message off `incoming`, failing the test if it does not arrive
/// before the deadline (so a wedged reader is a test failure, not a hang).
async fn recv_with_timeout(
    process: &mut ClaudeProcess,
    cx: &mut TestAppContext,
) -> Option<OutputMessage> {
    let timeout = cx.background_executor.timer(Duration::from_secs(10)).fuse();
    let next = process.incoming.next().fuse();
    futures::pin_mut!(timeout, next);
    futures::select! {
        message = next => message,
        _ = timeout => panic!("timed out waiting for output message"),
    }
}

#[gpui::test]
async fn reads_init_message(cx: &mut TestAppContext) {
    // Real subprocess stdio is driven by the executor's I/O reactor, which only
    // makes progress when the deterministic test executor is allowed to park.
    cx.executor().allow_parking();

    let spec = spec_for(mock_binary(), None);
    let mut process = cx
        .update(|cx| ClaudeProcess::spawn(spec, cx))
        .expect("spawn mock claude");

    process
        .outgoing
        .unbounded_send(InputMessage::user_text("hello"))
        .expect("send user message");

    let message = recv_with_timeout(&mut process, cx).await;
    match message {
        Some(OutputMessage::System(System::Init { session_id, .. })) => {
            assert_eq!(session_id, "mock-session");
        }
        other => panic!("expected init system message, got {other:?}"),
    }
}
