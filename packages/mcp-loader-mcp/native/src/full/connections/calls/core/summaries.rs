use crate::full::*;

pub(crate) fn typed_result_summary(result: &Value) -> Value {
    let structured = result
        .get("structuredContent")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    let mut summary = json!({
        "schema":structured.get("schema").and_then(Value::as_str).unwrap_or("narada.mcp_loader.child_result.v1"),
        "status":structured.get("status").and_then(Value::as_str).unwrap_or(if result.get("isError").and_then(Value::as_bool).unwrap_or(false) {"error"} else {"ok"}),
        "is_error":result.get("isError").and_then(Value::as_bool).unwrap_or(false)
    });
    if let Some(summary_object) = summary.as_object_mut() {
        for key in [
            "code",
            "message",
            "summary",
            "surface_id",
            "task_id",
            "task_number",
            "ref",
            "output_ref",
            "next_offset",
            "truncated",
        ] {
            if let Some(value) = structured.get(key) {
                if value.is_string() || value.is_number() || value.is_boolean() || value.is_null() {
                    summary_object.insert(key.to_string(), value.clone());
                }
            }
        }
        for key in ["count", "total", "checked_surface_count", "violation_count"] {
            if let Some(value) = structured.get(key).filter(|value| value.is_number()) {
                summary_object.insert(key.to_string(), value.clone());
            }
        }
        if let Some(items) = structured.get("items").and_then(Value::as_array) {
            summary_object.insert("item_count".to_string(), json!(items.len()));
        }
        if let Some(findings) = structured.get("findings").and_then(Value::as_array) {
            summary_object.insert("finding_count".to_string(), json!(findings.len()));
        }
    }
    summary
}

pub(crate) fn request_error_details(details: &Value, method: &str, timeout_ms: u64) -> Value {
    let mut result = details.as_object().cloned().unwrap_or_default();
    result.insert("method".to_string(), json!(method));
    result.insert("timeout_ms".to_string(), json!(timeout_ms));
    Value::Object(result)
}

pub(crate) fn child_runtime_diagnostic(connection: &Connection, extra: Value) -> Value {
    let mut result = json!({
        "connection_id":connection.connection_id,"surface_id":connection.surface_id,"entrypoint":connection.entrypoint,
        "args":connection.args,"exit_code":connection.session.exit_code(),"signal_code":connection.session.signal_code(),
        "stderr_tail":connection.session.stderr_tail(),
        "runtime_lifecycle":runtime_lifecycle(Some(&connection.connection_id),Some(&connection.lifecycle))
    });
    if let (Some(target), Some(source)) = (result.as_object_mut(), extra.as_object()) {
        for (key, value) in source {
            target.insert(key.clone(), value.clone());
        }
    }
    result
}
