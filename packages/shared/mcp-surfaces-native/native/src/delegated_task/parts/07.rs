fn markdown_field_value(line: &str, field: &str) -> Option<Value> {
    let mut candidate = line.trim();
    for prefix in ["- ", "* ", "+ ", "• "] {
        if let Some(rest) = candidate.strip_prefix(prefix) {
            candidate = rest.trim_start();
            break;
        }
    }
    if let Some(rest) = candidate.strip_prefix("**") {
        let end = rest.find("**")?;
        if rest[..end].trim() != field {
            return None;
        }
        candidate = &rest[end + 2..];
    } else if let Some(rest) = candidate.strip_prefix('`') {
        let end = rest.find('`')?;
        if rest[..end].trim() != field {
            return None;
        }
        candidate = &rest[end + 1..];
    } else {
        candidate = candidate.strip_prefix(field)?;
    }
    let separator = candidate.chars().next()?;
    if !matches!(separator, ':' | '=') {
        return None;
    }
    let value = candidate[separator.len_utf8()..]
        .trim()
        .trim_matches('`')
        .trim_matches('*')
        .trim();
    if value.is_empty() {
        return None;
    }
    serde_json::from_str(value)
        .ok()
        .filter(|parsed: &Value| !parsed.is_object() && !parsed.is_array())
        .or_else(|| Some(Value::String(value.to_string())))
}

fn parse_markdown_structured_output(text: &str, required_fields: &[String]) -> Option<Value> {
    if required_fields.is_empty() {
        return None;
    }
    let mut fields = Map::new();
    for field in required_fields {
        let value = text
            .lines()
            .flat_map(|line| std::iter::once(line).chain(line.split(", ")))
            .find_map(|line| markdown_field_value(line, field))?;
        fields.insert(field.clone(), value);
    }
    Some(Value::Object(fields))
}

fn worker_output_from_run_with_required_fields(
    run: &Value,
    required_fields: &[String],
) -> Option<Value> {
    let summary = run
        .get("summary")
        .or_else(|| run.get("summary_preview"))
        .filter(|value| !value.is_null())?;
    match summary {
        Value::String(text) => {
            let bounded = text.len() <= MAX_WORKER_OUTPUT_BYTES;
            if bounded {
                if let Some(structured) = parse_embedded_structured_output(text) {
                    let encoded_len = serde_json::to_vec(&structured)
                        .map(|bytes| bytes.len())
                        .unwrap_or(MAX_WORKER_OUTPUT_BYTES + 1);
                    if encoded_len <= MAX_WORKER_OUTPUT_BYTES {
                        return Some(json!({"summary_text":structured_output_summary(&structured),"diagnostics_text":diagnostics_prefix(text),"structured_output":structured,"truncated":false}));
                    }
                }
                if let Some(structured) = parse_markdown_structured_output(text, required_fields) {
                    return Some(json!({"summary_text":structured_output_summary(&structured),"diagnostics_text":diagnostics_prefix(text),"raw_summary_text":text.chars().take(MAX_WORKER_OUTPUT_BYTES).collect::<String>(),"structured_output":structured,"structured_output_normalization":"markdown_summary","truncated":false}));
                }
            }
            Some(if required_fields.is_empty() {
                json!({"summary_text":text.chars().take(MAX_WORKER_OUTPUT_BYTES).collect::<String>(),"truncated":!bounded || text.chars().count()>MAX_WORKER_OUTPUT_BYTES})
            } else {
                json!({"summary_text":text.chars().take(MAX_WORKER_OUTPUT_BYTES).collect::<String>(),"structured_output_required":true,"structured_output_error":{"code":"worker_structured_output_required","required_fields":required_fields},"truncated":!bounded || text.chars().count()>MAX_WORKER_OUTPUT_BYTES})
            })
        }
        value => {
            let encoded_len = serde_json::to_vec(value)
                .map(|bytes| bytes.len())
                .unwrap_or(MAX_WORKER_OUTPUT_BYTES + 1);
            (encoded_len <= MAX_WORKER_OUTPUT_BYTES)
                .then(|| json!({"summary_text":structured_output_summary(value),"structured_output":value,"truncated":false}))
        }
    }
}

fn structured_output_contract_failed(output: Option<&Value>, required_fields: &[String]) -> bool {
    !required_fields.is_empty()
        && output.is_none_or(|value| value.get("structured_output_required") == Some(&json!(true)))
}

fn record_worker_terminal(
    task: &mut Value,
    step_id: &str,
    run_id: &str,
    status: &str,
    run: &Value,
) {
    let required_fields = required_contract_fields(task, Some(step_id));
    let runtime_terminal_confirmed = run.get("terminal_event").and_then(Value::as_bool) == Some(true)
        || run.get("completion_state").and_then(Value::as_str) == Some("complete")
        || run.get("phase").and_then(Value::as_str) == Some("completed");
    let runtime_terminal_missing = run.get("phase").is_some() && !runtime_terminal_confirmed;
    let output = if runtime_terminal_missing {
        Some(json!({
            "summary_text":Value::Null,
            "worker_runtime_incomplete":true,
            "structured_output_error":{"code":"worker_runtime_incomplete_output","message":"terminal worker event was not observed"},
            "truncated":false
        }))
    } else {
        worker_output_from_run_with_required_fields(run, &required_fields)
    }.or_else(|| {
        (!required_fields.is_empty()).then(|| {
            json!({
                "summary_text":Value::Null,
                "structured_output_required":true,
                "structured_output_error":{"code":"worker_structured_output_required","required_fields":required_fields},
                "truncated":false
            })
        })
    });
    let contract_failed = structured_output_contract_failed(output.as_ref(), &required_fields);
    let effective_status = if contract_failed || runtime_terminal_missing { "failed" } else { status };
    if let Some(refs) = task["result"]["worker_refs"].as_array_mut() {
        if let Some(reference) = refs.iter_mut().find(|reference| {
            reference.get("run_id").and_then(Value::as_str) == Some(run_id)
        }) {
            reference["status"] = json!(effective_status);
            reference["finished_at"] = json!(now());
            if let Some(duration_ms) = run.get("duration_ms").or_else(|| run.pointer("/timing/duration_ms")) {
                reference["duration_ms"] = duration_ms.clone();
            }
            if let Some(value) = output.clone() {
                reference["output"] = value;
            }
            if let Some(error) = run.get("error").filter(|value| !value.is_null()) {
                reference["error"] = error.clone();
            }
        }
    }
    let step_state = &mut task["result"]["step_states"][step_id];
    step_state["worker_status"] = json!(effective_status);
    step_state["worker_output_contract"] = json!(if contract_failed { "failed" } else { "passed" });
    if let Some(value) = output.clone() {
        step_state["worker_output"] = value;
    }
    if !task["result"].get("worker_outputs").is_some_and(Value::is_array) {
        task["result"]["worker_outputs"] = json!([]);
    }
    if let Some(outputs) = task["result"]["worker_outputs"].as_array_mut() {
        outputs.retain(|value| value.get("run_id").and_then(Value::as_str) != Some(run_id));
        outputs.push(json!({"step_id":step_id,"run_id":run_id,"status":effective_status,"output":output,"error":run.get("error")}));
    }
    refresh_task_summary(task);
}
#[cfg(test)]
fn task_status(args: &Map<String, Value>, root: &Path) -> Result<Value, Value> {
    task_status_with_roots(args, root, &[root.to_path_buf()])
}
fn task_status_with_roots(
    args: &Map<String, Value>,
    root: &Path,
    allowed_roots: &[PathBuf],
) -> Result<Value, Value> {
    let id = task_id(args)?;
    let current = read_task(root, &id)?;
    let task = if !task_is_terminal(&current) {
        // Worker and dependency transitions are lifecycle-owned projections.
        // Ordinary status must reconcile every nonterminal task; otherwise an
        // independent task can remain durably "running" after its worker has
        // completed or become orphaned.
        advance_task_closure(root, &id, allowed_roots, &mut std::collections::BTreeSet::new())?
    } else {
        current
    };
    let obj = task.as_object().cloned().unwrap_or_default();
    Ok(
        json!({"schema":"narada.delegated_task.status.v1","status":"ok","task_id":id,"task_status":obj.get("status"),"objective":obj.get("objective"),"ownership":ownership(&task),"execution_binding":obj.get("execution_binding"),"request_fingerprint":obj.get("request_fingerprint"),"created_at":obj.get("created_at"),"updated_at":obj.get("updated_at"),"timing":timing_projection(&task),"result":compact_task(&task, root)}),
    )
}
fn task_is_terminal(task: &Value) -> bool {
    matches!(
        task.get("status").and_then(Value::as_str),
        Some("completed" | "failed" | "cancelled")
    )
}
fn task_result(args: &Map<String, Value>, root: &Path) -> Result<Value, Value> {
    let id = task_id(args)?;
    let task = read_task(root, &id)?;
    let terminal = task_is_terminal(&task);
    let result = task.get("result").cloned().unwrap_or_else(|| json!({}));
    let (derived_verdict, _) = acceptance_verdict(&task, root);
    let acceptance_checks = acceptance_checks_or_derive(&task, root, task.get("result").and_then(Value::as_object));
    let output_contract = output_contract_verdict(&task);
    let objective_verdict_value = objective_verdict(&task).0;
    let final_projection = final_step_projection(&task);
    Ok(
        json!({"schema":"narada.delegated_task.result.v1","status":"ok","task_id":id,"task_status":task.get("status"),"result":result,"summary":task_summary_value(&task),"output_contract_verdict":output_contract,"objective_verdict":objective_verdict_value,"acceptance_verdict":task.pointer("/result/acceptance_verdict").cloned().unwrap_or_else(||json!(derived_verdict)),"acceptance_checks":acceptance_checks,"final_step":final_projection.get("final_step"),"final_structured_output":final_projection.get("final_structured_output"),"prior_step_outputs_ref":prior_outputs_ref(&task, &final_projection),"canonical_terminal_handoff":terminal,"canonical_readback_tool":"delegated_task_wait","readback_role":"secondary_durable_readback"}),
    )
}
fn task_summary(args: &Map<String, Value>, root: &Path) -> Result<Value, Value> {
    let id = task_id(args)?;
    let task = read_task(root, &id)?;
    let result = task
        .get("result")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    let terminal = task_is_terminal(&task);
    let (derived_verdict, _) = acceptance_verdict(&task, root);
    let acceptance_checks = acceptance_checks_or_derive(&task, root, Some(&result));
    let output_contract = output_contract_verdict(&task);
    let objective_verdict_value = objective_verdict(&task).0;
    let final_projection = final_step_projection(&task);
    Ok(
        json!({"schema":"narada.delegated_task.summary.v1","status":"ok","task_id":id,"task_status":task.get("status"),"objective":task.get("objective"),"summary":task_summary_value(&task),"output_contract_verdict":output_contract,"objective_verdict":objective_verdict_value,"acceptance_verdict":result.get("acceptance_verdict").cloned().unwrap_or_else(||json!(derived_verdict)),"acceptance_checks":acceptance_checks,"final_step":final_projection.get("final_step"),"final_structured_output":final_projection.get("final_structured_output"),"prior_step_outputs_ref":prior_outputs_ref(&task, &final_projection),"residual_risks":result.get("residual_risks").cloned().unwrap_or_else(||json!([])),"progress":result.get("progress"),"timing":timing_projection(&task),"canonical_terminal_handoff":terminal,"canonical_readback_tool":"delegated_task_wait"}),
    )
}
fn terminal_handoff(task: &Value, root: &Path) -> Value {
    let result = task.get("result").and_then(Value::as_object);
    let (derived_verdict, _) = acceptance_verdict(task, root);
    let output_contract = output_contract_verdict(task);
    let objective_verdict_value = objective_verdict(task).0;
    let final_projection = final_step_projection(task);
    let task_id = task.get("task_id").and_then(Value::as_str).unwrap_or("unknown");
    json!({
        "task_id":task.get("task_id"),
        "task_status":task.get("status"),
        "summary":task_summary_value(task),
        "output_contract_verdict":output_contract,
        "objective_verdict":objective_verdict_value,
        "acceptance_verdict":result.and_then(|value| value.get("acceptance_verdict")).cloned().unwrap_or_else(||json!(derived_verdict)),
        "acceptance_checks":acceptance_checks_or_derive(task, root, result),
        "final_step":final_projection.get("final_step"),
        "final_structured_output":final_projection.get("final_structured_output"),
        "prior_step_outputs_ref":prior_outputs_ref(task, &final_projection),
        "created_at":task.get("created_at"),
        "started_at":task.get("started_at"),
        "finished_at":task.get("finished_at"),
        "duration_ms":task.get("duration_ms"),
        "timing":timing_projection(task),
        "details_ref":format!("delegated-task://{task_id}/result"),
        "details_tool":"delegated_task_result"
    })
}
fn task_execute(args: &Map<String, Value>, root: &Path, allowed_roots: &[PathBuf]) -> Result<Value, Value> {
    let validation = validate(args, root)?;
    if validation.get("request_valid").and_then(Value::as_bool) != Some(true) {
        let mut failure = error("delegated_task_validation_failed", "delegated_task_validation_failed");
        failure["validation"] = validation;
        return Err(failure);
    }
    let validated_request_ref = validation.get("validated_request_ref").cloned()
        .ok_or_else(|| error("validated_request_ref_missing", "validated_request_ref_missing"))?;
    let mut run_args = Map::new();
    run_args.insert("validated_request_ref".into(), validated_request_ref);
    run_args.insert("idempotency_key".into(), args.get("idempotency_key").cloned().unwrap_or(Value::Null));
    let run = task_run_with_roots(&run_args, root, allowed_roots)?;
    let task_id = run.get("task_id").cloned().ok_or_else(|| error("task_id_required", "task_id_required"))?;
    let mut wait_args = Map::new();
    wait_args.insert("task_id".into(), task_id);
    for field in ["timeout_ms", "poll_ms"] {
        if let Some(value) = args.get(field) { wait_args.insert(field.into(), value.clone()); }
    }
    let wait = task_wait_with_roots(&wait_args, root, allowed_roots)?;
    let idempotency_replay = run.get("created").and_then(Value::as_bool) == Some(false);
    Ok(json!({"schema":"narada.delegated_task.execute.v1","status":wait.get("status"),"idempotency_replay":idempotency_replay,"validation":validation,"run":{"task_id":run.get("task_id"),"created":run.get("created")},"terminal":wait}))
}

