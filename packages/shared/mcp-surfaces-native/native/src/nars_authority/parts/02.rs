fn read_session_record(root: &Path, id: &str, requested_site: Option<&str>) -> Result<Value, Value> {
    if id.is_empty() || id.len() > 128 || !id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') {
        return Err(error("session_id_invalid", "session_id_invalid"));
    }
    if user_site_scope() {
        let authorities = user_site_authorities(root)?;
        if let Some(requested_site) = requested_site {
            if !authorities.iter().any(|(site_id, _)| site_id == requested_site) {
                return Err(error("site_scope_refused", "site_scope_refused"));
            }
        }
        let mut matches = Vec::new();
        for (site_id, site_root) in authorities {
            if requested_site.is_some_and(|requested| requested != site_id) { continue; }
            for base in session_roots(&site_root) {
                let path = base.join(id).join("session-index-record.json");
                if !path.exists() { continue; }
                let record = read_bounded_json(&path)?;
                if record.get("session_id").and_then(Value::as_str) != Some(id) { return Err(error("session_record_mismatch", "session record does not match requested session")); }
                if record.get("site_id").and_then(Value::as_str) != Some(site_id.as_str()) { return Err(error("session_site_id_mismatch", "session record belongs to a different admitted Site")); }
                matches.push(record);
            }
        }
        if matches.len() > 1 { return Err(error("session_ambiguous", "session_ambiguous")); }
        if let Some(record) = matches.into_iter().next() { return Ok(record); }
    } else {
        for base in session_roots(root) {
            let path = base.join(id).join("session-index-record.json");
            if path.exists() {
                let record = read_bounded_json(&path)?;
                if record.get("session_id").and_then(Value::as_str) != Some(id) {
                    return Err(error("session_record_mismatch", "session record does not match requested session"));
                }
                return Ok(record);
            }
        }
    }
    Err(error("session_not_found", "session_not_found"))
}

fn user_site_scope() -> bool {
    matches!(std::env::var("NARADA_NARS_SESSION_SCOPE").ok().as_deref(), Some("user_site"))
        || matches!(std::env::var("NARADA_NARS_SESSION_PROJECTION").ok().as_deref(), Some("user-site-operator"))
}

fn user_site_authorities(root: &Path) -> Result<Vec<(String, PathBuf)>, Value> {
    let user_root = std::env::var("NARADA_USER_SITE_ROOT").ok().filter(|value| !value.trim().is_empty()).map(PathBuf::from).unwrap_or_else(|| root.to_path_buf());
    let registry_path = std::env::var("NARADA_SITE_REGISTRY_DB").ok().filter(|value| !value.trim().is_empty()).map(PathBuf::from).unwrap_or_else(|| user_root.join("registry.db"));
    let connection = Connection::open_with_flags(registry_path, OpenFlags::SQLITE_OPEN_READ_ONLY).map_err(|_| error("user_site_registry_unreadable", "user_site_registry_unreadable"))?;
    let mut statement = connection.prepare("SELECT site_id, site_root FROM site_registry ORDER BY created_at ASC, site_id ASC").map_err(|_| error("user_site_registry_unreadable", "user_site_registry_unreadable"))?;
    let rows = statement.query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))).map_err(|_| error("user_site_registry_unreadable", "user_site_registry_unreadable"))?;
    let mut authorities = Vec::new();
    for row in rows { let (site_id, site_root) = row.map_err(|_| error("user_site_registry_unreadable", "user_site_registry_unreadable"))?; if !site_id.trim().is_empty() && !site_root.trim().is_empty() { authorities.push((site_id, PathBuf::from(site_root))); } }
    if authorities.is_empty() { return Err(error("user_site_registry_empty", "user_site_registry_empty")); }
    Ok(authorities)
}

fn session_roots(root: &Path) -> Vec<PathBuf> {
    let control = if root.file_name().and_then(|value| value.to_str()).is_some_and(|value| value.eq_ignore_ascii_case(".narada")) { root.to_path_buf() } else { root.join(".narada") };
    vec![control.join("crew/nars-sessions"), root.join("crew/nars-sessions")]
}

fn read_bounded_json(path: &Path) -> Result<Value, Value> {
    let size = std::fs::metadata(path).map_err(|_| error("record_not_found", "record_not_found"))?.len();
    if size > MAX_RECORD_BYTES as u64 { return Err(error("record_too_large", "record_too_large")); }
    let body = std::fs::read_to_string(path).map_err(|_| error("record_read_failed", "record_read_failed"))?;
    serde_json::from_str(&body).map_err(|_| error("record_invalid_json", "record_invalid_json"))
}

fn assert_requested_site(args: &Map<String, Value>, record: &Value) -> Result<(), Value> {
    let requested = optional_text(args.get("site_id"));
    let configured = std::env::var("NARADA_SITE_ID").ok().filter(|value| !value.trim().is_empty());
    let actual = record.get("site_id").and_then(Value::as_str).map(str::to_string).or(configured);
    if requested.is_some() && actual.is_some() && requested != actual {
        return Err(error("site_scope_refused", "site_scope_refused"));
    }
    Ok(())
}

fn health_is_healthy(value: &Value) -> bool {
    value.get("status").and_then(Value::as_str).is_some_and(|status| status.eq_ignore_ascii_case("healthy"))
}

fn directive_content(args: &Map<String, Value>) -> Result<String, Value> {
    let direct = optional_text(args.get("content"));
    let directive = args.get("directive").and_then(Value::as_object);
    let nested = directive.and_then(|value| value.get("content")).and_then(Value::as_object).and_then(|value| value.get("text")).and_then(Value::as_str).map(str::trim).filter(|value| !value.is_empty()).map(str::to_string);
    let content = direct.or(nested).ok_or_else(|| error("content_required", "content or directive.content.text is required"))?;
    if content.len() > MAX_INLINE_CONTENT { return Err(error("content_too_large", "content exceeds 20,000 characters")); }
    Ok(content)
}

fn delivery(args: &Map<String, Value>) -> Result<&'static str, Value> {
    match optional_text(args.get("delivery")).or_else(|| optional_text(args.get("delivery_mode")).map(|value| if value == "admit_for_current_turn" { "send".to_string() } else { "enqueue".to_string() })) .as_deref() {
        Some("send") => Ok("send"),
        Some("enqueue") => Ok("enqueue"),
        Some("steer") => Ok("steer"),
        _ => Err(error("delivery_invalid", "delivery must be send, enqueue, or steer")),
    }
}

fn summarize_events(events: &[Value]) -> Summary {
    let mut admission = "unknown";
    let mut terminal: Option<(String, String, Option<String>, i32)> = None;
    let mut request_state = None;
    for event in events {
        let name = event_name(event).unwrap_or_default();
        if name == "input_event_queued" { admission = "queued"; }
        if matches!(name, "input_event_started" | "turn_started" | "user_message") { admission = "admitted_to_turn"; }
        if let Some(value) = event.get("request_state").and_then(Value::as_str) { request_state = Some(value.to_string()); }
        let state = event
            .get("terminal_state")
            .or_else(|| event.get("terminal_status"))
            .and_then(terminal_state)
            .or_else(|| event.get("turn_state").and_then(terminal_state))
            .or_else(|| {
                if matches!(name, "input_event_completed" | "input_completed" | "turn_complete") {
                    Some("completed")
                } else if matches!(name, "session_control_rejected") {
                    Some("rejected")
                } else if matches!(name, "error" | "turn_failed" | "runtime_request_failed") {
                    Some("failed")
                } else {
                    None
                }
            });
        if let Some(state) = state {
            let rank = if state == "completed" { 1 } else if state == "interrupted" { 2 } else { 3 };
            if terminal.as_ref().map(|value| rank >= value.3).unwrap_or(true) {
                terminal = Some((state.to_string(), name.to_string(), event.get("error").or_else(|| event.get("message")).or_else(|| event.get("code")).and_then(Value::as_str).map(str::to_string), rank));
            }
        }
    }
    let terminal_state = terminal.as_ref().map(|value| value.0.clone());
    let terminal_event = terminal.as_ref().map(|value| value.1.clone());
    let outcome_reason = terminal.as_ref().and_then(|value| value.2.clone());
    let outcome = match terminal_state.as_deref() {
        Some("completed") => "completed",
        Some("rejected") => "refused",
        Some("interrupted") => "interrupted",
        Some(_) => "failed",
        None if admission == "unknown" => "unknown",
        None => "pending",
    };
    Summary { status: admission, admission_status: admission, terminal_state, request_state, outcome, outcome_reason, terminal_event }
}

struct Summary {
    status: &'static str,
    admission_status: &'static str,
    terminal_state: Option<String>,
    request_state: Option<String>,
    outcome: &'static str,
    outcome_reason: Option<String>,
    terminal_event: Option<String>,
}

fn matches_event(event: &Value, input_event_id: Option<&str>, request_id: Option<&str>, directive_id: Option<&str>) -> bool {
    let payload = event.get("payload").unwrap_or(&Value::Null);
    let input = event.get("input_event_id").or_else(|| event.get("event_id")).or_else(|| payload.get("input_event_id")).or_else(|| payload.get("event_id")).and_then(Value::as_str);
    let request = event.get("request_id").or_else(|| payload.get("request_id")).and_then(Value::as_str);
    let directive = event.get("directive_id").or_else(|| payload.get("directive_id")).and_then(Value::as_str);
    input_event_id.is_some_and(|value| input == Some(value)) || request_id.is_some_and(|value| request == Some(value)) || directive_id.is_some_and(|value| directive == Some(value))
}

fn terminal_state(value: &Value) -> Option<&'static str> {
    match value.as_str()?.to_ascii_lowercase().as_str() {
        "completed" | "complete" | "success" | "succeeded" => Some("completed"),
        "failed" | "error" => Some("failed"),
        "rejected" | "refused" => Some("rejected"),
        "interrupted" | "cancelled" | "canceled" => Some("interrupted"),
        _ => None,
    }
}

fn event_name(value: &Value) -> Option<&str> {
    if value.get("event").and_then(Value::as_str) == Some("session_event") {
        return value.get("payload").and_then(event_name);
    }
    value.get("event").and_then(Value::as_str).or_else(|| value.get("type").and_then(Value::as_str))
}

enum WaitFor { Delivery, EventsRead, Health }

fn ws_call(endpoint: &str, request: Value, wait_for: WaitFor) -> Result<Value, Value> {
    let parsed = WsEndpoint::parse(endpoint)?;
    let address = format!("{}:{}", parsed.host, parsed.port);
    let mut addrs = address.to_socket_addrs().map_err(|error| unavailable("websocket_resolve_failed", &error.to_string()))?;
    let socket_addr = addrs.next().ok_or_else(|| unavailable("websocket_resolve_failed", "no address"))?;
    let mut stream = TcpStream::connect_timeout(&socket_addr, timeout()) .map_err(|error| unavailable("websocket_connect_failed", &error.to_string()))?;
    stream.set_read_timeout(Some(timeout())).ok();
    stream.set_write_timeout(Some(timeout())).ok();
    let key = base64_encode(Uuid::new_v4().as_bytes());
    let expected = base64_encode(&sha1(format!("{key}258EAFA5-E914-47DA-95CA-C5AB0DC85B11").as_bytes()));
    let handshake = format!("GET {} HTTP/1.1\r\nHost: {}:{}\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: {key}\r\nSec-WebSocket-Version: 13\r\n\r\n", parsed.path, parsed.host, parsed.port);
    stream.write_all(handshake.as_bytes()).map_err(|error| unavailable("websocket_handshake_write_failed", &error.to_string()))?;
    let response = read_http_headers(&mut stream)?;
    if !response.starts_with("HTTP/1.1 101") || !response.lines().any(|line| line.to_ascii_lowercase().starts_with("sec-websocket-accept:") && line.split_once(':').map(|(_, value)| value.trim().eq_ignore_ascii_case(&expected)).unwrap_or(false)) {
        return Err(unavailable("websocket_handshake_failed", response.lines().next().unwrap_or("unknown")));
    }
    let request_id = request.get("id").cloned().unwrap_or(Value::Null);
    let subscription_id = json!(format!("{}_events", request_id.as_str().unwrap_or("request")));
    send_frame(&mut stream, &json!({"id": subscription_id, "method":"session.events.subscribe", "params":{"subscription_id":subscription_id,"filters":{},"include_replay":false,"max_replay":0}}))?;
    let mut sent_request = false;
    loop {
        let (opcode, payload) = read_frame(&mut stream)?;
        match opcode {
            0x9 => { send_control_frame(&mut stream, 0xA, &payload)?; }
            0x8 => return Err(unavailable("websocket_closed_before_response", "NARS websocket closed before response")),
            0x1 => {
                let message: Value = serde_json::from_slice(&payload).map_err(|_| unavailable("websocket_json_invalid", "websocket response is not JSON"))?;
                let name = event_name(&message).unwrap_or_default();
                if name == "session_events_subscription_started" && message.get("request_id") == Some(&subscription_id) && !sent_request {
                    send_frame(&mut stream, &request)?;
                    sent_request = true;
                    continue;
                }
                if !sent_request { continue; }
                let matches = match wait_for { WaitFor::Delivery => matches!(name, "input_event_queued" | "input_event_started" | "input_admitted_to_turn" | "session_control_accepted" | "input_completed" | "user_message" | "turn_started" | "error" | "websocket_error"), WaitFor::EventsRead => name == "session_events_read", WaitFor::Health => name == "session_health" };
                let same_request = message.get("request_id") == request.get("id") || message.get("id") == request.get("id");
                if matches && (same_request || matches!(wait_for, WaitFor::EventsRead)) {
                    let _ = stream.shutdown(Shutdown::Both);
                    return Ok(message);
                }
            }
            _ => {}
        }
    }
}

struct WsEndpoint { host: String, port: u16, path: String }

impl WsEndpoint {
    fn parse(value: &str) -> Result<Self, Value> {
        let rest = value.strip_prefix("ws://").ok_or_else(|| unavailable("websocket_protocol_unsupported", "only ws:// native endpoints are supported"))?;
        let (authority, path) = rest.split_once('/').map(|(host, path)| (host, format!("/{path}"))).unwrap_or((rest, "/".to_string()));
        let (host, port) = if let Some(stripped) = authority.strip_prefix('[') { let (host, port) = stripped.split_once(']').ok_or_else(|| unavailable("websocket_endpoint_invalid", "invalid IPv6 endpoint"))?; let port = port.strip_prefix(':').unwrap_or("80").parse().map_err(|_| unavailable("websocket_endpoint_invalid", "invalid endpoint port"))?; (host.to_string(), port) } else { let (host, port) = authority.rsplit_once(':').unwrap_or((authority, "80")); let port = port.parse().map_err(|_| unavailable("websocket_endpoint_invalid", "invalid endpoint port"))?; (host.to_string(), port) };
        if host.trim().is_empty() || port == 0 { return Err(unavailable("websocket_endpoint_invalid", "endpoint host and port are required")); }
        Ok(Self { host, port, path })
    }
}

fn read_http_headers(stream: &mut TcpStream) -> Result<String, Value> {
    let mut buffer = Vec::new();
    let mut byte = [0_u8; 1];
    while buffer.len() < MAX_HTTP_HEADER_BYTES {
        stream.read_exact(&mut byte).map_err(|error| unavailable("websocket_handshake_read_failed", &error.to_string()))?;
        buffer.push(byte[0]);
        if buffer.ends_with(b"\r\n\r\n") { return String::from_utf8(buffer).map_err(|_| unavailable("websocket_handshake_invalid", "handshake is not UTF-8")); }
    }
    Err(unavailable("websocket_handshake_too_large", "handshake headers exceed limit"))
}

fn read_frame(stream: &mut TcpStream) -> Result<(u8, Vec<u8>), Value> {
    let mut header = [0_u8; 2];
    stream.read_exact(&mut header).map_err(|error| unavailable("websocket_read_failed", &error.to_string()))?;
    let opcode = header[0] & 0x0f;
    let masked = header[1] & 0x80 != 0;
    let mut length = (header[1] & 0x7f) as usize;
    if length == 126 { let mut bytes = [0_u8; 2]; stream.read_exact(&mut bytes).map_err(|error| unavailable("websocket_read_failed", &error.to_string()))?; length = u16::from_be_bytes(bytes) as usize; }
    else if length == 127 { let mut bytes = [0_u8; 8]; stream.read_exact(&mut bytes).map_err(|error| unavailable("websocket_read_failed", &error.to_string()))?; let value = u64::from_be_bytes(bytes); if value > MAX_WEBSOCKET_FRAME_BYTES as u64 { return Err(unavailable("websocket_frame_too_large", "websocket frame exceeds limit")); } length = value as usize; }
    if length > MAX_WEBSOCKET_FRAME_BYTES { return Err(unavailable("websocket_frame_too_large", "websocket frame exceeds limit")); }
    let mut mask = [0_u8; 4]; if masked { stream.read_exact(&mut mask).map_err(|error| unavailable("websocket_read_failed", &error.to_string()))?; }
    let mut payload = vec![0_u8; length]; stream.read_exact(&mut payload).map_err(|error| unavailable("websocket_read_failed", &error.to_string()))?;
    if masked { for (index, byte) in payload.iter_mut().enumerate() { *byte ^= mask[index % 4]; } }
    Ok((opcode, payload))
}

fn send_frame(stream: &mut TcpStream, value: &Value) -> Result<(), Value> {
    let body = serde_json::to_vec(value).map_err(|error| unavailable("websocket_request_encode_failed", &error.to_string()))?;
    send_control_frame(stream, 0x1, &body)
}

