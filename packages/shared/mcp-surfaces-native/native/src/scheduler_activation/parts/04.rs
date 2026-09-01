fn activation_list(db: &Connection, args: &Map<String, Value>) -> Result<Value, Value> {
    let limit = args
        .get("limit")
        .and_then(Value::as_i64)
        .unwrap_or(100)
        .clamp(1, 500);
    let offset = args
        .get("offset")
        .and_then(Value::as_i64)
        .unwrap_or(0)
        .clamp(0, 10_000);
    let activations = list_activations(
        db,
        args.get("status").and_then(Value::as_str),
        args.get("binding_id").and_then(Value::as_str),
        args.get("source_event_id").and_then(Value::as_str),
        args.get("sop_run_id").and_then(Value::as_str),
        limit + 1,
        offset,
    )?;
    let has_more = activations.len() as i64 > limit;
    let activations = activations
        .into_iter()
        .take(limit as usize)
        .collect::<Vec<_>>();
    let returned = activations.len();
    Ok(
        json!({"schema":"narada.scheduler.activation_list.v1","status":"ok","count":returned,"returned":returned,"activations":activations,"offset":offset,"limit":limit,"has_more":has_more,"next_offset":if has_more{json!(offset + returned as i64)}else{Value::Null},"bounded":true}),
    )
}

fn list_activations(
    db: &Connection,
    status: Option<&str>,
    binding_id: Option<&str>,
    source_event_id: Option<&str>,
    sop_run_id: Option<&str>,
    limit: i64,
    offset: i64,
) -> Result<Vec<Value>, Value> {
    let mut clauses = Vec::new();
    let mut values = Vec::<SqlValue>::new();
    for (column, value) in [
        ("status", status),
        ("binding_id", binding_id),
        ("source_event_id", source_event_id),
        ("sop_run_id", sop_run_id),
    ] {
        if let Some(value) = value {
            clauses.push(format!("{column}=?"));
            values.push(SqlValue::Text(value.to_string()));
        }
    }
    values.push(SqlValue::Integer(limit));
    values.push(SqlValue::Integer(offset));
    let sql = format!(
        "select * from scheduler_activations {} order by due_at,activation_id limit ? offset ?",
        if clauses.is_empty() {
            String::new()
        } else {
            format!("where {}", clauses.join(" and "))
        }
    );
    let mut statement = db
        .prepare(&sql)
        .map_err(|cause| db_error("scheduler_activation_query_failed", cause))?;
    let rows = statement
        .query_map(params_from_iter(values.iter()), activation_from_row)
        .map_err(|cause| db_error("scheduler_activation_query_failed", cause))?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(|cause| db_error("scheduler_activation_query_failed", cause))?;
    Ok(rows)
}

fn query_activation(db: &Connection, id: &str) -> Result<Option<Value>, Value> {
    db.query_row(
        "select * from scheduler_activations where activation_id=?1",
        params![id],
        activation_from_row,
    )
    .optional()
    .map_err(|cause| db_error("scheduler_activation_query_failed", cause))
}
fn require_activation(db: &Connection, id: &str) -> Result<Value, Value> {
    query_activation(db, id)?.ok_or_else(|| {
        error(
            "scheduler_activation_not_found",
            "scheduler_activation_not_found",
        )
    })
}

fn activation_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Value> {
    Ok(json!({
        "activation_id":row.get::<_,String>("activation_id")?,"binding_id":row.get::<_,String>("binding_id")?,"source_event_id":row.get::<_,String>("source_event_id")?,"occurrence_key":row.get::<_,String>("occurrence_key")?,
        "target_sop_id":row.get::<_,String>("target_sop_id")?,"target_template_version":row.get::<_,String>("target_template_version")?,"partition_key":row.get::<_,String>("partition_key")?,"due_at":row.get::<_,String>("due_at")?,
        "status":row.get::<_,String>("status")?,"attempt_count":row.get::<_,i64>("attempt_count")?,"lease_owner":row.get::<_,Option<String>>("lease_owner")?,"lease_token":row.get::<_,Option<String>>("lease_token")?,"lease_expires_at":row.get::<_,Option<String>>("lease_expires_at")?,
        "sop_run_id":row.get::<_,Option<String>>("sop_run_id")?,"terminal_outcome":row.get::<_,Option<String>>("terminal_outcome")?,"last_error":row.get::<_,Option<String>>("last_error")?,"created_at":row.get::<_,String>("created_at")?,"updated_at":row.get::<_,String>("updated_at")?
    }))
}

fn activation_claim(db: &Connection, args: &Map<String, Value>) -> Result<Value, Value> {
    let consumer = required(args, "consumer_id")?;
    let lease_ms = args
        .get("lease_ms")
        .and_then(Value::as_i64)
        .unwrap_or(30_000);
    if !(1_000..=300_000).contains(&lease_ms) {
        return Err(error(
            "scheduler_activation_lease_ms_invalid",
            "scheduler_activation_lease_ms_invalid",
        ));
    }
    let now_time = OffsetDateTime::now_utc();
    let now = format_iso(now_time);
    let expires = format_iso(now_time + Duration::milliseconds(lease_ms));
    transaction(db, || {
        db.execute("update scheduler_activations set status='terminal',lease_owner=null,lease_token=null,lease_expires_at=null,terminal_outcome='cancelled_binding_paused',last_error='binding_paused_after_lease_expiry',updated_at=?1 where status='leased' and lease_expires_at<=?1 and binding_id in (select binding_id from scheduler_bindings where status='paused')",params![now]).map_err(|cause|db_error("scheduler_activation_recovery_failed",cause))?;
        db.execute("update scheduler_activations set status='pending',lease_owner=null,lease_token=null,lease_expires_at=null,attempt_count=attempt_count+1,last_error='lease_expired',updated_at=?1 where status='leased' and lease_expires_at<=?1 and binding_id in (select binding_id from scheduler_bindings where status in ('active','retired'))",params![now]).map_err(|cause|db_error("scheduler_activation_recovery_failed",cause))?;
        let id:Option<String>=db.query_row("select activation.activation_id from scheduler_activations activation join scheduler_bindings binding on binding.binding_id=activation.binding_id where activation.status='pending' and activation.due_at<=?1 and binding.status in ('active','retired') and not exists (select 1 from scheduler_activations active where active.binding_id=activation.binding_id and active.partition_key=activation.partition_key and active.activation_id<>activation.activation_id and active.status in ('leased','admitted')) order by activation.due_at,activation.activation_id limit 1",params![now],|row|row.get(0)).optional().map_err(|cause|db_error("scheduler_activation_claim_failed",cause))?;
        let activation = if let Some(id) = id {
            let token = Uuid::new_v4().to_string();
            db.execute("update scheduler_activations set status='leased',lease_owner=?1,lease_token=?2,lease_expires_at=?3,updated_at=?4 where activation_id=?5 and status='pending'",params![consumer,token,expires,now,id]).map_err(|cause|db_error("scheduler_activation_claim_failed",cause))?;
            query_activation(db, &id)?
        } else {
            None
        };
        Ok(json!({"schema":"narada.scheduler.activation_claim.v1","activation":activation}))
    })
}

fn require_leased(db: &Connection, id: &str, consumer: &str, token: &str) -> Result<Value, Value> {
    let activation = require_activation(db, id)?;
    let status = activation
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or("");
    if status != "leased" {
        return Err(error(
            "scheduler_activation_not_leased",
            &format!("scheduler_activation_not_leased:{status}"),
        ));
    }
    if activation.get("lease_owner").and_then(Value::as_str) != Some(consumer) {
        return Err(error(
            "scheduler_activation_lease_owner_mismatch",
            "scheduler_activation_lease_owner_mismatch",
        ));
    }
    if activation.get("lease_token").and_then(Value::as_str) != Some(token) {
        return Err(error(
            "scheduler_activation_lease_token_mismatch",
            "scheduler_activation_lease_token_mismatch",
        ));
    }
    let expires = activation
        .get("lease_expires_at")
        .and_then(Value::as_str)
        .unwrap_or("");
    if expires <= now_iso().as_str() {
        return Err(error(
            "scheduler_activation_lease_expired",
            "scheduler_activation_lease_expired",
        ));
    }
    Ok(activation)
}

fn record_receipt(
    db: &Connection,
    activation_id: &str,
    kind: &str,
    receipt_id: &str,
    receipt: &Value,
    now: &str,
) -> Result<(), Value> {
    let encoded = bounded_json(receipt, "scheduler_activation_receipt", MAX_EVENT_BYTES)?;
    db.execute("insert into scheduler_activation_receipts(activation_id,receipt_kind,receipt_id,receipt_json,recorded_at) values (?1,?2,?3,?4,?5)",params![activation_id,kind,receipt_id,encoded,now])
        .map_err(|cause|db_error("scheduler_activation_receipt_failed",cause))?;
    Ok(())
}

fn activation_admit_sop(db: &Connection, args: &Map<String, Value>) -> Result<Value, Value> {
    let id = required(args, "activation_id")?;
    let consumer = required(args, "consumer_id")?;
    let token = required(args, "lease_token")?;
    let sop_run_id = required(args, "sop_run_id")?;
    let receipt_id = required(args, "receipt_id")?;
    let receipt = args.get("receipt").cloned().unwrap_or_else(|| json!({}));
    let now = now_iso();
    transaction(db, || {
        require_leased(db, &id, &consumer, &token)?;
        db.execute("update scheduler_activations set status='admitted',sop_run_id=?1,lease_owner=null,lease_token=null,lease_expires_at=null,updated_at=?2 where activation_id=?3",params![sop_run_id,now,id]).map_err(|cause|db_error("scheduler_activation_admit_failed",cause))?;
        record_receipt(db, &id, "sop_admission", &receipt_id, &receipt, &now)?;
        Ok(
            json!({"schema":"narada.scheduler.activation.v1","activation":require_activation(db,&id)?}),
        )
    })
}

fn activation_fail(db: &Connection, args: &Map<String, Value>) -> Result<Value, Value> {
    let id = required(args, "activation_id")?;
    let consumer = required(args, "consumer_id")?;
    let token = required(args, "lease_token")?;
    let retryable = args.get("retryable").and_then(Value::as_bool) == Some(true);
    let failure = required(args, "error")?;
    let now_time = OffsetDateTime::now_utc();
    let now = format_iso(now_time);
    transaction(db, || {
        let activation = require_leased(db, &id, &consumer, &token)?;
        let binding = require_binding(
            db,
            activation
                .get("binding_id")
                .and_then(Value::as_str)
                .unwrap_or(""),
        )?;
        let attempt = activation
            .get("attempt_count")
            .and_then(Value::as_i64)
            .unwrap_or(0)
            + 1;
        let max = binding
            .get("max_attempts")
            .and_then(Value::as_i64)
            .unwrap_or(5);
        let retry = retryable && attempt < max;
        let base = binding
            .get("retry_base_ms")
            .and_then(Value::as_i64)
            .unwrap_or(1_000);
        let cap = binding
            .get("retry_max_ms")
            .and_then(Value::as_i64)
            .unwrap_or(300_000);
        let exponent = (attempt - 1).clamp(0, 30);
        let delay = base.saturating_mul(1_i64 << exponent).min(cap);
        let due = format_iso(now_time + Duration::milliseconds(delay));
        let bounded = failure.chars().take(MAX_ERROR_BYTES).collect::<String>();
        db.execute("update scheduler_activations set status=?1,attempt_count=?2,lease_owner=null,lease_token=null,lease_expires_at=null,due_at=?3,last_error=?4,updated_at=?5 where activation_id=?6",params![if retry{"pending"}else{"blocked"},attempt,due,bounded,now,id]).map_err(|cause|db_error("scheduler_activation_fail_failed",cause))?;
        Ok(
            json!({"schema":"narada.scheduler.activation.v1","activation":require_activation(db,&id)?}),
        )
    })
}

