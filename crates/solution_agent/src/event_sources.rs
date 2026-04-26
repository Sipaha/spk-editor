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
        this.subscriptions.push(cx.subscribe(
            &store,
            |_this, _store, event, cx| match event {
                SolutionAgentStoreEvent::SessionCreated(id) => {
                    editor_mcp::emit_notification(
                        cx,
                        "agent_session_created",
                        json!({ "session_id": id.to_string() }),
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
                SolutionAgentStoreEvent::SessionMessageAppended(id) => {
                    editor_mcp::emit_notification(
                        cx,
                        "agent_session_message_appended",
                        json!({ "session_id": id.to_string() }),
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
            },
        ));
    });

    cx.set_global(GlobalEventSourceCoordinator(coordinator));
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
                cx.emit(SolutionAgentStoreEvent::SessionCreated(
                    crate::model::SolutionSessionId::new(),
                ));
            });
        });
        cx.run_until_parked();
    }
}
