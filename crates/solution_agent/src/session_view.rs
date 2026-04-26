use acp_thread::{
    AgentThreadEntry, AssistantMessage, AssistantMessageChunk, ContentBlock, PlanEntry, ToolCall,
    ToolCallContent, ToolCallStatus, UserMessage,
};
use gpui::{
    AnyElement, App, Context, Entity, EventEmitter, FocusHandle, Focusable, IntoElement,
    ParentElement, Render, SharedString, Styled, WeakEntity, Window, div,
};
use ui::prelude::*;
use ui::{Icon, IconName, Label};
use workspace::{Workspace, item::Item};

use crate::model::{SolutionSession, SolutionSessionId};

pub struct SolutionSessionView {
    session_id: SolutionSessionId,
    session: Entity<SolutionSession>,
    focus_handle: FocusHandle,
    workspace: WeakEntity<Workspace>,
}

impl SolutionSessionView {
    pub fn new(
        session_id: SolutionSessionId,
        session: Entity<SolutionSession>,
        workspace: WeakEntity<Workspace>,
        cx: &mut Context<Self>,
    ) -> Self {
        cx.observe(&session, |_, _, cx| cx.notify()).detach();
        Self {
            session_id,
            session,
            focus_handle: cx.focus_handle(),
            workspace,
        }
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
        let session = self.session.read(cx);
        let header = format!("{} • {:?}", session.agent_id, session.state);
        div()
            .flex()
            .flex_col()
            .size_full()
            .bg(cx.theme().colors().panel_background)
            .child(
                div()
                    .flex()
                    .h_8()
                    .px_3()
                    .border_b_1()
                    .border_color(cx.theme().colors().border)
                    .child(Label::new(header)),
            )
            .child({
                let mut body = div()
                    .id("solution-session-conversation")
                    .flex_1()
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
            .child(
                div()
                    .h_16()
                    .border_t_1()
                    .border_color(cx.theme().colors().border)
                    .child(Label::new("(compose box — Task 6.4)")),
            )
    }
}

impl Item for SolutionSessionView {
    type Event = SolutionSessionViewEvent;

    fn tab_content_text(&self, _detail: usize, cx: &App) -> SharedString {
        self.session.read(cx).title.clone()
    }

    fn tab_icon(&self, _window: &Window, _cx: &App) -> Option<Icon> {
        Some(Icon::new(IconName::AiClaude))
    }

    fn telemetry_event_text(&self) -> Option<&'static str> {
        Some("solution_agent_session")
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
    for chunk in &message.chunks {
        let (prefix, block) = match chunk {
            AssistantMessageChunk::Message { block } => (None, block),
            AssistantMessageChunk::Thought { block } => (Some("thinking: "), block),
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
