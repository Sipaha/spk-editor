//! Connection + `AgentServer` implementations for the native claude
//! stream-json backend.
//!
//! `ClaudeNativeAgentServer` implements `agent_servers::AgentServer`; its
//! `connect` hands back a `ClaudeNativeConnection` (an `acp_thread::Agent
//! Connection`). The connection owns one `claude` subprocess per session and a
//! per-session update-pump task that drains the process's `incoming` stream,
//! translates each message into `acp::SessionUpdate`s the `AcpThread` consumes,
//! and resolves the in-flight prompt's oneshot on the turn-ending `result`
//! message (the deterministic turn-end that fixes the Running-hang).

use std::any::Any;
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::Duration;

use acp_thread::{
    AcpThread, AcpThreadEvent, AgentConnection, RequestPermissionOutcome, UserMessageId,
};
use action_log::ActionLog;
use agent_client_protocol::schema as acp;
use agent_servers::{AgentServer, AgentServerDelegate, mcp_servers_for_project};
use anyhow::{Result, anyhow};
use futures::channel::oneshot;
use futures::{FutureExt as _, StreamExt as _, select_biased};
use gpui::{App, AppContext as _, Entity, SharedString, Task, WeakEntity};
use project::{AgentId, Project};
use scheduler::Instant;
use ui::IconName;
use util::ResultExt as _;
use util::path_list::PathList;

use crate::command::{ClaudeCommandSpec, SessionArg, mcp_config_json};
use crate::process::ClaudeProcess;
use crate::protocol::{
    ControlRequestEnvelope, ControlRequestKind, ControlRequestOut, InputMessage, OutputMessage,
    System,
};
use crate::translate::{TurnEnd, classify_result, translate, usage_update};
use crate::watchdog::{AnalyzerContext, ClaudeAnalyzer, Watchdog};

/// Default grace period after a soft `interrupt` before the Stop escalates to
/// a hard kill + `--resume` respawn. Overridable for tests via
/// [`ClaudeNativeConnection::set_escalation_timeout_for_test`].
const DEFAULT_ESCALATION_TIMEOUT: Duration = Duration::from_secs(30);

/// Default quiet period a turn may go without any output before the silence
/// watchdog asks the analyzer whether `claude` is hung. Overridable for tests
/// via [`ClaudeNativeConnection::set_silence_window_for_test`].
const DEFAULT_SILENCE_WINDOW: Duration = Duration::from_secs(15 * 60);

/// `AgentServer` that spawns the `claude` binary directly (no node wrapper).
pub struct ClaudeNativeAgentServer {
    agent_id: AgentId,
    binary: PathBuf,
    extra_env: Vec<(String, String)>,
}

impl ClaudeNativeAgentServer {
    pub fn new(agent_id: AgentId) -> Self {
        Self {
            agent_id,
            binary: PathBuf::from("claude"),
            extra_env: Vec::new(),
        }
    }

    /// Construct a server bound to a specific `claude` binary (an integration
    /// test points this at the mock script) plus extra environment variables.
    pub fn with_binary(
        agent_id: AgentId,
        binary: PathBuf,
        extra_env: Vec<(String, String)>,
    ) -> Self {
        Self {
            agent_id,
            binary,
            extra_env,
        }
    }
}

impl AgentServer for ClaudeNativeAgentServer {
    fn logo(&self) -> IconName {
        IconName::AiClaude
    }

    fn agent_id(&self) -> AgentId {
        self.agent_id.clone()
    }

    fn connect(
        &self,
        _delegate: AgentServerDelegate,
        _project: Entity<Project>,
        _cx: &mut App,
    ) -> Task<Result<Rc<dyn AgentConnection>>> {
        let connection = Rc::new(ClaudeNativeConnection {
            agent_id: self.agent_id.clone(),
            binary: self.binary.clone(),
            extra_env: self.extra_env.clone(),
            sessions: RefCell::new(HashMap::new()),
            escalation_timeout: Cell::new(DEFAULT_ESCALATION_TIMEOUT),
            silence_window: Cell::new(DEFAULT_SILENCE_WINDOW),
            self_handle: RefCell::new(std::rc::Weak::new()),
        });
        *connection.self_handle.borrow_mut() = Rc::downgrade(&connection);
        Task::ready(Ok(connection as Rc<dyn AgentConnection>))
    }

    fn into_any(self: Rc<Self>) -> Rc<dyn Any> {
        self
    }
}

/// State shared between a session's update-pump and its `prompt`/exit handlers.
/// `prompt_tx` carries the in-flight turn's resolver; the pump fulfils it on the
/// turn-ending `result`, the exit-handler fulfils it with an error on process
/// death. `sticky_window` retains the last advertised context window so the
/// token meter never regresses (the 200k/1M flicker fix).
struct SessionShared {
    prompt_tx: RefCell<Option<oneshot::Sender<Result<TurnEnd>>>>,
    sticky_window: Cell<Option<u64>>,
    /// Wall time (executor clock) of the last message the pump pulled off
    /// `incoming`. The silence watchdog reads this to know how long the turn
    /// has been quiet; the pump bumps it on every message (deltas AND control
    /// requests) so any progress resets the silence timer. An `Rc` so the
    /// watchdog timer task shares the very same cell the pump bumps.
    last_output: Rc<Cell<Instant>>,
}

/// Everything needed to respawn a session's `claude` process under the same
/// session id (Stop-escalation kill+resume, and — in Phase 7.2 — the watchdog's
/// `Hung` recovery). Kept separate from the live process so a respawn can build
/// a fresh `ClaudeCommandSpec` without re-deriving it from scratch.
#[derive(Clone)]
struct RespawnBlueprint {
    project: Entity<Project>,
    work_dirs: PathList,
    append_system_prompt: Option<String>,
}

struct SessionState {
    process: ClaudeProcess,
    thread: WeakEntity<AcpThread>,
    shared: Rc<SessionShared>,
    blueprint: RespawnBlueprint,
    /// The update-pump task. Stored so dropping the session cancels it.
    _update_pump: Task<()>,
    /// The Stop-escalation task armed by `cancel`. Held so a clean
    /// `result(cancelled)` (which resolves the prompt oneshot) can drop it and
    /// thereby cancel the pending kill+resume. `None` when no Stop is in flight.
    escalation: Option<Task<()>>,
    /// The silence watchdog for the in-flight turn. Armed when a prompt starts,
    /// dropped (which cancels its timer) when the prompt resolves. `None` while
    /// the session is idle.
    watchdog: Option<Watchdog>,
}

/// Per-process connection to one or more `claude` subprocesses (one per
/// session). Implements `acp_thread::AgentConnection`.
pub struct ClaudeNativeConnection {
    agent_id: AgentId,
    binary: PathBuf,
    extra_env: Vec<(String, String)>,
    sessions: RefCell<HashMap<acp::SessionId, SessionState>>,
    /// Grace period between a soft `interrupt` and the hard kill+resume
    /// escalation. A `Cell` so tests can shrink it to milliseconds.
    escalation_timeout: Cell<Duration>,
    /// Quiet period the silence watchdog waits before analyzing a turn. A `Cell`
    /// so a test can shrink it to milliseconds.
    silence_window: Cell<Duration>,
    /// A handle back to the `Rc` that owns this connection, set once right after
    /// construction. `cancel` (a `&self` method) needs an owned `Rc<Self>` to
    /// arm the escalation task that may outlive the call; upgrading this weak
    /// handle yields it without changing the trait signature.
    self_handle: RefCell<std::rc::Weak<ClaudeNativeConnection>>,
}

impl ClaudeNativeConnection {
    /// Extract the `--append-system-prompt` text from the ACP `_meta` extension
    /// the fork uses: `{ "systemPrompt": { "append": "<text>" } }`. Absent /
    /// malformed meta yields `None` (no flag added).
    fn append_system_prompt_from_meta(extra_meta: &Option<acp::Meta>) -> Option<String> {
        extra_meta
            .as_ref()?
            .get("systemPrompt")?
            .get("append")?
            .as_str()
            .map(|text| text.to_string())
    }

    /// Shrink the Stop-escalation grace period so an integration test can drive
    /// the kill+resume path without waiting the real 30 seconds.
    pub fn set_escalation_timeout_for_test(&self, timeout: Duration) {
        self.escalation_timeout.set(timeout);
    }

    /// Shrink the silence watchdog window so a test drives the analyzer path
    /// without waiting the real 15 minutes.
    pub fn set_silence_window_for_test(&self, window: Duration) {
        self.silence_window.set(window);
    }

    /// The OS process id backing a session, or `None` if the session is gone.
    /// A changed value across a cancel proves the process was killed+respawned.
    pub fn session_process_id_for_test(&self, session_id: &acp::SessionId) -> Option<u32> {
        self.sessions
            .borrow()
            .get(session_id)
            .map(|session| session.process.process_id())
    }

    /// Spawn a `claude` subprocess for `session`, await its `init` message to
    /// learn the real session id, build the `AcpThread`, and start the
    /// per-session update-pump. Shared by `new_session`/`resume_session`.
    fn open_session(
        self: Rc<Self>,
        session: SessionArg,
        project: Entity<Project>,
        work_dirs: PathList,
        title: Option<SharedString>,
        extra_meta: Option<acp::Meta>,
        cx: &mut App,
    ) -> Task<Result<Entity<AcpThread>>> {
        let Some(work_dir) = work_dirs.ordered_paths().next().cloned() else {
            return Task::ready(Err(anyhow!("Working directory cannot be empty")));
        };
        let mcp_servers = mcp_servers_for_project(&project, cx);
        let append_system_prompt = Self::append_system_prompt_from_meta(&extra_meta);

        let blueprint = RespawnBlueprint {
            project: project.clone(),
            work_dirs: work_dirs.clone(),
            append_system_prompt: append_system_prompt.clone(),
        };

        let spec = ClaudeCommandSpec {
            binary: self.binary.clone(),
            work_dir,
            session,
            mcp_servers_json: mcp_config_json(&mcp_servers),
            append_system_prompt,
            extra_env: self.extra_env.clone(),
        };

        let mut process = match ClaudeProcess::spawn(spec, cx) {
            Ok(process) => process,
            Err(error) => return Task::ready(Err(error)),
        };

        cx.spawn(async move |cx| {
            // `claude` emits an `init` system message with the canonical session
            // id before any turn output. We must adopt that id (a resumed
            // session reports its existing id; a new one echoes the uuid we
            // requested, but going through `init` keeps both paths identical).
            let session_id = await_init(&mut process).await?;

            let shared = Rc::new(SessionShared {
                prompt_tx: RefCell::new(None),
                sticky_window: Cell::new(None),
                last_output: Rc::new(Cell::new(cx.background_executor().now())),
            });

            let thread: Entity<AcpThread> = cx.update(|cx| {
                let action_log = cx.new(|_| ActionLog::new(project.clone()));
                cx.new(|cx| {
                    AcpThread::new(
                        None,
                        title,
                        Some(work_dirs),
                        self.clone(),
                        project,
                        action_log,
                        session_id.clone(),
                        watch::Receiver::constant(acp::PromptCapabilities::new().image(true)),
                        cx,
                    )
                })
            });

            let incoming = process.take_incoming();
            let exited = process.wait_status();
            let outgoing = process.outgoing.clone();
            let update_pump = cx.spawn({
                let thread = thread.downgrade();
                let shared = shared.clone();
                async move |cx| {
                    run_update_pump(incoming, exited, outgoing, thread, shared, cx).await;
                }
            });

            self.sessions.borrow_mut().insert(
                session_id,
                SessionState {
                    process,
                    thread: thread.downgrade(),
                    shared,
                    blueprint,
                    _update_pump: update_pump,
                    escalation: None,
                    watchdog: None,
                },
            );

            Ok(thread)
        })
    }

    /// Arm a silence watchdog for the just-started turn on `session_id`. On a
    /// `Hung` verdict it routes through the same `recover_session` recovery as
    /// the Stop-escalation; on `Working`/`Unknown`/analyzer-failure it re-arms.
    /// Stores the watchdog on the session so the prompt's resolution can drop it.
    fn arm_watchdog(self: &Rc<Self>, session_id: &acp::SessionId, cx: &mut App) {
        let mut sessions = self.sessions.borrow_mut();
        let Some(session) = sessions.get_mut(session_id) else {
            return;
        };

        let last_output = session.shared.last_output.clone();
        let process_id = session.process.process_id();
        let window = self.silence_window.get();
        let analyzer: Rc<dyn crate::watchdog::Analyzer> =
            Rc::new(ClaudeAnalyzer::new(self.binary.clone()));

        // The watchdog asks for fresh context at fire time, not arm time. The
        // thread's full event history isn't cheaply readable from a plain `Fn`
        // (it needs a `cx` to `read`); the Foundation analyzer prompt works from
        // silence-duration + pid alone. SP2 can enrich `recent_events`.
        let context_provider: Rc<dyn Fn() -> AnalyzerContext> = Rc::new(move || AnalyzerContext {
            silence_duration: window,
            process_id: Some(process_id),
            recent_events: Vec::new(),
            pending_tool_use: None,
        });

        let connection = Rc::downgrade(self);
        let session_id_for_recovery = session_id.clone();
        let recovery: crate::watchdog::RecoveryCallback = Rc::new(move |cx: &mut gpui::AsyncApp| {
            if let Some(connection) = connection.upgrade() {
                connection.recover_session(session_id_for_recovery.clone(), cx);
            }
        });

        let mut async_cx = cx.to_async();
        let watchdog = Watchdog::arm(
            last_output,
            window,
            analyzer,
            context_provider,
            recovery,
            &mut async_cx,
        );
        session.watchdog = Some(watchdog);
    }

    /// Recovery primitive shared by Stop-escalation and (Phase 7.2) the
    /// watchdog's `Hung` verdict: SIGKILL the wedged `claude`, respawn it under
    /// the same session id with `--resume`, rewire a fresh update-pump onto the
    /// *existing* `AcpThread`, and force-resolve the in-flight prompt oneshot
    /// `Ok(TurnEnd::Stop(Cancelled))` so `store.rs`'s Cancelled queue logic runs.
    ///
    /// Spawned (not awaited) by the caller — it must not hold the `sessions`
    /// `RefCell` borrow across its `.await`s, so it re-borrows for the swap.
    fn recover_session(self: Rc<Self>, session_id: acp::SessionId, cx: &mut gpui::AsyncApp) {
        cx.spawn(async move |cx| {
            // Take only what we need out of the borrow, then drop it before any
            // await — `await_init` and `cx.update` below mustn't run while the
            // `sessions` map is borrowed (re-entrancy + borrow-across-await).
            let Some((blueprint, thread, prompt_tx)) = ({
                let mut sessions = self.sessions.borrow_mut();
                sessions.get_mut(&session_id).map(|session| {
                    session.escalation = None;
                    (
                        session.blueprint.clone(),
                        session.thread.clone(),
                        session.shared.prompt_tx.borrow_mut().take(),
                    )
                })
            }) else {
                return;
            };

            // Resolve the wedged prompt first so the UI leaves Running even if
            // the respawn below fails for any reason.
            if let Some(prompt_tx) = prompt_tx {
                prompt_tx
                    .send(Ok(TurnEnd::Stop(acp::StopReason::Cancelled)))
                    .ok();
            }

            // Kill the old process. Done via a short-lived borrow so the kill
            // call doesn't straddle an await.
            {
                let mut sessions = self.sessions.borrow_mut();
                if let Some(session) = sessions.get_mut(&session_id) {
                    session.process.kill().log_err();
                }
            }

            let Some(work_dir) = blueprint.work_dirs.ordered_paths().next().cloned() else {
                return;
            };
            let spec = cx.update(|cx| ClaudeCommandSpec {
                binary: self.binary.clone(),
                work_dir,
                session: SessionArg::Resume(session_id.0.to_string()),
                mcp_servers_json: mcp_config_json(&mcp_servers_for_project(
                    &blueprint.project,
                    cx,
                )),
                append_system_prompt: blueprint.append_system_prompt.clone(),
                extra_env: self.extra_env.clone(),
            });

            let mut process = match cx.update(|cx| ClaudeProcess::spawn(spec, cx)) {
                Ok(process) => process,
                Err(error) => {
                    log::error!("claude_native: respawn on Stop escalation failed: {error}");
                    return;
                }
            };

            if await_init(&mut process).await.log_err().is_none() {
                return;
            }

            let shared = Rc::new(SessionShared {
                prompt_tx: RefCell::new(None),
                sticky_window: Cell::new(None),
                last_output: Rc::new(Cell::new(cx.background_executor().now())),
            });
            let incoming = process.take_incoming();
            let exited = process.wait_status();
            let outgoing = process.outgoing.clone();
            let update_pump = cx.spawn({
                let thread = thread.clone();
                let shared = shared.clone();
                async move |cx| {
                    run_update_pump(incoming, exited, outgoing, thread, shared, cx).await;
                }
            });

            let mut sessions = self.sessions.borrow_mut();
            if let Some(session) = sessions.get_mut(&session_id) {
                session.process = process;
                session.shared = shared;
                session._update_pump = update_pump;
                // The recovered turn is force-resolved Cancelled; drop the
                // watchdog so its timer (which referenced the old `last_output`)
                // stops. A fresh prompt arms a new one.
                session.watchdog = None;
            }
            // If the session vanished while we were respawning (closed), the new
            // `process`/`update_pump` drop here and tear themselves down.
        })
        .detach();
    }
}

/// Await the `claude` `init` system message that announces the canonical
/// session id. Shared by initial spawn and the recovery respawn.
async fn await_init(process: &mut ClaudeProcess) -> Result<acp::SessionId> {
    loop {
        match process.incoming.next().await {
            Some(OutputMessage::System(System::Init { session_id, .. })) => {
                return Ok(acp::SessionId::new(session_id));
            }
            Some(_) => continue,
            None => return Err(anyhow!("claude exited before init message")),
        }
    }
}

/// Drain the process's `incoming` stream until EOF or process exit, applying
/// each message to the `AcpThread` and resolving the in-flight prompt oneshot on
/// the turn-ending `result`. On process exit with a prompt still pending, the
/// prompt is resolved with an error so the thread transitions to `Errored`
/// rather than hanging.
async fn run_update_pump(
    mut incoming: futures::channel::mpsc::UnboundedReceiver<OutputMessage>,
    exited: impl std::future::Future<Output = Option<std::process::ExitStatus>>,
    outgoing: futures::channel::mpsc::UnboundedSender<InputMessage>,
    thread: WeakEntity<AcpThread>,
    shared: Rc<SessionShared>,
    cx: &mut gpui::AsyncApp,
) {
    // A `can_use_tool` authorization can take arbitrarily long (it waits on the
    // user). The await is spawned off the pump so the loop keeps draining
    // `incoming`; the tasks are retained here for the pump's lifetime (= the
    // session's) so they aren't cancelled before the user responds.
    let mut authorization_tasks: Vec<Task<()>> = Vec::new();
    let mut exited = std::pin::pin!(exited.fuse());
    loop {
        let message = select_biased! {
            message = incoming.next().fuse() => message,
            status = exited.as_mut() => {
                // Process died. If a turn was in flight, fail it so the thread
                // surfaces an error instead of hanging forever.
                if let Some(sender) = shared.prompt_tx.borrow_mut().take() {
                    let detail = match status {
                        Some(status) => format!("claude exited: {status}"),
                        None => "claude exited".to_string(),
                    };
                    sender.send(Err(anyhow!(detail))).ok();
                }
                return;
            }
        };

        let Some(message) = message else {
            // stdout EOF. Fail any in-flight turn for the same reason as exit.
            if let Some(sender) = shared.prompt_tx.borrow_mut().take() {
                sender.send(Err(anyhow!("claude output stream closed"))).ok();
            }
            return;
        };

        // Any output (partial delta or control request) is progress — reset the
        // silence watchdog's baseline before dispatching the message.
        shared.last_output.set(cx.background_executor().now());

        if let OutputMessage::Result(result) = &message {
            let update = usage_update(result, shared.sticky_window.get());
            if let Some(window) = result.context_window_for_active_model() {
                shared.sticky_window.set(Some(window));
            }
            if let Some(update) = update {
                thread
                    .update(cx, |thread, cx| {
                        thread.handle_session_update(update, cx).log_err();
                    })
                    .ok();
            }

            let turn_end = classify_result(result);
            if let Some(sender) = shared.prompt_tx.borrow_mut().take() {
                sender.send(Ok(turn_end)).ok();
            }
            continue;
        }

        if let OutputMessage::ControlRequest(envelope) = message {
            if let Some(task) =
                spawn_tool_authorization(envelope, outgoing.clone(), thread.clone(), cx)
            {
                authorization_tasks.push(task);
            }
            continue;
        }

        for update in translate(&message) {
            thread
                .update(cx, |thread, cx| {
                    thread.handle_session_update(update, cx).log_err();
                })
                .ok();
        }
    }
}

/// Bridge a `can_use_tool` control request to the `AcpThread`'s authorization
/// flow. Surfaces a pending tool-call confirmation on the thread, then (in a
/// spawned task, since the user may take arbitrarily long) writes the matching
/// `control_response` back to `claude`'s stdin. Returns the task so the caller
/// can retain it; returns `None` for non-`can_use_tool` control requests (the
/// Foundation handles no others) or when the thread is already gone.
fn spawn_tool_authorization(
    envelope: ControlRequestEnvelope,
    outgoing: futures::channel::mpsc::UnboundedSender<InputMessage>,
    thread: WeakEntity<AcpThread>,
    cx: &mut gpui::AsyncApp,
) -> Option<Task<()>> {
    let ControlRequestKind::CanUseTool {
        tool_name,
        tool_use_id,
        input,
        ..
    } = envelope.request
    else {
        return None;
    };

    // `claude` already streams an `assistant` `tool_use` block (translated to a
    // ToolCall) before this request, so the id will usually exist; passing the
    // fields again is a harmless upsert that also covers the case where the
    // permission request races ahead of the tool_use block.
    let fields = acp::ToolCallUpdateFields::new()
        .title(tool_name)
        .raw_input(input);
    let tool_call_update = acp::ToolCallUpdate::new(acp::ToolCallId::new(tool_use_id), fields);

    // claude's control protocol is binary (allow / deny); the thread's flat
    // allow-once / reject-once pair maps onto that. `option_kind` on the outcome
    // tells us which the user picked.
    let options = acp_thread::PermissionOptions::Flat(vec![
        acp::PermissionOption::new(
            acp::PermissionOptionId::new("allow"),
            "Allow",
            acp::PermissionOptionKind::AllowOnce,
        ),
        acp::PermissionOption::new(
            acp::PermissionOptionId::new("reject"),
            "Reject",
            acp::PermissionOptionKind::RejectOnce,
        ),
    ]);

    let authorization = thread
        .update(cx, |thread, cx| {
            thread.request_tool_call_authorization(tool_call_update, options, cx)
        })
        .ok()?
        .log_err()?;

    let request_id = envelope.request_id;
    Some(cx.spawn(async move |_cx| {
        let allow = match authorization.await {
            RequestPermissionOutcome::Selected(outcome) => matches!(
                outcome.option_kind,
                acp::PermissionOptionKind::AllowOnce | acp::PermissionOptionKind::AllowAlways
            ),
            RequestPermissionOutcome::Cancelled => false,
        };
        outgoing
            .unbounded_send(InputMessage::permission_response(request_id, allow))
            .log_err();
    }))
}

impl AgentConnection for ClaudeNativeConnection {
    fn agent_id(&self) -> AgentId {
        self.agent_id.clone()
    }

    fn telemetry_id(&self) -> SharedString {
        SharedString::new_static("claude-native")
    }

    fn new_session(
        self: Rc<Self>,
        project: Entity<Project>,
        work_dirs: PathList,
        cx: &mut App,
    ) -> Task<Result<Entity<AcpThread>>> {
        self.new_session_with_meta(project, work_dirs, None, cx)
    }

    fn new_session_with_meta(
        self: Rc<Self>,
        project: Entity<Project>,
        work_dirs: PathList,
        extra_meta: Option<acp::Meta>,
        cx: &mut App,
    ) -> Task<Result<Entity<AcpThread>>> {
        let session = SessionArg::New(uuid::Uuid::new_v4().to_string());
        self.open_session(session, project, work_dirs, None, extra_meta, cx)
    }

    fn supports_resume_session(&self) -> bool {
        true
    }

    fn resume_session(
        self: Rc<Self>,
        session_id: acp::SessionId,
        project: Entity<Project>,
        work_dirs: PathList,
        title: Option<SharedString>,
        cx: &mut App,
    ) -> Task<Result<Entity<AcpThread>>> {
        let session = SessionArg::Resume(session_id.0.to_string());
        self.open_session(session, project, work_dirs, title, None, cx)
    }

    fn supports_close_session(&self) -> bool {
        true
    }

    fn auth_methods(&self) -> &[acp::AuthMethod] {
        &[]
    }

    fn authenticate(&self, _method: acp::AuthMethodId, _cx: &mut App) -> Task<Result<()>> {
        Task::ready(Ok(()))
    }

    fn prompt(
        &self,
        _user_message_id: UserMessageId,
        params: acp::PromptRequest,
        cx: &mut App,
    ) -> Task<Result<acp::PromptResponse>> {
        let thread;
        let receiver;
        {
            let sessions = self.sessions.borrow();
            let Some(session) = sessions.get(&params.session_id) else {
                return Task::ready(Err(anyhow!(
                    "no native claude session for {}",
                    params.session_id.0
                )));
            };

            let (sender, prompt_receiver) = oneshot::channel();
            *session.shared.prompt_tx.borrow_mut() = Some(sender);

            if let Err(error) = session
                .process
                .outgoing
                .unbounded_send(InputMessage::user_blocks(&params.prompt))
            {
                session.shared.prompt_tx.borrow_mut().take();
                return Task::ready(Err(anyhow!("claude stdin closed: {error}")));
            }

            thread = session.thread.clone();
            receiver = prompt_receiver;
        }

        // Arm the silence watchdog for this turn (re-borrow mutably now that the
        // immutable borrow above is dropped). Disarmed below once the prompt
        // resolves, by whichever path resolves it. `arm_watchdog` needs an owned
        // `Rc<Self>` for the recovery callback; the `self_handle` weak yields it.
        let connection = self.self_handle.borrow().clone();
        if let Some(connection) = connection.upgrade() {
            connection.arm_watchdog(&params.session_id, cx);
        }

        let session_id = params.session_id.clone();
        let connection = self.self_handle.borrow().clone();
        cx.spawn(async move |cx| {
            let outcome = match receiver.await {
                Ok(Ok(TurnEnd::Stop(stop_reason))) => Ok(acp::PromptResponse::new(stop_reason)),
                Ok(Ok(TurnEnd::Error(detail))) => {
                    thread
                        .update(cx, |_thread, cx| cx.emit(AcpThreadEvent::Error))
                        .ok();
                    Err(anyhow!(detail))
                }
                Ok(Err(error)) => Err(error),
                // Sender dropped without sending (session torn down): treat as a
                // cancellation rather than a hard error.
                Err(_) => Ok(acp::PromptResponse::new(acp::StopReason::Cancelled)),
            };

            // Turn ended (any path) — drop the watchdog so its silence timer
            // stops until the next prompt re-arms it.
            if let Some(connection) = connection.upgrade()
                && let Some(session) = connection.sessions.borrow_mut().get_mut(&session_id)
            {
                session.watchdog = None;
            }

            outcome
        })
    }

    fn cancel(&self, session_id: &acp::SessionId, cx: &mut App) {
        // Stage 1: a soft `interrupt` control request. A well-behaved `claude`
        // ends the turn with `result(cancelled)`, which the update-pump resolves
        // through the prompt oneshot (the normal path) — no escalation needed.
        {
            let sessions = self.sessions.borrow();
            let Some(session) = sessions.get(session_id) else {
                return;
            };
            // No turn in flight → nothing to cancel.
            if session.shared.prompt_tx.borrow().is_none() {
                return;
            }
            // The interrupt's control_response (the returned receiver) is
            // irrelevant to escalation timing — we escalate on the prompt
            // staying pending, not on the ack — so it is dropped here.
            match session.process.send_control(ControlRequestOut::Interrupt) {
                Ok(_receiver) => {}
                Err(error) => log::warn!("claude_native: interrupt write failed: {error}"),
            }
        }

        // Stage 2: arm the escalation. After the grace period, if the prompt
        // oneshot is still pending (claude ignored the interrupt), kill + resume.
        // Capture a *weak* handle so the stored task (owned by the session, owned
        // by this `Rc`) doesn't form a strong cycle that pins the connection.
        let connection = self.self_handle.borrow().clone();
        let timeout = self.escalation_timeout.get();
        let session_id_for_task = session_id.clone();
        let escalation = cx.spawn(async move |cx| {
            cx.background_executor().timer(timeout).await;
            let Some(connection) = connection.upgrade() else {
                return;
            };
            // Re-check under the borrow: a clean `result(cancelled)` resolves
            // (takes) the prompt oneshot via the pump, so a still-present sender
            // is the signal that the interrupt was ignored and we must escalate.
            let still_pending = connection
                .sessions
                .borrow()
                .get(&session_id_for_task)
                .map(|session| session.shared.prompt_tx.borrow().is_some())
                .unwrap_or(false);
            if still_pending {
                connection.recover_session(session_id_for_task, cx);
            }
        });
        if let Some(session) = self.sessions.borrow_mut().get_mut(session_id) {
            session.escalation = Some(escalation);
        }
    }

    fn close_session(
        self: Rc<Self>,
        session_id: &acp::SessionId,
        _cx: &mut App,
    ) -> Task<Result<()>> {
        if let Some(mut session) = self.sessions.borrow_mut().remove(session_id) {
            session.process.kill().log_err();
        }
        Task::ready(Ok(()))
    }

    fn into_any(self: Rc<Self>) -> Rc<dyn Any> {
        self
    }
}
