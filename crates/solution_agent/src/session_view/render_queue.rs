//! Pending-message ghost section rendered beneath the conversation list.
//!
//! When the user types a follow-up while the agent is still working, the
//! message is parked in `SolutionSession::pending_messages`. This module
//! paints the optional bubble (selectable text + clickable image links)
//! and the always-present footer strip with chevron + "queued message"
//! label + Bolt-button (interrupt-and-flush). Click on the strip toggles
//! `queue_collapsed`. The section returns `None` when the queue is empty
//! so the caller can `when_some(...)` it into the layout without
//! reserving spacing.

use std::sync::Arc;

use agent_client_protocol::schema as acp;
use gpui::{
    Context, Div, IntoElement, ParentElement, SharedString, StatefulInteractiveElement, Styled,
};
use markdown::MarkdownElement;
use ui::prelude::*;
use ui::{Color, CommonAnimationExt, Icon, IconName, IconSize, Label, LabelSize, Tooltip};

use super::SolutionSessionView;
use crate::conversation_render::{
    clean_user_message_text, decode_image_local, open_image_preview, pending_blocks_preview,
};
use crate::model::SessionState;

impl SolutionSessionView {
    /// Build the pending-message ghost section. Reads `pending_messages`
    /// off `self.session` internally rather than accepting a snapshot:
    /// the surrounding render path holds an immutable borrow on `cx` via
    /// `self.session.read(cx)`, so passing the snapshot back in would
    /// double-borrow when `cx.listener` (mutable) constructs the click
    /// handler.
    pub(super) fn render_pending_section(
        &self,
        window: &mut gpui::Window,
        cx: &mut Context<Self>,
    ) -> Option<Div> {
        let bundles = self
            .session
            .read(cx)
            .pending_messages
            .iter()
            .cloned()
            .collect::<Vec<_>>();
        if bundles.is_empty() {
            return None;
        }
        let queue_collapsed = self.queue_collapsed;
        let chevron = if queue_collapsed {
            IconName::ChevronRight
        } else {
            IconName::ChevronDown
        };
        let is_running = matches!(self.session.read(cx).state, SessionState::Running { .. });

        // Bubble (expanded only): selectable markdown text + clickable
        // `[image #N]` links wired through `spk-image://` to
        // `open_image_preview`. Reuses the cached `pending_markdown`
        // entity refreshed by `ensure_pending_markdown` in the render
        // pre-pass — building a fresh `Markdown::new` per frame would
        // never finish parsing.
        let bubble = (!queue_collapsed)
            .then(|| {
                let entity = self.pending_markdown.as_ref()?.clone();
                let style = self.markdown_style_for_render.as_ref()?.clone();
                let bubble_bg = cx.theme().colors().text_accent.opacity(0.06);
                let border_color = cx.theme().colors().text_accent.opacity(0.4);
                // Decode any image blocks in the bundle so the
                // `spk-image://idx` URL handler can pop them up. Mirrors
                // the live-user-message path in
                // `render_user_message`.
                let mut images: Vec<Arc<gpui::Image>> = Vec::new();
                for blocks in &bundles {
                    for block in blocks {
                        if let agent_client_protocol::schema::ContentBlock::Image(img) = block
                            && let Some(decoded) = decode_image_local(img)
                        {
                            images.push(decoded);
                        }
                    }
                }
                let images_for_handler = images;
                let body = MarkdownElement::new(entity, style).on_url_click(
                    move |url, window, cx| {
                        if let Some(idx_str) = url.strip_prefix("spk-image://")
                            && let Ok(idx) = idx_str.parse::<usize>()
                            && let Some(image) = images_for_handler.get(idx).cloned()
                        {
                            open_image_preview(image, window, cx);
                            return;
                        }
                        cx.open_url(url.as_ref());
                    },
                );
                Some(
                    h_flex().w_full().child(
                        div()
                            .relative()
                            .w_full()
                            .px_2p5()
                            .py_1()
                            .bg(bubble_bg)
                            .border_1()
                            .border_dashed()
                            .border_color(border_color)
                            .rounded_md()
                            .child(body),
                    ),
                )
            })
            .flatten();

        // Footer strip — always rendered. Hosts the chevron + label +
        // Bolt button. Tooltip carries the "Queued — sends when agent
        // finishes" copy that used to live as a footer label inside
        // the bubble (the user's request: keep the bubble clean,
        // surface the explanation only on intent-to-learn-more).
        let strip = h_flex()
            .id("solution-session-queue-header")
            .gap_2()
            .px_2()
            .py_1()
            .rounded_sm()
            .cursor_pointer()
            .items_center()
            .hover(|this| this.bg(cx.theme().colors().element_hover))
            .child(
                Icon::new(chevron)
                    .size(IconSize::Small)
                    .color(Color::Default),
            )
            .child(
                Icon::new(IconName::CountdownTimer)
                    .size(IconSize::Small)
                    .color(Color::Accent),
            )
            .child(
                Label::new(SharedString::from("queued message"))
                    .size(LabelSize::Default)
                    .color(Color::Default),
            )
            .child(div().flex_1())
            .when(is_running, |this| {
                // Send-now bolt: cancels the current turn and
                // immediately flushes the queue. Same affordance as
                // the Bolt button next to Stop in the compose row,
                // duplicated here so the user can interrupt straight
                // from the queue UI without leaving the bubble.
                this.child(
                    ui::IconButton::new("solution-queue-send-now", IconName::BoltFilled)
                        .icon_size(IconSize::Small)
                        .icon_color(Color::Accent)
                        .tooltip(Tooltip::text(
                            "Send now — interrupts the current turn and runs your queued follow-up",
                        ))
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.submit_compose_and_interrupt(window, cx);
                        })),
                )
            })
            .on_click(cx.listener(|this, _, _, cx| {
                this.queue_collapsed = !this.queue_collapsed;
                cx.notify();
            }))
            .tooltip(Tooltip::text(
                "Queued — sends when agent finishes. Click to expand/collapse. \
                 Up arrow in an empty compose recalls; Esc cancels recall.",
            ));

        // Compose: bubble (when expanded) FIRST, then the always-
        // visible strip — matches the user's request to anchor the
        // collapse control at the BOTTOM of the queue UI, right above
        // the status row.
        let _ = window;
        let mut section = v_flex().w_full().px_1();
        if let Some(bubble) = bubble {
            section = section.child(bubble);
        }
        section = section.child(strip);
        Some(section)
    }

    /// Optimistic-resume section painted while a cold tab is doing its
    /// 3-4 s ACP handshake after the user clicked Send. Shows the
    /// queued text as a muted ghost bubble plus a "Starting agent…"
    /// spinner so the chat reflects the action immediately — without
    /// this the cold-resume path looked like Send did nothing for
    /// several seconds. Returns `None` outside the resuming window;
    /// `pending_send`/`resuming` are both cleared by
    /// `flush_pending_send_if_ready` once the live thread attaches,
    /// at which point `acp_thread.send` re-emits the message as a
    /// real `UserMessage` entry and the ghost goes away.
    pub(super) fn render_resuming_section(
        &self,
        cx: &mut Context<Self>,
    ) -> Option<Div> {
        if !self.resuming {
            return None;
        }
        let blocks = self.pending_send.as_ref()?;
        let raw = pending_blocks_preview(blocks, cx);
        let text = clean_user_message_text(&raw);
        let images: Vec<Arc<gpui::Image>> = blocks
            .iter()
            .filter_map(|b| match b {
                acp::ContentBlock::Image(img) => decode_image_local(img),
                _ => None,
            })
            .collect();

        let bubble_bg = cx.theme().colors().text_accent.opacity(0.06);
        let border_color = cx.theme().colors().text_accent.opacity(0.4);
        let body: gpui::AnyElement = if text.is_empty() && images.is_empty() {
            gpui::Empty.into_any_element()
        } else if !text.is_empty() {
            // No `Markdown` widget cache here — the bubble lives only
            // for the 3-4 s handshake window, and its source is
            // captured at Send time and never edited. A flat `Label`
            // is selectable enough for that lifetime; the
            // `[image #N](spk-image://idx)` link rewriting still works
            // because we run `clean_user_message_text` above and then
            // hand the text to a `Markdown` widget below for the
            // selectable+clickable variant.
            let md_entity =
                cx.new(|cx| markdown::Markdown::new(text.clone().into(), None, None, cx));
            let style = self.markdown_style_for_render.clone()?;
            let images_for_handler = images;
            MarkdownElement::new(md_entity, style)
                .on_url_click(move |url, window, cx| {
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
        } else {
            Label::new(SharedString::from("(image only)"))
                .size(LabelSize::Small)
                .color(Color::Muted)
                .into_any_element()
        };

        let bubble = h_flex().w_full().child(
            div()
                .relative()
                .w_full()
                .px_2p5()
                .py_1()
                .bg(bubble_bg)
                .border_1()
                .border_dashed()
                .border_color(border_color)
                .rounded_md()
                .child(body),
        );
        let spinner = h_flex()
            .gap_1()
            .px_2()
            .py_1()
            .items_center()
            .child(
                Icon::new(IconName::ArrowCircle)
                    .size(IconSize::XSmall)
                    .color(Color::Accent)
                    .with_rotate_animation(2),
            )
            .child(
                Label::new(SharedString::from("Starting agent…"))
                    .size(LabelSize::XSmall)
                    .color(Color::Muted)
                    .italic(),
            );
        Some(v_flex().w_full().px_1().child(bubble).child(spinner))
    }
}
