fn evaluate_condition(condition: &Value, context: &Value) -> Result<bool, Value> {
    if condition.is_null() {
        return Ok(true);
    }
    let object = condition.as_object().ok_or_else(|| {
        diagnostic(
            "sop_condition_invalid",
            "sop_condition_invalid",
            json!({"reason":"condition_must_be_object"}),
        )
    })?;
    if let Some(all) = object.get("all").and_then(Value::as_array) {
        for child in all {
            if !evaluate_condition(child, context)? {
                return Ok(false);
            }
        }
        return Ok(true);
    }
    if let Some(any) = object.get("any").and_then(Value::as_array) {
        for child in any {
            if evaluate_condition(child, context)? {
                return Ok(true);
            }
        }
        return Ok(false);
    }
    if let Some(not) = object.get("not") {
        return Ok(!evaluate_condition(not, context)?);
    }
    let reference = required_string(object.get("ref"), "sop_string_required", 512)?;
    let operation = required_string(object.get("op"), "sop_string_required", 32)?;
    let resolved = read_reference(&reference, context);
    let comparison = object.get("value").cloned().unwrap_or(Value::Null);
    Ok(match operation.as_str() {
        "exists" => resolved.is_some(),
        "not_exists" => resolved.is_none(),
        "truthy" => resolved.as_ref().is_some_and(js_truthy),
        "falsy" => resolved.as_ref().is_some_and(|value| !js_truthy(value)),
        "equals" => resolved.as_ref().is_some_and(|value| value == &comparison),
        "not_equals" => resolved.as_ref().is_none_or(|value| value != &comparison),
        "in" => resolved.as_ref().is_some_and(|value| {
            comparison
                .as_array()
                .is_some_and(|values| values.iter().any(|candidate| candidate == value))
        }),
        "contains" => resolved.as_ref().is_some_and(|value| {
            value
                .as_array()
                .is_some_and(|values| values.iter().any(|candidate| candidate == &comparison))
        }),
        _ => {
            return Err(diagnostic(
                "sop_condition_invalid",
                "sop_condition_invalid",
                json!({"reason":"unsupported_operator","op":operation}),
            ))
        }
    })
}

fn js_truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(value) => *value,
        Value::Number(value) => value.as_f64().is_some_and(|value| value != 0.0),
        Value::String(value) => !value.is_empty(),
        Value::Array(_) | Value::Object(_) => true,
    }
}

fn render_instructions(template: &str, context: &Value) -> Result<String, Value> {
    let mut output = String::new();
    let mut remaining = template;
    while let Some(start) = remaining.find("{{") {
        output.push_str(&remaining[..start]);
        let tail = &remaining[start + 2..];
        let Some(end) = tail.find("}}") else {
            output.push_str(&remaining[start..]);
            return Ok(output);
        };
        let reference = tail[..end].trim();
        let resolved = read_reference(reference, context).ok_or_else(|| {
            diagnostic(
                "sop_mapping_reference_missing",
                "sop_mapping_reference_missing",
                json!({"ref":reference}),
            )
        })?;
        match resolved {
            Value::String(value) => output.push_str(&value),
            Value::Number(value) => output.push_str(&value.to_string()),
            Value::Bool(value) => output.push_str(if value { "true" } else { "false" }),
            value => output.push_str(&canonical_json(&value)),
        }
        remaining = &tail[end + 2..];
    }
    output.push_str(remaining);
    Ok(output)
}

fn ensure_handoff_intent(
    db: &Connection,
    run: &Run,
    step: &Value,
    rendered_instructions: Option<&str>,
) -> Result<Value, Value> {
    let executor = step_string(step, "executor");
    let step_id = step_string(step, "step_id");
    if !matches!(executor.as_str(), "agent" | "operator") {
        return Err(diagnostic(
            "sop_step_not_manual_handoff",
            &format!("sop_step_not_manual_handoff:{step_id}"),
            json!({"executor":executor}),
        ));
    }
    let context = value_context(run);
    let input = match step.get("input") {
        None | Some(Value::Null) => json!({}),
        Some(mapping) => resolve_mapping(mapping, &context)?,
    };
    assert_bound(&input, "sop_handoff_input", MAX_INLINE_VALUE_BYTES)?;
    let input_ref = match step.get("input_ref") {
        None | Some(Value::Null) => Value::Null,
        Some(mapping) => {
            let resolved = resolve_mapping(mapping, &context)?;
            normalize_value_ref(Some(&resolved), "sop_handoff_input_ref")?
        }
    };
    let instructions = if let Some(rendered) = rendered_instructions {
        rendered.to_string()
    } else if let Some(recorded) = step
        .get("result")
        .and_then(|result| result.get("instructions"))
        .and_then(Value::as_str)
    {
        recorded.to_string()
    } else {
        render_instructions(
            step.get("instructions")
                .and_then(Value::as_str)
                .unwrap_or_default(),
            &context,
        )?
    };
    let title = step_string(step, "title");
    let result_schema = step.get("result_schema").cloned().unwrap_or(Value::Null);
    let identity = format!("{}\0{}", run.run_id, step_id);
    let handoff_id = deterministic_id("soh_", &identity);
    let occurrence_key = deterministic_id("sop_handoff_", &identity);
    let request_fingerprint = fingerprint(&json!({
        "run_id":run.run_id,"step_id":step_id,"sop_id":run.sop_id,
        "sop_version":run.sop_version,"executor":executor,"title":title,
        "instructions":instructions,"input":input,"input_ref":input_ref,
        "result_schema":result_schema
    }));
    let existing_id = db
        .query_row(
            "SELECT handoff_id FROM sop_handoffs WHERE run_id = ? AND step_id = ?",
            params![run.run_id, step_id],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .map_err(|error| diagnostic("sop_handoff_query_failed", &error.to_string(), json!({})))?;
    if let Some(existing_id) = existing_id {
        let existing = get_handoff(db, &existing_id)?;
        if existing.get("handoff_id").and_then(Value::as_str) != Some(handoff_id.as_str())
            || existing.get("request_fingerprint").and_then(Value::as_str)
                != Some(request_fingerprint.as_str())
        {
            return Err(diagnostic(
                "sop_handoff_intent_conflict",
                "sop_handoff_intent_conflict",
                json!({"run_id":run.run_id,"step_id":step_id}),
            ));
        }
        return Ok(existing);
    }
    let now = now_iso();
    db.execute(
        "INSERT INTO sop_handoffs(handoff_id, run_id, step_id, occurrence_key, sop_id, sop_version, executor, title, instructions, input_json, input_ref_json, result_schema_json, request_fingerprint, status, created_at, updated_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, 'pending', ?, ?)",
        params![handoff_id,run.run_id,step_id,occurrence_key,run.sop_id,run.sop_version,
            executor,title,instructions,canonical_json(&input),nullable_json(&input_ref),
            nullable_json(&result_schema),request_fingerprint,now,now],
    )
    .map_err(|error| diagnostic("sop_handoff_insert_failed", &error.to_string(), json!({})))?;
    get_handoff(db, &handoff_id)
}

