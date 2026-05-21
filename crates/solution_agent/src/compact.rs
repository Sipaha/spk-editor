//! "Compact context" workflow: dump the current session's running summary to handoff files, then continue in a fresh ACP session.

use anyhow::{Result, anyhow};
use gpui::{App, AppContext as _, Context, SharedString};
use solutions::SolutionStore;
use workspace::notifications::{NotificationId, simple_message_notification::MessageNotification};

use crate::model::{SessionState, SolutionSessionId};
use crate::navigator::SolutionSessionsNavigator;
use crate::status_row::DEFAULT_CONTEXT_WINDOW;
use crate::store::SolutionAgentStore;

/// Outcome of [`start_compact_for_session`] — distinguishes "we ran the
/// orchestration and the prompt is now queued on the agent" from "we
/// declined to compact and here's why". Errors out only when the
/// session id is unknown or the underlying filesystem refuses to create
/// the dump directory (the two cases that no client retry can fix
/// without operator intervention).
#[derive(Debug, Clone)]
pub(crate) struct StartCompactOutcome {
    pub queued: bool,
    /// Human-readable reason when `queued == false`. `None` when queued
    /// successfully — keeps the success path cheap on the wire.
    pub reason: Option<String>,
}

/// Shared orchestration: precondition gate → render the compact prompt
/// (creates the `<root>/.agents/<sid>/c<NN>/` dump dir as a side effect)
/// → enqueue the rendered prompt as a user message on the live
/// `AcpThread`. Driven by both the desktop status-row popover and the
/// `solution_agent.start_compact` MCP tool so the two surfaces share a
/// single notion of "is this session compactable right now".
///
/// The cold-session branch is intentionally NOT in here: queueing on a
/// `SolutionSessionView` requires `&mut Window`, which the MCP path
/// doesn't have. The desktop entry point handles cold separately via
/// `start_compact_from_cold` on the navigator.
pub(crate) fn start_compact_for_session(
    session_id: SolutionSessionId,
    cx: &mut App,
) -> Result<StartCompactOutcome> {
    let store = SolutionAgentStore::global(cx);
    let session_entity = store
        .read_with(cx, |s, _| s.session(session_id))
        .ok_or_else(|| anyhow!("unknown session {session_id}"))?;

    // Precondition: must be Idle. A Running/AwaitingInput session would
    // race with the in-flight turn (claude-acp queues prompts in
    // `pending_messages`, which would deliver the compact instructions
    // AFTER the active turn — possibly minutes later — and surprise
    // the user). Cold sessions can't be compacted via MCP either
    // (no Window to drive the wake-flush hook).
    {
        let s = session_entity.read(cx);
        if s.is_cold() {
            return Ok(StartCompactOutcome {
                queued: false,
                reason: Some(
                    "session is cold; open it in the editor first so the agent can receive the \
                     compact prompt"
                        .into(),
                ),
            });
        }
        if !matches!(s.state, SessionState::Idle) {
            return Ok(StartCompactOutcome {
                queued: false,
                reason: Some(format!(
                    "session is busy ({:?}); wait for the current turn to finish",
                    s.state
                )),
            });
        }

        // Precondition: meaningful context to compact AND headroom to
        // dump the summary. Matches `status_row::render_status_row`'s
        // gate so MCP and the desktop UI agree on "compactable".
        let usage = s
            .acp_thread()
            .and_then(|thread| thread.read(cx).token_usage().cloned());
        let used = usage
            .as_ref()
            .map(|u| u.used_tokens)
            .or(s.cached_total_tokens)
            .unwrap_or(0);
        let max = usage
            .as_ref()
            .map(|u| u.max_tokens)
            .filter(|m| *m > 0)
            .or(s.cached_max_tokens)
            .unwrap_or(DEFAULT_CONTEXT_WINDOW);
        let pct = if max == 0 {
            0.0
        } else {
            (used as f64 / max as f64).clamp(0.0, 1.0)
        };
        let remaining = max.saturating_sub(used);
        if pct < COMPACT_BUTTON_MIN_PCT {
            return Ok(StartCompactOutcome {
                queued: false,
                reason: Some(format!(
                    "conversation is short ({:.1}%); compact later",
                    pct * 100.0
                )),
            });
        }
        if remaining < COMPACT_HEADROOM_MIN_TOKENS {
            return Ok(StartCompactOutcome {
                queued: false,
                reason: Some(format!(
                    "only {} tokens of headroom left — start a fresh session manually",
                    remaining
                )),
            });
        }
    }

    let rendered = render_compact_prompt_inner(session_id, cx)?;
    store.update(cx, |store, cx| {
        store
            .send_message(session_id, rendered, cx)
            .detach_and_log_err(cx);
    });
    Ok(StartCompactOutcome {
        queued: true,
        reason: None,
    })
}

/// Render the compact-instruction template for `session_id` and create
/// the per-rotation dump directory. Free-function counterpart of the
/// navigator's `render_compact_prompt` — returns an `anyhow::Error` so
/// MCP callers get a structured error instead of a workspace toast.
pub(crate) fn render_compact_prompt_inner(
    session_id: SolutionSessionId,
    cx: &mut App,
) -> Result<String> {
    let store = SolutionAgentStore::global(cx);
    let session_entity = store
        .read_with(cx, |s, _| s.session(session_id))
        .ok_or_else(|| anyhow!("unknown session {session_id}"))?;
    let (solution_id, agent_id, started_at, context_count, used, max) = {
        let s = session_entity.read(cx);
        let context_count = s.context_count;
        // Live `token_usage` when hot, else fall back to `cached_total_tokens`
        // so a cold caller still gets a meaningful prompt header.
        let usage = s
            .acp_thread()
            .and_then(|thread| thread.read(cx).token_usage().cloned());
        let used = usage
            .as_ref()
            .map(|u| u.used_tokens)
            .or(s.cached_total_tokens)
            .unwrap_or(0);
        let max = usage
            .as_ref()
            .map(|u| u.max_tokens)
            .filter(|m| *m > 0)
            .or(s.cached_max_tokens)
            .unwrap_or(DEFAULT_CONTEXT_WINDOW);
        (
            s.solution_id.clone(),
            s.agent_id.clone(),
            s.created_at,
            context_count,
            used,
            max,
        )
    };

    let solution_root = SolutionStore::try_global(cx)
        .and_then(|store| {
            store.read_with(cx, |s, _| {
                s.solutions()
                    .iter()
                    .find(|sol| sol.id == solution_id)
                    .map(|sol| sol.root.clone())
            })
        })
        .ok_or_else(|| {
            anyhow!(
                "Compact failed: solution {:?} not registered",
                solution_id.0
            )
        })?;

    // `<root>/.agents/<sid>/c<count>/` — `c01`, `c02`, … so a
    // single `<sid>` directory groups every rotation of one
    // logical conversation. The leading `c` keeps the names from
    // accidentally colliding with the legacy timestamp scheme.
    let context_label = format!("c{context_count:02}");
    let compact_dir = solution_root
        .join(".agents")
        .join(session_id.to_string())
        .join(&context_label);
    std::fs::create_dir_all(&compact_dir).map_err(|err| {
        anyhow!(
            "Compact failed: cannot create {}: {err}",
            compact_dir.display()
        )
    })?;

    let mut compact_dir_str = compact_dir.to_string_lossy().to_string();
    if !compact_dir_str.ends_with(std::path::MAIN_SEPARATOR) {
        compact_dir_str.push(std::path::MAIN_SEPARATOR);
    }

    Ok(COMPACT_INSTRUCTIONS_TEMPLATE
        .replace("{{session_id}}", &session_id.to_string())
        .replace("{{compact_dir}}", &compact_dir_str)
        .replace("{{solution_id}}", solution_id.0.as_str())
        .replace("{{agent_id}}", agent_id.as_ref())
        .replace("{{started_at_iso}}", &started_at.to_rfc3339())
        .replace("{{tokens_used}}", &used.to_string())
        .replace("{{tokens_max}}", &max.to_string()))
}

impl SolutionSessionsNavigator {
    /// Renders the current compact-instruction template, creates the
    /// per-rotation handoff directory, and ships the rendered prompt as
    /// a regular user message. The agent then writes its summary files
    /// into that directory and (after we've handed it `compact_dir`)
    /// calls back via `solution_agent.compact_session`.
    pub(crate) fn start_compact(
        &self,
        session_id: crate::model::SolutionSessionId,
        cx: &mut Context<Self>,
    ) {
        match start_compact_for_session(session_id, cx) {
            Ok(StartCompactOutcome { queued: true, .. }) => {}
            Ok(StartCompactOutcome {
                queued: false,
                reason: Some(reason),
            }) => {
                log::info!("solution_agent compact declined: {reason}");
            }
            Ok(StartCompactOutcome {
                queued: false,
                reason: None,
            }) => {}
            Err(err) => {
                self.toast_error(SharedString::from(format!("Compact failed: {err}")), cx);
            }
        }
    }

    /// Render the compact-instruction template for `session_id` and create
    /// the `<root>/.agents/<sid>/c<NN>/` dump directory. Returns the rendered
    /// prompt body. Surfaces a workspace toast and returns `None` on the
    /// same failure modes the inline path used to handle (unknown solution,
    /// mkdir failure).
    pub(crate) fn render_compact_prompt(
        &self,
        session_id: crate::model::SolutionSessionId,
        cx: &mut Context<Self>,
    ) -> Option<String> {
        match render_compact_prompt_inner(session_id, cx) {
            Ok(rendered) => Some(rendered),
            Err(err) => {
                self.toast_error(SharedString::from(err.to_string()), cx);
                None
            }
        }
    }

    /// Cold-state compact: render the prompt now, queue it on the
    /// active `SolutionSessionView` as `pending_send`, and kick off
    /// `start_resume`. The view's existing wake-flush hook
    /// (`flush_pending_send_if_ready`) dispatches the queued prompt
    /// the moment `acp_thread` becomes `Some`. Status badge sequence
    /// the user sees: `Sleeping → Resuming… → Thinking… → Idle`.
    ///
    /// No-ops if there's no rendered prompt (template render +
    /// mkdir already toasted the failure) or if the view is gone.
    pub(crate) fn start_compact_from_cold(
        &self,
        session_id: crate::model::SolutionSessionId,
        view: gpui::Entity<crate::session_view::SolutionSessionView>,
        window: &mut gpui::Window,
        cx: &mut Context<Self>,
    ) {
        let Some(rendered) = self.render_compact_prompt(session_id, cx) else {
            return;
        };
        view.update(cx, |view, cx| {
            view.enqueue_text_pending_send_and_resume(rendered, window, cx);
        });
    }

    fn toast_error(&self, message: SharedString, cx: &mut Context<Self>) {
        let Some(workspace) = self.workspace.upgrade() else {
            log::warn!("solution_agent toast (no workspace): {message}");
            return;
        };
        workspace.update(cx, |workspace, cx| {
            struct CompactFailed;
            workspace.show_notification(NotificationId::unique::<CompactFailed>(), cx, move |cx| {
                cx.new(|cx| MessageNotification::new(message, cx))
            });
        });
    }
}

/// Compact button activation threshold. Below this the conversation is
/// too short for a compact to be worth the round-trip.
pub(crate) const COMPACT_BUTTON_MIN_PCT: f64 = 0.20;

/// Threshold at which the compact button paints in warning colour.
/// Past this, the user should rotate before the model starts dropping
/// context off the back of the window.
pub(crate) const COMPACT_BUTTON_WARN_PCT: f64 = 0.50;

/// Minimum free tokens we require before allowing a compact: enough
/// for the instruction prompt (~3 k) and the agent's dump (state.md +
/// decisions.md + next.md + continue.md, typically ~10–20 k combined),
/// plus a buffer for tool-call traces. Below this, refuse the button —
/// a half-truncated compact loses more than just starting over does.
pub(crate) const COMPACT_HEADROOM_MIN_TOKENS: u64 = 30_000;

/// Markdown template fed to the agent on compact. `{{var}}` placeholders
/// are filled from session state at click time. Source-of-truth lives in
/// the resources file so the prose can be reviewed without recompiling.
const COMPACT_INSTRUCTIONS_TEMPLATE: &str =
    include_str!("../resources/compact_context_instructions.md");

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapter::AdapterRegistry;
    use crate::model::SolutionSessionId;
    use gpui::{TestAppContext, VisualTestContext};
    use std::rc::Rc;
    use std::sync::Arc;
    use std::sync::atomic::AtomicUsize;

    /// Cold-compact orchestrator must:
    ///   1. Render the compact instructions prompt (template variables
    ///      replaced with cached cold-state values).
    ///   2. Queue it as a single-block `pending_send` on the view.
    ///   3. Set `resuming = true` so the badge flips to `Resuming…`.
    ///
    /// Assertions are checked synchronously — before `run_until_parked()` —
    /// so the spawned `resume_session` task never fires and we don't have
    /// to mock the full ACP handshake. The workspace entity is kept alive
    /// for the duration of the test so `start_resume`'s synchronous
    /// `workspace.upgrade()` check returns `Some` and does not clear
    /// `pending_send` / `resuming` before we can read them.
    #[gpui::test]
    async fn cold_compact_queues_prompt_and_kicks_resume(cx: &mut TestAppContext) {
        let (solution_id, _tmp, project) =
            crate::store::tests::setup_solution_and_project(cx).await;
        let agent_id = gpui::SharedString::from("mock-agent");

        cx.update(|cx| {
            // `Workspace::new` calls `theme_settings::track_window_appearance`
            // which requires `GlobalSystemAppearance` to be initialized.
            theme_settings::init(theme::LoadThemes::JustBase, cx);

            let registry = Arc::new(AdapterRegistry::new());
            SolutionAgentStore::init_global(cx, registry);
            let store = SolutionAgentStore::global(cx);
            store.update(cx, |store, _| {
                store.register_agent_server(
                    agent_id.clone(),
                    Rc::new(crate::test_support::MockAgentServer::new(Arc::new(
                        AtomicUsize::new(0),
                    ))),
                );
            });
        });

        let session_id = SolutionSessionId::new();

        // Open a Workspace window so `start_resume` can synchronously
        // upgrade `self.workspace` — without a valid workspace entity,
        // `start_resume` immediately clears `pending_send` + `resuming`.
        let workspace_window =
            cx.add_window(|window, cx| workspace::Workspace::test_new(project.clone(), window, cx));

        // Obtain a weak handle to the workspace entity BEFORE creating
        // the `VisualTestContext` so we can call `workspace_window.root`
        // without a re-entrant `update_window` (which would deadlock
        // because `vcx.update` already holds the window lock).
        let workspace_weak = cx.update(|cx| {
            workspace_window
                .root(cx)
                .expect("workspace window is alive")
                .downgrade()
        });

        let mut vcx = VisualTestContext::from_window(*workspace_window, cx);

        let (navigator_entity, view_entity) = vcx.update(|window, cx| {
            let store = SolutionAgentStore::global(cx);
            let session = store.update(cx, |store, cx| {
                crate::store::tests::insert_cold_session(
                    session_id,
                    solution_id.clone(),
                    agent_id.clone(),
                    Some(120_000),
                    Some(project.clone()),
                    store,
                    cx,
                )
            });

            let navigator = cx.new(|cx| crate::navigator::SolutionSessionsNavigator::for_test(cx));
            let view = cx.new(|cx| {
                crate::session_view::SolutionSessionView::for_test(
                    session_id,
                    session,
                    workspace_weak.clone(),
                    navigator.downgrade(),
                    window,
                    cx,
                )
            });
            (navigator, view)
        });

        vcx.update(|window, cx| {
            navigator_entity.update(cx, |nav, cx| {
                nav.start_compact_from_cold(session_id, view_entity.clone(), window, cx);
            });
        });

        vcx.update(|_window, cx| {
            view_entity.read_with(cx, |view, _| {
                let pending = view
                    .pending_send_for_test()
                    .expect("pending_send populated after start_compact_from_cold");
                assert_eq!(pending.len(), 1, "exactly one content block");
                let agent_client_protocol::schema::ContentBlock::Text(text) = &pending[0] else {
                    panic!("expected text block, got {:?}", pending[0]);
                };
                assert!(
                    !text.text.contains("{{compact_dir}}"),
                    "template variable {{{{compact_dir}}}} must be resolved; got: {:?}",
                    &text.text[..text.text.len().min(200)]
                );
                assert!(
                    text.text.contains(session_id.as_str()),
                    "rendered prompt must contain session_id={session_id}",
                );
                assert!(view.is_resuming(), "resuming flag set after enqueue");
            });
        });
    }
}
