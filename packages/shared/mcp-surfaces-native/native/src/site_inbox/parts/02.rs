fn submit(args: &Map<String, Value>, root: &Path) -> Result<Value, Value> {
    let kind = required(args, "kind")?;
    if !KINDS.contains(&kind.as_str()) {
        return Err(error(
            "invalid_envelope_kind",
            &format!("invalid_envelope_kind:{kind}; allowed={}", KINDS.join(",")),
        ));
    }
    let title = required(args, "title")?;
    let principal = required(args, "principal")?;
    if let Some(role) = optional_string(args, "target_role") {
        if !ROLES.contains(&role.as_str()) {
            return Err(error(
                "invalid_request",
                "target_role_must_be_architect_builder_or_operator",
            ));
        }
    }
    let submission_fingerprint = submission_fingerprint(args);
    let idempotency_key = optional_string(args, "idempotency_key");
    let envelope_id = idempotency_key.as_ref().map(|key| {
        let digest = hash(&json!(key));
        format!("env_submit_{}", &digest[7..47])
    });
    if let Some(id) = envelope_id.as_deref() {
        refresh(root)?;
        if let Some(row) = read_row(root, id)? {
            let path = row.get("file_path").and_then(Value::as_str).ok_or_else(|| error("inbox_idempotency_record_invalid", "existing idempotent envelope has no file path"))?;
            if fs::metadata(path).map_err(|e| error("inbox_idempotency_record_invalid", &e.to_string()))?.len() > MAX_ENVELOPE_BYTES { return Err(error("inbox_idempotency_record_invalid", "existing idempotent envelope exceeds size bound")); }
            let existing: Value = serde_json::from_str(&fs::read_to_string(path).map_err(|e| error("inbox_idempotency_record_invalid", &e.to_string()))?).map_err(|e| error("inbox_idempotency_record_invalid", &e.to_string()))?;
            if existing.get("submission_fingerprint").and_then(Value::as_str) != Some(submission_fingerprint.as_str()) {
                return Err(error("inbox_idempotency_key_conflict", "idempotency_key already names a different inbox submission"));
            }
            return Ok(json!({"status":"replayed","idempotency_replay":true,"site_root":root.to_string_lossy(),"envelope_id":id,"envelope_path":path}));
        }
    }
    let mut payload = args
        .get("payload")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    payload.insert("title".into(), Value::String(title.clone()));
    payload.insert(
        "summary".into(),
        args.get("summary").cloned().unwrap_or(Value::Null),
    );
    payload.insert("principal".into(), Value::String(principal.clone()));
    let authority = json!({
        "level": optional_string(args,"authority_level").unwrap_or_else(||"agent_reported".into()),
        "principal": principal
    });
    let source = json!({"kind":"inbox_mcp_submit","principal":principal});
    let mut envelope = json!({
        "kind":kind, "title":title, "summary":args.get("summary").cloned().unwrap_or(Value::Null),
        "status":"received", "target_role":args.get("target_role").cloned().unwrap_or(Value::Null),
        "severity":args.get("severity").cloned().unwrap_or(Value::Null),
        "submission_fingerprint":submission_fingerprint,
        "authority":authority, "source":source, "payload":payload
    });
    if let Some(id) = envelope_id { envelope["envelope_id"] = json!(id); }
    let (id, path, event) = admit(root, envelope)?;
    refresh(root)?;
    Ok(json!({
        "status":"admitted","site_root":root.to_string_lossy(),"envelope_id":id,
        "envelope_path":path,"event_id":event.get("event_id"),"event_sequence":event.get("event_sequence")
    }))
}

fn submission_fingerprint(args: &Map<String, Value>) -> String {
    let ordered = args.iter().filter(|(key, _)| key.as_str() != "idempotency_key")
        .map(|(key, value)|(key.clone(), value.clone())).collect::<std::collections::BTreeMap<_,_>>();
    hash(&serde_json::to_value(ordered).unwrap_or(Value::Null))
}

fn disposition(args: &Map<String, Value>, root: &Path, status: &str) -> Result<Value, Value> {
    let id = required(args, "envelope_id")?;
    let principal = required(args, "principal")?;
    let Some(existing) = read_row(root, &id)? else {
        return Ok(json!({"status":"not_found","envelope_id":id}));
    };
    let reason = optional_string(args, "reason");
    if status == "dismissed" && reason.is_none() {
        return Err(error("reason_required", "reason_required"));
    }
    if existing.get("status").and_then(Value::as_str) == Some(status) {
        return Ok(json!({"status":status,"envelope_id":id,"idempotency_replay":true,"reason":reason}));
    }
    let event = append(
        root,
        json!({
            "envelope_id":id, "event_kind":format!("envelope_{status}"), "principal":principal,
            "authority_level":"agent_reported", "event_payload":{"reason":reason}
        }),
    )?;
    refresh(root)?;
    Ok(json!({
        "status":status,"envelope_id":id,"event_id":event.get("event_id"),
        "event_sequence":event.get("event_sequence"),"reason":reason
    }))
}

fn doctor(root: &Path) -> Result<Value, Value> {
    let (indexed, invalid) = refresh(root)?;
    let rows = rows_after_refresh(root)?;
    let mut counts = Map::new();
    for row in rows
        .iter()
        .filter(|r| r.get("status").and_then(Value::as_str) == Some("received"))
    {
        inc(&mut counts, "total");
        if row.get("severity").and_then(Value::as_i64).unwrap_or(0) >= 70 {
            inc(&mut counts, "high_severity");
        }
        if row.get("kind").and_then(Value::as_str) == Some("incident") {
            inc(&mut counts, "incidents");
        }
        if row.get("action").and_then(Value::as_str) == Some("review_capa_request") {
            inc(&mut counts, "capa_requests");
        }
        if row.get("kind").and_then(Value::as_str) == Some("observation") {
            inc(&mut counts, "observations");
        }
        if row.get("kind").and_then(Value::as_str) == Some("proposal") {
            inc(&mut counts, "proposals");
        }
    }
    for key in [
        "total",
        "high_severity",
        "incidents",
        "capa_requests",
        "observations",
        "proposals",
    ] {
        counts.entry(key).or_insert(Value::from(0));
    }
    Ok(json!({
        "status":"ok","site_root":root.to_string_lossy(),
        "db_path":root.join(".ai/state/inbox-index.sqlite").to_string_lossy(),
        "storage_mode":"native_sqlite","indexed_count":indexed,"invalid_count":invalid,
        "counts":counts,"server_name":SERVER_NAME
    }))
}

fn inc(map: &mut Map<String, Value>, key: &str) {
    let n = map.get(key).and_then(Value::as_i64).unwrap_or(0) + 1;
    map.insert(key.into(), Value::from(n));
}

fn list(args: &Map<String, Value>, root: &Path) -> Result<Value, Value> {
    let status = enum_arg(args, "status", Some("received"), STATUSES)?;
    let kind = enum_arg(args, "kind", None, KINDS)?;
    let role = enum_arg(args, "target_role", None, ROLES)?;
    let action = enum_arg(args, "action", None, ACTIONS)?;
    let limit = bounded(args.get("limit"), 20, 100);
    let mut rows = rows(root)?;
    rows.retain(|r| {
        status
            .as_deref()
            .map(|v| r.get("status").and_then(Value::as_str) == Some(v))
            .unwrap_or(true)
    });
    rows.retain(|r| {
        kind.as_deref()
            .map(|v| r.get("kind").and_then(Value::as_str) == Some(v))
            .unwrap_or(true)
    });
    rows.retain(|r| {
        role.as_deref()
            .map(|v| r.get("target_role").and_then(Value::as_str) == Some(v))
            .unwrap_or(true)
    });
    rows.retain(|r| {
        action
            .as_deref()
            .map(|v| r.get("action").and_then(Value::as_str) == Some(v))
            .unwrap_or(true)
    });
    Ok(json!({
        "status":"ok","site_root":root.to_string_lossy(),"storage_mode":"native_sqlite",
        "filters":{"status":status,"kind":kind,"target_role":role,"action":action},
        "count":rows.len(),"envelopes":rows.iter().take(limit).map(summary).collect::<Vec<_>>()
    }))
}

fn show(args: &Map<String, Value>, root: &Path) -> Result<Value, Value> {
    let id = required(args, "envelope_id")?;
    let Some(row) = read_row(root, &id)? else {
        return Ok(json!({"status":"not_found","envelope_id":id}));
    };
    let mut envelope = summary(&row).as_object().cloned().unwrap_or_default();
    envelope.insert(
        "payload".into(),
        row.get("payload_json")
            .and_then(Value::as_str)
            .and_then(|v| serde_json::from_str(v).ok())
            .unwrap_or(Value::Null),
    );
    Ok(json!({"status":"ok","site_root":root.to_string_lossy(),"envelope":envelope}))
}

fn next(args: &Map<String, Value>, root: &Path) -> Result<Value, Value> {
    let role = optional_string(args, "target_role");
    let rows = rows(root)?
        .into_iter()
        .filter(|r| r.get("status").and_then(Value::as_str) == Some("received"))
        .filter(|r| {
            role.as_deref()
                .map(|v| r.get("target_role").and_then(Value::as_str) == Some(v))
                .unwrap_or(true)
        })
        .collect::<Vec<_>>();
    Ok(json!({
        "status":if rows.is_empty(){"empty"}else{"ok"},
        "site_root":root.to_string_lossy(),"envelope":rows.first().map(summary)
    }))
}

fn capa(args: &Map<String, Value>, root: &Path) -> Result<Value, Value> {
    let limit = bounded(args.get("limit"), 20, 100);
    let rows = rows(root)?
        .into_iter()
        .filter(|r| r.get("status").and_then(Value::as_str) == Some("received"))
        .filter(|r| {
            r.get("action").and_then(Value::as_str) == Some("review_capa_request")
                || r.get("kind").and_then(Value::as_str) == Some("incident")
        })
        .collect::<Vec<_>>();
    Ok(json!({
        "status":"ok","site_root":root.to_string_lossy(),"count":rows.len(),
        "envelopes":rows.iter().take(limit).map(summary).collect::<Vec<_>>()
    }))
}

fn audit(args: &Map<String, Value>, root: &Path) -> Result<Value, Value> {
    let limit = bounded(args.get("limit"), 50, 200);
    let id = optional_string(args, "envelope_id");
    let mut entries = read_log(root)?;
    if let Some(id) = id {
        entries.retain(|e| e.get("envelope_id").and_then(Value::as_str) == Some(id.as_str()));
    }
    let total = entries.len();
    let entries = entries
        .into_iter()
        .rev()
        .take(limit)
        .map(|e| {
            json!({
                "event_id":e.get("event_id"),"event_sequence":e.get("event_sequence"),
                "event_kind":e.get("event_kind"),"envelope_id":e.get("envelope_id"),
                "principal":e.get("principal"),"timestamp":e.get("timestamp"),
                "payload":e.get("event_payload")
            })
        })
        .collect::<Vec<_>>();
    Ok(
        json!({"status":"ok","site_root":root.to_string_lossy(),"total_entries":total,"count":entries.len(),"entries":entries}),
    )
}

fn summary(row: &Map<String, Value>) -> Value {
    json!({
        "envelope_id":row.get("envelope_id"),"status":row.get("status"),"kind":row.get("kind"),
        "title":row.get("title"),"summary":row.get("summary"),"received_at":row.get("received_at"),
        "target_role":row.get("target_role"),"severity":row.get("severity"),
        "severity_reason":row.get("severity_reason"),"action":row.get("action"),
        "file_path":row.get("file_path")
    })
}

