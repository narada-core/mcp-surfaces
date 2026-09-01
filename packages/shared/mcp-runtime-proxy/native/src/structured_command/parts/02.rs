
pub fn run_background(args: &[String]) -> Result<(), String> {
    let request_path = argument_value(args, "--request")
        .ok_or_else(|| "structured_command_background_request_required".to_string())?;
    let expected_sha256 = argument_value(args, "--sha256")
        .ok_or_else(|| "structured_command_background_sha256_required".to_string())?;
    let bytes = fs::read(&request_path)
        .map_err(|error| format!("structured_command_background_request_read_failed:{error}"))?;
    if hex::encode(Sha256::digest(&bytes)) != expected_sha256 {
        return Err("structured_command_background_request_integrity_mismatch".to_string());
    }
    let request: Value = serde_json::from_slice(&bytes)
        .map_err(|error| format!("structured_command_background_request_invalid:{error}"))?;
    if request.get("schema").and_then(Value::as_str)
        != Some("narada.structured_command.background_request.v1")
    {
        return Err("structured_command_background_request_schema_invalid".to_string());
    }
    let command = request
        .get("command")
        .and_then(Value::as_str)
        .ok_or_else(|| "structured_command_background_command_required".to_string())?;
    let command_args = value_strings(request.get("args"));
    let cwd = PathBuf::from(
        request
            .get("working_directory")
            .and_then(Value::as_str)
            .ok_or_else(|| "structured_command_background_cwd_required".to_string())?,
    );
    let timeout_ms = request
        .get("timeout_ms")
        .and_then(Value::as_u64)
        .ok_or_else(|| "structured_command_background_timeout_required".to_string())?;
    let max_output_bytes = request
        .get("max_output_bytes")
        .and_then(Value::as_u64)
        .unwrap_or(DEFAULT_MAX_OUTPUT_BYTES as u64)
        .clamp(1, MAX_OUTPUT_BYTES as u64) as usize;
    let storage_root = PathBuf::from(
        request
            .get("storage_root")
            .and_then(Value::as_str)
            .ok_or_else(|| "structured_command_background_storage_root_required".to_string())?,
    );
    let execution_ref = request
        .get("execution_ref")
        .and_then(Value::as_str)
        .ok_or_else(|| "structured_command_background_execution_ref_required".to_string())?;
    let state = State {
        allowed_roots: vec![cwd.clone()],
        allowed_commands: vec![],
        allowed_prefixes: vec![],
        blocked_commands: vec![],
        max_timeout_ms: timeout_ms,
        max_output_bytes,
        audit_log_dir: request
            .get("audit_log_dir")
            .and_then(Value::as_str)
            .map(PathBuf::from),
        site_root: cwd.clone(),
        storage_root,
        env: execution_environment(),
    };
    let result = run_process(
        command,
        &command_args,
        &cwd,
        timeout_ms,
        max_output_bytes,
        None,
        &state.env,
    );
    let payload = execution_payload(
        command,
        &command_args,
        &cwd,
        request
            .get("started_at")
            .and_then(Value::as_str)
            .unwrap_or_default(),
        timeout_ms,
        request
            .get("execution_posture")
            .cloned()
            .unwrap_or_else(|| json!({})),
        result,
        "background",
        false,
        request.get("input_ref").cloned().unwrap_or(Value::Null),
    );
    audit(&state, &payload);
    update_execution_record(&state, execution_ref, &payload).map_err(|error| error.message)?;
    let _ = fs::remove_file(request_path);
    Ok(())
}

fn argument_value(args: &[String], name: &str) -> Option<String> {
    args.windows(2)
        .find(|pair| pair[0] == name)
        .map(|pair| pair[1].clone())
}

enum Event {
    Request(Value, bool),
    Response(Value, bool, String),
    InputClosed,
}

fn value_key(value: &Value) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| "null".to_string())
}

fn parse_state(args: &[String]) -> Result<State, String> {
    let mut roots = Vec::new();
    let mut allowed_commands = DEFAULT_ALLOWED_COMMANDS
        .iter()
        .map(|value| (*value).to_string())
        .collect::<Vec<_>>();
    let mut allowed_prefixes = DEFAULT_ALLOWED_PREFIXES
        .iter()
        .map(|prefix| prefix.iter().map(|part| (*part).to_string()).collect())
        .collect::<Vec<Vec<String>>>();
    let mut blocked_commands = DEFAULT_BLOCKED_COMMANDS
        .iter()
        .map(|value| (*value).to_string())
        .collect::<Vec<_>>();
    let mut max_timeout_ms = DEFAULT_MAX_TIMEOUT_MS;
    let mut max_output_bytes = DEFAULT_MAX_OUTPUT_BYTES;
    let mut audit_log_dir = None;
    let mut site_root = None;
    let mut storage_root = None;
    let mut index = 0;
    while index < args.len() {
        let flag = args[index].as_str();
        let needs_value = |index: &mut usize, name: &str| -> Result<String, String> {
            *index += 1;
            args.get(*index)
                .cloned()
                .ok_or_else(|| format!("structured_command_{name}_required"))
        };
        match flag {
            "--allowed-root" => roots.push(needs_value(&mut index, "allowed_root")?),
            "--allow-command" => allowed_commands
                .push(needs_value(&mut index, "allow_command")?.to_ascii_lowercase()),
            "--allow-prefix" => {
                let value = needs_value(&mut index, "allow_prefix")?;
                let prefix = value
                    .split_whitespace()
                    .map(|part| part.to_ascii_lowercase())
                    .collect::<Vec<_>>();
                if prefix.is_empty() {
                    return Err("structured_command_allow_prefix_must_not_be_empty".to_string());
                }
                allowed_prefixes.push(prefix);
            }
            "--blocked-command" => blocked_commands
                .push(needs_value(&mut index, "blocked_command")?.to_ascii_lowercase()),
            "--max-timeout-ms" => {
                max_timeout_ms = parse_bounded_u64(
                    &needs_value(&mut index, "max_timeout_ms")?,
                    1,
                    3_600_000,
                    DEFAULT_MAX_TIMEOUT_MS,
                    "max_timeout_ms",
                )?
            }
            "--max-output-bytes" => {
                max_output_bytes = parse_bounded_usize(
                    &needs_value(&mut index, "max_output_bytes")?,
                    1,
                    MAX_OUTPUT_BYTES,
                    DEFAULT_MAX_OUTPUT_BYTES,
                    "max_output_bytes",
                )?
            }
            "--audit-log-dir" => audit_log_dir = Some(needs_value(&mut index, "audit_log_dir")?),
            "--site-root" => site_root = Some(needs_value(&mut index, "site_root")?),
            "--storage-root" => storage_root = Some(needs_value(&mut index, "storage_root")?),
            "--help" => return Err("structured_command_help".to_string()),
            other => return Err(format!("structured_command_unknown_argument:{other}")),
        }
        index += 1;
    }
    if roots.is_empty() {
        return Err("structured_command_mcp_requires_at_least_one_allowed_root".to_string());
    }
    let mut allowed_roots = roots
        .into_iter()
        .map(|root| absolute(PathBuf::from(root)))
        .collect::<Vec<_>>();
    let site_root =
        absolute(PathBuf::from(site_root.unwrap_or_else(|| {
            allowed_roots[0].to_string_lossy().to_string()
        })));
    for root in parse_site_extra_allowed_roots(&site_root) {
        let root = absolute(PathBuf::from(root));
        if !allowed_roots.iter().any(|candidate| candidate == &root) {
            allowed_roots.push(root);
        }
    }
    let storage_root =
        absolute(PathBuf::from(storage_root.unwrap_or_else(|| {
            allowed_roots[0].to_string_lossy().to_string()
        })));
    Ok(State {
        allowed_roots,
        allowed_commands: dedupe(allowed_commands),
        allowed_prefixes,
        blocked_commands: dedupe(blocked_commands),
        max_timeout_ms,
        max_output_bytes,
        audit_log_dir: audit_log_dir.map(|path| absolute(PathBuf::from(path))),
        site_root,
        storage_root,
        env: execution_environment(),
    })
}
