fn worker_resume(
    args: &Map<String, Value>,
    root: &Path,
    allowed_roots: &[PathBuf],
) -> Result<Value, Value> {
    let session =
        required_string(args, "worker_session_id", "worker_session_id_required")?.to_string();
    worker_run(args, root, allowed_roots, Some(session), "worker_resume")
}
fn worker_run_batch(
    args: &Map<String, Value>,
    root: &Path,
    allowed_roots: &[PathBuf],
) -> Result<Value, Value> {
    let requests = args
        .get("requests")
        .and_then(Value::as_array)
        .filter(|v| !v.is_empty())
        .ok_or_else(|| {
            error(
                "worker_run_batch_requests_required",
                "worker_run_batch_requests_required",
            )
        })?;
    if requests.len() > 50 {
        return Err(error(
            "worker_run_batch_too_large",
            "worker_run_batch_too_large",
        ));
    }
    let started = now();
    let mut runs = Vec::new();
    let mut failures = Vec::new();
    for (index, item) in requests.iter().enumerate() {
        match item
            .as_object()
            .ok_or_else(|| {
                error(
                    "worker_run_batch_item_invalid",
                    "worker_run_batch_item_invalid",
                )
            })
            .and_then(|v| worker_run(v, root, allowed_roots, None, "worker_run_batch"))
        {
            Ok(run) => {
                runs.push(json!({"index":index,"run_id":run["run_id"],"status":run["status"]}))
            }
            Err(err) => failures.push(json!({"index":index,"error":err})),
        }
    }
    Ok(
        json!({"schema":"narada.worker.run_batch.v1","status":if failures.is_empty(){"ok"}else{"completed_with_errors"},"requested_count":requests.len(),"started_count":runs.len(),"failed_count":failures.len(),"run_ids":runs.iter().map(|v|v["run_id"].clone()).collect::<Vec<_>>(),"runs":runs,"failures":failures,"timing":{"started_at":started,"finished_at":now()}}),
    )
}
fn worker_run_reap(args: &Map<String, Value>, root: &Path) -> Result<Value, Value> {
    let id = run_id(args)?;
    let reason = required_string(args, "reason", "worker_run_reap_reason_required")?;
    let path = run_path(root, &id)?;
    let mut run = read_json(&path)?;
    if is_terminal_status(run.get("status").and_then(Value::as_str)) {
        return Ok(
            json!({"schema":"narada.worker.run_reap.v1","status":"already_terminal","run_id":id,"reaped":false,"run":run}),
        );
    }
    if args.get("force").and_then(Value::as_bool) != Some(true) {
        return Err(error(
            "worker_run_reap_refused_active_run",
            "worker_run_reap_refused_active_run",
        ));
    }
    run["status"] = json!("cancelled");
    run["completion_state"] = json!("partial");
    run["error"] = json!(format!("worker_run_reaped:{reason}"));
    run["timing"]["finished_at"] = json!(now());
    run["reaped"] = json!({"reason":reason,"force":true,"at":now()});
    write_json_atomic(&path, &run)?;
    Ok(
        json!({"schema":"narada.worker.run_reap.v1","status":"reaped","run_id":id,"reaped":true,"run":run}),
    )
}
fn repair_mojibake(text: &str) -> String {
    text.replace("Â·", "·")
        .replace("â€“", "–")
        .replace("â€”", "—")
        .replace("â€œ", "“")
        .replace("â€\u{009d}", "”")
        .replace("â€˜", "‘")
        .replace("â€™", "’")
        .replace("â€¦", "…")
        .replace("Â ", " ")
}

fn timeout_failure(run_id: &str, max_run_ms: u64, elapsed_ms: u64) -> Value {
    json!({
        "schema":"narada.worker.failure.v1",
        "code":"worker_runtime_timed_out",
        "run_id":run_id,
        "max_run_ms":max_run_ms,
        "elapsed_ms":elapsed_ms,
        "remediation":"Increase constraints.max_run_ms or inspect the worker runtime before retrying."
    })
}
fn queue_timeout_failure(run_id: &str, queue_timeout_ms: u64, elapsed_ms: u64) -> Value {
    json!({"schema":"narada.worker.failure.v1","code":"provider_queue_timed_out","run_id":run_id,"queue_timeout_ms":queue_timeout_ms,"elapsed_ms":elapsed_ms,"remediation":"Retry after provider capacity is available or increase constraints.queue_timeout_ms."})
}

fn event_text(event: &Value) -> Option<String> {
    for key in ["content", "message", "text", "summary"] {
        if let Some(value) = event
            .get(key)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|v| !v.is_empty())
        {
            return Some(repair_mojibake(value));
        }
    }
    event
        .get("message")
        .and_then(Value::as_object)
        .and_then(|message| {
            ["content", "text", "summary"].into_iter().find_map(|key| {
                message
                    .get(key)
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|v| !v.is_empty())
                    .map(repair_mojibake)
            })
        })
}
fn phase_for_event(kind: &str) -> &'static str {
    match kind {
        "assistant_message" | "turn_complete" | "carrier_turn_completed" => "formatting_output",
        "tool_call" | "tool_use" | "command_started" | "command_finished" => "executing_command",
        "reasoning" | "thinking" | "provider_reasoning" => "reasoning",
        "session_started" | "provider_request_started" | "provider_request" => "awaiting_provider",
        "error" | "turn_failed" | "carrier_turn_failed" | "carrier_turn_blocked" | "session_control_rejected" => "failed",
        _ => "awaiting_provider",
    }
}
fn update_run_progress(path: &Path, phase: &str, broker_generation: Option<&str>) {
    if let Ok(mut run) = read_json(path) {
        if run.get("status").and_then(Value::as_str) == Some("running") {
            run["phase"] = json!(phase);
            run["heartbeat_at"] = json!(now());
            if let Some(generation) = broker_generation {
                run["broker_generation"] = json!(generation);
            }
            let _ = write_json_atomic(path, &run);
        }
    }
}
fn update_provider_admission_progress(path: &Path, event: &Value, admitted_at: Option<&str>) {
    if let Ok(mut run) = read_json(path) {
        if run.get("status").and_then(Value::as_str) != Some("running") { return; }
        if let Some(position) = event.get("queue_position") { run["provider_queue"]["position"] = position.clone(); }
        if let Some(capacity) = event.get("capacity") { run["provider_queue"]["capacity"] = capacity.clone(); }
        if let Some(admitted_at) = admitted_at {
            run["timing"]["admitted_at"] = json!(admitted_at);
            run["provider_queue"]["position"] = Value::Null;
        }
        run["heartbeat_at"] = json!(now());
        let _ = write_json_atomic(path, &run);
    }
}
fn refusal_records(path: &Path) -> Value {
    let Ok(file) = fs::File::open(path) else { return json!([]); };
    let mut records = Vec::new();
    for line in BufReader::new(file).lines().take(256).flatten() {
        if line.len() > 16_384 { continue; }
        let Ok(event) = serde_json::from_str::<Value>(&line) else { continue; };
        let kind = event.get("event").or_else(|| event.get("type")).and_then(Value::as_str).unwrap_or_default();
        let schema = event.get("schema").and_then(Value::as_str).unwrap_or_default();
        if kind.contains("refusal") || schema.contains("refusal") {
            records.push(json!({
                "event":kind,
                "tool":event.get("tool"),
                "operation":event.get("operation"),
                "target_path":event.get("target_path"),
                "actual_refusal":event.get("actual_refusal").or_else(||event.get("message"))
            }));
            if records.len() >= 32 { break; }
        }
    }
    json!(records)
}
