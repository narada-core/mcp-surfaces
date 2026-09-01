pub(crate) fn get_handoff(db: &Connection, handoff_id: &str) -> Result<Value, Value> {
    let row = db
        .query_row(
            "SELECT * FROM sop_handoffs WHERE handoff_id = ?",
            params![handoff_id],
            row_json,
        )
        .optional()
        .map_err(|error| diagnostic("sop_handoff_query_failed", &error.to_string(), json!({})))?
        .ok_or_else(|| {
            diagnostic(
                "sop_handoff_not_found",
                "sop_handoff_not_found",
                json!({"handoff_id":handoff_id}),
            )
        })?;
    hydrate_handoff(row)
}

pub(crate) fn hydrate_handoff(row: Value) -> Result<Value, Value> {
    let object = row
        .as_object()
        .ok_or_else(|| diagnostic("sop_handoff_corrupt", "sop_handoff_corrupt", json!({})))?;
    let run_id = required_string(object.get("run_id"), "sop_handoff_corrupt", 512)?;
    let step_id = required_string(object.get("step_id"), "sop_handoff_corrupt", 512)?;
    let handoff_id = required_string(object.get("handoff_id"), "sop_handoff_corrupt", 512)?;
    let occurrence_key = required_string(object.get("occurrence_key"), "sop_handoff_corrupt", 512)?;
    let identity = format!("{run_id}\0{step_id}");
    let expected_handoff_id = deterministic_id("soh_", &identity);
    let expected_occurrence_key = deterministic_id("sop_handoff_", &identity);
    if handoff_id != expected_handoff_id || occurrence_key != expected_occurrence_key {
        return Err(diagnostic(
            "sop_handoff_identity_mismatch",
            "sop_handoff_identity_mismatch",
            json!({"handoff_id":handoff_id,"expected_handoff_id":expected_handoff_id}),
        ));
    }
    let sop_id = required_string(object.get("sop_id"), "sop_handoff_corrupt", 512)?;
    let sop_version = positive_integer_member(object.get("sop_version"), "sop_handoff_corrupt")?;
    let executor = normalize_handoff_executor(
        object
            .get("executor")
            .and_then(Value::as_str)
            .unwrap_or_default(),
    )?;
    let title = required_string(object.get("title"), "sop_handoff_corrupt", 512)?;
    let instructions =
        required_string(object.get("instructions"), "sop_handoff_corrupt", 16 * 1024)?;
    let input = object.get("input_json").cloned().unwrap_or(Value::Null);
    let input_ref = object.get("input_ref_json").cloned().unwrap_or(Value::Null);
    let result_schema = object
        .get("result_schema_json")
        .cloned()
        .unwrap_or(Value::Null);
    let request_fingerprint = required_string(
        object.get("request_fingerprint"),
        "sop_handoff_corrupt",
        512,
    )?;
    let actual_request_fingerprint = fingerprint(&json!({
        "run_id":run_id,"step_id":step_id,"sop_id":sop_id,"sop_version":sop_version,
        "executor":executor,"title":title,"instructions":instructions,"input":input,
        "input_ref":input_ref,"result_schema":result_schema
    }));
    if request_fingerprint != actual_request_fingerprint {
        return Err(diagnostic(
            "sop_handoff_request_fingerprint_mismatch",
            "sop_handoff_request_fingerprint_mismatch",
            json!({"handoff_id":handoff_id}),
        ));
    }
    let status = normalize_handoff_status(
        object
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or_default(),
    )?;
    let lease_owner = optional_string(object.get("lease_owner"));
    let lease_token = optional_string(object.get("lease_token"));
    let lease_expires_at = optional_string(object.get("lease_expires_at"));
    if status == "leased" {
        if lease_owner.is_none() || lease_token.is_none() || lease_expires_at.is_none() {
            return Err(diagnostic(
                "sop_handoff_lease_corrupt",
                "sop_handoff_lease_corrupt",
                json!({"handoff_id":handoff_id}),
            ));
        }
    } else if lease_owner.is_some() || lease_token.is_some() || lease_expires_at.is_some() {
        return Err(diagnostic(
            "sop_handoff_lease_corrupt",
            "sop_handoff_lease_corrupt",
            json!({"handoff_id":handoff_id,"status":status}),
        ));
    }
    let attempt_count = nonnegative_integer_member(
        object.get("attempt_count"),
        "sop_handoff_attempt_count_invalid",
    )?;
    let completion_key = optional_string(object.get("completion_key"));
    let completion_fingerprint = optional_string(object.get("completion_fingerprint"));
    let principal = optional_string(object.get("principal"));
    let result = object
        .get("result_json")
        .cloned()
        .unwrap_or_else(|| json!({}));
    if !result.is_object() {
        return Err(diagnostic(
            "sop_handoff_result_corrupt",
            "sop_handoff_result_corrupt",
            json!({"handoff_id":handoff_id}),
        ));
    }
    let result_ref = object
        .get("result_ref_json")
        .cloned()
        .unwrap_or(Value::Null);
    let error_message = optional_string(object.get("error_message"));
    if let Some(recorded) = completion_fingerprint.as_ref() {
        if completion_key.is_none()
            || principal.is_none()
            || !matches!(status.as_str(), "completed" | "failed")
        {
            return Err(diagnostic(
                "sop_handoff_completion_identity_invalid",
                "sop_handoff_completion_identity_invalid",
                json!({"handoff_id":handoff_id,"status":status}),
            ));
        }
        let actual = fingerprint(&json!({
            "completion_key":completion_key,"outcome":status,"principal":principal,
            "result":result,"result_ref":result_ref,"error_message":error_message
        }));
        if recorded != &actual {
            return Err(diagnostic(
                "sop_handoff_completion_fingerprint_mismatch",
                "sop_handoff_completion_fingerprint_mismatch",
                json!({"handoff_id":handoff_id}),
            ));
        }
    } else if completion_key.is_some()
        || principal.is_some()
        || matches!(status.as_str(), "completed" | "failed")
    {
        return Err(diagnostic(
            "sop_handoff_completion_identity_invalid",
            "sop_handoff_completion_identity_invalid",
            json!({"handoff_id":handoff_id,"status":status}),
        ));
    }
    Ok(json!({
        "schema":"narada.sop.handoff.v1","handoff_id":handoff_id,"run_id":run_id,
        "step_id":step_id,"occurrence_key":occurrence_key,"sop_id":sop_id,
        "sop_version":sop_version,"executor":executor,"title":title,
        "instructions":instructions,"input":input,"input_ref":input_ref,
        "result_schema":result_schema,"request_fingerprint":request_fingerprint,
        "status":status,"lease_owner":lease_owner,"lease_token":lease_token,
        "lease_expires_at":lease_expires_at,"attempt_count":attempt_count,
        "last_error":optional_string(object.get("last_error")),"completion_key":completion_key,
        "completion_fingerprint":completion_fingerprint,"principal":principal,"result":result,
        "result_ref":result_ref,"error_message":error_message,
        "created_at":required_string(object.get("created_at"),"sop_handoff_corrupt",512)?,
        "updated_at":required_string(object.get("updated_at"),"sop_handoff_corrupt",512)?,
        "completed_at":optional_string(object.get("completed_at"))
    }))
}

pub(crate) fn public_handoff(mut handoff: Value, include_lease_token: bool) -> Value {
    if !include_lease_token {
        if let Some(object) = handoff.as_object_mut() {
            object.remove("lease_token");
        }
    }
    handoff
}

fn outbox_consumer_register(args: &Map<String, Value>, root: &Path) -> Result<Value, Value> {
    transactional(root, |db| {
        let topic = normalize_outbox_topic(
            optional_string(args.get("topic"))
                .as_deref()
                .unwrap_or(SOP_TERMINAL_TOPIC),
        )?;
        let consumer_id = required_string(
            args.get("consumer_id"),
            "sop_outbox_consumer_id_required",
            512,
        )?;
        let now = OffsetDateTime::now_utc();
        let start_at = match optional_string(args.get("start_at")) {
            Some(value) => normalize_timestamp(&value, "sop_outbox_start_at_invalid")?,
            None => format_iso(now),
        };
        let existing = db
            .query_row(
                "SELECT * FROM sop_outbox_consumer_requirements WHERE topic = ? AND consumer_id = ?",
                params![topic, consumer_id],
                row_json,
            )
            .optional()
            .map_err(|error| {
                diagnostic(
                    "sop_outbox_consumer_query_failed",
                    &error.to_string(),
                    json!({}),
                )
            })?;
        if let Some(existing) = existing {
            let recorded_start_at = existing
                .get("start_at")
                .and_then(Value::as_str)
                .unwrap_or_default();
            if recorded_start_at != start_at {
                return Err(diagnostic(
                    "sop_outbox_consumer_registration_conflict",
                    "sop_outbox_consumer_registration_conflict",
                    json!({
                        "topic":topic,"consumer_id":consumer_id,
                        "recorded_start_at":recorded_start_at,"supplied_start_at":start_at
                    }),
                ));
            }
            return Ok(json!({
                "schema":"narada.sop.outbox_consumer.v1",
                "topic":existing.get("topic").cloned().unwrap_or(Value::Null),
                "consumer_id":existing.get("consumer_id").cloned().unwrap_or(Value::Null),
                "start_at":existing.get("start_at").cloned().unwrap_or(Value::Null),
                "registered_at":existing.get("registered_at").cloned().unwrap_or(Value::Null),
                "registration_replayed":true
            }));
        }
        let compacted = db
            .query_row(
                "SELECT event_id, created_at FROM sop_outbox WHERE topic = ? AND created_at >= ? AND compacted_at IS NOT NULL ORDER BY created_at LIMIT 1",
                params![topic, start_at],
                row_json,
            )
            .optional()
            .map_err(|error| diagnostic("sop_outbox_query_failed", &error.to_string(), json!({})))?;
        if let Some(compacted) = compacted {
            return Err(diagnostic(
                "sop_outbox_registration_history_compacted",
                "sop_outbox_registration_history_compacted",
                json!({
                    "topic":topic,"consumer_id":consumer_id,"start_at":start_at,
                    "first_compacted_event_id":compacted.get("event_id"),
                    "first_compacted_event_created_at":compacted.get("created_at")
                }),
            ));
        }
        let registered_at = format_iso(now);
        db.execute(
            "INSERT INTO sop_outbox_consumer_requirements(topic, consumer_id, start_at, registered_at) VALUES (?, ?, ?, ?)",
            params![topic, consumer_id, start_at, registered_at],
        )
        .map_err(|error| diagnostic("sop_outbox_consumer_insert_failed", &error.to_string(), json!({})))?;
        Ok(json!({
            "schema":"narada.sop.outbox_consumer.v1","topic":topic,
            "consumer_id":consumer_id,"start_at":start_at,"registered_at":registered_at,
            "registration_replayed":false
        }))
    })
}

