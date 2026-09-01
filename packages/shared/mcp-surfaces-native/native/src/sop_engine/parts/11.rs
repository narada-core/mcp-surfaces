fn reconcile_run(db: &Connection, run_id: &str, stack: &mut HashSet<String>) -> Result<Run, Value> {
    if !stack.insert(run_id.to_string()) {
        return Err(diagnostic(
            "sop_child_run_cycle",
            &format!("sop_child_run_cycle:{run_id}"),
            json!({}),
        ));
    }
    let result = (|| -> Result<Run, Value> {
        let mut run = get_run(db, run_id)?;
        if is_run_terminal(&run.status) {
            return Ok(run);
        }
        let mut changed = false;
        let mut progress = true;
        let mut passes = 0usize;
        while progress {
            progress = false;
            passes += 1;
            if passes > run.step_states.len() * 4 + 8 {
                return Err(diagnostic(
                    "sop_reconciliation_did_not_converge",
                    &format!("sop_reconciliation_did_not_converge:{run_id}"),
                    json!({}),
                ));
            }
            let (running_changed, running_progress) = reconcile_running_steps(db, &mut run, stack)?;
            let (pending_changed, pending_progress) = reconcile_pending_steps(db, &mut run, stack)?;
            changed |= running_changed || pending_changed;
            progress |= running_progress || pending_progress;
        }
        let prior_status = run.status.clone();
        let all_terminal = run.step_states.iter().all(|step| {
            matches!(
                step.get("status").and_then(Value::as_str),
                Some("completed" | "failed" | "skipped")
            )
        });
        if all_terminal {
            run.status = if run
                .step_states
                .iter()
                .any(|step| step.get("status").and_then(Value::as_str) == Some("failed"))
            {
                "failed".to_string()
            } else {
                "completed".to_string()
            };
            if run.status == "completed" {
                if let Err(error) = derive_run_output(&mut run) {
                    run.status = "failed".to_string();
                    run.output = json!({});
                    run.output_ref = Value::Null;
                    append_run_event(
                        db,
                        run_id,
                        None,
                        "run_output_failed",
                        json!({"diagnostic":error}),
                    )?;
                }
            } else {
                run.output = json!({});
                run.output_ref = Value::Null;
            }
            if run.completed_at.is_none() {
                run.completed_at = Some(now_iso());
            }
        } else {
            let awaiting_confirmation = run.step_states.iter().any(|step| {
                step.get("status").and_then(Value::as_str) == Some("running")
                    && matches!(
                        step.get("executor").and_then(Value::as_str),
                        Some("agent" | "operator")
                    )
            });
            run.status = if awaiting_confirmation {
                "awaiting_confirmation".to_string()
            } else {
                "running".to_string()
            };
            run.completed_at = None;
        }
        if changed || prior_status != run.status {
            persist_run(db, &mut run)?;
            if prior_status != run.status {
                let event_kind = if is_run_terminal(&run.status) {
                    if run.status == "completed" {
                        "run_completed"
                    } else {
                        "run_failed"
                    }
                } else {
                    "run_state_changed"
                };
                let states = run
                    .step_states
                    .iter()
                    .map(|step| json!({"step_id":step.get("step_id"),"status":step.get("status")}))
                    .collect::<Vec<_>>();
                append_run_event(
                    db,
                    run_id,
                    None,
                    event_kind,
                    json!({"from":prior_status,"to":run.status,"step_states":states}),
                )?;
                if is_run_terminal(&run.status) {
                    put_terminal_outbox(db, &run)?;
                }
            }
        }
        Ok(run)
    })();
    stack.remove(run_id);
    result
}

