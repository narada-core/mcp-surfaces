
fn touch_snapshot(state: &mut State, id: &str) {
    state.snapshot_order.retain(|entry| entry != id);
    state.snapshot_order.push(id.to_string());
    while state.snapshot_order.len() > 4 {
        let evicted = state.snapshot_order.remove(0);
        state.snapshots.remove(&evicted);
        state
            .cache
            .retain(|_, (snapshot, _, _)| snapshot != &evicted);
    }
}

fn run_search_command(
    scope: &Path,
    pattern: &str,
    args: &Value,
    grep: bool,
    output_mode: &str,
    operation: &str,
) -> Result<(Vec<String>, bool), FsError> {
    let mut rg_args = Vec::new();
    if grep {
        rg_args.extend(
            ["--field-match-separator", "\u{1f}", "--with-filename"]
                .iter()
                .map(|value| value.to_string()),
        );
        rg_args.push(
            match output_mode {
                "content" => "-n",
                "count_matches" => "-c",
                _ => "-l",
            }
            .to_string(),
        );
    } else {
        rg_args.extend(
            ["--files", "--hidden", "--no-ignore"]
                .iter()
                .map(|value| value.to_string()),
        );
        rg_args.push("-g".to_string());
        rg_args.push(pattern.to_string());
    }
    let ignores = if grep {
        DEFAULT_GREP_IGNORES
    } else {
        DEFAULT_GLOB_IGNORES
    };
    for ignore in ignores {
        rg_args.push("-g".to_string());
        rg_args.push(format!("!{ignore}"));
    }
    if let Some(extra) = args.get("ignore").and_then(Value::as_array) {
        for ignore in extra.iter().filter_map(Value::as_str) {
            rg_args.push("-g".to_string());
            rg_args.push(format!("!{ignore}"));
        }
    }
    if grep {
        rg_args.extend(
            ["--", pattern, &scope.to_string_lossy()]
                .iter()
                .map(|value| value.to_string()),
        );
    } else {
        rg_args.push(scope.to_string_lossy().to_string());
    }
    let matches = run_rg(
        &rg_args,
        args.get("timeout_ms")
            .and_then(Value::as_u64)
            .unwrap_or(SEARCH_TIMEOUT_MS),
        operation,
    )?;
    Ok((
        matches
            .0
            .into_iter()
            .map(|value| normalize_search_result(&value, grep))
            .collect(),
        matches.1,
    ))
}

fn normalize_search_result(value: &str, grep: bool) -> String {
    if !grep {
        return value.replace('\\', "/");
    }
    if let Some((path, remainder)) = value.split_once('\u{1f}') {
        return format!("{}\u{1f}{}", path.replace('\\', "/"), remainder);
    }
    value.replace('\\', "/")
}

fn repository_inventory(state: &mut State, args: &Value) -> Result<Value, FsError> {
    if args.get("directory").and_then(Value::as_str).is_some()
        && args.get("root").and_then(Value::as_str).is_some()
    {
        return Err(FsError::new(
            "repository_inventory_scope_ambiguous",
            "repository_inventory_scope_ambiguous",
            json!({"remediation": "Pass either directory or root, not both."}),
        ));
    }
    let include_generated = args
        .get("include_generated")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let mut cloned = args.clone();
    let object = cloned.as_object_mut().ok_or_else(|| {
        FsError::new(
            "arguments_must_be_object",
            "arguments_must_be_object",
            json!({}),
        )
    })?;
    if object.get("directory").and_then(Value::as_str).is_none() {
        if let Some(root) = object.remove("root") {
            object.insert("directory".into(), root);
        }
    }
    object.entry("pattern").or_insert(json!("**/*"));
    if !include_generated {
        let ignores = object.entry("ignore").or_insert(json!([]));
        if let Some(array) = ignores.as_array_mut() {
            array.extend(
                [
                    "**/.ai/runtime/**",
                    "**/.ai/tmp/**",
                    "**/.ai/output/**",
                    "**/.narada/runtime/**",
                    "**/.narada/tmp/**",
                    "**/.narada/local-filesystem-mcp/patch-outcomes/**",
                    "**/.tmp-tests/**",
                ]
                .iter()
                .map(|value| json!(value)),
            );
        }
    }
    let value = search_tool(state, &cloned, false)?;
    let matches = value.get("matches").cloned().unwrap_or(json!([]));
    let mut classifications = Vec::new();
    let mut candidates = Vec::new();
    let mut generated = Vec::new();
    if let Some(items) = matches.as_array() {
        for item in items.iter().filter_map(Value::as_str) {
            let classification = classify(item);
            classifications.push(json!({"path": item, "classification": classification}));
            if classification == "generated_artifact" {
                generated.push(item);
            } else {
                candidates.push(item);
            }
        }
    }
    let mut result = value.as_object().cloned().unwrap_or_default();
    result.insert(
        "schema".into(),
        json!("local.filesystem.repository_inventory.v1"),
    );
    result.insert(
        "directory".into(),
        json!(args
            .get("directory")
            .and_then(Value::as_str)
            .or_else(|| args.get("root").and_then(Value::as_str))
            .unwrap_or(".")),
    );
    result.insert(
        "pattern".into(),
        json!(args
            .get("pattern")
            .and_then(Value::as_str)
            .unwrap_or("**/*")),
    );
    result.insert("include_generated".into(), json!(include_generated));
    result.insert("classifications".into(), Value::Array(classifications));
    result.insert("candidate_source_paths".into(), json!(candidates));
    result.insert("candidate_source_count".into(), json!(candidates.len()));
    result.insert("generated_artifact_paths".into(), json!(generated));
    result.insert("generated_artifact_count".into(), json!(generated.len()));
    result.insert(
        "generated_artifacts_excluded_by_default".into(),
        json!(!include_generated),
    );
    result.insert("git_tracking_boundary".into(), json!({"tracked_paths": null, "ignored_paths": null, "authority": "git-mcp", "next_tool": "git_changed_summary", "note": "This filesystem inventory identifies bounded candidate and generated paths; Git-tracked and Git-ignored state is authoritative in git-mcp."}));
    Ok(Value::Object(result))
}

fn file_metrics(state: &mut State, args: &Value) -> Result<Value, FsError> {
    if args.get("directory").and_then(Value::as_str).is_some()
        && args.get("root").and_then(Value::as_str).is_some()
    {
        return Err(FsError::new(
            "file_metrics_directory_ambiguous",
            "file_metrics_directory_ambiguous",
            json!({"remediation": "Pass either directory or root, not both."}),
        ));
    }
    let directory_arg = args
        .get("directory")
        .and_then(Value::as_str)
        .or_else(|| args.get("root").and_then(Value::as_str))
        .unwrap_or(".");
    let (directory, root) = resolve_allowed(state, Some(directory_arg), "fs_file_metrics")?;
    let mut glob_args = args.clone();
    let object = glob_args.as_object_mut().ok_or_else(|| {
        FsError::new(
            "arguments_must_be_object",
            "arguments_must_be_object",
            json!({}),
        )
    })?;
    object.insert("directory".into(), json!(directory.to_string_lossy()));
    object.entry("pattern").or_insert(json!("**/*"));
    let mut all_matches = Vec::new();
    let mut page_offset = 0_i64;
    if let Some(object) = glob_args.as_object_mut() {
        object.insert("offset".into(), json!(page_offset));
        object.insert("limit".into(), json!(500));
    }
    let mut all = search_tool(state, &glob_args, false)?;
    loop {
        all_matches.extend(
            all.get("matches")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default(),
        );
        if all.get("has_more").and_then(Value::as_bool) != Some(true) {
            break;
        }
        page_offset = all
            .get("next_offset")
            .and_then(Value::as_i64)
            .unwrap_or(page_offset + 500);
        if all_matches.len() > 10_000 {
            break;
        }
        if let Some(object) = glob_args.as_object_mut() {
            object.insert("offset".into(), json!(page_offset));
            object.insert("limit".into(), json!(500));
        }
        all = search_tool(state, &glob_args, false)?;
    }
    let matches = all_matches;
    let metrics_snapshot_id = args
        .get("snapshot_id")
        .and_then(Value::as_str)
        .map(str::to_string)
        .or_else(|| {
            all.get("snapshot_id")
                .and_then(Value::as_str)
                .map(str::to_string)
        });
    let offset = integer(args, "offset").unwrap_or(0).max(0) as usize;
    let limit = integer(args, "limit").unwrap_or(100).clamp(1, 100) as usize;
    let max_file = integer(args, "max_bytes_per_file")
        .unwrap_or(8 * 1024 * 1024)
        .max(1) as u64;
    let max_total = integer(args, "max_total_scan_bytes")
        .unwrap_or(256 * 1024 * 1024)
        .max(1) as u64;
    let mut reserved = 0_u64;
    let mut files = Vec::new();
    for item in matches.iter().skip(offset).take(limit) {
        let Some(raw) = item.as_str() else { continue };
        let path = PathBuf::from(raw);
        let metadata = match fs::metadata(&path) {
            Ok(value) if value.is_file() => value,
            _ => continue,
        };
        let (line_count, status) = if metadata.len() > max_file {
            (Value::Null, "too_large")
        } else if reserved + metadata.len() > max_total {
            (Value::Null, "scan_budget_exceeded")
        } else {
            reserved += metadata.len();
            match count_lines(&path) {
                Ok((_count, binary)) if binary => (Value::Null, "binary"),
                Ok((count, _)) => (json!(count), "exact"),
                Err(_) => (Value::Null, "unavailable"),
            }
        };
        let relative = relative_path(&directory, &path);
        let root_relative = relative_path(&root, &path);
        files.push(json!({
            "path": path,
            "relative_path": relative,
            "root_relative_path": root_relative,
            "line_count": line_count,
            "line_count_status": status,
            "byte_count": metadata.len(),
            "file_type": if status == "binary" { "binary" } else { path.extension().and_then(|value| value.to_str()).unwrap_or("no_extension") },
            "scope_classification": classify(&root_relative),
            "mtime": mtime_iso(&metadata)
        }));
    }
    let has_more = offset + files.len() < matches.len();
    let mut result = Map::new();
    result.insert("schema".into(), json!("local.filesystem.file_metrics.v1"));
    result.insert("status".into(), json!("ok"));
    result.insert("directory".into(), json!(directory));
    result.insert(
        "pattern".into(),
        json!(args
            .get("pattern")
            .and_then(Value::as_str)
            .unwrap_or("**/*")),
    );
    result.insert("offset".into(), json!(offset));
    result.insert("limit".into(), json!(limit));
    result.insert("count".into(), json!(matches.len()));
    result.insert("count_exact".into(), json!(true));
    result.insert("returned".into(), json!(files.len()));
    result.insert("has_more".into(), json!(has_more));
    result.insert(
        "next_offset".into(),
        if has_more {
            json!(offset + files.len())
        } else {
            Value::Null
        },
    );
    result.insert("order".into(), json!("ripgrep_traversal"));
    result.insert("cache_hit".into(), json!(false));
    result.insert(
        "cache_policy".into(),
        json!(args
            .get("cache_policy")
            .and_then(Value::as_str)
            .unwrap_or("auto")),
    );
    result.insert(
        "snapshot_id".into(),
        metrics_snapshot_id
            .clone()
            .map(Value::String)
            .unwrap_or(Value::Null),
    );
    result.insert(
        "requested_snapshot_id".into(),
        args.get("snapshot_id").cloned().unwrap_or(Value::Null),
    );
    result.insert(
        "snapshot_complete".into(),
        json!(metrics_snapshot_id.is_some()),
    );
    result.insert(
        "timeout_ms".into(),
        args.get("timeout_ms").cloned().unwrap_or(json!(10_000)),
    );
    result.insert("scan_budget_bytes".into(), json!(max_total));
    result.insert("scan_bytes_reserved".into(), json!(reserved));
    result.insert("scope".into(), json!({"directory": directory, "allowed_root": root, "allowed_roots": state.allowed_roots, "include_pattern": args.get("pattern").and_then(Value::as_str).unwrap_or("**/*"), "ignore_patterns": DEFAULT_GLOB_IGNORES, "ignored_paths": [], "ignored_path_count": 0, "ignored_paths_complete": true, "ignored_paths_truncated": false, "out_of_scope_paths": [], "out_of_scope_path_count": 0, "out_of_scope_paths_complete": !has_more, "boundary": {"allowed_root": root, "directory": directory, "realpath_enforced": true}, "contents_returned": false}));
    result.insert("totals".into(), aggregate_metrics(&files));
    result.insert("totals_scope".into(), json!("returned_page"));
    result.insert("files".into(), Value::Array(files));
    Ok(Value::Object(result))
}
