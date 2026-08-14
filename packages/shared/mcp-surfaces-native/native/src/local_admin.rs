use serde_json::{json, Map, Value};
use rusqlite::{Connection, OpenFlags};
use std::env;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::Duration;
use time::{format_description::well_known::Rfc3339, OffsetDateTime};

#[path = "nars_authority.rs"]
mod nars_authority;
#[path = "artifact_authority.rs"]
mod artifact_authority;
use crate::quota_authority;

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
    let session=json!({"type":"string","minLength":1,"maxLength":160}); let artifact_id=json!({"type":"string","minLength":1,"maxLength":256}); let kind=json!({"type":"string","enum":["html","markdown","json","text","image","audio"]}); let render=json!({"type":"string","enum":["inline","link"]});
    vec![guidance("artifacts_guidance"), tool("artifacts_doctor", "Report bound native NARS artifact endpoint and session-index readiness.", json!({"type":"object","additionalProperties":false}), true), tool("artifact_register_file", "Idempotently register one Site-local file with the bound NARS artifact authority.", json!({"type":"object","properties":{"session_id":session,"path":{"type":"string","minLength":1,"maxLength":4096},"kind":kind,"title":{"type":"string","maxLength":2048},"render_hint":render,"content_type":{"type":"string","maxLength":256},"access_scope":{"type":"string","enum":["session","site"]},"idempotency_key":{"type":"string","minLength":1,"maxLength":128}},"required":["path","kind","idempotency_key"],"additionalProperties":false}), false), tool("artifact_list", "List a bounded page of artifacts in the bound NARS session.", json!({"type":"object","properties":{"session_id":session,"offset":{"type":"integer","minimum":0,"maximum":1000000},"limit":{"type":"integer","minimum":1,"maximum":200}},"additionalProperties":false}), true), tool("artifact_read", "Read one artifact metadata record from the bound NARS session.", json!({"type":"object","properties":{"session_id":session,"artifact_id":artifact_id},"required":["artifact_id"],"additionalProperties":false}), true), tool("artifact_present", "Idempotently present an artifact through the bound NARS authority.", json!({"type":"object","properties":{"session_id":session,"artifact_id":artifact_id,"text":{"type":"string","maxLength":32768},"title":{"type":"string","maxLength":2048},"render_hint":render,"idempotency_key":{"type":"string","minLength":1,"maxLength":128}},"required":["artifact_id","idempotency_key"],"additionalProperties":false}), false), tool("artifact_message_part_create", "Create a pure renderable artifact_ref message part from known metadata.", json!({"type":"object","properties":{"artifact_id":artifact_id,"kind":kind,"title":{"type":"string","maxLength":2048},"render_hint":render},"required":["artifact_id"],"additionalProperties":false}), true)]
}
fn nars_tools() -> Vec<Value> {
    vec![
        guidance("nars_session_guidance"),
        tool("nars_session_list", "List bounded local NARS session index records.", json!({"type":"object","properties":{"site_id":{"type":"string"},"limit":{"type":"integer","minimum":1,"maximum":100}},"additionalProperties":false}), true),
        tool("nars_session_show", "Show one bounded local NARS session index record.", json!({"type":"object","properties":{"site_id":{"type":"string"},"session_id":{"type":"string"}},"required":["session_id"],"additionalProperties":false}), true),
        tool("nars_session_input_deliver", "Deliver one explicit send, enqueue, or steer request to a concrete existing NARS session.", json!({"type":"object","properties":{"site_id":{"type":"string"},"session_id":{"type":"string"},"content":{"type":"string","maxLength":20000},"directive":{"type":"object","additionalProperties":true},"delivery":{"type":"string","enum":["send","enqueue","steer"]},"idempotency_key":{"type":"string","minLength":1,"maxLength":128},"expected_authority_epoch":{"type":"integer","minimum":1},"payload_ref":{"type":"string"}},"required":["session_id","delivery","idempotency_key"],"additionalProperties":false}), false),
        tool("nars_session_input_status", "Read authoritative NARS admission, request-state, terminal-state, and outcome evidence for a submitted input.", json!({"type":"object","properties":{"site_id":{"type":"string"},"session_id":{"type":"string"},"input_event_id":{"type":"string"},"request_id":{"type":"string"},"directive_id":{"type":"string"},"limit":{"type":"integer","minimum":1,"maximum":200},"payload_ref":{"type":"string"}},"required":["session_id"],"additionalProperties":false}), true),
    ]
}
fn quota_tools() -> Vec<Value> {
    let providers=json!({"type":"string","enum":["all","codex","kimi","codex,kimi","kimi,codex"],"default":"all"});
    vec![guidance("quota_meter_guidance"), tool("quota_meter_glide_status", "Read current quota windows and glide factors through native provider authorities without launching login.", json!({"type":"object","properties":{"providers":providers,"timeout_ms":{"type":"integer","minimum":100,"maximum":60000,"default":15000}},"additionalProperties":false}), true), tool("quota_meter_overlay_status", "Inspect the quota overlay process and bounded persisted telemetry.", json!({"type":"object","additionalProperties":false}), true), tool("quota_meter_overlay_start", "Idempotently start the native-refresh quota overlay.", json!({"type":"object","properties":{"providers":providers,"refresh_seconds":{"type":"integer","minimum":5,"maximum":3600,"default":60}},"additionalProperties":false}), false), tool("quota_meter_overlay_stop", "Idempotently stop the quota-meter-owned overlay.", json!({"type":"object","additionalProperties":false}), false)]
}

fn artifacts_call(name: &str, args: &Map<String, Value>, root: &Path) -> Result<Value, Value> {
    match name {
        "artifacts_guidance" => Ok(guidance_result("artifacts", args)),
        "artifacts_doctor" => Ok(artifact_doctor(root)),
        "artifact_message_part_create" => { let id = required(args, "artifact_id")?; let part = artifact_message_part(&id, args.get("kind").cloned(), args.get("title").cloned(), args.get("render_hint").cloned()); let operator_title = part.get("title").and_then(Value::as_str).unwrap_or(&id); Ok(json!({"schema":"narada.artifacts.message_part.v1","status":"ok","verification_status":"unverified","message_part":part.clone(),"assistant_content_parts":[part],"operator_message":format!("Artifact ready: {operator_title}"),"recommended_verification":"Prefer artifact_read before emitting this part when a NARS endpoint is available.","native_read_only":true})) }
        "artifact_list" => artifact_authority::list(args, root),
        "artifact_read" => artifact_authority::read(args, root),
        "artifact_register_file" => artifact_authority::register(args, root),
        "artifact_present" => artifact_authority::present(args, root),
        _ => Err(error("unknown_tool", &format!("unknown_tool:{name}"))),
    }
}
fn artifact_doctor(root: &Path) -> Value { let session_id=env::var("NARADA_SESSION_ID").ok().filter(|v|!v.trim().is_empty()).or_else(||env::var("NARADA_CARRIER_SESSION_ID").ok()); let paths=session_id.as_deref().map(|id|session_index_paths(root,id)).unwrap_or_default(); let artifact_paths=session_id.as_deref().map(|id|artifact_index_paths(root,id)).unwrap_or_default(); let existing=paths.iter().filter(|p|p.exists()).map(|p|p.to_string_lossy().to_string()).take(4).collect::<Vec<_>>(); let existing_artifact_indexes=artifact_paths.iter().filter(|p|p.exists()).map(|p|p.to_string_lossy().to_string()).take(4).collect::<Vec<_>>(); json!({"schema":"narada.artifacts.doctor.v1","status":if existing_artifact_indexes.is_empty(){"not_configured"}else{"ok"},"server_name":"artifacts-mcp","site_root":root.to_string_lossy(),"session_id":session_id,"session_index_paths":paths.iter().map(|p|p.to_string_lossy().to_string()).collect::<Vec<_>>(),"existing_session_indexes":existing,"artifact_index_paths":artifact_paths.iter().map(|p|p.to_string_lossy().to_string()).collect::<Vec<_>>(),"existing_artifact_indexes":existing_artifact_indexes,"native_adapter":"local_index_read","external_registration":"authority_boundary"}) }

fn artifact_list(args: &Map<String, Value>, root: &Path) -> Result<Value, Value> {
    let session_id = current_session_id(args)?.ok_or_else(|| error("nars_session_missing", "nars_session_missing"))?;
    let (path, index) = read_artifact_index(root, &session_id)?;
    artifact_list_projection(&session_id,&index,args,Some(&path))
}

fn artifact_read(args: &Map<String, Value>, root: &Path) -> Result<Value, Value> {
    let session_id = current_session_id(args)?.ok_or_else(|| error("nars_session_missing", "nars_session_missing"))?;
    let artifact_id = required(args, "artifact_id")?;
    let (_, index) = read_artifact_index(root, &session_id)?;
    let artifact = index.get("artifacts").and_then(Value::as_array).and_then(|items| items.iter().find(|item| item.get("artifact_id").and_then(Value::as_str) == Some(artifact_id.as_str()))).cloned().ok_or_else(|| error("artifact_not_found", "artifact_not_found"))?;
    let part = artifact_message_part(&artifact_id, artifact.get("kind").cloned(), artifact.get("title").cloned(), artifact.get("render_hint").cloned());
    Ok(json!({"schema":"narada.artifacts.read.v1","status":"ok","artifact":artifact,"message_part":part.clone(),"assistant_content_parts":[part],"operator_message":format!("Artifact ready: {}", artifact.get("title").and_then(Value::as_str).unwrap_or(&artifact_id)),"native_read_only":true}))
}

fn current_session_id(args:&Map<String,Value>)->Result<Option<String>,Value>{let requested=args.get("session_id").and_then(Value::as_str).map(str::trim).filter(|value|!value.is_empty()).map(str::to_string);let bound=env::var("NARADA_SESSION_ID").ok().filter(|value|!value.trim().is_empty()).or_else(||env::var("NARADA_CARRIER_SESSION_ID").ok().filter(|value|!value.trim().is_empty()));if let(Some(requested),Some(bound))=(requested.as_deref(),bound.as_deref()){if requested!=bound{return Err(error("artifact_session_scope_refused","artifact_session_scope_refused"));}}Ok(bound.or(requested))}
fn artifact_list_projection(session_id:&str,index:&Value,args:&Map<String,Value>,path:Option<&Path>)->Result<Value,Value>{let offset=args.get("offset").and_then(Value::as_u64).unwrap_or(0)as usize;let limit=args.get("limit").and_then(Value::as_u64).unwrap_or(50).clamp(1,200)as usize;let items=index.get("artifacts").and_then(Value::as_array).cloned().unwrap_or_default();let total=items.len();let page=items.into_iter().skip(offset).take(limit).collect::<Vec<_>>();let next=if offset+page.len()<total{Some(offset+page.len())}else{None};Ok(json!({"schema":"narada.artifacts.list.v1","status":"ok","session_id":session_id,"offset":offset,"limit":limit,"count":page.len(),"total_count":total,"next_offset":next,"items":page,"index_path":path.map(|value|value.to_string_lossy().to_string()),"native_read_only":true}))}
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
        "nars_session_input_deliver" => nars_authority::deliver(args, root),
        _ => Err(error("unknown_tool", &format!("unknown_tool:{name}"))),
    }
}
fn nars_list(args: &Map<String, Value>, root: &Path) -> Result<Value, Value> {
    let limit = args.get("limit").and_then(Value::as_u64).unwrap_or(20).clamp(1, 100) as usize;
    let site_id = args.get("site_id").and_then(Value::as_str).map(str::to_string).or_else(|| env::var("NARADA_SITE_ID").ok().filter(|value| !value.trim().is_empty()));
    if let (Some(requested), Ok(bound)) = (site_id.as_deref(), env::var("NARADA_SITE_ID")) {
        if !bound.trim().is_empty() && requested != bound { return Err(error("site_scope_refused", "site_scope_refused")); }
    }
    let include_health = args.get("include_health").and_then(Value::as_bool).unwrap_or(false);
    if user_site_scope() {
        return nars_user_site_list(args, root, site_id.as_deref(), limit, include_health);
    }
    let mut sessions = Vec::new();
    for base in session_roots(root).iter().take(4) {
        if let Ok(entries) = fs::read_dir(base) {
            for entry in entries.filter_map(Result::ok).take(MAX_SESSIONS) {
                if !entry.path().is_dir() { continue; }
                let Some(id) = entry.file_name().to_str().map(str::to_string) else { continue; };
                if let Ok(record) = read_session(root, &id, site_id.as_deref()) {
                    let health = if include_health { probe_health(&record) } else { json!({"status":"not_requested"}) };
                    sessions.push(public_session(&record, &id, root, health));
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
    let site_id = args.get("site_id").and_then(Value::as_str).map(str::to_string).or_else(|| env::var("NARADA_SITE_ID").ok().filter(|value| !value.trim().is_empty()));
    if user_site_scope() {
        return nars_user_site_show(args, root, &id, site_id.as_deref());
    }
    let record = read_session(root, &id, site_id.as_deref())?;
    let health = if args.get("include_health").and_then(Value::as_bool) == Some(false) { json!({"status":"not_requested"}) } else { probe_health(&record) };
    Ok(json!({
        "schema":"narada.nars_session_mcp.session.v1",
        "status":"ok",
        "scope":"local_site",
        "authority_root":root.to_string_lossy(),
        "scope_root":root.to_string_lossy(),
        "session":public_session(&record, &id, root, health),
        "authority":authority_summary(&record),
    }))
}
fn input_status(args: &Map<String, Value>, root: &Path) -> Result<Value, Value> {
    if args.get("input_event_id").is_some() || args.get("request_id").is_some() || args.get("directive_id").is_some() {
        return nars_authority::status(args, root);
    }
    let id=required(args,"session_id")?; let input=args.get("input_event_id").or_else(||args.get("request_id")).or_else(||args.get("directive_id")).and_then(Value::as_str); let base=session_roots(root).into_iter().find(|p|p.join(&id).is_dir()).unwrap_or_else(||root.to_path_buf()); let path=base.join(&id).join("input-status.json"); if !path.exists() { return Ok(json!({"schema":"narada.nars_session.input_status.v1","status":"not_materialized","session_id":id,"input_event_id":input,"outcome":null,"terminal_state":null,"native_read_only":true})); } let value=read_bounded_json(&path)?; Ok(json!({"schema":"narada.nars_session.input_status.v1","status":"ok","session_id":id,"input_event_id":input,"record":value,"native_read_only":true}))
}
fn read_session(root: &Path, id: &str, site_id: Option<&str>) -> Result<Value, Value> { if id.is_empty()||id.len()>160||!id.chars().all(|c|c.is_ascii_alphanumeric()||c=='-'||c=='_') { return Err(error("session_id_invalid","session_id_invalid")); } for path in session_index_paths(root,id) { if path.exists() { let record = read_bounded_json(&path)?; if record.get("session_id").and_then(Value::as_str) != Some(id) { return Err(error("session_record_mismatch","session_record_mismatch")); } if let (Some(requested), Some(actual)) = (site_id, record.get("site_id").and_then(Value::as_str)) { if requested != actual { return Err(error("site_scope_refused","site_scope_refused")); } } return Ok(record); } } Err(error("nars_session_not_found","nars_session_not_found")) }

fn user_site_scope() -> bool {
    matches!(env::var("NARADA_NARS_SESSION_SCOPE").ok().as_deref(), Some("user_site"))
        || matches!(env::var("NARADA_NARS_SESSION_PROJECTION").ok().as_deref(), Some("user-site-operator"))
}

fn nars_user_site_list(_args: &Map<String, Value>, root: &Path, requested_site: Option<&str>, limit: usize, include_health: bool) -> Result<Value, Value> {
    let (authority_root, authorities) = user_site_authorities(root)?;
    let selected = select_user_site_authorities(&authorities, requested_site)?;
    let mut sessions = Vec::new();
    for (site_id, site_root) in &selected {
        for base in session_roots(site_root).iter().take(4) {
            let Ok(entries) = fs::read_dir(base) else { continue };
            for entry in entries.filter_map(Result::ok).take(MAX_SESSIONS) {
                if !entry.path().is_dir() { continue; }
                let Some(id) = entry.file_name().to_str().map(str::to_string) else { continue; };
                let Ok(record) = read_session_from_roots(&session_roots(site_root), &id, Some(site_id)) else { continue };
                let health = if include_health { probe_health(&record) } else { json!({"status":"not_requested"}) };
                sessions.push(public_session(&record, &id, site_root, health));
                if sessions.len() >= limit { break; }
            }
            if sessions.len() >= limit { break; }
        }
        if sessions.len() >= limit { break; }
    }
    Ok(json!({
        "schema":"narada.nars_session_mcp.sessions.v1",
        "status":"ok",
        "site_id":requested_site,
        "authority_root":authority_root.to_string_lossy(),
        "scope_root":authority_root.to_string_lossy(),
        "site_root":authority_root.to_string_lossy(),
        "scope":"user_site",
        "scope_semantics":"The envelope roots identify the bound discovery authority; each session.site_root identifies that session's admitted Site root.",
        "authority_count":authorities.len(),
        "selected_site_ids":selected.iter().map(|(site_id, _)| json!(site_id)).collect::<Vec<_>>(),
        "count":sessions.len(),
        "sessions":sessions,
    }))
}

fn nars_user_site_show(args: &Map<String, Value>, root: &Path, id: &str, requested_site: Option<&str>) -> Result<Value, Value> {
    let (authority_root, authorities) = user_site_authorities(root)?;
    let selected = select_user_site_authorities(&authorities, requested_site)?;
    let mut matches = Vec::new();
    for (site_id, site_root) in selected {
        if let Ok(record) = read_session_from_roots(&session_roots(&site_root), id, Some(&site_id)) {
            matches.push((record, site_root));
        }
    }
    if matches.is_empty() { return Err(error("nars_session_not_found", "nars_session_not_found")); }
    if matches.len() > 1 { return Err(error("session_ambiguous", "session_ambiguous")); }
    let (record, site_root) = matches.remove(0);
    let health = if args.get("include_health").and_then(Value::as_bool) == Some(false) { json!({"status":"not_requested"}) } else { probe_health(&record) };
    Ok(json!({
        "schema":"narada.nars_session_mcp.session.v1",
        "status":"ok",
        "scope":"user_site",
        "authority_root":authority_root.to_string_lossy(),
        "scope_root":authority_root.to_string_lossy(),
        "session":public_session(&record, id, &site_root, health),
        "authority":authority_summary(&record),
    }))
}

fn user_site_authorities(root: &Path) -> Result<(PathBuf, Vec<(String, PathBuf)>), Value> {
    let authority_root = env::var("NARADA_USER_SITE_ROOT").ok().filter(|value| !value.trim().is_empty()).map(PathBuf::from).unwrap_or_else(|| root.to_path_buf());
    let registry_path = env::var("NARADA_SITE_REGISTRY_DB").ok().filter(|value| !value.trim().is_empty()).map(PathBuf::from).unwrap_or_else(|| authority_root.join("registry.db"));
    let size = fs::metadata(&registry_path).map_err(|_| error("user_site_registry_required", "user_site_registry_required"))?.len();
    if size > MAX_BYTES as u64 { return Err(error("user_site_registry_too_large", "user_site_registry_too_large")); }
    let connection = Connection::open_with_flags(&registry_path, OpenFlags::SQLITE_OPEN_READ_ONLY).map_err(|_| error("user_site_registry_unreadable", "user_site_registry_unreadable"))?;
    let mut statement = connection.prepare("SELECT site_id, site_root FROM site_registry ORDER BY created_at ASC, site_id ASC").map_err(|_| error("user_site_registry_unreadable", "user_site_registry_unreadable"))?;
    let rows = statement.query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))).map_err(|_| error("user_site_registry_unreadable", "user_site_registry_unreadable"))?;
    let mut authorities = Vec::new();
    for row in rows { let (site_id, site_root) = row.map_err(|_| error("user_site_registry_unreadable", "user_site_registry_unreadable"))?; if !site_id.trim().is_empty() && !site_root.trim().is_empty() { authorities.push((site_id, PathBuf::from(site_root))); } }
    if authorities.is_empty() { return Err(error("user_site_registry_empty", "user_site_registry_empty")); }
    Ok((authority_root, authorities))
}

fn select_user_site_authorities(authorities: &[(String, PathBuf)], requested: Option<&str>) -> Result<Vec<(String, PathBuf)>, Value> {
    if let Some(site_id) = requested { let selected = authorities.iter().filter(|(candidate, _)| candidate == site_id).cloned().collect::<Vec<_>>(); if selected.is_empty() { return Err(error("site_scope_refused", "site_scope_refused")); } return Ok(selected); }
    Ok(authorities.to_vec())
}

fn read_session_from_roots(roots: &[PathBuf], id: &str, site_id: Option<&str>) -> Result<Value, Value> {
    if id.is_empty() || id.len() > 160 || !id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') { return Err(error("session_id_invalid", "session_id_invalid")); }
    for base in roots {
        let path = base.join(id).join("session-index-record.json");
        if !path.exists() { continue; }
        let record = read_bounded_json(&path)?;
        if record.get("session_id").and_then(Value::as_str) != Some(id) { return Err(error("session_record_mismatch", "session_record_mismatch")); }
        if let (Some(requested), Some(actual)) = (site_id, record.get("site_id").and_then(Value::as_str)) { if requested != actual { return Err(error("site_scope_refused", "site_scope_refused")); } }
        return Ok(record);
    }
    Err(error("nars_session_not_found", "nars_session_not_found"))
}
fn public_session(record: &Value, id: &str, root: &Path, health: Value) -> Value {
    let heartbeat_path = record.get("heartbeat_path").and_then(Value::as_str).map(str::to_string).or_else(|| record.get("session_dir").and_then(Value::as_str).map(|directory| PathBuf::from(directory).join("heartbeat.json").to_string_lossy().to_string()));
    let heartbeat_value = heartbeat_path.as_deref().and_then(|path| read_bounded_json(Path::new(path)).ok()).and_then(|value| value.get("heartbeat_at").or_else(|| value.get("last_written_at")).or_else(|| value.get("timestamp")).cloned());
    let heartbeat_age_ms = heartbeat_value.as_ref().and_then(timestamp_ms).map(|timestamp| (now_ms() - timestamp).max(0));
    let heartbeat_fresh = heartbeat_age_ms.map(|age| age <= 30_000).unwrap_or(false);
    let health_status = health.get("status").and_then(Value::as_str).map(|value| value.to_ascii_lowercase());
    let terminal_state = record.get("terminal_state").cloned().unwrap_or(Value::Null);
    let (display_state, display_state_reason, liveness_source) = match health_status.as_deref() {
        Some("healthy") => ("active", "health_probe_succeeded", "health_probe"),
        Some("starting") => ("starting_or_degraded", "health_probe_starting", "health_probe"),
        Some("degraded") => ("starting_or_degraded", "health_probe_degraded", "health_probe"),
        Some("closing") => ("closing", "health_probe_closing", "health_probe"),
        _ if terminal_state.as_str() == Some("closed") => ("closed", "terminal_state_closed", "session_index_and_heartbeat"),
        Some("unhealthy") => ("unhealthy", "health_probe_unhealthy", "health_probe"),
        Some("unavailable") => ("unavailable", "health_probe_unavailable", "health_probe"),
        _ if heartbeat_fresh => ("starting_or_degraded", "fresh_heartbeat_without_health", "heartbeat"),
        _ if heartbeat_age_ms.is_some() || record.get("status_hint").and_then(Value::as_str) == Some("alive") => ("stale", "stale_or_missing_liveness", "session_index_and_heartbeat"),
        _ => ("historical", "historical_record_only", "session_index"),
    };
    let health_observed_at = health.get("health_observed_at").cloned().unwrap_or(Value::Null);
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
        "heartbeat_fresh":heartbeat_fresh,
        "heartbeat_age_ms":heartbeat_age_ms,
        "health_observed_at":health_observed_at.clone(),
        "liveness":{"source":liveness_source,"observed_at":health_observed_at,"heartbeat_path":heartbeat_path,"heartbeat_at":heartbeat_value.unwrap_or(Value::Null),"heartbeat_age_ms":heartbeat_age_ms,"heartbeat_fresh":heartbeat_fresh},
        "terminal_state":terminal_state,
        "health":health,
        "event_endpoint_available":record.get("event_endpoint").and_then(Value::as_str).map(|value| !value.is_empty()).unwrap_or(false),
        "health_endpoint_available":record.get("health_endpoint").and_then(Value::as_str).map(|value| !value.is_empty()).unwrap_or(false),
        "authority":authority,
    })
}

fn probe_health(record: &Value) -> Value {
    let Some(endpoint) = record.get("health_endpoint").and_then(Value::as_str).filter(|value| !value.trim().is_empty()) else {
        if let Some(endpoint) = record.get("event_endpoint").and_then(Value::as_str).filter(|value| !value.trim().is_empty()) {
            return probe_event_health(endpoint);
        }
        return json!({"status":"unavailable","probe_status":"unavailable","reason":"session_health_endpoint_missing","health_observed_at":super::now_iso(),"health_source":"endpoint_missing"});
    };
    let timeout_ms = env::var("NARADA_NARS_SESSION_HEALTH_TIMEOUT_MS").ok().and_then(|value| value.parse::<u64>().ok()).unwrap_or(1500).clamp(250, 5000);
    let agent = ureq::AgentBuilder::new().timeout(Duration::from_millis(timeout_ms)).build();
    let response = match agent.get(endpoint).call() {
        Ok(response) => response,
        Err(ureq::Error::Status(_, response)) => response,
        Err(error) => return json!({"status":"unavailable","probe_status":"unreachable","reason":error.to_string(),"health_observed_at":super::now_iso(),"health_source":"health_endpoint"}),
    };
    let status_code = response.status();
    let http_ok = (200..400).contains(&status_code);
    let body = bounded_response_json(response).unwrap_or_else(|_| json!({}));
    let fallback = if http_ok { "healthy" } else { "unhealthy" };
    let semantic = match body.get("status").and_then(Value::as_str).map(|value| value.to_ascii_lowercase()).as_deref() {
        Some("starting") | Some("healthy") | Some("degraded") | Some("unhealthy") | Some("closing") | Some("unavailable") => body.get("status").and_then(Value::as_str).unwrap_or(fallback),
        _ => fallback,
    };
    let mut result = body.as_object().cloned().unwrap_or_default();
    result.insert("status".into(), json!(semantic));
    result.insert("http_status".into(), json!(status_code));
    result.insert("http_ok".into(), json!(http_ok));
    result.insert("probe_status".into(), json!("reachable"));
    result.insert("health_observed_at".into(), json!(super::now_iso()));
    result.insert("health_source".into(), json!("health_endpoint"));
    Value::Object(result)
}

fn probe_event_health(endpoint: &str) -> Value {
    let response = match nars_authority::health(endpoint) {
        Ok(value) => value,
        Err(error) => {
            let reason = error.get("message").cloned().unwrap_or_else(|| json!("event health probe failed"));
            return json!({"status":"unavailable","probe_status":"unreachable","reason":reason,"health_observed_at":super::now_iso(),"health_source":"event_endpoint"});
        }
    };
    let mut result = response.as_object().cloned().unwrap_or_default();
    let status = match result.get("status").and_then(Value::as_str).map(|value| value.to_ascii_lowercase()).as_deref() {
        Some("starting") | Some("healthy") | Some("degraded") | Some("unhealthy") | Some("closing") | Some("unavailable") => result.get("status").and_then(Value::as_str).unwrap_or("healthy"),
        _ => "healthy",
    };
    result.insert("status".into(), json!(status));
    result.insert("probe_status".into(), json!("reachable"));
    result.insert("health_observed_at".into(), json!(super::now_iso()));
    result.insert("health_source".into(), json!("event_endpoint"));
    Value::Object(result)
}

fn bounded_response_json(response: ureq::Response) -> Result<Value, Value> {
    let mut bytes = Vec::new();
    response.into_reader().take(MAX_BYTES as u64 + 1).read_to_end(&mut bytes).map_err(|_| error("health_response_read_failed", "health_response_read_failed"))?;
    if bytes.len() > MAX_BYTES { return Err(error("health_response_too_large", "health_response_too_large")); }
    if bytes.is_empty() { return Ok(json!({})); }
    serde_json::from_slice(&bytes).map_err(|_| error("health_response_not_json", "health_response_not_json"))
}

fn timestamp_ms(value: &Value) -> Option<i64> {
    let text = value.as_str()?;
    OffsetDateTime::parse(text, &Rfc3339).ok().map(|timestamp| (timestamp.unix_timestamp_nanos() / 1_000_000) as i64)
}

fn now_ms() -> i64 { (OffsetDateTime::now_utc().unix_timestamp_nanos() / 1_000_000) as i64 }

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

fn quota_call(name: &str, args: &Map<String, Value>, root: &Path) -> Result<Value, Value> { match name { "quota_meter_guidance" => Ok(quota_authority::guidance(root)), "quota_meter_glide_status" => quota_authority::glide_status(args), "quota_meter_overlay_status" => Ok(quota_authority::overlay_status(root)), "quota_meter_overlay_start" => quota_authority::overlay_start(args,root), "quota_meter_overlay_stop" => quota_authority::overlay_stop(root), _ => Err(error("unknown_tool", &format!("unknown_tool:{name}"))), } }
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
        assert_eq!(list["items"][0]["artifact_id"], "artifact-1");
        assert_eq!(list["total_count"], 1);
        let read = artifact_read(&Map::from_iter([(String::from("session_id"), json!("session-1")), (String::from("artifact_id"), json!("artifact-1"))]), &root).expect("read");
        assert_eq!(read["artifact"]["title"], "Read me");
        assert_eq!(read["message_part"]["type"], "artifact_ref");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn nars_session_liveness_projects_fresh_heartbeat_without_health_probe() {
        let root = std::env::temp_dir().join(format!("narada-nars-liveness-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&root).expect("root");
        let heartbeat = root.join("heartbeat.json");
        fs::write(&heartbeat, format!(r#"{{"heartbeat_at":"{}"}}"#, super::super::now_iso())).expect("heartbeat");
        let record = json!({
            "session_id":"session-liveness",
            "session_dir":root.to_string_lossy(),
            "status_hint":"alive",
            "terminal_state":null,
            "site_root":root.to_string_lossy(),
        });
        let projected = public_session(&record, "session-liveness", &root, json!({"status":"not_requested"}));
        assert_eq!(projected["display_state"], "starting_or_degraded");
        assert_eq!(projected["display_state_reason"], "fresh_heartbeat_without_health");
        assert_eq!(projected["heartbeat_fresh"], true);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn nars_user_site_scope_reads_only_registry_admitted_sites() {
        let user_root = std::env::temp_dir().join(format!("narada-user-site-{}", uuid::Uuid::new_v4()));
        let site_root = std::env::temp_dir().join(format!("narada-user-child-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&user_root).expect("user root");
        fs::create_dir_all(site_root.join(".narada/crew/nars-sessions/session-user")).expect("session root");
        let connection = Connection::open(user_root.join("registry.db")).expect("registry");
        connection.execute_batch("CREATE TABLE site_registry (site_id TEXT NOT NULL, site_root TEXT NOT NULL, created_at TEXT NOT NULL);").expect("schema");
        connection.execute("INSERT INTO site_registry (site_id, site_root, created_at) VALUES (?1, ?2, ?3)", rusqlite::params!["fixture-site", site_root.to_string_lossy(), "2026-01-01T00:00:00Z"]).expect("row");
        drop(connection);
        fs::write(site_root.join(".narada/crew/nars-sessions/session-user/session-index-record.json"), json!({"session_id":"session-user","site_id":"fixture-site","site_root":site_root.to_string_lossy()}).to_string()).expect("record");
        env::set_var("NARADA_NARS_SESSION_SCOPE", "user_site");
        env::set_var("NARADA_USER_SITE_ROOT", &user_root);
        let listed = nars_list(&Map::from_iter([(String::from("limit"), json!(10))]), &user_root).expect("list");
        assert_eq!(listed["scope"], "user_site");
        assert_eq!(listed["authority_root"], user_root.to_string_lossy().to_string());
        assert_eq!(listed["sessions"][0]["site_id"], "fixture-site");
        let shown = nars_show(&Map::from_iter([(String::from("session_id"), json!("session-user")), (String::from("site_id"), json!("fixture-site"))]), &user_root).expect("show");
        assert_eq!(shown["session"]["site_root"], site_root.to_string_lossy().to_string());
        env::remove_var("NARADA_NARS_SESSION_SCOPE");
        env::remove_var("NARADA_USER_SITE_ROOT");
        let _ = fs::remove_dir_all(user_root);
        let _ = fs::remove_dir_all(site_root);
    }
}
