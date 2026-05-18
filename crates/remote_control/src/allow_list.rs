//! `remote.*` allow-list: maps WS-side method names to upstream MCP tool
//! names, and filters which `editor/notification` kinds get forwarded out
//! to the WebSocket client.
//!
//! Method allow-list is the only authorisation gate — the post-HMAC
//! WebSocket session has carte blanche on whatever methods this module
//! says yes to. Everything else returns JSON-RPC -32601 ("method not
//! found"), so reconnaissance can't distinguish "banned" from "typo".
//!
//! Notification filter is a BLOCK-list at the fan-out layer: the upstream
//! server gladly fires every kind the client subscribed to, and we drop
//! the disallowed ones before they hit the WS write. This keeps the
//! upstream `editor.subscribe` protocol untouched and the filter
//! self-contained.

/// Translate a `remote.*` method name to the bare upstream tool name.
/// Returns `None` for any method outside the allow-list — the caller
/// reports -32601.
pub fn translate(method: &str) -> Option<&'static str> {
    match method {
        "remote.editor.capabilities" => Some("editor.capabilities"),
        "remote.editor.subscribe" => Some("editor.subscribe"),
        "remote.editor.unsubscribe" => Some("editor.unsubscribe"),
        "remote.editor.list_subscriptions" => Some("editor.list_subscriptions"),
        "remote.solutions.list" => Some("solutions.list"),
        "remote.solutions.get" => Some("solutions.get"),
        "remote.solutions.open" => Some("solutions.open"),
        "remote.solution_agent.list_agents" => Some("solution_agent.list_agents"),
        "remote.solution_agent.list_sessions" => Some("solution_agent.list_sessions"),
        "remote.solution_agent.get_session" => Some("solution_agent.get_session"),
        "remote.solution_agent.get_session_entry" => Some("solution_agent.get_session_entry"),
        "remote.solution_agent.create_session" => Some("solution_agent.create_session"),
        "remote.solution_agent.send_message" => Some("solution_agent.send_message"),
        "remote.solution_agent.cancel_turn" => Some("solution_agent.cancel_turn"),
        "remote.solution_agent.get_session_children" => {
            Some("solution_agent.get_session_children")
        }
        "remote.solution_agent.rename_session" => Some("solution_agent.rename_session"),
        _ => None,
    }
}

/// Forward `agent_session_*` events to the WS client, drop everything
/// else. The `kind` lives at `params.kind` on the upstream notification
/// frame — see `crates/editor_mcp/src/notifications.rs::emit` and
/// `crates/editor_mcp/tests/notifications_e2e_test.rs` for the on-wire
/// shape.
///
/// Block-list rationale: local-state events (`buffer_opened`,
/// `lsp_started`, `solution_changed`, etc.) leak filesystem and project
/// state we don't want the Android client poking at this phase. The
/// agent-session events are exactly what an Android pager-like client
/// needs to stream a turn live.
pub fn should_forward_event(kind: &str) -> bool {
    kind.starts_with("agent_session_")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allow_list_round_trip() {
        // Every documented allow-listed method translates to its bare
        // counterpart. The strings are paired by hand; if you add an
        // entry to `translate`, mirror it here.
        let cases = &[
            ("remote.editor.capabilities", "editor.capabilities"),
            ("remote.editor.subscribe", "editor.subscribe"),
            ("remote.editor.unsubscribe", "editor.unsubscribe"),
            (
                "remote.editor.list_subscriptions",
                "editor.list_subscriptions",
            ),
            ("remote.solutions.list", "solutions.list"),
            ("remote.solutions.get", "solutions.get"),
            ("remote.solutions.open", "solutions.open"),
            (
                "remote.solution_agent.list_agents",
                "solution_agent.list_agents",
            ),
            (
                "remote.solution_agent.list_sessions",
                "solution_agent.list_sessions",
            ),
            (
                "remote.solution_agent.get_session",
                "solution_agent.get_session",
            ),
            (
                "remote.solution_agent.get_session_entry",
                "solution_agent.get_session_entry",
            ),
            (
                "remote.solution_agent.create_session",
                "solution_agent.create_session",
            ),
            (
                "remote.solution_agent.send_message",
                "solution_agent.send_message",
            ),
            (
                "remote.solution_agent.cancel_turn",
                "solution_agent.cancel_turn",
            ),
            (
                "remote.solution_agent.get_session_children",
                "solution_agent.get_session_children",
            ),
            (
                "remote.solution_agent.rename_session",
                "solution_agent.rename_session",
            ),
        ];
        for (wire, bare) in cases {
            assert_eq!(translate(wire), Some(*bare), "for {wire}");
        }
    }

    #[test]
    fn banned_methods_return_none() {
        // File CRUD, project ops, full workspace dumps — explicitly NOT
        // exposed to remote clients per ADR-0003 § "How to apply".
        let banned = &[
            "remote.lsp.start",
            "remote.project.open_file",
            "remote.project.delete_file",
            "remote.workspace.screenshot",
            "remote.windows.send_keystroke",
            "remote.editor.handle_cli_args",
            "editor.capabilities", // bare name without `remote.` prefix
            "remote.solutions.delete",
            "remote.catalog.add_project",
        ];
        for method in banned {
            assert_eq!(translate(method), None, "for {method}");
        }
    }

    #[test]
    fn unknown_method_returns_none() {
        assert_eq!(translate(""), None);
        assert_eq!(translate("garbage"), None);
        assert_eq!(translate("remote."), None);
    }

    #[test]
    fn agent_session_kinds_forward() {
        assert!(should_forward_event("agent_session_created"));
        assert!(should_forward_event("agent_session_closed"));
        assert!(should_forward_event("agent_session_state_changed"));
        assert!(should_forward_event("agent_session_title_changed"));
        assert!(should_forward_event("agent_session_message_appended"));
        assert!(should_forward_event("agent_session_notification_sent"));
    }

    #[test]
    fn local_state_kinds_are_blocked() {
        assert!(!should_forward_event("solution_changed"));
        assert!(!should_forward_event("solution_active_changed"));
        assert!(!should_forward_event("buffer_opened"));
        assert!(!should_forward_event("buffer_saved"));
        assert!(!should_forward_event("lsp_started"));
        assert!(!should_forward_event("lsp_stopped"));
        assert!(!should_forward_event("diagnostic_updated"));
        assert!(!should_forward_event("operation_progress"));
        assert!(!should_forward_event("operation_completed"));
        assert!(!should_forward_event("window_focused"));
        assert!(!should_forward_event(""));
        // A typo'd `agentsession_` (missing underscore) must NOT match —
        // the prefix is exact, not fuzzy.
        assert!(!should_forward_event("agentsession_created"));
    }
}
