fn reconcile_running_steps(
    db: &Connection,
    run: &mut Run,
    stack: &mut HashSet<String>,
) -> Result<(bool, bool), Value> {
    let mut changed = false;
    let mut progress = false;
    for index in 0..run.step_states.len() {
        let step = run.step_states[index].clone();
        if step.get("status").and_then(Value::as_str) != Some("running") {
            continue;
        }
        let executor = step_string(&step, "executor");
        let step_id = step_string(&step, "step_id");
        match executor.as_str() {
            "agent" | "operator" => {
                let handoff = ensure_handoff_intent(db, run, &step, None)?;
                let handoff_id = handoff.get("handoff_id").cloned().unwrap_or(Value::Null);
                let occurrence_key = handoff
                    .get("occurrence_key")
                    .cloned()
                    .unwrap_or(Value::Null);
                let prior_id = step.get("result").and_then(|value| value.get("handoff_id"));
                if prior_id != Some(&handoff_id) {
                    let mut result = step
                        .get("result")
                        .and_then(Value::as_object)
                        .cloned()
                        .unwrap_or_default();
                    result.insert("handoff_id".to_string(), handoff_id);
                    result.insert("handoff_occurrence_key".to_string(), occurrence_key);
                    set_step(&mut run.step_states[index], "result", Value::Object(result));
                    changed = true;
                }
            }
            "sop" => {
                let Some(child_run_id) = step.get("child_run_id").and_then(Value::as_str) else {
                    continue;
                };
                reconcile_run(db, child_run_id, stack)?;
                let child = get_run(db, child_run_id)?;
                assert_child_run_binding(run, &step, &child)?;
                if child.status == "completed" {
                    if let Err(error) = validate_schema(
                        step.get("result_schema").filter(|value| !value.is_null()),
                        &child.output,
                        "sop_step_result_schema_mismatch",
                        json!({"run_id":run.run_id,"step_id":step_id}),
                    ) {
                        fail_step(&mut run.step_states[index], diagnostic_text(&error));
                        append_run_event(
                            db,
                            &run.run_id,
                            Some(&step_id),
                            "step_failed",
                            json!({"child_run_id":child.run_id,"diagnostic":error}),
                        )?;
                        changed = true;
                        progress = true;
                        continue;
                    }
                    let full_result = json!({
                        "child_run_id":child.run_id,"child_sop_id":child.sop_id,
                        "child_sop_version":child.sop_version,"child_status":child.status,
                        "output":child.output
                    });
                    let compact_result = json!({
                        "child_run_id":child.run_id,"child_sop_id":child.sop_id,
                        "child_sop_version":child.sop_version,"child_status":child.status
                    });
                    let completed_at = child.completed_at.clone().unwrap_or_else(now_iso);
                    let retained = complete_step_with_bounded_run_state(
                        run,
                        index,
                        &completed_at,
                        full_result,
                        child.output_ref.clone(),
                        compact_result,
                        db,
                    )?;
                    if retained {
                        append_run_event(
                            db,
                            &run.run_id,
                            Some(&step_id),
                            "child_sop_completed",
                            json!({"child_run_id":child.run_id,"child_status":child.status,"output_ref":child.output_ref}),
                        )?;
                    }
                    changed = true;
                    progress = true;
                } else if matches!(child.status.as_str(), "failed" | "cancelled") {
                    fail_step(
                        &mut run.step_states[index],
                        format!("child_sop_{}:{}", child.status, child.run_id),
                    );
                    set_step(
                        &mut run.step_states[index],
                        "result",
                        json!({
                            "child_run_id":child.run_id,"child_sop_id":child.sop_id,
                            "child_sop_version":child.sop_version,"child_status":child.status
                        }),
                    );
                    append_run_event(
                        db,
                        &run.run_id,
                        Some(&step_id),
                        "child_sop_failed",
                        json!({"child_run_id":child.run_id,"child_status":child.status}),
                    )?;
                    changed = true;
                    progress = true;
                }
            }
            "action" => {
                let Some(action_id) = step.get("action_id").and_then(Value::as_str) else {
                    continue;
                };
                let action = ensure_action_intent(db, run, &step)?;
                if action.get("action_id").and_then(Value::as_str) != Some(action_id) {
                    return Err(diagnostic(
                        "sop_action_run_binding_mismatch",
                        &format!("sop_action_run_binding_mismatch:{}:{step_id}", run.run_id),
                        json!({"action_id":action.get("action_id")}),
                    ));
                }
                assert_action_run_binding(run, &step, &action)?;
                let action_status = action
                    .get("status")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                if action_status == "completed" {
                    let action_result = action.get("result").cloned().unwrap_or_else(|| json!({}));
                    if let Err(error) = validate_schema(
                        step.get("result_schema").filter(|value| !value.is_null()),
                        &action_result,
                        "sop_step_result_schema_mismatch",
                        json!({"run_id":run.run_id,"step_id":step_id}),
                    ) {
                        fail_step(&mut run.step_states[index], diagnostic_text(&error));
                        set_step(
                            &mut run.step_states[index],
                            "result",
                            json!({"action_id":action.get("action_id"),"operation_ref":action.get("operation_ref"),"surface_id":action.get("surface_id"),"tool_name":action.get("tool_name")}),
                        );
                        set_step(
                            &mut run.step_states[index],
                            "result_ref",
                            action.get("result_ref").cloned().unwrap_or(Value::Null),
                        );
                        append_run_event(
                            db,
                            &run.run_id,
                            Some(&step_id),
                            "step_failed",
                            json!({"action_id":action.get("action_id"),"diagnostic":error}),
                        )?;
                        changed = true;
                        progress = true;
                        continue;
                    }
                    let mut full = action_result.as_object().cloned().unwrap_or_default();
                    for key in ["action_id", "operation_ref", "surface_id", "tool_name"] {
                        full.insert(
                            key.to_string(),
                            action.get(key).cloned().unwrap_or(Value::Null),
                        );
                    }
                    let compact = json!({
                        "action_id":action.get("action_id"),"operation_ref":action.get("operation_ref"),
                        "surface_id":action.get("surface_id"),"tool_name":action.get("tool_name")
                    });
                    let completed_at = action
                        .get("completed_at")
                        .and_then(Value::as_str)
                        .map(ToString::to_string)
                        .unwrap_or_else(now_iso);
                    complete_step_with_bounded_run_state(
                        run,
                        index,
                        &completed_at,
                        Value::Object(full),
                        action.get("result_ref").cloned().unwrap_or(Value::Null),
                        compact,
                        db,
                    )?;
                    changed = true;
                    progress = true;
                } else if matches!(action_status, "failed" | "cancelled") {
                    let error_message = action
                        .get("error_message")
                        .and_then(Value::as_str)
                        .map(ToString::to_string)
                        .unwrap_or_else(|| {
                            format!(
                                "action_{action_status}:{}",
                                action
                                    .get("action_id")
                                    .and_then(Value::as_str)
                                    .unwrap_or_default()
                            )
                        });
                    fail_step(&mut run.step_states[index], error_message);
                    set_step(
                        &mut run.step_states[index],
                        "result",
                        json!({"action_id":action.get("action_id"),"operation_ref":action.get("operation_ref"),"surface_id":action.get("surface_id"),"tool_name":action.get("tool_name")}),
                    );
                    set_step(
                        &mut run.step_states[index],
                        "result_ref",
                        action.get("result_ref").cloned().unwrap_or(Value::Null),
                    );
                    changed = true;
                    progress = true;
                }
            }
            _ => {}
        }
    }
    Ok((changed, progress))
}

