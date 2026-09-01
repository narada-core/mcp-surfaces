fn init_outbox_schema(db: &Connection) -> Result<(), Value> {
    db.execute_batch(
        r#"
        CREATE TABLE IF NOT EXISTS mailbox_sync_generations(
          generation_id TEXT PRIMARY KEY,
          idempotency_key TEXT NOT NULL UNIQUE,
          request_fingerprint TEXT NOT NULL,
          scope_id TEXT NOT NULL,
          config_fingerprint TEXT NOT NULL,
          status TEXT NOT NULL CHECK(status IN ('accepted','staged','completed','failed')),
          parent_cursor TEXT,
          next_cursor TEXT,
          batch_path TEXT,
          batch_sha256 TEXT,
          batch_record_count INTEGER NOT NULL DEFAULT 0,
          staged_at TEXT,
          receipt_json TEXT,
          error_message TEXT,
          lease_token TEXT,
          lease_expires_at TEXT,
          created_at TEXT NOT NULL,
          updated_at TEXT NOT NULL,
          completed_at TEXT
        );
        CREATE TABLE IF NOT EXISTS mailbox_sync_generation_records(
          generation_id TEXT NOT NULL REFERENCES mailbox_sync_generations(generation_id),
          record_id TEXT NOT NULL,
          ordinal TEXT,
          fact_id TEXT NOT NULL,
          event_kind TEXT NOT NULL,
          message_id TEXT,
          mailbox_id TEXT,
          conversation_id TEXT,
          source_version TEXT,
          application_status TEXT NOT NULL CHECK(application_status IN ('staged','already_applied','projected','not_applied','reconciled')),
          PRIMARY KEY(generation_id, record_id)
        );
        CREATE TABLE IF NOT EXISTS mailbox_sync_scope_leases(
          scope_id TEXT PRIMARY KEY,
          generation_id TEXT NOT NULL REFERENCES mailbox_sync_generations(generation_id),
          lease_token TEXT NOT NULL,
          expires_at TEXT NOT NULL,
          updated_at TEXT NOT NULL
        );
        CREATE TABLE IF NOT EXISTS mailbox_message_observations(
          observation_id TEXT PRIMARY KEY,
          mailbox_id TEXT NOT NULL,
          message_id TEXT NOT NULL,
          first_generation_id TEXT NOT NULL REFERENCES mailbox_sync_generations(generation_id),
          first_fact_id TEXT NOT NULL,
          observed_at TEXT NOT NULL,
          UNIQUE(mailbox_id, message_id)
        );
        CREATE TABLE IF NOT EXISTS mailbox_outbox(
          event_id TEXT PRIMARY KEY,
          scope_id TEXT NOT NULL,
          topic TEXT NOT NULL,
          aggregate_id TEXT NOT NULL,
          aggregate_revision INTEGER NOT NULL,
          schema_version INTEGER NOT NULL,
          causation_id TEXT NOT NULL,
          idempotency_key TEXT NOT NULL UNIQUE,
          partition_key TEXT NOT NULL,
          occurred_at TEXT NOT NULL,
          payload_json TEXT NOT NULL
        );
        CREATE TABLE IF NOT EXISTS mailbox_outbox_consumers(
          consumer_id TEXT PRIMARY KEY,
          scope_id TEXT,
          topics_json TEXT,
          start_at TEXT NOT NULL,
          created_at TEXT NOT NULL
        );
        CREATE TABLE IF NOT EXISTS mailbox_outbox_receipts(
          consumer_id TEXT NOT NULL REFERENCES mailbox_outbox_consumers(consumer_id),
          event_id TEXT NOT NULL REFERENCES mailbox_outbox(event_id),
          receipt_fingerprint TEXT NOT NULL,
          receipt_json TEXT NOT NULL,
          acknowledged_at TEXT NOT NULL,
          PRIMARY KEY(consumer_id, event_id)
        );
        CREATE TABLE IF NOT EXISTS mailbox_admission_receipts(
          admission_id TEXT PRIMARY KEY,
          idempotency_key TEXT NOT NULL UNIQUE,
          request_fingerprint TEXT NOT NULL,
          scope_id TEXT NOT NULL,
          fact_id TEXT NOT NULL,
          policy_version TEXT NOT NULL,
          decision_json TEXT NOT NULL,
          created_at TEXT NOT NULL
        );
        CREATE TABLE IF NOT EXISTS mailbox_reconciliation_operations(
          operation_id TEXT PRIMARY KEY,
          idempotency_key TEXT NOT NULL UNIQUE,
          request_fingerprint TEXT NOT NULL,
          scope_id TEXT NOT NULL,
          generation_id TEXT NOT NULL REFERENCES mailbox_sync_generations(generation_id),
          result_json TEXT NOT NULL,
          created_at TEXT NOT NULL
        );
        CREATE INDEX IF NOT EXISTS mailbox_outbox_order_idx
          ON mailbox_outbox(occurred_at, event_id);
        CREATE INDEX IF NOT EXISTS mailbox_outbox_subscription_idx
          ON mailbox_outbox(scope_id, topic, occurred_at, event_id);
        CREATE INDEX IF NOT EXISTS mailbox_generation_scope_idx
          ON mailbox_sync_generations(scope_id, created_at);
        CREATE UNIQUE INDEX IF NOT EXISTS mailbox_admission_scope_fact_idx
          ON mailbox_admission_receipts(scope_id, fact_id);
        PRAGMA user_version = 2;
        "#,
    )
    .map_err(|e| error("mailbox_domain_schema_failed", &e.to_string()))?;
    Ok(())
}

fn generation_show(args: &Map<String, Value>, root: &Path) -> Result<Value, Value> {
    let generation_id = required_bounded(args, "generation_id", "mailbox_generation_id_required", 128)?;
    let offset = bounded_integer(args.get("offset"), 0, 0, 1_000_000)?;
    let limit = bounded_integer(args.get("limit"), 100, 1, 100)?;
    let Some(db) = open_domain_db(root)? else {
        let code = format!("mailbox_sync_generation_not_found:{generation_id}");
        return Err(error(&code, &code));
    };
    let row: Option<(
        String,
        String,
        String,
        Option<String>,
        Option<String>,
        Option<String>,
        i64,
        Option<String>,
        Option<String>,
        String,
        String,
        Option<String>,
    )> = db
        .query_row(
            "SELECT scope_id,config_fingerprint,status,parent_cursor,next_cursor,batch_sha256,batch_record_count,receipt_json,error_message,created_at,updated_at,completed_at FROM mailbox_sync_generations WHERE generation_id=?",
            params![generation_id],
            |row| {
                Ok((
                    row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?, row.get(5)?,
                    row.get(6)?, row.get(7)?, row.get(8)?, row.get(9)?, row.get(10)?, row.get(11)?,
                ))
            },
        )
        .optional()
        .map_err(|e| error("mailbox_generation_query_failed", &e.to_string()))?;
    let Some((
        scope_id,
        config_fingerprint,
        status,
        parent_cursor,
        next_cursor,
        batch_sha256,
        batch_record_count,
        receipt_json,
        error_message,
        created_at,
        updated_at,
        completed_at,
    )) = row
    else {
        let code = format!("mailbox_sync_generation_not_found:{generation_id}");
        return Err(error(&code, &code));
    };
    let receipt = receipt_json
        .map(|value| serde_json::from_str::<Value>(&value))
        .transpose()
        .map_err(|e| error("mailbox_generation_receipt_invalid", &e.to_string()))?
        .unwrap_or(Value::Null);
    let mut statement = db
        .prepare("SELECT record_id,fact_id,event_kind,message_id,mailbox_id,conversation_id,source_version,application_status FROM mailbox_sync_generation_records WHERE generation_id=? ORDER BY rowid LIMIT ? OFFSET ?")
        .map_err(|e| error("mailbox_generation_record_query_failed", &e.to_string()))?;
    let rows = statement
        .query_map(params![generation_id,limit,offset], |row| {
            Ok(json!({
                "record_id":row.get::<_,String>(0)?,
                "fact_id":row.get::<_,String>(1)?,
                "event_kind":row.get::<_,String>(2)?,
                "message_id":row.get::<_,Option<String>>(3)?,
                "mailbox_id":row.get::<_,Option<String>>(4)?,
                "conversation_id":row.get::<_,Option<String>>(5)?,
                "source_version":row.get::<_,Option<String>>(6)?,
                "application_status":row.get::<_,String>(7)?
            }))
        })
        .map_err(|e| error("mailbox_generation_record_query_failed", &e.to_string()))?;
    let mut records = Vec::new();
    for row in rows {
        records.push(row.map_err(|e| error("mailbox_generation_record_row_failed", &e.to_string()))?);
    }
    let records_len = records.len() as i64;
    Ok(json!({
        "schema":"narada.mailbox.sync_generation.v1",
        "generation":{
            "generation_id":generation_id,
            "scope_id":scope_id,
            "config_fingerprint":config_fingerprint,
            "status":status,
            "parent_cursor_sha256":parent_cursor.map(|value| sha256_hex(value.as_bytes())),
            "next_cursor_sha256":next_cursor.map(|value| sha256_hex(value.as_bytes())),
            "batch_sha256":batch_sha256,
            "batch_record_count":batch_record_count,
            "receipt":receipt,
            "error_message":error_message,
            "created_at":created_at,
            "updated_at":updated_at,
            "completed_at":completed_at
        },
        "offset":offset,"limit":limit,"records":records,
        "next_offset":if offset+records_len<batch_record_count{Some(offset+records_len)}else{None},
        "records_truncated":offset+records_len<batch_record_count
    }))
}

fn message_fact_find(args: &Map<String, Value>, root: &Path) -> Result<Value, Value> {
    let scope_id = required_bounded(args, "scope_id", "mailbox_fact_find_scope_id_required", 256)?;
    let message_id = required_bounded(args, "message_id", "mailbox_fact_find_message_id_required", 1024)?;
    let Some(db) = open_domain_db(root)? else {
        return Ok(json!({
            "schema":"narada.mailbox.message_fact_lookup.v1",
            "status":"not_found",
            "scope_id":scope_id,
            "message_id":message_id
        }));
    };
    let observation: Option<Value> = db
        .query_row(
            "SELECT observation.observation_id,observation.mailbox_id,observation.message_id,observation.first_generation_id,observation.first_fact_id,observation.observed_at,event.event_id FROM mailbox_message_observations observation JOIN mailbox_sync_generations generation ON generation.generation_id=observation.first_generation_id LEFT JOIN mailbox_outbox event ON event.aggregate_id=observation.observation_id AND event.topic='mailbox.message.first_observed' WHERE generation.scope_id=? AND observation.message_id=?",
            params![scope_id, message_id],
            |row| {
                Ok(json!({
                    "observation_id":row.get::<_,String>(0)?,
                    "mailbox_id":row.get::<_,String>(1)?,
                    "message_id":row.get::<_,String>(2)?,
                    "first_generation_id":row.get::<_,String>(3)?,
                    "first_fact_id":row.get::<_,String>(4)?,
                    "observed_at":row.get::<_,String>(5)?,
                    "event_id":row.get::<_,Option<String>>(6)?
                }))
            },
        )
        .optional()
        .map_err(|e| error("mailbox_fact_find_query_failed", &e.to_string()))?;
    if let Some(observation) = observation {
        Ok(json!({
            "schema":"narada.mailbox.message_fact_lookup.v1",
            "status":"ok",
            "scope_id":scope_id,
            "message_id":message_id,
            "fact_id":observation.get("first_fact_id"),
            "source_event_id":observation.get("event_id"),
            "observation":observation
        }))
    } else {
        Ok(json!({
            "schema":"narada.mailbox.message_fact_lookup.v1",
            "status":"not_found",
            "scope_id":scope_id,
            "message_id":message_id
        }))
    }
}

