fn broker_event(request_id: &str, state: &str, extra: Value) -> Value {
    let mut event = json!({
        "schema":"narada.codex_app_server.broker_event.v2",
        "request_id":request_id,
        "state":state,
    });
    if let (Some(target), Some(values)) = (event.as_object_mut(), extra.as_object()) {
        for (key, value) in values {
            target.insert(key.clone(), value.clone());
        }
    }
    event
}

fn write_frame(stream: &mut TcpStream, value: &Value) -> Result<(), String> {
    serde_json::to_writer(&mut *stream, value)
        .map_err(|error| format!("codex_app_server_broker_response_encode_failed:{error}"))?;
    stream
        .write_all(b"\n")
        .and_then(|()| stream.flush())
        .map_err(|error| format!("codex_app_server_broker_response_write_failed:{error}"))
}

fn read_frame(stream: &mut TcpStream) -> Result<Value, String> {
    let mut bytes = Vec::new();
    BufReader::new(stream)
        .take((MAX_FRAME_BYTES + 1) as u64)
        .read_until(b'\n', &mut bytes)
        .map_err(|error| format!("codex_app_server_broker_request_read_failed:{error}"))?;
    if bytes.len() > MAX_FRAME_BYTES {
        return Err("codex_app_server_broker_request_too_large".to_string());
    }
    serde_json::from_slice(&bytes)
        .map_err(|error| format!("codex_app_server_broker_request_invalid:{error}"))
}

struct AppServer {
    site_root: PathBuf,
    child: Child,
    input: ChildStdin,
    output: mpsc::Receiver<String>,
    next_id: u64,
    generation: String,
}

impl AppServer {
    fn start(site_root: &Path) -> Result<Self, String> {
        let site_root = site_root
            .canonicalize()
            .map_err(|error| format!("codex_app_server_site_root_invalid:{error}"))?;
        if !site_root.is_dir() {
            return Err("codex_app_server_site_root_not_directory".to_string());
        }
        let command =
            std::env::var_os("NARADA_NATIVE_CODEX_COMMAND").unwrap_or_else(|| "codex".into());
        let mut child = Command::new(command)
            .args(app_server_args())
            .current_dir(&site_root)
            .env_remove("CODEX_PERMISSION_PROFILE")
            .env_remove("CODEX_THREAD_ID")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|error| format!("codex_app_server_spawn_failed:{error}"))?;
        let input = child
            .stdin
            .take()
            .ok_or_else(|| "codex_app_server_stdin_missing".to_string())?;
        let output = child
            .stdout
            .take()
            .ok_or_else(|| "codex_app_server_stdout_missing".to_string())?;
        let (output_tx, output_rx) = mpsc::channel();
        thread::Builder::new()
            .name("codex-app-server-output".to_string())
            .spawn(move || {
                for line in BufReader::new(output).lines().map_while(Result::ok) {
                    if output_tx.send(line).is_err() {
                        break;
                    }
                }
            })
            .map_err(|error| format!("codex_app_server_output_thread_failed:{error}"))?;
        let mut server = Self {
            site_root,
            child,
            input,
            output: output_rx,
            next_id: 1,
            generation: uuid::Uuid::new_v4().to_string(),
        };
        let id = server.request(
            "initialize",
            json!({"clientInfo":{"name":"narada-native-provider","version":"1"},"capabilities":{"experimentalApi":true}}),
        )?;
        let _ = server.response(id)?;
        Ok(server)
    }

    fn perform_turn(&mut self, request: &Value, cancelled: &AtomicBool) -> Result<Value, String> {
        if self
            .child
            .try_wait()
            .map_err(|error| error.to_string())?
            .is_some()
        {
            *self = Self::start(&self.site_root)?;
        }
        let prompt = required_string(request, "prompt")?;
        let cwd = required_string(request, "cwd")?;
        let model = required_string(request, "model")?;
        let effort = required_string(request, "reasoning_effort")?;
        let sandbox = required_string(request, "sandbox")?;
        let writable_roots = request
            .get("writable_roots")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let thread_request = self.request(
            "thread/start",
            json!({
                "cwd":cwd,
                "model":model,
                "approvalPolicy":"never",
                "sandbox":sandbox,
                "runtimeWorkspaceRoots":writable_roots,
                "ephemeral":true,
                "config":{"mcp_servers":{},"features":{"apps":false}},
            }),
        )?;
        let thread = self.response(thread_request)?;
        let thread_id = thread
            .pointer("/thread/id")
            .and_then(Value::as_str)
            .ok_or_else(|| "codex_app_server_thread_id_missing".to_string())?
            .to_string();
        let sandbox_policy = if sandbox == "workspace-write" {
            json!({"type":"workspaceWrite","writableRoots":writable_roots,"networkAccess":false})
        } else {
            json!({"type":"readOnly"})
        };
        let turn_request = self.request(
            "turn/start",
            json!({
                "threadId":thread_id,
                "input":[{"type":"text","text":prompt}],
                "model":model,
                "effort":effort,
                "cwd":cwd,
                "approvalPolicy":"never",
                "sandboxPolicy":sandbox_policy,
            }),
        )?;
        let turn = self.response(turn_request)?;
        let turn_id = turn
            .pointer("/turn/id")
            .and_then(Value::as_str)
            .ok_or_else(|| "codex_app_server_turn_id_missing".to_string())?
            .to_string();
        let mut content = None;
        loop {
            if cancelled.load(Ordering::Acquire) {
                let interrupt = self.request(
                    "turn/interrupt",
                    json!({"threadId":thread_id,"turnId":turn_id}),
                )?;
                let _ = self.response(interrupt);
                return Err("codex_app_server_turn_interrupted".to_string());
            }
            let Some(message) = self.read_message_timeout(Duration::from_millis(100))? else {
                continue;
            };
            if message.get("method").and_then(Value::as_str) == Some("item/completed")
                && message.pointer("/params/item/type").and_then(Value::as_str)
                    == Some("agentMessage")
            {
                content = message
                    .pointer("/params/item/text")
                    .and_then(Value::as_str)
                    .map(str::to_string);
            }
            if message.get("method").and_then(Value::as_str) == Some("turn/completed")
                && message.pointer("/params/turn/id").and_then(Value::as_str)
                    == Some(turn_id.as_str())
            {
                let status = message
                    .pointer("/params/turn/status")
                    .and_then(Value::as_str)
                    .unwrap_or("failed");
                if status != "completed" {
                    return Err(format!("codex_app_server_turn_{status}"));
                }
                break;
            }
        }
        Ok(broker_event(
            required_string(request, "request_id")?,
            "completed",
            json!({
                "content":content.ok_or_else(|| "codex_app_server_content_missing".to_string())?,
                "thread_id":thread_id,
                "turn_id":turn_id,
                "host_generation":self.generation,
            }),
        ))
    }

    fn request(&mut self, method: &str, params: Value) -> Result<u64, String> {
        let id = self.next_id;
        self.next_id += 1;
        serde_json::to_writer(
            &mut self.input,
            &json!({"id":id,"method":method,"params":params}),
        )
        .map_err(|error| format!("codex_app_server_request_encode_failed:{error}"))?;
        self.input
            .write_all(b"\n")
            .and_then(|()| self.input.flush())
            .map_err(|error| format!("codex_app_server_request_write_failed:{error}"))?;
        Ok(id)
    }

    fn response(&mut self, id: u64) -> Result<Value, String> {
        loop {
            let message = self.read_message()?;
            if message.get("id").and_then(Value::as_u64) == Some(id) {
                if let Some(error) = message.get("error").filter(|value| !value.is_null()) {
                    return Err(format!("codex_app_server_response_error:{error}"));
                }
                return Ok(message.get("result").cloned().unwrap_or(Value::Null));
            }
        }
    }

    fn read_message(&mut self) -> Result<Value, String> {
        self.read_message_timeout(IO_TIMEOUT)?
            .ok_or_else(|| "codex_app_server_read_timed_out".to_string())
    }

    fn read_message_timeout(&mut self, timeout: Duration) -> Result<Option<Value>, String> {
        let line = match self.output.recv_timeout(timeout) {
            Ok(line) => line,
            Err(mpsc::RecvTimeoutError::Timeout) => return Ok(None),
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                return Err("codex_app_server_stdout_closed".to_string())
            }
        };
        if line.len() > MAX_FRAME_BYTES {
            return Err("codex_app_server_response_too_large".to_string());
        }
        serde_json::from_str(&line)
            .map(Some)
            .map_err(|error| format!("codex_app_server_response_invalid:{error}"))
    }
}

fn app_server_args() -> [&'static str; 9] {
    [
        "app-server",
        "--listen",
        "stdio://",
        "-c",
        "mcp_servers={}",
        "-c",
        "features.apps=false",
        "-c",
        "windows.sandbox=\"unelevated\"",
    ]
}

impl Drop for AppServer {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

