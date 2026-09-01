fn run_detail(value: Value) -> Value {
    let steps = member(&value, "step_states_json");
    let parse_error = if steps.is_array() { Value::Null } else { json!("step_states_json is not an array") };
    let step_values = steps.as_array().cloned().unwrap_or_default();
    let next_steps = step_values.iter().filter(|step| member(step, "status").as_str() == Some("running")).map(|step| {
        let action = member(step, "action");
        let action_target = action.as_object().map(|object| json!({"surface_id":object.get("surface_id").cloned().unwrap_or(Value::Null),"tool_name":object.get("tool_name").cloned().unwrap_or(Value::Null)}));
        let result = member(step, "result");
        let instructions = result.as_object().and_then(|object| object.get("instructions")).cloned().filter(|value| !value.is_null()).unwrap_or_else(|| member(step, "instructions"));
        json!({"step_id":member(step,"step_id"),"executor":member(step,"executor"),"title":member(step,"title"),"instructions":instructions,"child_run_id":member(step,"child_run_id"),"child_sop_id":member(step,"sop_id"),"action_id":member(step,"action_id"),"action_target":action_target,"result":result,"result_ref":member(step,"result_ref")})
    }).collect::<Vec<_>>();
    let child_pins = step_values.iter().filter(|step| member(step, "executor").as_str() == Some("sop")).map(|step| json!({"step_id":member(step,"step_id"),"sop_id":member(step,"sop_id"),"sop_version":member(step,"sop_version"),"definition_fingerprint":member(step,"pinned_child_definition_fingerprint")})).collect::<Vec<_>>();
    json!({
        "schema":"narada.sop.run.v2",
        "run_id":member(&value,"run_id"),
        "sop_id":member(&value,"sop_id"),
        "sop_version":member(&value,"sop_version"),
        "sop_title":member(&value,"sop_title"),
        "status":member(&value,"status"),
        "occurrence_key":member(&value,"occurrence_key"),
        "request_fingerprint":member(&value,"request_fingerprint"),
        "definition_fingerprint":member(&value,"definition_fingerprint"),
        "input":member(&value,"input_json"),
        "input_ref":member(&value,"input_ref_json"),
        "output":member(&value,"output_json"),
        "output_ref":member(&value,"output_ref_json"),
        "step_states":step_values,
        "step_states_parse_error":parse_error,
        "trigger_source_kind":member(&value,"trigger_source_kind"),
        "trigger_source_ref":member(&value,"trigger_source_ref"),
        "triggered_by":member(&value,"triggered_by"),
        "parent_run_id":member(&value,"parent_run_id"),
        "parent_step_id":member(&value,"parent_step_id"),
        "created_at":member(&value,"created_at"),
        "updated_at":member(&value,"updated_at"),
        "completed_at":member(&value,"completed_at"),
        "definition_snapshot":{"stored":true,"fingerprint":member(&value,"definition_fingerprint"),"sop_id":member(&value,"sop_id"),"sop_version":member(&value,"sop_version"),"child_pins":child_pins},
        "admission":Value::Null,
        "next_awaits_confirmation":next_steps.iter().any(|step| matches!(step.get("executor").and_then(Value::as_str),Some("agent")|Some("operator"))),
        "next_steps":next_steps,
        "next_step":next_steps.first().cloned().unwrap_or(Value::Null),
        "relationship_reconciliation":{"mode":"automatic","repair_tool":"sop_run_refresh"},
        "native_hydration":"bounded_sqlite_read"
    })
}

fn handoff_list(args: &Map<String, Value>, root: &Path) -> Result<Value, Value> {
    let limit = args.get("limit").and_then(Value::as_u64).unwrap_or(50).clamp(1, 100);
    let run_id = args.get("run_id").and_then(Value::as_str).filter(|value| !value.trim().is_empty());
    let executor = args.get("executor").and_then(Value::as_str).filter(|value| !value.trim().is_empty());
    let status = args.get("status").and_then(Value::as_str).filter(|value| !value.trim().is_empty());
    if let Some(executor) = executor {
        if !matches!(executor, "agent" | "operator") { return Err(error("sop_handoff_executor_invalid", &format!("sop_handoff_executor_invalid:{executor}"))); }
    }
    if let Some(status) = status {
        if !matches!(status, "pending" | "leased" | "completed" | "failed" | "cancelled") { return Err(error("sop_handoff_status_invalid", &format!("sop_handoff_status_invalid:{status}"))); }
    }
    let mut sql = String::from("SELECT * FROM sop_handoffs");
    let mut conditions = Vec::<&str>::new();
    let mut values = Vec::<String>::new();
    if let Some(run_id) = run_id { conditions.push("run_id = ?"); values.push(run_id.to_string()); }
    if let Some(executor) = executor { conditions.push("executor = ?"); values.push(executor.to_string()); }
    if let Some(status) = status { conditions.push("status = ?"); values.push(status.to_string()); }
    if !conditions.is_empty() { sql.push_str(" WHERE "); sql.push_str(&conditions.join(" AND ")); }
    sql.push_str(" ORDER BY created_at, handoff_id LIMIT ?");
    values.push(limit.to_string());
    let Some(connection) = open_db(root)? else { return Ok(json!({"schema":"narada.sop.handoff_list.v1","items":[],"count":0})); };
    let mut statement = connection.prepare(&sql).map_err(|e| error("sop_handoff_query_failed", &e.to_string()))?;
    let rows = statement.query_map(rusqlite::params_from_iter(values.iter()), row_value).map_err(|e| error("sop_handoff_query_failed", &e.to_string()))?;
    let items = rows.take(100).map(|row| row.map(handoff_record).map_err(|e| error("sop_handoff_row_failed", &e.to_string()))).collect::<Result<Vec<_>, _>>()?;
    Ok(json!({"schema":"narada.sop.handoff_list.v1","items":items,"count":items.len()}))
}

fn handoff_show(args: &Map<String, Value>, root: &Path) -> Result<Value, Value> {
    let handoff_id = args.get("handoff_id").and_then(Value::as_str).filter(|value| !value.trim().is_empty()).ok_or_else(|| error("sop_handoff_id_required", "sop_handoff_id_required"))?;
    let Some(connection) = open_db(root)? else { return Err(error("sop_handoff_not_found", &format!("sop_handoff_not_found:{handoff_id}"))); };
    let row = connection.query_row("SELECT * FROM sop_handoffs WHERE handoff_id = ? LIMIT 1", params![handoff_id], row_value).optional().map_err(|e| error("sop_handoff_query_failed", &e.to_string()))?;
    row.map(handoff_record).ok_or_else(|| error("sop_handoff_not_found", &format!("sop_handoff_not_found:{handoff_id}")))
}

fn handoff_record(value: Value) -> Value {
    json!({"schema":"narada.sop.handoff.v1","handoff_id":member(&value,"handoff_id"),"run_id":member(&value,"run_id"),"step_id":member(&value,"step_id"),"occurrence_key":member(&value,"occurrence_key"),"sop_id":member(&value,"sop_id"),"sop_version":member(&value,"sop_version"),"executor":member(&value,"executor"),"title":member(&value,"title"),"instructions":member(&value,"instructions"),"input":member(&value,"input_json"),"input_ref":member(&value,"input_ref_json"),"result_schema":member(&value,"result_schema_json"),"request_fingerprint":member(&value,"request_fingerprint"),"status":member(&value,"status"),"lease_owner":member(&value,"lease_owner"),"lease_expires_at":member(&value,"lease_expires_at"),"attempt_count":member(&value,"attempt_count"),"last_error":member(&value,"last_error"),"completion_key":member(&value,"completion_key"),"completion_fingerprint":member(&value,"completion_fingerprint"),"principal":member(&value,"principal"),"result":member(&value,"result_json"),"result_ref":member(&value,"result_ref_json"),"error_message":member(&value,"error_message"),"created_at":member(&value,"created_at"),"updated_at":member(&value,"updated_at"),"completed_at":member(&value,"completed_at")})
}

fn run_events(args: &Map<String, Value>, root: &Path) -> Result<Value, Value> {
    let run_id = args.get("run_id").and_then(Value::as_str).filter(|value| !value.trim().is_empty()).ok_or_else(|| error("sop_requires_run_id", "sop_requires_run_id"))?;
    let limit = args.get("limit").and_then(Value::as_u64).unwrap_or(50).clamp(1, 500);
    let offset = args.get("offset").and_then(Value::as_u64).unwrap_or(0).min(100_000);
    let Some(connection) = open_db(root)? else { return Ok(json!({"items":[],"count":0,"run_id":run_id})); };
    let mut statement = connection.prepare("SELECT * FROM sop_events WHERE run_id = ? ORDER BY rowid DESC LIMIT ? OFFSET ?").map_err(|e| error("sop_event_query_failed", &e.to_string()))?;
    let rows = statement.query_map(params![run_id, limit, offset], row_value).map_err(|e| error("sop_event_query_failed", &e.to_string()))?;
    let items = rows.take(500).map(|row| row.map(event_record).map_err(|e| error("sop_event_row_failed", &e.to_string()))).collect::<Result<Vec<_>, _>>()?;
    Ok(json!({"items":items,"count":items.len(),"run_id":run_id}))
}

fn event_record(value: Value) -> Value {
    json!({"event_id":member(&value,"event_id"),"run_id":member(&value,"run_id"),"step_id":member(&value,"step_id"),"event_kind":member(&value,"event_kind"),"details":member(&value,"details_json"),"recorded_at":member(&value,"recorded_at")})
}

fn run_coverage_since(args: &Map<String, Value>, root: &Path) -> Result<Value, Value> {
    let since = args.get("since").and_then(Value::as_str).map(str::trim).filter(|value| !value.is_empty()).ok_or_else(|| error("sop_requires_since", "sop_requires_since"))?;
    let since_time = parse_timestamp(since).ok_or_else(|| error("sop_since_must_be_iso_timestamp", &format!("sop_since_must_be_iso_timestamp:{since}")))?;
    let limit = args.get("limit").and_then(Value::as_u64).unwrap_or(200).clamp(1, 500);
    let template_status = args.get("template_status").and_then(Value::as_str).filter(|value| !value.trim().is_empty()).unwrap_or("active");
    if !matches!(template_status, "draft" | "active" | "deprecated") { return Err(error("sop_template_status_unsupported", &format!("sop_template_status_unsupported:{template_status}"))); }
    let run_status = args.get("status").and_then(Value::as_str).filter(|value| !value.trim().is_empty());
    if let Some(status) = run_status {
        if !RUN_STATUSES.contains(&status) { return Err(error("sop_run_status_unsupported", &format!("sop_run_status_unsupported:{status}"))); }
    }
    let include_terminal = args.get("include_terminal").and_then(Value::as_bool).unwrap_or(true);
    let Some(connection) = open_db(root)? else { return Ok(json!({"schema":"narada.sop.run_coverage_since.v1","status":"missing","since":since,"template_status":template_status,"run_status":run_status,"include_terminal":include_terminal,"items":[],"count":0,"classification_counts":{}})); };
    let mut template_statement = connection.prepare("SELECT t.* FROM sop_templates t JOIN (SELECT sop_id, MAX(version) AS mv FROM sop_templates GROUP BY sop_id) latest ON t.sop_id = latest.sop_id AND t.version = latest.mv WHERE t.status = ? ORDER BY t.updated_at DESC LIMIT ?").map_err(|e| error("sop_template_query_failed", &e.to_string()))?;
    let templates = template_statement.query_map(params![template_status, limit], row_value).map_err(|e| error("sop_template_query_failed", &e.to_string()))?;
    let mut items = Vec::new();
    let mut run_statement = connection.prepare("SELECT * FROM sop_runs WHERE sop_id = ? AND sop_version = ? ORDER BY created_at DESC LIMIT 1").map_err(|e| error("sop_run_query_failed", &e.to_string()))?;
    for template in templates.take(500) {
        let template = template.map_err(|e| error("sop_template_row_failed", &e.to_string()))?;
        let sop_id = member(&template, "sop_id");
        let version = member(&template, "version");
        let latest = run_statement.query_row(params![sop_id.as_str().unwrap_or_default(), version.as_i64().unwrap_or(0)], row_value).optional().map_err(|e| error("sop_run_query_failed", &e.to_string()))?;
        let latest_run_at = latest.as_ref().map(|run| { let created = member(run, "created_at"); if created.is_null() { member(run, "updated_at") } else { created } });
        let latest_run_time = latest_run_at.as_ref().and_then(Value::as_str).and_then(parse_timestamp);
        let classification = match latest_run_time { None => if latest.is_some() { "stale" } else { "not_run" }, Some(value) if value >= since_time => "recent", Some(_) => "stale" };
        let latest_status = latest.as_ref().map(|run| member(run, "status"));
        if !include_terminal && latest_status.as_ref().and_then(Value::as_str).map(|value| matches!(value, "completed" | "failed" | "cancelled")).unwrap_or(false) { continue; }
        if let Some(status) = run_status { if latest_status.as_ref().and_then(Value::as_str) != Some(status) { continue; } }
        if classification == "recent" { continue; }
        let latest_summary = latest.as_ref().map(|run| run_summary(run)).unwrap_or(Value::Null);
        items.push(json!({"sop_id":sop_id,"version":version,"title":member(&template,"title"),"template_status":member(&template,"status"),"classification":classification,"stale":classification != "recent","latest_run_id":latest.as_ref().map(|run|member(run,"run_id")).unwrap_or(Value::Null),"latest_run_at":latest_run_at.unwrap_or(Value::Null),"latest_run_status":latest_status.unwrap_or(Value::Null),"latest_run":latest_summary}));
    }
    let mut classification_counts = Map::new();
    for item in &items { let key = item.get("classification").and_then(Value::as_str).unwrap_or("unknown"); let current = classification_counts.get(key).and_then(Value::as_u64).unwrap_or(0); classification_counts.insert(key.to_string(), json!(current + 1)); }
    Ok(json!({"schema":"narada.sop.run_coverage_since.v1","status":"ok","since":since,"template_status":template_status,"run_status":run_status,"include_terminal":include_terminal,"items":items,"count":items.len(),"classification_counts":classification_counts}))
}

fn parse_timestamp(value: &str) -> Option<OffsetDateTime> {
    OffsetDateTime::parse(value, &Rfc3339).ok().or_else(|| {
        if value.len() == 10 { OffsetDateTime::parse(&format!("{value}T00:00:00Z"), &Rfc3339).ok() } else { None }
    })
}

fn outbox_list(args: &Map<String, Value>, root: &Path) -> Result<Value, Value> {
    let consumer_id = args.get("consumer_id").and_then(Value::as_str).filter(|value| !value.trim().is_empty()).ok_or_else(|| error("sop_outbox_consumer_id_required", "sop_outbox_consumer_id_required"))?;
    let topic = args.get("topic").and_then(Value::as_str).filter(|value| !value.trim().is_empty());
    if let Some(topic) = topic {
        if topic != "sop.run.terminal.v1" { return Err(error("sop_outbox_topic_unsupported", &format!("sop_outbox_topic_unsupported:{topic}"))); }
    }
    let limit = args.get("limit").and_then(Value::as_u64).unwrap_or(100).clamp(1, 500);
    let Some(connection) = open_db(root)? else { return Err(error("sop_outbox_consumer_not_registered", &format!("sop_outbox_consumer_not_registered:{consumer_id}"))); };
    let registered: i64 = if let Some(topic) = topic {
        connection.query_row("SELECT COUNT(*) FROM sop_outbox_consumer_requirements WHERE consumer_id = ? AND topic = ?", params![consumer_id, topic], |row| row.get(0))
    } else {
        connection.query_row("SELECT COUNT(*) FROM sop_outbox_consumer_requirements WHERE consumer_id = ?", params![consumer_id], |row| row.get(0))
    }.map_err(|e| error("sop_outbox_consumer_query_failed", &e.to_string()))?;
    if registered == 0 { return Err(error("sop_outbox_consumer_not_registered", &format!("sop_outbox_consumer_not_registered:{consumer_id}"))); }
    let now = OffsetDateTime::now_utc().format(&Rfc3339).map_err(|e| error("sop_outbox_time_failed", &e.to_string()))?;
    let mut statement = connection.prepare("SELECT outbox.* FROM sop_outbox outbox JOIN sop_outbox_consumer_requirements requirement ON requirement.topic = outbox.topic AND requirement.consumer_id = ? WHERE (? IS NULL OR requirement.topic = ?) AND outbox.created_at >= requirement.start_at AND outbox.available_at <= ? AND NOT EXISTS (SELECT 1 FROM sop_outbox_receipts receipt WHERE receipt.event_id = outbox.event_id AND receipt.consumer_id = ?) ORDER BY outbox.created_at, outbox.event_id LIMIT ?").map_err(|e| error("sop_outbox_query_failed", &e.to_string()))?;
    let rows = statement.query_map(params![consumer_id, topic, topic, now, consumer_id, limit], row_value).map_err(|e| error("sop_outbox_query_failed", &e.to_string()))?;
    let items = rows.take(500).map(|row| row.map(outbox_record).map_err(|e| error("sop_outbox_row_failed", &e.to_string()))).collect::<Result<Vec<_>, _>>()?;
    Ok(json!({"schema":"narada.sop.outbox_list.v1","items":items,"count":items.len()}))
}

fn outbox_record(value: Value) -> Value {
    json!({"schema":"narada.sop.outbox_event.v1","event_id":member(&value,"event_id"),"topic":member(&value,"topic"),"partition_key":member(&value,"partition_key"),"run_id":member(&value,"run_id"),"sop_id":member(&value,"sop_id"),"sop_version":member(&value,"sop_version"),"occurrence_key":member(&value,"occurrence_key"),"outcome":member(&value,"outcome"),"payload":member(&value,"payload_json"),"created_at":member(&value,"created_at"),"available_at":member(&value,"available_at"),"compacted_at":member(&value,"compacted_at")})
}

fn doctor(root: &Path) -> Result<Value, Value> {
    let dirs = sops_dirs(root); let mut counts = Vec::new();
    for dir in &dirs { let count = fs::read_dir(dir).ok().map(|entries| entries.filter_map(Result::ok).filter(|entry| entry.path().file_name().and_then(|v|v.to_str()).map(|v|v.ends_with(".sop.yaml")).unwrap_or(false)).take(MAX_CANDIDATES).count()).unwrap_or(0); counts.push(json!({"path":dir.to_string_lossy(),"candidate_count":count})); }
    Ok(json!({"schema":"narada.sop_mcp.doctor.v1","status":"ok","site_root":root.to_string_lossy(),"sops_dirs":counts,"native_adapter":"complete_sop_authority","execution":"native_rust","server_name":SERVER_NAME}))
}

fn candidate_entries(root: &Path) -> Vec<(String, PathBuf)> {
    let mut entries = Vec::new();
    for dir in sops_dirs(root) { if let Ok(read) = fs::read_dir(dir) { for entry in read.filter_map(Result::ok).take(MAX_CANDIDATES) { let path = entry.path(); if path.file_name().and_then(|v|v.to_str()).map(|v|v.ends_with(".sop.yaml")).unwrap_or(false) { if let Some(name) = path.file_name().and_then(|v|v.to_str()).map(|v|v.trim_end_matches(".sop.yaml").to_string()) { entries.push((name,path)); } } if entries.len() >= MAX_CANDIDATES { break; } } } }
    entries
}

fn candidate_list(args: &Map<String, Value>, root: &Path) -> Result<Value, Value> {
    let limit = args.get("limit").and_then(Value::as_u64).unwrap_or(50).clamp(1, MAX_CANDIDATES as u64) as usize;
    let candidates = candidate_entries(root).into_iter().take(limit).map(|(sop_id,path)| { let meta = fs::metadata(&path).ok(); json!({"sop_id":sop_id,"path":path.to_string_lossy(),"bytes":meta.as_ref().map(|m|m.len()),"modified":meta.and_then(|m|m.modified().ok()).and_then(|v|v.duration_since(std::time::UNIX_EPOCH).ok()).map(|v|v.as_secs()),"import_state":"unverified"}) }).collect::<Vec<_>>();
    Ok(json!({"schema":"narada.sop_mcp.template_candidates.v1","status":"ok","count":candidates.len(),"limit":limit,"candidates":candidates,"native_read_only":true}))
}

fn safe_id(args: &Map<String, Value>) -> Result<String, Value> { let id = args.get("sop_id").and_then(Value::as_str).filter(|v|!v.trim().is_empty()).ok_or_else(||error("sop_id_required","sop_id_required"))?.trim().to_string(); if id.len()>120 || !id.chars().all(|c|c.is_ascii_alphanumeric() || c=='-' || c=='_' || c=='.') { return Err(error("sop_id_invalid","sop_id_invalid")); } Ok(id) }

fn candidate_show(args: &Map<String, Value>, root: &Path) -> Result<Value, Value> {
    let id = safe_id(args)?; let Some((_, path)) = candidate_entries(root).into_iter().find(|(candidate, _)| candidate == &id) else { return Err(error("sop_yaml_not_found","sop_yaml_not_found")); };
    if fs::metadata(&path).map_err(|e|error("sop_yaml_read_failed",&e.to_string()))?.len() > MAX_TEMPLATE_BYTES { return Err(error("sop_yaml_too_large", "sop_yaml_too_large")); }
    let text = fs::read_to_string(&path).map_err(|e|error("sop_yaml_read_failed",&e.to_string()))?; let truncated = text.chars().count() > MAX_TEMPLATE_CHARS; let bounded = text.chars().take(MAX_TEMPLATE_CHARS).collect::<String>();
    Ok(json!({"schema":"narada.sop_mcp.template_candidate.v1","status":"ok","sop_id":id,"path":path.to_string_lossy(),"raw_yaml":bounded,"truncated":truncated,"import_state":"unverified","native_read_only":true}))
}

fn error(code: &str, message: &str) -> Value { json!({"schema":"narada.sop_mcp.error.v1","code":code,"message":message}) }
fn tool(name: &str, description: &str, schema: Value, read_only: bool) -> Value { json!({"name":name,"description":description,"annotations":{"title":name,"readOnlyHint":read_only,"destructiveHint":!read_only,"idempotentHint":read_only,"openWorldHint":false},"inputSchema":schema,"outputSchema":{"type":"object","additionalProperties":true}}) }

