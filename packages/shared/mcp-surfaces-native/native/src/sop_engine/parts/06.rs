fn normalize_persisted_step(value: &Value) -> Result<Value, Value> {
    let object = value.as_object().ok_or_else(|| {
        diagnostic(
            "sop_persisted_step_invalid",
            "sop_persisted_step_invalid",
            json!({}),
        )
    })?;
    let executor = object
        .get("executor")
        .and_then(Value::as_str)
        .or_else(|| match object.get("kind").and_then(Value::as_str) {
            Some("manual") => Some("operator"),
            Some("note") => Some("engine"),
            value => value,
        })
        .unwrap_or_default();
    if !matches!(executor, "engine" | "agent" | "operator" | "sop" | "action") {
        return Err(diagnostic(
            "sop_persisted_step_executor_invalid",
            &format!("sop_persisted_step_executor_invalid:{executor}"),
            json!({"executor":executor,"step_id":object.get("id")}),
        ));
    }
    let id = required_string(object.get("id"), "sop_step_requires_id", 128)?;
    let title = required_string(object.get("title"), "sop_step_requires_title", 512)?;
    let instructions = required_string(
        object.get("instructions"),
        "sop_step_requires_instructions",
        16 * 1024,
    )?;
    let depends_on = object
        .get("depends_on")
        .and_then(Value::as_array)
        .map(|values| {
            values
                .iter()
                .filter_map(Value::as_str)
                .map(ToString::to_string)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let sop_version = object.get("sop_version").and_then(Value::as_i64);
    Ok(json!({
        "id":id,"executor":executor,"blocking":matches!(executor,"agent"|"operator"),
        "title":title,"depends_on":depends_on,"instructions":instructions,
        "when":object.get("when").cloned().unwrap_or(Value::Null),
        "input":object.get("input").cloned().unwrap_or(Value::Null),
        "input_ref":object.get("input_ref").cloned().unwrap_or(Value::Null),
        "result_schema":object.get("result_schema").cloned().unwrap_or(Value::Null),
        "action":object.get("action").cloned().unwrap_or(Value::Null),
        "sop_id":object.get("sop_id").cloned().unwrap_or(Value::Null),
        "sop_version":sop_version,"wait_policy":object.get("wait_policy").cloned().unwrap_or_else(||if executor=="sop"{json!("wait")}else{Value::Null}),
        "legacy_command":object.get("command").cloned().unwrap_or(Value::Null)
    }))
}

fn executable_definition(template: &Value) -> Value {
    json!({
        "schema":"narada.sop.definition.v2",
        "sop_id":template.get("sop_id"),"version":template.get("version"),
        "title":template.get("title"),"steps":template.get("steps"),
        "input_schema":template.get("input_schema"),"output":template.get("output"),
        "output_ref":template.get("output_ref"),"output_schema":template.get("output_schema"),
        "acceptance_criteria":template.get("acceptance_criteria"),
        "evidence_requirements":template.get("evidence_requirements")
    })
}

fn initialize_step_states(db: &Connection, template: &Value) -> Result<Vec<Value>, Value> {
    let steps = template
        .get("steps")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            diagnostic(
                "sop_definition_steps_invalid",
                "sop_definition_steps_invalid",
                json!({}),
            )
        })?;
    let mut output = Vec::with_capacity(steps.len());
    for step in steps {
        let executor = step
            .get("executor")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let mut pinned_version = step.get("sop_version").and_then(Value::as_i64);
        let mut pinned_fingerprint = None;
        if executor == "sop" {
            let child_sop_id =
                required_string(step.get("sop_id"), "sop_step_requires_child_sop_id", 256)?;
            let version = match pinned_version {
                Some(version) => version,
                None => latest_runnable_template_version(db, &child_sop_id)?,
            };
            let child = template_by_version(db, &child_sop_id, version)?;
            assert_no_legacy_effects(&child)?;
            pinned_version = Some(version);
            pinned_fingerprint = Some(fingerprint(&executable_definition(&child)));
        }
        output.push(json!({
            "step_id":step.get("id"),"executor":executor,"blocking":step.get("blocking"),
            "title":step.get("title"),"status":"pending","depends_on":step.get("depends_on"),
            "instructions":step.get("instructions"),"when":step.get("when"),
            "input":step.get("input"),"input_ref":step.get("input_ref"),
            "result_schema":step.get("result_schema"),"action":step.get("action"),
            "sop_id":step.get("sop_id"),"sop_version":pinned_version,
            "wait_policy":step.get("wait_policy"),
            "pinned_child_definition_fingerprint":pinned_fingerprint,
            "child_run_id":null,"action_id":null,"started_at":null,"completed_at":null,
            "result":{},"result_ref":null,"completion_key":null,
            "completion_fingerprint":null,"error_message":null
        }));
    }
    Ok(output)
}

fn assert_no_legacy_effects(template: &Value) -> Result<(), Value> {
    let legacy = template
        .get("steps")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|step| {
            step.get("legacy_command")
                .is_some_and(|value| !value.is_null())
        })
        .filter_map(|step| {
            step.get("id")
                .and_then(Value::as_str)
                .map(ToString::to_string)
        })
        .collect::<Vec<_>>();
    if legacy.is_empty() {
        return Ok(());
    }
    Err(diagnostic(
        "sop_legacy_command_unsupported",
        "sop_legacy_command_unsupported",
        json!({"step_ids":legacy,"remediation":"Replace each command step with a governed action step targeting the domain MCP surface that owns the effect."}),
    ))
}

fn assert_no_recursive_child(
    db: &Connection,
    parent_run_id: &str,
    child_sop_id: &str,
    child_version: i64,
) -> Result<(), Value> {
    let mut current = Some(parent_run_id.to_string());
    let mut seen = HashSet::new();
    let mut chain = Vec::new();
    while let Some(run_id) = current {
        if !seen.insert(run_id.clone()) {
            return Err(diagnostic(
                "sop_parent_chain_cycle",
                &format!("sop_parent_chain_cycle:{run_id}"),
                json!({}),
            ));
        }
        let run = get_run(db, &run_id)?;
        chain.push(json!({"run_id":run.run_id,"sop_id":run.sop_id,"sop_version":run.sop_version}));
        if run.sop_id == child_sop_id {
            return Err(diagnostic(
                "sop_recursive_child_occurrence",
                &format!("sop_recursive_child_occurrence:{child_sop_id}@v{child_version}"),
                json!({"ancestor_chain":chain}),
            ));
        }
        current = run.parent_run_id;
    }
    Ok(())
}

fn get_run(db: &Connection, run_id: &str) -> Result<Run, Value> {
    let row = db
        .query_row(
            "SELECT * FROM sop_runs WHERE run_id = ?",
            params![run_id],
            row_json,
        )
        .optional()
        .map_err(|error| diagnostic("sop_run_query_failed", &error.to_string(), json!({})))?
        .ok_or_else(|| {
            diagnostic(
                "sop_run_not_found",
                &format!("sop_run_not_found:{run_id}"),
                json!({}),
            )
        })?;
    hydrate_run(row)
}

