fn dashboard(args: &Map<String, Value>, root: &Path) -> Result<Value, Value> {
    let mode = match args
        .get("mode")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        Some("all_active") => "all_active",
        Some("single_run") => "single_run",
        Some(_) => {
            return Err(error(
                "worker_invalid_dashboard_mode",
                "worker_invalid_dashboard_mode",
            ))
        }
        None if args
            .get("run_id")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .is_some() =>
        {
            "single_run"
        }
        None => "all_active",
    };
    let include_terminal = args
        .get("include_terminal")
        .and_then(Value::as_bool)
        .unwrap_or(mode == "single_run");
    let limit = args
        .get("limit")
        .and_then(Value::as_u64)
        .unwrap_or(25)
        .clamp(1, 200) as usize;
    let mut runs = if mode == "single_run" {
        let id = run_id(args)?;
        vec![compact_run(&read_reconciled_run(root, &id)?)]
    } else {
        let list = runs_list(&json!({"limit":200}).as_object().unwrap(), root)?;
        list.get("runs")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
    };
    if !include_terminal {
        runs.retain(|run| !is_terminal_status(run.get("status").and_then(Value::as_str)));
    }
    runs.truncate(limit);
    let total = runs.len();
    let active = runs
        .iter()
        .filter(|run| !is_terminal_status(run.get("status").and_then(Value::as_str)))
        .count();
    let failed = runs
        .iter()
        .filter(|run| {
            matches!(
                run.get("status").and_then(Value::as_str),
                Some("failed" | "completed_with_errors")
            )
        })
        .count();
    let nodes = runs
        .iter()
        .map(|run| {
            json!({
                "id":run.get("run_id").cloned().unwrap_or(Value::Null),
                "label":run.get("run_id").cloned().unwrap_or(Value::Null),
                "status":run.get("status").cloned().unwrap_or(Value::Null),
                "worker_session_id":run.get("worker_session_id").cloned().unwrap_or(Value::Null),
            })
        })
        .collect::<Vec<_>>();
    let pending = runs.iter().filter(|run| !is_terminal_status(run.get("status").and_then(Value::as_str))).map(|run| json!({
        "gate_id":format!("join:{}", run.get("run_id").and_then(Value::as_str).unwrap_or("")),
        "run_id":run.get("run_id").cloned().unwrap_or(Value::Null),
        "status":"pending",
        "waiting_for":[run.get("run_id").cloned().unwrap_or(Value::Null)],
    })).collect::<Vec<_>>();
    Ok(json!({
        "schema":"narada.worker.dashboard.v1",
        "status":"ok",
        "mode":mode,
        "include_terminal":include_terminal,
        "dashboard":{
            "kind":"read_only_dashboard_descriptor",
            "server":{"started":false,"reason":"mcp_tool_is_request_response; use the listed JSON API tool calls or wrap them in a local HTTP process if a long-lived dashboard is required"},
            "suggested_local_command":Value::Null,
            "api_endpoints":[
                {"path":"mcp://tools/worker_dashboard_describe","method":"tools/call","description":"Read-only compact dashboard payload for one run or all active runs.","arguments":{"mode":"all_active|single_run","run_id":"optional run id","include_terminal":"boolean","limit":"1..200"}},
                {"path":"mcp://tools/worker_runs_list","method":"tools/call","description":"Recent run index with compact status fields.","arguments":{"include_running":true,"include_completed":true,"verbose":false}},
                {"path":"mcp://tools/worker_run_status","method":"tools/call","description":"Full status for one run, including artifact readback and progress.","arguments":{"run_id":"run-*"}},
                {"path":"mcp://resources/worker-artifact","method":"resources/read","description":"Read run artifacts such as events.jsonl and result.json for primary run-root records."}
            ],
            "refresh":{"tool":"worker_dashboard_describe","arguments":{"mode":mode,"include_terminal":include_terminal,"limit":limit}},
        },
        "counts":{"total":total,"active":active,"terminal":total-active,"failed":failed,"runs":total},
        "runs":runs,
        "topology":{"graph_kind":"run_dag","dependency_source":"worker-delegation run records; explicit inter-run dependencies are not currently recorded","nodes":nodes,"edges":[]},
        "steps":[],
        "pending_join_gates":pending,
        "event_stream":[],
        "native_read_only":true
    }))
}

fn is_terminal_status(status: Option<&str>) -> bool {
    matches!(
        status,
        Some("completed" | "completed_with_errors" | "failed" | "cancelled")
    )
}
fn output_show(args: &Map<String, Value>, root: &Path) -> Result<Value, Value> {
    let reference = args
        .get("ref")
        .or_else(|| args.get("output_ref"))
        .and_then(Value::as_str)
        .ok_or_else(|| error("worker_output_ref_required", "worker_output_ref_required"))?;
    let raw = reference
        .strip_prefix("worker-artifact:")
        .ok_or_else(|| error("worker_output_ref_invalid", "worker_output_ref_invalid"))?;
    let (id, name) = raw
        .split_once('/')
        .ok_or_else(|| error("worker_output_ref_invalid", "worker_output_ref_invalid"))?;
    safe_run_id(id)?;
    if name.is_empty()
        || name.len() > 100
        || name.contains('/')
        || name.contains('\\')
        || name.contains("..")
    {
        return Err(error(
            "worker_output_ref_invalid",
            "worker_output_ref_invalid",
        ));
    }
    let path = run_root(root).join(id).join(name);
    let byte_size = fs::metadata(&path)
        .map_err(|_| error("worker_output_not_found", "worker_output_not_found"))?
        .len();
    if byte_size > MAX_FILE_BYTES as u64 {
        return Err(error("worker_output_too_large", "worker_output_too_large"));
    }
    let bytes =
        fs::read(&path).map_err(|_| error("worker_output_not_found", "worker_output_not_found"))?;
    let chars = String::from_utf8_lossy(&bytes).chars().collect::<Vec<_>>();
    let offset = args
        .get("offset")
        .and_then(Value::as_u64)
        .unwrap_or(0)
        .min(chars.len() as u64) as usize;
    let limit = args
        .get("limit")
        .and_then(Value::as_u64)
        .unwrap_or(MAX_FILE_BYTES as u64)
        .min(MAX_FILE_BYTES as u64) as usize;
    let chunk = chars.iter().skip(offset).take(limit).collect::<String>();
    let end = offset + chunk.chars().count();
    Ok(
        json!({"schema":"narada.worker.output_page.v1","status":"ok","ref":reference,"path":path.to_string_lossy(),"byte_size":byte_size,"offset":offset,"limit":limit,"next_offset":if end<chars.len(){json!(end)}else{Value::Null},"output_text":chunk,"output_truncated":end<chars.len(),"native_read_only":true}),
    )
}
fn result_show(args: &Map<String, Value>, root: &Path) -> Result<Value, Value> {
    let id = run_id(args)?;
    let run = read_reconciled_run(root, &id)?;
    let reference = format!("worker-artifact:{id}/last_message.json");
    let mut page_args = args.clone();
    page_args.insert("ref".into(), json!(reference));
    let page = output_show(&page_args, root)?;
    let object = run.as_object().cloned().unwrap_or_default();
    Ok(json!({
        "schema":"narada.worker.result.v1",
        "status":"ok",
        "run_id":id,
        "run_status":object.get("status"),
        "completion_state":object.get("completion_state"),
        "result_ref":reference,
        "result":page.get("output_text"),
        "result_page":page,
        "execution_log":execution_log_refs(&id, object.get("artifacts")),
        "refusals":refusals_value(&object),
        "native_read_only":true
    }))
}

fn bounded_text(text: &[u8], limit: usize) -> String {
    let value = String::from_utf8_lossy(text);
    let chars = value.chars().take(limit).collect::<String>();
    if value.chars().count() > limit { format!("{}…", chars) } else { chars }
}
fn command_run(args: &Map<String, Value>, root: &Path, allowed_roots: &[PathBuf]) -> Result<Value, Value> {
    if args.get("authority").and_then(Value::as_str) != Some("command") {
        return Err(error("worker_command_authority_required", "worker_command_authority_required"));
    }
    let command = required_string(args, "command", "worker_command_required")?;
    if command.chars().any(|c| matches!(c, '&' | ';' | '|' | '>' | '<' | '`' | '$')) {
        return Err(error("worker_command_literal_argv_required", "worker_command_literal_argv_required"));
    }
    let cwd = args.get("cwd").and_then(Value::as_str).map(PathBuf::from).unwrap_or_else(|| root.to_path_buf());
    if !allowed_roots.iter().any(|allowed| is_within(&cwd, allowed)) {
        return Err(error("worker_cwd_outside_allowed_roots", "worker_cwd_outside_allowed_roots"));
    }
    let argv = args.get("args").and_then(Value::as_array).cloned().unwrap_or_default();
    if argv.len() > 64 || argv.iter().any(|value| !value.is_string() || value.as_str().unwrap_or_default().len() > 4096) {
        return Err(error("worker_command_args_invalid", "worker_command_args_invalid"));
    }
    let timeout_ms = args.get("timeout_ms").and_then(Value::as_u64).unwrap_or(10_000).clamp(1, 60_000);
    let stdout_limit = args.get("stdout_limit").and_then(Value::as_u64).unwrap_or(4_096).clamp(1, 65_536) as usize;
    let stderr_limit = args.get("stderr_limit").and_then(Value::as_u64).unwrap_or(4_096).clamp(1, 65_536) as usize;
    let started = Instant::now();
    let mut child = Command::new(command).args(argv.iter().filter_map(Value::as_str)).current_dir(&cwd).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().map_err(|err| json!({"schema":"narada.worker.command.v1","code":"worker_command_launch_failed","message":"worker_command_launch_failed","error":err.to_string(),"execution_verdict":"failed","objective_verdict":"failed"}))?;
    let mut timed_out = false;
    loop {
        if child.try_wait().map_err(|_| error("worker_command_wait_failed", "worker_command_wait_failed"))?.is_some() { break; }
        if started.elapsed() >= Duration::from_millis(timeout_ms) {
            timed_out = true;
            let _ = child.kill();
            break;
        }
        thread::sleep(Duration::from_millis(20));
    }
    let output = child.wait_with_output().map_err(|_| error("worker_command_output_failed", "worker_command_output_failed"))?;
    let exit_code = output.status.code();
    let execution_verdict = if timed_out { "failed" } else { "passed" };
    let objective_verdict = if !timed_out && output.status.success() { "passed" } else { "failed" };
    Ok(json!({
        "schema":"narada.worker.command.v1",
        "status":"ok",
        "command":command,
        "cwd":cwd.to_string_lossy(),
        "exit_code":exit_code,
        "timed_out":timed_out,
        "timeout_ms":timeout_ms,
        "elapsed_ms":started.elapsed().as_millis() as u64,
        "execution_verdict":execution_verdict,
        "objective_verdict":objective_verdict,
        "objective_result":objective_verdict,
        "stdout":bounded_text(&output.stdout, stdout_limit),
        "stderr":bounded_text(&output.stderr, stderr_limit),
        "native_read_only":false
    }))
}

fn affordances() -> Value {
    json!({"schema":"narada.worker.operator_affordances.v1","status":"ok","read_tools":READ_TOOLS.iter().map(|(n,_)|*n).collect::<Vec<_>>(),"mutation_tools":MUTATING_TOOLS,"command_tools":COMMAND_TOOLS.iter().map(|(n,_)|*n).collect::<Vec<_>>(),"native_read_only":true,"execution_authority":"rust"})
}
fn require_current_site_scope(args: &Map<String, Value>) -> Result<(), Value> {
    match args.get("site_scope").and_then(Value::as_str) {
        None | Some("current_site") => Ok(()),
        Some(_) => Err(error(
            "worker_site_scope_invalid",
            "worker_site_scope_invalid: only server-bound current_site is available",
        )),
    }
}
fn compact_text(value: Option<&Value>) -> Value {
    let Some(value) = value else { return Value::Null; };
    if value.is_null() {
        return Value::Null;
    }
    let text = value
        .as_str()
        .map(str::to_string)
        .unwrap_or_else(|| serde_json::to_string(value).unwrap_or_default());
    if text.chars().count() <= 320 {
        return json!(text);
    }
    let prefix = text.chars().take(320).collect::<String>();
    let boundary = prefix
        .char_indices()
        .rev()
        .find(|(_, ch)| ch.is_whitespace())
        .map(|(index, _)| index)
        .unwrap_or(prefix.len());
    json!(format!("{}…", prefix[..boundary].trim_end()))
}
