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

