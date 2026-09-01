
fn native_work_ticket_sources(
    server: &LifecycleServer,
    args: Value,
) -> Result<Value, String> {
    let ticket_id = required_string(&args, "ticket_id")?;
    let shown = native_work_ticket_show(server, json!({"ticket_id":ticket_id}))?;
    Ok(json!({
        "schema":"narada.work_lifecycle.ticket_sources.v1",
        "ticket_id":ticket_id,
        "sources":shown.get("sources").cloned().unwrap_or_else(||json!([]))
    }))
}

fn native_work_admit_source(
    server: &mut LifecycleServer,
    args: Value,
) -> Result<Value, String> {
    let source_kind = required_string(&args, "source_kind")?;
    let source_scope = required_string(&args, "source_scope")?;
    let immutable_source_id = required_string(&args, "immutable_source_id")?;
    let operation_key = required_string(&args, "idempotency_key")?;
    let causation_id = required_string(&args, "causation_id")?;
    let policy_version = required_string(&args, "policy_version")?;
    let summary = required_string(&args, "summary")?;
    if summary.as_bytes().len() > 2_048 {
        return Err("summary_too_large".to_string());
    }
    let source_ref = args.get("source_ref").cloned().unwrap_or_else(|| json!({}));
    let source_ref_json = native_work_ref_json(&source_ref, "source_ref")?;
    let work_due_policy = args
        .get("work_due_policy")
        .and_then(Value::as_str)
        .unwrap_or("deferred");
    if !matches!(work_due_policy, "deferred" | "inline") {
        return Err("work_due_policy_invalid".to_string());
    }
    let mut correlation_keys = Vec::new();
    let values = args
        .get("correlation_keys")
        .and_then(Value::as_array)
        .ok_or("correlation_keys_required")?;
    for candidate in values {
        let kind = required_string(candidate, "kind")
            .map_err(|_| "correlation_kind_required".to_string())?;
        let scope = required_string(candidate, "scope")
            .map_err(|_| "correlation_scope_required".to_string())?;
        let value = required_string(candidate, "value")
            .map_err(|_| "correlation_value_required".to_string())?;
        if value.as_bytes().len() > 1_024 {
            return Err("correlation_value_too_large".to_string());
        }
        correlation_keys.push(json!({"kind":kind,"scope":scope,"value":value}));
    }
    let request = json!({
        "source_kind":source_kind,
        "source_scope":source_scope,
        "immutable_source_id":immutable_source_id,
        "idempotency_key":operation_key,
        "causation_id":causation_id,
        "policy_version":policy_version,
        "summary":summary,
        "source_ref_json":source_ref_json,
        "correlation_keys":correlation_keys,
        "work_due_policy":work_due_policy
    });
    let request_digest = native_canonical_digest(&request);
    if let Some(existing) =
        native_work_existing_operation(server, &operation_key, &request_digest)?
    {
        return Ok(native_work_domain(&operation_key, existing));
    }
    let timestamp = now();
    let existing_source: Option<(String, String, String)> = server
        .connection()?
        .query_row(
            "select source_id,ticket_id,receipt_id from ticket_sources
              where source_kind=?1 and source_scope=?2 and immutable_source_id=?3",
            params![&source_kind,&source_scope,&immutable_source_id],
            |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?)),
        )
        .optional()
        .map_err(db_error)?;
    if let Some((source_id, ticket_id, receipt_id)) = existing_source {
        let event_id = server
            .query_one(
                "select event_id from work_lifecycle_events
                  where aggregate_kind='ticket' and aggregate_id=?1
                    and event_type in ('ticket.created','ticket.source.admitted')
                    and json_extract(payload_json,'$.source_id')=?2
                  order by aggregate_revision,created_at,event_id limit 1",
                params![&ticket_id,&source_id],
            )?
            .and_then(|value| value.get("event_id").cloned())
            .unwrap_or_else(|| {
                Value::String(native_work_stable_id(
                    "event",
                    &json!({"ticket_id":ticket_id,"source_id":source_id}),
                    32,
                ))
            });
        let ticket = native_work_ticket(server, &ticket_id)?;
        let result = json!({
            "schema":"narada.work_lifecycle.ticket_source_admission.v1",
            "status":"already_associated",
            "ticket_id":ticket.get("ticket_id"),
            "ticket_number":ticket.get("ticket_number"),
            "ticket_revision":ticket.get("revision"),
            "source_id":source_id,
            "receipt_id":receipt_id,
            "event_id":event_id
        });
        native_work_record_operation(
            server,&operation_key,"ticket.admit_source",&request_digest,
            Some(&ticket_id),ticket.get("revision").and_then(Value::as_i64),&result
        )?;
        return Ok(native_work_domain(&operation_key,result));
    }
    let mut candidates = Vec::new();
    for key in &correlation_keys {
        let candidate: Option<String> = server
            .connection()?
            .query_row(
                "select ticket_id from ticket_correlation_keys
                  where kind=?1 and scope=?2 and value=?3",
                params![
                    key.get("kind").and_then(Value::as_str).unwrap_or_default(),
                    key.get("scope").and_then(Value::as_str).unwrap_or_default(),
                    key.get("value").and_then(Value::as_str).unwrap_or_default()
                ],
                |row| row.get(0),
            )
            .optional()
            .map_err(db_error)?;
        if let Some(candidate) = candidate {
            if !candidates.contains(&candidate) {
                candidates.push(candidate);
            }
        }
    }
    candidates.sort();
    let receipt_id = native_work_stable_id(
        "receipt_source",
        &json!({"source_kind":source_kind,"source_scope":source_scope,"immutable_source_id":immutable_source_id}),
        32,
    );
    if candidates.len() > 1 {
        let result = json!({
            "schema":"narada.work_lifecycle.ticket_source_admission.v1",
            "status":"blocked",
            "ticket_id":null,
            "ticket_number":null,
            "ticket_revision":null,
            "source_id":null,
            "receipt_id":receipt_id,
            "reason":"ambiguous_correlation",
            "candidate_ticket_ids":candidates
        });
        native_work_record_operation(server,&operation_key,"ticket.admit_source",&request_digest,None,None,&result)?;
        return Ok(native_work_domain(&operation_key,result));
    }
    let (ticket_id, ticket_number) = {
        let connection = server.connection_mut()?;
        if candidates.is_empty() {
            let ticket_number: i64 = connection
                .query_row(
                    "select next_value from work_sequences where sequence_name='ticket'",
                    [],
                    |row| row.get(0),
                )
                .map_err(db_error)?;
            connection
                .execute(
                    "update work_sequences set next_value=next_value+1 where sequence_name='ticket'",
                    [],
                )
                .map_err(db_error)?;
            let ticket_id = format!("ticket-{ticket_number}");
            connection
                .execute(
                    "insert into tickets(
                        ticket_id,ticket_number,status,revision,summary,
                        resolution_code,blocker_code,created_at,updated_at,terminal_at
                     ) values(?1,?2,'actionable',1,?3,null,null,?4,?4,null)",
                    params![&ticket_id,ticket_number,&summary,&timestamp],
                )
                .map_err(db_error)?;
            (ticket_id,ticket_number)
        } else {
            let ticket_id = candidates[0].clone();
            connection
                .execute(
                    "update tickets set status='actionable',revision=revision+1,
                            summary=?1,resolution_code=null,blocker_code=null,
                            terminal_at=null,updated_at=?2 where ticket_id=?3",
                    params![&summary,&timestamp,&ticket_id],
                )
                .map_err(db_error)?;
            let ticket_number: i64 = connection
                .query_row(
                    "select ticket_number from tickets where ticket_id=?1",
                    params![&ticket_id],
                    |row| row.get(0),
                )
                .map_err(db_error)?;
            (ticket_id,ticket_number)
        }
    };
    let source_id = native_work_stable_id(
        "source",
        &json!({"source_kind":source_kind,"source_scope":source_scope,"immutable_source_id":immutable_source_id}),
        32,
    );
    {
        let connection = server.connection_mut()?;
        connection
            .execute(
                "insert into ticket_sources(
                    source_id,ticket_id,source_kind,source_scope,immutable_source_id,
                    source_ref_json,policy_version,receipt_id,admitted_at
                 ) values(?1,?2,?3,?4,?5,?6,?7,?8,?9)",
                params![&source_id,&ticket_id,&source_kind,&source_scope,
                        &immutable_source_id,&source_ref_json,&policy_version,&receipt_id,&timestamp],
            )
            .map_err(db_error)?;
        for key in &correlation_keys {
            let kind = key.get("kind").and_then(Value::as_str).unwrap_or_default();
            let scope = key.get("scope").and_then(Value::as_str).unwrap_or_default();
            let value = key.get("value").and_then(Value::as_str).unwrap_or_default();
            let existing: Option<String> = connection
                .query_row(
                    "select ticket_id from ticket_correlation_keys
                      where kind=?1 and scope=?2 and value=?3",
                    params![kind,scope,value],
                    |row| row.get(0),
                )
                .optional()
                .map_err(db_error)?;
            if let Some(existing) = existing {
                if existing != ticket_id {
                    return Err("ticket_source_correlation_conflict".to_string());
                }
            } else {
                connection
                    .execute(
                        "insert into ticket_correlation_keys(
                            kind,scope,value,ticket_id,policy_version,admitted_at
                         ) values(?1,?2,?3,?4,?5,?6)",
                        params![kind,scope,value,&ticket_id,&policy_version,&timestamp],
                    )
                    .map_err(db_error)?;
            }
        }
    }
    let ticket = native_work_ticket(server,&ticket_id)?;
    let event_type = if candidates.is_empty() {"ticket.created"} else {"ticket.source.admitted"};
    let topic = if work_due_policy == "inline" {
        "work.ticket-inline-processing.v1"
    } else {
        "work.ticket-work-due.v1"
    };
    let event_id = native_work_event(
        server,&ticket,event_type,&causation_id,&operation_key,topic,
        &json!({"source_id":source_id,"source_kind":source_kind}),
    )?;
    let result = json!({
        "schema":"narada.work_lifecycle.ticket_source_admission.v1",
        "status":if candidates.is_empty(){"created"}else{"attached"},
        "ticket_id":ticket_id,
        "ticket_number":ticket_number,
        "ticket_revision":ticket.get("revision"),
        "source_id":source_id,
        "receipt_id":receipt_id,
        "event_id":event_id
    });
    native_work_record_operation(
        server,&operation_key,"ticket.admit_source",&request_digest,
        Some(&ticket_id),ticket.get("revision").and_then(Value::as_i64),&result
    )?;
    Ok(native_work_domain(&operation_key,result))
}
