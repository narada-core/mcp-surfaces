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
        Some("starting") | Some("healthy") | Some("degraded") | Some("unhealthy") | Some("closing") | Some("unavailable") => result.get("status").and_then(Value::as_str).unwrap_or("unavailable"),
        _ => "unavailable",
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
    fn nars_catalog_exposes_exact_bounded_native_arguments() {
        let tools = nars_tools();
        assert_eq!(tools.len(), 5);
        for tool in &tools {
            assert_eq!(tool["inputSchema"]["additionalProperties"], false);
        }
        let list = tools.iter().find(|tool| tool["name"] == "nars_session_list").expect("list");
        assert_eq!(list["inputSchema"]["properties"]["include_health"]["type"], "boolean");
        let delivery = tools.iter().find(|tool| tool["name"] == "nars_session_input_deliver").expect("delivery");
        assert!(delivery["inputSchema"]["properties"].get("payload_ref").is_none());
        assert_eq!(delivery["inputSchema"]["properties"]["directive"]["additionalProperties"], false);
        assert_eq!(delivery["annotations"]["idempotentHint"], true);
        assert_eq!(delivery["annotations"]["destructiveHint"], false);
    }

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
