//! "Compact context" workflow: dump the current session's running summary to handoff files, then continue in a fresh ACP session.

use gpui::{AppContext as _, Context, SharedString};
use solutions::SolutionStore;
use workspace::notifications::{NotificationId, simple_message_notification::MessageNotification};

use crate::model::SessionState;
use crate::navigator::SolutionSessionsNavigator;
use crate::status_row::DEFAULT_CONTEXT_WINDOW;
use crate::store::SolutionAgentStore;

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
        let store = SolutionAgentStore::global(cx);
        let Some(session_entity) = store.read_with(cx, |s, _| s.session(session_id)) else {
            return;
        };
        let s = session_entity.read(cx);
        if !matches!(s.state, SessionState::Idle) {
            return;
        }
        let solution_id = s.solution_id.clone();
        let agent_id = s.agent_id.clone();
        let started_at = s.created_at;
        // Snapshot the count *before* rotation: the dump dir captures
        // the context being closed (`c01` for the first compact, `c02`
        // for the second, …). After the agent finishes writing files
        // and `compact_session` runs, the session's context_count
        // increments to count + 1 for the next round.
        let context_count = s.context_count;
        let usage = s
            .acp_thread
            .as_ref()
            .and_then(|thread| thread.read(cx).token_usage().cloned());
        let used = usage.as_ref().map(|u| u.used_tokens).unwrap_or(0);
        let max = usage
            .as_ref()
            .map(|u| u.max_tokens)
            .filter(|m| *m > 0)
            .unwrap_or(DEFAULT_CONTEXT_WINDOW);
        let _ = s;

        let solution_root = match SolutionStore::try_global(cx).and_then(|store| {
            store.read_with(cx, |s, _| {
                s.solutions()
                    .iter()
                    .find(|sol| sol.id == solution_id)
                    .map(|sol| sol.root.clone())
            })
        }) {
            Some(root) => root,
            None => {
                self.toast_error(
                    SharedString::from(format!(
                        "Compact failed: solution {:?} not registered",
                        solution_id.0
                    )),
                    cx,
                );
                return;
            }
        };

        // `<root>/.agents/<sid>/c<count>/` — `c01`, `c02`, … so a
        // single `<sid>` directory groups every rotation of one
        // logical conversation. The leading `c` keeps the names from
        // accidentally colliding with the legacy timestamp scheme.
        let context_label = format!("c{context_count:02}");
        let compact_dir = solution_root
            .join(".agents")
            .join(session_id.to_string())
            .join(&context_label);
        if let Err(err) = std::fs::create_dir_all(&compact_dir) {
            self.toast_error(
                SharedString::from(format!(
                    "Compact failed: cannot create {}: {err}",
                    compact_dir.display()
                )),
                cx,
            );
            return;
        }

        let mut compact_dir_str = compact_dir.to_string_lossy().to_string();
        if !compact_dir_str.ends_with(std::path::MAIN_SEPARATOR) {
            compact_dir_str.push(std::path::MAIN_SEPARATOR);
        }

        let rendered = COMPACT_INSTRUCTIONS_TEMPLATE
            .replace("{{session_id}}", &session_id.to_string())
            .replace("{{compact_dir}}", &compact_dir_str)
            .replace("{{solution_id}}", solution_id.0.as_str())
            .replace("{{agent_id}}", agent_id.as_ref())
            .replace("{{started_at_iso}}", &started_at.to_rfc3339())
            .replace("{{tokens_used}}", &used.to_string())
            .replace("{{tokens_max}}", &max.to_string());

        store.update(cx, |store, cx| {
            store
                .send_message(session_id, rendered, cx)
                .detach_and_log_err(cx);
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
