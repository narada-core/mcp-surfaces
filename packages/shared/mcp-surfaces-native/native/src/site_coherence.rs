use serde_json::{json, Map, Value};
use std::fs;
use std::path::{Path, PathBuf};
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
            json!({"type":"object","properties":{"site_id":{"type":"string"},"fetch_cloudflare":{"type":"boolean","default":true}},"required":["site_id"],"additionalProperties":false}),
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
        "boundaries":["This surface is read-only.","The native implementation never forwards session cookies or performs mutation.","Cloudflare comparison requires an injected Rust authority port; absent that port it is reported unavailable."]
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
        return Ok(json!({
            "schema":"narada.site_coherence.check.v1","status":"missing_local","site_id":site_id,
            "health_file":paths.health_file.to_string_lossy(),"local":null,"cloudflare":null,
            "coherence":{"state":"unknown","local_available":false,"cloudflare_available":false},
            "attention":["local_health_snapshot_missing"]
        }));
    };
    let sync_file = paths
        .continuity_dir
        .join(format!("{site_id}-cloudflare-sync.json"));
    let sync = read_json(&sync_file);
    let local = local_posture(&health, sync.as_ref(), &site_id);
    let coherence = if fetch_cloudflare {
        json!({
            "state":"degraded","site_id":site_id,"mismatches":[],
            "attention":["cloudflare_unavailable","cloudflare_authority_port_not_injected"],
            "local_next_action":local["overall_product_next_action"],"cloudflare_next_action":null,
            "posture_agrees":false,"diagnosis":"cloudflare_site_read_unavailable_cannot_compare",
            "operator_action":"inject_or_select_a Rust Cloudflare authority port before remote comparison"
        })
    } else {
        json!({
            "state":"local_only","site_id":site_id,"mismatches":[],"attention":[],
            "local_next_action":local["overall_product_next_action"],"cloudflare_next_action":null,
            "posture_agrees":null,"diagnosis":"cloudflare_not_queried"
        })
    };
    Ok(json!({
        "schema":"narada.site_coherence.check.v1","status":"ok","site_id":site_id,
        "checked_at":now_iso(),"local":local,"cloudflare":null,"coherence":coherence,
        "implementation":"rust-native"
    }))
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
        "worker_url":std::env::var("CLOUDFLARE_CARRIER_URL").unwrap_or_else(|_| "https://narada-cloudflare-carrier.andrei-kokoev.workers.dev".to_string()),
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
    if fs::metadata(path).ok()?.len() > MAX_JSON_BYTES { return None; }
    let content = fs::read_to_string(path).ok()?;
    serde_json::from_str(&content).ok()
}

fn required_string(args: &Map<String, Value>, key: &str) -> Result<String, Value> {
    args.get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
        .ok_or_else(|| {
            diagnostic(
                "site_coherence_requires_site_id",
                "site_coherence_requires_site_id",
            )
        })
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
