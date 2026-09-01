fn outbox_ack(args: &Map<String, Value>, root: &Path) -> Result<Value, Value> {
    let receipt = args.get("receipt").cloned().ok_or_else(|| {
        diagnostic(
            "sop_outbox_receipt_must_be_object",
            "sop_outbox_receipt_must_be_object",
            json!({}),
        )
    })?;
    if !receipt.is_object() {
        return Err(diagnostic(
            "sop_outbox_receipt_must_be_object",
            "sop_outbox_receipt_must_be_object",
            json!({}),
        ));
    }
    assert_bound(&receipt, "sop_outbox_receipt", MAX_OUTBOX_RECEIPT_BYTES)?;
    transactional(root, |db| {
        let event_id = required_string(args.get("event_id"), "sop_outbox_event_id_required", 512)?;
        let consumer_id = required_string(
            args.get("consumer_id"),
            "sop_outbox_consumer_id_required",
            512,
        )?;
        let receipt_json = canonical_json(&receipt);
        let event = require_outbox_event(db, &event_id)?;
        let topic = event
            .get("topic")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let requirement = db
            .query_row(
                "SELECT * FROM sop_outbox_consumer_requirements WHERE topic = ? AND consumer_id = ?",
                params![topic, consumer_id],
                row_json,
            )
            .optional()
            .map_err(|error| {
                diagnostic(
                    "sop_outbox_consumer_query_failed",
                    &error.to_string(),
                    json!({}),
                )
            })?
            .ok_or_else(|| {
                diagnostic(
                    "sop_outbox_consumer_not_registered",
                    "sop_outbox_consumer_not_registered",
                    json!({"consumer_id":consumer_id,"topic":topic}),
                )
            })?;
        let event_created_at = event
            .get("created_at")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let start_at = requirement
            .get("start_at")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if event_created_at < start_at {
            return Err(diagnostic(
                "sop_outbox_event_before_consumer_start",
                "sop_outbox_event_before_consumer_start",
                json!({"event_id":event_id,"consumer_id":consumer_id,"start_at":start_at}),
            ));
        }
        let existing = db
            .query_row(
                "SELECT * FROM sop_outbox_receipts WHERE event_id = ? AND consumer_id = ?",
                params![event_id, consumer_id],
                row_json,
            )
            .optional()
            .map_err(|error| {
                diagnostic(
                    "sop_outbox_receipt_query_failed",
                    &error.to_string(),
                    json!({}),
                )
            })?;
        if let Some(existing) = existing {
            let recorded = existing.get("receipt_json").cloned().unwrap_or(Value::Null);
            if !recorded.is_object() {
                return Err(diagnostic(
                    "sop_outbox_receipt_corrupt",
                    "sop_outbox_receipt_corrupt",
                    json!({"event_id":event_id,"consumer_id":consumer_id}),
                ));
            }
            if canonical_json(&recorded) != receipt_json {
                return Err(diagnostic(
                    "sop_outbox_receipt_conflict",
                    "sop_outbox_receipt_conflict",
                    json!({"event_id":event_id,"consumer_id":consumer_id}),
                ));
            }
            return Ok(json!({
                "schema":"narada.sop.outbox_ack.v1","event_id":event_id,
                "consumer_id":consumer_id,
                "processed_at":existing.get("processed_at").cloned().unwrap_or(Value::Null),
                "acknowledgement_replayed":true
            }));
        }
        let processed_at = now_iso();
        db.execute(
            "INSERT INTO sop_outbox_receipts(event_id, consumer_id, processed_at, receipt_json) VALUES (?, ?, ?, ?)",
            params![event_id, consumer_id, processed_at, receipt_json],
        )
        .map_err(|error| diagnostic("sop_outbox_receipt_insert_failed", &error.to_string(), json!({})))?;
        Ok(json!({
            "schema":"narada.sop.outbox_ack.v1","event_id":event_id,
            "consumer_id":consumer_id,"processed_at":processed_at,
            "acknowledgement_replayed":false
        }))
    })
}

fn outbox_compact(args: &Map<String, Value>, root: &Path) -> Result<Value, Value> {
    transactional(root, |db| {
        let before = required_string(
            args.get("before"),
            "sop_outbox_compact_before_required",
            512,
        )?;
        let cutoff = normalize_timestamp(&before, "sop_outbox_compact_before_invalid")?;
        let compacted_at = now_iso();
        let compacted = db
            .execute(
                "UPDATE sop_outbox AS outbox SET payload_json = '{}', compacted_at = ? WHERE outbox.compacted_at IS NULL AND outbox.created_at < ? AND EXISTS (SELECT 1 FROM sop_outbox_consumer_requirements requirement WHERE requirement.topic = outbox.topic AND requirement.start_at <= outbox.created_at) AND NOT EXISTS (SELECT 1 FROM sop_outbox_consumer_requirements requirement WHERE requirement.topic = outbox.topic AND requirement.start_at <= outbox.created_at AND NOT EXISTS (SELECT 1 FROM sop_outbox_receipts receipt WHERE receipt.event_id = outbox.event_id AND receipt.consumer_id = requirement.consumer_id))",
                params![compacted_at, cutoff],
            )
            .map_err(|error| diagnostic("sop_outbox_compaction_failed", &error.to_string(), json!({})))?;
        Ok(json!({
            "schema":"narada.sop.outbox_compaction.v1","before":cutoff,
            "compacted_at":compacted_at,"compacted":compacted
        }))
    })
}

pub(crate) fn require_outbox_event(db: &Connection, event_id: &str) -> Result<Value, Value> {
    let row = db
        .query_row(
            "SELECT * FROM sop_outbox WHERE event_id = ?",
            params![event_id],
            row_json,
        )
        .optional()
        .map_err(|error| diagnostic("sop_outbox_query_failed", &error.to_string(), json!({})))?
        .ok_or_else(|| {
            diagnostic(
                "sop_outbox_event_not_found",
                "sop_outbox_event_not_found",
                json!({"event_id":event_id}),
            )
        })?;
    hydrate_outbox_event(row)
}

pub(crate) fn hydrate_outbox_event(row: Value) -> Result<Value, Value> {
    let object = row.as_object().ok_or_else(|| {
        diagnostic(
            "sop_outbox_event_corrupt",
            "sop_outbox_event_corrupt",
            json!({}),
        )
    })?;
    let outcome = required_string(object.get("outcome"), "sop_outbox_event_corrupt", 64)?;
    if !matches!(outcome.as_str(), "completed" | "failed" | "cancelled") {
        return Err(diagnostic(
            "sop_outbox_outcome_corrupt",
            "sop_outbox_outcome_corrupt",
            json!({"outcome":outcome}),
        ));
    }
    let topic = normalize_outbox_topic(
        object
            .get("topic")
            .and_then(Value::as_str)
            .unwrap_or_default(),
    )?;
    let payload = object.get("payload_json").cloned().unwrap_or(Value::Null);
    if !payload.is_object() {
        return Err(diagnostic(
            "sop_outbox_payload_corrupt",
            "sop_outbox_payload_corrupt",
            json!({}),
        ));
    }
    assert_bound(&payload, "sop_outbox_payload", MAX_OUTBOX_PAYLOAD_BYTES)?;
    Ok(json!({
        "schema":"narada.sop.outbox_event.v1",
        "event_id":required_string(object.get("event_id"),"sop_outbox_event_corrupt",512)?,
        "topic":topic,
        "partition_key":required_string(object.get("partition_key"),"sop_outbox_event_corrupt",512)?,
        "run_id":required_string(object.get("run_id"),"sop_outbox_event_corrupt",512)?,
        "sop_id":required_string(object.get("sop_id"),"sop_outbox_event_corrupt",512)?,
        "sop_version":positive_integer_member(object.get("sop_version"),"sop_outbox_event_corrupt")?,
        "occurrence_key":required_string(object.get("occurrence_key"),"sop_outbox_event_corrupt",512)?,
        "outcome":outcome,"payload":payload,
        "created_at":required_string(object.get("created_at"),"sop_outbox_event_corrupt",512)?,
        "available_at":required_string(object.get("available_at"),"sop_outbox_event_corrupt",512)?,
        "compacted_at":optional_string(object.get("compacted_at"))
    }))
}

pub(crate) fn transactional<F>(root: &Path, work: F) -> Result<Value, Value>
where
    F: FnOnce(&Connection) -> Result<Value, Value>,
{
    let mut db = open_db(root)?;
    let transaction = db
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|error| {
            diagnostic(
                "sop_transaction_begin_failed",
                &error.to_string(),
                json!({}),
            )
        })?;
    match work(&transaction) {
        Ok(value) => {
            transaction.commit().map_err(|error| {
                diagnostic(
                    "sop_transaction_commit_failed",
                    &error.to_string(),
                    json!({}),
                )
            })?;
            Ok(value)
        }
        Err(error) => {
            let _ = transaction.rollback();
            Err(error)
        }
    }
}

pub(crate) fn row_json(row: &rusqlite::Row<'_>) -> rusqlite::Result<Value> {
    let mut object = Map::new();
    for index in 0..row.as_ref().column_count() {
        let name = row
            .as_ref()
            .column_name(index)
            .unwrap_or("column")
            .to_string();
        let value = match row.get_ref(index)? {
            rusqlite::types::ValueRef::Null => Value::Null,
            rusqlite::types::ValueRef::Integer(value) => json!(value),
            rusqlite::types::ValueRef::Real(value) => json!(value),
            rusqlite::types::ValueRef::Text(value) => {
                let text = String::from_utf8_lossy(value).to_string();
                if name.ends_with("_json") {
                    serde_json::from_str(&text).unwrap_or(Value::String(text))
                } else {
                    Value::String(text)
                }
            }
            rusqlite::types::ValueRef::Blob(value) => json!({"byte_length":value.len()}),
        };
        object.insert(name, value);
    }
    Ok(Value::Object(object))
}

fn normalize_handoff_executor(value: &str) -> Result<String, Value> {
    if !matches!(value, "agent" | "operator") {
        return Err(diagnostic(
            "sop_handoff_executor_invalid",
            "sop_handoff_executor_invalid",
            json!({"executor":value}),
        ));
    }
    Ok(value.to_string())
}

