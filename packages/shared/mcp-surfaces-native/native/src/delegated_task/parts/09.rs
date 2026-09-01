fn stable_task_id(args: &Map<String, Value>) -> String {
    if let Some(id) = args.get("task_id").and_then(Value::as_str) {
        return id.to_string();
    }
    // A caller-supplied idempotency key is the durable operation identity.
    // Validation references identify payload records and must not fork retries.
    if let Some(key) = args.get("idempotency_key").and_then(Value::as_str) {
        let digest = Sha256::digest(key.as_bytes());
        let prefix = digest[..8]
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        return format!("task_{prefix}");
    }
    if let Some(reference) = args.get("validated_request_ref").and_then(Value::as_str) {
        let digest = Sha256::digest(reference.as_bytes());
        let prefix = digest[..8]
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        return format!("task_{prefix}");
    }
    format!("task_{}", uuid::Uuid::new_v4().simple())
}
fn canonicalize(value: &Value) -> Value {
    match value {
        Value::Array(items) => Value::Array(items.iter().map(canonicalize).collect()),
        Value::Object(object) => {
            let mut keys = object.keys().collect::<Vec<_>>();
            keys.sort();
            let mut out = Map::new();
            for key in keys {
                out.insert(key.clone(), canonicalize(&object[key]));
            }
            Value::Object(out)
        }
        _ => value.clone(),
    }
}
fn sha256_json(value: &Value) -> String {
    let digest = Sha256::digest(
        serde_json::to_string(&canonicalize(value))
            .unwrap_or_default()
            .as_bytes(),
    );
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}
fn normalized_execution(value: Option<&Value>) -> Value {
    let input = value.and_then(Value::as_object);
    let wait = input
        .and_then(|v| v.get("wait_for_completion"))
        .and_then(Value::as_bool)
        == Some(true);
    json!({"start":input.and_then(|v|v.get("start")).and_then(Value::as_bool)!=Some(false),"wait_for_completion":wait,"timeout_ms":input.and_then(|v|v.get("timeout_ms")).and_then(Value::as_u64).unwrap_or(if wait{30000}else{0}).min(600000),"poll_ms":input.and_then(|v|v.get("poll_ms")).and_then(Value::as_u64).unwrap_or(5000).clamp(50,30000),"resumable":input.and_then(|v|v.get("resumable")).and_then(Value::as_bool)!=Some(false),"exit_interview":input.and_then(|v|v.get("exit_interview")).and_then(Value::as_bool)==Some(true),"max_concurrency":input.and_then(|v|v.get("max_concurrency")).and_then(Value::as_u64).unwrap_or(10).clamp(1,32),"max_retries":input.and_then(|v|v.get("max_retries")).and_then(Value::as_u64).unwrap_or(0).min(10)})
}
fn normalized_constraints(value: Option<&Value>) -> Value {
    let mut constraints = value
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    let cognition = constraints
        .get("cognition")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty());
    if cognition.is_none() {
        constraints.insert("cognition".into(), json!(DEFAULT_COGNITION));
    }
    Value::Object(constraints)
}

fn merged_step_constraints(task: &Value, step: &Value) -> Value {
    let mut merged = normalized_constraints(task.get("constraints"));
    let Some(step_constraints) = step.get("constraints").and_then(Value::as_object) else {
        return merged;
    };
    let Some(target) = merged.as_object_mut() else {
        return merged;
    };
    for (key, value) in step_constraints {
        let preserve_task_preflight = key == "preflight_paths"
            && value.as_array().is_some_and(Vec::is_empty)
            && target
                .get(key)
                .and_then(Value::as_array)
                .is_some_and(|paths| !paths.is_empty());
        if !preserve_task_preflight {
            target.insert(key.clone(), value.clone());
        }
    }
    normalized_constraints(Some(&merged))
}
fn asynchronous_worker_constraints(task: &Value, step: &Value) -> Value {
    let mut constraints = merged_step_constraints(task, step);
    if let Some(constraints) = constraints.as_object_mut() {
        constraints.insert("wait_for_completion".into(), json!(false));
        constraints.remove("wait_timeout_ms");
    }
    constraints
}
fn worker_status_args(run_id: &str) -> Map<String, Value> {
    json!({"run_id":run_id,"compact":false})
        .as_object()
        .cloned()
        .expect("worker status arguments are an object")
}
const CONSTRAINT_FIELDS: &[&str] = &[
    "authority",
    "cwd",
    "site_root",
    "provider",
    "profile",
    "cognition",
    "model",
    "sandbox",
    "runtime",
    "invocation_plan_ref",
    "skip_git_repo_check",
    "resumable",
    "wait_for_completion",
    "wait_timeout_ms",
    "max_run_ms",
    "queue_timeout_ms",
    "exit_interview",
    "max_concurrency",
    "max_retries",
    "repair_policy",
    "authority_gates",
    "required_mcp_tools",
    "preflight_paths",
    "overrides",
];
fn constraints_schema() -> Value {
    json!({
        "type":"object",
        "properties":{
            "authority":{"type":"string","enum":["read","write","command"]},
            "cwd":{"type":"string","minLength":1,"maxLength":4096},
            "site_root":{"type":"string","minLength":1,"maxLength":4096},
            "provider":{"type":"string","minLength":1,"maxLength":256},
            "profile":{"type":"string","minLength":1,"maxLength":256},
            "cognition":{"type":"string","enum":["low","medium","high"],"default":DEFAULT_COGNITION},
            "model":{"type":"string","minLength":1,"maxLength":256},
            "sandbox":{"type":"string","enum":["read-only","workspace-write","danger-full-access"]},
            "runtime":{"type":"string","minLength":1,"maxLength":256},
            "invocation_plan_ref":{"type":"string","minLength":6,"maxLength":512,"pattern":"^plan:[A-Za-z0-9._:-]+$"},
            "skip_git_repo_check":{"type":"boolean"},
            "resumable":{"type":"boolean"},
            "wait_for_completion":{"type":"boolean"},
            "wait_timeout_ms":{"type":"integer","minimum":1,"maximum":180000},
            "max_run_ms":{"type":"integer","minimum":1,"maximum":1800000},
            "queue_timeout_ms":{"type":"integer","minimum":1,"maximum":1800000,"default":300000,"description":"Maximum provider-admission wait; max_run_ms begins only after provider admission."},
            "exit_interview":{"type":"boolean"},
            "max_concurrency":{"type":"integer","minimum":1,"maximum":32},
            "max_retries":{"type":"integer","minimum":0,"maximum":10},
            "repair_policy":{"type":"object","properties":{"strategy":{"type":"string","enum":["retry_same_step","named_repair_step"]},"repair_step_id":{"type":"string","minLength":1,"maxLength":256},"require_review_after_repair":{"type":"boolean"}},"additionalProperties":false},
            "authority_gates":{"type":"object","properties":{"commit":{"type":"object","properties":{"operation":{"type":"string","enum":["commit","push"]},"mode":{"type":"string","enum":["disallowed","requires_explicit_authority","allowed"]},"reason":{"type":"string","maxLength":2048},"required_authority":{"type":"string","enum":["write","command"]}},"additionalProperties":false},"push":{"type":"object","properties":{"operation":{"type":"string","enum":["commit","push"]},"mode":{"type":"string","enum":["disallowed","requires_explicit_authority","allowed"]},"reason":{"type":"string","maxLength":2048},"required_authority":{"type":"string","enum":["write","command"]}},"additionalProperties":false}},"additionalProperties":false},
            "required_mcp_tools":{"type":"array","maxItems":64,"items":{"type":"string","minLength":1,"maxLength":256}},
            "preflight_paths":{"type":"array","maxItems":64,"items":{"type":"object","properties":{"path":{"type":"string","minLength":1,"maxLength":4096},"access":{"type":"string","enum":["read","write","create"]},"label":{"type":"string","maxLength":256}},"required":["path","access"],"additionalProperties":false}},
            "overrides":{"type":"object","properties":{"runtime":{"type":"string","minLength":1,"maxLength":256},"sandbox":{"type":"string","enum":["read-only","workspace-write","danger-full-access"]},"model":{"type":"string","minLength":1,"maxLength":256},"reasoning_effort":{"type":"string","minLength":1,"maxLength":64},"config":{"type":"object","additionalProperties":{"oneOf":[{"type":"string"},{"type":"number"},{"type":"boolean"}]}},"skip_git_repo_check":{"type":"boolean"}},"additionalProperties":false}
        },
        "additionalProperties":false
    })
}
fn constraint_diagnostics(value: Option<&Value>, locus: &str) -> Vec<Value> {
    let Some(value) = value else { return Vec::new(); };
    let Some(object) = value.as_object() else {
        return vec![json!({"severity":"error","code":"constraints_must_be_object","locus":locus})];
    };
    let mut diagnostics = Vec::new();
    for key in object.keys() {
        if !CONSTRAINT_FIELDS.contains(&key.as_str()) {
            diagnostics.push(json!({"severity":"error","code":"unknown_constraint","locus":locus,"field":key}));
        }
    }
    if let Some(cognition) = object.get("cognition") {
        if !matches!(cognition.as_str(), Some("low" | "medium" | "high")) {
            diagnostics.push(json!({"severity":"error","code":"constraint_cognition_invalid","locus":format!("{locus}.cognition")}));
        }
    }
    if let Some(paths) = object.get("preflight_paths") {
        match paths.as_array() {
            Some(items) => for (index, item) in items.iter().enumerate() {
                let item_locus = format!("{locus}.preflight_paths[{index}]");
                let Some(path) = item.as_object() else {
                    diagnostics.push(json!({"severity":"error","code":"constraint_preflight_path_must_be_object","locus":item_locus}));
                    continue;
                };
                if path.get("path").and_then(Value::as_str).is_none_or(|value| value.trim().is_empty()) {
                    diagnostics.push(json!({"severity":"error","code":"constraint_preflight_path_requires_path","locus":item_locus}));
                }
                if !matches!(path.get("access").and_then(Value::as_str), Some("read" | "write" | "create")) {
                    diagnostics.push(json!({"severity":"error","code":"constraint_preflight_path_access_invalid","locus":item_locus}));
                }
            },
            None => diagnostics.push(json!({"severity":"error","code":"constraints_preflight_paths_must_be_array","locus":format!("{locus}.preflight_paths")})),
        }
    }
    if let Some(tools) = object.get("required_mcp_tools") {
        match tools.as_array() {
            Some(items) => for (index, item) in items.iter().enumerate() {
                if item.as_str().is_none_or(|value| value.trim().is_empty()) {
                    diagnostics.push(json!({"severity":"error","code":"constraint_required_mcp_tool_invalid","locus":format!("{locus}.required_mcp_tools[{index}]" )}));
                }
            },
            None => diagnostics.push(json!({"severity":"error","code":"constraints_required_mcp_tools_must_be_array","locus":format!("{locus}.required_mcp_tools")})),
        }
    }
    if let Some(overrides) = object.get("overrides") {
        if let Some(overrides) = overrides.as_object() {
            for key in overrides.keys() {
                if !["runtime", "sandbox", "model", "reasoning_effort", "config", "skip_git_repo_check"].contains(&key.as_str()) {
                    diagnostics.push(json!({"severity":"error","code":"unknown_constraint_override","locus":format!("{locus}.overrides"),"field":key}));
                }
            }
        } else {
            diagnostics.push(json!({"severity":"error","code":"constraints_overrides_must_be_object","locus":format!("{locus}.overrides")}));
        }
    }
    diagnostics
}
fn normalize_persisted_constraints(task: &mut Value) -> bool {
    let mut changed = false;
    let normalized = normalized_constraints(task.get("constraints"));
    if task.get("constraints") != Some(&normalized) {
        task["constraints"] = normalized;
        changed = true;
    }
    if let Some(steps) = task.pointer_mut("/workflow/steps").and_then(Value::as_array_mut) {
        for step in steps {
            if step.get("constraints").is_some() {
                let normalized = normalized_constraints(step.get("constraints"));
                if step.get("constraints") != Some(&normalized) {
                    step["constraints"] = normalized;
                    changed = true;
                }
            }
        }
    }
    changed
}
fn request_fingerprint(args: &Map<String, Value>, root: &Path, id: &str) -> String {
    let mut material = Map::new();
    material.insert("objective".into(),json!({"objective":objective(args).unwrap_or_default(),"instructions":args.get("intent").and_then(|v|v.get("instructions")).cloned().unwrap_or(Value::Null),"behavior":args.get("intent").and_then(|v|v.get("behavior")).cloned().unwrap_or(Value::Null),"mode":args.get("intent").and_then(|v|v.get("mode")).cloned().unwrap_or(Value::Null)}));
    material.insert(
        "constraints".into(),
        normalized_constraints(args.get("constraints")),
    );
    for key in ["workflow", "acceptance", "result_policy"] {
        if let Some(value) = args.get(key) {
            material.insert(key.into(), value.clone());
        }
    }
    material.insert(
        "execution".into(),
        normalized_execution(args.get("execution")),
    );
    let binding = args.get("execution_binding").and_then(Value::as_object);
    material.insert("execution_binding".into(),json!({"workspace_root":binding.and_then(|v|v.get("workspace_root")).cloned().unwrap_or_else(||json!(root.to_string_lossy())),"executor_kind":binding.and_then(|v|v.get("executor_kind")).cloned().unwrap_or_else(||json!("delegated_task")),"executor_profile":binding.and_then(|v|v.get("executor_profile")).cloned().unwrap_or(Value::Null),"executor_id":binding.and_then(|v|v.get("executor_id")).cloned().unwrap_or(Value::Null),"repository_root":binding.and_then(|v|v.get("repository_root")).cloned().unwrap_or(Value::Null),"site_root":binding.and_then(|v|v.get("site_root")).cloned().unwrap_or_else(||json!(root.to_string_lossy())),"correlation_key":binding.and_then(|v|v.get("correlation_key")).cloned().unwrap_or_else(||json!(args.get("idempotency_key").and_then(Value::as_str).unwrap_or(id)))}));
    material.insert("external_dependencies".into(),json!({"depends_on_task_ids":args.get("depends_on_task_ids").cloned().unwrap_or_else(||json!([])),"import_task_outputs":args.get("import_task_outputs").cloned().unwrap_or_else(||json!([])),"import_worker_refs":args.get("import_worker_refs").cloned().unwrap_or_else(||json!([])),"source_task_ref":args.get("source_task_ref").cloned().unwrap_or_else(||json!({}))}));
    sha256_json(&Value::Object(material))
}

fn string_array(value: Option<&Value>) -> Option<Vec<String>> {
    value?.as_array().map(|items| {
        items.iter().filter_map(Value::as_str).map(str::to_string).collect()
    })
}

