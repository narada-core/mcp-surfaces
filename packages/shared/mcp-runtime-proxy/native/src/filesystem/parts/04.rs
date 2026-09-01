
fn tool_result(value: Value) -> Value {
    let text = if value.get("content").and_then(Value::as_str).is_some() {
        serde_json::to_string_pretty(&value)
    } else {
        serde_json::to_string(&json!({
            "schema": value.get("schema"),
            "status": value.get("status"),
            "path": value.get("path"),
            "count": value.get("count"),
            "returned": value.get("returned")
        }))
    }
    .unwrap_or_else(|_| "{}".to_string());
    let structured_content = without_duplicated_read_content(&value);
    json!({"content": [{"type": "text", "text": text, "annotations": {"audience": ["assistant"]}}], "structuredContent": structured_content})
}

fn without_duplicated_read_content(value: &Value) -> Value {
    if value.get("schema").and_then(Value::as_str) != Some("local.filesystem.read.v1") {
        return value.clone();
    }
    let mut structured = value.as_object().cloned().unwrap_or_default();
    if structured.remove("content").is_some() {
        structured.insert(
            "content_delivery".to_string(),
            json!({
                "channel": "content",
                "block_index": 0,
                "format": "filesystem_read_text",
                "duplicated_in_structured_content": false
            }),
        );
    }
    Value::Object(structured)
}

fn diagnostic(error: &FsError) -> Value {
    json!({"schema": "local.filesystem.error.v1", "code": error.code, "message": error.message, "details": add_diagnostic_details(error.details.clone())})
}

fn add_diagnostic_details(value: Value) -> Value {
    let mut details = value.as_object().cloned().unwrap_or_default();
    details.insert(
        "diagnostic_owner".to_string(),
        json!("local-filesystem-mcp"),
    );
    details.insert(
        "diagnostic_rule".to_string(),
        json!("surface_policy_or_tool_validation"),
    );
    details.insert("false_positive_route".to_string(), json!("Submit surface feedback with surface_id=local-filesystem, the refusal code, requested_path, and why the path classification is wrong. Do not include secret content."));
    Value::Object(details)
}

fn guidance(state: &State, args: &Value) -> Result<Value, FsError> {
    let apply_patch_available = list_tools(&state.mode)
        .iter()
        .any(|tool| tool.get("name").and_then(Value::as_str) == Some("fs_apply_patch"));
    Ok(json!({
        "schema": "narada.mcp_surface.guidance.v0",
        "status": "ok",
        "surface_id": "local-filesystem",
        "guidance_tool": "fs_guidance",
        "purpose": "Governed filesystem inspection and mutation under allowed roots.",
        "requested": {"workflow": args.get("workflow"), "tool": args.get("tool")},
        "path_resolution": {
            "base": "The first allowed root returned by fs_doctor.allowed_roots.",
            "relative_paths": "Resolve relative filesystem paths against the first allowed root.",
            "absolute_paths": "Prefer absolute paths when multiple roots are allowed.",
            "git_boundary": "Use git-mcp for authoritative tracked and ignored state."
        },
        "patch_recovery": {
            "apply_patch_available": apply_patch_available,
            "sequence": if apply_patch_available {
                json!(["Choose a stable operation_id.", "Call fs_apply_patch once.", "After timeout call fs_patch_outcome_show.", "Retry only when retry_safe is true."])
            } else {
                json!(["Use fs_patch_outcome_show only to inspect an operation_id produced by another compatible filesystem surface."])
            },
            "statuses": {"failed_before_mutation": "Parsing, validation, or planning failed and no mutation started."},
            "read_mode": "fs_patch_outcome_show is available in read mode."
        },
        "repository_inventory": {
            "sequence": ["Call fs_repository_inventory with an explicit directory, pattern, limit, and cache policy.", "Use candidate_source_paths and generated_artifact_paths.", "Set include_generated only for an explicit investigation.", "Call git_changed_summary for authoritative tracked and ignored state."],
            "default_behavior": "Known generated runtime/artifact patterns are excluded unless include_generated is true."
        },
        "file_metrics": {
            "sequence": ["Call fs_file_metrics with an explicit directory, pattern, limit, and cache policy.", "Use the files table for path, line_count, byte_count, and file_type.", "Use offset and next_offset to page larger trees."],
            "semantics": {"line_count": "Exact within the configured byte and scan budgets.", "byte_count": "Filesystem byte size from stat metadata.", "scope": "The response declares the allowed root and selected directory."}
        },
        "range_reads": {
            "page_size_lines": MAX_READ_LINES,
            "behavior": "A logical range larger than one page succeeds with a bounded first page.",
            "sequence": ["Call fs_read_file_range with the complete logical start_line and end_line.", "When has_more is true, call the same tool with continuation.arguments.", "Do not switch to a native filesystem or shell reader to bypass pagination."]
        },
        "first_use": ["Call fs_doctor before discovery.", "Use bounded reads and searches.", "Preserve structuredContent as authoritative evidence."]
    }))
}

fn doctor(state: &State) -> Value {
    let read_tools = vec![
        "fs_guidance",
        "fs_read_file",
        "fs_read_file_range",
        "fs_stat",
        "fs_glob_search",
        "fs_grep_search",
        "fs_repository_inventory",
        "fs_file_metrics",
        "fs_doctor",
        "fs_patch_outcome_show",
    ];
    let write_tools: Vec<&str> = vec![
        "fs_write_file",
        "fs_str_replace_file",
        "fs_replace_range",
        "fs_apply_patch",
        "fs_move_path",
        "fs_create_directory",
        "fs_rename_directory",
        "fs_delete_directory",
    ];
    let available_tools: Vec<String> = list_tools(&state.mode)
        .iter()
        .filter_map(|tool| tool.get("name").and_then(Value::as_str).map(str::to_string))
        .collect();
    let can_write = state.mode == "write";
    json!({
        "schema": "local.filesystem.doctor.v1",
        "status": "ok",
        "mode": state.mode,
        "allowed_roots": state.allowed_roots.iter().map(|path| path.to_string_lossy().to_string()).collect::<Vec<_>>(),
        "allowed_root_entries": state.root_entries,
        "relative_path_resolution": {
            "base": state.allowed_roots.first().map(|path| path.to_string_lossy().to_string()),
            "rule": "first_allowed_root",
            "relative_paths": "Resolve relative filesystem paths against base; the process current directory is not used.",
            "absolute_paths": "Resolve absolute paths as given, then enforce containment under an allowed root.",
            "recommendation": "Pass an absolute path when multiple roots are active or when the target root matters."
        },
        "output_root": state.output_root.to_string_lossy(),
        "audit_log_dir": state.audit_log_dir.as_ref().map(|path| path.to_string_lossy().to_string()),
        "client_roots": {"supported": false, "roots": [], "lastUpdatedAt": Value::Null},
        "effective_permissions": {"can_read": true, "can_write": can_write, "can_mutate_paths": can_write, "can_delete_directories": can_write,"can_write_patch_recovery_records":true},
        "available_tools": available_tools,
        "read_tools": read_tools,
        "recovery_tools":["fs_patch_outcome_show"],
        "write_tools": write_tools,
        "default_glob_ignore_patterns": DEFAULT_GLOB_IGNORES,
        "default_grep_ignore_patterns": DEFAULT_GREP_IGNORES
    })
}

fn read_file(state: &State, args: &Value, range: bool) -> Result<Value, FsError> {
    let (path, root) = resolve_allowed(
        state,
        args.get("path").and_then(Value::as_str),
        if range {
            "fs_read_file_range"
        } else {
            "fs_read_file"
        },
    )?;
    let (offset, requested_limit, limit, requested_end_line) = if range {
        let start = integer(args, "start_line").ok_or_else(|| {
            FsError::new(
                "start_line_must_be_positive_integer",
                "start_line_must_be_positive_integer",
                json!({}),
            )
        })?;
        let end = integer(args, "end_line").ok_or_else(|| {
            FsError::new(
                "end_line_must_be_greater_than_or_equal_start_line",
                "end_line_must_be_greater_than_or_equal_start_line",
                json!({}),
            )
        })?;
        if start < 1 {
            return Err(FsError::new(
                "start_line_must_be_positive_integer",
                "start_line_must_be_positive_integer",
                json!({"start_line": start}),
            ));
        }
        if end < start {
            return Err(FsError::new(
                "end_line_must_be_greater_than_or_equal_start_line",
                "end_line_must_be_greater_than_or_equal_start_line",
                json!({"start_line": start, "end_line": end}),
            ));
        }
        let requested = end - start + 1;
        (start, requested, requested.min(MAX_READ_LINES), Some(end))
    } else {
        let requested = integer(args, "limit").unwrap_or(400).max(1);
        if requested > MAX_READ_LINES {
            return Err(FsError::new(
                "fs_read_file_limit_exceeds_max",
                "fs_read_file_limit_exceeds_max",
                json!({"offset": integer(args, "offset").unwrap_or(1).max(1), "requested_limit": requested, "max_limit": MAX_READ_LINES, "pagination_required": true, "mutation_started": false}),
            ));
        }
        (
            integer(args, "offset").unwrap_or(1).max(1),
            requested,
            requested,
            None,
        )
    };
    let timeout = integer(args, "timeout_ms")
        .unwrap_or(READ_TIMEOUT_MS as i64)
        .clamp(1, 60_000) as u64;
    let window = stream_text_window(
        &path,
        &root,
        offset as usize,
        limit as usize,
        timeout,
        if range {
            "fs_read_file_range"
        } else {
            "fs_read_file"
        },
    )?;
    let content = window.selected.join("\n");
    let next_offset = if let Some(requested_end) = requested_end_line {
        window.next_offset.filter(|next| *next <= requested_end)
    } else {
        window.next_offset
    };
    let continuation = if range {
        next_offset
            .map(|next| {
                json!({
                    "tool": "fs_read_file_range",
                    "arguments": {
                        "path": path,
                        "start_line": next,
                        "end_line": requested_end_line,
                        "timeout_ms": timeout
                    }
                })
            })
            .unwrap_or(Value::Null)
    } else {
        Value::Null
    };
    let served_end_line = if window.selected.is_empty() {
        Value::Null
    } else {
        json!(offset + window.selected.len() as i64 - 1)
    };
    let (total_lines, total_lines_exact, line_window_complete) = if window.complete {
        (json!(window.total_lines), true, true)
    } else {
        (Value::Null, false, false)
    };
    Ok(json!({
        "schema": "local.filesystem.read.v1",
        "path": path,
        "root": root,
        "relative_path": relative_path(&root, &path),
        "total_lines": total_lines,
        "total_lines_exact": total_lines_exact,
        "total_lines_status": if total_lines_exact { "exact" } else { "unknown_after_window" },
        "line_window_complete": line_window_complete,
        "offset": offset,
        "limit": limit,
        "requested_limit": requested_limit,
        "requested_start_line": if range { json!(offset) } else { Value::Null },
        "requested_end_line": requested_end_line,
        "served_end_line": served_end_line,
        "returned_lines": window.selected.len(),
        "next_offset": next_offset,
        "next_start_line": if range { next_offset.map_or(Value::Null, Value::from) } else { Value::Null },
        "continuation": continuation,
        "content": content,
        "content_sha256": window.sha256,
        "content_hash_scope": "full_file",
        "hash_source": "live_file_bytes",
        "cache_used": false,
        "content_window_sha256": sha256_bytes(content.as_bytes()),
        "max_limit": MAX_READ_LINES,
        "limit_adjusted": limit != requested_limit,
        "pagination_required": next_offset.is_some(),
        "has_more": next_offset.is_some(),
        "requested_range_complete": if range { next_offset.is_none() } else { true },
        "timeout_ms": timeout
    }))
}
