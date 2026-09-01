fn attachment_download_file(
    policy: &Policy,
    args: &Map<String, Value>,
    root: &Path,
) -> Result<Value, Value> {
    let message_id = attachment_message_id(args)?;
    let attachment_id = required_string(args, "attachment_id")?;
    let destination = resolve_attachment_output_path(
        root,
        args,
        &policy.allowed_attachment_roots,
    )?;
    let suffix = format!(
        "messages/{}/attachments/{}",
        encode_component(&message_id),
        encode_component(&attachment_id)
    );
    let graph = policy
        .adapter
        .request("GET", mailbox(args), &suffix, &Map::new(), None)?;
    let name = graph
        .get("name")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .unwrap_or(&attachment_id)
        .to_string();
    let content_type = graph
        .get("contentType")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| infer_content_type(&name));
    if !ALLOWED_DOWNLOADED_ATTACHMENT_TYPES
        .iter()
        .any(|value| value.eq_ignore_ascii_case(&content_type))
    {
        return Err(unavailable(
            "attachment_download_content_type_not_allowed",
            &content_type,
        ));
    }
    let content_base64 = graph
        .get("contentBytes")
        .and_then(Value::as_str)
        .or_else(|| graph.get("content_base64").and_then(Value::as_str))
        .ok_or_else(|| unavailable("attachment_download_content_missing", "contentBytes missing"))?;
    let bytes = decode_base64(content_base64).map_err(|reason| unavailable(&reason, "invalid attachment content"))?;
    if bytes.is_empty() {
        return Err(unavailable(
            "attachment_download_content_empty",
            "attachment content is empty",
        ));
    }
    if bytes.len() > MAX_DOWNLOADED_ATTACHMENT_BYTES {
        return Err(unavailable(
            "attachment_download_too_large",
            &bytes.len().to_string(),
        ));
    }
    if let Some(size) = graph.get("size").and_then(Value::as_u64) {
        if size != bytes.len() as u64 {
            return Err(unavailable(
                "attachment_download_size_mismatch",
                &format!("{size}:{}", bytes.len()),
            ));
        }
    }
    let digest = hex_lower(&Sha256::digest(&bytes));
    if destination.exists() {
        let existing = fs::read(&destination)
            .map_err(|error| unavailable("attachment_download_read_failed", &error.to_string()))?;
        if hex_lower(&Sha256::digest(&existing)) != digest {
            return Err(unavailable(
                "attachment_download_destination_conflict",
                "existing destination has a different digest",
            ));
        }
        return Ok(json!({
            "schema":"narada.graph_mail_mcp.attachment_download_file.v1",
            "status":"already_materialized",
            "message_id":message_id,
            "attachment_id":attachment_id,
            "file_path":display_path(&destination),
            "name":name,
            "content_type":content_type,
            "size":bytes.len(),
            "sha256":digest
        }));
    }
    if let Some(parent) = destination.parent() {
        fs::create_dir_all(parent)
            .map_err(|error| unavailable("attachment_download_directory_failed", &error.to_string()))?;
    }
    let temporary = PathBuf::from(format!("{}.{}.tmp", destination.display(), std::process::id()));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary)
        .map_err(|error| unavailable("attachment_download_write_failed", &error.to_string()))?;
    if let Err(error) = file.write_all(&bytes) {
        let _ = fs::remove_file(&temporary);
        return Err(unavailable("attachment_download_write_failed", &error.to_string()));
    }
    drop(file);
    if let Err(error) = fs::rename(&temporary, &destination) {
        let _ = fs::remove_file(&temporary);
        return Err(unavailable("attachment_download_materialize_failed", &error.to_string()));
    }
    record_audit(
        root,
        json!({
            "event_kind":"attachment_download_file_completed",
            "mailbox_id":mailbox_value(args),
            "message_id":message_id,
            "attachment_id":attachment_id,
            "name":name,
            "content_type":content_type,
            "size":bytes.len(),
            "sha256":digest
        }),
    )?;
    Ok(json!({
        "schema":"narada.graph_mail_mcp.attachment_download_file.v1",
        "status":"materialized",
        "message_id":message_id,
        "attachment_id":attachment_id,
        "file_path":display_path(&destination),
        "name":name,
        "content_type":content_type,
        "size":bytes.len(),
        "sha256":digest
    }))
}

fn attachment_add(policy: &Policy, args: &Map<String, Value>) -> Result<Value, Value> {
    let message_id = attachment_message_id(args)?;
    let content_base64 = required_string(args, "content_base64")?;
    if !valid_base64(&content_base64) {
        return Err(invalid("content_base64"));
    }
    if base64_decoded_size(&content_base64) > 3 * 1024 * 1024 {
        return Err(unavailable(
            "attachment_small_file_too_large",
            "small attachment limit is 3 MiB",
        ));
    }
    let name = required_string(args, "name")?;
    let content_type = required_string(args, "content_type")?;
    let mut body = Map::new();
    body.insert(
        "@odata.type".to_string(),
        json!("#microsoft.graph.fileAttachment"),
    );
    body.insert("name".to_string(), json!(name));
    body.insert("contentType".to_string(), json!(content_type));
    body.insert("contentBytes".to_string(), json!(content_base64));
    if let Some(value) = args.get("is_inline").and_then(Value::as_bool) {
        body.insert("isInline".to_string(), json!(value));
    }
    if let Some(value) = optional_string(args, "content_id") {
        body.insert("contentId".to_string(), json!(value));
    }
    let suffix = format!(
        "messages/{}/attachments",
        encode_component(&message_id)
    );
    let result = policy.adapter.request(
        "POST",
        mailbox(args),
        &suffix,
        &Map::new(),
        Some(&Value::Object(body)),
    )?;
    Ok(json!({
        "schema":"narada.graph_mail_mcp.attachment.v1",
        "status":"created",
        "attachment":result
    }))
}

fn attachment_delete(policy: &Policy, args: &Map<String, Value>) -> Result<Value, Value> {
    let message_id = attachment_message_id(args)?;
    let attachment_id = required_string(args, "attachment_id")?;
    let suffix = format!(
        "messages/{}/attachments/{}",
        encode_component(&message_id),
        encode_component(&attachment_id)
    );
    let result = policy
        .adapter
        .request("DELETE", mailbox(args), &suffix, &Map::new(), None)?;
    Ok(json!({
        "schema":"narada.graph_mail_mcp.attachment_delete.v1",
        "status":"deleted",
        "result":result
    }))
}

fn attachment_upload_session_create(
    policy: &Policy,
    args: &Map<String, Value>,
) -> Result<Value, Value> {
    let message_id = attachment_message_id(args)?;
    let name = required_string(args, "name")?;
    let size = required_positive_number(args, "size")?;
    let mut attachment_item = Map::new();
    attachment_item.insert("attachmentType".to_string(), json!("file"));
    attachment_item.insert("name".to_string(), json!(name));
    attachment_item.insert("size".to_string(), json!(size));
    if let Some(value) = optional_string(args, "content_type") {
        attachment_item.insert("contentType".to_string(), json!(value));
    }
    if let Some(value) = args.get("is_inline").and_then(Value::as_bool) {
        attachment_item.insert("isInline".to_string(), json!(value));
    }
    if let Some(value) = optional_string(args, "content_id") {
        attachment_item.insert("contentId".to_string(), json!(value));
    }
    let body = json!({"AttachmentItem":Value::Object(attachment_item)});
    let suffix = format!(
        "messages/{}/attachments/createUploadSession",
        encode_component(&message_id)
    );
    let result = policy.adapter.request(
        "POST",
        mailbox(args),
        &suffix,
        &Map::new(),
        Some(&body),
    )?;
    Ok(json!({
        "schema":"narada.graph_mail_mcp.attachment_upload_session.v1",
        "status":"created",
        "upload_session":result
    }))
}

