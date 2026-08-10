use serde_json::{json, Map, Value};
use std::env;
use std::fs;
use std::path::{Path, PathBuf};

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
        "artifact_message_part_create" => { let id = required(args, "artifact_id")?; Ok(json!({"schema":"narada.artifacts.message_part.v1","status":"ok","message_part":{"type":"artifact_ref","artifact_id":id,"kind":args.get("kind").cloned().unwrap_or(json!("file")),"title":args.get("title").cloned().unwrap_or(Value::Null),"render_hint":args.get("render_hint").cloned().unwrap_or(json!("inline"))},"native_read_only":true})) }
        "artifact_list" | "artifact_read" => Err(authority_boundary("artifacts", name, "nars_artifact_read_authority_not_enabled_in_native_slice", "Use the owning NARS artifact endpoint or materialize a local artifact index.")),
        "artifact_register_file" | "artifact_present" => Err(authority_boundary("artifacts", name, "nars_artifact_write_authority_not_enabled_in_native_slice", "Use the owning NARS artifact authority for registration or presentation.")),
        _ => Err(error("unknown_tool", &format!("unknown_tool:{name}"))),
    }
}
fn artifact_doctor(root: &Path) -> Value { let session_id=env::var("NARADA_SESSION_ID").ok().filter(|v|!v.trim().is_empty()).or_else(||env::var("NARADA_CARRIER_SESSION_ID").ok()); let paths=session_id.as_deref().map(|id|session_index_paths(root,id)).unwrap_or_default(); let existing=paths.iter().filter(|p|p.exists()).map(|p|p.to_string_lossy().to_string()).take(4).collect::<Vec<_>>(); json!({"schema":"narada.artifacts.doctor.v1","status":if existing.is_empty(){"not_configured"}else{"ok"},"server_name":"artifacts-mcp","site_root":root.to_string_lossy(),"session_id":session_id,"session_index_paths":paths.iter().map(|p|p.to_string_lossy().to_string()).collect::<Vec<_>>(),"existing_session_indexes":existing,"native_adapter":"local_contract","external_registration":"authority_boundary"}) }

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
fn nars_list(args: &Map<String, Value>, root: &Path) -> Result<Value, Value> { let limit=args.get("limit").and_then(Value::as_u64).unwrap_or(20).clamp(1,100) as usize; let mut sessions=Vec::new(); let dirs=session_roots(root); for base in dirs.iter().take(4) { if let Ok(entries)=fs::read_dir(base) { for entry in entries.filter_map(Result::ok).take(MAX_SESSIONS) { if !entry.path().is_dir(){continue;} let Some(id)=entry.file_name().to_str().map(str::to_string) else {continue;}; if let Ok(record)=read_session(root,&id,args.get("site_id").and_then(Value::as_str)) { sessions.push(compact_session(&record,&id)); } } } } sessions.sort_by(|a,b| b.get("updated_at").and_then(Value::as_str).cmp(&a.get("updated_at").and_then(Value::as_str))); sessions.truncate(limit); Ok(json!({"schema":"narada.nars_session.sessions.v1","status":"ok","count":sessions.len(),"limit":limit,"authority_root":root.to_string_lossy(),"sessions":sessions,"native_read_only":true})) }
fn nars_show(args: &Map<String, Value>, root: &Path) -> Result<Value, Value> { let id=required(args,"session_id")?; let record=read_session(root,&id,args.get("site_id").and_then(Value::as_str))?; Ok(json!({"schema":"narada.nars_session.session.v1","status":"ok","session":record,"native_read_only":true})) }
fn input_status(args: &Map<String, Value>, root: &Path) -> Result<Value, Value> { let id=required(args,"session_id")?; let input=args.get("input_event_id").or_else(||args.get("request_id")).or_else(||args.get("directive_id")).and_then(Value::as_str); let base=session_roots(root).into_iter().find(|p|p.join(&id).is_dir()).unwrap_or_else(||root.to_path_buf()); let path=base.join(&id).join("input-status.json"); if !path.exists() { return Ok(json!({"schema":"narada.nars_session.input_status.v1","status":"not_materialized","session_id":id,"input_event_id":input,"outcome":null,"terminal_state":null,"native_read_only":true})); } let value=read_bounded_json(&path)?; Ok(json!({"schema":"narada.nars_session.input_status.v1","status":"ok","session_id":id,"input_event_id":input,"record":value,"native_read_only":true})) }
fn read_session(root: &Path, id: &str, _site_id: Option<&str>) -> Result<Value, Value> { if id.is_empty()||id.len()>160||!id.chars().all(|c|c.is_ascii_alphanumeric()||c=='-'||c=='_') { return Err(error("session_id_invalid","session_id_invalid")); } for path in session_index_paths(root,id) { if path.exists() { return read_bounded_json(&path); } } Err(error("nars_session_not_found","nars_session_not_found")) }
fn compact_session(record: &Value, id: &str) -> Value { json!({"session_id":record.get("session_id").and_then(Value::as_str).unwrap_or(id),"site_id":record.get("site_id"),"site_root":record.get("site_root"),"status":record.get("status"),"health_endpoint":record.get("health_endpoint"),"updated_at":record.get("updated_at").or_else(||record.get("last_seen_at")),"authority_epoch":record.get("authority_epoch")}) }

fn quota_call(name: &str, _args: &Map<String, Value>, root: &Path) -> Result<Value, Value> { match name { "quota_meter_guidance" => Ok(guidance_result("quota-meter", _args)), "quota_meter_overlay_status" => Ok(quota_status(root)), "quota_meter_glide_status" => Err(authority_boundary("quota-meter", name, "quota_provider_read_authority_not_enabled_in_native_slice", "Use the quota-meter provider adapter without passing credentials through MCP.")), "quota_meter_overlay_start" | "quota_meter_overlay_stop" => Err(authority_boundary("quota-meter", name, "quota_overlay_process_authority_not_enabled_in_native_slice", "Use the owning quota-meter process authority.")), _ => Err(error("unknown_tool", &format!("unknown_tool:{name}"))), } }
fn quota_status(root: &Path) -> Value { let base=if root.file_name().and_then(|v|v.to_str()).map(|v|v.eq_ignore_ascii_case(".narada")).unwrap_or(false){root.join("runtime/quota-meter")}else{root.join(".narada/runtime/quota-meter")}; let pid_path=base.join("overlay.pid"); let position_path=base.join("overlay-position.json"); let pid=fs::read_to_string(&pid_path).ok().and_then(|v|v.trim().parse::<u32>().ok()); let position=read_bounded_json(&position_path).ok(); json!({"schema":"narada.quota_meter.overlay_status.v1","status":if pid.is_some(){"unknown_liveness"}else{"stopped"},"running":null,"pid":pid,"pid_path":pid_path.to_string_lossy(),"position_path":position_path.to_string_lossy(),"position":position,"native_read_only":true}) }

fn session_roots(root: &Path) -> Vec<PathBuf> { let control=if root.file_name().and_then(|v|v.to_str()).map(|v|v.eq_ignore_ascii_case(".narada")).unwrap_or(false){root.to_path_buf()}else{root.join(".narada")}; vec![control.join("crew/nars-sessions"),root.join("crew/nars-sessions")] }
fn session_index_paths(root: &Path, id: &str) -> Vec<PathBuf> { session_roots(root).into_iter().map(|base|base.join(id).join("session-index-record.json")).collect() }
fn read_bounded_json(path: &Path) -> Result<Value, Value> { let size=fs::metadata(path).map_err(|_|error("record_not_found","record_not_found"))?.len(); if size>MAX_BYTES as u64{return Err(error("record_too_large","record_too_large"));} let text=fs::read_to_string(path).map_err(|_|error("record_read_failed","record_read_failed"))?; serde_json::from_str(&text).map_err(|_|error("record_invalid_json","record_invalid_json")) }
fn required(args: &Map<String, Value>, key: &str) -> Result<String, Value> { args.get(key).and_then(Value::as_str).map(str::trim).filter(|v|!v.is_empty()).map(str::to_string).ok_or_else(||error(&format!("{key}_required"),&format!("{key}_required"))) }
fn guidance(name: &str) -> Value { tool(name, "Show model-facing operating guidance.", json!({"type":"object","additionalProperties":false}), true) }
fn guidance_result(surface: &str, args: &Map<String, Value>) -> Value { json!({"schema":"narada.mcp_surface.guidance.v1","status":"ok","surface_id":surface,"requested":args,"native_read_only":true,"external_authority":"explicit_boundary"}) }
fn authority_boundary(surface: &str, name: &str, reason: &str, remediation: &str) -> Value { json!({"schema":format!("narada.{surface}.authority_boundary.v1"),"status":"unavailable","tool_name":name,"reason":reason,"remediation":remediation}) }
fn error(code: &str, message: &str) -> Value { json!({"schema":"narada.local_admin.error.v1","code":code,"message":message}) }
fn tool(name: &str, description: &str, schema: Value, read_only: bool) -> Value { json!({"name":name,"description":description,"inputSchema":schema,"annotations":{"title":name,"readOnlyHint":read_only,"destructiveHint":!read_only,"idempotentHint":read_only,"openWorldHint":false},"outputSchema":{"type":"object","additionalProperties":true}}) }
