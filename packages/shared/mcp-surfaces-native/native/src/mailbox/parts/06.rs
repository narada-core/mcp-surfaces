fn reconcile_first_observations(
    args: &Map<String, Value>,
    root: &Path,
) -> Result<Value, Value> {
    let idempotency_key = required_bounded(
        args,
        "idempotency_key",
        "mailbox_reconciliation_idempotency_key_required",
        512,
    )?;
    let generation_id = required_bounded(
        args,
        "generation_id",
        "mailbox_reconciliation_generation_id_required",
        128,
    )?;
    let scope = load_mailbox_scope(args, root)?;
    let limit = bounded_integer(args.get("limit"), 100, 1, 100)? as usize;
    let mut db = open_domain_db_write(root)?;
    let mut observed = HashSet::new();
    {
        let mut statement = db
            .prepare("SELECT mailbox_id,message_id FROM mailbox_message_observations WHERE mailbox_id=?")
            .map_err(|e| error("mailbox_observation_query_failed", &e.to_string()))?;
        let rows = statement
            .query_map(params![scope.scope_id], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })
            .map_err(|e| error("mailbox_observation_query_failed", &e.to_string()))?;
        for row in rows {
            let (mailbox_id, message_id) =
                row.map_err(|e| error("mailbox_observation_row_failed", &e.to_string()))?;
            observed.insert(format!("{mailbox_id}\0{message_id}"));
        }
    }
    let mut candidates = Vec::new();
    let mut candidate_identities = HashSet::new();
    {
        let mut statement = db
            .prepare("SELECT fact_id,event_kind,message_id,mailbox_id,conversation_id,application_status FROM mailbox_sync_generation_records WHERE generation_id=? ORDER BY rowid")
            .map_err(|e| error("mailbox_generation_record_query_failed", &e.to_string()))?;
        let rows = statement
            .query_map(params![generation_id], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, Option<String>>(3)?,
                    row.get::<_, Option<String>>(4)?,
                    row.get::<_, String>(5)?,
                ))
            })
            .map_err(|e| error("mailbox_generation_record_query_failed", &e.to_string()))?;
        for row in rows {
            let (fact_id, event_kind, message_id, mailbox_id, conversation_id, application_status) =
                row.map_err(|e| error("mailbox_generation_record_row_failed", &e.to_string()))?;
            if application_status == "not_applied" || matches!(event_kind.as_str(), "delete" | "deleted") {
                continue;
            }
            let (Some(message_id), Some(mailbox_id)) = (message_id, mailbox_id) else {
                continue;
            };
            if mailbox_id != scope.scope_id {
                let code = format!("mailbox_reconciliation_scope_mismatch:{mailbox_id}:{}", scope.scope_id);
                return Err(error(&code, &code));
            }
            let identity = format!("{mailbox_id}\0{message_id}");
            if !observed.contains(&identity) && candidate_identities.insert(identity) {
                candidates.push(FirstObservationCandidate {
                    mailbox_id,
                    message_id,
                    fact_id,
                    conversation_id,
                });
            }
        }
    }
    let unobserved_count = candidates.len();
    candidates.truncate(limit);
    for candidate in &mut candidates {
        let fact = load_mail_fact(&scope, &candidate.fact_id).map_err(|value| {
            let message = value
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("mailbox_reconciliation_fact_validation_failed");
            error(
                &format!("mailbox_reconciliation_fact_validation_failed:{message}"),
                &format!("mailbox_reconciliation_fact_validation_failed:{message}"),
            )
        })?;
        if fact.fact_type != "mail.message.discovered" {
            let code = format!("mailbox_reconciliation_fact_type_invalid:{}", fact.fact_type);
            return Err(error(
                &format!("mailbox_reconciliation_fact_validation_failed:{code}"),
                &format!("mailbox_reconciliation_fact_validation_failed:{code}"),
            ));
        }
        let metadata = mail_metadata(&fact)?;
        if metadata.mailbox_id != candidate.mailbox_id || metadata.message_id != candidate.message_id {
            let code = format!("mailbox_reconciliation_fact_identity_mismatch:{}", candidate.fact_id);
            return Err(error(
                &format!("mailbox_reconciliation_fact_validation_failed:{code}"),
                &format!("mailbox_reconciliation_fact_validation_failed:{code}"),
            ));
        }
        if metadata.conversation_id.is_some() {
            candidate.conversation_id = metadata.conversation_id;
        }
    }
    let request_fingerprint = sha256_hex(
        canonical_json(&json!({
            "schema":"narada.mailbox.reconcile_first_observations_request.v1",
            "scope_id":scope.scope_id,
            "generation_id":generation_id,
            "limit":limit
        }))
        .as_bytes(),
    );
    let operation_id = stable_id("mbr_", &idempotency_key);
    let remaining_unobserved = unobserved_count.saturating_sub(candidates.len());
    let has_more = unobserved_count > candidates.len();
    let now = now_iso_millis();
    let tx = db
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|e| error("mailbox_domain_transaction_failed", &e.to_string()))?;
    let result = (|| {
        let existing: Option<(String, String)> = tx
            .query_row(
                "SELECT request_fingerprint,result_json FROM mailbox_reconciliation_operations WHERE idempotency_key=?",
                params![idempotency_key],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .map_err(|e| error("mailbox_reconciliation_query_failed", &e.to_string()))?;
        if let Some((existing_fingerprint, existing_json)) = existing {
            if existing_fingerprint != request_fingerprint {
                let code = format!("mailbox_reconciliation_idempotency_conflict:{idempotency_key}");
                return Err(error(&code, &code));
            }
            let mut replay = serde_json::from_str::<Value>(&existing_json)
                .map_err(|e| error("mailbox_reconciliation_receipt_invalid", &e.to_string()))?;
            if let Some(object) = replay.as_object_mut() {
                object.insert("idempotency_replayed".to_string(), Value::Bool(true));
            }
            return Ok(json!({
                "schema":"narada.domain_operation.v1",
                "operation_ref":format!("mailbox-reconcile:{operation_id}"),
                "outcome":"completed",
                "result":replay
            }));
        }
        let generation: Option<(String, String)> = tx
            .query_row(
                "SELECT scope_id,status FROM mailbox_sync_generations WHERE generation_id=?",
                params![generation_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .map_err(|e| error("mailbox_sync_generation_query_failed", &e.to_string()))?;
        let Some((generation_scope, generation_status)) = generation else {
            let code = format!("mailbox_sync_generation_not_found:{generation_id}");
            return Err(error(&code, &code));
        };
        if generation_scope != scope.scope_id {
            let code = format!(
                "mailbox_reconciliation_scope_mismatch:{}:{generation_scope}",
                scope.scope_id
            );
            return Err(error(&code, &code));
        }
        if generation_status != "completed" {
            let code = format!("mailbox_reconciliation_generation_not_completed:{generation_status}");
            return Err(error(&code, &code));
        }
        let mut observations_recorded = 0_i64;
        let mut events_published = 0_i64;
        let mut skipped_existing = 0_i64;
        for candidate in &candidates {
            let identity = format!("{}\0{}", candidate.mailbox_id, candidate.message_id);
            let observation_id = stable_id("mobs_", &identity);
            let observation_changes = tx
                .execute(
                    "INSERT OR IGNORE INTO mailbox_message_observations(observation_id,mailbox_id,message_id,first_generation_id,first_fact_id,observed_at) VALUES (?,?,?,?,?,?)",
                    params![observation_id,candidate.mailbox_id,candidate.message_id,generation_id,candidate.fact_id,now],
                )
                .map_err(|e| error("mailbox_observation_insert_failed", &e.to_string()))?;
            if observation_changes == 1 {
                observations_recorded += 1;
            } else {
                skipped_existing += 1;
            }
            let event_id = stable_id("mbe_", &format!("first-observed\0{identity}"));
            let mut payload = json!({
                "schema":"narada.mailbox.message_first_observed.v1",
                "generation_id":generation_id,
                "observation_id":observation_id,
                "mailbox_id":candidate.mailbox_id,
                "message_id":candidate.message_id,
                "fact_id":candidate.fact_id
            });
            if let Some(conversation_id) = &candidate.conversation_id {
                payload
                    .as_object_mut()
                    .expect("payload object")
                    .insert("conversation_id".to_string(), Value::String(conversation_id.clone()));
            }
            let event_changes = tx
                .execute(
                    "INSERT OR IGNORE INTO mailbox_outbox(event_id,scope_id,topic,aggregate_id,aggregate_revision,schema_version,causation_id,idempotency_key,partition_key,occurred_at,payload_json) VALUES (?,?,'mailbox.message.first_observed',?,1,1,?,?,?,?,?)",
                    params![event_id,scope.scope_id,observation_id,generation_id,event_id,observation_id,now,serde_json::to_string(&payload).unwrap_or_else(|_| "{}".to_string())],
                )
                .map_err(|e| error("mailbox_outbox_insert_failed", &e.to_string()))?;
            if event_changes == 1 {
                events_published += 1;
            }
        }
        let receipt = json!({
            "schema":"narada.mailbox.reconcile_first_observations_receipt.v1",
            "operation_id":operation_id,
            "scope_id":scope.scope_id,
            "generation_id":generation_id,
            "candidates_scanned":candidates.len(),
            "observations_recorded":observations_recorded,
            "events_published":events_published,
            "skipped_existing":skipped_existing,
            "remaining_unobserved":remaining_unobserved,
            "has_more":has_more,
            "status":"completed"
        });
        tx.execute(
            "INSERT INTO mailbox_reconciliation_operations(operation_id,idempotency_key,request_fingerprint,scope_id,generation_id,result_json,created_at) VALUES (?,?,?,?,?,?,?)",
            params![operation_id,idempotency_key,request_fingerprint,scope.scope_id,generation_id,serde_json::to_string(&receipt).unwrap_or_else(|_| "{}".to_string()),now],
        )
        .map_err(|e| error("mailbox_reconciliation_insert_failed", &e.to_string()))?;
        let mut result = receipt;
        result
            .as_object_mut()
            .expect("receipt object")
            .insert("idempotency_replayed".to_string(), Value::Bool(false));
        Ok(json!({
            "schema":"narada.domain_operation.v1",
            "operation_ref":format!("mailbox-reconcile:{operation_id}"),
            "outcome":"completed",
            "result":result
        }))
    })();
    match result {
        Ok(value) => {
            tx.commit()
                .map_err(|e| error("mailbox_domain_transaction_commit_failed", &e.to_string()))?;
            Ok(value)
        }
        Err(value) => Err(value),
    }
}

struct AdmissionEvaluation {
    admitted: bool,
    reason: &'static str,
    folder_refs: Vec<String>,
    sender_email: Option<String>,
}

fn fact_event(fact: &MailFact) -> Option<(&Map<String, Value>, Option<&Map<String, Value>>)> {
    let event = fact.payload.get("event")?.as_object()?;
    Some((event, event.get("payload").and_then(Value::as_object)))
}

fn string_set(value: Option<&Value>) -> HashSet<String> {
    value
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| value.to_ascii_lowercase())
        .collect()
}

