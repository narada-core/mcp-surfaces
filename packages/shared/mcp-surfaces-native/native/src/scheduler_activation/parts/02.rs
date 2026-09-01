fn with_prepared<F>(root: &Path, action: F) -> Result<Value, Value>
where
    F: FnOnce(&Connection) -> Result<Value, Value>,
{
    let path = db_path(root);
    if !path.exists() {
        return Err(error(
            "scheduler_activation_store_not_prepared",
            "scheduler_activation_store_not_prepared:database_missing",
        ));
    }
    let db = Connection::open(&path)
        .map_err(|cause| db_error("scheduler_activation_store_open_failed", cause))?;
    configure(&db, false)?;
    let version: Option<i64> = db
        .query_row(
            "select schema_version from scheduler_meta where singleton = 1",
            [],
            |row| row.get(0),
        )
        .optional()
        .map_err(|cause| db_error("scheduler_activation_store_inspect_failed", cause))?;
    if version != Some(SCHEMA_VERSION) {
        return Err(error(
            "scheduler_activation_store_not_prepared",
            &format!(
                "scheduler_activation_store_not_prepared:schema_version_{}",
                version
                    .map(|value| value.to_string())
                    .unwrap_or_else(|| "missing".to_string())
            ),
        ));
    }
    action(&db)
}

fn binding_upsert(db: &Connection, args: &Map<String, Value>) -> Result<Value, Value> {
    let spec = normalize_binding(args)?;
    let binding_id = required_string(&spec, "binding_id")?;
    let spec_digest = digest(&Value::Object(spec.clone()));
    let now = now_iso();
    transaction(db, || {
        let existing = query_binding(db, &binding_id)?;
        if let Some(current) = existing {
            let current_digest = current
                .get("spec_digest")
                .and_then(Value::as_str)
                .unwrap_or("");
            let expected = args.get("expected_revision").and_then(Value::as_i64);
            if expected.is_none() {
                if current_digest == spec_digest {
                    return Ok(json!({"schema":"narada.scheduler.binding.v1","binding":current}));
                }
                return Err(error(
                    "scheduler_binding_expected_revision_required",
                    "scheduler_binding_expected_revision_required",
                ));
            }
            let actual = current.get("revision").and_then(Value::as_i64).unwrap_or(0);
            if expected != Some(actual) {
                return Err(error(
                    "scheduler_binding_revision_conflict",
                    &format!(
                        "scheduler_binding_revision_conflict:expected_{}:actual_{actual}",
                        expected.unwrap_or_default()
                    ),
                ));
            }
            db.execute(
                r#"update scheduler_bindings set trigger_kind=?1,source_topic=?2,source_sop_id=?3,terminal_outcomes_json=?4,target_sop_id=?5,target_template_version=?6,concurrency=?7,delay_by_outcome_ms_json=?8,default_delay_ms=?9,retry_base_ms=?10,retry_max_ms=?11,max_attempts=?12,blocked_policy=?13,revision=revision+1,spec_digest=?14,updated_at=?15 where binding_id=?16"#,
                params![
                    text(&spec,"trigger_kind"), text(&spec,"source_topic"), optional_text(&spec,"source_sop_id"), canonical_json(spec.get("terminal_outcomes").unwrap_or(&Value::Array(Vec::new()))),
                    text(&spec,"target_sop_id"), text(&spec,"target_template_version"), text(&spec,"concurrency"), canonical_json(spec.get("delay_by_outcome_ms").unwrap_or(&Value::Object(Map::new()))),
                    integer(&spec,"default_delay_ms"), integer(&spec,"retry_base_ms"), integer(&spec,"retry_max_ms"), integer(&spec,"max_attempts"), "manual_unblock", spec_digest, now, binding_id
                ],
            ).map_err(|cause| db_error("scheduler_binding_update_failed", cause))?;
        } else {
            if args.get("expected_revision").is_some() {
                return Err(error(
                    "scheduler_binding_not_found",
                    "scheduler_binding_not_found",
                ));
            }
            db.execute(
                r#"insert into scheduler_bindings(binding_id,trigger_kind,source_topic,source_sop_id,terminal_outcomes_json,target_sop_id,target_template_version,concurrency,delay_by_outcome_ms_json,default_delay_ms,retry_base_ms,retry_max_ms,max_attempts,blocked_policy,status,revision,spec_digest,created_at,updated_at) values (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,'manual_unblock','active',1,?14,?15,?15)"#,
                params![
                    binding_id, text(&spec,"trigger_kind"), text(&spec,"source_topic"), optional_text(&spec,"source_sop_id"), canonical_json(spec.get("terminal_outcomes").unwrap_or(&Value::Array(Vec::new()))),
                    text(&spec,"target_sop_id"), text(&spec,"target_template_version"), text(&spec,"concurrency"), canonical_json(spec.get("delay_by_outcome_ms").unwrap_or(&Value::Object(Map::new()))),
                    integer(&spec,"default_delay_ms"), integer(&spec,"retry_base_ms"), integer(&spec,"retry_max_ms"), integer(&spec,"max_attempts"), spec_digest, now
                ],
            ).map_err(|cause| db_error("scheduler_binding_insert_failed", cause))?;
        }
        let binding = require_binding(db, &binding_id)?;
        Ok(json!({"schema":"narada.scheduler.binding.v1","binding":binding}))
    })
}

fn normalize_binding(args: &Map<String, Value>) -> Result<Map<String, Value>, Value> {
    let trigger_kind = required(args, "trigger_kind")?;
    if !matches!(
        trigger_kind.as_str(),
        "bootstrap" | "completion" | "domain_event"
    ) {
        return Err(error(
            "scheduler_binding_trigger_kind_invalid",
            "scheduler_binding_trigger_kind_invalid",
        ));
    }
    let concurrency = required(args, "concurrency")?;
    if !matches!(concurrency.as_str(), "singleton" | "partitioned") {
        return Err(error(
            "scheduler_binding_concurrency_invalid",
            "scheduler_binding_concurrency_invalid",
        ));
    }
    let mut terminal = args
        .get("terminal_outcomes")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .filter_map(|value| {
            value
                .as_str()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(ToString::to_string)
        })
        .collect::<Vec<_>>();
    terminal.sort();
    terminal.dedup();
    let mut delays = BTreeMap::new();
    for (key, value) in args
        .get("delay_by_outcome_ms")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default()
    {
        let Some(delay) = value.as_i64().filter(|delay| *delay >= 0) else {
            return Err(error(
                "scheduler_binding_delay_invalid",
                "scheduler_binding_delay_invalid",
            ));
        };
        delays.insert(key, json!(delay));
    }
    let retry_base = nonnegative(args, "retry_base_ms", 1_000)?;
    let retry_max = nonnegative(args, "retry_max_ms", 300_000)?;
    if retry_max < retry_base {
        return Err(error("retry_max_ms_below_base", "retry_max_ms_below_base"));
    }
    let max_attempts = nonnegative(args, "max_attempts", 5)?;
    if max_attempts < 1 {
        return Err(error("max_attempts_invalid", "max_attempts_invalid"));
    }
    let mut spec = Map::new();
    spec.insert("binding_id".into(), json!(required(args, "binding_id")?));
    spec.insert("trigger_kind".into(), json!(trigger_kind));
    spec.insert(
        "source_topic".into(),
        json!(required(args, "source_topic")?),
    );
    spec.insert(
        "source_sop_id".into(),
        args.get("source_sop_id")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(|value| json!(value))
            .unwrap_or(Value::Null),
    );
    spec.insert("terminal_outcomes".into(), json!(terminal));
    spec.insert(
        "target_sop_id".into(),
        json!(required(args, "target_sop_id")?),
    );
    spec.insert(
        "target_template_version".into(),
        json!(required(args, "target_template_version")?),
    );
    spec.insert("concurrency".into(), json!(concurrency));
    spec.insert(
        "delay_by_outcome_ms".into(),
        Value::Object(delays.into_iter().collect()),
    );
    spec.insert(
        "default_delay_ms".into(),
        json!(nonnegative(args, "default_delay_ms", 0)?),
    );
    spec.insert("retry_base_ms".into(), json!(retry_base));
    spec.insert("retry_max_ms".into(), json!(retry_max));
    spec.insert("max_attempts".into(), json!(max_attempts));
    spec.insert("blocked_policy".into(), json!("manual_unblock"));
    Ok(spec)
}

fn binding_list(db: &Connection, args: &Map<String, Value>) -> Result<Value, Value> {
    let status = args.get("status").and_then(Value::as_str);
    if let Some(value) = status {
        if !matches!(value, "active" | "paused" | "retired") {
            return Err(error(
                "scheduler_binding_status_invalid",
                "scheduler_binding_status_invalid",
            ));
        }
    }
    let limit = args
        .get("limit")
        .and_then(Value::as_i64)
        .unwrap_or(100)
        .clamp(1, 500);
    let offset = args
        .get("offset")
        .and_then(Value::as_i64)
        .unwrap_or(0)
        .clamp(0, 10_000);
    let mut statement = db
        .prepare(if status.is_some() {
            "select * from scheduler_bindings where status=?1 order by binding_id limit ?2 offset ?3"
        } else {
            "select * from scheduler_bindings order by binding_id limit ?1 offset ?2"
        })
        .map_err(|cause| db_error("scheduler_binding_query_failed", cause))?;
    let bindings = if let Some(status) = status {
        statement.query_map(params![status, limit + 1, offset], binding_from_row)
    } else {
        statement.query_map(params![limit + 1, offset], binding_from_row)
    }
    .map_err(|cause| db_error("scheduler_binding_query_failed", cause))?
    .collect::<rusqlite::Result<Vec<_>>>()
    .map_err(|cause| db_error("scheduler_binding_query_failed", cause))?;
    let has_more = bindings.len() as i64 > limit;
    let bindings = bindings
        .into_iter()
        .take(limit as usize)
        .collect::<Vec<_>>();
    let returned = bindings.len();
    Ok(
        json!({"schema":"narada.scheduler.binding_list.v1","status":"ok","count":returned,"returned":returned,"bindings":bindings,"offset":offset,"limit":limit,"has_more":has_more,"next_offset":if has_more{json!(offset + returned as i64)}else{Value::Null},"bounded":true}),
    )
}

fn binding_show(db: &Connection, args: &Map<String, Value>) -> Result<Value, Value> {
    let binding = require_binding(db, &required(args, "binding_id")?)?;
    Ok(json!({"schema":"narada.scheduler.binding.v1","binding":binding}))
}

