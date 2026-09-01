fn handoff_claim(args: &Map<String, Value>, root: &Path) -> Result<Value, Value> {
    transactional(root, |db| {
        let consumer_id = required_string(
            args.get("consumer_id"),
            "sop_handoff_consumer_id_required",
            512,
        )?;
        let requested_handoff_id = optional_string(args.get("handoff_id"));
        let executor = match optional_string(args.get("executor")) {
            Some(value) => Some(normalize_handoff_executor(&value)?),
            None => None,
        };
        let lease_ms = bounded_integer_arg(
            args.get("lease_ms"),
            60_000,
            MIN_LEASE_MS,
            MAX_LEASE_MS,
            "sop_handoff_lease_ms_invalid",
        )?;
        let now = OffsetDateTime::now_utc();
        let now_text = format_iso(now);
        let mut conditions = vec![
            "(handoff.status = 'pending' OR (handoff.status = 'leased' AND handoff.lease_expires_at <= ?))",
            "run.status NOT IN ('completed', 'failed', 'cancelled')",
        ];
        let mut values = vec![now_text.clone()];
        if let Some(handoff_id) = requested_handoff_id.as_ref() {
            conditions.push("handoff.handoff_id = ?");
            values.push(handoff_id.clone());
        }
        if let Some(executor) = executor.as_ref() {
            conditions.push("handoff.executor = ?");
            values.push(executor.clone());
        }
        let sql = format!(
            "SELECT handoff.* FROM sop_handoffs handoff JOIN sop_runs run ON run.run_id = handoff.run_id WHERE {} ORDER BY handoff.created_at, handoff.handoff_id LIMIT 1",
            conditions.join(" AND ")
        );
        let candidate = db
            .query_row(&sql, rusqlite::params_from_iter(values.iter()), row_json)
            .optional()
            .map_err(|error| {
                diagnostic("sop_handoff_query_failed", &error.to_string(), json!({}))
            })?;
        let Some(candidate) = candidate else {
            return Ok(json!({
                "schema":"narada.sop.handoff_claim.v1",
                "status":"empty",
                "handoff":null
            }));
        };
        let handoff_id = required_string(candidate.get("handoff_id"), "sop_handoff_corrupt", 512)?;
        let lease_token = Uuid::new_v4().to_string();
        let lease_expires_at = format_iso(now + Duration::milliseconds(lease_ms));
        let changes = db
            .execute(
                "UPDATE sop_handoffs SET status = 'leased', lease_owner = ?, lease_token = ?, lease_expires_at = ?, attempt_count = attempt_count + 1, last_error = CASE WHEN status = 'leased' THEN 'lease_expired' ELSE last_error END, updated_at = ? WHERE handoff_id = ? AND (status = 'pending' OR (status = 'leased' AND lease_expires_at <= ?))",
                params![consumer_id, lease_token, lease_expires_at, now_text, handoff_id, now_text],
            )
            .map_err(|error| diagnostic("sop_handoff_claim_failed", &error.to_string(), json!({})))?;
        if changes != 1 {
            return Err(diagnostic(
                "sop_handoff_claim_race",
                "sop_handoff_claim_race",
                json!({"handoff_id":handoff_id}),
            ));
        }
        let handoff = public_handoff(get_handoff(db, &handoff_id)?, true);
        Ok(json!({
            "schema":"narada.sop.handoff_claim.v1",
            "status":"claimed",
            "handoff":handoff,
            "lease_ms":lease_ms,
            "lease_remaining_ms":lease_ms,
            "next":{"tool":"sop_run_advance","required_from_claim":["handoff.handoff_id","handoff.run_id","handoff.step_id","handoff.lease_token"]}
        }))
    })
}

fn handoff_claim_and_advance(args: &Map<String, Value>, root: &Path) -> Result<Value, Value> {
    let claim = handoff_claim(args, root)?;
    if claim.get("status").and_then(Value::as_str) == Some("empty") {
        return Ok(
            json!({"schema":"narada.sop.handoff_claim_and_advance.v1","status":"empty","handoff":null,"advanced":false}),
        );
    }
    let handoff = claim.get("handoff").cloned().ok_or_else(|| {
        diagnostic(
            "sop_handoff_claim_response_corrupt",
            "sop_handoff_claim_response_corrupt",
            json!({}),
        )
    })?;
    let mut advance = Map::new();
    for (target, source) in [
        ("handoff_id", "handoff_id"),
        ("run_id", "run_id"),
        ("step_id", "step_id"),
        ("lease_token", "lease_token"),
    ] {
        advance.insert(
            target.to_string(),
            handoff.get(source).cloned().unwrap_or(Value::Null),
        );
    }
    for key in [
        "consumer_id",
        "completion_key",
        "outcome",
        "result",
        "result_ref",
        "error_message",
        "principal",
    ] {
        if let Some(value) = args.get(key) {
            advance.insert(key.to_string(), value.clone());
        }
    }
    match crate::sop_engine::call_tool("sop_run_advance", &advance, root) {
        Ok(result) => Ok(
            json!({"schema":"narada.sop.handoff_claim_and_advance.v1","status":"advanced","advanced":true,"claim":claim,"result":result}),
        ),
        Err(mut failure) => {
            let cleanup = handoff_release(
                &Map::from_iter([
                    ("handoff_id".into(), handoff["handoff_id"].clone()),
                    (
                        "consumer_id".into(),
                        args.get("consumer_id").cloned().unwrap_or(Value::Null),
                    ),
                    ("lease_token".into(), handoff["lease_token"].clone()),
                    (
                        "error_message".into(),
                        json!("compound_advance_failed_released"),
                    ),
                ]),
                root,
            );
            if let Some(object) = failure.as_object_mut() {
                object.insert(
                    "compound_cleanup".into(),
                    match cleanup {
                        Ok(_) => json!({"status":"released","handoff_id":handoff["handoff_id"]}),
                        Err(error) => json!({"status":"release_failed","error":error}),
                    },
                );
            }
            Err(failure)
        }
    }
}

fn handoff_renew(args: &Map<String, Value>, root: &Path) -> Result<Value, Value> {
    transactional(root, |db| {
        let now = OffsetDateTime::now_utc();
        let handoff = require_lease(db, args, now, false)?;
        let lease_ms = bounded_integer_arg(
            args.get("lease_ms"),
            60_000,
            MIN_LEASE_MS,
            MAX_LEASE_MS,
            "sop_handoff_lease_ms_invalid",
        )?;
        let handoff_id = handoff
            .get("handoff_id")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let lease_expires_at = format_iso(now + Duration::milliseconds(lease_ms));
        db.execute(
            "UPDATE sop_handoffs SET lease_expires_at = ?, updated_at = ? WHERE handoff_id = ?",
            params![lease_expires_at, format_iso(now), handoff_id],
        )
        .map_err(|error| diagnostic("sop_handoff_renew_failed", &error.to_string(), json!({})))?;
        let mut handoff = public_handoff(get_handoff(db, handoff_id)?, true);
        handoff["lease_ms"] = json!(lease_ms);
        handoff["lease_remaining_ms"] = json!(lease_ms);
        Ok(handoff)
    })
}

fn handoff_release(args: &Map<String, Value>, root: &Path) -> Result<Value, Value> {
    transactional(root, |db| {
        let now = OffsetDateTime::now_utc();
        let handoff = require_lease(db, args, now, true)?;
        let handoff_id = handoff
            .get("handoff_id")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let error_message = optional_bounded_string(
            args.get("error_message"),
            "sop_handoff_error_message_too_long",
            4096,
        )?;
        db.execute(
            "UPDATE sop_handoffs SET status = 'pending', lease_owner = NULL, lease_token = NULL, lease_expires_at = NULL, last_error = ?, updated_at = ? WHERE handoff_id = ?",
            params![error_message, format_iso(now), handoff_id],
        )
        .map_err(|error| diagnostic("sop_handoff_release_failed", &error.to_string(), json!({})))?;
        Ok(public_handoff(get_handoff(db, handoff_id)?, false))
    })
}

fn require_lease(
    db: &Connection,
    args: &Map<String, Value>,
    now: OffsetDateTime,
    allow_expired: bool,
) -> Result<Value, Value> {
    let handoff_id = required_string(args.get("handoff_id"), "sop_handoff_id_required", 512)?;
    let consumer_id = required_string(
        args.get("consumer_id"),
        "sop_handoff_consumer_id_required",
        512,
    )?;
    let lease_token = required_string(
        args.get("lease_token"),
        "sop_handoff_lease_token_required",
        512,
    )?;
    let handoff = get_handoff(db, &handoff_id)?;
    let status = handoff
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if status != "leased" {
        return Err(diagnostic(
            "sop_handoff_not_leased",
            "sop_handoff_not_leased",
            json!({"handoff_id":handoff_id,"status":status}),
        ));
    }
    if handoff.get("lease_owner").and_then(Value::as_str) != Some(consumer_id.as_str())
        || handoff.get("lease_token").and_then(Value::as_str) != Some(lease_token.as_str())
    {
        return Err(diagnostic(
            "sop_handoff_lease_mismatch",
            "sop_handoff_lease_mismatch",
            json!({"handoff_id":handoff_id,"lease_owner":handoff.get("lease_owner")}),
        ));
    }
    if !allow_expired {
        let lease_expires_at = handoff
            .get("lease_expires_at")
            .and_then(Value::as_str)
            .ok_or_else(|| {
                diagnostic(
                    "sop_handoff_lease_corrupt",
                    "sop_handoff_lease_corrupt",
                    json!({"handoff_id":handoff_id}),
                )
            })?;
        let expires = parse_iso(lease_expires_at).ok_or_else(|| {
            diagnostic(
                "sop_handoff_lease_corrupt",
                "sop_handoff_lease_corrupt",
                json!({"handoff_id":handoff_id}),
            )
        })?;
        if expires <= now {
            return Err(diagnostic(
                "sop_handoff_lease_expired",
                "sop_handoff_lease_expired",
                json!({"handoff_id":handoff_id,"lease_expires_at":lease_expires_at,"recovery":{"tool":"sop_handoff_claim","arguments":{"handoff_id":handoff_id,"consumer_id":consumer_id},"guidance":"Reclaim the expired handoff with a new lease token; the stale token cannot commit."}}),
            ));
        }
    }
    Ok(handoff)
}

