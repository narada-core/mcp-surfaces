
fn backup_sibling(path: &Path) -> PathBuf {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let name = path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("target");
    let stamp = OffsetDateTime::now_utc().unix_timestamp_nanos();
    let mut candidate = parent.join(format!(".{name}.overwrite-backup-{stamp}"));
    let mut index = 0_u32;
    while candidate.exists() {
        index += 1;
        candidate = parent.join(format!(".{name}.overwrite-backup-{stamp}-{index}"));
    }
    candidate
}

fn same_path(left: &Path, right: &Path) -> bool {
    normalize_path(left) == normalize_path(right)
}

fn assert_mutation_target_allowed(
    path: &Path,
    root: &Path,
    operation: &str,
) -> Result<(), FsError> {
    let normalized = normalize_path(path);
    let in_transient_directory = normalized.contains("/.ai/tmp/")
        || normalized.contains("/.ai/temp/")
        || normalized.starts_with(".ai/tmp/")
        || normalized.starts_with(".ai/temp/");
    let extension = path
        .extension()
        .and_then(|value| value.to_str())
        .map(|value| format!(".{}", value.to_ascii_lowercase()));
    if in_transient_directory
        && extension
            .as_deref()
            .is_some_and(|value| TRANSIENT_EXECUTABLE_EXTENSIONS.contains(&value))
    {
        return Err(FsError::new(
            "transient_executable_write_disallowed",
            "transient_executable_write_disallowed",
            json!({
                "operation": operation,
                "path": path,
                "root": root,
                "relative_path": relative_path(root, path),
                "refusal_reason": format!("transient_executable_write_disallowed:{}", path.display()),
                "remediation": "Do not create or edit executable wrappers/scripts under .ai/tmp or .ai/temp. Use structured_command_start or the owning MCP surface directly and preserve its execution_ref as evidence.",
            }),
        ));
    }
    Ok(())
}

fn assert_not_authority_root(path: &Path, root: &Path, operation: &str) -> Result<(), FsError> {
    if same_path(path, root) {
        return Err(FsError::new(
            "filesystem_authority_root_mutation_refused",
            "filesystem_authority_root_mutation_refused",
            json!({"operation":operation,"path":path,"root":root,"remediation":"Choose a descendant path; an allowed authority root cannot itself be moved, overwritten, or deleted."}),
        ));
    }
    Ok(())
}

fn append_audit(
    state: &State,
    operation: &str,
    path: &Path,
    root: &Path,
    detail: Value,
) -> Result<(), FsError> {
    let Some(directory) = state.audit_log_dir.as_ref() else {
        return Ok(());
    };
    fs::create_dir_all(directory).map_err(|error| {
        FsError::new(
            "fs_write_file_audit_failed",
            format!("fs_write_file_audit_failed: {error}"),
            json!({"directory": directory}),
        )
    })?;
    let audit_path = directory.join("filesystem-mcp-audit.jsonl");
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&audit_path)
        .map_err(|error| {
            FsError::new(
                "fs_write_file_audit_failed",
                format!("fs_write_file_audit_failed: {error}"),
                json!({"path": audit_path}),
            )
        })?;
    let record = json!({
        "schema": "local.filesystem.audit.v1",
        "at": OffsetDateTime::now_utc().format(&Rfc3339).unwrap_or_else(|_| "unknown".to_string()),
        "operation": operation,
        "path": path,
        "root": root,
        "relative_path": relative_path(root, path),
        "detail": detail,
    });
    writeln!(
        file,
        "{}",
        serde_json::to_string(&record).unwrap_or_else(|_| "{}".to_string())
    )
    .map_err(|error| {
        FsError::new(
            "fs_write_file_audit_failed",
            format!("fs_write_file_audit_failed: {error}"),
            json!({"path": audit_path}),
        )
    })?;
    Ok(())
}

fn stat_tool(state: &State, args: &Value) -> Result<Value, FsError> {
    let (path, root) = resolve_allowed(state, args.get("path").and_then(Value::as_str), "fs_stat")?;
    let metadata = fs::metadata(&path).map_err(|error| {
        FsError::new(
            "fs_stat_failed",
            format!("fs_stat_failed: {error}"),
            path_details(&path, &root),
        )
    })?;
    let kind = if metadata.is_dir() {
        "directory"
    } else if metadata.is_file() {
        "file"
    } else {
        "other"
    };
    let mut value = Map::new();
    value.insert("schema".into(), json!("local.filesystem.stat.v1"));
    value.insert("path".into(), json!(path));
    value.insert("root".into(), json!(root));
    value.insert("relative_path".into(), json!(relative_path(&root, &path)));
    value.insert("type".into(), json!(kind));
    value.insert("size".into(), json!(metadata.len()));
    value.insert("mtime".into(), json!(mtime_iso(&metadata)));
    if metadata.is_file() {
        let timeout = args
            .get("timeout_ms")
            .and_then(Value::as_u64)
            .unwrap_or(60_000);
        value.insert(
            "sha256".into(),
            json!(sha256_file_with_timeout(&path, timeout, "fs_stat")?),
        );
    }
    if metadata.is_dir() {
        let (entry_count, tree_entry_count, tree_sha256, truncated) =
            directory_fingerprint(&path, &path);
        value.insert("entry_count".into(), json!(entry_count));
        value.insert("tree_entry_count".into(), json!(tree_entry_count));
        value.insert("tree_truncated".into(), json!(truncated));
        value.insert("tree_sha256".into(), json!(tree_sha256));
    }
    Ok(Value::Object(value))
}

fn sha256_file_with_timeout(
    path: &Path,
    timeout_ms: u64,
    operation: &str,
) -> Result<String, FsError> {
    let mut file = fs::File::open(path).map_err(|error| {
        FsError::new(
            format!("{operation}_read_failed"),
            format!("{operation}_read_failed: {error}"),
            json!({"path":path}),
        )
    })?;
    let started = std::time::Instant::now();
    let mut digest = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        if started.elapsed().as_millis() as u64 > timeout_ms {
            return Err(FsError::new(
                format!("{operation}_timed_out"),
                format!("{operation}_timed_out"),
                json!({"path":path,"timeout_ms":timeout_ms}),
            ));
        }
        let count = file.read(&mut buffer).map_err(|error| {
            FsError::new(
                format!("{operation}_read_failed"),
                format!("{operation}_read_failed: {error}"),
                json!({"path":path}),
            )
        })?;
        if count == 0 {
            break;
        }
        digest.update(&buffer[..count]);
    }
    Ok(hex::encode(digest.finalize()))
}
