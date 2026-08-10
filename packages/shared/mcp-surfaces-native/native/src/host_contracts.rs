use serde_json::{json, Map, Value};
use std::collections::HashMap;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

const MAX_BYTES: u64 = 256_000;
const DEFAULT_GRAPH_BASE_URL: &str = "https://graph.microsoft.com/v1.0";
const CLOUDFLARE_PACKAGE_FILTER: &str = "@narada-core/cloudflare-carrier";
const CLOUDFLARE_WORKER_URL: &str = "https://narada-cloudflare-carrier.andrei-kokoev.workers.dev";
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
        ("browser-control", "browser_control_session_inventory") => Ok(browser_session_inventory(root)),
        _ => Err(boundary(surface_id, name, "external_or_host_authority_not_enabled_in_native_contract", "Use the configured owning surface authority for this operation.")),
    }
}

fn entries(surface_id: &str) -> &'static [(&'static str, bool)] { match surface_id { "browser-control" => BROWSER, "operator-console-overlay" => OPERATOR, "cloudflare-carrier" => CLOUDFLARE, "speech" => SPEECH, "scheduler" => SCHEDULER, "graph-mail" => GRAPH_MAIL, _ => &[] } }
fn guidance(surface_id: &str) -> Value { tool(&format!("{}_guidance", surface_id.replace('-', "_")), format!("Show model-facing operating guidance for {surface_id} MCP workflows."), true) }
fn guidance_result(surface_id: &str, args: &Map<String, Value>) -> Value { json!({"schema":"narada.host_surface.guidance.v1","status":"ok","surface_id":surface_id,"requested":args,"native_contract":"status probes and explicit authority boundaries","native_read_only":true}) }
fn description(surface_id: &str, name: &str) -> String { format!("Native {surface_id} contract for {name}; external authority remains explicit.") }
fn operator_status(root: &Path) -> Value {
    let state_root = operator_state_root();
    operator_status_at(root, &state_root)
}

fn browser_session_inventory(root: &Path) -> Value {
    json!({
        "schema":"narada.browser_control.result.v1",
        "status":"ok",
        "site_root":root.to_string_lossy(),
        "sessions":[],
        "count":0,
    })
}

fn operator_status_at(root: &Path, state_root: &Path) -> Value {
    let state_directory = state_root.join("operator-console");
    let pid_path = state_directory.join("overlay.pid");
    let pid = fs::read_to_string(&pid_path)
        .ok()
        .and_then(|value| value.trim().parse::<u32>().ok())
        .filter(|value| *value > 0);
    let running = pid.map(pid_is_running).unwrap_or(false);
    let overlay = json!({
        "schema":"narada.window_surface_overlay.result.v1",
        "id":"operator-console",
        "state":if running { "running" } else { "stopped" },
        "pid":if running { pid.map(|value| json!(value)).unwrap_or(Value::Null) } else { Value::Null },
        "state_directory":state_directory.to_string_lossy(),
        "document_path":state_directory.join("document.json").to_string_lossy(),
        "document":operator_json(&state_directory.join("document.json")),
        "action_state":operator_json(&state_directory.join("action-state.json")),
        "visibility_state":operator_json(&state_directory.join("visibility.state.json")),
        "surface_snapshot":operator_json(&state_root.join("surface.snapshot.json")),
        "focus_owner":operator_json(&state_root.join("focus.owner.json")),
    });
    json!({
        "schema":"narada.operator_console_overlay.mcp_result.v1",
        "status":"ok",
        "operation":"status",
        "command":"inspect",
        "overlay_id":"operator-console",
        "narada_root":root.to_string_lossy(),
        "overlay":overlay,
    })
}

fn operator_state_root() -> PathBuf {
    if let Ok(value) = env::var("NARADA_WINDOW_SURFACE_OVERLAY_STATE_ROOT") {
        if !value.trim().is_empty() { return PathBuf::from(value); }
    }
    let local_app_data = env::var("LOCALAPPDATA")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .map(PathBuf::from)
        .or_else(|| env::var("USERPROFILE").ok().filter(|value| !value.trim().is_empty()).map(|value| PathBuf::from(value).join("AppData/Local")))
        .or_else(|| env::var("HOME").ok().filter(|value| !value.trim().is_empty()).map(|value| PathBuf::from(value).join("AppData/Local")))
        .unwrap_or_else(|| PathBuf::from("AppData/Local"));
    local_app_data.join("Narada/window-surface-overlays")
}

fn operator_json(path: &Path) -> Value {
    bounded_json(path).ok().flatten().unwrap_or(Value::Null)
}

fn pid_is_running(pid: u32) -> bool {
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
fn cloudflare_doctor(root: &Path) -> Value {
    let empty = Map::new();
    let repo_root = env::var("NARADA_ROOT").ok().filter(|value| !value.trim().is_empty()).map(PathBuf::from).unwrap_or_else(|| root.to_path_buf());
    let session = cloudflare_session_status(&empty, &repo_root);
    let (health_path, _) = cloudflare_path(&empty, "health_file", &repo_root, "CLOUDFLARE_HEALTH_FILE", ".narada/site-continuity/health/cloudflare-continuity-health-last.json");
    let health_status = match bounded_json(&health_path) {
        Ok(Some(value)) => value.get("status").cloned().unwrap_or_else(|| json!("unknown")),
        Ok(None) => json!("missing"),
        Err(_) => json!("invalid_json"),
    };
    let (session_path, _) = cloudflare_path(&empty, "session_file", &repo_root, "CLOUDFLARE_SESSION_FILE", ".narada/auth/cloudflare-operator-session.json");
    let projection_root = env::var("NARADA_CLOUDFLARE_PROJECTION_REGISTRY_ROOT").ok().filter(|value| !value.trim().is_empty()).map(PathBuf::from).unwrap_or_else(|| repo_root.join(".narada/crew/nars-projections"));
    let worker_url = env::var("CLOUDFLARE_CARRIER_URL").ok().filter(|value| !value.trim().is_empty()).map(|value| value.trim_end_matches('/').to_string()).unwrap_or_else(|| CLOUDFLARE_WORKER_URL.to_string());
    json!({"schema":"narada.cloudflare_carrier_mcp.doctor.v1","status":"ok","repo_root":normalized_path(&repo_root),"package_filter":CLOUDFLARE_PACKAGE_FILTER,"worker_url":worker_url,"session_file":normalized_path(&session_path),"session_status":session.get("status").cloned().unwrap_or(Value::Null),"session_fresh":session.get("is_fresh").cloned().unwrap_or(Value::Null),"operator_action":cloudflare_doctor_operator_action(&session),"health_file":normalized_path(&health_path),"health_file_exists":metadata(&health_path).is_some(),"health_status":health_status,"projection_registry_root":normalized_path(&projection_root),"projection_registry_exists":projection_root.exists(),"projection_registry_status":if projection_root.exists(){"ready"}else{"missing"}})
}

fn cloudflare_doctor_operator_action(session: &Value) -> Value {
    match session.get("status").and_then(Value::as_str) {
        Some("missing") => json!("run_pnpm_cloudflare_operator_login"),
        Some("present") if session.get("is_fresh").and_then(Value::as_bool) == Some(false) => json!("run_pnpm_cloudflare_operator_login_then_cloudflare_operator_check_human"),
        Some("present") if session.get("has_cookie").and_then(Value::as_bool) == Some(false) => json!("run_pnpm_cloudflare_operator_login_to_capture_cookie"),
        _ => Value::Null,
    }
}

fn normalized_path(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

fn cloudflare_session_status(args: &Map<String, Value>, root: &Path) -> Value {
    let (path, _) = cloudflare_path(args, "session_file", root, "CLOUDFLARE_SESSION_FILE", ".narada/auth/cloudflare-operator-session.json");
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
    let (path, _) = cloudflare_path(args, "health_file", root, "CLOUDFLARE_HEALTH_FILE", ".narada/site-continuity/health/cloudflare-continuity-health-last.json");
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
    let graph_base_url = object.get("graph_base_url").and_then(Value::as_str).filter(|value| !value.trim().is_empty()).map(|value| value.trim_end_matches('/').to_string()).unwrap_or_else(|| DEFAULT_GRAPH_BASE_URL.to_string());
    let allowed_mailboxes = graph_string_array(&object, "allowed_mailboxes", "allowedMailboxes");
    let allowed_attachment_roots = {
        let values = graph_string_array(&object, "allowed_attachment_roots", "allowedAttachmentRoots");
        if values.is_empty() { vec![Value::String(root.to_string_lossy().to_string())] } else { values.into_iter().filter_map(|value| value.as_str().map(|path| Value::String(resolve_graph_path(root, path)))).collect() }
    };
    let allow_device_code_auth = graph_bool(&object, "allow_device_code_auth", "allowDeviceCodeAuth");
    let device_code_tenant = graph_string(&object, "device_code_tenant_id", "deviceCodeTenantId");
    let device_code_client = graph_string(&object, "device_code_client_id", "deviceCodeClientId");
    let device_code_allowed_scopes = graph_string_array(&object, "device_code_allowed_scopes", "deviceCodeAllowedScopes");
    let (has_access_token, auth_mode) = graph_auth_posture(root, allow_device_code_auth, &device_code_allowed_scopes);
    let delegated_token = graph_delegated_token_summary(root);
    json!({"schema":"narada.graph_mail_mcp.doctor.v1","status":"ok","site_root":root.to_string_lossy(),"graph_base_url":graph_base_url,"has_access_token":has_access_token,"auth_mode":auth_mode,"allowed_mailboxes":allowed_mailboxes,"allowed_attachment_roots":allowed_attachment_roots,"allow_device_code_auth":allow_device_code_auth,"device_code_tenant_configured":device_code_tenant.is_some() || graph_non_empty_env(root, "GRAPH_TENANT_ID"),"device_code_client_configured":device_code_client.is_some() || graph_non_empty_env(root, "GRAPH_CLIENT_ID"),"device_code_allowed_scopes":device_code_allowed_scopes,"delegated_token":delegated_token,"allow_send_draft":graph_bool(&object, "allow_send_draft", "allowSendDraft"),"send_approval_token_configured":graph_token_configured(&object, "send_approval_token", "sendApprovalToken"),"allow_folder_create":graph_bool(&object, "allow_folder_create", "allowFolderCreate"),"allow_message_move":graph_bool(&object, "allow_message_move", "allowMessageMove"),"allow_message_mark_read":graph_bool(&object, "allow_message_mark_read", "allowMessageMarkRead"),"mailbox_organization_approval_token_configured":graph_token_configured(&object, "mailbox_organization_approval_token", "mailboxOrganizationApprovalToken"),"server_name":"narada-graph-mail-mcp"})
}
fn graph_mail_auth_status(root: &Path) -> Value {
    let object = read_json_file(&root.join(".ai/graph-mail-mcp.json")).as_object().cloned().unwrap_or_default();
    let scopes = graph_string_array(&object, "device_code_allowed_scopes", "deviceCodeAllowedScopes");
    json!({"schema":"narada.graph_mail_mcp.auth_status.v1","status":"ok","allow_device_code_auth":graph_bool(&object, "allow_device_code_auth", "allowDeviceCodeAuth"),"device_code_tenant_configured":graph_string(&object, "device_code_tenant_id", "deviceCodeTenantId").is_some() || graph_non_empty_env(root, "GRAPH_TENANT_ID"),"device_code_client_configured":graph_string(&object, "device_code_client_id", "deviceCodeClientId").is_some() || graph_non_empty_env(root, "GRAPH_CLIENT_ID"),"device_code_allowed_scopes":scopes,"delegated_token":graph_delegated_token_summary(root)})
}

fn graph_string(object: &Map<String, Value>, snake: &str, camel: &str) -> Option<String> {
    object.get(snake).or_else(|| object.get(camel)).and_then(Value::as_str).filter(|value| !value.trim().is_empty()).map(ToOwned::to_owned)
}
fn graph_string_array(object: &Map<String, Value>, snake: &str, camel: &str) -> Vec<Value> {
    object.get(snake).or_else(|| object.get(camel)).and_then(Value::as_array).map(|items| items.iter().filter_map(|value| value.as_str().filter(|item| !item.trim().is_empty()).map(|item| Value::String(item.to_string()))).collect()).unwrap_or_default()
}
fn graph_bool(object: &Map<String, Value>, snake: &str, camel: &str) -> bool {
    object.get(snake).or_else(|| object.get(camel)).and_then(Value::as_bool).unwrap_or(false)
}
fn graph_token_configured(object: &Map<String, Value>, snake: &str, camel: &str) -> bool {
    graph_string(object, snake, camel).is_some()
}
fn resolve_graph_path(root: &Path, value: &str) -> String {
    let path = PathBuf::from(value);
    if path.is_absolute() { path.to_string_lossy().to_string() } else { root.join(path).to_string_lossy().to_string() }
}
fn graph_auth_posture(root: &Path, allow_device_code: bool, scopes: &[Value]) -> (bool, &'static str) {
    let delegated = graph_delegated_token_summary(root);
    let delegated_allowed = allow_device_code && delegated.get("status").and_then(Value::as_str).map(|status| status == "available" || status == "refreshable").unwrap_or(false) && delegated.get("scope").and_then(Value::as_str).map(|scope| scopes.iter().any(|value| value.as_str() == Some(scope))).unwrap_or(false);
    if delegated_allowed { return (true, "delegated_device_code"); }
    let graph_access = graph_non_empty_env(root, "GRAPH_ACCESS_TOKEN");
    let client_credentials = graph_non_empty_env(root, "GRAPH_TENANT_ID") && graph_non_empty_env(root, "GRAPH_CLIENT_ID") && graph_non_empty_env(root, "GRAPH_CLIENT_SECRET");
    let ms_access = graph_non_empty_env(root, "MS_GRAPH_ACCESS_TOKEN");
    if graph_access || (!client_credentials && ms_access) { (true, "access_token") } else if client_credentials { (true, "client_credentials") } else { (false, "missing") }
}
fn graph_delegated_token_summary(root: &Path) -> Value {
    let value = read_json_file(&root.join(".ai/runtime/graph-mail-mcp/delegated-token.json"));
    let Some(object) = value.as_object() else { return json!({"status":"missing","fresh":false}); };
    if object.get("schema").and_then(Value::as_str) != Some("narada.graph_mail_mcp.delegated_token.v1") { return json!({"status":"missing","fresh":false}); }
    let expires = object.get("expires_at_ms").and_then(Value::as_i64).unwrap_or(0);
    let fresh = expires > chrono_now_ms() + 60_000;
    let refreshable = object.get("refresh_token").and_then(Value::as_str).map(|value| !value.trim().is_empty()).unwrap_or(false);
    json!({"status":if fresh{"available"}else if refreshable{"refreshable"}else{"expired"},"fresh":fresh,"refreshable":refreshable,"auth_mode":object.get("auth_mode").cloned().unwrap_or(Value::Null),"tenant_id":object.get("tenant_id").cloned().unwrap_or(Value::Null),"client_id":object.get("client_id").cloned().unwrap_or(Value::Null),"scope":object.get("scope").cloned().unwrap_or(Value::Null),"acquired_at":object.get("acquired_at").cloned().unwrap_or(Value::Null),"expires_at_ms":object.get("expires_at_ms").cloned().unwrap_or(Value::Null)})
}
fn chrono_now_ms() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|duration| duration.as_millis() as i64).unwrap_or(0)
}
fn graph_non_empty_env(root: &Path, key: &str) -> bool {
    let mut values = HashMap::new();
    for path in [root.parent().map(|parent| parent.join(".env")), Some(root.join(".env"))].into_iter().flatten() {
        if let Ok(text) = fs::read_to_string(path) { for line in text.lines() { if let Some((name, value)) = line.split_once('=') { values.insert(name.trim().to_string(), value.trim().trim_matches(['\'', '"']).to_string()); } } }
    }
    if let Ok(value) = env::var(key) { values.insert(key.to_string(), value); }
    values.get(key).map(|value| !value.trim().is_empty()).unwrap_or(false)
}
fn read_json_file(path: &Path) -> Value {
    let Ok(meta) = fs::metadata(path) else { return Value::Object(Map::new()); };
    if !meta.is_file() || meta.len() > MAX_BYTES { return Value::Object(Map::new()); }
    fs::read_to_string(path).ok().and_then(|text|serde_json::from_str(&text).ok()).unwrap_or_else(||Value::Object(Map::new()))
}
fn cloudflare_path(args: &Map<String, Value>, field: &str, root: &Path, variable: &str, default_name: &str) -> (PathBuf, bool) {
    let requested = args.get(field).and_then(Value::as_str).map(str::to_string).filter(|value| !value.trim().is_empty()).or_else(|| env::var(variable).ok().filter(|value| !value.trim().is_empty()));
    let configured = requested.is_some();
    let path = requested.map(PathBuf::from).unwrap_or_else(|| root.join(default_name));
    (path, configured)
}
fn bounded_json(path: &Path) -> Result<Option<Value>, &'static str> {
    let Ok(meta) = fs::metadata(path) else { return Ok(None); };
    if !meta.is_file() || meta.len() > MAX_BYTES { return Err("cloudflare evidence file exceeds bounded size"); }
    let bytes = fs::read(path).map_err(|_| "cloudflare evidence file could not be read")?;
    serde_json::from_slice(&bytes).map(Some).map_err(|_| "cloudflare evidence file is invalid JSON")
}
fn metadata(path: &Path) -> Option<fs::Metadata> { fs::metadata(path).ok().filter(|m|m.is_file() && m.len() <= MAX_BYTES) }
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
        let path = root.join(".narada/auth/cloudflare-operator-session.json");
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
        let path = root.join(".narada/site-continuity/health/cloudflare-continuity-health-last.json");
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

    #[test]
    fn operator_status_projects_persisted_overlay_state() {
        let root = temp_root("operator");
        let state_root = root.join("overlay-state");
        let state_directory = state_root.join("operator-console");
        fs::create_dir_all(&state_directory).expect("directory");
        fs::write(state_directory.join("document.json"), r#"{"schema":"narada.window_surface_overlay.document.v1","id":"operator-console","title":"Fixture","title_tone":"default","subtitle":null,"rows":[],"actions":[],"updated_at":"2026-08-09T00:00:00Z"}"#).expect("document");
        fs::write(state_directory.join("action-state.json"), r#"{"schema":"narada.window_surface_overlay.action_state.v1","action_id":"refresh","request_id":"request-1","status":"succeeded"}"#).expect("action");
        fs::write(state_directory.join("visibility.state.json"), r#"{"schema":"narada.window_surface_overlay.visibility_state.v1","state":"visible"}"#).expect("visibility");
        fs::write(state_root.join("surface.snapshot.json"), r#"{"schema":"narada.window_surface_overlay.surface_snapshot.v1","status":"ready"}"#).expect("snapshot");
        fs::write(state_root.join("focus.owner.json"), r#"{"schema":"narada.window_surface_overlay.focus_owner.v1","owner":"fixture"}"#).expect("focus");
        let response = operator_status_at(&root, &state_root);
        assert_eq!(response["schema"], "narada.operator_console_overlay.mcp_result.v1");
        assert_eq!(response["overlay"]["schema"], "narada.window_surface_overlay.result.v1");
        assert_eq!(response["overlay"]["state"], "stopped");
        assert_eq!(response["overlay"]["document"]["title"], "Fixture");
        assert_eq!(response["overlay"]["action_state"]["status"], "succeeded");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn browser_session_inventory_is_empty_without_injection() {
        let root = temp_root("browser");
        let response = browser_session_inventory(&root);
        assert_eq!(response["schema"], "narada.browser_control.result.v1");
        assert_eq!(response["status"], "ok");
        assert_eq!(response["count"], 0);
        assert_eq!(response["sessions"].as_array().map(Vec::len), Some(0));
        let _ = fs::remove_dir_all(root);
    }
}
