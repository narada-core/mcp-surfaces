fn hydrate_run(row: Value) -> Result<Run, Value> {
    let object = row
        .as_object()
        .ok_or_else(|| diagnostic("sop_run_corrupt", "sop_run_corrupt", json!({})))?;
    let run_id = required_string(object.get("run_id"), "sop_run_corrupt", 512)?;
    let sop_id = required_string(object.get("sop_id"), "sop_run_corrupt", 256)?;
    let sop_version = object
        .get("sop_version")
        .and_then(Value::as_i64)
        .filter(|value| *value >= 1)
        .ok_or_else(|| {
            diagnostic(
                "sop_run_corrupt",
                "sop_run_corrupt",
                json!({"run_id":run_id}),
            )
        })?;
    let status = required_string(object.get("status"), "sop_run_status_invalid", 64)?;
    if !RUN_STATUSES.contains(&status.as_str()) {
        return Err(diagnostic(
            "sop_run_status_invalid",
            &format!("sop_run_status_invalid:{status}"),
            json!({"run_id":run_id}),
        ));
    }
    let definition = object
        .get("definition_json")
        .cloned()
        .unwrap_or_else(|| json!({}));
    let step_states = object
        .get("step_states_json")
        .and_then(Value::as_array)
        .cloned()
        .ok_or_else(|| {
            diagnostic(
                "sop_run_corrupt",
                &format!("sop_run_corrupt:{run_id}"),
                json!({"reason":"step_states_json is not an array"}),
            )
        })?;
    validate_step_graph(&step_states)?;
    for step in &step_states {
        let step_status = step
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if !STEP_STATUSES.contains(&step_status) {
            return Err(diagnostic(
                "sop_persisted_step_status_invalid",
                &format!("sop_persisted_step_status_invalid:{step_status}"),
                json!({"step_id":step.get("step_id")}),
            ));
        }
        if !step.get("result").is_some_and(Value::is_object) {
            return Err(diagnostic(
                "sop_persisted_step_result_invalid",
                "sop_persisted_step_result_invalid",
                json!({"step_id":step.get("step_id")}),
            ));
        }
    }
    let occurrence_key = object
        .get("occurrence_key")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let request_fingerprint = object
        .get("request_fingerprint")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let definition_fingerprint = object
        .get("definition_fingerprint")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let input = object
        .get("input_json")
        .cloned()
        .unwrap_or_else(|| json!({}));
    let input_ref = normalize_value_ref(object.get("input_ref_json"), "sop_input_ref")?;
    let trigger_source_kind = object
        .get("trigger_source_kind")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let trigger_source_ref = object
        .get("trigger_source_ref")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let triggered_by = object
        .get("triggered_by")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let parent_run_id = optional_string(object.get("parent_run_id"));
    let parent_step_id = optional_string(object.get("parent_step_id"));
    if !definition_fingerprint.is_empty() {
        if definition.get("schema").and_then(Value::as_str) != Some("narada.sop.definition.v2")
            || definition.get("sop_id").and_then(Value::as_str) != Some(sop_id.as_str())
            || definition.get("version").and_then(Value::as_i64) != Some(sop_version)
        {
            return Err(diagnostic(
                "sop_definition_identity_mismatch",
                &format!("sop_definition_identity_mismatch:{run_id}"),
                json!({"run_id":run_id}),
            ));
        }
        let actual = fingerprint(&definition);
        if actual != definition_fingerprint {
            return Err(diagnostic(
                "sop_definition_fingerprint_mismatch",
                &format!("sop_definition_fingerprint_mismatch:{run_id}"),
                json!({"run_id":run_id,"expected":definition_fingerprint,"actual":actual}),
            ));
        }
    }
    if !request_fingerprint.is_empty() {
        let actual = fingerprint(&json!({
            "sop_id":sop_id,"sop_version":sop_version,"occurrence_key":occurrence_key,
            "input":input,"input_ref":input_ref,"trigger_source_kind":trigger_source_kind,
            "trigger_source_ref":trigger_source_ref,"triggered_by":triggered_by,
            "parent_run_id":parent_run_id,"parent_step_id":parent_step_id
        }));
        if actual != request_fingerprint {
            return Err(diagnostic(
                "sop_request_fingerprint_mismatch",
                &format!("sop_request_fingerprint_mismatch:{run_id}"),
                json!({"run_id":run_id,"expected":request_fingerprint,"actual":actual}),
            ));
        }
    }
    Ok(Run {
        run_id,
        sop_id,
        sop_version,
        sop_title: required_string(object.get("sop_title"), "sop_run_corrupt", 512)?,
        status,
        occurrence_key,
        request_fingerprint,
        definition_fingerprint,
        definition,
        input,
        input_ref,
        output: object
            .get("output_json")
            .cloned()
            .unwrap_or_else(|| json!({})),
        output_ref: normalize_value_ref(object.get("output_ref_json"), "sop_output_ref")?,
        step_states,
        trigger_source_kind,
        trigger_source_ref,
        triggered_by,
        parent_run_id,
        parent_step_id,
        created_at: required_string(object.get("created_at"), "sop_run_corrupt", 512)?,
        updated_at: required_string(object.get("updated_at"), "sop_run_corrupt", 512)?,
        completed_at: optional_string(object.get("completed_at")),
    })
}

fn run_result(run: &Run, admission: Option<&str>) -> Value {
    let next_steps = run
        .step_states
        .iter()
        .filter(|step| step.get("status").and_then(Value::as_str) == Some("running"))
        .map(|step| {
            let result = step.get("result").cloned().unwrap_or_else(|| json!({}));
            let instructions = result
                .get("instructions")
                .cloned()
                .unwrap_or_else(|| step.get("instructions").cloned().unwrap_or(Value::Null));
            let action_target = step.get("action").and_then(Value::as_object).map(|action| {
                json!({"surface_id":action.get("surface_id"),"tool_name":action.get("tool_name")})
            });
            json!({
                "step_id":step.get("step_id"),"executor":step.get("executor"),
                "title":step.get("title"),"instructions":instructions,
                "child_run_id":step.get("child_run_id"),"child_sop_id":step.get("sop_id"),
                "action_id":step.get("action_id"),"action_target":action_target,
                "result":result,"result_ref":step.get("result_ref")
            })
        })
        .collect::<Vec<_>>();
    let child_pins = run
        .step_states
        .iter()
        .filter(|step| step.get("executor").and_then(Value::as_str) == Some("sop"))
        .map(|step| {
            json!({
                "step_id":step.get("step_id"),"sop_id":step.get("sop_id"),
                "sop_version":step.get("sop_version"),
                "definition_fingerprint":step.get("pinned_child_definition_fingerprint")
            })
        })
        .collect::<Vec<_>>();
    json!({
        "schema":"narada.sop.run.v2","run_id":run.run_id,"sop_id":run.sop_id,
        "sop_version":run.sop_version,"sop_title":run.sop_title,"status":run.status,
        "occurrence_key":run.occurrence_key,"request_fingerprint":run.request_fingerprint,
        "definition_fingerprint":run.definition_fingerprint,"input":run.input,
        "input_ref":run.input_ref,"output":run.output,"output_ref":run.output_ref,
        "step_states":run.step_states,"step_states_parse_error":null,
        "trigger_source_kind":run.trigger_source_kind,"trigger_source_ref":run.trigger_source_ref,
        "triggered_by":run.triggered_by,"parent_run_id":run.parent_run_id,
        "parent_step_id":run.parent_step_id,"created_at":run.created_at,
        "updated_at":run.updated_at,"completed_at":run.completed_at,
        "definition_snapshot":{"stored":true,"fingerprint":run.definition_fingerprint,
            "sop_id":run.sop_id,"sop_version":run.sop_version,"child_pins":child_pins},
        "admission":admission,"next_awaits_confirmation":next_steps.iter().any(|step|matches!(step.get("executor").and_then(Value::as_str),Some("agent")|Some("operator"))),
        "next_steps":next_steps,"next_step":next_steps.first().cloned().unwrap_or(Value::Null),
        "relationship_reconciliation":{"mode":"automatic","repair_tool":"sop_run_refresh"}
    })
}

fn validate_step_graph(steps: &[Value]) -> Result<(), Value> {
    if steps.is_empty() || steps.len() > 128 {
        return Err(diagnostic(
            "sop_step_count_invalid",
            "sop_step_count_invalid",
            json!({"count":steps.len(),"min":1,"max":128}),
        ));
    }
    let mut ids = HashSet::new();
    for step in steps {
        let id = step
            .get("step_id")
            .or_else(|| step.get("id"))
            .and_then(Value::as_str)
            .unwrap_or_default();
        if id.is_empty() || !ids.insert(id.to_string()) {
            return Err(diagnostic(
                "sop_duplicate_step_id",
                "sop_duplicate_step_id",
                json!({"step_id":id}),
            ));
        }
    }
    for step in steps {
        let id = step
            .get("step_id")
            .or_else(|| step.get("id"))
            .and_then(Value::as_str)
            .unwrap_or_default();
        for dependency in step
            .get("depends_on")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
        {
            if !ids.contains(dependency) {
                return Err(diagnostic(
                    "sop_unknown_dependency",
                    "sop_unknown_dependency",
                    json!({"step_id":id,"dependency":dependency}),
                ));
            }
        }
    }
    Ok(())
}

