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
        ("cloudflare-carrier", "cloudflare_session_status") => Ok(file_metadata_status("cloudflare_session", root, "CLOUDFLARE_SESSION_FILE")),
        ("cloudflare-carrier", "cloudflare_health") => Ok(file_metadata_status("cloudflare_health", root, "CLOUDFLARE_HEALTH_FILE")),
        ("speech", "speech_listen_status") => Ok(json!({"schema":"narada.speech.listen_status.v1","status":"not_active","active_sessions":[],"native_read_only":true})),
        ("speech", "speech_voices") => Err(boundary(surface_id, name, "speech_provider_authority_not_enabled_in_native_slice", "Use the registry-resolved speech adapter.")),
        ("scheduler", "scheduler_runtime_status") => Ok(json!({"schema":"narada.scheduler.runtime_status.v1","status":"authority_boundary","implementation":"rust-native-contract","native_task_scheduler":false,"native_read_only":true})),
        ("graph-mail", "graph_mail_doctor") | ("graph-mail", "graph_mail_auth_status") => Ok(json!({"schema":"narada.graph_mail.authority_status.v1","status":"authority_boundary","credentials_present":false,"token_values_exposed":false,"native_read_only":true})),
        ("browser-control", "browser_control_session_inventory") => Ok(json!({"schema":"narada.browser_control.session_inventory.v1","status":"not_injected","sessions":[],"native_read_only":true})),
        _ => Err(boundary(surface_id, name, "external_or_host_authority_not_enabled_in_native_contract", "Use the configured owning surface authority for this operation.")),
    }
}

fn entries(surface_id: &str) -> &'static [(&'static str, bool)] { match surface_id { "browser-control" => BROWSER, "operator-console-overlay" => OPERATOR, "cloudflare-carrier" => CLOUDFLARE, "speech" => SPEECH, "scheduler" => SCHEDULER, "graph-mail" => GRAPH_MAIL, _ => &[] } }
fn guidance(surface_id: &str) -> Value { tool(&format!("{}_guidance", surface_id.replace('-', "_")), format!("Show model-facing operating guidance for {surface_id} MCP workflows."), true) }
fn guidance_result(surface_id: &str, args: &Map<String, Value>) -> Value { json!({"schema":"narada.host_surface.guidance.v1","status":"ok","surface_id":surface_id,"requested":args,"native_contract":"status probes and explicit authority boundaries","native_read_only":true}) }
fn description(surface_id: &str, name: &str) -> String { format!("Native {surface_id} contract for {name}; external authority remains explicit.") }
fn operator_status(root: &Path) -> Value { let state_root=env::var("NARADA_WINDOW_SURFACE_OVERLAY_STATE_ROOT").ok().unwrap_or_else(||root.join(".narada/runtime/operator-console-overlay").to_string_lossy().to_string()); let state_path=Path::new(&state_root).join("overlay-state.json"); let metadata=metadata(&state_path); json!({"schema":"narada.operator_console_overlay.status.v1","status":if metadata.is_some(){"present"}else{"not_active"},"state_root_configured":true,"state_file_present":metadata.is_some(),"state_file":file_meta(metadata),"native_read_only":true}) }
fn cloudflare_doctor(root: &Path) -> Value { json!({"schema":"narada.cloudflare_carrier.doctor.v1","status":"authority_boundary","site_root":root.to_string_lossy(),"session_file_configured":env::var("CLOUDFLARE_SESSION_FILE").ok().is_some(),"health_file_configured":env::var("CLOUDFLARE_HEALTH_FILE").ok().is_some(),"native_external_api":false,"native_read_only":true}) }
fn file_metadata_status(kind: &str, root: &Path, variable: &str) -> Value { let configured=env::var(variable).ok().filter(|v|!v.trim().is_empty()); let path=configured.as_deref().map(PathBuf::from).unwrap_or_else(||root.join(format!(".narada/runtime/cloudflare/{kind}.json"))); let meta=metadata(&path); json!({"schema":format!("narada.{kind}.v1"),"status":if meta.is_some(){"present"}else{"not_configured"},"configured":configured.is_some(),"file_present":meta.is_some(),"file":file_meta(meta),"native_read_only":true}) }
fn metadata(path: &Path) -> Option<fs::Metadata> { fs::metadata(path).ok().filter(|m|m.is_file() && m.len() <= MAX_BYTES) }
fn file_meta(meta: Option<fs::Metadata>) -> Value { meta.map(|m|json!({"bytes":m.len()})).unwrap_or(Value::Null) }
fn boundary(surface_id: &str, name: &str, reason: &str, remediation: &str) -> Value { json!({"schema":format!("narada.{surface_id}.authority_boundary.v1"),"status":"unavailable","tool_name":name,"reason":reason,"remediation":remediation}) }
fn error(code: &str, message: &str) -> Value { json!({"schema":"narada.host_surface.error.v1","code":code,"message":message}) }
fn tool(name: &str, description: String, read_only: bool) -> Value { json!({"name":name,"description":description,"inputSchema":{"type":"object","additionalProperties":true},"annotations":{"title":name,"readOnlyHint":read_only,"destructiveHint":!read_only,"idempotentHint":read_only,"openWorldHint":!read_only},"outputSchema":{"type":"object","additionalProperties":true}}) }
