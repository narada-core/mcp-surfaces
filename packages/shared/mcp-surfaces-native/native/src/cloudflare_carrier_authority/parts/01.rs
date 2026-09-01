use serde_json::{json, Map, Value};
use std::env;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::Duration;
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;
use url::Url;
use uuid::Uuid;

const DEFAULT_WORKER_URL: &str = "https://narada-cloudflare-carrier.andrei-kokoev.workers.dev";
const MAX_FILE_BYTES: u64 = 2 * 1024 * 1024;
const MAX_HTTP_BYTES: u64 = 2 * 1024 * 1024;

struct State {
    repo_root: PathBuf,
    site_root: PathBuf,
    session_file: PathBuf,
    health_file: PathBuf,
    projection_root: PathBuf,
    worker_url: String,
}

pub fn list_tools() -> Vec<Value> {
    vec![
    tool("cloudflare_carrier_guidance","Show the native Cloudflare carrier read workflow and authority boundaries.",json!({"type":"object","properties":{"workflow":{"type":"string","maxLength":256},"tool":{"type":"string","maxLength":256}},"additionalProperties":false}),true),
    tool("cloudflare_product_read","Read one bounded Cloudflare carrier product view through the server-bound worker and operator session.",json!({"type":"object","properties":{"operation":{"type":"string","enum":["site.list","site.read","operation.list","operation.read"],"default":"site.list"},"site_id":{"type":"string","minLength":1,"maxLength":512},"operation_id":{"type":"string","minLength":1,"maxLength":512},"limit":{"type":"integer","minimum":1,"maximum":100},"format":{"type":"string","enum":["json","summary","text"],"default":"json"},"continuation":{"type":"boolean","default":false}},"additionalProperties":false}),true),
    tool("cloudflare_session_status","Inspect bounded metadata for the server-bound operator-session file without exposing its cookie.",empty(),true),
    tool("cloudflare_health","Read the bounded server-bound continuity health snapshot.",empty(),true),
    tool("cloudflare_doctor","Inspect native Cloudflare carrier configuration and local evidence readiness.",empty(),true),
    tool("cloudflare_carrier_health","Join one registered projection's live readback with its explicit Cloudflare carrier lineage.",json!({"type":"object","properties":{"projection_id":{"type":"string","minLength":1,"maxLength":256,"pattern":"^[A-Za-z0-9._-]+$"}},"required":["projection_id"],"additionalProperties":false}),true),
]
}

pub fn call_tool(name: &str, args: &Map<String, Value>, root: &Path) -> Result<Value, Value> {
    let state = state(root)?;
    match name {
        "cloudflare_carrier_guidance" => Ok(guidance(args, &state)),
        "cloudflare_product_read" => product_read(args, &state),
        "cloudflare_session_status" => Ok(session_status(&state)),
        "cloudflare_health" => health(&state),
        "cloudflare_doctor" => Ok(doctor(&state)),
        "cloudflare_carrier_health" => carrier_health(args, &state),
        _ => Err(error(
            "unknown_tool",
            &format!("unknown_tool:{name}"),
            json!({"tool_name":name}),
        )),
    }
}

pub fn auxiliary(method: &str, params: &Map<String, Value>) -> Result<Value, Value> {
    match method {
        "prompts/list" => Ok(
            json!({"prompts":[{"name":"cloudflare_carrier_workflow","title":"Cloudflare Carrier Workflow","description":"Inspect readiness, read carrier product state, and verify registered projection lineage.","arguments":[]}]}),
        ),
        "prompts/get" => {
            if params.get("name").and_then(Value::as_str) != Some("cloudflare_carrier_workflow") {
                return Err(error(
                    "unknown_prompt",
                    "Unknown Cloudflare carrier prompt.",
                    json!({}),
                ));
            }
            Ok(
                json!({"description":"Native Cloudflare carrier workflow","messages":[{"role":"user","content":{"type":"text","text":"Call cloudflare_doctor first. Use cloudflare_product_read for carrier state and cloudflare_carrier_health only for a projection id already present in the server-bound registry. Treat unauthorized carrier state and healthy projection state independently."}}]}),
            )
        }
        "completion/complete" => {
            let values = if params
                .get("argument")
                .and_then(Value::as_object)
                .and_then(|v| v.get("name"))
                .and_then(Value::as_str)
                == Some("name")
            {
                list_tools()
                    .into_iter()
                    .filter_map(|v| v.get("name").cloned())
                    .collect::<Vec<_>>()
            } else {
                Vec::new()
            };
            Ok(json!({"completion":{"values":values,"total":values.len(),"hasMore":false}}))
        }
        "logging/setLevel" => Ok(json!({})),
        _ => Err(error(
            "unsupported_mcp_method",
            &format!("unsupported_mcp_method:{method}"),
            json!({"method":method}),
        )),
    }
}

fn state(site_root: &Path) -> Result<State, Value> {
    let repo_root =
        env_path(&["NARADA_ROOT", "NARADA_PROPER_ROOT"]).unwrap_or_else(|| site_root.to_path_buf());
    let session_file = env_path(&["CLOUDFLARE_SESSION_FILE"])
        .unwrap_or_else(|| repo_root.join(".narada/auth/cloudflare-operator-session.json"));
    let health_file = env_path(&["CLOUDFLARE_HEALTH_FILE"]).unwrap_or_else(|| {
        repo_root.join(".narada/site-continuity/health/cloudflare-continuity-health-last.json")
    });
    let projection_root = env_path(&["NARADA_CLOUDFLARE_PROJECTION_REGISTRY_ROOT"])
        .unwrap_or_else(|| site_root.join(".narada/crew/nars-projections"));
    let worker_url = env::var("CLOUDFLARE_CARRIER_URL")
        .ok()
        .filter(|v| !v.trim().is_empty())
        .unwrap_or_else(|| DEFAULT_WORKER_URL.into());
    let worker_url = validate_base_url(&worker_url, true)?;
    Ok(State {
        repo_root,
        site_root: site_root.to_path_buf(),
        session_file,
        health_file,
        projection_root,
        worker_url,
    })
}
fn guidance(args: &Map<String, Value>, state: &State) -> Value {
    json!({"schema":"narada.cloudflare_carrier_mcp.guidance.v1","status":"ok","workflow":["Inspect doctor and session freshness.","Read bounded carrier product state; credentials remain server-bound.","Use carrier_health only for an exact server-registered projection id.","Do not infer joined health when projection lineage is unknown or carrier authorization fails."],"boundaries":{"read_only":true,"server_bound_paths":true,"cookie_disclosed":false,"worker_url":state.worker_url},"requested":args})
}

fn product_read(args: &Map<String, Value>, state: &State) -> Result<Value, Value> {
    let operation = args
        .get("operation")
        .and_then(Value::as_str)
        .unwrap_or("site.list");
    let site = id(args, "site_id");
    let operation_id = id(args, "operation_id");
    if matches!(operation, "site.read" | "operation.list" | "operation.read") && site.is_none() {
        return Err(error(
            "site_id_required",
            &format!("site_id is required for {operation}."),
            json!({"operation":operation}),
        ));
    }
    if operation == "operation.read" && operation_id.is_none() {
        return Err(error(
            "operation_id_required",
            "operation_id is required for operation.read.",
            json!({}),
        ));
    }
    let mut params = Map::new();
    if let Some(v) = site.clone() {
        params.insert("site_id".into(), json!(v));
    }
    if let Some(v) = operation_id.clone() {
        params.insert("operation_id".into(), json!(v));
    }
    if let Some(v) = args.get("limit") {
        params.insert("limit".into(), v.clone());
    }
    let body = json!({"operation":operation,"request_id":format!("mcp_product_read_{}",Uuid::new_v4()),"params":params});
    let (status, response) = request_json(
        "POST",
        &format!("{}/api/carrier", state.worker_url),
        cookie(state).as_deref(),
        Some(&body),
    )?;
    let response = sanitize(response);
    if !(200..300).contains(&status) {
        return Err(error(
            "cloudflare_product_read_failed",
            &format!("Cloudflare carrier returned HTTP {status}."),
            json!({"status":status,"code":response.get("code").or_else(||response.get("error")).cloned().unwrap_or(Value::Null),"body":response}),
        ));
    }
    let format = args.get("format").and_then(Value::as_str).unwrap_or("json");
    let continuation = args
        .get("continuation")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let mut out = json!({"schema":"narada.cloudflare_carrier_mcp.product_read.v1","status":"ok","operation":operation,"worker_url":state.worker_url,"session_file":normalized(&state.session_file),"has_session":cookie(state).is_some(),"native_authority":true});
    if format == "summary" {
        out["summary"] = summarize(operation, &response, continuation);
    } else {
        out["response"] = response;
    }
    out["recovery"] = json!({"login_required":cookie(state).is_none(),"owner":"operator","instruction":"Refresh the server-bound Cloudflare operator session, then retry this same read."});
    Ok(out)
}

fn session_status(state: &State) -> Value {
    let path = &state.session_file;
    let Some(meta) = bounded_metadata(path) else {
        return json!({"schema":"narada.cloudflare_carrier_mcp.session_status.v1","status":"missing","session_file":normalized(path),"has_cookie":false,"is_fresh":false});
    };
    let age = meta
        .modified()
        .ok()
        .and_then(|v| v.elapsed().ok())
        .map(|v| v.as_secs() / 60);
    match bounded_json(path) {
        Ok(value) => {
            let has = value
                .get("cookie")
                .and_then(Value::as_str)
                .is_some_and(|v| !v.is_empty());
            json!({"schema":"narada.cloudflare_carrier_mcp.session_status.v1","status":if has{"present"}else{"incomplete"},"session_file":normalized(path),"has_cookie":has,"captured_at":value.get("captured_at").cloned().unwrap_or(Value::Null),"worker_url":value.get("worker_url").cloned().unwrap_or(Value::Null),"principal":value.get("principal").cloned().unwrap_or(Value::Null),"age_minutes":age,"is_fresh":age.is_some_and(|v|v<60),"size_bytes":meta.len()})
        }
        Err(code) => {
            json!({"schema":"narada.cloudflare_carrier_mcp.session_status.v1","status":code,"session_file":normalized(path),"has_cookie":false,"is_fresh":false,"age_minutes":age})
        }
    }
}
fn health(state: &State) -> Result<Value, Value> {
    let path = &state.health_file;
    if !path.exists() {
        return Ok(
            json!({"schema":"narada.cloudflare_carrier_mcp.health.v1","status":"missing","health_file":normalized(path)}),
        );
    }
    let v = bounded_json(path).map_err(|code| {
        error(
            "cloudflare_health_parse_failed",
            "The server-bound health snapshot is invalid or oversized.",
            json!({"health_file":normalized(path),"reason":code}),
        )
    })?;
    let continuity = obj(v.get("continuity_health"));
    let cloud = obj(v.get("cloudflare_product_posture"));
    let alignment = obj(v.get("cloudflare_product_binding_alignment"));
    let scheduler = obj(v.get("scheduler_task_readback"));
    let overview = obj(cloud.get("site_product_overview"));
    Ok(
        json!({"schema":"narada.cloudflare_carrier_mcp.health.v1","status":"ok","generated_at":v.get("generated_at"),"health_file":normalized(path),"local":{"sync_status":continuity.get("local_sync_status"),"sync_artifacts":continuity.get("local_sync_artifact_count").unwrap_or(&json!(0)),"inbound_status":continuity.get("local_inbound_status"),"inbound_artifacts":continuity.get("local_inbound_artifact_count").unwrap_or(&json!(0)),"reconciliation_status":continuity.get("reconciliation_execution_status"),"reconciliation_plan":continuity.get("reconciliation_execution_plan_status")},"scheduler":{"task_state":scheduler.get("scheduled_task_state"),"last_run":scheduler.get("last_run_time"),"last_result":scheduler.get("last_result"),"next_run":scheduler.get("next_run_time"),"cadence":scheduler.get("cadence_status")},"cloudflare":{"posture_state":cloud.get("state"),"posture_status":cloud.get("status"),"site_count":overview.get("site_count").unwrap_or(&json!(0)),"health_counts":overview.get("health_counts"),"next_action":overview.get("next_action"),"next_reason":overview.get("next_reason")},"alignment":{"state":alignment.get("state"),"status":alignment.get("status"),"reason":alignment.get("reason"),"local_site_count":alignment.get("local_site_count").unwrap_or(&json!(0)),"cloudflare_next_action":alignment.get("cloudflare_product_next_action")}}),
    )
}
fn doctor(state: &State) -> Value {
    let session = session_status(state);
    let health = bounded_json(&state.health_file).ok();
    json!({"schema":"narada.cloudflare_carrier_mcp.doctor.v1","status":"ok","native_authority":true,"repo_root":normalized(&state.repo_root),"site_root":normalized(&state.site_root),"worker_url":state.worker_url,"session_file":normalized(&state.session_file),"session_status":session.get("status"),"session_fresh":session.get("is_fresh"),"operator_action":operator_action(&session),"health_file":normalized(&state.health_file),"health_file_exists":state.health_file.exists(),"health_status":health.as_ref().and_then(|v|v.get("status")).cloned().unwrap_or_else(||json!(if state.health_file.exists(){"invalid_json"}else{"missing"})),"projection_registry_root":normalized(&state.projection_root),"projection_registry_exists":state.projection_root.is_dir(),"projection_registry_status":if state.projection_root.is_dir(){"ready"}else{"missing"}})
}

struct Projection {
    site_id: Option<String>,
    operation_id: Option<String>,
    lineage: &'static str,
    api: Option<String>,
    token: Option<String>,
    lifecycle: &'static str,
    expires: Option<String>,
    revoked: Option<String>,
}
