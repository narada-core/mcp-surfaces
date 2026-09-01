fn outbox_ack(args: &Map<String, Value>, root: &Path) -> Result<Value, Value> {
    let consumer_id = required_bounded(args, "consumer_id", "mailbox_outbox_consumer_id_required", 256)?;
    let event_id = required_bounded(args, "event_id", "mailbox_outbox_event_id_required", 256)?;
    let raw_receipt = args
        .get("receipt")
        .and_then(Value::as_object)
        .ok_or_else(|| error("mailbox_outbox_receipt_required", "mailbox_outbox_receipt_required"))?;
    if raw_receipt
        .keys()
        .any(|key| !matches!(key.as_str(), "schema" | "outcome" | "effect_ref"))
    {
        return Err(error(
            "mailbox_outbox_receipt_fields_invalid",
            "mailbox_outbox_receipt_fields_invalid",
        ));
    }
    let receipt = json!({
        "schema":required_bounded(raw_receipt, "schema", "mailbox_outbox_receipt_schema_required", 128)?,
        "outcome":required_bounded(raw_receipt, "outcome", "mailbox_outbox_receipt_outcome_required", 64)?,
        "effect_ref":required_bounded(raw_receipt, "effect_ref", "mailbox_outbox_receipt_effect_ref_required", 512)?
    });
    let receipt_json = serde_json::to_string(&receipt)
        .map_err(|e| error("mailbox_outbox_receipt_encode_failed", &e.to_string()))?;
    let receipt_fingerprint = sha256_hex(canonical_json(&receipt).as_bytes());
    let now = now_iso_millis();
    let mut db = open_domain_db_write(root)?;
    let tx = db
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|e| error("mailbox_domain_transaction_failed", &e.to_string()))?;
    let result = (|| {
        let consumer: Option<(Option<String>, Option<String>)> = tx
            .query_row(
                "SELECT scope_id,topics_json FROM mailbox_outbox_consumers WHERE consumer_id=?",
                params![consumer_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .map_err(|e| error("mailbox_outbox_consumer_query_failed", &e.to_string()))?;
        let Some((Some(scope_id), Some(topics_json))) = consumer else {
            let code = if consumer.is_some() {
                format!("mailbox_outbox_consumer_v2_registration_required:{consumer_id}")
            } else {
                format!("mailbox_outbox_consumer_not_registered:{consumer_id}")
            };
            return Err(error(&code, &code));
        };
        let topics = parsed_topics(Some(&topics_json), &consumer_id)?;
        let event: Option<(String, String)> = tx
            .query_row(
                "SELECT scope_id,topic FROM mailbox_outbox WHERE event_id=?",
                params![event_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .map_err(|e| error("mailbox_outbox_event_query_failed", &e.to_string()))?;
        let Some((event_scope, event_topic)) = event else {
            let code = format!("mailbox_outbox_event_not_found:{event_id}");
            return Err(error(&code, &code));
        };
        if event_scope != scope_id || !topics.iter().any(|topic| topic == &event_topic) {
            let code = format!("mailbox_outbox_event_not_subscribed:{consumer_id}:{event_id}");
            return Err(error(&code, &code));
        }
        let existing: Option<(String, String)> = tx
            .query_row(
                "SELECT receipt_fingerprint,receipt_json FROM mailbox_outbox_receipts WHERE consumer_id=? AND event_id=?",
                params![consumer_id, event_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .map_err(|e| error("mailbox_outbox_receipt_query_failed", &e.to_string()))?;
        if let Some((existing_fingerprint, existing_json)) = existing {
            if existing_fingerprint != receipt_fingerprint {
                let code = format!("mailbox_outbox_ack_conflict:{consumer_id}:{event_id}");
                return Err(error(&code, &code));
            }
            let existing_receipt = serde_json::from_str::<Value>(&existing_json)
                .map_err(|e| error("mailbox_outbox_receipt_invalid", &e.to_string()))?;
            return Ok(json!({
                "schema":"narada.mailbox.outbox_ack.v1",
                "consumer_id":consumer_id,
                "event_id":event_id,
                "replayed":true,
                "receipt":existing_receipt
            }));
        }
        tx.execute(
            "INSERT INTO mailbox_outbox_receipts(consumer_id,event_id,receipt_fingerprint,receipt_json,acknowledged_at) VALUES (?,?,?,?,?)",
            params![consumer_id, event_id, receipt_fingerprint, receipt_json, now],
        )
        .map_err(|e| error("mailbox_outbox_receipt_insert_failed", &e.to_string()))?;
        Ok(json!({
            "schema":"narada.mailbox.outbox_ack.v1",
            "consumer_id":consumer_id,
            "event_id":event_id,
            "replayed":false,
            "receipt":receipt
        }))
    })();
    match result {
        Ok(value) => {
            tx.commit()
                .map_err(|e| error("mailbox_domain_transaction_commit_failed", &e.to_string()))?;
            Ok(value)
        }
        Err(value) => Err(value),
    }
}

#[derive(Clone)]
struct MailboxScope {
    scope_id: String,
    root_dir: PathBuf,
    graph_mailbox_id: Option<String>,
    admission: Value,
}

struct MailFact {
    fact_id: String,
    fact_type: String,
    provenance: Value,
    payload_json: String,
    payload: Value,
    created_at: String,
}

#[derive(Clone)]
struct FirstObservationCandidate {
    mailbox_id: String,
    message_id: String,
    fact_id: String,
    conversation_id: Option<String>,
}

struct MailMetadata {
    mailbox_id: String,
    message_id: String,
    conversation_id: Option<String>,
    internet_message_id: Option<String>,
    subject: Option<String>,
}

fn load_mailbox_scope(args: &Map<String, Value>, root: &Path) -> Result<MailboxScope, Value> {
    let config_argument = args
        .get("config_path")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("config/config.json");
    if config_argument.chars().count() > 1024 {
        return Err(error("mailbox_string_argument_too_long", "mailbox_string_argument_too_long"));
    }
    let requested = args
        .get("scope_id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string);
    if requested.as_ref().is_some_and(|value| value.chars().count() > 256) {
        return Err(error("mailbox_string_argument_too_long", "mailbox_string_argument_too_long"));
    }
    let candidate = PathBuf::from(config_argument);
    let config_path = if candidate.is_absolute() { candidate } else { root.join(candidate) };
    let root_canonical = fs::canonicalize(root)
        .map_err(|e| error("mailbox_site_root_invalid", &e.to_string()))?;
    let config_canonical = fs::canonicalize(&config_path)
        .map_err(|e| error("mailbox_config_read_failed", &e.to_string()))?;
    if !config_canonical.starts_with(&root_canonical) {
        return Err(error(
            "mailbox_config_path_outside_site",
            &format!("mailbox_config_path_outside_site:{}", config_path.to_string_lossy()),
        ));
    }
    if fs::metadata(&config_canonical)
        .map_err(|e| error("mailbox_config_read_failed", &e.to_string()))?
        .len()
        > MAX_BYTES
    {
        return Err(error("mailbox_config_too_large", "mailbox_config_too_large"));
    }
    let document: Value = serde_json::from_str(
        &fs::read_to_string(&config_canonical)
            .map_err(|e| error("mailbox_config_read_failed", &e.to_string()))?,
    )
    .map_err(|e| error("mailbox_config_invalid", &e.to_string()))?;
    let scopes = document
        .get("scopes")
        .and_then(Value::as_array)
        .ok_or_else(|| error("mailbox_config_scopes_invalid", "mailbox_config_scopes_invalid"))?;
    let scope = if let Some(requested) = requested.as_deref() {
        scopes.iter().find(|scope| {
            scope.get("scope_id").and_then(Value::as_str) == Some(requested)
        })
    } else if scopes.len() == 1 {
        scopes.first()
    } else {
        None
    };
    let scope = scope.ok_or_else(|| {
        if let Some(requested) = requested.as_deref() {
            error(
                &format!("mailbox_scope_not_found:{requested}"),
                &format!("mailbox_scope_not_found:{requested}"),
            )
        } else {
            error("mailbox_scope_id_required", "mailbox_scope_id_required")
        }
    })?;
    let scope_object = scope
        .as_object()
        .ok_or_else(|| error("mailbox_scope_invalid", "mailbox_scope_invalid"))?;
    let scope_id = required_bounded(scope_object, "scope_id", "mailbox_scope_id_required", 256)?;
    let scope_root = required_bounded(scope_object, "root_dir", "mailbox_scope_root_required", 1024)?;
    let scope_root_candidate = PathBuf::from(scope_root);
    let scope_root_path = if scope_root_candidate.is_absolute() {
        scope_root_candidate
    } else {
        root.join(scope_root_candidate)
    };
    let scope_root_canonical = fs::canonicalize(&scope_root_path)
        .map_err(|e| error("mailbox_scope_root_invalid", &e.to_string()))?;
    if !scope_root_canonical.starts_with(&root_canonical) {
        return Err(error(
            "mailbox_scope_root_outside_site",
            &format!("mailbox_scope_root_outside_site:{}", scope_root_path.to_string_lossy()),
        ));
    }
    let graph = scope.get("graph").and_then(Value::as_object).or_else(|| {
        scope
            .get("sources")
            .and_then(Value::as_array)
            .and_then(|sources| {
                sources.iter().find(|source| {
                    source.get("type").and_then(Value::as_str) == Some("graph")
                })
            })
            .and_then(Value::as_object)
    });
    let graph_mailbox_id = graph.and_then(|graph| {
        graph
            .get("mailbox_id")
            .or_else(|| graph.get("user_id"))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToString::to_string)
    });
    let admission = scope
        .get("admission")
        .and_then(|value| value.get("mail"))
        .cloned()
        .unwrap_or_else(|| json!({}));
    Ok(MailboxScope {
        scope_id,
        root_dir: scope_root_canonical,
        graph_mailbox_id,
        admission,
    })
}

