
fn page_execution(payload: &Value, args: &Map<String, Value>, reference: Option<String>) -> Value {
    let persisted = args.contains_key("execution_ref");
    if payload.get("executed").and_then(Value::as_bool) == Some(false) {
        let mut result = payload.clone();
        if let Some(object) = result.as_object_mut() {
            object.insert(
                "execution_ref".to_string(),
                reference.clone().map(Value::String).unwrap_or(Value::Null),
            );
        }
        return result;
    }
    let stdout = payload
        .get("stdout")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let stderr = payload
        .get("stderr")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let stdout_offset = args
        .get("stdout_offset")
        .and_then(Value::as_u64)
        .unwrap_or(0) as usize;
    let stderr_offset = args
        .get("stderr_offset")
        .and_then(Value::as_u64)
        .unwrap_or(0) as usize;
    let stdout_limit = args
        .get("stdout_limit")
        .and_then(Value::as_u64)
        .unwrap_or(1000)
        .clamp(1, 20_000) as usize;
    let stderr_limit = args
        .get("stderr_limit")
        .and_then(Value::as_u64)
        .unwrap_or(1000)
        .clamp(1, 20_000) as usize;
    let stdout_page = text_page(stdout, stdout_offset, stdout_limit);
    let stderr_page = text_page(stderr, stderr_offset, stderr_limit);
    let mut result = payload.clone();
    if let Some(object) = result.as_object_mut() {
        object.insert(
            "execution_ref".to_string(),
            reference.clone().map(Value::String).unwrap_or(Value::Null),
        );
        object.insert("stdout".to_string(), Value::String(stdout_page.0));
        object.insert("stderr".to_string(), Value::String(stderr_page.0));
        object.insert("stdout_offset".to_string(), json!(stdout_offset));
        object.insert("stderr_offset".to_string(), json!(stderr_offset));
        object.insert("stdout_limit".to_string(), json!(stdout_limit));
        object.insert("stderr_limit".to_string(), json!(stderr_limit));
        object.insert(
            "stdout_next_offset".to_string(),
            stdout_page
                .1
                .map(|value| json!(value))
                .unwrap_or(Value::Null),
        );
        object.insert(
            "stderr_next_offset".to_string(),
            stderr_page
                .1
                .map(|value| json!(value))
                .unwrap_or(Value::Null),
        );
        object.insert(
            "stdout_output_truncated".to_string(),
            json!(stdout_page.1.is_some()),
        );
        object.insert(
            "stderr_output_truncated".to_string(),
            json!(stderr_page.1.is_some()),
        );
        object.insert(
            "stdout_char_length".to_string(),
            json!(stdout.chars().count()),
        );
        object.insert(
            "stderr_char_length".to_string(),
            json!(stderr.chars().count()),
        );
        object.insert(
            "page_source".to_string(),
            json!(if persisted {
                "persisted_execution"
            } else {
                "new_execution"
            }),
        );
    }
    result
}

fn text_page(text: &str, offset: usize, limit: usize) -> (String, Option<usize>) {
    let chars = text.chars().collect::<Vec<_>>();
    let start = offset.min(chars.len());
    let end = (start + limit).min(chars.len());
    let chunk = chars[start..end].iter().collect::<String>();
    let next = if end < chars.len() { Some(end) } else { None };
    (chunk, next)
}

fn decide(state: &State, command: &str, args: &[String], cwd: &Path) -> Value {
    let mut reasons = Vec::<String>::new();
    if command.is_empty() {
        reasons.push("command_required".to_string());
    }
    let command_lower = command.to_ascii_lowercase();
    if state
        .blocked_commands
        .iter()
        .any(|value| value == &command_lower)
    {
        reasons.push(format!("blocked_command:{command}"));
    }
    if wraps_cargo_with_pnpm(command, args) {
        reasons.push("package_manager_wrapper_for_native_tool:pnpm cargo".to_string());
    }
    if !inside_any_root(cwd, &state.allowed_roots) {
        reasons.push(format!(
            "working_directory_outside_allowed_roots:{}",
            cwd.to_string_lossy()
        ));
    }
    for value in std::iter::once(command).chain(args.iter().map(String::as_str)) {
        let normalized = value.replace('\\', "/");
        let extension = Path::new(&normalized)
            .extension()
            .and_then(|value| value.to_str())
            .map(|value| format!(".{value}").to_ascii_lowercase());
        if matches!(extension.as_deref(), Some(".cmd") | Some(".bat")) {
            let candidate = resolve_path(value, cwd);
            if !inside_any_root(&candidate, &state.allowed_roots)
                || !candidate.is_file()
                || transient_path(&normalized)
            {
                reasons.push(format!("wrapper_execution_disallowed:{value}"));
            }
        }
        if transient_path(&normalized)
            && extension
                .as_deref()
                .is_some_and(|extension| TRANSIENT_EXTENSIONS.contains(&extension))
        {
            reasons.push(format!("transient_wrapper_path_disallowed:{value}"));
        }
    }
    if !is_command_allowed(
        command,
        args,
        &state.allowed_commands,
        &state.allowed_prefixes,
    ) {
        reasons.push(format!(
            "command_not_allowed:{}",
            std::iter::once(command)
                .chain(args.iter().map(String::as_str))
                .collect::<Vec<_>>()
                .join(" ")
        ));
    }
    let status = if reasons.is_empty() {
        "allowed"
    } else {
        "refused"
    };
    let command_name = Path::new(command)
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or(command)
        .to_ascii_lowercase();
    let is_git = command_name == "git";
    let is_content_search = matches!(command_name.as_str(), "rg" | "grep" | "findstr");
    let is_file_listing = matches!(command_name.as_str(), "ls" | "dir" | "find")
        || (command_name == "rg" && args.iter().any(|value| value == "--files"));
    let remediation_hints = reasons
        .iter()
        .map(|reason| {
            if reason.starts_with("blocked_command:") {
                "Use an explicit argv-based allowed command; shell interpreters remain disallowed."
            } else if reason.starts_with("working_directory_outside_allowed_roots:") {
                "Run from an allowed root or request a policy update."
            } else if reason.starts_with("command_not_allowed:") && is_git {
                "Use the Site-owned Git MCP binding through mcp-loader; do not add git to structured-command policy."
            } else if reason.starts_with("command_not_allowed:") && is_file_listing {
                "Use local-filesystem fs_glob_search for bounded file discovery."
            } else if reason.starts_with("command_not_allowed:") && is_content_search {
                "Use local-filesystem fs_grep_search for content search or fs_glob_search for file discovery."
            } else if reason.starts_with("command_not_allowed:") {
                "Inspect policy and use an allowlisted command or prefix."
            } else if reason.starts_with("package_manager_wrapper_for_native_tool:") {
                "Invoke cargo directly; pnpm is not part of the native Rust toolchain."
            } else {
                "Use the owning MCP surface or a canonical repository entrypoint."
            }
        })
        .map(String::from)
        .collect::<Vec<_>>();
    let refused_by_command_policy = reasons
        .iter()
        .any(|reason| reason.starts_with("command_not_allowed:"));
    let mcp_fallbacks = if !refused_by_command_policy {
        json!([])
    } else if is_git {
        let tool_name = match args.first().map(String::as_str) {
            Some("add") => "git_add",
            Some("commit") => "git_commit",
            Some("diff") => "git_diff",
            Some("log") => "git_log",
            Some("push") => "git_push",
            Some("show") => "git_show",
            _ => "git_status",
        };
        json!([{
            "surface_id": "git",
            "binding_id_pattern": "<site-id>-git",
            "activation_tool": "mcp_loader_resume_or_open_surface",
            "inspection_tool": "mcp_loader_inspect_binding_tool",
            "call_tool": "mcp_loader_call_binding_tool",
            "child_tool_name": tool_name
        }])
    } else if is_file_listing {
        json!([{
            "surface_id": "local-filesystem",
            "tool_name": "fs_glob_search",
            "purpose": "bounded_file_discovery",
            "arguments": { "pattern": "*", "directory": cwd.to_string_lossy() }
        }])
    } else if is_content_search {
        let pattern = args
            .iter()
            .find(|value| !value.starts_with('-'))
            .cloned()
            .unwrap_or_else(|| "<search pattern>".to_string());
        json!([{
            "surface_id": "local-filesystem",
            "tool_name": "fs_grep_search",
            "purpose": "bounded_content_search",
            "arguments": { "pattern": pattern, "path": cwd.to_string_lossy() }
        }])
    } else {
        json!([])
    };
    json!({"schema": "narada.structured_command.execution_decision.v0", "status": status, "reasons": reasons, "remediation_hints": remediation_hints, "mcp_fallbacks": mcp_fallbacks, "command": command, "args": args, "working_directory": cwd.to_string_lossy(), "shell_interpolation": false})
}
