fn validate_launch_descriptor(server: &ServerInput) -> Result<(), Failure> {
    let mut validation_args = Vec::<String>::new();
    let mut arguments = server.args.iter();
    while let Some(argument) = arguments.next() {
        if matches!(
            argument.as_str(),
            "--materialization-sidecar" | "--binding-admission-path" | "--binding-admission-digest"
        ) {
            arguments.next();
            continue;
        }
        validation_args.push(argument.clone());
    }
    let mut child = Command::new(&server.command)
        .args(&validation_args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|error| {
            Failure::new("materializer_fresh_process_spawn_failed", error.to_string())
        })?;
    let modern_meta = json!({
        "io.modelcontextprotocol/protocolVersion":"2026-07-28",
        "io.modelcontextprotocol/clientInfo":{"name":"narada-mcp-materializer","version":"0.1.0"},
        "io.modelcontextprotocol/clientCapabilities":{}
    });
    let requests = vec![
        json!({"jsonrpc":"2.0","id":1,"method":"server/discover","params":{"_meta":modern_meta.clone()}}),
        json!({"jsonrpc":"2.0","id":2,"method":"tools/list","params":{"_meta":modern_meta}}),
    ];
    {
        let stdin = child.stdin.as_mut().ok_or_else(|| {
            Failure::new(
                "materializer_fresh_process_stdin_missing",
                server.name.clone(),
            )
        })?;
        for request in requests {
            let bytes = serde_json::to_vec(&request).map_err(json_failure)?;
            stdin.write_all(&bytes).map_err(|error| {
                Failure::new("materializer_fresh_process_write_failed", error.to_string())
            })?;
            stdin.write_all(b"\n").map_err(|error| {
                Failure::new("materializer_fresh_process_write_failed", error.to_string())
            })?;
        }
        stdin.flush().map_err(|error| {
            Failure::new("materializer_fresh_process_write_failed", error.to_string())
        })?;
    }
    let stdout = child.stdout.take().ok_or_else(|| {
        Failure::new(
            "materializer_fresh_process_stdout_missing",
            server.name.clone(),
        )
    })?;
    let (sender, receiver) = mpsc::channel::<String>();
    thread::spawn(move || {
        let mut reader = BufReader::new(stdout);
        loop {
            let mut line = String::new();
            match reader.read_line(&mut line) {
                Ok(0) | Err(_) => break,
                Ok(_) => {
                    if sender.send(line).is_err() {
                        break;
                    }
                }
            }
        }
    });
    let timeout = StdDuration::from_secs(server.startup_timeout_sec.unwrap_or(30).clamp(5, 120));
    let deadline = Instant::now() + timeout;
    let mut initialized = false;
    let mut tools_listed = false;
    while Instant::now() < deadline && !(initialized && tools_listed) {
        if child
            .try_wait()
            .map_err(|error| {
                Failure::new("materializer_fresh_process_wait_failed", error.to_string())
            })?
            .is_some()
        {
            break;
        }
        match receiver.recv_timeout(StdDuration::from_millis(100)) {
            Ok(line) => {
                let trimmed = line.trim();
                if !trimmed.starts_with('{') {
                    continue;
                }
                let Ok(response) = serde_json::from_str::<Value>(trimmed) else {
                    continue;
                };
                if response.get("id").and_then(Value::as_i64) == Some(1) {
                    initialized = response
                        .pointer("/result/resultType")
                        .and_then(Value::as_str)
                        == Some("complete")
                        && response
                            .pointer("/result/supportedVersions")
                            .and_then(Value::as_array)
                            .is_some_and(|versions| {
                                versions
                                    .iter()
                                    .any(|version| version.as_str() == Some("2026-07-28"))
                            });
                }
                if response.get("id").and_then(Value::as_i64) == Some(2)
                    && response
                        .pointer("/result/tools")
                        .and_then(Value::as_array)
                        .is_some()
                {
                    tools_listed = true;
                }
                if response.get("error").is_some() {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(Failure::new(
                        "materializer_fresh_process_protocol_refused",
                        server.name.clone(),
                    )
                    .with_details(json!({"response":response})));
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
    let _ = child.kill();
    let _ = child.wait();
    if !initialized || !tools_listed {
        return Err(Failure::new(
            "materializer_fresh_process_validation_failed",
            server.name.clone(),
        )
        .with_details(json!({
            "protocol_discovery_succeeded":initialized,
            "protocol_mode":"2026-07-28",
            "tools_list_succeeded":tools_listed,
            "timeout_seconds":timeout.as_secs(),
        })));
    }
    Ok(())
}

fn acquire_publication_lock(carrier_root: &Path) -> Result<fs::File, Failure> {
    let lock_root = carrier_root.join("locks");
    fs::create_dir_all(&lock_root).map_err(|error| {
        Failure::new(
            "materializer_publication_lock_directory_failed",
            error.to_string(),
        )
    })?;
    let lock_path = lock_root.join("carrier-publication.lock");
    let mut lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .open(&lock_path)
        .map_err(|error| {
            Failure::new(
                "materializer_publication_lock_open_failed",
                error.to_string(),
            )
        })?;
    lock.try_lock_exclusive().map_err(|error| {
        Failure::new("materializer_publication_locked", error.to_string())
            .with_details(json!({"lock_path": path_text(&lock_path)}))
    })?;
    lock.set_len(0).map_err(|error| {
        Failure::new(
            "materializer_publication_lock_metadata_failed",
            error.to_string(),
        )
    })?;
    lock.write_all(
        format!(
            "{{\"schema\":\"narada.carrier_publication_lock.v1\",\"pid\":{},\"acquired_at\":{}}}\n",
            std::process::id(),
            serde_json::to_string(
                &OffsetDateTime::now_utc()
                    .format(&Rfc3339)
                    .unwrap_or_default()
            )
            .unwrap_or_else(|_| "\"\"".to_string())
        )
        .as_bytes(),
    )
    .map_err(|error| {
        Failure::new(
            "materializer_publication_lock_metadata_failed",
            error.to_string(),
        )
    })?;
    lock.sync_all().map_err(|error| {
        Failure::new(
            "materializer_publication_lock_metadata_failed",
            error.to_string(),
        )
    })?;
    Ok(lock)
}

