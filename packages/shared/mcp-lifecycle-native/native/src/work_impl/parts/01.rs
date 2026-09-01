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
