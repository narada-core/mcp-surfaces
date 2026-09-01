fn binding_set_status(
    db: &Connection,
    name: &str,
    args: &Map<String, Value>,
) -> Result<Value, Value> {
    let binding_id = required(args, "binding_id")?;
    let expected = required_integer(args, "expected_revision")?;
    let status = if name.ends_with("_pause") {
        "paused"
    } else if name.ends_with("_resume") {
        "active"
    } else {
        "retired"
    };
    let now = now_iso();
    transaction(db, || {
        let current = require_binding(db, &binding_id)?;
        let actual = current.get("revision").and_then(Value::as_i64).unwrap_or(0);
        if actual != expected {
            return Err(error(
                "scheduler_binding_revision_conflict",
                &format!("scheduler_binding_revision_conflict:expected_{expected}:actual_{actual}"),
            ));
        }
        db.execute("update scheduler_bindings set status=?1,revision=revision+1,updated_at=?2 where binding_id=?3", params![status,now,binding_id])
            .map_err(|cause| db_error("scheduler_binding_status_update_failed", cause))?;
        if status == "paused" {
            db.execute("update scheduler_activations set status='terminal',terminal_outcome='cancelled_binding_paused',lease_owner=null,lease_token=null,lease_expires_at=null,last_error='binding_paused_before_admission',updated_at=?1 where binding_id=?2 and (status='pending' or (status='leased' and lease_expires_at<=?1))", params![now,binding_id])
                .map_err(|cause| db_error("scheduler_binding_quiesce_failed", cause))?;
        }
        Ok(
            json!({"schema":"narada.scheduler.binding.v1","binding":require_binding(db,&binding_id)?}),
        )
    })
}

fn query_binding(db: &Connection, id: &str) -> Result<Option<Value>, Value> {
    db.query_row(
        "select * from scheduler_bindings where binding_id=?1",
        params![id],
        binding_from_row,
    )
    .optional()
    .map_err(|cause| db_error("scheduler_binding_query_failed", cause))
}

fn require_binding(db: &Connection, id: &str) -> Result<Value, Value> {
    query_binding(db, id)?.ok_or_else(|| {
        error(
            "scheduler_binding_not_found",
            &format!("scheduler_binding_not_found:{id}"),
        )
    })
}

fn binding_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Value> {
    let terminal: String = row.get("terminal_outcomes_json")?;
    let delays: String = row.get("delay_by_outcome_ms_json")?;
    Ok(json!({
        "binding_id":row.get::<_,String>("binding_id")?,"trigger_kind":row.get::<_,String>("trigger_kind")?,"source_topic":row.get::<_,String>("source_topic")?,"source_sop_id":row.get::<_,Option<String>>("source_sop_id")?,
        "terminal_outcomes":serde_json::from_str::<Value>(&terminal).unwrap_or_else(|_|json!([])),"target_sop_id":row.get::<_,String>("target_sop_id")?,"target_template_version":row.get::<_,String>("target_template_version")?,
        "concurrency":row.get::<_,String>("concurrency")?,"delay_by_outcome_ms":serde_json::from_str::<Value>(&delays).unwrap_or_else(|_|json!({})),"default_delay_ms":row.get::<_,i64>("default_delay_ms")?,
        "retry_base_ms":row.get::<_,i64>("retry_base_ms")?,"retry_max_ms":row.get::<_,i64>("retry_max_ms")?,"max_attempts":row.get::<_,i64>("max_attempts")?,"blocked_policy":"manual_unblock",
        "status":row.get::<_,String>("status")?,"revision":row.get::<_,i64>("revision")?,"spec_digest":row.get::<_,String>("spec_digest")?,"created_at":row.get::<_,String>("created_at")?,"updated_at":row.get::<_,String>("updated_at")?
    }))
}

fn event_admit(db: &Connection, args: &Map<String, Value>) -> Result<Value, Value> {
    let event = normalize_event(args)?;
    let event_id = required_string(&event, "event_id")?;
    let event_digest = digest(&Value::Object(event.clone()));
    let now = now_iso();
    transaction(db, || {
        let existing_digest: Option<String> = db
            .query_row(
                "select event_digest from scheduler_source_events where event_id=?1",
                params![event_id],
                |row| row.get(0),
            )
            .optional()
            .map_err(|cause| db_error("scheduler_event_query_failed", cause))?;
        if let Some(existing) = existing_digest.as_deref() {
            if existing != event_digest {
                return Err(error(
                    "scheduler_event_idempotency_conflict",
                    &format!("scheduler_event_idempotency_conflict:{event_id}"),
                ));
            }
        } else {
            db.execute(
                "insert into scheduler_source_events(event_id,topic,partition_key,aggregate_id,aggregate_revision,schema_version,causation_id,idempotency_key,payload_json,event_digest,occurred_at,admitted_at) values (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12)",
                params![event_id,text(&event,"topic"),text(&event,"partition_key"),text(&event,"aggregate_id"),integer(&event,"aggregate_revision"),integer(&event,"schema_version"),text(&event,"causation_id"),text(&event,"idempotency_key"),canonical_json(event.get("payload").unwrap_or(&json!({}))),event_digest,text(&event,"occurred_at"),now],
            ).map_err(|cause| db_error("scheduler_event_insert_failed", cause))?;
        }
        let mut statement = db.prepare("select * from scheduler_bindings where source_topic=?1 and status='active' order by binding_id")
            .map_err(|cause| db_error("scheduler_binding_query_failed", cause))?;
        let bindings = statement
            .query_map(params![text(&event, "topic")], binding_from_row)
            .map_err(|cause| db_error("scheduler_binding_query_failed", cause))?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(|cause| db_error("scheduler_binding_query_failed", cause))?;
        for binding in bindings {
            if !binding_matches(&binding, &event) {
                continue;
            }
            let binding_id = binding
                .get("binding_id")
                .and_then(Value::as_str)
                .unwrap_or("");
            let activation_id = stable_id(
                "activation",
                &json!({"binding_id":binding_id,"source_event_id":event_id}),
            );
            if query_activation(db, &activation_id)?.is_some() {
                continue;
            }
            let payload = event
                .get("payload")
                .and_then(Value::as_object)
                .cloned()
                .unwrap_or_default();
            let outcome = payload
                .get("outcome")
                .and_then(Value::as_str)
                .unwrap_or("default");
            let delay = delay_for(&binding, outcome, &payload);
            let occurred = parse_iso(text(&event, "occurred_at").as_str())?;
            let due_at = format_iso(occurred + Duration::milliseconds(delay));
            let blocked = outcome == "blocked";
            let partition_key =
                if binding.get("concurrency").and_then(Value::as_str) == Some("singleton") {
                    binding_id.to_string()
                } else {
                    text(&event, "partition_key")
                };
            db.execute(
                "insert into scheduler_activations(activation_id,binding_id,source_event_id,occurrence_key,target_sop_id,target_template_version,partition_key,due_at,status,attempt_count,lease_owner,lease_token,lease_expires_at,sop_run_id,terminal_outcome,last_error,created_at,updated_at) values (?1,?2,?3,?4,?5,?6,?7,?8,?9,0,null,null,null,null,null,?10,?11,?11)",
                params![activation_id,binding_id,event_id,format!("{binding_id}:{event_id}"),binding.get("target_sop_id").and_then(Value::as_str).unwrap_or(""),binding.get("target_template_version").and_then(Value::as_str).unwrap_or(""),partition_key,due_at,if blocked{"blocked"}else{"pending"},if blocked{Some("blocked_outcome_requires_explicit_unblock")}else{None},now],
            ).map_err(|cause| db_error("scheduler_activation_insert_failed", cause))?;
        }
        let activations = list_activations(db, None, None, Some(&event_id), None, 500, 0)?;
        Ok(
            json!({"schema":"narada.scheduler.event_admission.v1","status":if existing_digest.is_some(){"replayed"}else{"admitted"},"event_id":event_id,"activation_count":activations.len(),"activations":activations}),
        )
    })
}

fn normalize_event(args: &Map<String, Value>) -> Result<Map<String, Value>, Value> {
    let occurred = parse_iso(&required(args, "occurred_at")?)?;
    let aggregate_revision = required_integer(args, "aggregate_revision")?;
    if aggregate_revision < 0 {
        return Err(error(
            "scheduler_event_aggregate_revision_invalid",
            "scheduler_event_aggregate_revision_invalid",
        ));
    }
    let schema_version = required_integer(args, "schema_version")?;
    if schema_version < 1 {
        return Err(error(
            "scheduler_event_schema_version_invalid",
            "scheduler_event_schema_version_invalid",
        ));
    }
    let payload = args
        .get("payload")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    bounded_json(
        &Value::Object(payload.clone()),
        "scheduler_event_payload",
        MAX_EVENT_BYTES,
    )?;
    let mut event = Map::new();
    for field in [
        "event_id",
        "topic",
        "partition_key",
        "aggregate_id",
        "causation_id",
        "idempotency_key",
    ] {
        event.insert(field.into(), json!(required(args, field)?));
    }
    event.insert("aggregate_revision".into(), json!(aggregate_revision));
    event.insert("schema_version".into(), json!(schema_version));
    event.insert("payload".into(), Value::Object(payload));
    event.insert("occurred_at".into(), json!(format_iso(occurred)));
    Ok(event)
}

fn binding_matches(binding: &Value, event: &Map<String, Value>) -> bool {
    let payload = event.get("payload").and_then(Value::as_object);
    if let Some(expected) = binding.get("source_sop_id").and_then(Value::as_str) {
        if payload
            .and_then(|value| value.get("sop_id"))
            .and_then(Value::as_str)
            != Some(expected)
        {
            return false;
        }
    }
    let outcomes = binding.get("terminal_outcomes").and_then(Value::as_array);
    if let Some(outcomes) = outcomes.filter(|values| !values.is_empty()) {
        let outcome = payload
            .and_then(|value| value.get("outcome"))
            .and_then(Value::as_str)
            .unwrap_or("");
        if !outcomes.iter().any(|value| value.as_str() == Some(outcome)) {
            return false;
        }
    }
    true
}

fn delay_for(binding: &Value, outcome: &str, payload: &Map<String, Value>) -> i64 {
    if outcome == "retryable_failure" {
        let attempt = payload
            .get("attempt")
            .and_then(Value::as_i64)
            .unwrap_or(1)
            .max(1)
            .min(31);
        let base = binding
            .get("retry_base_ms")
            .and_then(Value::as_i64)
            .unwrap_or(1_000);
        let cap = binding
            .get("retry_max_ms")
            .and_then(Value::as_i64)
            .unwrap_or(300_000);
        return base.saturating_mul(1_i64 << (attempt - 1)).min(cap);
    }
    binding
        .get("delay_by_outcome_ms")
        .and_then(Value::as_object)
        .and_then(|values| values.get(outcome))
        .and_then(Value::as_i64)
        .or_else(|| binding.get("default_delay_ms").and_then(Value::as_i64))
        .unwrap_or(0)
}

fn event_show(db: &Connection, args: &Map<String, Value>) -> Result<Value, Value> {
    let event_id = required(args, "event_id")?;
    let event = db
        .query_row(
            "select * from scheduler_source_events where event_id=?1",
            params![event_id],
            event_from_row,
        )
        .optional()
        .map_err(|cause| db_error("scheduler_event_query_failed", cause))?
        .ok_or_else(|| {
            error(
                "scheduler_source_event_not_found",
                &format!("scheduler_source_event_not_found:{event_id}"),
            )
        })?;
    Ok(json!({"schema":"narada.scheduler.source_event.v1","event":event}))
}

fn event_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Value> {
    let payload: String = row.get("payload_json")?;
    Ok(
        json!({"event_id":row.get::<_,String>("event_id")?,"topic":row.get::<_,String>("topic")?,"partition_key":row.get::<_,String>("partition_key")?,"aggregate_id":row.get::<_,String>("aggregate_id")?,"aggregate_revision":row.get::<_,i64>("aggregate_revision")?,"schema_version":row.get::<_,i64>("schema_version")?,"causation_id":row.get::<_,String>("causation_id")?,"idempotency_key":row.get::<_,String>("idempotency_key")?,"payload":serde_json::from_str::<Value>(&payload).unwrap_or_else(|_|json!({})),"occurred_at":row.get::<_,String>("occurred_at")?}),
    )
}

