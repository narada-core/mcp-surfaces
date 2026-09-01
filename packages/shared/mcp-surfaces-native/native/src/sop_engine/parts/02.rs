#[allow(clippy::too_many_arguments)]
fn complete_handoff(
    db: &Connection,
    handoff_id: &str,
    run_id: &str,
    step_id: &str,
    consumer_id: &str,
    lease_token: &str,
    completion_key: &str,
    outcome: &str,
    principal: &str,
    result: &Value,
    result_ref: &Value,
    error_message: Option<&str>,
) -> Result<(Value, bool), Value> {
    let handoff = get_handoff(db, handoff_id)?;
    if handoff.get("run_id").and_then(Value::as_str) != Some(run_id)
        || handoff.get("step_id").and_then(Value::as_str) != Some(step_id)
    {
        return Err(diagnostic(
            "sop_handoff_run_binding_mismatch",
            "sop_handoff_run_binding_mismatch",
            json!({"handoff_id":handoff_id,"run_id":run_id,"step_id":step_id}),
        ));
    }
    let completion_fingerprint = fingerprint(&json!({
        "completion_key":completion_key,"outcome":outcome,"principal":principal,
        "result":result,"result_ref":result_ref,"error_message":error_message
    }));
    if let Some(recorded_fingerprint) = handoff
        .get("completion_fingerprint")
        .and_then(Value::as_str)
    {
        if handoff.get("completion_key").and_then(Value::as_str) == Some(completion_key)
            && recorded_fingerprint == completion_fingerprint
        {
            return Ok((handoff, true));
        }
        return Err(diagnostic(
            "sop_handoff_completion_conflict",
            "sop_handoff_completion_conflict",
            json!({
                "handoff_id":handoff_id,
                "recorded_completion_key":handoff.get("completion_key"),
                "supplied_completion_key":completion_key
            }),
        ));
    }
    if handoff.get("status").and_then(Value::as_str) != Some("leased") {
        return Err(diagnostic(
            "sop_handoff_not_leased",
            "sop_handoff_not_leased",
            json!({"handoff_id":handoff_id,"status":handoff.get("status")}),
        ));
    }
    if handoff.get("lease_owner").and_then(Value::as_str) != Some(consumer_id)
        || handoff.get("lease_token").and_then(Value::as_str) != Some(lease_token)
    {
        return Err(diagnostic(
            "sop_handoff_lease_mismatch",
            "sop_handoff_lease_mismatch",
            json!({"handoff_id":handoff_id,"lease_owner":handoff.get("lease_owner")}),
        ));
    }
    let lease_expires_at = handoff
        .get("lease_expires_at")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let expired = match (parse_iso(lease_expires_at), parse_iso(&now_iso())) {
        (Some(expires), Some(now)) => expires <= now,
        _ => true,
    };
    if expired {
        return Err(diagnostic(
            "sop_handoff_lease_expired",
            "sop_handoff_lease_expired",
            json!({"handoff_id":handoff_id,"lease_expires_at":lease_expires_at}),
        ));
    }
    let completed_at = now_iso();
    db.execute(
        "UPDATE sop_handoffs SET status = ?, lease_owner = NULL, lease_token = NULL, lease_expires_at = NULL, completion_key = ?, completion_fingerprint = ?, principal = ?, result_json = ?, result_ref_json = ?, error_message = ?, updated_at = ?, completed_at = ? WHERE handoff_id = ?",
        params![outcome,completion_key,completion_fingerprint,principal,
            canonical_json(result),nullable_json(result_ref),error_message,
            completed_at,completed_at,handoff_id],
    )
    .map_err(|error| diagnostic("sop_handoff_update_failed", &error.to_string(), json!({})))?;
    Ok((get_handoff(db, handoff_id)?, false))
}

fn handoff_retry(args: &Map<String, Value>, root: &Path) -> Result<Value, Value> {
    transactional(root, |db| {
        let handoff_id = required_string(args.get("handoff_id"), "sop_handoff_id_required", 512)?;
        let principal = required_string(
            args.get("principal"),
            "sop_handoff_retry_principal_required",
            512,
        )?;
        let reason = required_string(
            args.get("reason"),
            "sop_handoff_retry_reason_required",
            4096,
        )?;
        let handoff = get_handoff(db, &handoff_id)?;
        let handoff_status = handoff
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let run_id = handoff
            .get("run_id")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        if matches!(handoff_status, "pending" | "leased") {
            let mut response = run_result(&get_run(db, &run_id)?, None);
            let object = response.as_object_mut().expect("run response object");
            object.insert("handoff".to_string(), public_handoff(handoff, false));
            object.insert("retry_replayed".to_string(), json!(true));
            return Ok(response);
        }
        if handoff_status != "failed" {
            return Err(diagnostic(
                "sop_handoff_retry_requires_failed",
                &format!("sop_handoff_retry_requires_failed:{handoff_id}"),
                json!({"status":handoff_status}),
            ));
        }
        if handoff.get("executor").and_then(Value::as_str) != Some("agent") {
            return Err(diagnostic(
                "sop_handoff_retry_agent_only",
                &format!("sop_handoff_retry_agent_only:{handoff_id}"),
                json!({"executor":handoff.get("executor")}),
            ));
        }
        let mut run = get_run(db, &run_id)?;
        let step_id = handoff
            .get("step_id")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let step_index = run
            .step_states
            .iter()
            .position(|step| step.get("step_id").and_then(Value::as_str) == Some(step_id.as_str()));
        let valid_step = step_index.is_some_and(|index| {
            let step = &run.step_states[index];
            step.get("executor").and_then(Value::as_str) == Some("agent")
                && step.get("status").and_then(Value::as_str) == Some("failed")
                && step
                    .get("completion_fingerprint")
                    .is_some_and(|value| !value.is_null())
        });
        if !valid_step {
            return Err(diagnostic(
                "sop_handoff_retry_state_conflict",
                &format!("sop_handoff_retry_state_conflict:{handoff_id}"),
                json!({
                    "run_id":run_id,"step_id":step_id,"run_status":run.status,
                    "step_status":step_index.and_then(|index|run.step_states[index].get("status")).cloned()
                }),
            ));
        }
        let step_index = step_index.expect("validated retry step");
        if run.step_states[step_index].get("completion_fingerprint")
            != handoff.get("completion_fingerprint")
        {
            return Err(diagnostic(
                "sop_handoff_retry_completion_conflict",
                &format!("sop_handoff_retry_completion_conflict:{handoff_id}"),
                json!({"run_id":run_id,"step_id":step_id}),
            ));
        }
        let reopened_event_id = match reopen_terminal_outbox_for_retry(db, &run_id) {
            Ok(value) => value,
            Err(error)
                if error.get("code").and_then(Value::as_str)
                    == Some("sop_outbox_retry_requires_new_run") =>
            {
                return retry_failed_handoff_as_new_run(
                    db, &handoff, &run, &principal, &reason, &error,
                );
            }
            Err(error) => return Err(error),
        };
        let now = now_iso();
        let reset_step_ids = reset_retryable_dependent_steps(&mut run, &step_id);
        let retry_marker = format!("worker_retryable:reopened:{reason}")
            .chars()
            .take(4096)
            .collect::<String>();
        db.execute(
            "UPDATE sop_handoffs SET status = 'pending', lease_owner = NULL, lease_token = NULL, lease_expires_at = NULL, completion_key = NULL, completion_fingerprint = NULL, principal = NULL, result_json = '{}', result_ref_json = NULL, error_message = NULL, last_error = ?, updated_at = ?, completed_at = NULL WHERE handoff_id = ? AND status = 'failed'",
            params![retry_marker,now,handoff_id],
        )
        .map_err(|error| diagnostic("sop_handoff_update_failed", &error.to_string(), json!({})))?;
        let step = &mut run.step_states[step_index];
        set_step(step, "status", json!("running"));
        set_step(step, "started_at", json!(now));
        set_step(step, "completed_at", Value::Null);
        set_step(
            step,
            "result",
            json!({
                "handoff_id":handoff.get("handoff_id"),
                "handoff_occurrence_key":handoff.get("occurrence_key")
            }),
        );
        set_step(step, "result_ref", Value::Null);
        set_step(step, "completion_key", Value::Null);
        set_step(step, "completion_fingerprint", Value::Null);
        set_step(step, "error_message", Value::Null);
        run.status = "awaiting_confirmation".to_string();
        run.output = json!({});
        run.output_ref = Value::Null;
        run.completed_at = None;
        persist_run(db, &mut run)?;
        append_run_event(
            db,
            &run_id,
            Some(&step_id),
            "handoff_reopened",
            json!({
                "handoff_id":handoff_id,"principal":principal,"reason":reason,
                "retry_marker":retry_marker,"reset_step_ids":reset_step_ids,
                "reopened_outbox_event_id":reopened_event_id
            }),
        )?;
        reconcile_run_and_ancestors(db, &run_id)?;
        let mut response = run_result(&get_run(db, &run_id)?, None);
        let object = response.as_object_mut().expect("run response object");
        object.insert(
            "handoff".to_string(),
            public_handoff(get_handoff(db, &handoff_id)?, false),
        );
        object.insert("retry_replayed".to_string(), json!(false));
        Ok(response)
    })
}

