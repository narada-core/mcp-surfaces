fn html_reply_draft_create(
    policy: &Policy,
    args: &Map<String, Value>,
    root: &Path,
    action: &str,
) -> Result<Value, Value> {
    let message_id = required_string(args, "message_id")?;
    let comment_html = required_string(args, "comment_html")?;
    if args.get("comment").and_then(Value::as_str).is_some()
        || args.get("body_text").and_then(Value::as_str).is_some()
        || args.get("body_html").and_then(Value::as_str).is_some()
    {
        return Err(unavailable(
            "comment_html_body_conflict",
            "provide comment_html alone",
        ));
    }
    let create_suffix = format!("messages/{}/{}", encode_component(&message_id), action);
    record_audit(
        root,
        json!({"event_kind":format!("{action}_html_requested"),"mailbox_id":mailbox_value(args),"message_id":message_id}),
    )?;
    let created = policy.adapter.request(
        "POST",
        mailbox(args),
        &create_suffix,
        &Map::new(),
        Some(&json!({})),
    )?;
    let draft_id = required_draft_id(&created)?;
    let draft_suffix = format!("messages/{}", encode_component(&draft_id));
    let observed = policy
        .adapter
        .request("GET", mailbox(args), &draft_suffix, &Map::new(), None)?;
    let quote_html = graph_body_as_html(observed.get("body").or_else(|| created.get("body")))?;
    if quote_html.trim().is_empty() {
        return Err(unavailable(
            "graph_reply_html_quote_missing",
            "Graph did not return quoted history",
        ));
    }
    let composed_html = compose_reply_html(
        &comment_html,
        &quote_html,
        policy.reply_signature_name.as_deref(),
    );
    let patched = policy.adapter.request(
        "PATCH",
        mailbox(args),
        &draft_suffix,
        &Map::new(),
        Some(&json!({"body":{"contentType":"HTML","content":composed_html}})),
    )?;
    if patched.get("isDraft").and_then(Value::as_bool) == Some(false) {
        return Err(unavailable(
            "graph_reply_html_draft_not_unsent",
            "Graph returned a sent message",
        ));
    }
    record_audit(
        root,
        json!({"event_kind":format!("{action}_html_completed"),"mailbox_id":mailbox_value(args),"message_id":message_id,"draft_id":draft_id,"quote_preserved":true}),
    )?;
    Ok(json!({
        "schema":"narada.graph_mail_mcp.draft.v1",
        "status":"created",
        "draft":patched,
        "reply_body_mode":"comment_html",
        "reply_signature_name":policy.reply_signature_name,
        "signature_applied":policy.reply_signature_name.is_some(),
        "quote_preserved":true,
        "unsent":patched.get("isDraft").and_then(Value::as_bool) != Some(false)
    }))
}

fn compose_reply_html(comment_html: &str, quote_html: &str, signature_name: Option<&str>) -> String {
    let signature_html = signature_name
        .map(|name| format!("<p>Thanks,<br>{}</p>", escape_html(name)))
        .unwrap_or_default();
    format!(
        "{}{}<div data-narada-quoted-history=\"true\">{}</div>",
        comment_html, signature_html, quote_html
    )
}

fn reply_all_to_last_in_thread(
    policy: &Policy,
    args: &Map<String, Value>,
    root: &Path,
) -> Result<Value, Value> {
    let conversation_id = required_string(args, "conversation_id")?;
    let filter = format!(
        "conversationId eq '{}'",
        conversation_id.replace('\'', "''")
    );
    let mut query = Map::new();
    query.insert("$filter".to_string(), json!(filter));
    query.insert(
        "$orderby".to_string(),
        json!("receivedDateTime desc"),
    );
    query.insert("$top".to_string(), json!(1));
    query.insert(
        "$select".to_string(),
        json!("id,conversationId,receivedDateTime"),
    );
    let messages = policy
        .adapter
        .request("GET", mailbox(args), "messages", &query, None)?;
    let last_message = messages
        .get("value")
        .and_then(Value::as_array)
        .and_then(|items| items.first())
        .ok_or_else(|| unavailable("graph_mail_thread_no_messages", "conversation has no messages"))?;
    let message_id = last_message
        .get("id")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| unavailable("graph_mail_thread_last_message_missing_id", "last message has no id"))?
        .to_string();
    let body = derived_draft_body(args, "createReplyAll")?;
    let suffix = format!(
        "messages/{}/createReplyAll",
        encode_component(&message_id)
    );
    record_audit(
        root,
        json!({"event_kind":"createReplyAll_to_last_in_thread_requested","mailbox_id":mailbox_value(args),"conversation_id":conversation_id,"message_id":message_id}),
    )?;
    let draft = policy.adapter.request(
        "POST",
        mailbox(args),
        &suffix,
        &Map::new(),
        Some(&Value::Object(body)),
    )?;
    record_audit(
        root,
        json!({"event_kind":"createReplyAll_to_last_in_thread_completed","mailbox_id":mailbox_value(args),"conversation_id":conversation_id,"message_id":message_id,"draft_id":draft.get("id").cloned().unwrap_or(Value::Null)}),
    )?;
    Ok(json!({
        "schema":"narada.graph_mail_mcp.draft.v1",
        "status":"created",
        "source_message_id":message_id,
        "draft":draft
    }))
}

