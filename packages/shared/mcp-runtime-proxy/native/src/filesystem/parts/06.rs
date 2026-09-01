
fn str_replace_file(state: &State, args: &Value) -> Result<Value, FsError> {
    let (path, root) = resolve_allowed(
        state,
        args.get("path").and_then(Value::as_str),
        "fs_str_replace_file",
    )?;
    assert_mutation_target_allowed(&path, &root, "fs_str_replace_file")?;
    let old = args.get("old").and_then(Value::as_str).unwrap_or_default();
    let new = args.get("new").and_then(Value::as_str).unwrap_or_default();
    if old.is_empty() {
        return Err(FsError::new(
            "str_replace_requires_old",
            "str_replace_requires_old",
            path_details(&path, &root),
        ));
    }
    let before = read_bounded_mutation_text(&path, &root, "fs_str_replace_file")?;
    let before_sha256 = sha256_bytes(before.as_bytes());
    if let Some(expected) = args
        .get("expected_sha256")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
    {
        if expected != before_sha256 {
            return Err(FsError::new(
                "fs_str_replace_file_expected_sha256_mismatch",
                "fs_str_replace_file_expected_sha256_mismatch",
                json!({"operation": "fs_str_replace_file", "expected_sha256": expected, "actual_sha256": before_sha256, "path": path, "root": root, "concurrency_diagnosis": {"reason": "file_content_changed_since_observation_or_guard_is_not_full_file_hash", "expected_hash_scope": "full_file", "actual_hash_scope": "full_file", "actual_hash_source": "live_file_bytes", "cache_used": false, "attribution": "external_or_unobserved_writer_unless_a_matching_filesystem_audit_record_exists"}, "remediation": "Re-read the full-file content_sha256, reconcile the concurrent change, and retry with that live hash."}),
            ));
        }
    }
    let occurrences = before.match_indices(old).count();
    if occurrences == 0 {
        return Err(FsError::new(
            "str_replace_not_found",
            "str_replace_not_found",
            json!({"path": path, "root": root, "old_length": old.len(), "recommended_tool": "fs_replace_range"}),
        ));
    }
    if occurrences > 1 {
        return Err(FsError::new(
            "str_replace_ambiguous",
            format!("str_replace_ambiguous: {occurrences}"),
            json!({"path": path, "root": root, "occurrences": occurrences}),
        ));
    }
    let after = before.replacen(old, new, 1);
    fs::write(&path, after.as_bytes()).map_err(|error| {
        FsError::new(
            "fs_str_replace_file_failed",
            format!("fs_str_replace_file_failed: {error}"),
            path_details(&path, &root),
        )
    })?;
    let after_sha256 = sha256_bytes(after.as_bytes());
    append_audit(
        state,
        "fs_str_replace_file",
        &path,
        &root,
        json!({"old_length": old.len(), "new_length": new.len(), "before_sha256": before_sha256, "after_sha256": after_sha256}),
    )?;
    Ok(
        json!({"schema": "local.filesystem.str_replace_file.v1", "status": "replaced", "path": path, "root": root, "relative_path": relative_path(&root, &path), "occurrences": 1, "before_sha256": before_sha256, "after_sha256": after_sha256, "sha256": after_sha256, "content_sha256": after_sha256}),
    )
}

fn replace_range(state: &State, args: &Value) -> Result<Value, FsError> {
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
    let (path, root) = resolve_allowed(
        state,
        args.get("path").and_then(Value::as_str),
        "fs_replace_range",
    )?;
    assert_mutation_target_allowed(&path, &root, "fs_replace_range")?;
    let replacement = args
        .get("replacement")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let before = read_bounded_mutation_text(&path, &root, "fs_replace_range")?;
    let before_sha256 = sha256_bytes(before.as_bytes());
    if let Some(expected) = args
        .get("expected_sha256")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
    {
        if expected != before_sha256 {
            return Err(FsError::new(
                "fs_replace_range_expected_sha256_mismatch",
                "fs_replace_range_expected_sha256_mismatch",
                json!({"operation": "fs_replace_range", "expected_sha256": expected, "actual_sha256": before_sha256, "path": path, "root": root, "concurrency_diagnosis": {"reason": "file_content_changed_since_observation_or_guard_is_not_full_file_hash", "expected_hash_scope": "full_file", "actual_hash_scope": "full_file", "actual_hash_source": "live_file_bytes", "cache_used": false, "attribution": "external_or_unobserved_writer_unless_a_matching_filesystem_audit_record_exists"}, "remediation": "Re-read the full-file content_sha256, reconcile the concurrent change, and retry with that live hash."}),
            ));
        }
    }
    let newline = if before.contains("\r\n") {
        "\r\n"
    } else {
        "\n"
    };
    let has_trailing_newline = before.ends_with('\n');
    let body = before
        .strip_suffix('\n')
        .unwrap_or(&before)
        .strip_suffix('\r')
        .unwrap_or_else(|| before.strip_suffix('\n').unwrap_or(&before));
    let lines = if body.is_empty() {
        Vec::new()
    } else {
        body.split('\n')
            .map(|line| line.strip_suffix('\r').unwrap_or(line))
            .collect::<Vec<_>>()
    };
    if start as usize > lines.len() + 1 {
        return Err(FsError::new(
            "start_line_out_of_range",
            format!("start_line_out_of_range: {start}"),
            json!({"path": path, "root": root, "start_line": start, "total_lines": lines.len()}),
        ));
    }
    if end as usize > lines.len() {
        return Err(FsError::new(
            "end_line_out_of_range",
            format!("end_line_out_of_range: {end}"),
            json!({"path": path, "root": root, "end_line": end, "total_lines": lines.len()}),
        ));
    }
    let replacement_lines = if replacement.is_empty() {
        Vec::new()
    } else {
        replacement.split('\n').collect::<Vec<_>>()
    };
    let mut after_lines = Vec::new();
    after_lines.extend_from_slice(&lines[..(start as usize - 1)]);
    after_lines.extend_from_slice(&replacement_lines);
    after_lines.extend_from_slice(&lines[end as usize..]);
    let mut after = after_lines.join(newline);
    if has_trailing_newline {
        after.push_str(newline);
    }
    fs::write(&path, after.as_bytes()).map_err(|error| {
        FsError::new(
            "fs_replace_range_failed",
            format!("fs_replace_range_failed: {error}"),
            path_details(&path, &root),
        )
    })?;
    let after_sha256 = sha256_bytes(after.as_bytes());
    append_audit(
        state,
        "fs_replace_range",
        &path,
        &root,
        json!({"start_line": start, "end_line": end, "before_sha256": before_sha256, "after_sha256": after_sha256}),
    )?;
    Ok(
        json!({"schema": "local.filesystem.replace_range.v1", "status": "replaced_range", "path": path, "root": root, "relative_path": relative_path(&root, &path), "start_line": start, "end_line": end, "inserted_lines": replacement_lines.len(), "before_sha256": before_sha256, "after_sha256": after_sha256, "sha256": after_sha256, "content_sha256": after_sha256}),
    )
}

fn create_directory(state: &State, args: &Value) -> Result<Value, FsError> {
    let (path, root) = resolve_allowed(
        state,
        args.get("path").and_then(Value::as_str),
        "fs_create_directory",
    )?;
    assert_mutation_target_allowed(&path, &root, "fs_create_directory")?;
    let recursive = args
        .get("recursive")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if path.exists() {
        if !path.is_dir() {
            return Err(FsError::new(
                "create_directory_destination_not_directory",
                "create_directory_destination_not_directory",
                path_details(&path, &root),
            ));
        }
        append_audit(
            state,
            "fs_create_directory",
            &path,
            &root,
            json!({"recursive": recursive, "created": false}),
        )?;
        return Ok(
            json!({"schema": "local.filesystem.create_directory.v1", "status": "exists", "path": path, "root": root, "relative_path": relative_path(&root, &path), "recursive": recursive, "created": false}),
        );
    }
    let parent = path.parent().unwrap_or(root.as_path());
    if !recursive && !parent.exists() {
        return Err(FsError::new(
            "create_directory_parent_not_found",
            "create_directory_parent_not_found",
            json!({"operation": "fs_create_directory", "requested_path": path, "parent": path_details(parent, &root)}),
        ));
    }
    if recursive {
        fs::create_dir_all(&path)
    } else {
        fs::create_dir(&path)
    }
    .map_err(|error| {
        FsError::new(
            "create_directory_failed",
            format!("create_directory_failed: {error}"),
            path_details(&path, &root),
        )
    })?;
    append_audit(
        state,
        "fs_create_directory",
        &path,
        &root,
        json!({"recursive": recursive}),
    )?;
    Ok(
        json!({"schema": "local.filesystem.create_directory.v1", "status": "created", "path": path, "root": root, "relative_path": relative_path(&root, &path), "recursive": recursive, "created": true}),
    )
}
