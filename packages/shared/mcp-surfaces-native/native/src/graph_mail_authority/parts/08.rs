fn ticket_draft_discard(
    policy: &Policy,
    args: &Map<String, Value>,
    root: &Path,
) -> Result<Value, Value> {
    if args.get("confirm_discard").and_then(Value::as_bool) != Some(true) {
        return Err(unavailable(
            "graph_ticket_draft_discard_confirmation_required",
            "confirm_discard=true is required",
        ));
    }
    let ticket_id = required_string(args, "ticket_id")?;
    let effect_claim_id = required_string(args, "effect_claim_id")?;
    let operation_key = required_string(args, "draft_operation_key")?;
    let mailbox_id = required_string(args, "mailbox_id")?;
    let draft_id = required_string(args, "draft_id")?;
    let idempotency_key = required_string(args, "idempotency_key")?;
    let request = json!({
        "ticket_id":ticket_id,
        "effect_claim_id":effect_claim_id,
        "draft_operation_key":operation_key,
        "mailbox_id":mailbox_id,
        "draft_id":draft_id,
        "idempotency_key":idempotency_key,
        "confirm_discard":true
    });
    let request_digest = sha256_canonical(&request);
    let connection = ticket_store(root)?;
    connection
        .execute_batch("BEGIN IMMEDIATE")
        .map_err(|error| unavailable("graph_ticket_draft_transaction_failed", &error.to_string()))?;
    let outcome = (|| {
        let operation = find_ticket_operation(&connection, &operation_key)?
            .ok_or_else(|| unavailable("graph_ticket_draft_operation_not_completed", &operation_key))?;
        if operation.status != "completed" {
            return Err(unavailable("graph_ticket_draft_operation_not_completed", &operation_key));
        }
        if operation.ticket_id != ticket_id
            || operation.effect_claim_id != effect_claim_id
            || operation.mailbox_id != mailbox_id
            || operation.draft_id.as_deref() != Some(draft_id.as_str())
        {
            return Err(unavailable("graph_ticket_draft_discard_linkage_mismatch", &operation_key));
        }
        let now = now_rfc3339();
        connection
            .execute(
                "insert into graph_ticket_draft_discard_intents(operation_key, idempotency_key, request_digest, status, verified_etag, verified_at, receipt_json, created_at, updated_at, completed_at) values (?1, ?2, ?3, 'pending', null, null, null, ?4, ?4, null) on conflict(operation_key) do nothing",
                params![operation_key, idempotency_key, request_digest, now],
            )
            .map_err(|error| unavailable("graph_ticket_draft_discard_intent_failed", &error.to_string()))?;
        let mut intent = find_discard_intent(&connection, &operation_key)?
            .ok_or_else(|| unavailable("graph_ticket_draft_discard_intent_not_found", &operation_key))?;
        if intent.idempotency_key != idempotency_key || intent.request_digest != request_digest {
            return Err(unavailable("graph_ticket_draft_discard_idempotency_conflict", &operation_key));
        }
        if intent.status == "completed" {
            let receipt = intent.receipt.take().ok_or_else(|| unavailable("graph_ticket_draft_discard_receipt_missing", &operation_key))?;
            return Ok(json!({"schema":"narada.graph_mail.ticket_draft_discard.v1","status":"discarded","disposition_receipt":receipt,"idempotency_replayed_or_recovered":true}));
        }
        let messages = find_ticket_remote_messages(policy, &mailbox_id, &operation_key)?;
        if messages.len() > 1 {
            return Err(unavailable("graph_ticket_draft_discard_remote_identity_ambiguous", &operation_key));
        }
        let Some(observed) = messages.into_iter().next() else {
            if intent.status != "verified" {
                return Err(unavailable("graph_ticket_draft_discard_absence_not_evidence", &operation_key));
            }
            let receipt = ticket_discard_receipt(&operation, "operator_authorized_graph_absence_after_verified_discard", false, true);
            complete_discard_intent(&connection, &operation_key, &receipt, &operation)?;
            return Ok(json!({"schema":"narada.graph_mail.ticket_draft_discard.v1","status":"discarded","disposition_receipt":receipt,"idempotency_replayed_or_recovered":true}));
        };
        let observed_id = observed.get("id").and_then(Value::as_str).ok_or_else(|| unavailable("graph_ticket_draft_discard_remote_identity_missing", &operation_key))?;
        if observed_id != draft_id {
            return Err(unavailable("graph_ticket_draft_discard_remote_identity_mismatch", &operation_key));
        }
        if observed.get("isDraft").and_then(Value::as_bool) != Some(true) {
            return Err(unavailable("graph_ticket_draft_discard_refused_not_draft", &operation_key));
        }
        let verifier = observed
            .get("@odata.etag")
            .and_then(Value::as_str)
            .or_else(|| observed.get("changeKey").and_then(Value::as_str))
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| unavailable("graph_ticket_draft_discard_remote_verifier_missing", &operation_key))?;
        connection
            .execute(
                "update graph_ticket_draft_discard_intents set status='verified', verified_etag=?1, verified_at=?2, updated_at=?2 where operation_key=?3 and status in ('pending','verified')",
                params![verifier, now_rfc3339(), operation_key],
            )
            .map_err(|error| unavailable("graph_ticket_draft_discard_verify_failed", &error.to_string()))?;
        record_audit(root, json!({"event_kind":"ticket_draft_discard_requested","ticket_id":ticket_id,"effect_claim_id":effect_claim_id,"draft_operation_key":operation_key,"mailbox_id":mailbox_id,"draft_id":draft_id}))?;
        let mut headers = Map::new();
        headers.insert("If-Match".to_string(), json!(verifier));
        policy.adapter.request_with_headers(
            "DELETE",
            Some(&mailbox_id),
            &format!("messages/{}", encode_component(&draft_id)),
            &Map::new(),
            None,
            &headers,
        )?;
        record_audit(root, json!({"event_kind":"ticket_draft_discard_completed","ticket_id":ticket_id,"effect_claim_id":effect_claim_id,"draft_operation_key":operation_key,"mailbox_id":mailbox_id,"draft_id":draft_id}))?;
        let receipt = ticket_discard_receipt(&operation, "operator_confirmed_graph_discard", true, false);
        complete_discard_intent(&connection, &operation_key, &receipt, &operation)?;
        Ok(json!({"schema":"narada.graph_mail.ticket_draft_discard.v1","status":"discarded","disposition_receipt":receipt,"idempotency_replayed_or_recovered":false}))
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

#[derive(Clone)]
struct DiscardIntent {
    idempotency_key: String,
    request_digest: String,
    status: String,
    receipt: Option<Value>,
}

fn find_discard_intent(connection: &Connection, operation_key: &str) -> Result<Option<DiscardIntent>, Value> {
    connection
        .query_row(
            "select idempotency_key, request_digest, status, receipt_json from graph_ticket_draft_discard_intents where operation_key=?1",
            params![operation_key],
            |row| {
                let receipt: Option<String> = row.get(3)?;
                Ok(DiscardIntent {
                    idempotency_key: row.get(0)?,
                    request_digest: row.get(1)?,
                    status: row.get(2)?,
                    receipt: receipt.and_then(|value| serde_json::from_str(&value).ok()),
                })
            },
        )
        .optional()
        .map_err(|error| unavailable("graph_ticket_draft_discard_database_read_failed", &error.to_string()))
}

fn find_ticket_remote_messages(
    policy: &Policy,
    mailbox_id: &str,
    operation_key: &str,
) -> Result<Vec<Value>, Value> {
    let property_id = TICKET_DRAFT_OPERATION_PROPERTY_ID.replace('\'', "''");
    let property_value = operation_key.replace('\'', "''");
    let mut query = Map::new();
    query.insert(
        "$filter".to_string(),
        json!(format!("singleValueExtendedProperties/Any(ep: ep/id eq '{property_id}' and ep/value eq '{property_value}')")),
    );
    query.insert(
        "$expand".to_string(),
        json!(format!("singleValueExtendedProperties($filter=id eq '{property_id}')")),
    );
    query.insert(
        "$select".to_string(),
        json!("id,isDraft,changeKey,createdDateTime,lastModifiedDateTime,sentDateTime,parentFolderId"),
    );
    query.insert("$top".to_string(), json!(2));
    let result = policy.adapter.request("GET", Some(mailbox_id), "messages", &query, None)?;
    Ok(result
        .get("value")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .filter(|value| value.get("id").and_then(Value::as_str).is_some())
        .collect())
}

fn ticket_discard_receipt(
    operation: &TicketOperation,
    evidence_kind: &str,
    graph_delete_confirmed: bool,
    graph_absence_confirmed: bool,
) -> Value {
    let draft_id = operation.draft_id.clone().unwrap_or_default();
    let observation_id = stable_disposition_observation_id(&operation.operation_key, "discarded", &draft_id);
    let mut receipt = json!({
        "schema":"narada.graph_mail.ticket_draft_disposition_receipt.v1",
        "observation_id":observation_id,
        "evidence_kind":evidence_kind,
        "evidence_id":observation_id,
        "disposition":"discarded",
        "ticket_id":operation.ticket_id,
        "effect_claim_id":operation.effect_claim_id,
        "draft_operation_key":operation.operation_key,
        "mailbox_id":operation.mailbox_id,
        "draft_id":draft_id,
        "observed_message_id":draft_id,
        "is_draft":true,
        "graph_delete_confirmed":graph_delete_confirmed,
        "graph_absence_confirmed":graph_absence_confirmed,
        "observed_at":now_rfc3339()
    });
    let digest = sha256_canonical(&receipt);
    receipt.as_object_mut().unwrap().insert("receipt_sha256".to_string(), json!(digest));
    receipt
}

fn complete_discard_intent(
    connection: &Connection,
    operation_key: &str,
    receipt: &Value,
    operation: &TicketOperation,
) -> Result<(), Value> {
    let receipt_json = canonical_json(receipt);
    connection
        .execute(
            "insert into graph_ticket_draft_disposition_observations(observation_id, operation_key, ticket_id, mailbox_id, draft_id, disposition, evidence_kind, evidence_id, receipt_json, observed_at) values (?1, ?2, ?3, ?4, ?5, 'discarded', ?6, ?7, ?8, ?9) on conflict(operation_key) do nothing",
            params![receipt.get("observation_id").and_then(Value::as_str).unwrap_or_default(), operation_key, operation.ticket_id, operation.mailbox_id, operation.draft_id.clone().unwrap_or_default(), receipt.get("evidence_kind").and_then(Value::as_str).unwrap_or_default(), receipt.get("evidence_id").and_then(Value::as_str).unwrap_or_default(), receipt_json, receipt.get("observed_at").and_then(Value::as_str).unwrap_or_default()],
        )
        .map_err(|error| unavailable("graph_ticket_draft_disposition_record_failed", &error.to_string()))?;
    connection
        .execute(
            "update graph_ticket_draft_discard_intents set status='completed', receipt_json=?1, updated_at=?2, completed_at=?2 where operation_key=?3 and status in ('verified','pending')",
            params![receipt_json, now_rfc3339(), operation_key],
        )
        .map_err(|error| unavailable("graph_ticket_draft_discard_completion_failed", &error.to_string()))?;
    Ok(())
}

fn stable_disposition_observation_id(operation_key: &str, disposition: &str, observed_message_id: &str) -> String {
    let input = format!("{operation_key}\0{disposition}\0{observed_message_id}");
    format!("graph_draft_disposition_{}", &hex_lower(&Sha256::digest(input.as_bytes()))[..32])
}

