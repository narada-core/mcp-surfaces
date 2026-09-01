fn external_dependency_diagnostics(args: &Map<String, Value>, root: &Path) -> Vec<Value> {
    let mut diagnostics = Vec::new();
    let task_id = stable_task_id(args);
    let dependencies = match args.get("depends_on_task_ids") {
        None => Vec::new(),
        Some(value) => match string_array(Some(value)) {
            Some(items) if items.len() == value.as_array().map(Vec::len).unwrap_or(0) => items,
            _ => {
                diagnostics.push(json!({"severity":"error","code":"depends_on_task_ids_must_be_string_array"}));
                Vec::new()
            }
        },
    };
    let mut unique = std::collections::BTreeSet::new();
    for dependency in &dependencies {
        if safe_id(dependency).is_err() {
            diagnostics.push(json!({"severity":"error","code":"dependency_task_id_invalid","task_id":dependency}));
        } else if dependency == &task_id {
            diagnostics.push(json!({"severity":"error","code":"task_dependency_cycle","task_id":task_id}));
        } else if !unique.insert(dependency.clone()) {
            diagnostics.push(json!({"severity":"error","code":"duplicate_dependency_task_id","task_id":dependency}));
        } else if !task_path(root, dependency).is_ok_and(|path| path.is_file()) {
            diagnostics.push(json!({"severity":"error","code":"dependency_task_not_found","task_id":dependency}));
        } else if let Ok(predecessor) = read_task(root, dependency) {
            if dependency_reaches(root, dependency, &task_id, &mut std::collections::BTreeSet::new()) {
                diagnostics.push(json!({"severity":"error","code":"task_dependency_cycle","task_id":task_id,"via":dependency}));
            }
            let authority_rank = |value: Option<&str>| match value { Some("read") => 0, Some("write") => 1, Some("command") => 2, _ => 0 };
            let predecessor_rank = authority_rank(predecessor.pointer("/constraints/authority").and_then(Value::as_str));
            let downstream_rank = authority_rank(args.get("constraints").and_then(|value| value.get("authority")).and_then(Value::as_str));
            if downstream_rank > predecessor_rank {
                diagnostics.push(json!({"severity":"error","code":"dependency_authority_escalation","task_id":dependency,"predecessor_authority":predecessor.pointer("/constraints/authority"),"downstream_authority":args.get("constraints").and_then(|value| value.get("authority"))}));
            }
        }
    }
    for field in ["import_task_outputs", "import_worker_refs"] {
        let imports = match args.get(field) {
            None => Vec::new(),
            Some(value) => match string_array(Some(value)) {
                Some(items) if items.len() == value.as_array().map(Vec::len).unwrap_or(0) => items,
                _ => {
                    diagnostics.push(json!({"severity":"error","code":format!("{field}_must_be_string_array")}));
                    Vec::new()
                }
            },
        };
        for imported in imports {
            if !dependencies.contains(&imported) {
                diagnostics.push(json!({"severity":"error","code":"task_import_must_name_declared_dependency","field":field,"task_id":imported}));
            }
        }
    }
    diagnostics
}

fn dependency_reaches(root: &Path, start: &str, target: &str, seen: &mut std::collections::BTreeSet<String>) -> bool {
    if start == target { return true; }
    if !seen.insert(start.to_string()) { return false; }
    read_task(root, start).ok()
        .and_then(|task| task.get("depends_on_task_ids").and_then(Value::as_array).cloned())
        .is_some_and(|dependencies| dependencies.iter().filter_map(Value::as_str).any(|dependency| dependency_reaches(root, dependency, target, seen)))
}

fn external_dependency_gate(task: &mut Value, root: &Path) -> Result<bool, Value> {
    let dependencies = task.get("depends_on_task_ids").and_then(Value::as_array).cloned().unwrap_or_default();
    if dependencies.is_empty() {
        task["external_dependencies"]["status"] = json!("resolved");
        return Ok(true);
    }
    let id = task.get("task_id").and_then(Value::as_str).unwrap_or("unknown").to_string();
    let mut waiting = Vec::new();
    let mut blocked = Vec::new();
    for dependency in &dependencies {
        let Some(dependency_id) = dependency.as_str() else { continue };
        let predecessor = read_task(root, dependency_id)?;
        match predecessor.get("status").and_then(Value::as_str) {
            Some("completed") => {}
            Some("failed" | "cancelled") => blocked.push(dependency_id.to_string()),
            _ => waiting.push(dependency_id.to_string()),
        }
    }
    if !blocked.is_empty() {
        task["external_dependencies"] = json!({"status":"blocked","resolved_at":null,"blocked_by":blocked});
        for state in task["result"]["step_states"].as_object_mut().into_iter().flat_map(|states| states.values_mut()) {
            if state.get("status").and_then(Value::as_str) == Some("pending") {
                state["status"] = json!("blocked");
                state["blocked_by_external_tasks"] = json!(blocked);
                state["finished_at"] = json!(now());
            }
        }
        task["status"] = json!("failed");
        set_outcome_verdicts(task, "failed");
        append_event(root, &id, "task_dependency_blocked", json!({"blocked_by":blocked}))?;
        return Ok(false);
    }
    if !waiting.is_empty() {
        task["external_dependencies"] = json!({"status":"waiting","resolved_at":null,"waiting_for":waiting,"blocked_by":[]});
        return Ok(false);
    }
    if task.pointer("/external_dependencies/status").and_then(Value::as_str) != Some("resolved") {
        let imports = task.get("import_task_outputs").and_then(Value::as_array).cloned().unwrap_or_default();
        let mut resolved = Vec::new();
        let mut total_bytes = 0usize;
        for imported in imports {
            let dependency_id = imported.as_str().unwrap_or_default();
            let predecessor = read_task(root, dependency_id)?;
            let output = final_step_projection(&predecessor).get("final_structured_output").cloned().unwrap_or(Value::Null);
            if output.is_null() {
                task["external_dependencies"] = json!({"status":"blocked","resolved_at":null,"blocked_by":[dependency_id],"reason":"predecessor_structured_output_missing"});
                task["status"] = json!("failed");
                set_outcome_verdicts(task, "failed");
                append_event(root, &id, "task_dependency_blocked", json!({"blocked_by":[dependency_id],"reason":"predecessor_structured_output_missing"}))?;
                return Ok(false);
            }
            total_bytes = total_bytes.saturating_add(serde_json::to_vec(&output).map(|bytes| bytes.len()).unwrap_or(MAX_IMPORTED_TASK_OUTPUT_BYTES + 1));
            if total_bytes > MAX_IMPORTED_TASK_OUTPUT_BYTES {
                return Err(error("imported_task_outputs_too_large", "imported_task_outputs_too_large"));
            }
            resolved.push(json!({"task_id":dependency_id,"source_ref":format!("delegated-task://{dependency_id}/result#final_structured_output"),"structured_output":output}));
        }
        let worker_imports = task.get("import_worker_refs").and_then(Value::as_array).cloned().unwrap_or_default();
        let mut resolved_worker_refs = Vec::new();
        for imported in worker_imports {
            let dependency_id = imported.as_str().unwrap_or_default();
            let predecessor = read_task(root, dependency_id)?;
            let refs = predecessor.pointer("/result/worker_refs").and_then(Value::as_array).cloned().unwrap_or_default();
            total_bytes = total_bytes.saturating_add(serde_json::to_vec(&refs).map(|bytes| bytes.len()).unwrap_or(MAX_IMPORTED_TASK_OUTPUT_BYTES + 1));
            if total_bytes > MAX_IMPORTED_TASK_OUTPUT_BYTES {
                return Err(error("imported_task_outputs_too_large", "imported_task_outputs_too_large"));
            }
            resolved_worker_refs.push(json!({"task_id":dependency_id,"source_ref":format!("delegated-task://{dependency_id}/result#worker_refs"),"worker_refs":refs}));
        }
        task["result"]["imported_task_outputs"] = json!(resolved);
        task["result"]["imported_worker_refs"] = json!(resolved_worker_refs);
        task["result"]["prior_step_outputs_ref"] = json!(format!("delegated-task://{id}/imported-task-outputs"));
        task["external_dependencies"] = json!({"status":"resolved","resolved_at":now(),"blocked_by":[]});
        append_event(root, &id, "task_dependencies_resolved", json!({"depends_on_task_ids":dependencies,"imported_task_ids":task["import_task_outputs"],"imported_worker_ref_task_ids":task["import_worker_refs"],"prior_step_outputs_ref":task["result"]["prior_step_outputs_ref"]}))?;
    }
    Ok(true)
}

fn imported_output_instruction(task: &Value) -> Option<String> {
    let imports = task.pointer("/result/imported_task_outputs").and_then(Value::as_array).filter(|items| !items.is_empty())?;
    let payload = serde_json::to_string(imports).ok()?;
    Some(format!("\n\nDECLARED PREDECESSOR OUTPUTS (the only cross-task context; consume these typed structured outputs, not predecessor transcripts):\n{payload}"))
}
#[cfg(test)]
fn task_run(args: &Map<String, Value>, root: &Path) -> Result<Value, Value> {
    task_run_with_roots(args, root, &[root.to_path_buf()])
}
fn task_run_with_roots(
    args: &Map<String, Value>,
    root: &Path,
    allowed_roots: &[PathBuf],
) -> Result<Value, Value> {
    let original_validation_ref = args.get("validated_request_ref").cloned();
    let resolved_args = materialize_validated_request(args, root)?;
    let args = &resolved_args;
    let id = stable_task_id(args);
    safe_id(&id)?;
    let _lock = lock_task(root, &id)?;
    if task_path(root, &id)?.is_file() {
        let mut task = read_task(root, &id)?;
        if args.get("objective").is_none() && args.get("intent").is_none() {
            assert_mutation_scope(&task, args, root)?;
            task = advance_value_with_roots(task, root, allowed_roots)?;
        } else if args.get("idempotency_key").is_some() {
            let fingerprint = request_fingerprint(args, root, &id);
            if task.get("request_fingerprint").and_then(Value::as_str) != Some(fingerprint.as_str())
            {
                return Err(
                    json!({"schema":"narada.delegated_task.error.v1","code":"delegated_task_idempotency_conflict","message":"delegated_task_idempotency_conflict","task_id":id,"existing_request_fingerprint":task.get("request_fingerprint"),"request_fingerprint":fingerprint}),
                );
            }
        }
        return Ok(
            json!({"schema":"narada.delegated_task.run.v1","status":"existing","request_status":"existing","execution_status":task["status"],"created":false,"task_id":id,"task_status":task["status"],"validated_request_ref":task.get("validated_request_ref"),"summary":task_summary_value(&task)}),
        );
    }
    let objective = objective(args)?;
    let admission = validate_with_options(args, root, false)?;
    if admission.get("status").and_then(Value::as_str) != Some("ok") {
        return Err(
            json!({"schema":"narada.delegated_task.error.v1","code":"delegated_task_validation_failed","message":"delegated_task_validation_failed","diagnostics":admission["diagnostics"]}),
        );
    }
    let created = now();
    let created_ms = now_ms();
    let workflow = normalize_workflow(args.get("workflow"));
    let step_states = initial_step_states(&workflow);
    let site = current_site_id(root);
    let fingerprint = request_fingerprint(args, root, &id);
    let dependencies = args.get("depends_on_task_ids").cloned().unwrap_or_else(||json!([]));
    let imports = args.get("import_task_outputs").cloned().unwrap_or_else(||json!([]));
    let worker_imports = args.get("import_worker_refs").cloned().unwrap_or_else(||json!([]));
    let result = json!({"schema":"narada.delegated_task.handoff.v1","output_contract_verdict":"pending","objective_verdict":"pending","acceptance_verdict":"pending","step_states":step_states,"worker_refs":[],"worker_outputs":[],"imported_task_outputs":[],"prior_step_outputs_ref":null,"residual_risks":[],"observed_incoherencies":[],"verification":[],"changed_files":[]});
    let mut task = json!({"schema":"narada.delegated_task.task.v1","task_id":id,"owner_site_id":site,"owner_site_root":if site.is_some(){json!(root.to_string_lossy())}else{Value::Null},"created_by_site_id":site,"visibility_scope":if site.is_some(){"site"}else{"user_global"},"task_root_scope":"site_root","status":"accepted_for_execution","objective":objective,"request_fingerprint":fingerprint,"validated_request_ref":original_validation_ref,"created_at":created,"created_at_ms":created_ms,"started_at":null,"started_at_ms":null,"finished_at":null,"finished_at_ms":null,"duration_ms":null,"updated_at":created,"cancelled_at":null,"idempotency_key":args.get("idempotency_key"),"constraints":normalized_constraints(args.get("constraints")),"workflow":workflow,"execution":normalized_execution(args.get("execution")),"acceptance":args.get("acceptance").cloned().unwrap_or_else(||json!({})),"depends_on_task_ids":dependencies,"import_task_outputs":imports,"import_worker_refs":worker_imports,"external_dependencies":{"status":"pending","resolved_at":null,"blocked_by":[]},"result":result,"summary":null});
    write_task(root, &task)?;
    append_event(root, &id, "task_created", json!({"objective":objective,"depends_on_task_ids":task["depends_on_task_ids"],"import_task_outputs":task["import_task_outputs"]}))?;
    if task.pointer("/execution/start").and_then(Value::as_bool) != Some(false) {
        task = advance_value_with_roots(task, root, allowed_roots)?;
    }
    Ok(
        json!({"schema":"narada.delegated_task.run.v1","status":"accepted_for_execution","request_status":"accepted_for_execution","execution_status":task["status"],"created":true,"task_id":id,"task_status":task["status"],"validated_request_ref":task.get("validated_request_ref"),"summary":task_summary_value(&task)}),
    )
}
#[cfg(test)]
fn task_advance(args: &Map<String, Value>, root: &Path) -> Result<Value, Value> {
    task_advance_with_roots(args, root, &[root.to_path_buf()])
}
fn task_advance_with_roots(
    args: &Map<String, Value>,
    root: &Path,
    allowed_roots: &[PathBuf],
) -> Result<Value, Value> {
    let id = task_id(args)?;
    let _lock = lock_task(root, &id)?;
    let current = read_task(root, &id)?;
    assert_mutation_scope(&current, args, root)?;
    let task = advance_value_with_roots(current, root, allowed_roots)?;
    Ok(
        json!({"schema":"narada.delegated_task.advance.v1","status":"ok","task_id":id,"task_status":task["status"],"task":compact_task(&task, root)}),
    )
}
fn step_status<'a>(task: &'a Value, id: &str) -> Option<&'a str> {
    task.pointer(&format!("/result/step_states/{id}/status"))
        .and_then(Value::as_str)
}
fn parse_condition_call(value: &str) -> Option<(&str, Vec<&str>)> {
    let open = value.find('(')?;
    if !value.ends_with(')') {
        return None;
    }
    let name = &value[..open];
    let body = &value[open + 1..value.len() - 1];
    let mut depth = 0;
    let mut start = 0;
    let mut args = Vec::new();
    for (index, ch) in body.char_indices() {
        match ch {
            '(' => depth += 1,
            ')' => depth -= 1,
            ',' if depth == 0 => {
                args.push(body[start..index].trim());
                start = index + 1
            }
            _ => {}
        }
    }
    args.push(body[start..].trim());
    Some((name, args))
}
