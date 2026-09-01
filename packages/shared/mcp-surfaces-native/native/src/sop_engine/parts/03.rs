fn reopen_terminal_outbox_for_retry(
    db: &Connection,
    run_id: &str,
) -> Result<Option<String>, Value> {
    let existing = db
        .query_row(
            "SELECT event_id, compacted_at FROM sop_outbox WHERE run_id = ?",
            params![run_id],
            row_json,
        )
        .optional()
        .map_err(|error| diagnostic("sop_outbox_query_failed", &error.to_string(), json!({})))?;
    let Some(existing) = existing else {
        return Ok(None);
    };
    let event_id = required_string(existing.get("event_id"), "sop_outbox_event_id_invalid", 512)?;
    let consumed = db
        .query_row(
            "SELECT 1 FROM sop_outbox_receipts WHERE event_id = ? LIMIT 1",
            params![event_id],
            |_| Ok(true),
        )
        .optional()
        .map_err(|error| {
            diagnostic(
                "sop_outbox_receipt_query_failed",
                &error.to_string(),
                json!({}),
            )
        })?
        .unwrap_or(false);
    let compacted = existing
        .get("compacted_at")
        .is_some_and(|value| !value.is_null());
    if consumed || compacted {
        return Err(diagnostic(
            "sop_outbox_retry_requires_new_run",
            "sop_outbox_retry_requires_new_run",
            json!({
                "event_id":event_id,"run_id":run_id,"consumed":consumed,"compacted":compacted
            }),
        ));
    }
    db.execute(
        "DELETE FROM sop_outbox WHERE event_id = ? AND run_id = ?",
        params![event_id, run_id],
    )
    .map_err(|error| diagnostic("sop_outbox_delete_failed", &error.to_string(), json!({})))?;
    Ok(Some(event_id))
}

fn reset_retryable_dependent_steps(run: &mut Run, root_step_id: &str) -> Vec<String> {
    let mut reset = HashSet::from([root_step_id.to_string()]);
    let mut changed = true;
    while changed {
        changed = false;
        for step in &mut run.step_states {
            let step_id = step_string(step, "step_id");
            if reset.contains(&step_id)
                || step.get("status").and_then(Value::as_str) != Some("failed")
                || !step
                    .get("error_message")
                    .and_then(Value::as_str)
                    .is_some_and(|message| message.starts_with("failed_dependency:"))
            {
                continue;
            }
            let depends_on_reset = step
                .get("depends_on")
                .and_then(Value::as_array)
                .is_some_and(|dependencies| {
                    dependencies.iter().any(|dependency| {
                        dependency
                            .as_str()
                            .is_some_and(|dependency| reset.contains(dependency))
                    })
                });
            if !depends_on_reset {
                continue;
            }
            set_step(step, "status", json!("pending"));
            for key in [
                "started_at",
                "completed_at",
                "result_ref",
                "completion_key",
                "completion_fingerprint",
                "error_message",
                "child_run_id",
                "action_id",
                "pinned_child_definition_fingerprint",
            ] {
                set_step(step, key, Value::Null);
            }
            set_step(step, "result", json!({}));
            reset.insert(step_id);
            changed = true;
        }
    }
    let mut dependent = reset
        .into_iter()
        .filter(|step_id| step_id != root_step_id)
        .collect::<Vec<_>>();
    dependent.sort();
    dependent
}

fn retry_failed_handoff_as_new_run(
    db: &Connection,
    handoff: &Value,
    run: &Run,
    principal: &str,
    reason: &str,
    outbox_diagnostic: &Value,
) -> Result<Value, Value> {
    let handoff_id = handoff
        .get("handoff_id")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let step_id = handoff
        .get("step_id")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let occurrence_key = deterministic_id("sop_retry_", handoff_id);
    let args = json!({
        "sop_id":run.sop_id,"sop_version":run.sop_version,
        "occurrence_key":occurrence_key,"input":run.input,"input_ref":run.input_ref,
        "trigger_source_kind":"manual",
        "trigger_source_ref":format!("sop_handoff_retry:{handoff_id}"),
        "triggered_by":"sop-handoff-retry"
    });
    let (admitted, admission) = admit_run(
        db,
        args.as_object().expect("retry admission object"),
        None,
        None,
    )?;
    reconcile_run_and_ancestors(db, &admitted.run_id)?;
    let retry_run = get_run(db, &admitted.run_id)?;
    let retry_handoff_id = db
        .query_row(
            "SELECT handoff_id FROM sop_handoffs WHERE run_id = ? AND step_id = ?",
            params![retry_run.run_id, step_id],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .map_err(|error| diagnostic("sop_handoff_query_failed", &error.to_string(), json!({})))?;
    let retry_handoff = retry_handoff_id
        .as_deref()
        .map(|id| get_handoff(db, id))
        .transpose()?;
    if admission == "created" {
        append_run_event(
            db,
            &run.run_id,
            Some(step_id),
            "handoff_retry_spawned",
            json!({
                "handoff_id":handoff_id,"principal":principal,"reason":reason,
                "retry_run_id":retry_run.run_id,
                "retry_handoff_id":retry_handoff.as_ref().and_then(|value|value.get("handoff_id")),
                "retry_occurrence_key":occurrence_key,
                "original_outbox_event_id":outbox_diagnostic.get("details").and_then(|value|value.get("event_id")),
                "original_outbox_preserved":true
            }),
        )?;
    }
    let mut response = run_result(&retry_run, Some(admission));
    let object = response.as_object_mut().expect("run response object");
    object.insert(
        "handoff".to_string(),
        retry_handoff
            .map(|value| public_handoff(value, false))
            .unwrap_or(Value::Null),
    );
    object.insert("retry_replayed".to_string(), json!(admission == "replayed"));
    object.insert("retry_mode".to_string(), json!("new_run"));
    object.insert("retry_of_run_id".to_string(), json!(run.run_id));
    object.insert("retry_of_handoff_id".to_string(), json!(handoff_id));
    object.insert("retry_reason".to_string(), json!(reason));
    object.insert("original_outbox_preserved".to_string(), json!(true));
    Ok(response)
}

