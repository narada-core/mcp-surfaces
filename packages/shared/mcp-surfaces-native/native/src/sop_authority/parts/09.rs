fn validate_steps(value: Option<&Value>, owner_sop_id: Option<&str>) -> Result<Value, Value> {
    let raw = value
        .and_then(Value::as_array)
        .ok_or_else(|| diagnostic("sop_requires_array", "sop_requires_array", json!({})))?;
    if raw.is_empty() || raw.len() > MAX_STEPS {
        return Err(diagnostic(
            "sop_step_count_invalid",
            "sop_step_count_invalid",
            json!({"count":raw.len(),"min":1,"max":MAX_STEPS}),
        ));
    }
    let mut ids = HashSet::new();
    let mut normalized = Vec::with_capacity(raw.len());
    for (index, value) in raw.iter().enumerate() {
        let step = value.as_object().ok_or_else(|| {
            diagnostic(
                "sop_array_entry_must_be_object",
                "sop_array_entry_must_be_object",
                json!({"index":index}),
            )
        })?;
        let id = required_string(step.get("id"), "sop_step_requires_id", 128)?;
        if !valid_step_id(&id) {
            return Err(diagnostic(
                "sop_step_id_invalid",
                &format!("sop_step_id_invalid:{id}"),
                json!({}),
            ));
        }
        if !ids.insert(id.clone()) {
            return Err(diagnostic(
                "sop_duplicate_step_id",
                &format!("sop_duplicate_step_id:{id}"),
                json!({}),
            ));
        }
        let executor = required_string(step.get("executor"), "sop_step_requires_executor", 32)?;
        if !matches!(
            executor.as_str(),
            "engine" | "agent" | "operator" | "sop" | "action"
        ) {
            return Err(diagnostic(
                "sop_invalid_executor",
                &format!("sop_invalid_executor:{executor}"),
                json!({"step_id":id,"allowed":["engine","agent","operator","sop","action"]}),
            ));
        }
        for legacy in ["command", "args", "timeout_ms", "cwd"] {
            if step.contains_key(legacy) {
                return Err(diagnostic(
                    "sop_effect_must_be_governed_action",
                    &format!("sop_effect_must_be_governed_action:{id}"),
                    json!({"step_id":id,"legacy_field":legacy,"remediation":"Use executor=action with an owning MCP surface/tool and idempotency_key_argument."}),
                ));
            }
        }
        let blocking = matches!(executor.as_str(), "agent" | "operator");
        if let Some(declared) = step.get("blocking") {
            if declared.as_bool() != Some(blocking) {
                return Err(diagnostic(
                    "sop_blocking_semantics_fixed",
                    &format!("sop_blocking_semantics_fixed:{id}"),
                    json!({"executor":executor,"required_blocking":blocking}),
                ));
            }
        }
        let child_sop_id = optional_string(step.get("sop_id"));
        let child_version = match step.get("sop_version") {
            None | Some(Value::Null) => None,
            Some(value) => value.as_i64(),
        };
        let wait_policy = optional_string(step.get("wait_policy"))
            .or_else(|| (executor == "sop").then(|| "wait".to_string()));
        if executor == "sop" {
            let child = child_sop_id.as_ref().ok_or_else(|| {
                diagnostic(
                    "sop_step_requires_child_sop_id",
                    &format!("sop_step_requires_child_sop_id:{id}"),
                    json!({"step_id":id}),
                )
            })?;
            if owner_sop_id == Some(child.as_str()) {
                return Err(diagnostic(
                    "sop_recursive_child_definition",
                    &format!("sop_recursive_child_definition:{child}"),
                    json!({"step_id":id}),
                ));
            }
            if wait_policy.as_deref() != Some("wait") {
                return Err(diagnostic(
                    "sop_invalid_wait_policy",
                    "sop_invalid_wait_policy",
                    json!({"step_id":id,"allowed":["wait"]}),
                ));
            }
            if step.contains_key("sop_version")
                && child_version.map(|version| version < 1).unwrap_or(true)
            {
                return Err(diagnostic(
                    "sop_invalid_child_sop_version",
                    "sop_invalid_child_sop_version",
                    json!({"step_id":id}),
                ));
            }
        } else if child_sop_id.is_some()
            || child_version.is_some()
            || step.contains_key("wait_policy")
        {
            return Err(diagnostic(
                "sop_child_fields_require_sop_executor",
                &format!("sop_child_fields_require_sop_executor:{id}"),
                json!({}),
            ));
        }
        let when = normalize_condition(step.get("when"), 0, &mut 0)?;
        let input = optional_value(step.get("input"), &format!("steps.{id}.input"))?;
        let input_ref = optional_value(step.get("input_ref"), &format!("steps.{id}.input_ref"))?;
        let result_schema = optional_schema(
            step.get("result_schema"),
            &format!("steps.{id}.result_schema"),
        )?;
        let action = normalize_action(step.get("action"), &id)?;
        if executor == "action" && action.is_none() {
            return Err(diagnostic(
                "sop_action_binding_required",
                &format!("sop_action_binding_required:{id}"),
                json!({}),
            ));
        }
        if executor != "action" && action.is_some() {
            return Err(diagnostic(
                "sop_action_binding_requires_action_executor",
                &format!("sop_action_binding_requires_action_executor:{id}"),
                json!({}),
            ));
        }
        normalized.push(json!({
            "id":id,
            "executor":executor,
            "blocking":blocking,
            "title":required_string(step.get("title"),"sop_step_requires_title",512)?,
            "depends_on":string_list(step.get("depends_on"))?,
            "instructions":required_string(step.get("instructions"),"sop_step_requires_instructions",16*1024)?,
            "when":when,
            "input":input,
            "input_ref":input_ref,
            "result_schema":result_schema,
            "action":action,
            "sop_id":child_sop_id,
            "sop_version":child_version,
            "wait_policy":wait_policy,
            "legacy_command":Value::Null,
        }));
    }
    validate_dag(&normalized)?;
    validate_step_references(&normalized)?;
    Ok(Value::Array(normalized))
}

fn validate_dag(steps: &[Value]) -> Result<(), Value> {
    let by_id = steps
        .iter()
        .filter_map(|step| Some((step.get("id")?.as_str()?.to_string(), step)))
        .collect::<HashMap<_, _>>();
    for step in steps {
        let id = step.get("id").and_then(Value::as_str).unwrap_or("");
        for dependency in step
            .get("depends_on")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
        {
            if !by_id.contains_key(dependency) {
                return Err(diagnostic(
                    "sop_unknown_dependency",
                    "sop_unknown_dependency",
                    json!({"step_id":id,"dependency":dependency}),
                ));
            }
            if dependency == id {
                return Err(diagnostic(
                    "sop_dependency_cycle",
                    "sop_dependency_cycle",
                    json!({"cycle":[id,id]}),
                ));
            }
        }
    }
    fn visit(
        id: &str,
        by_id: &HashMap<String, &Value>,
        visiting: &mut Vec<String>,
        visited: &mut HashSet<String>,
    ) -> Result<(), Vec<String>> {
        if visited.contains(id) {
            return Ok(());
        }
        if let Some(start) = visiting.iter().position(|value| value == id) {
            let mut cycle = visiting[start..].to_vec();
            cycle.push(id.to_string());
            return Err(cycle);
        }
        visiting.push(id.to_string());
        for dependency in by_id
            .get(id)
            .and_then(|step| step.get("depends_on"))
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
        {
            visit(dependency, by_id, visiting, visited)?;
        }
        visiting.pop();
        visited.insert(id.to_string());
        Ok(())
    }
    let mut visiting = Vec::new();
    let mut visited = HashSet::new();
    for id in by_id.keys() {
        if let Err(cycle) = visit(id, &by_id, &mut visiting, &mut visited) {
            return Err(diagnostic(
                "sop_dependency_cycle",
                "sop_dependency_cycle",
                json!({"cycle":cycle}),
            ));
        }
    }
    Ok(())
}

