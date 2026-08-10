use serde_json::{json, Map, Value};
use std::env;
use std::fs;
use std::path::{Path, PathBuf};

const MAX_BYTES: u64 = 256_000;
const BROWSER: &[(&str, bool)] = &[
    ("browser_control_session_inventory", true), ("browser_control_attach", false),
    ("browser_control_status", true), ("browser_control_navigate", false),
    ("browser_control_accessibility_snapshot", true), ("browser_control_screenshot", true),
    ("browser_control_click", false), ("browser_control_fill", false), ("browser_control_wait", false),
    ("browser_control_assert", true), ("browser_control_detach", false), ("mcp_output_show", true),
];
const OPERATOR: &[(&str, bool)] = &[
    ("operator_console_overlay_status", true), ("operator_console_overlay_open", false),
    ("operator_console_overlay_refresh", false), ("operator_console_overlay_close", false),
];
const CLOUDFLARE: &[(&str, bool)] = &[
    ("cloudflare_product_read", true), ("cloudflare_session_status", true),
    ("cloudflare_health", true), ("cloudflare_doctor", true), ("cloudflare_carrier_health", true),
];
const SPEECH: &[(&str, bool)] = &[
    ("speech_speak", false), ("speech_voices", true), ("speech_listen_status", true),
    ("speech_capture_transcribe", false), ("speech_prompt_capture_response", false),
    ("speech_listen_start", false), ("speech_listen_stop", false),
];
const SCHEDULER: &[(&str, bool)] = &[
    ("scheduler_runtime_status", true), ("scheduler_task_list", true), ("scheduler_task_show", true),
    ("scheduler_task_create", false), ("scheduler_task_delete", false), ("scheduler_task_update_action", false),
    ("scheduler_task_enable", false), ("scheduler_task_disable", false), ("scheduler_task_stop", false),
    ("scheduler_task_run", false), ("scheduler_task_history", true), ("scheduler_activation_doctor", true),
    ("scheduler_activation_prepare", false), ("scheduler_binding_list", true), ("scheduler_binding_show", true),
    ("scheduler_binding_upsert", false), ("scheduler_event_show", true), ("scheduler_event_admit", false),
    ("scheduler_activation_list", true), ("scheduler_activation_claim", false), ("scheduler_activation_admit_sop", false),
    ("scheduler_activation_fail", false), ("scheduler_activation_resolve", false), ("scheduler_activation_unblock", false),
];
const GRAPH_MAIL: &[(&str, bool)] = &[
    ("graph_mail_doctor", true), ("graph_mail_auth_device_code_start", false), ("graph_mail_auth_device_code_poll", false),
    ("graph_mail_auth_status", true), ("graph_mail_auth_clear", false), ("graph_mail_query", true),
    ("graph_mail_message_show", true), ("graph_mail_folder_list", true), ("graph_mail_folder_create", false),
    ("graph_mail_message_move", false), ("graph_mail_message_mark_read", false), ("graph_mail_attachment_list", true),
    ("graph_mail_attachment_get", true), ("graph_mail_attachment_download_file", false), ("graph_mail_attachment_add", false),
    ("graph_mail_attachment_upload_session_create", false), ("graph_mail_attachment_upload_chunk", false),
    ("graph_mail_attachment_upload_file", false), ("graph_mail_attachment_delete", false), ("graph_mail_draft_create", false),
    ("graph_mail_reply_draft_create", false), ("graph_mail_reply_all_draft_create", false), ("graph_mail_forward_draft_create", false),
    ("graph_mail_reply_all_to_last_in_thread_draft_create", false), ("graph_mail_ticket_draft_upsert", false),
    ("graph_mail_ticket_draft_discard", false), ("graph_mail_ticket_draft_disposition_scan", false),
    ("graph_mail_ticket_draft_disposition_list", true), ("graph_mail_ticket_draft_disposition_ack", false),
    ("graph_mail_draft_update", false), ("graph_mail_draft_discard", false), ("graph_mail_draft_send", false),
    ("graph_mail_output_show", true),
];

pub fn list_tools(surface_id: &str) -> Vec<Value> {
    let mut tools = vec![guidance(surface_id)];
    for (name, read_only) in entries(surface_id) { tools.push(tool(name, description(surface_id, name), *read_only)); }
    tools
}

pub fn auxiliary(surface_id: &str, method: &str, params: &Map<String, Value>) -> Result<Value, Value> {
    match method {
        "prompts/list" => Ok(json!({"prompts":[{"name":format!("{}_workflow", surface_id.replace('-', "_")),"title":format!("{} Workflow", surface_id),"description":"Inspect native status and authority posture before host/provider operations.","arguments":[]}]})),
        "prompts/get" => {
            let expected = format!("{}_workflow", surface_id.replace('-', "_"));
            if params.get("name").and_then(Value::as_str) != Some(expected.as_str()) { return Err(error("unknown_prompt", "unknown_prompt")); }
            Ok(json!({"description":"Inspect native status and authority posture before host/provider operations.","messages":[{"role":"user","content":{"type":"text","text":"Use the read-only status/doctor tool first. Native contract mode never transmits credentials, drives an external browser/provider, or mutates the host scheduler/process."}}]}))
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
    if name.ends_with("_guidance") { return Ok(guidance_result(surface_id, args)); }
    match (surface_id, name) {
        ("operator-console-overlay", "operator_console_overlay_status") => Ok(operator_status(root)),
        ("cloudflare-carrier", "cloudflare_doctor") => Ok(cloudflare_doctor(root)),
        ("cloudflare-carrier", "cloudflare_session_status") => Ok(cloudflare_session_status(args, root)),
        ("cloudflare-carrier", "cloudflare_health") => cloudflare_health(args, root),
        ("speech", "speech_listen_status") => Ok(json!({"schema":"narada.speech.listen_status.v1","status":"not_active","active_sessions":[],"native_read_only":true})),
        ("speech", "speech_voices") => Err(boundary(surface_id, name, "speech_provider_authority_not_enabled_in_native_slice", "Use the registry-resolved speech adapter.")),
        ("scheduler", "scheduler_runtime_status") => Ok(json!({"schema":"narada.scheduler.runtime_status.v1","status":"authority_boundary","implementation":"rust-native-contract","native_task_scheduler":false,"native_read_only":true})),
        ("graph-mail", "graph_mail_doctor") => Ok(graph_mail_doctor(root)),
        ("graph-mail", "graph_mail_auth_status") => Ok(graph_mail_auth_status(root)),
        ("browser-control", "browser_control_session_inventory") => Ok(json!({"schema":"narada.browser_control.session_inventory.v1","status":"not_injected","sessions":[],"native_read_only":true})),
        _ => Err(boundary(surface_id, name, "external_or_host_authority_not_enabled_in_native_contract", "Use the configured owning surface authority for this operation.")),
    }
}

fn entries(surface_id: &str) -> &'static [(&'static str, bool)] { match surface_id { "browser-control" => BROWSER, "operator-console-overlay" => OPERATOR, "cloudflare-carrier" => CLOUDFLARE, "speech" => SPEECH, "scheduler" => SCHEDULER, "graph-mail" => GRAPH_MAIL, _ => &[] } }
fn guidance(surface_id: &str) -> Value { tool(&format!("{}_guidance", surface_id.replace('-', "_")), format!("Show model-facing operating guidance for {surface_id} MCP workflows."), true) }
fn guidance_result(surface_id: &str, args: &Map<String, Value>) -> Value { json!({"schema":"narada.host_surface.guidance.v1","status":"ok","surface_id":surface_id,"requested":args,"native_contract":"status probes and explicit authority boundaries","native_read_only":true}) }
fn description(surface_id: &str, name: &str) -> String { format!("Native {surface_id} contract for {name}; external authority remains explicit.") }
fn operator_status(root: &Path) -> Value { let state_root=env::var("NARADA_WINDOW_SURFACE_OVERLAY_STATE_ROOT").ok().unwrap_or_else(||root.join(".narada/runtime/operator-console-overlay").to_string_lossy().to_string()); let state_path=Path::new(&state_root).join("overlay-state.json"); let metadata=metadata(&state_path); json!({"schema":"narada.operator_console_overlay.status.v1","status":if metadata.is_some(){"present"}else{"not_active"},"state_root_configured":true,"state_file_present":metadata.is_some(),"state_file":file_meta(metadata),"native_read_only":true}) }
fn cloudflare_doctor(root: &Path) -> Value {
    let empty = Map::new();
    let session = cloudflare_session_status(&empty, root);
    let (health_path, health_configured) = cloudflare_path(&empty, "health_file", root, "CLOUDFLARE_HEALTH_FILE", "cloudflare_health.json");
    let health_status = match bounded_json(&health_path) {
        Ok(Some(value)) => value.get("status").cloned().unwrap_or_else(|| json!("unknown")),
        Ok(None) => json!("missing"),
        Err(_) => json!("invalid_json"),
    };
    json!({"schema":"narada.cloudflare_carrier.doctor.v1","status":"ok","site_root":root.to_string_lossy(),"session_status":session.get("status"),"session_fresh":session.get("is_fresh"),"session_file_configured":cloudflare_path(&empty, "session_file", root, "CLOUDFLARE_SESSION_FILE", "cloudflare_session.json").1,"health_file_configured":health_configured,"health_status":health_status,"native_external_api":false,"native_read_only":true})
}

fn cloudflare_session_status(args: &Map<String, Value>, root: &Path) -> Value {
    let (path, _) = cloudflare_path(args, "session_file", root, "CLOUDFLARE_SESSION_FILE", "cloudflare_session.json");
    let Some(metadata) = metadata(&path) else {
        return json!({"status":"missing","session_file":path.to_string_lossy(),"has_cookie":false,"is_fresh":false});
    };
    let age_minutes = metadata.modified().ok().and_then(|modified| modified.elapsed().ok()).map(|age| age.as_secs() / 60);
    let is_fresh = age_minutes.map(|age| age < 60).unwrap_or(false);
    let value = match bounded_json(&path) {
        Ok(Some(value)) => value,
        Ok(None) => return json!({"status":"missing","session_file":path.to_string_lossy(),"has_cookie":false,"is_fresh":false}),
        Err(_) => return json!({"status":"invalid_json","session_file":path.to_string_lossy(),"has_cookie":false,"is_fresh":false,"age_minutes":age_minutes}),
    };
    let has_cookie = value.get("cookie").and_then(Value::as_str).map(|cookie| !cookie.is_empty()).unwrap_or(false);
    json!({"status":if has_cookie {"present"} else {"incomplete"},"session_file":path.to_string_lossy(),"has_cookie":has_cookie,"captured_at":value.get("captured_at").cloned().unwrap_or(Value::Null),"worker_url":value.get("worker_url").cloned().unwrap_or(Value::Null),"principal":value.get("principal").cloned().unwrap_or(Value::Null),"age_minutes":age_minutes,"is_fresh":is_fresh,"size_bytes":metadata.len()})
}

fn cloudflare_health(args: &Map<String, Value>, root: &Path) -> Result<Value, Value> {
    let (path, _) = cloudflare_path(args, "health_file", root, "CLOUDFLARE_HEALTH_FILE", "cloudflare_health.json");
    let Some(value) = bounded_json(&path).map_err(|detail| error("cloudflare_health_parse_failed", detail))? else {
        return Ok(json!({"status":"missing","health_file":path.to_string_lossy()}));
    };
    let continuity = value.get("continuity_health").and_then(Value::as_object);
    let cloudflare = value.get("cloudflare_product_posture").and_then(Value::as_object);
    let alignment = value.get("cloudflare_product_binding_alignment").and_then(Value::as_object);
    let scheduler = value.get("scheduler_task_readback").and_then(Value::as_object);
    let get = |object: Option<&Map<String, Value>>, key: &str, default: Value| object.and_then(|value| value.get(key)).cloned().unwrap_or(default);
    Ok(json!({
        "schema":"narada.cloudflare_carrier_mcp.health.v1",
        "status":"ok",
        "generated_at":value.get("generated_at").cloned().unwrap_or(Value::Null),
        "health_file":path.to_string_lossy(),
        "local":{
            "sync_status":get(continuity,"local_sync_status",Value::Null),
            "sync_artifacts":get(continuity,"local_sync_artifact_count",json!(0)),
            "inbound_status":get(continuity,"local_inbound_status",Value::Null),
            "inbound_artifacts":get(continuity,"local_inbound_artifact_count",json!(0)),
            "reconciliation_status":get(continuity,"reconciliation_execution_status",Value::Null),
            "reconciliation_plan":get(continuity,"reconciliation_execution_plan_status",Value::Null)
        },
        "scheduler":{
            "task_state":get(scheduler,"scheduled_task_state",Value::Null),
            "last_run":get(scheduler,"last_run_time",Value::Null),
            "last_result":get(scheduler,"last_result",Value::Null),
            "next_run":get(scheduler,"next_run_time",Value::Null),
            "cadence":get(scheduler,"cadence_status",Value::Null)
        },
        "cloudflare":{
            "posture_state":get(cloudflare,"state",Value::Null),
            "posture_status":get(cloudflare,"status",Value::Null),
            "site_count":cloudflare.and_then(|value| value.get("site_product_overview")).and_then(Value::as_object).and_then(|value| value.get("site_count")).cloned().unwrap_or_else(||json!(0)),
            "health_counts":cloudflare.and_then(|value| value.get("site_product_overview")).and_then(Value::as_object).and_then(|value| value.get("health_counts")).cloned().unwrap_or(Value::Null),
            "next_action":cloudflare.and_then(|value| value.get("site_product_overview")).and_then(Value::as_object).and_then(|value| value.get("next_action")).cloned().unwrap_or(Value::Null),
            "next_reason":cloudflare.and_then(|value| value.get("site_product_overview")).and_then(Value::as_object).and_then(|value| value.get("next_reason")).cloned().unwrap_or(Value::Null)
        },
        "alignment":{
            "state":get(alignment,"state",Value::Null),
            "status":get(alignment,"status",Value::Null),
            "reason":get(alignment,"reason",Value::Null),
            "local_site_count":get(alignment,"local_site_count",json!(0)),
            "cloudflare_next_action":get(alignment,"cloudflare_product_next_action",Value::Null)
        },
    }))
}

fn graph_mail_doctor(root: &Path) -> Value {
    let path = root.join(".ai/graph-mail-mcp.json");
    let policy = read_json_file(&path);
    let object = policy.as_object().cloned().unwrap_or_default();
    let token_present = env::var("MS_GRAPH_ACCESS_TOKEN").ok().or_else(|| env::var("GRAPH_ACCESS_TOKEN").ok()).map(|value| !value.trim().is_empty()).unwrap_or(false);
    json!({"schema":"narada.graph_mail_mcp.doctor.v1","status":"ok","site_root":root.to_string_lossy(),"graph_base_url":object.get("graph_base_url").cloned().unwrap_or_else(||json!("https://graph.microsoft.com/v1.0")),"has_access_token":token_present,"auth_mode":if token_present{"access_token"}else{"missing"},"allowed_mailboxes":object.get("allowed_mailboxes").or_else(||object.get("allowedMailboxes")).cloned().unwrap_or_else(||json!([])),"allowed_attachment_roots":object.get("allowed_attachment_roots").or_else(||object.get("allowedAttachmentRoots")).cloned().unwrap_or_else(||json!([])),"allow_device_code_auth":object.get("allow_device_code_auth").and_then(Value::as_bool).unwrap_or(false),"allow_send_draft":object.get("allow_send_draft").and_then(Value::as_bool).unwrap_or(false),"allow_folder_create":object.get("allow_folder_create").and_then(Value::as_bool).unwrap_or(false),"allow_message_move":object.get("allow_message_move").and_then(Value::as_bool).unwrap_or(false),"allow_message_mark_read":object.get("allow_message_mark_read").and_then(Value::as_bool).unwrap_or(false),"token_values_exposed":false,"native_read_only":true})
}
fn graph_mail_auth_status(root: &Path) -> Value {
    let token_present = env::var("MS_GRAPH_ACCESS_TOKEN").ok().or_else(|| env::var("GRAPH_ACCESS_TOKEN").ok()).map(|value| !value.trim().is_empty()).unwrap_or(false);
    json!({"schema":"narada.graph_mail_mcp.auth_status.v1","status":if token_present{"configured"}else{"missing"},"site_root":root.to_string_lossy(),"access_token_present":token_present,"token_values_exposed":false,"native_read_only":true})
}
fn read_json_file(path: &Path) -> Value {
    let Ok(meta) = fs::metadata(path) else { return Value::Object(Map::new()); };
    if !meta.is_file() || meta.len() > MAX_BYTES { return Value::Object(Map::new()); }
    fs::read_to_string(path).ok().and_then(|text|serde_json::from_str(&text).ok()).unwrap_or_else(||Value::Object(Map::new()))
}
fn cloudflare_path(args: &Map<String, Value>, field: &str, root: &Path, variable: &str, default_name: &str) -> (PathBuf, bool) {
    let requested = args.get(field).and_then(Value::as_str).map(str::to_string).filter(|value| !value.trim().is_empty()).or_else(|| env::var(variable).ok().filter(|value| !value.trim().is_empty()));
    let configured = requested.is_some();
    let path = requested.map(PathBuf::from).unwrap_or_else(|| root.join(".narada/runtime/cloudflare").join(default_name));
    (path, configured)
}
fn bounded_json(path: &Path) -> Result<Option<Value>, &'static str> {
    let Ok(meta) = fs::metadata(path) else { return Ok(None); };
    if !meta.is_file() || meta.len() > MAX_BYTES { return Err("cloudflare evidence file exceeds bounded size"); }
    let bytes = fs::read(path).map_err(|_| "cloudflare evidence file could not be read")?;
    serde_json::from_slice(&bytes).map(Some).map_err(|_| "cloudflare evidence file is invalid JSON")
}
fn metadata(path: &Path) -> Option<fs::Metadata> { fs::metadata(path).ok().filter(|m|m.is_file() && m.len() <= MAX_BYTES) }
fn file_meta(meta: Option<fs::Metadata>) -> Value { meta.map(|m|json!({"bytes":m.len()})).unwrap_or(Value::Null) }
fn boundary(surface_id: &str, name: &str, reason: &str, remediation: &str) -> Value { json!({"schema":format!("narada.{surface_id}.authority_boundary.v1"),"status":"unavailable","tool_name":name,"reason":reason,"remediation":remediation}) }
fn error(code: &str, message: &str) -> Value { json!({"schema":"narada.host_surface.error.v1","code":code,"message":message}) }
fn tool(name: &str, description: String, read_only: bool) -> Value { json!({"name":name,"description":description,"inputSchema":{"type":"object","additionalProperties":true},"annotations":{"title":name,"readOnlyHint":read_only,"destructiveHint":!read_only,"idempotentHint":read_only,"openWorldHint":!read_only},"outputSchema":{"type":"object","additionalProperties":true}}) }

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_root(label: &str) -> PathBuf {
        let suffix = SystemTime::now().duration_since(UNIX_EPOCH).expect("clock").as_nanos();
        std::env::temp_dir().join(format!("narada-host-contracts-{label}-{suffix}"))
    }

    #[test]
    fn cloudflare_session_status_redacts_cookie_and_reports_freshness() {
        let root = temp_root("session");
        let path = root.join(".narada/runtime/cloudflare/cloudflare_session.json");
        fs::create_dir_all(path.parent().expect("parent")).expect("directory");
        fs::write(&path, r#"{"cookie":"narada_operator_session=secret","captured_at":"2026-08-09T00:00:00Z","worker_url":"https://worker.example","principal":"operator"}"#).expect("write");
        let response = cloudflare_session_status(&Map::new(), &root);
        assert_eq!(response["status"], "present");
        assert_eq!(response["has_cookie"], true);
        assert!(response.get("cookie").is_none());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn cloudflare_health_reads_bounded_projection_fields() {
        let root = temp_root("health");
        let path = root.join(".narada/runtime/cloudflare/cloudflare_health.json");
        fs::create_dir_all(path.parent().expect("parent")).expect("directory");
        fs::write(&path, r#"{"generated_at":"2026-08-09T00:00:00Z","continuity_health":{"local_sync_status":"healthy"},"scheduler_task_readback":{"scheduled_task_state":"Ready"},"cloudflare_product_posture":{"site_product_overview":{"site_count":2}},"cloudflare_product_binding_alignment":{"state":"aligned"}}"#).expect("write");
        let response = cloudflare_health(&Map::new(), &root).expect("health");
        assert_eq!(response["status"], "ok");
        assert_eq!(response["local"]["sync_status"], "healthy");
        assert_eq!(response["scheduler"]["task_state"], "Ready");
        assert_eq!(response["cloudflare"]["site_count"], 2);
        assert_eq!(response["alignment"]["state"], "aligned");
        let _ = fs::remove_dir_all(root);
    }
}
