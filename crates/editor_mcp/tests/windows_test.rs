use editor_mcp::tools_for_test::ListWindowsParams;

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
