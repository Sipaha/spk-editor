use std::collections::HashMap;
use std::ops::Range;
use std::rc::Rc;

use acp_thread::{
    AcpThread, AgentThreadEntry, AssistantMessage, AssistantMessageChunk, ContentBlock, PlanEntry,
    ToolCall, ToolCallContent, ToolCallStatus, UserMessage, UserMessageId,
};
use agent_client_protocol::schema as acp;
use anyhow::Result as AnyhowResult;
use base64::Engine;
use editor::{CompletionContext, CompletionProvider as EditorCompletionProvider};
use gpui::{
    AnyElement, App, ClipboardEntry, Context, DragMoveEvent, Empty, Entity, EntityId,
    EventEmitter, ExternalPaths, FocusHandle, Focusable, InteractiveElement as _, IntoElement,
    MouseButton, MouseDownEvent, ParentElement, Pixels, Render, ScrollDelta, ScrollWheelEvent,
    SharedString, Styled, StatefulInteractiveElement as _, Subscription, Task, WeakEntity, Window,
    div, px, relative,
};
use language::{Anchor, Buffer, CodeLabel, Point, ToPoint};
use markdown::{Markdown, MarkdownElement, MarkdownFont, MarkdownStyle};
use project::{
    Completion, CompletionDisplayOptions, CompletionResponse, CompletionSource,
    lsp_store::CompletionDocumentation,
};
use ui::prelude::*;
use ui::{ContextMenu, CopyButton, IconButton, IconName, Label, Tooltip, WithScrollbar, right_click_menu};
use workspace::{
    Workspace,
    notifications::{NotificationId, simple_message_notification::MessageNotification},
};

use crate::actions::{FindClose, FindInSession, FindNextMatch, FindPreviousMatch, StopResponse};
use crate::model::{SessionState, SolutionSession, SolutionSessionId};
use crate::store::SolutionAgentStore;

struct PendingImage {
    mime_type: String,
    data_base64: String,
    label: SharedString,
}

#[derive(Clone, Debug)]
struct FindMatch {
    entry_idx: usize,
    span_idx: usize,
    range: Range<usize>,
}

struct FindState {
    editor: Entity<editor::Editor>,
    matches: Vec<FindMatch>,
    selected: Option<usize>,
    _subscription: Subscription,
}

/// Marker payload for the compose-row resize drag. GPUI's drag-drop system
/// requires a `Render`-able entity to track the in-flight drag; since the
/// resize is purely state-mutating (no visible drag preview) we render
/// nothing and let the parent's `on_drag_move` handler do all the work.
#[derive(Clone)]
struct DraggedComposeHandle;

impl Render for DraggedComposeHandle {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        Empty
    }
}

/// Cached `Markdown` entity + the source string we last fed it. Without
/// caching, each render frame would create a new entity (which schedules
/// an async parse) and immediately drop the previous one — content would
/// either flicker or never finish parsing on a static conversation.
struct CachedMarkdown {
    entity: Entity<Markdown>,
    source: SharedString,
}

/// Default compose-row height in logical pixels. Matches the previous
/// hard-coded `h_24` (24 * 4px) so existing users see no visual jump.
const DEFAULT_COMPOSE_HEIGHT: f32 = 96.0;
/// Lower bound — leave enough room for one editor line + Send button.
const MIN_COMPOSE_HEIGHT: f32 = 56.0;
/// Upper bound — past this the conversation starts feeling cramped on
/// reasonable bottom-dock heights.
const MAX_COMPOSE_HEIGHT: f32 = 400.0;

pub struct SolutionSessionView {
    session_id: SolutionSessionId,
    session: Entity<SolutionSession>,
    focus_handle: FocusHandle,
    workspace: WeakEntity<Workspace>,
    compose_editor: Entity<editor::Editor>,
    pending_images: Vec<PendingImage>,
    find: Option<FindState>,
    /// User-controlled compose-row height (resize handle drag).
    compose_height: Pixels,
    /// Captured at mouse-down on the resize handle so `on_drag_move` can
    /// compute the new height as `start_height + (start_y - current_y)`.
    /// Inverted Y: dragging UP grows the compose row.
    resize_start_y: Pixels,
    resize_start_height: Pixels,
    /// `Markdown` entities reused across renders. Key is `(entry_idx,
    /// span_idx)` — same coords find_matches uses. Entries grow as the
    /// thread streams; we update an existing entity's source rather than
    /// recreating it so partial-parsed content keeps rendering smoothly.
    markdown_cache: HashMap<(usize, usize), CachedMarkdown>,
    /// Tracks the conversation body's scroll offset so we can both auto-
    /// scroll on new messages and detect when the user has manually
    /// scrolled away from the bottom.
    conversation_scroll: gpui::ScrollHandle,
    /// "Sticky to bottom" mode: when true, every render that observes new
    /// content snaps the conversation to the latest line; when false, the
    /// user has scrolled up and we leave their position alone (and render
    /// a "Jump to latest" affordance). Mouse-wheel and scroll-bar drags
    /// flip the flag; clicking the affordance flips it back.
    stuck_to_bottom: bool,
    /// Per-inner-terminal `cx.observe` subscriptions, keyed by the inner
    /// `terminal::Terminal` entity id. Streaming terminal output only
    /// notifies that inner entity (write_output → cx.notify on terminal),
    /// nothing higher up — so without this map our view would render the
    /// captured output only when an unrelated event (new assistant message,
    /// scroll) happened to retrigger render.
    terminal_observers: HashMap<EntityId, Subscription>,
    /// Handle to the detached "expanded compose" OS window if one is
    /// currently open. While open the inline compose row is replaced with
    /// a placeholder + Cancel button, and clicks on the placeholder
    /// re-activate the popup window. Cleared back to None whenever the
    /// popup closes (Save / Cancel / OS close button).
    expanded_window: Option<gpui::WindowHandle<ExpandedComposeWindowView>>,
}

impl SolutionSessionView {
    pub fn new(
        session_id: SolutionSessionId,
        session: Entity<SolutionSession>,
        workspace: WeakEntity<Workspace>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        cx.observe(&session, |this, _, cx| {
            // Thread mutated (new chunk streamed in, tool call appended, etc.).
            // Match indices stored in `find` reference (entry_idx, span_idx,
            // byte range) so a streaming append before/inside an existing
            // match can shift everything; recompute defensively while find is
            // open. Cheap for typical chats (<1000 entries × short query).
            if this.find.is_some() {
                this.recompute_matches(cx);
            }
            cx.notify();
        })
        .detach();
        let compose_editor = cx.new(|cx| {
            // `multi_line` (Full mode) instead of `auto_height` — the
            // editor fills its container vertically, so a click anywhere
            // inside the compose row hits the editor element directly,
            // and the editor handles focus + cursor placement uniformly
            // (no special-casing for "click below first line"). With
            // auto_height the editor was only as tall as content (1 line
            // for an empty draft) leaving an empty wrapper area below
            // that no widget owned — clicks there bounced.
            let mut e = editor::Editor::multi_line(window, cx);
            e.set_placeholder_text("Send a message…", window, cx);
            e.set_show_gutter(false, cx);
            e.set_show_line_numbers(false, cx);
            e.set_show_scrollbars(false, cx);
            // Disable current-line highlight — for a chat input it shows
            // up as a stripe across the whole editor under the cursor row,
            // visually splitting the compose area in half.
            e.set_current_line_highlight(Some(editor::CurrentLineHighlight::None));
            // Disable indent guides — irrelevant for prose, just adds
            // vertical lines that look broken in a one-line draft.
            e.set_show_indent_guides(false, cx);
            // Default `EditorMode::Full` reserves a full page of empty
            // overscroll under the last line so the cursor can stay
            // centred on long files. For a chat input that feels
            // broken: typing scrolls the text up while the bottom half
            // of the visible area is blank. Switch to the no-overscroll
            // sizing variant so the editor only scrolls when the text
            // genuinely outgrows the visible area.
            e.set_mode(editor::EditorMode::Full {
                scale_ui_elements_with_buffer_font_size: false,
                show_active_line_background: false,
                sizing_behavior: editor::SizingBehavior::ExcludeOverscrollMargin,
            });
            // Force the completions popup on every keystroke regardless of
            // user/language settings — the only completions this editor
            // ever surfaces are slash commands, and they should always
            // appear the moment the user types `/`.
            e.set_show_completions_on_input(Some(true));
            // Pin the popup above the cursor: the compose row sits at the
            // bottom of the chat panel, so the default "below" placement
            // immediately overflows the panel and clips. `Above` flips it
            // to grow upward into the conversation area where there's
            // always room.
            e.set_context_menu_options(editor::ContextMenuOptions {
                min_entries_visible: 4,
                max_entries_visible: 12,
                placement: Some(editor::ContextMenuPlacement::Above),
            });
            e.set_completion_provider(Some(Rc::new(SlashCommandsProvider {
                session: session.downgrade(),
            })));
            e
        });
        Self {
            session_id,
            session,
            focus_handle: cx.focus_handle(),
            workspace,
            compose_editor,
            pending_images: Vec::new(),
            find: None,
            compose_height: px(DEFAULT_COMPOSE_HEIGHT),
            resize_start_y: px(0.0),
            resize_start_height: px(DEFAULT_COMPOSE_HEIGHT),
            markdown_cache: HashMap::new(),
            conversation_scroll: gpui::ScrollHandle::new(),
            // Default to "follow latest" — chat panels are read-as-it-arrives
            // surfaces. The flag flips off the first time the user scrolls
            // away from the bottom (mouse wheel up, scrollbar drag, etc.).
            stuck_to_bottom: true,
            terminal_observers: HashMap::new(),
            expanded_window: None,
        }
    }

    fn ensure_markdown(
        &mut self,
        key: (usize, usize),
        source: SharedString,
        cx: &mut Context<Self>,
    ) -> Entity<Markdown> {
        if let Some(cached) = self.markdown_cache.get_mut(&key) {
            if cached.source != source {
                cached.entity.update(cx, |md, cx| md.replace(source.clone(), cx));
                cached.source = source;
            }
            return cached.entity.clone();
        }
        // Hand the project's LanguageRegistry to the markdown entity so
        // fenced code blocks (most importantly ```diff for tool-call
        // diff renders) get tree-sitter syntax highlighting — green for
        // `+`, red for `-`. Without it the markdown widget paints code
        // blocks plain monospace.
        let language_registry = self
            .session
            .read(cx)
            .acp_thread
            .as_ref()
            .map(|thread| thread.read(cx).project().read(cx).languages().clone());
        let entity = cx.new(|cx| Markdown::new(source.clone(), language_registry, None, cx));
        self.markdown_cache.insert(
            key,
            CachedMarkdown {
                entity: entity.clone(),
                source,
            },
        );
        entity
    }

    fn open_find(&mut self, _: &FindInSession, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(find) = self.find.as_ref() {
            // Already open — re-focus the input so a second Ctrl+F lands the
            // user back in the find bar after they've moved focus elsewhere
            // (e.g. clicked a tool-call body, then hit Ctrl+F again).
            let handle = find.editor.read(cx).focus_handle(cx);
            window.focus(&handle, cx);
            return;
        }
        let editor = cx.new(|cx| {
            let mut e = editor::Editor::single_line(window, cx);
            e.set_placeholder_text("Find in session…", window, cx);
            e
        });
        let subscription = cx.subscribe(&editor, |this: &mut Self, _, event, cx| {
            if let editor::EditorEvent::BufferEdited = event {
                this.recompute_matches(cx);
                cx.notify();
            }
        });
        let handle = editor.read(cx).focus_handle(cx);
        self.find = Some(FindState {
            editor,
            matches: Vec::new(),
            selected: None,
            _subscription: subscription,
        });
        self.recompute_matches(cx);
        window.focus(&handle, cx);
        cx.notify();
    }

    fn close_find(&mut self, _: &FindClose, window: &mut Window, cx: &mut Context<Self>) {
        if self.find.take().is_some() {
            window.focus(&self.focus_handle, cx);
            cx.notify();
        }
    }

    fn next_match(&mut self, _: &FindNextMatch, _window: &mut Window, cx: &mut Context<Self>) {
        let Some(find) = self.find.as_mut() else {
            return;
        };
        if find.matches.is_empty() {
            return;
        }
        let next = match find.selected {
            Some(i) => (i + 1) % find.matches.len(),
            None => 0,
        };
        find.selected = Some(next);
        cx.notify();
    }

    fn previous_match(
        &mut self,
        _: &FindPreviousMatch,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(find) = self.find.as_mut() else {
            return;
        };
        if find.matches.is_empty() {
            return;
        }
        let len = find.matches.len();
        let prev = match find.selected {
            Some(0) => len - 1,
            Some(i) => i - 1,
            None => 0,
        };
        find.selected = Some(prev);
        cx.notify();
    }

    fn recompute_matches(&mut self, cx: &mut Context<Self>) {
        let Some(find) = self.find.as_mut() else {
            return;
        };
        let query = find.editor.read(cx).text(cx);
        if query.is_empty() {
            find.matches.clear();
            find.selected = None;
            return;
        }
        let query_lower = query.to_lowercase();
        let mut matches = Vec::new();
        let session = self.session.read(cx);
        if let Some(thread) = session.acp_thread.as_ref() {
            let thread = thread.read(cx);
            for (entry_idx, entry) in thread.entries().iter().enumerate() {
                for (span_idx, text) in entry_text_spans(entry, cx).into_iter().enumerate() {
                    find_all(&text, &query_lower, |range| {
                        matches.push(FindMatch {
                            entry_idx,
                            span_idx,
                            range,
                        });
                    });
                }
            }
        }
        find.selected = if matches.is_empty() { None } else { Some(0) };
        find.matches = matches;
    }

    /// Floating "Jump to latest" affordance shown in the bottom-right of the
    /// conversation body when the user has scrolled away from the tail. Only
    /// visible while `stuck_to_bottom` is false; clicking restores stickiness
    /// + snaps the scroll to the latest entry.
    fn render_jump_to_latest(&self, cx: &mut Context<Self>) -> AnyElement {
        let btn = ui::IconButton::new("solution-session-jump-to-latest", IconName::ArrowDown)
            .shape(ui::IconButtonShape::Square)
            .icon_size(IconSize::Small)
            .icon_color(Color::Default)
            .tooltip(ui::Tooltip::text("Jump to latest"))
            .on_click(cx.listener(|this, _, _window, cx| {
                this.stuck_to_bottom = true;
                this.conversation_scroll.scroll_to_bottom();
                cx.notify();
            }));
        div()
            .absolute()
            .bottom_3()
            .right_3()
            .rounded_full()
            .shadow_md()
            .bg(cx.theme().colors().elevated_surface_background)
            .border_1()
            .border_color(cx.theme().colors().border)
            .child(btn)
            .into_any_element()
    }

    fn render_find_bar(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let find = self.find.as_ref()?;
        let total = find.matches.len();
        let pos_text = if total == 0 {
            "no results".to_string()
        } else {
            let i = find.selected.unwrap_or(0) + 1;
            format!("{i} of {total}")
        };
        Some(
            div()
                .key_context("SolutionSessionFindEditor")
                .track_focus(&find.editor.read(cx).focus_handle(cx))
                .flex()
                .h_8()
                .px_2()
                .gap_2()
                .items_center()
                .border_b_1()
                .border_color(cx.theme().colors().border_variant)
                .bg(cx.theme().colors().elevated_surface_background)
                .child(div().flex_1().child(find.editor.clone()))
                .child(
                    Label::new(pos_text)
                        .size(LabelSize::XSmall)
                        .color(Color::Muted),
                )
                .child(
                    IconButton::new("solution-find-prev", IconName::ChevronUp)
                        .icon_size(IconSize::Small)
                        .tooltip(Tooltip::text("Previous match"))
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.previous_match(&FindPreviousMatch, window, cx);
                        })),
                )
                .child(
                    IconButton::new("solution-find-next", IconName::ChevronDown)
                        .icon_size(IconSize::Small)
                        .tooltip(Tooltip::text("Next match"))
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.next_match(&FindNextMatch, window, cx);
                        })),
                )
                .child(
                    IconButton::new("solution-find-close", IconName::Close)
                        .icon_size(IconSize::Small)
                        .tooltip(Tooltip::text("Close (Esc)"))
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.close_find(&FindClose, window, cx);
                        })),
                )
                .into_any_element(),
        )
    }

    fn submit_compose_action(
        &mut self,
        _: &menu::Confirm,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // `menu::Confirm` is the catch-all "Enter" action and bubbles up the
        // focus chain. If focus isn't actually in the compose editor (e.g.
        // user is in the find bar, or just clicked into the conversation
        // body and pressed Enter), do nothing — sending stale draft text
        // because something elsewhere generated a Confirm event would be a
        // destructive surprise. Send button click goes through
        // `submit_compose_now`, bypassing this guard.
        let compose_focus = self.compose_editor.read(cx).focus_handle(cx);
        if !compose_focus.is_focused(window) {
            return;
        }
        self.submit_compose_now(window, cx);
    }

    /// Opens the compose buffer in a detached OS popup window. Picked over
    /// a workspace modal so the user can keep reading the conversation /
    /// browse code while writing a long prompt. While the popup is alive
    /// the inline compose row swaps to a placeholder + Cancel button (see
    /// `render` for the swap). If the popup is already open this call
    /// just brings it to the foreground.
    fn open_expanded_compose(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(handle) = self.expanded_window {
            // Already open — activate it and bail. If the window has been
            // closed behind our back (OS close button), the update fails
            // and we fall through to opening a fresh one.
            let alive = handle
                .update(cx, |_, window, _| {
                    window.activate_window();
                })
                .is_ok();
            if alive {
                return;
            }
            self.expanded_window = None;
        }
        let target = self.compose_editor.clone();
        let initial_text = target.read(cx).text(cx);
        let owner = cx.weak_entity();
        // Height tracks `EXPANDED_COMPOSE_HEIGHT_RATIO` of the *physical*
        // screen height. `display.bounds().size.height` is in logical
        // pixels (already physical / scale_factor on X11/Wayland), and
        // GPUI multiplies window bounds by scale_factor when handing them
        // to the platform — so a logical-pixel ratio comes out as the
        // same physical-pixel ratio on screen, regardless of HiDPI scale.
        // Manual origin math used to broke on multi-monitor / HiDPI mixes
        // (popup landed off-centre), so we hand off to the platform's
        // native centring via `WindowBounds::centered` — costs us
        // "non-primary monitor" placement on multi-display setups, but
        // wins us reliable centring everywhere else.
        let display_height = window
            .display(cx)
            .or_else(|| cx.primary_display())
            .map(|d| d.bounds().size.height)
            .unwrap_or(px(EXPANDED_COMPOSE_DEFAULT_H / EXPANDED_COMPOSE_HEIGHT_RATIO));
        let size = gpui::Size {
            width: px(EXPANDED_COMPOSE_DEFAULT_W),
            height: display_height * EXPANDED_COMPOSE_HEIGHT_RATIO,
        };
        let bounds = gpui::WindowBounds::centered(size, cx);
        let opened = cx.open_window(
            gpui::WindowOptions {
                titlebar: Some(gpui::TitlebarOptions {
                    title: Some("Edit prompt".into()),
                    appears_transparent: false,
                    traffic_light_position: None,
                }),
                window_bounds: Some(bounds),
                is_resizable: true,
                is_minimizable: true,
                kind: gpui::WindowKind::Normal,
                ..Default::default()
            },
            move |window, cx| {
                let view = cx.new(|cx| {
                    ExpandedComposeWindowView::new(
                        initial_text,
                        target.downgrade(),
                        owner,
                        window,
                        cx,
                    )
                });
                window.activate_window();
                let focus_handle = view.read(cx).editor.focus_handle(cx);
                focus_handle.focus(window, cx);
                // Closing via the OS title-bar X commits the draft —
                // hitting X on a long edit and losing the text was the
                // most surprising/punishing thing about an earlier
                // version. Cancel button stays as the explicit-discard
                // path. We do this by intercepting `should_close` and
                // running the save path before allowing the close;
                // returning `true` lets the framework finish closing
                // (which calls remove_window in the deferred close path).
                let weak = view.downgrade();
                window.on_window_should_close(cx, move |window, cx| {
                    if let Some(view) = weak.upgrade() {
                        view.update(cx, |this, cx| {
                            this.save(window, cx);
                        });
                    }
                    true
                });
                view
            },
        );
        match opened {
            Ok(handle) => self.expanded_window = Some(handle),
            Err(err) => log::error!("failed to open expanded compose window: {err:?}"),
        }
    }

    /// Closes the popup window without applying its text. Called from the
    /// inline Cancel button so users don't have to hunt the popup down on
    /// the desktop just to discard it. Handle is cleared either way (if
    /// the popup has already been closed externally, `update` errors and
    /// we just drop the stale handle).
    fn close_expanded_compose(&mut self, cx: &mut Context<Self>) {
        let Some(handle) = self.expanded_window.take() else {
            return;
        };
        handle
            .update(cx, |_, window, _| {
                window.remove_window();
            })
            .ok();
        cx.notify();
    }

    /// Cancel the in-flight agent turn for this session. Wired to the
    /// Stop button that swaps in for "Send" while `state == Running`,
    /// and to Esc via the action handler in this view.
    fn cancel_turn(&self, cx: &mut Context<Self>) {
        let session_id = self.session_id;
        let store = SolutionAgentStore::global(cx);
        store.update(cx, |store, cx| {
            if let Err(err) = store.cancel_turn(session_id, cx) {
                log::warn!("solution_agent: cancel_turn failed: {err:#}");
            }
        });
    }

    fn handle_stop_response(
        &mut self,
        _: &StopResponse,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if matches!(self.session.read(cx).state, SessionState::Running { .. }) {
            self.cancel_turn(cx);
            cx.stop_propagation();
        }
    }

    fn submit_compose_now(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let content = self.compose_editor.read(cx).text(cx);
        if content.trim().is_empty() && self.pending_images.is_empty() {
            return;
        }
        // Pre-flight slash-command validation so a typo'd `/clearr` doesn't
        // disappear silently into the agent (where it gets treated as a
        // plain prompt). Show a toast and bail; user fixes the typo and
        // resends. Commands without arguments that the agent advertises
        // pass through as text — claude-acp parses them server-side.
        if let Some(rejection) = self.validate_slash_command(&content, cx) {
            self.show_toast(rejection, cx);
            return;
        }
        self.compose_editor
            .update(cx, |e, cx| e.clear(window, cx));
        // Sending implies "I want to follow what happens next." Re-stick to
        // the bottom even if the user had scrolled up to read older context.
        self.stuck_to_bottom = true;
        self.conversation_scroll.scroll_to_bottom();
        let session_id = self.session_id;

        if self.pending_images.is_empty() {
            SolutionAgentStore::global(cx).update(cx, |store, cx| {
                store
                    .send_message(session_id, content, cx)
                    .detach_and_log_err(cx);
            });
            return;
        }

        let images = std::mem::take(&mut self.pending_images);
        let mut blocks: Vec<acp::ContentBlock> = Vec::with_capacity(images.len() + 1);
        if !content.trim().is_empty() {
            blocks.push(acp::ContentBlock::Text(acp::TextContent::new(content)));
        }
        for image in images {
            blocks.push(acp::ContentBlock::Image(acp::ImageContent::new(
                image.data_base64,
                image.mime_type,
            )));
        }
        SolutionAgentStore::global(cx).update(cx, |store, cx| {
            store
                .send_message_blocks(session_id, blocks, cx)
                .detach_and_log_err(cx);
        });
    }

    /// Returns `Some(error_message)` if `text` starts with a `/command` form
    /// the agent did not advertise (or with a known command that requires
    /// an argument but none was given). `None` means the message is fine to
    /// send as-is. Bare `/` and any text not starting with `/` always pass.
    fn validate_slash_command(&self, text: &str, cx: &App) -> Option<SharedString> {
        let trimmed = text.trim_start();
        if !trimmed.starts_with('/') {
            return None;
        }
        let first_line = trimmed.lines().next().unwrap_or("");
        let after_slash = &first_line[1..];
        let (name, rest) = match after_slash.find(char::is_whitespace) {
            Some(idx) => (&after_slash[..idx], after_slash[idx..].trim()),
            None => (after_slash, ""),
        };
        if name.is_empty() {
            return None;
        }
        let commands = self
            .session
            .read(cx)
            .acp_thread
            .as_ref()
            .map(|thread| thread.read(cx).available_commands().to_vec())
            .unwrap_or_default();
        let matched = commands.iter().find(|cmd| cmd.name == name);
        match matched {
            None => {
                let mut available = commands
                    .iter()
                    .map(|cmd| format!("/{}", cmd.name))
                    .collect::<Vec<_>>();
                available.sort();
                let suffix = if available.is_empty() {
                    "The agent has not advertised any commands.".to_string()
                } else {
                    format!("Available: {}", available.join(", "))
                };
                Some(format!("Unknown command /{name}. {suffix}").into())
            }
            Some(cmd) if cmd.input.is_some() && rest.is_empty() => {
                let hint = cmd
                    .input
                    .as_ref()
                    .and_then(|input| match input {
                        acp::AvailableCommandInput::Unstructured(payload) => {
                            Some(payload.hint.clone())
                        }
                        _ => None,
                    })
                    .unwrap_or_default();
                let detail = if hint.is_empty() {
                    String::new()
                } else {
                    format!(" ({hint})")
                };
                Some(format!("/{name} requires an argument{detail}.").into())
            }
            Some(_) => None,
        }
    }

    fn show_toast(&self, message: SharedString, cx: &mut Context<Self>) {
        let Some(workspace) = self.workspace.upgrade() else {
            log::warn!("solution_agent toast (no workspace): {message}");
            return;
        };
        workspace.update(cx, |workspace, cx| {
            struct SlashCommandRejected;
            workspace.show_notification(
                NotificationId::unique::<SlashCommandRejected>(),
                cx,
                move |cx| cx.new(|cx| MessageNotification::new(message, cx)),
            );
        });
    }

    fn paste_intercept(
        &mut self,
        _: &editor::actions::Paste,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(clipboard) = cx.read_from_clipboard() else {
            return;
        };
        // Respect source-app priority: if the first entry is text, fall through
        // to the editor's default text-paste action. Returning without
        // consuming via `cx.stop_propagation()` lets the action propagate.
        let first = clipboard.entries().first();
        let has_image = matches!(
            first,
            Some(ClipboardEntry::Image(_)) | Some(ClipboardEntry::ExternalPaths(_))
        );
        if !has_image {
            return;
        }

        let mut new_images: Vec<PendingImage> = Vec::new();
        for entry in clipboard.into_entries() {
            if let ClipboardEntry::Image(image) = entry {
                let mime_type = image.format().mime_type().to_string();
                let data = base64::engine::general_purpose::STANDARD.encode(image.bytes());
                let label = SharedString::from(format!(
                    "image #{}",
                    self.pending_images.len() + new_images.len() + 1
                ));
                new_images.push(PendingImage {
                    mime_type,
                    data_base64: data,
                    label,
                });
            }
            // Other entries (paths, strings) — ignore for v1. File paths from
            // drag-drop are handled separately by handle_external_paths_drop.
        }

        if new_images.is_empty() {
            return;
        }

        let placeholder_text = new_images
            .iter()
            .map(|img| format!("[{}]", img.label))
            .collect::<Vec<_>>()
            .join(" ");
        self.pending_images.extend(new_images);
        self.compose_editor.update(cx, |editor, cx| {
            editor.insert(&placeholder_text, window, cx);
            editor.insert(" ", window, cx);
        });
        cx.stop_propagation();
        cx.notify();
    }

    fn handle_external_paths_drop(
        &mut self,
        paths: &ExternalPaths,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if paths.0.is_empty() {
            return;
        }
        let workspace_root = self
            .workspace
            .upgrade()
            .and_then(|workspace| {
                workspace.read(cx).visible_worktrees(cx).next().map(|w| {
                    w.read(cx).abs_path().to_path_buf()
                })
            });
        let mention_text = paths
            .0
            .iter()
            .map(|abs_path| {
                let display = workspace_root
                    .as_ref()
                    .and_then(|root| abs_path.strip_prefix(root).ok())
                    .map(|rel| rel.to_string_lossy().to_string())
                    .unwrap_or_else(|| abs_path.to_string_lossy().to_string());
                format!("@{display}")
            })
            .collect::<Vec<_>>()
            .join(" ");
        self.compose_editor.update(cx, |editor, cx| {
            editor.insert(&mention_text, window, cx);
            editor.insert(" ", window, cx);
        });
        let focus = self.compose_editor.read(cx).focus_handle(cx);
        window.focus(&focus, cx);
    }
}

impl Focusable for SolutionSessionView {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        // Forward to the compose editor — when the workspace restores
        // focus to this view (panel toggle, modal close, dock activation,
        // navigator pointing back here), the user expects the input
        // caret to come back, not some abstract container. Without this
        // forward, panel-level focus restoration kept landing on the
        // SolutionSessionView's own handle and the click-to-focus on the
        // compose row got immediately stolen back by the next focus
        // restoration cycle.
        self.compose_editor.read(cx).focus_handle(cx)
    }
}

pub enum SolutionSessionViewEvent {}

impl EventEmitter<SolutionSessionViewEvent> for SolutionSessionView {}

impl SolutionSessionView {
    /// Walks the active thread once and returns the same per-entry
    /// per-span text shape `entry_text_spans` produces — but as cloned
    /// `String`s so the caller can release the session/thread borrow on
    /// `cx` before doing any mutating work (like ensuring the markdown
    /// cache). Empty if there's no thread yet.
    fn collect_entry_texts(&self, cx: &App) -> Vec<Vec<String>> {
        let session = self.session.read(cx);
        let Some(thread) = session.acp_thread.as_ref() else {
            return Vec::new();
        };
        let thread = thread.read(cx);
        thread
            .entries()
            .iter()
            .map(|entry| entry_text_spans(entry, cx))
            .collect()
    }

    /// Walks every tool-call terminal currently in the conversation and
    /// makes sure we're subscribed to its inner `terminal::Terminal` for
    /// streaming-output events. The PTY/pipe-injection paths emit
    /// `terminal::Event::Wakeup` (NOT `cx.notify`) on every chunk of bytes,
    /// so a plain `cx.observe` would never fire and the view would only
    /// repaint when an unrelated event (new assistant message, user
    /// typing) happened to retrigger render. Subscriptions for terminals
    /// no longer present are dropped to keep the map bounded across long
    /// sessions.
    fn sync_terminal_observers(&mut self, cx: &mut Context<Self>) {
        let mut current = Vec::new();
        let session = self.session.read(cx);
        if let Some(thread) = session.acp_thread.as_ref() {
            for entry in thread.read(cx).entries() {
                if let AgentThreadEntry::ToolCall(call) = entry {
                    for content in &call.content {
                        if let ToolCallContent::Terminal(term) = content {
                            current.push(term.read(cx).inner().clone());
                        }
                    }
                }
            }
        }
        let mut keep: std::collections::HashSet<EntityId> =
            std::collections::HashSet::with_capacity(current.len());
        for inner in current {
            let id = Entity::entity_id(&inner);
            keep.insert(id);
            self.terminal_observers.entry(id).or_insert_with(|| {
                cx.subscribe(
                    &inner,
                    |_this, _, event: &::terminal::Event, cx| {
                        if matches!(event, ::terminal::Event::Wakeup) {
                            cx.notify();
                        }
                    },
                )
            });
        }
        self.terminal_observers.retain(|id, _| keep.contains(id));
    }
}

impl Render for SolutionSessionView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // The view used to render its own header (which leaked Debug-formatted
        // SessionState as JSON-looking goo) and was an upstream `Item` so it
        // could open as a workspace pane tab. Both are gone now: the chat
        // panel hosts views inside its own tab strip + status row, so this
        // view is just the conversation + compose box.
        // Compute the find bar first because `render_find_bar` needs `&mut cx`,
        // while the `session.read(cx)` borrow held for the conversation body
        // section is immutable. Borrow checker rejects nesting them.
        let find_bar = self.render_find_bar(cx);

        // Pre-pass: build the list of (entry_idx, span_idx) → markdown
        // entity mappings. Done up-front so the borrow on `cx` released by
        // `collect_entry_texts` lets us mutate the markdown cache before
        // we re-borrow `cx` immutably for the rendering pass.
        // Refresh terminal subscriptions before collecting texts so any
        // newly-arrived tool-call terminal starts streaming into our view
        // on its very first chunk (vs the next unrelated render).
        self.sync_terminal_observers(cx);
        let texts_per_entry = self.collect_entry_texts(cx);
        let mut markdown_for: HashMap<(usize, usize), Entity<Markdown>> = HashMap::new();
        let (find_matches_owned, find_selected_for_md) = self
            .find
            .as_ref()
            .map(|f| (f.matches.clone(), f.selected))
            .unwrap_or_default();
        for (entry_idx, spans) in texts_per_entry.iter().enumerate() {
            for (span_idx, text) in spans.iter().enumerate() {
                let key = (entry_idx, span_idx);
                let entity =
                    self.ensure_markdown(key, SharedString::from(text.clone()), cx);
                // Apply find-bar highlights right where the markdown
                // entity lives — its renderer paints the ranges itself,
                // so we don't have to thread highlight state through the
                // entry-render functions for every label.
                let (span_ranges, active_in_span) = matches_for_span(
                    &find_matches_owned,
                    find_selected_for_md,
                    entry_idx,
                    span_idx,
                );
                entity.update(cx, |md, cx| {
                    md.set_search_highlights(span_ranges, active_in_span, cx);
                });
                markdown_for.insert(key, entity);
            }
        }
        // Drop cache entries for entries that no longer exist (e.g. after
        // a session reset). Without this the HashMap grows unbounded as
        // sessions are switched in the same view.
        let entry_count = texts_per_entry.len();
        self.markdown_cache
            .retain(|(idx, _), _| *idx < entry_count);

        // Override inline-code color to a muted text-accent. Pure
        // text_accent is too saturated for prose — it stings on long
        // turns where every other word is `identifier`-y. 0.75 alpha +
        // restoring a very faint background gives the cyan-ish "this is
        // code" cue (à la Claude Code CLI) without the glare.
        let mut markdown_style = MarkdownStyle::themed(MarkdownFont::Agent, window, cx);
        let accent = cx.theme().colors().text_accent;
        markdown_style.inline_code.color = Some(accent.opacity(0.75));
        markdown_style.inline_code.background_color =
            Some(cx.theme().colors().editor_foreground.opacity(0.05));

        let session = self.session.read(cx);
        // Resolve the assistant label dynamically from the session's adapter
        // — never bake a specific provider name into the chrome. Falls back
        // to a generic "Assistant" if the adapter is gone (config edited
        // mid-session, etc.).
        let assistant_label: SharedString = SolutionAgentStore::try_global(cx)
            .and_then(|store| {
                store.read_with(cx, |s, _| s.adapters.get(&session.agent_id).map(|a| a.display_name()))
            })
            .unwrap_or_else(|| SharedString::from("Assistant"));
        let pending_image_count = self.pending_images.len();
        div()
            .id("solution-session-view")
            .key_context("SolutionSessionView")
            .track_focus(&self.focus_handle)
            .capture_action(cx.listener(Self::paste_intercept))
            .on_action(cx.listener(Self::submit_compose_action))
            .on_drag_move(cx.listener(
                |this, e: &DragMoveEvent<DraggedComposeHandle>, _, cx| {
                    log::debug!(
                        "compose drag move: pos.y={:?} start_y={:?} start_h={:?}",
                        e.event.position.y,
                        this.resize_start_y,
                        this.resize_start_height,
                    );
                    // Inverted: handle is at the top of the compose row, so
                    // mouse moving UP (smaller y) should INCREASE height.
                    let delta = this.resize_start_y - e.event.position.y;
                    let new_height =
                        (this.resize_start_height + delta).clamp(
                            px(MIN_COMPOSE_HEIGHT),
                            px(MAX_COMPOSE_HEIGHT),
                        );
                    if new_height != this.compose_height {
                        this.compose_height = new_height;
                        cx.notify();
                    }
                },
            ))
            .on_action(cx.listener(Self::open_find))
            .on_action(cx.listener(Self::close_find))
            .on_action(cx.listener(Self::next_match))
            .on_action(cx.listener(Self::previous_match))
            .on_action(cx.listener(Self::handle_stop_response))
            .on_drop(cx.listener(
                |this, paths: &ExternalPaths, window, cx| {
                    this.handle_external_paths_drop(paths, window, cx);
                },
            ))
            .flex()
            .flex_col()
            .size_full()
            .bg(cx.theme().colors().panel_background)
            .when_some(find_bar, |this, bar| this.child(bar))
            .child({
                // Body + "Jump to latest" affordance share a relative wrapper
                // so the button can position absolute against the scrolling
                // body without escaping it. flex_1 + min_h_0 mirrors the body
                // div pattern (see comment on body) so this wrapper is what
                // takes the available space in the column.
                let stuck = self.stuck_to_bottom;
                if stuck {
                    // Flag the next layout to snap to the bottom — does
                    // nothing visible if we're already there, otherwise
                    // catches new entries that grew past the viewport.
                    self.conversation_scroll.scroll_to_bottom();
                }
                let mut body = div()
                    .id("solution-session-conversation")
                    .flex_1()
                    // Without min_h_0 a flex-child defaults to min-height:auto
                    // which equals its content height — long conversations
                    // then push the compose row off the bottom of the panel
                    // *and* prevent overflow_y_scroll from ever kicking in
                    // (no overflow because the container grew to fit). This
                    // is the standard scrollable-flex-column pattern; see
                    // upstream agent_ui thread_view.rs:3224 for the same fix.
                    .min_h_0()
                    .px_2()
                    .py_1()
                    .overflow_y_scroll()
                    .track_scroll(&self.conversation_scroll)
                    .on_scroll_wheel(cx.listener(|this, ev: &ScrollWheelEvent, _window, cx| {
                        // Only break out of stuck-to-bottom when the user
                        // scrolls UP. Scrolling down is either a no-op at
                        // the bottom or a "catch up" toward fresh content,
                        // so flipping the flag there would race with the
                        // post-stream `scroll_to_bottom` and produce a
                        // snap-back-down loop. Re-attach is explicit:
                        // "Jump to latest" button, or sending a message.
                        let scrolling_up = match ev.delta {
                            ScrollDelta::Pixels(p) => p.y > Pixels::ZERO,
                            ScrollDelta::Lines(l) => l.y > 0.0,
                        };
                        if scrolling_up && this.stuck_to_bottom {
                            this.stuck_to_bottom = false;
                            cx.notify();
                        }
                    }));
                let mut content = div().flex().flex_col().w_full();
                if let Some(thread_entity) = session.acp_thread.as_ref() {
                    let thread = thread_entity.read(cx);
                    let entries = thread.entries();
                    let supports_rewind = thread.supports_truncate(cx);
                    if entries.is_empty() {
                        content = content
                            .child(Label::new("(no messages yet)").size(LabelSize::Default));
                    } else {
                        for (idx, entry) in entries.iter().enumerate() {
                            // For an assistant message, rewinding "to here"
                            // means truncating starting from the next user
                            // message — that keeps everything up to and
                            // including this assistant response.
                            let rewind_target = if supports_rewind
                                && matches!(
                                    entry,
                                    AgentThreadEntry::AssistantMessage(_)
                                        | AgentThreadEntry::ToolCall(_)
                                )
                            {
                                entries
                                    .iter()
                                    .skip(idx + 1)
                                    .find_map(|e| match e {
                                        AgentThreadEntry::UserMessage(m) => m.id.clone(),
                                        _ => None,
                                    })
                            } else {
                                None
                            };
                            content = content.child(render_entry(
                                idx,
                                entry,
                                &markdown_for,
                                &markdown_style,
                                &assistant_label,
                                rewind_target,
                                thread_entity.downgrade(),
                                cx,
                            ));
                        }
                    }
                } else {
                    content =
                        content.child(Label::new("(no thread yet)").size(LabelSize::Default));
                }
                // Render queued follow-ups (typed while the agent is
                // still working) as ghost user-message bubbles. They
                // disappear on Stopped when the store flushes them
                // into a real ACP UserMessage entry.
                let pending = session.pending_messages.clone();
                for (q_idx, blocks) in pending.iter().enumerate() {
                    let preview = pending_blocks_preview(blocks, cx);
                    if preview.is_empty() {
                        continue;
                    }
                    content = content.child(render_pending_message(q_idx, &preview, cx));
                }
                // Right-click → Copy / Copy as markdown. The menu pins the
                // currently-focused element (typically a Markdown widget the
                // user just clicked into to drag a selection) as its
                // dispatch context, so the markdown::Copy / CopyAsMarkdown
                // actions land on that widget. Empty selection ⇒ no-op.
                let body_content = right_click_menu("solution-session-context-menu")
                    .trigger(move |_, _, _| content)
                    .menu(move |window, cx| {
                        let focus = window.focused(cx);
                        ContextMenu::build(window, cx, move |menu, _, _| {
                            menu.when_some(focus, |menu, focus| menu.context(focus))
                                .action("Copy", Box::new(markdown::Copy))
                                .action(
                                    "Copy as markdown",
                                    Box::new(markdown::CopyAsMarkdown),
                                )
                        })
                    });
                body = body.child(body_content);
                // The wrapper has to be a flex column so the body's `.flex_1()
                // .min_h_0()` actually claims height; without `.flex().flex_col()`
                // here the body collapsed to its content height, killing the
                // overflow_y_scroll.
                // Scrollbar lives on the OUTER (non-scrolling) wrapper —
                // applied to the body itself, the absolute-positioned thumb
                // gets painted at the top of the *content* and scrolls out
                // of view with the conversation. The wrapper bounds == the
                // viewport, so the thumb stays pinned correctly.
                div()
                    .relative()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_h_0()
                    .child(body)
                    .when(!stuck, |this| this.child(self.render_jump_to_latest(cx)))
                    .vertical_scrollbar_for(&self.conversation_scroll, window, cx)
            })
            .child({
                // Compose row + resize handle in a single flex_col:
                // top 6px is the drag handle (sticks out of the editor
                // bg, makes itself visible against the conversation),
                // below it lives the original flex_row with editor +
                // buttons. Wrapping them as one block stops the handle
                // from getting pushed off the panel by flex math when
                // the panel is short.
                let compose_row = div()
                    .flex()
                    .flex_col()
                    .flex_none()
                    .h(self.compose_height + px(3.0))
                    .child(
                        div()
                            .id("solution-session-compose-resize")
                            .flex_none()
                            .h(px(3.0))
                            .w_full()
                            .cursor_row_resize()
                            .bg(cx.theme().colors().border)
                            .hover(|s| s.bg(cx.theme().colors().border_focused))
                            .occlude()
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(|this, e: &MouseDownEvent, _, cx| {
                                    this.resize_start_y = e.position.y;
                                    this.resize_start_height = this.compose_height;
                                    log::debug!(
                                        "compose drag down: start_y={:?} start_h={:?}",
                                        this.resize_start_y,
                                        this.resize_start_height,
                                    );
                                    cx.stop_propagation();
                                }),
                            )
                            .on_drag(DraggedComposeHandle, |handle, _, _, cx| {
                                cx.stop_propagation();
                                cx.new(|_| handle.clone())
                            }),
                    );
                let mut compose_inner = div()
                    .flex()
                    .flex_none()
                    .h(self.compose_height)
                    .p_2()
                    .gap_2()
                    // Match the editor's own background colour so the
                    // compose area reads as a single input rectangle, not
                    // a panel-bg strip with a darker editor block stacked
                    // inside it (which is what showed up after switching
                    // to `multi_line`: editor renders with
                    // `editor_background`, but the row around it kept
                    // panel_bg, producing a visible seam).
                    .bg(cx.theme().colors().editor_background)
                    .child(
                        // While the popup window is open the inline editor
                        // is unreachable — clicking it should bring the
                        // popup forward instead. The placeholder div is
                        // sized + styled so the layout doesn't jump when
                        // we swap.
                        div()
                            .id("solution-session-compose-area")
                            .flex_1()
                            .h_full()
                            .map(|this| {
                                if self.expanded_window.is_some() {
                                    this.flex()
                                        .items_center()
                                        .justify_center()
                                        .cursor_pointer()
                                        .child(
                                            Label::new(
                                                "Extended editor open — click to focus",
                                            )
                                            .color(Color::Muted)
                                            .size(LabelSize::Small),
                                        )
                                        .on_mouse_down(
                                            MouseButton::Left,
                                            cx.listener(|this, _, window, cx| {
                                                this.open_expanded_compose(window, cx);
                                            }),
                                        )
                                } else {
                                    this.child(self.compose_editor.clone())
                                }
                            }),
                    )
                    .child(
                        v_flex()
                            .flex_none()
                            .gap_1()
                            .child(
                                IconButton::new(
                                    "solution-session-expand-compose",
                                    IconName::Maximize,
                                )
                                .icon_size(IconSize::Small)
                                .icon_color(Color::Muted)
                                .tooltip(Tooltip::text(
                                    "Open prompt in detached editor window",
                                ))
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.open_expanded_compose(window, cx);
                                })),
                            )
                            .map(|this| {
                                if self.expanded_window.is_some() {
                                    return this.child(
                                        ui::Button::new(
                                            "solution-session-cancel-expanded",
                                            "Cancel",
                                        )
                                        .style(ui::ButtonStyle::Subtle)
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            this.close_expanded_compose(cx);
                                        })),
                                    );
                                }
                                let is_working = matches!(
                                    self.session.read(cx).state,
                                    SessionState::Running { .. }
                                );
                                let pending_count =
                                    self.session.read(cx).pending_messages.len();
                                let send_label: SharedString = if is_working {
                                    "Queue".into()
                                } else {
                                    "Send".into()
                                };
                                let send_tooltip: SharedString = if is_working {
                                    if pending_count > 0 {
                                        format!(
                                            "Queue follow-up — {pending_count} already waiting"
                                        )
                                        .into()
                                    } else {
                                        "Queue follow-up — runs after current turn".into()
                                    }
                                } else {
                                    "Send message".into()
                                };
                                this.child(
                                    ui::Button::new("solution-session-send", send_label)
                                        .tooltip(Tooltip::text(send_tooltip))
                                        .on_click(cx.listener(|this, _, window, cx| {
                                            this.submit_compose_now(window, cx);
                                        })),
                                )
                                .when(is_working, |this| {
                                    this.child(
                                        IconButton::new(
                                            "solution-session-stop",
                                            IconName::Stop,
                                        )
                                        .icon_color(Color::Error)
                                        .tooltip(Tooltip::text(
                                            "Stop response (Esc) — clears queued follow-ups",
                                        ))
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            this.cancel_turn(cx);
                                        })),
                                    )
                                })
                            }),
                    );
                if pending_image_count > 0 {
                    compose_inner = compose_inner.child(
                        Label::new(format!(
                            "{pending_image_count} image{} attached",
                            if pending_image_count == 1 { "" } else { "s" }
                        ))
                        .size(LabelSize::XSmall),
                    );
                }
                compose_row.child(compose_inner)
            })
    }
}

/// Per-entry text spans used by the find bar.
///
/// MUST iterate the entry in the same order as `render_*` functions emit
/// labels, so `(entry_idx, span_idx)` produced by `recompute_matches` lines
/// up with the label rendered for that span. If you add or reorder labels
/// in a render function, mirror the change here or matches will be applied
/// to the wrong line.
fn entry_text_spans(entry: &AgentThreadEntry, cx: &App) -> Vec<String> {
    match entry {
        AgentThreadEntry::UserMessage(message) => vec![clean_user_message_text(
            &content_block_text(&message.content, cx),
        )],
        AgentThreadEntry::AssistantMessage(message) => {
            let has_message = message
                .chunks
                .iter()
                .any(|c| matches!(c, AssistantMessageChunk::Message { .. }));
            let mut spans = Vec::new();
            for chunk in &message.chunks {
                let (prefix, block) = match chunk {
                    AssistantMessageChunk::Message { block } => (None, block),
                    AssistantMessageChunk::Thought { block } if !has_message => {
                        (Some("thinking: "), block)
                    }
                    AssistantMessageChunk::Thought { .. } => continue,
                };
                let mut text = content_block_text(block, cx);
                if let Some(prefix) = prefix {
                    text = format!("{prefix}{text}");
                }
                if !text.is_empty() {
                    spans.push(text);
                }
            }
            spans
        }
        AgentThreadEntry::ToolCall(call) => {
            let label_text = call.label.read(cx).source().to_string();
            let status_text = tool_call_status_text(&call.status);
            let mut spans = vec![format!("Tool: {label_text} ({status_text})")];
            for content in &call.content {
                let summary = tool_call_content_summary(call, content, cx);
                if !summary.is_empty() {
                    spans.push(summary);
                }
            }
            spans
        }
        AgentThreadEntry::CompletedPlan(entries) => {
            let mut spans = vec!["Plan".to_string()];
            for entry in entries {
                let source = entry.content.read(cx).source().to_string();
                spans.push(format!("• {source}"));
            }
            spans
        }
    }
}

/// Find every (case-insensitive) occurrence of `query_lower` in `text`.
/// Caller pre-lowercases the query — we lowercase `text` here so matches
/// are case-insensitive without modifying the caller's data. Range returned
/// is in BYTE offsets into the *original* `text` (lowercase preserves
/// length only for ASCII; for non-ASCII fold this is approximate but
/// fine for v1 — Latin/Cyrillic typical case-insensitive use works).
fn find_all(text: &str, query_lower: &str, mut emit: impl FnMut(Range<usize>)) {
    if query_lower.is_empty() {
        return;
    }
    let haystack = text.to_lowercase();
    let mut start = 0;
    while let Some(rel) = haystack[start..].find(query_lower) {
        let abs = start + rel;
        emit(abs..abs + query_lower.len());
        start = abs + query_lower.len().max(1);
    }
}

fn tool_call_status_text(status: &ToolCallStatus) -> &'static str {
    match status {
        ToolCallStatus::Pending => "pending",
        ToolCallStatus::WaitingForConfirmation { .. } => "waiting for confirmation",
        ToolCallStatus::InProgress => "running",
        ToolCallStatus::Completed => "done",
        ToolCallStatus::Failed => "failed",
        ToolCallStatus::Rejected => "rejected",
        ToolCallStatus::Canceled => "canceled",
    }
}

/// Filters `matches` down to the ones that fall in span `(entry_idx,
/// span_idx)`, preserving order, and translates the global `selected`
/// index into a span-local index (None if the active match isn't in
/// this span). Used by the search-highlight pre-pass in `Render` to
/// hand per-span ranges to each Markdown entity.
fn matches_for_span(
    matches: &[FindMatch],
    selected: Option<usize>,
    entry_idx: usize,
    span_idx: usize,
) -> (Vec<Range<usize>>, Option<usize>) {
    let mut ranges = Vec::new();
    let mut selected_in_span = None;
    for (i, m) in matches.iter().enumerate() {
        if m.entry_idx == entry_idx && m.span_idx == span_idx {
            if Some(i) == selected {
                selected_in_span = Some(ranges.len());
            }
            ranges.push(m.range.clone());
        }
    }
    (ranges, selected_in_span)
}

/// Render a span as either a Markdown widget (preferred — handles
/// headings, bold, lists, code blocks) or, if the entity is missing, a
/// plain Label fallback. Falls back to `Empty` when the text is empty.
fn render_span(
    key: (usize, usize),
    fallback_text: &str,
    markdown_for: &HashMap<(usize, usize), Entity<Markdown>>,
    style: &MarkdownStyle,
) -> AnyElement {
    if let Some(entity) = markdown_for.get(&key) {
        MarkdownElement::new(entity.clone(), style.clone()).into_any_element()
    } else if fallback_text.is_empty() {
        Empty.into_any_element()
    } else {
        Label::new(fallback_text.to_string())
            .size(LabelSize::Small)
            .into_any_element()
    }
}

fn render_entry(
    entry_idx: usize,
    entry: &AgentThreadEntry,
    markdown_for: &HashMap<(usize, usize), Entity<Markdown>>,
    style: &MarkdownStyle,
    assistant_label: &SharedString,
    rewind_target: Option<UserMessageId>,
    thread: gpui::WeakEntity<AcpThread>,
    cx: &App,
) -> AnyElement {
    let inner: AnyElement = match entry {
        AgentThreadEntry::UserMessage(message) => {
            render_user_message(entry_idx, message, markdown_for, style, cx)
        }
        AgentThreadEntry::AssistantMessage(message) => {
            render_assistant_message(entry_idx, message, markdown_for, style, assistant_label, cx)
        }
        AgentThreadEntry::ToolCall(call) => {
            render_tool_call(entry_idx, call, markdown_for, style, cx)
        }
        AgentThreadEntry::CompletedPlan(entries) => {
            render_plan(entry_idx, entries, markdown_for, style, cx)
        }
    };

    // Wrap with a "Rewind to this point" right-click menu when the
    // agent supports truncation AND there's a downstream user message
    // we can truncate at. Without those preconditions the menu is just
    // a no-op, so we skip the wrap entirely (keeps native text
    // selection on the message body).
    let Some(target_id) = rewind_target else {
        return inner;
    };
    let inner_cell = std::cell::RefCell::new(Some(inner));
    let thread_for_action = thread.clone();
    ui::right_click_menu(("rewind-entry", entry_idx))
        .trigger(move |_, _, _| {
            inner_cell
                .borrow_mut()
                .take()
                .unwrap_or_else(|| Empty.into_any_element())
        })
        .menu(move |window, cx| {
            let target_id = target_id.clone();
            let thread = thread_for_action.clone();
            // Pin the currently-focused element (the Markdown widget the
            // user just clicked into to drag a selection) so Copy /
            // Copy-as-markdown land on it — same trick the body-wide
            // context menu uses. Without this the entry-scoped menu
            // would silently swallow the copy actions.
            let focus = window.focused(cx);
            ContextMenu::build(window, cx, move |menu, _, _| {
                menu.entry("Rewind to this point", None, {
                    let target_id = target_id.clone();
                    let thread = thread.clone();
                    move |_window, cx| {
                        let target_id = target_id.clone();
                        if let Some(thread) = thread.upgrade() {
                            thread.update(cx, |thread: &mut AcpThread, cx| {
                                thread.rewind(target_id, cx).detach_and_log_err(cx);
                            });
                        }
                    }
                })
                .separator()
                .when_some(focus, |menu, focus| menu.context(focus))
                .action("Copy", Box::new(markdown::Copy))
                .action("Copy as markdown", Box::new(markdown::CopyAsMarkdown))
            })
        })
        .into_any_element()
}

/// Plain-text preview of a queued follow-up, used by the "ghost"
/// bubble we draw while the message is sitting in `pending_messages`.
/// Concatenates text blocks AS-IS (preserving the `\n\n` separators
/// that `send_message_blocks` injects when merging queued submits)
/// and substitutes `[image #N]` placeholders for image blocks.
fn pending_blocks_preview(blocks: &[acp::ContentBlock], _cx: &App) -> String {
    let mut out = String::new();
    let mut image_idx = 1usize;
    for block in blocks {
        match block {
            acp::ContentBlock::Text(t) => {
                out.push_str(&t.text);
            }
            acp::ContentBlock::Image(_) => {
                out.push_str(&format!("[image #{image_idx}]"));
                image_idx += 1;
            }
            _ => {}
        }
    }
    out.trim().to_string()
}

/// Ghost bubble for a queued follow-up. Same shape as a user message
/// but slightly muted so it reads as "this hasn't been sent yet" while
/// still using the user-message visual vocabulary.
fn render_pending_message(idx: usize, preview: &str, cx: &App) -> AnyElement {
    let bubble_bg = cx.theme().colors().text_accent.opacity(0.06);
    let border_color = cx.theme().colors().text_accent.opacity(0.4);
    v_flex()
        .px_1()
        .mb_3()
        .child(
            h_flex().child(
                div()
                    .relative()
                    .max_w(relative(0.85))
                    .px_2p5()
                    .py_1()
                    .bg(bubble_bg)
                    .border_1()
                    .border_dashed()
                    .border_color(border_color)
                    .rounded_md()
                    .child(Label::new(preview.to_string()).color(Color::Muted))
                    .child(
                        div()
                            .id(SharedString::from(format!("pending-msg-{idx}")))
                            .child(
                                Label::new("Queued — sends when agent finishes")
                                    .size(LabelSize::XSmall)
                                    .color(Color::Muted)
                                    .italic(),
                            ),
                    ),
            ),
        )
        .into_any_element()
}

fn render_user_message(
    entry_idx: usize,
    message: &UserMessage,
    markdown_for: &HashMap<(usize, usize), Entity<Markdown>>,
    style: &MarkdownStyle,
    cx: &App,
) -> AnyElement {
    // `clean_user_message_text` strips the literal "`Image`"
    // placeholder our acp_thread merger emits AND rewrites the
    // user-typed `[image #N]` placeholders into markdown links so the
    // Markdown widget paints them as clickable spans. The actual
    // image preview opens through the `on_url_click` hook below.
    let text = clean_user_message_text(&content_block_text(&message.content, cx));
    let bubble_bg = cx.theme().colors().text_accent.opacity(0.12);
    let group_name = SharedString::from(format!("user-msg-{entry_idx}"));

    let images: Vec<std::sync::Arc<gpui::Image>> = message
        .chunks
        .iter()
        .filter_map(|chunk| match chunk {
            acp::ContentBlock::Image(image_content) => decode_image_local(image_content),
            _ => None,
        })
        .collect();

    let body = if let Some(entity) = markdown_for.get(&(entry_idx, 0)) {
        let images_for_handler = images;
        MarkdownElement::new(entity.clone(), style.clone())
            .on_url_click(move |url, window, cx| {
                // Custom URL scheme `spk-image://<idx>` is rewritten
                // by `clean_user_message_text`. Anything else is a
                // genuine link the user typed; defer to the system
                // browser via `cx.open_url`.
                if let Some(idx_str) = url.strip_prefix("spk-image://")
                    && let Ok(idx) = idx_str.parse::<usize>()
                    && let Some(image) = images_for_handler.get(idx).cloned()
                {
                    open_image_preview(image, window, cx);
                    return;
                }
                cx.open_url(url.as_ref());
            })
            .into_any_element()
    } else if text.is_empty() {
        Empty.into_any_element()
    } else {
        Label::new(text.clone())
            .size(LabelSize::Small)
            .into_any_element()
    };

    v_flex()
        .group(group_name.clone())
        .px_1()
        .mb_3()
        .child(
            // h_flex wrap so the bubble shrinks to content (no full-
            // panel-width slab). max_w(85%) caps long messages.
            h_flex().child(
                div()
                    .relative()
                    .max_w(relative(0.85))
                    .px_2p5()
                    .py_1()
                    .bg(bubble_bg)
                    .rounded_md()
                    .child(body)
                    .child(render_floating_copy_button(
                        SharedString::from(format!("copy-user-{entry_idx}")),
                        text,
                        group_name,
                    )),
            ),
        )
        .into_any_element()
}

/// Cleans a user message's merged-markdown source for display:
///   1. Strips the literal "`Image`" placeholder that
///      `acp_thread::ContentBlock::append` emits when merging image
///      chunks (we render images via clickable text spans, not as
///      inline thumbnails, so the placeholder is pure noise).
///   2. Rewrites the user-typed `[image #N]` placeholders into
///      markdown links of the form `[image #N](spk-image://<N-1>)`.
///      The Markdown widget paints them as clickable spans; our
///      `on_url_click` handler intercepts the `spk-image://` scheme
///      and opens an image-preview window for the matching chunk.
///   3. Collapses leftover double-blank lines so the bubble doesn't
///      grow an empty paragraph where `Image` used to live.
fn clean_user_message_text(text: &str) -> String {
    let stripped = text.replace("`Image`", "");
    let with_links = IMAGE_PLACEHOLDER_RE.replace_all(&stripped, |caps: &regex::Captures| {
        let n: usize = caps[1].parse().unwrap_or(1);
        let idx = n.saturating_sub(1);
        format!("[image #{n}](spk-image://{idx})")
    });
    let mut out = String::with_capacity(with_links.len());
    let mut blanks = 0;
    for line in with_links.lines() {
        let trimmed = line.trim_end();
        if trimmed.is_empty() {
            blanks += 1;
            if blanks <= 1 && !out.is_empty() {
                out.push('\n');
            }
        } else {
            blanks = 0;
            if !out.is_empty() && !out.ends_with('\n') {
                out.push('\n');
            }
            out.push_str(trimmed);
        }
    }
    out.trim_end().to_string()
}

/// `[image #N]` placeholder pattern injected by the compose paste
/// handler. The capture group is the 1-based image index.
static IMAGE_PLACEHOLDER_RE: std::sync::LazyLock<regex::Regex> =
    std::sync::LazyLock::new(|| {
        regex::Regex::new(r"\[image #(\d+)\]").expect("static regex compiles")
    });

/// Mirrors `acp_thread::ContentBlock::decode_image` (private upstream)
/// so we can re-decode image chunks at render time without exposing a
/// new `pub` surface in the acp_thread crate. Returns None on malformed
/// base64 / unsupported mime — caller falls back to the placeholder.
fn decode_image_local(
    image_content: &acp::ImageContent,
) -> Option<std::sync::Arc<gpui::Image>> {
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(image_content.data.as_bytes())
        .ok()?;
    let format = gpui::ImageFormat::from_mime_type(&image_content.mime_type)?;
    Some(std::sync::Arc::new(gpui::Image::from_bytes(format, bytes)))
}

/// Opens the given image in a centred OS popup window for full-size
/// inspection. Used by the chat thumbnail click handler.
fn open_image_preview(
    image: std::sync::Arc<gpui::Image>,
    window: &mut Window,
    cx: &mut App,
) {
    let display_size = window
        .display(cx)
        .or_else(|| cx.primary_display())
        .map(|d| d.bounds().size)
        .unwrap_or(gpui::Size {
            width: px(800.0),
            height: px(600.0),
        });
    let size = gpui::Size {
        width: display_size.width * 0.6,
        height: display_size.height * 0.7,
    };
    let bounds = gpui::WindowBounds::centered(size, cx);
    if let Err(err) = cx.open_window(
        gpui::WindowOptions {
            titlebar: Some(gpui::TitlebarOptions {
                title: Some("Image preview".into()),
                appears_transparent: false,
                traffic_light_position: None,
            }),
            window_bounds: Some(bounds),
            is_resizable: true,
            is_minimizable: true,
            kind: gpui::WindowKind::Normal,
            ..Default::default()
        },
        move |window, cx| {
            window.activate_window();
            cx.new(|_| ImagePreviewWindowView { image })
        },
    ) {
        log::error!("failed to open image preview window: {err:?}");
    }
}

struct ImagePreviewWindowView {
    image: std::sync::Arc<gpui::Image>,
}

impl Render for ImagePreviewWindowView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .size_full()
            .bg(cx.theme().colors().editor_background)
            .flex()
            .items_center()
            .justify_center()
            .child(
                gpui::img(self.image.clone())
                    .object_fit(gpui::ObjectFit::Contain)
                    .size_full(),
            )
    }
}

fn render_assistant_message(
    entry_idx: usize,
    message: &AssistantMessage,
    markdown_for: &HashMap<(usize, usize), Entity<Markdown>>,
    style: &MarkdownStyle,
    _assistant_label: &SharedString,
    cx: &App,
) -> AnyElement {
    let group_name = SharedString::from(format!("assistant-msg-{entry_idx}"));
    // No "<Adapter>" header above assistant messages either — the absence of
    // the user bubble tint is the role cue. The status row at the top of the
    // panel still shows which adapter owns the active session, so users who
    // need to know which AI is talking still have that signal.
    // Assistant text starts further LEFT than user-bubble inner text
    // on purpose: the offset is the role cue. Aligning them (a
    // previous attempt did `pl_3p5` to match the bubble's inner pad)
    // made the conversation read as one undifferentiated column.
    // `mb_3` mirrors the user bubble's bottom margin so messages
    // breathe without the chunky `my_0p5` gaps.
    let mut container = v_flex()
        .group(group_name.clone())
        .relative()
        .px_1()
        .mb_3();  // 12 px — a hair more than the user bubble's mb_3 above; both stay synced.
    // While the agent is mid-turn we may have only `Thought` chunks —
    // show them so the user sees activity. Once any real `Message`
    // chunk arrives the thoughts become noise (Claude was reasoning)
    // and we drop them. Matches Cursor / upstream Zed AgentPanel which
    // collapse reasoning tokens once the answer starts streaming.
    let has_message = message
        .chunks
        .iter()
        .any(|c| matches!(c, AssistantMessageChunk::Message { .. }));
    let mut span_idx = 0;
    // Accumulate the user-visible markdown source across non-thought
    // chunks for the footer copy button — matches what's painted, no
    // hidden reasoning leaks into the clipboard.
    let mut visible_text = String::new();
    for chunk in &message.chunks {
        let (is_thought, block) = match chunk {
            AssistantMessageChunk::Message { block } => (false, block),
            AssistantMessageChunk::Thought { block } if !has_message => (true, block),
            AssistantMessageChunk::Thought { .. } => continue,
        };
        let text = content_block_text(block, cx);
        if !text.is_empty() {
            let element = render_span((entry_idx, span_idx), &text, markdown_for, style);
            if is_thought {
                container = container.child(
                    div()
                        .child(
                            Label::new("thinking…")
                                .size(LabelSize::XSmall)
                                .color(Color::Muted)
                                .italic(),
                        )
                        .child(element),
                );
            } else {
                if !visible_text.is_empty() {
                    visible_text.push_str("\n\n");
                }
                visible_text.push_str(&text);
                container = container.child(element);
            }
            span_idx += 1;
        }
    }
    if !visible_text.is_empty() {
        container = container.child(render_floating_copy_button(
            SharedString::from(format!("copy-assistant-{entry_idx}")),
            visible_text,
            group_name,
        ));
    }
    container.into_any_element()
}

/// Bottom-right copy affordance, absolute-positioned so it overlays
/// the parent message bubble's lower-right corner instead of sitting
/// on its own row beneath. The previous footer-row layout reserved
/// ~24 px of vertical space whether or not the user hovered, smearing
/// every message with empty padding the user never asked for.
///
/// Caller must wrap the bubble (or assistant container) in `relative()`
/// so the absolute child anchors correctly.
fn render_floating_copy_button(
    button_id: SharedString,
    source: String,
    group_name: SharedString,
) -> impl IntoElement {
    div()
        .absolute()
        .bottom_0p5()
        .right_0p5()
        .child(
            CopyButton::new(button_id, source)
                .icon_size(IconSize::XSmall)
                .tooltip_label("Copy as markdown")
                .visible_on_hover(group_name),
        )
}

fn render_tool_call(
    entry_idx: usize,
    call: &ToolCall,
    markdown_for: &HashMap<(usize, usize), Entity<Markdown>>,
    style: &MarkdownStyle,
    cx: &App,
) -> AnyElement {
    let label_text = call.label.read(cx).source().to_string();
    let status_text = tool_call_status_text(&call.status);
    let status_color = match call.status {
        ToolCallStatus::Failed => Color::Error,
        ToolCallStatus::Rejected | ToolCallStatus::Canceled => Color::Warning,
        ToolCallStatus::Completed => Color::Success,
        _ => Color::Muted,
    };

    let mut container = v_flex()
        .gap_0p5()
        .my_1()
        .pl_2()
        .border_l_2()
        .border_color(cx.theme().colors().border_variant)
        .child(
            h_flex()
                .gap_1p5()
                .items_center()
                .child(
                    Icon::new(IconName::ToolHammer)
                        .size(IconSize::XSmall)
                        .color(Color::Muted),
                )
                .child(render_span((entry_idx, 0), &label_text, markdown_for, style))
                .child(
                    Label::new(status_text)
                        .size(LabelSize::XSmall)
                        .color(status_color),
                ),
        );

    let mut span_idx = 1;
    for content in &call.content {
        let summary = tool_call_content_summary(call, content, cx);
        if !summary.is_empty() {
            container = container.child(div().child(render_span(
                (entry_idx, span_idx),
                &summary,
                markdown_for,
                style,
            )));
            span_idx += 1;
        }
    }

    container.into_any_element()
}

/// Produces the markdown source for one item of a tool call's `content`.
/// Shared by `entry_text_spans` (the find-bar / markdown-cache pre-pass)
/// and `render_tool_call` so they always agree — historically they
/// diverged and the cache won, sticking the placeholder text on screen
/// even after the real output arrived in `raw_output`. Special-cases
/// `Terminal` blocks: when the inner terminal has no bytes (claude-acp
/// often skips meta.terminal_output for short/synchronous commands)
/// falls back to the call's `raw_output` field, which is where the
/// captured stdout typically ends up in those cases.
fn tool_call_content_summary(
    call: &ToolCall,
    content: &ToolCallContent,
    cx: &App,
) -> String {
    let raw = match content {
        ToolCallContent::ContentBlock(block) => content_block_text(block, cx),
        ToolCallContent::Diff(diff) => diff_summary_markdown(diff, cx),
        ToolCallContent::Terminal(terminal) => {
            let primary = terminal_output_markdown(terminal, cx);
            if primary.contains("(no output yet)") {
                raw_output_fallback_markdown(call.raw_output.as_ref())
                    .unwrap_or(primary)
            } else {
                primary
            }
        }
    };
    truncate_tool_summary(&raw)
}

/// Trims tool-call output for the inline chat view — long Read / Bash /
/// Diff results would otherwise push the rest of the conversation off the
/// screen on every turn. Caps at `MAX_LINES` and appends a `… (+N more
/// lines)` hint matching the claude-code CLI convention. The full content
/// is still available via the original tool / file in the editor; this is
/// just the chat-side preview.
fn truncate_tool_summary(text: &str) -> String {
    const MAX_LINES: usize = 15;
    let mut lines = text.lines();
    let head: Vec<&str> = lines.by_ref().take(MAX_LINES).collect();
    let remaining = lines.count();
    if remaining == 0 {
        return text.to_string();
    }
    // Preserve the closing fence if the truncated output started one,
    // otherwise the markdown widget would parse the rest of the message
    // as a runaway code block.
    let opens_fence = head
        .iter()
        .filter(|line| line.starts_with("```") || line.starts_with("````"))
        .count()
        % 2
        == 1;
    let mut out = head.join("\n");
    if opens_fence {
        // Match whichever fence width opened (prefer 4 to be safe).
        let fence = if head
            .iter()
            .any(|line| line.trim_start().starts_with("````"))
        {
            "````"
        } else {
            "```"
        };
        out.push('\n');
        out.push_str(fence);
    }
    out.push_str(&format!("\n\n_… (+{remaining} more lines)_"));
    out
}

/// Try to coerce a tool call's `raw_output` JSON into something printable
/// in the chat. Strings get returned as-is, objects/arrays land as a JSON
/// code block. Returns None when there's nothing usable (Null / empty
/// string / empty object) so the caller can fall through to its own
/// placeholder.
fn raw_output_fallback_markdown(raw: Option<&serde_json::Value>) -> Option<String> {
    let raw = raw?;
    match raw {
        serde_json::Value::Null => None,
        serde_json::Value::String(s) => {
            let trimmed = s.trim_end();
            if trimmed.is_empty() {
                return None;
            }
            // 4-backtick fence so embedded triple-backticks in the
            // captured stdout don't break the markdown widget. Same
            // trick `terminal_output_markdown` uses.
            Some(format!("````\n{trimmed}\n````"))
        }
        serde_json::Value::Bool(b) => Some(b.to_string()),
        serde_json::Value::Number(n) => Some(n.to_string()),
        other => {
            let pretty = serde_json::to_string_pretty(other).ok()?;
            if pretty.trim().is_empty()
                || pretty.trim() == "{}"
                || pretty.trim() == "[]"
            {
                return None;
            }
            Some(format!("```json\n{pretty}\n```"))
        }
    }
}

/// Format a `Diff` tool-call content as a `diff`-fenced markdown block —
/// the markdown widget syntax-highlights `+` lines green and `-` lines
/// red, matching the inline-diff style claude-code shows in the CLI.
/// Includes a one-line "Edited <path>" header + `Δ +X / -Y` summary so
/// the cost of the change is visible without expanding. The full diff is
/// still passed through `truncate_tool_summary`, capping the body at the
/// same line limit as other tool output.
fn diff_summary_markdown(diff: &Entity<acp_thread::Diff>, cx: &App) -> String {
    let diff = diff.read(cx);
    let path = diff.file_path(cx).unwrap_or_else(|| "file".to_string());
    let old_text = diff.base_text().to_string();
    let new_text = diff.buffer().read(cx).text();
    let body = language::unified_diff(&old_text, &new_text);
    if body.is_empty() {
        return format!("**Edited** `{path}`");
    }
    let added = body.lines().filter(|l| l.starts_with('+')).count();
    let removed = body.lines().filter(|l| l.starts_with('-')).count();
    format!(
        "**Edited** `{path}` · +{added} / −{removed}\n```diff\n{body}\n```"
    )
}

/// Render `Terminal` tool-call content as fenced code in markdown so the
/// existing markdown widget paints it monospaced (matches how command
/// labels are already rendered above the output). For an empty / still-
/// starting terminal returns a hint placeholder so the user sees the
/// command body has not produced bytes yet, instead of a blank gap.
/// Truncates to keep the markdown parser snappy on long outputs — tighter
/// than the agent-side byte limit on purpose; the user reads "the gist",
/// not the full stream, in this inline view.
fn terminal_output_markdown(terminal: &Entity<acp_thread::Terminal>, cx: &App) -> String {
    const MAX_BYTES: usize = 8 * 1024;
    let term = terminal.read(cx);
    let mut content = if let Some(output) = term.output() {
        output.content.clone()
    } else {
        term.inner().read(cx).get_content()
    };
    let was_truncated = content.len() > MAX_BYTES;
    if was_truncated {
        let mut cut = MAX_BYTES;
        while cut > 0 && !content.is_char_boundary(cut) {
            cut -= 1;
        }
        content.truncate(cut);
    }
    let trimmed = content.trim_end();
    if trimmed.is_empty() {
        return "_(no output yet)_".to_string();
    }
    // 4-backtick fence so an embedded ```…``` in the captured output (e.g.
    // an agent that ran `cat README.md`) does not close our fence early.
    let mut out = String::with_capacity(trimmed.len() + 16);
    out.push_str("````\n");
    out.push_str(trimmed);
    if !trimmed.ends_with('\n') {
        out.push('\n');
    }
    out.push_str("````");
    if was_truncated {
        out.push_str("\n_(output truncated)_");
    }
    out
}

fn render_plan(
    entry_idx: usize,
    entries: &[PlanEntry],
    markdown_for: &HashMap<(usize, usize), Entity<Markdown>>,
    style: &MarkdownStyle,
    cx: &App,
) -> AnyElement {
    let mut container = v_flex()
        .gap_0p5()
        .my_1()
        .pl_2()
        .border_l_2()
        .border_color(cx.theme().colors().border_variant)
        .child(
            h_flex()
                .gap_1p5()
                .items_center()
                .child(
                    Icon::new(IconName::ListTree)
                        .size(IconSize::XSmall)
                        .color(Color::Muted),
                )
                .child(render_span((entry_idx, 0), "Plan", markdown_for, style)),
        );
    for (i, _entry) in entries.iter().enumerate() {
        let span_idx = 1 + i;
        // Bullet prefix is now part of the span text (see
        // entry_text_spans), so the rendered markdown already includes
        // it — list items render as a list line.
        container = container.child(render_span(
            (entry_idx, span_idx),
            "",
            markdown_for,
            style,
        ));
    }
    container.into_any_element()
}

fn content_block_text(block: &ContentBlock, cx: &App) -> String {
    block.to_markdown(cx).to_string()
}

/// Detached OS popup window for editing the chat draft as a long-form
/// document. Renders a roomy multi-line editor + Save / Cancel footer in
/// its own resizable + movable OS window so the user can keep reading the
/// AI conversation or browsing project files while the popup is open.
/// Save writes the popup text back to the original compose editor;
/// Cancel and the OS close button both discard the change.
pub struct ExpandedComposeWindowView {
    editor: Entity<editor::Editor>,
    target: gpui::WeakEntity<editor::Editor>,
    owner: gpui::WeakEntity<SolutionSessionView>,
}

const EXPANDED_COMPOSE_DEFAULT_W: f32 = 1080.0;
/// Fallback height when no display is available (off-screen / headless).
/// In normal usage we open at `EXPANDED_COMPOSE_HEIGHT_RATIO` of the
/// current display's height.
const EXPANDED_COMPOSE_DEFAULT_H: f32 = 720.0;
const EXPANDED_COMPOSE_HEIGHT_RATIO: f32 = 0.8;

impl ExpandedComposeWindowView {
    fn new(
        initial_text: String,
        target: gpui::WeakEntity<editor::Editor>,
        owner: gpui::WeakEntity<SolutionSessionView>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let editor = cx.new(|cx| {
            let mut e = editor::Editor::multi_line(window, cx);
            e.set_text(initial_text, window, cx);
            e.set_show_gutter(false, cx);
            e.set_show_line_numbers(false, cx);
            e.set_show_scrollbars(true, cx);
            e
        });
        Self {
            editor,
            target,
            owner,
        }
    }

    fn save(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let text = self.editor.read(cx).text(cx);
        if let Some(target) = self.target.upgrade() {
            target.update(cx, |editor, cx| {
                editor.set_text(text, window, cx);
            });
        }
        self.dismiss(window, cx);
    }

    fn dismiss(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // Clear the parent view's handle so the inline compose row can
        // swap back to the editor and the next "expand" click opens a
        // fresh window instead of trying to revive this one.
        if let Some(owner) = self.owner.upgrade() {
            owner
                .update(cx, |owner, cx| {
                    owner.expanded_window = None;
                    cx.notify();
                });
        }
        window.remove_window();
    }
}

impl Focusable for ExpandedComposeWindowView {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.editor.focus_handle(cx)
    }
}

impl Render for ExpandedComposeWindowView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .key_context("ExpandedComposeWindow")
            .size_full()
            .p_3()
            .gap_3()
            .bg(cx.theme().colors().elevated_surface_background)
            .child(
                div()
                    .id("expanded-compose-editor-frame")
                    .flex_1()
                    .min_h_0()
                    .border_1()
                    .border_color(cx.theme().colors().border_variant)
                    .rounded_md()
                    .bg(cx.theme().colors().editor_background)
                    .p_2()
                    .overflow_hidden()
                    .child(self.editor.clone()),
            )
            .child(
                h_flex()
                    .justify_end()
                    .gap_2()
                    .child(
                        ui::Button::new("expanded-compose-cancel", "Cancel")
                            .style(ui::ButtonStyle::Subtle)
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.dismiss(window, cx);
                            })),
                    )
                    .child(
                        ui::Button::new("expanded-compose-save", "Save")
                            .style(ui::ButtonStyle::Filled)
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.save(window, cx);
                            })),
                    ),
            )
    }
}

/// Editor `CompletionProvider` that surfaces ACP slash commands while the
/// user types `/<query>` at the very start of the compose buffer. Only
/// triggers on row 0 / column 0 — slash commands are sent as the entire
/// first line of a prompt, not embedded mid-message.
///
/// Reads the live `available_commands` list off the session's `AcpThread`
/// on each invocation so a freshly-arrived `AvailableCommandsUpdate` is
/// reflected immediately without a separate subscription (the popup is
/// only built on demand when the user types `/`).
struct SlashCommandsProvider {
    session: WeakEntity<SolutionSession>,
}

impl SlashCommandsProvider {
    fn read_commands(&self, cx: &App) -> Vec<acp::AvailableCommand> {
        let Some(session) = self.session.upgrade() else {
            return Vec::new();
        };
        session
            .read(cx)
            .acp_thread
            .as_ref()
            .map(|thread| thread.read(cx).available_commands().to_vec())
            .unwrap_or_default()
    }
}

/// First non-empty line of `text`, trimmed and clamped to ~80 characters
/// with an ellipsis when truncation actually drops content. Returns
/// `None` for empty / whitespace-only input so callers can drop the
/// documentation field entirely.
fn first_line_summary(text: &str) -> Option<String> {
    const MAX_LEN: usize = 80;
    let line = text.lines().map(str::trim).find(|line| !line.is_empty())?;
    let mut buf = String::with_capacity(MAX_LEN.min(line.len()) + 1);
    let mut truncated = false;
    for (count, ch) in line.chars().enumerate() {
        if count == MAX_LEN {
            truncated = true;
            break;
        }
        buf.push(ch);
    }
    if truncated || text.lines().filter(|l| !l.trim().is_empty()).count() > 1 {
        buf.push('…');
    }
    Some(buf)
}

/// Returns the prefix of the buffer up to `position` if and only if the
/// cursor is on the first line, the line begins with `/`, and no
/// whitespace has been typed yet (i.e. we are still completing the
/// command name, not its argument).
fn slash_query_prefix(buffer: &Buffer, position: Point) -> Option<String> {
    if position.row != 0 {
        return None;
    }
    let line_start = Point::new(0, 0);
    let prefix: String = buffer.text_for_range(line_start..position).collect();
    if !prefix.starts_with('/') {
        return None;
    }
    if prefix[1..].chars().any(|c| c.is_whitespace()) {
        return None;
    }
    Some(prefix)
}

impl EditorCompletionProvider for SlashCommandsProvider {
    fn completions(
        &self,
        buffer: &Entity<Buffer>,
        buffer_position: Anchor,
        _trigger: CompletionContext,
        _window: &mut Window,
        cx: &mut Context<editor::Editor>,
    ) -> Task<AnyhowResult<Vec<CompletionResponse>>> {
        let prefix = buffer.update(cx, |buffer, _| {
            let position = buffer_position.to_point(buffer);
            slash_query_prefix(buffer, position)
        });
        let Some(prefix) = prefix else {
            return Task::ready(Ok(Vec::new()));
        };
        let commands = self.read_commands(cx);
        if commands.is_empty() {
            return Task::ready(Ok(Vec::new()));
        }
        let snapshot = buffer.read(cx).snapshot();
        let source_range =
            snapshot.anchor_before(0)..snapshot.anchor_after(prefix.len());
        let query_lower = prefix[1..].to_lowercase();
        let completions: Vec<Completion> = commands
            .into_iter()
            .filter(|cmd| {
                query_lower.is_empty() || cmd.name.to_lowercase().contains(&query_lower)
            })
            .map(|cmd| {
                let new_text = format!("/{} ", cmd.name);
                let label = CodeLabel::plain(format!("/{}", cmd.name), None);
                // The completions popup paints `SingleLine` documentation
                // with a no-wrap `Label`, but if the agent shipped a
                // multi-line description the row blows up vertically and
                // shoulder-checks the items below. Trim to the first
                // non-empty line and cap at ~80 chars + ellipsis so each
                // row stays exactly one text line tall.
                let documentation = first_line_summary(&cmd.description)
                    .map(|line| CompletionDocumentation::SingleLine(line.into()));
                Completion {
                    replace_range: source_range.clone(),
                    new_text,
                    label,
                    documentation,
                    source: CompletionSource::Custom,
                    icon_path: None,
                    match_start: None,
                    snippet_deduplication_key: None,
                    insert_text_mode: None,
                    confirm: None,
                }
            })
            .collect();
        Task::ready(Ok(vec![CompletionResponse {
            completions,
            display_options: CompletionDisplayOptions {
                dynamic_width: true,
            },
            // `true` forces the editor to re-invoke `completions()` on every
            // keystroke instead of reusing the cached list. We need that
            // because `filter_completions: false` short-circuits the
            // editor's built-in client-side filter — without a fresh
            // call we'd keep showing the unfiltered popup as the user
            // narrows the query.
            is_incomplete: true,
        }]))
    }

    fn is_completion_trigger(
        &self,
        buffer: &Entity<Buffer>,
        position: Anchor,
        _text: &str,
        _trigger_in_words: bool,
        cx: &mut Context<editor::Editor>,
    ) -> bool {
        let buffer = buffer.read(cx);
        let pos = position.to_point(buffer);
        slash_query_prefix(buffer, pos).is_some()
    }

    fn filter_completions(&self) -> bool {
        // Filtering happens above (case-insensitive substring match) so the
        // editor's default fuzzy filter doesn't drop entries we already
        // matched, and so descriptions (which we don't want to fuzzy on)
        // can stay in the documentation slot.
        false
    }

    fn sort_completions(&self) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn collect(text: &str, query: &str) -> Vec<Range<usize>> {
        let mut out = Vec::new();
        find_all(text, &query.to_lowercase(), |r| out.push(r));
        out
    }

    #[test]
    fn find_all_basic() {
        assert_eq!(collect("hello world", "hello"), vec![0..5]);
        assert_eq!(
            collect("hello hello hello", "hello"),
            vec![0..5, 6..11, 12..17]
        );
    }

    #[test]
    fn find_all_case_insensitive() {
        assert_eq!(collect("Hello World", "hello"), vec![0..5]);
        assert_eq!(collect("HELLO HeLLo hello", "Hello"), vec![0..5, 6..11, 12..17]);
    }

    #[test]
    fn find_all_no_match() {
        assert_eq!(collect("abc", "xyz"), Vec::<Range<usize>>::new());
    }

    #[test]
    fn find_all_empty_query() {
        assert_eq!(collect("anything", ""), Vec::<Range<usize>>::new());
    }

    #[test]
    fn find_all_overlapping_advances_by_query_len() {
        // Advances past the match — does NOT find overlapping matches. This
        // mirrors common find-bar behavior (Cursor / VS Code) where typing
        // "aa" in "aaaa" highlights two non-overlapping pairs at 0..2 and
        // 2..4 rather than three at 0..2, 1..3, 2..4.
        assert_eq!(collect("aaaa", "aa"), vec![0..2, 2..4]);
    }

    #[test]
    fn matches_for_span_filters_and_finds_selected() {
        let matches = vec![
            FindMatch { entry_idx: 0, span_idx: 0, range: 0..5 },
            FindMatch { entry_idx: 0, span_idx: 1, range: 0..3 },
            FindMatch { entry_idx: 1, span_idx: 0, range: 5..8 },
            FindMatch { entry_idx: 0, span_idx: 0, range: 10..15 },
        ];
        let (ranges, sel) = matches_for_span(&matches, Some(3), 0, 0);
        assert_eq!(ranges, vec![0..5, 10..15]);
        assert_eq!(sel, Some(1));

        let (ranges, sel) = matches_for_span(&matches, Some(3), 1, 0);
        assert_eq!(ranges, vec![5..8]);
        assert_eq!(sel, None);

        let (ranges, sel) = matches_for_span(&matches, Some(2), 1, 0);
        assert_eq!(ranges, vec![5..8]);
        assert_eq!(sel, Some(0));
    }
}
