fn assert_ticket_operation_matches(
    operation: &TicketOperation,
    operation_key: &str,
    request_digest: &str,
    draft_request_digest: &str,
    ticket_id: &str,
    effect_claim_id: &str,
    mailbox_id: &str,
    source_message_id: &str,
    reply_mode: &str,
    idempotency_key: &str,
) -> Result<(), Value> {
    if operation.operation_key != operation_key
        || operation.request_digest != request_digest
        || operation.action_idempotency_key != idempotency_key
        || operation.draft_request_digest != draft_request_digest
        || operation.ticket_id != ticket_id
        || operation.effect_claim_id != effect_claim_id
        || operation.mailbox_id != mailbox_id
        || operation.source_message_id != source_message_id
        || operation.reply_mode != reply_mode
    {
        return Err(unavailable(
            "graph_ticket_draft_idempotency_conflict",
            operation_key,
        ));
    }
    Ok(())
}

fn find_ticket_remote_draft(
    policy: &Policy,
    mailbox_id: &str,
    operation_key: &str,
) -> Result<Option<Value>, Value> {
    let property_id = TICKET_DRAFT_OPERATION_PROPERTY_ID.replace('\'', "''");
    let property_value = operation_key.replace('\'', "''");
    let mut query = Map::new();
    query.insert(
        "$filter".to_string(),
        json!(format!("isDraft eq true and singleValueExtendedProperties/Any(ep: ep/id eq '{property_id}' and ep/value eq '{property_value}')")),
    );
    query.insert(
        "$expand".to_string(),
        json!(format!("singleValueExtendedProperties($filter=id eq '{property_id}')")),
    );
    query.insert(
        "$select".to_string(),
        json!("id,conversationId,subject,isDraft,createdDateTime,lastModifiedDateTime"),
    );
    query.insert("$top".to_string(), json!(2));
    let result = policy
        .adapter
        .request("GET", Some(mailbox_id), "messages", &query, None)?;
    let drafts = result
        .get("value")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .filter(|value| value.get("id").and_then(Value::as_str).is_some() && value.get("isDraft").and_then(Value::as_bool) != Some(false))
        .collect::<Vec<_>>();
    if drafts.len() > 1 {
        return Err(unavailable(
            "graph_ticket_draft_remote_identity_ambiguous",
            operation_key,
        ));
    }
    Ok(drafts.into_iter().next())
}

fn ticket_draft_ref_value(normalized: &Map<String, Value>, draft: &Value, draft_id: &str) -> Value {
    let mut reference = Map::new();
    reference.insert("schema".to_string(), json!("narada.graph_mail.ticket_draft_ref.v1"));
    for key in [
        "ticket_id",
        "effect_claim_id",
        "draft_operation_key",
        "draft_request_digest",
        "mailbox_id",
        "source_message_id",
        "reply_mode",
    ] {
        if let Some(value) = normalized.get(key) {
            reference.insert(key.to_string(), value.clone());
        }
    }
    reference.insert("draft_id".to_string(), json!(draft_id));
    if let Some(value) = draft.get("conversationId").and_then(Value::as_str) {
        reference.insert("conversation_id".to_string(), json!(value));
    }
    if let Some(value) = draft.get("@odata.etag").and_then(Value::as_str) {
        reference.insert("etag".to_string(), json!(value));
    }
    Value::Object(reference)
}

fn stable_receipt_id(operation_key: &str, draft_id: &str) -> String {
    let mut input = operation_key.as_bytes().to_vec();
    input.push(0);
    input.extend_from_slice(draft_id.as_bytes());
    format!("graph_draft_receipt_{}", &hex_lower(&Sha256::digest(input))[..32])
}

fn ticket_domain_operation(operation: &TicketOperation, replayed_or_recovered: bool) -> Result<Value, Value> {
    let (Some(draft_id), Some(receipt_id), Some(draft_ref), Some(completed_at)) = (
        operation.draft_id.as_ref(),
        operation.receipt_id.as_ref(),
        operation.draft_ref.as_ref(),
        operation.completed_at.as_ref(),
    ) else {
        return Err(unavailable("graph_ticket_draft_operation_incomplete", &operation.operation_key));
    };
    Ok(json!({
        "schema":"narada.domain_operation.v1",
        "operation_ref":format!("graph-mail-ticket-draft:{}", operation.operation_key),
        "outcome":"completed",
        "result":{
            "schema":"narada.graph_mail.ticket_draft_receipt.v1",
            "ticket_id":operation.ticket_id,
            "effect_claim_id":operation.effect_claim_id,
            "draft_operation_key":operation.operation_key,
            "draft_request_digest":operation.draft_request_digest,
            "receipt_id":receipt_id,
            "draft_id":draft_id,
            "draft_ref":draft_ref,
            "idempotency_replayed_or_recovered":replayed_or_recovered,
            "completed_at":completed_at
        }
    }))
}

fn canonical_json(value: &Value) -> String {
    serde_json::to_string(&canonical_value(value)).unwrap_or_else(|_| "null".to_string())
}

fn canonical_value(value: &Value) -> Value {
    match value {
        Value::Array(values) => Value::Array(values.iter().map(canonical_value).collect()),
        Value::Object(object) => {
            let mut keys = object.keys().cloned().collect::<Vec<_>>();
            keys.sort();
            let mut output = Map::new();
            for key in keys {
                if let Some(value) = object.get(&key) {
                    output.insert(key, canonical_value(value));
                }
            }
            Value::Object(output)
        }
        other => other.clone(),
    }
}

fn sha256_canonical(value: &Value) -> String {
    hex_lower(&Sha256::digest(canonical_json(value).as_bytes()))
}

fn draft_update(
    policy: &Policy,
    args: &Map<String, Value>,
    root: &Path,
) -> Result<Value, Value> {
    let draft_id = required_string(args, "draft_id")?;
    let patch = message_patch(args);
    let suffix = format!("messages/{}", encode_component(&draft_id));
    let body_replacement_requested = patch.contains_key("body");
    let mut reply_reference = None;
    if body_replacement_requested {
        let existing = policy
            .adapter
            .request("GET", mailbox(args), &suffix, &Map::new(), None)?;
        reply_reference = graph_reply_reference(&existing);
        if reply_reference.is_some()
            && args.get("allow_replace_full_body").and_then(Value::as_bool) != Some(true)
            && args.get("allow_replace_quoted_body").and_then(Value::as_bool) != Some(true)
        {
            record_audit(
                root,
                json!({
                    "event_kind":"draft_update_refused",
                    "mailbox_id":mailbox_value(args),
                    "draft_id":draft_id,
                    "reason":"reply_or_forward_body_replacement_requires_explicit_authorization"
                }),
            )?;
            return Ok(json!({
                "schema":"narada.graph_mail_mcp.draft.v1",
                "status":"refused",
                "reason":"reply_or_forward_body_replacement_requires_explicit_authorization",
                "draft_id":draft_id,
                "body_replacement":{"requested":true,"reply_or_forward":true,"authorization_required":true,"remediation":"Pass allow_replace_quoted_body=true or allow_replace_full_body=true, or update non-body fields separately."}
            }));
        }
    }
    record_audit(
        root,
        json!({
            "event_kind":"draft_update_requested",
            "mailbox_id":mailbox_value(args),
            "draft_id":draft_id
        }),
    )?;
    let result = policy.adapter.request(
        "PATCH",
        mailbox(args),
        &suffix,
        &Map::new(),
        Some(&Value::Object(patch)),
    )?;
    record_audit(
        root,
        json!({
            "event_kind":"draft_update_completed",
            "mailbox_id":mailbox_value(args),
            "draft_id":draft_id
        }),
    )?;
    Ok(json!({
        "schema":"narada.graph_mail_mcp.draft.v1",
        "status":"updated",
        "draft":result,
        "body_replacement":{
            "requested":body_replacement_requested,
            "reply_or_forward":reply_reference.is_some(),
            "authorization":if body_replacement_requested && reply_reference.is_some() { if args.get("allow_replace_full_body").and_then(Value::as_bool) == Some(true) { "allow_replace_full_body" } else { "allow_replace_quoted_body" } } else { "not_required" },
            "postcondition":if body_replacement_requested { "patch_accepted_by_graph" } else { "not_applicable" }
        }
    }))
}

