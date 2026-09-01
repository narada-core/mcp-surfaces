
fn native_work_event(
    server: &mut LifecycleServer,
    ticket: &Value,
    event_type: &str,
    causation_id: &str,
    idempotency_key: &str,
    topic: &str,
    extra: &Value,
) -> Result<String, String> {
    let ticket_id = ticket
        .get("ticket_id")
        .and_then(Value::as_str)
        .ok_or("ticket_id_required")?;
    let revision = ticket
        .get("revision")
        .and_then(Value::as_i64)
        .ok_or("ticket_revision_required")?;
    let mut payload = json!({
        "ticket_id":ticket_id,
        "ticket_number":ticket.get("ticket_number"),
        "ticket_revision":revision,
        "ticket_status":ticket.get("status"),
        "event_type":event_type
    });
    if let (Some(target), Some(source)) = (payload.as_object_mut(), extra.as_object()) {
        for (key, value) in source {
            target.insert(key.clone(), value.clone());
        }
    }
    let payload_json = serde_json::to_string(&native_canonical_value(&payload))
        .map_err(|e| format!("event_payload_invalid:{e}"))?;
    if payload_json.len() > 16_384 {
        return Err("event_payload_too_large".to_string());
    }
    let event_id = native_work_stable_id(
        "event",
        &json!({
            "aggregate_kind":"ticket",
            "aggregate_id":ticket_id,
            "aggregate_revision":revision,
            "event_type":event_type,
            "idempotency_key":idempotency_key
        }),
        32,
    );
    let timestamp = now();
    let connection = server.connection_mut()?;
    connection
        .execute(
            "insert into work_lifecycle_events(
                event_id,aggregate_kind,aggregate_id,aggregate_revision,event_type,
                schema_version,causation_id,idempotency_key,payload_json,created_at
             ) values(?1,'ticket',?2,?3,?4,1,?5,?6,?7,?8)",
            params![
                &event_id,
                ticket_id,
                revision,
                event_type,
                causation_id,
                format!("event:{idempotency_key}"),
                &payload_json,
                &timestamp
            ],
        )
        .map_err(db_error)?;
    connection
        .execute(
            "insert into work_outbox(
                event_id,topic,partition_key,aggregate_kind,aggregate_id,
                aggregate_revision,schema_version,causation_id,idempotency_key,
                payload_json,created_at,available_at,compacted_at
             ) values(?1,?2,?3,'ticket',?3,?4,1,?5,?6,?7,?8,?8,null)",
            params![
                &event_id,
                topic,
                ticket_id,
                revision,
                causation_id,
                format!("outbox:{idempotency_key}"),
                &payload_json,
                &timestamp
            ],
        )
        .map_err(db_error)?;
    Ok(event_id)
}

fn native_work_ticket_list(server: &LifecycleServer, args: Value) -> Result<Value, String> {
    let limit = args
        .get("limit")
        .and_then(Value::as_i64)
        .unwrap_or(100)
        .clamp(1, 500);
    let offset = args
        .get("offset")
        .and_then(Value::as_i64)
        .unwrap_or(0)
        .clamp(0, 100_000);
    let status = args
        .get("status")
        .and_then(Value::as_str)
        .map(ToString::to_string);
    if let Some(value) = status.as_deref() {
        if !matches!(
            value,
            "actionable"
                | "effect_claimed"
                | "waiting_on_draft"
                | "waiting_on_task"
                | "blocked"
                | "resolved"
        ) {
            return Err("ticket_status_invalid".to_string());
        }
    }
    let tickets = server.query_objects(
        "select ticket_id,ticket_number,status,revision,summary,
                resolution_code,blocker_code,created_at,updated_at,terminal_at
           from tickets
          where (?1 is null or status=?1)
          order by ticket_number desc limit ?2 offset ?3",
        params![status.as_deref(), limit, offset],
    )?;
    Ok(json!({
        "schema":"narada.work_lifecycle.ticket_list.v1",
        "count":tickets.len(),
        "tickets":tickets,
        "limit":limit,
        "offset":offset
    }))
}

fn native_work_ticket_show(
    server: &LifecycleServer,
    args: Value,
) -> Result<Value, String> {
    let ticket = native_work_ticket_by_args(server, &args)?;
    let ticket_id = ticket
        .get("ticket_id")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let sources = server.query_objects(
        "select source_id,ticket_id,source_kind,source_scope,
                immutable_source_id,source_ref_json,policy_version,receipt_id,admitted_at
           from ticket_sources where ticket_id=?1
          order by admitted_at,source_id limit 50",
        params![ticket_id],
    )?;
    let sources = sources
        .into_iter()
        .map(|mut source| {
            if let Some(text) = source.get("source_ref_json").and_then(Value::as_str) {
                if let Ok(value) = serde_json::from_str::<Value>(text) {
                    if let Some(object) = source.as_object_mut() {
                        object.remove("source_ref_json");
                        object.insert("source_ref".to_string(), value);
                    }
                }
            }
            source
        })
        .collect::<Vec<_>>();
    let task_links = server.query_objects(
        "select link.ticket_id,link.task_id,task.task_number,
                link.link_kind,link.status as link_status,task.status as task_status,
                task.revision as task_revision,spec.title,link.linked_at,link.terminal_at
           from ticket_task_links link
           join task_lifecycle task on task.task_id=link.task_id
           left join task_specs spec on spec.task_id=task.task_id
          where link.ticket_id=?1 order by link.linked_at,link.task_id limit 50",
        params![ticket_id],
    )?;
    let draft_rows = server.query_objects(
        "select ticket_id,draft_id,effect_claim_id,draft_ref_json,receipt_id,
                disposition,disposition_evidence_kind,disposition_evidence_id,
                disposition_evidence_json,created_at,disposed_at
           from ticket_draft_refs where ticket_id=?1
          order by created_at,draft_id limit 50",
        params![ticket_id],
    )?;
    let draft_refs = draft_rows
        .into_iter()
        .map(|mut row| {
            for (source, target) in [
                ("draft_ref_json", "draft_ref"),
                ("disposition_evidence_json", "disposition_evidence"),
            ] {
                if let Some(text) = row.get(source).and_then(Value::as_str) {
                    if let Ok(value) = serde_json::from_str::<Value>(text) {
                        if let Some(object) = row.as_object_mut() {
                            object.remove(source);
                            object.insert(target.to_string(), value);
                        }
                    }
                }
            }
            row
        })
        .collect::<Vec<_>>();
    Ok(json!({
        "schema":"narada.work_lifecycle.ticket.v1",
        "ticket":ticket,
        "sources":sources,
        "task_links":task_links,
        "draft_refs":draft_refs
    }))
}
