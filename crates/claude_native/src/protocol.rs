//! Serde types for the `claude` stream-json protocol.
//!
//! Pure (no GPUI, no I/O). Only the fields the connection consumes are
//! modeled; every struct ignores unknown fields and an unknown top-level
//! message `type` parses to [`OutputMessage::Unknown`] rather than erroring,
//! so a future `claude` that adds message kinds (or emits a stray
//! `{"type":"ping"}`) does not break the reader.

use serde::Deserialize;
use std::collections::BTreeMap;

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum OutputMessage {
    System(System),
    Assistant(ConversationMessage),
    User(ConversationMessage),
    StreamEvent(StreamEvent),
    Result(ResultMessage),
    ControlRequest(ControlRequestEnvelope),
    ControlResponse(ControlResponseEnvelope),
    #[serde(other)]
    Unknown,
}

impl OutputMessage {
    pub fn parse(line: &str) -> anyhow::Result<Self> {
        Ok(serde_json::from_str(line)?)
    }
}

#[derive(Debug, Deserialize)]
#[serde(tag = "subtype", rename_all = "snake_case")]
pub enum System {
    Init {
        session_id: String,
        #[serde(default)]
        uuid: String,
    },
    SessionStateChanged {
        state: String,
    },
    #[serde(other)]
    Other,
}

#[derive(Debug, Deserialize)]
pub struct StreamEvent {
    pub event: serde_json::Value,
    #[serde(default)]
    pub parent_tool_use_id: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct ConversationMessage {
    pub message: serde_json::Value,
    #[serde(default)]
    pub parent_tool_use_id: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct ResultMessage {
    pub subtype: String,
    #[serde(default)]
    pub is_error: bool,
    #[serde(default)]
    pub result: Option<String>,
    #[serde(default)]
    pub stop_reason: Option<String>,
    #[serde(default)]
    pub errors: Vec<String>,
    #[serde(default, rename = "modelUsage")]
    pub model_usage: BTreeMap<String, ModelUsage>,
    #[serde(default)]
    pub usage: Option<Usage>,
}

#[derive(Debug, Deserialize)]
pub struct ModelUsage {
    #[serde(rename = "contextWindow")]
    pub context_window: u64,
}

#[derive(Debug, Deserialize)]
pub struct Usage {
    pub input_tokens: u64,
    pub output_tokens: u64,
}

impl ResultMessage {
    /// The largest advertised context window across the models used this turn.
    /// `claude` reports per-model windows in `modelUsage`; the active model's is
    /// the relevant budget. When several appear (rare — e.g. a subagent on a
    /// different model), the max is the safe upper bound for the meter.
    pub fn context_window_for_active_model(&self) -> Option<u64> {
        self.model_usage.values().map(|m| m.context_window).max()
    }
}

#[derive(Debug, Deserialize)]
pub struct ControlRequestEnvelope {
    pub request_id: String,
    pub request: ControlRequestKind,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "subtype", rename_all = "snake_case")]
pub enum ControlRequestKind {
    CanUseTool {
        tool_name: String,
        tool_use_id: String,
        #[serde(default)]
        input: serde_json::Value,
        #[serde(default)]
        permission_suggestions: Vec<serde_json::Value>,
    },
    #[serde(other)]
    Other,
}

#[derive(Debug, Deserialize)]
pub struct ControlResponseEnvelope {
    pub request_id: String,
    #[serde(default)]
    pub response: serde_json::Value,
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn parses_init_session_id() {
        let v = r#"{"type":"system","subtype":"init","session_id":"abc","uuid":"u1"}"#;
        match OutputMessage::parse(v).unwrap() {
            OutputMessage::System(System::Init { session_id, .. }) => assert_eq!(session_id, "abc"),
            other => panic!("{other:?}"),
        }
    }
    #[test]
    fn parses_text_delta() {
        let v = r#"{"type":"stream_event","parent_tool_use_id":null,"event":{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Hi"}},"uuid":"u","session_id":"s"}"#;
        let m = OutputMessage::parse(v).unwrap();
        assert!(matches!(m, OutputMessage::StreamEvent(_)));
    }
    #[test]
    fn parses_result_success() {
        let v = r#"{"type":"result","subtype":"success","is_error":false,"result":"done","stop_reason":"end_turn","usage":{"input_tokens":1,"output_tokens":2,"cache_read_input_tokens":0,"cache_creation_input_tokens":0},"modelUsage":{"claude-x":{"contextWindow":1000000,"maxOutputTokens":64000,"inputTokens":1,"outputTokens":2,"cachedReadTokens":0,"cachedWriteTokens":0,"costUSD":0.0}},"uuid":"u","session_id":"s"}"#;
        match OutputMessage::parse(v).unwrap() {
            OutputMessage::Result(r) => {
                assert!(!r.is_error);
                assert_eq!(r.stop_reason.as_deref(), Some("end_turn"));
                assert_eq!(r.context_window_for_active_model(), Some(1_000_000));
            }
            other => panic!("{other:?}"),
        }
    }
    #[test]
    fn parses_error_result() {
        let v = r#"{"type":"result","subtype":"error_during_execution","is_error":true,"errors":["boom"],"stop_reason":null,"uuid":"u","session_id":"s"}"#;
        match OutputMessage::parse(v).unwrap() {
            OutputMessage::Result(r) => {
                assert!(r.is_error);
                assert_eq!(r.errors, vec!["boom".to_string()]);
            }
            other => panic!("{other:?}"),
        }
    }
    #[test]
    fn parses_can_use_tool_control_request() {
        let v = r#"{"type":"control_request","request_id":"r1","request":{"subtype":"can_use_tool","tool_name":"Bash","tool_use_id":"t1","input":{"command":"ls"}}}"#;
        match OutputMessage::parse(v).unwrap() {
            OutputMessage::ControlRequest(env) => {
                assert_eq!(env.request_id, "r1");
                assert!(matches!(env.request, ControlRequestKind::CanUseTool { .. }));
            }
            other => panic!("{other:?}"),
        }
    }
    #[test]
    fn parses_assistant_with_parent_tool_use_id() {
        let v = r#"{"type":"assistant","parent_tool_use_id":"toolu_1","message":{"role":"assistant","content":[]},"uuid":"u","session_id":"s"}"#;
        match OutputMessage::parse(v).unwrap() {
            OutputMessage::Assistant(m) => {
                assert_eq!(m.parent_tool_use_id.as_deref(), Some("toolu_1"))
            }
            other => panic!("{other:?}"),
        }
    }
    #[test]
    fn unknown_type_is_unknown_not_error() {
        assert!(matches!(
            OutputMessage::parse(r#"{"type":"ping"}"#).unwrap(),
            OutputMessage::Unknown
        ));
        assert!(matches!(
            OutputMessage::parse(r#"{"type":"rate_limit_event","x":1}"#).unwrap(),
            OutputMessage::Unknown
        ));
    }
}
