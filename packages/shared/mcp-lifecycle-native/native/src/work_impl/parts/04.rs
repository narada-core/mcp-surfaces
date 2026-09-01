
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
            "select outbox.event_id,outbox.topic,outbox.aggregate_revision,
                    event.event_type,outbox.schema_version,outbox.causation_id,
                    outbox.idempotency_key,outbox.payload_json,outbox.created_at
               from work_outbox outbox
               join work_lifecycle_events event on event.event_id=outbox.event_id
              where outbox.event_id=?1",
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
