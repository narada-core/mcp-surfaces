//! Native adapter for the live NARS session authority.
//!
//! The NARS MCP surface is a projection.  It may discover session records
//! locally, but delivery and authoritative status readback belong to the
//! already-running session runtime.  This module speaks that runtime's small
//! WebSocket control protocol directly so the native surface does not spawn a
//! second session or write the session journal behind the authority's back.

use serde_json::{json, Map, Value};
use std::io::{Read, Write};
use std::net::{Shutdown, TcpStream, ToSocketAddrs};
use std::path::{Path, PathBuf};
use std::time::Duration;
use uuid::Uuid;

const MAX_RECORD_BYTES: usize = 256 * 1024;
const MAX_HTTP_HEADER_BYTES: usize = 64 * 1024;
const MAX_WEBSOCKET_FRAME_BYTES: usize = 4 * 1024 * 1024;
const MAX_INLINE_CONTENT: usize = 20_000;
const DEFAULT_TIMEOUT_MS: u64 = 5_000;

pub fn deliver(args: &Map<String, Value>, root: &Path) -> Result<Value, Value> {
    let session_id = required(args, "session_id")?;
    let record = read_session_record(root, &session_id)?;
    assert_requested_site(args, &record)?;
    let authority = assert_writable_authority(args, &record)?;
    let delivery = delivery(args)?;
    if delivery == "steer" && !env_true("NARADA_NARS_SESSION_ALLOW_STEER") {
        return Err(error("steer_not_admitted", "steer delivery is disabled by site policy"));
    }
    let content = directive_content(args)?;
    let idempotency_key = required(args, "idempotency_key")?;
    if idempotency_key.len() > 128 {
        return Err(error("idempotency_key_too_large", "idempotency_key exceeds 128 characters"));
    }

    let input_event_id = format!("input_{}", Uuid::new_v4().simple());
    let request_id = format!("nars_input_request_{}", Uuid::new_v4().simple());
    let directive_id = format!("dir_nars_input_{}", Uuid::new_v4().simple());
    let site_id = record
        .get("site_id")
        .and_then(Value::as_str)
        .map(str::to_string)
        .or_else(|| std::env::var("NARADA_SITE_ID").ok().filter(|value| !value.trim().is_empty()));
    let source_kind = std::env::var("NARADA_NARS_SESSION_SOURCE_KIND")
        .ok()
        .filter(|value| matches!(value.as_str(), "agent" | "operator"))
        .unwrap_or_else(|| "agent".to_string());
    let source_id = std::env::var("NARADA_NARS_SESSION_SOURCE_ID")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .or_else(|| std::env::var("NARADA_AGENT_ID").ok().filter(|value| !value.trim().is_empty()))
        .unwrap_or_else(|| "narada-native-surface".to_string());
    let source = if source_kind == "agent" { "agent_control" } else { "operator_control" };
    let delivery_mode = if delivery == "send" {
        "admit_for_current_turn"
    } else {
        "admit_after_active_turn"
    };
    let authority_ref = format!(
        "nars-session-mcp:{}:{}:{}",
        site_id.as_deref().unwrap_or("site"),
        session_id,
        authority.epoch
    );
    let caller_carrier_session_id = std::env::var("NARADA_CARRIER_SESSION_ID")
        .ok()
        .filter(|value| !value.trim().is_empty());
    let input = json!({
        "schema": "narada.carrier.input_event.v1",
        "event_id": input_event_id,
        "request_id": request_id,
        "source_kind": source_kind,
        "source_id": source_id,
        "source": source,
        "transport": "carrier_server_api",
        "delivery_mode": delivery_mode,
        "hold_condition": Value::Null,
        "content": content,
        "authority_ref": authority_ref,
        "directive_id": directive_id,
        "metadata": {
            "input_source": "nars_session_mcp",
            "agent_control_input": source_kind == "agent",
            "directive_provenance": {
                "kind": if source_kind == "agent" { "agent_directive_surface" } else { "explicit_operator_directive_surface" },
                "surface_id": "nars-session-mcp"
            },
            "nars_session_input": {
                "delivery_constructor": delivery,
                "idempotency_key": idempotency_key,
                "target_session_id": session_id,
                "target_site_id": site_id,
                "authority_epoch": authority.epoch,
                "authority_runtime_id": authority.runtime_id,
                "caller_carrier_session_id": caller_carrier_session_id
            }
        },
        "idempotency_key": idempotency_key,
        "created_at": now_iso()
    });
    let mut params = Map::new();
    params.insert("content".into(), input["content"].clone());
    for key in [
        "event_id",
        "request_id",
        "source",
        "source_kind",
        "source_id",
        "transport",
        "delivery_mode",
        "hold_condition",
        "authority_ref",
        "directive_id",
        "metadata",
        "idempotency_key",
        "created_at",
    ] {
        if let Some(value) = input.get(key) {
            params.insert(key.to_string(), value.clone());
        }
    }
    let response = ws_call(
        &authority.event_endpoint,
        json!({ "id": request_id, "method": "session.submit", "params": params }),
        WaitFor::Delivery,
    )?;
    let event = event_name(&response).unwrap_or("session_control_accepted");
    Ok(json!({
        "schema": "narada.nars_session_mcp.input_delivery.v1",
        "status": "admitted",
        "admission": if event == "input_event_queued" { "queued" } else { "accepted" },
        "site_id": site_id,
        "session_id": session_id,
        "request_id": request_id,
        "input_event_id": input["event_id"],
        "directive_id": input["directive_id"],
        "delivery": delivery,
        "protocol_method": "carrier.input.deliver",
        "authority": authority.value,
        "queue_state": "queued_for_turn_boundary",
        "evidence": { "event": event, "request_id": input["request_id"], "source": input["source_id"], "idempotency_key": idempotency_key },
        "native_authority": true
    }))
}

pub fn status(args: &Map<String, Value>, root: &Path) -> Result<Value, Value> {
    let session_id = required(args, "session_id")?;
    let record = read_session_record(root, &session_id)?;
    assert_requested_site(args, &record)?;
    let authority = authority_from_record(&record)?;
    let input_event_id = optional_text(args.get("input_event_id"));
    let request_id = optional_text(args.get("request_id"));
    let directive_id = optional_text(args.get("directive_id"));
    if input_event_id.is_none() && request_id.is_none() && directive_id.is_none() {
        return Err(error("input_status_selector_required", "input_event_id, request_id, or directive_id is required"));
    }
    let mut any_of = Map::new();
    if let Some(value) = &input_event_id { any_of.insert("input_event_id".into(), json!(value)); }
    if let Some(value) = &request_id { any_of.insert("request_id".into(), json!(value)); }
    if let Some(value) = &directive_id { any_of.insert("directive_id".into(), json!(value)); }
    let limit = args.get("limit").and_then(Value::as_u64).unwrap_or(100).clamp(1, 200);
    let read_id = format!("nars_input_status_{}", Uuid::new_v4().simple());
    let response = ws_call(
        &authority.event_endpoint,
        json!({
            "id": read_id,
            "method": "session.events.read",
            "params": { "direction": "backward", "limit": limit, "filters": { "any_of": any_of } }
        }),
        WaitFor::EventsRead,
    )?;
    let raw_events = response.get("events").and_then(Value::as_array).cloned().unwrap_or_default();
    let events = raw_events
        .into_iter()
        .filter(|event| matches_event(event, input_event_id.as_deref(), request_id.as_deref(), directive_id.as_deref()))
        .collect::<Vec<_>>();
    let summary = summarize_events(&events);
    let corrupt_line_count = response.get("corrupt_line_count").and_then(Value::as_u64).unwrap_or(0);
    let has_more = response.get("has_more").and_then(Value::as_bool).unwrap_or(false);
    let evidence_complete = !has_more && corrupt_line_count == 0;
    Ok(json!({
        "schema": "narada.nars_session_mcp.input_status.v1",
        "status": summary.status,
        "status_semantics": "admission",
        "admission_status": summary.admission_status,
        "terminal_state": summary.terminal_state,
        "request_state": summary.request_state,
        "outcome": summary.outcome,
        "outcome_reason": summary.outcome_reason,
        "terminal_event": summary.terminal_event,
        "site_id": record.get("site_id").cloned().unwrap_or(Value::Null),
        "session_id": session_id,
        "selectors": { "input_event_id": input_event_id, "request_id": request_id, "directive_id": directive_id },
        "events": events,
        "evidence_complete": evidence_complete,
        "history_truncated": !evidence_complete,
        "corrupt_line_count": corrupt_line_count,
        "evidence": { "source": response.get("source").cloned().unwrap_or_else(|| json!("events_jsonl")), "complete": evidence_complete, "has_more": has_more, "event_count": response.get("event_count").cloned().unwrap_or_else(|| json!(0)), "cursor": response.get("cursor").cloned().unwrap_or(Value::Null), "filters": { "any_of": any_of } },
        "authority": authority.value,
        "native_authority": true
    }))
}

#[derive(Clone)]
struct Authority {
    event_endpoint: String,
    runtime_id: Value,
    epoch: i64,
    value: Value,
}

fn assert_writable_authority(args: &Map<String, Value>, record: &Value) -> Result<Authority, Value> {
    let authority = authority_from_record(record)?;
    if record.get("terminal_state").and_then(Value::as_str) == Some("closed") {
        return Err(error("session_closed", "session is closed"));
    }
    if record.get("superseded_by_session_id").and_then(Value::as_str).is_some() {
        return Err(error("session_superseded", "session has been superseded"));
    }
    if record.get("source_write_admission").and_then(Value::as_str) != Some("active") {
        return Err(error("session_authority_not_writable", "session source write admission is not active"));
    }
    if let Some(expected) = args.get("expected_authority_epoch").and_then(Value::as_i64) {
        if expected != authority.epoch {
            return Err(error("authority_epoch_mismatch", "session authority epoch changed"));
        }
    }
    if std::env::var("NARADA_NARS_SESSION_SOURCE_ID").ok().or_else(|| std::env::var("NARADA_AGENT_ID").ok()).filter(|value| !value.trim().is_empty()).is_none() {
        return Err(error("caller_identity_required", "caller identity is required"));
    }
    Ok(authority)
}

fn authority_from_record(record: &Value) -> Result<Authority, Value> {
    let endpoint = record.get("event_endpoint").and_then(Value::as_str).map(str::trim).filter(|value| !value.is_empty()).ok_or_else(|| error("session_event_endpoint_missing", "session has no live event endpoint"))?;
    let epoch = record.get("authority_epoch").and_then(Value::as_i64).unwrap_or(1);
    if epoch < 1 { return Err(error("session_authority_epoch_missing", "session authority epoch is missing or invalid")); }
    let runtime_id = record.get("authority_runtime_id").cloned().unwrap_or_else(|| json!(null));
    if runtime_id.as_str().map(str::trim).filter(|value| !value.is_empty()).is_none() {
        return Err(error("session_authority_runtime_missing", "session authority runtime identity is missing"));
    }
    Ok(Authority { event_endpoint: endpoint.to_string(), runtime_id, epoch, value: authority_summary(record) })
}

fn read_session_record(root: &Path, id: &str) -> Result<Value, Value> {
    if id.is_empty() || id.len() > 128 || !id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') {
        return Err(error("session_id_invalid", "session_id_invalid"));
    }
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
    Err(error("session_not_found", "session_not_found"))
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

enum WaitFor { Delivery, EventsRead }

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
                let matches = match wait_for { WaitFor::Delivery => matches!(name, "input_event_queued" | "input_event_started" | "input_admitted_to_turn" | "session_control_accepted" | "input_completed" | "user_message" | "turn_started" | "error" | "websocket_error"), WaitFor::EventsRead => name == "session_events_read" };
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

fn send_control_frame(stream: &mut TcpStream, opcode: u8, body: &[u8]) -> Result<(), Value> {
    if body.len() > MAX_WEBSOCKET_FRAME_BYTES { return Err(unavailable("websocket_frame_too_large", "request frame exceeds limit")); }
    let mask = Uuid::new_v4().as_bytes()[..4].to_vec();
    let mut header = Vec::new(); header.push(0x80 | opcode);
    if body.len() < 126 { header.push(0x80 | body.len() as u8); }
    else if body.len() < 65_536 { header.push(0x80 | 126); header.extend_from_slice(&(body.len() as u16).to_be_bytes()); }
    else { header.push(0x80 | 127); header.extend_from_slice(&(body.len() as u64).to_be_bytes()); }
    header.extend_from_slice(&mask);
    let masked = body.iter().enumerate().map(|(index, byte)| byte ^ mask[index % 4]).collect::<Vec<_>>();
    stream.write_all(&header).and_then(|_| stream.write_all(&masked)).and_then(|_| stream.flush()).map_err(|error| unavailable("websocket_write_failed", &error.to_string()))
}

fn timeout() -> Duration { Duration::from_millis(std::env::var("NARADA_NARS_SESSION_REQUEST_TIMEOUT_MS").ok().and_then(|value| value.parse().ok()).unwrap_or(DEFAULT_TIMEOUT_MS).clamp(500, 30_000)) }
fn optional_text(value: Option<&Value>) -> Option<String> { value.and_then(Value::as_str).map(str::trim).filter(|value| !value.is_empty()).map(str::to_string) }
fn required(args: &Map<String, Value>, key: &str) -> Result<String, Value> { optional_text(args.get(key)).ok_or_else(|| error(&format!("{key}_required"), &format!("{key} is required"))) }
fn env_true(name: &str) -> bool { matches!(std::env::var(name).ok().as_deref(), Some("1" | "true" | "TRUE" | "yes")) }
fn authority_summary(record: &Value) -> Value { json!({ "authority_runtime_id": record.get("authority_runtime_id").cloned().unwrap_or(Value::Null), "authority_epoch": record.get("authority_epoch").cloned().unwrap_or_else(|| json!(1)), "source_write_admission": record.get("source_write_admission").cloned().unwrap_or(Value::Null), "authority_transition_state": record.get("authority_transition_state").cloned().unwrap_or(Value::Null), "superseded_by_session_id": record.get("superseded_by_session_id").cloned().unwrap_or(Value::Null), "authority_locator_ref": record.get("authority_locator_ref").cloned().unwrap_or(Value::Null) }) }
fn error(code: &str, message: &str) -> Value { json!({ "schema": "narada.nars_session_mcp.error.v1", "code": code, "message": message, "details": {} }) }
fn unavailable(reason: &str, detail: &str) -> Value { json!({ "schema": "narada.nars_session_mcp.error.v1", "code": reason, "message": detail, "details": { "reason": reason, "detail": detail } }) }
fn now_iso() -> String { time::OffsetDateTime::now_utc().format(&time::format_description::well_known::Rfc3339).unwrap_or_else(|_| "1970-01-01T00:00:00Z".to_string()) }

fn sha1(input: &[u8]) -> [u8; 20] {
    let mut message = input.to_vec(); let bit_len = (message.len() as u64) * 8; message.push(0x80); while message.len() % 64 != 56 { message.push(0); } message.extend_from_slice(&bit_len.to_be_bytes());
    let mut h = [0x67452301_u32, 0xefcdab89, 0x98badcfe, 0x10325476, 0xc3d2e1f0];
    for chunk in message.chunks_exact(64) {
        let mut words = [0_u32; 80]; for index in 0..16 { words[index] = u32::from_be_bytes([chunk[index*4], chunk[index*4+1], chunk[index*4+2], chunk[index*4+3]]); }
        for index in 16..80 { words[index] = (words[index-3] ^ words[index-8] ^ words[index-14] ^ words[index-16]).rotate_left(1); }
        let (mut a, mut b, mut c, mut d, mut e) = (h[0], h[1], h[2], h[3], h[4]);
        for index in 0..80 { let (f, k) = if index < 20 { ((b & c) | ((!b) & d), 0x5a827999) } else if index < 40 { (b ^ c ^ d, 0x6ed9eba1) } else if index < 60 { ((b & c) | (b & d) | (c & d), 0x8f1bbcdc) } else { (b ^ c ^ d, 0xca62c1d6) }; let temp = a.rotate_left(5).wrapping_add(f).wrapping_add(e).wrapping_add(k).wrapping_add(words[index]); e = d; d = c; c = b.rotate_left(30); b = a; a = temp; }
        h[0] = h[0].wrapping_add(a); h[1] = h[1].wrapping_add(b); h[2] = h[2].wrapping_add(c); h[3] = h[3].wrapping_add(d); h[4] = h[4].wrapping_add(e);
    }
    let mut output = [0_u8; 20]; for (index, value) in h.iter().enumerate() { output[index*4..index*4+4].copy_from_slice(&value.to_be_bytes()); } output
}

fn base64_encode(bytes: &[u8]) -> String { const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/"; let mut output = String::new(); let mut index = 0; while index < bytes.len() { let a = bytes[index]; let b = bytes.get(index+1).copied().unwrap_or(0); let c = bytes.get(index+2).copied().unwrap_or(0); output.push(TABLE[(a >> 2) as usize] as char); output.push(TABLE[(((a & 3) << 4) | (b >> 4)) as usize] as char); output.push(if index + 1 < bytes.len() { TABLE[((b & 15) << 2 | (c >> 6)) as usize] as char } else { '=' }); output.push(if index + 2 < bytes.len() { TABLE[(c & 63) as usize] as char } else { '=' }); index += 3; } output }

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn websocket_handshake_hash_matches_rfc_example() {
        assert_eq!(
            base64_encode(&sha1(b"The quick brown fox jumps over the lazy dog")),
            "L9ThxnotKPzthJ7hu3bnORuT6xI="
        );
    }

    #[test]
    fn endpoint_parser_accepts_bounded_local_ws_endpoint() {
        let endpoint = WsEndpoint::parse("ws://127.0.0.1:4123/events").expect("endpoint");
        assert_eq!(endpoint.host, "127.0.0.1");
        assert_eq!(endpoint.port, 4123);
        assert_eq!(endpoint.path, "/events");
        assert!(WsEndpoint::parse("http://127.0.0.1:4123/events").is_err());
    }

    #[test]
    fn status_summary_prefers_terminal_evidence() {
        let summary = summarize_events(&[
            json!({"event":"input_event_queued"}),
            json!({"event":"input_event_started"}),
            json!({"event":"turn_complete", "terminal_state":"completed"}),
        ]);
        assert_eq!(summary.status, "admitted_to_turn");
        assert_eq!(summary.outcome, "completed");
        assert_eq!(summary.terminal_state.as_deref(), Some("completed"));
    }
}
