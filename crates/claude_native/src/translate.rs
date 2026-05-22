//! Pure translation of parsed `claude` stream-json messages into the
//! `acp::SessionUpdate` values `AcpThread::handle_session_update` already
//! consumes, plus turn-end classification. No GPUI / I/O.

use agent_client_protocol::schema as acp;

use crate::protocol::{ConversationMessage, OutputMessage, ResultMessage, StreamEvent};

/// How a turn ended, derived from the `result` message.
#[derive(Debug, PartialEq)]
pub enum TurnEnd {
    Stop(acp::StopReason),
    Error(String),
}

/// Translate one output message into zero or more `SessionUpdate`s.
///
/// Subagent output (`parent_tool_use_id.is_some()`) is collapsed in the
/// Foundation — only `parent_tool_use_id == None` (top-level) is rendered.
pub fn translate(msg: &OutputMessage) -> Vec<acp::SessionUpdate> {
    match msg {
        OutputMessage::StreamEvent(ev) => translate_stream_event(ev),
        OutputMessage::Assistant(m) if m.parent_tool_use_id.is_none() => translate_assistant(m),
        OutputMessage::User(m) if m.parent_tool_use_id.is_none() => translate_user(m),
        // SP2: render subagent output (parent_tool_use_id.is_some()) here.
        _ => Vec::new(),
    }
}

fn translate_stream_event(ev: &StreamEvent) -> Vec<acp::SessionUpdate> {
    if ev.event.get("type").and_then(|t| t.as_str()) != Some("content_block_delta") {
        return Vec::new();
    }
    let delta = match ev.event.get("delta") {
        Some(d) => d,
        None => return Vec::new(),
    };
    match delta.get("type").and_then(|t| t.as_str()) {
        Some("text_delta") => delta
            .get("text")
            .and_then(|t| t.as_str())
            .map(|text| {
                vec![acp::SessionUpdate::AgentMessageChunk(acp::ContentChunk::new(
                    text_block(text),
                ))]
            })
            .unwrap_or_default(),
        Some("thinking_delta") => delta
            .get("thinking")
            .and_then(|t| t.as_str())
            .map(|text| {
                vec![acp::SessionUpdate::AgentThoughtChunk(acp::ContentChunk::new(
                    text_block(text),
                ))]
            })
            .unwrap_or_default(),
        _ => Vec::new(),
    }
}

fn translate_assistant(m: &ConversationMessage) -> Vec<acp::SessionUpdate> {
    content_blocks(&m.message)
        .iter()
        .filter_map(|block| {
            if block.get("type").and_then(|t| t.as_str()) != Some("tool_use") {
                // Text arrives via stream_event deltas; skip assistant text here.
                return None;
            }
            let id = block.get("id").and_then(|v| v.as_str())?;
            let name = block.get("name").and_then(|v| v.as_str()).unwrap_or("tool");
            let mut call = acp::ToolCall::new(acp::ToolCallId::new(id), name.to_string())
                .status(acp::ToolCallStatus::InProgress);
            if let Some(input) = block.get("input") {
                call = call.raw_input(input.clone());
            }
            Some(acp::SessionUpdate::ToolCall(call))
        })
        .collect()
}

fn translate_user(m: &ConversationMessage) -> Vec<acp::SessionUpdate> {
    content_blocks(&m.message)
        .iter()
        .filter_map(|block| {
            if block.get("type").and_then(|t| t.as_str()) != Some("tool_result") {
                return None;
            }
            let id = block.get("tool_use_id").and_then(|v| v.as_str())?;
            let text = tool_result_text(block.get("content"));
            let fields = acp::ToolCallUpdateFields::new()
                .status(acp::ToolCallStatus::Completed)
                .content(vec![acp::ToolCallContent::from(text_block(&text))]);
            Some(acp::SessionUpdate::ToolCallUpdate(
                acp::ToolCallUpdate::new(acp::ToolCallId::new(id), fields),
            ))
        })
        .collect()
}

/// Classify a `result` message as a turn-end stop reason or an error.
pub fn classify_result(r: &ResultMessage) -> TurnEnd {
    if r.is_error {
        let msg = if r.errors.is_empty() {
            format!("agent error ({})", r.subtype)
        } else {
            r.errors.join("; ")
        };
        return TurnEnd::Error(msg);
    }
    let stop = match r.stop_reason.as_deref() {
        Some("max_tokens") => acp::StopReason::MaxTokens,
        Some("refusal") => acp::StopReason::Refusal,
        Some("cancelled") | Some("canceled") => acp::StopReason::Cancelled,
        // "end_turn", "tool_use", null, or anything else → a normal end.
        _ => acp::StopReason::EndTurn,
    };
    TurnEnd::Stop(stop)
}

/// Build a `UsageUpdate` from a `result`. The window is the model's advertised
/// `contextWindow`, falling back to the last-known `sticky_window` so the meter
/// limit never regresses (the 200k/1M flicker fix). Returns `None` when neither
/// a window nor a used-token count is available.
pub fn usage_update(r: &ResultMessage, sticky_window: Option<u64>) -> Option<acp::SessionUpdate> {
    let window = r.context_window_for_active_model().or(sticky_window);
    let used = r.usage.as_ref().map(|u| u.input_tokens + u.output_tokens);
    match (used, window) {
        (None, None) => None,
        (used, window) => Some(acp::SessionUpdate::UsageUpdate(acp::UsageUpdate::new(
            used.unwrap_or(0),
            window.unwrap_or(0),
        ))),
    }
}

fn text_block(text: &str) -> acp::ContentBlock {
    acp::ContentBlock::Text(acp::TextContent::new(text.to_string()))
}

fn content_blocks(message: &serde_json::Value) -> Vec<serde_json::Value> {
    message
        .get("content")
        .and_then(|c| c.as_array())
        .cloned()
        .unwrap_or_default()
}

/// A `tool_result` block's `content` is either a string or an array of content
/// blocks. Reduce it to display text (text parts joined; non-text serialized).
fn tool_result_text(content: Option<&serde_json::Value>) -> String {
    match content {
        Some(serde_json::Value::String(s)) => s.clone(),
        Some(serde_json::Value::Array(items)) => items
            .iter()
            .map(|item| {
                item.get("text")
                    .and_then(|t| t.as_str())
                    .map(|s| s.to_string())
                    .unwrap_or_else(|| item.to_string())
            })
            .collect::<Vec<_>>()
            .join("\n"),
        Some(other) => other.to_string(),
        None => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::OutputMessage;

    #[test]
    fn text_delta_becomes_agent_message_chunk() {
        let msg = OutputMessage::parse(r#"{"type":"stream_event","parent_tool_use_id":null,"event":{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Hi"}},"uuid":"u","session_id":"s"}"#).unwrap();
        let ups = translate(&msg);
        assert_eq!(ups.len(), 1);
        match &ups[0] {
            acp::SessionUpdate::AgentMessageChunk(chunk) => match &chunk.content {
                acp::ContentBlock::Text(t) => assert_eq!(t.text, "Hi"),
                other => panic!("{other:?}"),
            },
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn thinking_delta_becomes_thought_chunk() {
        let msg = OutputMessage::parse(r#"{"type":"stream_event","event":{"type":"content_block_delta","delta":{"type":"thinking_delta","thinking":"hmm"}},"uuid":"u","session_id":"s"}"#).unwrap();
        let ups = translate(&msg);
        assert!(matches!(ups.as_slice(), [acp::SessionUpdate::AgentThoughtChunk(_)]));
    }

    #[test]
    fn message_start_yields_nothing() {
        let msg = OutputMessage::parse(r#"{"type":"stream_event","event":{"type":"message_start","message":{"model":"m","usage":{}}},"uuid":"u","session_id":"s"}"#).unwrap();
        assert!(translate(&msg).is_empty());
    }

    #[test]
    fn assistant_tool_use_becomes_tool_call() {
        let msg = OutputMessage::parse(r#"{"type":"assistant","parent_tool_use_id":null,"message":{"role":"assistant","content":[{"type":"text","text":"x"},{"type":"tool_use","id":"toolu_1","name":"Bash","input":{"command":"ls"}}]},"uuid":"u","session_id":"s"}"#).unwrap();
        let ups = translate(&msg);
        assert_eq!(ups.len(), 1, "text block skipped, one tool_use mapped");
        match &ups[0] {
            acp::SessionUpdate::ToolCall(call) => {
                assert_eq!(call.tool_call_id.0.as_ref(), "toolu_1");
                assert_eq!(call.title, "Bash");
                assert_eq!(call.status, acp::ToolCallStatus::InProgress);
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn user_tool_result_becomes_tool_call_update() {
        let msg = OutputMessage::parse(r#"{"type":"user","parent_tool_use_id":null,"message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"toolu_1","content":"file.txt"}]},"uuid":"u","session_id":"s"}"#).unwrap();
        let ups = translate(&msg);
        match ups.as_slice() {
            [acp::SessionUpdate::ToolCallUpdate(update)] => {
                assert_eq!(update.tool_call_id.0.as_ref(), "toolu_1");
                assert_eq!(update.fields.status, Some(acp::ToolCallStatus::Completed));
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn subagent_assistant_is_collapsed() {
        let msg = OutputMessage::parse(r#"{"type":"assistant","parent_tool_use_id":"toolu_parent","message":{"role":"assistant","content":[{"type":"tool_use","id":"x","name":"Read","input":{}}]},"uuid":"u","session_id":"s"}"#).unwrap();
        assert!(translate(&msg).is_empty());
    }

    #[test]
    fn classifies_success_end_turn() {
        let r = serde_json::from_str::<ResultMessage>(
            r#"{"subtype":"success","is_error":false,"stop_reason":"end_turn"}"#,
        )
        .unwrap();
        assert_eq!(classify_result(&r), TurnEnd::Stop(acp::StopReason::EndTurn));
    }

    #[test]
    fn classifies_max_tokens_and_cancelled() {
        let mt = serde_json::from_str::<ResultMessage>(
            r#"{"subtype":"success","stop_reason":"max_tokens"}"#,
        )
        .unwrap();
        assert_eq!(classify_result(&mt), TurnEnd::Stop(acp::StopReason::MaxTokens));
        let c = serde_json::from_str::<ResultMessage>(
            r#"{"subtype":"success","stop_reason":"cancelled"}"#,
        )
        .unwrap();
        assert_eq!(classify_result(&c), TurnEnd::Stop(acp::StopReason::Cancelled));
    }

    #[test]
    fn classifies_error() {
        let r = serde_json::from_str::<ResultMessage>(
            r#"{"subtype":"error_during_execution","is_error":true,"errors":["boom"]}"#,
        )
        .unwrap();
        match classify_result(&r) {
            TurnEnd::Error(e) => assert!(e.contains("boom")),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn usage_prefers_real_window_then_sticky() {
        let with_window = serde_json::from_str::<ResultMessage>(
            r#"{"subtype":"success","modelUsage":{"m":{"contextWindow":1000000}},"usage":{"input_tokens":10,"output_tokens":5}}"#,
        )
        .unwrap();
        match usage_update(&with_window, None) {
            Some(acp::SessionUpdate::UsageUpdate(u)) => {
                assert_eq!(u.size, 1_000_000);
                assert_eq!(u.used, 15);
            }
            other => panic!("{other:?}"),
        }
        let no_window = serde_json::from_str::<ResultMessage>(
            r#"{"subtype":"success","usage":{"input_tokens":10,"output_tokens":5}}"#,
        )
        .unwrap();
        match usage_update(&no_window, Some(1_000_000)) {
            Some(acp::SessionUpdate::UsageUpdate(u)) => assert_eq!(u.size, 1_000_000),
            other => panic!("{other:?}"),
        }
        let nothing =
            serde_json::from_str::<ResultMessage>(r#"{"subtype":"success"}"#).unwrap();
        assert!(usage_update(&nothing, None).is_none());
    }
}
