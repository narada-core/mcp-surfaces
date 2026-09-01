fn stable_id(format: &str, events: &[Event]) -> String {
    let bytes = serde_json::to_vec(&json!({"format":format,"events":events})).unwrap_or_default();
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    let digest = hasher.finalize();
    format!(
        "analysis_{}",
        digest
            .iter()
            .take(6)
            .map(|b| format!("{b:02x}"))
            .collect::<String>()
    )
}
fn timeline(analysis: &Value) -> Vec<Event> {
    analysis
        .get("timeline")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|value| serde_json::from_value(value.clone()).ok())
        .collect()
}
fn normalize_enum(
    value: Option<&Value>,
    fallback: &str,
    allowed: &[&str],
    code: &str,
) -> Result<String, Value> {
    let selected = value.and_then(Value::as_str).unwrap_or(fallback);
    if allowed.contains(&selected) {
        Ok(selected.to_string())
    } else {
        Err(diagnostic(code, &format!("{code}:{selected}")))
    }
}
fn bounded(value: Option<&Value>, fallback: usize, min: usize, max: usize) -> usize {
    let candidate = value
        .and_then(Value::as_i64)
        .map(|n| n as usize)
        .unwrap_or(fallback);
    candidate.clamp(min, max)
}
fn now_iso() -> String {
    OffsetDateTime::now_utc()
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_else(|_| "1970-01-01T00:00:00Z".to_string())
}
fn input_schema() -> Value {
    trace_query_schema(json!({}))
}
fn trace_query_schema(extra: Value) -> Value {
    let mut properties = Map::new();
    properties.insert(
        "analysis_id".to_string(),
        json!({"type":"string","minLength":1,"maxLength":512}),
    );
    properties.insert(
        "format".to_string(),
        json!({"type":"string","enum":FORMATS}),
    );
    properties.insert(
        "events".to_string(),
        json!({"type":"array","maxItems":500,"items":event_input_schema()}),
    );
    properties.insert(
        "jsonl".to_string(),
        json!({"type":"string","maxLength":1048576}),
    );
    properties.insert(
        "transcript".to_string(),
        json!({"type":"array","maxItems":500,"items":event_input_schema()}),
    );
    if let Some(extra) = extra.as_object() {
        properties.extend(extra.clone());
    }
    json!({"type":"object","properties":properties,"additionalProperties":false})
}
fn event_input_schema() -> Value {
    json!({
        "type":"object",
        "properties":{
            "event_id":{"type":"string","maxLength":512},
            "id":{"type":"string","maxLength":512},
            "timestamp":{"type":"string","maxLength":128},
            "input_adapter":{"type":"string","maxLength":256},
            "source":{"type":"string","maxLength":256},
            "kind":{"type":"string","maxLength":256},
            "type":{"type":"string","maxLength":256},
            "event":{"type":"string","maxLength":256},
            "role":{"type":"string","maxLength":256},
            "status":{"type":"string","maxLength":256},
            "outcome":{"type":"string","maxLength":256},
            "surface_id":{"type":"string","maxLength":512},
            "tool_name":{"type":"string","maxLength":512},
            "name":{"type":"string","maxLength":512},
            "namespace":{"type":"string","maxLength":512},
            "duration_ms":{"type":"number","minimum":0},
            "duration":{"type":"number","minimum":0},
            "elapsed_ms":{"type":"number","minimum":0},
            "message":{"type":"string","maxLength":32768},
            "content":{"type":"string","maxLength":32768},
            "error":{"type":"string","maxLength":32768}
        },
        "additionalProperties":false
    })
}
fn tool(name: &str, description: &str, schema: Value) -> Value {
    json!({"name":name,"description":description,"inputSchema":schema,"annotations":{"title":name,"readOnlyHint":true,"destructiveHint":false,"idempotentHint":true,"openWorldHint":false},"outputSchema":{"type":"object","additionalProperties":true}})
}
fn memory_tool(name: &str, description: &str, schema: Value, _required: &[&str]) -> Value {
    tool(name, description, schema)
}
fn guidance_tool() -> Value {
    tool(
        "runtime_introspection_guidance",
        "Show model-facing operating guidance for runtime introspection workflows.",
        json!({"type":"object","properties":{"workflow":{"type":"string"},"tool":{"type":"string"}},"additionalProperties":false}),
    )
}
fn diagnostic(code: &str, message: &str) -> Value {
    json!({"code":code,"message":message})
}

fn database_path(root: &Path) -> std::path::PathBuf {
    root.join(".narada")
        .join("runtime")
        .join("mcp-runtime-observer")
        .join("observations.db")
}
fn open_database(root: &Path) -> Result<Connection, Value> {
    let path = database_path(root);
    if !path.exists() {
        return Err(diagnostic(
            "runtime_introspection_memory_store_unavailable",
            "runtime_introspection_memory_store_unavailable",
        ));
    }
    Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY).map_err(|_| {
        diagnostic(
            "runtime_introspection_memory_store_unavailable",
            "runtime_introspection_memory_store_unavailable",
        )
    })
}
fn memory_status(root: &Path) -> Result<Value, Value> {
    let path = database_path(root);
    let database_updated_at = std::fs::metadata(&path)
        .and_then(|metadata| metadata.modified())
        .ok()
        .map(OffsetDateTime::from)
        .and_then(|instant| {
            instant
                .format(&time::format_description::well_known::Rfc3339)
                .ok()
        });
    let db = open_database(root)?;
    let process=query_one(&db,"SELECT COUNT(*) samples,MAX(sampled_at_ms) last_sample_at_ms,COUNT(DISTINCT owner_id) sampled_owners FROM process_samples",params![])?;
    let workers=query_one(&db,"SELECT COUNT(*) samples,MAX(sampled_at_ms) last_sample_at_ms,COUNT(DISTINCT owner_id) sampled_owners FROM worker_samples",params![])?;
    let owners=query_one(&db,"SELECT COUNT(*) owners,SUM(CASE WHEN active=1 THEN 1 ELSE 0 END) active_owners FROM owners",params![])?;
    let incidents=query_one(&db,"SELECT COUNT(*) incidents,SUM(CASE WHEN status='open' THEN 1 ELSE 0 END) open_incidents FROM incidents",params![])?;
    let observer = observer_overhead(&db)?;
    let last = process["last_sample_at_ms"]
        .as_i64()
        .unwrap_or(0)
        .max(workers["last_sample_at_ms"].as_i64().unwrap_or(0));
    let now_ms = (OffsetDateTime::now_utc().unix_timestamp_nanos() / 1_000_000) as i64;
    let status = if last == 0 {
        "empty"
    } else if now_ms.saturating_sub(last) > 30_000 {
        "stale"
    } else {
        "ready"
    };
    let last_sample_at = if last > 0 {
        OffsetDateTime::from_unix_timestamp_nanos(last as i128 * 1_000_000)
            .ok()
            .and_then(|instant| {
                instant
                    .format(&time::format_description::well_known::Rfc3339)
                    .ok()
            })
            .map(Value::String)
            .unwrap_or(Value::Null)
    } else {
        Value::Null
    };
    let mut result = json!({"schema":"narada.runtime_introspection.memory_status.v1","status":status,"observed_at":now_iso(),"database_updated_at":database_updated_at,"last_sample_at":last_sample_at,"process":process,"workers":workers,"observer":observer,"authority":"server_bound_site","response":"evidence_only_no_automatic_actuation"});
    if let Some(obj) = result.as_object_mut() {
        obj.extend(owners);
        obj.extend(incidents);
    }
    Ok(result)
}
fn memory_owners(args: &Map<String, Value>, root: &Path) -> Result<Value, Value> {
    let db = open_database(root)?;
    let limit = bounded(args.get("limit"), 50, 1, 200) as i64;
    let active = if args.get("active_only").and_then(Value::as_bool) == Some(false) {
        0
    } else {
        1
    };
    let items=query_rows(&db,"SELECT o.owner_id,o.site_id,o.authority_ref,o.owner_kind,o.pid,o.process_started_at,CAST(o.process_creation_ticks AS TEXT) process_creation_ticks,o.parent_owner_id,o.surface_id,o.instance_id,o.generation_id,o.carrier_session_id,o.executable_name,o.observed_at,o.active,(SELECT private_bytes FROM process_samples p WHERE p.owner_id=COALESCE(o.parent_owner_id,o.owner_id) ORDER BY sampled_at_ms DESC LIMIT 1) private_bytes,(SELECT working_set_bytes FROM process_samples p WHERE p.owner_id=COALESCE(o.parent_owner_id,o.owner_id) ORDER BY sampled_at_ms DESC LIMIT 1) working_set_bytes,(SELECT heap_used_bytes FROM worker_samples w WHERE w.owner_id=o.owner_id ORDER BY sampled_at_ms DESC LIMIT 1) heap_used_bytes,(SELECT sampled_at_ms FROM process_samples p WHERE p.owner_id=COALESCE(o.parent_owner_id,o.owner_id) ORDER BY sampled_at_ms DESC LIMIT 1) last_sample_at_ms FROM owners o WHERE (?1=0 OR active=1) ORDER BY active DESC,last_sample_at_ms DESC LIMIT ?2",params![active,limit])?;
    Ok(
        json!({"schema":"narada.runtime_introspection.memory_owners.v1","items":items,"count":items.len(),"limit":limit}),
    )
}
fn memory_timeline(args: &Map<String, Value>, root: &Path) -> Result<Value, Value> {
    let db = open_database(root)?;
    let owner = require_string(args, "owner_id")?;
    let limit = bounded(args.get("limit"), 100, 1, 500) as i64;
    let before = args
        .get("before_ms")
        .and_then(Value::as_i64)
        .unwrap_or(i64::MAX);
    let items=query_rows(&db,"SELECT sampled_at_ms,'process' sample_kind,private_bytes primary_bytes,working_set_bytes,commit_bytes,handle_count,thread_count,NULL heap_used_bytes,NULL external_bytes,NULL array_buffers_bytes FROM process_samples WHERE owner_id=?1 AND sampled_at_ms<?2 UNION ALL SELECT sampled_at_ms,'worker',heap_used_bytes,NULL,NULL,NULL,NULL,heap_used_bytes,external_bytes,array_buffers_bytes FROM worker_samples WHERE owner_id=?1 AND sampled_at_ms<?2 ORDER BY sampled_at_ms DESC LIMIT ?3",params![owner,before,limit])?;
    Ok(
        json!({"schema":"narada.runtime_introspection.memory_timeline.v1","owner_id":owner,"items":items,"count":items.len(),"next_before_ms":if items.len()==limit as usize {items.last().and_then(|v|v["sampled_at_ms"].clone().as_i64())} else {None}}),
    )
}
fn memory_attribution(args: &Map<String, Value>, root: &Path) -> Result<Value, Value> {
    let db = open_database(root)?;
    let owner = require_string(args, "owner_id")?;
    let process=query_one(&db,"SELECT * FROM process_samples WHERE owner_id IN (?1,COALESCE((SELECT parent_owner_id FROM owners WHERE owner_id=?1),?1)) ORDER BY CASE WHEN owner_id=?1 THEN 0 ELSE 1 END,sampled_at_ms DESC LIMIT 1",params![owner])?;
    let worker = query_one(
        &db,
        "SELECT * FROM worker_samples WHERE owner_id=?1 ORDER BY sampled_at_ms DESC LIMIT 1",
        params![owner],
    )?;
    let private_bytes = number_field(&process, "private_bytes");
    let heap = number_field(&worker, "heap_used_bytes");
    let external = number_field(&worker, "external_bytes");
    let buffers = number_field(&worker, "array_buffers_bytes");
    let attributed = private_bytes.min(heap + external);
    let ratio = if private_bytes > 0 {
        attributed as f64 / private_bytes as f64
    } else {
        0.0
    };
    let (classification, confidence) = if ratio >= 0.7 {
        ("direct", 0.92)
    } else if ratio >= 0.4 {
        ("partial", 0.7)
    } else {
        ("residual", 0.45)
    };
    Ok(
        json!({"schema":"narada.runtime_introspection.memory_attribution.v1","owner_id":owner,"attribution":classification,"confidence":confidence,"process_private_bytes":if private_bytes>0{json!(private_bytes)}else{Value::Null},"worker_heap_used_bytes":if heap>0{json!(heap)}else{Value::Null},"worker_external_bytes":if external>0{json!(external)}else{Value::Null},"worker_array_buffers_bytes":if buffers>0{json!(buffers)}else{Value::Null},"attributed_v8_bytes":if attributed>0{json!(attributed)}else{Value::Null},"non_v8_residual_bytes":if private_bytes>0{json!(private_bytes-attributed)}else{Value::Null},"note":"array_buffers_are_reported_as_evidence_but_not_added_to_external_to_avoid_double_counting"}),
    )
}
fn memory_incidents(args: &Map<String, Value>, root: &Path) -> Result<Value, Value> {
    let db = open_database(root)?;
    let limit = bounded(args.get("limit"), 50, 1, 200) as i64;
    let status = args.get("status").and_then(Value::as_str).unwrap_or("open");
    let items=query_rows(&db,"SELECT * FROM incidents WHERE (?1='all' OR status=?1) ORDER BY updated_at_ms DESC LIMIT ?2",params![status,limit])?;
    Ok(
        json!({"schema":"narada.runtime_introspection.memory_incidents.v1","status":status,"items":items,"count":items.len()}),
    )
}
