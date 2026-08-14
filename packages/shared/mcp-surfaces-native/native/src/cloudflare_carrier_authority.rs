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
fn carrier_health(args: &Map<String, Value>, state: &State) -> Result<Value, Value> {
    let id = args
        .get("projection_id")
        .and_then(Value::as_str)
        .unwrap_or("");
    let observed = now();
    let Some(entry) = projection(state, id, &observed)? else {
        return Ok(joined(
            "missing",
            Some("projection_registry_entry_missing"),
            id,
            json!({"status":"not_checked","site_id":null,"operation_id":null,"auth_source":null}),
            json!({"status":"not_checked","projection_id":id,"lineage_status":"unknown","last_event_sequence":null,"last_projected_at":null,"observed_at":observed}),
            "Configure the server-bound projection registry and retry.",
        ));
    };
    let mut projection = json!({"status":if entry.lifecycle=="active"{"not_checked"}else{entry.lifecycle},"projection_id":id,"lineage_status":entry.lineage,"last_event_sequence":null,"last_projected_at":null,"observed_at":observed,"expires_at":entry.expires,"revoked_at":entry.revoked});
    let mut carrier = json!({"status":"not_checked","site_id":entry.site_id,"operation_id":entry.operation_id,"auth_source":null});
    if entry.lifecycle == "active" {
        if let (Some(api), Some(token)) = (entry.api.as_ref(), entry.token.as_ref()) {
            let headers = Some(("x-narada-browser-token-fingerprint", token.as_str()));
            let (status, body) =
                get_json(&format!("{api}/api/nars/projections/{id}/health"), headers)?;
            if (200..300).contains(&status)
                && body.get("status").and_then(Value::as_str) == Some("healthy")
            {
                projection["status"] = json!("healthy");
                projection["last_event_sequence"] = body
                    .get("last_event_sequence")
                    .cloned()
                    .unwrap_or(Value::Null);
                projection["last_projected_at"] = body
                    .get("last_projected_at")
                    .cloned()
                    .unwrap_or(Value::Null);
                if projection["last_event_sequence"].is_null()
                    || projection["last_projected_at"].is_null()
                {
                    let (_,events)=get_json(&format!("{api}/api/nars/projections/{id}/events?direction=backward&max_events=1"),headers)?;
                    projection["last_event_sequence"] = events
                        .pointer("/cursor/last_sequence")
                        .cloned()
                        .unwrap_or(Value::Null);
                    projection["last_projected_at"] = events
                        .pointer("/events/0/projected_at")
                        .cloned()
                        .unwrap_or(Value::Null);
                }
            } else {
                projection["status"] = json!("unavailable");
                projection["code"] = json!(projection_unavailable(status));
            }
        } else {
            projection["status"] = json!("unavailable");
            projection["code"] = json!(if entry.api.is_some() {
                "projection_browser_credential_missing"
            } else {
                "projection_api_base_url_missing"
            });
        }
    }
    if projection["status"] == "healthy" && entry.lineage == "matched" {
        if let Some(site) = entry.site_id.as_ref() {
            let operation = if entry.operation_id.is_some() {
                "operation.read"
            } else {
                "site.read"
            };
            let mut params = json!({"site_id":site});
            if let Some(op) = entry.operation_id.as_ref() {
                params["operation_id"] = json!(op);
            }
            let body = json!({"operation":operation,"request_id":format!("mcp_carrier_health_{}",Uuid::new_v4()),"params":params});
            let (status, response) = request_json(
                "POST",
                &format!("{}/api/carrier", state.worker_url),
                cookie(state).as_deref(),
                Some(&body),
            )?;
            carrier["auth_source"] = if cookie(state).is_some() {
                json!("operator_session_file")
            } else {
                Value::Null
            };
            if (200..300).contains(&status) {
                carrier["status"] = json!("ok");
                carrier["product_health"] = response
                    .pointer("/site_product_status/health")
                    .or_else(|| response.pointer("/product_status/health"))
                    .cloned()
                    .unwrap_or(Value::Null);
                carrier["next_action"] = response
                    .pointer("/site_product_status/next_action")
                    .or_else(|| response.pointer("/product_status/next_action"))
                    .cloned()
                    .unwrap_or(Value::Null);
            } else {
                carrier["status"] = json!(if status == 401 {
                    "unauthorized"
                } else if status == 403 {
                    "forbidden"
                } else {
                    "unavailable"
                });
            }
        }
    }
    let (status, code, next) = if projection["status"] == "healthy" {
        if entry.lineage != "matched" {
            (
                "unverified",
                Some(if entry.lineage == "unknown" {
                    "projection_lineage_unknown"
                } else {
                    "projection_lineage_mismatched"
                }),
                "Register explicit Cloudflare carrier lineage before claiming joined health.",
            )
        } else if carrier["status"] == "ok" {
            ("healthy", None, "")
        } else if carrier["status"] == "unauthorized" {
            (
                "degraded",
                Some("carrier_api_unauthorized_projection_available"),
                "Refresh the operator session, then retry.",
            )
        } else if carrier["status"] == "forbidden" {
            (
                "degraded",
                Some("carrier_api_forbidden_projection_available"),
                "Inspect carrier Site membership.",
            )
        } else {
            (
                "degraded",
                Some("carrier_api_unavailable_projection_available"),
                "Inspect the carrier worker and network.",
            )
        }
    } else if matches!(entry.lifecycle, "revoked" | "expired") {
        (
            "degraded",
            Some(if entry.lifecycle == "revoked" {
                "projection_revoked"
            } else {
                "projection_expired"
            }),
            "Re-register or renew the projection.",
        )
    } else {
        (
            "unverified",
            projection.get("code").and_then(Value::as_str),
            "Repair projection readback before relying on joined health.",
        )
    };
    let code = code.map(str::to_string);
    Ok(joined(
        status,
        code.as_deref(),
        id,
        carrier,
        projection,
        next,
    ))
}

fn projection(state: &State, id: &str, observed: &str) -> Result<Option<Projection>, Value> {
    if id.is_empty()
        || id.len() > 256
        || !id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
    {
        return Err(error(
            "projection_id_invalid",
            "projection_id must use 1-256 ASCII letters, digits, dot, underscore, or hyphen.",
            json!({}),
        ));
    }
    let root = state.projection_root.join(id);
    if !confined(&root, &state.projection_root) {
        return Err(error(
            "projection_path_refused",
            "Projection path escaped the server-bound registry.",
            json!({}),
        ));
    }
    let intent = optional_json(&root.join("intent.json"));
    let remote = optional_json(&root.join("remote-access.json"));
    if intent.is_none() && remote.is_none() {
        return Ok(None);
    }
    let a = intent.as_ref().unwrap_or(&Value::Null);
    let b = remote.as_ref().unwrap_or(&Value::Null);
    let site = string(a.get("site_id")).or_else(|| string(b.get("site_id")));
    let source = a.get("source_ref").or_else(|| b.get("source_ref"));
    let source_obj = source.and_then(Value::as_object);
    let kind = source_obj
        .and_then(|v| v.get("kind"))
        .and_then(Value::as_str);
    let operation_id = source_obj
        .and_then(|v| v.get("operation_id"))
        .and_then(Value::as_str)
        .map(str::to_string);
    let carrier_id = source_obj
        .and_then(|v| v.get("carrier_session_id"))
        .and_then(Value::as_str);
    let lineage = if source.is_none() {
        "unknown"
    } else if kind == Some("cloudflare_carrier")
        && site.is_some()
        && (operation_id.is_some() || carrier_id.is_some())
    {
        "matched"
    } else {
        "mismatched"
    };
    let api = string(a.get("projection_api_base_url"))
        .or_else(|| string(b.get("projection_api_base_url")))
        .and_then(|v| validate_base_url(&v, false).ok())
        .or_else(|| legacy_projection_base(a).or_else(|| legacy_projection_base(b)));
    let tokens = b.get("browser_access_tokens").and_then(Value::as_array);
    let token = tokens
        .and_then(|v| {
            v.iter().find(|x| {
                x.get("kind").and_then(Value::as_str) == Some("browser")
                    && x.get("status")
                        .and_then(Value::as_str)
                        .is_none_or(|s| s == "active")
            })
        })
        .and_then(|v| string(v.get("token_fingerprint")));
    let expires = string(b.get("expires_at")).or_else(|| string(a.get("expires_at")));
    let revoked = string(b.get("revoked_at")).or_else(|| string(a.get("revoked_at")));
    let declared = string(b.get("lifecycle_state")).or_else(|| string(a.get("lifecycle_state")));
    let lifecycle = if revoked.is_some() || declared.as_deref() == Some("revoked") {
        "revoked"
    } else if expires
        .as_ref()
        .and_then(|v| OffsetDateTime::parse(v, &Rfc3339).ok())
        .is_some_and(|v| {
            v <= OffsetDateTime::parse(observed, &Rfc3339).unwrap_or(OffsetDateTime::UNIX_EPOCH)
        })
    {
        "expired"
    } else {
        "active"
    };
    Ok(Some(Projection {
        site_id: site,
        operation_id,
        lineage,
        api,
        token,
        lifecycle,
        expires,
        revoked,
    }))
}

fn request_json(
    method: &str,
    url: &str,
    cookie_value: Option<&str>,
    body: Option<&Value>,
) -> Result<(u16, Value), Value> {
    let parsed = validate_request_url(url)?;
    let timeout = Duration::from_millis(
        env::var("NARADA_CLOUDFLARE_REQUEST_TIMEOUT_MS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(5_000)
            .clamp(100, 30_000),
    );
    let mut request = if method == "POST" {
        ureq::post(parsed.as_str())
    } else {
        ureq::get(parsed.as_str())
    }
    .timeout(timeout)
    .set("content-type", "application/json");
    if let Some(cookie) = cookie_value {
        request = request.set("cookie", &format!("narada_operator_session={cookie}"));
    }
    let response = match if let Some(value) = body {
        request.send_string(&value.to_string())
    } else {
        request.call()
    } {
        Ok(v) => v,
        Err(ureq::Error::Status(_, v)) => v,
        Err(cause) => {
            return Err(error(
                "cloudflare_transport_failed",
                &cause.to_string(),
                json!({"url":redacted(&parsed),"timeout_ms":timeout.as_millis()}),
            ))
        }
    };
    read_response(response)
}
fn get_json(url: &str, header: Option<(&str, &str)>) -> Result<(u16, Value), Value> {
    let parsed = validate_request_url(url)?;
    let timeout = Duration::from_millis(
        env::var("NARADA_CLOUDFLARE_REQUEST_TIMEOUT_MS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(5_000)
            .clamp(100, 30_000),
    );
    let mut request = ureq::get(parsed.as_str()).timeout(timeout);
    if let Some((k, v)) = header {
        request = request.set(k, v);
    }
    let response = match request.call() {
        Ok(v) => v,
        Err(ureq::Error::Status(_, v)) => v,
        Err(cause) => return Ok((0, json!({"transport_error":cause.to_string()}))),
    };
    read_response(response)
}
fn read_response(response: ureq::Response) -> Result<(u16, Value), Value> {
    let status = response.status();
    if response
        .header("content-length")
        .and_then(|v| v.parse::<u64>().ok())
        .is_some_and(|v| v > MAX_HTTP_BYTES)
    {
        return Err(error(
            "cloudflare_response_too_large",
            "Provider response exceeds 2 MiB.",
            json!({"status":status}),
        ));
    }
    let mut reader = response.into_reader();
    let mut limited = (&mut reader).take(MAX_HTTP_BYTES + 1);
    let mut bytes = Vec::new();
    limited.read_to_end(&mut bytes).map_err(|cause| {
        error(
            "cloudflare_response_read_failed",
            &cause.to_string(),
            json!({}),
        )
    })?;
    if bytes.len() as u64 > MAX_HTTP_BYTES {
        return Err(error(
            "cloudflare_response_too_large",
            "Provider response exceeds 2 MiB.",
            json!({"status":status}),
        ));
    }
    let value = serde_json::from_slice(&bytes).unwrap_or_else(|_| json!({}));
    Ok((status, value))
}
fn validate_request_url(value: &str) -> Result<Url, Value> {
    let url = Url::parse(value).map_err(|_| {
        error(
            "cloudflare_url_invalid",
            "Configured URL is invalid.",
            json!({}),
        )
    })?;
    if !matches!(url.scheme(), "http" | "https")
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return Err(error(
            "cloudflare_url_refused",
            "Configured URL must be credential-free HTTP(S).",
            json!({}),
        ));
    }
    if url.scheme() == "http" && !allow_insecure(&url) {
        return Err(error(
            "cloudflare_insecure_url_refused",
            "Plain HTTP is permitted only for an explicit loopback test fixture.",
            json!({"url":redacted(&url)}),
        ));
    }
    Ok(url)
}
fn validate_base_url(value: &str, worker: bool) -> Result<String, Value> {
    let url = validate_request_url(value.trim_end_matches('/'))?;
    if worker && url.path() != "/" {
        return Err(error(
            "cloudflare_worker_url_invalid",
            "Worker URL must be an origin without a path.",
            json!({"url":redacted(&url)}),
        ));
    }
    Ok(url.to_string().trim_end_matches('/').to_string())
}
fn allow_insecure(url: &Url) -> bool {
    env::var("NARADA_CLOUDFLARE_ALLOW_INSECURE_TEST")
        .ok()
        .as_deref()
        == Some("1")
        && url
            .host_str()
            .is_some_and(|v| matches!(v, "127.0.0.1" | "localhost" | "::1"))
}
fn cookie(state: &State) -> Option<String> {
    let v = bounded_json(&state.session_file).ok()?;
    let raw = v.get("cookie")?.as_str()?;
    let value = raw
        .split(';')
        .find_map(|part| part.trim().strip_prefix("narada_operator_session="))
        .unwrap_or(raw);
    (!value.is_empty()).then(|| value.to_string())
}
fn bounded_json(path: &Path) -> Result<Value, &'static str> {
    let meta = fs::symlink_metadata(path).map_err(|_| "missing")?;
    if !meta.is_file() || meta.file_type().is_symlink() {
        return Err("not_regular_file");
    }
    if meta.len() > MAX_FILE_BYTES {
        return Err("too_large");
    }
    serde_json::from_slice(&fs::read(path).map_err(|_| "read_failed")?).map_err(|_| "invalid_json")
}
fn optional_json(path: &Path) -> Option<Value> {
    bounded_json(path).ok()
}
fn bounded_metadata(path: &Path) -> Option<fs::Metadata> {
    let m = fs::symlink_metadata(path).ok()?;
    if !m.is_file() || m.file_type().is_symlink() || m.len() > MAX_FILE_BYTES {
        None
    } else {
        Some(m)
    }
}
fn summarize(operation: &str, body: &Value, continuation: bool) -> Value {
    match operation {
        "site.list" => {
            json!({"operation":operation,"site_count":body.pointer("/site_product_overview/site_count").cloned().unwrap_or(json!(0)),"next_health":body.pointer("/site_product_overview/next_health"),"next_action":body.pointer("/site_product_overview/next_action"),"next_reason":body.pointer("/site_product_overview/next_reason"),"health_counts":body.pointer("/site_product_overview/health_counts")})
        }
        "site.read" => {
            json!({"operation":operation,"site_id":body.pointer("/site/site_id").or_else(||body.get("site_id")),"health":body.pointer("/site_product_status/health").or_else(||body.pointer("/product_status/health")),"next_action":body.pointer("/site_product_status/next_action").or_else(||body.pointer("/product_status/next_action")),"continuity_state":body.pointer("/site_product_status/continuity_state")})
        }
        "operation.list" => {
            let ops = body
                .get("operations")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            let next = continuation
                .then(|| {
                    ops.iter()
                        .find(|v| {
                            v.get("status").and_then(Value::as_str) == Some("needs_continuation")
                        })
                        .and_then(|v| v.get("operation_id"))
                        .cloned()
                })
                .flatten();
            json!({"operation":operation,"operation_count":ops.len(),"needs_continuation_count":ops.iter().filter(|v|v.get("status").and_then(Value::as_str)==Some("needs_continuation")).count(),"next_continuation_id":next})
        }
        "operation.read" => {
            json!({"operation":operation,"operation_id":body.pointer("/operation/operation_id"),"current_status":body.pointer("/operation/status"),"phase":body.pointer("/operation_lifecycle_status/phase"),"health":body.pointer("/operation_lifecycle_status/health"),"next_action":body.pointer("/operation_lifecycle_status/next_action")})
        }
        _ => json!({"operation":operation}),
    }
}
fn joined(
    status: &str,
    code: Option<&str>,
    id: &str,
    carrier: Value,
    projection: Value,
    next: &str,
) -> Value {
    json!({"schema":"narada.cloudflare_carrier_mcp.carrier_health.v1","status":status,"code":code,"carrier_api":carrier,"projection":projection,"next_action":if next.is_empty(){Value::Null}else{json!(next)},"projection_id":id})
}
fn projection_unavailable(status: u16) -> String {
    if matches!(status, 401 | 403) {
        "projection_browser_access_refused".into()
    } else if status == 0 {
        "projection_unavailable".into()
    } else {
        format!("projection_http_{status}")
    }
}
fn legacy_projection_base(value: &Value) -> Option<String> {
    let endpoint = value
        .get("remote_registration")
        .and_then(|registration| registration.get("endpoint"))
        .and_then(Value::as_str)?;
    let suffix = "/api/nars/projections/register";
    let base = endpoint.strip_suffix(suffix)?.trim_end_matches('/');
    validate_base_url(base, false).ok()
}
fn operator_action(session: &Value) -> Value {
    match session.get("status").and_then(Value::as_str) {
        Some("missing") => json!("refresh_cloudflare_operator_session"),
        Some("present") if session.get("is_fresh").and_then(Value::as_bool) == Some(false) => {
            json!("refresh_cloudflare_operator_session_then_retry")
        }
        Some("incomplete") => json!("capture_cloudflare_operator_session_cookie"),
        _ => Value::Null,
    }
}
fn id(args: &Map<String, Value>, key: &str) -> Option<String> {
    args.get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(str::to_string)
}
fn string(v: Option<&Value>) -> Option<String> {
    v.and_then(Value::as_str)
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(str::to_string)
}
fn obj(v: Option<&Value>) -> Map<String, Value> {
    v.and_then(Value::as_object).cloned().unwrap_or_default()
}
fn env_path(names: &[&str]) -> Option<PathBuf> {
    names.iter().find_map(|name| {
        env::var(name)
            .ok()
            .filter(|v| !v.trim().is_empty())
            .map(PathBuf::from)
    })
}
fn confined(path: &Path, root: &Path) -> bool {
    let root = fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
    let parent = fs::canonicalize(path.parent().unwrap_or(path))
        .unwrap_or_else(|_| path.parent().unwrap_or(path).to_path_buf());
    parent.starts_with(root)
}
fn normalized(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}
fn redacted(url: &Url) -> String {
    let mut value = url.clone();
    let _ = value.set_username("");
    let _ = value.set_password(None);
    value.set_query(None);
    value.set_fragment(None);
    value.to_string()
}
fn sanitize(value: Value) -> Value {
    match value {
        Value::Object(object) => Value::Object(
            object
                .into_iter()
                .map(|(key, value)| {
                    let lower = key.to_ascii_lowercase();
                    let secret = [
                        "cookie",
                        "token",
                        "secret",
                        "password",
                        "authorization",
                        "credential",
                        "api_key",
                        "api-key",
                    ]
                    .iter()
                    .any(|needle| lower.contains(needle));
                    (
                        key,
                        if secret {
                            json!("[redacted]")
                        } else {
                            sanitize(value)
                        },
                    )
                })
                .collect(),
        ),
        Value::Array(values) => Value::Array(values.into_iter().take(500).map(sanitize).collect()),
        other => other,
    }
}
fn now() -> String {
    OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .unwrap_or_else(|_| "1970-01-01T00:00:00Z".into())
}
fn empty() -> Value {
    json!({"type":"object","properties":{},"additionalProperties":false})
}
fn tool(name: &str, description: &str, input: Value, read_only: bool) -> Value {
    json!({"name":name,"description":description,"inputSchema":input,"annotations":{"title":name,"readOnlyHint":read_only,"destructiveHint":false,"idempotentHint":true,"openWorldHint":true},"outputSchema":{"type":"object","additionalProperties":true}})
}
fn error(code: &str, message: &str, details: Value) -> Value {
    json!({"schema":"narada.cloudflare_carrier_mcp.error.v1","status":"unavailable","code":code,"message":message,"details":details})
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn schemas_are_closed_and_server_bound() {
        for tool in list_tools() {
            assert_eq!(tool["inputSchema"]["additionalProperties"], false);
            assert!(tool["inputSchema"]["properties"]
                .get("session_file")
                .is_none());
            assert!(tool["inputSchema"]["properties"]
                .get("health_file")
                .is_none());
        }
    }
    #[test]
    fn projection_ids_are_path_safe() {
        let root = std::env::temp_dir().join(format!("cloudflare-projection-{}", Uuid::new_v4()));
        let state = State {
            repo_root: root.clone(),
            site_root: root.clone(),
            session_file: root.join("s"),
            health_file: root.join("h"),
            projection_root: root.join("p"),
            worker_url: "https://example.com".into(),
        };
        fs::create_dir_all(&state.projection_root).unwrap();
        assert!(projection(&state, "../escape", &now()).is_err());
        assert!(projection(&state, "missing", &now()).unwrap().is_none());
        let _ = fs::remove_dir_all(root);
    }
}
