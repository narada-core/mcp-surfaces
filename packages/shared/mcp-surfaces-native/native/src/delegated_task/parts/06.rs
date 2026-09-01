fn required_contract_fields(task: &Value, step_id: Option<&str>) -> Vec<String> {
    let mut fields = acceptance_required_fields(task);
    if let Some(steps) = task.pointer("/workflow/steps").and_then(Value::as_array) {
        for step in steps {
            if step_id.is_some_and(|wanted| step.get("id").and_then(Value::as_str) != Some(wanted)) {
                continue;
            }
            for field in required_field_names(step.pointer("/output_schema/required")) {
                if !fields.iter().any(|known| known == &field) {
                    fields.push(field);
                }
            }
        }
    }
    fields
}

fn is_executability_assessment(task: &Value) -> bool {
    task.pointer("/workflow/steps")
        .and_then(Value::as_array)
        .is_some_and(|steps| {
            steps.iter().any(|step| {
                step.pointer("/output_schema/name").and_then(Value::as_str)
                    == Some("task_executability_assessment_v1")
                    || step.get("profile").and_then(Value::as_str)
                        == Some("shoshin-task-executability-v1")
            })
        })
}

fn assessment_consistency_check(task: &Value) -> Option<Value> {
    if !is_executability_assessment(task) {
        return None;
    }
    let projection = final_step_projection(task);
    let output = projection
        .get("final_structured_output")
        .and_then(Value::as_object)?;
    let assessment = output.get("assessment_result").and_then(Value::as_object)?;
    let assessment_status = assessment
        .get("status")
        .and_then(Value::as_str)
        .map(|value| value.trim().to_ascii_lowercase());
    let implementation_ready = assessment.get("implementation_ready").and_then(Value::as_bool);
    let blocker_count = assessment
        .get("blockers")
        .and_then(Value::as_array)
        .map(Vec::len)
        .unwrap_or(0);
    let blocking_decision_count = output
        .get("required_decisions")
        .and_then(Value::as_array)
        .map(|decisions| {
            decisions
                .iter()
                .filter(|decision| decision.get("blocking").and_then(Value::as_bool) == Some(true))
                .count()
        })
        .unwrap_or(0);
    let executable_status = matches!(
        assessment_status.as_deref(),
        Some("executable" | "passed" | "pass" | "ready" | "complete" | "completed" | "success")
    );
    let strict_blocked_status =
        matches!(assessment_status.as_deref(), Some("blocked" | "not_executable"));
    let explicit_reason_present = assessment
        .get("reason")
        .and_then(Value::as_str)
        .map(str::trim)
        .is_some_and(|reason| !reason.is_empty());
    let mut reasons = Vec::new();
    if assessment_status.is_none() {
        reasons.push("assessment_status_missing".to_string());
    } else if !executable_status
        && !matches!(
            assessment_status.as_deref(),
            Some("blocked" | "not_executable" | "failed" | "failure" | "undetermined" | "inconclusive" | "unavailable")
        )
    {
        reasons.push("assessment_status_unknown".to_string());
    }
    if executable_status && implementation_ready != Some(true) {
        reasons.push("executable_status_requires_implementation_ready_true".to_string());
    }
    if executable_status && blocker_count > 0 {
        reasons.push("executable_status_has_blockers".to_string());
    }
    if executable_status && blocking_decision_count > 0 {
        reasons.push("executable_status_has_blocking_required_decisions".to_string());
    }
    if strict_blocked_status && implementation_ready != Some(false) {
        reasons.push("blocked_status_requires_implementation_ready_false".to_string());
    }
    if assessment_status.as_deref() == Some("blocked") && blocker_count == 0 {
        reasons.push("blocked_status_requires_blockers".to_string());
    }
    if assessment_status.as_deref() == Some("not_executable") && !explicit_reason_present {
        reasons.push("not_executable_status_requires_reason".to_string());
    }
    Some(json!({
        "kind":"assessment_consistency",
        "status":if reasons.is_empty(){"passed"}else{"failed"},
        "verdict":if reasons.is_empty(){"consistent"}else{"inconsistent"},
        "assessment_status":assessment_status,
        "implementation_ready":implementation_ready,
        "blocker_count":blocker_count,
        "blocking_decision_count":blocking_decision_count,
        "reasons":reasons
    }))
}

fn assessment_consistency_failed(task: &Value) -> bool {
    assessment_consistency_check(task)
        .is_some_and(|check| check.get("status").and_then(Value::as_str) == Some("failed"))
}

fn objective_signal(task: &Value) -> Option<String> {
    let projection = final_step_projection(task);
    if projection.get("final_step").and_then(Value::as_str).is_some()
        && projection.get("final_structured_output").is_none_or(Value::is_null)
        && projection.get("final_summary").is_none_or(|value| {
            value.is_null() || value.as_str().is_none_or(|text| text.trim().is_empty())
        })
    {
        return Some("missing_terminal_result".to_string());
    }
    let output = projection.get("final_structured_output")?;
    let object = output.as_object()?;
    for key in ["objective_verdict", "assessment_result", "objective_status"] {
        let Some(value) = object.get(key) else {
            continue;
        };
        if let Some(text) = value.as_str().map(str::trim).filter(|text| !text.is_empty()) {
            return Some(text.to_ascii_lowercase());
        }
        if let Some(nested) = value.as_object() {
            if key == "assessment_result" {
                if assessment_consistency_failed(task) {
                    return Some("inconsistent".to_string());
                }
                if let Some(text) = nested
                    .get("status")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|text| !text.is_empty())
                {
                    return Some(text.to_ascii_lowercase());
                }
                if nested.get("implementation_ready").and_then(Value::as_bool) == Some(true)
                    && nested
                        .get("blockers")
                        .and_then(Value::as_array)
                        .is_some_and(Vec::is_empty)
                {
                    return Some("passed".to_string());
                }
            }
            for nested_key in ["verdict", "status", "result"] {
                if let Some(text) = nested
                    .get(nested_key)
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|text| !text.is_empty())
                {
                    return Some(text.to_ascii_lowercase());
                }
            }
        }
    }
    None
}

fn objective_verdict(task: &Value) -> (&'static str, Option<String>) {
    let signal = objective_signal(task);
    let verdict = match signal.as_deref() {
        Some("passed" | "pass" | "achieved" | "success" | "succeeded" | "completed" | "complete" | "coherent" | "executable" | "ready") => "passed",
        Some("pending" | "running") => "pending",
        Some("failed" | "failure" | "missing_terminal_result") => "failed",
        Some("blocked" | "not_executable" | "undetermined" | "inconclusive" | "unavailable" | "not_found") => "blocked",
        Some("inconsistent") => "blocked",
        Some(_) => "blocked",
        None if !task_is_terminal(task) => "pending",
        None if is_executability_assessment(task) => "blocked",
        None if task.get("status").and_then(Value::as_str) == Some("completed") => "passed",
        None if task.get("status").and_then(Value::as_str) == Some("failed") => "failed",
        None => "blocked",
    };
    (verdict, signal)
}

fn output_contract_verdict(task: &Value) -> &'static str {
    if task
        .pointer("/result/step_states")
        .and_then(Value::as_object)
        .is_some_and(|states| {
            states.values().any(|state| {
                state.get("worker_output_contract").and_then(Value::as_str) == Some("failed")
            })
        })
    {
        return "failed";
    }
    if assessment_consistency_failed(task) {
        return "failed";
    }
    let final_step = final_step_projection(task);
    let step_id = final_step.get("final_step").and_then(Value::as_str);
    let fields = required_contract_fields(task, step_id);
    if fields.is_empty() {
        return "not_applicable";
    }
    let Some(output) = final_step.get("final_structured_output").filter(|value| !value.is_null()) else {
        return "pending";
    };
    if fields.iter().all(|field| output.get(field).is_some()) {
        "passed"
    } else {
        "failed"
    }
}

fn set_outcome_verdicts(task: &mut Value, acceptance: &str) {
    let output_contract = output_contract_verdict(task);
    let objective = objective_verdict(task).0;
    task["result"]["output_contract_verdict"] = json!(output_contract);
    task["result"]["objective_verdict"] = json!(objective);
    task["result"]["acceptance_verdict"] = json!(acceptance);
}

fn structured_output_instruction(task: &Value) -> Option<String> {
    structured_output_instruction_for_step(task, None)
}

fn structured_output_instruction_for_step(
    task: &Value,
    step: Option<&Value>,
) -> Option<String> {
    let mut fields = acceptance_required_fields(task);
    for field in required_field_names(step.and_then(|value| value.pointer("/output_schema/required"))) {
        if !fields.iter().any(|known| known == &field) {
            fields.push(field);
        }
    }
    if fields.is_empty() {
        return None;
    }
    let assessment_contract = step
        .and_then(|value| value.pointer("/output_schema/name"))
        .and_then(Value::as_str)
        .filter(|name| *name == "task_executability_assessment_v1")
        .map(|_| {
            " EXECUTABILITY ASSESSMENT SUBSCHEMA: assessment_result MUST be an object, not a string, with status, implementation_ready, blockers, and (for not_executable) reason. Rules: executable => implementation_ready=true and blockers=[]; blocked => implementation_ready=false and blockers nonempty; not_executable => implementation_ready=false and reason nonempty.".to_string()
        })
        .unwrap_or_default();
    Some(format!(
        "\n\nMANDATORY TERMINAL OUTPUT CONTRACT: return exactly one JSON object with these required top-level keys: {}. The JSON object must be the entire final answer: no Markdown fence, preamble, narration, or trailing explanation. Complete every required field before returning.{}\nREAD-ONLY PROBE RULE: use supplied preflight evidence for path checks; if a probe is necessary, issue one executable with literal arguments and no shell operators, pipes, redirection, or generated scripts.",
        fields.join(", "),
        assessment_contract
    ))
}

