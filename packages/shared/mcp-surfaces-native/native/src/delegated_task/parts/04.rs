fn validate(args: &Map<String, Value>, root: &Path) -> Result<Value, Value> {
    validate_with_options(args, root, true)
}
fn validate_with_options(
    args: &Map<String, Value>,
    root: &Path,
    persist_reference: bool,
) -> Result<Value, Value> {
    let objective = args
        .get("objective")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|v| !v.is_empty());
    let workflow = normalize_workflow(args.get("workflow"));
    let mut diagnostics = workflow_diagnostics(&workflow);
    diagnostics.extend(constraint_diagnostics(args.get("constraints"), "constraints"));
    if let Some(steps) = workflow.get("steps").and_then(Value::as_array) {
        for (index, step) in steps.iter().enumerate() {
            diagnostics.extend(constraint_diagnostics(
                step.get("constraints"),
                &format!("workflow.steps[{index}].constraints"),
            ));
        }
    }
    if objective.is_none() {
        diagnostics.push(json!({"severity":"error","code":"objective_required"}));
    }
    if let Some(binding) = args.get("execution_binding").and_then(Value::as_object) {
        if let Some(workspace) = binding.get("workspace_root").and_then(Value::as_str) {
            if !is_within(Path::new(workspace), root) {
                diagnostics.push(json!({"severity":"error","code":"execution_binding_workspace_outside_site_root"}));
            }
        }
    }
    diagnostics.extend(external_dependency_diagnostics(args, root));
    let preflight_requested = args
        .get("constraints")
        .and_then(Value::as_object)
        .and_then(|constraints| constraints.get("preflight_paths"))
        .and_then(Value::as_array)
        .is_some_and(|paths| !paths.is_empty())
        || workflow
            .get("steps")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .any(|step| {
                step.get("constraints")
                    .and_then(Value::as_object)
                    .and_then(|constraints| constraints.get("preflight_paths"))
                    .and_then(Value::as_array)
                    .is_some_and(|paths| !paths.is_empty())
            });
    let errors = diagnostics
        .iter()
        .filter_map(|item| item.get("code").and_then(Value::as_str))
        .collect::<Vec<_>>();
    let valid = errors.is_empty();
    let mut response = json!({
        "schema":"narada.delegated_task.validate.v1",
        "status":if diagnostics.is_empty(){"ok"}else{"rejected"},
        "dry_run":true,
        "validation_persisted":false,
        "diagnostics":diagnostics,
        "valid":valid,
        "request_valid":valid,
        "execution_preflight_pending":preflight_requested && valid,
        "task_root":task_root(root).to_string_lossy(),
        "errors":errors,
        "objective":objective,
        "resolved_constraints":normalized_constraints(args.get("constraints")),
        "worker_execution":"not_run",
        "preflight_status":if preflight_requested{"deferred"}else{"not_requested"},
        "preflight_authority":if preflight_requested{"worker-delegation.worker_run"}else{"none"},
        "preflight_remediation":if preflight_requested{"worker_run enforces path existence and scope immediately before launch; validation does not inspect the filesystem"}else{"No preflight paths were requested."}
    });
    if valid && persist_reference {
        let request = Value::Object(args.clone());
        let digest = sha256_json(&request);
        let reference = format!("vr_{}", &digest[..32]);
        let record = json!({
            "schema":"narada.delegated_task.validated_request.v1",
            "validated_request_ref":reference,
            "created_at":now(),
            "site_root":root.to_string_lossy(),
            "owner_site_id":current_site_id(root),
            "request":request,
            "request_digest":digest
            ,"preflight_status":if preflight_requested{"deferred"}else{"not_requested"}
        });
        write_validated_request(root, &record)?;
        response["validated_request_ref"] = json!(reference);
        response["request_digest"] = json!(digest);
        response["validation_persisted"] = json!(true);
    }
    Ok(response)
}
fn is_within(path: &Path, root: &Path) -> bool {
    let p = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    let r = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    p == r || p.starts_with(&r)
}

fn tasks_list(args: &Map<String, Value>, root: &Path) -> Result<Value, Value> {
    let limit = args
        .get("limit")
        .and_then(Value::as_u64)
        .unwrap_or(20)
        .clamp(1, MAX_ITEMS as u64) as usize;
    let mut records = Vec::new();
    if let Ok(entries) = fs::read_dir(tasks_dir(root)) {
        for entry in entries.filter_map(Result::ok).take(MAX_ITEMS) {
            if !entry.path().is_dir() {
                continue;
            }
            let Some(id) = entry.file_name().to_str().map(ToOwned::to_owned) else {
                continue;
            };
            if let Ok(task) = read_task(root, &id) {
                records.push(task);
            }
        }
    }
    let view = args
        .get("view")
        .and_then(Value::as_str)
        .unwrap_or("active_queue");
    let site_scope = args
        .get("site_scope")
        .and_then(Value::as_str)
        .unwrap_or("current_site");
    let current = current_site_id(root);
    let owner_filter = args.get("owner_site_id").and_then(Value::as_str);
    let include_ack = args.get("include_acknowledged").and_then(Value::as_bool) == Some(true);
    let legacy = args.contains_key("include_terminal") || args.contains_key("include_active");
    let include_terminal = args.get("include_terminal").and_then(Value::as_bool) == Some(true);
    let include_active = args.get("include_active").and_then(Value::as_bool) != Some(false);
    records.retain(|task| {
        let projected = ownership(task);
        let owner = projected
            .get("owner_site_id")
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        if owner_filter.is_some_and(|expected| expected != owner) {
            return false;
        }
        if site_scope == "current_site" && current.as_deref().is_some_and(|site| site != owner) {
            return false;
        }
        if site_scope == "user_global"
            && !matches!(
                projected.get("visibility_scope").and_then(Value::as_str),
                Some("user_global" | "user_global_legacy")
            )
        {
            return false;
        }
        let terminal = matches!(
            task.get("status").and_then(Value::as_str),
            Some("completed" | "failed" | "cancelled")
        );
        let acknowledged = task
            .pointer("/result/lifecycle_acknowledgement/acknowledged")
            .and_then(Value::as_bool)
            == Some(true);
        if legacy {
            return (if terminal {
                include_terminal
            } else {
                include_active
            }) && (include_ack || !acknowledged);
        }
        match view {
            "all" => include_ack || !acknowledged,
            "active_queue" => !terminal,
            "operator_inbox" => terminal && !acknowledged,
            "history" => terminal && (include_ack || !acknowledged),
            "acknowledged_archive" => terminal && acknowledged,
            _ => !terminal,
        }
    });
    records.sort_by(|a, b| {
        b.get("updated_at")
            .and_then(Value::as_str)
            .cmp(&a.get("updated_at").and_then(Value::as_str))
    });
    let total = records.len();
    records.truncate(limit);
    let tasks = records
        .iter()
        .map(|task| compact_task(task, root))
        .collect::<Vec<_>>();
    Ok(
        json!({"schema":"narada.delegated_task.list.v1","status":"ok","view":view,"site_scope":site_scope,"current_site_id":current,"owner_site_id":owner_filter,"count":tasks.len(),"total_scoped_count":total,"limit":limit,"include_active":include_active,"include_terminal":include_terminal,"include_acknowledged":include_ack,"tasks":tasks}),
    )
}
fn concise_value(value: &Value) -> String {
    let text = match value {
        Value::String(value) => value.clone(),
        Value::Null => "null".into(),
        Value::Bool(value) => value.to_string(),
        Value::Number(value) => value.to_string(),
        Value::Array(values) => format!("[{} items]", values.len()),
        Value::Object(values) => format!("{{{} fields}}", values.len()),
    };
    truncate_summary(&text, 160)
}
fn structured_output_summary(value: &Value) -> String {
    if let Some(summary) = value
        .get("summary")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|summary| !summary.is_empty())
    {
        return truncate_summary(summary, 512);
    }
    match value {
        Value::Object(fields) => {
            let parts = fields
                .iter()
                .take(4)
                .map(|(key, value)| format!("{key}={}", concise_value(value)))
                .collect::<Vec<_>>();
            if parts.is_empty() {
                format!("{} fields", fields.len())
            } else {
                parts.join(", ")
            }
        }
        Value::Array(values) => format!("{} items", values.len()),
        _ => concise_value(value),
    }
}
fn diagnostics_prefix(text: &str) -> Option<String> {
    let trimmed = text.trim();
    let boundary = trimmed
        .find("```")
        .or_else(|| trimmed.char_indices().find_map(|(index, character)| {
            matches!(character, '{' | '[').then_some(index)
        }))?;
    if boundary == 0 {
        return None;
    }
    let prefix = trimmed[..boundary].trim();
    (!prefix.is_empty()).then(|| prefix.chars().take(2000).collect())
}
