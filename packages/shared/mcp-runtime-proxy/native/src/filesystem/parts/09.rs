
fn read_bounded_mutation_text(
    path: &Path,
    root: &Path,
    operation: &str,
) -> Result<String, FsError> {
    let metadata = fs::metadata(path).map_err(|error| {
        FsError::new(
            format!("{operation}_read_failed"),
            format!("{operation}_read_failed: {error}"),
            path_details(path, root),
        )
    })?;
    if metadata.len() > MAX_TEXT_MUTATION_BYTES {
        return Err(FsError::new(
            format!("{operation}_file_too_large"),
            format!("{operation}_file_too_large"),
            json!({"path":path,"size":metadata.len(),"max_bytes":MAX_TEXT_MUTATION_BYTES}),
        ));
    }
    fs::read_to_string(path).map_err(|error| {
        FsError::new(
            format!("{operation}_read_failed"),
            format!("{operation}_read_failed: {error}"),
            path_details(path, root),
        )
    })
}

fn search_tool(state: &mut State, args: &Value, grep: bool) -> Result<Value, FsError> {
    let operation = if grep {
        "fs_grep_search"
    } else {
        "fs_glob_search"
    };
    let scope_arg = if grep {
        args.get("path").and_then(Value::as_str).unwrap_or(".")
    } else {
        args.get("directory").and_then(Value::as_str).unwrap_or(".")
    };
    let (scope, _root) = resolve_allowed(state, Some(scope_arg), operation)?;
    let pattern = args.get("pattern").and_then(Value::as_str).ok_or_else(|| {
        FsError::new(
            if grep {
                "grep_requires_pattern"
            } else {
                "glob_requires_pattern"
            },
            if grep {
                "grep_requires_pattern"
            } else {
                "glob_requires_pattern"
            },
            json!({}),
        )
    })?;
    let offset = integer(args, "offset").unwrap_or(0).max(0) as usize;
    let limit = integer(args, "limit")
        .unwrap_or(if grep { 80 } else { 100 })
        .clamp(1, 500) as usize;
    let cache_policy = args
        .get("cache_policy")
        .and_then(Value::as_str)
        .unwrap_or("auto");
    if !["auto", "snapshot", "refresh", "bypass"].contains(&cache_policy) {
        return Err(FsError::new(
            "search_cache_policy_unsupported",
            format!("search_cache_policy_unsupported: {cache_policy}"),
            json!({"cache_policy": cache_policy}),
        ));
    }
    let output_mode = args
        .get("output_mode")
        .and_then(Value::as_str)
        .unwrap_or("content");
    if grep && !["files_with_matches", "count_matches", "content"].contains(&output_mode) {
        return Err(FsError::new(
            "grep_output_mode_unsupported",
            format!("grep_output_mode_unsupported: {output_mode}"),
            json!({"output_mode": output_mode}),
        ));
    }
    let snapshot_id = args.get("snapshot_id").and_then(Value::as_str);
    let cache_key = sha256_bytes(
        format!(
            "{}|{}|{}|{}|{}",
            grep,
            scope.to_string_lossy(),
            pattern,
            output_mode,
            args.get("ignore").cloned().unwrap_or(Value::Null)
        )
        .as_bytes(),
    );
    let mut cache_hit = false;
    let mut snapshot_reused = false;
    let mut cached_snapshot: Option<String> = None;
    let (all_matches, snapshot_complete) = if let Some(snapshot) = snapshot_id {
        let captured = state.snapshots.get(snapshot).cloned().ok_or_else(|| {
            FsError::new(
                format!("{operation}_snapshot_not_found"),
                format!("{operation}_snapshot_not_found: {snapshot}"),
                json!({"snapshot_id": snapshot}),
            )
        })?;
        cache_hit = true;
        snapshot_reused = true;
        captured
    } else if cache_policy != "bypass" && cache_policy != "refresh" {
        if let Some((id, matches, complete)) = state.cache.get(&cache_key).cloned() {
            cache_hit = true;
            cached_snapshot = Some(id);
            (matches, complete)
        } else {
            run_search_command(&scope, pattern, args, grep, output_mode, operation)?
        }
    } else {
        run_search_command(&scope, pattern, args, grep, output_mode, operation)?
    };
    let snapshot = if let Some(snapshot) = snapshot_id {
        Some(snapshot.to_string())
    } else if let Some(snapshot) = cached_snapshot {
        Some(snapshot)
    } else if cache_policy != "bypass" {
        let digest = sha256_bytes(
            format!(
                "{cache_key}\n{}\n{snapshot_complete}",
                all_matches.join("\n")
            )
            .as_bytes(),
        );
        let id = format!("s_{}", &digest[..24]);
        state.cache.insert(
            cache_key,
            (id.clone(), all_matches.clone(), snapshot_complete),
        );
        state
            .snapshots
            .insert(id.clone(), (all_matches.clone(), snapshot_complete));
        Some(id)
    } else {
        snapshot_id.map(str::to_string)
    };
    if let Some(id) = snapshot.as_deref() {
        touch_snapshot(state, id);
    }
    if !snapshot_complete && offset >= all_matches.len() {
        return Err(FsError::new(
            format!("{operation}_capture_boundary_reached"),
            format!(
                "{operation}_capture_boundary_reached: the bounded search capture is exhausted"
            ),
            json!({
                "offset": offset,
                "captured_entries": all_matches.len(),
                "snapshot_id": snapshot,
                "remediation": "Narrow the search path or pattern, then start a refreshed search."
            }),
        ));
    }
    let page: Vec<String> = all_matches
        .iter()
        .skip(offset)
        .take(limit)
        .cloned()
        .collect();
    let has_more = offset + page.len() < all_matches.len() || !snapshot_complete;
    let mut value = Map::new();
    value.insert(
        "schema".into(),
        json!(if grep {
            "local.filesystem.grep.v1"
        } else {
            "local.filesystem.glob.v1"
        }),
    );
    value.insert("status".into(), json!("ok"));
    if grep {
        value.insert("output_mode".into(), json!(output_mode));
    }
    value.insert("offset".into(), json!(offset));
    value.insert("limit".into(), json!(limit));
    value.insert("count".into(), json!(all_matches.len()));
    value.insert("count_exact".into(), json!(snapshot_complete));
    value.insert("scanned".into(), json!(all_matches.len()));
    value.insert("scanned_unit".into(), json!("matched_entries"));
    value.insert("returned".into(), json!(page.len()));
    value.insert("order".into(), json!("ripgrep_traversal"));
    value.insert("cache_hit".into(), json!(cache_hit));
    value.insert("snapshot_reused".into(), json!(snapshot_reused));
    value.insert("cache_policy".into(), json!(cache_policy));
    value.insert(
        "snapshot_id".into(),
        snapshot.clone().map(Value::String).unwrap_or(Value::Null),
    );
    value.insert(
        "requested_snapshot_id".into(),
        snapshot_id.map(|value| json!(value)).unwrap_or(Value::Null),
    );
    value.insert("snapshot_complete".into(), json!(snapshot_complete));
    value.insert(
        "cache_memory_bytes".into(),
        json!(all_matches.iter().map(|value| value.len()).sum::<usize>()),
    );
    value.insert("page_match_bytes".into(), Value::Null);
    value.insert("page_match_bytes_limit".into(), json!(512 * 1024));
    value.insert("page_matches_truncated".into(), json!(0));
    value.insert(
        "timeout_ms".into(),
        args.get("timeout_ms")
            .cloned()
            .unwrap_or(json!(SEARCH_TIMEOUT_MS)),
    );
    value.insert("freshness".into(), freshness(&scope));
    value.insert("has_more".into(), json!(has_more));
    value.insert(
        "next_offset".into(),
        if has_more {
            json!(offset + page.len())
        } else {
            Value::Null
        },
    );
    value.insert(
        "continuation".into(),
        if has_more {
            let mut next_arguments = args.as_object().cloned().unwrap_or_default();
            next_arguments.insert("offset".into(), json!(offset + page.len()));
            if grep {
                next_arguments.insert("output_mode".into(), json!(output_mode));
            }
            json!({"tool_name": if grep {"fs_grep_search"} else {"fs_glob_search"}, "arguments": next_arguments})
        } else {
            Value::Null
        },
    );
    if grep {
        value.insert("matches_format".into(), json!("human"));
        value.insert(
            "matches".into(),
            json!(page
                .iter()
                .map(|line| render_grep(line, output_mode))
                .collect::<Vec<_>>()),
        );
        value.insert("match_objects_authoritative".into(), json!(true));
        value.insert(
            "match_objects".into(),
            Value::Array(
                page.iter()
                    .map(|line| grep_match_object(line, output_mode))
                    .collect(),
            ),
        );
    } else {
        value.insert("matches_format".into(), json!("path"));
        value.insert("matches".into(), json!(page));
    }
    if page.is_empty() && all_matches.is_empty() {
        value.insert("no_match_diagnostics".into(), json!({
            "status": "no_matches_observed",
            "cache_hit": cache_hit,
            "cache_policy": cache_policy,
            "snapshot_complete": true,
            "freshness": value.get("freshness").cloned().unwrap_or(Value::Null),
            "stale_cache_evidence": false,
            "remediation": "No matches were returned for the current path freshness fingerprint."
        }));
    }
    Ok(Value::Object(value))
}
