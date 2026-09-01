
struct TextWindow {
    selected: Vec<String>,
    next_offset: Option<i64>,
    total_lines: usize,
    complete: bool,
    sha256: String,
}

fn stream_text_window(
    path: &Path,
    root: &Path,
    offset: usize,
    limit: usize,
    timeout_ms: u64,
    operation: &str,
) -> Result<TextWindow, FsError> {
    let mut file = fs::File::open(path).map_err(|error| {
        FsError::new(
            format!("{operation}_failed"),
            format!("{operation}_failed: {error}"),
            path_details(path, root),
        )
    })?;
    let started = std::time::Instant::now();
    let mut digest = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    let mut pending = Vec::new();
    let mut selected = Vec::new();
    let mut retained = 0_usize;
    let mut line_number = 0_usize;
    let mut bounded = false;
    let mut next_offset = None;
    loop {
        if started.elapsed().as_millis() as u64 > timeout_ms {
            return Err(FsError::new(
                format!("{operation}_timed_out"),
                format!("{operation}_timed_out"),
                json!({"timeout_ms":timeout_ms,"path":path,"root":root,"offset":offset,"limit":limit}),
            ));
        }
        let count = file.read(&mut buffer).map_err(|error| {
            FsError::new(
                format!("{operation}_failed"),
                format!("{operation}_failed: {error}"),
                path_details(path, root),
            )
        })?;
        if count == 0 {
            break;
        }
        let chunk = &buffer[..count];
        if chunk.contains(&0) {
            return Err(FsError::new(
                "binary_file_not_supported",
                format!("binary_file_not_supported: {}", path.display()),
                path_details(path, root),
            ));
        }
        digest.update(chunk);
        if bounded {
            continue;
        }
        for byte in chunk {
            if selected.len() >= limit
                && line_number >= offset.saturating_add(limit).saturating_sub(1)
            {
                next_offset = Some((line_number + 1) as i64);
                bounded = true;
                break;
            }
            if *byte == b'\n' {
                line_number += 1;
                let line = if pending.last() == Some(&b'\r') {
                    &pending[..pending.len() - 1]
                } else {
                    pending.as_slice()
                };
                if line_number >= offset && selected.len() < limit {
                    retained = retained.saturating_add(line.len());
                    if retained > MAX_READ_WINDOW_BYTES {
                        return Err(FsError::new(
                            "fs_read_window_too_large",
                            "fs_read_window_too_large",
                            json!({"path":path,"max_window_bytes":MAX_READ_WINDOW_BYTES,"line":line_number}),
                        ));
                    }
                    selected.push(String::from_utf8(line.to_vec()).map_err(|_| {
                        FsError::new(
                            "text_file_not_utf8",
                            "text_file_not_utf8",
                            path_details(path, root),
                        )
                    })?);
                } else if line_number >= offset.saturating_add(limit) {
                    next_offset = Some(line_number as i64);
                    bounded = true;
                }
                pending.clear();
                if bounded {
                    break;
                }
            } else {
                pending.push(*byte);
                if pending.len() > MAX_READ_LINE_BYTES {
                    return Err(FsError::new(
                        "fs_read_line_too_large",
                        "fs_read_line_too_large",
                        json!({"path":path,"max_line_bytes":MAX_READ_LINE_BYTES,"line":line_number+1}),
                    ));
                }
            }
        }
    }
    if !bounded && !pending.is_empty() {
        line_number += 1;
        let line = if pending.last() == Some(&b'\r') {
            &pending[..pending.len() - 1]
        } else {
            pending.as_slice()
        };
        if line_number >= offset && selected.len() < limit {
            retained = retained.saturating_add(line.len());
            if retained > MAX_READ_WINDOW_BYTES {
                return Err(FsError::new(
                    "fs_read_window_too_large",
                    "fs_read_window_too_large",
                    json!({"path":path,"max_window_bytes":MAX_READ_WINDOW_BYTES,"line":line_number}),
                ));
            }
            selected.push(String::from_utf8(line.to_vec()).map_err(|_| {
                FsError::new(
                    "text_file_not_utf8",
                    "text_file_not_utf8",
                    path_details(path, root),
                )
            })?);
        } else if line_number >= offset.saturating_add(limit) {
            next_offset = Some(line_number as i64);
            bounded = true;
        }
    }
    Ok(TextWindow {
        selected,
        next_offset,
        total_lines: line_number,
        complete: !bounded,
        sha256: hex::encode(digest.finalize()),
    })
}

fn write_file(state: &State, args: &Value) -> Result<Value, FsError> {
    let (path, root) = resolve_allowed(
        state,
        args.get("path").and_then(Value::as_str),
        "fs_write_file",
    )?;
    assert_mutation_target_allowed(&path, &root, "fs_write_file")?;
    let content = args
        .get("content")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let overwrite = args
        .get("overwrite")
        .and_then(Value::as_bool)
        .unwrap_or(true);
    let create_only = args
        .get("create_only")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let create_parent_directories = args
        .get("create_parent_directories")
        .and_then(Value::as_bool)
        .unwrap_or(true);
    let timeout_ms = integer(args, "timeout_ms")
        .unwrap_or(WRITE_TIMEOUT_MS as i64)
        .clamp(1, 300_000) as u64;
    let started = std::time::Instant::now();

    let before_sha256 = match fs::metadata(&path) {
        Ok(metadata) if metadata.is_file() => Some(sha256_file_with_timeout(
            &path,
            timeout_ms,
            "fs_write_file",
        )?),
        Ok(metadata) if metadata.is_dir() => {
            return Err(FsError::new(
                "fs_write_file_destination_is_directory",
                "fs_write_file_destination_is_directory",
                path_details(&path, &root),
            ));
        }
        Ok(_) => {
            return Err(FsError::new(
                "fs_write_file_destination_not_regular_file",
                "fs_write_file_destination_not_regular_file",
                path_details(&path, &root),
            ));
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => None,
        Err(error) => {
            return Err(FsError::new(
                "fs_write_file_read_failed",
                format!("fs_write_file_read_failed: {error}"),
                path_details(&path, &root),
            ));
        }
    };
    if create_only && before_sha256.is_some() {
        return Err(FsError::new(
            "write_file_destination_exists",
            "write_file_destination_exists",
            path_details(&path, &root),
        ));
    }
    if !overwrite && before_sha256.is_some() {
        return Err(FsError::new(
            "write_file_overwrite_refused",
            "write_file_overwrite_refused",
            path_details(&path, &root),
        ));
    }
    if let Some(expected) = args
        .get("expected_sha256")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
    {
        if before_sha256.as_deref() != Some(expected) {
            return Err(FsError::new(
                "fs_write_file_expected_sha256_mismatch",
                "fs_write_file_expected_sha256_mismatch",
                json!({"operation": "fs_write_file", "expected_sha256": expected, "actual_sha256": before_sha256, "path": path, "root": root, "relative_path": relative_path(&root, &path), "concurrency_diagnosis": {"reason": "file_content_changed_since_observation_or_guard_is_not_full_file_hash", "expected_hash_scope": "full_file", "actual_hash_scope": "full_file", "actual_hash_source": "live_file_bytes", "cache_used": false, "attribution": "external_or_unobserved_writer_unless_a_matching_filesystem_audit_record_exists"}, "remediation": "Re-read the full-file content_sha256, reconcile the concurrent change, and retry with that live hash."}),
            ));
        }
    }

    let parent = path.parent().unwrap_or(root.as_path());
    if !parent.exists() {
        if !create_parent_directories {
            return Err(FsError::new(
                "write_file_parent_not_found",
                "write_file_parent_not_found",
                json!({"path": path, "root": root, "relative_path": relative_path(&root, &path), "parent": parent}),
            ));
        }
        fs::create_dir_all(parent).map_err(|error| FsError::new("fs_write_file_parent_failed", format!("fs_write_file_parent_failed: {error}"), json!({"path": path, "root": root, "relative_path": relative_path(&root, &path), "parent": parent})))?;
    }
    if started.elapsed().as_millis() as u64 > timeout_ms {
        return Err(FsError::new(
            "fs_write_file_timed_out",
            "filesystem write timed out",
            json!({"timeout_ms": timeout_ms, "path": path, "root": root, "relative_path": relative_path(&root, &path)}),
        ));
    }
    fs::write(&path, content.as_bytes()).map_err(|error| {
        FsError::new(
            "fs_write_file_failed",
            format!("fs_write_file_failed: {error}"),
            path_details(&path, &root),
        )
    })?;
    let after_sha256 = sha256_bytes(content.as_bytes());
    append_audit(
        state,
        "fs_write_file",
        &path,
        &root,
        json!({
            "size": content.len(),
            "create_parent_directories": create_parent_directories,
            "before_sha256": before_sha256,
            "after_sha256": after_sha256,
        }),
    )?;
    Ok(json!({
        "schema": "local.filesystem.write_file.v1",
        "status": "written",
        "path": path,
        "root": root,
        "relative_path": relative_path(&root, &path),
        "size": content.len(),
        "create_parent_directories": create_parent_directories,
        "before_sha256": before_sha256,
        "after_sha256": after_sha256,
        "sha256": after_sha256,
        "content_sha256": after_sha256,
        "timeout_ms": timeout_ms,
    }))
}
