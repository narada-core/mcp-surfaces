
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
