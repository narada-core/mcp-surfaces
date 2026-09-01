fn attachment_extensions(attachment: &Map<String, Value>, kind: &str) -> Option<Value> {
    let mut graph = Map::new();
    if kind != "#microsoft.graph.fileAttachment" {
        graph.insert("odata_type".to_string(), json!(kind));
    }
    if kind == "#microsoft.graph.referenceAttachment" {
        for (target, source) in [
            ("source_url", "sourceUrl"),
            ("provider_type", "providerType"),
            ("permission", "permission"),
            ("is_folder", "isFolder"),
        ] {
            if let Some(value) = attachment.get(source) {
                graph.insert(target.to_string(), value.clone());
            }
        }
    }
    if let Some(value) = attachment.get("lastModifiedDateTime") {
        graph.insert("last_modified_at".to_string(), value.clone());
    }
    if graph.is_empty() {
        None
    } else {
        Some(json!({"namespaces":{"graph":Value::Object(graph)}}))
    }
}

fn graph_message_extensions(message: &Map<String, Value>) -> Option<Value> {
    let mut graph = Map::new();
    for (target, source) in [
        ("raw_id", "id"),
        ("change_key", "changeKey"),
        ("parent_folder_id", "parentFolderId"),
        ("queried_folder_ref", "sourceQueriedFolderRef"),
        ("web_link", "webLink"),
        ("inference_classification", "inferenceClassification"),
        ("flag", "flag"),
        ("unique_body", "uniqueBody"),
    ] {
        if let Some(value) = message.get(source) {
            graph.insert(target.to_string(), value.clone());
        }
    }
    if graph.is_empty() {
        None
    } else {
        Some(json!({"namespaces":{"graph":Value::Object(graph)}}))
    }
}

fn insert_optional_string(target: &mut Value, key: &str, value: Option<&Value>) {
    if let Some(value) = value
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
    {
        target
            .as_object_mut()
            .expect("object")
            .insert(key.to_string(), json!(value));
    }
}

fn process_batch(
    domain_db: &mut Connection,
    scope: &ScopeConfig,
    generation_id: &str,
    lease_token: &str,
    batch: &SourceBatch,
) -> Result<(), Value> {
    let facts_path = scope.root_dir.join(".narada/facts.db");
    if let Some(parent) = facts_path.parent() {
        fs::create_dir_all(parent)
            .map_err(|e| error("mailbox_fact_store_directory_failed", &e.to_string()))?;
    }
    let facts = Connection::open(facts_path)
        .map_err(|e| error("mailbox_fact_store_open_failed", &e.to_string()))?;
    facts
        .busy_timeout(Duration::from_millis(5_000))
        .map_err(|e| error("mailbox_fact_store_pragma_failed", &e.to_string()))?;
    facts
        .pragma_update(None, "journal_mode", "WAL")
        .map_err(|e| error("mailbox_fact_store_pragma_failed", &e.to_string()))?;
    init_fact_schema(&facts)?;
    for (index, record) in batch.records.iter().enumerate() {
        if index % 10 == 0 {
            renew_lease(domain_db, &scope.scope_id, generation_id, lease_token)?;
        }
        // Fact ingestion and projection idempotency are separate concerns. An apply
        // marker proves that the filesystem projection exists; it does not prove
        // that the fact referenced by this generation exists in the fact store.
        // Always ingest the immutable fact before using the projection marker.
        let (fact_id, fact_type, provenance, payload_json) =
            fact_for_record(record, batch.next_checkpoint.as_deref())?;
        ingest_fact(&facts, &fact_id, &fact_type, &provenance, &payload_json)?;
        let marker = apply_marker_path(&scope.root_dir, &record.record_id);
        if marker.is_file() {
            validate_apply_marker(&marker)?;
            mark_record_application(
                domain_db,
                generation_id,
                &record.record_id,
                "already_applied",
            )?;
            continue;
        }
        assert_lease(domain_db, &scope.scope_id, generation_id, lease_token)?;
        let applied = project_record(scope, record)?;
        assert_lease(domain_db, &scope.scope_id, generation_id, lease_token)?;
        mark_record_application(
            domain_db,
            generation_id,
            &record.record_id,
            if applied { "projected" } else { "not_applied" },
        )?;
        if applied {
            write_apply_marker(&marker, record)?;
        }
    }
    if let Some(next) = &batch.next_checkpoint {
        renew_lease(domain_db, &scope.scope_id, generation_id, lease_token)?;
        commit_cursor(scope, next)?;
        assert_lease(domain_db, &scope.scope_id, generation_id, lease_token)?;
    }
    Ok(())
}

fn init_fact_schema(db: &Connection) -> Result<(), Value> {
    db.execute_batch(
        r#"
        CREATE TABLE IF NOT EXISTS facts(
          fact_id TEXT PRIMARY KEY,
          fact_type TEXT NOT NULL,
          source_id TEXT NOT NULL,
          source_record_id TEXT NOT NULL,
          source_version TEXT,
          source_cursor TEXT,
          provenance_json TEXT NOT NULL,
          payload_json TEXT NOT NULL,
          created_at TEXT NOT NULL DEFAULT (datetime('now')),
          admitted_at TEXT
        );
        CREATE INDEX IF NOT EXISTS idx_facts_source_record ON facts(source_id,source_record_id);
        CREATE INDEX IF NOT EXISTS idx_facts_source_cursor ON facts(source_id,source_cursor,created_at);
        CREATE INDEX IF NOT EXISTS idx_facts_type ON facts(fact_type,created_at);
        CREATE INDEX IF NOT EXISTS idx_facts_admitted ON facts(source_id,admitted_at,created_at);
        "#,
    )
    .map_err(|e| error("mailbox_fact_schema_failed", &e.to_string()))
}

fn ingest_fact(
    db: &Connection,
    fact_id: &str,
    fact_type: &str,
    provenance: &Value,
    payload_json: &str,
) -> Result<(), Value> {
    if db
        .query_row(
            "SELECT 1 FROM facts WHERE fact_id=?",
            params![fact_id],
            |row| row.get::<_, i64>(0),
        )
        .optional()
        .map_err(|e| error("mailbox_fact_query_failed", &e.to_string()))?
        .is_some()
    {
        return Ok(());
    }
    db.execute(
        "INSERT INTO facts(fact_id,fact_type,source_id,source_record_id,source_version,source_cursor,provenance_json,payload_json,created_at) VALUES (?,?,?,?,?,?,?,?,datetime('now'))",
        params![
            fact_id,
            fact_type,
            provenance.get("source_id").and_then(Value::as_str),
            provenance.get("source_record_id").and_then(Value::as_str),
            provenance.get("source_version").and_then(Value::as_str),
            provenance.get("source_cursor").and_then(Value::as_str),
            serde_json::to_string(provenance).unwrap_or_else(|_| "{}".to_string()),
            payload_json,
        ],
    )
    .map_err(|e| error("mailbox_fact_insert_failed", &e.to_string()))?;
    Ok(())
}

fn project_record(scope: &ScopeConfig, record: &SourceRecord) -> Result<bool, Value> {
    let event = record.payload.as_object().ok_or_else(|| {
        error(
            "mailbox_projection_event_invalid",
            "mailbox_projection_event_invalid",
        )
    })?;
    let kind = event
        .get("event_kind")
        .and_then(Value::as_str)
        .unwrap_or("");
    let message_id = event
        .get("message_id")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            error(
                "mailbox_projection_message_id_missing",
                "mailbox_projection_message_id_missing",
            )
        })?;
    match kind {
        "upsert" | "created" | "updated" => {
            let payload = event.get("payload").ok_or_else(|| {
                error(
                    "mailbox_projection_payload_missing",
                    &format!("Upsert event {} is missing payload", record.record_id),
                )
            })?;
            install_blobs(&scope.root_dir, payload)?;
            write_message_projection(&scope.root_dir, payload)?;
            if scope.tombstones_enabled {
                remove_path(
                    &scope
                        .root_dir
                        .join("tombstones")
                        .join(format!("{}.json", safe_segment(message_id))),
                )?;
            }
            mark_views(&scope.root_dir, payload)?;
            Ok(true)
        }
        "delete" | "deleted" => {
            if scope.tombstones_enabled {
                write_tombstone(&scope.root_dir, event)?;
            }
            let message_path = scope
                .root_dir
                .join("messages")
                .join(safe_segment(message_id));
            if message_path.exists() {
                fs::remove_dir_all(&message_path).map_err(|e| {
                    error("mailbox_projection_message_remove_failed", &e.to_string())
                })?;
            }
            unlink_view(
                &scope
                    .root_dir
                    .join("views/unread")
                    .join(safe_segment(message_id)),
            )?;
            unlink_view(
                &scope
                    .root_dir
                    .join("views/flagged")
                    .join(safe_segment(message_id)),
            )?;
            Ok(true)
        }
        _ => Err(error(
            "mailbox_projection_event_kind_unknown",
            &format!("Unknown event kind: {kind}"),
        )),
    }
}

