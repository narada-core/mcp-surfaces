
fn show_execution(state: &State, args: &Value) -> Result<Value, StructuredError> {
    let object = args.as_object().cloned().unwrap_or_default();
    let reference = object
        .get("execution_ref")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            StructuredError::new(
                "structured_command_execution_ref_required",
                "structured_command_execution_ref_required",
                json!({}),
            )
        })?;
    let payload = read_execution_record(state, reference)?;
    Ok(page_execution(
        &payload,
        &object,
        Some(reference.to_string()),
    ))
}

fn start_background(
    state: &State,
    command: &str,
    args: &[String],
    cwd: &Path,
    timeout_ms: u64,
    posture: Value,
    input_ref: Value,
) -> Result<Value, StructuredError> {
    let started_at = now_rfc3339();
    let pending = json!({
        "schema":"narada.structured_command.execution_result.v0","status":"running","executed":true,
        "command":command,"args":args,"working_directory":cwd.to_string_lossy(),"started_at":started_at,
        "finished_at":Value::Null,"timeout_ms":timeout_ms,"execution_timeout_ms":Value::Null,
        "observation_timeout_ms":0,"durable_process_lifetime_ms":timeout_ms,
        "scheduled_termination":{"kind":"relative_deadline","after_ms":timeout_ms},"execution_posture":posture,
        "test_scope":posture.get("test_scope").and_then(Value::as_str).unwrap_or("unknown"),
        "expected_cost":posture.get("expected_cost").and_then(Value::as_str).unwrap_or("unknown"),
        "execution_mode":"background","wait_for_completion":false,"pending":true,"exit_code":Value::Null,
        "stdout":"","stderr":"","stdout_truncated":false,"stderr_truncated":false,
        "timed_out":false,"cancelled":false,"input_ref":input_ref,
    });
    let execution_ref = create_execution_record(state, &pending)?;
    let execution_id = parse_ref(&execution_ref, "execution")?;
    let request_path = state
        .storage_root
        .join("background")
        .join(format!("{execution_id}.json"));
    let request = json!({
        "schema":"narada.structured_command.background_request.v1","execution_ref":execution_ref,
        "command":command,"args":args,"working_directory":cwd.to_string_lossy(),"timeout_ms":timeout_ms,
        "max_output_bytes":state.max_output_bytes,"storage_root":state.storage_root.to_string_lossy(),
        "audit_log_dir":state.audit_log_dir.as_ref().map(|path| path.to_string_lossy().to_string()),
        "started_at":started_at,"execution_posture":posture,"input_ref":input_ref,
    });
    write_json_record(&request_path, &request)?;
    let bytes = fs::read(&request_path).map_err(|error| {
        StructuredError::new(
            "structured_command_background_request_read_failed",
            error.to_string(),
            json!({"path":request_path.to_string_lossy()}),
        )
    })?;
    let digest = hex::encode(Sha256::digest(&bytes));
    let executable = env::current_exe().map_err(|error| {
        StructuredError::new(
            "structured_command_background_executable_unavailable",
            error.to_string(),
            json!({}),
        )
    })?;
    let mut runner = Command::new(executable);
    runner
        .args([
            "structured-command-background",
            "--request",
            &request_path.to_string_lossy(),
            "--sha256",
            &digest,
        ])
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    apply_headless_process_posture(&mut runner);
    match runner.spawn() {
        Ok(child) => {
            let mut running = pending.clone();
            running["runner_pid"] = json!(child.id());
            update_execution_record(state, &execution_ref, &running)?;
            Ok(page_execution(&running, &Map::new(), Some(execution_ref)))
        }
        Err(error) => {
            let failed = json!({"schema":"narada.structured_command.execution_result.v0","status":"failed","executed":false,"pending":false,"command":command,"args":args,"working_directory":cwd.to_string_lossy(),"started_at":started_at,"finished_at":now_rfc3339(),"timeout_ms":timeout_ms,"execution_mode":"background","wait_for_completion":false,"error":"background_runner_spawn_failed","stderr":error.to_string(),"stdout":"","exit_code":Value::Null,"timed_out":false,"cancelled":false});
            update_execution_record(state, &execution_ref, &failed)?;
            let _ = fs::remove_file(request_path);
            Err(StructuredError::new(
                "structured_command_background_spawn_failed",
                error.to_string(),
                json!({"execution_ref":execution_ref}),
            ))
        }
    }
}

fn stop_execution(state: &State, args: &Value) -> Result<Value, StructuredError> {
    let reference = args
        .get("execution_ref")
        .and_then(Value::as_str)
        .ok_or_else(|| StructuredError::new("structured_command_execution_ref_required", "structured_command_execution_ref_required", json!({})))?;
    let mut payload = read_execution_record(state, reference)?;
    if payload.get("pending").and_then(Value::as_bool) != Some(true) {
        return Ok(json!({"schema":"narada.structured_command.execution_control.v1","status":"already_terminal","execution_ref":reference,"execution":page_execution(&payload,&Map::new(),Some(reference.to_string()))}));
    }
    let pid = payload
        .get("runner_pid")
        .and_then(Value::as_u64)
        .ok_or_else(|| StructuredError::new("structured_command_runner_pid_missing", "structured_command_runner_pid_missing", json!({"execution_ref":reference})))?;
    #[cfg(windows)]
    let status = Command::new("taskkill")
        .args(["/PID", &pid.to_string(), "/T", "/F"])
        .creation_flags(CREATE_NO_WINDOW)
        .status();
    #[cfg(not(windows))]
    let status = Command::new("kill").args(["-TERM", &pid.to_string()]).status();
    let stopped = status.map(|value| value.success()).unwrap_or(false);
    if !stopped {
        return Err(StructuredError::new("structured_command_execution_stop_failed", "structured_command_execution_stop_failed", json!({"execution_ref":reference,"runner_pid":pid})));
    }
    payload["status"] = json!("stopped");
    payload["pending"] = json!(false);
    payload["cancelled"] = json!(true);
    payload["finished_at"] = json!(now_rfc3339());
    payload["stop_reason"] = args.get("reason").cloned().unwrap_or_else(|| json!("operator_requested"));
    update_execution_record(state, reference, &payload)?;
    Ok(json!({"schema":"narada.structured_command.execution_control.v1","status":"stopped","execution_ref":reference,"runner_pid":pid}))
}

fn restart_execution(state: &State, args: &Value) -> Result<Value, StructuredError> {
    let reference = args
        .get("execution_ref")
        .and_then(Value::as_str)
        .ok_or_else(|| StructuredError::new("structured_command_execution_ref_required", "structured_command_execution_ref_required", json!({})))?;
    let payload = read_execution_record(state, reference)?;
    if payload.get("pending").and_then(Value::as_bool) == Some(true) {
        stop_execution(state, &json!({"execution_ref":reference,"reason":"restart"}))?;
    }
    let command = payload.get("command").and_then(Value::as_str).unwrap_or_default();
    let command_args = value_strings(payload.get("args"));
    let cwd = PathBuf::from(payload.get("working_directory").and_then(Value::as_str).unwrap_or_default());
    let lifetime_ms = args
        .get("durable_process_lifetime_ms")
        .and_then(Value::as_u64)
        .or_else(|| payload.get("durable_process_lifetime_ms").and_then(Value::as_u64))
        .unwrap_or(state.max_timeout_ms)
        .clamp(1, state.max_timeout_ms);
    let mut replacement = start_background(
        state,
        command,
        &command_args,
        &cwd,
        lifetime_ms,
        payload.get("execution_posture").cloned().unwrap_or_else(|| json!({})),
        payload.get("input_ref").cloned().unwrap_or(Value::Null),
    )?;
    replacement["restarts_execution_ref"] = json!(reference);
    Ok(replacement)
}

fn infer_test_scope(command: &str, args: &[String]) -> &'static str {
    if command.eq_ignore_ascii_case("pnpm")
        && args.iter().any(|value| value.eq_ignore_ascii_case("test"))
    {
        if args.iter().any(|value| value == "--filter") {
            "focused"
        } else {
            "broad"
        }
    } else if command.eq_ignore_ascii_case("npm")
        && args.iter().any(|value| value.eq_ignore_ascii_case("test"))
    {
        "broad"
    } else {
        "unknown"
    }
}

fn infer_expected_cost(test_scope: &str) -> &'static str {
    match test_scope {
        "focused" => "low",
        "broad" | "known_slow" => "high",
        _ => "unknown",
    }
}

#[allow(clippy::too_many_arguments)]
fn execution_payload(
    command: &str,
    args: &[String],
    cwd: &Path,
    started_at: &str,
    timeout_ms: u64,
    posture: Value,
    result: ProcessResult,
    mode: &str,
    wait_for_completion: bool,
    input_ref: Value,
) -> Value {
    json!({
        "schema": "narada.structured_command.execution_result.v0",
        "status": if result.cancelled { "cancelled" } else if result.timed_out { "timed_out" } else if result.exit_code == Some(0) { "ok" } else { "failed" },
        "executed": true,
        "command": command,
        "args": args,
        "working_directory": cwd.to_string_lossy(),
        "started_at": started_at,
        "finished_at": now_rfc3339(),
        "timeout_ms": timeout_ms,
        "execution_posture": posture,
        "test_scope": posture.get("test_scope").and_then(Value::as_str).unwrap_or("unknown"),
        "expected_cost": posture.get("expected_cost").and_then(Value::as_str).unwrap_or("unknown"),
        "execution_mode": mode,
        "wait_for_completion": wait_for_completion,
        "pending": false,
        "exit_code": result.exit_code,
        "stdout": result.stdout,
        "stderr": result.stderr,
        "stdout_truncated": result.stdout_truncated,
        "stderr_truncated": result.stderr_truncated,
        "timed_out": result.timed_out,
        "cancelled": result.cancelled,
        "input_ref": input_ref,
    })
}
