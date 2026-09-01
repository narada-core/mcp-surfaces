fn normalize_value_ref(value: Option<&Value>, field: &str) -> Result<Value, Value> {
    let Some(value) = value else {
        return Ok(Value::Null);
    };
    if value.is_null() {
        return Ok(Value::Null);
    }
    let object = value.as_object().ok_or_else(|| {
        diagnostic(
            &format!("{field}_invalid"),
            &format!("{field}_invalid"),
            json!({"field":field,"reason":"must_be_object"}),
        )
    })?;
    let allowed = ["ref", "sha256", "byte_length", "media_type"];
    let unknown = object
        .keys()
        .filter(|key| !allowed.contains(&key.as_str()))
        .cloned()
        .collect::<Vec<_>>();
    if !unknown.is_empty() {
        return Err(diagnostic(
            &format!("{field}_invalid"),
            &format!("{field}_invalid"),
            json!({"field":field,"reason":"unknown_fields","fields":unknown}),
        ));
    }
    let reference = required_string(object.get("ref"), "sop_string_required", 2048)?;
    let sha256 = required_string(object.get("sha256"), "sop_string_required", 64)?.to_lowercase();
    if sha256.len() != 64
        || !sha256
            .chars()
            .all(|character| character.is_ascii_hexdigit() && !character.is_ascii_uppercase())
    {
        return Err(diagnostic(
            &format!("{field}_invalid"),
            &format!("{field}_invalid"),
            json!({"field":field,"reason":"sha256_must_be_64_lowercase_hex"}),
        ));
    }
    let byte_length = match object.get("byte_length") {
        None | Some(Value::Null) => Value::Null,
        Some(value) => match value.as_i64() {
            Some(length) if length >= 0 => json!(length),
            _ => {
                return Err(diagnostic(
                    &format!("{field}_invalid"),
                    &format!("{field}_invalid"),
                    json!({"field":field,"reason":"byte_length_must_be_nonnegative_safe_integer"}),
                ))
            }
        },
    };
    let media_type = match object.get("media_type") {
        None | Some(Value::Null) => Value::Null,
        value => json!(required_string(value, "sop_string_required", 200)?),
    };
    Ok(json!({"ref":reference,"sha256":sha256,"byte_length":byte_length,"media_type":media_type}))
}

fn validate_schema(
    schema: Option<&Value>,
    value: &Value,
    code: &str,
    details: Value,
) -> Result<(), Value> {
    let Some(schema) = schema else {
        return Ok(());
    };
    let validator = validator_for(schema).map_err(|error| {
        diagnostic(
            "sop_json_schema_invalid",
            "sop_json_schema_invalid",
            json!({"message":error.to_string()}),
        )
    })?;
    let errors = validator
        .iter_errors(value)
        .take(20)
        .map(|error| error.to_string())
        .collect::<Vec<_>>();
    if errors.is_empty() {
        Ok(())
    } else {
        Err(diagnostic(
            code,
            code,
            merge_details(details, json!({"errors":errors})),
        ))
    }
}

fn merge_details(mut left: Value, right: Value) -> Value {
    if let (Some(left), Some(right)) = (left.as_object_mut(), right.as_object()) {
        for (key, value) in right {
            left.insert(key.clone(), value.clone());
        }
    }
    left
}

fn nullable_json(value: &Value) -> Option<String> {
    if value.is_null() {
        None
    } else {
        Some(canonical_json(value))
    }
}

fn append_run_event(
    db: &Connection,
    run_id: &str,
    step_id: Option<&str>,
    event_kind: &str,
    details: Value,
) -> Result<(), Value> {
    db.execute(
        "INSERT INTO sop_events (event_id, run_id, step_id, event_kind, details_json, recorded_at) VALUES (?, ?, ?, ?, ?, ?)",
        params![format!("soe_{}", &Uuid::new_v4().to_string()[..12]),run_id,step_id.unwrap_or(""),event_kind,canonical_json(&details),now_iso()],
    )
    .map_err(|error| diagnostic("sop_event_insert_failed", &error.to_string(), json!({})))?;
    Ok(())
}

fn persist_run(db: &Connection, run: &mut Run) -> Result<(), Value> {
    let step_states = Value::Array(run.step_states.clone());
    assert_bound(&step_states, "sop_run_state", MAX_RUN_STATE_BYTES)?;
    run.updated_at = now_iso();
    db.execute(
        "UPDATE sop_runs SET status = ?, output_json = ?, output_ref_json = ?, step_states_json = ?, updated_at = ?, completed_at = ? WHERE run_id = ?",
        params![
            run.status,
            canonical_json(&run.output),
            nullable_json(&run.output_ref),
            canonical_json(&step_states),
            run.updated_at,
            run.completed_at,
            run.run_id
        ],
    )
    .map_err(|error| diagnostic("sop_run_update_failed", &error.to_string(), json!({})))?;
    Ok(())
}

fn step_string(step: &Value, key: &str) -> String {
    step.get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

fn set_step(step: &mut Value, key: &str, value: Value) {
    step.as_object_mut()
        .expect("normalized step state")
        .insert(key.to_string(), value);
}

fn value_context(run: &Run) -> Value {
    let steps = run
        .step_states
        .iter()
        .map(|step| {
            json!({
                "step_id":step.get("step_id"),"status":step.get("status"),
                "result":step.get("result"),"result_ref":step.get("result_ref")
            })
        })
        .collect::<Vec<_>>();
    json!({"input":run.input,"input_ref":run.input_ref,"steps":steps})
}

fn read_reference(reference: &str, context: &Value) -> Option<Value> {
    let mut segments = reference.split('.').collect::<Vec<_>>();
    if segments.iter().any(|segment| {
        segment.is_empty() || matches!(*segment, "__proto__" | "prototype" | "constructor")
    }) {
        return None;
    }
    let mut current = if segments.first() == Some(&"input") {
        segments.remove(0);
        context.get("input")?.clone()
    } else if segments.first() == Some(&"input_ref") {
        segments.remove(0);
        context.get("input_ref")?.clone()
    } else if segments.first() == Some(&"steps") && segments.len() >= 3 {
        let step_id = segments[1];
        let step = context
            .get("steps")?
            .as_array()?
            .iter()
            .find(|step| step.get("step_id").and_then(Value::as_str) == Some(step_id))?
            .clone();
        segments.drain(0..2);
        step
    } else {
        return None;
    };
    for segment in segments {
        current = match current {
            Value::Array(values) => {
                let index = segment.parse::<usize>().ok()?;
                values.get(index)?.clone()
            }
            Value::Object(object) => object.get(segment)?.clone(),
            _ => return None,
        };
    }
    Some(current)
}

fn resolve_mapping(mapping: &Value, context: &Value) -> Result<Value, Value> {
    match mapping {
        Value::Array(values) => values
            .iter()
            .map(|value| resolve_mapping(value, context))
            .collect::<Result<Vec<_>, _>>()
            .map(Value::Array),
        Value::Object(object) if object.len() == 1 && object.contains_key("$ref") => {
            let reference = required_string(object.get("$ref"), "sop_string_required", 512)?;
            read_reference(&reference, context).ok_or_else(|| {
                diagnostic(
                    "sop_mapping_reference_missing",
                    "sop_mapping_reference_missing",
                    json!({"ref":reference}),
                )
            })
        }
        Value::Object(object) => {
            let mut output = Map::new();
            for (key, value) in object {
                output.insert(key.clone(), resolve_mapping(value, context)?);
            }
            Ok(Value::Object(output))
        }
        _ => Ok(mapping.clone()),
    }
}

