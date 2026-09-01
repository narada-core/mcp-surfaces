fn memory_incident_show(args: &Map<String, Value>, root: &Path) -> Result<Value, Value> {
    let db = open_database(root)?;
    let id = require_string(args, "incident_id")?;
    let incident = query_one(
        &db,
        "SELECT * FROM incidents WHERE incident_id=?1",
        params![id],
    )?;
    if incident.is_empty() {
        return Err(diagnostic(
            "runtime_introspection_memory_incident_not_found",
            "runtime_introspection_memory_incident_not_found",
        ));
    }
    let mut evidence=query_rows(&db,"SELECT evidence_id,created_at_ms,evidence_type,payload_json FROM evidence WHERE incident_id=?1 ORDER BY created_at_ms",params![id])?;
    for item in &mut evidence {
        project_evidence_payload(item)?;
    }
    let artifacts=query_rows(&db,"SELECT artifact_id,created_at_ms,path,kind,bytes FROM artifacts WHERE incident_id=?1 ORDER BY created_at_ms",params![id])?;
    Ok(
        json!({"schema":"narada.runtime_introspection.memory_incident.v1","incident":incident,"evidence":evidence,"artifacts":artifacts}),
    )
}
fn project_evidence_payload(item: &mut Value) -> Result<(), Value> {
    let object = item.as_object_mut().ok_or_else(|| {
        diagnostic(
            "runtime_introspection_memory_evidence_corrupt",
            "runtime_introspection_memory_evidence_corrupt",
        )
    })?;
    let text = object
        .remove("payload_json")
        .and_then(|value| value.as_str().map(ToString::to_string))
        .ok_or_else(|| {
            diagnostic(
                "runtime_introspection_memory_evidence_corrupt",
                "runtime_introspection_memory_evidence_corrupt",
            )
        })?;
    let payload = serde_json::from_str::<Value>(&text).map_err(|_| {
        diagnostic(
            "runtime_introspection_memory_evidence_corrupt",
            "runtime_introspection_memory_evidence_corrupt",
        )
    })?;
    object.insert("payload".to_string(), payload);
    Ok(())
}
fn table_exists(db: &Connection, name: &str) -> Result<bool, Value> {
    let row = query_one(
        db,
        "SELECT 1 present FROM sqlite_master WHERE type='table' AND name=?1",
        params![name],
    )?;
    Ok(row.get("present").and_then(Value::as_i64) == Some(1))
}
fn observer_overhead(db: &Connection) -> Result<Value, Value> {
    if !table_exists(db, "observer_cycles")? {
        return Ok(
            json!({"cycles":0,"last_cycle_at_ms":Value::Null,"average_cycle_duration_ms":Value::Null,"p95_cycle_duration_ms":Value::Null,"maximum_cycle_duration_ms":Value::Null,"average_cpu_percent":Value::Null,"average_single_core_cpu_percent":Value::Null,"logical_processor_count":Value::Null,"private_bytes":Value::Null,"sampled_processes":Value::Null}),
        );
    }
    let cutoff =
        (OffsetDateTime::now_utc().unix_timestamp_nanos() / 1_000_000) as i64 - 60 * 60_000;
    let cycles = query_rows(db, "SELECT started_at_ms,duration_ms,sampled_processes FROM observer_cycles WHERE started_at_ms>=?1 ORDER BY started_at_ms DESC LIMIT 360", params![cutoff])?;
    let mut durations = cycles
        .iter()
        .filter_map(|row| {
            row["duration_ms"]
                .as_f64()
                .or_else(|| row["duration_ms"].as_i64().map(|value| value as f64))
        })
        .collect::<Vec<_>>();
    durations.sort_by(|left, right| left.partial_cmp(right).unwrap_or(Ordering::Equal));
    let process = query_rows(db, "SELECT sampled_at_ms,cpu_time_ms,private_bytes FROM process_samples WHERE owner_id='observer-overhead' AND sampled_at_ms>=?1 ORDER BY sampled_at_ms ASC", params![cutoff])?;
    let first = process.first();
    let last = process.last();
    let elapsed = first
        .zip(last)
        .map(|(first, last)| {
            number_field(last.as_object().unwrap_or(&Map::new()), "sampled_at_ms")
                - number_field(first.as_object().unwrap_or(&Map::new()), "sampled_at_ms")
        })
        .unwrap_or(0);
    let cpu = first
        .zip(last)
        .map(|(first, last)| {
            number_field(last.as_object().unwrap_or(&Map::new()), "cpu_time_ms")
                - number_field(first.as_object().unwrap_or(&Map::new()), "cpu_time_ms")
        })
        .unwrap_or(0);
    let logical = std::thread::available_parallelism()
        .map(|count| count.get())
        .unwrap_or(1);
    let single_core = if elapsed > 0 {
        Some(cpu as f64 / elapsed as f64 * 100.0)
    } else {
        None
    };
    let average = if durations.is_empty() {
        None
    } else {
        Some((durations.iter().sum::<f64>() / durations.len() as f64).round() as i64)
    };
    let p95 = if durations.is_empty() {
        None
    } else {
        Some(durations[((durations.len() as f64 * 0.95).ceil() as usize).saturating_sub(1)])
    };
    Ok(json!({
        "cycles":cycles.len(),
        "last_cycle_at_ms":cycles.first().map(|row|row["started_at_ms"].clone()).unwrap_or(Value::Null),
        "average_cycle_duration_ms":average,
        "p95_cycle_duration_ms":p95,
        "maximum_cycle_duration_ms":durations.last().copied(),
        "average_cpu_percent":single_core.map(|value| ((value / logical as f64) * 1000.0).round() / 1000.0),
        "average_single_core_cpu_percent":single_core.map(|value| (value * 1000.0).round() / 1000.0),
        "logical_processor_count":logical,
        "private_bytes":last.map(|row|row["private_bytes"].clone()).unwrap_or(Value::Null),
        "sampled_processes":cycles.first().map(|row|row["sampled_processes"].clone()).unwrap_or(Value::Null),
        "window":"last_hour_bounded_to_360_cycles"
    }))
}
fn number_field(value: &Map<String, Value>, key: &str) -> i64 {
    value
        .get(key)
        .and_then(|v| v.as_i64().or_else(|| v.as_f64().map(|n| n as i64)))
        .unwrap_or(0)
}
fn require_string(args: &Map<String, Value>, key: &str) -> Result<String, Value> {
    args.get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(str::to_string)
        .ok_or_else(|| {
            diagnostic(
                "runtime_introspection_memory_argument_required",
                &format!("runtime_introspection_memory_argument_required:{key}"),
            )
        })
}
fn query_one<P: rusqlite::Params>(
    db: &Connection,
    sql: &str,
    params: P,
) -> Result<Map<String, Value>, Value> {
    let mut statement = db.prepare(sql).map_err(|_| {
        diagnostic(
            "runtime_introspection_memory_store_unavailable",
            "runtime_introspection_memory_store_unavailable",
        )
    })?;
    let mut rows = statement.query(params).map_err(|_| {
        diagnostic(
            "runtime_introspection_memory_store_unavailable",
            "runtime_introspection_memory_store_unavailable",
        )
    })?;
    if let Some(row) = rows.next().map_err(|_| {
        diagnostic(
            "runtime_introspection_memory_store_unavailable",
            "runtime_introspection_memory_store_unavailable",
        )
    })? {
        row_to_map(row).map_err(|_| {
            diagnostic(
                "runtime_introspection_memory_store_unavailable",
                "runtime_introspection_memory_store_unavailable",
            )
        })
    } else {
        Ok(Map::new())
    }
}
fn query_rows<P: rusqlite::Params>(
    db: &Connection,
    sql: &str,
    params: P,
) -> Result<Vec<Value>, Value> {
    let mut statement = db.prepare(sql).map_err(|_| {
        diagnostic(
            "runtime_introspection_memory_store_unavailable",
            "runtime_introspection_memory_store_unavailable",
        )
    })?;
    let mut rows = statement.query(params).map_err(|_| {
        diagnostic(
            "runtime_introspection_memory_store_unavailable",
            "runtime_introspection_memory_store_unavailable",
        )
    })?;
    let mut result = Vec::new();
    while let Some(row) = rows.next().map_err(|_| {
        diagnostic(
            "runtime_introspection_memory_store_unavailable",
            "runtime_introspection_memory_store_unavailable",
        )
    })? {
        result.push(Value::Object(row_to_map(row).map_err(|_| {
            diagnostic(
                "runtime_introspection_memory_store_unavailable",
                "runtime_introspection_memory_store_unavailable",
            )
        })?));
    }
    Ok(result)
}
fn row_to_map(row: &rusqlite::Row<'_>) -> rusqlite::Result<Map<String, Value>> {
    let mut result = Map::new();
    let count = row.as_ref().column_count();
    for index in 0..count {
        let name = row.as_ref().column_name(index)?.to_string();
        let value = match row.get_ref(index)? {
            ValueRef::Null => Value::Null,
            ValueRef::Integer(v) => json!(v),
            ValueRef::Real(v) => json!(v),
            ValueRef::Text(v) => Value::String(String::from_utf8_lossy(v).to_string()),
            ValueRef::Blob(v) => Value::String(format!("blob:{}", v.len())),
        };
        result.insert(name, value);
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_tool_contract_is_read_only_and_bounded() {
        let tools = list_tools();
        assert_eq!(tools.len(), 14);
        assert_eq!(tools[0]["name"], "runtime_introspection_guidance");
        assert_eq!(tools[7]["name"], "runtime_introspection_show_event");
        assert!(tools
            .iter()
            .all(|tool| tool["annotations"]["readOnlyHint"] == true));
        assert!(tools
            .iter()
            .all(|tool| tool["inputSchema"]["additionalProperties"] == false));
    }

    #[test]
    fn native_analysis_matches_surface_and_refusal_counts() {
        let mut args = Map::new();
        args.insert("format".to_string(), json!("codex-transcript"));
        args.insert(
            "transcript".to_string(),
            json!([
                {"id":"1","timestamp":"2026-06-20T14:00:00.000Z","type":"tool_call","tool_name":"mcp__narada_andrey_local_filesystem.fs_read_file","status":"ok","duration_ms":12},
                {"id":"2","type":"tool_call","tool_name":"mcp__narada_andrey_structured_command.structured_c","status":"refused","duration_ms":3}
            ]),
        );
        let analysis = analyze(&args).unwrap();
        assert_eq!(analysis["summary"]["event_count"], 2);
        assert_eq!(analysis["summary"]["refused_count"], 1);
        assert_eq!(analysis["counts"]["by_surface"]["local-filesystem"], 1);
        assert_eq!(analysis["counts"]["by_surface"]["structured-command"], 1);
        assert_eq!(analysis["summary"]["input_adapters"][0], "codex");
    }

    #[test]
    fn invalid_jsonl_is_refused_with_bounded_diagnostic() {
        let mut args = Map::new();
        args.insert("format".to_string(), json!("codex-jsonl"));
        args.insert("jsonl".to_string(), json!("{\"id\":\"ok\"}\nnot-json"));
        let error = analyze(&args).unwrap_err();
        assert_eq!(error["code"], "runtime_introspection_invalid_jsonl");
    }

    #[test]
    fn incident_evidence_projects_payload_json_as_domain_payload() {
        let mut evidence = json!({"evidence_id":"e1","payload_json":"{\"rss_bytes\":42}"});
        project_evidence_payload(&mut evidence).unwrap();
        assert_eq!(evidence["payload"]["rss_bytes"], 42);
        assert!(evidence.get("payload_json").is_none());
    }
}
