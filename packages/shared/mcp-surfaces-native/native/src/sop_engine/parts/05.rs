fn admit_run(
    db: &Connection,
    args: &Map<String, Value>,
    parent_run_id: Option<&str>,
    parent_step_id: Option<&str>,
) -> Result<(Run, &'static str), Value> {
    let sop_id = required_string(args.get("sop_id"), "sop_requires_sop_id", 256)?;
    let occurrence_key = required_string(
        args.get("occurrence_key"),
        "sop_requires_occurrence_key",
        512,
    )?;
    let triggered_by = required_string(args.get("triggered_by"), "sop_requires_triggered_by", 512)?;
    let trigger_source_kind = match args.get("trigger_source_kind") {
        None | Some(Value::Null) => "manual".to_string(),
        value => required_string(value, "sop_requires_trigger_source_kind", 128)?,
    };
    let trigger_source_ref = optional_string(args.get("trigger_source_ref")).unwrap_or_default();
    if trigger_source_ref.chars().count() > 2048 {
        return Err(diagnostic(
            "sop_trigger_source_ref_too_long",
            "sop_trigger_source_ref_too_long",
            json!({"max_length":2048}),
        ));
    }
    let input = args.get("input").cloned().unwrap_or_else(|| json!({}));
    assert_bound(&input, "sop_input", MAX_INLINE_VALUE_BYTES)?;
    if !input.is_object() {
        return Err(diagnostic(
            "sop_input_must_be_object",
            "sop_input_must_be_object",
            json!({}),
        ));
    }
    let input_ref = normalize_value_ref(args.get("input_ref"), "sop_input_ref")?;
    let existing = db
        .query_row(
            "SELECT * FROM sop_runs WHERE sop_id = ? AND occurrence_key = ?",
            params![sop_id, occurrence_key],
            row_json,
        )
        .optional()
        .map_err(|error| diagnostic("sop_run_query_failed", &error.to_string(), json!({})))?;
    let version = match args.get("sop_version") {
        Some(value) if !value.is_null() => {
            let version = value.as_i64().ok_or_else(|| {
                diagnostic(
                    "sop_invalid_version",
                    &format!("sop_invalid_version:{value}"),
                    json!({}),
                )
            })?;
            if version < 1 {
                return Err(diagnostic(
                    "sop_invalid_version",
                    &format!("sop_invalid_version:{version}"),
                    json!({}),
                ));
            }
            version
        }
        _ => match existing
            .as_ref()
            .and_then(|value| value.get("sop_version"))
            .and_then(Value::as_i64)
        {
            Some(version) => version,
            None => latest_runnable_template_version(db, &sop_id)?,
        },
    };
    let template = template_by_version(db, &sop_id, version)?;
    assert_no_legacy_effects(&template)?;
    validate_schema(
        template
            .get("input_schema")
            .filter(|value| !value.is_null()),
        &input,
        "sop_input_schema_mismatch",
        json!({"sop_id":sop_id,"sop_version":version}),
    )?;
    let admission_request = json!({
        "sop_id":sop_id,"sop_version":version,"occurrence_key":occurrence_key,
        "input":input,"input_ref":input_ref,"trigger_source_kind":trigger_source_kind,
        "trigger_source_ref":trigger_source_ref,"triggered_by":triggered_by,
        "parent_run_id":parent_run_id,"parent_step_id":parent_step_id
    });
    let request_fingerprint = fingerprint(&admission_request);
    if let Some(existing) = existing {
        let existing = hydrate_run(existing)?;
        if existing.request_fingerprint != request_fingerprint {
            return Err(diagnostic(
                "sop_occurrence_conflict",
                &format!("sop_occurrence_conflict:{sop_id}:{occurrence_key}"),
                json!({
                    "occurrence_key":occurrence_key,
                    "recorded_request_fingerprint":existing.request_fingerprint,
                    "supplied_request_fingerprint":request_fingerprint,
                    "recorded_sop_version":existing.sop_version,"supplied_sop_version":version
                }),
            ));
        }
        return Ok((existing, "replayed"));
    }
    if let (Some(parent_run_id), Some(_)) = (parent_run_id, parent_step_id) {
        assert_no_recursive_child(db, parent_run_id, &sop_id, version)?;
    }
    let definition = executable_definition(&template);
    assert_bound(&definition, "sop_definition", MAX_TEMPLATE_DEFINITION_BYTES)?;
    let definition_fingerprint = fingerprint(&definition);
    let step_states = initialize_step_states(db, &template)?;
    let step_states_value = Value::Array(step_states.clone());
    assert_bound(&step_states_value, "sop_run_state", MAX_RUN_STATE_BYTES)?;
    let run_id = format!(
        "sop_run_{}_{}",
        now_iso()
            .replace(['-', ':', '.'], "")
            .chars()
            .take(15)
            .collect::<String>(),
        &Uuid::new_v4().to_string()[..8]
    );
    let now = now_iso();
    let title = template
        .get("title")
        .and_then(Value::as_str)
        .unwrap_or_default();
    db.execute(
        "INSERT INTO sop_runs (run_id, sop_id, sop_version, sop_title, status, occurrence_key, request_fingerprint, definition_fingerprint, definition_json, input_json, input_ref_json, output_json, output_ref_json, step_states_json, trigger_source_kind, trigger_source_ref, triggered_by, parent_run_id, parent_step_id, created_at, updated_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        params![
            run_id,sop_id,version,title,"pending",occurrence_key,request_fingerprint,
            definition_fingerprint,canonical_json(&definition),canonical_json(&input),
            nullable_json(&input_ref),"{}",Option::<String>::None,
            canonical_json(&step_states_value),trigger_source_kind,trigger_source_ref,
            triggered_by,parent_run_id,parent_step_id,now,now
        ],
    )
    .map_err(|error| diagnostic("sop_run_insert_failed", &error.to_string(), json!({})))?;
    append_run_event(
        db,
        &run_id,
        None,
        "run_admitted",
        json!({
            "sop_id":sop_id,"sop_version":version,"occurrence_key":occurrence_key,
            "request_fingerprint":request_fingerprint,"definition_fingerprint":definition_fingerprint,
            "triggered_by":triggered_by,"parent_run_id":parent_run_id,"parent_step_id":parent_step_id
        }),
    )?;
    Ok((get_run(db, &run_id)?, "created"))
}

fn latest_runnable_template_version(db: &Connection, sop_id: &str) -> Result<i64, Value> {
    let version = db
        .query_row(
            "SELECT COALESCE(MAX(version), 0) FROM sop_templates WHERE sop_id = ? AND status != 'deprecated'",
            params![sop_id],
            |row| row.get::<_, i64>(0),
        )
        .map_err(|error| diagnostic("sop_template_query_failed", &error.to_string(), json!({})))?;
    if version < 1 {
        return Err(diagnostic(
            "sop_no_active_version",
            &format!("sop_no_active_version:{sop_id}"),
            json!({}),
        ));
    }
    Ok(version)
}

fn template_by_version(db: &Connection, sop_id: &str, version: i64) -> Result<Value, Value> {
    let row = db
        .query_row(
            "SELECT * FROM sop_templates WHERE sop_id = ? AND version = ?",
            params![sop_id, version],
            row_json,
        )
        .optional()
        .map_err(|error| diagnostic("sop_template_query_failed", &error.to_string(), json!({})))?
        .ok_or_else(|| {
            diagnostic(
                "sop_not_found",
                &format!("sop_not_found:{sop_id}@v{version}"),
                json!({}),
            )
        })?;
    hydrate_template(row)
}

fn hydrate_template(row: Value) -> Result<Value, Value> {
    let object = row
        .as_object()
        .ok_or_else(|| diagnostic("sop_template_corrupt", "sop_template_corrupt", json!({})))?;
    let raw_steps = object
        .get("steps_json")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            diagnostic(
                "sop_template_corrupt",
                "sop_template_corrupt",
                json!({"field":"steps_json"}),
            )
        })?;
    let steps = raw_steps
        .iter()
        .map(normalize_persisted_step)
        .collect::<Result<Vec<_>, _>>()?;
    validate_step_graph(&steps)?;
    Ok(json!({
        "sop_id":object.get("sop_id").cloned().unwrap_or(Value::Null),
        "version":object.get("version").cloned().unwrap_or(Value::Null),
        "title":object.get("title").cloned().unwrap_or(Value::Null),
        "status":object.get("status").cloned().unwrap_or(Value::Null),
        "description":object.get("description").cloned().unwrap_or_else(||json!("")),
        "steps":steps,
        "trigger_kind":object.get("trigger_kind").cloned().unwrap_or_else(||json!("manual")),
        "input_schema":object.get("input_schema_json").cloned().unwrap_or(Value::Null),
        "output":object.get("output_mapping_json").cloned().unwrap_or(Value::Null),
        "output_ref":object.get("output_ref_mapping_json").cloned().unwrap_or(Value::Null),
        "output_schema":object.get("output_schema_json").cloned().unwrap_or(Value::Null),
        "acceptance_criteria":object.get("acceptance_criteria_json").cloned().unwrap_or_else(||json!([])),
        "evidence_requirements":object.get("evidence_requirements_json").cloned().unwrap_or_else(||json!([])),
        "created_at":object.get("created_at").cloned().unwrap_or(Value::Null),
        "updated_at":object.get("updated_at").cloned().unwrap_or(Value::Null)
    }))
}

