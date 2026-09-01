fn advance_value_with_roots(
    mut task: Value,
    root: &Path,
    allowed_roots: &[PathBuf],
) -> Result<Value, Value> {
    let constraints_changed = normalize_persisted_constraints(&mut task);
    if matches!(
        task.get("status").and_then(Value::as_str),
        Some("completed" | "failed" | "cancelled")
    ) {
        if constraints_changed {
            task["updated_at"] = json!(now());
            write_task(root, &task)?;
        }
        return Ok(task);
    }
    let id = task["task_id"].as_str().unwrap_or_default().to_string();
    if !external_dependency_gate(&mut task, root)? {
        finalize_timing(&mut task);
        task["updated_at"] = json!(now());
        write_task(root, &task)?;
        return Ok(task);
    }
    let step_ids = task
        .pointer("/workflow/steps")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|step| step.get("id").and_then(Value::as_str).map(str::to_string))
        .collect::<Vec<_>>();
    for step_id in &step_ids {
        if step_status(&task, step_id) != Some("running") {
            continue;
        }
        let run_id = task
            .pointer(&format!("/result/step_states/{step_id}/current_run_id"))
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let status = crate::worker_delegation::call_tool(
            "worker_run_status",
            &worker_status_args(&run_id),
            root,
            allowed_roots,
        )?;
        let worker = status
            .pointer("/run/status")
            .and_then(Value::as_str)
            .unwrap_or("running")
            .to_string();
        let worker_run = status.get("run").cloned().unwrap_or(Value::Null);
        if worker == "completed" {
            record_worker_terminal(&mut task, step_id, &run_id, "completed", &worker_run);
            if task
                .pointer(&format!("/result/step_states/{step_id}/worker_output_contract"))
                .and_then(Value::as_str)
                == Some("failed")
            {
                task["status"] = json!("failed");
                task["result"]["step_states"][step_id]["status"] = json!("failed");
                task["result"]["step_states"][step_id]["finished_at"] = json!(now());
                set_outcome_verdicts(&mut task, "failed");
                append_event(
                    root,
                    &id,
                    "worker_output_contract_failed",
                    json!({"step_id":step_id,"run_id":run_id,"code":"worker_structured_output_required"}),
                )?;
            } else {
                task["result"]["step_states"][step_id]["status"] = json!("completed");
                task["result"]["step_states"][step_id]["finished_at"] = json!(now());
                append_event(
                    root,
                    &id,
                    "worker_completed",
                    json!({"step_id":step_id,"run_id":run_id}),
                )?;
            }
        } else if matches!(
            worker.as_str(),
            "failed" | "cancelled" | "completed_with_errors" | "orphaned"
        ) {
            record_worker_terminal(&mut task, step_id, &run_id, &worker, &worker_run);
            let attempts = task["result"]["step_states"][step_id]["attempts"]
                .as_u64()
                .unwrap_or(1);
            if attempts <= max_retries(&task) {
                task["result"]["step_states"][step_id]["status"] = json!("pending");
                task["result"]["step_states"][step_id]["current_run_id"] = Value::Null;
                append_event(
                    root,
                    &id,
                    "step_retry_scheduled",
                    json!({"step_id":step_id,"run_id":run_id,"attempts":attempts,"max_retries":max_retries(&task)}),
                )?;
            } else {
                task["status"] = json!("failed");
                task["result"]["step_states"][step_id]["status"] = json!("failed");
                set_outcome_verdicts(&mut task, "failed");
                append_event(
                    root,
                    &id,
                    "task_failed",
                    json!({"step_id":step_id,"run_id":run_id,"worker_status":worker}),
                )?;
            }
        }
    }
    let (current_acceptance, current_checks) = acceptance_verdict(&task, root);
    if task_is_terminal(&task) {
        set_outcome_verdicts(&mut task, current_acceptance);
    } else {
        set_outcome_verdicts(&mut task, "pending");
    }
    task["result"]["acceptance_checks"] = json!(current_checks);
    if task.get("status").and_then(Value::as_str) != Some("failed") {
        let steps = task
            .pointer("/workflow/steps")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        loop {
            let mut local_progress = false;
            for step in &steps {
                let Some(step_id) = step.get("id").and_then(Value::as_str) else {
                    continue;
                };
                if step_status(&task, step_id) != Some("pending") {
                    continue;
                }
                let blocked = step
                    .get("depends_on")
                    .and_then(Value::as_array)
                    .map(|items| {
                        items
                            .iter()
                            .filter_map(Value::as_str)
                            .filter(|dependency| {
                                matches!(step_status(&task, dependency), Some("failed" | "blocked"))
                            })
                            .map(str::to_string)
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default();
                if !blocked.is_empty() {
                    task["result"]["step_states"][step_id]["status"] = json!("blocked");
                    task["result"]["step_states"][step_id]["blocked_by"] = json!(blocked);
                    task["result"]["step_states"][step_id]["finished_at"] = json!(now());
                    append_event(
                        root,
                        &id,
                        "step_blocked",
                        json!({"step_id":step_id,"blocked_by":task["result"]["step_states"][step_id]["blocked_by"]}),
                    )?;
                    local_progress = true;
                    continue;
                }
                let dependencies_ready = step
                    .get("depends_on")
                    .and_then(Value::as_array)
                    .map(|items| {
                        items.iter().filter_map(Value::as_str).all(|dependency| {
                            matches!(
                                step_status(&task, dependency),
                                Some("completed" | "skipped" | "noted")
                            )
                        })
                    })
                    .unwrap_or(true);
                if !dependencies_ready {
                    continue;
                }
                if !condition_passes(step.get("if").and_then(Value::as_str), &task) {
                    task["result"]["step_states"][step_id]["status"] = json!("skipped");
                    task["result"]["step_states"][step_id]["finished_at"] = json!(now());
                    append_event(
                        root,
                        &id,
                        "step_skipped",
                        json!({"step_id":step_id,"condition":step.get("if")}),
                    )?;
                    local_progress = true;
                    continue;
                }
                let kind = step.get("kind").and_then(Value::as_str).unwrap_or("worker");
                if matches!(kind, "gate" | "join" | "note") {
                    task["result"]["step_states"][step_id]["status"] =
                        json!(if kind == "note" { "noted" } else { "completed" });
                    task["result"]["step_states"][step_id]["finished_at"] = json!(now());
                    append_event(
                        root,
                        &id,
                        if kind == "gate" {
                            "step_gate_evaluated"
                        } else if kind == "join" {
                            "step_join_completed"
                        } else {
                            "step_recorded"
                        },
                        json!({"step_id":step_id,"kind":kind,"authority_gate":step.get("authority_gate"),"executed":false}),
                    )?;
                    local_progress = true;
                }
            }
            if !local_progress {
                break;
            }
        }
        let ready = ready_step_ids(&task);
        let mut active = step_ids
            .iter()
            .filter(|step_id| step_status(&task, step_id) == Some("running"))
            .count();
        let concurrency = max_concurrency(&task);
        for step_id in ready {
            let Some(step) = steps
                .iter()
                .find(|step| step.get("id").and_then(Value::as_str) == Some(step_id.as_str()))
            else {
                continue;
            };
            let kind = step.get("kind").and_then(Value::as_str).unwrap_or("worker");
            if matches!(kind, "gate" | "join" | "note") {
                continue;
            }
            if active >= concurrency {
                continue;
            }
            let instruction = step
                .get("instruction")
                .and_then(Value::as_str)
                .or_else(|| task.get("objective").and_then(Value::as_str))
                .unwrap_or_default();
            let instruction = structured_output_instruction_for_step(&task, Some(step))
                .map(|contract| format!("{instruction}{contract}"))
                .unwrap_or_else(|| instruction.to_string());
            let instruction = imported_output_instruction(&task)
                .map(|imports| format!("{instruction}{imports}"))
                .unwrap_or(instruction);
            // The lifecycle authority owns polling and durable recovery. Never
            // let a worker child synchronously occupy this MCP request.
            let constraints = asynchronous_worker_constraints(&task, step);
            let worker_args =
                json!({"intent":{"instruction":instruction},"constraints":constraints});
            let run = crate::worker_delegation::call_tool(
                "worker_run",
                worker_args.as_object().unwrap(),
                root,
                allowed_roots,
            )?;
            let run_id = run
                .get("run_id")
                .and_then(Value::as_str)
                .unwrap_or_default();
            if task.get("started_at").is_none_or(Value::is_null) {
                task["started_at"] = json!(now());
                task["started_at_ms"] = json!(now_ms());
            }
            let attempts = task["result"]["step_states"][&step_id]["attempts"]
                .as_u64()
                .unwrap_or(0)
                + 1;
            task["status"] = json!("running");
            task["result"]["step_states"][&step_id]["status"] = json!("running");
            task["result"]["step_states"][&step_id]["attempts"] = json!(attempts);
            task["result"]["step_states"][&step_id]["current_run_id"] = json!(run_id);
            if let Some(run_ids) = task["result"]["step_states"][&step_id]["run_ids"].as_array_mut()
            {
                run_ids.push(json!(run_id));
            }
            task["result"]["step_states"][&step_id]["started_at"] = json!(now());
            if let Some(refs) = task["result"]["worker_refs"].as_array_mut() {
                refs.push(
                    json!({"step_id":step_id,"step_kind":kind,"run_id":run_id,"status":"running"}),
                );
            }
            append_event(
                root,
                &id,
                "worker_started",
                json!({"step_id":step_id,"run_id":run_id,"attempt":attempts}),
            )?;
            append_event(
                root,
                &id,
                "evidence_resolution_completed",
                json!({
                    "step_id":step_id,
                    "run_id":run_id,
                    "preflight_evidence_ref":run.pointer("/resolved_invocation/preflight_evidence_ref"),
                    "native_evidence_count":run.pointer("/capability_snapshot/preflight/items").and_then(Value::as_array).map(Vec::len).unwrap_or(0)
                }),
            )?;
            active += 1;
        }
        if !step_ids.is_empty()
            && step_ids.iter().all(|step_id| {
                matches!(
                    step_status(&task, step_id),
                    Some("completed" | "skipped" | "noted")
                )
            })
        {
            // Acceptance checks such as strict_clean_run are terminal-state
            // predicates. Mark the candidate outcome terminal before deriving
            // them, then downgrade to failed if a check or output contract fails.
            task["status"] = json!("completed");
            let (verdict, checks) = acceptance_verdict(&task, root);
            set_outcome_verdicts(&mut task, verdict);
            task["result"]["acceptance_checks"] = json!(checks);
            let terminal_failed = verdict == "failed"
                || output_contract_verdict(&task) == "failed";
            task["status"] = json!(if terminal_failed {
                "failed"
            } else {
                "completed"
            });
            append_event(
                root,
                &id,
                if terminal_failed {
                    "task_failed"
                } else {
                    "task_completed"
                },
                json!({
                    "output_contract_verdict":task["result"]["output_contract_verdict"],
                    "objective_verdict":task["result"]["objective_verdict"],
                    "acceptance_verdict":verdict
                }),
            )?;
        } else if step_ids
            .iter()
            .any(|step_id| matches!(step_status(&task, step_id), Some("failed" | "blocked")))
            && !step_ids
                .iter()
                .any(|step_id| matches!(step_status(&task, step_id), Some("pending" | "running")))
        {
            task["status"] = json!("failed");
            set_outcome_verdicts(&mut task, "failed");
            append_event(
                root,
                &id,
                "task_failed",
                json!({"reason":"blocked_or_failed_steps"}),
            )?;
        }
    }
    finalize_timing(&mut task);
    task["updated_at"] = json!(now());
    write_task(root, &task)?;
    Ok(task)
}

