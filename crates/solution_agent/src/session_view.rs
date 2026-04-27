use std::collections::HashMap;
use std::ops::Range;

use acp_thread::{
    AgentThreadEntry, AssistantMessage, AssistantMessageChunk, ContentBlock, PlanEntry, ToolCall,
    ToolCallContent, ToolCallStatus, UserMessage,
};
use agent_client_protocol::schema as acp;
use base64::Engine;
use gpui::{
    AnyElement, App, ClipboardEntry, Context, DragMoveEvent, Empty, Entity, EventEmitter,
    ExternalPaths, FocusHandle, Focusable, InteractiveElement as _, IntoElement, MouseButton,
    MouseDownEvent, ParentElement, Pixels, Render, SharedString, Styled,
    StatefulInteractiveElement as _, Subscription, WeakEntity, Window, div, px, relative,
};
use markdown::{Markdown, MarkdownElement, MarkdownFont, MarkdownStyle};
use ui::prelude::*;
use ui::{IconButton, IconName, Label, Tooltip};
use workspace::Workspace;

use crate::actions::{FindClose, FindInSession, FindNextMatch, FindPreviousMatch};
use crate::model::{SolutionSession, SolutionSessionId};
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
        let entity = cx.new(|cx| Markdown::new(source.clone(), None, None, cx));
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

    fn submit_compose_now(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let content = self.compose_editor.read(cx).text(cx);
        if content.trim().is_empty() && self.pending_images.is_empty() {
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
                    .p_3()
                    .overflow_y_scroll()
                    .track_scroll(&self.conversation_scroll)
                    .on_scroll_wheel(cx.listener(|this, _ev, _window, cx| {
                        // Any wheel input from the user means they're taking
                        // manual control. Detach from the bottom; the user
                        // explicitly re-attaches via the "Jump to latest"
                        // button or by sending a message. Re-checking the
                        // post-scroll offset to auto-restick at the bottom
                        // would race with `scroll_to_bottom` and produce a
                        // snap-back-down loop.
                        if this.stuck_to_bottom {
                            this.stuck_to_bottom = false;
                            cx.notify();
                        }
                    }));
                if let Some(thread) = session.acp_thread.as_ref() {
                    let thread = thread.read(cx);
                    let entries = thread.entries();
                    if entries.is_empty() {
                        body = body.child(Label::new("(no messages yet)").size(LabelSize::Small));
                    } else {
                        for (idx, entry) in entries.iter().enumerate() {
                            body = body.child(render_entry(
                                idx,
                                entry,
                                &markdown_for,
                                &markdown_style,
                                &assistant_label,
                                cx,
                            ));
                        }
                    }
                } else {
                    body = body.child(Label::new("(no thread yet)").size(LabelSize::Small));
                }
                div()
                    .relative()
                    .flex_1()
                    .min_h_0()
                    .child(body)
                    .when(!stuck, |this| this.child(self.render_jump_to_latest(cx)))
            })
            .child(
                // Resize handle: thin, full-width strip on top of the
                // compose row. `on_mouse_down` snapshots the starting Y +
                // height so `on_drag_move` (registered on the root) can
                // compute the live new height. The handle is `flex_none`
                // so flex layout doesn't squeeze it. `hover` lights it up
                // so users discover that it's draggable.
                div()
                    .id("solution-session-compose-resize")
                    .flex_none()
                    .h(px(5.0))
                    .w_full()
                    .cursor_row_resize()
                    .hover(|s| s.bg(cx.theme().colors().border_focused))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, e: &MouseDownEvent, _, _| {
                            this.resize_start_y = e.position.y;
                            this.resize_start_height = this.compose_height;
                        }),
                    )
                    .on_drag(DraggedComposeHandle, |handle, _, _, cx| {
                        cx.new(|_| handle.clone())
                    }),
            )
            .child({
                let mut compose_row = div()
                    .flex()
                    .flex_none()
                    .h(self.compose_height)
                    .p_2()
                    .gap_2()
                    .border_t_1()
                    .border_color(cx.theme().colors().border)
                    // Match the editor's own background colour so the
                    // compose area reads as a single input rectangle, not
                    // a panel-bg strip with a darker editor block stacked
                    // inside it (which is what showed up after switching
                    // to `multi_line`: editor renders with
                    // `editor_background`, but the row around it kept
                    // panel_bg, producing a visible seam).
                    .bg(cx.theme().colors().editor_background)
                    .child(
                        // The editor (now `multi_line`) fills this wrapper
                        // vertically, so a click anywhere in the compose
                        // area lands directly on the editor element —
                        // editor handles focus, cursor placement, and key
                        // events natively. No more parent-level click
                        // shims needed.
                        div()
                            .flex_1()
                            .h_full()
                            .child(self.compose_editor.clone()),
                    )
                    .child(
                        ui::Button::new("solution-session-send", "Send").on_click(cx.listener(
                            |this, _, window, cx| {
                                this.submit_compose_now(window, cx);
                            },
                        )),
                    );
                if pending_image_count > 0 {
                    compose_row = compose_row.child(
                        Label::new(format!(
                            "{pending_image_count} image{} attached",
                            if pending_image_count == 1 { "" } else { "s" }
                        ))
                        .size(LabelSize::XSmall),
                    );
                }
                compose_row
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
        AgentThreadEntry::UserMessage(message) => vec![content_block_text(&message.content, cx)],
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
                let summary = match content {
                    ToolCallContent::ContentBlock(block) => content_block_text(block, cx),
                    ToolCallContent::Diff(_) => "[diff]".to_string(),
                    ToolCallContent::Terminal(_) => "[terminal output]".to_string(),
                };
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
    cx: &App,
) -> AnyElement {
    match entry {
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
    }
}

fn render_user_message(
    entry_idx: usize,
    message: &UserMessage,
    markdown_for: &HashMap<(usize, usize), Entity<Markdown>>,
    style: &MarkdownStyle,
    cx: &App,
) -> AnyElement {
    let text = content_block_text(&message.content, cx);
    let bubble_bg = cx.theme().colors().text_accent.opacity(0.12);
    v_flex()
        .gap_1()
        .px_1()
        .my_2()
        .child(
            h_flex()
                .gap_1p5()
                .items_center()
                .child(
                    Icon::new(IconName::Person)
                        .size(IconSize::XSmall)
                        .color(Color::Muted),
                )
                .child(
                    Label::new("You")
                        .size(LabelSize::XSmall)
                        .color(Color::Muted),
                ),
        )
        .child(
            // h_flex wrap so the bubble shrinks to content (no full-
            // panel-width slab). max_w(85%) caps long messages.
            h_flex().child(
                div()
                    .max_w(relative(0.85))
                    .px_3()
                    .py_1p5()
                    .bg(bubble_bg)
                    .rounded_md()
                    .child(render_span((entry_idx, 0), &text, markdown_for, style)),
            ),
        )
        .into_any_element()
}

fn render_assistant_message(
    entry_idx: usize,
    message: &AssistantMessage,
    markdown_for: &HashMap<(usize, usize), Entity<Markdown>>,
    style: &MarkdownStyle,
    assistant_label: &SharedString,
    cx: &App,
) -> AnyElement {
    let mut container = v_flex()
        .gap_1()
        .px_1()
        .my_2()
        .child(
            h_flex()
                .gap_1p5()
                .items_center()
                .child(
                    Icon::new(IconName::Sparkle)
                        .size(IconSize::XSmall)
                        .color(Color::Accent),
                )
                .child(
                    Label::new(assistant_label.clone())
                        .size(LabelSize::XSmall)
                        .color(Color::Muted),
                ),
        );
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
                container = container.child(element);
            }
            span_idx += 1;
        }
    }
    container.into_any_element()
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
        let summary = match content {
            ToolCallContent::ContentBlock(block) => content_block_text(block, cx),
            ToolCallContent::Diff(_) => "[diff]".to_string(),
            ToolCallContent::Terminal(_) => "[terminal output]".to_string(),
        };
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
