fn ticket_draft_upsert(
    policy: &Policy,
    args: &Map<String, Value>,
    root: &Path,
) -> Result<Value, Value> {
    let body_text = optional_string(args, "body_text");
    let body_html = optional_string(args, "body_html");
    if body_text.is_some() == body_html.is_some() {
        return Err(unavailable(
            "graph_ticket_draft_exactly_one_body_required",
            "provide exactly one of body_text or body_html",
        ));
    }
    let reply_mode = required_string(args, "reply_mode")?;
    if reply_mode != "reply" && reply_mode != "reply_all" {
        return Err(unavailable(
            "graph_ticket_draft_reply_mode_invalid",
            "reply_mode must be reply or reply_all",
        ));
    }
    let operation_key = required_string(args, "draft_operation_key")?;
    if operation_key.len() > 256
        || operation_key.is_empty()
        || !operation_key
            .chars()
            .all(|value| value.is_ascii_alphanumeric() || matches!(value, '.' | '_' | ':' | '-'))
    {
        return Err(unavailable(
            "graph_ticket_draft_operation_key_invalid",
            "operation key contains unsupported characters",
        ));
    }
    let admitted_digest = required_string(args, "draft_request_digest")?.to_ascii_lowercase();
    if admitted_digest.len() != 64 || !admitted_digest.chars().all(|value| value.is_ascii_hexdigit()) {
        return Err(unavailable(
            "graph_ticket_draft_request_digest_invalid",
            "draft request digest must be 64 hexadecimal characters",
        ));
    }
    let ticket_id = required_string(args, "ticket_id")?;
    let effect_claim_id = required_string(args, "effect_claim_id")?;
    let draft_source_id = required_string(args, "draft_source_id")?;
    let mailbox_id = required_string(args, "mailbox_id")?;
    let source_message_id = required_string(args, "source_message_id")?;
    let idempotency_key = required_string(args, "idempotency_key")?;
    let mut normalized = Map::new();
    normalized.insert("ticket_id".to_string(), json!(ticket_id));
    normalized.insert("effect_claim_id".to_string(), json!(effect_claim_id));
    normalized.insert("draft_operation_key".to_string(), json!(operation_key));
    normalized.insert("draft_request_digest".to_string(), json!(admitted_digest));
    normalized.insert("draft_source_id".to_string(), json!(draft_source_id));
    normalized.insert("mailbox_id".to_string(), json!(mailbox_id));
    normalized.insert("source_message_id".to_string(), json!(source_message_id));
    normalized.insert("reply_mode".to_string(), json!(reply_mode));
    if let Some(value) = body_text.as_deref() {
        normalized.insert("body_text".to_string(), json!(value));
    }
    if let Some(value) = body_html.as_deref() {
        normalized.insert("body_html".to_string(), json!(value));
    }
    normalized.insert("idempotency_key".to_string(), json!(idempotency_key));
    let mut draft_request = Map::new();
    draft_request.insert("source_id".to_string(), json!(draft_source_id));
    draft_request.insert("mailbox_id".to_string(), json!(mailbox_id));
    draft_request.insert("source_message_id".to_string(), json!(source_message_id));
    draft_request.insert("reply_mode".to_string(), json!(reply_mode));
    if let Some(value) = body_text.as_deref() {
        draft_request.insert("body_text".to_string(), json!(value));
    }
    if let Some(value) = body_html.as_deref() {
        draft_request.insert("body_html".to_string(), json!(value));
    }
    let actual_digest = sha256_canonical(&Value::Object(draft_request));
    if actual_digest != admitted_digest {
        return Err(unavailable(
            "graph_ticket_draft_request_digest_mismatch",
            &format!("{admitted_digest}:{actual_digest}"),
        ));
    }
    let request_digest = sha256_canonical(&Value::Object(normalized.clone()));
    let connection = ticket_store(root)?;
    connection
        .execute_batch("BEGIN IMMEDIATE")
        .map_err(|error| unavailable("graph_ticket_draft_transaction_failed", &error.to_string()))?;
    let outcome = (|| {
        let mut operation = find_ticket_operation(&connection, &operation_key)?;
        let replayed = operation.is_some();
        if let Some(existing) = operation.as_ref() {
            assert_ticket_operation_matches(
                existing,
                &operation_key,
                &request_digest,
                &admitted_digest,
                &ticket_id,
                &effect_claim_id,
                &mailbox_id,
                &source_message_id,
                &reply_mode,
                &idempotency_key,
            )?;
            if existing.status == "completed" {
                return ticket_domain_operation(existing, true);
            }
        } else {
            let now = now_rfc3339();
            connection
                .execute(
                    "insert into graph_ticket_draft_operations(operation_key, action_idempotency_key, request_digest, draft_request_digest, ticket_id, effect_claim_id, mailbox_id, source_message_id, reply_mode, status, draft_id, receipt_id, draft_ref_json, created_at, updated_at, completed_at) values (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, 'pending', null, null, null, ?10, ?10, null)",
                    params![operation_key, idempotency_key, request_digest, admitted_digest, ticket_id, effect_claim_id, mailbox_id, source_message_id, reply_mode, now],
                )
                .map_err(|error| unavailable("graph_ticket_draft_insert_failed", &error.to_string()))?;
            operation = find_ticket_operation(&connection, &operation_key)?;
        }
        let _existing = operation.ok_or_else(|| unavailable("graph_ticket_draft_operation_not_found", &operation_key))?;
        let mut draft = find_ticket_remote_draft(policy, &mailbox_id, &operation_key)?;
        let mut recovered = true;
        if draft.is_none() {
            recovered = false;
            let mut message = Map::new();
            if let Some(value) = normalized.get("body_text").and_then(Value::as_str) {
                message.insert("body".to_string(), json!({"contentType":"Text","content":value}));
            }
            if let Some(value) = normalized.get("body_html").and_then(Value::as_str) {
                message.insert("body".to_string(), json!({"contentType":"HTML","content":value}));
            }
            message.insert(
                "singleValueExtendedProperties".to_string(),
                json!([{"id":TICKET_DRAFT_OPERATION_PROPERTY_ID,"value":operation_key}]),
            );
            let action = if reply_mode == "reply" { "createReply" } else { "createReplyAll" };
            let suffix = format!("messages/{}/{}", encode_component(&source_message_id), action);
            record_audit(root, json!({
                "event_kind":"ticket_draft_create_requested",
                "ticket_id":ticket_id,
                "effect_claim_id":effect_claim_id,
                "draft_operation_key":operation_key,
                "mailbox_id":mailbox_id,
                "source_message_id":source_message_id,
                "reply_mode":reply_mode,
                "draft_request_digest":admitted_digest
            }))?;
            let created = policy.adapter.request(
                "POST",
                Some(&mailbox_id),
                &suffix,
                &Map::new(),
                Some(&json!({"message":Value::Object(message)})),
            )?;
            if created.get("isDraft").and_then(Value::as_bool) == Some(false)
                || created.get("id").and_then(Value::as_str).is_none()
            {
                return Err(unavailable("graph_ticket_draft_create_result_invalid", "Graph did not return an unsent draft"));
            }
            record_audit(root, json!({"event_kind":"ticket_draft_create_completed","ticket_id":ticket_id,"effect_claim_id":effect_claim_id,"draft_operation_key":operation_key,"draft_id":created.get("id").cloned().unwrap_or(Value::Null)}))?;
            draft = Some(created);
        }
        let draft = draft.ok_or_else(|| unavailable("graph_ticket_draft_create_result_invalid", "draft missing"))?;
        let draft_id = required_draft_id(&draft)?;
        let draft_ref = ticket_draft_ref_value(&normalized, &draft, &draft_id);
        let receipt_id = stable_receipt_id(&operation_key, &draft_id);
        let completed_at = now_rfc3339();
        connection
            .execute(
                "update graph_ticket_draft_operations set status='completed', draft_id=?1, receipt_id=?2, draft_ref_json=?3, updated_at=?4, completed_at=?4 where operation_key=?5 and status='pending'",
                params![draft_id, receipt_id, canonical_json(&draft_ref), completed_at, operation_key],
            )
            .map_err(|error| unavailable("graph_ticket_draft_completion_failed", &error.to_string()))?;
        let completed = find_ticket_operation(&connection, &operation_key)?
            .ok_or_else(|| unavailable("graph_ticket_draft_operation_not_found", &operation_key))?;
        ticket_domain_operation(&completed, replayed || recovered)
    })();
    match outcome {
        Ok(value) => {
            connection
                .execute_batch("COMMIT")
                .map_err(|error| unavailable("graph_ticket_draft_commit_failed", &error.to_string()))?;
            Ok(value)
        }
        Err(error) => {
            let _ = connection.execute_batch("ROLLBACK");
            Err(error)
        }
    }
}

