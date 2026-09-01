
fn page_text(text: &str, offset: usize, limit: usize) -> (String, Option<usize>) {
    let chars = text.chars().collect::<Vec<_>>();
    let start = offset.min(chars.len());
    let end = (start + limit).min(chars.len());
    (
        chars[start..end].iter().collect(),
        if end < chars.len() { Some(end) } else { None },
    )
}

fn tool_result(state: &State, payload: Value, tool_name: &str) -> Result<Value, GitError> {
    let text = serde_json::to_string_pretty(&payload).unwrap_or_else(|_| "{}".to_string());
    if text.chars().count() <= 6_000 {
        return Ok(
            json!({"content": [{"type": "text", "text": text, "annotations": {"audience": ["assistant"]}}], "structuredContent": payload}),
        );
    }
    let id = unique_id("o");
    let reference = format!("mcp_output:{id}");
    let path = state
        .output_root
        .join(".ai")
        .join("tmp")
        .join("mcp-outputs")
        .join("workspace")
        .join(format!("{id}.json"));
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|error| {
            GitError::new("git_output_persist_failed", error.to_string(), json!({}))
        })?;
    }
    let record = json!({"schema": "narada.mcp_output_ref.v1", "ref": reference, "output_id": id, "tool_name": tool_name, "created_at": "", "full_output_char_length": text.chars().count(), "truncated": true, "sha256": "", "full_output": payload});
    fs::write(
        &path,
        serde_json::to_string(&record).unwrap_or_else(|_| "{}".to_string()),
    )
    .map_err(|error| GitError::new("git_output_persist_failed", error.to_string(), json!({})))?;
    let preview = text.chars().take(4_000).collect::<String>();
    let envelope = json!({"schema": "narada.producer_output_page.v1", "status": payload.get("status").and_then(Value::as_str).unwrap_or("ok"), "truncated": true, "output_ref": reference, "ref": reference, "result_materialized": true, "tool_name": tool_name, "offset": 0, "limit": 4_000, "next_offset": if text.chars().count() > 4_000 { json!(4_000) } else { Value::Null }, "output_text": preview, "output_truncated": text.chars().count() > 4_000, "reader_tool": "git_output_show", "full_output_char_length": text.chars().count()});
    Ok(
        json!({"content": [{"type": "text", "text": serde_json::to_string(&envelope).unwrap_or_else(|_| "{}".to_string()), "annotations": {"audience": ["assistant"]}}], "structuredContent": envelope}),
    )
}

fn unique_id(prefix: &str) -> String {
    static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    format!(
        "{prefix}_{}_{}_{}",
        std::process::id(),
        nanos,
        COUNTER.fetch_add(1, Ordering::Relaxed)
    )
}

fn output_show(state: &State, args: &Value) -> Result<Value, GitError> {
    let reference = args
        .get("ref")
        .and_then(Value::as_str)
        .or_else(|| args.get("output_ref").and_then(Value::as_str))
        .unwrap_or_default();
    let Some(id) = reference.strip_prefix("mcp_output:") else {
        return Err(GitError::new(
            "output_ref_invalid",
            "output_ref_invalid",
            json!({"ref": reference}),
        ));
    };
    if id.len() < 8
        || !id.chars().all(|character| {
            character.is_ascii_alphanumeric() || character == '_' || character == '-'
        })
    {
        return Err(GitError::new(
            "output_ref_invalid",
            "output_ref_invalid",
            json!({"ref": reference}),
        ));
    }
    let path = state
        .output_root
        .join(".ai")
        .join("tmp")
        .join("mcp-outputs")
        .join("workspace")
        .join(format!("{id}.json"));
    let record: Value = serde_json::from_slice(&fs::read(&path).map_err(|_| {
        GitError::new(
            "output_ref_not_found",
            "output_ref_not_found",
            json!({"ref": reference}),
        )
    })?)
    .map_err(|error| {
        GitError::new(
            "output_ref_invalid_json",
            error.to_string(),
            json!({"ref": reference}),
        )
    })?;
    let payload = record.get("full_output").cloned().unwrap_or(Value::Null);
    let text = serde_json::to_string_pretty(&payload).unwrap_or_else(|_| "null".to_string());
    let offset = args.get("offset").and_then(Value::as_u64).unwrap_or(0) as usize;
    let limit = args
        .get("limit")
        .and_then(Value::as_u64)
        .unwrap_or(10_000)
        .clamp(1, 20_000) as usize;
    let page = page_text(&text, offset, limit);
    Ok(
        json!({"schema": "narada.mcp_output_page.v1", "status": "ok", "ref": reference, "tool_name": record.get("tool_name"), "full_output_char_length": text.chars().count(), "offset": offset.min(text.chars().count()), "limit": limit, "output_limit": limit, "output_truncated": page.1.is_some(), "next_offset": page.1.map(|value| json!(value)).unwrap_or(Value::Null), "output_text": page.0}),
    )
}

