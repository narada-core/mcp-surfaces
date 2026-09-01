fn draft_discard(
    policy: &Policy,
    args: &Map<String, Value>,
    root: &Path,
) -> Result<Value, Value> {
    let draft_id = required_string(args, "draft_id")?;
    let suffix = format!("messages/{}", encode_component(&draft_id));
    let property_id = "String {d700a6f2-79ad-4f44-9df7-3e9b622f09f8} Name NaradaTicketDraftOperation";
    let mut query = Map::new();
    query.insert("$select".to_string(), json!("id,isDraft,changeKey"));
    query.insert(
        "$expand".to_string(),
        json!(format!("singleValueExtendedProperties($filter=id eq '{}')", property_id.replace('\'', "''"))),
    );
    let draft = policy
        .adapter
        .request("GET", mailbox(args), &suffix, &query, None)?;
    if draft.get("isDraft").and_then(Value::as_bool) != Some(true) {
        return Err(unavailable(
            "graph_mail_draft_discard_refused_not_draft",
            "Graph object is not an unsent draft",
        ));
    }
    if draft
        .get("singleValueExtendedProperties")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .any(|property| property.get("id").and_then(Value::as_str) == Some(property_id) && optional_string(property.as_object().unwrap_or(&Map::new()), "value").is_some())
    {
        return Err(unavailable(
            "graph_ticket_draft_requires_ticket_discard_tool",
            "Ticket drafts use the transactional ticket-discard operation",
        ));
    }
    let mut headers = Map::new();
    if let Some(verifier) = draft
        .get("@odata.etag")
        .and_then(Value::as_str)
        .or_else(|| draft.get("changeKey").and_then(Value::as_str))
    {
        headers.insert("If-Match".to_string(), json!(verifier));
    }
    record_audit(
        root,
        json!({"event_kind":"draft_discard_requested","mailbox_id":mailbox_value(args),"draft_id":draft_id}),
    )?;
    let result = policy.adapter.request_with_headers(
        "DELETE",
        mailbox(args),
        &suffix,
        &Map::new(),
        None,
        &headers,
    )?;
    record_audit(
        root,
        json!({"event_kind":"draft_discard_completed","mailbox_id":mailbox_value(args),"draft_id":draft_id}),
    )?;
    Ok(json!({
        "schema":"narada.graph_mail_mcp.draft_discard.v1",
        "status":"discarded",
        "result":result
    }))
}

fn draft_send(
    policy: &Policy,
    args: &Map<String, Value>,
    root: &Path,
) -> Result<Value, Value> {
    let draft_id = required_string(args, "draft_id")?;
    if let Err(reason) = policy.draft_send_allowed(args) {
        record_audit(
            root,
            json!({"event_kind":"draft_send_refused","mailbox_id":mailbox_value(args),"draft_id":draft_id,"reason":reason}),
        )?;
        return Ok(json!({
            "schema":"narada.graph_mail_mcp.draft_send.v1",
            "status":"refused",
            "reason":reason,
            "draft_id":draft_id
        }));
    }
    let suffix = format!("messages/{}/send", encode_component(&draft_id));
    record_audit(
        root,
        json!({"event_kind":"draft_send_requested","mailbox_id":mailbox_value(args),"draft_id":draft_id}),
    )?;
    let result = policy
        .adapter
        .request("POST", mailbox(args), &suffix, &Map::new(), None)?;
    record_audit(
        root,
        json!({"event_kind":"draft_send_completed","mailbox_id":mailbox_value(args),"draft_id":draft_id}),
    )?;
    Ok(json!({
        "schema":"narada.graph_mail_mcp.draft_send.v1",
        "status":"sent",
        "result":result
    }))
}

fn attachment_message_id(args: &Map<String, Value>) -> Result<String, Value> {
    optional_string(args, "draft_id")
        .or_else(|| optional_string(args, "message_id"))
        .ok_or_else(|| invalid("message_id"))
}

fn message_patch(args: &Map<String, Value>) -> Map<String, Value> {
    let mut patch = Map::new();
    if let Some(value) = args.get("subject").and_then(Value::as_str) {
        patch.insert("subject".to_string(), json!(value));
    }
    if let Some(value) = args.get("body_text").and_then(Value::as_str) {
        patch.insert("body".to_string(), json!({"contentType":"Text","content":value}));
    }
    if let Some(value) = args.get("body_html").and_then(Value::as_str) {
        patch.insert("body".to_string(), json!({"contentType":"HTML","content":value}));
    }
    for (source, target) in [
        ("to_recipients", "toRecipients"),
        ("cc_recipients", "ccRecipients"),
        ("bcc_recipients", "bccRecipients"),
    ] {
        if let Some(value) = args.get(source).and_then(Value::as_array) {
            patch.insert(target.to_string(), Value::Array(recipients(value)));
        }
    }
    if let Some(value) = args.get("importance").and_then(Value::as_str) {
        patch.insert("importance".to_string(), json!(value));
    }
    patch
}

fn recipients(values: &[Value]) -> Vec<Value> {
    values
        .iter()
        .map(|value| {
            if let Some(address) = value.as_str() {
                json!({"emailAddress":{"address":address}})
            } else {
                value.clone()
            }
        })
        .collect()
}

fn derived_draft_body(args: &Map<String, Value>, action: &str) -> Result<Map<String, Value>, Value> {
    let message = message_patch(args);
    if args.get("comment").and_then(Value::as_str).is_some() && message.contains_key("body") {
        return Err(unavailable(
            "derived_draft_comment_body_conflict",
            "provide comment or body_text/body_html, not both",
        ));
    }
    let mut body = Map::new();
    if let Some(comment) = args.get("comment").and_then(Value::as_str) {
        body.insert("comment".to_string(), json!(comment));
    }
    let mut message = message;
    if action == "createForward" {
        if let Some(value) = args.get("to_recipients").and_then(Value::as_array) {
            message.insert("toRecipients".to_string(), Value::Array(recipients(value)));
        }
    }
    if !message.is_empty() {
        body.insert("message".to_string(), Value::Object(message));
    }
    Ok(body)
}

fn graph_reply_reference(value: &Value) -> Option<String> {
    value
        .get("inReplyTo")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .or_else(|| {
            value
                .get("inReplyTo")
                .and_then(Value::as_object)
                .and_then(|object| object.get("id"))
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(ToOwned::to_owned)
        })
}

fn required_draft_id(value: &Value) -> Result<String, Value> {
    let draft_id = value
        .get("id")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| unavailable("graph_ticket_draft_id_missing", "Graph draft id is missing"))?;
    if value.get("isDraft").and_then(Value::as_bool) == Some(false) {
        return Err(unavailable(
            "graph_ticket_draft_not_unsent",
            "Graph returned a sent message instead of a draft",
        ));
    }
    Ok(draft_id.to_string())
}

fn graph_body_as_html(value: Option<&Value>) -> Result<String, Value> {
    let Some(body) = value.and_then(Value::as_object) else {
        return Ok(String::new());
    };
    let Some(content) = body.get("content").and_then(Value::as_str) else {
        return Ok(String::new());
    };
    if body
        .get("contentType")
        .and_then(Value::as_str)
        .map(|value| value.eq_ignore_ascii_case("html"))
        == Some(true)
    {
        return Ok(content.to_string());
    }
    Ok(content
        .split(['\r', '\n'])
        .filter(|line| !line.is_empty())
        .map(|line| format!("<p>{}</p>", escape_html(line)))
        .collect::<String>())
}

fn escape_html(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

fn strip_graph_attachment_contents(value: Value) -> Value {
    match value {
        Value::Array(items) => Value::Array(
            items
                .into_iter()
                .map(strip_graph_attachment_contents)
                .collect(),
        ),
        Value::Object(object) => {
            let attachment_like = object.keys().any(|key| {
                matches!(key.to_ascii_lowercase().as_str(), "id" | "name" | "attachmenttype")
            });
            Value::Object(
                object
                    .into_iter()
                    .filter_map(|(key, value)| {
                        if attachment_like
                            && matches!(key.to_ascii_lowercase().as_str(), "contentbytes" | "content_base64" | "content" | "data" | "bytes" | "raw")
                        {
                            None
                        } else if attachment_like {
                            Some((key, value))
                        } else {
                            Some((key, strip_graph_attachment_contents(value)))
                        }
                    })
                    .collect(),
            )
        }
        other => other,
    }
}

fn strip_attachment_content(value: Value) -> Value {
    strip_graph_attachment_contents(value)
}

