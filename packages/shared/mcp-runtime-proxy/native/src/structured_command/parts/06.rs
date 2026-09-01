
fn update_execution_record(
    state: &State,
    reference: &str,
    payload: &Value,
) -> Result<(), StructuredError> {
    let id = parse_ref(reference, "execution")?;
    let path = execution_path(state, &id);
    let existing = read_json_record(&path)?;
    let created_at = existing
        .get("created_at")
        .cloned()
        .unwrap_or_else(|| json!(now_rfc3339()));
    let record = json!({"schema": "narada.structured_command.execution.v0", "ref": reference, "created_at": created_at, "updated_at": now_rfc3339(), "sha256": sha256_json(payload), "result": payload});
    write_json_record(&path, &record)
}

fn read_execution_record(state: &State, reference: &str) -> Result<Value, StructuredError> {
    let id = parse_ref(reference, "execution")?;
    let record = read_json_record(&execution_path(state, &id))?;
    record.get("result").cloned().ok_or_else(|| {
        StructuredError::new(
            "structured_command_ref_invalid_json",
            "structured_command_execution_result_missing",
            json!({"ref": reference}),
        )
    })
}

fn create_input_record(state: &State, args: &Value) -> Result<Value, StructuredError> {
    let object = args.as_object().cloned().unwrap_or_default();
    let command = object
        .get("command")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| {
            StructuredError::new(
                "structured_command_command_required",
                "structured_command_command_required",
                json!({}),
            )
        })?;
    let id = object
        .get("input_id")
        .and_then(Value::as_str)
        .map(str::to_string)
        .unwrap_or_else(|| unique_id("i"));
    if id.len() < 8
        || !id.chars().all(|character| {
            character.is_ascii_alphanumeric() || character == '_' || character == '-'
        })
    {
        return Err(StructuredError::new(
            "structured_command_invalid_ref_id",
            "structured_command_invalid_ref_id",
            json!({"input_id": id}),
        ));
    }
    let input = json!({
        "command": command,
        "args": object.get("args").and_then(Value::as_array).map(|values| values.iter().map(|value| value.as_str().unwrap_or_default()).collect::<Vec<_>>()).unwrap_or_default(),
        "working_directory": object.get("working_directory"),
        "timeout_ms": object.get("timeout_ms"),
        "wait_for_completion": object.get("wait_for_completion"),
        "test_scope": object.get("test_scope").and_then(Value::as_str).unwrap_or("unknown"),
        "expected_cost": object.get("expected_cost").and_then(Value::as_str).unwrap_or("unknown"),
    });
    let reference = format!("structured_command_input:{id}");
    let record = json!({"schema": "narada.structured_command.input.v0", "ref": reference, "created_at": now_rfc3339(), "sha256": sha256_json(&input), "input": input});
    write_json_record(&input_path(state, &id), &record)?;
    Ok(
        json!({"schema": "narada.structured_command.input_create_result.v0", "status": "created", "input_ref": reference, "sha256": sha256_json(record.get("input").unwrap_or(&Value::Null))}),
    )
}

fn read_input_record(state: &State, reference: &str) -> Result<Value, StructuredError> {
    let id = parse_ref(reference, "input")?;
    let record = read_json_record(&input_path(state, &id))?;
    record.get("input").cloned().ok_or_else(|| {
        StructuredError::new(
            "structured_command_ref_invalid_json",
            "structured_command_input_missing",
            json!({"ref": reference}),
        )
    })
}

fn unique_id(prefix: &str) -> String {
    static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    format!(
        "{prefix}_{}_{}_{}",
        std::process::id(),
        nanos,
        COUNTER.fetch_add(1, Ordering::Relaxed)
    )
}

fn sha256_json(value: &Value) -> String {
    let bytes = serde_json::to_vec(value).unwrap_or_else(|_| b"null".to_vec());
    hex::encode(Sha256::digest(bytes))
}

fn execute(
    state: &State,
    args: &Value,
    cancellation: Option<Arc<AtomicBool>>,
    force_background: bool,
) -> Result<Value, StructuredError> {
    let args_object = args.as_object().cloned().unwrap_or_default();
    let selectors = ["command", "input_ref", "execution_ref"]
        .iter()
        .filter(|name| {
            args_object
                .get(**name)
                .and_then(Value::as_str)
                .is_some_and(|value| !value.is_empty())
        })
        .copied()
        .collect::<Vec<_>>();
    let valid = selectors.len() == 1 && (!force_background || selectors[0] != "execution_ref");
    if !valid {
        return Err(StructuredError::new(
            "structured_command_execution_selector_invalid",
            "structured_command_execution_selector_invalid",
            json!({"provided":selectors,"required":"Supply exactly one of command or input_ref; structured_command_execute also accepts execution_ref."}),
        ));
    }
    if args_object
        .get("execution_ref")
        .and_then(Value::as_str)
        .is_some()
    {
        return show_execution(state, args);
    }
    let effective_args =
        if let Some(reference) = args_object.get("input_ref").and_then(Value::as_str) {
            read_input_record(state, reference)?
                .as_object()
                .cloned()
                .unwrap_or_default()
        } else {
            args_object.clone()
        };
    let command = normalize_command(
        effective_args
            .get("command")
            .and_then(Value::as_str)
            .unwrap_or_default(),
    );
    let command_args = effective_args
        .get("args")
        .and_then(Value::as_array)
        .map(|values| {
            values
                .iter()
                .map(|value| value.as_str().unwrap_or_default().to_string())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let working_directory = effective_args
        .get("working_directory")
        .and_then(Value::as_str)
        .map(|value| resolve_path(value, &state.allowed_roots[0]))
        .unwrap_or_else(|| state.allowed_roots[0].clone());
    let timeout_ms = effective_args
        .get("timeout_ms")
        .and_then(Value::as_u64)
        .unwrap_or(60_000)
        .clamp(1, state.max_timeout_ms);
    let test_scope = effective_args
        .get("test_scope")
        .and_then(Value::as_str)
        .unwrap_or_else(|| infer_test_scope(&command, &command_args));
    let expected_cost = effective_args
        .get("expected_cost")
        .and_then(Value::as_str)
        .unwrap_or_else(|| infer_expected_cost(test_scope));
    let posture = json!({"schema": "narada.structured_command.execution_posture.v0", "test_scope": test_scope, "expected_cost": expected_cost, "source": if args_object.get("test_scope").is_some() || args_object.get("expected_cost").is_some() { "caller_declared" } else { "derived" }});
    let decision = decide(state, &command, &command_args, &working_directory);
    if decision.get("status").and_then(Value::as_str) != Some("allowed") {
        let reasons = decision
            .get("reasons")
            .cloned()
            .unwrap_or_else(|| json!([]));
        return Ok(
            json!({"schema": "narada.structured_command.execution_result.v0", "status": "refused", "decision": decision, "refusal_reasons": reasons, "remediation_hints": decision.get("remediation_hints").cloned().unwrap_or_else(|| json!([])), "mcp_fallbacks": decision.get("mcp_fallbacks").cloned().unwrap_or_else(|| json!([])), "command": command, "args": command_args, "working_directory": working_directory.to_string_lossy(), "execution_posture": posture, "test_scope": test_scope, "expected_cost": expected_cost, "executed": false}),
        );
    }
    let background = force_background
        || args_object
            .get("wait_for_completion")
            .and_then(Value::as_bool)
            == Some(false);
    if background {
        let process_lifetime_ms = effective_args
            .get("durable_process_lifetime_ms")
            .or_else(|| effective_args.get("timeout_ms"))
            .and_then(Value::as_u64)
            .unwrap_or(state.max_timeout_ms)
            .clamp(1, state.max_timeout_ms);
        return start_background(
            state,
            &command,
            &command_args,
            &working_directory,
            process_lifetime_ms,
            posture,
            args_object.get("input_ref").cloned().unwrap_or(Value::Null),
        );
    }
    if timeout_ms > MAX_SYNCHRONOUS_TIMEOUT_MS {
        return Ok(
            json!({"schema": "narada.structured_command.execution_result.v0", "status": "refused", "executed": false, "decision": decision, "refusal_reasons": ["synchronous_timeout_exceeds_reliable_bound"], "remediation_hints": [format!("Use structured_command_start for commands requiring more than {MAX_SYNCHRONOUS_TIMEOUT_MS}ms, then poll structured_command_execution_show.")], "mcp_fallbacks": [], "command": command, "args": command_args, "working_directory": working_directory.to_string_lossy(), "timeout_ms": timeout_ms, "max_synchronous_timeout_ms": MAX_SYNCHRONOUS_TIMEOUT_MS}),
        );
    }
    let started_at = now_rfc3339();
    let result = run_process(
        &command,
        &command_args,
        &working_directory,
        timeout_ms,
        state.max_output_bytes,
        cancellation,
        &state.env,
    );
    let payload = execution_payload(
        &command,
        &command_args,
        &working_directory,
        &started_at,
        timeout_ms,
        posture,
        result,
        "synchronous",
        true,
        args_object.get("input_ref").cloned().unwrap_or(Value::Null),
    );
    audit(state, &payload);
    let reference = create_execution_record(state, &payload)?;
    Ok(page_execution(&payload, &args_object, Some(reference)))
}
