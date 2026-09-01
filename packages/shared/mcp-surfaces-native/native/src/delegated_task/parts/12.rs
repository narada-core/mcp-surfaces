fn acceptance_verdict(task: &Value, root: &Path) -> (&'static str, Vec<Value>) {
    let mut checks = Vec::new();
    let result = task.get("result").cloned().unwrap_or_else(|| json!({}));
    let result_text = result.to_string();
    let objective_present = task
        .get("objective")
        .and_then(Value::as_str)
        .is_some_and(|value| !value.trim().is_empty());
    checks.push(json!({
        "kind":"objective_present",
        "status":if objective_present {"passed"} else {"failed"}
    }));
    let owner_site_id = task.get("owner_site_id").and_then(Value::as_str);
    let owner_site_root = task.get("owner_site_root").and_then(Value::as_str);
    let provenance_status = match (owner_site_id, owner_site_root) {
        (Some(site), Some(site_root)) if !site.trim().is_empty() && !site_root.trim().is_empty() => {
            if is_within(Path::new(site_root), root) { "passed" } else { "failed" }
        }
        _ => "not_applicable",
    };
    checks.push(json!({
        "kind":"site_provenance",
        "owner_site_id":owner_site_id,
        "owner_site_root":owner_site_root,
        "status":provenance_status
    }));
    let requested_fields = acceptance_required_fields(task);
    let mut returned_fields = Vec::new();
    for list_name in ["worker_outputs", "worker_refs"] {
        if let Some(items) = result.get(list_name).and_then(Value::as_array) {
            for item in items {
                if let Some(fields) = item
                    .pointer("/output/structured_output")
                    .or_else(|| item.pointer("/structured_output"))
                    .and_then(Value::as_object)
                {
                    for field in fields.keys() {
                        if !returned_fields.contains(field) {
                            returned_fields.push(field.clone());
                        }
                    }
                }
            }
        }
    }
    if requested_fields.is_empty() {
        checks.push(json!({"kind":"requested_fields","requested":[],"returned":returned_fields,"missing":[],"status":"not_applicable"}));
    } else {
        let missing = requested_fields
            .iter()
            .filter(|field| !returned_fields.contains(field))
            .cloned()
            .collect::<Vec<_>>();
        checks.push(json!({"kind":"requested_fields","requested":requested_fields,"returned":returned_fields,"missing":missing,"status":if missing.is_empty(){"passed"}else{"failed"}}));
    }
    let changed_files = result
        .get("changed_files")
        .and_then(Value::as_array)
        .map(Vec::len)
        .unwrap_or(0);
    let mut changes_made = false;
    for list_name in ["worker_outputs", "worker_refs"] {
        if let Some(items) = result.get(list_name).and_then(Value::as_array) {
            changes_made |= items.iter().any(|item| {
                let output = item
                    .pointer("/output/structured_output")
                    .or_else(|| item.pointer("/structured_output"));
                output.is_some_and(|value| {
                    value.pointer("/verification/changes_made").and_then(Value::as_bool) == Some(true)
                        || value.get("changes_made").and_then(Value::as_bool) == Some(true)
                        || value.pointer("/verification/target_state_changed").and_then(Value::as_bool) == Some(true)
                })
            });
        }
    }
    let authority = task
        .pointer("/constraints/authority")
        .and_then(Value::as_str)
        .unwrap_or("read");
    checks.push(json!({
        "kind":"no_write",
        "authority":authority,
        "changed_files":changed_files,
        "changes_made":changes_made,
        "status":if authority == "read" {if changed_files == 0 && !changes_made {"passed"} else {"failed"}} else {"not_applicable"}
    }));
    if task.pointer("/acceptance/strict_clean_run").and_then(Value::as_bool) == Some(true) {
        let terminal = task_is_terminal(task);
        let states = result.get("step_states").and_then(Value::as_object);
        let clean = states.is_some_and(|states| !states.is_empty() && states.values().all(|state| {
            state.get("attempts").and_then(Value::as_u64).unwrap_or(0) <= 1
                && state.get("error").is_none_or(Value::is_null)
                && matches!(state.get("status").and_then(Value::as_str), Some("completed" | "skipped" | "noted"))
        }));
        checks.push(json!({"kind":"strict_clean_run","requested":true,"attempts_at_most_one":clean,"no_step_errors":clean,"status":if !terminal {"pending"} else if clean {"passed"} else {"failed"}}));
    }
    for item in task
        .pointer("/acceptance/required_files")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let target = item
            .as_str()
            .or_else(|| item.get("path").and_then(Value::as_str))
            .unwrap_or_default();
        let path = root.join(target);
        let mut passed = !target.is_empty() && is_within(&path, root) && path.exists();
        if passed {
            if let Some(needle) = item.get("contains").and_then(Value::as_str) {
                passed = fs::read_to_string(&path).is_ok_and(|text| text.contains(needle));
            }
        }
        checks.push(json!({"kind":"required_file","target":target,"status":if passed{"passed"}else{"failed"}}));
    }
    for (field, kind) in [
        ("required_tests", "required_test"),
        ("focused_tests", "focused_test"),
    ] {
        for item in task
            .pointer(&format!("/acceptance/{field}"))
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let target = item
                .as_str()
                .or_else(|| item.get("command").and_then(Value::as_str))
                .unwrap_or_default();
            let required = item
                .get("status")
                .and_then(Value::as_str)
                .unwrap_or("passed");
            let matched = result
                .pointer("/verification")
                .and_then(Value::as_array)
                .is_some_and(|records| {
                    records.iter().any(|record| {
                        record.to_string().contains(target)
                            && record
                                .get("status")
                                .and_then(Value::as_str)
                                .is_some_and(|status| status.contains(required))
                    })
                });
            checks.push(json!({"kind":kind,"target":target,"required_status":required,"status":if matched{"passed"}else{"pending"}}));
        }
    }
    for item in task
        .pointer("/acceptance/required_tools")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let target = item
            .as_str()
            .or_else(|| item.get("name").and_then(Value::as_str))
            .unwrap_or_default();
        checks.push(json!({"kind":"required_tool","target":target,"status":if result_text.contains(target){"passed"}else{"pending"}}));
    }
    for item in task
        .pointer("/acceptance/forbidden_patterns")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let target = item
            .as_str()
            .or_else(|| item.get("pattern").and_then(Value::as_str))
            .unwrap_or_default();
        checks.push(json!({"kind":"forbidden_pattern","target":target,"status":if !target.is_empty()&&result_text.contains(target){"failed"}else{"passed"}}));
    }
    if let Some(budget) = task
        .pointer("/acceptance/verification_budget")
        .and_then(Value::as_object)
    {
        let count = result
            .pointer("/verification")
            .and_then(Value::as_array)
            .map(Vec::len)
            .unwrap_or(0) as u64;
        let attempts = budget
            .get("max_attempts")
            .and_then(Value::as_u64)
            .unwrap_or(u64::MAX);
        let commands = budget
            .get("max_commands")
            .and_then(Value::as_u64)
            .unwrap_or(u64::MAX);
        checks.push(json!({"kind":"verification_budget","verification_count":count,"max_attempts":attempts,"max_commands":commands,"status":if count<=attempts&&count<=commands{"passed"}else{"failed"}}));
    }
    if let Some(quorum) = task
        .pointer("/acceptance/review_quorum")
        .and_then(Value::as_object)
    {
        let states = task
            .pointer("/result/step_states")
            .and_then(Value::as_object);
        let passed = states
            .map(|states| {
                states
                    .values()
                    .filter(|state| {
                        state.get("kind").and_then(Value::as_str) == Some("review")
                            && state.get("status").and_then(Value::as_str) == Some("completed")
                    })
                    .count()
            })
            .unwrap_or(0) as u64;
        let failed = states
            .map(|states| {
                states
                    .values()
                    .filter(|state| {
                        state.get("kind").and_then(Value::as_str) == Some("review")
                            && state.get("status").and_then(Value::as_str) == Some("failed")
                    })
                    .count()
            })
            .unwrap_or(0) as u64;
        let running = states
            .map(|states| {
                states
                    .values()
                    .filter(|state| {
                        state.get("kind").and_then(Value::as_str) == Some("review")
                            && state.get("status").and_then(Value::as_str) == Some("running")
                    })
                    .count()
            })
            .unwrap_or(0) as u64;
        let min = quorum
            .get("min_passed")
            .and_then(Value::as_u64)
            .unwrap_or(1);
        let max = quorum
            .get("max_failed")
            .and_then(Value::as_u64)
            .unwrap_or(0);
        let status = if passed == 0 && failed == 0 && running == 0 {
            "pending"
        } else if passed >= min && failed <= max {
            "passed"
        } else if running > 0 {
            "pending"
        } else {
            "failed"
        };
        checks.push(json!({"kind":"review_quorum","min_passed":min,"max_failed":max,"passed":passed,"failed":failed,"status":status}));
    }
    if task
        .pointer("/acceptance/residual_risk_policy")
        .and_then(Value::as_str)
        == Some("none_allowed")
    {
        let count = task
            .pointer("/result/residual_risks")
            .and_then(Value::as_array)
            .map(Vec::len)
            .unwrap_or(0);
        checks.push(json!({"kind":"residual_risk_policy","status":if count==0{"passed"}else{"failed"},"risk_count":count}));
    }
    let output_contract = output_contract_verdict(task);
    checks.push(json!({
        "kind":"output_contract",
        "verdict":output_contract,
        "status":output_contract
    }));
    if let Some(check) = assessment_consistency_check(task) {
        checks.push(check);
    }
    let (objective, signal) = objective_verdict(task);
    checks.push(json!({
        "kind":"objective_outcome",
        "verdict":objective,
        "signal":signal,
        "status":objective
    }));
    let verdict = if output_contract == "failed"
        || checks.iter().any(|check| check.get("status").and_then(Value::as_str) == Some("failed"))
    {
        "failed"
    } else if objective == "failed" {
        "failed"
    } else if objective == "blocked" {
        "blocked"
    } else if output_contract == "pending"
        || checks.iter().any(|check| check.get("status").and_then(Value::as_str) == Some("pending"))
    {
        "pending"
    } else {
        "passed"
    };
    (verdict, checks)
}
