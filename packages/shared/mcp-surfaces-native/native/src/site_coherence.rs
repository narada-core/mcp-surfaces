use serde_json::{json, Map, Value};
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::Duration;
use time::OffsetDateTime;

const MAX_JSON_BYTES: u64 = 256_000;

pub fn list_tools() -> Vec<Value> {
    vec![
        tool(
            "site_coherence_guidance",
            "Show model-facing operating guidance for site coherence workflows.",
            json!({"type":"object","properties":{"workflow":{"type":"string"},"tool":{"type":"string"}},"additionalProperties":false}),
        ),
        tool(
            "site_coherence_check",
            "Check local site continuity posture and report whether a Cloudflare comparison is available.",
            json!({"type":"object","properties":{"site_id":{"type":"string","minLength":1,"maxLength":256},"fetch_cloudflare":{"type":"boolean","default":true}},"required":["site_id"],"additionalProperties":false}),
        ),
        tool(
            "site_coherence_doctor",
            "Check site coherence MCP readiness: health file, bindings, operator session, and worker URL.",
            json!({"type":"object","properties":{},"additionalProperties":false}),
        ),
    ]
}

pub fn call_tool(name: &str, args: &Map<String, Value>, root: &Path) -> Result<Value, Value> {
    match name {
        "site_coherence_guidance" => Ok(guidance(args)),
        "site_coherence_check" => check(args, root),
        "site_coherence_doctor" => Ok(doctor(root)),
        _ => Err(diagnostic("unknown_tool", &format!("unknown_tool:{name}"))),
    }
}

fn guidance(args: &Map<String, Value>) -> Value {
    json!({
        "schema":"narada.mcp_surface.guidance.v0","status":"ok","surface_id":"site-coherence",
        "guidance_tool":"site_coherence_guidance","purpose":"Compare bounded local continuity posture with an explicitly requested remote posture.",
        "requested":{"workflow":args.get("workflow").cloned().unwrap_or(Value::Null),"tool":args.get("tool").cloned().unwrap_or(Value::Null)},
        "first_use":["Run site_coherence_doctor to inspect local readiness.","Use site_coherence_check with fetch_cloudflare false for a local-only evidence check.","Treat remote-unavailable results as degraded evidence, not as proof of coherence."],
        "boundaries":["This surface is read-only.","The native implementation forwards only the server-bound operator session cookie to the configured Cloudflare carrier and never returns it.","Remote responses are size- and time-bounded; failures become sanitized degraded evidence."]
    })
}

fn check(args: &Map<String, Value>, root: &Path) -> Result<Value, Value> {
    let site_id = required_string(args, "site_id")?;
    let fetch_cloudflare = args
        .get("fetch_cloudflare")
        .and_then(Value::as_bool)
        .unwrap_or(true);
    let paths = Paths::new(root);
    let health = read_json(&paths.health_file);
    let Some(health) = health else {
        let exists = paths.health_file.exists();
        return Ok(json!({
            "schema":"narada.site_coherence.check.v1","status":if exists {"invalid_local"} else {"missing_local"},"site_id":site_id,
            "health_file":paths.health_file.to_string_lossy(),"local":null,"cloudflare":null,
            "coherence":{"state":"unknown","local_available":false,"cloudflare_available":false},
            "attention":[if exists {"local_health_snapshot_invalid_or_oversized"} else {"local_health_snapshot_missing"}]
        }));
    };
    let sync_file = paths
        .continuity_dir
        .join(format!("{site_id}-cloudflare-sync.json"));
    let sync = read_json(&sync_file);
    let local = local_posture(&health, sync.as_ref(), &site_id);
    let (cloudflare, cloudflare_error) = if fetch_cloudflare {
        match fetch_cloudflare_site_read(&paths, &site_id) {
            Ok(response) => (Some(cloudflare_posture(&response, &site_id)), None),
            Err(error) => (None, Some(error)),
        }
    } else {
        (None, None)
    };
    let coherence = compute_coherence(
        &local,
        cloudflare.as_ref(),
        cloudflare_error.as_ref(),
        fetch_cloudflare,
        &site_id,
    );
    Ok(json!({
        "schema":"narada.site_coherence.check.v1","status":"ok","site_id":site_id,
        "checked_at":now_iso(),"local":local,"cloudflare":cloudflare,"coherence":coherence,
        "implementation":"rust-native"
    }))
}

fn worker_url() -> String {
    std::env::var("CLOUDFLARE_CARRIER_URL")
        .ok()
        .map(|value| value.trim().trim_end_matches('/').to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| {
            "https://narada-cloudflare-carrier.andrei-kokoev.workers.dev".to_string()
        })
}

fn fetch_cloudflare_site_read(paths: &Paths, site_id: &str) -> Result<Value, Value> {
    let url = format!("{}/api/carrier", worker_url());
    let agent = ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(5))
        .timeout_read(Duration::from_secs(10))
        .timeout_write(Duration::from_secs(10))
        .build();
    let mut request = agent.post(&url).set("content-type", "application/json");
    if let Some(cookie) = session_cookie(&paths.session_file) {
        request = request.set("cookie", &format!("narada_operator_session={cookie}"));
    }
    let body = json!({"operation":"site.read","request_id":format!("coherence_site_read_{}",OffsetDateTime::now_utc().unix_timestamp_nanos()),"params":{"site_id":site_id}});
    match request.send_string(&body.to_string()) {
        Ok(response) => read_remote_response(response),
        Err(ureq::Error::Status(status, response)) => {
            let body = read_remote_response(response).unwrap_or_else(|_| json!({}));
            let code = body
                .get("code")
                .and_then(Value::as_str)
                .unwrap_or("unknown");
            Err(
                json!({"code":"site_coherence_cloudflare_read_failed","message":format!("site_read_failed:{status}:{code}"),"details":{"status":status,"code":code,"operator_action":if status==401 {"authenticate_the_cloudflare_operator_session"} else {"inspect_cloudflare_site_coherence_and_operator_membership"}}}),
            )
        }
        Err(ureq::Error::Transport(error)) => Err(
            json!({"code":"site_coherence_cloudflare_transport_failed","message":"site_coherence_cloudflare_transport_failed","details":{"kind":error.kind().to_string(),"operator_action":"check_the_configured_cloudflare_carrier_endpoint"}}),
        ),
    }
}

fn read_remote_response(response: ureq::Response) -> Result<Value, Value> {
    let mut bytes = Vec::new();
    response
        .into_reader()
        .take(MAX_JSON_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| {
            diagnostic(
                "site_coherence_cloudflare_response_read_failed",
                "site_coherence_cloudflare_response_read_failed",
            )
        })?;
    if bytes.len() as u64 > MAX_JSON_BYTES {
        return Err(diagnostic(
            "site_coherence_cloudflare_response_too_large",
            "site_coherence_cloudflare_response_too_large",
        ));
    }
    serde_json::from_slice(&bytes).map_err(|_| {
        diagnostic(
            "site_coherence_cloudflare_response_invalid",
            "site_coherence_cloudflare_response_invalid",
        )
    })
}

fn session_cookie(path: &Path) -> Option<String> {
    let raw = read_json(path)?.get("cookie")?.as_str()?.trim().to_string();
    if raw.is_empty() {
        return None;
    }
    raw.split(';')
        .find_map(|part| {
            part.trim()
                .strip_prefix("narada_operator_session=")
                .map(str::to_string)
        })
        .or(Some(raw))
}

fn cloudflare_posture(response: &Value, site_id: &str) -> Value {
    let status = response
        .get("site_product_status")
        .or_else(|| response.get("product_status"))
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    let nested = |name: &str| {
        status
            .get(name)
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default()
    };
    json!({
        "schema":"narada.site_coherence.cloudflare_posture.v1","site_id":site_id,
        "site_record_available":response.get("site").is_some_and(|value|!value.is_null()),
        "health":status.get("health").cloned().unwrap_or(Value::Null),
        "next_action":status.get("next_action").cloned().unwrap_or(Value::Null),
        "continuity_state":status.get("continuity_state").cloned().unwrap_or(Value::Null),
        "continuity_direction_state":status.get("continuity_direction_state").cloned().unwrap_or(Value::Null),
        "continuity_direction_missing":status.get("continuity_direction_missing").cloned().unwrap_or(Value::Null),
        "continuity_loop_state":status.get("continuity_loop_state").cloned().unwrap_or(Value::Null),
        "continuity_reconciliation_state":status.get("continuity_reconciliation_execution_state").cloned().unwrap_or(Value::Null),
        "continuity_reconciliation_health":nested("site_continuity_reconciliation_execution_status").get("health").cloned().or_else(||status.get("continuity_reconciliation_execution_health").cloned()).unwrap_or(Value::Null),
        "continuity_packet_count":status.get("continuity_packet_count").cloned().unwrap_or(json!(0)),
        "continuity_loop_report_count":status.get("continuity_loop_report_count").cloned().unwrap_or(json!(0)),
        "persistence_state":nested("cloudflare_persistence_posture").get("state").cloned().or_else(||response.get("cloudflare_persistence_posture").and_then(|value|value.get("state")).cloned()).unwrap_or(Value::Null),
        "recovery_state":nested("cloudflare_recovery_posture").get("state").cloned().or_else(||response.get("cloudflare_recovery_posture").and_then(|value|value.get("state")).cloned()).unwrap_or(Value::Null),
        "session_count":status.get("session_count").cloned().unwrap_or(json!(0)),
        "membership_count":response.get("memberships").and_then(Value::as_array).map(Vec::len).unwrap_or(0),
        "raw_next_action":status.get("next_action").cloned().unwrap_or(Value::Null)
    })
}

fn compute_coherence(
    local: &Value,
    cloudflare: Option<&Value>,
    error: Option<&Value>,
    fetch_requested: bool,
    site_id: &str,
) -> Value {
    if cloudflare.is_none() && fetch_requested {
        let message = error
            .and_then(|value| value.get("message"))
            .and_then(Value::as_str);
        let mut attention = vec![json!("cloudflare_unavailable")];
        if let Some(message) = message {
            attention.push(json!(format!("cloudflare_error:{message}")));
        }
        return json!({"state":"degraded","site_id":site_id,"mismatches":[],"attention":attention,"local_next_action":local["overall_product_next_action"],"cloudflare_next_action":Value::Null,"posture_agrees":false,"diagnosis":"cloudflare_site_read_unavailable_cannot_compare","operator_action":error.and_then(|value|value.pointer("/details/operator_action")).cloned().unwrap_or(Value::Null)});
    }
    let Some(cloudflare) = cloudflare else {
        return json!({"state":"local_only","site_id":site_id,"mismatches":[],"attention":[],"local_next_action":local["overall_product_next_action"],"cloudflare_next_action":Value::Null,"posture_agrees":Value::Null,"diagnosis":"cloudflare_not_queried"});
    };
    let local_action = local
        .get("overall_product_next_action")
        .filter(|value| !value.is_null())
        .cloned()
        .unwrap_or(json!("unknown"));
    let remote_action = cloudflare
        .get("next_action")
        .filter(|value| !value.is_null())
        .cloned()
        .unwrap_or(json!("unknown"));
    let mut mismatches = Vec::new();
    if local_action != remote_action {
        mismatches.push(json!({"field":"next_action","local":local_action,"cloudflare":remote_action,"severity":"mismatch","description":format!("Local and Cloudflare next_action differ for site {site_id}.")}));
    }
    let mut attention = Vec::new();
    if local["local_sync_status"] != "synced" || local["local_inbound_status"] != "synced" {
        attention.push(json!("local_sync_degraded"));
    }
    if local["scheduler_task_state"] != "Enabled"
        || local["scheduler_last_result"] != "0"
        || local["scheduler_cadence"] != "matches_plan"
    {
        attention.push(json!("scheduler_degraded"));
    }
    if let Some(state) = cloudflare.get("continuity_state").and_then(Value::as_str) {
        if state != "synced" && state != "ready" {
            attention.push(json!(format!("cloudflare_continuity:{state}")));
        }
    }
    let state = if !mismatches.is_empty() {
        "mismatch"
    } else if !attention.is_empty() {
        "attention"
    } else {
        "coherent"
    };
    json!({"state":state,"site_id":site_id,"mismatches":mismatches,"attention":attention,"local_next_action":local_action,"cloudflare_next_action":remote_action,"posture_agrees":mismatches.is_empty(),"posture_attention":!attention.is_empty(),"diagnosis":if state=="coherent" {"posture_coherent"} else if state=="mismatch" {"posture_mismatch"} else {"attention_required"}})
}

fn doctor(root: &Path) -> Value {
    let paths = Paths::new(root);
    let health = read_json(&paths.health_file);
    let bindings = read_json(&paths.bindings_file);
    let session = read_json(&paths.session_file);
    let health_exists = paths.health_file.exists();
    let bindings_exists = paths.bindings_file.exists();
    let session_exists = paths.session_file.exists();
    let health_status = health
        .as_ref()
        .and_then(|value| value.get("status"))
        .cloned()
        .unwrap_or_else(|| {
            if health_exists {
                json!("invalid_json")
            } else {
                Value::Null
            }
        });
    let health_generated_at = health
        .as_ref()
        .and_then(|value| {
            value
                .get("generated_at")
                .or_else(|| value.get("persisted_at"))
        })
        .cloned()
        .unwrap_or(Value::Null);
    let bindings_count = bindings
        .as_ref()
        .and_then(|value| value.get("bindings"))
        .and_then(Value::as_array)
        .map(Vec::len)
        .unwrap_or(0);
    let session_has_cookie = session
        .as_ref()
        .and_then(|value| value.get("cookie"))
        .and_then(Value::as_str)
        .map(|value| !value.is_empty())
        .unwrap_or(false);
    json!({
        "schema":"narada.site_coherence.doctor.v1","status":"ok","repo_root":root.to_string_lossy(),
        "worker_url":worker_url(),
        "health_file":paths.health_file.to_string_lossy(),"health_exists":health_exists,"health_status":health_status,"health_generated_at":health_generated_at,
        "bindings_file":paths.bindings_file.to_string_lossy(),"bindings_exist":bindings_exists,"bindings_count":bindings_count,
        "session_file":paths.session_file.to_string_lossy(),"session_exists":session_exists,"session_has_cookie":session_has_cookie,
        "implementation":"rust-native"
    })
}

fn local_posture(health: &Value, sync: Option<&Value>, site_id: &str) -> Value {
    let continuity = object_field(health, "continuity_health");
    let binding = object_field(health, "cloudflare_product_binding_alignment");
    let scheduler = object_field(health, "scheduler_task_readback");
    let product = object_field(health, "cloudflare_product_posture");
    let overview = product
        .get("site_product_overview")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    json!({
        "schema":"narada.site_coherence.local_posture.v1","site_id":site_id,"health_file_exists":true,
        "health_generated_at":health.get("generated_at").or_else(||health.get("persisted_at")).cloned().unwrap_or(Value::Null),
        "local_sync_status":continuity.get("local_sync_status").cloned().unwrap_or(Value::Null),
        "local_sync_artifacts":continuity.get("local_sync_artifact_count").cloned().unwrap_or(json!(0)),
        "local_inbound_status":continuity.get("local_inbound_status").cloned().unwrap_or(Value::Null),
        "local_inbound_artifacts":continuity.get("local_inbound_artifact_count").cloned().unwrap_or(json!(0)),
        "reconciliation_status":continuity.get("reconciliation_execution_status").cloned().unwrap_or(Value::Null),
        "reconciliation_plan":continuity.get("reconciliation_execution_plan_status").cloned().unwrap_or(Value::Null),
        "scheduler_task_state":scheduler.get("scheduled_task_state").cloned().unwrap_or(Value::Null),
        "scheduler_last_run":scheduler.get("last_run_time").cloned().unwrap_or(Value::Null),
        "scheduler_last_result":scheduler.get("last_result").cloned().unwrap_or(Value::Null),
        "scheduler_next_run":scheduler.get("next_run_time").cloned().unwrap_or(Value::Null),
        "scheduler_cadence":scheduler.get("cadence_status").cloned().unwrap_or(Value::Null),
        "overall_product_posture_state":product.get("state").cloned().unwrap_or(Value::Null),
        "overall_product_next_action":overview.get("next_action").cloned().or_else(||product.get("site_posture_route").and_then(|v|v.get("next_action")).cloned()).unwrap_or(Value::Null),
        "binding_alignment_state":binding.get("state").cloned().unwrap_or(Value::Null),
        "binding_alignment_reason":binding.get("reason").cloned().unwrap_or(Value::Null),
        "has_site_sync":sync.is_some(),"site_sync_status":sync.and_then(|v|v.get("status")).cloned().unwrap_or(Value::Null),
        "site_sync_admission_action":sync.and_then(|v|v.get("local_packet_admission")).and_then(|v|v.get("action")).cloned().unwrap_or(Value::Null),
        "cloudflare_admission_action":sync.and_then(|v|v.get("cloudflare_packet_admission")).and_then(|v|v.get("action")).cloned().unwrap_or(Value::Null)
    })
}

fn object_field(value: &Value, key: &str) -> Map<String, Value> {
    value
        .get(key)
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default()
}

struct Paths {
    continuity_dir: PathBuf,
    health_file: PathBuf,
    bindings_file: PathBuf,
    session_file: PathBuf,
}

impl Paths {
    fn new(root: &Path) -> Self {
        let continuity_dir = root.join(".narada").join("site-continuity");
        Self {
            health_file: continuity_dir
                .join("health")
                .join("cloudflare-continuity-health-last.json"),
            bindings_file: continuity_dir.join("bindings.json"),
            session_file: root
                .join(".narada")
                .join("auth")
                .join("cloudflare-operator-session.json"),
            continuity_dir,
        }
    }
}

fn read_json(path: &Path) -> Option<Value> {
    if fs::metadata(path).ok()?.len() > MAX_JSON_BYTES {
        return None;
    }
    let content = fs::read_to_string(path).ok()?;
    serde_json::from_str(&content).ok()
}

fn required_string(args: &Map<String, Value>, key: &str) -> Result<String, Value> {
    let value = args
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
        .ok_or_else(|| {
            diagnostic(
                "site_coherence_requires_site_id",
                "site_coherence_requires_site_id",
            )
        })?;
    if value.len() > 256
        || value.chars().any(|character| {
            character.is_control()
                || matches!(
                    character,
                    '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|'
                )
        })
    {
        return Err(diagnostic(
            "site_coherence_site_id_invalid",
            "site_coherence_site_id_invalid",
        ));
    }
    Ok(value)
}

fn now_iso() -> String {
    OffsetDateTime::now_utc()
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_else(|_| "1970-01-01T00:00:00Z".to_string())
}

fn tool(name: &str, description: &str, schema: Value) -> Value {
    json!({
        "name":name,"description":description,"inputSchema":schema,
        "annotations":{"title":name,"readOnlyHint":true,"destructiveHint":false,"idempotentHint":true,"openWorldHint":false},
        "outputSchema":{"type":"object","additionalProperties":true}
    })
}

fn diagnostic(code: &str, message: &str) -> Value {
    json!({"code":code,"message":message})
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::{create_dir_all, write};

    #[test]
    fn native_site_coherence_doctor_is_bounded() {
        let root =
            std::env::temp_dir().join(format!("narada-site-coherence-{}", std::process::id()));
        let health = root.join(".narada/site-continuity/health");
        create_dir_all(&health).unwrap();
        write(
            health.join("cloudflare-continuity-health-last.json"),
            r#"{"status":"ready","generated_at":"2026-01-01T00:00:00Z"}"#,
        )
        .unwrap();
        let result = doctor(&root);
        assert_eq!(result["status"], "ok");
        assert_eq!(result["health_status"], "ready");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn local_only_check_does_not_claim_remote_coherence() {
        let root = std::env::temp_dir().join(format!(
            "narada-site-coherence-check-{}",
            std::process::id()
        ));
        let health = root.join(".narada/site-continuity/health");
        create_dir_all(&health).unwrap();
        write(
            health.join("cloudflare-continuity-health-last.json"),
            r#"{"cloudflare_product_posture":{"state":"ready"}}"#,
        )
        .unwrap();
        let mut args = Map::new();
        args.insert("site_id".to_string(), json!("demo"));
        args.insert("fetch_cloudflare".to_string(), json!(false));
        let result = check(&args, &root).unwrap();
        assert_eq!(result["coherence"]["state"], "local_only");
        assert_eq!(result["coherence"]["posture_agrees"], Value::Null);
        let _ = fs::remove_dir_all(root);
    }
}
