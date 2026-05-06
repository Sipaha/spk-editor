//! Pending-message ghost section rendered beneath the conversation list.
//!
//! When the user types a follow-up while the agent is still working, the
//! message is parked in `SolutionSession::pending_messages`. This module
//! paints the "▸ N queued messages" header (collapsed by default) and the
//! optional bubble bodies underneath. Click on the header toggles
//! `queue_collapsed`. The section returns `None` when the queue is
//! empty so the caller can `when_some(...)` it into the layout without
//! reserving spacing.

use gpui::{Context, Div, ParentElement, SharedString, StatefulInteractiveElement, Styled};
use ui::prelude::*;
use ui::{Color, Icon, IconName, IconSize, Label, LabelSize, Tooltip};

use super::SolutionSessionView;
use crate::conversation_render::{pending_blocks_preview, render_pending_message};

impl SolutionSessionView {
    /// Build the pending-message ghost section. Reads `pending_messages`
    /// off `self.session` internally rather than accepting a snapshot:
    /// the surrounding render path holds an immutable borrow on `cx` via
    /// `self.session.read(cx)`, so passing the snapshot back in would
    /// double-borrow when `cx.listener` (mutable) constructs the click
    /// handler.
    pub(super) fn render_pending_section(&self, cx: &Context<Self>) -> Option<Div> {
        let pending_blocks = self
            .session
            .read(cx)
            .pending_messages
            .iter()
            .cloned()
            .collect::<Vec<_>>();
        let mut pending_previews: Vec<String> = Vec::new();
        for blocks in pending_blocks.iter() {
            let preview = pending_blocks_preview(blocks, cx);
            if !preview.is_empty() {
                pending_previews.push(preview);
            }
        }
        if pending_previews.is_empty() {
            return None;
        }

        let queue_collapsed = self.queue_collapsed;
        let pending_count = pending_previews.len();
        let header_label = if pending_count == 1 {
            SharedString::from("1 queued message")
        } else {
            SharedString::from(format!("{pending_count} queued messages"))
        };
        let chevron = if queue_collapsed {
            IconName::ChevronRight
        } else {
            IconName::ChevronDown
        };

        let mut section = v_flex().w_full().px_1().child(
            h_flex()
                .id("solution-session-queue-header")
                .gap_2()
                .px_2()
                .py_1()
                .rounded_sm()
                .cursor_pointer()
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
                    Label::new(header_label)
                        .size(LabelSize::Default)
                        .color(Color::Default),
                )
                .on_click(cx.listener(|this, _, _, cx| {
                    this.queue_collapsed = !this.queue_collapsed;
                    cx.notify();
                }))
                .tooltip(Tooltip::text(
                    "Click to expand. Up arrow in an empty compose recalls.",
                )),
        );
        if !queue_collapsed {
            for (q_idx, preview) in pending_previews.iter().enumerate() {
                section = section.child(render_pending_message(q_idx, preview, cx));
            }
        }
        Some(section)
    }
}
