
fn native_work_record_draft_receipt(
    server: &mut LifecycleServer,
    args: Value,
) -> Result<Value, String> {
    let ticket_id = required_string(&args, "ticket_id")?;
    let claim_id = required_string(&args, "effect_claim_id")?;
    let draft_operation = required_string(&args, "draft_operation_key")?;
    let request_digest = required_string(&args, "draft_request_digest")?;
    if request_digest.len() != 64
        || !request_digest.chars().all(|value| value.is_ascii_hexdigit() && !value.is_ascii_uppercase())
    {
        return Err("draft_request_digest_invalid".to_string());
    }
    let receipt_id = required_string(&args, "receipt_id")?;
    let draft_id = required_string(&args, "draft_id")?;
    let draft_ref = args.get("draft_ref").cloned().unwrap_or_else(||json!({}));
    let draft_ref_json = native_work_ref_json(&draft_ref, "draft_ref")?;
    let operation_key = required_string(&args, "idempotency_key")?;
    let causation_id = required_string(&args, "causation_id")?;
    let request = json!({
        "ticket_id":ticket_id,
        "effect_claim_id":claim_id,
        "draft_operation_key":draft_operation,
        "draft_request_digest":request_digest,
        "receipt_id":receipt_id,
        "draft_id":draft_id,
        "draft_ref":draft_ref,
        "idempotency_key":operation_key,
        "causation_id":causation_id
    });
    let request_hash = native_canonical_digest(&request);
    if let Some(existing) =
        native_work_existing_operation(server, &operation_key, &request_hash)?
    {
        let mut wrapped = existing;
        if wrapped.get("status").and_then(Value::as_str) == Some("recorded") {
            if let Some(object) = wrapped.as_object_mut() {
                object.insert("status".to_string(), Value::String("already_recorded".to_string()));
            }
        }
        return Ok(native_work_domain(&operation_key, wrapped));
    }
    let claim = server
        .query_one(
            "select ticket_id,ticket_revision,operation_key,request_digest,status
               from ticket_effect_claims where claim_id=?1 and ticket_id=?2",
            params![&claim_id,&ticket_id],
        )?
        .ok_or("ticket_effect_claim_not_found")?;
    if claim.get("operation_key").and_then(Value::as_str) != Some(draft_operation.as_str()) {
        return Err("ticket_effect_claim_operation_mismatch".to_string());
    }
    if claim.get("request_digest").and_then(Value::as_str) != Some(request_digest.as_str()) {
        return Err("ticket_effect_claim_request_digest_mismatch".to_string());
    }
    let ticket = native_work_ticket(server, &ticket_id)?;
    let current_revision = ticket.get("revision").and_then(Value::as_i64).unwrap_or(0);
    let claim_revision = claim.get("ticket_revision").and_then(Value::as_i64).unwrap_or(0);
    if current_revision != claim_revision {
        let timestamp = now();
        server.connection_mut()?.execute(
            "update ticket_effect_claims set status='superseded' where claim_id=?1",
            params![&claim_id],
        ).map_err(db_error)?;
        let event_id = native_work_event(
            server,
            &ticket,
            "ticket.draft_effect.superseded",
            &causation_id,
            &operation_key,
            "work.ticket-lifecycle.v1",
            &json!({"effect_claim_id":claim_id,"claimed_revision":claim_revision,"current_revision":current_revision}),
        )?;
        let result = json!({"status":"superseded","ticket":ticket,"event_id":event_id});
        native_work_record_operation(server,&operation_key,"ticket.draft.receipt",&request_hash,Some(&ticket_id),Some(current_revision),&result)?;
        let _ = timestamp;
        return Ok(native_work_domain(&operation_key,result));
    }
    let timestamp = now();
    {
        let connection = server.connection_mut()?;
        connection.execute(
            "update ticket_effect_claims set status='completed',receipt_id=?1,receipt_json=?2,completed_at=?3 where claim_id=?4 and status='claimed'",
            params![&receipt_id, &serde_json::to_string(&native_canonical_value(&json!({"draft_id":draft_id,"draft_ref":draft_ref}))).unwrap_or_else(|_|"{}".to_string()), &timestamp, &claim_id],
        ).map_err(db_error)?;
        connection.execute(
            "insert into ticket_draft_refs(
                ticket_id,draft_id,effect_claim_id,draft_ref_json,receipt_id,
                disposition,disposition_evidence_kind,disposition_evidence_id,
                disposition_evidence_json,created_at,disposed_at
             ) values(?1,?2,?3,?4,?5,null,null,null,null,?6,null)
             on conflict(ticket_id,draft_id) do update set
               draft_ref_json=excluded.draft_ref_json,receipt_id=excluded.receipt_id",
            params![&ticket_id,&draft_id,&claim_id,&draft_ref_json,&receipt_id,&timestamp],
        ).map_err(db_error)?;
        let old_drafts = {
            let mut statement = connection.prepare(
                "select draft_id from ticket_draft_refs where ticket_id=?1 and draft_id<>?2 and disposition is null order by created_at,draft_id",
            ).map_err(db_error)?;
            let rows = statement.query_map(params![&ticket_id,&draft_id], |row|row.get::<_,String>(0)).map_err(db_error)?;
            rows.collect::<Result<Vec<_>,_>>().map_err(db_error)?
        };
        for old in &old_drafts {
            let evidence = native_canonical_digest(&json!({
                "schema":"narada.work_lifecycle.draft_supersession.v1",
                "superseded_by_draft_id":draft_id,
                "superseded_by_effect_claim_id":claim_id
            }));
            connection.execute(
                "update ticket_draft_refs set disposition='superseded',
                    disposition_evidence_kind='system_superseded_by_newer_draft',
                    disposition_evidence_id=?1,disposition_evidence_json=?2,
                    disposed_at=?3 where ticket_id=?4 and draft_id=?5",
                params![&draft_id,&evidence,&timestamp,&ticket_id,old],
            ).map_err(db_error)?;
        }
    }
    let ticket = native_work_transition(
        server,
        &ticket_id,
        "waiting_on_draft",
        None,
        None,
        None,
        None,
    )?;
    let event_id = native_work_event(
        server,
        &ticket,
        "ticket.draft.receipt_recorded",
        &causation_id,
        &operation_key,
        "work.ticket-lifecycle.v1",
        &json!({"effect_claim_id":claim_id,"draft_id":draft_id,"receipt_id":receipt_id}),
    )?;
    let result = json!({"status":"recorded","ticket":ticket,"event_id":event_id});
    native_work_record_operation(server,&operation_key,"ticket.draft.receipt",&request_hash,Some(&ticket_id),ticket.get("revision").and_then(Value::as_i64),&result)?;
    Ok(native_work_domain(&operation_key,result))
}

fn native_work_reconcile_draft(
    server: &mut LifecycleServer,
    args: Value,
) -> Result<Value, String> {
    let ticket_id = required_string(&args, "ticket_id")?;
    let draft_id = required_string(&args, "draft_id")?;
    let evidence = args
        .get("evidence")
        .filter(|value| value.is_object())
        .ok_or("ticket_draft_disposition_evidence_required")?
        .clone();
    if evidence.get("schema").and_then(Value::as_str)
        != Some("narada.graph_mail.ticket_draft_disposition_receipt.v1")
    {
        return Err("ticket_draft_disposition_evidence_schema_invalid".to_string());
    }
    let disposition = evidence
        .get("disposition")
        .and_then(Value::as_str)
        .ok_or("ticket_draft_disposition_evidence_state_invalid")?;
    let evidence_kind = evidence
        .get("evidence_kind")
        .and_then(Value::as_str)
        .ok_or("ticket_draft_disposition_evidence_state_invalid")?;
    let sent = evidence_kind == "synchronized_graph_observation"
        && disposition == "sent"
        && evidence.get("is_draft").and_then(Value::as_bool) == Some(false);
    let confirmed_discard = evidence_kind == "operator_confirmed_graph_discard"
        && disposition == "discarded"
        && evidence.get("is_draft").and_then(Value::as_bool) == Some(true)
        && evidence.get("graph_delete_confirmed").and_then(Value::as_bool) == Some(true)
        && evidence.get("graph_absence_confirmed").and_then(Value::as_bool) == Some(false);
    let recovered_discard = evidence_kind
        == "operator_authorized_graph_absence_after_verified_discard"
        && disposition == "discarded"
        && evidence.get("is_draft").and_then(Value::as_bool) == Some(true)
        && evidence.get("graph_delete_confirmed").and_then(Value::as_bool) == Some(false)
        && evidence.get("graph_absence_confirmed").and_then(Value::as_bool) == Some(true);
    if !sent && !confirmed_discard && !recovered_discard {
        return Err("ticket_draft_disposition_evidence_state_invalid".to_string());
    }
    for (field, code) in [
        ("observation_id", "draft_disposition_observation_id"),
        ("evidence_id", "draft_disposition_evidence_id"),
        ("ticket_id", "draft_disposition_evidence_ticket_id"),
        ("effect_claim_id", "draft_disposition_effect_claim_id"),
        ("draft_operation_key", "draft_disposition_operation_key"),
        ("mailbox_id", "draft_disposition_mailbox_id"),
        ("draft_id", "draft_disposition_evidence_draft_id"),
        ("observed_message_id", "draft_disposition_message_id"),
        ("observed_at", "draft_disposition_observed_at"),
        ("receipt_sha256", "draft_disposition_receipt_sha256"),
    ] {
        if evidence
            .get(field)
            .and_then(Value::as_str)
            .is_none_or(|value| value.trim().is_empty())
        {
            return Err(format!("{code}_required"));
        }
    }
    if evidence.get("observation_id") != evidence.get("evidence_id") {
        return Err("ticket_draft_disposition_evidence_identity_mismatch".to_string());
    }
    let receipt_sha = evidence
        .get("receipt_sha256")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if receipt_sha.len() != 64
        || !receipt_sha
            .chars()
            .all(|value| value.is_ascii_hexdigit() && !value.is_ascii_uppercase())
    {
        return Err("ticket_draft_disposition_receipt_sha256_invalid".to_string());
    }
    let mut unsigned = evidence.clone();
    unsigned
        .as_object_mut()
        .ok_or("ticket_draft_disposition_evidence_required")?
        .remove("receipt_sha256");
    if native_canonical_digest(&unsigned) != receipt_sha {
        return Err("ticket_draft_disposition_receipt_digest_mismatch".to_string());
    }
    let evidence_json = native_work_ref_json(&evidence, "draft_disposition_evidence")?;
    if evidence.get("ticket_id").and_then(Value::as_str) != Some(ticket_id.as_str())
        || evidence.get("draft_id").and_then(Value::as_str) != Some(draft_id.as_str())
    {
        return Err("ticket_draft_disposition_evidence_target_mismatch".to_string());
    }
    let operation_key = required_string(&args, "idempotency_key")?;
    let causation_id = required_string(&args, "causation_id")?;
    let request = json!({
        "ticket_id":ticket_id,
        "draft_id":draft_id,
        "evidence":evidence,
        "idempotency_key":operation_key,
        "causation_id":causation_id
    });
    let request_hash = native_canonical_digest(&request);
    if let Some(existing) =
        native_work_existing_operation(server, &operation_key, &request_hash)?
    {
        if let Some(object) = existing.as_object() {
            let mut replay = object.clone();
            replay.insert("status".to_string(), Value::String("already_reconciled".to_string()));
            return Ok(native_work_domain(&operation_key, Value::Object(replay)));
        }
        return Ok(native_work_domain(&operation_key, existing));
    }
    let draft = server
        .query_one(
            "select effect_claim_id,draft_ref_json from ticket_draft_refs
              where ticket_id=?1 and draft_id=?2",
            params![&ticket_id,&draft_id],
        )?
        .ok_or("ticket_draft_ref_not_found")?;
    let draft_ref = draft
        .get("draft_ref_json")
        .and_then(Value::as_str)
        .and_then(|value| serde_json::from_str::<Value>(value).ok())
        .unwrap_or_else(||json!({}));
    if draft.get("effect_claim_id").and_then(Value::as_str)
        != evidence.get("effect_claim_id").and_then(Value::as_str)
        || draft_ref.get("draft_operation_key").and_then(Value::as_str)
            != evidence.get("draft_operation_key").and_then(Value::as_str)
        || draft_ref.get("mailbox_id").and_then(Value::as_str)
            != evidence.get("mailbox_id").and_then(Value::as_str)
    {
        return Err("ticket_draft_disposition_evidence_linkage_mismatch".to_string());
    }
    let timestamp = now();
    let mut superseded = Vec::new();
    {
        let connection = server.connection_mut()?;
        if disposition == "sent" {
            let mut statement = connection
                .prepare(
                    "select draft_id from ticket_draft_refs
                      where ticket_id=?1 and draft_id<>?2 and disposition is null
                      order by created_at,draft_id",
                )
                .map_err(db_error)?;
            let rows = statement
                .query_map(params![&ticket_id,&draft_id], |row| row.get::<_,String>(0))
                .map_err(db_error)?;
            superseded = rows.collect::<Result<Vec<_>,_>>().map_err(db_error)?;
        }
        for old in &superseded {
            let supersession = if disposition == "sent" {
                json!({
                    "schema":"narada.work_lifecycle.draft_supersession.v1",
                    "superseded_by_sent_draft_id":draft_id,
                    "sent_disposition_evidence_id":evidence.get("evidence_id")
                })
            } else {
                json!({
                    "schema":"narada.work_lifecycle.draft_supersession.v1",
                    "superseded_by_draft_id":draft_id,
                    "superseded_by_effect_claim_id":evidence.get("effect_claim_id")
                })
            };
            connection
                .execute(
                    "update ticket_draft_refs set disposition='superseded',
                        disposition_evidence_kind=?1,disposition_evidence_id=?2,
                        disposition_evidence_json=?3,disposed_at=?4
                      where ticket_id=?5 and draft_id=?6",
                    params![
                        if disposition == "sent" {
                            "system_superseded_by_sent_draft"
                        } else {
                            "system_superseded_by_newer_draft"
                        },
                        draft_id,
                        serde_json::to_string(&native_canonical_value(&supersession))
                            .unwrap_or_else(|_|"{}".to_string()),
                        &timestamp,
                        &ticket_id,
                        old
                    ],
                )
                .map_err(db_error)?;
        }
        connection
            .execute(
                "update ticket_draft_refs
                    set disposition=?1,disposition_evidence_kind=?2,
                        disposition_evidence_id=?3,disposition_evidence_json=?4,
                        disposed_at=?5
                  where ticket_id=?6 and draft_id=?7",
                params![
                    disposition,
                    evidence_kind,
                    evidence.get("evidence_id").and_then(Value::as_str),
                    &evidence_json,
                    &timestamp,
                    &ticket_id,
                    &draft_id
                ],
            )
            .map_err(db_error)?;
    }
    let ticket = native_work_transition(
        server,
        &ticket_id,
        "actionable",
        None,
        None,
        None,
        None,
    )?;
    let event_id = native_work_event(
        server,
        &ticket,
        "ticket.draft.disposition",
        &causation_id,
        &operation_key,
        "work.ticket-work-due.v1",
        &json!({
            "draft_id":draft_id,
            "disposition":disposition,
            "evidence_kind":evidence_kind,
            "evidence_id":evidence.get("evidence_id"),
            "superseded_draft_ids":superseded
        }),
    )?;
    let result = json!({
        "status":"reconciled",
        "ticket":ticket,
        "event_id":event_id
    });
    native_work_record_operation(
        server,
        &operation_key,
        "ticket.draft.disposition",
        &request_hash,
        Some(&ticket_id),
        ticket.get("revision").and_then(Value::as_i64),
        &result,
    )?;
    Ok(native_work_domain(&operation_key, result))
}
