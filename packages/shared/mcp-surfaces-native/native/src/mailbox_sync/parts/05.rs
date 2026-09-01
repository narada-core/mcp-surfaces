fn stage_batch(
    db: &mut Connection,
    generation_id: &str,
    lease_token: &str,
    checkpoint: Option<&str>,
    batch: &SourceBatch,
    artifact_path: &Path,
) -> Result<(), Value> {
    let parent_cursor = batch.prior_checkpoint.as_deref().or(checkpoint);
    if parent_cursor != checkpoint {
        let code = format!("mailbox_sync_source_parent_cursor_mismatch:{generation_id}");
        return Err(error(&code, &code));
    }
    let bytes = fs::read(artifact_path).map_err(|e| {
        error(
            "mailbox_sync_generation_artifact_read_failed",
            &e.to_string(),
        )
    })?;
    let artifact_hash = sha256_hex(&bytes);
    let staged = batch
        .records
        .iter()
        .map(|record| staged_record(record, batch.next_checkpoint.as_deref()))
        .collect::<Result<Vec<_>, _>>()?;
    let now = now_iso_millis();
    let tx = db
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|e| error("mailbox_domain_transaction_failed", &e.to_string()))?;
    let generation = require_generation_tx(&tx, generation_id)?;
    if generation.status == "staged" {
        if generation.parent_cursor.as_deref() != parent_cursor
            || generation.next_cursor != batch.next_checkpoint
            || generation.batch_path.as_deref()
                != Some(normalized_path_text(artifact_path).as_str())
            || generation.batch_sha256.as_deref() != Some(artifact_hash.as_str())
            || generation.batch_record_count != staged.len() as i64
        {
            let code = format!("mailbox_sync_staged_batch_conflict:{generation_id}");
            return Err(error(&code, &code));
        }
        tx.commit()
            .map_err(|e| error("mailbox_domain_transaction_commit_failed", &e.to_string()))?;
        return Ok(());
    }
    if generation.status != "accepted" {
        let code = format!(
            "mailbox_sync_generation_not_stageable:{}",
            generation.status
        );
        return Err(error(&code, &code));
    }
    if generation.lease_token.as_deref() != Some(lease_token) {
        let code = format!("mailbox_sync_lease_lost:{}", generation.scope_id);
        return Err(error(&code, &code));
    }
    tx.execute(
        "UPDATE mailbox_sync_generations SET status='staged',parent_cursor=?,next_cursor=?,batch_path=?,batch_sha256=?,batch_record_count=?,staged_at=?,updated_at=? WHERE generation_id=?",
        params![parent_cursor,batch.next_checkpoint,normalized_path_text(artifact_path),artifact_hash,staged.len() as i64,now,now,generation_id],
    )
    .map_err(|e| error("mailbox_sync_generation_update_failed", &e.to_string()))?;
    for record in staged {
        tx.execute(
            "INSERT INTO mailbox_sync_generation_records(generation_id,record_id,ordinal,fact_id,event_kind,message_id,mailbox_id,conversation_id,source_version,application_status) VALUES (?,?,?,?,?,?,?,?,?,'staged')",
            params![generation_id,record.record_id,record.ordinal,record.fact_id,record.event_kind,record.message_id,record.mailbox_id,record.conversation_id,record.source_version],
        )
        .map_err(|e| error("mailbox_sync_generation_record_insert_failed", &e.to_string()))?;
    }
    tx.commit()
        .map_err(|e| error("mailbox_domain_transaction_commit_failed", &e.to_string()))
}

fn generation_records(db: &Connection, generation_id: &str) -> Result<Vec<StagedRecord>, Value> {
    let mut statement = db
        .prepare("SELECT record_id,ordinal,fact_id,event_kind,message_id,mailbox_id,conversation_id,source_version,application_status FROM mailbox_sync_generation_records WHERE generation_id=? ORDER BY rowid")
        .map_err(|e| error("mailbox_sync_generation_record_query_failed", &e.to_string()))?;
    let rows = statement
        .query_map(params![generation_id], |row| {
            Ok(StagedRecord {
                record_id: row.get(0)?,
                ordinal: row.get(1)?,
                fact_id: row.get(2)?,
                event_kind: row.get(3)?,
                message_id: row.get(4)?,
                mailbox_id: row.get(5)?,
                conversation_id: row.get(6)?,
                source_version: row.get(7)?,
                application_status: row.get(8)?,
            })
        })
        .map_err(|e| {
            error(
                "mailbox_sync_generation_record_query_failed",
                &e.to_string(),
            )
        })?;
    let mut records = Vec::new();
    for row in rows {
        records.push(
            row.map_err(|e| error("mailbox_sync_generation_record_row_failed", &e.to_string()))?,
        );
    }
    Ok(records)
}

fn generation_ready(db: &Connection, generation_id: &str) -> Result<bool, Value> {
    Ok(generation_records(db, generation_id)?
        .iter()
        .all(|record| record.application_status != "staged"))
}

fn mark_record_application(
    db: &Connection,
    generation_id: &str,
    record_id: &str,
    status: &str,
) -> Result<(), Value> {
    let changes = db
        .execute(
            "UPDATE mailbox_sync_generation_records SET application_status=? WHERE generation_id=? AND record_id=?",
            params![status,generation_id,record_id],
        )
        .map_err(|e| error("mailbox_sync_generation_record_update_failed", &e.to_string()))?;
    if changes != 1 {
        let code = format!("mailbox_sync_record_unknown:{record_id}");
        return Err(error(&code, &code));
    }
    Ok(())
}

fn finalize_generation(
    db: &mut Connection,
    generation_id: &str,
    lease_token: &str,
    now: &str,
) -> Result<Generation, Value> {
    let tx = db
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|e| error("mailbox_domain_transaction_failed", &e.to_string()))?;
    let generation = require_generation_tx(&tx, generation_id)?;
    if generation.status == "completed" {
        tx.commit()
            .map_err(|e| error("mailbox_domain_transaction_commit_failed", &e.to_string()))?;
        return Ok(generation);
    }
    if generation.status != "staged" {
        let code = format!(
            "mailbox_sync_generation_not_finalizable:{}",
            generation.status
        );
        return Err(error(&code, &code));
    }
    let records = generation_records(&tx, generation_id)?;
    if let Some(incomplete) = records
        .iter()
        .find(|record| record.application_status == "staged")
    {
        let code = format!(
            "mailbox_sync_generation_incomplete:{}",
            incomplete.record_id
        );
        return Err(error(&code, &code));
    }
    let mut first_observation_count = 0_u64;
    let mut observed: Vec<Value> = Vec::new();
    let mut observed_keys = HashSet::new();
    let mut tombstone_count = 0_u64;
    for record in &records {
        if record.application_status == "not_applied" {
            continue;
        }
        if matches!(record.event_kind.as_str(), "delete" | "deleted") {
            tombstone_count += 1;
            continue;
        }
        let (Some(message_id), Some(mailbox_id)) = (&record.message_id, &record.mailbox_id) else {
            continue;
        };
        let identity = format!("{mailbox_id}\0{message_id}");
        let mut reference = json!({
            "mailbox_id":mailbox_id,
            "message_id":message_id,
            "fact_id":record.fact_id,
        });
        if let Some(conversation_id) = &record.conversation_id {
            reference
                .as_object_mut()
                .expect("object")
                .insert("conversation_id".to_string(), json!(conversation_id));
        }
        if observed_keys.insert(identity.clone()) {
            observed.push(reference);
        } else if let Some(slot) = observed.iter_mut().find(|value| {
            value.get("mailbox_id").and_then(Value::as_str) == Some(mailbox_id)
                && value.get("message_id").and_then(Value::as_str) == Some(message_id)
        }) {
            *slot = reference;
        }
        let observation_id = stable_id("mobs_", &identity);
        let inserted = tx
            .execute(
                "INSERT OR IGNORE INTO mailbox_message_observations(observation_id,mailbox_id,message_id,first_generation_id,first_fact_id,observed_at) VALUES (?,?,?,?,?,?)",
                params![observation_id,mailbox_id,message_id,generation_id,record.fact_id,now],
            )
            .map_err(|e| error("mailbox_sync_observation_insert_failed", &e.to_string()))?;
        if inserted != 1 {
            continue;
        }
        first_observation_count += 1;
        let event_id = stable_id("mbe_", &format!("first-observed\0{identity}"));
        let mut payload = json!({
            "schema":"narada.mailbox.message_first_observed.v1",
            "generation_id":generation_id,
            "observation_id":observation_id,
            "mailbox_id":mailbox_id,
            "message_id":message_id,
            "fact_id":record.fact_id,
        });
        if let Some(conversation_id) = &record.conversation_id {
            payload
                .as_object_mut()
                .expect("object")
                .insert("conversation_id".to_string(), json!(conversation_id));
        }
        tx.execute(
            "INSERT INTO mailbox_outbox(event_id,scope_id,topic,aggregate_id,aggregate_revision,schema_version,causation_id,idempotency_key,partition_key,occurred_at,payload_json) VALUES (?,?,'mailbox.message.first_observed',?,1,1,?,?,?,?,?)",
            params![event_id,generation.scope_id,observation_id,generation_id,event_id,observation_id,now,serde_json::to_string(&payload).unwrap_or_else(|_| "{}".to_string())],
        )
        .map_err(|e| error("mailbox_sync_outbox_insert_failed", &e.to_string()))?;
    }
    let status = if first_observation_count > 0 {
        "synced"
    } else {
        "no_change"
    };
    let receipt = json!({
        "schema":"narada.mailbox.sync_generation_receipt.v1",
        "generation_id":generation_id,
        "scope_id":generation.scope_id,
        "status":status,
        "config_fingerprint":generation.config_fingerprint,
        "parent_cursor_sha256":nullable_hash(generation.parent_cursor.as_deref()),
        "next_cursor_sha256":nullable_hash(generation.next_cursor.as_deref()),
        "record_count":records.len(),
        "observed_message_count":observed.len(),
        "first_observation_count":first_observation_count,
        "tombstone_count":tombstone_count,
        "observed_message_refs":observed.iter().take(100).cloned().collect::<Vec<_>>(),
        "observed_message_refs_truncated":observed.len()>100,
        "completed_at":now,
    });
    let changes = tx
        .execute(
            "UPDATE mailbox_sync_generations SET status='completed',receipt_json=?,error_message=NULL,completed_at=?,updated_at=?,lease_token=NULL,lease_expires_at=NULL WHERE generation_id=? AND lease_token=?",
            params![serde_json::to_string(&receipt).unwrap_or_else(|_| "{}".to_string()),now,now,generation_id,lease_token],
        )
        .map_err(|e| error("mailbox_sync_generation_update_failed", &e.to_string()))?;
    if changes != 1 {
        let code = format!("mailbox_sync_lease_lost:{}", generation.scope_id);
        return Err(error(&code, &code));
    }
    tx.execute(
        "DELETE FROM mailbox_sync_scope_leases WHERE scope_id=? AND generation_id=? AND lease_token=?",
        params![generation.scope_id,generation_id,lease_token],
    )
    .map_err(|e| error("mailbox_sync_lease_delete_failed", &e.to_string()))?;
    let completed = require_generation_tx(&tx, generation_id)?;
    tx.commit()
        .map_err(|e| error("mailbox_domain_transaction_commit_failed", &e.to_string()))?;
    Ok(completed)
}

