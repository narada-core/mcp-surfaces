fn ticket_disposition_scan(
    policy: &Policy,
    args: &Map<String, Value>,
    root: &Path,
) -> Result<Value, Value> {
    let limit = args
        .get("limit")
        .and_then(Value::as_u64)
        .unwrap_or(5)
        .clamp(1, 5);
    let connection = ticket_store(root)?;
    let mut statement = connection
        .prepare(
            "select operation_key from graph_ticket_draft_operations operation where operation.status='completed' and not exists (select 1 from graph_ticket_draft_disposition_observations observation where observation.operation_key=operation.operation_key) order by operation.completed_at asc, operation.operation_key asc limit ?1",
        )
        .map_err(|error| unavailable("graph_ticket_draft_scan_query_failed", &error.to_string()))?;
    let keys = statement
        .query_map(params![limit], |row| row.get::<_, String>(0))
        .map_err(|error| unavailable("graph_ticket_draft_scan_query_failed", &error.to_string()))?
        .filter_map(Result::ok)
        .collect::<Vec<_>>();
    drop(statement);
    let mut errors = Vec::new();
    let mut observations_recorded = 0u64;
    let mut still_pending = 0u64;
    for operation_key in &keys {
        let result = (|| {
            let operation = find_ticket_operation(&connection, operation_key)?
                .ok_or_else(|| unavailable("graph_ticket_draft_operation_not_found", operation_key))?;
            let messages = find_ticket_remote_messages(policy, &operation.mailbox_id, operation_key)?;
            if messages.len() > 1 {
                return Err(unavailable("graph_ticket_draft_disposition_remote_identity_ambiguous", operation_key));
            }
            let Some(observed) = messages.into_iter().next() else {
                return Ok(false);
            };
            if observed.get("isDraft").and_then(Value::as_bool) != Some(false) {
                return Ok(false);
            }
            let observed_id = observed
                .get("id")
                .and_then(Value::as_str)
                .ok_or_else(|| unavailable("graph_ticket_draft_disposition_message_id_missing", operation_key))?;
            let observation_id = stable_disposition_observation_id(operation_key, "sent", observed_id);
            let mut receipt = json!({
                "schema":"narada.graph_mail.ticket_draft_disposition_receipt.v1",
                "observation_id":observation_id,
                "evidence_kind":"synchronized_graph_observation",
                "evidence_id":observation_id,
                "disposition":"sent",
                "ticket_id":operation.ticket_id,
                "effect_claim_id":operation.effect_claim_id,
                "draft_operation_key":operation_key,
                "mailbox_id":operation.mailbox_id,
                "draft_id":operation.draft_id,
                "observed_message_id":observed_id,
                "is_draft":false,
                "observed_at":now_rfc3339()
            });
            if let Some(value) = observed.get("@odata.etag").and_then(Value::as_str) {
                receipt.as_object_mut().unwrap().insert("etag".to_string(), json!(value));
            }
            if let Some(value) = observed.get("changeKey").and_then(Value::as_str) {
                receipt.as_object_mut().unwrap().insert("change_key".to_string(), json!(value));
            }
            if let Some(value) = observed.get("lastModifiedDateTime").and_then(Value::as_str) {
                receipt.as_object_mut().unwrap().insert("last_modified_at".to_string(), json!(value));
            }
            let digest = sha256_canonical(&receipt);
            receipt.as_object_mut().unwrap().insert("receipt_sha256".to_string(), json!(digest));
            let recorded = insert_disposition_observation(&connection, &operation, &receipt)?;
            connection
                .execute(
                    "insert into graph_ticket_draft_disposition_scan_state(operation_key, last_scanned_at, scan_count) values (?1, ?2, 1) on conflict(operation_key) do update set last_scanned_at=excluded.last_scanned_at, scan_count=graph_ticket_draft_disposition_scan_state.scan_count+1",
                    params![operation_key, now_rfc3339()],
                )
                .map_err(|error| unavailable("graph_ticket_draft_scan_state_failed", &error.to_string()))?;
            Ok(recorded)
        })();
        match result {
            Ok(true) => observations_recorded += 1,
            Ok(false) => {
                still_pending += 1;
                let _ = connection.execute(
                    "insert into graph_ticket_draft_disposition_scan_state(operation_key, last_scanned_at, scan_count) values (?1, ?2, 1) on conflict(operation_key) do update set last_scanned_at=excluded.last_scanned_at, scan_count=graph_ticket_draft_disposition_scan_state.scan_count+1",
                    params![operation_key, now_rfc3339()],
                );
            }
            Err(error) => {
                errors.push(json!({"operation_key":operation_key,"error":error}));
                let _ = connection.execute(
                    "insert into graph_ticket_draft_disposition_scan_state(operation_key, last_scanned_at, scan_count) values (?1, ?2, 1) on conflict(operation_key) do update set last_scanned_at=excluded.last_scanned_at, scan_count=graph_ticket_draft_disposition_scan_state.scan_count+1",
                    params![operation_key, now_rfc3339()],
                );
            }
        }
    }
    Ok(json!({
        "schema":"narada.graph_mail.ticket_draft_disposition_scan.v1",
        "status":if errors.is_empty() { "completed" } else { "completed_with_errors" },
        "operations_scanned":keys.len(),
        "observations_recorded":observations_recorded,
        "still_pending":still_pending,
        "errors":errors
    }))
}

fn insert_disposition_observation(
    connection: &Connection,
    operation: &TicketOperation,
    receipt: &Value,
) -> Result<bool, Value> {
    let receipt_json = canonical_json(receipt);
    let result = connection
        .execute(
            "insert into graph_ticket_draft_disposition_observations(observation_id, operation_key, ticket_id, mailbox_id, draft_id, disposition, evidence_kind, evidence_id, receipt_json, observed_at) values (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10) on conflict(operation_key) do nothing",
            params![receipt.get("observation_id").and_then(Value::as_str).unwrap_or_default(), operation.operation_key, operation.ticket_id, operation.mailbox_id, operation.draft_id.clone().unwrap_or_default(), receipt.get("disposition").and_then(Value::as_str).unwrap_or_default(), receipt.get("evidence_kind").and_then(Value::as_str).unwrap_or_default(), receipt.get("evidence_id").and_then(Value::as_str).unwrap_or_default(), receipt_json, receipt.get("observed_at").and_then(Value::as_str).unwrap_or_default()],
        )
        .map_err(|error| unavailable("graph_ticket_draft_disposition_record_failed", &error.to_string()))?;
    Ok(result == 1)
}

fn ticket_disposition_list(args: &Map<String, Value>, root: &Path) -> Result<Value, Value> {
    let consumer_id = required_string(args, "consumer_id")?;
    let limit = args
        .get("limit")
        .and_then(Value::as_u64)
        .unwrap_or(5)
        .clamp(1, 5);
    let connection = ticket_store(root)?;
    let mut statement = connection
        .prepare("select observation.receipt_json from graph_ticket_draft_disposition_observations observation where not exists (select 1 from graph_ticket_draft_disposition_receipts receipt where receipt.observation_id=observation.observation_id and receipt.consumer_id=?1) order by observation.observed_at asc, observation.observation_id asc limit ?2")
        .map_err(|error| unavailable("graph_ticket_draft_disposition_list_failed", &error.to_string()))?;
    let rows = statement
        .query_map(params![consumer_id, limit], |row| row.get::<_, String>(0))
        .map_err(|error| unavailable("graph_ticket_draft_disposition_list_failed", &error.to_string()))?;
    let mut items = Vec::new();
    for row in rows {
        let text = row.map_err(|error| unavailable("graph_ticket_draft_disposition_list_failed", &error.to_string()))?;
        if let Ok(value) = serde_json::from_str::<Value>(&text) {
            items.push(value);
        }
    }
    Ok(json!({
        "schema":"narada.graph_mail.ticket_draft_disposition_list.v1",
        "consumer_id":consumer_id,
        "items":items,
        "count":items.len()
    }))
}

fn ticket_disposition_ack(args: &Map<String, Value>, root: &Path) -> Result<Value, Value> {
    let observation_id = required_string(args, "observation_id")?;
    let consumer_id = required_string(args, "consumer_id")?;
    let reconciliation_ref = required_string(args, "reconciliation_ref")?;
    let receipt = args
        .get("reconciliation_receipt")
        .cloned()
        .filter(|value| value.is_object())
        .unwrap_or_else(|| json!({}));
    let receipt_json = canonical_json(&receipt);
    let connection = ticket_store(root)?;
    let changes = connection
        .execute(
            "insert into graph_ticket_draft_disposition_receipts(observation_id, consumer_id, reconciliation_ref, receipt_json, acknowledged_at) values (?1, ?2, ?3, ?4, ?5) on conflict(observation_id, consumer_id) do nothing",
            params![observation_id, consumer_id, reconciliation_ref, receipt_json, now_rfc3339()],
        )
        .map_err(|error| unavailable("graph_ticket_draft_disposition_ack_failed", &error.to_string()))?;
    let existing = connection
        .query_row(
            "select reconciliation_ref, receipt_json from graph_ticket_draft_disposition_receipts where observation_id=?1 and consumer_id=?2",
            params![observation_id, consumer_id],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
        )
        .optional()
        .map_err(|error| unavailable("graph_ticket_draft_disposition_ack_failed", &error.to_string()))?;
    let Some((stored_ref, stored_receipt)) = existing else {
        return Err(unavailable("graph_ticket_draft_disposition_ack_not_found", &observation_id));
    };
    if stored_ref != reconciliation_ref || stored_receipt != receipt_json {
        return Err(unavailable("graph_ticket_draft_disposition_ack_conflict", &observation_id));
    }
    Ok(json!({
        "schema":"narada.graph_mail.ticket_draft_disposition_ack.v1",
        "status":if changes == 1 { "acknowledged" } else { "already_acknowledged" },
        "observation_id":observation_id,
        "consumer_id":consumer_id,
        "reconciliation_ref":reconciliation_ref
    }))
}

#[derive(Clone)]
struct TicketOperation {
    operation_key: String,
    action_idempotency_key: String,
    request_digest: String,
    draft_request_digest: String,
    ticket_id: String,
    effect_claim_id: String,
    mailbox_id: String,
    source_message_id: String,
    reply_mode: String,
    status: String,
    draft_id: Option<String>,
    receipt_id: Option<String>,
    draft_ref: Option<Value>,
    completed_at: Option<String>,
}

fn ticket_store(root: &Path) -> Result<Connection, Value> {
    let directory = root.join(".narada/runtime/graph-mail-domain");
    fs::create_dir_all(&directory)
        .map_err(|error| unavailable("graph_ticket_draft_directory_failed", &error.to_string()))?;
    let connection = Connection::open(directory.join("graph-mail-domain.db"))
        .map_err(|error| unavailable("graph_ticket_draft_database_open_failed", &error.to_string()))?;
    connection
        .execute_batch(
            "pragma busy_timeout = 30000; pragma foreign_keys = on; create table if not exists graph_ticket_draft_operations(operation_key text primary key, action_idempotency_key text not null unique, request_digest text not null, draft_request_digest text not null, ticket_id text not null, effect_claim_id text not null, mailbox_id text not null, source_message_id text not null, reply_mode text not null, status text not null, draft_id text, receipt_id text, draft_ref_json text, created_at text not null, updated_at text not null, completed_at text); create table if not exists graph_ticket_draft_disposition_scan_state(operation_key text primary key, last_scanned_at text not null, scan_count integer not null); create table if not exists graph_ticket_draft_disposition_observations(observation_id text primary key, operation_key text not null unique, ticket_id text not null, mailbox_id text not null, draft_id text not null, disposition text not null, evidence_kind text not null, evidence_id text not null unique, receipt_json text not null, observed_at text not null); create table if not exists graph_ticket_draft_disposition_receipts(observation_id text not null, consumer_id text not null, reconciliation_ref text not null, receipt_json text not null, acknowledged_at text not null, primary key(observation_id, consumer_id)); create table if not exists graph_ticket_draft_discard_intents(operation_key text primary key, idempotency_key text not null unique, request_digest text not null, status text not null, verified_etag text, verified_at text, receipt_json text, created_at text not null, updated_at text not null, completed_at text);",
        )
        .map_err(|error| unavailable("graph_ticket_draft_schema_failed", &error.to_string()))?;
    Ok(connection)
}

fn find_ticket_operation(connection: &Connection, operation_key: &str) -> Result<Option<TicketOperation>, Value> {
    connection
        .query_row(
            "select operation_key, action_idempotency_key, request_digest, draft_request_digest, ticket_id, effect_claim_id, mailbox_id, source_message_id, reply_mode, status, draft_id, receipt_id, draft_ref_json, completed_at from graph_ticket_draft_operations where operation_key=?1",
            params![operation_key],
            |row| {
                let draft_ref: Option<String> = row.get(12)?;
                Ok(TicketOperation {
                    operation_key: row.get(0)?,
                    action_idempotency_key: row.get(1)?,
                    request_digest: row.get(2)?,
                    draft_request_digest: row.get(3)?,
                    ticket_id: row.get(4)?,
                    effect_claim_id: row.get(5)?,
                    mailbox_id: row.get(6)?,
                    source_message_id: row.get(7)?,
                    reply_mode: row.get(8)?,
                    status: row.get(9)?,
                    draft_id: row.get(10)?,
                    receipt_id: row.get(11)?,
                    draft_ref: draft_ref.and_then(|value| serde_json::from_str(&value).ok()),
                    completed_at: row.get(13)?,
                })
            },
        )
        .optional()
        .map_err(|error| unavailable("graph_ticket_draft_database_read_failed", &error.to_string()))
}

