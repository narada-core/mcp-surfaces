fn parse_yaml_template(path: &Path, expected_sop_id: &str) -> Result<Value, Value> {
    let metadata = fs::metadata(path).map_err(|error| {
        diagnostic(
            "sop_yaml_read_error",
            &format!("sop_yaml_read_error:{expected_sop_id}"),
            json!({"yaml_path":path,"message":error.to_string()}),
        )
    })?;
    if metadata.len() > MAX_TEMPLATE_FILE_BYTES {
        return Err(diagnostic(
            "sop_yaml_too_large",
            &format!("sop_yaml_too_large:{expected_sop_id}"),
            json!({"yaml_path":path,"byte_length":metadata.len(),"max_bytes":MAX_TEMPLATE_FILE_BYTES}),
        ));
    }
    let raw = fs::read_to_string(path).map_err(|error| {
        diagnostic(
            "sop_yaml_read_error",
            &format!("sop_yaml_read_error:{expected_sop_id}"),
            json!({"yaml_path":path,"message":error.to_string()}),
        )
    })?;
    let document: Value = yaml_serde::from_str(&raw).map_err(|error| {
        diagnostic(
            "sop_yaml_parse_error",
            &format!("sop_yaml_parse_error:{expected_sop_id}"),
            json!({"yaml_path":path,"message":error.to_string()}),
        )
    })?;
    let schema: Value = serde_json::from_str(TEMPLATE_SCHEMA)
        .map_err(|error| diagnostic("sop_schema_load_failed", &error.to_string(), json!({})))?;
    let validator = validator_for(&schema)
        .map_err(|error| diagnostic("sop_schema_load_failed", &error.to_string(), json!({})))?;
    let schema_errors = validator
        .iter_errors(&document)
        .take(20)
        .map(|error| error.to_string())
        .collect::<Vec<_>>();
    if !schema_errors.is_empty() {
        return Err(diagnostic(
            "sop_yaml_schema_error",
            &format!("sop_yaml_schema_error:{expected_sop_id}"),
            json!({"yaml_path":path,"errors":schema_errors.join("; ")}),
        ));
    }
    let object = document.as_object().ok_or_else(|| {
        diagnostic(
            "sop_yaml_schema_error",
            &format!("sop_yaml_schema_error:{expected_sop_id}"),
            json!({"yaml_path":path,"errors":"(root) must be object"}),
        )
    })?;
    let yaml_sop_id = required_string(object.get("sop_id"), "sop_yaml_requires_sop_id", 512)?;
    if yaml_sop_id != expected_sop_id {
        return Err(diagnostic(
            "sop_yaml_id_mismatch",
            &format!("sop_yaml_id_mismatch:arg={expected_sop_id} yaml={yaml_sop_id}"),
            json!({"yaml_path":path}),
        ));
    }
    let title = required_string(object.get("title"), "sop_yaml_requires_title", 512)?;
    let description = optional_string(object.get("description")).unwrap_or_default();
    let trigger_kind = normalize_trigger(object.get("trigger_kind"))?;
    let status = normalize_template_status(object.get("status"))?;
    let steps = validate_steps(object.get("steps"), Some(&yaml_sop_id))?;
    let input_schema = optional_schema(object.get("input_schema"), "input_schema")?;
    let output = optional_value(object.get("output"), "output")?;
    let output_ref = optional_value(object.get("output_ref"), "output_ref")?;
    let output_schema = optional_schema(object.get("output_schema"), "output_schema")?;
    validate_output_references(output.as_ref(), &steps)?;
    validate_output_references(output_ref.as_ref(), &steps)?;
    let acceptance = string_list(object.get("acceptance_criteria"))?;
    let evidence = string_list(object.get("evidence_requirements"))?;
    let normalized = json!({
        "sop_id":yaml_sop_id,"title":title,"description":description,"trigger_kind":trigger_kind,
        "status":status,"steps":steps,"input_schema":input_schema,"output":output,
        "output_ref":output_ref,"output_schema":output_schema,"acceptance_criteria":acceptance,
        "evidence_requirements":evidence
    });
    assert_template_bound(&normalized)?;
    Ok(normalized)
}

fn template_matches(current: &Value, next: &Value) -> Result<bool, Value> {
    let current = current
        .as_object()
        .ok_or_else(|| diagnostic("sop_template_corrupt", "sop_template_corrupt", json!({})))?;
    let next = next.as_object().expect("normalized template");
    let comparisons = [
        (
            Value::String(text_member(current, "title")),
            next.get("title").cloned().unwrap_or(Value::Null),
        ),
        (
            Value::String(text_member(current, "status")),
            next.get("status").cloned().unwrap_or(Value::Null),
        ),
        (
            Value::String(text_member(current, "description")),
            next.get("description").cloned().unwrap_or(Value::Null),
        ),
        (
            Value::String(text_member(current, "trigger_kind")),
            next.get("trigger_kind").cloned().unwrap_or(Value::Null),
        ),
        (
            parse_json_member(current, "steps_json", json!([]))?,
            next.get("steps").cloned().unwrap_or(Value::Null),
        ),
        (
            parse_nullable_member(current, "input_schema_json")?.unwrap_or(Value::Null),
            next.get("input_schema").cloned().unwrap_or(Value::Null),
        ),
        (
            parse_nullable_member(current, "output_mapping_json")?.unwrap_or(Value::Null),
            next.get("output").cloned().unwrap_or(Value::Null),
        ),
        (
            parse_nullable_member(current, "output_ref_mapping_json")?.unwrap_or(Value::Null),
            next.get("output_ref").cloned().unwrap_or(Value::Null),
        ),
        (
            parse_nullable_member(current, "output_schema_json")?.unwrap_or(Value::Null),
            next.get("output_schema").cloned().unwrap_or(Value::Null),
        ),
        (
            parse_json_member(current, "acceptance_criteria_json", json!([]))?,
            next.get("acceptance_criteria")
                .cloned()
                .unwrap_or(Value::Null),
        ),
        (
            parse_json_member(current, "evidence_requirements_json", json!([]))?,
            next.get("evidence_requirements")
                .cloned()
                .unwrap_or(Value::Null),
        ),
    ];
    Ok(comparisons.into_iter().all(|(left, right)| left == right))
}

