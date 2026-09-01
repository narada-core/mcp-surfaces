fn absolute_binding_path(value: &str, field: &str) -> Result<String, String> {
    let path = PathBuf::from(value);
    if !path.is_absolute() { return Err(format!("{field}_must_be_absolute")); }
    Ok(path.to_string_lossy().to_string())
}
fn normalize_execution_binding(root: &Path, value: Option<&Value>, correlation_key: &str) -> Result<Value, String> {
    let input = value.and_then(Value::as_object).cloned().unwrap_or_default();
    for key in input.keys() {
        if !matches!(key.as_str(), "workspace_root" | "executor_kind" | "executor_profile" | "executor_id" | "repository_root" | "site_root" | "correlation_key") {
            return Err(format!("execution_binding_unknown_fields: {key}"));
        }
    }
    let workspace_root = binding_string(&input, "workspace_root", true)?
        .unwrap_or_else(|| root.to_string_lossy().to_string());
    let workspace_root = absolute_binding_path(&workspace_root, "execution_binding_workspace_root")?;
    let executor_kind = binding_string(&input, "executor_kind", true)?
        .unwrap_or_else(|| "manual".to_string());
    if !matches!(executor_kind.as_str(), "manual" | "operator" | "worker_delegation" | "delegated_task") {
        return Err(format!("execution_binding_executor_kind_invalid: {executor_kind}"));
    }
    let correlation_key = binding_string(&input, "correlation_key", true)?
        .unwrap_or_else(|| correlation_key.to_string());
    let executor_profile = binding_string(&input, "executor_profile", false)?;
    let executor_id = binding_string(&input, "executor_id", false)?;
    let repository_root = binding_string(&input, "repository_root", false)?
        .map(|value| absolute_binding_path(&value, "execution_binding_repository_root"))
        .transpose()?;
    let site_root = binding_string(&input, "site_root", false)?.or_else(|| Some(root.to_string_lossy().to_string()))
        .map(|value| absolute_binding_path(&value, "execution_binding_site_root"))
        .transpose()?;
    Ok(json!({
        "workspace_root": workspace_root,
        "executor_kind": executor_kind,
        "executor_profile": executor_profile,
        "executor_id": executor_id,
        "repository_root": repository_root,
        "site_root": site_root,
        "correlation_key": correlation_key,
    }))
}
fn path_within_root(candidate: &str, root: &Path) -> bool {
    let candidate = normalized_path_string(Path::new(candidate));
    let root = normalized_path_string(root);
    candidate == root || candidate.starts_with(&(root + "/"))
}
fn validate_execution_binding_scope(binding: &Value, site_root: &Path) -> Result<(), String> {
    let Some(binding) = binding.as_object() else { return Err("execution_binding_invalid".to_string()); };
    let binding_site_root = binding.get("site_root").and_then(Value::as_str)
        .map(PathBuf::from)
        .unwrap_or_else(|| site_root.to_path_buf());
    let current_root = normalized_path_string(site_root);
    let binding_root = normalized_path_string(&binding_site_root);
    let contained_site_root = site_root.join(".narada");
    let contained_site_declared = binding_root == normalized_path_string(&contained_site_root)
        && site_root.join(".git").exists()
        && contained_site_root.join("config.json").exists();
    if binding_root != current_root && !contained_site_declared {
        return Err("task_lifecycle_execution_binding_site_root_mismatch".to_string());
    }

    let workspace = binding.get("workspace_root").and_then(Value::as_str).ok_or("execution_binding_workspace_root_required")?;
    let authority_root = binding_site_root.as_path();
    let site_is_narada = authority_root.file_name().and_then(|value| value.to_str()).is_some_and(|value| value.eq_ignore_ascii_case(".narada"));
    let workspace_authorized = path_within_root(workspace, authority_root)
        || (site_is_narada && normalized_path_string(Path::new(workspace)) == normalized_path_string(authority_root.parent().unwrap_or(authority_root)) && authority_root.parent().is_some_and(|value| value.join(".git").exists()));
    if !workspace_authorized { return Err("task_lifecycle_execution_binding_workspace_outside_site_root".to_string()); }
    if let Some(repository) = binding.get("repository_root").and_then(Value::as_str) {
        if !path_within_root(repository, authority_root)
            && !(site_is_narada && normalized_path_string(Path::new(repository)) == normalized_path_string(authority_root.parent().unwrap_or(authority_root)) && authority_root.parent().is_some_and(|value| value.join(".git").exists()))
        {
            return Err("task_lifecycle_execution_binding_repository_outside_site_root".to_string());
        }
    }
    Ok(())
}
fn resolve_payload_args(root: &Path, args: &Value) -> Result<Value, String> {
    let Some(reference) = string_arg(args, "payload_ref") else {
        return Ok(args.clone());
    };
    let payload = if parse_payload_reference(&reference).is_ok() {
        read_payload_revision_payload(root, &reference)?
    } else {
        let id = safe_reference_id(&reference, "mcp_payload:")?;
        let path = root
            .join(".ai")
            .join("mcp-payloads")
            .join(format!("{id}.json"));
        let text =
            fs::read_to_string(path).map_err(|_| format!("payload_ref_not_found: {reference}"))?;
        let payload: Value =
            serde_json::from_str(&text).map_err(|e| format!("payload_invalid:{e}"))?;
        if !payload.is_object() {
            return Err(format!("payload_ref_payload_must_be_object:{reference}"));
        }
        payload
    };
    let mut merged = payload
        .as_object()
        .cloned()
        .ok_or_else(|| format!("payload_ref_payload_must_be_object:{reference}"))?;
    if let Some(object) = args.as_object() {
        for (key, value) in object {
            if !matches!(
                key.as_str(),
                "payload_ref" | "payload_path" | "payload" | "payload_file"
            ) {
                merged.insert(key.clone(), value.clone());
            }
        }
    }
    Ok(Value::Object(merged))
}
fn task_file_path(root: &Path, task_id: &str) -> String {
    root.join(".ai/do-not-open/tasks")
        .join(format!("{task_id}.md"))
        .to_string_lossy()
        .to_string()
}
fn task_file_body(root: &Path, task_id: &str, number: i64) -> Option<String> {
    let path = root.join(".ai/do-not-open/tasks").join(format!("{task_id}.md"));
    let text = fs::read_to_string(path).ok()?;
    text.lines()
        .any(|line| line.trim() == format!("number: {number}"))
        .then_some(text)
}
fn project_task_status(root: &Path, task_id: &str, number: i64, status: &str) -> Result<(), String> {
    let path = root.join(".ai/do-not-open/tasks").join(format!("{task_id}.md"));
    let text = fs::read_to_string(&path).map_err(|e| format!("task_projection_read_failed:{e}"))?;
    if !text.lines().any(|line| line.trim() == format!("number: {number}")) {
        return Err(format!("task_projection_number_mismatch:{task_id}:{number}"));
    }
    let mut replaced = false;
    let mut output = String::with_capacity(text.len() + status.len());
    for line in text.lines() {
        if line.starts_with("status:") && !replaced {
            output.push_str(&format!("status: {status}"));
            replaced = true;
        } else { output.push_str(line); }
        output.push('\n');
    }
    if !replaced { output = format!("status: {status}\n{output}"); }
    fs::write(path, output).map_err(|e| format!("task_projection_write_failed:{e}"))
}
fn write_task_file(
    root: &Path,
    task_id: &str,
    number: i64,
    title: &str,
    goal: &str,
    work: &str,
    non_goals: &str,
    criteria: &Value,
    tags: &Value,
    role: Option<&str>,
    idem: &str,
) -> Result<(), String> {
    let dir = root.join(".ai/do-not-open/tasks");
    fs::create_dir_all(&dir).map_err(|e| format!("task_projection_directory_create_failed:{e}"))?;
    let path = dir.join(format!("{task_id}.md"));
    let tags_text = tags
        .as_array()
        .map(|v| {
            v.iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join(", ")
        })
        .unwrap_or_default();
    let body=format!("---\nnumber: {number}\ngoverned_by: {}\nstatus: opened\n{}{}tags: {tags_text}\nidempotency_key: {idem}\n---\n# {title}\n\n## Goal\n{goal}\n\n## Required Work\n{work}\n\n## Non-Goals\n{non_goals}\n\n## Acceptance Criteria\n{}\n\n## Execution Notes\n\n## Verification\n",role.unwrap_or("unknown"),role.map(|v|format!("preferred_role: {v}\n")).unwrap_or_default(),if tags_text.is_empty(){String::new()}else{String::new()},criteria.as_array().map(|v|v.iter().filter_map(Value::as_str).map(|v|format!("- [ ] {v}\n")).collect::<String>()).unwrap_or_default());
    fs::write(path, body).map_err(|e| format!("task_projection_write_failed:{e}"))
}
fn append_task_body(root: &Path, task_id: &str, number: i64, summary: &str) -> Result<(), String> {
    let path = root.join(".ai/do-not-open/tasks").join(format!("{task_id}.md"));
    let text = fs::read_to_string(&path).map_err(|e| format!("task_file_read_failed:{e}"))?;
    if !text.lines().any(|line| line.trim() == format!("number: {number}")) {
        return Err(format!("task_projection_number_mismatch:{task_id}:{number}"));
    }
    let next = format!("{text}\n{summary}\n");
    fs::write(path, next).map_err(|e| format!("task_file_write_failed:{e}"))
}
fn replace_task_markdown_section(root: &Path, task_id: &str, number: i64, heading: &str, body: &str) -> Result<(), String> {
    let path = root.join(".ai/do-not-open/tasks").join(format!("{task_id}.md"));
    let original = fs::read_to_string(&path).map_err(|error| format!("task_file_read_failed:{error}"))?;
    if !original.lines().any(|line| line.trim() == format!("number: {number}")) {
        return Err(format!("task_projection_number_mismatch:{task_id}:{number}"));
    }
    let marker = format!("## {heading}");
    let Some(start) = original.find(&marker) else { return Err(format!("task_lifecycle_submit_work_section_missing:{heading}")); };
    let content_start = start + marker.len();
    let remainder = &original[content_start..];
    let next_heading = remainder.find("\n## ").map(|offset| content_start + offset).unwrap_or(original.len());
    let replacement = format!("{marker}\n\n{}\n", body.trim());
    let mut updated = String::with_capacity(original.len() + replacement.len());
    updated.push_str(&original[..start]);
    updated.push_str(&replacement);
    if next_heading < original.len() { updated.push_str(&original[next_heading..]); }
    fs::write(path, updated).map_err(|error| format!("task_file_write_failed:{error}"))
}
fn normalized_path_string(path: &Path) -> String {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        env::current_dir().unwrap_or_else(|_| PathBuf::from(".")).join(path)
    };
    let text = absolute.to_string_lossy().replace('\\', "/");
    let text = text.trim_end_matches('/');
    if cfg!(windows) { text.to_ascii_lowercase() } else { text.to_string() }
}
