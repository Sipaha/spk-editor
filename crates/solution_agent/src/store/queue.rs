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
use gpui::{AsyncApp, Context, Entity, SharedString, Task};

use acp_thread::{AcpThread, AgentThreadEntry, SelectedPermissionOutcome, ToolCallStatus};
use agent_client_protocol::schema as acp;
use claude_native::ClaudeNativeConnection;

use super::{SolutionAgentStore, SolutionAgentStoreEvent};
use crate::model::{SessionState, SolutionSessionId, SolutionSessionMetadata};

/// Scan a thread's entries for a tool call sitting in
/// `WaitingForConfirmation` and, if found, return its id together with a
/// REJECT-flavoured `SelectedPermissionOutcome` to unblock it.
///
/// WHY this exists: when the agent asks for authorization, the ACP turn
/// BLOCKS on a oneshot inside `request_tool_call_authorization` until the
/// client answers. If the user ignores the allow/reject buttons and just
/// types a new message, the old behaviour silently queued the text into
/// `pending_messages` and the turn stayed blocked forever — the reported
/// "messages pile up and nothing happens" bug. Detecting the pending
/// confirmation here lets the send path resolve it first (see
/// `send_message_blocks`).
///
/// Reject-outcome selection: we reuse `conversation_render::permission_buttons`
/// to flatten the live options into clickable buttons, then pick a
/// non-allow (`!is_allow()`) button, preferring `RejectOnce` over
/// `RejectAlways` (decline just this once, don't poison future prompts
/// with a remembered "always reject"). If — unexpectedly — there is no
/// reject-flavoured button at all (a malformed server response offering
/// only allow options), we return `None` and the caller skips the resolve:
/// a stuck turn is the acceptable failure mode here, silently picking an
/// allow button (which would AUTO-APPROVE the tool call) is NOT.
///
/// NOTE on the "custom / free-text answer" branch: the agreed design also
/// wanted, when a question offers a free-text answer, to submit the user's
/// typed text AS that answer. The current ACP protocol cannot express
/// this — `PermissionOptionKind` is only {AllowOnce, AllowAlways,
/// RejectOnce, RejectAlways} and the only `SelectedPermissionParams`
/// variant is `Terminal { patterns }` (terminal command globs, not
/// arbitrary text). There is no option kind or params variant carrying a
/// free-text answer, so the custom-answer branch is currently
/// unreachable. If a future protocol adds one, build that outcome here
/// instead of the reject outcome and short-circuit the send.
pub(crate) fn pending_authorization_reject(
    thread: &Entity<AcpThread>,
    cx: &Context<SolutionAgentStore>,
) -> Option<(acp::ToolCallId, SelectedPermissionOutcome)> {
    let thread = thread.read(cx);
    for entry in thread.entries() {
        let AgentThreadEntry::ToolCall(call) = entry else {
            continue;
        };
        let ToolCallStatus::WaitingForConfirmation { options, .. } = &call.status else {
            continue;
        };
        if let Some(button) = crate::conversation_render::pick_reject_button(options) {
            return Some((call.id.clone(), button.outcome()));
        }
    }
    None
}

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

/// Compact one-line summary of a content-block bundle for the audit log
/// — enough to reconstruct what was queued / dropped from log lines
/// alone, without dumping multi-MB image blobs. Text is truncated to
/// `MAX_PREVIEW`; images / resources collapse to a typed marker. Kept
/// in this file (vs `conversation_render`) because the queue codepath
/// is the only consumer.
pub(crate) fn summarize_blocks_for_log(
    blocks: &[agent_client_protocol::schema::ContentBlock],
) -> String {
    use agent_client_protocol::schema as acp;
    const MAX_PREVIEW: usize = 200;
    let mut out = String::new();
    let mut text_total = 0usize;
    let mut images = 0usize;
    let mut other = 0usize;
    for block in blocks {
        match block {
            acp::ContentBlock::Text(t) => {
                let snippet: String = t.text.chars().take(MAX_PREVIEW).collect();
                let truncated = t.text.chars().count() > MAX_PREVIEW;
                if !out.is_empty() {
                    out.push_str(" + ");
                }
                out.push('"');
                // Keep the log a single line: replace newlines with `\n`.
                for ch in snippet.chars() {
                    if ch == '\n' {
                        out.push_str("\\n");
                    } else if ch == '"' {
                        out.push_str("\\\"");
                    } else {
                        out.push(ch);
                    }
                }
                if truncated {
                    out.push('…');
                }
                out.push('"');
                text_total += t.text.chars().count();
            }
            acp::ContentBlock::Image(_) => images += 1,
            _ => other += 1,
        }
    }
    if images > 0 || other > 0 || text_total > MAX_PREVIEW {
        let mut suffix = String::new();
        if images > 0 {
            suffix.push_str(&format!(" +{images}img"));
        }
        if other > 0 {
            suffix.push_str(&format!(" +{other}other"));
        }
        if text_total > MAX_PREVIEW {
            suffix.push_str(&format!(" total_chars={text_total}"));
        }
        out.push_str(&suffix);
    }
    if out.is_empty() {
        out.push_str("(empty)");
    }
    out
}

/// Flatten a content-block bundle into a single human-readable string the
/// native backend can hand to the agent as `additionalContext`. Text blocks
/// are concatenated verbatim; image blocks collapse to numbered placeholders
/// (`[image #1]`, `[image #2]`, …) so a text-only side channel can still
/// signal "the user attached an image" without trying to ship the bytes.
/// Other variants are silently dropped — the inject side channel is text-only.
fn inject_text_from_blocks(blocks: &[acp::ContentBlock]) -> String {
    let mut out = String::new();
    let mut image_idx = 1usize;
    for block in blocks {
        match block {
            acp::ContentBlock::Text(t) => {
                if !out.is_empty() && !out.ends_with('\n') {
                    out.push('\n');
                }
                out.push_str(&t.text);
            }
            acp::ContentBlock::Image(_) => {
                if !out.is_empty() && !out.ends_with('\n') {
                    out.push('\n');
                }
                out.push_str(&format!("[image #{image_idx}]"));
                image_idx += 1;
            }
            _ => {}
        }
    }
    out.trim().to_string()
}

impl SolutionAgentStore {
    /// Best-effort cancel of an in-flight turn. Forwards to the underlying
    /// `AgentConnection::cancel`. Errors only when the session is unknown
    /// or has no live `AcpThread` yet — once the connection accepts the
    /// cancel request, downstream `AcpThreadEvent::Stopped` (or `Error`)
    /// drives the state transition through `handle_acp_event`.
    pub fn cancel_turn(
        &mut self,
        session_id: SolutionSessionId,
        cx: &mut Context<Self>,
    ) -> Result<()> {
        let session = self
            .session(session_id)
            .ok_or_else(|| anyhow!("unknown session {session_id}"))?;
        // Idempotent: only an in-flight turn can be stopped. A cancel in
        // Stopping/Idle/Errored is a safe no-op (covers repeated taps and the
        // mobile's deferred resend-on-reconnect).
        let in_flight = matches!(
            session.read(cx).state,
            SessionState::Running { .. } | SessionState::AwaitingInput
        );
        if !in_flight {
            return Ok(());
        }
        let (connection, acp_session_id) = {
            let s = session.read(cx);
            let thread = s
                .acp_thread()
                .ok_or_else(|| anyhow!("session {session_id} has no ACP thread yet"))?;
            (
                thread.read(cx).connection().clone(),
                s.acp_session_id.clone(),
            )
        };
        // Authoritative, backend-agnostic: flip to Stopping (broadcasts
        // SessionStateChanged) BEFORE forwarding. Stopping -> Idle still arrives
        // via the AcpThreadEvent::Stopped handler.
        self.mutate_state(session_id, |state| *state = SessionState::Stopping, cx);
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

        // "Chat About This" path. If the open session has a tool call
        // sitting in `WaitingForConfirmation`, the ACP turn is BLOCKED on a
        // oneshot until someone answers the allow/reject prompt. Typing a
        // new message used to just enqueue into `pending_messages` while the
        // turn stayed blocked forever — the user appeared stuck and their
        // follow-ups piled up invisibly. Instead, resolve the pending
        // authorization with a REJECT outcome FIRST (so the agent stops
        // waiting and the turn can run to `Stopped`), THEN fall through to
        // the normal send below. Because the session is still `Running`, the
        // message lands in `pending_messages` and the existing
        // flush-on-`Stopped` machinery delivers it as the next turn once the
        // (now-unblocked) turn ends — so it is never dropped.
        //
        // (The "submit typed text AS a custom/free-text answer" branch is
        // intentionally absent: the current ACP protocol can't express a
        // free-text permission answer — see `pending_authorization_reject`.)
        if let Some(thread) = session_entity.read(cx).acp_thread().cloned()
            && let Some((tool_call_id, reject_outcome)) = pending_authorization_reject(&thread, cx)
        {
            log::info!(
                target: "solution_agent::queue",
                "session={session_id} send while tool call {tool_call_id} awaiting \
                 authorization — declining (reject) to unblock the turn, then delivering \
                 the user's message as the next turn",
            );
            // Guarantee the just-queued message is delivered even if the
            // agent treats the rejection as a turn *cancel* rather than an
            // EndTurn: the Cancelled-stop handler clears `pending_messages`
            // WITHOUT sending unless `flush_after_cancel` is set. One-shot
            // flag — no-op on EndTurn, consumed/reset by the Stopped handler.
            session_entity.update(cx, |s, _| s.flush_after_cancel = true);
            thread.update(cx, |thread, cx| {
                thread.authorize_tool_call(tool_call_id, reject_outcome, cx);
            });
            // Fall through. The session is still `Running` (the rejected
            // turn hasn't emitted `Stopped` yet), so the block below enqueues
            // this message and the `Stopped` handler flushes it.
        }

        // Already running? Two routes:
        //
        //   (a) Native `claude` backend → push the user message into the
        //       live `AcpThread` as a real user entry AND buffer the text
        //       on the `ClaudeNativeConnection`'s `pending_inject` slot.
        //       The next `hook_callback` (PostToolUse or Stop) consumes the
        //       slot and feeds it to the running turn as `additionalContext`
        //       — the agent reacts in the SAME turn, no interrupt, no new
        //       prompt, no broken tool. The chat history stays a single
        //       timeline (user bubble + the agent's response that follows).
        //
        //   (b) Anything else → fall through to the legacy
        //       `pending_messages` queue: merge the new bundle into the
        //       existing pending entry (separated by a blank line) and flush
        //       on `Stopped`. Realistically there's no non-native backend
        //       wired up right now (the ACP wrapper path was retired), but
        //       the fallback stays for safety and for tests using
        //       `MockConnection`.
        //
        // For repeated sends in the same Running window on the native path:
        // we use `inject_user_message_append`, NOT `inject_user_message`, so
        // a second send before the next hook fires merges with the first
        // (blank-line separator) instead of overwriting it — mirroring the
        // queue's existing "one growing message" UX.
        let already_running = matches!(session_entity.read(cx).state, SessionState::Running { .. });
        if already_running {
            if let Some(thread) = session_entity.read(cx).acp_thread().cloned() {
                let connection = thread.read(cx).connection().clone();
                if let Some(native) = connection.downcast::<ClaudeNativeConnection>() {
                    let blocks_text_summary = summarize_blocks_for_log(&blocks);
                    let injected_text = inject_text_from_blocks(&blocks);
                    let acp_session_id =
                        session_entity.read(cx).acp_session_id.clone();
                    let chars = injected_text.chars().count();
                    let appended =
                        native.inject_user_message_append(&acp_session_id, injected_text);
                    thread.update(cx, |thread, cx| {
                        for block in blocks {
                            thread.push_user_content_block(None, block, cx);
                        }
                    });
                    session_entity.update(cx, |s, _| {
                        s.last_activity_at = Utc::now();
                    });
                    log::info!(
                        target: "solution_agent::queue",
                        "session={session_id} via=hook chars={chars} appended={appended} preview={blocks_text_summary}",
                    );
                    cx.emit(SolutionAgentStoreEvent::SessionStateChanged(session_id));
                    cx.notify();
                    return Task::ready(Ok(()));
                }
            }
            // Audit log: queueing is a frequent source of "where did
            // my message go?" bug reports — having every enqueue +
            // queue size in the log lets us reconstruct what reached
            // pending_messages even when the message later got dropped
            // silently (e.g. by a `/clear` or a Cancelled stop).
            // `target: "solution_agent::queue"` makes these greppable.
            let blocks_text_summary = summarize_blocks_for_log(&blocks);
            let merged = session_entity.update(cx, |s, _| {
                let merged = s.pending_messages.back().is_some();
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
                    let mut bundle = Vec::with_capacity(2);
                    bundle.push(agent_client_protocol::schema::ContentBlock::Text(
                        agent_client_protocol::schema::TextContent::new(marker),
                    ));
                    bundle.extend(blocks);
                    s.pending_messages.push_back(bundle);
                }
                s.last_activity_at = Utc::now();
                merged
            });
            let queue_len = session_entity.read(cx).pending_messages.len();
            log::info!(
                target: "solution_agent::queue",
                "session={session_id} enqueued (merged={merged}, queue_len={queue_len}) preview={blocks_text_summary}",
            );
            cx.emit(SolutionAgentStoreEvent::SessionStateChanged(session_id));
            // External MCP consumers (the mobile client) render queued
            // bundles as Queued bubbles in real time off this event —
            // without it a desktop-typed follow-up to a busy session
            // would stay invisible on a paired mobile until the
            // eventual flush.
            cx.emit(SolutionAgentStoreEvent::SessionQueueChanged(session_id));
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

        let Some(acp_thread) = session_entity.read(cx).acp_thread().cloned() else {
            // Cold session — no live ACP thread. Wake the agent
            // synchronously via `resume_session` (mirrors what the
            // desktop's `SolutionSessionView::start_resume` does, minus
            // the Window — MCP-driven sends don't have one). Re-enters
            // `send_message_blocks` once the thread is attached so the
            // normal hot-path code below runs unchanged.
            return self.send_message_blocks_with_wake(session_id, blocks, cx);
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

    /// Wake a cold session (no ACP thread attached) and queue the
    /// supplied blocks for delivery once the wake handshake completes.
    /// Driven by `send_message_blocks` when it discovers an empty
    /// `acp_thread()` — the user (typically the mobile client over
    /// MCP) sent to a sleeping session and the previous behaviour was
    /// to return a hard "session has no ACP thread yet" error.
    ///
    /// Snapshots the session metadata, resolves the owning solution
    /// from `SolutionStore`, builds a headless project (no worktree —
    /// `resume_session` keys claude-acp's jsonl lookup off
    /// `meta.cwd`, not the project's worktree), then awaits
    /// `resume_session` + re-enters `send_message_blocks`. The
    /// second entry sees the now-attached thread and forwards
    /// normally — if the session became hot during the wake (some
    /// other path attached a thread first), that's benign.
    ///
    /// Reuses `session.project` if it's still cached (sessions
    /// created in this process keep the original handle) instead of
    /// constructing a headless one — keeps the existing worktree set
    /// intact for the resume call.
    fn send_message_blocks_with_wake(
        &mut self,
        session_id: SolutionSessionId,
        blocks: Vec<agent_client_protocol::schema::ContentBlock>,
        cx: &mut Context<Self>,
    ) -> Task<Result<()>> {
        let Some(session_entity) = self.session(session_id) else {
            return Task::ready(Err(anyhow!("unknown session {session_id}")));
        };
        let (meta, cached_project) = session_entity.read_with(cx, |s, _| {
            let meta = SolutionSessionMetadata {
                id: s.id,
                solution_id: s.solution_id.clone(),
                agent_id: s.agent_id.clone(),
                acp_session_id: s.acp_session_id.clone(),
                title: s.title.clone(),
                created_at: s.created_at,
                last_activity_at: s.last_activity_at,
                preview: None,
                total_tokens: None,
                context_count: s.context_count,
                cwd: s.cwd.clone(),
                parent_session_id: s.parent_session_id,
            };
            (meta, s.project.clone())
        });

        let solution_id = meta.solution_id.clone();
        let solution = solutions::SolutionStore::try_global(cx).and_then(|store| {
            store
                .read(cx)
                .solutions()
                .iter()
                .find(|s| s.id == solution_id)
                .cloned()
        });
        let Some(solution) = solution else {
            return Task::ready(Err(anyhow!(
                "unknown_solution: cannot wake session {session_id} — solution {solution_id:?} \
                 not found in SolutionStore"
            )));
        };

        let project = match cached_project {
            Some(project) => project,
            None => match SolutionAgentStore::make_headless_project_for_solution(&solution, cx) {
                Ok(project) => project,
                Err(err) => {
                    return Task::ready(Err(anyhow!(
                        "wake_for_send: headless project construction failed for {session_id}: \
                         {err:#}"
                    )));
                }
            },
        };

        log::info!(
            target: "solution_agent::queue",
            "session={session_id} cold-send wake: invoking resume_session before forwarding \
             {} block(s)",
            blocks.len()
        );

        let resume_task = self.resume_session(meta, project, cx);
        cx.spawn(async move |this, cx: &mut AsyncApp| {
            let _resumed_id = resume_task.await?;
            // The thread is now attached on the same session entity;
            // re-enter the send path so the hot branch fires. If a
            // racing path attached the thread first, this still
            // resolves correctly — `send_message_blocks` always
            // re-reads `acp_thread()` after the cold check.
            let task = this.update(cx, |store, cx| {
                store.send_message_blocks(session_id, blocks, cx)
            })?;
            task.await
        })
    }
}
