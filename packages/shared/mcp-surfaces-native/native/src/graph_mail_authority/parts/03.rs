fn read_auth_response(response: ureq::Response) -> Result<(u16, Value), Value> {
    let status = response.status();
    let mut reader = response.into_reader().take(MAX_AUTH_RESPONSE_BYTES + 1);
    let mut bytes = Vec::new();
    reader
        .read_to_end(&mut bytes)
        .map_err(|error| unavailable("graph_auth_response_read_failed", &error.to_string()))?;
    if bytes.len() as u64 > MAX_AUTH_RESPONSE_BYTES {
        return Err(unavailable("graph_auth_response_too_large", &MAX_AUTH_RESPONSE_BYTES.to_string()));
    }
    let text = String::from_utf8_lossy(&bytes);
    let payload = serde_json::from_str::<Value>(&text).unwrap_or_else(|_| json!({}));
    Ok((status, payload))
}

fn required_value_string(value: &Value, key: &str) -> Result<String, Value> {
    value
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .ok_or_else(|| invalid(key))
}

fn flow_path(root: &Path, flow_id: &str) -> PathBuf {
    let safe = flow_id
        .chars()
        .map(|value| if value.is_ascii_alphanumeric() || matches!(value, '-' | '_' | '.') { value } else { '_' })
        .collect::<String>();
    root.join(format!(".ai/runtime/graph-mail-mcp/device-code-flows/{safe}.json"))
}

fn read_flow(root: &Path, flow_id: &str) -> Result<Option<Value>, Value> {
    let path = flow_path(root, flow_id);
    if !path.exists() {
        return Ok(None);
    }
    let metadata = fs::metadata(&path)
        .map_err(|error| unavailable("device_code_flow_read_failed", &error.to_string()))?;
    if metadata.len() > MAX_FLOW_BYTES {
        return Err(unavailable("device_code_flow_too_large", &MAX_FLOW_BYTES.to_string()));
    }
    let text = fs::read_to_string(&path)
        .map_err(|error| unavailable("device_code_flow_read_failed", &error.to_string()))?;
    serde_json::from_str::<Value>(&text)
        .map(Some)
        .map_err(|error| unavailable("device_code_flow_invalid", &error.to_string()))
}

fn write_bounded_json(path: &Path, value: &Value) -> Result<(), Value> {
    let text = serde_json::to_string_pretty(value)
        .map_err(|error| unavailable("graph_auth_state_encode_failed", &error.to_string()))?;
    if text.len() as u64 > MAX_FLOW_BYTES {
        return Err(unavailable("graph_auth_state_too_large", &MAX_FLOW_BYTES.to_string()));
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|error| unavailable("graph_auth_state_directory_failed", &error.to_string()))?;
    }
    fs::write(path, text)
        .map_err(|error| unavailable("graph_auth_state_write_failed", &error.to_string()))
}

fn now_millis() -> i64 {
    (OffsetDateTime::now_utc().unix_timestamp_nanos() / 1_000_000) as i64
}

fn seconds_millis(seconds: u64) -> i64 {
    seconds
        .min(i64::MAX as u64 / 1_000)
        .saturating_mul(1_000) as i64
}

fn now_rfc3339() -> String {
    OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .unwrap_or_else(|_| "unknown".to_string())
}

fn delegated_token_path(root: &Path) -> PathBuf {
    root.join(".ai/runtime/graph-mail-mcp/delegated-token.json")
}

fn delegated_token_summary(root: &Path) -> Value {
    let path = delegated_token_path(root);
    let Ok(metadata) = fs::metadata(&path) else {
        return json!({"status":"missing","fresh":false});
    };
    if metadata.len() > MAX_CONFIG_BYTES {
        return json!({"status":"invalid","fresh":false});
    }
    let Ok(text) = fs::read_to_string(&path) else {
        return json!({"status":"invalid","fresh":false});
    };
    let Ok(value) = serde_json::from_str::<Value>(&text) else {
        return json!({"status":"invalid","fresh":false});
    };
    let Some(object) = value.as_object() else {
        return json!({"status":"invalid","fresh":false});
    };
    if object.get("schema").and_then(Value::as_str) != Some("narada.graph_mail_mcp.delegated_token.v1") {
        return json!({"status":"invalid","fresh":false});
    }
    let expires_at_ms = object.get("expires_at_ms").and_then(Value::as_i64).unwrap_or(0);
    let now_ms = (OffsetDateTime::now_utc().unix_timestamp_nanos() / 1_000_000) as i64;
    let fresh = expires_at_ms > now_ms + 60_000;
    let refreshable = object.get("refresh_token").and_then(Value::as_str).is_some();
    json!({
        "status":if fresh { "available" } else if refreshable { "refreshable" } else { "expired" },
        "fresh":fresh,
        "refreshable":refreshable,
        "auth_mode":object.get("auth_mode").cloned().unwrap_or(Value::Null),
        "tenant_id":object.get("tenant_id").cloned().unwrap_or(Value::Null),
        "client_id":object.get("client_id").cloned().unwrap_or(Value::Null),
        "scope":object.get("scope").cloned().unwrap_or(Value::Null),
        "acquired_at":object.get("acquired_at").cloned().unwrap_or(Value::Null),
        "expires_at_ms":expires_at_ms
    })
}

fn query(policy: &Policy, args: &Map<String, Value>) -> Result<Value, Value> {
    let folder_id = optional_string(args, "folder_id");
    let suffix = folder_id
        .as_deref()
        .map(|id| format!("mailFolders/{}/messages", encode_component(id)))
        .unwrap_or_else(|| "messages".to_string());
    let mut query = Map::new();
    query.insert("$top".to_string(), json!(bounded_top(args.get("limit"), DEFAULT_QUERY_TOP)));
    for key in ["$select", "$filter", "$orderby"] {
        let source = key.trim_start_matches('$');
        if let Some(value) = optional_string(args, source) {
            query.insert(key.to_string(), Value::String(value));
        }
    }
    if let Some(value) = optional_string(args, "query") {
        query.insert("$search".to_string(), Value::String(format!("\"{}\"", value.replace('"', "\\\""))));
    } else if !query.contains_key("$orderby") {
        query.insert("$orderby".to_string(), Value::String("receivedDateTime desc".to_string()));
    }
    let url = policy.adapter.build_url(mailbox(args), &suffix, &query)?;
    let result = policy.adapter.request("GET", mailbox(args), &suffix, &query, None)?;
    Ok(json!({"schema":"narada.graph_mail_mcp.query.v1","status":"ok","request_url":url,"result":result}))
}

fn message_show(policy: &Policy, args: &Map<String, Value>) -> Result<Value, Value> {
    let id = required_string(args, "message_id")?;
    let suffix = format!("messages/{}", encode_component(&id));
    let mut query = Map::new();
    if let Some(select) = optional_string(args, "select") {
        query.insert("$select".to_string(), Value::String(select));
    }
    let result = policy.adapter.request("GET", mailbox(args), &suffix, &query, None)?;
    Ok(json!({"schema":"narada.graph_mail_mcp.message.v1","status":"ok","message":result}))
}

fn folder_list(policy: &Policy, args: &Map<String, Value>) -> Result<Value, Value> {
    let parent = optional_string(args, "parent_folder_id");
    let suffix = parent
        .as_deref()
        .map(|id| format!("mailFolders/{}/childFolders", encode_component(id)))
        .unwrap_or_else(|| "mailFolders".to_string());
    let mut query = Map::new();
    query.insert("$top".to_string(), json!(bounded_top(args.get("limit"), DEFAULT_FOLDER_TOP)));
    if let Some(select) = optional_string(args, "select") {
        query.insert("$select".to_string(), Value::String(select));
    }
    let url = policy.adapter.build_url(mailbox(args), &suffix, &query)?;
    let result = policy.adapter.request("GET", mailbox(args), &suffix, &query, None)?;
    Ok(json!({"schema":"narada.graph_mail_mcp.folders.v1","status":"ok","request_url":url,"folders":result}))
}

fn folder_create(policy: &Policy, args: &Map<String, Value>, root: &Path) -> Result<Value, Value> {
    let display_name = required_string(args, "display_name")?;
    if let Err(reason) = policy.organization_write_allowed(args, "folder_create") {
        return refused(root, "folder_create_refused", reason, json!({"display_name":display_name}));
    }
    let parent = optional_string(args, "parent_folder_id");
    let suffix = parent
        .as_deref()
        .map(|id| format!("mailFolders/{}/childFolders", encode_component(id)))
        .unwrap_or_else(|| "mailFolders".to_string());
    record_audit(root, json!({"event_kind":"folder_create_requested","mailbox_id":mailbox_value(args),"display_name":display_name}))?;
    let result = policy.adapter.request("POST", mailbox(args), &suffix, &Map::new(), Some(&json!({"displayName":display_name})))?;
    record_audit(root, json!({"event_kind":"folder_create_completed","mailbox_id":mailbox_value(args),"folder_id":result.get("id").cloned().unwrap_or(Value::Null)}))?;
    Ok(json!({"schema":"narada.graph_mail_mcp.folder.v1","status":"created","folder":result}))
}

fn message_move(policy: &Policy, args: &Map<String, Value>, root: &Path) -> Result<Value, Value> {
    let id = required_string(args, "message_id")?;
    let destination = required_string(args, "destination_folder_id")?;
    if let Err(reason) = policy.organization_write_allowed(args, "message_move") {
        return refused(root, "message_move_refused", reason, json!({"message_id":id,"destination_folder_id":destination}));
    }
    let suffix = format!("messages/{}/move", encode_component(&id));
    record_audit(root, json!({"event_kind":"message_move_requested","mailbox_id":mailbox_value(args),"message_id":id,"destination_folder_id":destination}))?;
    let result = policy.adapter.request("POST", mailbox(args), &suffix, &Map::new(), Some(&json!({"destinationId":destination})))?;
    record_audit(root, json!({"event_kind":"message_move_completed","mailbox_id":mailbox_value(args),"message_id":id,"destination_folder_id":destination}))?;
    Ok(json!({"schema":"narada.graph_mail_mcp.message_move.v1","status":"moved","message":result}))
}

fn mark_read(policy: &Policy, args: &Map<String, Value>, root: &Path) -> Result<Value, Value> {
    let id = required_string(args, "message_id")?;
    let idempotency = required_string(args, "idempotency_key")?;
    let digest = Sha256::digest(idempotency.as_bytes());
    let digest_hex = hex_lower(&digest);
    let operation_ref = format!("graph-mail-mark-read:{}", &digest_hex[..32]);
    if let Err(reason) = policy.organization_write_allowed(args, "message_mark_read") {
        record_audit(root, json!({"event_kind":"message_mark_read_refused","mailbox_id":mailbox_value(args),"message_id":id,"reason":reason}))?;
        return Ok(json!({"schema":"narada.domain_operation.v1","operation_ref":operation_ref,"outcome":"failed","error_message":reason,"result":{"schema":"narada.graph_mail_mcp.message_mark_read.v1","status":"refused","reason":reason,"message_id":id}}));
    }
    let suffix = format!("messages/{}", encode_component(&id));
    record_audit(root, json!({"event_kind":"message_mark_read_requested","mailbox_id":mailbox_value(args),"message_id":id}))?;
    let _ = policy.adapter.request("PATCH", mailbox(args), &suffix, &Map::new(), Some(&json!({"isRead":true})))?;
    record_audit(root, json!({"event_kind":"message_mark_read_completed","mailbox_id":mailbox_value(args),"message_id":id}))?;
    Ok(json!({"schema":"narada.domain_operation.v1","operation_ref":operation_ref,"outcome":"completed","result":{"schema":"narada.graph_mail_mcp.message_mark_read.v1","status":"marked_read","message_id":id}}))
}

fn attachment_list(policy: &Policy, args: &Map<String, Value>) -> Result<Value, Value> {
    let message_id = attachment_message_id(args)?;
    let suffix = format!(
        "messages/{}/attachments",
        encode_component(&message_id)
    );
    let mut query = Map::new();
    query.insert(
        "$top".to_string(),
        json!(bounded_top(args.get("top").or_else(|| args.get("limit")), 20)),
    );
    let result = policy
        .adapter
        .request("GET", mailbox(args), &suffix, &query, None)?;
    Ok(json!({
        "schema":"narada.graph_mail_mcp.attachments.v1",
        "status":"ok",
        "attachments":strip_graph_attachment_contents(result)
    }))
}

fn attachment_get(policy: &Policy, args: &Map<String, Value>) -> Result<Value, Value> {
    let message_id = attachment_message_id(args)?;
    let attachment_id = required_string(args, "attachment_id")?;
    let suffix = format!(
        "messages/{}/attachments/{}",
        encode_component(&message_id),
        encode_component(&attachment_id)
    );
    let result = policy
        .adapter
        .request("GET", mailbox(args), &suffix, &Map::new(), None)?;
    let attachment = if args
        .get("include_content")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        result
    } else {
        strip_attachment_content(result)
    };
    Ok(json!({
        "schema":"narada.graph_mail_mcp.attachment.v1",
        "status":"ok",
        "attachment":attachment
    }))
}

