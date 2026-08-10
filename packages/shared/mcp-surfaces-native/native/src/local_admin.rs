use serde_json::{json, Map, Value};
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

const MAX_BYTES: usize = 256_000;
const MAX_SESSIONS: usize = 100;

pub fn list_tools(surface_id: &str) -> Vec<Value> {
    match surface_id {
        "artifacts" => artifact_tools(),
        "nars-session" => nars_tools(),
        "quota-meter" => quota_tools(),
        _ => Vec::new(),
    }
}

pub fn auxiliary(surface_id: &str, method: &str, params: &Map<String, Value>) -> Result<Value, Value> {
    match method {
        "prompts/list" => {
            let (name, title, description) = match surface_id {
                "artifacts" => ("artifacts_workflow", "Artifacts Workflow", "Inspect local artifact/session posture before registration or presentation."),
                "nars-session" => ("nars_session_workflow", "NARS Session Workflow", "Discover and inspect bounded NARS session records before delivery."),
                "quota-meter" => ("quota_meter_workflow", "Quota Meter Workflow", "Inspect local quota overlay posture before provider reads or overlay control."),
                _ => return Err(error("unsupported_surface", "unsupported_surface")),
            };
            Ok(json!({"prompts":[{"name":name,"title":title,"description":description,"arguments":[]}]}))
        }
        "prompts/get" => {
            let expected = match surface_id { "artifacts" => "artifacts_workflow", "nars-session" => "nars_session_workflow", "quota-meter" => "quota_meter_workflow", _ => "" };
            if params.get("name").and_then(Value::as_str) != Some(expected) { return Err(error("unknown_prompt", "unknown_prompt")); }
            Ok(json!({"description":"Use bounded native inspection before delegating external or runtime authority.","messages":[{"role":"user","content":{"type":"text","text":"Inspect the local doctor/status tool first. Native read slices do not transmit credentials or perform external runtime writes."}}]}))
        }
        "completion/complete" => {
            let values = if params.get("argument").and_then(Value::as_object).and_then(|v| v.get("name")).and_then(Value::as_str) == Some("name") { list_tools(surface_id).iter().filter_map(|v| v.get("name").cloned()).take(100).collect::<Vec<_>>() } else { Vec::new() };
            Ok(json!({"completion":{"values":values,"total":values.len(),"hasMore":false}}))
        }
        "logging/setLevel" => Ok(json!({})),
        _ => Err(error("unsupported_mcp_method", &format!("unsupported_mcp_method:{method}"))),
    }
}

pub fn call_tool(surface_id: &str, name: &str, args: &Map<String, Value>, root: &Path) -> Result<Value, Value> {
    match surface_id {
        "artifacts" => artifacts_call(name, args, root),
        "nars-session" => nars_call(name, args, root),
        "quota-meter" => quota_call(name, args, root),
        _ => Err(error("unknown_surface", &format!("unknown_surface:{surface_id}"))),
    }
}

fn artifact_tools() -> Vec<Value> {
    vec![guidance("artifacts_guidance"), tool("artifacts_doctor", "Report local NARS artifact endpoint and session-index readiness.", json!({"type":"object","additionalProperties":false}), true), tool("artifact_register_file", "Register a local file with the owning NARS artifact authority.", json!({"type":"object","properties":{"path":{"type":"string"},"kind":{"type":"string"},"title":{"type":"string"},"render_hint":{"type":"string"}},"required":["path","kind"],"additionalProperties":false}), false), tool("artifact_list", "Read artifacts registered in the current NARS session when a local index is available.", json!({"type":"object","additionalProperties":false}), true), tool("artifact_read", "Read one artifact metadata record from the local NARS session index.", json!({"type":"object","properties":{"artifact_id":{"type":"string"}},"required":["artifact_id"],"additionalProperties":false}), true), tool("artifact_present", "Ask the owning NARS authority to present an artifact.", json!({"type":"object","properties":{"artifact_id":{"type":"string"},"text":{"type":"string"},"title":{"type":"string"},"render_hint":{"type":"string"}},"required":["artifact_id"],"additionalProperties":false}), false), tool("artifact_message_part_create", "Create a pure renderable artifact_ref message part from known metadata.", json!({"type":"object","properties":{"artifact_id":{"type":"string"},"kind":{"type":"string"},"title":{"type":"string"},"render_hint":{"type":"string"}},"required":["artifact_id"],"additionalProperties":false}), true)]
}
fn nars_tools() -> Vec<Value> {
    vec![guidance("nars_session_guidance"), tool("nars_session_list", "List bounded local NARS session index records.", json!({"type":"object","properties":{"site_id":{"type":"string"},"limit":{"type":"integer","minimum":1,"maximum":100}},"additionalProperties":false}), true), tool("nars_session_show", "Show one bounded local NARS session index record.", json!({"type":"object","properties":{"site_id":{"type":"string"},"session_id":{"type":"string"}},"required":["session_id"],"additionalProperties":false}), true), tool("nars_session_input_deliver", "Deliver input to an existing NARS session through the owning runtime authority.", json!({"type":"object","additionalProperties":true}), false), tool("nars_session_input_status", "Read bounded local input status evidence when materialized.", json!({"type":"object","properties":{"site_id":{"type":"string"},"session_id":{"type":"string"},"input_event_id":{"type":"string"},"request_id":{"type":"string"},"directive_id":{"type":"string"}},"required":["session_id"],"additionalProperties":false}), true)]
}
fn quota_tools() -> Vec<Value> {
    vec![guidance("quota_meter_guidance"), tool("quota_meter_glide_status", "Inspect quota provider posture without launching provider login.", json!({"type":"object","properties":{"providers":{"type":"string"}},"additionalProperties":false}), true), tool("quota_meter_overlay_status", "Inspect local quota overlay pid and position state.", json!({"type":"object","additionalProperties":false}), true), tool("quota_meter_overlay_start", "Start the quota overlay through its owning runtime authority.", json!({"type":"object","additionalProperties":true}), false), tool("quota_meter_overlay_stop", "Stop the quota overlay through its owning runtime authority.", json!({"type":"object","additionalProperties":true}), false)]
}

fn artifacts_call(name: &str, args: &Map<String, Value>, root: &Path) -> Result<Value, Value> {
    match name {
        "artifacts_guidance" => Ok(guidance_result("artifacts", args)),
        "artifacts_doctor" => Ok(artifact_doctor(root)),
        "artifact_message_part_create" => { let id = required(args, "artifact_id")?; let part = artifact_message_part(&id, args.get("kind").cloned(), args.get("title").cloned(), args.get("render_hint").cloned()); let operator_title = part.get("title").and_then(Value::as_str).unwrap_or(&id); Ok(json!({"schema":"narada.artifacts.message_part.v1","status":"ok","verification_status":"unverified","message_part":part.clone(),"assistant_content_parts":[part],"operator_message":format!("Artifact ready: {operator_title}"),"recommended_verification":"Prefer artifact_read before emitting this part when a NARS endpoint is available.","native_read_only":true})) }
        "artifact_list" => artifact_list(args, root),
        "artifact_read" => artifact_read(args, root),
        "artifact_register_file" | "artifact_present" => Err(authority_boundary("artifacts", name, "nars_artifact_write_authority_not_enabled_in_native_slice", "Use the owning NARS artifact authority for registration or presentation.")),
        _ => Err(error("unknown_tool", &format!("unknown_tool:{name}"))),
    }
}
fn artifact_doctor(root: &Path) -> Value { let session_id=env::var("NARADA_SESSION_ID").ok().filter(|v|!v.trim().is_empty()).or_else(||env::var("NARADA_CARRIER_SESSION_ID").ok()); let paths=session_id.as_deref().map(|id|session_index_paths(root,id)).unwrap_or_default(); let artifact_paths=session_id.as_deref().map(|id|artifact_index_paths(root,id)).unwrap_or_default(); let existing=paths.iter().filter(|p|p.exists()).map(|p|p.to_string_lossy().to_string()).take(4).collect::<Vec<_>>(); let existing_artifact_indexes=artifact_paths.iter().filter(|p|p.exists()).map(|p|p.to_string_lossy().to_string()).take(4).collect::<Vec<_>>(); json!({"schema":"narada.artifacts.doctor.v1","status":if existing_artifact_indexes.is_empty(){"not_configured"}else{"ok"},"server_name":"artifacts-mcp","site_root":root.to_string_lossy(),"session_id":session_id,"session_index_paths":paths.iter().map(|p|p.to_string_lossy().to_string()).collect::<Vec<_>>(),"existing_session_indexes":existing,"artifact_index_paths":artifact_paths.iter().map(|p|p.to_string_lossy().to_string()).collect::<Vec<_>>(),"existing_artifact_indexes":existing_artifact_indexes,"native_adapter":"local_index_read","external_registration":"authority_boundary"}) }

fn artifact_list(args: &Map<String, Value>, root: &Path) -> Result<Value, Value> {
    let session_id = current_session_id(args).ok_or_else(|| error("nars_session_missing", "nars_session_missing"))?;
    let (path, index) = read_artifact_index(root, &session_id)?;
    Ok(json!({"schema":"narada.artifacts.list.v1","status":"ok","session_id":session_id,"index":index,"index_path":path.to_string_lossy(),"native_read_only":true}))
}

fn artifact_read(args: &Map<String, Value>, root: &Path) -> Result<Value, Value> {
    let session_id = current_session_id(args).ok_or_else(|| error("nars_session_missing", "nars_session_missing"))?;
    let artifact_id = required(args, "artifact_id")?;
    let (_, index) = read_artifact_index(root, &session_id)?;
    let artifact = index.get("artifacts").and_then(Value::as_array).and_then(|items| items.iter().find(|item| item.get("artifact_id").and_then(Value::as_str) == Some(artifact_id.as_str()))).cloned().ok_or_else(|| error("artifact_not_found", "artifact_not_found"))?;
    let part = artifact_message_part(&artifact_id, artifact.get("kind").cloned(), artifact.get("title").cloned(), artifact.get("render_hint").cloned());
    Ok(json!({"schema":"narada.artifacts.read.v1","status":"ok","artifact":artifact,"message_part":part.clone(),"assistant_content_parts":[part],"operator_message":format!("Artifact ready: {}", artifact.get("title").and_then(Value::as_str).unwrap_or(&artifact_id)),"native_read_only":true}))
}

fn current_session_id(args: &Map<String, Value>) -> Option<String> { args.get("session_id").and_then(Value::as_str).map(str::trim).filter(|value|!value.is_empty()).map(str::to_string).or_else(||env::var("NARADA_SESSION_ID").ok().filter(|value|!value.trim().is_empty())).or_else(||env::var("NARADA_CARRIER_SESSION_ID").ok().filter(|value|!value.trim().is_empty())) }
fn artifact_index_paths(root: &Path, id: &str) -> Vec<PathBuf> { session_index_paths(root,id).into_iter().filter_map(|path|path.parent().map(|parent|parent.join("artifacts/index.json"))).collect() }
fn read_artifact_index(root: &Path, id: &str) -> Result<(PathBuf, Value), Value> { if id.is_empty()||id.len()>160||!id.chars().all(|c|c.is_ascii_alphanumeric()||c=='-'||c=='_') { return Err(error("session_id_invalid", "session_id_invalid")); } for path in artifact_index_paths(root,id) { if path.exists() { return Ok((path.clone(), read_bounded_json(&path)?)); } } Err(error("artifact_index_not_found", "artifact_index_not_found")) }
fn artifact_message_part(id: &str, kind: Option<Value>, title: Option<Value>, render_hint: Option<Value>) -> Value {
    let mut part = Map::new();
    part.insert("type".into(), json!("artifact_ref"));
    part.insert("artifact_id".into(), json!(id));
    if let Some(value) = kind.and_then(|value| value.as_str().map(str::trim).filter(|value| !value.is_empty()).map(|value| value.to_ascii_lowercase())) { part.insert("kind".into(), json!(value)); }
    if let Some(value) = title.and_then(|value| value.as_str().map(str::trim).filter(|value| !value.is_empty()).map(str::to_string)) { part.insert("title".into(), json!(value)); }
    let render_hint = render_hint.and_then(|value| value.as_str().map(str::trim).filter(|value| !value.is_empty()).map(|value| value.to_ascii_lowercase())).unwrap_or_else(|| "inline".into());
    part.insert("render_hint".into(), json!(render_hint));
    Value::Object(part)
}

fn nars_call(name: &str, args: &Map<String, Value>, root: &Path) -> Result<Value, Value> {
    match name {
        "nars_session_guidance" => Ok(guidance_result("nars-session", args)),
        "nars_session_list" => nars_list(args, root),
        "nars_session_show" => nars_show(args, root),
        "nars_session_input_status" => input_status(args, root),
        "nars_session_input_deliver" => Err(authority_boundary("nars-session", name, "nars_input_authority_not_enabled_in_native_slice", "Use the owning NARS runtime for delivery.")),
        _ => Err(error("unknown_tool", &format!("unknown_tool:{name}"))),
    }
}
fn nars_list(args: &Map<String, Value>, root: &Path) -> Result<Value, Value> {
    let limit = args.get("limit").and_then(Value::as_u64).unwrap_or(20).clamp(1, 100) as usize;
    let site_id = args.get("site_id").and_then(Value::as_str).map(str::to_string).or_else(|| env::var("NARADA_SITE_ID").ok().filter(|value| !value.trim().is_empty()));
    let mut sessions = Vec::new();
    for base in session_roots(root).iter().take(4) {
        if let Ok(entries) = fs::read_dir(base) {
            for entry in entries.filter_map(Result::ok).take(MAX_SESSIONS) {
                if !entry.path().is_dir() { continue; }
                let Some(id) = entry.file_name().to_str().map(str::to_string) else { continue; };
                if let Ok(record) = read_session(root, &id, site_id.as_deref()) {
                    sessions.push(public_session(&record, &id, root, json!({"status":"not_requested"})));
                }
            }
        }
    }
    sessions.sort_by(|left, right| right.get("last_seen_at").and_then(Value::as_str).cmp(&left.get("last_seen_at").and_then(Value::as_str)));
    sessions.truncate(limit);
    Ok(json!({
        "schema":"narada.nars_session_mcp.sessions.v1",
        "status":"ok",
        "site_id":site_id,
        "authority_root":root.to_string_lossy(),
        "scope_root":root.to_string_lossy(),
        "site_root":root.to_string_lossy(),
        "scope":"local_site",
        "scope_semantics":"The envelope roots identify the bound discovery authority; each session.site_root identifies that session's admitted Site root.",
        "authority_count":1,
        "selected_site_ids":[site_id],
        "count":sessions.len(),
        "sessions":sessions,
    }))
}

fn nars_show(args: &Map<String, Value>, root: &Path) -> Result<Value, Value> {
    let id = required(args, "session_id")?;
    let record = read_session(root, &id, args.get("site_id").and_then(Value::as_str))?;
    Ok(json!({
        "schema":"narada.nars_session_mcp.session.v1",
        "status":"ok",
        "scope":"local_site",
        "authority_root":root.to_string_lossy(),
        "scope_root":root.to_string_lossy(),
        "session":public_session(&record, &id, root, json!({"status":"not_requested"})),
        "authority":authority_summary(&record),
    }))
}
fn input_status(args: &Map<String, Value>, root: &Path) -> Result<Value, Value> { let id=required(args,"session_id")?; let input=args.get("input_event_id").or_else(||args.get("request_id")).or_else(||args.get("directive_id")).and_then(Value::as_str); let base=session_roots(root).into_iter().find(|p|p.join(&id).is_dir()).unwrap_or_else(||root.to_path_buf()); let path=base.join(&id).join("input-status.json"); if !path.exists() { return Ok(json!({"schema":"narada.nars_session.input_status.v1","status":"not_materialized","session_id":id,"input_event_id":input,"outcome":null,"terminal_state":null,"native_read_only":true})); } let value=read_bounded_json(&path)?; Ok(json!({"schema":"narada.nars_session.input_status.v1","status":"ok","session_id":id,"input_event_id":input,"record":value,"native_read_only":true})) }
fn read_session(root: &Path, id: &str, _site_id: Option<&str>) -> Result<Value, Value> { if id.is_empty()||id.len()>160||!id.chars().all(|c|c.is_ascii_alphanumeric()||c=='-'||c=='_') { return Err(error("session_id_invalid","session_id_invalid")); } for path in session_index_paths(root,id) { if path.exists() { return read_bounded_json(&path); } } Err(error("nars_session_not_found","nars_session_not_found")) }
fn public_session(record: &Value, id: &str, root: &Path, health: Value) -> Value {
    let heartbeat_path = record.get("heartbeat_path").and_then(Value::as_str).map(str::to_string).or_else(|| record.get("session_dir").and_then(Value::as_str).map(|directory| PathBuf::from(directory).join("heartbeat.json").to_string_lossy().to_string()));
    let heartbeat_value = heartbeat_path.as_deref().and_then(|path| read_bounded_json(Path::new(path)).ok()).and_then(|value| value.get("heartbeat_at").or_else(|| value.get("last_written_at")).or_else(|| value.get("timestamp")).cloned());
    let terminal_state = record.get("terminal_state").cloned().unwrap_or(Value::Null);
    let (display_state, display_state_reason, liveness_source) = if terminal_state.as_str() == Some("closed") {
        ("closed", "terminal_state_closed", "session_index_and_heartbeat")
    } else {
        ("historical", "historical_record_only", "session_index")
    };
    let authority = authority_summary(record);
    json!({
        "session_id":record.get("session_id").and_then(Value::as_str).unwrap_or(id),
        "carrier_session_id":record.get("carrier_session_id").or_else(||record.get("session_id")).cloned().unwrap_or_else(||json!(id)),
        "nars_session_id":record.get("nars_session_id").or_else(||record.get("session_id")).cloned().unwrap_or_else(||json!(id)),
        "site_id":record.get("site_id").cloned().unwrap_or(Value::Null),
        "site_root":record.get("site_root").cloned().unwrap_or_else(||json!(root.to_string_lossy())),
        "agent_id":record.get("agent_id").cloned().unwrap_or(Value::Null),
        "runtime_kind":record.get("runtime_kind").cloned().unwrap_or(Value::Null),
        "launch_operator_surface_kind":record.get("launch_operator_surface_kind").cloned().unwrap_or(Value::Null),
        "display_state":display_state,
        "display_state_reason":display_state_reason,
        "persisted_display_state":record.get("display_state").cloned().unwrap_or(Value::Null),
        "status_hint":record.get("status_hint").cloned().unwrap_or(Value::Null),
        "started_at":record.get("started_at").cloned().unwrap_or(Value::Null),
        "last_seen_at":record.get("last_seen_at").cloned().unwrap_or(Value::Null),
        "last_seen_source":"session_index_projection",
        "heartbeat_at":heartbeat_value.clone().unwrap_or(Value::Null),
        "heartbeat_fresh":false,
        "heartbeat_age_ms":Value::Null,
        "health_observed_at":Value::Null,
        "liveness":{"source":liveness_source,"observed_at":Value::Null,"heartbeat_path":heartbeat_path,"heartbeat_at":heartbeat_value.unwrap_or(Value::Null),"heartbeat_age_ms":Value::Null,"heartbeat_fresh":false},
        "terminal_state":terminal_state,
        "health":health,
        "event_endpoint_available":record.get("event_endpoint").and_then(Value::as_str).map(|value| !value.is_empty()).unwrap_or(false),
        "health_endpoint_available":record.get("health_endpoint").and_then(Value::as_str).map(|value| !value.is_empty()).unwrap_or(false),
        "authority":authority,
    })
}

fn authority_summary(record: &Value) -> Value {
    json!({
        "authority_runtime_id":record.get("authority_runtime_id").cloned().unwrap_or(Value::Null),
        "authority_epoch":record.get("authority_epoch").cloned().unwrap_or(Value::Null),
        "source_write_admission":record.get("source_write_admission").cloned().unwrap_or(Value::Null),
        "authority_transition_state":record.get("authority_transition_state").cloned().unwrap_or(Value::Null),
        "superseded_by_session_id":record.get("superseded_by_session_id").cloned().unwrap_or(Value::Null),
        "authority_locator_ref":record.get("authority_locator_ref").cloned().unwrap_or(Value::Null),
    })
}

fn quota_call(name: &str, _args: &Map<String, Value>, root: &Path) -> Result<Value, Value> { match name { "quota_meter_guidance" => Ok(guidance_result("quota-meter", _args)), "quota_meter_overlay_status" => Ok(quota_status(root)), "quota_meter_glide_status" => Err(authority_boundary("quota-meter", name, "quota_provider_read_authority_not_enabled_in_native_slice", "Use the quota-meter provider adapter without passing credentials through MCP.")), "quota_meter_overlay_start" | "quota_meter_overlay_stop" => Err(authority_boundary("quota-meter", name, "quota_overlay_process_authority_not_enabled_in_native_slice", "Use the owning quota-meter process authority.")), _ => Err(error("unknown_tool", &format!("unknown_tool:{name}"))), } }
fn quota_status(root: &Path) -> Value {
    let base = quota_state_root(root);
    quota_status_at(&base)
}

fn quota_status_at(base: &Path) -> Value {
    let pid_path = base.join("overlay.pid");
    let position_path = base.join("overlay-position.json");
    let pid = fs::read_to_string(&pid_path).ok().and_then(|value| value.trim().parse::<u32>().ok()).filter(|value| *value > 0);
    let running = pid.map(quota_process_alive).unwrap_or(false);
    json!({
        "schema":"narada.quota_meter.overlay_status.v1",
        "status":if running { "running" } else if pid.is_some() { "stale" } else { "stopped" },
        "running":running,
        "pid":pid,
        "pid_path":pid_path.to_string_lossy(),
        "position_path":position_path.to_string_lossy(),
        "position":quota_position(&position_path),
    })
}

fn quota_state_root(root: &Path) -> PathBuf {
    if let Ok(value) = env::var("QUOTA_METER_STATE_ROOT") {
        if !value.trim().is_empty() { return PathBuf::from(value); }
    }
    let base = env::var("LOCALAPPDATA")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .or_else(|| env::var("TEMP").ok().filter(|value| !value.trim().is_empty()))
        .or_else(|| env::var("TMP").ok().filter(|value| !value.trim().is_empty()))
        .unwrap_or_else(|| root.to_string_lossy().to_string());
    PathBuf::from(base).join("quota-meter")
}

fn quota_position(path: &Path) -> Value {
    let Ok(value) = read_bounded_json(path) else { return Value::Null; };
    let Some(object) = value.as_object() else { return Value::Null; };
    json!({
        "left":object.get("left").cloned().unwrap_or(Value::Null),
        "top":object.get("top").cloned().unwrap_or(Value::Null),
        "updated_at":object.get("updatedAt").cloned().unwrap_or(Value::Null),
    })
}

fn quota_process_alive(pid: u32) -> bool {
    #[cfg(windows)]
    {
        let filter = format!("PID eq {pid}");
        let needle = format!("\"{pid}\"");
        return Command::new("tasklist")
            .args(["/FI", filter.as_str(), "/FO", "CSV", "/NH"])
            .output()
            .ok()
            .filter(|output| output.status.success())
            .and_then(|output| String::from_utf8(output.stdout).ok())
            .map(|output| output.contains(&needle))
            .unwrap_or(false);
    }
    #[cfg(unix)]
    {
        return Command::new("kill")
            .args(["-0", &pid.to_string()])
            .status()
            .map(|status| status.success())
            .unwrap_or(false);
    }
    #[cfg(not(any(windows, unix)))]
    {
        let _ = pid;
        false
    }
}

fn session_roots(root: &Path) -> Vec<PathBuf> { let control=if root.file_name().and_then(|v|v.to_str()).map(|v|v.eq_ignore_ascii_case(".narada")).unwrap_or(false){root.to_path_buf()}else{root.join(".narada")}; vec![control.join("crew/nars-sessions"),root.join("crew/nars-sessions")] }
fn session_index_paths(root: &Path, id: &str) -> Vec<PathBuf> { session_roots(root).into_iter().map(|base|base.join(id).join("session-index-record.json")).collect() }
fn read_bounded_json(path: &Path) -> Result<Value, Value> { let size=fs::metadata(path).map_err(|_|error("record_not_found","record_not_found"))?.len(); if size>MAX_BYTES as u64{return Err(error("record_too_large","record_too_large"));} let text=fs::read_to_string(path).map_err(|_|error("record_read_failed","record_read_failed"))?; serde_json::from_str(&text).map_err(|_|error("record_invalid_json","record_invalid_json")) }
fn required(args: &Map<String, Value>, key: &str) -> Result<String, Value> { args.get(key).and_then(Value::as_str).map(str::trim).filter(|v|!v.is_empty()).map(str::to_string).ok_or_else(||error(&format!("{key}_required"),&format!("{key}_required"))) }
fn guidance(name: &str) -> Value { tool(name, "Show model-facing operating guidance.", json!({"type":"object","additionalProperties":false}), true) }
fn guidance_result(surface: &str, args: &Map<String, Value>) -> Value { json!({"schema":"narada.mcp_surface.guidance.v1","status":"ok","surface_id":surface,"requested":args,"native_read_only":true,"external_authority":"explicit_boundary"}) }
fn authority_boundary(surface: &str, name: &str, reason: &str, remediation: &str) -> Value { json!({"schema":format!("narada.{surface}.authority_boundary.v1"),"status":"unavailable","tool_name":name,"reason":reason,"remediation":remediation}) }
fn error(code: &str, message: &str) -> Value { json!({"schema":"narada.local_admin.error.v1","code":code,"message":message}) }
fn tool(name: &str, description: &str, schema: Value, read_only: bool) -> Value { json!({"name":name,"description":description,"inputSchema":schema,"annotations":{"title":name,"readOnlyHint":read_only,"destructiveHint":!read_only,"idempotentHint":read_only,"openWorldHint":false},"outputSchema":{"type":"object","additionalProperties":true}}) }

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn artifact_reads_use_the_local_bounded_index() {
        let suffix = SystemTime::now().duration_since(UNIX_EPOCH).expect("clock").as_nanos();
        let root = std::env::temp_dir().join(format!("narada-artifact-index-{suffix}"));
        let index_path = root.join(".narada/crew/nars-sessions/session-1/artifacts/index.json");
        fs::create_dir_all(index_path.parent().expect("parent")).expect("directory");
        fs::write(&index_path, r#"{"schema":"narada.nars.artifact_index.v1","artifacts":[{"artifact_id":"artifact-1","kind":"markdown","title":"Read me","render_hint":"inline"}]}"#).expect("write");
        let list = artifact_list(&Map::from_iter([(String::from("session_id"), json!("session-1"))]), &root).expect("list");
        assert_eq!(list["status"], "ok");
        assert_eq!(list["index"]["artifacts"][0]["artifact_id"], "artifact-1");
        let read = artifact_read(&Map::from_iter([(String::from("session_id"), json!("session-1")), (String::from("artifact_id"), json!("artifact-1"))]), &root).expect("read");
        assert_eq!(read["artifact"]["title"], "Read me");
        assert_eq!(read["message_part"]["type"], "artifact_ref");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn quota_status_projects_position_without_process_authority() {
        let suffix = SystemTime::now().duration_since(UNIX_EPOCH).expect("clock").as_nanos();
        let root = std::env::temp_dir().join(format!("narada-quota-status-{suffix}"));
        fs::create_dir_all(&root).expect("directory");
        fs::write(root.join("overlay-position.json"), r#"{"left":42,"top":24,"updatedAt":"2026-08-09T00:00:00Z"}"#).expect("position");
        let response = quota_status_at(&root);
        assert_eq!(response["schema"], "narada.quota_meter.overlay_status.v1");
        assert_eq!(response["status"], "stopped");
        assert_eq!(response["running"], false);
        assert_eq!(response["position"]["left"], 42);
        assert_eq!(response["position"]["updated_at"], "2026-08-09T00:00:00Z");
        let _ = fs::remove_dir_all(root);
    }
}
