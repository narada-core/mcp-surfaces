// Native adapter for the live NARS session authority.
//
// The NARS MCP surface is a projection.  It may discover session records
// locally, but delivery and authoritative status readback belong to the
// already-running session runtime.  This module speaks that runtime's small
// WebSocket control protocol directly so the native surface does not spawn a
// second session or write the session journal behind the authority's back.

use serde_json::{json, Map, Value};
use rusqlite::{Connection, OpenFlags};
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
    let requested_site = optional_text(args.get("site_id"));
    let record = read_session_record(root, &session_id, requested_site.as_deref())?;
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
    let health_response = super::probe_health(&record);
    if !health_is_healthy(&health_response) {
        return Err(json!({
            "schema": "narada.nars_session_mcp.error.v1",
            "code": "session_health_unavailable",
            "message": "session health did not confirm a live authority runtime",
            "details": { "health": health_response }
        }));
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
    let requested_site = optional_text(args.get("site_id"));
    let record = read_session_record(root, &session_id, requested_site.as_deref())?;
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

pub fn health(endpoint: &str) -> Result<Value, Value> {
    let request_id = format!("nars_health_{}", Uuid::new_v4().simple());
    ws_call(
        endpoint,
        json!({"id":request_id,"method":"session.health","params":{}}),
        WaitFor::Health,
    )
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

