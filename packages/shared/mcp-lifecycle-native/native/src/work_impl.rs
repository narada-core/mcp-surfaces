/*
 * Native Work Lifecycle authority operations.
 *
 * The task and work MCP adapters share the same SQLite authority; these
 * operations preserve Work's revision, idempotency, event, and outbox rules.
 */

fn native_work_transaction<T, F>(server: &mut LifecycleServer, operation: F) -> Result<T, String>
where
    F: FnOnce(&mut LifecycleServer) -> Result<T, String>,
{
    server.connection_mut()?.execute_batch("BEGIN IMMEDIATE").map_err(db_error)?;
    match operation(server) {
        Ok(value) => {
            server.connection_mut()?.execute_batch("COMMIT").map_err(db_error)?;
            Ok(value)
        }
        Err(error) => {
            let _ = server.connection_mut()?.execute_batch("ROLLBACK");
            Err(error)
        }
    }
}
fn native_work_stable_id(prefix: &str, value: &Value, length: usize) -> String {
    let digest = native_canonical_digest(value);
    format!("{}_{}", prefix, &digest[..length.min(digest.len())])
}

fn native_work_ref_json(value: &Value, field: &str) -> Result<String, String> {
    let Some(object) = value.as_object() else {
        return Err(format!("{field}_required"));
    };
    let forbidden = [
        "body",
        "body_html",
        "body_text",
        "content",
        "email_body",
        "html",
        "raw",
        "raw_message",
        "transcript",
    ];
    fn inspect(value: &Value, path: &str, forbidden: &[&str], field_name: &str) -> Result<(), String> {
        if let Some(object) = value.as_object() {
            for (key, nested) in object {
                if forbidden
                    .iter()
                    .any(|candidate| candidate.eq_ignore_ascii_case(key))
                {
                    return Err(format!("{field_name}_contains_unbounded_payload:{path}.{key}"));
                }
                inspect(nested, &format!("{path}.{key}"), forbidden, field_name)?;
            }
        } else if let Some(values) = value.as_array() {
            for (index, nested) in values.iter().enumerate() {
                inspect(nested, &format!("{path}[{index}]"), forbidden, field_name)?;
            }
        }
        Ok(())
    }
    inspect(value, field, &forbidden, field)?;
    let encoded = serde_json::to_string(&native_canonical_value(&Value::Object(object.clone())))
        .map_err(|e| format!("{field}_invalid:{e}"))?;
    if encoded.len() > 16_384 {
        return Err(format!("{field}_too_large"));
    }
    Ok(encoded)
}

fn native_work_domain(operation_key: &str, result: Value) -> Value {
    json!({
        "schema":"narada.domain_operation.v1",
        "operation_ref":format!("work-lifecycle:{operation_key}"),
        "outcome":"completed",
        "result":result
    })
}

fn native_work_existing_operation(
    server: &LifecycleServer,
    operation_key: &str,
    request_digest: &str,
) -> Result<Option<Value>, String> {
    let row = server.query_one(
        "select request_digest,result_json from work_operations where operation_key=?1",
        params![operation_key],
    )?;
    let Some(row) = row else {
        return Ok(None);
    };
    if row.get("request_digest").and_then(Value::as_str) != Some(request_digest) {
        return Err(format!(
            "work_operation_idempotency_conflict:{operation_key}"
        ));
    }
    let result = row
        .get("result_json")
        .and_then(Value::as_str)
        .ok_or("work_operation_result_invalid")?;
    serde_json::from_str(result)
        .map(Some)
        .map_err(|e| format!("work_operation_result_invalid:{e}"))
}

fn native_work_record_operation(
    server: &mut LifecycleServer,
    operation_key: &str,
    operation_kind: &str,
    request_digest: &str,
    aggregate_id: Option<&str>,
    aggregate_revision: Option<i64>,
    result: &Value,
) -> Result<(), String> {
    let encoded = serde_json::to_string(&native_canonical_value(result))
        .map_err(|e| format!("operation_result_invalid:{e}"))?;
    if encoded.len() > 32_768 {
        return Err("operation_result_too_large".to_string());
    }
    server
        .connection_mut()?
        .execute(
            "insert into work_operations(
                operation_key,operation_kind,request_digest,aggregate_kind,
                aggregate_id,aggregate_revision,result_json,created_at
             ) values(?1,?2,?3,'ticket',?4,?5,?6,?7)",
            params![
                operation_key,
                operation_kind,
                request_digest,
                aggregate_id,
                aggregate_revision,
                encoded,
                now()
            ],
        )
        .map_err(db_error)?;
    Ok(())
}

fn native_work_ticket(server: &LifecycleServer, ticket_id: &str) -> Result<Value, String> {
    server
        .query_one(
            "select ticket_id,ticket_number,status,revision,summary,
                    resolution_code,blocker_code,created_at,updated_at,terminal_at
               from tickets where ticket_id=?1",
            params![ticket_id],
        )?
        .ok_or_else(|| format!("ticket_not_found:{ticket_id}"))
}

fn native_work_ticket_by_args(
    server: &LifecycleServer,
    args: &Value,
) -> Result<Value, String> {
    if let Some(ticket_id) = string_arg(args, "ticket_id") {
        return native_work_ticket(server, &ticket_id);
    }
    if let Some(ticket_number) = args.get("ticket_number").and_then(Value::as_i64) {
        return server
            .query_one(
                "select ticket_id,ticket_number,status,revision,summary,
                        resolution_code,blocker_code,created_at,updated_at,terminal_at
                   from tickets where ticket_number=?1",
                params![ticket_number],
            )?
            .ok_or_else(|| format!("ticket_not_found:{ticket_number}"));
    }
    Err("ticket_identity_required".to_string())
}

fn native_work_transition(
    server: &mut LifecycleServer,
    ticket_id: &str,
    status: &str,
    summary: Option<&str>,
    resolution_code: Option<&str>,
    blocker_code: Option<&str>,
    terminal_at: Option<&str>,
) -> Result<Value, String> {
    let current = native_work_ticket(server, ticket_id)?;
    let summary = summary
        .map(ToString::to_string)
        .or_else(|| current.get("summary").and_then(Value::as_str).map(ToString::to_string))
        .unwrap_or_default();
    server
        .connection_mut()?
        .execute(
            "update tickets
                set status=?1,revision=revision+1,summary=?2,
                    resolution_code=?3,blocker_code=?4,terminal_at=?5,
                    updated_at=?6
              where ticket_id=?7",
            params![
                status,
                summary,
                resolution_code,
                blocker_code,
                terminal_at,
                now(),
                ticket_id
            ],
        )
        .map_err(db_error)?;
    native_work_ticket(server, ticket_id)
}

fn native_work_event(
    server: &mut LifecycleServer,
    ticket: &Value,
    event_type: &str,
    causation_id: &str,
    idempotency_key: &str,
    topic: &str,
    extra: &Value,
) -> Result<String, String> {
    let ticket_id = ticket
        .get("ticket_id")
        .and_then(Value::as_str)
        .ok_or("ticket_id_required")?;
    let revision = ticket
        .get("revision")
        .and_then(Value::as_i64)
        .ok_or("ticket_revision_required")?;
    let mut payload = json!({
        "ticket_id":ticket_id,
        "ticket_number":ticket.get("ticket_number"),
        "ticket_revision":revision,
        "ticket_status":ticket.get("status"),
        "event_type":event_type
    });
    if let (Some(target), Some(source)) = (payload.as_object_mut(), extra.as_object()) {
        for (key, value) in source {
            target.insert(key.clone(), value.clone());
        }
    }
    let payload_json = serde_json::to_string(&native_canonical_value(&payload))
        .map_err(|e| format!("event_payload_invalid:{e}"))?;
    if payload_json.len() > 16_384 {
        return Err("event_payload_too_large".to_string());
    }
    let event_id = native_work_stable_id(
        "event",
        &json!({
            "aggregate_kind":"ticket",
            "aggregate_id":ticket_id,
            "aggregate_revision":revision,
            "event_type":event_type,
            "idempotency_key":idempotency_key
        }),
        32,
    );
    let timestamp = now();
    let connection = server.connection_mut()?;
    connection
        .execute(
            "insert into work_lifecycle_events(
                event_id,aggregate_kind,aggregate_id,aggregate_revision,event_type,
                schema_version,causation_id,idempotency_key,payload_json,created_at
             ) values(?1,'ticket',?2,?3,?4,1,?5,?6,?7,?8)",
            params![
                &event_id,
                ticket_id,
                revision,
                event_type,
                causation_id,
                format!("event:{idempotency_key}"),
                &payload_json,
                &timestamp
            ],
        )
        .map_err(db_error)?;
    connection
        .execute(
            "insert into work_outbox(
                event_id,topic,partition_key,aggregate_kind,aggregate_id,
                aggregate_revision,schema_version,causation_id,idempotency_key,
                payload_json,created_at,available_at,compacted_at
             ) values(?1,?2,?3,'ticket',?3,?4,1,?5,?6,?7,?8,?8,null)",
            params![
                &event_id,
                topic,
                ticket_id,
                revision,
                causation_id,
                format!("outbox:{idempotency_key}"),
                &payload_json,
                &timestamp
            ],
        )
        .map_err(db_error)?;
    Ok(event_id)
}

fn native_work_ticket_list(server: &LifecycleServer, args: Value) -> Result<Value, String> {
    let limit = args
        .get("limit")
        .and_then(Value::as_i64)
        .unwrap_or(100)
        .clamp(1, 500);
    let offset = args
        .get("offset")
        .and_then(Value::as_i64)
        .unwrap_or(0)
        .clamp(0, 100_000);
    let status = args
        .get("status")
        .and_then(Value::as_str)
        .map(ToString::to_string);
    if let Some(value) = status.as_deref() {
        if !matches!(
            value,
            "actionable"
                | "effect_claimed"
                | "waiting_on_draft"
                | "waiting_on_task"
                | "blocked"
                | "resolved"
        ) {
            return Err("ticket_status_invalid".to_string());
        }
    }
    let tickets = server.query_objects(
        "select ticket_id,ticket_number,status,revision,summary,
                resolution_code,blocker_code,created_at,updated_at,terminal_at
           from tickets
          where (?1 is null or status=?1)
          order by ticket_number desc limit ?2 offset ?3",
        params![status.as_deref(), limit, offset],
    )?;
    Ok(json!({
        "schema":"narada.work_lifecycle.ticket_list.v1",
        "count":tickets.len(),
        "tickets":tickets,
        "limit":limit,
        "offset":offset
    }))
}

fn native_work_ticket_show(
    server: &LifecycleServer,
    args: Value,
) -> Result<Value, String> {
    let ticket = native_work_ticket_by_args(server, &args)?;
    let ticket_id = ticket
        .get("ticket_id")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let sources = server.query_objects(
        "select source_id,ticket_id,source_kind,source_scope,
                immutable_source_id,source_ref_json,policy_version,receipt_id,admitted_at
           from ticket_sources where ticket_id=?1
          order by admitted_at,source_id limit 50",
        params![ticket_id],
    )?;
    let sources = sources
        .into_iter()
        .map(|mut source| {
            if let Some(text) = source.get("source_ref_json").and_then(Value::as_str) {
                if let Ok(value) = serde_json::from_str::<Value>(text) {
                    if let Some(object) = source.as_object_mut() {
                        object.remove("source_ref_json");
                        object.insert("source_ref".to_string(), value);
                    }
                }
            }
            source
        })
        .collect::<Vec<_>>();
    let task_links = server.query_objects(
        "select link.ticket_id,link.task_id,task.task_number,
                link.link_kind,link.status as link_status,task.status as task_status,
                task.revision as task_revision,spec.title,link.linked_at,link.terminal_at
           from ticket_task_links link
           join task_lifecycle task on task.task_id=link.task_id
           left join task_specs spec on spec.task_id=task.task_id
          where link.ticket_id=?1 order by link.linked_at,link.task_id limit 50",
        params![ticket_id],
    )?;
    let draft_rows = server.query_objects(
        "select ticket_id,draft_id,effect_claim_id,draft_ref_json,receipt_id,
                disposition,disposition_evidence_kind,disposition_evidence_id,
                disposition_evidence_json,created_at,disposed_at
           from ticket_draft_refs where ticket_id=?1
          order by created_at,draft_id limit 50",
        params![ticket_id],
    )?;
    let draft_refs = draft_rows
        .into_iter()
        .map(|mut row| {
            for (source, target) in [
                ("draft_ref_json", "draft_ref"),
                ("disposition_evidence_json", "disposition_evidence"),
            ] {
                if let Some(text) = row.get(source).and_then(Value::as_str) {
                    if let Ok(value) = serde_json::from_str::<Value>(text) {
                        if let Some(object) = row.as_object_mut() {
                            object.remove(source);
                            object.insert(target.to_string(), value);
                        }
                    }
                }
            }
            row
        })
        .collect::<Vec<_>>();
    Ok(json!({
        "schema":"narada.work_lifecycle.ticket.v1",
        "ticket":ticket,
        "sources":sources,
        "task_links":task_links,
        "draft_refs":draft_refs
    }))
}

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

fn native_work_processing_context(
    server: &mut LifecycleServer,
    args: Value,
) -> Result<Value, String> {
    let operation_key = required_string(&args, "idempotency_key")?;
    let ticket_id = required_string(&args, "ticket_id")?;
    let event_id = required_string(&args, "triggering_event_id")?;
    let request = json!({
        "ticket_id":ticket_id,
        "triggering_event_id":event_id,
        "idempotency_key":operation_key
    });
    let request_digest = native_canonical_digest(&request);
    if let Some(existing) =
        native_work_existing_operation(server, &operation_key, &request_digest)?
    {
        return Ok(native_work_domain(&operation_key, existing));
    }
    let ticket = native_work_ticket(server, &ticket_id)?;
    let triggering = server
        .query_one(
            "select event_id,topic,aggregate_revision,event_type,schema_version,
                    causation_id,idempotency_key,payload_json,created_at
               from work_outbox where event_id=?1",
            params![&event_id],
        )?
        .or_else(|| {
            server
                .query_one(
                    "select event_id,aggregate_revision,event_type,schema_version,
                            causation_id,idempotency_key,payload_json,created_at
                       from work_lifecycle_events where event_id=?1",
                    params![&event_id],
                )
                .ok()
                .flatten()
        })
        .ok_or("triggering_event_not_found")?;
    let sources = native_work_ticket_show(server, json!({"ticket_id":ticket_id}))?;
    let result = json!({
        "schema":"narada.work_lifecycle.ticket_processing_context.v1",
        "ticket":ticket,
        "triggering_event":triggering,
        "sources":sources.get("sources").cloned().unwrap_or_else(||json!([])),
        "task_links":sources.get("task_links").cloned().unwrap_or_else(||json!([])),
        "draft_refs":sources.get("draft_refs").cloned().unwrap_or_else(||json!([])),
        "counts":{
            "sources":sources.get("sources").and_then(Value::as_array).map(|v|v.len()).unwrap_or(0),
            "task_links":sources.get("task_links").and_then(Value::as_array).map(|v|v.len()).unwrap_or(0),
            "draft_refs":sources.get("draft_refs").and_then(Value::as_array).map(|v|v.len()).unwrap_or(0)
        },
        "truncated":{"sources":false,"task_links":false,"draft_refs":false}
    });
    native_work_record_operation(
        server,
        &operation_key,
        "ticket.processing_context.load",
        &request_digest,
        Some(&ticket_id),
        ticket.get("revision").and_then(Value::as_i64),
        &result,
    )?;
    Ok(native_work_domain(&operation_key, result))
}

fn native_work_admit_proposal(
    server: &mut LifecycleServer,
    args: Value,
) -> Result<Value, String> {
    let ticket_id = required_string(&args, "ticket_id")?;
    let expected_revision = required_i64(&args, "expected_revision")?;
    if expected_revision < 1 {
        return Err("expected_revision_invalid".to_string());
    }
    let route = required_string(&args, "route")?;
    if !matches!(
        route.as_str(),
        "response_draft" | "followup_task" | "resolved" | "blocked_operator"
    ) {
        return Err("ticket_proposal_route_invalid".to_string());
    }
    let operation_key = required_string(&args, "idempotency_key")?;
    let causation_id = required_string(&args, "causation_id")?;
    let actor_id = required_string(&args, "actor_id")?;
    let summary = required_string(&args, "summary")?;
    if summary.as_bytes().len() > 2_048 {
        return Err("summary_too_large".to_string());
    }
    let request = json!({
        "ticket_id":ticket_id,
        "expected_revision":expected_revision,
        "route":route,
        "idempotency_key":operation_key,
        "causation_id":causation_id,
        "actor_id":actor_id,
        "summary":summary,
        "task":args.get("task"),
        "draft":args.get("draft"),
        "resolution_code":args.get("resolution_code"),
        "blocker_code":args.get("blocker_code")
    });
    let request_digest = native_canonical_digest(&request);
    if let Some(existing) =
        native_work_existing_operation(server, &operation_key, &request_digest)?
    {
        return Ok(native_work_domain(&operation_key, existing));
    }
    let before = native_work_ticket(server, &ticket_id)?;
    if before.get("revision").and_then(Value::as_i64) != Some(expected_revision) {
        return Err(format!(
            "ticket_revision_conflict:expected_{expected_revision}:actual_{}",
            before.get("revision").and_then(Value::as_i64).unwrap_or(0)
        ));
    }
    let mut task_id: Option<String> = None;
    let mut task_number: Option<i64> = None;
    let mut effect_claim_id: Option<String> = None;
    let mut draft_operation_key: Option<String> = None;
    let mut draft_request_digest: Option<String> = None;
    let mut draft_source_id: Option<String> = None;
    let mut draft_mailbox_id: Option<String> = None;
    let mut draft_source_message_id: Option<String> = None;
    let mut draft_reply_mode: Option<String> = None;
    let event_type: String;
    let topic = "work.ticket-lifecycle.v1";
    let after: Value;
    match route.as_str() {
        "followup_task" => {
            let task = args
                .get("task")
                .filter(|value| value.is_object())
                .ok_or("ticket_proposal_task_required")?;
            let title = required_string(task, "title")?;
            let goal = required_string(task, "goal")?;
            let required_work = required_string(task, "required_work")?;
            if title.as_bytes().len() > 1_024 {
                return Err("task_title_too_large".to_string());
            }
            if goal.as_bytes().len() > 8_192 {
                return Err("task_goal_too_large".to_string());
            }
            if required_work.as_bytes().len() > 16_384 {
                return Err("task_required_work_too_large".to_string());
            }
            let criteria = task
                .get("acceptance_criteria")
                .filter(|value| value.is_array())
                .cloned()
                .ok_or("task_acceptance_criteria_required")?;
            let tags = task.get("tags").cloned().unwrap_or_else(||json!([]));
            let existing_task: Option<String> = server
                .connection()?
                .query_row(
                    "select task_id from ticket_task_links where operation_key=?1",
                    params![&operation_key],
                    |row| row.get(0),
                )
                .optional()
                .map_err(db_error)?;
            if let Some(existing_task) = existing_task {
                task_id = Some(existing_task.clone());
                task_number = server
                    .connection()?
                    .query_row(
                        "select task_number from task_lifecycle where task_id=?1",
                        params![&existing_task],
                        |row| row.get(0),
                    )
                    .optional()
                    .map_err(db_error)?;
            } else {
                let allocated = {
                    let connection = server.connection_mut()?;
                    let number: i64 = connection
                        .query_row(
                            "select last_allocated from task_number_sequence where singleton=1",
                            [],
                            |row| row.get::<_, i64>(0),
                        )
                        .optional()
                        .map_err(db_error)?
                        .map(|last| last + 1)
                        .ok_or("task_number_sequence_missing")?;
                    connection
                        .execute(
                            "update task_number_sequence set last_allocated=?1 where singleton=1",
                            params![number],
                        )
                        .map_err(db_error)?;
                    number
                };
                let new_task_id = native_work_stable_id(
                    "task",
                    &json!({"kind":"ticket_followup","operation_key":operation_key}),
                    24,
                );
                {
                    let connection = server.connection_mut()?;
                    connection
                        .execute(
                            "insert into task_lifecycle(
                                task_id,task_number,status,governed_by,closed_at,closed_by,
                                closure_mode,relative_priority,priority_reason,reopened_at,
                                reopened_by,continuation_packet_json,updated_at,revision
                             ) values(?1,?2,'opened',null,null,null,null,0,null,null,null,null,?3,1)",
                            params![&new_task_id, allocated, &now()],
                        )
                        .map_err(db_error)?;
                    connection
                        .execute(
                            "insert into task_specs(
                                task_id,task_number,title,chapter_markdown,goal_markdown,
                                context_markdown,required_work_markdown,non_goals_markdown,
                                acceptance_criteria_json,dependencies_json,tags_json,updated_at
                             ) values(?1,?2,?3,null,?4,?5,?6,?7,?8,'[]',?9,?10)",
                            params![
                                &new_task_id,
                                allocated,
                                &title,
                                &goal,
                                task.get("context").and_then(Value::as_str),
                                &required_work,
                                task.get("non_goals").and_then(Value::as_str),
                                serde_json::to_string(&native_canonical_value(&criteria)).unwrap_or_else(|_|"[]".to_string()),
                                serde_json::to_string(&native_canonical_value(&tags)).unwrap_or_else(|_|"[]".to_string()),
                                &now()
                            ],
                        )
                        .map_err(db_error)?;
                    let contract_id = native_work_stable_id(
                        "contract_ticket_followup",
                        &json!({"operation_key":operation_key}),
                        24,
                    );
                    connection
                        .execute(
                            "insert into task_outcome_contracts(
                                contract_id,task_id,outcome_type,allowed_outcomes_json,
                                satisfying_outcomes_json,blocking_outcomes_json,
                                required_fields_json,capability_requirement,created_by,created_at
                             ) values(?1,?2,'ticket_followup_completion',
                                '[\"completed\"]','[\"completed\"]','[]',
                                '[\"summary\"]',null,?3,?4)",
                            params![&contract_id,&new_task_id,&actor_id,&now()],
                        )
                        .map_err(db_error)?;
                    connection
                        .execute(
                            "insert into ticket_task_links(
                                ticket_id,task_id,link_kind,operation_key,status,linked_at,terminal_at
                             ) values(?1,?2,'followup',?3,'active',?4,null)",
                            params![&ticket_id,&new_task_id,&operation_key,&now()],
                        )
                        .map_err(db_error)?;
                }
                task_id = Some(new_task_id);
                task_number = Some(allocated);
            }
            after = native_work_transition(
                server,
                &ticket_id,
                "waiting_on_task",
                Some(&summary),
                None,
                None,
                None,
            )?;
            event_type = "ticket.followup_task.created".to_string();
        }
        "response_draft" => {
            let draft = args
                .get("draft")
                .filter(|value| value.is_object())
                .ok_or("ticket_proposal_draft_required")?;
            let source_id = required_string(draft, "source_id")?;
            let reply_mode = required_string(draft, "reply_mode")?;
            if !matches!(reply_mode.as_str(), "reply" | "reply_all") {
                return Err("ticket_proposal_draft_reply_mode_invalid".to_string());
            }
            let body_text = draft.get("body_text").and_then(Value::as_str);
            let body_html = draft.get("body_html").and_then(Value::as_str);
            if body_text.is_some() == body_html.is_some()
                || body_text.is_some_and(|value| value.is_empty())
                || body_html.is_some_and(|value| value.is_empty())
            {
                return Err("ticket_proposal_draft_exactly_one_body_required".to_string());
            }
            if body_text.is_some_and(|value| value.as_bytes().len() > 12 * 1024)
                || body_html.is_some_and(|value| value.as_bytes().len() > 12 * 1024)
            {
                return Err("draft_body_too_large".to_string());
            }
            let source = server
                .query_one(
                    "select source_id,source_scope,immutable_source_id,source_ref_json
                       from ticket_sources
                      where ticket_id=?1 and source_id=?2 and source_kind='mailbox_message'",
                    params![&ticket_id,&source_id],
                )?
                .ok_or("ticket_proposal_draft_source_invalid")?;
            let source_scope = source
                .get("source_scope")
                .and_then(Value::as_str)
                .ok_or("ticket_draft_source_scope")?;
            let source_ref = source
                .get("source_ref_json")
                .and_then(Value::as_str)
                .and_then(|value| serde_json::from_str::<Value>(value).ok())
                .unwrap_or_else(||json!({}));
            let source_ref_scope = source_ref
                .get("scope_id")
                .and_then(Value::as_str)
                .ok_or("ticket_draft_source_ref_scope_id")?;
            if source_ref_scope != source_scope {
                return Err("ticket_draft_source_scope_mismatch".to_string());
            }
            let mailbox_id = required_string(&source_ref, "mailbox_id")?;
            let source_message_id = required_string(&source_ref, "message_id")?;
            if source
                .get("immutable_source_id")
                .and_then(Value::as_str)
                != Some(source_message_id.as_str())
            {
                return Err("ticket_draft_source_identity_mismatch".to_string());
            }
            let mut draft_request = json!({
                "source_id":source_id,
                "mailbox_id":mailbox_id,
                "source_message_id":source_message_id,
                "reply_mode":reply_mode
            });
            if let Some(value) = body_text {
                draft_request
                    .as_object_mut()
                    .unwrap()
                    .insert("body_text".to_string(), Value::String(value.to_string()));
            }
            if let Some(value) = body_html {
                draft_request
                    .as_object_mut()
                    .unwrap()
                    .insert("body_html".to_string(), Value::String(value.to_string()));
            }
            let request_digest = native_canonical_digest(&draft_request);
            let operation = native_work_stable_id(
                "draft_operation",
                &json!({"idempotency_key":operation_key}),
                32,
            );
            let claim = native_work_stable_id(
                "effect_claim",
                &json!({"operation_key":operation}),
                24,
            );
            let next_revision = expected_revision + 1;
            {
                let connection = server.connection_mut()?;
                connection
                    .execute(
                        "insert into ticket_effect_claims(
                            claim_id,ticket_id,ticket_revision,effect_kind,operation_key,
                            request_digest,status,receipt_id,receipt_json,claimed_at,completed_at
                         ) values(?1,?2,?3,'graph.unsent_draft',?4,?5,'claimed',null,null,?6,null)",
                        params![&claim,&ticket_id,next_revision,&operation,&request_digest,&now()],
                    )
                    .map_err(db_error)?;
            }
            after = native_work_transition(
                server,
                &ticket_id,
                "effect_claimed",
                Some(&summary),
                None,
                None,
                None,
            )?;
            effect_claim_id = Some(claim);
            draft_operation_key = Some(operation);
            draft_request_digest = Some(request_digest);
            draft_source_id = Some(source_id);
            draft_mailbox_id = Some(mailbox_id);
            draft_source_message_id = Some(source_message_id);
            draft_reply_mode = Some(reply_mode);
            event_type = "ticket.draft_effect.claimed".to_string();
        }
        "resolved" => {
            let unresolved_task: Option<String> = server
                .connection()?
                .query_row(
                    "select link.task_id from ticket_task_links link
                       join task_lifecycle task on task.task_id=link.task_id
                      where link.ticket_id=?1 and link.status='active'
                        and task.status not in ('closed','confirmed') limit 1",
                    params![&ticket_id],
                    |row| row.get(0),
                )
                .optional()
                .map_err(db_error)?;
            if unresolved_task.is_some() {
                return Err("ticket_resolution_blocked_by_task".to_string());
            }
            let unresolved_claim: Option<String> = server
                .connection()?
                .query_row(
                    "select claim_id from ticket_effect_claims where ticket_id=?1 and status='claimed' limit 1",
                    params![&ticket_id],
                    |row| row.get(0),
                )
                .optional()
                .map_err(db_error)?;
            if unresolved_claim.is_some() {
                return Err("ticket_resolution_blocked_by_effect_claim".to_string());
            }
            let waiting_draft: Option<String> = server
                .connection()?
                .query_row(
                    "select draft_id from ticket_draft_refs where ticket_id=?1 and disposition is null limit 1",
                    params![&ticket_id],
                    |row| row.get(0),
                )
                .optional()
                .map_err(db_error)?;
            if waiting_draft.is_some() {
                return Err("ticket_resolution_blocked_by_draft".to_string());
            }
            let resolution = string_arg(&args, "resolution_code")
                .unwrap_or_else(|| "resolved".to_string());
            after = native_work_transition(
                server,
                &ticket_id,
                "resolved",
                Some(&summary),
                Some(&resolution),
                None,
                Some(&now()),
            )?;
            event_type = "ticket.resolved".to_string();
        }
        "blocked_operator" => {
            let blocker = string_arg(&args, "blocker_code")
                .unwrap_or_else(|| "operator_required".to_string());
            after = native_work_transition(
                server,
                &ticket_id,
                "blocked",
                Some(&summary),
                None,
                Some(&blocker),
                None,
            )?;
            event_type = "ticket.blocked.operator".to_string();
        }
        _ => unreachable!(),
    }
    let mut extra = json!({
        "route":route,
        "actor_id":actor_id
    });
    if let Some(object) = extra.as_object_mut() {
        if let Some(value) = task_id.as_ref() {
            object.insert("task_id".to_string(), Value::String(value.clone()));
        }
        if let Some(value) = task_number {
            object.insert("task_number".to_string(), json!(value));
        }
        if let Some(value) = effect_claim_id.as_ref() {
            object.insert("effect_claim_id".to_string(), Value::String(value.clone()));
        }
        if let Some(value) = draft_operation_key.as_ref() {
            object.insert("draft_operation_key".to_string(), Value::String(value.clone()));
        }
        if let Some(value) = draft_request_digest.as_ref() {
            object.insert("draft_request_digest".to_string(), Value::String(value.clone()));
        }
        if let Some(value) = draft_source_id.as_ref() {
            object.insert("draft_source_id".to_string(), Value::String(value.clone()));
        }
        if let Some(value) = draft_mailbox_id.as_ref() {
            object.insert("mailbox_id".to_string(), Value::String(value.clone()));
        }
        if let Some(value) = draft_source_message_id.as_ref() {
            object.insert("source_message_id".to_string(), Value::String(value.clone()));
        }
        if let Some(value) = draft_reply_mode.as_ref() {
            object.insert("reply_mode".to_string(), Value::String(value.clone()));
        }
    }
    let event_id = native_work_event(
        server,
        &after,
        &event_type,
        &causation_id,
        &operation_key,
        topic,
        &extra,
    )?;
    let mut result = json!({
        "schema":"narada.work_lifecycle.ticket_proposal.v1",
        "status":"admitted",
        "route":route,
        "ticket_id":ticket_id,
        "ticket_revision":after.get("revision"),
        "event_id":event_id
    });
    if let Some(object) = result.as_object_mut() {
        if let Some(value) = task_id {
            object.insert("task_id".to_string(), Value::String(value));
        }
        if let Some(value) = task_number {
            object.insert("task_number".to_string(), json!(value));
        }
        if let Some(value) = effect_claim_id {
            object.insert("effect_claim_id".to_string(), Value::String(value));
        }
        if let Some(value) = draft_operation_key {
            object.insert("draft_operation_key".to_string(), Value::String(value));
        }
        if let Some(value) = draft_request_digest {
            object.insert("draft_request_digest".to_string(), Value::String(value));
        }
        if let Some(value) = draft_source_id {
            object.insert("draft_source_id".to_string(), Value::String(value));
        }
        if let Some(value) = draft_mailbox_id {
            object.insert("mailbox_id".to_string(), Value::String(value));
        }
        if let Some(value) = draft_source_message_id {
            object.insert("source_message_id".to_string(), Value::String(value));
        }
        if let Some(value) = draft_reply_mode {
            object.insert("reply_mode".to_string(), Value::String(value));
        }
    }
    native_work_record_operation(
        server,
        &operation_key,
        &format!("ticket.proposal.{route}"),
        &request_digest,
        Some(&ticket_id),
        after.get("revision").and_then(Value::as_i64),
        &result,
    )?;
    Ok(native_work_domain(&operation_key, result))
}

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

fn native_work_outbox_list(
    server: &LifecycleServer,
    args: Value,
) -> Result<Value, String> {
    let consumer = required_string(&args, "consumer_id")?;
    let limit = args
        .get("limit")
        .and_then(Value::as_i64)
        .unwrap_or(100)
        .clamp(1, 500);
    let topics = args
        .get("topics")
        .and_then(Value::as_array)
        .map(|values| {
            values
                .iter()
                .filter_map(Value::as_str)
                .filter(|value| !value.trim().is_empty())
                .map(ToString::to_string)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let mut events = if topics.is_empty() {
        server.query_objects(
            "select outbox.event_id,outbox.topic,outbox.partition_key,
                    outbox.aggregate_kind,outbox.aggregate_id,
                    outbox.aggregate_revision,outbox.schema_version,
                    outbox.causation_id,outbox.idempotency_key,
                    outbox.payload_json,outbox.created_at,outbox.available_at,
                    outbox.compacted_at
               from work_outbox outbox
              where outbox.available_at<=?1
                and not exists(
                    select 1 from work_outbox_receipts receipt
                     where receipt.event_id=outbox.event_id
                       and receipt.consumer_id=?2)
              order by outbox.created_at,outbox.event_id limit ?3",
            params![now(),&consumer,limit],
        )?
    } else {
        let placeholders = std::iter::repeat("?")
            .take(topics.len())
            .collect::<Vec<_>>()
            .join(",");
        let sql = format!(
            "select outbox.event_id,outbox.topic,outbox.partition_key,
                    outbox.aggregate_kind,outbox.aggregate_id,
                    outbox.aggregate_revision,outbox.schema_version,
                    outbox.causation_id,outbox.idempotency_key,
                    outbox.payload_json,outbox.created_at,outbox.available_at,
                    outbox.compacted_at
               from work_outbox outbox
              where outbox.topic in ({placeholders})
                and outbox.available_at<=?{next}
                and not exists(
                    select 1 from work_outbox_receipts receipt
                     where receipt.event_id=outbox.event_id
                       and receipt.consumer_id=?{next2})
              order by outbox.created_at,outbox.event_id limit ?{next3}",
            next = topics.len() + 1,
            next2 = topics.len() + 2,
            next3 = topics.len() + 3
        );
        let mut values = topics
            .iter()
            .map(|value| value.clone())
            .collect::<Vec<_>>();
        values.push(now());
        values.push(consumer.clone());
        values.push(limit.to_string());
        let params = values
            .iter()
            .map(|value| value as &dyn rusqlite::ToSql)
            .collect::<Vec<_>>();
        let connection = server.connection()?;
        let mut statement = connection.prepare(&sql).map_err(db_error)?;
        let rows = statement
            .query_map(rusqlite::params_from_iter(params), |row| row_to_object(row))
            .map_err(db_error)?;
        rows.collect::<Result<Vec<_>,_>>().map_err(db_error)?
    };
    for event in &mut events {
        if let Some(text) = event.get("payload_json").and_then(Value::as_str) {
            if let Ok(payload) = serde_json::from_str::<Value>(text) {
                if let Some(object) = event.as_object_mut() {
                    object.remove("payload_json");
                    object.insert("payload".to_string(), payload);
                }
            }
        }
    }
    Ok(json!({
        "schema":"narada.work_lifecycle.outbox.v1",
        "count":events.len(),
        "events":events
    }))
}

fn native_work_outbox_register(
    server: &mut LifecycleServer,
    args: Value,
) -> Result<Value, String> {
    let topic = required_string(&args, "topic")?;
    let consumer = required_string(&args, "consumer_id")?;
    server
        .connection_mut()?
        .execute(
            "insert into work_outbox_consumer_requirements(topic,consumer_id,registered_at)
             values(?1,?2,?3) on conflict(topic,consumer_id) do nothing",
            params![topic,consumer,now()],
        )
        .map_err(db_error)?;
    Ok(json!({"status":"registered"}))
}

fn native_work_outbox_ack(
    server: &mut LifecycleServer,
    args: Value,
) -> Result<Value, String> {
    let event_id = required_string(&args, "event_id")?;
    let consumer = required_string(&args, "consumer_id")?;
    let receipt = args
        .get("receipt")
        .filter(|value| value.is_object())
        .ok_or("outbox_receipt_required")?;
    let receipt_json = native_work_ref_json(receipt, "outbox_receipt")?;
    let exists: Option<String> = server
        .connection()?
        .query_row(
            "select event_id from work_outbox where event_id=?1",
            params![&event_id],
            |row| row.get(0),
        )
        .optional()
        .map_err(db_error)?;
    if exists.is_none() {
        return Err("work_outbox_event_not_found".to_string());
    }
    server
        .connection_mut()?
        .execute(
            "insert into work_outbox_receipts(event_id,consumer_id,processed_at,receipt_json)
             values(?1,?2,?3,?4)
             on conflict(event_id,consumer_id) do update set
               processed_at=excluded.processed_at,receipt_json=excluded.receipt_json",
            params![event_id,consumer,now(),receipt_json],
        )
        .map_err(db_error)?;
    Ok(json!({"status":"acknowledged"}))
}

fn native_work_outbox_compact(
    server: &mut LifecycleServer,
    args: Value,
) -> Result<Value, String> {
    let before = required_string(&args, "before")?;
    OffsetDateTime::parse(&before, &Rfc3339)
        .map_err(|_| "compact_before_invalid".to_string())?;
    let compacted = server
        .connection_mut()?
        .execute(
            "update work_outbox as outbox
                set payload_json='{}',compacted_at=?1
              where outbox.compacted_at is null
                and outbox.created_at<?2
                and exists(
                    select 1 from work_outbox_consumer_requirements requirement
                     where requirement.topic=outbox.topic)
                and not exists(
                    select 1 from work_outbox_consumer_requirements requirement
                     where requirement.topic=outbox.topic
                       and not exists(
                           select 1 from work_outbox_receipts receipt
                            where receipt.event_id=outbox.event_id
                              and receipt.consumer_id=requirement.consumer_id))
            ",
            params![now(),before],
        )
        .map_err(db_error)?;
    Ok(json!({"compacted":compacted}))
}

fn native_work_admit_source_tx(
    server: &mut LifecycleServer,
    args: Value,
) -> Result<Value, String> {
    native_work_transaction(server, |server| native_work_admit_source(server, args))
}

fn native_work_processing_context_tx(
    server: &mut LifecycleServer,
    args: Value,
) -> Result<Value, String> {
    native_work_transaction(server, |server| native_work_processing_context(server, args))
}

fn native_work_admit_proposal_tx(
    server: &mut LifecycleServer,
    args: Value,
) -> Result<Value, String> {
    native_work_transaction(server, |server| native_work_admit_proposal(server, args))
}

fn native_work_record_draft_receipt_tx(
    server: &mut LifecycleServer,
    args: Value,
) -> Result<Value, String> {
    native_work_transaction(server, |server| native_work_record_draft_receipt(server, args))
}

fn native_work_reconcile_draft_tx(
    server: &mut LifecycleServer,
    args: Value,
) -> Result<Value, String> {
    native_work_transaction(server, |server| native_work_reconcile_draft(server, args))
}

fn native_work_outbox_register_tx(
    server: &mut LifecycleServer,
    args: Value,
) -> Result<Value, String> {
    native_work_transaction(server, |server| native_work_outbox_register(server, args))
}

fn native_work_outbox_ack_tx(
    server: &mut LifecycleServer,
    args: Value,
) -> Result<Value, String> {
    native_work_transaction(server, |server| native_work_outbox_ack(server, args))
}

fn native_work_outbox_compact_tx(
    server: &mut LifecycleServer,
    args: Value,
) -> Result<Value, String> {
    native_work_transaction(server, |server| native_work_outbox_compact(server, args))
}
fn native_work_storage_inspect(server: &LifecycleServer) -> Result<Value, String> {
    let checks = [
        ("tickets", "ticket_id", "summary", 2_048_i64),
        ("ticket_sources", "source_id", "source_ref_json", 16_384_i64),
        ("work_lifecycle_events", "event_id", "payload_json", 16_384_i64),
        ("work_outbox", "event_id", "payload_json", 16_384_i64),
        ("work_operations", "operation_key", "result_json", 32_768_i64),
    ];
    let mut violations = Vec::new();
    for (table, id_column, value_column, limit) in checks {
        let sql = format!(
            "select {id_column} as row_id,length(cast({value_column} as blob)) as bytes from {table}
              where length(cast({value_column} as blob))>?1 limit 200"
        );
        let rows = server.query_objects(&sql, params![limit])?;
        for row in rows {
            violations.push(json!({
                "table":table,
                "row_id":row.get("row_id"),
                "bytes":row.get("bytes"),
                "limit":limit
            }));
        }
    }
    Ok(json!({
        "status":if violations.is_empty(){"ok"}else{"violation"},
        "violations":violations
    }))
}
