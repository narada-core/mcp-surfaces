fn normalize_action(value: Option<&Value>, step_id: &str) -> Result<Option<Value>, Value> {
    let Some(value) = value else {
        return Ok(None);
    };
    if value.is_null() {
        return Ok(None);
    }
    let object = value.as_object().ok_or_else(|| {
        diagnostic(
            "sop_action_binding_invalid",
            &format!("sop_action_binding_invalid:{step_id}"),
            json!({"reason":"must_be_object"}),
        )
    })?;
    let allowed = [
        "surface_id",
        "tool_name",
        "arguments",
        "idempotency_key_argument",
    ];
    let unknown = object
        .keys()
        .filter(|key| !allowed.contains(&key.as_str()))
        .cloned()
        .collect::<Vec<_>>();
    if !unknown.is_empty() {
        return Err(diagnostic(
            "sop_action_binding_invalid",
            &format!("sop_action_binding_invalid:{step_id}"),
            json!({"reason":"unknown_fields","fields":unknown}),
        ));
    }
    let surface_id = required_string(
        object.get("surface_id"),
        "sop_action_requires_surface_id",
        256,
    )?;
    let tool_name = required_string(
        object.get("tool_name"),
        "sop_action_requires_tool_name",
        256,
    )?;
    let idempotency = required_string(
        object.get("idempotency_key_argument"),
        "sop_action_requires_idempotency_key_argument",
        128,
    )?;
    if !valid_identifier(&idempotency) {
        return Err(diagnostic(
            "sop_action_idempotency_key_argument_invalid",
            &format!("sop_action_idempotency_key_argument_invalid:{idempotency}"),
            json!({}),
        ));
    }
    let arguments = object
        .get("arguments")
        .cloned()
        .unwrap_or_else(|| json!({}));
    if !arguments.is_object() {
        return Err(diagnostic(
            "sop_action_arguments_must_be_object",
            &format!("sop_action_arguments_must_be_object:{step_id}"),
            json!({}),
        ));
    }
    assert_bound(
        &arguments,
        "sop_action_arguments_mapping",
        MAX_INLINE_VALUE_BYTES,
    )?;
    if arguments.get(&idempotency).is_some() {
        return Err(diagnostic(
            "sop_action_idempotency_argument_reserved",
            &format!("sop_action_idempotency_argument_reserved:{step_id}"),
            json!({"field":idempotency}),
        ));
    }
    Ok(Some(json!({
        "surface_id":surface_id,"tool_name":tool_name,"arguments":arguments,
        "idempotency_key_argument":idempotency
    })))
}

fn optional_schema(value: Option<&Value>, field: &str) -> Result<Option<Value>, Value> {
    let Some(value) = value else {
        return Ok(None);
    };
    if value.is_null() {
        return Ok(None);
    }
    if !value.is_object() {
        return Err(diagnostic(
            "sop_json_schema_must_be_object",
            &format!("sop_json_schema_must_be_object:{field}"),
            json!({"field":field}),
        ));
    }
    assert_bound(value, "sop_json_schema", MAX_INLINE_VALUE_BYTES)?;
    validator_for(value).map_err(|error| {
        diagnostic(
            "sop_json_schema_invalid",
            &format!("sop_json_schema_invalid:{field}"),
            json!({"field":field,"message":error.to_string()}),
        )
    })?;
    Ok(Some(value.clone()))
}

fn optional_value(value: Option<&Value>, field: &str) -> Result<Option<Value>, Value> {
    let Some(value) = value else {
        return Ok(None);
    };
    if value.is_null() {
        return Ok(None);
    }
    assert_bound(value, field, MAX_INLINE_VALUE_BYTES)?;
    Ok(Some(value.clone()))
}

pub(crate) fn required_string(
    value: Option<&Value>,
    code: &str,
    max: usize,
) -> Result<String, Value> {
    let text = value
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| diagnostic(code, code, json!({})))?;
    if text.chars().count() > max {
        return Err(diagnostic(
            &format!("{code}_too_long"),
            &format!("{code}_too_long"),
            json!({"length":text.chars().count(),"max_length":max}),
        ));
    }
    Ok(text.to_string())
}

pub(crate) fn optional_string(value: Option<&Value>) -> Option<String> {
    value
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
}

fn string_list(value: Option<&Value>) -> Result<Vec<Value>, Value> {
    let Some(value) = value else {
        return Ok(Vec::new());
    };
    if value.is_null() {
        return Ok(Vec::new());
    }
    let values = value.as_array().ok_or_else(|| {
        diagnostic(
            "sop_string_list_invalid",
            "sop_string_list_invalid",
            json!({"reason":"must_be_array"}),
        )
    })?;
    let mut output = Vec::new();
    let mut seen = HashSet::new();
    for (index, value) in values.iter().enumerate() {
        let text = value
            .as_str()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| {
                diagnostic(
                    "sop_string_list_invalid",
                    "sop_string_list_invalid",
                    json!({"reason":"entry_must_be_nonempty_string","index":index}),
                )
            })?;
        if !seen.insert(text.to_string()) {
            return Err(diagnostic(
                "sop_string_list_invalid",
                "sop_string_list_invalid",
                json!({"reason":"duplicate_entries"}),
            ));
        }
        output.push(json!(text));
    }
    Ok(output)
}

fn normalize_trigger(value: Option<&Value>) -> Result<String, Value> {
    let value = optional_string(value).unwrap_or_else(|| "manual".to_string());
    if !matches!(value.as_str(), "manual" | "inbox_event" | "schedule") {
        return Err(diagnostic(
            "sop_invalid_trigger_kind",
            &format!("sop_invalid_trigger_kind:{value}"),
            json!({"trigger_kind":value,"allowed":["manual","inbox_event","schedule"]}),
        ));
    }
    Ok(value)
}

fn normalize_template_status(value: Option<&Value>) -> Result<String, Value> {
    let value = optional_string(value).unwrap_or_else(|| "draft".to_string());
    if !matches!(value.as_str(), "draft" | "active" | "deprecated") {
        return Err(diagnostic(
            "sop_invalid_template_status",
            &format!("sop_invalid_template_status:{value}"),
            json!({"status":value,"allowed":["draft","active","deprecated"]}),
        ));
    }
    Ok(value)
}

fn append_event(db: &Connection, kind: &str, details: Value) -> Result<String, Value> {
    let event_id = format!("soe_{}", &Uuid::new_v4().to_string()[..12]);
    db.execute(
        "INSERT INTO sop_events(event_id,run_id,step_id,event_kind,details_json,recorded_at) VALUES (?,'','',?,?,?)",
        params![event_id, kind, encode(&details)?, now_iso()],
    )
    .map_err(|error| diagnostic("sop_event_insert_failed", &error.to_string(), json!({})))?;
    Ok(event_id)
}

fn pinned_child_references(
    db: &Connection,
    sop_id: &str,
    version: i64,
) -> Result<Vec<Value>, Value> {
    let mut statement = db
        .prepare(
            "SELECT run_id,step_states_json FROM sop_runs ORDER BY created_at DESC LIMIT 10000",
        )
        .map_err(|error| diagnostic("sop_run_query_failed", &error.to_string(), json!({})))?;
    let rows = statement
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })
        .map_err(|error| diagnostic("sop_run_query_failed", &error.to_string(), json!({})))?;
    let mut references = Vec::new();
    for row in rows.take(10_000) {
        let (run_id, encoded) =
            row.map_err(|error| diagnostic("sop_run_query_failed", &error.to_string(), json!({})))?;
        let Ok(states) = serde_json::from_str::<Value>(&encoded) else {
            continue;
        };
        for step in states.as_array().into_iter().flatten() {
            if step.get("sop_id").and_then(Value::as_str) == Some(sop_id)
                && step.get("sop_version").and_then(Value::as_i64) == Some(version)
            {
                references.push(json!({"run_id":run_id,"step_id":step.get("step_id").cloned().unwrap_or(Value::Null)}));
                if references.len() >= 20 {
                    return Ok(references);
                }
            }
        }
    }
    Ok(references)
}

fn sops_dirs(root: &Path) -> Vec<PathBuf> {
    let mut directories = Vec::new();
    if let Some(value) = std::env::var_os("NARADA_SOPS_DIR") {
        directories.push(PathBuf::from(value));
    }
    directories.push(root.join("sops"));
    directories.push(root.join(".ai/sops"));
    let mut seen = HashSet::new();
    directories
        .into_iter()
        .filter(|path| seen.insert(path.clone()))
        .take(10)
        .collect()
}

