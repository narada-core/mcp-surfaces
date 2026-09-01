fn normalize_graph_event(
    scope: &ScopeConfig,
    raw: &Value,
    observed_at: &str,
) -> Result<Value, Value> {
    let message = raw.as_object().ok_or_else(|| {
        error(
            "mailbox_graph_delta_message_invalid",
            "mailbox_graph_delta_message_invalid",
        )
    })?;
    let message_id = message
        .get("id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            error(
                "mailbox_graph_message_id_missing",
                "Graph delta entry is missing id",
            )
        })?;
    let source_version = message
        .get("changeKey")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty());
    if message.get("@removed").is_some() {
        let mut identity = json!({
            "scope_id":scope.scope_id,
            "message_id":message_id,
            "event_kind":"delete",
            "source_version":source_version,
        });
        let event_id = format!("evt_{}", sha256_hex(canonical_json(&identity).as_bytes()));
        let mut event = json!({
            "schema_version":1,
            "event_id":event_id,
            "mailbox_id":scope.scope_id,
            "message_id":message_id,
        });
        insert_optional_string(&mut event, "conversation_id", message.get("conversationId"));
        event
            .as_object_mut()
            .expect("object")
            .insert("source_item_id".to_string(), json!(message_id));
        if let Some(value) = source_version {
            event
                .as_object_mut()
                .expect("object")
                .insert("source_version".to_string(), json!(value));
        } else if let Some(object) = identity.as_object_mut() {
            object.insert("source_version".to_string(), Value::Null);
        }
        event
            .as_object_mut()
            .expect("object")
            .insert("event_kind".to_string(), json!("delete"));
        event
            .as_object_mut()
            .expect("object")
            .insert("observed_at".to_string(), json!(observed_at));
        return Ok(event);
    }
    let payload = normalize_message_payload(scope, message)?;
    let mut identity = json!({
        "scope_id":scope.scope_id,
        "message_id":message_id,
        "event_kind":"upsert",
    });
    if let Some(value) = source_version {
        identity
            .as_object_mut()
            .expect("object")
            .insert("source_version".to_string(), json!(value));
    } else {
        identity.as_object_mut().expect("object").insert(
            "payload_hash".to_string(),
            json!(sha256_hex(canonical_json(&payload).as_bytes())),
        );
    }
    let event_id = format!("evt_{}", sha256_hex(canonical_json(&identity).as_bytes()));
    let mut event = json!({
        "schema_version":1,
        "event_id":event_id,
        "mailbox_id":scope.scope_id,
        "message_id":message_id,
    });
    insert_optional_string(&mut event, "conversation_id", message.get("conversationId"));
    event
        .as_object_mut()
        .expect("object")
        .insert("source_item_id".to_string(), json!(message_id));
    if let Some(value) = source_version {
        event
            .as_object_mut()
            .expect("object")
            .insert("source_version".to_string(), json!(value));
    }
    event
        .as_object_mut()
        .expect("object")
        .insert("event_kind".to_string(), json!("upsert"));
    event
        .as_object_mut()
        .expect("object")
        .insert("observed_at".to_string(), json!(observed_at));
    event
        .as_object_mut()
        .expect("object")
        .insert("payload".to_string(), payload);
    Ok(event)
}

fn normalize_message_payload(
    scope: &ScopeConfig,
    message: &Map<String, Value>,
) -> Result<Value, Value> {
    let message_id = message.get("id").and_then(Value::as_str).ok_or_else(|| {
        error(
            "mailbox_graph_message_id_missing",
            "mailbox_graph_message_id_missing",
        )
    })?;
    let mut payload = json!({
        "schema_version":1,
        "mailbox_id":scope.scope_id,
        "message_id":message_id,
    });
    insert_optional_string(
        &mut payload,
        "conversation_id",
        message.get("conversationId"),
    );
    insert_optional_string(
        &mut payload,
        "internet_message_id",
        message.get("internetMessageId"),
    );
    payload.as_object_mut().expect("object").insert(
        "subject".to_string(),
        message
            .get("subject")
            .filter(|value| value.is_string())
            .cloned()
            .unwrap_or_else(|| json!("")),
    );
    if let Some(value) = normalize_recipient(message.get("from")) {
        payload
            .as_object_mut()
            .expect("object")
            .insert("from".to_string(), value);
    }
    if let Some(value) = normalize_recipient(message.get("sender")) {
        payload
            .as_object_mut()
            .expect("object")
            .insert("sender".to_string(), value);
    }
    payload.as_object_mut().expect("object").insert(
        "reply_to".to_string(),
        normalize_recipients(message.get("replyTo")),
    );
    payload.as_object_mut().expect("object").insert(
        "to".to_string(),
        normalize_recipients(message.get("toRecipients")),
    );
    payload.as_object_mut().expect("object").insert(
        "cc".to_string(),
        normalize_recipients(message.get("ccRecipients")),
    );
    payload.as_object_mut().expect("object").insert(
        "bcc".to_string(),
        normalize_recipients(message.get("bccRecipients")),
    );
    for (target, source) in [
        ("sent_at", "sentDateTime"),
        ("received_at", "receivedDateTime"),
        ("created_at", "createdDateTime"),
        ("last_modified_at", "lastModifiedDateTime"),
    ] {
        insert_optional_string(&mut payload, target, message.get(source));
    }
    let folders = message
        .get("parentFolderId")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| vec![json!(value)])
        .unwrap_or_default();
    payload
        .as_object_mut()
        .expect("object")
        .insert("folder_refs".to_string(), Value::Array(folders));
    let mut categories = message
        .get("categories")
        .and_then(Value::as_array)
        .map(|values| {
            values
                .iter()
                .filter_map(Value::as_str)
                .map(ToString::to_string)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    categories.sort();
    payload
        .as_object_mut()
        .expect("object")
        .insert("category_refs".to_string(), json!(categories));
    let importance = message
        .get("importance")
        .and_then(Value::as_str)
        .filter(|value| matches!(*value, "low" | "normal" | "high"));
    let flagged = message
        .get("flag")
        .and_then(Value::as_object)
        .and_then(|flag| flag.get("flagStatus"))
        .and_then(Value::as_str)
        .is_some_and(|value| matches!(value, "flagged" | "complete"));
    let mut flags = json!({
        "is_read":message.get("isRead").and_then(Value::as_bool).unwrap_or(false),
        "is_draft":message.get("isDraft").and_then(Value::as_bool).unwrap_or(false),
        "is_flagged":flagged,
        "has_attachments":message.get("hasAttachments").and_then(Value::as_bool).unwrap_or(false),
    });
    if let Some(value) = importance {
        flags
            .as_object_mut()
            .expect("object")
            .insert("importance".to_string(), json!(value));
    }
    payload
        .as_object_mut()
        .expect("object")
        .insert("flags".to_string(), flags);
    if scope.include_headers {
        if let Some(headers) = normalize_headers(message.get("internetMessageHeaders")) {
            payload
                .as_object_mut()
                .expect("object")
                .insert("headers".to_string(), headers);
        }
    }
    payload.as_object_mut().expect("object").insert(
        "body".to_string(),
        normalize_body(
            message.get("body"),
            &scope.body_policy,
            message.get("bodyPreview"),
        ),
    );
    payload.as_object_mut().expect("object").insert(
        "attachments".to_string(),
        normalize_attachments(message.get("attachments"), &scope.attachment_policy)?,
    );
    if let Some(extensions) = graph_message_extensions(message) {
        payload
            .as_object_mut()
            .expect("object")
            .insert("source_extensions".to_string(), extensions);
    }
    Ok(payload)
}

