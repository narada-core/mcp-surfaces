fn template_unimport(args: &Map<String, Value>, root: &Path) -> Result<Value, Value> {
    let sop_id = required_string(args.get("sop_id"), "sop_requires_sop_id", 512)?;
    let reason = required_string(args.get("reason"), "sop_unimport_requires_reason", 4096)?;
    let principal = required_string(
        args.get("principal"),
        "sop_unimport_requires_principal",
        512,
    )?;
    let requested_version = match args.get("version") {
        None | Some(Value::Null) => None,
        Some(value) => {
            let version = value.as_i64().ok_or_else(|| {
                diagnostic(
                    "sop_invalid_version",
                    "sop_invalid_version",
                    json!({"sop_id":sop_id}),
                )
            })?;
            if version < 1 {
                return Err(diagnostic(
                    "sop_invalid_version",
                    &format!("sop_invalid_version:{version}"),
                    json!({"sop_id":sop_id}),
                ));
            }
            Some(version)
        }
    };
    let db = open_db(root)?;
    let selected = if let Some(version) = requested_version {
        template_by_version(&db, &sop_id, version)?
    } else {
        latest_template(&db, &sop_id)?
    }
    .ok_or_else(|| {
        let suffix = requested_version
            .map(|version| format!("@v{version}"))
            .unwrap_or_default();
        diagnostic(
            "sop_not_found",
            &format!("sop_not_found:{sop_id}{suffix}"),
            json!({}),
        )
    })?;
    let version = selected.get("version").and_then(Value::as_i64).unwrap_or(0);
    let run_count: i64 = db
        .query_row(
            "SELECT COUNT(*) FROM sop_runs WHERE sop_id = ? AND sop_version = ?",
            params![sop_id, version],
            |row| row.get(0),
        )
        .map_err(|error| diagnostic("sop_run_query_failed", &error.to_string(), json!({})))?;
    let mut run_statement = db
        .prepare("SELECT run_id,status,created_at FROM sop_runs WHERE sop_id = ? AND sop_version = ? ORDER BY created_at DESC LIMIT 10")
        .map_err(|error| diagnostic("sop_run_query_failed", &error.to_string(), json!({})))?;
    let run_refs = run_statement
        .query_map(params![sop_id, version], |row| {
            Ok(json!({"run_id":row.get::<_,String>(0)?,"status":row.get::<_,String>(1)?,"created_at":row.get::<_,String>(2)?}))
        })
        .map_err(|error| diagnostic("sop_run_query_failed", &error.to_string(), json!({})))?
        .take(10)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| diagnostic("sop_run_query_failed", &error.to_string(), json!({})))?;
    let pinned_child_refs = pinned_child_references(&db, &sop_id, version)?;
    if run_count > 0 || !pinned_child_refs.is_empty() {
        return Err(diagnostic(
            "sop_template_has_runs",
            &format!("sop_template_has_runs:{sop_id}@v{version}"),
            json!({"sop_id":sop_id,"version":version,"run_count":run_count,"run_refs":run_refs,"pinned_child_refs":pinned_child_refs}),
        ));
    }
    db.execute(
        "DELETE FROM sop_templates WHERE sop_id = ? AND version = ?",
        params![sop_id, version],
    )
    .map_err(|error| diagnostic("sop_template_delete_failed", &error.to_string(), json!({})))?;
    let mut statement = db
        .prepare("SELECT version FROM sop_templates WHERE sop_id = ? ORDER BY version ASC")
        .map_err(|error| diagnostic("sop_template_query_failed", &error.to_string(), json!({})))?;
    let remaining = statement
        .query_map(params![sop_id], |row| row.get::<_, i64>(0))
        .map_err(|error| diagnostic("sop_template_query_failed", &error.to_string(), json!({})))?
        .take(10_000)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| diagnostic("sop_template_query_failed", &error.to_string(), json!({})))?;
    let event_id = append_event(
        &db,
        "template_unimported",
        json!({"sop_id":sop_id,"version":version,"reason":reason,"principal":principal,"remaining_versions":remaining}),
    )?;
    Ok(
        json!({"status":"unimported","sop_id":sop_id,"version":version,"remaining_versions":remaining,"runs_checked":run_count,"event_id":event_id}),
    )
}

fn template_import_yaml(args: &Map<String, Value>, root: &Path) -> Result<Value, Value> {
    let sop_id = required_string(args.get("sop_id"), "sop_requires_sop_id", 512)?;
    let file_name = format!("{sop_id}.sop.yaml");
    let yaml_path = sops_dirs(root)
        .into_iter()
        .map(|directory| directory.join(&file_name))
        .find(|candidate| candidate.is_file())
        .ok_or_else(|| {
            diagnostic(
                "sop_yaml_not_found",
                &format!("sop_yaml_not_found:{sop_id}"),
                json!({"searched":sops_dirs(root),"file":file_name}),
            )
        })?;
    let parsed = parse_yaml_template(&yaml_path, &sop_id)?;
    let db = open_db(root)?;
    let current = latest_template(&db, &sop_id)?;
    if let Some(current) = current.as_ref() {
        if template_matches(current, &parsed)? {
            return Ok(json!({
                "status":"unchanged","sop_id":sop_id,
                "version":current.get("version").and_then(Value::as_i64).unwrap_or(0),
                "title":parsed.get("title"),
                "step_count":parsed.get("steps").and_then(Value::as_array).map(Vec::len).unwrap_or(0)
            }));
        }
    }
    let previous_version = current
        .as_ref()
        .and_then(|value| value.get("version"))
        .and_then(Value::as_i64);
    let version = previous_version.unwrap_or(0) + 1;
    let object = parsed.as_object().expect("normalized YAML object");
    let now = now_iso();
    insert_template(
        &db,
        &sop_id,
        version,
        object.get("title").and_then(Value::as_str).unwrap_or(""),
        object
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or("draft"),
        object
            .get("description")
            .and_then(Value::as_str)
            .unwrap_or(""),
        object.get("steps").unwrap_or(&Value::Array(Vec::new())),
        object
            .get("trigger_kind")
            .and_then(Value::as_str)
            .unwrap_or("manual"),
        nullable_member(object, "input_schema"),
        nullable_member(object, "output"),
        nullable_member(object, "output_ref"),
        nullable_member(object, "output_schema"),
        object
            .get("acceptance_criteria")
            .and_then(Value::as_array)
            .unwrap_or(&Vec::new()),
        object
            .get("evidence_requirements")
            .and_then(Value::as_array)
            .unwrap_or(&Vec::new()),
        &now,
    )?;
    let event_kind = if previous_version.is_some() {
        "template_updated"
    } else {
        "template_created"
    };
    let mut details = Map::new();
    details.insert("sop_id".to_string(), json!(sop_id));
    details.insert("version".to_string(), json!(version));
    if let Some(previous) = previous_version {
        details.insert("previous_version".to_string(), json!(previous));
    }
    details.insert("source".to_string(), json!("yaml_import"));
    details.insert("yaml_path".to_string(), json!(yaml_path.to_string_lossy()));
    append_event(&db, event_kind, Value::Object(details))?;
    let status = if previous_version.is_some() {
        "updated"
    } else {
        "created"
    };
    let mut response = Map::new();
    response.insert("status".to_string(), json!(status));
    response.insert("sop_id".to_string(), json!(sop_id));
    response.insert("version".to_string(), json!(version));
    if let Some(previous) = previous_version {
        response.insert("previous_version".to_string(), json!(previous));
    }
    response.insert(
        "title".to_string(),
        object.get("title").cloned().unwrap_or(Value::Null),
    );
    response.insert(
        "step_count".to_string(),
        json!(object
            .get("steps")
            .and_then(Value::as_array)
            .map(Vec::len)
            .unwrap_or(0)),
    );
    Ok(Value::Object(response))
}

