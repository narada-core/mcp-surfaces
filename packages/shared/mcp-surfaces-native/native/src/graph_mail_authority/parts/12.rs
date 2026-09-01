fn valid_base64(value: &str) -> bool {
    value.len() % 4 == 0
        && value
            .bytes()
            .enumerate()
            .all(|(index, byte)| match byte {
                b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'+' | b'/' => true,
                b'=' => index + 2 >= value.len(),
                _ => false,
            })
}

fn base64_decoded_size(value: &str) -> usize {
    value.len() / 4 * 3 - value.as_bytes().iter().rev().take_while(|byte| **byte == b'=').count()
}

fn decode_base64(value: &str) -> Result<Vec<u8>, &'static str> {
    if !valid_base64(value) {
        return Err("attachment_content_base64_invalid");
    }
    let mut output = Vec::with_capacity(base64_decoded_size(value));
    let bytes = value.as_bytes();
    for chunk in bytes.chunks(4) {
        let a = base64_value(chunk[0]).ok_or("attachment_content_base64_invalid")?;
        let b = base64_value(chunk[1]).ok_or("attachment_content_base64_invalid")?;
        let c = if chunk[2] == b'=' { 0 } else { base64_value(chunk[2]).ok_or("attachment_content_base64_invalid")? };
        let d = if chunk[3] == b'=' { 0 } else { base64_value(chunk[3]).ok_or("attachment_content_base64_invalid")? };
        output.push((a << 2) | (b >> 4));
        if chunk[2] != b'=' {
            output.push((b << 4) | (c >> 2));
        }
        if chunk[3] != b'=' {
            output.push((c << 6) | d);
        }
    }
    Ok(output)
}

fn base64_value(byte: u8) -> Option<u8> {
    match byte {
        b'A'..=b'Z' => Some(byte - b'A'),
        b'a'..=b'z' => Some(byte - b'a' + 26),
        b'0'..=b'9' => Some(byte - b'0' + 52),
        b'+' => Some(62),
        b'/' => Some(63),
        _ => None,
    }
}

fn infer_content_type(file_name: &str) -> String {
    let lower = file_name.to_ascii_lowercase();
    if lower.ends_with(".pdf") {
        "application/pdf".to_string()
    } else if lower.ends_with(".pptx") {
        "application/vnd.openxmlformats-officedocument.presentationml.presentation".to_string()
    } else if lower.ends_with(".docx") {
        "application/vnd.openxmlformats-officedocument.wordprocessingml.document".to_string()
    } else if lower.ends_with(".xlsx") {
        "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet".to_string()
    } else if lower.ends_with(".csv") {
        "text/csv".to_string()
    } else if lower.ends_with(".txt") {
        "text/plain".to_string()
    } else if lower.ends_with(".png") {
        "image/png".to_string()
    } else if lower.ends_with(".jpg") || lower.ends_with(".jpeg") {
        "image/jpeg".to_string()
    } else {
        "application/octet-stream".to_string()
    }
}

fn resolve_attachment_output_path(
    root: &Path,
    args: &Map<String, Value>,
    configured_roots: &[PathBuf],
) -> Result<PathBuf, Value> {
    let input = required_string(args, "file_path")?;
    let candidate = root.join(&input);
    let roots: Vec<PathBuf> = if configured_roots.is_empty() {
        vec![root.to_path_buf()]
    } else {
        configured_roots.to_vec()
    };
    if !roots.iter().any(|parent| path_inside(&candidate, parent)) {
        return Err(unavailable(
            "attachment_file_path_not_allowed",
            "destination is outside the configured attachment roots",
        ));
    }
    if same_path(&candidate, root) {
        return Err(unavailable(
            "attachment_file_path_not_file",
            "destination must be a file",
        ));
    }
    let canonical_roots: Vec<PathBuf> = roots
        .iter()
        .map(|path| {
            fs::canonicalize(path)
                .map_err(|_| unavailable("attachment_file_root_missing", &path.to_string_lossy()))
        })
        .collect::<Result<_, _>>()?;
    let existing = nearest_existing_path(&candidate)?;
    let canonical_existing = fs::canonicalize(&existing)
        .map_err(|error| unavailable("attachment_file_path_parent_missing", &error.to_string()))?;
    if !canonical_roots
        .iter()
        .any(|parent| path_inside(&canonical_existing, parent))
    {
        return Err(unavailable(
            "attachment_file_path_symlink_escape",
            "destination parent escapes the configured attachment roots",
        ));
    }
    if candidate.exists() {
        let canonical_candidate = fs::canonicalize(&candidate)
            .map_err(|error| unavailable("attachment_file_path_symlink_escape", &error.to_string()))?;
        if !canonical_roots
            .iter()
            .any(|parent| path_inside(&canonical_candidate, parent))
        {
            return Err(unavailable(
                "attachment_file_path_symlink_escape",
                "destination escapes the configured attachment roots",
            ));
        }
    }
    Ok(candidate)
}

fn resolve_attachment_input_path(
    root: &Path,
    args: &Map<String, Value>,
    configured_roots: &[PathBuf],
) -> Result<PathBuf, Value> {
    let input = required_string(args, "file_path")?;
    let candidate = root.join(&input);
    let roots: Vec<PathBuf> = if configured_roots.is_empty() {
        vec![root.to_path_buf()]
    } else {
        configured_roots.to_vec()
    };
    if !roots.iter().any(|parent| path_inside(&candidate, parent)) {
        return Err(unavailable(
            "attachment_file_path_not_allowed",
            "file is outside the configured attachment roots",
        ));
    }
    let canonical_candidate = fs::canonicalize(&candidate)
        .map_err(|error| unavailable("attachment_file_path_not_allowed", &error.to_string()))?;
    let canonical_roots: Vec<PathBuf> = roots
        .iter()
        .map(|path| {
            fs::canonicalize(path)
                .map_err(|_| unavailable("attachment_file_root_missing", &path.to_string_lossy()))
        })
        .collect::<Result<_, _>>()?;
    if !canonical_roots
        .iter()
        .any(|parent| path_inside(&canonical_candidate, parent))
    {
        return Err(unavailable(
            "attachment_file_path_symlink_escape",
            "file escapes the configured attachment roots",
        ));
    }
    if !fs::metadata(&canonical_candidate)
        .map_err(|error| unavailable("attachment_file_stat_failed", &error.to_string()))?
        .is_file()
    {
        return Err(unavailable(
            "attachment_file_path_not_file",
            "attachment path is not a file",
        ));
    }
    Ok(canonical_candidate)
}

fn upload_chunk_size(args: &Map<String, Value>) -> Result<u64, Value> {
    let size = args
        .get("chunk_size")
        .and_then(Value::as_u64)
        .unwrap_or(DEFAULT_ATTACHMENT_UPLOAD_CHUNK_SIZE);
    if !(ATTACHMENT_UPLOAD_CHUNK_GRANULARITY..=10 * 1024 * 1024).contains(&size) {
        return Err(unavailable(
            "attachment_upload_chunk_size_invalid",
            "chunk size must be between 320 KiB and 10 MiB",
        ));
    }
    if size % ATTACHMENT_UPLOAD_CHUNK_GRANULARITY != 0 {
        return Err(unavailable(
            "attachment_upload_chunk_size_must_be_multiple_of_320kib",
            "chunk size must be a multiple of 320 KiB",
        ));
    }
    Ok(size)
}

fn nearest_existing_path(candidate: &Path) -> Result<PathBuf, Value> {
    let mut current = candidate.to_path_buf();
    while !current.exists() {
        let Some(parent) = current.parent() else {
            return Err(unavailable(
                "attachment_file_path_parent_missing",
                "destination parent is missing",
            ));
        };
        if parent == current {
            return Err(unavailable(
                "attachment_file_path_parent_missing",
                "destination parent is missing",
            ));
        }
        current = parent.to_path_buf();
    }
    Ok(current)
}

fn same_path(left: &Path, right: &Path) -> bool {
    let left = left.to_string_lossy();
    let right = right.to_string_lossy();
    if cfg!(windows) {
        left.eq_ignore_ascii_case(&right)
    } else {
        left == right
    }
}

fn path_inside(child: &Path, parent: &Path) -> bool {
    let child = child.to_string_lossy();
    let parent = parent.to_string_lossy();
    if cfg!(windows) {
        let child = child.to_ascii_lowercase();
        let parent = parent.to_ascii_lowercase();
        child == parent || child.starts_with(&format!("{}\\", parent)) || child.starts_with(&format!("{}/", parent))
    } else {
        child == parent || child.starts_with(&format!("{}/", parent))
    }
}

fn display_path(path: &Path) -> String {
    let value = path.to_string_lossy().to_string();
    if cfg!(windows) {
        value.replace('/', "\\")
    } else {
        value
    }
}

fn refused(root: &Path, event_kind: &str, reason: &str, extra: Value) -> Result<Value, Value> {
    record_audit(root, merge(json!({"event_kind":event_kind,"reason":reason}), extra))?;
    Ok(json!({"schema":"narada.graph_mail_mcp.mailbox_organization_write.v1","status":"refused","reason":reason}))
}

pub(crate) fn record_audit(root: &Path, event: Value) -> Result<(), Value> {
    let path = root.join(".ai/audit/graph-mail-mcp.jsonl");
    if let Some(parent) = path.parent() { fs::create_dir_all(parent).map_err(|e| unavailable("graph_mail_audit_write_failed", &e.to_string()))?; }
    let mut object = event.as_object().cloned().unwrap_or_default();
    object.insert("schema".to_string(), json!("narada.graph_mail_mcp.audit.v1"));
    object.insert("recorded_at".to_string(), json!(OffsetDateTime::now_utc().format(&Rfc3339).unwrap_or_else(|_| "unknown".to_string())));
    let line = serde_json::to_string(&Value::Object(object)).map_err(|e| unavailable("graph_mail_audit_encode_failed", &e.to_string()))?;
    if line.len() > MAX_AUDIT_BYTES { return Err(unavailable("graph_mail_audit_record_too_large", "bounded audit record exceeded")); }
    let mut file = OpenOptions::new().create(true).append(true).open(path).map_err(|e| unavailable("graph_mail_audit_write_failed", &e.to_string()))?;
    file.write_all(line.as_bytes()).and_then(|_| file.write_all(b"\n")).map_err(|e| unavailable("graph_mail_audit_write_failed", &e.to_string()))
}

fn merge(left: Value, right: Value) -> Value {
    let mut object = left.as_object().cloned().unwrap_or_default();
    if let Some(extra) = right.as_object() { object.extend(extra.clone()); }
    Value::Object(object)
}

fn bool_value(object: &Map<String, Value>, snake: &str, camel: &str) -> bool {
    object.get(snake).and_then(Value::as_bool).unwrap_or(false) || object.get(camel).and_then(Value::as_bool).unwrap_or(false)
}

