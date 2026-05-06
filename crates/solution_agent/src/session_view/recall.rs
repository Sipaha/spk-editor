//! Up-arrow recall path for the compose editor: pulls the queued
//! follow-up message back into the editor so the user can edit it
//! before it sends. Wired to `editor::actions::MoveUp` via
//! `capture_action` on the session-view root (top-down dispatch); if
//! conditions don't match, the handler does NOT call `stop_propagation`
//! and the editor's default cursor-up behavior runs as usual.

use agent_client_protocol::schema as acp;
use gpui::{Context, Focusable, SharedString, Window};

use super::{PendingImage, SolutionSessionView};
use crate::store::SolutionAgentStore;

/// Reverse of `submit_compose_now`'s blocks-from-draft step: takes a queued
/// bundle (already-merged user submissions, possibly with a leading
/// timestamp marker and embedded images) and rebuilds the inputs the user
/// originally had — concatenated text + a list of `PendingImage`s.
///
/// The marker (prepended by `store::build_queue_marker` on first enqueue)
/// is stripped if present so the recovered draft is just what the user
/// typed. Image labels are re-derived from the `[image #N]` placeholders
/// already present in the text — they're only used for paste-time inserts
/// and never displayed otherwise, so a missing tag falls back to "image
/// #?" (won't appear in the UI; a follow-up paste will assign the next
/// real number from `image_count_so_far`).
pub(super) fn unpack_recalled_bundle(
    blocks: Vec<acp::ContentBlock>,
) -> (String, Vec<PendingImage>) {
    let mut text = String::new();
    let mut images: Vec<PendingImage> = Vec::new();
    let mut first_text = true;
    for block in blocks {
        match block {
            acp::ContentBlock::Text(t) => {
                let chunk = if first_text {
                    crate::conversation_render::strip_queue_marker(&t.text).to_string()
                } else {
                    t.text
                };
                first_text = false;
                text.push_str(&chunk);
            }
            acp::ContentBlock::Image(img) => {
                images.push(PendingImage {
                    mime_type: img.mime_type,
                    data_base64: img.data,
                    label: SharedString::from("image #?"),
                });
            }
            _ => {}
        }
    }
    let placeholders: Vec<usize> = crate::conversation_render::IMAGE_PLACEHOLDER_RE
        .captures_iter(&text)
        .filter_map(|c| c.get(1)?.as_str().parse::<usize>().ok())
        .collect();
    for (img, n) in images.iter_mut().zip(placeholders.iter()) {
        img.label = SharedString::from(format!("image #{n}"));
    }
    (text, images)
}

impl SolutionSessionView {
    /// `Up` keystroke in the compose editor. When the editor is empty and a
    /// queued follow-up is sitting in `pending_messages` (typed while the
    /// agent was still working), pull that draft back into the editor —
    /// "I changed my mind, let me edit this before it sends."
    ///
    /// In every other case (editor non-empty, attached images, no queue,
    /// focus is elsewhere) returns without consuming the action so the
    /// editor's default `MoveUp` handler runs and the cursor moves up
    /// as the user expects.
    pub(super) fn recall_queued_message(
        &mut self,
        _: &zed_actions::editor::MoveUp,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let compose_focus = self.compose_editor.read(cx).focus_handle(cx);
        if !compose_focus.is_focused(window) {
            return;
        }
        let compose_empty = self.compose_editor.read(cx).text(cx).is_empty();
        if !compose_empty || !self.pending_images.is_empty() {
            return;
        }
        let session_id = self.session_id;
        // `pending_messages` carries at most one bundle: the second `send`
        // while the agent is Running merges into the existing back entry
        // via `back_mut().push(...)` (see
        // `SolutionAgentStore::send_message_blocks`), so `back()` and
        // `front()` reference the same element when one exists.
        //
        // Peek before pop: `unpack_recalled_bundle` can produce empty
        // `(text, images)` if the bundle is somehow marker-only
        // (defensive — shouldn't happen since `submit_compose_now`
        // rejects empty submissions). Keeping the queue intact instead
        // of silently draining it is the safer default if that
        // invariant ever breaks.
        let peeked = self
            .session
            .read(cx)
            .pending_messages
            .back()
            .cloned();
        let Some(bundle) = peeked else {
            return;
        };
        let (text, images) = unpack_recalled_bundle(bundle);
        if text.is_empty() && images.is_empty() {
            return;
        }
        self.session.update(cx, |session, _| {
            session.pending_messages.pop_back();
        });
        if !text.is_empty() {
            self.compose_editor.update(cx, |editor, cx| {
                editor.set_text(text, window, cx);
            });
        }
        if !images.is_empty() {
            self.pending_images.extend(images);
        }
        // Persist the now-empty queue and emit a state-changed event so any
        // listeners (navigator tab indicator, status row, …) refresh from
        // the new state — the bundle just moved out of `pending_messages`,
        // and the on-disk snapshot needs to follow.
        SolutionAgentStore::global(cx).update(cx, |store, cx| {
            store.persist_session_blob(session_id, cx);
            cx.emit(crate::store::SolutionAgentStoreEvent::SessionStateChanged(
                session_id,
            ));
        });
        cx.stop_propagation();
        cx.notify();
    }
}
