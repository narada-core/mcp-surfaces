
fn wraps_cargo_with_pnpm(command: &str, args: &[String]) -> bool {
    command.eq_ignore_ascii_case("pnpm")
        && args
            .first()
            .is_some_and(|value| value.eq_ignore_ascii_case("exec"))
        && args.get(1).is_some_and(|value| {
            value.eq_ignore_ascii_case("cargo") || value.eq_ignore_ascii_case("cargo.exe")
        })
}

fn is_command_allowed(
    command: &str,
    args: &[String],
    allowed_commands: &[String],
    allowed_prefixes: &[Vec<String>],
) -> bool {
    let command_lower = command.to_ascii_lowercase();
    if allowed_commands.iter().any(|value| value == &command_lower) {
        return true;
    }
    let argv = std::iter::once(command)
        .chain(args.iter().map(String::as_str))
        .map(|value| value.to_ascii_lowercase())
        .collect::<Vec<_>>();
    allowed_prefixes.iter().any(|prefix| {
        prefix.iter().enumerate().all(|(index, expected)| {
            let Some(actual) = argv.get(index) else {
                return false;
            };
            if index == 0 {
                actual == expected || (expected == "pwsh" && actual == "pwsh.exe")
            } else {
                actual == expected
            }
        }) && !(prefix.len() >= 2
            && prefix[0] == "pnpm"
            && prefix[1] == "--filter"
            && !matches!(
                argv.get(3).map(String::as_str),
                Some("test" | "build" | "typecheck")
            ))
    })
}

fn transient_path(value: &str) -> bool {
    let normalized = value.replace('\\', "/").to_ascii_lowercase();
    normalized.contains("/.ai/tmp/")
        || normalized.contains("/.ai/temp/")
        || normalized.starts_with(".ai/tmp/")
        || normalized.starts_with(".ai/temp/")
}

fn normalize_command(value: &str) -> String {
    let trimmed = value.trim();
    if trimmed.is_empty()
        || trimmed
            .chars()
            .any(|character| matches!(character, '\r' | '\n' | ';' | '&' | '|' | '<' | '>'))
    {
        String::new()
    } else {
        trimmed.to_string()
    }
}

fn tool_result(state: &State, payload: Value, tool_name: &str) -> Result<Value, StructuredError> {
    // Guidance carries its complete recovery contract in structuredContent. Keep the
    // text projection deliberately small so clients that retain both MCP projections
    // cannot exceed the compact-output budget by storing the same guidance twice.
    if tool_name == "structured_command_guidance" {
        return Ok(json!({
            "content": [{"type": "text", "text": "Structured guidance and recovery_commands are available in structuredContent.", "annotations": {"audience": ["assistant"]}}],
            "structuredContent": payload
        }));
    }
    let text = serde_json::to_string_pretty(&payload).unwrap_or_else(|_| "{}".to_string());
    if text.chars().count() <= 4_000 {
        return Ok(
            json!({"content": [{"type": "text", "text": text, "annotations": {"audience": ["assistant"]}}], "structuredContent": payload}),
        );
    }
    let (reference, full_length) = materialize_output(state, tool_name, &payload, &text)?;
    let preview = text.chars().take(3_200).collect::<String>();
    let envelope = json!({"schema": "narada.producer_output_page.v1", "status": payload.get("status").and_then(Value::as_str).unwrap_or("ok"), "truncated": true, "output_ref": reference, "ref": reference, "result_materialized": true, "tool_name": tool_name, "offset": 0, "limit": 3_200, "next_offset": if full_length > 3_200 { json!(3_200) } else { Value::Null }, "transport_offset": 0, "transport_limit": 3_200, "transport_next_offset": if full_length > 3_200 { json!(3_200) } else { Value::Null }, "output_text": preview, "output_truncated": full_length > 3_200, "reader_tool": "structured_command_output_show", "site_root": state.site_root.to_string_lossy(), "read_command": format!("structured_command_output_show({{ ref: \\\"{reference}\\\" }})"), "remediation": format!("Use structured_command_output_show with ref={reference} to read bounded pages."), "inline_limit": 3_200, "full_output_char_length": full_length});
    let content = serde_json::to_string(&envelope).unwrap_or_else(|_| "{}".to_string());
    Ok(
        json!({"content": [{"type": "text", "text": content, "annotations": {"audience": ["assistant"]}}], "structuredContent": envelope}),
    )
}

fn materialize_output(
    state: &State,
    tool_name: &str,
    payload: &Value,
    text: &str,
) -> Result<(String, usize), StructuredError> {
    let id = unique_id("o");
    let reference = format!("mcp_output:{id}");
    let path = state
        .site_root
        .join(".ai")
        .join("tmp")
        .join("mcp-outputs")
        .join("workspace")
        .join(format!("{id}.json"));
    let record = json!({"schema": "narada.mcp_output_ref.v1", "ref": reference, "output_id": id, "tool_name": tool_name, "created_at": now_rfc3339(), "created_by": Value::Null, "content_type": "application/json", "inline_char_limit": 3_200, "full_output_char_length": text.chars().count(), "truncated": true, "sha256": sha256_json(payload), "max_bytes": 20 * 1024 * 1024, "full_output": payload});
    let serialized = format!(
        "{}\n",
        serde_json::to_string(&record).unwrap_or_else(|_| "{}".to_string())
    );
    if serialized.len() > 20 * 1024 * 1024 {
        return Err(StructuredError::new(
            "mcp_output_too_large",
            "mcp_output_too_large",
            json!({"ref": reference}),
        ));
    }
    write_json_record(&path, &record)?;
    Ok((reference, text.chars().count()))
}

fn output_show(state: &State, args: &Value) -> Result<Value, StructuredError> {
    let object = args.as_object().cloned().unwrap_or_default();
    let reference = object
        .get("ref")
        .and_then(Value::as_str)
        .or_else(|| object.get("output_ref").and_then(Value::as_str))
        .unwrap_or_default();
    let Some(id) = reference.strip_prefix("mcp_output:") else {
        return Err(StructuredError::new(
            "output_ref_invalid",
            "output_ref_invalid",
            json!({"ref": reference}),
        ));
    };
    if id.len() < 8
        || !id.chars().all(|character| {
            character.is_ascii_alphanumeric() || character == '_' || character == '-'
        })
    {
        return Err(StructuredError::new(
            "output_ref_invalid",
            "output_ref_invalid",
            json!({"ref": reference}),
        ));
    }
    let record = read_json_record(
        &state
            .site_root
            .join(".ai")
            .join("tmp")
            .join("mcp-outputs")
            .join("workspace")
            .join(format!("{id}.json")),
    )?;
    if record.get("schema").and_then(Value::as_str) != Some("narada.mcp_output_ref.v1") {
        return Err(StructuredError::new(
            "output_ref_schema_unsupported",
            "output_ref_schema_unsupported",
            json!({"ref": reference}),
        ));
    }
    let payload = record.get("full_output").cloned().unwrap_or(Value::Null);
    let text = serde_json::to_string_pretty(&payload).unwrap_or_else(|_| "null".to_string());
    let offset = object.get("offset").and_then(Value::as_u64).unwrap_or(0) as usize;
    let limit = object
        .get("limit")
        .and_then(Value::as_u64)
        .unwrap_or(20_000)
        .clamp(1, 20_000) as usize;
    let page = text_page(&text, offset, limit);
    Ok(
        json!({"schema": "narada.mcp_output_page.v1", "status": "ok", "ref": reference, "tool_name": record.get("tool_name"), "full_output_char_length": text.chars().count(), "byte_size": Value::Null, "original_truncated": true, "path": format!(".ai/tmp/mcp-outputs/workspace/{id}.json"), "offset": offset.min(text.chars().count()), "limit": limit, "output_limit": limit, "output_truncated": page.1.is_some(), "next_offset": page.1.map(|value| json!(value)).unwrap_or(Value::Null), "output_text": page.0}),
    )
}

fn error_diagnostic(error: &StructuredError) -> Value {
    let mut details = error.details.as_object().cloned().unwrap_or_default();
    details.insert(
        "diagnostic_owner".to_string(),
        json!("structured-command-mcp"),
    );
    details.insert(
        "diagnostic_rule".to_string(),
        json!("surface_policy_or_tool_validation"),
    );
    json!({"schema": "narada.structured_command.error.v0", "code": error.code, "message": error.message, "details": details})
}

struct ProcessResult {
    exit_code: Option<i32>,
    timed_out: bool,
    cancelled: bool,
    stdout: String,
    stderr: String,
    stdout_truncated: bool,
    stderr_truncated: bool,
}

fn run_process(
    command: &str,
    args: &[String],
    cwd: &Path,
    timeout_ms: u64,
    max_output_bytes: usize,
    cancellation: Option<Arc<AtomicBool>>,
    environment: &std::collections::HashMap<String, String>,
) -> ProcessResult {
    let (spawn_command, spawn_args) = resolve_command_for_spawn(command, args, environment);
    let mut process = Command::new(spawn_command);
    process
        .args(spawn_args)
        .current_dir(cwd)
        .envs(environment)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    apply_headless_process_posture(&mut process);
    let child_result = process.spawn();
    let Ok(mut child) = child_result else {
        return ProcessResult {
            exit_code: None,
            timed_out: false,
            cancelled: false,
            stdout: String::new(),
            stderr: child_result
                .err()
                .map(|error| error.to_string())
                .unwrap_or_else(|| "process_spawn_failed".to_string()),
            stdout_truncated: false,
            stderr_truncated: false,
        };
    };
    let stdout_handle = child
        .stdout
        .take()
        .map(|stream| thread::spawn(move || read_bounded(stream, max_output_bytes)));
    let stderr_handle = child
        .stderr
        .take()
        .map(|stream| thread::spawn(move || read_bounded(stream, max_output_bytes)));
    let deadline = Instant::now() + Duration::from_millis(timeout_ms);
    let mut timed_out = false;
    let mut cancelled = false;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None)
                if cancellation
                    .as_ref()
                    .is_some_and(|token| token.load(Ordering::Acquire)) =>
            {
                cancelled = true;
                kill_child(&mut child);
                break child.wait().ok();
            }
            Ok(None) if Instant::now() >= deadline => {
                timed_out = true;
                kill_child(&mut child);
                break child.wait().ok();
            }
            Ok(None) => thread::sleep(Duration::from_millis(5)),
            Err(_) => break child.wait().ok(),
        }
    };
    let stdout_result = stdout_handle
        .and_then(|handle| handle.join().ok())
        .unwrap_or((Vec::new(), false));
    let stderr_result = stderr_handle
        .and_then(|handle| handle.join().ok())
        .unwrap_or((Vec::new(), false));
    ProcessResult {
        exit_code: status.and_then(|status| status.code()),
        timed_out,
        cancelled,
        stdout: String::from_utf8_lossy(&stdout_result.0).to_string(),
        stderr: String::from_utf8_lossy(&stderr_result.0).to_string(),
        stdout_truncated: stdout_result.1,
        stderr_truncated: stderr_result.1,
    }
}
