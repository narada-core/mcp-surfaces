
fn native_work_outbox_list(
    server: &LifecycleServer,
    args: Value,
) -> Result<Value, String> {
    let consumer = required_string(&args, "consumer_id")?;
    let limit = args
        .get("limit")
        .and_then(Value::as_i64)
        .unwrap_or(100)
        .clamp(1, 500);
    let topics = args
        .get("topics")
        .and_then(Value::as_array)
        .map(|values| {
            values
                .iter()
                .filter_map(Value::as_str)
                .filter(|value| !value.trim().is_empty())
                .map(ToString::to_string)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let mut events = if topics.is_empty() {
        server.query_objects(
            "select outbox.event_id,outbox.topic,outbox.partition_key,
                    outbox.aggregate_kind,outbox.aggregate_id,
                    outbox.aggregate_revision,outbox.schema_version,
                    outbox.causation_id,outbox.idempotency_key,
                    outbox.payload_json,outbox.created_at,outbox.available_at,
                    outbox.compacted_at
               from work_outbox outbox
              where outbox.available_at<=?1
                and not exists(
                    select 1 from work_outbox_receipts receipt
                     where receipt.event_id=outbox.event_id
                       and receipt.consumer_id=?2)
              order by outbox.created_at,outbox.event_id limit ?3",
            params![now(),&consumer,limit],
        )?
    } else {
        let placeholders = std::iter::repeat("?")
            .take(topics.len())
            .collect::<Vec<_>>()
            .join(",");
        let sql = format!(
            "select outbox.event_id,outbox.topic,outbox.partition_key,
                    outbox.aggregate_kind,outbox.aggregate_id,
                    outbox.aggregate_revision,outbox.schema_version,
                    outbox.causation_id,outbox.idempotency_key,
                    outbox.payload_json,outbox.created_at,outbox.available_at,
                    outbox.compacted_at
               from work_outbox outbox
              where outbox.topic in ({placeholders})
                and outbox.available_at<=?{next}
                and not exists(
                    select 1 from work_outbox_receipts receipt
                     where receipt.event_id=outbox.event_id
                       and receipt.consumer_id=?{next2})
              order by outbox.created_at,outbox.event_id limit ?{next3}",
            next = topics.len() + 1,
            next2 = topics.len() + 2,
            next3 = topics.len() + 3
        );
        let mut values = topics
            .iter()
            .map(|value| value.clone())
            .collect::<Vec<_>>();
        values.push(now());
        values.push(consumer.clone());
        values.push(limit.to_string());
        let params = values
            .iter()
            .map(|value| value as &dyn rusqlite::ToSql)
            .collect::<Vec<_>>();
        let connection = server.connection()?;
        let mut statement = connection.prepare(&sql).map_err(db_error)?;
        let rows = statement
            .query_map(rusqlite::params_from_iter(params), |row| row_to_object(row))
            .map_err(db_error)?;
        rows.collect::<Result<Vec<_>,_>>().map_err(db_error)?
    };
    for event in &mut events {
        if let Some(text) = event.get("payload_json").and_then(Value::as_str) {
            if let Ok(payload) = serde_json::from_str::<Value>(text) {
                if let Some(object) = event.as_object_mut() {
                    object.remove("payload_json");
                    object.insert("payload".to_string(), payload);
                }
            }
        }
    }
    Ok(json!({
        "schema":"narada.work_lifecycle.outbox.v1",
        "count":events.len(),
        "events":events
    }))
}

fn native_work_outbox_register(
    server: &mut LifecycleServer,
    args: Value,
) -> Result<Value, String> {
    let topic = required_string(&args, "topic")?;
    let consumer = required_string(&args, "consumer_id")?;
    server
        .connection_mut()?
        .execute(
            "insert into work_outbox_consumer_requirements(topic,consumer_id,registered_at)
             values(?1,?2,?3) on conflict(topic,consumer_id) do nothing",
            params![topic,consumer,now()],
        )
        .map_err(db_error)?;
    Ok(json!({"status":"registered"}))
}

fn native_work_outbox_ack(
    server: &mut LifecycleServer,
    args: Value,
) -> Result<Value, String> {
    let event_id = required_string(&args, "event_id")?;
    let consumer = required_string(&args, "consumer_id")?;
    let receipt = args
        .get("receipt")
        .filter(|value| value.is_object())
        .ok_or("outbox_receipt_required")?;
    let receipt_json = native_work_ref_json(receipt, "outbox_receipt")?;
    let exists: Option<String> = server
        .connection()?
        .query_row(
            "select event_id from work_outbox where event_id=?1",
            params![&event_id],
            |row| row.get(0),
        )
        .optional()
        .map_err(db_error)?;
    if exists.is_none() {
        return Err("work_outbox_event_not_found".to_string());
    }
    server
        .connection_mut()?
        .execute(
            "insert into work_outbox_receipts(event_id,consumer_id,processed_at,receipt_json)
             values(?1,?2,?3,?4)
             on conflict(event_id,consumer_id) do update set
               processed_at=excluded.processed_at,receipt_json=excluded.receipt_json",
            params![event_id,consumer,now(),receipt_json],
        )
        .map_err(db_error)?;
    Ok(json!({"status":"acknowledged"}))
}

fn native_work_outbox_compact(
    server: &mut LifecycleServer,
    args: Value,
) -> Result<Value, String> {
    let before = required_string(&args, "before")?;
    OffsetDateTime::parse(&before, &Rfc3339)
        .map_err(|_| "compact_before_invalid".to_string())?;
    let compacted = server
        .connection_mut()?
        .execute(
            "update work_outbox as outbox
                set payload_json='{}',compacted_at=?1
              where outbox.compacted_at is null
                and outbox.created_at<?2
                and exists(
                    select 1 from work_outbox_consumer_requirements requirement
                     where requirement.topic=outbox.topic)
                and not exists(
                    select 1 from work_outbox_consumer_requirements requirement
                     where requirement.topic=outbox.topic
                       and not exists(
                           select 1 from work_outbox_receipts receipt
                            where receipt.event_id=outbox.event_id
                              and receipt.consumer_id=requirement.consumer_id))
            ",
            params![now(),before],
        )
        .map_err(db_error)?;
    Ok(json!({"compacted":compacted}))
}

fn native_work_admit_source_tx(
    server: &mut LifecycleServer,
    args: Value,
) -> Result<Value, String> {
    native_work_transaction(server, |server| native_work_admit_source(server, args))
}

fn native_work_processing_context_tx(
    server: &mut LifecycleServer,
    args: Value,
) -> Result<Value, String> {
    native_work_transaction(server, |server| native_work_processing_context(server, args))
}

fn native_work_admit_proposal_tx(
    server: &mut LifecycleServer,
    args: Value,
) -> Result<Value, String> {
    native_work_transaction(server, |server| native_work_admit_proposal(server, args))
}
