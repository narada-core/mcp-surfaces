
fn call_tool(state: &mut State, params: &Value) -> Result<Value, FsError> {
    let name = params.get("name").and_then(Value::as_str).ok_or_else(|| {
        FsError::new(
            "tools_call_requires_name",
            "tools_call_requires_name",
            json!({}),
        )
    })?;
    let raw_args = params.get("arguments").unwrap_or(&Value::Null);
    validate_tool_arguments(&state.mode, name, raw_args)?;
    let (resolved_args, payload_source) = if name == "fs_write_file" {
        resolve_write_payload(state, raw_args)?
    } else {
        (raw_args.clone(), None)
    };
    let args = &resolved_args;
    validate_tool_arguments(&state.mode, name, args)?;
    if is_write_tool(name) && state.mode != "write" {
        return Err(FsError::new(
            format!("tool_not_available_in_{}_mode", state.mode),
            format!("tool_not_available_in_{}_mode: {name}", state.mode),
            json!({"tool_name": name, "mode": state.mode}),
        ));
    }
    let mut value = match name {
        "fs_guidance" => guidance(state, args),
        "fs_read_file" => read_file(state, args, false),
        "fs_read_file_range" => read_file(state, args, true),
        "fs_stat" => stat_tool(state, args),
        "fs_glob_search" => search_tool(state, args, false),
        "fs_repository_inventory" => repository_inventory(state, args),
        "fs_file_metrics" => file_metrics(state, args),
        "fs_search" => fs_search_tool(state, args),
        "fs_search_results_read" => fs_search_results_read(state, args),
        "fs_grep_search" => search_tool(state, args, true),
        "fs_doctor" => Ok(doctor(state)),
        "fs_patch_outcome_show" => patch_outcome(state, args),
        "fs_write_file" => write_file(state, args),
        "fs_str_replace_file" => str_replace_file(state, args),
        "fs_replace_range" => replace_range(state, args),
        "fs_apply_patch" => apply_patch_tool(state, args),
        "fs_move_path" => move_path(state, args, false),
        "fs_create_directory" => create_directory(state, args),
        "fs_rename_directory" => move_path(state, args, true),
        "fs_delete_directory" => delete_directory(state, args),
        _ => Err(FsError::new(
            format!("tool_not_available_in_{}_mode", state.mode),
            format!("tool_not_available_in_{}_mode: {name}", state.mode),
            json!({"tool_name": name, "mode": state.mode}),
        )),
    }?;
    if is_write_tool(name) {
        state.cache.clear();
        state.snapshots.clear();
        state.snapshot_order.clear();
    }
    if let (Some(source), Some(object)) = (payload_source, value.as_object_mut()) {
        object.insert("payload_source".into(), source);
    }
    if matches!(name, "fs_search" | "fs_grep_search") {
        return Ok(bounded_search_tool_result(state, name, value, args));
    }
    Ok(tool_result(value))
}

fn resolve_write_payload(state: &State, args: &Value) -> Result<(Value, Option<Value>), FsError> {
    let object = args.as_object().ok_or_else(|| {
        FsError::new(
            "tool_arguments_must_be_object",
            "tool_arguments_must_be_object",
            json!({}),
        )
    })?;
    let path = object
        .get("payload_path")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty());
    let reference = object
        .get("payload_ref")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty());
    if path.is_some() && reference.is_some() {
        return Err(FsError::new(
            "payload_transport_must_choose_one_of_payload_path_or_payload_ref",
            "payload_transport_must_choose_one_of_payload_path_or_payload_ref",
            json!({}),
        ));
    }
    let Some((candidate, source_kind)) = (if let Some(value) = path {
        Some((state.output_root.join(value), "file"))
    } else if let Some(value) = reference {
        let Some(rest) = value.strip_prefix("mcp_payload:") else {
            return Err(FsError::new(
                "payload_ref_invalid",
                "payload_ref_invalid",
                json!({"payload_ref":value}),
            ));
        };
        let Some((id, revision)) = rest.split_once("@v") else {
            return Err(FsError::new(
                "payload_ref_invalid",
                "payload_ref_invalid",
                json!({"payload_ref":value}),
            ));
        };
        if id.len() < 3
            || id.len() > 64
            || !id
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
            || revision
                .parse::<u64>()
                .ok()
                .filter(|value| *value > 0)
                .is_none()
        {
            return Err(FsError::new(
                "payload_ref_invalid",
                "payload_ref_invalid",
                json!({"payload_ref":value}),
            ));
        }
        Some((
            state
                .output_root
                .join(".ai/tmp/mcp-payloads/workspace")
                .join(id)
                .join(format!("v{revision}.json")),
            "ref",
        ))
    } else {
        None
    }) else {
        return Ok((args.clone(), None));
    };
    let allowed = canonicalize_with_missing(&state.output_root.join(".ai/tmp/mcp-payloads"));
    let resolved = canonicalize_with_missing(&candidate);
    if !within(&allowed, &resolved) {
        return Err(FsError::new(
            "payload_path_outside_allowed_staging",
            "payload_path_outside_allowed_staging",
            json!({"path":candidate}),
        ));
    }
    let metadata = fs::metadata(&resolved).map_err(|_| {
        FsError::new(
            "payload_path_not_found",
            "payload_path_not_found",
            json!({"path":candidate}),
        )
    })?;
    if !metadata.is_file() {
        return Err(FsError::new(
            "payload_path_not_file",
            "payload_path_not_file",
            json!({"path":candidate}),
        ));
    }
    if metadata.len() > 5 * 1024 * 1024 {
        return Err(FsError::new(
            "payload_path_too_large",
            "payload_path_too_large",
            json!({"size":metadata.len(),"max_bytes":5*1024*1024}),
        ));
    }
    let bytes = fs::read(&resolved).map_err(|e| {
        FsError::new(
            "payload_path_read_failed",
            format!("payload_path_read_failed: {e}"),
            json!({"path":candidate}),
        )
    })?;
    let record: Value = serde_json::from_slice(&bytes).map_err(|e| {
        FsError::new(
            "payload_path_invalid_json",
            format!("payload_path_invalid_json: {e}"),
            json!({"path":candidate}),
        )
    })?;
    let payload = if source_kind == "ref" {
        record.get("payload").cloned().ok_or_else(|| {
            FsError::new(
                "payload_ref_payload_missing",
                "payload_ref_payload_missing",
                json!({"path":candidate}),
            )
        })?
    } else {
        record
    };
    if !payload.is_object() {
        return Err(FsError::new(
            "payload_path_json_must_be_object",
            "payload_path_json_must_be_object",
            json!({"path":candidate}),
        ));
    }
    let source = json!({"kind":source_kind,"path":relative_path(&state.output_root,&resolved),"byte_size":metadata.len(),"max_bytes":5*1024*1024,"sha256":sha256_bytes(&bytes),"transient_not_authority":true});
    Ok((payload, Some(source)))
}

fn is_write_tool(name: &str) -> bool {
    matches!(
        name,
        "fs_write_file"
            | "fs_str_replace_file"
            | "fs_replace_range"
            | "fs_apply_patch"
            | "fs_move_path"
            | "fs_create_directory"
            | "fs_rename_directory"
            | "fs_delete_directory"
    )
}
