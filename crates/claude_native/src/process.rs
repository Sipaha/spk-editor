//! Subprocess lifecycle for the `claude` binary: spawn + the reader/writer/
//! stderr async tasks over its stdio. This is the only module that touches the
//! OS; everything above it works against the [`InputMessage`]/[`OutputMessage`]
//! channels this type exposes.

use std::process::Stdio;

use anyhow::{Context as _, Result};
use futures::channel::mpsc::{self, UnboundedReceiver, UnboundedSender};
use futures::io::{AsyncBufReadExt as _, AsyncWriteExt as _, BufReader};
use futures::StreamExt as _;
use gpui::{App, Task};
use util::process::Child;

use crate::command::ClaudeCommandSpec;
use crate::protocol::{InputMessage, OutputMessage};

/// One running `claude` process. Holds the child plus the three stdio tasks;
/// dropping it cancels the tasks and (via `Child`'s process-group kill on an
/// explicit `kill`) tears down the subprocess. Messages flow over the
/// `outgoing` sender (stdin) and `incoming` receiver (stdout).
pub struct ClaudeProcess {
    child: Child,
    pub outgoing: UnboundedSender<InputMessage>,
    pub incoming: UnboundedReceiver<OutputMessage>,
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

        let executor = cx.background_executor();
        let reader = executor.spawn(read_stdout(stdout, incoming_sender));
        let writer = executor.spawn(write_stdin(stdin, outgoing_receiver));
        let stderr_task = executor.spawn(drain_stderr(stderr));

        Ok(Self {
            child,
            outgoing,
            incoming,
            _reader: reader,
            _writer: writer,
            _stderr: stderr_task,
        })
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
