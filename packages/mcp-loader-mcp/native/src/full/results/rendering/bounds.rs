use crate::full::*;

pub(crate) fn output_id() -> String {
    format!(
        "o_{}{}",
        now_ms(),
        ID_COUNTER.fetch_add(1, Ordering::SeqCst)
    )
}

pub(crate) fn bounded_page(
    text: &str,
    offset: usize,
    limit: usize,
    max_bytes: usize,
) -> (String, usize) {
    let chars: Vec<char> = text.chars().collect();
    if offset >= chars.len() {
        return (String::new(), chars.len());
    }
    let mut end = (offset + limit).min(chars.len());
    while end > offset {
        let chunk: String = chars[offset..end].iter().collect();
        if chunk.len() <= max_bytes {
            return (chunk, end);
        }
        end -= 1;
    }
    (String::new(), offset)
}

pub(crate) fn compact_child_result(value: &Value) -> Value {
    let Some(object) = value.as_object() else {
        return value.clone();
    };
    if !object.contains_key("structuredContent") {
        return value.clone();
    }
    let mut compacted = object.clone();
    compacted.remove("content");
    Value::Object(compacted)
}

pub(crate) fn build_bounded_result(
    state: &LoaderState,
    connection_id: &str,
    tool_name: &str,
    value: &Value,
    is_error: bool,
) -> Result<Value, Diagnostic> {
    let full_text = pretty_json(value);
    let inline_limit = DEFAULT_LOADER_RESULT_INLINE_LIMIT;
    if utf16_len(&full_text) <= inline_limit
        && full_text.len() + json_byte_len(value) <= MAX_INLINE_RESPONSE_BYTES
    {
        return Ok(
            json!({"content":[{"type":"text","text":full_text,"annotations":{"audience":["assistant"]}}],"structuredContent":value,"isError":if is_error {Value::Bool(true)} else {Value::Null}}),
        );
    }
    let connection = state.connections.get(connection_id).ok_or_else(|| {
        Diagnostic::new(
            "connection_not_found",
            format!("connection_not_found:{}", connection_id),
        )
    })?;
    let id = output_id();
    let reference = format!("mcp_output:{}", id);
    let record = json!({
        "schema":"narada.mcp_output_ref.v1","ref":reference,"output_id":id,"tool_name":tool_name,
        "created_at":now_iso(),"created_by":env::var("NARADA_AGENT_ID").ok(),"content_type":"application/json",
        "inline_char_limit":inline_limit,"full_output_char_length":utf16_len(&full_text),"truncated":true,
        "sha256":sha256(&stable_json(value)),"max_bytes":state.policy.max_response_bytes,"full_output":value
    });
    let serialized = format!("{}\n", stable_json(&record));
    let path = join_path(&output_root(&connection.site_root), &format!("{}.json", id));
    if !write_immutable(&path, &serialized)? {
        return Err(Diagnostic::new(
            "mcp_output_ref_collision",
            format!("mcp_output_ref_collision:{}", reference),
        ));
    }
    let mut preview_limit = inline_limit.min(MAX_OUTPUT_SHOW_CHAR_LIMIT);
    let envelope = loop {
        let (preview, end) = bounded_page(&full_text, 0, preview_limit, MAX_OUTPUT_PAGE_BYTES);
        let next = if end < full_text.chars().count() {
            Some(end)
        } else {
            None
        };
        let envelope = json!({
            "schema":"narada.producer_output_page.v1","status":output_status(value,is_error),"truncated":true,
            "output_ref":reference,"ref":reference,"result_materialized":true,"tool_name":tool_name,
            "offset":0,"limit":inline_limit,"next_offset":next,"transport_offset":0,"transport_limit":inline_limit,
            "transport_next_offset":next,"output_text":preview,"output_truncated":next.is_some(),"reader_tool":"mcp_loader_read_result",
            "site_root":connection.site_root,
            "read_command":format!("mcp_loader_read_result({{ \"ref\": \"{}\", \"offset\": 0, \"limit\": {} }})",reference,DEFAULT_OUTPUT_SHOW_CHAR_LIMIT),
            "remediation":format!("Use mcp_loader_read_result with output_ref/ref={} to read the bounded produced JSON pages; continue with the returned next_offset.",reference),
            "inline_limit":inline_limit,"full_output_char_length":utf16_len(&full_text)
        });
        if json_byte_len(&Value::String(
            serde_json::to_string(&envelope).unwrap_or_default(),
        )) <= inline_limit
            && json_byte_len(&envelope) <= MAX_INLINE_RESPONSE_BYTES
        {
            break envelope;
        }
        if preview_limit == 0 {
            return Err(Diagnostic::new(
                "inline_output_envelope_limit_too_small",
                "inline_output_envelope_limit_too_small",
            ));
        }
        preview_limit = preview_limit.saturating_mul(3) / 4;
    };
    Ok(
        json!({"content":[{"type":"text","text":serde_json::to_string(&envelope).unwrap_or_default(),"annotations":{"audience":["assistant"]}}],"structuredContent":envelope,"isError":if is_error {Value::Bool(true)} else {Value::Null}}),
    )
}

pub(crate) fn output_status(value: &Value, is_error: bool) -> String {
    value
        .get("status")
        .and_then(Value::as_str)
        .filter(|text| text.len() <= 32)
        .map(String::from)
        .unwrap_or_else(|| {
            if is_error {
                "error".to_string()
            } else {
                "ok".to_string()
            }
        })
}
