fn complete_native_run(
    runtime: PathBuf,
    cwd: PathBuf,
    site_root: PathBuf,
    dir: PathBuf,
    id: String,
    runtime_session: String,
    session: String,
    resume_session: Option<String>,
    authority: String,
    cognition: String,
    mut resolved_invocation: Value,
    plan_ref: String,
    provider_mode: String,
    provider_model: String,
    reasoning_effort: Option<String>,
    provider_binding_path: Option<PathBuf>,
    codex_broker: Option<crate::codex_app_server_broker::BrokerBinding>,
    codex_transport: String,
    allowed_roots: Vec<PathBuf>,
    max_run_ms: u64,
    queue_timeout_ms: u64,
    task_label: String,
    started_at: String,
    prompt: String,
) {
    let result_path = dir.join("result.json");
    let events_path = dir.join("events.jsonl");
    let diagnostic_path = dir.join("diagnostic.log");
    let started = std::time::Instant::now();
    let mut command = Command::new(&runtime);
    command
        .args([
            "--raw-jsonl",
            "--authority",
            &authority,
            "--session",
            &runtime_session,
        ])
        .current_dir(&cwd)
        .env("NARADA_SITE_ROOT", &site_root)
        .env("NARADA_WORKSPACE_ROOT", &cwd)
        .env("NARADA_CARRIER_SESSION_ID", &runtime_session)
        .env("NARADA_INTELLIGENCE_PLAN_REF", &plan_ref)
        .env("NARADA_NATIVE_PROVIDER_MODE", &provider_mode)
        .env("NARADA_NATIVE_PROVIDER_TIMEOUT_MS", max_run_ms.to_string())
        .env("NARADA_NATIVE_PROVIDER_QUEUE_TIMEOUT_MS", queue_timeout_ms.to_string())
        .env(
            "NARADA_WORKER_CAPABILITY_JSON",
            serde_json::to_string(&capability_snapshot(
                &authority,
                &cwd,
                &allowed_roots,
                None,
            ))
            .unwrap_or_else(|_| "{}".to_string()),
        )
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(binding_path) = provider_binding_path {
        command.env("NARADA_NATIVE_PROVIDER_BINDING_PATH", binding_path);
    }
    if let Some(broker) = codex_broker {
        command
            .env("NARADA_NATIVE_CODEX_TRANSPORT", "codex-app-server")
            .env("NARADA_NATIVE_CODEX_BROKER_ENDPOINT", broker.endpoint)
            .env("NARADA_NATIVE_CODEX_BROKER_CAPABILITY", broker.capability)
            .env("NARADA_NATIVE_CODEX_BROKER_GENERATION", broker.broker_generation);
    }
    if provider_mode == "codex-subscription" {
        command.env("NARADA_NATIVE_CODEX_TRANSPORT", codex_transport);
    }
    command.env("NARADA_NATIVE_CODEX_MODEL", provider_model);
    if let Some(reasoning_effort) = reasoning_effort {
        command.env("NARADA_NATIVE_CODEX_REASONING_EFFORT", reasoning_effort);
    }
    if let Some(codex) = codex_command() {
        command.env("NARADA_NATIVE_CODEX_COMMAND", codex);
    }
    if let Some(resume_session) = resume_session {
        command.env("NARADA_NATIVE_CODEX_RESUME_SESSION_ID", resume_session);
    }
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(err) => {
            let failed = json!({"schema":"narada.worker.run.v1","run_id":id,"task_label":task_label,"status":"failed","completion_state":"absent","phase":"failed","heartbeat_at":now(),"runtime":"narada-agent-runtime-server","authority":authority,"cognition":cognition,"resolved_invocation":resolved_invocation,"worker_session_id":session,"summary":null,"result":null,"execution_log":{"events_ref":artifact_ref(&id,"events.jsonl"),"diagnostic_ref":artifact_ref(&id,"diagnostic.log")},"refusals":[],"error":format!("worker_launch_failed:{err}"),"timing":{"started_at":started_at,"finished_at":now(),"duration_ms":0}});
            let _ = write_json_atomic(&result_path, &failed);
            return;
        }
    };
    if let Ok(mut running) = read_json(&result_path) {
        running["pid"] = json!(child.id());
        running["phase"] = json!("awaiting_provider");
        running["heartbeat_at"] = json!(now());
        let _ = write_json_atomic(&result_path, &running);
    }
    update_run_progress(
        &result_path,
        "awaiting_provider",
        resolved_invocation
            .get("provider_broker_generation")
            .and_then(Value::as_str),
    );
    let stderr = child.stderr.take();
    let diagnostics = diagnostic_path.clone();
    thread::spawn(move || {
        if let Some(mut source) = stderr {
            if let Ok(mut target) = fs::File::create(diagnostics) {
                let _ = std::io::copy(&mut source, &mut target);
            }
        }
    });
    if let Some(mut stdin) = child.stdin.take() {
        let frame = json!({"id":format!("worker-conversation-{id}"),"method":"session.submit","params":{"content":prompt,"source":"programmatic_worker","source_id":"worker-delegation-mcp"}});
        let _ = writeln!(stdin, "{frame}");
        let _ = stdin.flush();
        let mut events = fs::File::create(&events_path).ok();
        let mut assistant = None;
        let mut provider_session = None;
        let mut provider_host_generation = None;
        let mut runtime_error = None;
        let mut failure = Value::Null;
        let mut turn_completed = false;
        let mut close_sent = false;
        let mut admitted_at: Option<std::time::Instant> = None;
        let mut admitted_at_text: Option<String> = None;
        if let Some(stdout) = child.stdout.take() {
            let (line_tx, line_rx) = mpsc::channel();
            thread::spawn(move || {
                for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                    if line_tx.send(line).is_err() {
                        break;
                    }
                }
            });
            loop {
                let execution_timed_out = admitted_at.as_ref().is_some_and(|admitted| admitted.elapsed() >= Duration::from_millis(max_run_ms));
                let queue_timed_out = admitted_at.is_none() && started.elapsed() >= Duration::from_millis(queue_timeout_ms);
                if execution_timed_out || queue_timed_out {
                    let elapsed_ms = admitted_at.as_ref().map(|admitted| admitted.elapsed().as_millis() as u64).unwrap_or_else(|| started.elapsed().as_millis() as u64);
                    let (event_name, error_text) = if queue_timed_out {
                        failure = queue_timeout_failure(&id, queue_timeout_ms, elapsed_ms);
                        ("provider_queue_timed_out", format!("provider_queue_timed_out:queue_timeout_ms={queue_timeout_ms}:elapsed_ms={elapsed_ms}"))
                    } else {
                        failure = timeout_failure(&id, max_run_ms, elapsed_ms);
                        ("worker_runtime_timed_out", format!("worker_runtime_timed_out:max_run_ms={max_run_ms}:elapsed_ms={elapsed_ms}"))
                    };
                    runtime_error = Some(error_text);
                    if let Some(file) = events.as_mut() {
                        let _ = writeln!(
                            file,
                            "{}",
                            json!({"schema":"narada.worker.event.v1","event":event_name,"run_id":id,"elapsed_ms":elapsed_ms,"max_run_ms":max_run_ms,"queue_timeout_ms":queue_timeout_ms,"failure":failure})
                        );
                    }
                    let _ = child.kill();
                    update_run_progress(&result_path, "failed", provider_host_generation.as_deref());
                    break;
                }
                if read_json(&result_path)
                    .ok()
                    .and_then(|v| v.get("status").and_then(Value::as_str).map(str::to_string))
                    .as_deref()
                    == Some("cancelled")
                {
                    let _ = child.kill();
                    let _ = child.wait();
                    return;
                }
                let line = match line_rx.recv_timeout(Duration::from_millis(50)) {
                    Ok(line) => line,
                    Err(mpsc::RecvTimeoutError::Timeout) => continue,
                    Err(mpsc::RecvTimeoutError::Disconnected) => break,
                };
                if let Some(file) = events.as_mut() {
                    let _ = writeln!(file, "{line}");
                }
                let Ok(event) = serde_json::from_str::<Value>(&line) else {
                    continue;
                };
                let kind = event
                    .get("event")
                    .or_else(|| event.get("type"))
                    .and_then(Value::as_str)
                    .unwrap_or("");
                let provider_state = event.get("next_state").or_else(|| event.get("invocation_state")).and_then(Value::as_str);
                let phase = match provider_state {
                    Some("queued_for_provider") => "queued_for_provider",
                    Some("admitted") => {
                        if admitted_at.is_none() { admitted_at = Some(std::time::Instant::now()); admitted_at_text = Some(now()); }
                        "provider_executing"
                    }
                    _ => phase_for_event(kind),
                };
                update_run_progress(&result_path, phase, provider_host_generation.as_deref());
                if kind == "provider_invocation_state_transition" {
                    update_provider_admission_progress(&result_path, &event, admitted_at_text.as_deref());
                }
                if kind == "assistant_message" {
                    assistant = event_text(&event);
                }
                if let Some(value) = event
                    .get("provider_session_id")
                    .and_then(Value::as_str)
                    .filter(|value| !value.trim().is_empty())
                {
                    provider_session = Some(value.to_string());
                }
                if let Some(value) = event
                    .get("provider_host_generation")
                    .and_then(Value::as_str)
                    .filter(|value| !value.trim().is_empty())
                {
                    provider_host_generation = Some(value.to_string());
                }
                if matches!(
                    kind,
                    "error"
                        | "turn_failed"
                        | "carrier_turn_failed"
                        | "carrier_turn_blocked"
                        | "session_control_rejected"
                ) {
                    runtime_error = event_text(&event).or_else(|| Some(kind.into()));
                }
                if matches!(
                    kind,
                    "turn_complete"
                        | "carrier_turn_completed"
                        | "turn_failed"
                        | "carrier_turn_failed"
                        | "carrier_turn_blocked"
                ) && !close_sent
                {
                    turn_completed = matches!(kind, "turn_complete" | "carrier_turn_completed");
                    close_sent = true;
                    let close = json!({"id":format!("worker-close-{id}"),"method":"session.close","params":{}});
                    let _ = writeln!(stdin, "{close}");
                    let _ = stdin.flush();
                }
                if kind == "session_closed" {
                    break;
                }
            }
        }
        drop(stdin);
        let status = child.wait().ok();
        let finished = now();
        let successful = status.as_ref().is_some_and(|v| v.success())
            && assistant.is_some()
            && runtime_error.is_none()
            && turn_completed;
        if let Some(message) = assistant.as_ref() {
            let _ = write_json_atomic(
                &dir.join("last_message.json"),
                &json!({"result":message,"summary":message,"deliverables":[],"open_questions":[],"next_actions":[]}),
            );
        }
        let snapshot = read_json(&dir.join("request.json"))
            .ok()
            .and_then(|request| request.get("capability_snapshot").cloned())
            .unwrap_or(Value::Null);
        let elapsed_ms = started.elapsed().as_millis() as u64;
        let queue_ms = admitted_at.as_ref().map(|admitted| elapsed_ms.saturating_sub(admitted.elapsed().as_millis() as u64)).unwrap_or(elapsed_ms);
        let execution_ms = admitted_at.as_ref().map(|admitted| admitted.elapsed().as_millis() as u64);
        if let Some(generation) = provider_host_generation {
            resolved_invocation["provider_host_generation"] = json!(generation);
        }
        let final_error = runtime_error.or_else(|| {
            if successful {
                None
            } else if !turn_completed {
                Some("worker_runtime_incomplete_output:terminal_turn_event_missing".to_string())
            } else {
                Some(format!("worker_runtime_exit:{:?}", status.and_then(|v| v.code())))
            }
        });
        let _ = write_json_atomic(
            &dir.join("last_message.json"),
            &json!({"result":assistant.clone(),"summary":assistant.clone(),"error":final_error.clone(),"failure":failure.clone(),"deliverables":[],"open_questions":[],"next_actions":[]}),
        );
        let refusals = refusal_records(&events_path);
        let payload = json!({"schema":"narada.worker.run.v1","run_id":id,"task_label":task_label,"status":if successful{"completed"}else{"failed"},"completion_state":if turn_completed && assistant.is_some(){"complete"}else{"absent"},"phase":if successful{"completed"}else{"failed"},"heartbeat_at":finished,"terminal_event":turn_completed,"runtime":"narada-agent-runtime-server","authority":authority,"cognition":cognition,"resolved_invocation":resolved_invocation,"capability_snapshot":snapshot,"worker_session_id":provider_session.unwrap_or(session),"pid":child.id(),"summary":assistant.clone(),"result":assistant.clone(),"execution_log":{"events_ref":artifact_ref(&id,"events.jsonl"),"diagnostic_ref":artifact_ref(&id,"diagnostic.log")},"refusals":refusals,"error":final_error,"failure":failure,"timing":{"started_at":started_at,"admitted_at":admitted_at_text,"finished_at":finished,"queue_ms":queue_ms,"execution_ms":execution_ms,"duration_ms":elapsed_ms},"artifacts":{"request":dir.join("request.json").to_string_lossy(),"events":events_path.to_string_lossy(),"diagnostic":diagnostic_path.to_string_lossy(),"last_message":dir.join("last_message.json").to_string_lossy()}});
        let _ = write_json_atomic(&result_path, &payload);
    }
}

