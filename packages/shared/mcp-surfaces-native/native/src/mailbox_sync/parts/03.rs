fn sync_config_fingerprint(scope: &ScopeConfig) -> String {
    let mut source = Map::new();
    source.insert("type".to_string(), json!("graph"));
    if let Some(value) = &scope.graph.mailbox_id {
        source.insert("mailbox_id".to_string(), json!(value));
    }
    source.insert("user_id".to_string(), json!(scope.graph.user_id));
    if let Some(value) = &scope.graph.configured_base_url {
        source.insert("base_url".to_string(), json!(value));
    }
    source.insert(
        "prefer_immutable_ids".to_string(),
        json!(scope.graph.prefer_immutable_ids),
    );
    fingerprint(&json!({
        "schema":"narada.mailbox.sync_config.v1",
        "scope_id":scope.scope_id,
        "root_dir":scope.root_dir_text,
        "source":Value::Object(source),
        "scope":{
            "included_container_refs":scope.included_container_refs,
            "included_item_kinds":scope.included_item_kinds,
        },
        "normalize":{
            "attachment_policy":scope.attachment_policy,
            "body_policy":scope.body_policy,
            "include_headers":scope.include_headers,
            "tombstones_enabled":scope.tombstones_enabled,
        },
    }))
}

fn open_domain_db(site_root: &Path) -> Result<Connection, Value> {
    let path = site_root.join(DOMAIN_DB_RELATIVE);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|e| error("mailbox_domain_store_directory_failed", &e.to_string()))?;
    }
    let db = Connection::open(path)
        .map_err(|e| error("mailbox_domain_store_open_failed", &e.to_string()))?;
    db.busy_timeout(Duration::from_millis(5_000))
        .map_err(|e| error("mailbox_domain_store_pragma_failed", &e.to_string()))?;
    db.pragma_update(None, "journal_mode", "WAL")
        .map_err(|e| error("mailbox_domain_store_pragma_failed", &e.to_string()))?;
    db.pragma_update(None, "foreign_keys", true)
        .map_err(|e| error("mailbox_domain_store_pragma_failed", &e.to_string()))?;
    init_domain_schema(&db)?;
    Ok(db)
}

fn init_domain_schema(db: &Connection) -> Result<(), Value> {
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
        CREATE INDEX IF NOT EXISTS mailbox_outbox_order_idx ON mailbox_outbox(occurred_at,event_id);
        CREATE INDEX IF NOT EXISTS mailbox_outbox_subscription_idx ON mailbox_outbox(scope_id,topic,occurred_at,event_id);
        CREATE INDEX IF NOT EXISTS mailbox_generation_scope_idx ON mailbox_sync_generations(scope_id,created_at);
        CREATE UNIQUE INDEX IF NOT EXISTS mailbox_admission_scope_fact_idx ON mailbox_admission_receipts(scope_id,fact_id);
        PRAGMA user_version=2;
        "#,
    )
    .map_err(|e| error("mailbox_domain_schema_failed", &e.to_string()))
}

fn claim_generation(
    db: &mut Connection,
    generation_id: &str,
    idempotency_key: &str,
    request_fingerprint: &str,
    scope_id: &str,
    config_fingerprint: &str,
    now: &str,
) -> Result<(Generation, Option<String>), Value> {
    let tx = db
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|e| error("mailbox_domain_transaction_failed", &e.to_string()))?;
    let existing_id: Option<String> = tx
        .query_row(
            "SELECT generation_id FROM mailbox_sync_generations WHERE idempotency_key=?",
            params![idempotency_key],
            |row| row.get(0),
        )
        .optional()
        .map_err(|e| error("mailbox_sync_generation_query_failed", &e.to_string()))?;
    if existing_id.is_none() {
        tx.execute(
            "INSERT INTO mailbox_sync_generations(generation_id,idempotency_key,request_fingerprint,scope_id,config_fingerprint,status,created_at,updated_at) VALUES (?,?,?,?,?,'accepted',?,?)",
            params![generation_id,idempotency_key,request_fingerprint,scope_id,config_fingerprint,now,now],
        )
        .map_err(|e| error("mailbox_sync_generation_insert_failed", &e.to_string()))?;
    }
    let generation = require_generation_tx(&tx, existing_id.as_deref().unwrap_or(generation_id))?;
    if generation.generation_id != generation_id
        || generation.request_fingerprint != request_fingerprint
        || generation.scope_id != scope_id
        || generation.config_fingerprint != config_fingerprint
    {
        let code = format!("mailbox_sync_idempotency_conflict:{idempotency_key}");
        return Err(error(&code, &code));
    }
    if matches!(generation.status.as_str(), "completed" | "failed") {
        tx.commit()
            .map_err(|e| error("mailbox_domain_transaction_commit_failed", &e.to_string()))?;
        return Ok((generation, None));
    }
    let active: Option<(String, String)> = tx
        .query_row(
            "SELECT generation_id,expires_at FROM mailbox_sync_scope_leases WHERE scope_id=?",
            params![scope_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .map_err(|e| error("mailbox_sync_lease_query_failed", &e.to_string()))?;
    if let Some((active_generation, expires_at)) = &active {
        if expires_at.as_str() > now {
            let code = format!("mailbox_sync_scope_busy:{scope_id}:{active_generation}");
            return Err(error(&code, &code));
        }
        tx.execute(
            "DELETE FROM mailbox_sync_scope_leases WHERE scope_id=?",
            params![scope_id],
        )
        .map_err(|e| error("mailbox_sync_lease_delete_failed", &e.to_string()))?;
    }
    let token = Uuid::new_v4().to_string();
    let expires_at = add_millis_iso(now, LEASE_MS)?;
    tx.execute(
        "INSERT INTO mailbox_sync_scope_leases(scope_id,generation_id,lease_token,expires_at,updated_at) VALUES (?,?,?,?,?)",
        params![scope_id,generation.generation_id,token,expires_at,now],
    )
    .map_err(|e| error("mailbox_sync_lease_insert_failed", &e.to_string()))?;
    tx.execute(
        "UPDATE mailbox_sync_generations SET lease_token=?,lease_expires_at=?,updated_at=? WHERE generation_id=?",
        params![token,expires_at,now,generation.generation_id],
    )
    .map_err(|e| error("mailbox_sync_generation_update_failed", &e.to_string()))?;
    let claimed = require_generation_tx(&tx, &generation.generation_id)?;
    tx.commit()
        .map_err(|e| error("mailbox_domain_transaction_commit_failed", &e.to_string()))?;
    Ok((claimed, Some(token)))
}

fn renew_lease(
    db: &mut Connection,
    scope_id: &str,
    generation_id: &str,
    token: &str,
) -> Result<(), Value> {
    let now = now_iso_millis();
    let expires_at = add_millis_iso(&now, LEASE_MS)?;
    let tx = db
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|e| error("mailbox_domain_transaction_failed", &e.to_string()))?;
    let changes = tx
        .execute(
            "UPDATE mailbox_sync_scope_leases SET expires_at=?,updated_at=? WHERE scope_id=? AND generation_id=? AND lease_token=?",
            params![expires_at,now,scope_id,generation_id,token],
        )
        .map_err(|e| error("mailbox_sync_lease_update_failed", &e.to_string()))?;
    if changes != 1 {
        let code = format!("mailbox_sync_lease_lost:{scope_id}");
        return Err(error(&code, &code));
    }
    tx.execute(
        "UPDATE mailbox_sync_generations SET lease_expires_at=?,updated_at=? WHERE generation_id=? AND lease_token=?",
        params![expires_at,now,generation_id,token],
    )
    .map_err(|e| error("mailbox_sync_generation_update_failed", &e.to_string()))?;
    tx.commit()
        .map_err(|e| error("mailbox_domain_transaction_commit_failed", &e.to_string()))
}

