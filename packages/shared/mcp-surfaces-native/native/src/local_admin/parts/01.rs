use serde_json::{json, Map, Value};
use rusqlite::{Connection, OpenFlags};
use std::env;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::Duration;
use time::{format_description::well_known::Rfc3339, OffsetDateTime};

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
    let site_id=||json!({"type":"string","minLength":1,"maxLength":160});
    let session_id=||json!({"type":"string","minLength":1,"maxLength":128,"pattern":"^[A-Za-z0-9_-]+$"});
    let selector=||json!({"type":"string","minLength":1,"maxLength":256});
    let mut deliver=tool("nars_session_input_deliver", "Idempotently deliver one explicit send, enqueue, or policy-admitted steer request to a concrete live NARS session.", json!({"type":"object","properties":{"site_id":site_id(),"session_id":session_id(),"content":{"type":"string","minLength":1,"maxLength":20000},"directive":{"type":"object","properties":{"content":{"type":"object","properties":{"text":{"type":"string","minLength":1,"maxLength":20000}},"required":["text"],"additionalProperties":false}},"required":["content"],"additionalProperties":false},"delivery":{"type":"string","enum":["send","enqueue","steer"]},"idempotency_key":{"type":"string","minLength":1,"maxLength":128},"expected_authority_epoch":{"type":"integer","minimum":1}},"required":["session_id","delivery","idempotency_key"],"anyOf":[{"required":["content"]},{"required":["directive"]}],"additionalProperties":false}), false);
    deliver["annotations"]["idempotentHint"]=json!(true);
    deliver["annotations"]["destructiveHint"]=json!(false);
    vec![
        guidance("nars_session_guidance"),
        tool("nars_session_list", "List bounded local NARS session index records.", json!({"type":"object","properties":{"site_id":site_id(),"limit":{"type":"integer","minimum":1,"maximum":100},"include_health":{"type":"boolean"}},"additionalProperties":false}), true),
        tool("nars_session_show", "Show one bounded local NARS session index record.", json!({"type":"object","properties":{"site_id":site_id(),"session_id":session_id(),"include_health":{"type":"boolean"}},"required":["session_id"],"additionalProperties":false}), true),
        deliver,
        tool("nars_session_input_status", "Read authoritative NARS admission, request-state, terminal-state, and outcome evidence for a submitted input; omit selectors only for legacy materialized status readback.", json!({"type":"object","properties":{"site_id":site_id(),"session_id":session_id(),"input_event_id":selector(),"request_id":selector(),"directive_id":selector(),"limit":{"type":"integer","minimum":1,"maximum":200}},"required":["session_id"],"additionalProperties":false}), true),
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

