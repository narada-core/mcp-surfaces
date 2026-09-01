fn admit_message(args: &Map<String, Value>, root: &Path) -> Result<Value, Value> {
    let idempotency_key = required_bounded(
        args,
        "idempotency_key",
        "mailbox_admission_idempotency_key_required",
        512,
    )?;
    let fact_id = required_bounded(args, "fact_id", "mailbox_admission_fact_id_required", 256)?;
    let source_event_id = required_bounded(
        args,
        "source_event_id",
        "mailbox_admission_source_event_id_required",
        256,
    )?;
    let scope = load_mailbox_scope(args, root)?;
    let policy_version = format!(
        "sha256:{}",
        sha256_hex(
            canonical_json(&json!({
                "schema":"narada.mailbox.admission_policy.v1",
                "scope_id":scope.scope_id,
                "policy":scope.admission
            }))
            .as_bytes()
        )
    );
    if let Some(expected) = args
        .get("policy_version")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        if expected != policy_version {
            let code = format!("mailbox_admission_policy_version_mismatch:{expected}:{policy_version}");
            return Err(error(&code, &code));
        }
    }
    let fact = load_mail_fact(&scope, &fact_id).map_err(|value| {
        if value
            .get("code")
            .and_then(Value::as_str)
            .is_some_and(|code| code.contains("fact_not_found"))
        {
            let code = format!("mailbox_admission_fact_not_found:{fact_id}");
            error(&code, &code)
        } else {
            value
        }
    })?;
    if fact.fact_type != "mail.message.discovered" {
        let code = format!("mailbox_admission_fact_type_invalid:{}", fact.fact_type);
        return Err(error(&code, &code));
    }
    let metadata = mail_metadata(&fact)?;
    if metadata.mailbox_id != scope.scope_id {
        let code = format!(
            "mailbox_admission_scope_mismatch:{}:{}",
            metadata.mailbox_id, scope.scope_id
        );
        return Err(error(&code, &code));
    }
    let mut db = open_domain_db_write(root)?;
    let source_event: Option<(String, String, String)> = db
        .query_row(
            "SELECT scope_id,topic,payload_json FROM mailbox_outbox WHERE event_id=?",
            params![source_event_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()
        .map_err(|e| error("mailbox_admission_source_event_query_failed", &e.to_string()))?;
    let Some((event_scope, event_topic, event_payload_json)) = source_event else {
        let code = format!("mailbox_admission_source_event_not_found:{source_event_id}");
        return Err(error(&code, &code));
    };
    let event_payload = serde_json::from_str::<Value>(&event_payload_json)
        .map_err(|e| error("mailbox_admission_source_event_invalid", &e.to_string()))?;
    if event_topic != "mailbox.message.first_observed"
        || event_scope != scope.scope_id
        || event_payload.get("fact_id").and_then(Value::as_str) != Some(fact_id.as_str())
        || event_payload.get("mailbox_id").and_then(Value::as_str) != Some(scope.scope_id.as_str())
    {
        let code = format!("mailbox_admission_source_event_mismatch:{source_event_id}");
        return Err(error(&code, &code));
    }
    let evaluation = evaluate_admission(&fact, &scope.admission);
    let request_fingerprint = sha256_hex(
        canonical_json(&json!({
            "schema":"narada.mailbox.message_admission_request.v2",
            "scope_id":scope.scope_id,
            "fact_id":fact_id,
            "source_event_id":source_event_id,
            "policy_version":policy_version
        }))
        .as_bytes(),
    );
    let admission_id = stable_id("mba_", &format!("{}\0{fact_id}", scope.scope_id));
    let provenance = fact.provenance.as_object().cloned().unwrap_or_default();
    let source_record_id = provenance
        .get("source_record_id")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let source_version = provenance
        .get("source_version")
        .cloned()
        .unwrap_or(Value::Null);
    let graph_mailbox_id = scope.graph_mailbox_id.clone().ok_or_else(|| {
        let code = format!("mailbox_scope_graph_user_id_required:{}", scope.scope_id);
        error(&code, &code)
    })?;
    let mut source_ref = json!({
        "schema":"narada.mailbox.source_ref.v1",
        "scope_id":scope.scope_id,
        "mailbox_id":graph_mailbox_id,
        "message_id":metadata.message_id,
        "fact_id":fact_id,
        "source_record_id":source_record_id,
        "source_version":source_version
    });
    if let Some(conversation_id) = &metadata.conversation_id {
        source_ref
            .as_object_mut()
            .expect("source ref")
            .insert("conversation_id".to_string(), Value::String(conversation_id.clone()));
    }
    if let Some(internet_message_id) = &metadata.internet_message_id {
        source_ref
            .as_object_mut()
            .expect("source ref")
            .insert(
                "internet_message_id".to_string(),
                Value::String(internet_message_id.clone()),
            );
    }
    let mut correlation_keys = Vec::new();
    if let Some(conversation_id) = &metadata.conversation_id {
        correlation_keys.push(json!({
            "kind":"mailbox_conversation",
            "scope":metadata.mailbox_id,
            "value":conversation_id
        }));
    }
    if let Some(internet_message_id) = &metadata.internet_message_id {
        correlation_keys.push(json!({
            "kind":"internet_message_id",
            "scope":"rfc5322",
            "value":internet_message_id
        }));
    }
    let summary = metadata
        .subject
        .as_ref()
        .map(|subject| format!("Mailbox message: {subject}").chars().take(500).collect::<String>())
        .unwrap_or_else(|| "Mailbox message".to_string());
    let source = json!({
        "source_kind":"mailbox_message",
        "source_scope":metadata.mailbox_id,
        "immutable_source_id":metadata.message_id,
        "summary":summary,
        "source_ref":source_ref,
        "correlation_keys":correlation_keys
    });
    let decision = json!({
        "schema":"narada.mailbox.message_admission_receipt.v2",
        "admission_id":admission_id,
        "decision":if evaluation.admitted { "admitted" } else { "rejected" },
        "reason":evaluation.reason,
        "policy_version":policy_version,
        "source_event_id":source_event_id,
        "scope_id":scope.scope_id,
        "fact_id":fact_id,
        "source":source,
        "evaluated_metadata":{
            "folder_refs":evaluation.folder_refs,
            "sender_email":evaluation.sender_email
        }
    });
    let event_topic = if evaluation.admitted {
        "mailbox.message.admitted"
    } else {
        "mailbox.message.rejected"
    };
    let event_payload = json!({
        "schema":if evaluation.admitted { "narada.mailbox.message_admitted.v1" } else { "narada.mailbox.message_rejected.v1" },
        "admission_id":admission_id,
        "source_event_id":source_event_id,
        "scope_id":scope.scope_id,
        "fact_id":fact_id,
        "decision":if evaluation.admitted { "admitted" } else { "rejected" },
        "reason":evaluation.reason,
        "policy_version":policy_version,
        "source":source
    });
    let now = now_iso_millis();
    let tx = db
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|e| error("mailbox_domain_transaction_failed", &e.to_string()))?;
    let result = (|| {
        let existing: Option<(String, String, String)> = tx
            .query_row(
                "SELECT scope_id,fact_id,decision_json FROM mailbox_admission_receipts WHERE idempotency_key=?",
                params![idempotency_key],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()
            .map_err(|e| error("mailbox_admission_query_failed", &e.to_string()))?;
        let (stored_decision, replayed) = if let Some((existing_scope, existing_fact, existing_json)) = existing {
            if existing_scope != scope.scope_id || existing_fact != fact_id {
                let code = format!("mailbox_admission_idempotency_conflict:{idempotency_key}");
                return Err(error(&code, &code));
            }
            (
                serde_json::from_str::<Value>(&existing_json)
                    .map_err(|e| error("mailbox_admission_receipt_invalid", &e.to_string()))?,
                true,
            )
        } else if let Some(existing_json) = tx
            .query_row(
                "SELECT decision_json FROM mailbox_admission_receipts WHERE scope_id=? AND fact_id=?",
                params![scope.scope_id, fact_id],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .map_err(|e| error("mailbox_admission_query_failed", &e.to_string()))?
        {
            (
                serde_json::from_str::<Value>(&existing_json)
                    .map_err(|e| error("mailbox_admission_receipt_invalid", &e.to_string()))?,
                true,
            )
        } else {
            tx.execute(
                "INSERT INTO mailbox_admission_receipts(admission_id,idempotency_key,request_fingerprint,scope_id,fact_id,policy_version,decision_json,created_at) VALUES (?,?,?,?,?,?,?,?)",
                params![admission_id,idempotency_key,request_fingerprint,scope.scope_id,fact_id,policy_version,serde_json::to_string(&decision).unwrap_or_else(|_| "{}".to_string()),now],
            )
            .map_err(|e| error("mailbox_admission_insert_failed", &e.to_string()))?;
            let event_id = stable_id("mbe_", &format!("admission\0{}\0{fact_id}", scope.scope_id));
            tx.execute(
                "INSERT INTO mailbox_outbox(event_id,scope_id,topic,aggregate_id,aggregate_revision,schema_version,causation_id,idempotency_key,partition_key,occurred_at,payload_json) VALUES (?,?,?,?,1,1,?,?,?,?,?)",
                params![event_id,scope.scope_id,event_topic,admission_id,source_event_id,event_id,admission_id,now,serde_json::to_string(&event_payload).unwrap_or_else(|_| "{}".to_string())],
            )
            .map_err(|e| error("mailbox_admission_event_insert_failed", &e.to_string()))?;
            (decision.clone(), false)
        };
        let mut result = stored_decision;
        result
            .as_object_mut()
            .expect("admission receipt")
            .insert("idempotency_replayed".to_string(), Value::Bool(replayed));
        Ok(json!({
            "schema":"narada.domain_operation.v1",
            "operation_ref":format!("mailbox-admission:{admission_id}"),
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

