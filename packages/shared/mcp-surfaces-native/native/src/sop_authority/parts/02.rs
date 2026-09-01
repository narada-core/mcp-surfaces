fn template_create(args: &Map<String, Value>, root: &Path) -> Result<Value, Value> {
    let sop_id = required_string(args.get("sop_id"), "sop_requires_sop_id", 512)?;
    let title = required_string(args.get("title"), "sop_requires_title", 512)?;
    let steps = validate_steps(args.get("steps"), Some(&sop_id))?;
    let input_schema = optional_schema(args.get("input_schema"), "input_schema")?;
    let output = optional_value(args.get("output"), "output")?;
    let output_ref = optional_value(args.get("output_ref"), "output_ref")?;
    let output_schema = optional_schema(args.get("output_schema"), "output_schema")?;
    validate_output_references(output.as_ref(), &steps)?;
    validate_output_references(output_ref.as_ref(), &steps)?;
    let acceptance = string_list(args.get("acceptance_criteria"))?;
    let evidence = string_list(args.get("evidence_requirements"))?;
    assert_template_bound(&json!({
        "sop_id":sop_id,"title":title,"steps":steps,"input_schema":input_schema,
        "output":output,"output_ref":output_ref,"output_schema":output_schema,
        "acceptance_criteria":acceptance,"evidence_requirements":evidence
    }))?;
    let description = optional_string(args.get("description")).unwrap_or_default();
    let trigger_kind = normalize_trigger(args.get("trigger_kind"))?;
    let db = open_db(root)?;
    let version = db
        .query_row(
            "SELECT MAX(version) FROM sop_templates WHERE sop_id = ?",
            params![sop_id],
            |row| row.get::<_, Option<i64>>(0),
        )
        .map_err(|error| diagnostic("sop_template_query_failed", &error.to_string(), json!({})))?
        .unwrap_or(0)
        + 1;
    let now = now_iso();
    insert_template(
        &db,
        &sop_id,
        version,
        &title,
        "draft",
        &description,
        &steps,
        &trigger_kind,
        input_schema.as_ref(),
        output.as_ref(),
        output_ref.as_ref(),
        output_schema.as_ref(),
        &acceptance,
        &evidence,
        &now,
    )?;
    append_event(
        &db,
        "template_created",
        json!({"sop_id":sop_id,"version":version}),
    )?;
    let step_count = steps.as_array().map_or(0, Vec::len);
    Ok(
        json!({"status":"created","sop_id":sop_id,"version":version,"title":title,"step_count":step_count}),
    )
}

fn template_update(args: &Map<String, Value>, root: &Path) -> Result<Value, Value> {
    let sop_id = required_string(args.get("sop_id"), "sop_requires_sop_id", 512)?;
    let db = open_db(root)?;
    let current = latest_template(&db, &sop_id)?.ok_or_else(|| {
        diagnostic(
            "sop_not_found",
            &format!("sop_not_found:{sop_id}"),
            json!({}),
        )
    })?;
    let current_object = current
        .as_object()
        .ok_or_else(|| diagnostic("sop_template_corrupt", "sop_template_corrupt", json!({})))?;
    let current_version = current_object
        .get("version")
        .and_then(Value::as_i64)
        .unwrap_or(0);
    let title =
        optional_string(args.get("title")).unwrap_or_else(|| text_member(current_object, "title"));
    let description = optional_string(args.get("description"))
        .unwrap_or_else(|| text_member(current_object, "description"));
    let steps = if args.contains_key("steps") {
        validate_steps(args.get("steps"), Some(&sop_id))?
    } else {
        parse_json_member(current_object, "steps_json", json!([]))?
    };
    let input_schema = if args.contains_key("input_schema") {
        optional_schema(args.get("input_schema"), "input_schema")?
    } else {
        parse_nullable_member(current_object, "input_schema_json")?
    };
    let output = if args.contains_key("output") {
        optional_value(args.get("output"), "output")?
    } else {
        parse_nullable_member(current_object, "output_mapping_json")?
    };
    let output_ref = if args.contains_key("output_ref") {
        optional_value(args.get("output_ref"), "output_ref")?
    } else {
        parse_nullable_member(current_object, "output_ref_mapping_json")?
    };
    let output_schema = if args.contains_key("output_schema") {
        optional_schema(args.get("output_schema"), "output_schema")?
    } else {
        parse_nullable_member(current_object, "output_schema_json")?
    };
    validate_output_references(output.as_ref(), &steps)?;
    validate_output_references(output_ref.as_ref(), &steps)?;
    let trigger_kind = if args.contains_key("trigger_kind") {
        normalize_trigger(args.get("trigger_kind"))?
    } else {
        normalize_trigger(current_object.get("trigger_kind"))?
    };
    let status = normalize_template_status(args.get("status"))?;
    let acceptance = if args.contains_key("acceptance_criteria") {
        string_list(args.get("acceptance_criteria"))?
    } else {
        string_list(Some(&parse_json_member(
            current_object,
            "acceptance_criteria_json",
            json!([]),
        )?))?
    };
    let evidence = if args.contains_key("evidence_requirements") {
        string_list(args.get("evidence_requirements"))?
    } else {
        string_list(Some(&parse_json_member(
            current_object,
            "evidence_requirements_json",
            json!([]),
        )?))?
    };
    assert_template_bound(&json!({
        "sop_id":sop_id,"title":title,"steps":steps,"input_schema":input_schema,
        "output":output,"output_ref":output_ref,"output_schema":output_schema,
        "acceptance_criteria":acceptance,"evidence_requirements":evidence
    }))?;
    let version = current_version + 1;
    let now = now_iso();
    insert_template(
        &db,
        &sop_id,
        version,
        &title,
        &status,
        &description,
        &steps,
        &trigger_kind,
        input_schema.as_ref(),
        output.as_ref(),
        output_ref.as_ref(),
        output_schema.as_ref(),
        &acceptance,
        &evidence,
        &now,
    )?;
    append_event(
        &db,
        "template_updated",
        json!({"sop_id":sop_id,"version":version,"previous_version":current_version}),
    )?;
    Ok(
        json!({"status":"updated","sop_id":sop_id,"version":version,"previous_version":current_version,"title":title,"step_count":steps.as_array().map(Vec::len).unwrap_or(0)}),
    )
}

fn template_deprecate(args: &Map<String, Value>, root: &Path) -> Result<Value, Value> {
    let sop_id = required_string(args.get("sop_id"), "sop_requires_sop_id", 512)?;
    let db = open_db(root)?;
    let current = latest_template(&db, &sop_id)?.ok_or_else(|| {
        diagnostic(
            "sop_not_found",
            &format!("sop_not_found:{sop_id}"),
            json!({}),
        )
    })?;
    let version = current.get("version").and_then(Value::as_i64).unwrap_or(0);
    db.execute(
        "UPDATE sop_templates SET status = 'deprecated' WHERE sop_id = ? AND version = ?",
        params![sop_id, version],
    )
    .map_err(|error| diagnostic("sop_template_update_failed", &error.to_string(), json!({})))?;
    let mut details = Map::new();
    details.insert("sop_id".to_string(), json!(sop_id));
    details.insert("version".to_string(), json!(version));
    if let Some(reason) = optional_string(args.get("reason")) {
        details.insert("reason".to_string(), json!(reason));
    }
    append_event(&db, "template_deprecated", Value::Object(details))?;
    Ok(json!({"status":"deprecated","sop_id":sop_id,"version":version}))
}

