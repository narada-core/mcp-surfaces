fn write_tombstone(root: &Path, event: &Map<String, Value>) -> Result<(), Value> {
    let message_id = event
        .get("message_id")
        .and_then(Value::as_str)
        .unwrap_or("");
    let mut value = json!({
        "message_id":message_id,
        "mailbox_id":event.get("mailbox_id").cloned().unwrap_or(Value::Null),
        "deleted_by_event_id":event.get("event_id").cloned().unwrap_or(Value::Null),
    });
    if let Some(source_version) = event.get("source_version") {
        value
            .as_object_mut()
            .expect("object")
            .insert("source_version".to_string(), source_version.clone());
    }
    value.as_object_mut().expect("object").insert(
        "observed_at".to_string(),
        event.get("observed_at").cloned().unwrap_or(Value::Null),
    );
    atomic_write_json_pretty(
        &root
            .join("tombstones")
            .join(format!("{}.json", safe_segment(message_id))),
        &value,
    )
}

fn apply_marker_path(root: &Path, record_id: &str) -> PathBuf {
    let shard = record_id.get(..2).unwrap_or("00");
    root.join("state/apply-log")
        .join(shard)
        .join(format!("{record_id}.json"))
}

fn validate_apply_marker(path: &Path) -> Result<(), Value> {
    let value: Value = serde_json::from_str(
        &fs::read_to_string(path)
            .map_err(|e| error("mailbox_apply_marker_read_failed", &e.to_string()))?,
    )
    .map_err(|e| error("mailbox_apply_marker_invalid", &e.to_string()))?;
    let valid = ["event_id", "message_id", "event_kind", "applied_at"]
        .iter()
        .all(|key| {
            value
                .get(*key)
                .and_then(Value::as_str)
                .is_some_and(|value| !value.trim().is_empty())
        });
    if !valid {
        return Err(error(
            "mailbox_apply_marker_invalid",
            "mailbox_apply_marker_invalid",
        ));
    }
    Ok(())
}

fn write_apply_marker(path: &Path, record: &SourceRecord) -> Result<(), Value> {
    if path.is_file() {
        return validate_apply_marker(path);
    }
    atomic_write_json_pretty(
        path,
        &json!({
            "event_id":record.record_id,
            "message_id":record.payload.get("message_id").and_then(Value::as_str).unwrap_or(""),
            "event_kind":record.payload.get("event_kind").and_then(Value::as_str).unwrap_or("upsert"),
            "applied_at":now_iso_millis(),
        }),
    )
}

fn safe_segment(value: &str) -> String {
    encode_component(value)
}

fn remove_path(path: &Path) -> Result<(), Value> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(value) if value.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(value) => Err(error(
            "mailbox_projection_remove_failed",
            &value.to_string(),
        )),
    }
}

fn batch_to_value(batch: &SourceBatch) -> Value {
    let records = batch
        .records
        .iter()
        .map(|record| {
            let mut value = json!({"recordId":record.record_id});
            if let Some(ordinal) = &record.ordinal {
                value
                    .as_object_mut()
                    .expect("object")
                    .insert("ordinal".to_string(), json!(ordinal));
            }
            value
                .as_object_mut()
                .expect("object")
                .insert("payload".to_string(), record.payload.clone());
            value
                .as_object_mut()
                .expect("object")
                .insert("provenance".to_string(), record.provenance.clone());
            value
        })
        .collect::<Vec<_>>();
    let mut value = json!({"records":records});
    value.as_object_mut().expect("object").insert(
        "priorCheckpoint".to_string(),
        batch
            .prior_checkpoint
            .clone()
            .map(Value::String)
            .unwrap_or(Value::Null),
    );
    if let Some(next) = &batch.next_checkpoint {
        value
            .as_object_mut()
            .expect("object")
            .insert("nextCheckpoint".to_string(), json!(next));
    }
    value
        .as_object_mut()
        .expect("object")
        .insert("hasMore".to_string(), json!(batch.has_more));
    value
        .as_object_mut()
        .expect("object")
        .insert("fetchedAt".to_string(), json!(batch.fetched_at));
    value
}

fn batch_from_value(value: &Value) -> Result<SourceBatch, Value> {
    let object = value.as_object().ok_or_else(|| {
        error(
            "mailbox_sync_generation_batch_invalid",
            "mailbox_sync_generation_batch_invalid",
        )
    })?;
    let raw_records = object
        .get("records")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            error(
                "mailbox_sync_generation_records_invalid",
                "mailbox_sync_generation_records_invalid",
            )
        })?;
    if raw_records.len() > MAX_GRAPH_RECORDS {
        return Err(error(
            "mailbox_sync_generation_records_too_many",
            "mailbox_sync_generation_records_too_many",
        ));
    }
    let mut records = Vec::with_capacity(raw_records.len());
    for raw in raw_records {
        let record = raw.as_object().ok_or_else(|| {
            error(
                "mailbox_sync_generation_record_invalid",
                "mailbox_sync_generation_record_invalid",
            )
        })?;
        let record_id = record
            .get("recordId")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| {
                error(
                    "mailbox_sync_generation_record_invalid",
                    "mailbox_sync_generation_record_invalid",
                )
            })?
            .to_string();
        records.push(SourceRecord {
            record_id,
            ordinal: record
                .get("ordinal")
                .and_then(Value::as_str)
                .map(ToString::to_string),
            payload: record.get("payload").cloned().unwrap_or(Value::Null),
            provenance: record
                .get("provenance")
                .cloned()
                .unwrap_or_else(|| json!({})),
        });
    }
    Ok(SourceBatch {
        records,
        prior_checkpoint: object
            .get("priorCheckpoint")
            .and_then(Value::as_str)
            .map(ToString::to_string),
        next_checkpoint: object
            .get("nextCheckpoint")
            .and_then(Value::as_str)
            .map(ToString::to_string),
        has_more: object
            .get("hasMore")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        fetched_at: object
            .get("fetchedAt")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
    })
}

fn staged_record(
    record: &SourceRecord,
    source_cursor: Option<&str>,
) -> Result<StagedRecord, Value> {
    let event = record.payload.as_object().ok_or_else(|| {
        let code = format!("mailbox_sync_record_payload_invalid:{}", record.record_id);
        error(&code, &code)
    })?;
    Ok(StagedRecord {
        record_id: record.record_id.clone(),
        ordinal: record.ordinal.clone(),
        fact_id: fact_for_record(record, source_cursor)?.0,
        event_kind: event
            .get("event_kind")
            .and_then(Value::as_str)
            .unwrap_or("unknown")
            .chars()
            .take(64)
            .collect(),
        message_id: optional_trimmed(event.get("message_id"))
            .map(|value| value.chars().take(512).collect()),
        mailbox_id: optional_trimmed(event.get("mailbox_id"))
            .map(|value| value.chars().take(512).collect()),
        conversation_id: optional_trimmed(event.get("conversation_id"))
            .map(|value| value.chars().take(1024).collect()),
        source_version: optional_trimmed(event.get("source_version"))
            .map(|value| value.chars().take(1024).collect()),
        application_status: "staged".to_string(),
    })
}

