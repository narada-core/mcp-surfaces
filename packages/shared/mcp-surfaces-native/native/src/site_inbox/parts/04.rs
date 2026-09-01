fn severity(e: &Map<String, Value>) -> Severity {
    if let Some(role) = e.get("target_role").and_then(Value::as_str) {
        return Severity {
            role: Some(role.into()),
            value: Some(e.get("severity").and_then(Value::as_i64).unwrap_or(50)),
            reason: Some("explicit_target_role".into()),
            action: Some("materialize".into()),
        };
    }
    let kind = e
        .get("kind")
        .and_then(Value::as_str)
        .unwrap_or("observation");
    let authority = e
        .get("authority")
        .and_then(Value::as_object)
        .and_then(|a| a.get("level"))
        .and_then(Value::as_str)
        .unwrap_or("agent_reported");
    let payload = e.get("payload").and_then(Value::as_object);
    if kind == "incident" {
        return Severity {
            role: Some("architect".into()),
            value: Some(90),
            reason: Some("incident_always_materializes".into()),
            action: Some("materialize".into()),
        };
    }
    if payload
        .and_then(|p| p.get("capa_request"))
        .and_then(Value::as_object)
        .is_some()
    {
        let value = if authority == "operator_confirmed" || authority == "operator_directed" {
            75
        } else {
            60
        };
        return Severity {
            role: Some("architect".into()),
            value: Some(value),
            reason: Some("capa_request_requires_promotion_review".into()),
            action: Some("review_capa_request".into()),
        };
    }
    if kind == "observation" {
        let recommendation = payload
            .and_then(|p| p.get("recommendation"))
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_lowercase();
        let proposals = payload
            .and_then(|p| p.get("proposal"))
            .and_then(Value::as_array)
            .map(Vec::len)
            .unwrap_or(0);
        let (value, reason) = if recommendation.contains("address before next operational cycle") {
            (70, "observation_urgent_recommendation")
        } else if proposals >= 3 {
            (50, "observation_many_proposals")
        } else if proposals >= 1 {
            (30, "observation_some_proposals")
        } else {
            (20, "observation_low_severity")
        };
        return Severity {
            role: Some("architect".into()),
            value: Some(value),
            reason: Some(reason.into()),
            action: Some("materialize".into()),
        };
    }
    let (value, reason) = match kind {
        "proposal" => (40, "proposal_architect_triage"),
        "command_request" => (45, "command_request_architect_triage"),
        _ => (20, "default_architect_triage"),
    };
    Severity {
        role: Some("architect".into()),
        value: Some(value),
        reason: Some(reason.into()),
        action: Some("materialize".into()),
    }
}

fn effective(e: &Map<String, Value>, latest: Option<&Value>) -> String {
    match latest
        .and_then(Value::as_object)
        .and_then(|v| v.get("event_kind"))
        .and_then(Value::as_str)
    {
        Some("envelope_acknowledged") => "acknowledged",
        Some("envelope_dismissed") => "dismissed",
        Some("envelope_promoted") => "promoted",
        _ => e
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or("received"),
    }
    .into()
}

fn latest(root: &Path) -> std::collections::HashMap<String, Value> {
    let mut out = std::collections::HashMap::new();
    if let Ok(entries) = read_log(root) {
        for entry in entries {
            if let Some(id) = entry.get("envelope_id").and_then(Value::as_str) {
                out.insert(id.into(), entry);
            }
        }
    }
    out
}

fn envelope_files(root: &Path) -> Vec<PathBuf> {
    root.join(".ai/inbox-envelopes")
        .read_dir()
        .ok()
        .into_iter()
        .flatten()
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| path.extension().and_then(|s| s.to_str()) == Some("json"))
        .collect()
}

fn valid_id(value: &str) -> bool {
    value
        .strip_prefix("env_")
        .map(|rest| {
            !rest.is_empty()
                && rest
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
        })
        .unwrap_or(false)
}

fn admit(root: &Path, envelope: Value) -> Result<(String, String, Value), Value> {
    let mut object = envelope
        .as_object()
        .cloned()
        .ok_or_else(|| error("invalid_envelope", "invalid_envelope"))?;
    let id = object
        .get("envelope_id")
        .and_then(Value::as_str)
        .map(str::to_string)
        .unwrap_or_else(|| format!("env_{}", Uuid::new_v4()));
    let received = object
        .get("received_at")
        .and_then(Value::as_str)
        .map(str::to_string)
        .unwrap_or_else(now_iso);
    object.insert("envelope_id".into(), Value::String(id.clone()));
    object.insert("received_at".into(), Value::String(received.clone()));
    let directory = root.join(".ai/inbox-envelopes");
    fs::create_dir_all(&directory)
        .map_err(|e| error("inbox_envelope_directory_failed", &e.to_string()))?;
    let name = format!(
        "{}-{}.json",
        received.replace(':', "-").replace('.', "-"),
        id
    );
    let path = directory.join(name);
    let text = serde_json::to_string_pretty(&Value::Object(object.clone()))
        .map_err(|e| error("inbox_envelope_encode_failed", &e.to_string()))?;
    if text.len() as u64 > MAX_ENVELOPE_BYTES { return Err(error("inbox_envelope_too_large", "serialized inbox envelope exceeds 512000 bytes")); }
    fs::write(&path, text).map_err(|e| error("inbox_envelope_write_failed", &e.to_string()))?;
    let authority = object.get("authority").and_then(Value::as_object);
    let source = object.get("source").and_then(Value::as_object);
    let payload_uri = format!(
        ".ai/inbox-envelopes/{}",
        path.file_name().and_then(|v| v.to_str()).unwrap_or("")
    );
    append(
        root,
        json!({
            "envelope_id":id,"event_kind":"envelope_received",
            "principal":authority.and_then(|a|a.get("principal")).and_then(Value::as_str).unwrap_or("unknown"),
            "authority_level":authority.and_then(|a|a.get("level")).and_then(Value::as_str).unwrap_or("agent_reported"),
            "payload_hash":hash(&Value::Object(object.clone())),"payload_uri":payload_uri,
            "event_payload":{"source_ref":source.and_then(|s|s.get("ref")),"source_kind":source.and_then(|s|s.get("kind")),"target_locus":"local_site","transport":"mcp_cli"}
        }),
    )?;
    let event = append(
        root,
        json!({
            "envelope_id":id,"event_kind":"envelope_admitted","principal":"inbox_mcp",
            "authority_level":"system_detected","payload_hash":hash(&Value::Object(object)),
            "payload_uri":payload_uri,"event_payload":{"admission_gate":"inbox_mcp_submit","validation_result":"passed","routing_decision":"local_site"}
        }),
    )?;
    Ok((id, path.to_string_lossy().to_string(), event))
}

fn append(root: &Path, event: Value) -> Result<Value, Value> {
    let directory = root.join(".ai/state");
    fs::create_dir_all(&directory)
        .map_err(|e| error("inbox_log_directory_failed", &e.to_string()))?;
    let path = directory.join("inbox-admission.log");
    if path.metadata().map(|m| m.len()).unwrap_or(0) >= 10 * 1024 * 1024 {
        let rotated = directory.join(format!("inbox-admission-{}.log", &now_iso()[..10]));
        let old = fs::read_to_string(&path).unwrap_or_default();
        fs::write(rotated, old).map_err(|e| error("inbox_log_rotation_failed", &e.to_string()))?;
        fs::write(&path, "").map_err(|e| error("inbox_log_rotation_failed", &e.to_string()))?;
    }
    let sequence = read_log(root)?
        .last()
        .and_then(|e| e.get("event_sequence"))
        .and_then(Value::as_i64)
        .unwrap_or(0)
        + 1;
    let mut result = Map::new();
    result.insert(
        "schema".into(),
        Value::String("narada.inbox.admission_log.entry.v0".into()),
    );
    result.insert(
        "event_id".into(),
        Value::String(format!(
            "evt_{}",
            Uuid::new_v4().to_string().replace('-', "")
        )),
    );
    result.insert("event_sequence".into(), Value::from(sequence));
    result.insert("timestamp".into(), Value::String(now_iso()));
    if let Some(input) = event.as_object() {
        for (key, value) in input {
            result.insert(key.clone(), value.clone());
        }
    }
    let line = serde_json::to_string(&Value::Object(result.clone()))
        .map_err(|e| error("inbox_log_encode_failed", &e.to_string()))?;
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|e| error("inbox_log_open_failed", &e.to_string()))?;
    writeln!(file, "{line}").map_err(|e| error("inbox_log_write_failed", &e.to_string()))?;
    Ok(Value::Object(result))
}

fn read_log(root: &Path) -> Result<Vec<Value>, Value> {
    let path = root.join(".ai/state/inbox-admission.log");
    if let Ok(metadata) = fs::metadata(&path) { if metadata.len() > MAX_LOG_BYTES { return Err(error("inbox_log_too_large", "inbox_log_too_large")); } }
    let text = match fs::read_to_string(path) {
        Ok(value) => value,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(error("inbox_log_read_failed", &e.to_string())),
    };
    Ok(text
        .lines()
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect())
}

fn hash(value: &Value) -> String {
    let mut digest = Sha256::new();
    digest.update(serde_json::to_vec(value).unwrap_or_default());
    let encoded = digest
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    format!("sha256:{encoded}")
}

fn required(args: &Map<String, Value>, key: &str) -> Result<String, Value> {
    optional_string(args, key)
        .ok_or_else(|| error("required_argument_missing", &format!("{key}_required")))
}
fn optional_string(args: &Map<String, Value>, key: &str) -> Option<String> {
    args.get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(str::to_string)
}
