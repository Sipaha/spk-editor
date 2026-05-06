//! User-message submission and follow-up queueing.
//!
//! `send_message` / `send_message_blocks` are the entry points the UI
//! hits when the user presses Send. Behaviour:
//!
//!   - Idle session → flip to `Running`, route through `AcpThread::send`,
//!     persist on success, transition to `Errored` on failure.
//!   - Already-`Running` session → merge into the queued bundle in
//!     `pending_messages` (one bundle ever; subsequent submissions
//!     append to it). The bundle is flushed when `Stopped` arrives.
//!
//! The first push into an empty queue gets a timestamp marker prepended
//! (see `build_queue_marker`) so the agent reads "the user typed this in
//! advance" rather than "the user is replying to my last question".
//!
//! `cancel_turn` and `interrupt_and_flush_pending` belong here because
//! they're the inverse path — stop the in-flight turn so the queue can
//! drain on the next Stopped event.

use anyhow::{Result, anyhow};
use chrono::Utc;
use gpui::{AsyncApp, Context, SharedString, Task};

use super::{SolutionAgentStore, SolutionAgentStoreEvent};
use crate::model::{SessionState, SolutionSessionId};

/// Opening of every queue-marker text block. Shared with
/// [`crate::conversation_render::strip_queue_marker`] so a future tweak to
/// the wording lands in exactly one place — without sharing, the strip
/// path silently desyncs from the writer and the marker leaks back into
/// the ghost preview / recalled draft.
pub(crate) const QUEUE_MARKER_PREFIX: &str = "[The user typed the following at ";

/// Trailing run after the marker's closing `]`. The `\n\n` is what
/// `strip_queue_marker` skips past to reach the user's content.
pub(crate) const QUEUE_MARKER_BODY_SEP: &str = "]\n\n";

/// Header text prepended to a queued message bundle on the first enqueue
/// during a `Running` turn. Tells the agent the user typed this in advance
/// (so it's not a direct reply to the last question or tool result) and
/// gives a local-time timestamp for when it landed in the queue. Follow-up
/// enqueues into the same bundle are merged without a second marker — by
/// design, since the queue is conceptually one growing message.
fn build_queue_marker(at: chrono::DateTime<Utc>) -> String {
    let local = at.with_timezone(&chrono::Local);
    format!(
        "{prefix}{time} (local time) while you were still on the previous turn — this is NOT a \
         direct reply to your last question or tool result, it was queued in advance.{sep}",
        prefix = QUEUE_MARKER_PREFIX,
        time = local.format("%H:%M:%S"),
        sep = QUEUE_MARKER_BODY_SEP,
    )
}

impl SolutionAgentStore {
    /// Best-effort cancel of an in-flight turn. Forwards to the underlying
    /// `AgentConnection::cancel`. Errors only when the session is unknown
    /// or has no live `AcpThread` yet — once the connection accepts the
    /// cancel request, downstream `AcpThreadEvent::Stopped` (or `Error`)
    /// drives the state transition through `handle_acp_event`.
    pub fn cancel_turn(&self, session_id: SolutionSessionId, cx: &mut Context<Self>) -> Result<()> {
        let session = self
            .session(session_id)
            .ok_or_else(|| anyhow!("unknown session {session_id}"))?;
        let (connection, acp_session_id) = {
            let s = session.read(cx);
            let thread = s
                .acp_thread
                .as_ref()
                .ok_or_else(|| anyhow!("session {session_id} has no ACP thread yet"))?;
            (
                thread.read(cx).connection().clone(),
                s.acp_session_id.clone(),
            )
        };
        connection.cancel(&acp_session_id, cx);
        Ok(())
    }

    /// Cancel the in-flight turn AND, once the resulting `Stopped(Cancelled)`
    /// arrives, flush `pending_messages` instead of clearing them. Wired to
    /// the "Send now" button in the compose row — the user typed a follow-up
    /// they want the agent to act on RIGHT NOW, not after the current turn
    /// completes.
    ///
    /// Internally just sets a one-shot flag on the session and delegates to
    /// `cancel_turn`. The handler in `handle_acp_event` (Stopped branch)
    /// reads the flag and routes the queue to `send_message_blocks`.
    pub fn interrupt_and_flush_pending(
        &mut self,
        session_id: SolutionSessionId,
        cx: &mut Context<Self>,
    ) -> Result<()> {
        let session = self
            .session(session_id)
            .ok_or_else(|| anyhow!("unknown session {session_id}"))?;
        if session.read(cx).pending_messages.is_empty() {
            anyhow::bail!("interrupt_and_flush_pending: no queued messages to flush");
        }
        session.update(cx, |s, _| s.flush_after_cancel = true);
        self.cancel_turn(session_id, cx)
    }

    /// Send a plain-text user message. Convenience wrapper around
    /// `send_message_blocks` for the common single-text-block case.
    pub fn send_message(
        &mut self,
        session_id: SolutionSessionId,
        content: String,
        cx: &mut Context<Self>,
    ) -> Task<Result<()>> {
        let blocks = vec![agent_client_protocol::schema::ContentBlock::Text(
            agent_client_protocol::schema::TextContent::new(content),
        )];
        self.send_message_blocks(session_id, blocks, cx)
    }

    /// Send a structured user message composed of one or more `ContentBlock`s
    /// (text + images, etc). Flips `SessionState` to `Running` synchronously
    /// (before the returned `Task` is awaited) so the UI shows activity
    /// immediately, then forwards the prompt to the underlying ACP connection.
    /// On success, schedules a persistence write of the session snapshot. On
    /// failure, transitions the session to `Errored`.
    pub fn send_message_blocks(
        &mut self,
        session_id: SolutionSessionId,
        blocks: Vec<agent_client_protocol::schema::ContentBlock>,
        cx: &mut Context<Self>,
    ) -> Task<Result<()>> {
        let Some(session_entity) = self.session(session_id) else {
            return Task::ready(Err(anyhow!("unknown session {session_id}")));
        };

        if blocks.is_empty() {
            return Task::ready(Err(anyhow!(
                "send_message_blocks: at least one ContentBlock required"
            )));
        }

        // Already running? Queue the message instead of restarting the
        // turn — matches Claude Code CLI's "type follow-ups while the
        // agent is still working" behaviour. Subsequent queued sends
        // are *merged* into the existing pending entry (separated by a
        // blank line) so the user sees a single ghost bubble that
        // grows, not a stack of fragments. Flush is one big prompt to
        // the agent, sent once `Stopped` fires.
        let already_running = matches!(session_entity.read(cx).state, SessionState::Running { .. });
        if already_running {
            session_entity.update(cx, |s, _| {
                if let Some(last) = s.pending_messages.back_mut() {
                    last.push(agent_client_protocol::schema::ContentBlock::Text(
                        agent_client_protocol::schema::TextContent::new("\n\n".to_string()),
                    ));
                    last.extend(blocks);
                } else {
                    // First enqueue → prepend a timestamp marker so when the
                    // queue flushes, the agent sees "this was typed in advance
                    // at HH:MM:SS, not in response to my last question". Once
                    // the bundle exists, follow-up enqueues are merged
                    // (above) WITHOUT a second marker — per UX, queued
                    // follow-ups are continuations of the same thought.
                    let marker = build_queue_marker(Utc::now());
                    let mut bundle = Vec::with_capacity(blocks.len() + 1);
                    bundle.push(agent_client_protocol::schema::ContentBlock::Text(
                        agent_client_protocol::schema::TextContent::new(marker),
                    ));
                    bundle.extend(blocks);
                    s.pending_messages.push_back(bundle);
                }
                s.last_activity_at = Utc::now();
            });
            cx.emit(SolutionAgentStoreEvent::SessionStateChanged(session_id));
            cx.notify();
            return Task::ready(Ok(()));
        }

        // Flip state immediately, before the spawn, so callers observing the
        // session right after this call returns see `Running`. Clear the
        // last-turn duration too — the "Done in Xs" indicator from the
        // previous turn is stale the moment a new turn begins.
        session_entity.update(cx, |s, _| {
            s.state = SessionState::Running {
                started_at: std::time::Instant::now(),
                notified: false,
            };
            s.last_activity_at = Utc::now();
            s.last_turn_duration = None;
        });
        cx.emit(SolutionAgentStoreEvent::SessionStateChanged(session_id));
        cx.notify();

        let Some(acp_thread) = session_entity.read(cx).acp_thread.clone() else {
            return Task::ready(Err(anyhow!("session {session_id} has no ACP thread yet")));
        };

        // Route through `AcpThread::send` (not `connection.prompt` directly)
        // so the turn runs inside `run_turn`. That wrapper appends the
        // user message, drives streaming-text flushing, and — crucially —
        // emits `AcpThreadEvent::Stopped` on success / `Error` on failure.
        // Without those events the store-side subscription never sees the
        // turn end, so `SessionState` stays stuck on `Running` after the
        // assistant has already replied.
        let send_task = acp_thread.update(cx, |thread, cx| thread.send(blocks, cx));
        // Stamped at spawn so the post-await branches can detect that the
        // session's underlying ACP thread was rotated out from under us
        // (`reset_context` for `/clear`, `rotate_context` for `/compact`)
        // while this turn was in flight. Without this guard, the old turn's
        // late `Err` would clobber the freshly-reset `Idle` state with
        // `Errored(...)` — a confusing UX where the user just typed
        // `/clear`, sees a blank conversation, and then watches it flip to
        // an error a second later because the previous turn finally
        // resolved.
        let expected_acp_session_id = session_entity.read(cx).acp_session_id.clone();

        cx.spawn(async move |this, cx: &mut AsyncApp| {
            let result = send_task.await;
            match &result {
                Err(err) => {
                    // run_turn already emitted `AcpThreadEvent::Error`,
                    // which the store subscription translated into
                    // `Errored("agent error")`. Overwrite that with the
                    // specific error string so the user sees the actual
                    // cause instead of a generic placeholder.
                    let err_message = SharedString::from(err.to_string());
                    this.update(cx, |store, cx| {
                        if let Some(s) = store.session(session_id) {
                            let still_same = s.read(cx).acp_session_id == expected_acp_session_id;
                            if !still_same {
                                log::debug!(
                                    "send_message_blocks: dropping late error for {session_id} \
                                     ({err_message:?}); session was rotated/reset mid-flight"
                                );
                                return;
                            }
                            s.update(cx, |s, _| {
                                s.state = SessionState::Errored(err_message.clone());
                            });
                            cx.emit(SolutionAgentStoreEvent::SessionStateChanged(session_id));
                            cx.notify();
                        }
                    })?;
                }
                Ok(_) => {
                    // Stopped event already transitioned state to Idle
                    // via the store subscription; just persist the snapshot.
                    // Skip the persist when the thread was swapped — the
                    // current snapshot reflects the NEW session, not the
                    // turn that just resolved, so writing it under the
                    // success-of-the-old-turn premise would just be a
                    // misleading log entry.
                    this.update(cx, |store, cx| {
                        if let Some(s) = store.session(session_id)
                            && s.read(cx).acp_session_id == expected_acp_session_id
                        {
                            store.persist_session_blob(session_id, cx);
                        }
                    })?;
                }
            }
            result.map(|_| ()).map_err(|err| anyhow!(err))
        })
    }
}
