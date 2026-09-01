const SEARCH_INLINE_CHAR_LIMIT: usize = 6_000;
const SEARCH_MAX_INLINE_CHAR_LIMIT: usize = 20_000;
const SEARCH_DEFAULT_ITEM_TEXT_CHARS: usize = 500;
const SEARCH_MAX_ITEM_TEXT_CHARS: usize = 2_000;
const OUTPUT_MAX_BYTES: u64 = 10 * 1024 * 1024;
const OUTPUT_PAGE_MAX_CHARS: usize = 20_000;

fn grep_pattern_matches_empty(pattern: &str) -> bool {
    matches!(pattern.trim(), "^" | "$" | ".*" | "^.*$" | "(?s:.*)" | "(?:)")
}

fn escape_regular_expression(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for character in value.chars() {
        if matches!(character, '.' | '*' | '+' | '?' | '^' | '$' | '(' | ')' | '[' | ']' | '{' | '}' | '|' | '\\') {
            escaped.push('\\');
        }
        escaped.push(character);
    }
    escaped
}

fn fs_search_tool(state: &mut State, args: &Value) -> Result<Value, FsError> {
    let object = args.as_object().ok_or_else(|| {
        FsError::new(
            "tool_arguments_must_be_object",
            "tool_arguments_must_be_object",
            json!({"tool_name": "fs_search"}),
        )
    })?;
    let query = object
        .get("query")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| FsError::new("fs_search_requires_query", "fs_search_requires_query", json!({})))?;
    let syntax = object.get("syntax").and_then(Value::as_str).unwrap_or("literal");
    if !["literal", "regex"].contains(&syntax) {
        return Err(FsError::new(
            "fs_search_syntax_unsupported",
            format!("fs_search_syntax_unsupported: {syntax}"),
            json!({"syntax": syntax}),
        ));
    }
    let result_kind = object
        .get("result_kind")
        .and_then(Value::as_str)
        .unwrap_or("matches");
    if !["matches", "files", "counts"].contains(&result_kind) {
        return Err(FsError::new(
            "fs_search_result_kind_unsupported",
            format!("fs_search_result_kind_unsupported: {result_kind}"),
            json!({"result_kind": result_kind}),
        ));
    }
    let case_mode = object.get("case").and_then(Value::as_str).unwrap_or("smart");
    if !["smart", "sensitive", "insensitive"].contains(&case_mode) {
        return Err(FsError::new(
            "fs_search_case_unsupported",
            format!("fs_search_case_unsupported: {case_mode}"),
            json!({"case": case_mode}),
        ));
    }
    let cursor = decode_search_cursor(object.get("cursor").and_then(Value::as_str))?;
    let pattern = if syntax == "literal" {
        escape_regular_expression(query)
    } else {
        query.to_string()
    };
    let output_mode = match result_kind {
        "matches" => "content",
        "files" => "files_with_matches",
        _ => "count_matches",
    };
    let max_results = integer(args, "max_results")
        .unwrap_or(20)
        .clamp(1, 100);
    let mut legacy = object.clone();
    legacy.insert("pattern".into(), Value::String(pattern));
    legacy.insert(
        "directory".into(),
        object
            .get("directory")
            .cloned()
            .unwrap_or_else(|| json!(".")),
    );
    legacy.insert("output_mode".into(), json!(output_mode));
    legacy.insert("limit".into(), json!(max_results));
    legacy.insert("max_matches".into(), json!(max_results));
    legacy.insert(
        "cache_policy".into(),
        json!(if cursor.is_some() { "auto" } else { "snapshot" }),
    );
    if let Some((offset, snapshot_id)) = cursor.as_ref() {
        legacy.insert("offset".into(), json!(offset));
        legacy.insert("snapshot_id".into(), json!(snapshot_id));
    } else {
        legacy.insert("offset".into(), json!(0));
    }
    legacy.insert("search_case".into(), json!(case_mode));
    if let Some(file_glob) = object.get("file_glob") {
        legacy.insert("glob".into(), file_glob.clone());
    }
    let result = search_tool(state, &Value::Object(legacy), true)?;
    Ok(build_fs_search_result(args, result))
}

fn build_fs_search_result(args: &Value, legacy: Value) -> Value {
    let object = args.as_object().cloned().unwrap_or_default();
    let result_kind = object
        .get("result_kind")
        .and_then(Value::as_str)
        .unwrap_or("matches");
    let syntax = object
        .get("syntax")
        .and_then(Value::as_str)
        .unwrap_or("literal");
    let case_mode = object.get("case").and_then(Value::as_str).unwrap_or("smart");
    let text_limit = integer(args, "max_text_chars_per_match")
        .unwrap_or(SEARCH_DEFAULT_ITEM_TEXT_CHARS as i64)
        .clamp(50, SEARCH_MAX_ITEM_TEXT_CHARS as i64) as usize;
    let empty = Vec::new();
    let matches = legacy
        .get("match_objects")
        .and_then(Value::as_array)
        .unwrap_or(&empty);
    let root = legacy.pointer("/scope/root").and_then(Value::as_str).unwrap_or("");
    let items = matches
        .iter()
        .map(|item| compact_search_item(item, result_kind, root, text_limit))
        .collect::<Vec<_>>();
    let returned = items.len();
    let has_more = legacy.get("has_more").and_then(Value::as_bool).unwrap_or(false);
    let truncated = legacy
        .get("page_matches_truncated")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let complete = !has_more && truncated == 0;
    let directory = legacy
        .pointer("/scope/path")
        .and_then(Value::as_str)
        .map(|path| relative_path(Path::new(root), Path::new(path)))
        .unwrap_or_else(|| ".".to_string());
    let mut value = Map::new();
    value.insert("schema".into(), json!("local.filesystem.search.v2"));
    value.insert("status".into(), json!(if truncated > 0 { "partial" } else { "ok" }));
    value.insert("result_kind".into(), json!(result_kind));
    value.insert(
        "scope".into(),
        json!({
            "directory": directory,
            "file_glob": object.get("file_glob"),
            "default_exclusions_applied": true
        }),
    );
    value.insert(
        "query".into(),
        json!({"text": object.get("query"), "syntax": syntax, "case": case_mode}),
    );
    value.insert("items".into(), Value::Array(items));
    value.insert(
        "page".into(),
        json!({
            "returned": returned,
            "complete": complete,
            "result_count": legacy.get("count").cloned().unwrap_or(Value::Null),
            "result_count_exact": legacy.get("count_exact").and_then(Value::as_bool).unwrap_or(false),
            "inline_chars": 0,
            "inline_char_limit": integer(args, "max_inline_chars").unwrap_or(SEARCH_INLINE_CHAR_LIMIT as i64).clamp(SEARCH_INLINE_CHAR_LIMIT as i64, SEARCH_MAX_INLINE_CHAR_LIMIT as i64),
            "clipped_items": 0
        }),
    );
    let continuation = if has_more {
        let offset = legacy
            .get("next_offset")
            .and_then(Value::as_u64)
            .unwrap_or(returned as u64);
        let snapshot_id = legacy
            .get("snapshot_id")
            .and_then(Value::as_str)
            .unwrap_or("");
        let mut next_args = object.clone();
        next_args.insert(
            "cursor".into(),
            json!(encode_search_cursor(offset as usize, snapshot_id)),
        );
        json!({"tool": "fs_search", "arguments": next_args})
    } else {
        Value::Null
    };
    value.insert("continuation".into(), continuation);
    value.insert("result_ref".into(), Value::Null);
    if object.get("diagnostics").and_then(Value::as_bool) == Some(true) {
        value.insert(
            "diagnostics".into(),
            json!({
                "cache_hit": legacy.get("cache_hit"),
                "cache_policy": legacy.get("cache_policy"),
                "snapshot_id": legacy.get("snapshot_id"),
                "snapshot_complete": legacy.get("snapshot_complete"),
                "order": legacy.get("order"),
                "timeout_ms": legacy.get("timeout_ms"),
                "page_match_bytes": legacy.get("page_match_bytes"),
                "page_match_bytes_limit": legacy.get("page_match_bytes_limit")
            }),
        );
    }
    Value::Object(value)
}

fn compact_search_item(item: &Value, result_kind: &str, root: &str, text_limit: usize) -> Value {
    let path = item.get("path").and_then(Value::as_str).unwrap_or("");
    let relative = relative_path(Path::new(root), Path::new(path));
    match result_kind {
        "files" => json!({"path": relative}),
        "counts" => json!({"path": relative, "count": item.get("count").cloned().unwrap_or(Value::Null)}),
        _ => {
            let original = item.get("text").and_then(Value::as_str).unwrap_or("");
            let original_chars = original.chars().count();
            if original_chars <= text_limit {
                json!({"path": relative, "line": item.get("line").cloned().unwrap_or(Value::Null), "text": original, "text_complete": true})
            } else {
                let suffix = "… [clipped]";
                let keep = text_limit.saturating_sub(suffix.chars().count());
                let text = format!("{}{}", original.chars().take(keep).collect::<String>(), suffix);
                json!({"path": relative, "line": item.get("line").cloned().unwrap_or(Value::Null), "text": text, "text_complete": false, "original_text_chars": original_chars, "returned_text_chars": text.chars().count()})
            }
        }
    }
}

fn decode_search_cursor(value: Option<&str>) -> Result<Option<(usize, String)>, FsError> {
    let Some(value) = value.filter(|value| !value.is_empty()) else {
        return Ok(None);
    };
    let mut parts = value.splitn(3, ':');
    if parts.next() != Some("nfs1") {
        return Err(FsError::new(
            "fs_search_cursor_invalid",
            "fs_search_cursor_invalid",
            json!({"remediation": "Use the opaque cursor returned by fs_search without modification."}),
        ));
    }
    let offset = parts.next().and_then(|part| part.parse::<usize>().ok());
    let snapshot = parts.next().filter(|part| !part.is_empty());
    match (offset, snapshot) {
        (Some(offset), Some(snapshot)) => Ok(Some((offset, snapshot.to_string()))),
        _ => Err(FsError::new(
            "fs_search_cursor_invalid",
            "fs_search_cursor_invalid",
            json!({"remediation": "Use the opaque cursor returned by fs_search without modification."}),
        )),
    }
}

fn encode_search_cursor(offset: usize, snapshot_id: &str) -> String {
    format!("nfs1:{offset}:{snapshot_id}")
}

fn bounded_search_tool_result(state: &State, tool_name: &str, value: Value, args: &Value) -> Value {
    let inline_limit = if tool_name == "fs_search" {
        integer(args, "max_inline_chars")
            .unwrap_or(SEARCH_INLINE_CHAR_LIMIT as i64)
            .clamp(SEARCH_INLINE_CHAR_LIMIT as i64, SEARCH_MAX_INLINE_CHAR_LIMIT as i64) as usize
    } else {
        SEARCH_INLINE_CHAR_LIMIT
    };
    let text = serde_json::to_string(&value).unwrap_or_else(|_| "{}".to_string());
    if text.chars().count() <= inline_limit {
        return tool_result(value);
    }
    let output_id = next_output_id();
    let reference = format!("mcp_output:{output_id}");
    let output_path = state
        .output_root
        .join(".ai")
        .join("tmp")
        .join("mcp-outputs")
        .join("workspace")
        .join(format!("{output_id}.json"));
    let record = json!({
        "schema": "narada.mcp_output_ref.v1",
        "ref": reference,
        "output_id": output_id,
        "tool_name": tool_name,
        "created_at": now_rfc3339(),
        "content_type": "application/json",
        "inline_char_limit": inline_limit,
        "full_output_char_length": text.chars().count(),
        "truncated": true,
        "sha256": sha256_bytes(text.as_bytes()),
        "max_bytes": OUTPUT_MAX_BYTES,
        "full_output": value,
    });
    let serialized = format!("{}\n", serde_json::to_string(&record).unwrap_or_else(|_| "{}".to_string()));
    if serialized.len() as u64 <= OUTPUT_MAX_BYTES {
        if let Some(parent) = output_path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        if let Ok(mut file) = OpenOptions::new().write(true).create_new(true).open(&output_path) {
            let _ = file.write_all(serialized.as_bytes());
            let _ = file.flush();
        }
    }
    let preview = text.chars().take(inline_limit.min(2_000)).collect::<String>();
    let envelope = json!({
        "schema": "narada.producer_output_page.v1",
        "status": value.get("status").and_then(Value::as_str).unwrap_or("ok"),
        "truncated": true,
        "truncation_reason": "inline_transport_bound",
        "output_ref": reference,
        "ref": reference,
        "result_materialized": true,
        "tool_name": tool_name,
        "offset": 0,
        "limit": inline_limit,
        "next_offset": text.chars().count(),
        "transport_offset": 0,
        "transport_limit": inline_limit,
        "transport_next_offset": text.chars().count(),
        "output_text": preview,
        "output_truncated": true,
        "reader_tool": "fs_search_results_read",
        "inline_limit": inline_limit,
        "full_output_char_length": text.chars().count(),
    });
    json!({"resultType":"complete","content":[{"type":"text","text":serde_json::to_string(&envelope).unwrap_or_else(|_| "{}".to_string()),"annotations":{"audience":["assistant"]}}],"structuredContent":envelope})
}

fn fs_search_results_read(state: &State, args: &Value) -> Result<Value, FsError> {
    let object = args.as_object().cloned().unwrap_or_default();
    let reference = object
        .get("ref")
        .or_else(|| object.get("output_ref"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    let Some(id) = reference.strip_prefix("mcp_output:") else {
        return Err(FsError::new(
            "output_ref_invalid",
            format!("output_ref_invalid: {reference}"),
            json!({"ref": reference}),
        ));
    };
    if id.len() < 3
        || id.len() > 64
        || !id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
    {
        return Err(FsError::new(
            "output_ref_invalid",
            format!("output_ref_invalid: {reference}"),
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
    let metadata = fs::metadata(&path).map_err(|_| {
        FsError::new(
            "output_ref_not_found",
            format!("output_ref_not_found: {reference}"),
            json!({"ref": reference}),
        )
    })?;
    if !metadata.is_file() {
        return Err(FsError::new(
            "output_ref_not_file",
            format!("output_ref_not_file: {reference}"),
            json!({"ref": reference}),
        ));
    }
    if metadata.len() > OUTPUT_MAX_BYTES {
        return Err(FsError::new(
            "output_ref_too_large",
            format!("output_ref_too_large: {}", metadata.len()),
            json!({"ref": reference, "size": metadata.len(), "max_bytes": OUTPUT_MAX_BYTES}),
        ));
    }
    let bytes = fs::read(&path).map_err(|error| {
        FsError::new(
            "output_ref_read_failed",
            format!("output_ref_read_failed: {error}"),
            json!({"ref": reference}),
        )
    })?;
    let record: Value = serde_json::from_slice(&bytes).map_err(|error| {
        FsError::new(
            "output_ref_invalid_json",
            format!("output_ref_invalid_json: {error}"),
            json!({"ref": reference}),
        )
    })?;
    if record.get("schema").and_then(Value::as_str) != Some("narada.mcp_output_ref.v1")
        || record.get("ref").and_then(Value::as_str) != Some(reference)
        || record.get("output_id").and_then(Value::as_str) != Some(id)
    {
        return Err(FsError::new(
            "output_ref_metadata_mismatch",
            format!("output_ref_metadata_mismatch: {reference}"),
            json!({"ref": reference}),
        ));
    }
    let full_output = record.get("full_output").cloned().unwrap_or(Value::Null);
    let text = serde_json::to_string(&full_output).unwrap_or_else(|_| "null".to_string());
    if record.get("sha256").and_then(Value::as_str) != Some(&sha256_bytes(text.as_bytes())) {
        return Err(FsError::new(
            "output_ref_sha256_mismatch",
            format!("output_ref_sha256_mismatch: {reference}"),
            json!({"ref": reference}),
        ));
    }
    let offset = integer(args, "offset").unwrap_or(0).max(0) as usize;
    let limit = integer(args, "limit")
        .unwrap_or(4_000)
        .clamp(1, OUTPUT_PAGE_MAX_CHARS as i64) as usize;
    let chars = text.chars().collect::<Vec<_>>();
    let start = offset.min(chars.len());
    let end = (start + limit).min(chars.len());
    let output_text = chars[start..end].iter().collect::<String>();
    Ok(json!({
        "schema": "narada.mcp_output_page.v1",
        "status": "ok",
        "ref": reference,
        "tool_name": record.get("tool_name"),
        "full_output_char_length": chars.len(),
        "byte_size": metadata.len(),
        "original_truncated": record.get("truncated").and_then(Value::as_bool).unwrap_or(true),
        "path": relative_path(&state.output_root, &path),
        "offset": offset,
        "limit": limit,
        "next_offset": if end < chars.len() { json!(end) } else { Value::Null },
        "output_limit": limit,
        "output_truncated": end < chars.len(),
        "output_text": output_text,
    }))
}

fn next_output_id() -> String {
    static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    format!("o_{:x}_{:x}", nanos, COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed))
}
