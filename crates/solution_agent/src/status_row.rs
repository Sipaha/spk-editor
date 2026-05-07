//! Status footer rendered below the session tab strip: token meter, model name, mode, state badge, history popover.

use gpui::{Animation, AnimationExt, ElementId, pulsating_between};
use gpui::{
    AppContext as _, Context, Entity, IntoElement, MouseButton, ParentElement, SharedString,
    StatefulInteractiveElement, Styled, div, px,
};
use ui::prelude::*;
use ui::{CommonAnimationExt, ContextMenu, Icon, IconName, Label, LabelSize, PopoverMenu};
use util::ResultExt as _;

use crate::compact::{
    COMPACT_BUTTON_MIN_PCT, COMPACT_BUTTON_WARN_PCT, COMPACT_HEADROOM_MIN_TOKENS,
};
use crate::model::{SessionState, SolutionSessionMetadata};
use crate::navigator::SolutionSessionsNavigator;
use crate::session_view::SolutionSessionView;
use crate::store::SolutionAgentStore;

impl SolutionSessionsNavigator {
    /// History popover trigger (clock icon). Lists the last 12 persisted
    /// sessions for the active solution; clicking a row resumes that
    /// session through `SolutionAgentStore::resume_session`.
    ///
    /// Hidden when there's nothing in the DB yet — no point rendering an
    /// always-empty popover.
    pub(crate) fn render_history_button(&self, cx: &mut Context<Self>) -> Option<gpui::AnyElement> {
        if self.active_solution.is_none() || self.historic_sessions.is_empty() {
            return None;
        }
        let metas: Vec<SolutionSessionMetadata> =
            self.historic_sessions.iter().take(12).cloned().collect();
        // Match the `+` new-session trigger's sizing.
        let trigger = ui::IconButton::new("solution-sessions-history", IconName::HistoryRerun)
            .shape(ui::IconButtonShape::Wide)
            .size(ButtonSize::Large)
            .icon_size(IconSize::Custom(rems_from_px(20.)))
            .icon_color(Color::Muted)
            .tooltip(ui::Tooltip::text("Recent sessions"));
        let weak = cx.entity().downgrade();
        Some(
            PopoverMenu::new("solution-sessions-history-popover")
                .trigger(trigger)
                .menu(move |window, cx| {
                    let metas = metas.clone();
                    let weak = weak.clone();
                    Some(ContextMenu::build(window, cx, move |mut menu, _, _| {
                        for meta in metas {
                            let weak = weak.clone();
                            let meta_for_action = meta.clone();
                            // Compose: "<preview-or-title>  ·  <time>  ·  <Ntok>"
                            // Preview takes precedence over the placeholder
                            // "Session <uuid>" title because identical titles
                            // are exactly the case the user wanted to fix.
                            // Truncate the primary at ~60 chars so a long
                            // first-prompt doesn't push the popover wide
                            // enough to overflow the navigator into the
                            // project panel — ContextMenu doesn't expose a
                            // width API, the only knob is the label string.
                            let primary_full = meta
                                .preview
                                .as_deref()
                                .filter(|s| !s.is_empty())
                                .unwrap_or(meta.title.as_ref());
                            let primary = truncate_history_label(primary_full, 60);
                            let mut label = format!(
                                "{}  ·  {}",
                                primary,
                                relative_time_short(meta.last_activity_at, chrono::Utc::now()),
                            );
                            if let Some(tokens) = meta.total_tokens {
                                label.push_str(&format!("  ·  {}", format_tokens(tokens)));
                            }
                            menu =
                                menu.entry(SharedString::from(label), None, move |window, cx| {
                                    if let Some(this) = weak.upgrade() {
                                        let meta = meta_for_action.clone();
                                        this.update(cx, |this, cx| {
                                            this.resume_and_open(meta, window, cx);
                                        });
                                    }
                                });
                        }
                        menu
                    }))
                })
                .anchor(gpui::Anchor::TopRight)
                .into_any_element(),
        )
    }

    /// Clickable card for the empty-state "Recent sessions" list. Two-line
    /// layout: preview as the visual anchor (Default size, truncated), then
    /// "<time ago>  ·  <Ntok>" as a muted Small subline. Each card resumes
    /// its session on left-click via `resume_and_open`.
    pub(crate) fn render_history_card(
        &self,
        meta: SolutionSessionMetadata,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let primary = meta
            .preview
            .as_deref()
            .filter(|s| !s.is_empty())
            .unwrap_or(meta.title.as_ref())
            .to_string();
        let activity = relative_time_short(meta.last_activity_at, chrono::Utc::now());
        let mut subline = activity;
        if let Some(tokens) = meta.total_tokens {
            subline.push_str(&format!("  ·  {}", format_tokens(tokens)));
        }
        let id = SharedString::from(format!("history-card-{}", meta.id));
        let meta_for_action = meta.clone();
        let session_id_for_delete = meta.id;
        let title_for_delete = meta.title;
        div()
            .id(id)
            .flex()
            .items_center()
            .gap_3()
            .px_3()
            .py_2()
            .w_full()
            .rounded_md()
            .border_1()
            .border_color(cx.theme().colors().border_variant)
            .bg(cx.theme().colors().elevated_surface_background)
            .hover(|s| s.bg(cx.theme().colors().element_hover))
            .cursor_pointer()
            .child(
                Icon::new(IconName::HistoryRerun)
                    .size(IconSize::Small)
                    .color(Color::Muted),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w_0()
                    .gap_0p5()
                    .child(Label::new(primary).size(LabelSize::Default).truncate())
                    .child(
                        Label::new(subline)
                            .color(Color::Muted)
                            .size(LabelSize::Small),
                    ),
            )
            .child(
                // Trash button — removes the persisted metadata + blob
                // for this session. Used as the escape hatch when a
                // session can't be resumed (agent storage was wiped,
                // or the session was never flushed past its first
                // turn) and the user wants to clean it out of History.
                // Stops propagation so clicking the icon doesn't also
                // fire the row's "resume" mouse-down.
                ui::IconButton::new(
                    SharedString::from(format!("history-card-delete-{}", session_id_for_delete)),
                    IconName::Trash,
                )
                .icon_size(IconSize::Small)
                .icon_color(Color::Muted)
                .tooltip(ui::Tooltip::text("Delete from history"))
                .on_click(cx.listener(move |this, _, _window, cx| {
                    let title = title_for_delete.clone();
                    let store = SolutionAgentStore::global(cx);
                    let Some(db) = store.read_with(cx, |s, _| s.db()) else {
                        return;
                    };
                    cx.background_spawn(async move {
                        if let Err(err) = db.delete(session_id_for_delete).await {
                            log::error!("history delete failed for {title:?}: {err:?}");
                        }
                    })
                    .detach();
                    this.refresh_historic_sessions(cx);
                })),
            )
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _, window, cx| {
                    let meta = meta_for_action.clone();
                    this.resume_and_open(meta, window, cx);
                }),
            )
    }

    /// Resolves the agent's currently-selected model name asynchronously
    /// and stores it in `cached_models`. The status row reads this cache
    /// on subsequent renders. We dedupe in-flight fetches via
    /// `pending_model_fetches` so the row doesn't fire a fresh request
    /// every frame.
    fn ensure_model_loaded(
        &mut self,
        session_id: crate::model::SolutionSessionId,
        cx: &mut Context<Self>,
    ) {
        if self.cached_models.contains_key(&session_id)
            || self.pending_model_fetches.contains(&session_id)
        {
            return;
        }
        let store = SolutionAgentStore::global(cx);
        let Some(thread) = store
            .read(cx)
            .session(session_id)
            .and_then(|s| s.read(cx).acp_thread.clone())
        else {
            return;
        };
        let acp_session_id = thread.read(cx).session_id().clone();
        let connection = thread.read(cx).connection().clone();
        let Some(selector) = connection.model_selector(&acp_session_id) else {
            return;
        };
        let task = selector.selected_model(cx);
        self.pending_model_fetches.insert(session_id);
        cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update(cx, |this, cx| {
                this.pending_model_fetches.remove(&session_id);
                if let Ok(info) = result {
                    this.cached_models.insert(session_id, info.name);
                    cx.notify();
                }
            })
            .log_err();
        })
        .detach();
    }

    /// Spawn a background tick that wakes the navigator once a second
    /// for as long as any open session sits in `Running`. Drives the
    /// "Thinking… Ns" counter in the status row without depending on
    /// AcpThreadEvent firing during quiet pauses. Idempotent: a second
    /// call while `thinking_tick` is already `Some` is a no-op.
    fn ensure_thinking_tick(&mut self, cx: &mut Context<Self>) {
        if self.thinking_tick.is_some() {
            return;
        }
        self.thinking_tick = Some(cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(std::time::Duration::from_secs(1))
                    .await;
                let still_running = this
                    .update(cx, |this, cx| {
                        let store = SolutionAgentStore::global(cx);
                        let active_session_running = this
                            .selected_index
                            .and_then(|idx| this.open_sessions.get(idx).copied())
                            .and_then(|sid| store.read(cx).session(sid))
                            .map(|s| matches!(s.read(cx).state, SessionState::Running { .. }))
                            .unwrap_or(false);
                        if active_session_running {
                            cx.notify();
                        }
                        active_session_running
                    })
                    .ok()
                    .unwrap_or(false);
                if !still_running {
                    break;
                }
            }
            // Self-cleanup so the next Running flip starts a fresh
            // tick instead of relying on the next render to reset the
            // slot.
            let _ = this.update(cx, |this, _| {
                this.thinking_tick = None;
            });
        }));
    }

    pub(crate) fn render_status_row(
        &mut self,
        active_view: Option<&Entity<SolutionSessionView>>,
        is_resuming: bool,
        cx: &mut Context<Self>,
    ) -> Option<gpui::AnyElement> {
        let session_id = self
            .selected_index
            .and_then(|i| self.open_sessions.get(i).copied())?;
        let session = active_view.and_then(|v| {
            let _ = v;
            SolutionAgentStore::global(cx).read_with(cx, |s, _| s.session(session_id))
        })?;
        let s = session.read(cx);
        let agent_id = s.agent_id.clone();
        // Working-directory label: the project name the user picked when
        // creating this session ("Solution root" → "ROOT", member project
        // → catalog name). Lookup needs the live `Solution` so we can
        // compare against `solution.root` and resolve member catalog ids.
        let cwd_label: SharedString = solutions::SolutionStore::try_global(cx)
            .and_then(|store| {
                store.read_with(cx, |store, _| {
                    store
                        .solutions()
                        .iter()
                        .find(|sol| sol.id == s.solution_id)
                        .cloned()
                })
            })
            .and_then(|solution| crate::store::project_name_for_cwd(&solution, &s.cwd, cx))
            .unwrap_or_else(|| SharedString::from("ROOT"));
        // For most states the short label ("Idle", "Running", …) is
        // the right thing to show. For `Errored(msg)` we surface the
        // full message inline so the user actually learns *what* went
        // wrong (e.g. "You've hit your limit · resets 2:10pm") instead
        // of just seeing "Error" with no follow-up. The tooltip carries
        // the same text so very long errors that get truncated by
        // flexbox can still be read in full on hover.
        // Cold (no live `AcpThread`) and resuming (cold-tab Send
        // handshake in flight) are session-level conditions that
        // don't fit into `SessionState`'s "agent activity" axis —
        // surface them as override states for the badge so the user
        // doesn't see a misleading bare "Idle" while the subprocess
        // is dead-asleep or mid-handshake.
        let is_cold = s.is_cold();
        // `is_resuming` is precomputed by the caller (`SolutionSessionView`)
        // and passed in. Reading it from `active_view` here would double-lease
        // the view: this method runs inside `nav.update(...)` invoked from
        // *within* `SolutionSessionView::render`, so the view entity is
        // already leased by GPUI's renderer.
        let (state_text, error_text): (SharedString, Option<SharedString>) = if is_resuming {
            (SharedString::from("Resuming…"), None)
        } else if is_cold {
            // The session was restored from disk and the subprocess
            // hasn't been spawned yet. Tooltip-less label is fine —
            // the meaning is glanceable (Sleeping = inactive, send
            // a message to wake it up).
            (SharedString::from("Sleeping"), None)
        } else {
            match &s.state {
                SessionState::Errored(msg) => (
                    SharedString::from(format!("Error: {msg}")),
                    Some(msg.clone()),
                ),
                SessionState::Running { started_at, .. } => {
                    let elapsed = started_at.elapsed().as_secs();
                    let label = if elapsed >= 1 {
                        format!("Thinking… {}", format_elapsed(elapsed))
                    } else {
                        "Thinking…".to_string()
                    };
                    (SharedString::from(label), None)
                }
                // "Done in Xs" replaces a bare "Idle" right after a turn
                // completes so a foreground user gets an explicit "the
                // agent finished" cue (the desktop notification path is
                // gated to unfocused panels and ≥5min turns, so without
                // this an in-foreground watcher only sees "Thinking…"
                // disappear). Cleared on the next Running transition.
                SessionState::Idle if s.last_turn_duration.is_some() => {
                    let secs = s.last_turn_duration.map(|d| d.as_secs()).unwrap_or(0);
                    let label = if secs >= 1 {
                        format!("Done in {}", format_elapsed(secs))
                    } else {
                        "Done".to_string()
                    };
                    (SharedString::from(label), None)
                }
                other => (SharedString::from(other.short_label()), None),
            }
        };
        let is_idle = matches!(s.state, SessionState::Idle) && !is_cold && !is_resuming;
        let is_running = matches!(s.state, SessionState::Running { .. }) && !is_resuming;
        let is_errored = matches!(s.state, SessionState::Errored(_));
        // Live thread → live `TokenUsage`; cold / sleeping → fall
        // back to the `cached_total_tokens` mirrored from metadata
        // at restore time + refreshed on every live
        // `TokenUsageUpdated`. Without this fallback the meter showed
        // "0 / 1.0M · 0.0%" for any restored conversation, which
        // looked like the editor lost the agent's context.
        let usage = s
            .acp_thread
            .as_ref()
            .and_then(|thread| thread.read(cx).token_usage().cloned())
            .or_else(|| {
                s.cached_total_tokens.map(|used| acp_thread::TokenUsage {
                    used_tokens: used,
                    ..Default::default()
                })
            });
        // Synchronous read of the agent's current session mode
        // ("default", "plan", …). Claude exposes this via ACP — when
        // the connection doesn't implement modes (e.g. mock test
        // adapter) we just hide the segment.
        let mode_text: Option<SharedString> = s.acp_thread.as_ref().and_then(|thread| {
            let thread = thread.read(cx);
            let modes = thread.connection().session_modes(thread.session_id(), cx)?;
            let current = modes.current_mode();
            modes
                .all_modes()
                .into_iter()
                .find(|m| m.id == current)
                .map(|m| SharedString::from(m.name))
                .or_else(|| Some(SharedString::from(current.0.to_string())))
        });
        let _ = s;
        // While the active session is in `Running`, drive a 1 Hz tick
        // so the elapsed counter ("Thinking… Ns") in `state_text`
        // advances even when no AcpThreadEvents fire (long pauses
        // between tool calls, etc.). Idempotent — the spawn happens
        // only on the first render that observes Running, and the
        // task self-cancels by checking `still_running` each tick.
        if is_running {
            self.ensure_thinking_tick(cx);
        } else if self.thinking_tick.is_some() {
            self.thinking_tick = None;
        }
        // Kick off a model lookup if we don't have one cached yet.
        // Stored in `cached_models` for synchronous reads on later
        // frames; the spawn de-dupes via `pending_model_fetches`.
        self.ensure_model_loaded(session_id, cx);
        let model_text = self.cached_models.get(&session_id).cloned();

        let used = usage.as_ref().map(|u| u.used_tokens).unwrap_or(0);
        // claude-acp doesn't always populate `max_tokens` (it's gated by an
        // upstream beta flag). Fall back to the Claude Opus 4 context
        // window so the meter and the compact button stay meaningful.
        let max = usage
            .as_ref()
            .map(|u| u.max_tokens)
            .filter(|m| *m > 0)
            .unwrap_or(DEFAULT_CONTEXT_WINDOW);
        let pct = if max == 0 {
            0.0
        } else {
            (used as f64 / max as f64).clamp(0.0, 1.0)
        };
        let meter_text = SharedString::from(format!(
            "{} / {} · {:.1}%",
            format_tokens_compact(used),
            format_tokens_compact(max),
            pct * 100.0
        ));
        let bar_color = if pct >= 0.8 {
            cx.theme().status().error
        } else if pct >= 0.5 {
            cx.theme().status().warning
        } else {
            cx.theme().colors().text_accent
        };

        // The compact prompt + the agent's dump need real headroom (~3k
        // for the prompt, ~10–20k for state.md / decisions.md / next.md
        // / continue.md combined). A percentage gate misbehaves across
        // model sizes — 10 % of a 200 k window is only 20 k tokens
        // (tight) while 10 % of a 1 M window is 100 k (more than
        // enough). Tie the disable threshold to absolute remaining
        // tokens instead so the button stays usable on long-context
        // models even past 90 %.
        let remaining = max.saturating_sub(used);
        let too_full = remaining < COMPACT_HEADROOM_MIN_TOKENS;
        let pct_ok = pct >= COMPACT_BUTTON_MIN_PCT;
        let compact_enabled = (is_idle || is_cold) && !is_errored && pct_ok && !too_full;
        let clear_enabled = !is_running && !is_resuming;
        let trigger_enabled = compact_enabled || clear_enabled;

        let compact_warning = pct >= COMPACT_BUTTON_WARN_PCT && !too_full;
        let compact_tooltip: SharedString = if is_running || is_resuming {
            "Wait for the current turn to finish before compacting".into()
        } else if is_errored {
            "Compact unavailable while the session is in error".into()
        } else if too_full {
            format!(
                "Only {} of headroom left — start a fresh session manually",
                format_tokens(remaining)
            )
            .into()
        } else if pct < COMPACT_BUTTON_MIN_PCT {
            "Conversation is short — compact later".into()
        } else if compact_warning {
            "Context is filling up — compact recommended".into()
        } else if is_cold {
            "Compact context: wake the session, dump a summary, then continue in a fresh context"
                .into()
        } else {
            "Compact context: agent dumps a summary, then a fresh session continues".into()
        };

        let clear_tooltip: SharedString = if !clear_enabled {
            "Wait for the current turn to finish before clearing".into()
        } else {
            "Clear context: wipe the conversation, keep the tab".into()
        };

        let trigger_tooltip: SharedString = if !trigger_enabled {
            "Wait for the current turn to finish before cleaning up context".into()
        } else {
            "Compact or clear the session's context".into()
        };

        let trigger_color = if compact_warning {
            Color::Warning
        } else {
            Color::Muted
        };

        let cleanup_button: gpui::AnyElement = if !trigger_enabled {
            ui::IconButton::new("solution-status-cleanup", IconName::Eraser)
                .icon_size(IconSize::Small)
                .icon_color(trigger_color)
                .disabled(true)
                .tooltip(ui::Tooltip::text(trigger_tooltip))
                .into_any_element()
        } else {
            let trigger = ui::IconButton::new("solution-status-cleanup", IconName::Eraser)
                .icon_size(IconSize::Small)
                .icon_color(trigger_color)
                .tooltip(ui::Tooltip::text(trigger_tooltip));
            let weak_nav = cx.entity().downgrade();
            let weak_view = active_view.map(|v| v.downgrade());
            PopoverMenu::new("solution-status-cleanup-menu")
                .trigger(trigger)
                .menu(move |window, cx| {
                    let weak_nav = weak_nav.clone();
                    let weak_view = weak_view.clone();
                    let compact_tooltip = compact_tooltip.clone();
                    let clear_tooltip = clear_tooltip.clone();
                    Some(ContextMenu::build(window, cx, move |mut menu, _, _| {
                        let compact_label: SharedString = if compact_enabled {
                            "Compact context".into()
                        } else {
                            format!("Compact context — {compact_tooltip}").into()
                        };
                        let compact_entry = ui::ContextMenuEntry::new(compact_label)
                            .icon(IconName::Archive)
                            .icon_color(Color::Muted)
                            .disabled(!compact_enabled)
                            .handler({
                                let weak_nav = weak_nav.clone();
                                let weak_view = weak_view.clone();
                                move |window, cx| {
                                    let Some(nav) = weak_nav.upgrade() else { return };
                                    nav.update(cx, |nav, cx| {
                                        if is_cold {
                                            let Some(view) = weak_view
                                                .as_ref()
                                                .and_then(|w| w.upgrade())
                                            else {
                                                return;
                                            };
                                            nav.start_compact_from_cold(
                                                session_id, view, window, cx,
                                            );
                                        } else {
                                            nav.start_compact(session_id, cx);
                                        }
                                    });
                                }
                            });
                        menu = menu.item(compact_entry);

                        let clear_label: SharedString = if clear_enabled {
                            "Clear context".into()
                        } else {
                            format!("Clear context — {clear_tooltip}").into()
                        };
                        let clear_entry = ui::ContextMenuEntry::new(clear_label)
                            .icon(IconName::Eraser)
                            .icon_color(Color::Muted)
                            .disabled(!clear_enabled)
                            .handler({
                                let weak_nav = weak_nav.clone();
                                move |window, cx| {
                                    let prompt = window.prompt(
                                        gpui::PromptLevel::Warning,
                                        "Clear conversation?",
                                        Some(
                                            "The current conversation will be removed from the \
                                             session. The agent's history is preserved on disk \
                                             but cannot be restored through History.",
                                        ),
                                        &["Cancel", "Clear"],
                                        cx,
                                    );
                                    let weak_nav = weak_nav.clone();
                                    window
                                        .spawn(cx, async move |cx| {
                                            // Button index 1 = Clear; 0 / Esc cancels.
                                            if prompt.await.ok() != Some(1) {
                                                return;
                                            }
                                            cx.update(|_, cx| {
                                                if let Some(nav) = weak_nav.upgrade() {
                                                    nav.update(cx, |_, cx| {
                                                        SolutionAgentStore::global(cx)
                                                            .update(cx, |store, cx| {
                                                                store
                                                                    .reset_context(session_id, cx)
                                                                    .detach_and_log_err(cx);
                                                            });
                                                    });
                                                }
                                            })
                                            .log_err();
                                        })
                                        .detach();
                                }
                            });
                        menu = menu.item(clear_entry);

                        menu
                    }))
                })
                .anchor(gpui::Anchor::TopRight)
                .into_any_element()
        };

        // State badge ("Thinking… 3m05s" / "Done in 12s" / "Error: …")
        // anchors the LEFT of the row — that's where the user's eye
        // lands first, and the row sits directly above the compose
        // box, so the active-state cue is glanceable while typing the
        // next message. The meter / agent / model / cwd / mode group
        // on the right is reference info for the current session, not
        // status — putting them after the badge gives a clean
        // "what's happening" → "where it's running" reading order.
        let state_badge: gpui::AnyElement = {
            let mut label = Label::new(state_text).size(LabelSize::Small);
            if error_text.is_some() {
                label = label.color(Color::Error);
            } else if is_running || is_resuming {
                label = label.color(Color::Accent);
            } else if is_cold {
                label = label.color(Color::Muted);
            }
            let inner: gpui::AnyElement = if is_running {
                let icon = div()
                    .flex_none()
                    .child(
                        ui::Icon::new(IconName::Sparkle)
                            .size(IconSize::Small)
                            .color(Color::Accent),
                    )
                    .with_animation(
                        ElementId::Name("solution-status-thinking-pulse".into()),
                        Animation::new(std::time::Duration::from_secs(1))
                            .repeat()
                            .with_easing(pulsating_between(0.4, 1.0)),
                        |element: gpui::Div, delta| element.opacity(delta),
                    );
                div()
                    .flex()
                    .items_center()
                    .gap_1()
                    .child(icon)
                    .child(label)
                    .into_any_element()
            } else if is_resuming {
                // Rotating ⟳ next to "Resuming…" — same visual
                // vocabulary the in-flight session-creation tabs use,
                // so the user reads it instantly as "agent is
                // attaching, hold on".
                let icon = ui::Icon::new(IconName::ArrowCircle)
                    .size(IconSize::Small)
                    .color(Color::Accent)
                    .with_rotate_animation(2);
                div()
                    .flex()
                    .items_center()
                    .gap_1()
                    .child(icon)
                    .child(label)
                    .into_any_element()
            } else {
                label.into_any_element()
            };
            match error_text {
                Some(full) => div()
                    .id("solution-status-error-text")
                    .tooltip(ui::Tooltip::text(full))
                    .child(inner)
                    .into_any_element(),
                None => inner,
            }
        };

        // `border_t_1()` (top border): the row now sits between the
        // conversation list and the compose box, so the separator we
        // want is at the TOP — without it the row blends into the
        // conversation. The previous `border_b_1()` made sense when
        // the row lived directly under the tab strip, but in the new
        // position a bottom border lands inside the editor_background
        // strip of the compose wrapper and is invisible.
        Some(
            div()
                .flex()
                .items_center()
                .gap_2()
                .px_3()
                .h_7()
                .border_t_1()
                .border_color(cx.theme().colors().border_variant)
                .child(div().flex_none().child(state_badge))
                .child(Label::new("·").color(Color::Muted).size(LabelSize::Small))
                .child(
                    div().flex_none().child(
                        Label::new(meter_text)
                            .size(LabelSize::Small)
                            .color(Color::Muted),
                    ),
                )
                .child(
                    div()
                        .flex_none()
                        .w(px(72.0))
                        .h(px(4.0))
                        .rounded_full()
                        .bg(cx.theme().colors().border)
                        .child(
                            div()
                                .h_full()
                                .w(relative((pct as f32).clamp(0.0, 1.0)))
                                .rounded_full()
                                .bg(bar_color),
                        ),
                )
                .child(div().flex_none().child(cleanup_button))
                .child(
                    Label::new(agent_id)
                        .color(Color::Muted)
                        .size(LabelSize::Small),
                )
                .child(Label::new("·").color(Color::Muted).size(LabelSize::Small))
                .child(
                    Label::new(cwd_label)
                        .color(Color::Muted)
                        .size(LabelSize::Small),
                )
                .when_some(model_text, |this, model| {
                    this.child(Label::new("·").color(Color::Muted).size(LabelSize::Small))
                        .child(Label::new(model).color(Color::Muted).size(LabelSize::Small))
                })
                .when_some(mode_text, |this, mode| {
                    this.child(Label::new("·").color(Color::Muted).size(LabelSize::Small))
                        .child(Label::new(mode).color(Color::Muted).size(LabelSize::Small))
                })
                .into_any_element(),
        )
    }
}

/// Hardcoded fallback when claude-acp doesn't advertise the model's
/// context-window size (the field is gated by an upstream beta flag).
/// 1M matches Claude Opus 4 with the long-context flag enabled, which
/// is the default for this fork.
pub(crate) const DEFAULT_CONTEXT_WINDOW: u64 = 1_000_000;

/// Char-count truncation with ellipsis for History-popover entry labels.
/// Operates on `chars()` (not bytes) so it never splits a multibyte
/// codepoint. Returns the input unchanged when shorter than `max_chars`.
fn truncate_history_label(text: &str, max_chars: usize) -> String {
    let mut iter = text.chars();
    let head: String = iter.by_ref().take(max_chars).collect();
    if iter.next().is_none() {
        head
    } else {
        format!("{head}…")
    }
}

/// "Thinking… 7s" / "Thinking… 1m32s" / "Thinking… 1h05m" — granularity
/// shifts up as the turn drags on so a 40-minute thought doesn't render
/// as "Thinking… 2412s" (mentally divide-by-60 every render). Hours +
/// minutes only past the hour mark; seconds drop off there because
/// minute-precision is enough at that scale and the extra digits just
/// added jitter without information.
fn format_elapsed(secs: u64) -> String {
    if secs < 60 {
        format!("{secs}s")
    } else if secs < 3600 {
        format!("{}m{:02}s", secs / 60, secs % 60)
    } else {
        format!("{}h{:02}m", secs / 3600, (secs % 3600) / 60)
    }
}

/// Compact token count, "12.3k tok" / "456 tok", for the History popover.
fn format_tokens(tokens: u64) -> String {
    if tokens >= 1_000_000 {
        format!("{:.1}M tok", tokens as f64 / 1_000_000.0)
    } else if tokens >= 1_000 {
        format!("{:.1}k tok", tokens as f64 / 1_000.0)
    } else {
        format!("{} tok", tokens)
    }
}

/// Short token count, "12.3k" / "456", with no unit suffix. Used in the
/// status row where the magnitudes of the two operands ("used / max")
/// already make their meaning unambiguous.
fn format_tokens_compact(tokens: u64) -> String {
    if tokens >= 1_000_000 {
        format!("{:.1}M", tokens as f64 / 1_000_000.0)
    } else if tokens >= 1_000 {
        format!("{:.1}k", tokens as f64 / 1_000.0)
    } else {
        tokens.to_string()
    }
}

/// Compact "X ago" formatter mirroring `solutions_ui::welcome::relative_time_label`
/// but kept local to avoid a fork-internal cross-crate dep cycle.
fn relative_time_short(
    ts: chrono::DateTime<chrono::Utc>,
    now: chrono::DateTime<chrono::Utc>,
) -> String {
    let secs = now.signed_duration_since(ts).num_seconds();
    if secs < 60 {
        "just now".into()
    } else if secs < 3600 {
        format!("{}m ago", secs / 60)
    } else if secs < 86_400 {
        format!("{}h ago", secs / 3600)
    } else if secs < 7 * 86_400 {
        format!("{}d ago", secs / 86_400)
    } else if secs < 30 * 86_400 {
        format!("{}w ago", secs / (7 * 86_400))
    } else if secs < 365 * 86_400 {
        format!("{}mo ago", secs / (30 * 86_400))
    } else {
        format!("{}y ago", secs / (365 * 86_400))
    }
}
