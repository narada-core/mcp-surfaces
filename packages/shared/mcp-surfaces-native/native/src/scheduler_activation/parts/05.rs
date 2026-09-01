fn activation_resolve(db: &Connection, args: &Map<String, Value>) -> Result<Value, Value> {
    let activation_id = args
        .get("activation_id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|v| !v.is_empty());
    let sop_run_id = args
        .get("sop_run_id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|v| !v.is_empty());
    if activation_id.is_none() && sop_run_id.is_none() {
        return Err(error(
            "scheduler_activation_not_found",
            "scheduler_activation_not_found",
        ));
    }
    let outcome = required(args, "outcome")?;
    let receipt_id = required(args, "receipt_id")?;
    let receipt = args.get("receipt").cloned().unwrap_or_else(|| json!({}));
    let now = now_iso();
    transaction(db, || {
        let activation = if let Some(id) = activation_id {
            query_activation(db, id)?
        } else {
            db.query_row(
                "select * from scheduler_activations where sop_run_id=?1",
                params![sop_run_id],
                activation_from_row,
            )
            .optional()
            .map_err(|cause| db_error("scheduler_activation_query_failed", cause))?
        }
        .ok_or_else(|| {
            error(
                "scheduler_activation_not_found",
                "scheduler_activation_not_found",
            )
        })?;
        let id = activation
            .get("activation_id")
            .and_then(Value::as_str)
            .unwrap_or("");
        let status = activation
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or("");
        if status == "terminal" {
            let existing:Option<String>=db.query_row("select receipt_id from scheduler_activation_receipts where activation_id=?1 and receipt_kind='terminal'",params![id],|row|row.get(0)).optional().map_err(|cause|db_error("scheduler_activation_receipt_query_failed",cause))?;
            if existing.as_deref() == Some(receipt_id.as_str()) {
                return Ok(
                    json!({"schema":"narada.scheduler.activation.v1","activation":activation}),
                );
            }
            return Err(error(
                "scheduler_activation_terminal_conflict",
                "scheduler_activation_terminal_conflict",
            ));
        }
        if status != "admitted" {
            return Err(error(
                "scheduler_activation_not_admitted",
                &format!("scheduler_activation_not_admitted:{status}"),
            ));
        }
        db.execute("update scheduler_activations set status='terminal',terminal_outcome=?1,updated_at=?2 where activation_id=?3",params![outcome,now,id]).map_err(|cause|db_error("scheduler_activation_resolve_failed",cause))?;
        record_receipt(db, id, "terminal", &receipt_id, &receipt, &now)?;
        Ok(
            json!({"schema":"narada.scheduler.activation.v1","activation":require_activation(db,id)?}),
        )
    })
}

fn activation_unblock(db: &Connection, args: &Map<String, Value>) -> Result<Value, Value> {
    let id = required(args, "activation_id")?;
    let now = now_iso();
    let due = if let Some(value) = args.get("due_at").and_then(Value::as_str) {
        format_iso(parse_iso(value)?)
    } else {
        now.clone()
    };
    transaction(db, || {
        let activation = require_activation(db, &id)?;
        let status = activation
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or("");
        if status != "blocked" {
            return Err(error(
                "scheduler_activation_not_blocked",
                &format!("scheduler_activation_not_blocked:{status}"),
            ));
        }
        db.execute("update scheduler_activations set status='pending',due_at=?1,last_error=null,updated_at=?2 where activation_id=?3",params![due,now,id]).map_err(|cause|db_error("scheduler_activation_unblock_failed",cause))?;
        Ok(
            json!({"schema":"narada.scheduler.activation.v1","activation":require_activation(db,&id)?}),
        )
    })
}

fn transaction<F>(db: &Connection, action: F) -> Result<Value, Value>
where
    F: FnOnce() -> Result<Value, Value>,
{
    db.execute_batch("begin immediate")
        .map_err(|cause| db_error("scheduler_activation_transaction_failed", cause))?;
    match action() {
        Ok(value) => {
            db.execute_batch("commit")
                .map_err(|cause| db_error("scheduler_activation_transaction_failed", cause))?;
            Ok(value)
        }
        Err(problem) => {
            let _ = db.execute_batch("rollback");
            Err(problem)
        }
    }
}

fn required(args: &Map<String, Value>, field: &str) -> Result<String, Value> {
    args.get(field)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
        .ok_or_else(|| error(&format!("{field}_required"), &format!("{field}_required")))
}
fn required_string(args: &Map<String, Value>, field: &str) -> Result<String, Value> {
    required(args, field)
}
fn required_integer(args: &Map<String, Value>, field: &str) -> Result<i64, Value> {
    args.get(field)
        .and_then(Value::as_i64)
        .ok_or_else(|| error(&format!("{field}_required"), &format!("{field}_required")))
}
fn nonnegative(args: &Map<String, Value>, field: &str, fallback: i64) -> Result<i64, Value> {
    let value = args.get(field).and_then(Value::as_i64).unwrap_or(fallback);
    if value < 0 {
        Err(error(
            &format!("{field}_invalid"),
            &format!("{field}_invalid"),
        ))
    } else {
        Ok(value)
    }
}
fn text(args: &Map<String, Value>, field: &str) -> String {
    args.get(field)
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string()
}
fn optional_text(args: &Map<String, Value>, field: &str) -> Option<String> {
    args.get(field)
        .and_then(Value::as_str)
        .map(ToString::to_string)
}
fn integer(args: &Map<String, Value>, field: &str) -> i64 {
    args.get(field).and_then(Value::as_i64).unwrap_or(0)
}

fn parse_iso(value: &str) -> Result<OffsetDateTime, Value> {
    OffsetDateTime::parse(value, &Rfc3339)
        .map_err(|_| error("scheduler_datetime_invalid", "scheduler_datetime_invalid"))
}
fn format_iso(value: OffsetDateTime) -> String {
    value
        .to_offset(time::UtcOffset::UTC)
        .format(format_description!(
            "[year]-[month]-[day]T[hour]:[minute]:[second].[subsecond digits:3]Z"
        ))
        .expect("timestamp")
}
fn now_iso() -> String {
    format_iso(OffsetDateTime::now_utc())
}

fn canonicalize(value: &Value) -> Value {
    match value {
        Value::Array(values) => Value::Array(values.iter().map(canonicalize).collect()),
        Value::Object(object) => {
            let mut keys = object.keys().collect::<Vec<_>>();
            keys.sort();
            let mut normalized = Map::new();
            for key in keys {
                normalized.insert(key.clone(), canonicalize(&object[key]));
            }
            Value::Object(normalized)
        }
        other => other.clone(),
    }
}
fn canonical_json(value: &Value) -> String {
    serde_json::to_string(&canonicalize(value)).unwrap_or_else(|_| "null".to_string())
}
fn bounded_json(value: &Value, field: &str, max: usize) -> Result<String, Value> {
    let encoded = canonical_json(value);
    if encoded.len() > max {
        Err(error(
            &format!("{field}_too_large"),
            &format!("{field}_too_large"),
        ))
    } else {
        Ok(encoded)
    }
}
fn digest(value: &Value) -> String {
    let bytes = Sha256::digest(canonical_json(value).as_bytes());
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}
fn stable_id(prefix: &str, value: &Value) -> String {
    format!("{prefix}_{}", &digest(value)[..32])
}
fn db_error(code: &str, cause: rusqlite::Error) -> Value {
    error(code, &format!("{code}:{cause}"))
}
fn error(code: &str, message: &str) -> Value {
    json!({"schema":"narada.scheduler_mcp.error.v1","code":code,"message":message})
}

