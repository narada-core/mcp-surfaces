fn fail_generation(
    db: &mut Connection,
    generation_id: &str,
    lease_token: &str,
    message: &str,
    now: &str,
) -> Result<Generation, Value> {
    let tx = db
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|e| error("mailbox_domain_transaction_failed", &e.to_string()))?;
    let generation = require_generation_tx(&tx, generation_id)?;
    let bounded = message.chars().take(2048).collect::<String>();
    let changes = tx
        .execute(
            "UPDATE mailbox_sync_generations SET status='failed',error_message=?,completed_at=?,updated_at=?,lease_token=NULL,lease_expires_at=NULL WHERE generation_id=? AND lease_token=?",
            params![bounded,now,now,generation_id,lease_token],
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
    let failed = require_generation_tx(&tx, generation_id)?;
    tx.commit()
        .map_err(|e| error("mailbox_domain_transaction_commit_failed", &e.to_string()))?;
    Ok(failed)
}

fn generation_operation(generation: &Generation, replayed: bool) -> Result<Value, Value> {
    let receipt = generation.receipt.as_ref().ok_or_else(|| {
        let code = format!("mailbox_sync_receipt_missing:{}", generation.generation_id);
        error(&code, &code)
    })?;
    let serialized = canonical_json(receipt);
    let observed_count = receipt
        .get("observed_message_refs")
        .and_then(Value::as_array)
        .map(Vec::len)
        .unwrap_or(0);
    Ok(json!({
        "schema":DOMAIN_SCHEMA,
        "operation_ref":format!("mailbox-sync:{}",generation.generation_id),
        "outcome":"completed",
        "result":{
            "schema":receipt.get("schema").and_then(Value::as_str).unwrap_or("narada.mailbox.sync_generation_receipt.v1"),
            "generation_id":receipt.get("generation_id").cloned().unwrap_or(Value::Null),
            "scope_id":receipt.get("scope_id").cloned().unwrap_or(Value::Null),
            "status":receipt.get("status").cloned().unwrap_or(Value::Null),
            "config_fingerprint":receipt.get("config_fingerprint").cloned().unwrap_or(Value::Null),
            "parent_cursor_sha256":receipt.get("parent_cursor_sha256").cloned().unwrap_or(Value::Null),
            "next_cursor_sha256":receipt.get("next_cursor_sha256").cloned().unwrap_or(Value::Null),
            "record_count":receipt.get("record_count").cloned().unwrap_or(Value::Null),
            "observed_message_count":receipt.get("observed_message_count").cloned().unwrap_or(Value::Null),
            "first_observation_count":receipt.get("first_observation_count").cloned().unwrap_or(Value::Null),
            "tombstone_count":receipt.get("tombstone_count").cloned().unwrap_or(Value::Null),
            "observed_message_refs_available_count":observed_count,
            "observed_message_refs_omitted":true,
            "observed_message_refs_truncated":receipt.get("observed_message_refs_truncated").and_then(Value::as_bool).unwrap_or(false),
            "completed_at":receipt.get("completed_at").cloned().unwrap_or(Value::Null),
            "idempotency_replayed":replayed,
        },
        "result_ref":{
            "ref":format!("mailbox-generation-receipt:{}",generation.generation_id),
            "sha256":sha256_hex(serialized.as_bytes()),
            "byte_length":serialized.len(),
            "media_type":"application/json",
        },
    }))
}

fn blocked_generation_operation(generation: &Generation, replayed: bool) -> Value {
    json!({
        "schema":DOMAIN_SCHEMA,
        "operation_ref":format!("mailbox-sync:{}",generation.generation_id),
        "outcome":"completed",
        "result":{
            "schema":"narada.mailbox.sync_generation_failure.v1",
            "generation_id":generation.generation_id,
            "scope_id":generation.scope_id,
            "status":"blocked",
            "error_message":generation.error_message.as_deref().unwrap_or("mailbox_sync_failed"),
            "idempotency_replayed":replayed,
        },
    })
}

fn fetch_graph_batch<F>(
    scope: &ScopeConfig,
    checkpoint: Option<&str>,
    mut heartbeat: F,
) -> Result<SourceBatch, Value>
where
    F: FnMut() -> Result<(), Value>,
{
    let token = graph_access_token(scope)?;
    let fetched_at = now_iso_millis();
    let mut messages = Vec::new();
    let mut next_by_folder = Map::new();
    let composite = checkpoint
        .and_then(|value| serde_json::from_str::<Value>(value).ok())
        .and_then(|value| value.as_object().cloned());
    for folder in &scope.included_container_refs {
        if folder.trim().is_empty() {
            return Err(error(
                "mailbox_graph_folder_ref_invalid",
                "Configured folder ref is empty",
            ));
        }
        heartbeat()?;
        let folder_cursor = if scope.included_container_refs.len() == 1 {
            checkpoint.map(ToString::to_string)
        } else {
            composite
                .as_ref()
                .and_then(|value| value.get(folder))
                .and_then(Value::as_str)
                .map(ToString::to_string)
        };
        let base = format!(
            "{}{}/mailFolders/{}/messages/delta",
            scope.graph.base_url,
            graph_mailbox_path(&scope.graph.user_id),
            encode_component(folder)
        );
        let walked = match walk_delta(
            scope,
            &token,
            folder_cursor.as_deref().unwrap_or(&base),
            folder,
            &mut heartbeat,
        ) {
            Ok(value) => value,
            Err(GraphWalkError::Stale) if folder_cursor.is_some() => {
                walk_delta(scope, &token, &base, folder, &mut heartbeat)
                    .map_err(GraphWalkError::into_value)?
            }
            Err(value) => return Err(value.into_value()),
        };
        if messages.len() + walked.0.len() > MAX_GRAPH_RECORDS {
            return Err(error(
                "mailbox_graph_record_limit_exceeded",
                "mailbox_graph_record_limit_exceeded",
            ));
        }
        messages.extend(walked.0);
        next_by_folder.insert(folder.clone(), json!(walked.1));
    }

    let next_checkpoint = if scope.included_container_refs.len() == 1 {
        next_by_folder
            .values()
            .next()
            .and_then(Value::as_str)
            .map(ToString::to_string)
    } else {
        Some(
            serde_json::to_string(&Value::Object(next_by_folder))
                .map_err(|e| error("mailbox_graph_cursor_encode_failed", &e.to_string()))?,
        )
    };
    let mut events = BTreeMap::new();
    for message in messages {
        let event = normalize_graph_event(scope, &message, &fetched_at)?;
        let event_id = event
            .get("event_id")
            .and_then(Value::as_str)
            .expect("normalized event id")
            .to_string();
        events.insert(event_id, event);
    }
    let records = events
        .into_values()
        .map(|event| {
            let record_id = event
                .get("event_id")
                .and_then(Value::as_str)
                .expect("normalized event id")
                .to_string();
            let ordinal = event
                .get("observed_at")
                .or_else(|| event.get("received_at"))
                .and_then(Value::as_str)
                .map(ToString::to_string)
                .or_else(|| Some(fetched_at.clone()));
            let source_version = event
                .get("source_version")
                .and_then(Value::as_str)
                .map(ToString::to_string);
            let mut provenance = json!({
                "sourceId":scope.scope_id,
                "observedAt":event.get("observed_at").and_then(Value::as_str).unwrap_or(&fetched_at),
            });
            if let Some(source_version) = source_version {
                provenance
                    .as_object_mut()
                    .expect("object")
                    .insert("sourceVersion".to_string(), json!(source_version));
            }
            SourceRecord {
                record_id,
                ordinal,
                payload: event,
                provenance,
            }
        })
        .collect();
    Ok(SourceBatch {
        records,
        prior_checkpoint: checkpoint.map(ToString::to_string),
        next_checkpoint,
        has_more: false,
        fetched_at,
    })
}

enum GraphWalkError {
    Stale,
    Failure(Value),
}

impl GraphWalkError {
    fn into_value(self) -> Value {
        match self {
            Self::Stale => error(
                "mailbox_graph_delta_cursor_stale",
                "mailbox_graph_delta_cursor_stale",
            ),
            Self::Failure(value) => value,
        }
    }
}

