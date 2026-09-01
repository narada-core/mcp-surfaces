fn load_mail_fact(scope: &MailboxScope, fact_id: &str) -> Result<MailFact, Value> {
    let path = scope.root_dir.join(".narada/facts.db");
    if !path.is_file() {
        return Err(error(
            &format!("mailbox_reconciliation_fact_db_missing:{}", path.to_string_lossy()),
            &format!("mailbox_reconciliation_fact_db_missing:{}", path.to_string_lossy()),
        ));
    }
    let db = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .map_err(|e| error("mailbox_fact_store_open_failed", &e.to_string()))?;
    let row: Option<(String, String, String, String)> = db
        .query_row(
            "SELECT fact_type,provenance_json,payload_json,created_at FROM facts WHERE fact_id=?",
            params![fact_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .optional()
        .map_err(|e| error("mailbox_fact_query_failed", &e.to_string()))?;
    let Some((fact_type, provenance_json, payload_json, created_at)) = row else {
        return Err(error(
            &format!("mailbox_reconciliation_fact_not_found:{fact_id}"),
            &format!("mailbox_reconciliation_fact_not_found:{fact_id}"),
        ));
    };
    let provenance = serde_json::from_str(&provenance_json)
        .map_err(|e| error("mailbox_fact_provenance_invalid", &e.to_string()))?;
    let payload = serde_json::from_str(&payload_json)
        .map_err(|e| error("mailbox_fact_payload_invalid", &e.to_string()))?;
    Ok(MailFact {
        fact_id: fact_id.to_string(),
        fact_type,
        provenance,
        payload_json,
        payload,
        created_at,
    })
}

fn mail_metadata(fact: &MailFact) -> Result<MailMetadata, Value> {
    let envelope = fact
        .payload
        .as_object()
        .ok_or_else(|| error("mailbox_fact_payload_invalid", "mailbox_fact_payload_invalid"))?;
    let event = envelope
        .get("event")
        .and_then(Value::as_object)
        .ok_or_else(|| error("mailbox_fact_event_invalid", "mailbox_fact_event_invalid"))?;
    let payload = event
        .get("payload")
        .and_then(Value::as_object);
    let value = |key: &str| {
        event
            .get(key)
            .or_else(|| payload.and_then(|payload| payload.get(key)))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToString::to_string)
    };
    let mailbox_id = value("mailbox_id")
        .ok_or_else(|| error("mailbox_fact_mailbox_id_missing", "mailbox_fact_mailbox_id_missing"))?;
    let message_id = value("message_id")
        .ok_or_else(|| error("mailbox_fact_message_id_missing", "mailbox_fact_message_id_missing"))?;
    Ok(MailMetadata {
        mailbox_id,
        message_id,
        conversation_id: value("conversation_id"),
        internet_message_id: payload
            .and_then(|payload| payload.get("internet_message_id"))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToString::to_string),
        subject: payload
            .and_then(|payload| payload.get("subject"))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(|value| value.chars().take(500).collect()),
    })
}

fn stable_id(prefix: &str, value: &str) -> String {
    format!("{prefix}{}", &sha256_hex(value.as_bytes())[..40])
}

