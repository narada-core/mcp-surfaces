fn operator_route_request(args: &Map<String, Value>, options: &Options) -> Result<Value, Value> {
    let transcript = required_string(args, "transcript")?;
    let target_runtime = required_string(args, "target_runtime")?;
    let target_identity = optional_string(args, "target_identity");
    let intent_kind = optional_string(args, "intent_kind");
    let speaker_agent_id = optional_string(args, "speaker_agent_id");
    let target_site_id = optional_string(args, "target_site_id");
    let target_site_root = optional_string(args, "target_site_root");
    let operation_kind = optional_string(args, "operation_kind")
        .or_else(|| infer_operation_kind(intent_kind.as_deref()));
    let role = optional_string(args, "role");
    let agent_kind = optional_string(args, "agent_kind");
    let principal = optional_string(args, "principal").or_else(|| speaker_agent_id.clone());
    let runtime_locus = optional_string(args, "runtime_locus");
    let runtime_handle = optional_string(args, "runtime_handle");
    let allow_inbox_fallback = args
        .get("allow_inbox_fallback")
        .and_then(Value::as_bool)
        .unwrap_or(true);
    let request_id = optional_string(args, "request_id").unwrap_or_else(|| {
        format!(
            "route_{}_{}",
            compact_timestamp(),
            &Uuid::new_v4().to_string()[..8]
        )
    });
    let request_fingerprint = operator_route_fingerprint(args)?;
    if let Some(existing) = find_route_record(&request_id, options)? {
        if existing.get("request_fingerprint").and_then(Value::as_str)
            != Some(request_fingerprint.as_str())
        {
            return Err(diagnostic(
                "operator_route_request_id_conflict",
                "request_id already names a different routing request",
                json!({"request_id":request_id}),
            ));
        }
        let mut replay = existing.as_object().cloned().unwrap_or_default();
        replay.insert("idempotency_replay".to_string(), json!(true));
        replay.insert(
            "log_path".to_string(),
            json!(route_log_path(options).to_string_lossy()),
        );
        return Ok(Value::Object(replay));
    }
    let recorded_at = now_iso();
    let spoken_text = if allow_inbox_fallback {
        "Request recorded. Direct delivery to that runtime is not available from this surface. I can route it through the admitted inbox path."
    } else {
        "Request recorded. Direct delivery to that runtime is not available from this surface, and no fallback path was enabled."
    };
    let route_kind = if allow_inbox_fallback {
        "inbox_fallback_draft"
    } else {
        "unroutable"
    };
    let handoff = operator_typed_handoff(
        operation_kind.as_deref(),
        &target_runtime,
        target_identity.as_deref(),
        target_site_id.as_deref(),
        target_site_root.as_deref(),
        role.as_deref(),
        agent_kind.as_deref(),
        principal.as_deref(),
        runtime_locus.as_deref(),
        runtime_handle.as_deref(),
    );
    let inbox_envelope = if allow_inbox_fallback {
        Some(json!({
            "kind": "command_request",
            "title": target_identity.as_ref().map(|id| format!("Route request for {id}")).unwrap_or_else(|| format!("Route request for {target_runtime}")),
            "summary": transcript.chars().take(240).collect::<String>(),
            "principal": speaker_agent_id,
            "target_role": Value::Null,
            "severity": 35,
            "authority_level": "operator_confirmed",
            "payload": { "request_id": request_id, "recorded_at": recorded_at, "transcript": transcript, "target_runtime": target_runtime, "target_identity": target_identity, "intent_kind": intent_kind, "speaker_agent_id": speaker_agent_id, "spoken_acknowledgement": spoken_text, "suggested_delivery_channel": "site-inbox", "typed_handoff": handoff }
        }))
    } else {
        None
    };
    let route_record = json!({
        "schema": "narada.operator_routing.route_request.v1",
        "status": if allow_inbox_fallback { "drafted_for_site_inbox" } else { "unroutable" },
        "request_id": request_id,
        "request_fingerprint": request_fingerprint,
        "recorded_at": recorded_at,
        "direct_delivery_supported": false,
        "direct_delivery_attempted": false,
        "direct_delivery_reason": "no_runtime_ingress_available",
        "target_runtime": target_runtime,
        "target_identity": target_identity,
        "intent_kind": intent_kind,
        "operation_kind": operation_kind,
        "speaker_agent_id": speaker_agent_id,
        "transcript": transcript,
        "routing": { "target_runtime": target_runtime, "target_identity": target_identity, "route_kind": route_kind, "fallback_channel": if allow_inbox_fallback { json!("site-inbox") } else { Value::Null }, "next_step": if handoff.is_some() { format!("handoff_to_{}", handoff.as_ref().and_then(|value| value.get("target_surface")).and_then(Value::as_str).unwrap_or("site-inbox")) } else if allow_inbox_fallback { "submit_to_site_inbox".to_string() } else { "none".to_string() }, "handoff": handoff },
        "spoken_acknowledgement": { "provider": "openai_api", "model": "tts-1", "voice": "nova", "text": spoken_text },
        "inbox_envelope": inbox_envelope
    });
    let log_path = append_route_record(&route_record, options)?;
    let mut result = route_record.as_object().cloned().unwrap_or_default();
    result.insert(
        "log_path".to_string(),
        Value::String(log_path.to_string_lossy().to_string()),
    );
    Ok(Value::Object(result))
}

fn operator_route_fingerprint(args: &Map<String, Value>) -> Result<String, Value> {
    let fields = [
        "transcript",
        "target_runtime",
        "target_identity",
        "intent_kind",
        "speaker_agent_id",
        "target_site_id",
        "target_site_root",
        "operation_kind",
        "role",
        "agent_kind",
        "principal",
        "runtime_locus",
        "runtime_handle",
        "allow_inbox_fallback",
    ];
    let canonical = Value::Object(
        fields
            .into_iter()
            .filter_map(|key| args.get(key).cloned().map(|value| (key.to_string(), value)))
            .collect(),
    );
    let bytes = serde_json::to_vec(&canonical).map_err(|cause| {
        diagnostic(
            "operator_route_fingerprint_failed",
            &cause.to_string(),
            Value::Null,
        )
    })?;
    Ok(format!("sha256:{:x}", Sha256::digest(bytes)))
}

fn route_log_path(options: &Options) -> PathBuf {
    options
        .log_root
        .clone()
        .unwrap_or_else(|| {
            options
                .site_root
                .join(".narada")
                .join("runtime")
                .join("operator-routing")
        })
        .join("operator-routing-log.jsonl")
}

fn find_route_record(request_id: &str, options: &Options) -> Result<Option<Value>, Value> {
    let path = route_log_path(options);
    if !path.exists() {
        return Ok(None);
    }
    let metadata = std::fs::metadata(&path).map_err(|cause| {
        diagnostic(
            "operator_route_log_read_failed",
            &cause.to_string(),
            Value::Null,
        )
    })?;
    if metadata.len() > 16 * 1024 * 1024 {
        return Err(diagnostic(
            "operator_route_log_too_large",
            "routing log exceeds the 16 MiB idempotency scan bound",
            json!({"path":path,"bytes":metadata.len()}),
        ));
    }
    let file = std::fs::File::open(&path).map_err(|cause| {
        diagnostic(
            "operator_route_log_read_failed",
            &cause.to_string(),
            Value::Null,
        )
    })?;
    for line in BufReader::new(file).lines().take(100_000) {
        let line = line.map_err(|cause| {
            diagnostic(
                "operator_route_log_read_failed",
                &cause.to_string(),
                Value::Null,
            )
        })?;
        let value: Value = serde_json::from_str(&line).map_err(|cause| {
            diagnostic(
                "operator_route_log_invalid",
                &cause.to_string(),
                Value::Null,
            )
        })?;
        if value.get("request_id").and_then(Value::as_str) == Some(request_id) {
            return Ok(Some(value));
        }
    }
    Ok(None)
}

fn infer_operation_kind(intent_kind: Option<&str>) -> Option<String> {
    let normalized = intent_kind
        .unwrap_or_default()
        .to_ascii_lowercase()
        .replace(['-', ' '], "_");
    if normalized.contains("bind") || normalized.contains("runtime") {
        Some("runtime_binding".to_string())
    } else if normalized.contains("admit")
        || normalized.contains("identity")
        || normalized.contains("role")
        || normalized.contains("instantiate")
    {
        Some("role_admission".to_string())
    } else {
        None
    }
}

