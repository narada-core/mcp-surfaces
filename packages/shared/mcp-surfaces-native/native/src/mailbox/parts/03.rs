fn safe_fact_payload(value: &Value) -> Value {
    match value {
        Value::Array(values) => Value::Array(values.iter().map(safe_fact_payload).collect()),
        Value::Object(object) => Value::Object(
            object
                .iter()
                .map(|(key, value)| {
                    if key.eq_ignore_ascii_case("attachments") {
                        (key.clone(), metadata_only_attachment(value))
                    } else {
                        (key.clone(), safe_fact_payload(value))
                    }
                })
                .collect(),
        ),
        _ => value.clone(),
    }
}

fn fact_show(args: &Map<String, Value>, root: &Path) -> Result<Value, Value> {
    let fact_id = required_bounded(args, "fact_id", "mailbox_fact_id_required", 256)?;
    let scope = load_mailbox_scope(args, root)?;
    let path = scope.root_dir.join(".narada/facts.db");
    if !path.is_file() {
        return Ok(json!({
            "schema":"narada.mailbox.immutable_fact.v1",
            "status":"not_found",
            "fact_id":fact_id,
            "scope_id":scope.scope_id
        }));
    }
    let fact = match load_mail_fact(&scope, &fact_id) {
        Ok(fact) => fact,
        Err(value)
            if value
                .get("code")
                .and_then(Value::as_str)
                .is_some_and(|code| code.contains("fact_not_found")) =>
        {
            return Ok(json!({
                "schema":"narada.mailbox.immutable_fact.v1",
                "status":"not_found",
                "fact_id":fact_id,
                "scope_id":scope.scope_id
            }));
        }
        Err(value) => return Err(value),
    };
    if fact.fact_type != "mail.message.discovered" {
        let code = format!("mailbox_fact_type_invalid:{}", fact.fact_type);
        return Err(error(&code, &code));
    }
    let metadata = mail_metadata(&fact)?;
    if metadata.mailbox_id != scope.scope_id {
        let code = format!("mailbox_fact_scope_mismatch:{}:{}", metadata.mailbox_id, scope.scope_id);
        return Err(error(&code, &code));
    }
    let include_content = args.get("include_content").and_then(Value::as_bool) == Some(true);
    if include_content && fact.payload_json.as_bytes().len() > 750 * 1024 {
        let code = format!("mailbox_fact_content_too_large:{}", fact.payload_json.as_bytes().len());
        return Err(error(&code, &code));
    }
    Ok(json!({
        "schema":"narada.mailbox.immutable_fact.v1",
        "status":"ok",
        "scope_id":scope.scope_id,
        "projection":if include_content { "full" } else { "safe" },
        "fact":{
            "fact_id":fact.fact_id,
            "fact_type":fact.fact_type,
            "provenance":fact.provenance,
            "payload_sha256":sha256_hex(fact.payload_json.as_bytes()),
            "payload":if include_content { fact.payload } else { safe_fact_payload(&fact.payload) },
            "payload_content_included":include_content,
            "created_at":fact.created_at
        }
    }))
}
fn outbox_consumer_show(args: &Map<String, Value>, root: &Path) -> Result<Value, Value> {
    let consumer_id = required_bounded(args, "consumer_id", "mailbox_outbox_consumer_id_required", 256)?;
    let Some(db) = open_domain_db(root)? else {
        return Ok(json!({"schema":"narada.mailbox.outbox_consumer_lookup.v1","status":"not_found","consumer_id":consumer_id}));
    };
    let row: Option<(String, Option<String>, Option<String>, String, String)> = db
        .query_row(
            "SELECT consumer_id,scope_id,topics_json,start_at,created_at FROM mailbox_outbox_consumers WHERE consumer_id=?",
            params![consumer_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?)),
        )
        .optional()
        .map_err(|e| error("mailbox_outbox_consumer_query_failed", &e.to_string()))?;
    let Some((consumer_id, scope_id, topics_json, start_at, created_at)) = row else {
        return Ok(json!({"schema":"narada.mailbox.outbox_consumer_lookup.v1","status":"not_found","consumer_id":consumer_id}));
    };
    let topics = parsed_topics(topics_json.as_deref(), &consumer_id)?;
    Ok(json!({
        "schema":"narada.mailbox.outbox_consumer_lookup.v1",
        "status":"ok",
        "consumer":{
            "consumer_id":consumer_id,
            "scope_id":scope_id,
            "topics":topics,
            "start_at":start_at,
            "created_at":created_at
        }
    }))
}

fn outbox_consumer_register(args: &Map<String, Value>, root: &Path) -> Result<Value, Value> {
    let consumer_id = required_bounded(args, "consumer_id", "mailbox_outbox_consumer_id_required", 256)?;
    let scope_id = required_bounded(args, "scope_id", "mailbox_outbox_scope_id_required", 256)?;
    let topics = required_topics(args.get("topics"))?;
    let start_at = required_timestamp(args, "start_at", "mailbox_outbox_start_at_required")?;
    let topics_json = canonical_json(&Value::Array(topics.iter().cloned().map(Value::String).collect()));
    let now = now_iso_millis();
    let mut db = open_domain_db_write(root)?;
    let tx = db
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|e| error("mailbox_domain_transaction_failed", &e.to_string()))?;
    let result = (|| {
        let existing: Option<(Option<String>, Option<String>, String, String)> = tx
            .query_row(
                "SELECT scope_id,topics_json,start_at,created_at FROM mailbox_outbox_consumers WHERE consumer_id=?",
                params![consumer_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .optional()
            .map_err(|e| error("mailbox_outbox_consumer_query_failed", &e.to_string()))?;
        let created_at = if let Some((existing_scope, existing_topics, existing_start, created_at)) = existing {
            if existing_scope.is_none() && existing_topics.is_none() {
                tx.execute(
                    "UPDATE mailbox_outbox_consumers SET scope_id=?,topics_json=? WHERE consumer_id=?",
                    params![scope_id, topics_json, consumer_id],
                )
                .map_err(|e| error("mailbox_outbox_consumer_update_failed", &e.to_string()))?;
                created_at
            } else {
                if existing_scope.as_deref() != Some(scope_id.as_str())
                    || existing_topics.as_deref() != Some(topics_json.as_str())
                    || existing_start != start_at
                {
                    return Err(error(
                        &format!("mailbox_outbox_consumer_conflict:{consumer_id}"),
                        &format!("mailbox_outbox_consumer_conflict:{consumer_id}"),
                    ));
                }
                created_at
            }
        } else {
            tx.execute(
                "INSERT INTO mailbox_outbox_consumers(consumer_id,scope_id,topics_json,start_at,created_at) VALUES (?,?,?,?,?)",
                params![consumer_id, scope_id, topics_json, start_at, now],
            )
            .map_err(|e| error("mailbox_outbox_consumer_insert_failed", &e.to_string()))?;
            now.clone()
        };
        Ok(json!({
            "schema":"narada.mailbox.outbox_consumer.v2",
            "consumer":{
                "consumer_id":consumer_id,
                "scope_id":scope_id,
                "topics_json":topics_json,
                "start_at":start_at,
                "created_at":created_at
            }
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

fn outbox_list(args: &Map<String, Value>, root: &Path) -> Result<Value, Value> {
    let consumer_id = required_bounded(args, "consumer_id", "mailbox_outbox_consumer_id_required", 256)?;
    let limit = bounded_integer(args.get("limit"), 100, 1, 100)? as usize;
    let Some(db) = open_domain_db(root)? else {
        return Err(error(
            &format!("mailbox_outbox_consumer_not_registered:{consumer_id}"),
            &format!("mailbox_outbox_consumer_not_registered:{consumer_id}"),
        ));
    };
    let consumer: Option<(Option<String>, Option<String>, String)> = db
        .query_row(
            "SELECT scope_id,topics_json,start_at FROM mailbox_outbox_consumers WHERE consumer_id=?",
            params![consumer_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()
        .map_err(|e| error("mailbox_outbox_consumer_query_failed", &e.to_string()))?;
    let Some((Some(scope_id), Some(topics_json), start_at)) = consumer else {
        let code = if consumer.is_some() {
            format!("mailbox_outbox_consumer_v2_registration_required:{consumer_id}")
        } else {
            format!("mailbox_outbox_consumer_not_registered:{consumer_id}")
        };
        return Err(error(&code, &code));
    };
    let _topics = parsed_topics(Some(&topics_json), &consumer_id)?;
    let mut statement = db
        .prepare(
            "SELECT event_id,scope_id,topic,aggregate_id,aggregate_revision,schema_version,causation_id,idempotency_key,partition_key,occurred_at,payload_json FROM mailbox_outbox event WHERE event.occurred_at>=? AND event.scope_id=? AND event.topic IN (SELECT value FROM json_each(?)) AND NOT EXISTS (SELECT 1 FROM mailbox_outbox_receipts receipt WHERE receipt.consumer_id=? AND receipt.event_id=event.event_id) ORDER BY event.occurred_at,event.event_id LIMIT ?",
        )
        .map_err(|e| error("mailbox_outbox_query_failed", &e.to_string()))?;
    let rows = statement
        .query_map(
            params![start_at, scope_id, topics_json, consumer_id, limit + 1],
            |row| {
                let payload_json: String = row.get(10)?;
                let payload = serde_json::from_str::<Value>(&payload_json).unwrap_or(Value::Null);
                Ok(json!({
                    "schema":"narada.mailbox.outbox_event.v1",
                    "event_id":row.get::<_,String>(0)?,
                    "scope_id":row.get::<_,String>(1)?,
                    "topic":row.get::<_,String>(2)?,
                    "aggregate_id":row.get::<_,String>(3)?,
                    "aggregate_revision":row.get::<_,i64>(4)?,
                    "schema_version":row.get::<_,i64>(5)?,
                    "causation_id":row.get::<_,String>(6)?,
                    "idempotency_key":row.get::<_,String>(7)?,
                    "partition_key":row.get::<_,String>(8)?,
                    "occurred_at":row.get::<_,String>(9)?,
                    "payload":payload
                }))
            },
        )
        .map_err(|e| error("mailbox_outbox_query_failed", &e.to_string()))?;
    let mut items = Vec::new();
    for row in rows {
        items.push(row.map_err(|e| error("mailbox_outbox_row_failed", &e.to_string()))?);
    }
    let has_more = items.len() > limit;
    if has_more {
        items.truncate(limit);
    }
    Ok(json!({
        "schema":"narada.mailbox.outbox_list.v2",
        "consumer_id":consumer_id,
        "count":items.len(),
        "items":items,
        "has_more":has_more
    }))
}

