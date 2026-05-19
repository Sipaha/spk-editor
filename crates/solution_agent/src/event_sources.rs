//! MCP event-source wiring for `SolutionAgentStore`.
//!
//! Subscribes a long-lived coordinator entity to `SolutionAgentStoreEvent`s
//! emitted by the global store and republishes them as `editor/notification`
//! frames so external MCP clients (and Phase 5.6 e2e tests) can observe
//! session lifecycle changes without polling.
//!
//! Wired event kinds: `agent_session_created`, `agent_session_closed`,
//! `agent_session_state_changed`, `agent_session_title_changed`,
//! `agent_session_message_appended`, `agent_session_notification_sent`.

use gpui::{App, AppContext as _, Entity, Global, Subscription};
use serde_json::json;

use crate::mcp::truncate_preview;
use crate::notifier::NotifyKind;
use crate::store::{SolutionAgentStore, SolutionAgentStoreEvent};

pub struct EventSourceCoordinator {
    #[allow(dead_code)]
    subscriptions: Vec<Subscription>,
}

struct GlobalEventSourceCoordinator(#[allow(dead_code)] Entity<EventSourceCoordinator>);
impl Global for GlobalEventSourceCoordinator {}

/// Install the coordinator as a global. Idempotent: a second call is a
/// no-op (useful in tests that re-enter `solution_agent::init`). When the
/// `SolutionAgentStore` global is not initialised, returns without wiring
/// anything — `solution_agent::init` is responsible for ordering store
/// init before this call.
pub fn install(cx: &mut App) {
    if cx.try_global::<GlobalEventSourceCoordinator>().is_some() {
        return;
    }
    let Some(store) = SolutionAgentStore::try_global(cx) else {
        return;
    };

    let coordinator = cx.new(|_| EventSourceCoordinator {
        subscriptions: Vec::new(),
    });
    coordinator.update(cx, |this, cx| {
        this.subscriptions.push(
            cx.subscribe(&store, |_this, _store, event, cx| match event {
                SolutionAgentStoreEvent::SessionCreated {
                    id,
                    parent_session_id,
                } => {
                    editor_mcp::emit_notification(
                        cx,
                        "agent_session_created",
                        json!({
                            "session_id": id.to_string(),
                            // `null` (not omitted) for top-level sessions
                            // so the wire shape is self-documenting: a
                            // missing field looks like "old server"; an
                            // explicit null looks like "top-level".
                            "parent_session_id": parent_session_id.map(|p| p.to_string()),
                        }),
                    );
                }
                SolutionAgentStoreEvent::SessionClosed(id) => {
                    editor_mcp::emit_notification(
                        cx,
                        "agent_session_closed",
                        json!({ "session_id": id.to_string() }),
                    );
                }
                SolutionAgentStoreEvent::SessionStateChanged(id) => {
                    editor_mcp::emit_notification(
                        cx,
                        "agent_session_state_changed",
                        json!({ "session_id": id.to_string() }),
                    );
                }
                SolutionAgentStoreEvent::SessionTitleChanged(id) => {
                    editor_mcp::emit_notification(
                        cx,
                        "agent_session_title_changed",
                        json!({ "session_id": id.to_string() }),
                    );
                }
                SolutionAgentStoreEvent::SessionMessageAppended(id, entry_index) => {
                    let payload = build_message_appended_payload(*id, *entry_index, cx);
                    editor_mcp::emit_notification(
                        cx,
                        "agent_session_message_appended",
                        payload,
                    );
                }
                SolutionAgentStoreEvent::SessionNotified(id, kind) => {
                    let kind_str = match kind {
                        NotifyKind::Completed => "completed",
                        NotifyKind::AwaitingInput => "awaiting_input",
                        NotifyKind::Errored => "errored",
                    };
                    editor_mcp::emit_notification(
                        cx,
                        "agent_session_notification_sent",
                        json!({
                            "session_id": id.to_string(),
                            "kind": kind_str,
                        }),
                    );
                }
            }),
        );
    });

    cx.set_global(GlobalEventSourceCoordinator(coordinator));
}

/// Build the JSON payload for an `agent_session_message_appended`
/// notification. Pure function (no side effects) so unit tests can
/// assert wire shape without running an MCP server.
///
/// When the session is closed or its `acp_thread` is gone (race
/// between rotate / close and the queued notification), falls back
/// to a minimal payload with just `session_id` + `entry_index` so the
/// consumer can still bump its append counter and re-fetch.
pub(crate) fn build_message_appended_payload(
    session_id: crate::model::SolutionSessionId,
    entry_index: usize,
    cx: &App,
) -> serde_json::Value {
    let role_preview_csid = SolutionAgentStore::try_global(cx).and_then(|store| {
        store.read_with(cx, |store, cx| {
            let session = store.session(session_id)?;
            let session_ref = session.read(cx);
            let thread = session_ref.acp_thread()?;
            let thread_ref = thread.read(cx);
            let entry = thread_ref.entries().get(entry_index)?;
            let role = match entry {
                acp_thread::AgentThreadEntry::UserMessage(_) => "user",
                acp_thread::AgentThreadEntry::AssistantMessage(_) => "assistant",
                acp_thread::AgentThreadEntry::ToolCall(_) => "tool_call",
                acp_thread::AgentThreadEntry::CompletedPlan(_) => "plan",
            };
            let preview = truncate_preview(&entry.to_markdown(cx), 200);
            // Only user messages can carry originating-client send ids
            // (stamped on each content block's `_meta` by the client).
            // For other roles return an empty Vec; for users return
            // every distinct id we find — a single id for the common
            // one-shot send, multiple when the server-side queue merge
            // rolled N originating bundles into one ACP message (see
            // `client_send_ids_from_user_message`). Clients use the
            // list to pop every contributing optimistic bubble.
            let client_send_ids: Vec<i64> =
                if let acp_thread::AgentThreadEntry::UserMessage(message) = entry {
                    acp_thread::client_send_ids_from_user_message(message)
                } else {
                    Vec::new()
                };
            Some((role.to_string(), preview, client_send_ids))
        })
    });
    match role_preview_csid {
        Some((role, preview, csids)) if !csids.is_empty() => json!({
            "session_id": session_id.to_string(),
            "entry_index": entry_index,
            "role": role,
            "preview": preview,
            // Back-compat alias for pre-R6h mobile builds that only
            // know the singular field. Always the FIRST csid so the
            // legacy "pop one" path keeps working.
            "client_send_id": csids[0],
            "client_send_ids": csids,
        }),
        Some((role, preview, _)) => json!({
            "session_id": session_id.to_string(),
            "entry_index": entry_index,
            "role": role,
            "preview": preview,
        }),
        None => json!({
            "session_id": session_id.to_string(),
            "entry_index": entry_index,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapter::AdapterRegistry;
    use gpui::TestAppContext;
    use std::sync::Arc;

    #[gpui::test]
    async fn install_is_idempotent(cx: &mut TestAppContext) {
        cx.update(|cx| {
            let registry = Arc::new(AdapterRegistry::new());
            SolutionAgentStore::init_global(cx, registry);
            install(cx);
            install(cx);
            assert!(cx.try_global::<GlobalEventSourceCoordinator>().is_some());
        });
    }

    #[gpui::test]
    async fn install_without_store_global_is_a_no_op(cx: &mut TestAppContext) {
        cx.update(|cx| {
            install(cx);
            assert!(cx.try_global::<GlobalEventSourceCoordinator>().is_none());
        });
    }

    #[gpui::test]
    async fn store_event_does_not_panic(cx: &mut TestAppContext) {
        cx.update(|cx| {
            let registry = Arc::new(AdapterRegistry::new());
            SolutionAgentStore::init_global(cx, registry);
            install(cx);
            let store = SolutionAgentStore::global(cx);
            // Emit an event via the store. No MCP server is connected — emit
            // is a no-op, but we exercise the subscription path end-to-end.
            store.update(cx, |_s, cx| {
                cx.emit(SolutionAgentStoreEvent::SessionCreated {
                    id: crate::model::SolutionSessionId::new(),
                    parent_session_id: None,
                });
            });
        });
        cx.run_until_parked();
    }

    #[gpui::test]
    async fn message_appended_payload_carries_index_role_and_preview(
        cx: &mut TestAppContext,
    ) {
        // Build a real session with one user entry, then call the pure
        // payload builder directly — emit is a no-op without a socket,
        // so this is the only way to observe the wire shape from a
        // unit test.
        let (session_id, _acp_thread, _tmp) =
            crate::store::tests::create_session_with_thread(cx).await;
        cx.update(|cx| {
            let thread = {
                let store = SolutionAgentStore::global(cx);
                store.read(cx).session(session_id).and_then(|s| {
                    s.read(cx).acp_thread().cloned()
                })
            }
            .expect("thread");
            thread.update(cx, |thread, cx| {
                let chunk = agent_client_protocol::schema::ContentBlock::Text(
                    agent_client_protocol::schema::TextContent::new("hi".to_string()),
                );
                thread.push_user_content_block(None, chunk, cx);
            });
        });
        cx.executor().run_until_parked();

        cx.update(|cx| {
            let payload = build_message_appended_payload(session_id, 0, cx);
            let obj = payload.as_object().expect("object");
            assert_eq!(
                obj.get("session_id").and_then(|v| v.as_str()),
                Some(session_id.to_string().as_str())
            );
            assert_eq!(obj.get("entry_index").and_then(|v| v.as_u64()), Some(0));
            assert_eq!(obj.get("role").and_then(|v| v.as_str()), Some("user"));
            let preview = obj
                .get("preview")
                .and_then(|v| v.as_str())
                .expect("preview");
            assert!(preview.contains("hi"), "preview should contain 'hi': {preview}");
        });
    }

    #[gpui::test]
    async fn message_appended_payload_falls_back_when_thread_missing(
        cx: &mut TestAppContext,
    ) {
        let registry = Arc::new(AdapterRegistry::new());
        cx.update(|cx| SolutionAgentStore::init_global(cx, registry));

        cx.update(|cx| {
            let payload = build_message_appended_payload(
                crate::model::SolutionSessionId::new(),
                7,
                cx,
            );
            let obj = payload.as_object().expect("object");
            assert_eq!(obj.get("entry_index").and_then(|v| v.as_u64()), Some(7));
            assert!(obj.get("role").is_none());
            assert!(obj.get("preview").is_none());
        });
    }
}
