
fn handle_request(
    state: &State,
    request: &Value,
    cancellation: Option<Arc<AtomicBool>>,
) -> Option<Value> {
    let method = request
        .get("method")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let id = request.get("id").cloned()?;
    let params = request.get("params").unwrap_or(&Value::Null);
    let result = match method {
        "initialize" => Ok(initialize(request)),
        "tools/list" => Ok(json!({"tools": list_tools()})),
        "tools/call" => call_tool(state, params, cancellation),
        "resources/list" => Ok(json!({"resources": []})),
        "resources/read" => Err(StructuredError::new(
            "resource_not_found",
            "resource_not_found",
            json!({}),
        )),
        "prompts/list" => Ok(
            json!({"prompts": [{"name": "structured_command_safe_execution", "title": "Structured Command Safe Execution", "description": "Guidance for argv-only command execution.", "arguments": []}]}),
        ),
        "prompts/get" => prompt_get(params),
        "completion/complete" => {
            Ok(json!({"completion": {"values": [], "total": 0, "hasMore": false}}))
        }
        "logging/setLevel" => Ok(json!({})),
        _ => Err(StructuredError::new(
            "unsupported_mcp_method",
            format!("unsupported_mcp_method:{method}"),
            json!({"method": method}),
        )),
    };
    Some(match result {
        Ok(value) => json!({"jsonrpc": "2.0", "id": id, "result": value}),
        Err(error) => {
            json!({"jsonrpc": "2.0", "id": id, "error": {"code": -32000, "message": error.message, "data": error_diagnostic(&error)}})
        }
    })
}

fn initialize(request: &Value) -> Value {
    json!({
        "protocolVersion": request.get("params").and_then(|params| params.get("protocolVersion")).cloned().unwrap_or(json!(PROTOCOL_VERSION)),
        "capabilities": {"tools": {}, "resources": {}, "prompts": {}, "completions": {}, "logging": {}},
        "serverInfo": {"name": "structured-command-native", "version": "0.1.0"}
    })
}

fn list_tools() -> Vec<Value> {
    vec![
        tool("structured_command_guidance", "Guidance for argv-only structured command execution.", json!({"type": "object", "properties": {"workflow": {"type": "string"}, "tool": {"type": "string"}}, "additionalProperties": false}), true),
        tool("structured_command_execution_policy_inspect", "Inspect the policy governing structured command execution.", json!({"type": "object", "additionalProperties": false}), true),
        tool("structured_command_output_show", "Read a materialized structured-command output ref with offset/limit paging.", json!({"type": "object", "properties": {"ref": {"type": "string"}, "output_ref": {"type": "string"}, "offset": {"type": "integer", "minimum": 0}, "limit": {"type": "integer", "minimum": 1, "maximum": 20000}}, "oneOf": [{"required":["ref"]},{"required":["output_ref"]}], "additionalProperties": false}), true),
        tool("structured_command_execute", "Execute a structured argv command under allowed-root and command policy, or read an existing execution_ref. Supply exactly one of command, input_ref, or execution_ref.", execution_schema(true), false),
        tool("structured_command_start", "Start a detached native command and return an execution_ref immediately. durable_process_lifetime_ms is the explicit kill deadline; observation_timeout_ms never terminates the process.", execution_schema(false), false),
        tool("structured_command_execution_show", "Read one durable structured command execution by execution_ref.", json!({"type": "object", "properties": {"execution_ref": {"type": "string", "minLength": 1}, "stdout_offset": {"type": "integer", "minimum": 0}, "stderr_offset": {"type": "integer", "minimum": 0}, "stdout_limit": {"type": "integer", "minimum": 1, "maximum": 20000}, "stderr_limit": {"type": "integer", "minimum": 1, "maximum": 20000}}, "required": ["execution_ref"], "additionalProperties": false}), true),
        tool("structured_command_execution_stop", "Stop one running structured-command process tree owned by this surface.", json!({"type":"object","properties":{"execution_ref":{"type":"string","minLength":1},"reason":{"type":"string"}},"required":["execution_ref"],"additionalProperties":false}), false),
        tool("structured_command_execution_restart", "Stop one running owned execution when necessary and start a replacement from its recorded argv and working directory.", json!({"type":"object","properties":{"execution_ref":{"type":"string","minLength":1},"durable_process_lifetime_ms":{"type":"integer","minimum":1}},"required":["execution_ref"],"additionalProperties":false}), false),
        tool("structured_command_powershell_parse_check", "Parse-check an allowed-root PowerShell script without admitting arbitrary execution.", json!({"type": "object", "properties": {"path": {"type": "string"}, "working_directory": {"type": "string"}, "timeout_ms": {"type": "integer"}}, "required": ["path"], "additionalProperties": false}), true),
        tool("structured_command_input_create", "Create a scoped structured command input ref.", json!({"type": "object", "properties": {"input_id": {"type": "string"}, "command": {"type": "string", "minLength": 1}, "args": {"type": "array", "items": {"type": "string"}}, "working_directory": {"type": "string"}, "timeout_ms": {"type": "integer", "minimum": 1}, "wait_for_completion": {"type": "boolean"}, "test_scope": {"type": "string"}, "expected_cost": {"type": "string"}}, "required": ["command"], "additionalProperties": false}), false),
        tool("structured_command_elevated_window_execute", "On Windows, launch a policy-approved command in a visible elevated UAC window. Execution requires confirm_elevation=true.", json!({"type": "object", "properties": {"command": {"type": "string", "minLength": 1}, "args": {"type": "array", "items": {"type": "string"}}, "working_directory": {"type": "string", "minLength": 1}, "confirm_elevation": {"type": "boolean"}, "wait": {"type": "boolean"}, "dry_run": {"type": "boolean"}, "timeout_ms":{"type":"integer","minimum":1}}, "required": ["command", "working_directory"], "additionalProperties": false}), false),
    ]
}

fn execution_schema(allow_execution_ref: bool) -> Value {
    let mut properties = json!({"input_ref": {"type": "string", "minLength": 1}, "command": {"type": "string", "minLength": 1}, "args": {"type": "array", "items": {"type": "string"}}, "working_directory": {"type": "string"}, "timeout_ms": {"type": "integer", "minimum": 1}, "wait_for_completion": {"type": "boolean"}, "test_scope": {"type": "string"}, "expected_cost": {"type": "string"}, "stdout_offset": {"type": "integer", "minimum": 0}, "stderr_offset": {"type": "integer", "minimum": 0}, "stdout_limit": {"type": "integer", "minimum": 1, "maximum": 20000}, "stderr_limit": {"type": "integer", "minimum": 1, "maximum": 20000}}).as_object().unwrap().clone();
    properties.insert("observation_timeout_ms".into(), json!({"type":"integer","minimum":1}));
    properties.insert("durable_process_lifetime_ms".into(), json!({"type":"integer","minimum":1}));
    if allow_execution_ref {
        properties.insert(
            "execution_ref".to_string(),
            json!({"type": "string", "minLength": 1}),
        );
    }
    let mut alternatives = vec![
        json!({"required": ["command"]}),
        json!({"required": ["input_ref"]}),
    ];
    if allow_execution_ref {
        alternatives.push(json!({"required": ["execution_ref"]}));
    }
    json!({"type": "object", "properties": properties, "oneOf": alternatives, "additionalProperties": false})
}

fn tool(name: &str, description: &str, schema: Value, read_only: bool) -> Value {
    json!({"name": name, "description": description, "inputSchema": schema, "annotations": {"title": name, "canonicalName": name, "readOnlyHint": read_only, "destructiveHint": !read_only, "idempotentHint": read_only, "openWorldHint": false}, "outputSchema": {"type": "object", "additionalProperties": true}})
}

fn prompt_get(params: &Value) -> Result<Value, StructuredError> {
    let name = params
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if name != "structured_command_safe_execution" {
        return Err(StructuredError::new(
            "unknown_prompt",
            format!("unknown_prompt:{name}"),
            json!({"name": name}),
        ));
    }
    Ok(
        json!({"description": "Guidance for argv-only command execution.", "messages": [{"role": "user", "content": {"type": "text", "text": "Use structured_command_execute with explicit argv arrays only. Inspect policy before relying on command availability."}}]}),
    )
}

fn call_tool(
    state: &State,
    params: &Value,
    cancellation: Option<Arc<AtomicBool>>,
) -> Result<Value, StructuredError> {
    let name = params.get("name").and_then(Value::as_str).ok_or_else(|| {
        StructuredError::new(
            "tools_call_requires_name",
            "tools_call_requires_name",
            json!({}),
        )
    })?;
    let args = params.get("arguments").unwrap_or(&Value::Null);
    let payload = match name {
        "structured_command_guidance" => guidance(args),
        "structured_command_execution_policy_inspect" => Ok(policy_payload(state)),
        "structured_command_execute" => execute(state, args, cancellation, false),
        "structured_command_execution_show" => show_execution(state, args),
        "structured_command_execution_stop" => stop_execution(state, args),
        "structured_command_execution_restart" => restart_execution(state, args),
        "structured_command_input_create" => create_input_record(state, args),
        "structured_command_output_show" => output_show(state, args),
        "structured_command_powershell_parse_check" => {
            powershell_parse_check(state, args, cancellation)
        }
        "structured_command_start" => execute(state, args, cancellation, true),
        "structured_command_elevated_window_execute" => elevated_execute(state, args, cancellation),
        _ => Err(StructuredError::new(
            "structured_command_unknown_tool",
            format!("structured_command_unknown_tool:{name}"),
            json!({"tool_name": name}),
        )),
    }?;
    tool_result(state, payload, name)
}

fn guidance(args: &Value) -> Result<Value, StructuredError> {
    Ok(
        json!({"schema": "narada.mcp_surface.guidance.v0", "status": "ok", "surface_id": "structured-command", "guidance_tool": "structured_command_guidance", "purpose": "Bounded argv-only process execution under explicit command and root policy.", "requested": {"workflow": args.get("workflow"), "tool": args.get("tool")}, "safety": ["Inspect policy before execution.", "Pass command arguments as an array; no shell interpolation is performed.", "Retain structuredContent as the authoritative execution record.", "Use structured_command_start for long work; it returns a durable execution_ref immediately. Poll structured_command_execution_show for terminal status."]}),
    )
}

fn powershell_parse_check(
    state: &State,
    args: &Value,
    cancellation: Option<Arc<AtomicBool>>,
) -> Result<Value, StructuredError> {
    let object = args.as_object().cloned().unwrap_or_default();
    let path_value = object
        .get("path")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let path = resolve_path(path_value, &state.allowed_roots[0]);
    if !path
        .to_string_lossy()
        .to_ascii_lowercase()
        .ends_with(".ps1")
    {
        return Err(StructuredError::new(
            "structured_command_powershell_parse_check_requires_ps1",
            "structured_command_powershell_parse_check_requires_ps1",
            json!({"path": path_value}),
        ));
    }
    if !inside_any_root(&path, &state.allowed_roots) {
        return Err(StructuredError::new(
            "structured_command_powershell_parse_check_path_outside_allowed_roots",
            "structured_command_powershell_parse_check_path_outside_allowed_roots",
            json!({"path": path.to_string_lossy()}),
        ));
    }
    if !path.is_file() {
        return Err(StructuredError::new(
            "structured_command_powershell_parse_check_file_not_found",
            "structured_command_powershell_parse_check_file_not_found",
            json!({"path": path.to_string_lossy()}),
        ));
    }
    let cwd = object
        .get("working_directory")
        .and_then(Value::as_str)
        .map(|value| resolve_path(value, &state.allowed_roots[0]))
        .unwrap_or_else(|| {
            path.parent()
                .unwrap_or(&state.allowed_roots[0])
                .to_path_buf()
        });
    if !inside_any_root(&cwd, &state.allowed_roots) {
        return Err(StructuredError::new(
            "structured_command_powershell_parse_check_cwd_outside_allowed_roots",
            "structured_command_powershell_parse_check_cwd_outside_allowed_roots",
            json!({"working_directory": cwd.to_string_lossy()}),
        ));
    }
    let timeout = object
        .get("timeout_ms")
        .and_then(Value::as_u64)
        .unwrap_or(30_000)
        .clamp(1, state.max_timeout_ms);
    let script = format!("$ErrorActionPreference = 'Stop'; $tokens = $null; $errors = $null; [System.Management.Automation.Language.Parser]::ParseFile('{}', [ref]$tokens, [ref]$errors) > $null; if ($errors.Count -gt 0) {{ $errors | ForEach-Object {{ Write-Error ($_.ToString()) }}; exit 1 }}; Write-Output 'parse_ok'", ps_single_quote(&path.to_string_lossy()));
    let result = run_process(
        "pwsh",
        &["-NoProfile".to_string(), "-Command".to_string(), script],
        &cwd,
        timeout,
        state.max_output_bytes,
        cancellation,
        &state.env,
    );
    Ok(
        json!({"schema": "narada.structured_command.powershell_parse_check.v0", "status": if result.cancelled { "cancelled" } else if result.timed_out { "timed_out" } else if result.exit_code == Some(0) { "ok" } else { "failed" }, "path": path.to_string_lossy(), "working_directory": cwd.to_string_lossy(), "timeout_ms": timeout, "exit_code": result.exit_code, "stdout": result.stdout, "stderr": result.stderr, "stdout_truncated": result.stdout_truncated, "stderr_truncated": result.stderr_truncated, "timed_out": result.timed_out, "cancelled": result.cancelled, "arbitrary_command_execution_admitted": false, "parser_api": "System.Management.Automation.Language.Parser.ParseFile"}),
    )
}
