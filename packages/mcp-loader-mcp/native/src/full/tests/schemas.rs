use crate::full::*;

fn assert_schema_bounded(schema: &Value) {
    match schema.get("type").and_then(Value::as_str) {
        Some("object") => {
            assert!(schema
                .get("maxProperties")
                .and_then(Value::as_u64)
                .is_some());
            if let Some(properties) = schema.get("properties").and_then(Value::as_object) {
                for child in properties.values() {
                    assert_schema_bounded(child);
                }
            }
        }
        Some("array") => {
            assert!(schema.get("maxItems").and_then(Value::as_u64).is_some());
            if let Some(items) = schema.get("items") {
                assert_schema_bounded(items);
            }
        }
        Some("string") => {
            assert!(schema.get("maxLength").and_then(Value::as_u64).is_some());
        }
        _ => {}
    }
}

#[test]
fn every_public_tool_schema_is_named_closed_bounded_and_rejects_unknown_input() {
    for tool in list_tools() {
        let name = tool["name"].as_str().expect("tool name");
        let schema = &tool["inputSchema"];
        assert_eq!(schema["title"], format!("{name}.input"));
        assert_eq!(schema["additionalProperties"], false);
        assert_schema_bounded(schema);
        let error = validate_input_schema(schema, &json!({"unexpected": true}), "$args")
            .expect_err("unknown input must be rejected");
        assert_eq!(error.code, "input_schema_validation_failed");
    }
}

#[test]
fn validation_error_reports_bound_received_value_and_corrected_call() {
    let schema = json!({"type":"object","properties":{"max_inline_chars":{"type":"integer","maximum":20000}},"additionalProperties":false});
    let error = validate_input_schema(&schema, &json!({"max_inline_chars":30000}), "/arguments")
        .expect_err("oversized value must be rejected");
    assert_eq!(error.code, "input_schema_validation_failed");
    assert_eq!(error.details["path"], "/arguments.max_inline_chars");
    assert_eq!(error.details["constraint"], "maximum");
    assert_eq!(error.details["expected"], 20000);
    assert_eq!(error.details["received"], 30000);
    assert_eq!(
        error.details["corrected_call_template"]["arguments"]["max_inline_chars"],
        20000
    );
    assert!(error.message.contains("expected 20000; received 30000"));
}

#[test]
fn wire_parser_refuses_oversized_framed_and_jsonl_messages() {
    let mut framed = b"Content-Length: 100\r\n\r\n{}".to_vec();
    assert_eq!(
        try_parse_wire(&mut framed, 16).unwrap_err().kind(),
        io::ErrorKind::InvalidData
    );
    let mut jsonl = vec![b'x'; 18];
    jsonl.push(b'\n');
    assert_eq!(
        try_parse_wire(&mut jsonl, 16).unwrap_err().kind(),
        io::ErrorKind::InvalidData
    );
}
