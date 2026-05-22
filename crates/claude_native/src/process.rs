//! Subprocess lifecycle for the `claude` binary: spawn + the reader/writer/
//! stderr async tasks over its stdio. This is the only module that touches the
//! OS; everything above it works against the [`InputMessage`]/[`OutputMessage`]
//! channels this type exposes.

use std::collections::HashMap;
use std::process::{ExitStatus, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use anyhow::{Context as _, Result};
use futures::channel::mpsc::{self, UnboundedReceiver, UnboundedSender};
use futures::channel::oneshot;
use futures::future::Shared;
use futures::io::{AsyncBufReadExt as _, AsyncWriteExt as _, BufReader};
use futures::{FutureExt as _, StreamExt as _};
use gpui::{App, Task};
use util::process::Child;

use crate::command::ClaudeCommandSpec;
use crate::protocol::{ControlRequestOut, InputMessage, OutputMessage};

/// Pending `send_control` calls awaiting their matching `control_response`,
/// keyed by the `request_id` we allocated. The reader fulfils and removes the
/// entry when the response arrives; a dropped sender (process gone) resolves
/// the awaiting receiver with `Cancelled`.
type PendingControls = Arc<Mutex<HashMap<String, oneshot::Sender<serde_json::Value>>>>;

/// One running `claude` process. Holds the child plus the three stdio tasks;
/// dropping it cancels the tasks and (via `Child`'s process-group kill on an
/// explicit `kill`) tears down the subprocess. Messages flow over the
/// `outgoing` sender (stdin) and `incoming` receiver (stdout).
pub struct ClaudeProcess {
    child: Child,
    pub outgoing: UnboundedSender<InputMessage>,
    pub incoming: UnboundedReceiver<OutputMessage>,
    pending_controls: PendingControls,
    next_request_id: AtomicU64,
    exited: Shared<Task<Option<ExitStatus>>>,
    _reader: Task<()>,
    _writer: Task<()>,
    _stderr: Task<()>,
}

impl ClaudeProcess {
    pub fn spawn(spec: ClaudeCommandSpec, cx: &App) -> Result<Self> {
        let mut child = Child::spawn(
            spec.to_std_command(),
            Stdio::piped(),
            Stdio::piped(),
            Stdio::piped(),
        )?;

        let stdout = child.stdout.take().context("claude stdout missing")?;
        let stdin = child.stdin.take().context("claude stdin missing")?;
        let stderr = child.stderr.take().context("claude stderr missing")?;

        let (incoming_sender, incoming) = mpsc::unbounded::<OutputMessage>();
        let (outgoing, outgoing_receiver) = mpsc::unbounded::<InputMessage>();
        let pending_controls: PendingControls = Arc::new(Mutex::new(HashMap::new()));

        let executor = cx.background_executor();
        let reader = executor.spawn(read_stdout(
            stdout,
            incoming_sender,
            pending_controls.clone(),
        ));
        let writer = executor.spawn(write_stdin(stdin, outgoing_receiver));
        let stderr_task = executor.spawn(drain_stderr(stderr));

        // `Child::status` clones an internal handle, so the resulting future is
        // independent of the `Child` we keep around (for `kill`). Driving it in
        // a shared task lets `wait_status()` hand out cheap clones.
        let status_future = child.status();
        let exited = executor
            .spawn(async move { status_future.await.ok() })
            .shared();

        Ok(Self {
            child,
            outgoing,
            incoming,
            pending_controls,
            next_request_id: AtomicU64::new(0),
            exited,
            _reader: reader,
            _writer: writer,
            _stderr: stderr_task,
        })
    }

    /// Take ownership of the `incoming` output stream, leaving a closed stream
    /// in its place. The connection's per-session update-pump owns the receiver
    /// (it drains it to translate output into thread updates), while the rest of
    /// `ClaudeProcess` stays in `SessionState` for stdin writes / control / kill.
    pub fn take_incoming(&mut self) -> UnboundedReceiver<OutputMessage> {
        let (sender, closed) = mpsc::unbounded::<OutputMessage>();
        drop(sender);
        std::mem::replace(&mut self.incoming, closed)
    }

    /// Resolves with the child's exit status once it terminates (or `None` if
    /// the status could not be collected). Cheap to call repeatedly — each call
    /// returns a clone of the same shared future.
    pub fn wait_status(&self) -> impl std::future::Future<Output = Option<ExitStatus>> + 'static {
        self.exited.clone()
    }

    /// Send a control request to `claude`, returning a receiver that resolves
    /// with the matching `control_response` payload. The request_id is
    /// allocated and registered here so the reader can route the response back.
    pub fn send_control(
        &self,
        request: ControlRequestOut,
    ) -> Result<oneshot::Receiver<serde_json::Value>> {
        let request_id = format!(
            "claude-native-{}",
            self.next_request_id.fetch_add(1, Ordering::Relaxed)
        );
        let (sender, receiver) = oneshot::channel();
        self.pending_controls
            .lock()
            .unwrap_or_else(|guard| guard.into_inner())
            .insert(request_id.clone(), sender);
        let message = InputMessage::ControlRequest { request_id, request };
        self.outgoing
            .unbounded_send(message)
            .context("claude process stdin closed")?;
        Ok(receiver)
    }

    /// Reply to a `can_use_tool` control request from `claude` with an
    /// allow/deny decision. `request_id` is the id `claude` sent.
    pub fn send_control_response(&self, request_id: &str, allow: bool) -> Result<()> {
        self.outgoing
            .unbounded_send(InputMessage::permission_response(request_id, allow))
            .context("claude process stdin closed")
    }

    /// SIGKILL the process group. Used by Stop escalation / close_session in
    /// later phases; kept here next to spawn so the OS surface stays in one
    /// place.
    pub fn kill(&mut self) -> Result<()> {
        self.child.kill()
    }
}

async fn read_stdout(
    stdout: impl futures::AsyncRead + Unpin,
    incoming_sender: UnboundedSender<OutputMessage>,
    pending_controls: PendingControls,
) {
    let mut lines = BufReader::new(stdout).lines();
    while let Some(line) = lines.next().await {
        let line = match line {
            Ok(line) => line,
            Err(error) => {
                log::warn!("claude stdout read error: {error}");
                break;
            }
        };
        if line.is_empty() {
            continue;
        }
        match OutputMessage::parse(&line) {
            // Control responses fulfil a pending `send_control`; route them to
            // the matching oneshot rather than the general output stream.
            Ok(OutputMessage::ControlResponse(envelope)) => {
                let sender = pending_controls
                    .lock()
                    .unwrap_or_else(|guard| guard.into_inner())
                    .remove(&envelope.request_id);
                match sender {
                    Some(sender) => {
                        if sender.send(envelope.response).is_err() {
                            log::debug!(
                                "control response for {} dropped: caller gone",
                                envelope.request_id
                            );
                        }
                    }
                    None => log::warn!(
                        "control response for unknown request_id {}",
                        envelope.request_id
                    ),
                }
            }
            Ok(message) => {
                if incoming_sender.unbounded_send(message).is_err() {
                    // Receiver dropped — nobody is listening anymore.
                    break;
                }
            }
            Err(error) => {
                log::warn!("claude stdout parse error: {error}; line: {line}");
            }
        }
    }
    // Reader ended (EOF or read error): dropping `incoming_sender` here closes
    // the `incoming` stream so awaiters observe the end-of-output.
    drop(incoming_sender);
}

async fn write_stdin(
    mut stdin: impl futures::AsyncWrite + Unpin,
    mut outgoing_receiver: UnboundedReceiver<InputMessage>,
) {
    while let Some(message) = outgoing_receiver.next().await {
        let mut line = match serde_json::to_string(&message) {
            Ok(line) => line,
            Err(error) => {
                log::error!("failed to serialize claude input message: {error}");
                continue;
            }
        };
        line.push('\n');
        if let Err(error) = stdin.write_all(line.as_bytes()).await {
            log::warn!("claude stdin write error: {error}");
            break;
        }
        if let Err(error) = stdin.flush().await {
            log::warn!("claude stdin flush error: {error}");
            break;
        }
    }
}

async fn drain_stderr(stderr: impl futures::AsyncRead + Unpin) {
    let mut reader = BufReader::new(stderr);
    let mut line = String::new();
    loop {
        line.clear();
        match reader.read_line(&mut line).await {
            Ok(0) | Err(_) => break,
            Ok(_) => {
                let trimmed = line.trim_end_matches(['\n', '\r']);
                if is_benign_agent_stderr(trimmed) {
                    log::debug!("claude stderr: {trimmed}");
                } else {
                    log::warn!("claude stderr: {trimmed}");
                }
            }
        }
    }
}

/// Mirrors `agent_servers::acp::is_benign_agent_stderr`: lines that fire on
/// routine internals (and the `{"type":"ping"}` keepalive the SDK sometimes
/// writes to stderr) are downgraded to debug so they don't look like errors.
fn is_benign_agent_stderr(line: &str) -> bool {
    line.contains("No onPostToolUseHook found for tool use ID") || line.contains(r#""type":"ping""#)
}
