fn action_resolve(args: &Map<String, Value>, root: &Path) -> Result<Value, Value> {
    let action_id = required_string(args.get("action_id"), "sop_requires_action_id", 512)?;
    let completion_key = required_string(
        args.get("completion_key"),
        "sop_requires_completion_key",
        512,
    )?;
    let outcome = required_string(args.get("outcome"), "sop_requires_outcome", 64)?;
    if !matches!(outcome.as_str(), "completed" | "failed") {
        return Err(diagnostic(
            "sop_outcome_invalid",
            &format!("sop_outcome_invalid:{outcome}"),
            json!({"allowed":["completed","failed"]}),
        ));
    }
    let operation_ref = required_string(
        args.get("operation_ref"),
        "sop_requires_operation_ref",
        2048,
    )?;
    let result = args.get("result").cloned().unwrap_or_else(|| json!({}));
    assert_bound(&result, "sop_result", MAX_INLINE_VALUE_BYTES)?;
    if !result.is_object() {
        return Err(diagnostic(
            "sop_result_must_be_object",
            "sop_result_must_be_object",
            json!({}),
        ));
    }
    let result_ref = normalize_value_ref(args.get("result_ref"), "sop_result_ref")?;
    let error_message = optional_bounded_string(
        args.get("error_message"),
        "sop_error_message_too_long",
        4096,
    )?;
    if outcome == "failed" && error_message.is_none() {
        return Err(diagnostic(
            "sop_failed_outcome_requires_error_message",
            "sop_failed_outcome_requires_error_message",
            json!({}),
        ));
    }
    let completion_fingerprint = fingerprint(&json!({
        "completion_key":completion_key,"outcome":outcome,"operation_ref":operation_ref,
        "result":result,"result_ref":result_ref,"error_message":error_message
    }));
    let receipt = transactional(root, |db| {
        let existing = get_action(db, &action_id)?;
        let run_id = existing
            .get("run_id")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        if let Some(recorded_fingerprint) = existing
            .get("completion_fingerprint")
            .and_then(Value::as_str)
        {
            if existing.get("completion_key").and_then(Value::as_str)
                == Some(completion_key.as_str())
                && recorded_fingerprint == completion_fingerprint
            {
                let run_status = db
                    .query_row(
                        "SELECT status FROM sop_runs WHERE run_id = ?",
                        params![run_id],
                        |row| row.get::<_, String>(0),
                    )
                    .optional()
                    .map_err(|error| {
                        diagnostic("sop_run_query_failed", &error.to_string(), json!({}))
                    })?
                    .unwrap_or_default();
                return Ok(json!({
                    "run_id":run_id,"completion_replayed":true,
                    "late_cancellation_acknowledgement":run_status=="cancelled"
                }));
            }
            return Err(diagnostic(
                "sop_action_completion_conflict",
                &format!("sop_action_completion_conflict:{action_id}"),
                json!({
                    "recorded_completion_key":existing.get("completion_key"),
                    "supplied_completion_key":completion_key
                }),
            ));
        }
        let run_status = db
            .query_row(
                "SELECT status FROM sop_runs WHERE run_id = ?",
                params![run_id],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .map_err(|error| diagnostic("sop_run_query_failed", &error.to_string(), json!({})))?
            .ok_or_else(|| {
                diagnostic(
                    "sop_run_not_found",
                    &format!("sop_run_not_found:{run_id}"),
                    json!({}),
                )
            })?;
        let late_cancellation = existing.get("status").and_then(Value::as_str) == Some("cancelled")
            && run_status == "cancelled";
        if existing.get("status").and_then(Value::as_str) != Some("pending") && !late_cancellation {
            return Err(diagnostic(
                "sop_action_not_pending",
                &format!("sop_action_not_pending:{action_id}"),
                json!({"status":existing.get("status")}),
            ));
        }
        let now = now_iso();
        db.execute(
            "UPDATE sop_actions SET status = ?, completion_key = ?, completion_fingerprint = ?, operation_ref = ?, result_json = ?, result_ref_json = ?, error_message = ?, updated_at = ?, completed_at = ? WHERE action_id = ?",
            params![outcome,completion_key,completion_fingerprint,operation_ref,
                canonical_json(&result),nullable_json(&result_ref),
                if outcome=="failed"{error_message.as_deref()}else{None},
                now,now,action_id],
        )
        .map_err(|error| diagnostic("sop_action_update_failed", &error.to_string(), json!({})))?;
        let step_id = existing
            .get("step_id")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let event_kind = match (late_cancellation, outcome.as_str()) {
            (true, "completed") => "action_completed_after_cancellation",
            (true, _) => "action_failed_after_cancellation",
            (false, "completed") => "action_completed",
            (false, _) => "action_failed",
        };
        append_run_event(
            db,
            &run_id,
            Some(step_id),
            event_kind,
            json!({
                "action_id":action_id,"completion_key":completion_key,
                "operation_ref":operation_ref,"result_ref":result_ref,
                "error_message":error_message
            }),
        )?;
        get_action(db, &action_id)?;
        Ok(json!({
            "run_id":run_id,"completion_replayed":false,
            "late_cancellation_acknowledgement":late_cancellation
        }))
    })?;
    let run_id = receipt
        .get("run_id")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let late_cancellation = receipt
        .get("late_cancellation_acknowledgement")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let reconciliation_error = if late_cancellation {
        None
    } else {
        transactional(root, |db| {
            reconcile_run_and_ancestors(db, &run_id)?;
            Ok(Value::Null)
        })
        .err()
    };
    let db = open_db(root)?;
    let mut response = get_action(&db, &action_id)?;
    let object = response.as_object_mut().expect("action response object");
    object.insert(
        "completion_replayed".to_string(),
        receipt
            .get("completion_replayed")
            .cloned()
            .unwrap_or(json!(false)),
    );
    object.insert(
        "late_cancellation_acknowledgement".to_string(),
        json!(late_cancellation),
    );
    object.insert(
        "reconciliation".to_string(),
        match reconciliation_error {
            Some(error) => json!({"status":"failed","diagnostic":error}),
            None => json!({"status":"completed"}),
        },
    );
    object.insert("run".to_string(), action_resolution_run_view(&db, &run_id));
    Ok(response)
}

fn run_cancel(args: &Map<String, Value>, root: &Path) -> Result<Value, Value> {
    transactional(root, |db| {
        let run_id = required_string(args.get("run_id"), "sop_requires_run_id", 512)?;
        let run = get_run(db, &run_id)?;
        if run.status == "cancelled" {
            let mut response = run_result(&run, None);
            response
                .as_object_mut()
                .expect("run response object")
                .insert("cancellation_replayed".to_string(), json!(true));
            return Ok(response);
        }
        if matches!(run.status.as_str(), "completed" | "failed") {
            return Err(diagnostic(
                "sop_run_already_terminal",
                &format!("sop_run_already_terminal:{run_id}"),
                json!({"status":run.status}),
            ));
        }
        let reason =
            optional_bounded_string(args.get("reason"), "sop_cancellation_reason_too_long", 4096)?
                .unwrap_or_else(|| "cancelled_by_caller".to_string());
        cancel_run_internal(db, &run_id, &reason, &mut HashSet::new())?;
        reconcile_run_and_ancestors(db, &run_id)?;
        let mut response = run_result(&get_run(db, &run_id)?, None);
        response
            .as_object_mut()
            .expect("run response object")
            .insert("cancellation_replayed".to_string(), json!(false));
        Ok(response)
    })
}

fn cancel_run_internal(
    db: &Connection,
    run_id: &str,
    reason: &str,
    seen: &mut HashSet<String>,
) -> Result<(), Value> {
    if !seen.insert(run_id.to_string()) {
        return Ok(());
    }
    let mut run = get_run(db, run_id)?;
    if is_run_terminal(&run.status) {
        return Ok(());
    }
    let child_ids = run
        .step_states
        .iter()
        .filter_map(|step| {
            step.get("child_run_id")
                .and_then(Value::as_str)
                .map(ToString::to_string)
        })
        .collect::<Vec<_>>();
    for child_id in child_ids {
        cancel_run_internal(db, &child_id, &format!("parent_cancelled:{run_id}"), seen)?;
    }
    for step in &mut run.step_states {
        if matches!(
            step.get("status").and_then(Value::as_str),
            Some("pending" | "running")
        ) {
            skip_step(step, &format!("run_cancelled:{reason}"));
        }
    }
    let now = now_iso();
    let cancellation_error = format!("run_cancelled:{reason}");
    db.execute(
        "UPDATE sop_actions SET status = 'cancelled', error_message = ?, updated_at = ?, completed_at = ? WHERE run_id = ? AND status = 'pending'",
        params![cancellation_error,now,now,run_id],
    )
    .map_err(|error| diagnostic("sop_action_update_failed", &error.to_string(), json!({})))?;
    db.execute(
        "UPDATE sop_handoffs SET status = 'cancelled', lease_owner = NULL, lease_token = NULL, lease_expires_at = NULL, last_error = ?, updated_at = ?, completed_at = ? WHERE run_id = ? AND status IN ('pending','leased')",
        params![cancellation_error,now,now,run_id],
    )
    .map_err(|error| diagnostic("sop_handoff_update_failed", &error.to_string(), json!({})))?;
    run.status = "cancelled".to_string();
    run.completed_at = Some(now);
    run.output = json!({});
    run.output_ref = Value::Null;
    persist_run(db, &mut run)?;
    append_run_event(db, run_id, None, "run_cancelled", json!({"reason":reason}))?;
    put_terminal_outbox(db, &run)?;
    Ok(())
}

