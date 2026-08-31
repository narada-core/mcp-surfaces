use crate::full::*;

impl ChildSession {
    #[allow(clippy::while_let_loop)]
    pub(crate) fn spawn(
        spec: ChildSpec,
        env_map: &HashMap<String, String>,
        max_response_bytes: usize,
    ) -> Result<Self, Diagnostic> {
        let mut command = Command::new(&spec.command);
        command
            .args(&spec.args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        command.env_clear();
        for (key, value) in env_map {
            command.env(key, value);
        }
        #[cfg(windows)]
        command.creation_flags(0x0800_0000);
        let mut child = command.spawn().map_err(|error| {
            Diagnostic::new(
                "child_spawn_failed",
                format!("child_spawn_failed:{}", error),
            )
            .with_details(json!({"command": spec.command, "args": spec.args}))
        })?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| Diagnostic::new("child_stdin_unavailable", "child_stdin_unavailable"))?;
        let stdout = child.stdout.take().ok_or_else(|| {
            Diagnostic::new("child_stdout_unavailable", "child_stdout_unavailable")
        })?;
        let stderr = child.stderr.take().ok_or_else(|| {
            Diagnostic::new("child_stderr_unavailable", "child_stderr_unavailable")
        })?;
        let pid = child.id();
        let child = Arc::new(Mutex::new(child));
        let stdin = Arc::new(Mutex::new(stdin));
        let pending: PendingRequests = Arc::new(Mutex::new(HashMap::new()));
        let closed = Arc::new(AtomicBool::new(false));
        let killed = Arc::new(AtomicBool::new(false));
        let reader_pending = Arc::clone(&pending);
        let reader_closed = Arc::clone(&closed);
        thread::spawn(move || {
            let mut reader = WireReader::new(stdout, max_response_bytes);
            loop {
                match reader.next() {
                    Ok(Some((message, _))) => {
                        let Some(object) = message.as_object() else {
                            continue;
                        };
                        let Some(id) = object.get("id").and_then(value_u64) else {
                            continue;
                        };
                        let sender = reader_pending
                            .lock()
                            .ok()
                            .and_then(|mut pending| pending.remove(&id));
                        let Some(sender) = sender else {
                            continue;
                        };
                        let result = if let Some(error) = object.get("error") {
                            Err(child_error_diagnostic(error))
                        } else {
                            Ok(object.get("result").cloned().unwrap_or(Value::Null))
                        };
                        let _ = sender.send(result);
                    }
                    Ok(None) | Err(_) => break,
                }
            }
            reader_closed.store(true, Ordering::SeqCst);
            let pending = reader_pending
                .lock()
                .ok()
                .map(|mut pending| {
                    pending
                        .drain()
                        .map(|(_, sender)| sender)
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            for sender in pending {
                let _ = sender.send(Err(Diagnostic::new("child_exited", "child_exited")));
            }
        });
        let tail = Arc::new(Mutex::new(String::new()));
        let reader_tail = Arc::clone(&tail);
        thread::spawn(move || {
            let mut reader = BufReader::new(stderr);
            let mut chunk = [0_u8; 2048];
            loop {
                match reader.read(&mut chunk) {
                    Ok(0) | Err(_) => break,
                    Ok(count) => {
                        if let Ok(mut value) = reader_tail.lock() {
                            value.push_str(&String::from_utf8_lossy(&chunk[..count]));
                            if value.len() > STDERR_TAIL_LIMIT {
                                let start = value.len() - STDERR_TAIL_LIMIT;
                                *value = value[start..].to_string();
                            }
                        }
                    }
                }
            }
        });
        Ok(Self {
            spec,
            child,
            stdin,
            pending,
            next_id: AtomicU64::new(1),
            closed,
            killed,
            stderr_tail: tail,
            pid,
        })
    }
}
