
fn json_type(value: &Value) -> &'static str {
    if value.is_null() {
        "null"
    } else if value.is_object() {
        "object"
    } else if value.is_array() {
        "array"
    } else if value.is_string() {
        "string"
    } else if value.is_boolean() {
        "boolean"
    } else {
        "number"
    }
}

fn valid_operation_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 160
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"._-".contains(&byte))
}

fn valid_sha256(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn resolve_allowed(
    state: &State,
    input: Option<&str>,
    operation: &str,
) -> Result<(PathBuf, PathBuf), FsError> {
    let input = input
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| {
            FsError::new(
                "path_required",
                "path_required",
                json!({"operation": operation}),
            )
        })?;
    if input.contains('%') {
        return Err(FsError::new(
            "path_environment_expansion_not_supported",
            format!("path_environment_expansion_not_supported: {input}"),
            json!({"operation": operation, "requested_path": input, "remediation": "Expand environment variables before calling the filesystem surface, or pass an absolute path."}),
        ));
    }
    let base = state.allowed_roots.first().cloned().ok_or_else(|| {
        FsError::new(
            "filesystem_mcp_requires_at_least_one_allowed_root",
            "filesystem_mcp_requires_at_least_one_allowed_root",
            json!({}),
        )
    })?;
    let candidate = if Path::new(input).is_absolute() {
        absolute(PathBuf::from(input))
    } else {
        absolute(base.join(input))
    };
    let root = state
        .allowed_roots
        .iter()
        .find(|root| within(root, &candidate))
        .cloned();
    let Some(root) = root else {
        return Err(FsError::new(
            "path_outside_allowed_roots",
            format!("path_outside_allowed_roots: {input}"),
            json!({"operation": operation, "requested_path": input, "active_resolution_base": base, "resolution_rule": "first_allowed_root_for_relative_paths", "allowed_roots": state.allowed_roots}),
        ));
    };
    let check_path = canonicalize_with_missing(&candidate);
    let check_root = canonicalize_with_missing(&root);
    if !within(&check_root, &check_path) {
        return Err(FsError::new(
            "path_outside_allowed_roots",
            format!("path_outside_allowed_roots: {input}"),
            path_details(&candidate, &root),
        ));
    }
    Ok((candidate, root))
}

fn canonicalize_with_missing(path: &Path) -> PathBuf {
    let mut missing = Vec::new();
    let mut current = path.to_path_buf();
    while !current.exists() {
        let Some(name) = current.file_name().map(|value| value.to_os_string()) else {
            break;
        };
        missing.push(name);
        if !current.pop() {
            break;
        }
    }
    let mut canonical = fs::canonicalize(&current).unwrap_or(current);
    for component in missing.iter().rev() {
        canonical.push(component);
    }
    canonical
}

fn path_details(path: &Path, root: &Path) -> Value {
    json!({"path": path, "root": root, "relative_path": relative_path(root, path)})
}

fn relative_path(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .map(|value| value.to_string_lossy().replace('\\', "/"))
        .unwrap_or_else(|_| path.to_string_lossy().replace('\\', "/"))
}

fn within(root: &Path, path: &Path) -> bool {
    let root = normalize_path(root);
    let path = normalize_path(path);
    path == root || path.starts_with(&(root + "/"))
}

fn absolute(path: PathBuf) -> PathBuf {
    if path.is_absolute() {
        path
    } else {
        env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join(path)
    }
}

fn normalize_path(path: &Path) -> String {
    path.to_string_lossy()
        .replace('\\', "/")
        .trim_end_matches('/')
        .to_ascii_lowercase()
}

fn integer(value: &Value, key: &str) -> Option<i64> {
    value.get(key).and_then(Value::as_i64)
}

fn sha256_bytes(bytes: &[u8]) -> String {
    let mut digest = Sha256::new();
    digest.update(bytes);
    hex::encode(digest.finalize())
}

fn mtime_iso(metadata: &fs::Metadata) -> String {
    let duration = metadata
        .modified()
        .ok()
        .and_then(|value| value.duration_since(std::time::UNIX_EPOCH).ok());
    if let Some(value) = duration {
        if let Ok(date) = OffsetDateTime::from_unix_timestamp(value.as_secs() as i64) {
            return date
                .format(&Rfc3339)
                .unwrap_or_else(|_| "1970-01-01T00:00:00Z".to_string());
        }
    }
    "1970-01-01T00:00:00Z".to_string()
}

fn freshness(path: &Path) -> Value {
    match fs::metadata(path) {
        Ok(metadata) => {
            let kind = if metadata.is_dir() {
                "directory"
            } else if metadata.is_file() {
                "file"
            } else {
                "other"
            };
            let modified_ms = metadata
                .modified()
                .ok()
                .and_then(|value| value.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|value| value.as_millis())
                .unwrap_or(0);
            let mut value = Map::new();
            value.insert("path".into(), json!(path));
            value.insert("type".into(), json!(kind));
            value.insert("size".into(), json!(metadata.len()));
            value.insert("mtime".into(), json!(mtime_iso(&metadata)));
            value.insert("mtime_ms".into(), json!(modified_ms));
            if metadata.is_file() {
                if let Ok(hash) = sha256_file_with_timeout(path, 60_000, "filesystem_freshness") {
                    value.insert("sha256".into(), json!(hash));
                }
            } else if metadata.is_dir() {
                let (entry_count, tree_entry_count, tree_sha256, truncated) =
                    directory_fingerprint(path, path);
                value.insert("entry_count".into(), json!(entry_count));
                value.insert("tree_entry_count".into(), json!(tree_entry_count));
                value.insert("tree_truncated".into(), json!(truncated));
                value.insert("tree_sha256".into(), json!(tree_sha256));
            }
            Value::Object(value)
        }
        Err(_) => json!({"path": path, "type": "missing"}),
    }
}
