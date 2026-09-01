
fn elevated_execute(
    state: &State,
    args: &Value,
    cancellation: Option<Arc<AtomicBool>>,
) -> Result<Value, StructuredError> {
    let object = args.as_object().cloned().unwrap_or_default();
    let command = object
        .get("command")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let command_args = value_strings(object.get("args"));
    let cwd = resolve_path(
        object
            .get("working_directory")
            .and_then(Value::as_str)
            .unwrap_or_default(),
        &state.allowed_roots[0],
    );
    let decision = decide(state, command, &command_args, &cwd);
    if decision.get("status").and_then(Value::as_str) != Some("allowed") {
        return Ok(
            json!({"schema":"narada.structured_command.elevated_window_result.v0","status":"refused","executed":false,"command":command,"args":command_args,"working_directory":cwd.to_string_lossy(),"decision":decision,"refusal_reasons":decision.get("reasons").cloned().unwrap_or_else(||json!([]))}),
        );
    }
    let wait = object.get("wait").and_then(Value::as_bool).unwrap_or(false);
    let script = format!("$ErrorActionPreference = 'Stop'; $p = Start-Process -FilePath {} -ArgumentList {} -WorkingDirectory {} -Verb RunAs -WindowStyle Normal -PassThru; {}", ps_single_quote(command), ps_array_literal(&command_args), ps_single_quote(&cwd.to_string_lossy()), if wait { "if ($p) { $p.WaitForExit(); exit $p.ExitCode }" } else { "if ($p) { Write-Output (\"started_pid=\" + $p.Id) }" });
    let broker = json!({"command": "powershell.exe", "args": ["-NoProfile", "-ExecutionPolicy", "Bypass", "-Command", script], "script": script});
    let dry_run = object
        .get("dry_run")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if dry_run {
        return Ok(
            json!({"schema": "narada.structured_command.elevated_window_result.v0", "status": "planned", "executed": false, "command": command, "args": command_args, "working_directory": cwd.to_string_lossy(), "wait": wait, "decision":decision,"broker": broker}),
        );
    }
    if object.get("confirm_elevation").and_then(Value::as_bool) != Some(true) {
        return Ok(
            json!({"schema":"narada.structured_command.elevated_window_result.v0","status":"refused","executed":false,"command":command,"args":command_args,"working_directory":cwd.to_string_lossy(),"wait":wait,"decision":decision,"refusal_reasons":["confirm_elevation_required"],"remediation_hints":["Retry with confirm_elevation=true only when a visible Windows UAC prompt is intended."],"broker":broker}),
        );
    }
    let timeout_ms = object
        .get("timeout_ms")
        .and_then(Value::as_u64)
        .unwrap_or(if wait { state.max_timeout_ms } else { 60_000 })
        .clamp(1, state.max_timeout_ms);
    let result = run_process(
        "powershell.exe",
        &[
            "-NoProfile".into(),
            "-NonInteractive".into(),
            "-ExecutionPolicy".into(),
            "Bypass".into(),
            "-Command".into(),
            script,
        ],
        &cwd,
        timeout_ms,
        state.max_output_bytes,
        cancellation,
        &state.env,
    );
    Ok(
        json!({"schema":"narada.structured_command.elevated_window_result.v0","status":if result.cancelled{"cancelled"}else if result.timed_out{"timed_out"}else if result.exit_code==Some(0){"started"}else{"failed"},"executed":true,"command":command,"args":command_args,"working_directory":cwd.to_string_lossy(),"wait":wait,"decision":decision,"timeout_ms":timeout_ms,"exit_code":result.exit_code,"stdout":result.stdout,"stderr":result.stderr,"stdout_truncated":result.stdout_truncated,"stderr_truncated":result.stderr_truncated,"timed_out":result.timed_out,"cancelled":result.cancelled,"broker":broker}),
    )
}

fn ps_single_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}

fn ps_array_literal(args: &[String]) -> String {
    if args.is_empty() {
        "@()".to_string()
    } else {
        format!(
            "@({})",
            args.iter()
                .map(|value| ps_single_quote(value))
                .collect::<Vec<_>>()
                .join(", ")
        )
    }
}

fn policy_payload(state: &State) -> Value {
    json!({
        "schema": "narada.structured_command.execution_policy.v0",
        "allowed_roots": state.allowed_roots.iter().map(|path| path.to_string_lossy().to_string()).collect::<Vec<_>>(),
        "allowed_commands": sorted_strings(&state.allowed_commands),
        "default_allowed_commands": DEFAULT_ALLOWED_COMMANDS,
        "allowed_prefixes": state.allowed_prefixes.iter().map(|prefix| prefix.join(" ")).collect::<Vec<_>>(),
        "default_allowed_prefixes": DEFAULT_ALLOWED_PREFIXES.iter().map(|prefix| prefix.join(" ")).collect::<Vec<_>>(),
        "blocked_commands": sorted_strings(&state.blocked_commands),
        "max_timeout_ms": state.max_timeout_ms,
        "max_output_bytes": state.max_output_bytes,
        "shell_interpolation": false,
    })
}

fn sorted_strings(values: &[String]) -> Vec<String> {
    let mut result = values.to_vec();
    result.sort();
    result
}

fn parse_ref(value: &str, kind: &str) -> Result<String, StructuredError> {
    let prefix = format!("structured_command_{kind}:");
    let Some(id) = value.strip_prefix(&prefix) else {
        return Err(StructuredError::new(
            format!("structured_command_invalid_{kind}_ref"),
            format!("structured_command_invalid_{kind}_ref"),
            json!({"ref": value, "expected_kind": kind}),
        ));
    };
    if id.len() < 8
        || !id.chars().all(|character| {
            character.is_ascii_alphanumeric() || character == '_' || character == '-'
        })
    {
        return Err(StructuredError::new(
            format!("structured_command_invalid_{kind}_ref"),
            format!("structured_command_invalid_{kind}_ref"),
            json!({"ref": value, "expected_kind": kind}),
        ));
    }
    Ok(id.to_string())
}

fn value_strings(value: Option<&Value>) -> Vec<String> {
    value
        .and_then(Value::as_array)
        .map(|values| {
            values
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

fn execution_path(state: &State, id: &str) -> PathBuf {
    state
        .storage_root
        .join("executions")
        .join(format!("{id}.json"))
}

fn input_path(state: &State, id: &str) -> PathBuf {
    state.storage_root.join("inputs").join(format!("{id}.json"))
}

fn write_json_record(path: &Path, value: &Value) -> Result<(), StructuredError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|error| {
            StructuredError::new(
                "structured_command_persistence_failed",
                error.to_string(),
                json!({"path": parent.to_string_lossy()}),
            )
        })?;
    }
    let serialized = format!(
        "{}\n",
        serde_json::to_string_pretty(value).map_err(|error| StructuredError::new(
            "structured_command_persistence_failed",
            error.to_string(),
            json!({"path": path.to_string_lossy()})
        ))?
    );
    fs::write(path, serialized).map_err(|error| {
        StructuredError::new(
            "structured_command_persistence_failed",
            error.to_string(),
            json!({"path": path.to_string_lossy()}),
        )
    })
}

fn read_json_record(path: &Path) -> Result<Value, StructuredError> {
    let bytes = fs::read(path).map_err(|_| {
        StructuredError::new(
            "structured_command_ref_not_found",
            "structured_command_ref_not_found",
            json!({"path": path.to_string_lossy()}),
        )
    })?;
    serde_json::from_slice(&bytes).map_err(|error| {
        StructuredError::new(
            "structured_command_ref_invalid_json",
            error.to_string(),
            json!({"path": path.to_string_lossy()}),
        )
    })
}

fn create_execution_record(state: &State, payload: &Value) -> Result<String, StructuredError> {
    let id = unique_id("e");
    let reference = format!("structured_command_execution:{id}");
    let record = json!({"schema": "narada.structured_command.execution.v0", "ref": reference, "created_at": now_rfc3339(), "sha256": sha256_json(payload), "result": payload});
    write_json_record(&execution_path(state, &id), &record)?;
    Ok(reference)
}
