fn final_step_projection(task: &Value) -> Value {
    let outputs = task
        .pointer("/result/worker_outputs")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let steps = task
        .pointer("/workflow/steps")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let terminal = |value: &Value| {
        matches!(
            value.get("status").and_then(Value::as_str),
            Some("completed" | "failed" | "cancelled" | "completed_with_errors")
        )
    };
    let mut selected: Option<(usize, Value)> = None;
    for prefer_review in [true, false] {
        for step in steps.iter().rev() {
            let Some(step_id) = step.get("id").and_then(Value::as_str) else {
                continue;
            };
            let is_review = step.get("kind").and_then(Value::as_str) == Some("review");
            if is_review != prefer_review {
                continue;
            }
            if let Some((index, output)) = outputs
                .iter()
                .enumerate()
                .rev()
                .find(|(_, output)| output.get("step_id").and_then(Value::as_str) == Some(step_id) && terminal(output))
            {
                selected = Some((index, output.clone()));
                break;
            }
        }
        if selected.is_some() {
            break;
        }
    }
    if selected.is_none() {
        selected = outputs
            .iter()
            .enumerate()
            .rev()
            .find(|(_, output)| terminal(output))
            .map(|(index, output)| (index, output.clone()));
    }
    let Some((index, output)) = selected else {
        return json!({"final_step":Value::Null,"final_structured_output":Value::Null,"final_summary":Value::Null,"prior_step_outputs_ref":Value::Null});
    };
    let task_id = task.get("task_id").and_then(Value::as_str).unwrap_or("unknown");
    let prior_ref = if index > 0 {
        json!(format!("delegated-task://{task_id}/prior-step-outputs"))
    } else {
        Value::Null
    };
    json!({
        "final_step":output.get("step_id"),
        "final_structured_output":output.pointer("/output/structured_output").cloned().unwrap_or(Value::Null),
        "final_summary":output.pointer("/output/summary_text").cloned().unwrap_or(Value::Null),
        "prior_step_outputs_ref":prior_ref
    })
}

fn derived_task_summary(task: &Value) -> Option<Value> {
    let projection = final_step_projection(task);
    let base = projection
        .get("final_structured_output")
        .filter(|value| !value.is_null())
        .map(structured_output_summary)
        .or_else(|| projection
        .get("final_summary")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(str::to_string));
    let (objective, _) = objective_verdict(task);
    if objective == "not_applicable" {
        return base.map(|text| json!(truncate_summary(&text, 512)));
    }
    let label = if is_executability_assessment(task) {
        "assessment_result"
    } else {
        "objective_result"
    };
    let body = base.unwrap_or_else(|| "No substantive objective result was reported.".to_string());
    Some(json!(truncate_summary(
        &format!("{label}: {objective}. {body}"),
        512,
    )))
}

fn truncate_summary(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    let prefix = text.chars().take(max_chars).collect::<String>();
    let boundary = prefix
        .char_indices()
        .rev()
        .find(|(_, ch)| ch.is_whitespace())
        .map(|(index, _)| index)
        .unwrap_or(prefix.len());
    format!("{}…", prefix[..boundary].trim_end())
}

fn timing_projection(task: &Value) -> Value {
    let created = task.get("created_at_ms").and_then(Value::as_i64);
    let started = task.get("started_at_ms").and_then(Value::as_i64);
    let finished = task.get("finished_at_ms").and_then(Value::as_i64);
    let queue_ms = created.zip(started).map(|(created, started)| started.saturating_sub(created));
    let worker_ms = task
        .pointer("/result/worker_refs")
        .and_then(Value::as_array)
        .map(|refs| refs.iter().filter_map(|reference| reference.get("duration_ms").and_then(Value::as_i64)).sum::<i64>());
    let active_ms = task.get("duration_ms").and_then(Value::as_i64);
    let orchestration_ms = active_ms.zip(worker_ms).map(|(active, worker)| active.saturating_sub(worker));
    let total_ms = created.zip(finished).map(|(created, finished)| finished.saturating_sub(created));
    json!({"queue_ms":queue_ms,"worker_ms":worker_ms,"orchestration_ms":orchestration_ms,"total_ms":total_ms})
}
fn task_summary_value(task: &Value) -> Option<Value> {
    derived_task_summary(task).or_else(|| {
        task.get("summary")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|summary| !summary.is_empty())
        .map(|summary| json!(summary))
    })
}
fn refresh_task_summary(task: &mut Value) {
    if let Some(summary) = derived_task_summary(task) {
        task["summary"] = summary;
    }
}
fn acceptance_checks_or_derive(
    task: &Value,
    root: &Path,
    result: Option<&Map<String, Value>>,
) -> Value {
    let (_, derived_checks) = acceptance_verdict(task, root);
    let derived_requested_fields = derived_checks
        .iter()
        .find(|check| check["kind"] == "requested_fields")
        .filter(|check| {
            check["requested"]
                .as_array()
                .is_some_and(|fields| !fields.is_empty())
        });
    let derived_outcome_checks = derived_checks
        .iter()
        .filter(|check| {
            matches!(
                check["kind"].as_str(),
                Some("output_contract" | "objective_outcome" | "assessment_consistency")
            )
        })
        .cloned()
        .collect::<Vec<_>>();
    if let Some(checks) = result.and_then(|value| value.get("acceptance_checks")) {
        if checks.as_array().is_some_and(|items| !items.is_empty()) {
            let mut refreshed = checks.as_array().cloned().unwrap_or_default();
            if let Some(derived_requested_fields) = derived_requested_fields {
                if let Some(existing) = refreshed
                    .iter_mut()
                    .find(|check| check["kind"] == "requested_fields")
                {
                    *existing = derived_requested_fields.clone();
                } else {
                    refreshed.push(derived_requested_fields.clone());
                }
            }
            for derived in derived_outcome_checks {
                if let Some(existing) = refreshed
                    .iter_mut()
                    .find(|check| check["kind"] == derived["kind"])
                {
                    *existing = derived;
                } else {
                    refreshed.push(derived);
                }
            }
            return json!(refreshed);
        }
    }
    json!(derived_checks)
}

fn compact_task(task: &Value, root: &Path) -> Value {
    let obj = task.as_object().cloned().unwrap_or_default();
    let result = obj.get("result").and_then(Value::as_object);
    let (derived_verdict, _) = acceptance_verdict(task, root);
    let acceptance_checks = acceptance_checks_or_derive(task, root, result);
    let output_contract = output_contract_verdict(task);
    let objective_verdict_value = objective_verdict(task).0;
    let final_projection = final_step_projection(task);
    let has_final_output = final_projection
        .get("final_structured_output")
        .is_some_and(|value| !value.is_null());
    let prior_ref = final_projection.get("prior_step_outputs_ref").filter(|value| !value.is_null()).or_else(|| result.and_then(|value| value.get("prior_step_outputs_ref")));
    json!({"task_id":obj.get("task_id"),"task_status":obj.get("status"),"objective":obj.get("objective"),"owner_site_id":obj.get("owner_site_id"),"created_by_site_id":obj.get("created_by_site_id"),"visibility_scope":obj.get("visibility_scope"),"updated_at":obj.get("updated_at"),"summary":task_summary_value(task),"output_contract_verdict":output_contract,"objective_verdict":objective_verdict_value,"acceptance_verdict":result.and_then(|v|v.get("acceptance_verdict")).cloned().unwrap_or_else(||json!(derived_verdict)),"acceptance_checks":acceptance_checks,"final_step":final_projection.get("final_step"),"final_structured_output":final_projection.get("final_structured_output"),"prior_step_outputs_ref":prior_ref,"depends_on_task_ids":obj.get("depends_on_task_ids"),"import_task_outputs":obj.get("import_task_outputs"),"external_dependencies":obj.get("external_dependencies"),"imported_task_outputs":result.and_then(|value| value.get("imported_task_outputs")),"execution_binding":obj.get("execution_binding"),"timing":timing_projection(task),"worker_refs":if has_final_output{None}else{result.and_then(|v|v.get("worker_refs"))},"worker_outputs":if has_final_output{None}else{result.and_then(|v|v.get("worker_outputs"))}})
}

fn prior_outputs_ref<'a>(task: &'a Value, projection: &'a Value) -> Option<&'a Value> {
    projection.get("prior_step_outputs_ref").filter(|value| !value.is_null())
        .or_else(|| task.pointer("/result/prior_step_outputs_ref").filter(|value| !value.is_null()))
}

fn parse_embedded_structured_output(text: &str) -> Option<Value> {
    if let Ok(value) = serde_json::from_str::<Value>(text) {
        return Some(value);
    }
    for block in text.split("```").skip(1).step_by(2) {
        let body = block.trim();
        let body = body
            .strip_prefix("json")
            .or_else(|| body.strip_prefix("JSON"))
            .unwrap_or(body)
            .trim();
        if let Ok(value) = serde_json::from_str::<Value>(body) {
            return Some(value);
        }
    }
    text.char_indices()
        .filter(|(_, character)| matches!(character, '{' | '['))
        .find_map(|(start, _)| {
            let candidate = text[start..].trim();
            let candidate = candidate
                .strip_suffix("```")
                .map(str::trim)
                .unwrap_or(candidate);
            serde_json::from_str::<Value>(candidate).ok().or_else(|| {
                let mut stream = serde_json::Deserializer::from_str(candidate).into_iter::<Value>();
                stream.next().and_then(Result::ok)
            })
        })
}

fn required_field_names(value: Option<&Value>) -> Vec<String> {
    let mut fields = Vec::new();
    if let Some(items) = value.and_then(Value::as_array) {
        for field in items.iter().filter_map(|item| {
            item.as_str()
                .or_else(|| item.get("name").and_then(Value::as_str))
        }) {
            if !fields.iter().any(|known| known == field) {
                fields.push(field.to_string());
            }
        }
    }
    fields
}

fn acceptance_required_fields(task: &Value) -> Vec<String> {
    for path in [
        "/acceptance/required_fields",
        "/acceptance/requested_fields",
        "/acceptance/required",
    ] {
        let fields = required_field_names(task.pointer(path));
        if !fields.is_empty() {
            return fields;
        }
    }
    Vec::new()
}

