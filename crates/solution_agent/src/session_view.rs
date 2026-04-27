use acp_thread::{
    AgentThreadEntry, AssistantMessage, AssistantMessageChunk, ContentBlock, PlanEntry, ToolCall,
    ToolCallContent, ToolCallStatus, UserMessage,
};
use agent_client_protocol::schema as acp;
use base64::Engine;
use gpui::{
    AnyElement, App, ClipboardEntry, Context, Entity, EventEmitter, ExternalPaths, FocusHandle,
    Focusable, InteractiveElement as _, IntoElement, ParentElement, Render, SharedString, Styled,
    StatefulInteractiveElement as _, WeakEntity, Window, div,
};
use ui::prelude::*;
use ui::Label;
use workspace::Workspace;

use crate::model::{SolutionSession, SolutionSessionId};
use crate::store::SolutionAgentStore;

struct PendingImage {
    mime_type: String,
    data_base64: String,
    label: SharedString,
}

pub struct SolutionSessionView {
    session_id: SolutionSessionId,
    session: Entity<SolutionSession>,
    focus_handle: FocusHandle,
    workspace: WeakEntity<Workspace>,
    compose_editor: Entity<editor::Editor>,
    pending_images: Vec<PendingImage>,
}

impl SolutionSessionView {
    pub fn new(
        session_id: SolutionSessionId,
        session: Entity<SolutionSession>,
        workspace: WeakEntity<Workspace>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        cx.observe(&session, |_, _, cx| cx.notify()).detach();
        let compose_editor = cx.new(|cx| {
            let mut e = editor::Editor::auto_height(1, 8, window, cx);
            e.set_placeholder_text("Send a message…", window, cx);
            e
        });
        Self {
            session_id,
            session,
            focus_handle: cx.focus_handle(),
            workspace,
            compose_editor,
            pending_images: Vec::new(),
        }
    }

    fn submit_compose(
        &mut self,
        _: &menu::Confirm,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let content = self.compose_editor.read(cx).text(cx);
        if content.trim().is_empty() && self.pending_images.is_empty() {
            return;
        }
        self.compose_editor
            .update(cx, |e, cx| e.clear(window, cx));
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
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

pub enum SolutionSessionViewEvent {}

impl EventEmitter<SolutionSessionViewEvent> for SolutionSessionView {}

impl Render for SolutionSessionView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // The view used to render its own header (which leaked Debug-formatted
        // SessionState as JSON-looking goo) and was an upstream `Item` so it
        // could open as a workspace pane tab. Both are gone now: the chat
        // panel hosts views inside its own tab strip + status row, so this
        // view is just the conversation + compose box.
        let session = self.session.read(cx);
        let pending_image_count = self.pending_images.len();
        div()
            .id("solution-session-view")
            .key_context("SolutionSessionView")
            .track_focus(&self.focus_handle)
            .capture_action(cx.listener(Self::paste_intercept))
            .on_action(cx.listener(Self::submit_compose))
            .on_drop(cx.listener(
                |this, paths: &ExternalPaths, window, cx| {
                    this.handle_external_paths_drop(paths, window, cx);
                },
            ))
            .flex()
            .flex_col()
            .size_full()
            .bg(cx.theme().colors().panel_background)
            .child({
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
                    .overflow_y_scroll();
                if let Some(thread) = session.acp_thread.as_ref() {
                    let thread = thread.read(cx);
                    let entries = thread.entries();
                    if entries.is_empty() {
                        body = body.child(Label::new("(no messages yet)").size(LabelSize::Small));
                    } else {
                        for entry in entries {
                            body = body.child(render_entry(entry, cx));
                        }
                    }
                } else {
                    body = body.child(Label::new("(no thread yet)").size(LabelSize::Small));
                }
                body
            })
            .child({
                let mut compose_row = div()
                    .flex()
                    .h_24()
                    .p_2()
                    .gap_2()
                    .border_t_1()
                    .border_color(cx.theme().colors().border)
                    .child(div().flex_1().child(self.compose_editor.clone()))
                    .child(
                        ui::Button::new("solution-session-send", "Send").on_click(cx.listener(
                            |this, _, window, cx| {
                                this.submit_compose(&menu::Confirm, window, cx);
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

fn render_entry(entry: &AgentThreadEntry, cx: &App) -> AnyElement {
    match entry {
        AgentThreadEntry::UserMessage(message) => render_user_message(message, cx),
        AgentThreadEntry::AssistantMessage(message) => render_assistant_message(message, cx),
        AgentThreadEntry::ToolCall(call) => render_tool_call(call, cx),
        AgentThreadEntry::CompletedPlan(entries) => render_plan(entries, cx),
    }
}

fn render_user_message(message: &UserMessage, cx: &App) -> AnyElement {
    let text = content_block_text(&message.content, cx);
    div()
        .px_3()
        .py_2()
        .my_1()
        .bg(cx.theme().colors().element_active)
        .rounded_md()
        .child(Label::new("You").size(LabelSize::XSmall))
        .child(Label::new(text))
        .into_any_element()
}

fn render_assistant_message(message: &AssistantMessage, cx: &App) -> AnyElement {
    let mut container = div()
        .px_3()
        .py_2()
        .my_1()
        .bg(cx.theme().colors().surface_background)
        .rounded_md()
        .child(Label::new("Assistant").size(LabelSize::XSmall));
    // While the agent is mid-turn we may have only `Thought` chunks — show
    // them so the user has feedback that something is happening. Once any
    // real `Message` chunk arrives the thoughts become noise (Claude was
    // just reasoning) and we drop them. Mirrors Cursor / upstream Zed
    // AgentPanel which hide reasoning tokens behind a collapsed
    // "Reasoned for Xs" disclosure once the answer starts streaming.
    let has_message = message
        .chunks
        .iter()
        .any(|c| matches!(c, AssistantMessageChunk::Message { .. }));
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
            container = container.child(Label::new(text));
        }
    }
    container.into_any_element()
}

fn render_tool_call(call: &ToolCall, cx: &App) -> AnyElement {
    let label_text = call.label.read(cx).source().to_string();
    let status_text = match &call.status {
        ToolCallStatus::Pending => "pending",
        ToolCallStatus::WaitingForConfirmation { .. } => "waiting for confirmation",
        ToolCallStatus::InProgress => "running",
        ToolCallStatus::Completed => "done",
        ToolCallStatus::Failed => "failed",
        ToolCallStatus::Rejected => "rejected",
        ToolCallStatus::Canceled => "canceled",
    };
    let header = format!("Tool: {label_text} ({status_text})");

    let mut container = div()
        .px_3()
        .py_2()
        .my_1()
        .border_1()
        .border_color(cx.theme().colors().border)
        .rounded_md()
        .child(Label::new(header).size(LabelSize::Small));

    for content in &call.content {
        let summary = match content {
            ToolCallContent::ContentBlock(block) => content_block_text(block, cx),
            ToolCallContent::Diff(_) => "[diff]".to_string(),
            ToolCallContent::Terminal(_) => "[terminal output]".to_string(),
        };
        if !summary.is_empty() {
            container = container.child(Label::new(summary).size(LabelSize::XSmall));
        }
    }

    container.into_any_element()
}

fn render_plan(entries: &[PlanEntry], cx: &App) -> AnyElement {
    let mut container = div()
        .px_3()
        .py_2()
        .my_1()
        .border_1()
        .border_color(cx.theme().colors().border)
        .rounded_md()
        .child(Label::new("Plan").size(LabelSize::XSmall));
    for entry in entries {
        let source = entry.content.read(cx).source().to_string();
        container = container.child(Label::new(format!("• {source}")).size(LabelSize::Small));
    }
    container.into_any_element()
}

fn content_block_text(block: &ContentBlock, cx: &App) -> String {
    block.to_markdown(cx).to_string()
}
