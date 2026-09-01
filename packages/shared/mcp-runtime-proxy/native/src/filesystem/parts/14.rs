fn write_patch_outcome(state: &State, operation: &str, value: &Value) -> Result<(), FsError> {
    let path = patch_outcome_path(state, operation);
    let parent = path.parent().ok_or_else(|| {
        FsError::new(
            "fs_patch_outcome_path_invalid",
            "fs_patch_outcome_path_invalid",
            json!({"path":path}),
        )
    })?;
    fs::create_dir_all(parent).map_err(|e| {
        FsError::new(
            "fs_patch_outcome_write_failed",
            format!("fs_patch_outcome_write_failed: {e}"),
            json!({"path":path}),
        )
    })?;
    let temp = path.with_extension(format!("json.tmp-{}", std::process::id()));
    let bytes = serde_json::to_vec_pretty(value)
        .map_err(|e| FsError::new("fs_patch_outcome_encode_failed", e.to_string(), json!({})))?;
    fs::write(&temp, bytes)
        .and_then(|_| {
            if path.exists() {
                fs::remove_file(&path)?;
            }
            fs::rename(&temp, &path)
        })
        .map_err(|e| {
            FsError::new(
                "fs_patch_outcome_write_failed",
                format!("fs_patch_outcome_write_failed: {e}"),
                json!({"path":path}),
            )
        })
}
fn patch_failure<T>(
    state: &State,
    operation: &str,
    patch_sha: &str,
    error: FsError,
) -> Result<T, FsError> {
    let _ = write_patch_outcome(
        state,
        operation,
        &json!({"schema":"local.filesystem.apply_patch.outcome.v1","status":"failed_before_mutation","operation_id":operation,"patch_sha256":patch_sha,"mutation_started":false,"retry_safe":true,"error":diagnostic(&error),"finished_at":now_rfc3339()}),
    );
    Err(error)
}
fn patch_parse_error(code: &str, line: usize) -> FsError {
    FsError::new(code, code, json!({"line":line}))
}
fn now_rfc3339() -> String {
    OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .unwrap_or_else(|_| "unknown".into())
}

fn patch_outcome(state: &State, args: &Value) -> Result<Value, FsError> {
    let operation = args
        .get("operation_id")
        .and_then(Value::as_str)
        .ok_or_else(|| FsError::new("operation_id_required", "operation_id_required", json!({})))?;
    if !valid_operation_id(operation) {
        return Err(FsError::new(
            "patch_operation_id_invalid",
            "patch_operation_id_invalid",
            json!({"operation_id":operation}),
        ));
    }
    let value = read_patch_outcome(state, operation)?.ok_or_else(|| {
        FsError::new(
            "fs_patch_outcome_not_found",
            format!("fs_patch_outcome_not_found: {operation}"),
            json!({"operation_id":operation,"path":patch_outcome_path(state,operation)}),
        )
    })?;
    reconcile_patch_outcome(state, operation, value)
}

fn reconcile_patch_outcome(
    state: &State,
    operation: &str,
    mut value: Value,
) -> Result<Value, FsError> {
    let status = value
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if !matches!(status, "accepted" | "applying") {
        return Ok(value);
    }
    let pid = value.get("owner_pid").and_then(Value::as_u64).unwrap_or(0) as u32;
    if process_is_alive(pid) {
        if let Some(object) = value.as_object_mut() {
            object.insert("recovery".into(),json!({"status":"owner_active","terminal":false,"retry_safe":false,"remediation":"Wait for the owning MCP surface to finish, then call fs_patch_outcome_show again."}));
        }
        return Ok(value);
    }
    let (terminal, retry_safe, reason) = if status == "accepted" {
        (
            "interrupted_before_mutation",
            true,
            "owner_exited_before_mutation_started",
        )
    } else {
        let plan = value.get("recovery_plan").cloned().unwrap_or(Value::Null);
        let after = patch_state_set_matches(state, plan.get("after_state"));
        let before = patch_state_set_matches(state, plan.get("before_state"));
        if after {
            (
                "patched_recovered",
                false,
                "filesystem_matches_planned_after_state",
            )
        } else if before {
            (
                "interrupted_before_mutation",
                true,
                "filesystem_matches_captured_before_state",
            )
        } else {
            (
                "interrupted_partial",
                false,
                "filesystem_matches_neither_captured_state",
            )
        }
    };
    if let Some(object) = value.as_object_mut() {
        object.insert("status".into(), json!(terminal));
        object.insert("finished_at".into(), json!(now_rfc3339()));
        object.insert("recovered_at".into(), json!(now_rfc3339()));
        object.insert("retry_safe".into(), json!(retry_safe));
        object.insert("recovery".into(),json!({"status":terminal,"terminal":true,"retry_safe":retry_safe,"reason":reason,"remediation":if retry_safe{"Retry fs_apply_patch with the same operation_id and identical patch."}else if terminal=="patched_recovered"{"Treat the operation as complete; do not retry it."}else{"Inspect affected files before using a new operation_id."}}));
    }
    write_patch_outcome(state, operation, &value)?;
    Ok(value)
}

fn patch_state_set_matches(state: &State, value: Option<&Value>) -> bool {
    let Some(entries) = value.and_then(Value::as_array) else {
        return false;
    };
    if entries.is_empty() {
        return false;
    }
    entries.iter().all(|entry| {
        let Some(path) = entry.get("path").and_then(Value::as_str) else {
            return false;
        };
        let Ok((resolved, _)) = resolve_allowed(state, Some(path), "fs_patch_outcome_show") else {
            return false;
        };
        let expected_exists = entry
            .get("exists")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        if resolved.exists() != expected_exists {
            return false;
        }
        if !expected_exists {
            return true;
        }
        let Some(expected) = entry.get("sha256").and_then(Value::as_str) else {
            return false;
        };
        fs::read(&resolved)
            .ok()
            .is_some_and(|bytes| sha256_bytes(&bytes) == expected)
    })
}

fn process_is_alive(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }
    #[cfg(windows)]
    {
        Command::new("tasklist")
            .args(["/FI", &format!("PID eq {pid}"), "/FO", "CSV", "/NH"])
            .output()
            .ok()
            .is_some_and(|output| {
                String::from_utf8_lossy(&output.stdout).contains(&format!("\"{pid}\""))
            })
    }
    #[cfg(not(windows))]
    {
        Path::new(&format!("/proc/{pid}")).exists()
    }
}

fn list_tools(mode: &str) -> Vec<Value> {
    let mut names = vec![
        (
            "fs_guidance",
            "Show model-facing operating guidance for local-filesystem MCP workflows.",
        ),
        (
            "fs_read_file",
            "Read a text file under an allowed root with line offset and limit.",
        ),
        (
            "fs_read_file_range",
            "Read a logical text-file line range under an allowed root. Lines are 1-based and inclusive; ranges over 1,000 lines return a bounded page with continuation.arguments for the same MCP tool.",
        ),
        (
            "fs_stat",
            "Return file or directory metadata under an allowed root.",
        ),
        (
            "fs_glob_search",
            "List files under an allowed root using ripgrep file globbing.",
        ),
        (
            "fs_grep_search",
            "Search file contents under an allowed root using ripgrep.",
        ),
        (
            "fs_repository_inventory",
            "Return a bounded candidate-source inventory under an allowed root.",
        ),
        (
            "fs_file_metrics",
            "Return bounded metadata-only file metrics under an allowed root.",
        ),
        ("fs_doctor", "Inspect local-filesystem MCP policy posture."),
        (
            "fs_patch_outcome_show",
            "Read and durably reconcile the outcome for an fs_apply_patch operation_id.",
        ),
    ];
    if mode == "write" {
        names.push(("fs_write_file", "Write a text file under an allowed root and append an audit record. Refuses executable scripts under .ai/tmp or .ai/temp."));
        names.push((
            "fs_str_replace_file",
            "Replace exactly one string occurrence in a text file under an allowed root.",
        ));
        names.push((
            "fs_replace_range",
            "Replace an inclusive line range in a text file under an allowed root.",
        ));
        names.push((
            "fs_apply_patch",
            "Apply a unified diff or Codex-style patch atomically under allowed roots, with durable replay and recovery by operation_id.",
        ));
        names.push((
            "fs_move_path",
            "Move a file or directory under allowed roots.",
        ));
        names.push((
            "fs_create_directory",
            "Create a directory under an allowed root.",
        ));
        names.push((
            "fs_rename_directory",
            "Rename a directory under allowed roots.",
        ));
        names.push((
            "fs_delete_directory",
            "Delete a directory under an allowed root with explicit recursive consent.",
        ));
    }
    names.iter().map(|(name, description)| {
        let mut properties = Map::new();
        match *name {
            "fs_guidance" => { properties.insert("workflow".into(), json!({"type":"string"})); properties.insert("tool".into(), json!({"type":"string"})); }
            "fs_read_file" => { properties.insert("path".into(), json!({"type":"string"})); properties.insert("offset".into(), json!({"type":"integer","minimum":1,"maximum":10_000_000,"default":1,"description":"One-based first line to return."})); properties.insert("limit".into(), json!({"type":"integer","minimum":1,"maximum":1_000,"default":400,"description":"Maximum lines returned; paginate requests over 1,000 lines."})); properties.insert("timeout_ms".into(), json!({"type":"integer","minimum":1,"maximum":60_000,"default":READ_TIMEOUT_MS})); }
            "fs_read_file_range" => { properties.insert("path".into(), json!({"type":"string"})); properties.insert("start_line".into(), json!({"type":"integer","minimum":1,"maximum":10_000_000,"description":"Inclusive logical start line."})); properties.insert("end_line".into(), json!({"type":"integer","minimum":1,"maximum":10_000_000,"description":"Inclusive logical end line. Requests spanning over 1,000 lines return a bounded page; follow continuation.arguments until has_more is false."})); properties.insert("timeout_ms".into(), json!({"type":"integer","minimum":1,"maximum":60_000,"default":READ_TIMEOUT_MS})); }
            "fs_stat" => { properties.insert("path".into(), json!({"type":"string"})); properties.insert("timeout_ms".into(),json!({"type":"integer","minimum":1,"maximum":300_000,"default":60_000})); }
            "fs_glob_search" => { properties.insert("pattern".into(), json!({"type":"string"})); properties.insert("directory".into(), json!({"type":"string","default":"."})); properties.insert("ignore".into(), json!({"type":"array","items":{"type":"string"}})); properties.insert("offset".into(), json!({"type":"integer","minimum":0,"maximum":10_000_000,"default":0})); properties.insert("limit".into(), json!({"type":"integer","minimum":1,"maximum":500,"default":100})); properties.insert("timeout_ms".into(), json!({"type":"integer","minimum":1,"maximum":300_000,"default":SEARCH_TIMEOUT_MS})); properties.insert("cache_policy".into(), json!({"type":"string","enum":["auto","snapshot","refresh","bypass"],"default":"auto"})); properties.insert("snapshot_id".into(), json!({"type":"string"})); }
            "fs_grep_search" => { properties.insert("pattern".into(), json!({"type":"string"})); properties.insert("path".into(), json!({"type":"string","default":"."})); properties.insert("output_mode".into(), json!({"type":"string","enum":["files_with_matches","count_matches","content"],"default":"content"})); properties.insert("ignore".into(), json!({"type":"array","items":{"type":"string"}})); properties.insert("offset".into(), json!({"type":"integer","minimum":0,"maximum":10_000_000,"default":0})); properties.insert("limit".into(), json!({"type":"integer","minimum":1,"maximum":500,"default":80})); properties.insert("timeout_ms".into(), json!({"type":"integer","minimum":1,"maximum":300_000,"default":SEARCH_TIMEOUT_MS})); properties.insert("cache_policy".into(), json!({"type":"string","enum":["auto","snapshot","refresh","bypass"],"default":"auto"})); properties.insert("snapshot_id".into(), json!({"type":"string"})); }
            "fs_repository_inventory" => { properties.insert("pattern".into(), json!({"type":"string","default":"**/*"})); properties.insert("directory".into(), json!({"type":"string","description":"Canonical inventory scope; mutually exclusive with root."})); properties.insert("root".into(), json!({"type":"string","description":"Compatibility alias for directory; mutually exclusive with directory."})); properties.insert("ignore".into(), json!({"type":"array","items":{"type":"string"}})); properties.insert("include_generated".into(), json!({"type":"boolean","default":false})); properties.insert("offset".into(), json!({"type":"integer","minimum":0,"maximum":10_000_000,"default":0})); properties.insert("limit".into(), json!({"type":"integer","minimum":1,"maximum":500,"default":100})); properties.insert("timeout_ms".into(), json!({"type":"integer","minimum":1,"maximum":300_000,"default":SEARCH_TIMEOUT_MS})); properties.insert("cache_policy".into(), json!({"type":"string","enum":["auto","snapshot","refresh","bypass"],"default":"auto"})); properties.insert("snapshot_id".into(), json!({"type":"string"})); }
            "fs_file_metrics" => { properties.insert("pattern".into(), json!({"type":"string","default":"**/*"})); properties.insert("directory".into(), json!({"type":"string","default":"."})); properties.insert("root".into(), json!({"type":"string"})); properties.insert("ignore".into(), json!({"type":"array","items":{"type":"string"}})); properties.insert("exclude".into(), json!({"type":"array","items":{"type":"string"}})); properties.insert("offset".into(), json!({"type":"integer","minimum":0,"maximum":10_000_000,"default":0})); properties.insert("limit".into(), json!({"type":"integer","minimum":1,"maximum":100,"default":100})); properties.insert("max_bytes_per_file".into(), json!({"type":"integer","minimum":1,"maximum":1_073_741_824,"default":8_388_608})); properties.insert("max_total_scan_bytes".into(), json!({"type":"integer","minimum":1,"maximum":1_073_741_824,"default":268_435_456})); properties.insert("timeout_ms".into(), json!({"type":"integer","minimum":1,"maximum":300_000,"default":SEARCH_TIMEOUT_MS})); properties.insert("cache_policy".into(), json!({"type":"string","enum":["auto","snapshot","refresh","bypass"],"default":"auto"})); properties.insert("snapshot_id".into(), json!({"type":"string"})); }
            "fs_patch_outcome_show" => { properties.insert("operation_id".into(), json!({"type":"string"})); }
            "fs_write_file" => {
                properties.insert("payload_ref".into(), json!({"type":"string","maxLength":96}));
                properties.insert("payload_path".into(), json!({"type":"string","maxLength":32_768}));
                properties.insert("path".into(), json!({"type":"string"}));
                properties.insert("content".into(), json!({"type":"string"}));
                properties.insert("overwrite".into(), json!({"type":"boolean","default":true}));
                properties.insert("create_only".into(), json!({"type":"boolean","default":false}));
                properties.insert("create_parent_directories".into(), json!({"type":"boolean","default":true}));
                properties.insert("timeout_ms".into(), json!({"type":"integer","default":WRITE_TIMEOUT_MS}));
                properties.insert("expected_sha256".into(), json!({"type":"string"}));
            }
            "fs_str_replace_file" => {
                properties.insert("path".into(), json!({"type":"string"}));
                properties.insert("old".into(), json!({"type":"string"}));
                properties.insert("new".into(), json!({"type":"string"}));
                properties.insert("expected_sha256".into(), json!({"type":"string"}));
            }
            "fs_replace_range" => {
                properties.insert("path".into(), json!({"type":"string"}));
                properties.insert("start_line".into(), json!({"type":"integer"}));
                properties.insert("end_line".into(), json!({"type":"integer"}));
                properties.insert("replacement".into(), json!({"type":"string"}));
                properties.insert("expected_sha256".into(), json!({"type":"string"}));
            }
            "fs_apply_patch" => {
                properties.insert("patch".into(), json!({"type":"string","maxLength":8_388_608}));
                properties.insert("operation_id".into(), json!({"type":"string","pattern":"^[A-Za-z0-9._-]{1,160}$","maxLength":160}));
                properties.insert("dry_run".into(), json!({"type":"boolean","default":false}));
                properties.insert("timeout_ms".into(), json!({"type":"integer","minimum":1,"maximum":300_000,"default":WRITE_TIMEOUT_MS}));
                properties.insert("expected_sha256".into(), json!({"type":"object","maxProperties":256,"additionalProperties":{"type":"string","pattern":"^[0-9a-fA-F]{64}$","maxLength":64}}));
            }
            "fs_move_path" => {
                properties.insert("from".into(), json!({"type":"string"}));
                properties.insert("to".into(), json!({"type":"string"}));
                properties.insert("overwrite".into(), json!({"type":"boolean","default":false}));
                properties.insert("expected_from_size".into(), json!({"type":"integer"}));
                properties.insert("expected_from_mtime".into(), json!({"type":"string"}));
                properties.insert("expected_from_sha256".into(), json!({"type":"string"}));
                properties.insert("expected_from_tree_sha256".into(), json!({"type":"string"}));
                properties.insert("expected_from_entry_count".into(), json!({"type":"integer"}));
                properties.insert("expected_to_size".into(), json!({"type":"integer"}));
                properties.insert("expected_to_mtime".into(), json!({"type":"string"}));
                properties.insert("expected_to_sha256".into(), json!({"type":"string"}));
                properties.insert("expected_to_tree_sha256".into(), json!({"type":"string"}));
                properties.insert("expected_to_entry_count".into(), json!({"type":"integer"}));
                properties.insert("expected_from".into(), expected_metadata_schema());
                properties.insert("expected_to".into(), expected_metadata_schema());
            }
            "fs_create_directory" => {
                properties.insert("path".into(), json!({"type":"string"}));
                properties.insert("recursive".into(), json!({"type":"boolean","default":false}));
            }
            "fs_rename_directory" => {
                properties.insert("from".into(), json!({"type":"string"}));
                properties.insert("to".into(), json!({"type":"string"}));
                properties.insert("expected_from_mtime".into(), json!({"type":"string"}));
                properties.insert("expected_from_size".into(), json!({"type":"integer"}));
                properties.insert("expected_from_tree_sha256".into(), json!({"type":"string"}));
                properties.insert("expected_from_entry_count".into(), json!({"type":"integer"}));
                properties.insert("expected_to_mtime".into(), json!({"type":"string"}));
                properties.insert("expected_to_size".into(), json!({"type":"integer"}));
                properties.insert("expected_to_tree_sha256".into(), json!({"type":"string"}));
                properties.insert("expected_to_entry_count".into(), json!({"type":"integer"}));
                properties.insert("expected_from".into(), expected_metadata_schema());
                properties.insert("expected_to".into(), expected_metadata_schema());
            }
            "fs_delete_directory" => {
                properties.insert("path".into(), json!({"type":"string"}));
                properties.insert("recursive".into(), json!({"type":"boolean","default":false}));
                properties.insert("expected_size".into(), json!({"type":"integer"}));
                properties.insert("expected_mtime".into(), json!({"type":"string"}));
                properties.insert("expected_tree_sha256".into(), json!({"type":"string"}));
                properties.insert("expected_entry_count".into(), json!({"type":"integer"}));
                properties.insert("expected".into(), expected_metadata_schema());
            }
            _ => {}
        }
        let required: Vec<&str> = match *name {
            "fs_read_file" => vec!["path"],
            "fs_read_file_range" => vec!["path", "start_line", "end_line"],
            "fs_stat" => vec!["path"],
            "fs_grep_search" => vec!["pattern"],
            "fs_glob_search" => vec!["pattern"],
            "fs_patch_outcome_show" => vec!["operation_id"],
            "fs_str_replace_file" => vec!["path", "old", "new"],
            "fs_replace_range" => vec!["path", "start_line", "end_line", "replacement"],
            "fs_apply_patch" => vec!["patch"],
            "fs_move_path" => vec!["from", "to"],
            "fs_create_directory" => vec!["path"],
            "fs_rename_directory" => vec!["from", "to"],
            "fs_delete_directory" => vec!["path"],
            _ => Vec::new()
        };
        let write_tool = tool_has_write_effect(name);
        let destructive = matches!(*name,"fs_str_replace_file"|"fs_replace_range"|"fs_apply_patch"|"fs_move_path"|"fs_rename_directory"|"fs_delete_directory");
        let idempotent = !matches!(*name,"fs_str_replace_file"|"fs_replace_range"|"fs_move_path"|"fs_rename_directory"|"fs_delete_directory");
        bound_tool_properties(&mut properties);
        json!({"name": name, "canonical_name": name, "description": description, "inputSchema": {"title":format!("{name} arguments"),"type":"object","properties": properties,"required": required,"additionalProperties":false}, "annotations": {"title":name,"readOnlyHint":!write_tool,"destructiveHint":destructive,"idempotentHint":idempotent,"openWorldHint":false}, "outputSchema":{"title":format!("{name} result"),"type":"object","maxProperties":256,"additionalProperties":true}})
    }).collect()
}
