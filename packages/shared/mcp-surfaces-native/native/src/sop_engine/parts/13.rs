fn reconcile_pending_steps(
    db: &Connection,
    run: &mut Run,
    stack: &mut HashSet<String>,
) -> Result<(bool, bool), Value> {
    let mut changed = false;
    let mut progress = false;
    for index in 0..run.step_states.len() {
        let step = run.step_states[index].clone();
        if step.get("status").and_then(Value::as_str) != Some("pending") {
            continue;
        }
        let step_id = step_string(&step, "step_id");
        let dependencies = step
            .get("depends_on")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .filter_map(|value| value.as_str().map(ToString::to_string))
            .collect::<Vec<_>>();
        let statuses = run
            .step_states
            .iter()
            .filter_map(|candidate| {
                Some((
                    candidate.get("step_id")?.as_str()?.to_string(),
                    candidate.get("status")?.as_str()?.to_string(),
                ))
            })
            .collect::<HashMap<_, _>>();
        let failed = dependencies
            .iter()
            .filter(|dependency| {
                statuses
                    .get(*dependency)
                    .is_some_and(|status| status == "failed")
            })
            .cloned()
            .collect::<Vec<_>>();
        if !failed.is_empty() {
            fail_step(
                &mut run.step_states[index],
                format!("failed_dependency:{}", failed.join(",")),
            );
            append_run_event(
                db,
                &run.run_id,
                Some(&step_id),
                "step_failed",
                json!({"failed_dependencies":failed}),
            )?;
            changed = true;
            progress = true;
            continue;
        }
        if !dependencies.iter().all(|dependency| {
            statuses
                .get(dependency)
                .is_some_and(|status| matches!(status.as_str(), "completed" | "skipped"))
        }) {
            continue;
        }
        let context = value_context(run);
        let attempt = (|| -> Result<(), Value> {
            let condition = step.get("when").cloned().unwrap_or(Value::Null);
            if !evaluate_condition(&condition, &context)? {
                skip_step(&mut run.step_states[index], "condition_false");
                append_run_event(
                    db,
                    &run.run_id,
                    Some(&step_id),
                    "step_skipped",
                    json!({"reason":"condition_false","when":condition}),
                )?;
                return Ok(());
            }
            let instructions = render_instructions(
                step.get("instructions")
                    .and_then(Value::as_str)
                    .unwrap_or_default(),
                &context,
            )?;
            set_step(&mut run.step_states[index], "started_at", json!(now_iso()));
            match step
                .get("executor")
                .and_then(Value::as_str)
                .unwrap_or_default()
            {
                "engine" => {
                    set_step(&mut run.step_states[index], "status", json!("completed"));
                    set_step(
                        &mut run.step_states[index],
                        "completed_at",
                        json!(now_iso()),
                    );
                    set_step(&mut run.step_states[index], "result", json!({}));
                    append_run_event(
                        db,
                        &run.run_id,
                        Some(&step_id),
                        "step_completed",
                        json!({"executor":"engine"}),
                    )?;
                }
                executor @ ("agent" | "operator") => {
                    let handoff = ensure_handoff_intent(db, run, &step, Some(&instructions))?;
                    set_step(&mut run.step_states[index], "status", json!("running"));
                    set_step(
                        &mut run.step_states[index],
                        "result",
                        json!({
                            "instructions":instructions,"handoff_id":handoff.get("handoff_id"),
                            "handoff_occurrence_key":handoff.get("occurrence_key")
                        }),
                    );
                    append_run_event(
                        db,
                        &run.run_id,
                        Some(&step_id),
                        "step_started",
                        json!({
                            "executor":executor,"handoff":true,
                            "handoff_id":handoff.get("handoff_id"),
                            "occurrence_key":handoff.get("occurrence_key")
                        }),
                    )?;
                }
                "action" => {
                    let action = ensure_action_intent(db, run, &step)?;
                    set_step(&mut run.step_states[index], "status", json!("running"));
                    set_step(
                        &mut run.step_states[index],
                        "action_id",
                        action.get("action_id").cloned().unwrap_or(Value::Null),
                    );
                    set_step(
                        &mut run.step_states[index],
                        "result",
                        json!({
                            "instructions":instructions,"action_id":action.get("action_id"),
                            "occurrence_key":action.get("occurrence_key"),
                            "surface_id":action.get("surface_id"),"tool_name":action.get("tool_name")
                        }),
                    );
                    append_run_event(
                        db,
                        &run.run_id,
                        Some(&step_id),
                        "action_admitted",
                        json!({
                            "action_id":action.get("action_id"),"occurrence_key":action.get("occurrence_key"),
                            "surface_id":action.get("surface_id"),"tool_name":action.get("tool_name"),
                            "request_fingerprint":action.get("request_fingerprint")
                        }),
                    )?;
                }
                "sop" => {
                    let child = start_child_run(db, run, &step, stack)?;
                    set_step(&mut run.step_states[index], "status", json!("running"));
                    set_step(
                        &mut run.step_states[index],
                        "child_run_id",
                        json!(child.run_id),
                    );
                    set_step(
                        &mut run.step_states[index],
                        "result",
                        json!({
                            "instructions":instructions,"child_run_id":child.run_id,
                            "child_sop_id":child.sop_id,"child_sop_version":child.sop_version,
                            "child_status":child.status,"wait_policy":"wait"
                        }),
                    );
                    append_run_event(
                        db,
                        &run.run_id,
                        Some(&step_id),
                        "child_sop_admitted",
                        json!({
                            "child_run_id":child.run_id,"child_sop_id":child.sop_id,
                            "child_sop_version":child.sop_version,
                            "child_definition_fingerprint":child.definition_fingerprint
                        }),
                    )?;
                }
                executor => {
                    return Err(diagnostic(
                        "sop_invalid_executor",
                        &format!("sop_invalid_executor:{executor}"),
                        json!({}),
                    ));
                }
            }
            Ok(())
        })();
        if let Err(error) = attempt {
            fail_step(&mut run.step_states[index], diagnostic_text(&error));
            append_run_event(
                db,
                &run.run_id,
                Some(&step_id),
                "step_failed",
                json!({"diagnostic":error}),
            )?;
        }
        changed = true;
        progress = true;
    }
    Ok((changed, progress))
}

fn fail_step(step: &mut Value, message: String) {
    set_step(step, "status", json!("failed"));
    set_step(step, "completed_at", json!(now_iso()));
    set_step(step, "error_message", json!(message));
}

fn skip_step(step: &mut Value, reason: &str) {
    set_step(step, "status", json!("skipped"));
    set_step(step, "completed_at", json!(now_iso()));
    set_step(step, "result", json!({"reason":reason}));
    set_step(step, "error_message", Value::Null);
}

fn diagnostic_text(error: &Value) -> String {
    let code = error
        .get("code")
        .and_then(Value::as_str)
        .unwrap_or("sop_internal_error");
    let message = error
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or("sop_internal_error");
    format!("{code}:{message}")
}

