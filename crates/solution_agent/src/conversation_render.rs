//! Pure rendering helpers and shared types for the Solution session conversation view. Extracted from session_view.rs to keep that file focused on view state + input handling.

use std::collections::HashMap;
use std::ops::Range;

use acp_thread::{
    AcpThread, AgentThreadEntry, AssistantMessage, AssistantMessageChunk, ContentBlock,
    PermissionOptions, PlanEntry, SelectedPermissionOutcome, SelectedPermissionParams, ToolCall,
    ToolCallContent, ToolCallStatus, UserMessage, UserMessageId,
};
use agent_client_protocol::schema as acp;
use base64::Engine;
use chrono::TimeZone as _;
use gpui::{
    AnyElement, App, Context, Empty, Entity, InteractiveElement as _, IntoElement, ParentElement,
    Render, SharedString, StatefulInteractiveElement as _, Styled, WeakEntity, Window, div, px,
    relative,
};
use markdown::{Markdown, MarkdownElement, MarkdownStyle};
use ui::prelude::*;
use ui::{ContextMenu, CopyButton, IconName, Label};
use util::ResultExt as _;

#[derive(Clone, Debug)]
pub(crate) struct FindMatch {
    pub(crate) entry_idx: usize,
    pub(crate) span_idx: usize,
    pub(crate) range: Range<usize>,
}

/// Pure backward-walk that computes, for each entry index, the id of the
/// next user message *after* it (the rewind target). Caller pre-projects
/// the entries list to `Option<UserMessageId>` per slot — `Some(id)` for a
/// user message that carries an id, `None` for everything else (assistant,
/// tool, plan, or a user message without an id).
///
/// At index `i` the result holds:
///   - `None` if `user_ids[i].is_some()` — rewinding TO a user message
///     means truncating its earlier turn; the message itself is never its
///     own rewind target. Also `None` past the last user message in the
///     conversation (no downstream user message exists).
///   - `Some(id)` of the next downstream user message otherwise.
///
/// O(N) once per thread mutation, replacing the previous O(N²) per-render
/// forward scan that lived inside the conversation render loop.
pub(crate) fn compute_rewind_table(
    user_ids: &[Option<UserMessageId>],
) -> Vec<Option<UserMessageId>> {
    let mut table = vec![None; user_ids.len()];
    let mut current: Option<UserMessageId> = None;
    for idx in (0..user_ids.len()).rev() {
        if let Some(id) = &user_ids[idx] {
            current = Some(id.clone());
            continue;
        }
        table[idx] = current.clone();
    }
    table
}

/// Per-entry text spans used by the find bar.
///
/// MUST iterate the entry in the same order as `render_*` functions emit
/// labels, so `(entry_idx, span_idx)` produced by `recompute_matches` lines
/// up with the label rendered for that span. If you add or reorder labels
/// in a render function, mirror the change here or matches will be applied
/// to the wrong line.
pub(crate) fn entry_text_spans(entry: &AgentThreadEntry, cx: &App) -> Vec<String> {
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
pub(crate) fn find_all(text: &str, query_lower: &str, mut emit: impl FnMut(Range<usize>)) {
    if query_lower.is_empty() {
        return;
    }
    let haystack = text.to_lowercase();
    let mut start = 0;
    while let Some(rel) = haystack[start..].find(query_lower) {
        let abs = start + rel;
        emit(abs..abs + query_lower.len());
        start = abs + query_lower.len();
    }
}

pub(crate) fn tool_call_status_text(status: &ToolCallStatus) -> &'static str {
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

/// A single clickable authorization choice flattened out of the
/// `PermissionOptions` the agent attached to a `WaitingForConfirmation`
/// tool call. Carries everything the render layer needs to draw a button
/// and everything the click handler needs to rebuild a
/// `SelectedPermissionOutcome` at click time (the outcome itself isn't
/// `Clone`, so we keep the raw pieces instead).
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct PermissionButton {
    pub(crate) option_id: acp::PermissionOptionId,
    pub(crate) label: SharedString,
    pub(crate) kind: acp::PermissionOptionKind,
    /// Sub-patterns to attach as `SelectedPermissionParams::Terminal` when
    /// answering — only ever non-empty for `Dropdown*` choices that carry
    /// terminal command patterns.
    pub(crate) patterns: Vec<String>,
}

impl PermissionButton {
    /// True for allow-flavoured kinds — used by the renderer to pick a
    /// filled/accent button vs a subtle one.
    pub(crate) fn is_allow(&self) -> bool {
        matches!(
            self.kind,
            acp::PermissionOptionKind::AllowOnce | acp::PermissionOptionKind::AllowAlways
        )
    }

    /// Rebuild the answer to hand to `AcpThread::authorize_tool_call`.
    pub(crate) fn outcome(&self) -> SelectedPermissionOutcome {
        let params = if self.patterns.is_empty() {
            None
        } else {
            Some(SelectedPermissionParams::Terminal {
                patterns: self.patterns.clone(),
            })
        };
        SelectedPermissionOutcome::new(self.option_id.clone(), self.kind).params(params)
    }
}

/// Pick the button to use when auto-resolving a pending authorization as a
/// rejection (the queue path resolves a `WaitingForConfirmation` tool call
/// before flushing a queued message — see `queue::pending_authorization_reject`).
/// Prefers `RejectOnce`, falling back to any non-allow button. Returns `None`
/// when the options offer ONLY allow-flavoured buttons: picking one would
/// silently AUTO-APPROVE the tool call, so a stuck turn is the safer failure.
pub(crate) fn pick_reject_button(options: &PermissionOptions) -> Option<PermissionButton> {
    let buttons = permission_buttons(options);
    buttons
        .iter()
        .find(|button| button.kind == acp::PermissionOptionKind::RejectOnce)
        .or_else(|| buttons.iter().find(|button| !button.is_allow()))
        .cloned()
}

/// Flatten a `PermissionOptions` into the list of buttons to render, in
/// display order. Pure (no `cx`) so it can be unit-tested and reused by
/// the future wire layer.
///
/// v1 simplification: the `Dropdown`/`DropdownWithPatterns` variants are
/// rendered as a flat pair of buttons per choice (its allow + deny
/// `PermissionOption`), reusing the choice's `sub_patterns` so a terminal
/// "always allow these commands" choice still answers correctly. The
/// pattern-picker UI (per-pattern checkboxes from `DropdownWithPatterns`)
/// is intentionally NOT rendered here — answering a dropdown choice
/// applies all of its `sub_patterns`.
pub(crate) fn permission_buttons(options: &PermissionOptions) -> Vec<PermissionButton> {
    let from_option = |option: &acp::PermissionOption, patterns: Vec<String>| PermissionButton {
        option_id: option.option_id.clone(),
        label: SharedString::from(option.name.clone()),
        kind: option.kind,
        patterns,
    };
    match options {
        PermissionOptions::Flat(options) => options
            .iter()
            .map(|option| from_option(option, Vec::new()))
            .collect(),
        PermissionOptions::Dropdown(choices)
        | PermissionOptions::DropdownWithPatterns { choices, .. } => choices
            .iter()
            .flat_map(|choice| {
                [
                    from_option(&choice.allow, choice.sub_patterns.clone()),
                    from_option(&choice.deny, choice.sub_patterns.clone()),
                ]
            })
            .collect(),
    }
}

/// Filters `matches` down to the ones that fall in span `(entry_idx,
/// span_idx)`, preserving order, and translates the global `selected`
/// index into a span-local index (None if the active match isn't in
/// this span). Used by the search-highlight pre-pass in `Render` to
/// hand per-span ranges to each Markdown entity.
pub(crate) fn matches_for_span(
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
pub(crate) fn render_span(
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

pub(crate) fn render_entry(
    entry_idx: usize,
    entry: &AgentThreadEntry,
    created_ms: Option<i64>,
    is_last: bool,
    date_separator: Option<String>,
    markdown_for: &HashMap<(usize, usize), Entity<Markdown>>,
    style: &MarkdownStyle,
    assistant_label: &SharedString,
    rewind_target: Option<UserMessageId>,
    thread: gpui::WeakEntity<AcpThread>,
    view: WeakEntity<crate::session_view::SolutionSessionView>,
    queue_marker_expanded: bool,
    cx: &App,
) -> AnyElement {
    let inner: AnyElement = match entry {
        AgentThreadEntry::UserMessage(message) => render_user_message(
            entry_idx,
            message,
            created_ms,
            is_last,
            markdown_for,
            style,
            view,
            queue_marker_expanded,
            cx,
        ),
        AgentThreadEntry::AssistantMessage(message) => render_assistant_message(
            entry_idx,
            message,
            created_ms,
            is_last,
            markdown_for,
            style,
            assistant_label,
            cx,
        ),
        AgentThreadEntry::ToolCall(call) => {
            render_tool_call(entry_idx, call, markdown_for, style, thread.clone(), cx)
        }
        AgentThreadEntry::CompletedPlan(entries) => {
            render_plan(entry_idx, entries, markdown_for, style, cx)
        }
    };

    // Always wrap each entry in a right-click menu. Copy / Copy-as-
    // markdown are unconditional (they pin the currently-focused
    // markdown widget so empty selection is just a no-op), and the
    // "Rewind to this point" entry only renders when the agent
    // supports truncation AND there's a downstream user message we
    // can truncate at — otherwise the body-wide menu would have been
    // the only Copy affordance, but that wrapper breaks the list's
    // flex layout so we host the menu per-entry instead.
    let inner_cell = std::cell::RefCell::new(Some(inner));
    let body = ui::right_click_menu(("session-entry-menu", entry_idx))
        .trigger(move |_, _, _| {
            inner_cell
                .borrow_mut()
                .take()
                .unwrap_or_else(|| Empty.into_any_element())
        })
        .menu(move |window, cx| {
            let rewind_target = rewind_target.clone();
            let thread = thread.clone();
            // Pin the currently-focused element (typically the Markdown
            // widget the user just clicked into to drag a selection)
            // so Copy / Copy-as-markdown land on it. Without this the
            // entry-scoped menu would silently swallow the actions.
            let focus = window.focused(cx);
            ContextMenu::build(window, cx, move |mut menu, _, _| {
                if let Some(target_id) = rewind_target {
                    menu = menu
                        .entry("Rewind to this point", None, {
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
                        .separator();
                }
                menu.when_some(focus, |menu, focus| menu.context(focus))
                    .action("Copy", Box::new(markdown::Copy))
                    .action("Copy as markdown", Box::new(markdown::CopyAsMarkdown))
            })
        });

    // The separator (when present) renders ABOVE the bubble as a child of
    // the same list item, keeping the list's idx↔entry mapping 1:1.
    // `w_full` is essential: without it this wrapper hugs its content, which
    // collapses the inner bubble row's `w_full`/right-alignment and shrinks
    // every bubble.
    v_flex()
        .w_full()
        .when_some(date_separator, |this, label| {
            this.child(
                h_flex()
                    .w_full()
                    .my_1()
                    .justify_center()
                    .child(
                        Label::new(label)
                            .size(LabelSize::XSmall)
                            .color(Color::Muted),
                    ),
            )
        })
        .child(body)
        .into_any_element()
}

/// Plain-text preview of a queued follow-up, used by the "ghost"
/// bubble we draw while the message is sitting in `pending_messages`.
/// Concatenates text blocks (preserving the `\n\n` separators that
/// `send_message_blocks` injects when merging queued submits) and
/// substitutes `[image #N]` placeholders for image blocks.
///
/// Strips the leading queue marker that `send_message_blocks` prepends
/// on first enqueue (`[The user typed the following at HH:MM:SS …]`) —
/// the marker is for the agent, not the user, and showing it in the
/// ghost just clutters the preview without telling the user anything
/// the "Queued — sends when agent finishes" caption doesn't already
/// convey.
/// Walk a pending-message bundle's content blocks and collect the
/// deduplicated `spk_client_send_id` csids, preserving first-seen
/// order. Shared by the `get_session` MCP surface and the live
/// `queue_changed` notification path so both report identical csids.
pub(crate) fn extract_bundle_csids(bundle: &[acp::ContentBlock]) -> Vec<i64> {
    let mut out: Vec<i64> = Vec::new();
    for block in bundle {
        let meta = match block {
            acp::ContentBlock::Text(t) => &t.meta,
            acp::ContentBlock::Image(i) => &i.meta,
            acp::ContentBlock::Audio(a) => &a.meta,
            acp::ContentBlock::ResourceLink(r) => &r.meta,
            acp::ContentBlock::Resource(r) => &r.meta,
            _ => continue,
        };
        if let Some(id) = meta
            .as_ref()
            .and_then(|m| m.get(acp_thread::SPK_CLIENT_SEND_ID_META_KEY))
            .and_then(|v| v.as_i64())
            && !out.contains(&id)
        {
            out.push(id);
        }
    }
    out
}

pub(crate) fn pending_blocks_preview(blocks: &[acp::ContentBlock], _cx: &App) -> String {
    let mut out = String::new();
    let mut image_idx = 1usize;
    let mut first_text = true;
    for block in blocks {
        match block {
            acp::ContentBlock::Text(t) => {
                let text = if first_text {
                    strip_queue_marker(&t.text)
                } else {
                    t.text.as_str()
                };
                first_text = false;
                out.push_str(text);
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

/// If `text` starts with the timestamp marker emitted by
/// `store::build_queue_marker`, return everything after the closing
/// `]` plus the trailing blank-line separator. Otherwise return
/// `text` unchanged.
///
/// Reads the prefix / body-separator from `store::QUEUE_MARKER_*` so the
/// writer and reader stay in sync — a wording change there propagates
/// here for free.
pub(crate) fn strip_queue_marker(text: &str) -> &str {
    if !text.starts_with(crate::store::QUEUE_MARKER_PREFIX) {
        return text;
    }
    let Some(close_idx) = text.find(crate::store::QUEUE_MARKER_BODY_SEP) else {
        return text;
    };
    &text[close_idx + crate::store::QUEUE_MARKER_BODY_SEP.len()..]
}

/// If `text` starts with the timestamp marker emitted by
/// `store::build_queue_marker`, return the marker portion (the bracketed
/// `[...]` block, no trailing `\n\n`). Otherwise return `None`.
///
/// Used by the user-message renderer to surface the original marker
/// behind a click affordance — hidden by default, expanded on demand.
pub(crate) fn extract_queue_marker(text: &str) -> Option<&str> {
    if !text.starts_with(crate::store::QUEUE_MARKER_PREFIX) {
        return None;
    }
    let close_idx = text.find(crate::store::QUEUE_MARKER_BODY_SEP)?;
    Some(&text[..close_idx + 1])
}

/// Pull just the `HH:MM:SS` substring out of a queue marker — the
/// marker's only user-meaningful payload. Returns `None` if the input
/// isn't a marker or the timestamp shape is off (defensive against
/// future wording tweaks). Used as the collapsed-chip label so the
/// glanceable cue is the time, not the boilerplate sentence around it.
pub(crate) fn queue_marker_timestamp(marker: &str) -> Option<&str> {
    let prefix = crate::store::QUEUE_MARKER_PREFIX;
    if !marker.starts_with(prefix) {
        return None;
    }
    let after = &marker[prefix.len()..];
    let space_idx = after.find(' ')?;
    Some(&after[..space_idx])
}

pub(crate) fn render_user_message(
    entry_idx: usize,
    message: &UserMessage,
    created_ms: Option<i64>,
    is_last: bool,
    markdown_for: &HashMap<(usize, usize), Entity<Markdown>>,
    style: &MarkdownStyle,
    view: WeakEntity<crate::session_view::SolutionSessionView>,
    queue_marker_expanded: bool,
    cx: &App,
) -> AnyElement {
    // `clean_user_message_text` strips the literal "`Image`"
    // placeholder our acp_thread merger emits AND rewrites the
    // user-typed `[image #N]` placeholders into markdown links so the
    // Markdown widget paints them as clickable spans. The actual
    // image preview opens through the `on_url_click` hook below.
    let raw_text = content_block_text(&message.content, cx);
    let text = clean_user_message_text(&raw_text);
    let queue_marker = extract_queue_marker(&raw_text).map(str::to_owned);
    let queue_marker_timestamp = queue_marker
        .as_deref()
        .and_then(|m| crate::conversation_render::queue_marker_timestamp(m).map(str::to_owned));
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

    // Queue-marker chip: tiny pill above the bubble that the user can
    // click to reveal the original "[The user typed the following at
    // HH:MM:SS …]" boilerplate. Hidden by default because the marker
    // is just a system bracket around the user's own text and adds
    // nothing they didn't already see when typing — but the timestamp
    // is occasionally useful ("when did I queue this follow-up?"), so
    // a one-click reveal is worth a small affordance.
    let queue_chip: Option<AnyElement> = queue_marker.as_deref().map(|marker| {
        let chip_id = SharedString::from(format!("queue-marker-toggle-{entry_idx}"));
        let view_for_click = view.clone();
        let label_text: SharedString = if let Some(ts) = queue_marker_timestamp.as_deref() {
            format!("queued · {ts}").into()
        } else {
            "queued".into()
        };
        let mut chip = h_flex()
            .id(chip_id)
            .gap_1()
            .px_1p5()
            .py_0p5()
            .rounded_sm()
            .cursor_pointer()
            .bg(cx.theme().colors().element_background)
            .hover(|s| s.bg(cx.theme().colors().element_hover))
            .child(
                ui::Icon::new(IconName::HistoryRerun)
                    .size(ui::IconSize::XSmall)
                    .color(Color::Muted),
            )
            .child(
                Label::new(label_text)
                    .size(LabelSize::XSmall)
                    .color(Color::Muted),
            )
            .on_click(move |_, _, cx| {
                if let Some(view) = view_for_click.upgrade() {
                    view.update(cx, |view, cx| {
                        view.toggle_queue_marker(entry_idx);
                        cx.notify();
                    });
                }
            });
        if queue_marker_expanded {
            chip = chip.child(
                Label::new(SharedString::from(marker.to_string()))
                    .size(LabelSize::XSmall)
                    .color(Color::Muted)
                    .italic(),
            );
        }
        chip.into_any_element()
    });

    v_flex()
        .group(group_name.clone())
        .px_1()
        .mb_3()
        .when_some(queue_chip, |this, chip| {
            this.child(h_flex().mb_1().child(chip))
        })
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
                        group_name.clone(),
                    ))
                    .when_some(
                        render_message_time(created_ms, is_last, group_name),
                        |this, time| this.child(time),
                    ),
            ),
        )
        .into_any_element()
}

/// Cleans a user message's merged-markdown source for display:
///   1. Strips the leading queue marker (`[The user typed the
///      following at HH:MM:SS …]\n\n`) that `send_message_blocks`
///      prepends to every queued follow-up. The marker is meaningful
///      for the agent (telling Claude "this was typed pre-emptively,
///      not in response to your last turn") but pure noise for the
///      user — they typed the message and don't need to see their own
///      submission re-narrated by a system bracket. Same helper as
///      `pending_blocks_preview` so the queued ghost bubble and the
///      sent message render identically.
///   2. Rewrites EVERY image placeholder in the text into a clickable
///      markdown link of the form `[image #N](spk-image://<idx>)`.
///      Two flavours of placeholder hit this path:
///       - `[image #N]` — injected by the desktop compose-paste handler
///         (label is the session-monotonic
///         `SolutionSessionView::image_count_so_far`), so the user-
///         facing `N` is preserved verbatim.
///       - "`Image`" — emitted by `acp_thread::ContentBlock::append`
///         when an Image chunk follows other content in the same
///         message (the common shape for a mobile-originated user
///         message that bundled text + attachments). These get a
///         synthesised 1-based label off the local ordinal so the
///         desktop bubble surfaces them as `[image #1]`, `[image #2]`
///         identically to a desktop-pasted message.
///      The on-click handler intercepts `spk-image://<idx>` and opens
///      an image-preview window for the matching chunk by ORDINAL
///      position — never `N - 1` — because the `N` from the desktop
///      label is a session counter, not a per-message index.
///   3. Collapses leftover double-blank lines so the bubble doesn't
///      grow an empty paragraph where the placeholder used to live.
pub(crate) fn clean_user_message_text(text: &str) -> String {
    let unmarked = strip_queue_marker(text);
    let mut ordinal: usize = 0;
    let with_links = USER_IMAGE_PLACEHOLDER_RE.replace_all(&unmarked, |caps: &regex::Captures| {
        let label_n = caps
            .get(1)
            .and_then(|m| m.as_str().parse::<usize>().ok())
            .unwrap_or(ordinal + 1);
        let idx = ordinal;
        ordinal += 1;
        format!("[image #{label_n}](spk-image://{idx})")
    });
    // Reconstruct with explicit markdown line-break semantics:
    //   * single `\n` between non-empty lines → `  \n` (CommonMark
    //     hard break — two trailing spaces + newline). Without this the
    //     parser folds them into soft breaks and the whole pasted block
    //     renders as one squished paragraph.
    //   * blank lines (≥1 in a row) → `\n\n` (paragraph break). Multiple
    //     consecutive blanks collapse to ONE paragraph break — pasted
    //     code with extra blank lines stays readable instead of
    //     stretching the bubble vertically.
    // Both rules preserve inline markdown the user might have typed
    // (bold, code spans, links). Wrapping the whole message in a code
    // fence would lose that.
    let mut out = String::with_capacity(with_links.len() + 16);
    let mut prev_blank = false;
    let mut first = true;
    for line in with_links.lines() {
        let trimmed = line.trim_end();
        if trimmed.is_empty() {
            if !first && !prev_blank {
                out.push_str("\n\n");
                prev_blank = true;
            }
            // Subsequent blanks within the same run: skip.
        } else {
            if !first && !prev_blank {
                out.push_str("  \n");
            }
            out.push_str(trimmed);
            first = false;
            prev_blank = false;
        }
    }
    out.trim_end().to_string()
}

/// `[image #N]` placeholder pattern injected by the compose paste
/// handler. The capture group is the 1-based image index. Used by
/// the recall path (`session_view::recall`) where we want ONLY the
/// desktop-typed placeholders, not the `\`Image\`` literals emitted
/// by acp_thread's image-chunk merge — those don't carry a recall
/// label and would just confuse the recall surface.
pub(crate) static IMAGE_PLACEHOLDER_RE: std::sync::LazyLock<regex::Regex> =
    std::sync::LazyLock::new(|| {
        regex::Regex::new(r"\[image #(\d+)\]").expect("static regex compiles")
    });

/// Combined regex for [clean_user_message_text]: matches either the
/// desktop-paste `[image #N]` placeholder OR the literal `\`Image\``
/// inline-code marker that `acp_thread::ContentBlock::append` emits
/// when merging an image chunk into a multi-block user message
/// (e.g. mobile-originated text + attachment bundle). The capture
/// group is the digits inside `[image #N]` when that variant matched;
/// `None` when the `\`Image\`` branch matched, in which case the
/// caller synthesises a 1-based ordinal from the match position.
pub(crate) static USER_IMAGE_PLACEHOLDER_RE: std::sync::LazyLock<regex::Regex> =
    std::sync::LazyLock::new(|| {
        regex::Regex::new(r"\[image #(\d+)\]|`Image`").expect("static regex compiles")
    });

/// Mirrors `acp_thread::ContentBlock::decode_image` (private upstream)
/// so we can re-decode image chunks at render time without exposing a
/// new `pub` surface in the acp_thread crate. Returns None on malformed
/// base64 / unsupported mime — caller falls back to the placeholder.
pub(crate) fn decode_image_local(
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
pub(crate) fn open_image_preview(
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

pub(crate) struct ImagePreviewWindowView {
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

pub(crate) fn render_assistant_message(
    entry_idx: usize,
    message: &AssistantMessage,
    created_ms: Option<i64>,
    is_last: bool,
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
    let mut container = v_flex().group(group_name.clone()).relative().px_1().mb_3(); // 12 px — a hair more than the user bubble's mb_3 above; both stay synced.
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
            group_name.clone(),
        ));
    }
    if let Some(time) = render_message_time(created_ms, is_last, group_name) {
        container = container.child(time);
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
pub(crate) fn render_floating_copy_button(
    button_id: SharedString,
    source: String,
    group_name: SharedString,
) -> impl IntoElement {
    div().absolute().bottom_0p5().right_0p5().child(
        CopyButton::new(button_id, source)
            .icon_size(IconSize::XSmall)
            .tooltip_label("Copy as markdown")
            .visible_on_hover(group_name),
    )
}

/// Top-right `HH:MM` affordance for a message bubble. Anchored absolutely
/// so it overlays the bubble's upper-right corner, sitting clear of the
/// bottom-right copy button. Always hover-only (same group the copy button
/// uses) — the always-visible "last activity" time now lives in the status
/// row instead, so no bubble needs a permanently-painted timestamp. The
/// `_is_last` param is kept for the caller's plumbing but no longer affects
/// visibility. Returns `None` for entries without a real timestamp
/// (`ms <= 0` is filtered upstream).
fn render_message_time(
    created_ms: Option<i64>,
    _is_last: bool,
    group_name: SharedString,
) -> Option<impl IntoElement> {
    let ms = created_ms.filter(|&ms| ms > 0)?;
    let dt = chrono::Utc.timestamp_millis_opt(ms).single()?;
    Some(
        div()
            .absolute()
            .top_0p5()
            .right_1p5()
            .child(
                Label::new(crate::status_row::format_hm(dt))
                    .size(LabelSize::XSmall)
                    .color(Color::Muted),
            )
            .visible_on_hover(group_name),
    )
}

pub(crate) fn render_tool_call(
    entry_idx: usize,
    call: &ToolCall,
    markdown_for: &HashMap<(usize, usize), Entity<Markdown>>,
    style: &MarkdownStyle,
    thread: gpui::WeakEntity<AcpThread>,
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

    // Elapsed-time badge: shown only while the tool is actively running
    // so the user can tell a 30-second hang apart from a freshly-started
    // call. Terminal statuses skip the badge — we keep the timestamp on
    // the entity (see acp_thread::ToolCall::status_started_at) but
    // rendering "ran for Xs" on done/failed/canceled calls is a
    // deliberate follow-up, not part of the live-counter surface.
    let elapsed_label = if matches!(call.status, ToolCallStatus::InProgress) {
        call.status_started_at.map(|started| {
            let elapsed_secs = (chrono::Utc::now() - started).num_seconds().max(0) as u64;
            crate::status_row::format_elapsed(elapsed_secs)
        })
    } else {
        None
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
                .child(render_span(
                    (entry_idx, 0),
                    &label_text,
                    markdown_for,
                    style,
                ))
                .child(
                    Label::new(status_text)
                        .size(LabelSize::XSmall)
                        .color(status_color),
                )
                .when_some(elapsed_label, |this, label| {
                    this.child(
                        Label::new(SharedString::from(label))
                            .size(LabelSize::XSmall)
                            .color(Color::Muted),
                    )
                }),
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

    // Authorization affordance: when the agent is blocked waiting for the
    // user to allow/deny this tool call, render its options as buttons.
    // Clicking one calls `AcpThread::authorize_tool_call`, which fulfills
    // the `respond_tx` oneshot the connection is awaiting and unblocks the
    // turn. The buttons disappear on the next render once the status moves
    // off `WaitingForConfirmation`.
    if let ToolCallStatus::WaitingForConfirmation { options, .. } = &call.status {
        let buttons = permission_buttons(options);
        if !buttons.is_empty() {
            let tool_call_id = call.id.clone();
            let mut row = h_flex().gap_1().mt_0p5().flex_wrap();
            for (button_idx, button) in buttons.into_iter().enumerate() {
                let style = if button.is_allow() {
                    ButtonStyle::Filled
                } else {
                    ButtonStyle::Subtle
                };
                let label_color = if button.is_allow() {
                    Color::Default
                } else {
                    Color::Muted
                };
                let thread = thread.clone();
                let tool_call_id = tool_call_id.clone();
                row = row.child(
                    Button::new(
                        ("tool-auth", entry_idx * 1000 + button_idx),
                        button.label.clone(),
                    )
                    .style(style)
                    .label_size(LabelSize::Small)
                    .color(label_color)
                    .on_click(move |_, _, cx| {
                        let outcome = button.outcome();
                        let tool_call_id = tool_call_id.clone();
                        thread
                            .update(cx, move |thread, cx| {
                                thread.authorize_tool_call(tool_call_id, outcome, cx);
                            })
                            .log_err();
                    }),
                );
            }
            container = container.child(row);
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
pub(crate) fn tool_call_content_summary(
    call: &ToolCall,
    content: &ToolCallContent,
    cx: &App,
) -> String {
    let raw = match content {
        // Tool output via `ContentBlock` is plain text the agent emitted
        // (grep matches, file reads, ls listings — anything not Diff and
        // not Terminal). claude-acp ships those as `ContentBlock::Text`
        // with single `\n`s between rows, which CommonMark renders as
        // soft breaks — i.e. all the rows get joined into one paragraph
        // and the user loses the line structure. Wrap in a 4-backtick
        // fence (same trick `terminal_output_markdown` and
        // `raw_output_fallback_markdown` use) so the markdown widget
        // paints it monospaced + line-preserving.
        ToolCallContent::ContentBlock(block) => fence_plain_text(&content_block_text(block, cx)),
        ToolCallContent::Diff(diff) => diff_summary_markdown(diff, cx),
        ToolCallContent::Terminal(terminal) => {
            let primary = terminal_output_markdown(terminal, cx);
            if primary.contains("(no output yet)") {
                raw_output_fallback_markdown(call.raw_output.as_ref()).unwrap_or(primary)
            } else {
                primary
            }
        }
    };
    truncate_tool_summary(&raw)
}

/// Wrap plain-text tool output in a 4-backtick fence so CommonMark
/// preserves newlines instead of joining them as soft breaks. No-op for
/// empty strings and for text that already opens with a code fence (the
/// agent occasionally returns pre-fenced markdown for table-like tools).
fn fence_plain_text(text: &str) -> String {
    let trimmed = text.trim_end();
    if trimmed.is_empty() {
        return text.to_string();
    }
    let already_fenced = trimmed
        .lines()
        .next()
        .map(|line| line.trim_start().starts_with("```"))
        .unwrap_or(false);
    if already_fenced {
        return text.to_string();
    }
    format!("````\n{trimmed}\n````")
}

/// Trims tool-call output for the inline chat view — long Read / Bash /
/// Diff results would otherwise push the rest of the conversation off the
/// screen on every turn. Caps at `MAX_LINES` and appends a `… (+N more
/// lines)` hint matching the claude-code CLI convention. The full content
/// is still available via the original tool / file in the editor; this is
/// just the chat-side preview.
pub(crate) fn truncate_tool_summary(text: &str) -> String {
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
pub(crate) fn raw_output_fallback_markdown(raw: Option<&serde_json::Value>) -> Option<String> {
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
            if pretty.trim().is_empty() || pretty.trim() == "{}" || pretty.trim() == "[]" {
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
pub(crate) fn diff_summary_markdown(diff: &Entity<acp_thread::Diff>, cx: &App) -> String {
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
    format!("**Edited** `{path}` · +{added} / −{removed}\n```diff\n{body}\n```")
}

/// Render `Terminal` tool-call content as fenced code in markdown so the
/// existing markdown widget paints it monospaced (matches how command
/// labels are already rendered above the output). For an empty / still-
/// starting terminal returns a hint placeholder so the user sees the
/// command body has not produced bytes yet, instead of a blank gap.
/// Truncates to keep the markdown parser snappy on long outputs — tighter
/// than the agent-side byte limit on purpose; the user reads "the gist",
/// not the full stream, in this inline view.
pub(crate) fn terminal_output_markdown(
    terminal: &Entity<acp_thread::Terminal>,
    cx: &App,
) -> String {
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

pub(crate) fn render_plan(
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
        container = container.child(render_span((entry_idx, span_idx), "", markdown_for, style));
    }
    container.into_any_element()
}

pub(crate) fn content_block_text(block: &ContentBlock, cx: &App) -> String {
    block.to_markdown(cx).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(label: &str) -> UserMessageId {
        // UserMessageId wraps Arc<str>; serde round-trip is the only public
        // way to mint one with a deterministic value (its `new()` always
        // generates a fresh UUID).
        serde_json::from_value(serde_json::Value::String(label.into()))
            .expect("UserMessageId deserializes from any string")
    }

    #[test]
    fn user_message_single_newline_becomes_hard_break() {
        let out = clean_user_message_text("first line\nsecond line");
        assert_eq!(out, "first line  \nsecond line");
    }

    #[test]
    fn user_message_blank_line_becomes_paragraph_break() {
        let out = clean_user_message_text("para 1\n\npara 2");
        assert_eq!(out, "para 1\n\npara 2");
    }

    #[test]
    fn user_message_multiple_blank_lines_collapse_to_one_paragraph_break() {
        let out = clean_user_message_text("para 1\n\n\n\npara 2");
        assert_eq!(out, "para 1\n\npara 2");
    }

    #[test]
    fn user_message_mixed_blocks_preserve_structure() {
        let out = clean_user_message_text("intro\nline 2\n\nnext para\nstill next");
        assert_eq!(out, "intro  \nline 2\n\nnext para  \nstill next");
    }

    #[test]
    fn image_placeholder_links_use_per_message_ordinal_not_label() {
        // Earlier code used `N - 1` as the URL idx, but `image #N` labels
        // are session-monotonic — message #2 might own only "image #5",
        // and `images.get(4)` against that message's chunks is `None`,
        // dumping the user into the OS "Open With…" dialog. Ordinal-
        // counted URLs (`spk-image://0`, `spk-image://1`, …) align with
        // the order images appear in `message.chunks`.
        let out = clean_user_message_text("look at [image #5] and then [image #7]");
        assert_eq!(
            out,
            "look at [image #5](spk-image://0) and then [image #7](spk-image://1)"
        );
    }

    #[test]
    fn image_placeholder_link_starts_at_ordinal_zero_in_each_message() {
        // Even if the only image is labelled `image #99`, the URL is
        // `spk-image://0` because there's exactly one image in this
        // message and it's the first.
        let out = clean_user_message_text("only [image #99]");
        assert_eq!(out, "only [image #99](spk-image://0)");
    }

    #[test]
    fn empty_entries_produce_empty_table() {
        assert_eq!(
            compute_rewind_table(&[]),
            Vec::<Option<UserMessageId>>::new()
        );
    }

    #[test]
    fn user_message_itself_is_never_its_own_target() {
        // [user(A)] — the user message at idx 0 must not target itself.
        let table = compute_rewind_table(&[Some(id("A"))]);
        assert_eq!(table, vec![None]);
    }

    #[test]
    fn assistant_after_last_user_has_no_target() {
        // [user(A), assistant, tool] — the trailing assistant + tool come
        // after the last user message, so they have nothing to rewind TO.
        let table = compute_rewind_table(&[Some(id("A")), None, None]);
        assert_eq!(table, vec![None, None, None]);
    }

    #[test]
    fn entries_between_two_user_messages_target_the_later_one() {
        // [user(A), assistant, tool, user(B), assistant] — the assistant
        // and tool between A and B both rewind to B; the assistant after
        // B has no downstream user message, so it's None.
        let table = compute_rewind_table(&[Some(id("A")), None, None, Some(id("B")), None]);
        assert_eq!(table, vec![None, Some(id("B")), Some(id("B")), None, None]);
    }

    #[test]
    fn user_message_without_id_inherits_next_users_target() {
        // [user(A), assistant, user(None), assistant, user(B)] — the
        // user-without-id at idx 2 falls through the gating branch and
        // gets the same target as the surrounding assistant entries:
        // the next user with id, which is B.
        let table = compute_rewind_table(&[Some(id("A")), None, None, None, Some(id("B"))]);
        assert_eq!(
            table,
            vec![None, Some(id("B")), Some(id("B")), Some(id("B")), None]
        );
    }

    #[test]
    fn many_users_chain_rewind_targets() {
        // [user(A), assistant, user(B), assistant, user(C)] — entries
        // after A but before B target B; entries after B but before C
        // target C; entries after C have no target.
        let table =
            compute_rewind_table(&[Some(id("A")), None, Some(id("B")), None, Some(id("C"))]);
        assert_eq!(table, vec![None, Some(id("B")), None, Some(id("C")), None]);
    }

    #[test]
    fn strip_queue_marker_drops_prefix_when_present() {
        let with_marker = "[The user typed the following at 14:23:01 (local time) while you were \
                           still on the previous turn — this is NOT a direct reply to your last \
                           question or tool result, it was queued in advance.]\n\nactual user text";
        assert_eq!(super::strip_queue_marker(with_marker), "actual user text");
    }

    #[test]
    fn strip_queue_marker_passes_through_unmarked_text() {
        // Plain user content (no leading marker) is returned untouched.
        assert_eq!(super::strip_queue_marker("hi there"), "hi there");
        // Looks like a marker but missing the closing `]\n\n` → leave it alone
        // rather than risk eating real content.
        assert_eq!(
            super::strip_queue_marker("[The user typed the following at "),
            "[The user typed the following at "
        );
    }

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
        assert_eq!(
            collect("HELLO HeLLo hello", "Hello"),
            vec![0..5, 6..11, 12..17]
        );
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

    fn opt(
        id: &'static str,
        name: &str,
        kind: acp::PermissionOptionKind,
    ) -> acp::PermissionOption {
        acp::PermissionOption::new(id, name.to_string(), kind)
    }

    #[test]
    fn permission_buttons_flat_preserves_order_and_kind() {
        let options = PermissionOptions::Flat(vec![
            opt("allow", "Allow", acp::PermissionOptionKind::AllowOnce),
            opt("reject", "Reject", acp::PermissionOptionKind::RejectOnce),
        ]);
        let buttons = permission_buttons(&options);
        assert_eq!(buttons.len(), 2);
        assert_eq!(buttons[0].label, SharedString::from("Allow"));
        assert!(buttons[0].is_allow());
        assert!(buttons[0].patterns.is_empty());
        assert_eq!(buttons[1].label, SharedString::from("Reject"));
        assert!(!buttons[1].is_allow());
        // The rebuilt outcome carries the option id + kind verbatim.
        let outcome = buttons[1].outcome();
        assert_eq!(outcome.option_id, buttons[1].option_id);
        assert_eq!(outcome.option_kind, acp::PermissionOptionKind::RejectOnce);
        assert!(outcome.params.is_none());
    }

    #[test]
    fn permission_buttons_dropdown_emits_allow_and_deny_per_choice_with_patterns() {
        let choice = acp_thread::PermissionOptionChoice {
            allow: opt("a", "Always allow", acp::PermissionOptionKind::AllowAlways),
            deny: opt("d", "Always deny", acp::PermissionOptionKind::RejectAlways),
            sub_patterns: vec!["^cargo build".to_string()],
        };
        let buttons = permission_buttons(&PermissionOptions::Dropdown(vec![choice]));
        assert_eq!(buttons.len(), 2);
        assert!(buttons[0].is_allow());
        assert!(!buttons[1].is_allow());
        // Patterns ride along on both the allow and deny buttons so the
        // answer applies them.
        assert_eq!(buttons[0].patterns, vec!["^cargo build".to_string()]);
        let outcome = buttons[0].outcome();
        match outcome.params {
            Some(SelectedPermissionParams::Terminal { patterns }) => {
                assert_eq!(patterns, vec!["^cargo build".to_string()]);
            }
            other => panic!("expected terminal params, got {other:?}"),
        }
    }

    #[test]
    fn pick_reject_button_none_when_only_allow_options() {
        // A malformed server response offering ONLY allow options must NOT
        // resolve to an auto-approve — `pick_reject_button` returns None so
        // the queue path leaves the turn stuck rather than approving the call.
        let options = PermissionOptions::Flat(vec![
            opt("allow-once", "Allow once", acp::PermissionOptionKind::AllowOnce),
            opt(
                "allow-always",
                "Allow always",
                acp::PermissionOptionKind::AllowAlways,
            ),
        ]);
        assert!(pick_reject_button(&options).is_none());
    }

    #[test]
    fn pick_reject_button_prefers_reject_once() {
        let options = PermissionOptions::Flat(vec![
            opt("allow", "Allow", acp::PermissionOptionKind::AllowOnce),
            opt("reject-always", "Reject always", acp::PermissionOptionKind::RejectAlways),
            opt("reject-once", "Reject once", acp::PermissionOptionKind::RejectOnce),
        ]);
        let button = pick_reject_button(&options).expect("a reject button must be picked");
        assert_eq!(button.kind, acp::PermissionOptionKind::RejectOnce);
        assert_eq!(button.option_id, acp::PermissionOptionId::new("reject-once"));
    }

    #[test]
    fn matches_for_span_filters_and_finds_selected() {
        let matches = vec![
            FindMatch {
                entry_idx: 0,
                span_idx: 0,
                range: 0..5,
            },
            FindMatch {
                entry_idx: 0,
                span_idx: 1,
                range: 0..3,
            },
            FindMatch {
                entry_idx: 1,
                span_idx: 0,
                range: 5..8,
            },
            FindMatch {
                entry_idx: 0,
                span_idx: 0,
                range: 10..15,
            },
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
