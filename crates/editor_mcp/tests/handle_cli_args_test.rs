use editor_mcp::tools_for_test::HandleCliArgsParams;

#[test]
fn params_deserialize_from_null() {
    let _: HandleCliArgsParams =
        serde_json::from_value(serde_json::Value::Null).expect("null accepted");
}

#[test]
fn params_deserialize_from_empty_object() {
    let _: HandleCliArgsParams =
        serde_json::from_value(serde_json::json!({})).expect("empty object accepted");
}

#[test]
fn params_deserialize_from_paths_only() {
    let p: HandleCliArgsParams = serde_json::from_value(serde_json::json!({
        "paths": ["/tmp/foo", "/tmp/bar"]
    }))
    .expect("parse");
    assert_eq!(p.paths.len(), 2);
    assert_eq!(p.paths[0], "/tmp/foo");
}
