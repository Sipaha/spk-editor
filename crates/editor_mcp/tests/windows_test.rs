use editor_mcp::tools_for_test::{
    CloseWindowParams, DispatchActionParams, FocusWindowParams, ListWindowsParams,
};

#[test]
fn params_deserialize_from_null() {
    let _: ListWindowsParams =
        serde_json::from_value(serde_json::Value::Null).expect("null accepted");
}

#[test]
fn params_deserialize_from_empty_object() {
    let _: ListWindowsParams =
        serde_json::from_value(serde_json::json!({})).expect("empty object accepted");
}

#[test]
fn focus_params_round_trip() {
    let p: FocusWindowParams = serde_json::from_value(serde_json::json!({
        "window_id": "window:42"
    }))
    .expect("parse");
    assert_eq!(p.window_id, "window:42");
}

#[test]
fn focus_params_accepts_null() {
    let p: FocusWindowParams =
        serde_json::from_value(serde_json::Value::Null).expect("null accepted");
    assert!(p.window_id.is_empty());
}

#[test]
fn close_params_round_trip() {
    let p: CloseWindowParams = serde_json::from_value(serde_json::json!({
        "window_id": "window:7"
    }))
    .expect("parse");
    assert_eq!(p.window_id, "window:7");
}

#[test]
fn dispatch_action_params_with_args() {
    let p: DispatchActionParams = serde_json::from_value(serde_json::json!({
        "window_id": "window:5",
        "action_name": "workspace::ToggleLeftDock",
        "args": null
    }))
    .expect("parse");
    assert_eq!(p.window_id, "window:5");
    assert_eq!(p.action_name, "workspace::ToggleLeftDock");
}
