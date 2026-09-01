
fn run_proxy(args: &[String]) -> Result<(), String> {
    if args.iter().any(|arg| arg == "--list-runtime-instances") {
        return list_instances(args);
    }
    let options = parse_options(args)?;
    let startup_clock = Instant::now();
    let manifest_fingerprint = match preflight_workspace(&options) {
        Ok(value) => value,
        Err(refusal) => return preflight_refusal(&options, refusal),
    };
    if options.runtime_contract_version != Some(CONTRACT_VERSION.into()) {
        let (code, reason) = if options.runtime_contract_version.is_none() {
            (
                "runtime_contract_version_missing",
                "The launch did not declare the MCP runtime contract version.",
            )
        } else {
            (
                "runtime_contract_version_mismatch",
                "The launch declares an obsolete MCP runtime contract version.",
            )
        };
        return preflight_refusal(
            &options,
            refusal(
                code,
                reason,
                json!({
                    "actual_runtime_contract_version": options.runtime_contract_version,
                    "expected_runtime_contract_version": CONTRACT_VERSION,
                    "remediation": "Regenerate the carrier configuration with the current registrar before launching this surface."
                }),
            ),
        );
    }
    if let Some(sidecar) = &options.materialization_sidecar {
        if let Err(refusal) =
            preflight_materialization(&options, sidecar, manifest_fingerprint.as_deref())
        {
            return preflight_refusal(&options, refusal);
        }
    }
    if !options.entrypoint.is_file() {
        return Err(format!(
            "mcp_runtime_proxy_entrypoint_not_found:{}",
            options.entrypoint.display()
        ));
    }

    fs::create_dir_all(&options.diagnostics_dir)
        .map_err(|error| format!("mcp_runtime_proxy_diagnostics_create_failed:{error}"))?;
    write_startup_phase_trace(&options, startup_clock.elapsed().as_secs_f64() * 1000.0);
    let mut startup_trace = NativeStartupTrace {
        started_at: now_iso(),
        started_clock: startup_clock,
        path: options.diagnostics_dir.join(format!(
            "startup-{}.json",
            safe_segment(options.surface_id.as_deref().unwrap_or("surface"))
        )),
        events: Vec::new(),
        completed: false,
    };
    record_startup_event(
        &mut startup_trace,
        &options,
        "preflight_ok",
        json!({
            "runtime_contract_version": options.runtime_contract_version,
            "artifact_manifest_fingerprint": manifest_fingerprint,
        }),
        false,
    );
    let resolved_child_command = resolve_child_command(&options.child_command)?;
    let mut command = Command::new(&resolved_child_command);
    let child_entry = if options.child_invocation_kind == "native_applet" {
        Some(Path::new(
            options.child_applet.as_deref().unwrap_or_default(),
        ))
    } else if options.child_invocation_kind == "native_entrypoint" {
        None
    } else {
        Some(options.entrypoint.as_path())
    };
    command.args(&options.child_prefix_args);
    if let Some(child_entry) = child_entry {
        command.arg(child_entry);
    }
    command.args(&options.child_args);
    if let Some(carrier_id) = options.carrier_id.as_deref() {
        command.env("NARADA_MATERIALIZED_CARRIER_ID", carrier_id);
    }
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000 | 0x00000004);
    }
    let child = command
        .spawn()
        .map_err(|error| format!("mcp_runtime_proxy_child_spawn_failed:{error}"))?;
    let child_pid = child.id();
    let child = Arc::new(Mutex::new(child));
    let _job = assign_kill_job(&child)?;
    resume_main_thread(child_pid)?;
    record_startup_event(
        &mut startup_trace,
        &options,
        "child_spawned",
        json!({
            "child_pid": child_pid,
            "child_command": options.child_command,
            "child_prefix_args": options.child_prefix_args,
            "child_invocation_kind": options.child_invocation_kind,
            "child_applet": options.child_applet,
        }),
        false,
    );
    let child_stdin = Arc::new(Mutex::new(child.lock().map_err(lock_error)?.stdin.take()));
    let child_stdout = child
        .lock()
        .map_err(lock_error)?
        .stdout
        .take()
        .ok_or("mcp_runtime_proxy_child_stdout_missing")?;
    let child_stderr = child
        .lock()
        .map_err(lock_error)?
        .stderr
        .take()
        .ok_or("mcp_runtime_proxy_child_stderr_missing")?;
    let proxy_pid = std::process::id();
    let started_at = now_iso();
    let freshness = FreshnessTracker {
        started_at: started_at.clone(),
        proxy_runtime: file_snapshot(
            &env::current_exe().unwrap_or_else(|_| PathBuf::from("narada-mcp-runtime")),
        ),
        child_runtime: file_snapshot(&options.entrypoint),
    };
    write_instance(
        &options,
        proxy_pid,
        child_pid,
        &started_at,
        "live",
        None,
        &freshness,
    )?;
    emit_runtime_start(&options, proxy_pid, child_pid);

    let (sender, receiver) = mpsc::channel::<Event>();
    spawn_reader(io::stdin(), sender.clone(), true);
    spawn_reader(child_stdout, sender.clone(), false);
    {
        let sender = sender.clone();
        thread::spawn(move || {
            let mut reader = BufReader::new(child_stderr);
            let mut buffer = [0_u8; 4096];
            loop {
                match reader.read(&mut buffer) {
                    Ok(0) | Err(_) => break,
                    Ok(count) => {
                        let _ = sender.send(Event::ChildStderr(buffer[..count].to_vec()));
                    }
                }
            }
        });
    }

    let mut stdout = io::stdout().lock();
    let mut pending = HashMap::<String, Pending>::new();
    let mut stderr_tail = Vec::<u8>::new();
    let mut carrier_closed_at: Option<Instant> = None;
    let mut child_output_closed = false;
    let mut last_heartbeat = Instant::now();
    loop {
        match receiver.recv_timeout(Duration::from_millis(25)) {
            Ok(Event::Carrier(message)) => {
                if is_status_call(&message.value) {
                    let response = status_response(
                        &message.value,
                        &options,
                        proxy_pid,
                        child_pid,
                        manifest_fingerprint.as_deref(),
                        &freshness,
                    );
                    write_wire(&mut stdout, &response, message.framed)?;
                    continue;
                }
                if let Some(state) = orientation_request_refusal(&options, &message.value) {
                    if let Some(id) = message
                        .value
                        .get("id")
                        .filter(|id| id.is_string() || id.is_number())
                        .cloned()
                    {
                        let reason = state
                            .get("reason")
                            .and_then(Value::as_str)
                            .unwrap_or("orientation_acknowledgement_required");
                        let response = json!({
                            "jsonrpc": "2.0",
                            "id": id,
                            "error": {
                                "code": -32000,
                                "message": format!("orientation_required:{reason}"),
                                "data": state,
                            },
                        });
                        write_wire(&mut stdout, &response, message.framed)?;
                    }
                    record_startup_event(
                        &mut startup_trace,
                        &options,
                        "request_refused",
                        json!({
                            "method": message.value.get("method"),
                            "request_id": message.value.get("id"),
                            "reason": state.get("reason"),
                            "ordinary_work_gate": state.get("ordinary_work_gate"),
                            "delivery_receipt_ref": state.get("delivery_receipt_ref"),
                        }),
                        false,
                    );
                    continue;
                }
                if let Some(response) = registrar_carrier_compatibility_response(
                    options.surface_id.as_deref(),
                    options.carrier_kind.as_deref(),
                    &message.value,
                ) {
                    write_wire(&mut stdout, &response, message.framed)?;
                    continue;
                }
                if registrar_carrier_compatibility_notification(
                    options.surface_id.as_deref(),
                    &message.value,
                ) {
                    continue;
                }
                if let Some((id, method)) = request_identity(&message.value) {
                    if method == "initialize" || method == "tools/list" {
                        record_startup_event(
                            &mut startup_trace,
                            &options,
                            "request_forwarded",
                            json!({
                                "method": method,
                                "request_id": id,
                            }),
                            false,
                        );
                    }
                    let requested = requested_transport_timeout(&message.value);
                    let effective = effective_timeout(
                        options.request_timeout_ms,
                        requested,
                        options.tool_timeout_grace_ms,
                    );
                    pending.insert(
                        id,
                        Pending {
                            method,
                            framed: message.framed,
                            deadline: Instant::now() + Duration::from_millis(effective),
                            effective_timeout_ms: effective,
                            requested_transport_timeout_ms: requested,
                        },
                    );
                }
                write_child(&child_stdin, &message.value)?;
            }
            Ok(Event::CarrierClosed) => {
                carrier_closed_at.get_or_insert_with(Instant::now);
                let _ = child_stdin.lock().map_err(lock_error)?.take();
            }
            Ok(Event::Child(mut message)) => {
                let id = json_id(&message.value);
                let framed = id
                    .as_ref()
                    .and_then(|value| pending.get(value).map(|entry| entry.framed))
                    .unwrap_or(message.framed);
                if let Some(id) = id {
                    let method = pending.get(&id).map(|entry| entry.method.clone());
                    if method.as_deref() == Some("tools/list") {
                        inject_status_tool(&mut message.value);
                    }
                    if matches!(method.as_deref(), Some("initialize" | "tools/list")) {
                        record_startup_event(
                            &mut startup_trace,
                            &options,
                            "child_response",
                            json!({
                                "method": method,
                                "request_id": id,
                            }),
                            method.as_deref() == Some("tools/list"),
                        );
                    }
                    pending.remove(&id);
                }
                write_wire(&mut stdout, &message.value, framed)?;
            }
            Ok(Event::ChildOutputClosed) => child_output_closed = true,
            Ok(Event::ChildStderr(bytes)) => {
                io::stderr()
                    .write_all(&bytes)
                    .map_err(|error| format!("mcp_runtime_proxy_stderr_forward_failed:{error}"))?;
                append_tail(&mut stderr_tail, &bytes);
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => child_output_closed = true,
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }

        let now = Instant::now();
        let timed_out = pending
            .iter()
            .filter(|(_, request)| request.deadline <= now)
            .map(|(id, _)| id.clone())
            .collect::<Vec<_>>();
        for id in timed_out {
            if let Some(request) = pending.remove(&id) {
                send_cancel(&child_stdin, &id)?;
                let error = proxy_error(
                    &id,
                    &request,
                    &options,
                    "child_request_timeout",
                    format!(
                        "child_request_timeout:{}:{}ms",
                        request.method, request.effective_timeout_ms
                    ),
                    None,
                    &stderr_tail,
                );
                write_wire(&mut stdout, &error, request.framed)?;
                write_forensic(
                    &options,
                    "child_request_timeout",
                    &id,
                    &request.method,
                    child_pid,
                    &stderr_tail,
                )?;
                child.lock().map_err(lock_error)?.kill().ok();
            }
        }

        let exit = child
            .lock()
            .map_err(lock_error)?
            .try_wait()
            .map_err(|error| format!("mcp_runtime_proxy_child_wait_failed:{error}"))?;
        if let Some(status) = exit {
            let code = status.code();
            for _ in 0..20 {
                match receiver.recv_timeout(Duration::from_millis(5)) {
                    Ok(Event::ChildStderr(bytes)) => {
                        io::stderr().write_all(&bytes).map_err(io_string)?;
                        append_tail(&mut stderr_tail, &bytes);
                    }
                    Ok(Event::Child(mut message)) => {
                        let id = json_id(&message.value);
                        let framed = id
                            .as_ref()
                            .and_then(|value| pending.get(value).map(|entry| entry.framed))
                            .unwrap_or(message.framed);
                        if let Some(id) = id {
                            if pending.get(&id).map(|entry| entry.method.as_str())
                                == Some("tools/list")
                            {
                                inject_status_tool(&mut message.value);
                            }
                            pending.remove(&id);
                        }
                        write_wire(&mut stdout, &message.value, framed)?;
                    }
                    Ok(_) => {}
                    Err(_) => break,
                }
            }
            for (id, request) in pending.drain() {
                let error = proxy_error(
                    &id,
                    &request,
                    &options,
                    "child_exited_before_response",
                    format!(
                        "child_exited_before_response:{}",
                        code.map(|value| value.to_string())
                            .unwrap_or_else(|| "signal".to_string())
                    ),
                    code,
                    &stderr_tail,
                );
                write_wire(&mut stdout, &error, request.framed)?;
                write_forensic(
                    &options,
                    "child_exited_before_response",
                    &id,
                    &request.method,
                    child_pid,
                    &stderr_tail,
                )?;
            }
            write_instance(
                &options,
                proxy_pid,
                child_pid,
                &started_at,
                "closed",
                code,
                &freshness,
            )?;
            emit_runtime_exit(
                &options,
                child_pid,
                if status.success() { "ok" } else { "failed" },
            );
            stdout.flush().ok();
            if status.success() {
                return Ok(());
            }
            return Err(format!(
                "mcp_runtime_proxy_child_exit:{}",
                code.unwrap_or(1)
            ));
        }
        if let Some(closed_at) = carrier_closed_at {
            if now.duration_since(closed_at) >= Duration::from_millis(options.orphan_grace_ms) {
                child.lock().map_err(lock_error)?.kill().ok();
            }
        }
        if carrier_closed_at.is_none()
            && last_heartbeat.elapsed() >= Duration::from_millis(options.liveness_check_ms)
        {
            write_instance(
                &options,
                proxy_pid,
                child_pid,
                &started_at,
                "live",
                None,
                &freshness,
            )?;
            last_heartbeat = Instant::now();
        }
        let _ = child_output_closed;
    }
}
