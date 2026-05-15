//! JSON-RPC 2.0 dispatch surface for Remote Control.
//!
//! R-2 ships a minimal stub (`MinimalDispatcher`) with two methods —
//! `remote.editor.capabilities` and `remote.editor.ping` — just enough to
//! prove the wire works end-to-end. R-4 will replace this with an
//! `editor_mcp::call_tool` proxy gated by a `remote.*` allow-list.

use std::sync::Arc;

use chrono::Utc;
use futures::future::BoxFuture;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// JSON-RPC 2.0 request frame. We accept `id` as `Value` (number, string,
/// or null per spec) and `params` as either an array or object.
#[derive(Debug, Clone, Deserialize)]
pub struct JsonRpcRequest {
    pub jsonrpc: String,
    #[serde(default)]
    pub id: Value,
    pub method: String,
    #[serde(default)]
    pub params: Option<Value>,
}

/// JSON-RPC 2.0 response frame. Either `result` xor `error` is set per the
/// spec; serde's `skip_serializing_if = "Option::is_none"` enforces the
/// "missing means absent" wire shape.
#[derive(Debug, Clone, Serialize)]
pub struct JsonRpcResponse {
    pub jsonrpc: &'static str,
    pub id: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<JsonRpcError>,
}

#[derive(Debug, Clone, Serialize)]
pub struct JsonRpcError {
    pub code: i32,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

impl JsonRpcResponse {
    pub fn ok(id: Value, result: Value) -> Self {
        Self {
            jsonrpc: "2.0",
            id,
            result: Some(result),
            error: None,
        }
    }

    pub fn error(id: Value, code: i32, message: impl Into<String>) -> Self {
        Self {
            jsonrpc: "2.0",
            id,
            result: None,
            error: Some(JsonRpcError {
                code,
                message: message.into(),
                data: None,
            }),
        }
    }
}

/// Parse a single JSON-RPC frame. Returns a parse-error response (`-32700`)
/// when the bytes aren't valid JSON, rather than failing the caller — the
/// transport contract is "always reply with a JSON-RPC frame, never close
/// on a single bad frame." `Box`-ing the error variant keeps
/// `Result<JsonRpcRequest, Box<JsonRpcResponse>>` small enough for
/// `clippy::result_large_err`.
pub fn parse_request(text: &str) -> Result<JsonRpcRequest, Box<JsonRpcResponse>> {
    match serde_json::from_str::<JsonRpcRequest>(text) {
        Ok(req) if req.jsonrpc == "2.0" => Ok(req),
        Ok(req) => Err(Box::new(JsonRpcResponse::error(
            req.id,
            -32600,
            format!("expected jsonrpc=2.0, got {:?}", req.jsonrpc),
        ))),
        Err(err) => Err(Box::new(JsonRpcResponse::error(
            Value::Null,
            -32700,
            format!("parse error: {err}"),
        ))),
    }
}

/// Abstraction the listener calls per request — R-2's `MinimalDispatcher`
/// is a stub; R-4 plugs in the real `editor_mcp` proxy here.
pub trait RemoteDispatcher: Send + Sync {
    fn dispatch(
        &self,
        client_name: &str,
        request: JsonRpcRequest,
    ) -> BoxFuture<'static, JsonRpcResponse>;
}

/// R-2 stub. Two allow-listed methods, anything else → `-32601`.
pub struct MinimalDispatcher;

impl MinimalDispatcher {
    pub fn new() -> Arc<Self> {
        Arc::new(Self)
    }
}

impl Default for MinimalDispatcher {
    fn default() -> Self {
        Self
    }
}

impl RemoteDispatcher for MinimalDispatcher {
    fn dispatch(
        &self,
        _client_name: &str,
        request: JsonRpcRequest,
    ) -> BoxFuture<'static, JsonRpcResponse> {
        Box::pin(async move {
            match request.method.as_str() {
                "remote.editor.capabilities" => JsonRpcResponse::ok(
                    request.id,
                    serde_json::json!({
                        "protocol_version": 1,
                        "server_software": "spk-editor",
                        "tool_namespaces": ["remote.editor"],
                        "capabilities": ["json-rpc-2.0", "hmac-sha256-challenge"],
                    }),
                ),
                "remote.editor.ping" => JsonRpcResponse::ok(
                    request.id,
                    serde_json::json!({
                        "pong": true,
                        "now": Utc::now().to_rfc3339(),
                    }),
                ),
                other => JsonRpcResponse::error(
                    request.id,
                    -32601,
                    format!("method not found: {other}"),
                ),
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::executor::block_on;

    #[test]
    fn parse_rejects_non_json() {
        let err = parse_request("not json").expect_err("should fail");
        let parsed: Value =
            serde_json::to_value(&*err).expect("re-serialize error response");
        assert_eq!(parsed["error"]["code"].as_i64(), Some(-32700));
        assert_eq!(parsed["id"], Value::Null);
    }

    #[test]
    fn parse_rejects_wrong_jsonrpc_version() {
        let err = parse_request(r#"{"jsonrpc":"1.0","id":1,"method":"x"}"#)
            .expect_err("should fail");
        let parsed: Value = serde_json::to_value(&*err).expect("re-serialize");
        assert_eq!(parsed["error"]["code"].as_i64(), Some(-32600));
    }

    #[test]
    fn dispatch_capabilities_round_trip() {
        let dispatcher = MinimalDispatcher::new();
        let request: JsonRpcRequest = serde_json::from_str(
            r#"{"jsonrpc":"2.0","id":1,"method":"remote.editor.capabilities"}"#,
        )
        .expect("parse");
        let response = block_on(dispatcher.dispatch("client", request));
        let parsed: Value = serde_json::to_value(&response).expect("re-serialize");
        assert_eq!(parsed["jsonrpc"], "2.0");
        assert_eq!(parsed["id"], 1);
        assert_eq!(parsed["result"]["protocol_version"], 1);
        assert_eq!(parsed["result"]["server_software"], "spk-editor");
    }

    #[test]
    fn dispatch_ping_round_trip() {
        let dispatcher = MinimalDispatcher::new();
        let request: JsonRpcRequest = serde_json::from_str(
            r#"{"jsonrpc":"2.0","id":"42","method":"remote.editor.ping"}"#,
        )
        .expect("parse");
        let response = block_on(dispatcher.dispatch("client", request));
        let parsed: Value = serde_json::to_value(&response).expect("re-serialize");
        assert_eq!(parsed["id"], "42");
        assert_eq!(parsed["result"]["pong"], true);
        let now = parsed["result"]["now"].as_str().expect("now is string");
        assert!(!now.is_empty());
    }

    #[test]
    fn dispatch_unknown_method_is_method_not_found() {
        let dispatcher = MinimalDispatcher::new();
        let request: JsonRpcRequest = serde_json::from_str(
            r#"{"jsonrpc":"2.0","id":9,"method":"remote.unknown"}"#,
        )
        .expect("parse");
        let response = block_on(dispatcher.dispatch("client", request));
        let parsed: Value = serde_json::to_value(&response).expect("re-serialize");
        assert_eq!(parsed["error"]["code"].as_i64(), Some(-32601));
        assert!(parsed["error"]["message"]
            .as_str()
            .expect("message string")
            .contains("method not found"));
    }
}
