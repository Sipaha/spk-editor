//! Integration tests driving a fake `claude` binary (a bash script) through
//! [`claude_native::process::ClaudeProcess`]. These exercise the real spawn +
//! stdio async tasks; the protocol/translate units are tested in-crate.

use std::path::PathBuf;
use std::time::Duration;

use acp_thread::{AcpThread, AgentConnection as _};
use agent_client_protocol::schema as acp;
use agent_servers::{AgentServer, AgentServerDelegate};
use claude_native::command::{ClaudeCommandSpec, SessionArg};
use claude_native::process::ClaudeProcess;
use claude_native::protocol::{InputMessage, OutputMessage, System};
use claude_native::{ClaudeNativeAgentServer, ClaudeNativeConnection};
use futures::{FutureExt as _, StreamExt as _};
use gpui::{Entity, TestAppContext};
use project::{AgentId, FakeFs, Project};
use std::rc::Rc;
use util::path_list::PathList;

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

/// Drain `incoming` until a message matching `predicate` arrives, failing on a
/// timeout so a missing message is a test failure rather than a hang.
async fn recv_until(
    process: &mut ClaudeProcess,
    cx: &mut TestAppContext,
    mut predicate: impl FnMut(&OutputMessage) -> bool,
) -> OutputMessage {
    loop {
        match recv_with_timeout(process, cx).await {
            Some(message) if predicate(&message) => return message,
            Some(_) => continue,
            None => panic!("incoming closed before matching message arrived"),
        }
    }
}

#[gpui::test]
async fn delivers_control_request_and_writes_response(cx: &mut TestAppContext) {
    cx.executor().allow_parking();

    let capture = std::env::temp_dir().join(format!(
        "claude_native_capture_{}.ndjson",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&capture);

    let mut spec = spec_for(mock_binary(), Some(capture.clone()));
    spec.extra_env
        .push(("MOCK_CLAUDE_CONTROL".to_string(), "1".to_string()));

    let mut process = cx
        .update(|cx| ClaudeProcess::spawn(spec, cx))
        .expect("spawn mock claude");

    process
        .outgoing
        .unbounded_send(InputMessage::user_text("hello"))
        .expect("send user message");

    let request = recv_until(&mut process, cx, |message| {
        matches!(message, OutputMessage::ControlRequest(_))
    })
    .await;
    let request_id = match request {
        OutputMessage::ControlRequest(envelope) => envelope.request_id,
        other => panic!("expected control request, got {other:?}"),
    };

    process
        .send_control_response(&request_id, true)
        .expect("write control response");

    // The mock only emits `result` after it reads our control_response, so
    // waiting for `result` proves the response reached its stdin.
    recv_until(&mut process, cx, |message| {
        matches!(message, OutputMessage::Result(_))
    })
    .await;

    let captured = std::fs::read_to_string(&capture).expect("read capture");
    assert!(
        captured.contains(r#""type":"control_response""#)
            && captured.contains(r#""behavior":"allow""#),
        "captured stdin missing control_response: {captured}"
    );
    let _ = std::fs::remove_file(&capture);
}

#[gpui::test]
async fn closes_incoming_and_resolves_wait_on_exit(cx: &mut TestAppContext) {
    cx.executor().allow_parking();

    let spec = spec_for(mock_binary(), None);
    let mut process = cx
        .update(|cx| ClaudeProcess::spawn(spec, cx))
        .expect("spawn mock claude");

    let exited = process.wait_status();

    // Closing the outgoing sender drops stdin; the mock loop hits EOF and exits.
    drop(process.outgoing.clone());
    process
        .outgoing
        .close_channel();

    // Reader must observe EOF and close `incoming`.
    loop {
        match recv_with_timeout(&mut process, cx).await {
            Some(_) => continue,
            None => break,
        }
    }

    let status = {
        let timeout = cx.background_executor.timer(Duration::from_secs(10)).fuse();
        let exited = exited.fuse();
        futures::pin_mut!(timeout, exited);
        futures::select! {
            status = exited => status,
            _ = timeout => panic!("timed out waiting for process exit"),
        }
    };
    assert!(status.is_some(), "wait_status resolved without an exit status");
}

async fn init_test(cx: &mut TestAppContext) -> Entity<Project> {
    cx.update(|cx| {
        let settings_store = settings::SettingsStore::test(cx);
        cx.set_global(settings_store);
    });
    cx.executor().allow_parking();
    let fs = FakeFs::new(cx.executor());
    Project::test(fs, [], cx).await
}

/// Build a native connection wired to the mock `claude` script, plus the extra
/// env a scenario needs (capture path, control/no-result toggles).
async fn connect_mock(
    project: &Entity<Project>,
    extra_env: Vec<(String, String)>,
    cx: &mut TestAppContext,
) -> Rc<ClaudeNativeConnection> {
    let server =
        ClaudeNativeAgentServer::with_binary(AgentId::new("claude-acp"), mock_binary(), extra_env);
    let store = project.read_with(cx, |project, _| project.agent_server_store().clone());
    let delegate = AgentServerDelegate::new(store, None);
    let connection = cx
        .update(|cx| AgentServer::connect(&server, delegate, project.clone(), cx))
        .await
        .expect("connect native backend");
    connection
        .into_any()
        .downcast::<ClaudeNativeConnection>()
        .expect("native connection type")
}

async fn await_thread(
    task: gpui::Task<anyhow::Result<Entity<AcpThread>>>,
    cx: &mut TestAppContext,
) -> Entity<AcpThread> {
    let timeout = cx.background_executor.timer(Duration::from_secs(10)).fuse();
    let task = task.fuse();
    futures::pin_mut!(timeout, task);
    futures::select! {
        thread = task => thread.expect("session created"),
        _ = timeout => panic!("timed out creating session"),
    }
}

#[gpui::test]
async fn new_session_captures_init_session_id(cx: &mut TestAppContext) {
    let project = init_test(cx).await;
    let connection = connect_mock(&project, Vec::new(), cx).await;

    let task = cx.update(|cx| {
        Rc::clone(&connection).new_session(
            project.clone(),
            PathList::new(&[std::env::temp_dir().as_path()]),
            cx,
        )
    });
    let thread = await_thread(task, cx).await;

    // The mock emits `init` with session_id "mock-session"; the thread the
    // connection returns must adopt that id (not the random uuid we spawned with).
    let session_id = thread.read_with(cx, |thread, _| thread.session_id().clone());
    assert_eq!(session_id.0.as_ref(), "mock-session");
}

#[gpui::test]
#[ignore = "prompt lands in Task 5.2"]
async fn prompt_resolves_on_result_and_streams_text(cx: &mut TestAppContext) {
    let project = init_test(cx).await;
    let connection = connect_mock(&project, Vec::new(), cx).await;

    let task = cx.update(|cx| {
        Rc::clone(&connection).new_session(
            project.clone(),
            PathList::new(&[std::env::temp_dir().as_path()]),
            cx,
        )
    });
    let thread = await_thread(task, cx).await;
    let session_id = thread.read_with(cx, |thread, _| thread.session_id().clone());

    let prompt = vec![acp::ContentBlock::Text(acp::TextContent::new(
        "hello".to_string(),
    ))];
    let request = acp::PromptRequest::new(session_id, prompt);
    let prompt_task = cx.update(|cx| {
        connection.prompt(acp_thread::UserMessageId::new(), request, cx)
    });

    let response = {
        let timeout = cx.background_executor.timer(Duration::from_secs(10)).fuse();
        let prompt_task = prompt_task.fuse();
        futures::pin_mut!(timeout, prompt_task);
        futures::select! {
            response = prompt_task => response.expect("prompt resolved Ok"),
            _ = timeout => panic!("prompt did not resolve on result message"),
        }
    };
    assert_eq!(response.stop_reason, acp::StopReason::EndTurn);

    let markdown = thread.read_with(cx, |thread, cx| thread.to_markdown(cx));
    assert!(
        markdown.contains("Hi"),
        "streamed assistant text missing from thread: {markdown}"
    );
}

#[gpui::test]
#[ignore = "prompt lands in Task 5.2"]
async fn prompt_stays_pending_without_result(cx: &mut TestAppContext) {
    let project = init_test(cx).await;
    let connection = connect_mock(
        &project,
        vec![("MOCK_CLAUDE_NO_RESULT".to_string(), "1".to_string())],
        cx,
    )
    .await;

    let task = cx.update(|cx| {
        Rc::clone(&connection).new_session(
            project.clone(),
            PathList::new(&[std::env::temp_dir().as_path()]),
            cx,
        )
    });
    let thread = await_thread(task, cx).await;
    let session_id = thread.read_with(cx, |thread, _| thread.session_id().clone());

    let prompt = vec![acp::ContentBlock::Text(acp::TextContent::new(
        "hello".to_string(),
    ))];
    let request = acp::PromptRequest::new(session_id, prompt);
    let prompt_task = cx.update(|cx| {
        connection.prompt(acp_thread::UserMessageId::new(), request, cx)
    });

    // The mock streams text but never sends `result`; the prompt must remain
    // pending. Race it against a short timer and assert the timer wins.
    let timeout = cx.background_executor.timer(Duration::from_secs(2)).fuse();
    let prompt_task = prompt_task.fuse();
    futures::pin_mut!(timeout, prompt_task);
    futures::select! {
        _ = prompt_task => panic!("prompt resolved despite no result message (hang scenario)"),
        _ = timeout => {}
    }
}
