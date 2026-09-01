fn complete_step_with_bounded_run_state(
    run: &mut Run,
    step_index: usize,
    completed_at: &str,
    full_result: Value,
    result_ref: Value,
    compact_result: Value,
    db: &Connection,
) -> Result<bool, Value> {
    {
        let step = &mut run.step_states[step_index];
        set_step(step, "status", json!("completed"));
        set_step(step, "completed_at", json!(completed_at));
        set_step(step, "error_message", Value::Null);
        set_step(step, "result", full_result);
        set_step(step, "result_ref", result_ref.clone());
    }
    let state = Value::Array(run.step_states.clone());
    match assert_bound(&state, "sop_run_state", MAX_RUN_STATE_BYTES) {
        Ok(()) => Ok(true),
        Err(error)
            if error.get("code").and_then(Value::as_str) == Some("sop_run_state_too_large") =>
        {
            let step_id = step_string(&run.step_states[step_index], "step_id");
            let step = &mut run.step_states[step_index];
            fail_step(step, diagnostic_text(&error));
            let mut compact = compact_result.as_object().cloned().unwrap_or_default();
            compact.insert("inline_result_omitted".to_string(), json!(true));
            set_step(step, "result", Value::Object(compact));
            set_step(step, "result_ref", result_ref.clone());
            assert_bound(
                &Value::Array(run.step_states.clone()),
                "sop_run_state",
                MAX_RUN_STATE_BYTES,
            )?;
            append_run_event(
                db,
                &run.run_id,
                Some(&step_id),
                "step_failed",
                json!({"diagnostic":error,"result_ref":result_ref,"inline_result_omitted":true}),
            )?;
            Ok(false)
        }
        Err(error) => Err(error),
    }
}

fn derive_run_output(run: &mut Run) -> Result<(), Value> {
    let output_mapping = run.definition.get("output").cloned().unwrap_or(Value::Null);
    let output_ref_mapping = run
        .definition
        .get("output_ref")
        .cloned()
        .unwrap_or(Value::Null);
    let context = value_context(run);
    let output = if output_mapping.is_null() {
        json!({})
    } else {
        resolve_mapping(&output_mapping, &context)?
    };
    assert_bound(&output, "sop_output", MAX_INLINE_VALUE_BYTES)?;
    if !output.is_object() {
        return Err(diagnostic(
            "sop_output_must_be_object",
            "sop_output_must_be_object",
            json!({}),
        ));
    }
    validate_schema(
        run.definition
            .get("output_schema")
            .filter(|value| !value.is_null()),
        &output,
        "sop_output_schema_mismatch",
        json!({"run_id":run.run_id}),
    )?;
    let output_ref = if output_ref_mapping.is_null() {
        Value::Null
    } else {
        let resolved = resolve_mapping(&output_ref_mapping, &context)?;
        normalize_value_ref(Some(&resolved), "sop_output_ref")?
    };
    run.output = output;
    run.output_ref = output_ref;
    Ok(())
}

fn assert_child_run_binding(parent: &Run, step: &Value, child: &Run) -> Result<(), Value> {
    let step_id = step_string(step, "step_id");
    let expected_occurrence_key = deterministic_id(
        "sop_child_",
        &format!("{}\0{}\0{}", parent.occurrence_key, parent.run_id, step_id),
    );
    let identity_matches = child.parent_run_id.as_deref() == Some(parent.run_id.as_str())
        && child.parent_step_id.as_deref() == Some(step_id.as_str())
        && step.get("sop_id").and_then(Value::as_str) == Some(child.sop_id.as_str())
        && step.get("sop_version").and_then(Value::as_i64) == Some(child.sop_version)
        && child.occurrence_key == expected_occurrence_key;
    if !identity_matches {
        return Err(diagnostic(
            "sop_child_run_binding_mismatch",
            &format!("sop_child_run_binding_mismatch:{}:{step_id}", parent.run_id),
            json!({"parent_run_id":parent.run_id,"step_id":step_id,"child_run_id":child.run_id}),
        ));
    }
    let expected_pin = step
        .get("pinned_child_definition_fingerprint")
        .and_then(Value::as_str);
    if expected_pin.is_none() || expected_pin != Some(child.definition_fingerprint.as_str()) {
        return Err(diagnostic(
            "sop_child_definition_pin_mismatch",
            &format!("sop_child_definition_pin_mismatch:{step_id}"),
            json!({"expected":expected_pin,"actual":child.definition_fingerprint}),
        ));
    }
    Ok(())
}

fn assert_action_run_binding(run: &Run, step: &Value, action: &Value) -> Result<(), Value> {
    let step_id = step_string(step, "step_id");
    let binding = step.get("action").and_then(Value::as_object);
    let valid = binding.is_some()
        && step.get("action_id").and_then(Value::as_str)
            == action.get("action_id").and_then(Value::as_str)
        && action.get("run_id").and_then(Value::as_str) == Some(run.run_id.as_str())
        && action.get("step_id").and_then(Value::as_str) == Some(step_id.as_str())
        && binding
            .and_then(|value| value.get("surface_id"))
            .and_then(Value::as_str)
            == action.get("surface_id").and_then(Value::as_str)
        && binding
            .and_then(|value| value.get("tool_name"))
            .and_then(Value::as_str)
            == action.get("tool_name").and_then(Value::as_str);
    if !valid {
        return Err(diagnostic(
            "sop_action_run_binding_mismatch",
            &format!("sop_action_run_binding_mismatch:{}:{step_id}", run.run_id),
            json!({"run_id":run.run_id,"step_id":step_id,"action_id":action.get("action_id")}),
        ));
    }
    Ok(())
}

fn start_child_run(
    db: &Connection,
    parent: &Run,
    step: &Value,
    stack: &mut HashSet<String>,
) -> Result<Run, Value> {
    let step_id = step_string(step, "step_id");
    let child_sop_id = required_string(step.get("sop_id"), "sop_step_requires_pinned_child", 256)?;
    let child_version = step
        .get("sop_version")
        .and_then(Value::as_i64)
        .filter(|value| *value >= 1)
        .ok_or_else(|| {
            diagnostic(
                "sop_step_requires_pinned_child",
                &format!("sop_step_requires_pinned_child:{step_id}"),
                json!({}),
            )
        })?;
    let context = value_context(parent);
    let child_input = match step.get("input") {
        None | Some(Value::Null) => json!({}),
        Some(mapping) => resolve_mapping(mapping, &context)?,
    };
    assert_bound(&child_input, "sop_input", MAX_INLINE_VALUE_BYTES)?;
    if !child_input.is_object() {
        return Err(diagnostic(
            "sop_child_input_must_be_object",
            &format!("sop_child_input_must_be_object:{step_id}"),
            json!({}),
        ));
    }
    let child_input_ref = match step.get("input_ref") {
        None | Some(Value::Null) => Value::Null,
        Some(mapping) => {
            let resolved = resolve_mapping(mapping, &context)?;
            normalize_value_ref(Some(&resolved), "sop_input_ref")?
        }
    };
    let occurrence_key = deterministic_id(
        "sop_child_",
        &format!("{}\0{}\0{}", parent.occurrence_key, parent.run_id, step_id),
    );
    let args = json!({
        "sop_id":child_sop_id,"sop_version":child_version,
        "occurrence_key":occurrence_key,"input":child_input,"input_ref":child_input_ref,
        "trigger_source_kind":"parent_sop_step",
        "trigger_source_ref":format!("{}:{step_id}",parent.run_id),
        "triggered_by":format!("sop:{}",parent.run_id)
    });
    let (admitted, _) = admit_run(
        db,
        args.as_object().expect("child admission object"),
        Some(&parent.run_id),
        Some(&step_id),
    )?;
    assert_child_run_binding(parent, step, &admitted)?;
    reconcile_run(db, &admitted.run_id, stack)?;
    get_run(db, &admitted.run_id)
}

