fn hydrate_action(row: Value) -> Result<Value, Value> {
    let object = row
        .as_object()
        .ok_or_else(|| diagnostic("sop_action_corrupt", "sop_action_corrupt", json!({})))?;
    let action_id = required_string(object.get("action_id"), "sop_action_corrupt", 512)?;
    let run_id = required_string(object.get("run_id"), "sop_action_corrupt", 512)?;
    let step_id = required_string(object.get("step_id"), "sop_action_corrupt", 512)?;
    let occurrence_key = required_string(object.get("occurrence_key"), "sop_action_corrupt", 512)?;
    let surface_id = required_string(object.get("surface_id"), "sop_action_corrupt", 256)?;
    let tool_name = required_string(object.get("tool_name"), "sop_action_corrupt", 256)?;
    let arguments = object
        .get("arguments_json")
        .cloned()
        .unwrap_or_else(|| json!({}));
    if !arguments.is_object() {
        return Err(diagnostic(
            "sop_action_corrupt",
            "sop_action_corrupt",
            json!({"field":"arguments_json"}),
        ));
    }
    assert_bound(&arguments, "sop_action_arguments", MAX_INLINE_VALUE_BYTES)?;
    let request_fingerprint =
        required_string(object.get("request_fingerprint"), "sop_action_corrupt", 512)?;
    let expected_action_id = deterministic_id("soa_", &format!("{run_id}\0{step_id}"));
    let expected_occurrence_key = deterministic_id("sop_action_", &format!("{run_id}\0{step_id}"));
    if action_id != expected_action_id || occurrence_key != expected_occurrence_key {
        return Err(diagnostic(
            "sop_action_identity_mismatch",
            &format!("sop_action_identity_mismatch:{action_id}"),
            json!({"action_id":action_id,"expected_action_id":expected_action_id,
                "occurrence_key":occurrence_key,"expected_occurrence_key":expected_occurrence_key}),
        ));
    }
    let actual_request_fingerprint = fingerprint(&json!({
        "surface_id":surface_id,"tool_name":tool_name,"arguments":arguments
    }));
    if request_fingerprint != actual_request_fingerprint {
        return Err(diagnostic(
            "sop_action_request_fingerprint_mismatch",
            &format!("sop_action_request_fingerprint_mismatch:{action_id}"),
            json!({"action_id":action_id,"expected":request_fingerprint,"actual":actual_request_fingerprint}),
        ));
    }
    let status = required_string(object.get("status"), "sop_action_status_invalid", 64)?;
    if !matches!(
        status.as_str(),
        "pending" | "completed" | "failed" | "cancelled"
    ) {
        return Err(diagnostic(
            "sop_action_status_invalid",
            &format!("sop_action_status_invalid:{status}"),
            json!({}),
        ));
    }
    let completion_key = optional_string(object.get("completion_key"));
    let completion_fingerprint = optional_string(object.get("completion_fingerprint"));
    let operation_ref = optional_string(object.get("operation_ref"));
    let result = object
        .get("result_json")
        .cloned()
        .unwrap_or_else(|| json!({}));
    if !result.is_object() {
        return Err(diagnostic(
            "sop_action_corrupt",
            "sop_action_corrupt",
            json!({"field":"result_json"}),
        ));
    }
    let result_ref = normalize_value_ref(object.get("result_ref_json"), "sop_result_ref")?;
    let error_message = optional_string(object.get("error_message"));
    if let Some(recorded) = completion_fingerprint.as_ref() {
        if completion_key.is_none()
            || operation_ref.is_none()
            || !matches!(status.as_str(), "completed" | "failed")
        {
            return Err(diagnostic(
                "sop_action_completion_identity_invalid",
                "sop_action_completion_identity_invalid",
                json!({"action_id":action_id,"status":status}),
            ));
        }
        let actual = fingerprint(&json!({
            "completion_key":completion_key,"outcome":status,"operation_ref":operation_ref,
            "result":result,"result_ref":result_ref,"error_message":error_message
        }));
        if recorded != &actual {
            return Err(diagnostic(
                "sop_action_completion_fingerprint_mismatch",
                "sop_action_completion_fingerprint_mismatch",
                json!({"action_id":action_id}),
            ));
        }
    } else if completion_key.is_some()
        || operation_ref.is_some()
        || matches!(status.as_str(), "completed" | "failed")
    {
        return Err(diagnostic(
            "sop_action_completion_identity_invalid",
            "sop_action_completion_identity_invalid",
            json!({"action_id":action_id,"status":status}),
        ));
    }
    Ok(json!({
        "schema":"narada.sop.action.v1","action_id":action_id,"run_id":run_id,
        "step_id":step_id,"occurrence_key":occurrence_key,"surface_id":surface_id,
        "tool_name":tool_name,"arguments":arguments,"request_fingerprint":request_fingerprint,
        "status":status,"completion_key":completion_key,"completion_fingerprint":completion_fingerprint,
        "operation_ref":operation_ref,"result":result,"result_ref":result_ref,
        "error_message":error_message,
        "created_at":object.get("created_at").cloned().unwrap_or(Value::Null),
        "updated_at":object.get("updated_at").cloned().unwrap_or(Value::Null),
        "completed_at":object.get("completed_at").cloned().unwrap_or(Value::Null)
    }))
}

fn get_action(db: &Connection, action_id: &str) -> Result<Value, Value> {
    let row = db
        .query_row(
            "SELECT * FROM sop_actions WHERE action_id = ?",
            params![action_id],
            row_json,
        )
        .optional()
        .map_err(|error| diagnostic("sop_action_query_failed", &error.to_string(), json!({})))?
        .ok_or_else(|| {
            diagnostic(
                "sop_action_not_found",
                &format!("sop_action_not_found:{action_id}"),
                json!({}),
            )
        })?;
    hydrate_action(row)
}

fn action_resolution_run_view(db: &Connection, run_id: &str) -> Value {
    match get_run(db, run_id) {
        Ok(run) => run_result(&run, None),
        Err(error) => {
            let fallback = db
                .query_row(
                    "SELECT run_id, sop_id, sop_version, status, occurrence_key, updated_at FROM sop_runs WHERE run_id = ?",
                    params![run_id],
                    row_json,
                )
                .optional()
                .ok()
                .flatten()
                .unwrap_or_else(|| json!({"run_id":run_id}));
            let mut fallback = fallback.as_object().cloned().unwrap_or_default();
            fallback.insert("unavailable".to_string(), json!(true));
            fallback.insert("diagnostic".to_string(), error);
            Value::Object(fallback)
        }
    }
}

fn ensure_action_intent(db: &Connection, run: &Run, step: &Value) -> Result<Value, Value> {
    let step_id = step_string(step, "step_id");
    let action = step
        .get("action")
        .and_then(Value::as_object)
        .ok_or_else(|| {
            diagnostic(
                "sop_action_binding_required",
                &format!("sop_action_binding_required:{step_id}"),
                json!({}),
            )
        })?;
    let action_id = deterministic_id("soa_", &format!("{}\0{}", run.run_id, step_id));
    let occurrence_key = deterministic_id("sop_action_", &format!("{}\0{}", run.run_id, step_id));
    let mapped = resolve_mapping(
        action.get("arguments").unwrap_or(&json!({})),
        &value_context(run),
    )?;
    let mut arguments = mapped.as_object().cloned().ok_or_else(|| {
        diagnostic(
            "sop_action_arguments_must_be_object",
            &format!("sop_action_arguments_must_be_object:{step_id}"),
            json!({}),
        )
    })?;
    let idempotency_field = required_string(
        action.get("idempotency_key_argument"),
        "sop_action_requires_idempotency_key_argument",
        128,
    )?;
    if arguments
        .get(&idempotency_field)
        .is_some_and(|value| value != &json!(occurrence_key))
    {
        return Err(diagnostic(
            "sop_action_idempotency_argument_conflict",
            &format!("sop_action_idempotency_argument_conflict:{step_id}"),
            json!({"field":idempotency_field}),
        ));
    }
    arguments.insert(idempotency_field, json!(occurrence_key));
    let arguments = Value::Object(arguments);
    assert_bound(&arguments, "sop_action_arguments", MAX_INLINE_VALUE_BYTES)?;
    let surface_id = required_string(
        action.get("surface_id"),
        "sop_action_requires_surface_id",
        256,
    )?;
    let tool_name = required_string(
        action.get("tool_name"),
        "sop_action_requires_tool_name",
        256,
    )?;
    let request_fingerprint = fingerprint(&json!({
        "surface_id":surface_id,"tool_name":tool_name,"arguments":arguments
    }));
    let existing_id = db
        .query_row(
            "SELECT action_id FROM sop_actions WHERE run_id = ? AND step_id = ?",
            params![run.run_id, step_id],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .map_err(|error| diagnostic("sop_action_query_failed", &error.to_string(), json!({})))?;
    if let Some(existing_id) = existing_id {
        let existing = get_action(db, &existing_id)?;
        if existing.get("action_id").and_then(Value::as_str) != Some(action_id.as_str())
            || existing.get("request_fingerprint").and_then(Value::as_str)
                != Some(request_fingerprint.as_str())
        {
            return Err(diagnostic(
                "sop_action_intent_conflict",
                &format!("sop_action_intent_conflict:{}:{step_id}", run.run_id),
                json!({}),
            ));
        }
        return Ok(existing);
    }
    let now = now_iso();
    db.execute(
        "INSERT INTO sop_actions (action_id, run_id, step_id, occurrence_key, surface_id, tool_name, arguments_json, request_fingerprint, status, created_at, updated_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?, 'pending', ?, ?)",
        params![action_id,run.run_id,step_id,occurrence_key,surface_id,tool_name,
            canonical_json(&arguments),request_fingerprint,now,now],
    )
    .map_err(|error| diagnostic("sop_action_insert_failed", &error.to_string(), json!({})))?;
    get_action(db, &action_id)
}

fn reconcile_run_and_ancestors(db: &Connection, run_id: &str) -> Result<(), Value> {
    let mut current = Some(run_id.to_string());
    let mut seen = HashSet::new();
    while let Some(id) = current {
        if !seen.insert(id.clone()) {
            return Err(diagnostic(
                "sop_parent_chain_cycle",
                &format!("sop_parent_chain_cycle:{id}"),
                json!({}),
            ));
        }
        reconcile_run(db, &id, &mut HashSet::new())?;
        current = get_run(db, &id)?.parent_run_id;
    }
    Ok(())
}

