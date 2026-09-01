
fn spawn_reader<R: Read + Send + 'static>(reader: R, sender: mpsc::Sender<Event>, carrier: bool) {
    thread::spawn(move || {
        let mut reader = BufReader::new(reader);
        loop {
            match read_wire(&mut reader) {
                Ok(Some(message)) => {
                    let event = if carrier {
                        Event::Carrier(message)
                    } else {
                        Event::Child(message)
                    };
                    if sender.send(event).is_err() {
                        break;
                    }
                }
                Ok(None) | Err(_) => {
                    let _ = sender.send(if carrier {
                        Event::CarrierClosed
                    } else {
                        Event::ChildOutputClosed
                    });
                    break;
                }
            }
        }
    });
}

fn read_wire<R: BufRead>(reader: &mut R) -> io::Result<Option<WireMessage>> {
    let mut first = String::new();
    if reader.read_line(&mut first)? == 0 {
        return Ok(None);
    }
    if first.trim().is_empty() {
        return read_wire(reader);
    }
    if first.to_ascii_lowercase().starts_with("content-length:") {
        let length = first
            .split_once(':')
            .and_then(|(_, value)| value.trim().parse::<usize>().ok())
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "invalid Content-Length"))?;
        loop {
            let mut header = String::new();
            reader.read_line(&mut header)?;
            if header == "\r\n" || header == "\n" || header.is_empty() {
                break;
            }
        }
        let mut body = vec![0_u8; length];
        reader.read_exact(&mut body)?;
        let value = serde_json::from_slice(&body).map_err(json_io)?;
        return Ok(Some(WireMessage {
            value,
            framed: true,
        }));
    }
    let value = parse_json_prefix(first.trim())
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "invalid JSON-RPC line"))?;
    Ok(Some(WireMessage {
        value,
        framed: false,
    }))
}

fn parse_json_prefix(text: &str) -> Option<Value> {
    serde_json::Deserializer::from_str(text)
        .into_iter::<Value>()
        .next()?
        .ok()
}

fn write_wire<W: Write>(writer: &mut W, value: &Value, framed: bool) -> Result<(), String> {
    let body = serde_json::to_vec(value)
        .map_err(|error| format!("mcp_runtime_proxy_json_encode_failed:{error}"))?;
    if framed {
        write!(writer, "Content-Length: {}\r\n\r\n", body.len()).map_err(io_string)?;
    }
    writer.write_all(&body).map_err(io_string)?;
    if !framed {
        writer.write_all(b"\n").map_err(io_string)?;
    }
    writer.flush().map_err(io_string)
}

fn write_child(stdin: &Arc<Mutex<Option<ChildStdin>>>, value: &Value) -> Result<(), String> {
    let mut guard = stdin.lock().map_err(lock_error)?;
    let stream = guard
        .as_mut()
        .ok_or("mcp_runtime_proxy_child_stdin_closed")?;
    write_wire(stream, value, false)
}

fn request_identity(value: &Value) -> Option<(String, String)> {
    let id = json_id(value)?;
    let method = value.get("method")?.as_str()?.to_string();
    Some((id, method))
}

fn json_id(value: &Value) -> Option<String> {
    match value.get("id")? {
        Value::String(value) => Some(format!("s:{value}")),
        Value::Number(value) => Some(format!("n:{value}")),
        _ => None,
    }
}

fn id_value(id: &str) -> Value {
    if let Some(value) = id.strip_prefix("s:") {
        Value::String(value.to_string())
    } else if let Some(value) = id.strip_prefix("n:") {
        serde_json::from_str(value).unwrap_or(Value::Null)
    } else {
        Value::Null
    }
}

fn requested_transport_timeout(value: &Value) -> Option<u64> {
    value
        .pointer("/params/_meta/narada_request_timeout_ms")?
        .as_u64()
        .filter(|value| *value > 0)
}

fn effective_timeout(proxy: u64, requested: Option<u64>, grace: u64) -> u64 {
    requested
        .map(|value| proxy.max(value.min(MAX_TRANSPORT_TIMEOUT_MS).saturating_add(grace)))
        .unwrap_or(proxy)
}

fn is_status_call(value: &Value) -> bool {
    value.get("method").and_then(Value::as_str) == Some("tools/call")
        && value.pointer("/params/name").and_then(Value::as_str) == Some(STATUS_TOOL)
        && value
            .get("id")
            .is_some_and(|id| id.is_string() || id.is_number())
}

fn inject_status_tool(value: &mut Value) {
    let Some(tools) = value
        .pointer_mut("/result/tools")
        .and_then(Value::as_array_mut)
    else {
        return;
    };
    if tools
        .iter()
        .any(|tool| tool.get("name").and_then(Value::as_str) == Some(STATUS_TOOL))
    {
        return;
    }
    tools.push(status_tool_definition());
}

fn status_tool_definition() -> Value {
    json!({
        "name": STATUS_TOOL,
        "description": "Inspect carrier-bound proxy/server liveness and build/runtime freshness, including the machine-readable supervisor restart action.",
        "inputSchema": { "type": "object", "properties": {}, "additionalProperties": false },
        "annotations": { "title": STATUS_TOOL, "readOnlyHint": true, "destructiveHint": false, "idempotentHint": true, "openWorldHint": false }
    })
}

fn status_response(
    request: &Value,
    options: &Options,
    proxy_pid: u32,
    child_pid: u32,
    manifest_fingerprint: Option<&str>,
    freshness: &FreshnessTracker,
) -> Value {
    let id = request.get("id").cloned().unwrap_or(Value::Null);
    let runtime_freshness = evaluate_freshness(
        options,
        proxy_pid,
        child_pid,
        manifest_fingerprint,
        freshness,
    );
    let freshness_status = runtime_freshness
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let heartbeat_at = now_iso();
    let lease_expires_at = OffsetDateTime::now_utc()
        .saturating_add(time::Duration::milliseconds(
            (options.liveness_check_ms.saturating_mul(3)) as i64,
        ))
        .format(&Rfc3339)
        .unwrap_or_else(|_| heartbeat_at.clone());
    let payload = json!({
        "schema": "narada.mcp_runtime_proxy.status.v1",
        "status": "ok",
        "surface_id": options.surface_id,
        "liveness": {
            "schema": "narada.mcp_runtime_proxy.instance.v2",
            "surface_id": options.surface_id,
            "proxy_pid": proxy_pid,
            "parent_pid": parent_pid(),
            "child_pid": child_pid,
            "supervisor_pid": Value::Null,
            "managed_child_pid": child_pid,
            "server_pid": child_pid,
            "entrypoint": options.entrypoint,
            "child_invocation_kind": options.child_invocation_kind,
            "child_applet": options.child_applet,
            "started_at": freshness.started_at,
            "heartbeat_at": heartbeat_at,
            "lease_expires_at": lease_expires_at,
            "state": "live",
            "liveness_evidence": { "proxy_implementation": "native", "carrier_id": options.carrier_id },
            "artifact_manifest_path": options.artifact_manifest,
            "artifact_manifest_fingerprint": manifest_fingerprint,
            "generation_id": format!("{}:{}", options.surface_id.as_deref().unwrap_or("surface"), freshness.started_at),
            "supervisor_identity_path": Value::Null,
            "closed_at": Value::Null,
            "observed_state": "live",
            "stale_reasons": [],
        },
        "runtime_freshness": runtime_freshness
    });
    json!({ "jsonrpc": "2.0", "id": id, "result": {
        "content": [{ "type": "text", "text": format!("mcp_runtime_proxy_status: {freshness_status}\nproxy_pid: {proxy_pid}\nchild_pid: {child_pid}\nchild_pid_role: server\nserver_pid: {child_pid}\nrestart_owner: carrier_or_runtime_supervisor") }],
        "structuredContent": payload
    }})
}
