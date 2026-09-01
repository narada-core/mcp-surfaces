fn attachment_upload_chunk(
    policy: &Policy,
    args: &Map<String, Value>,
) -> Result<Value, Value> {
    let upload_url = required_string(args, "upload_url")?;
    let content_base64 = required_string(args, "content_base64")?;
    let range_start = required_nonnegative_number(args, "range_start")?;
    let range_end = required_nonnegative_number(args, "range_end")?;
    let total_size = required_nonnegative_number(args, "total_size")?;
    let bytes = decode_base64(&content_base64)
        .map_err(|reason| unavailable(reason, "invalid upload chunk content"))?;
    if range_end < range_start
        || total_size <= range_end
        || bytes.len() as u64 != range_end - range_start + 1
    {
        return Err(unavailable(
            "attachment_upload_content_range_invalid",
            "chunk byte count does not match Content-Range",
        ));
    }
    let mut headers = Map::new();
    headers.insert("Content-Length".to_string(), json!(bytes.len()));
    headers.insert(
        "Content-Range".to_string(),
        json!(format!("bytes {range_start}-{range_end}/{total_size}")),
    );
    headers.insert(
        "Content-Type".to_string(),
        json!("application/octet-stream"),
    );
    let (status, result) = policy
        .adapter
        .request_upload_bytes("PUT", &upload_url, &bytes, &headers)?;
    if status == 202 || status == 204 {
        return Ok(json!({
            "schema":"narada.graph_mail_mcp.attachment_upload_chunk.v1",
            "status":"accepted",
            "http_status":status
        }));
    }
    Ok(json!({
        "schema":"narada.graph_mail_mcp.attachment_upload_chunk.v1",
        "status":"ok",
        "http_status":status,
        "result":result
    }))
}

fn attachment_upload_file(
    policy: &Policy,
    args: &Map<String, Value>,
    root: &Path,
) -> Result<Value, Value> {
    let file_path = resolve_attachment_input_path(root, args, &policy.allowed_attachment_roots)?;
    let metadata = fs::metadata(&file_path)
        .map_err(|error| unavailable("attachment_file_stat_failed", &error.to_string()))?;
    let file_size = metadata.len();
    if file_size == 0 {
        return Err(unavailable("attachment_file_empty", "attachment file is empty"));
    }
    if file_size > MAX_ATTACHMENT_UPLOAD_FILE_BYTES {
        return Err(unavailable(
            "attachment_file_too_large",
            &MAX_ATTACHMENT_UPLOAD_FILE_BYTES.to_string(),
        ));
    }
    let attachment_name = optional_string(args, "name").or_else(|| {
        file_path
            .file_name()
            .and_then(|value| value.to_str())
            .map(ToOwned::to_owned)
    }).ok_or_else(|| invalid("name"))?;
    let content_type = optional_string(args, "content_type")
        .unwrap_or_else(|| infer_content_type(&attachment_name));
    let chunk_size = upload_chunk_size(args)?;
    let mut session_args = args.clone();
    session_args.insert("name".to_string(), json!(attachment_name));
    session_args.insert("size".to_string(), json!(file_size));
    session_args.insert("content_type".to_string(), json!(content_type));
    let session = attachment_upload_session_create(policy, &session_args)?;
    let upload_url = session
        .get("upload_session")
        .and_then(|value| value.get("uploadUrl"))
        .and_then(Value::as_str)
        .ok_or_else(|| unavailable("attachment_upload_session_url_missing", "uploadUrl missing"))?;
    let mut file = fs::File::open(&file_path)
        .map_err(|error| unavailable("attachment_file_open_failed", &error.to_string()))?;
    let mut buffer = vec![0u8; chunk_size as usize];
    let mut offset = 0u64;
    let mut chunk_count = 0u64;
    let mut final_result = Value::Null;
    let mut hash = Sha256::new();
    while offset < file_size {
        let remaining = file_size - offset;
        let requested = remaining.min(chunk_size) as usize;
        let mut read = 0usize;
        while read < requested {
            let count = file
                .read(&mut buffer[read..requested])
                .map_err(|error| unavailable("attachment_file_read_failed", &error.to_string()))?;
            if count == 0 {
                return Err(unavailable("attachment_file_read_failed", "unexpected end of file"));
            }
            read += count;
        }
        let bytes = &buffer[..read];
        hash.update(bytes);
        let range_end = offset + read as u64 - 1;
        let mut headers = Map::new();
        headers.insert("Content-Length".to_string(), json!(read));
        headers.insert(
            "Content-Range".to_string(),
            json!(format!("bytes {offset}-{range_end}/{file_size}")),
        );
        headers.insert(
            "Content-Type".to_string(),
            json!("application/octet-stream"),
        );
        let (status, result) = policy
            .adapter
            .request_upload_bytes("PUT", upload_url, bytes, &headers)?;
        if status != 202 && status != 204 {
            final_result = result;
        }
        offset = range_end + 1;
        chunk_count += 1;
    }
    let sha256 = hex_lower(&hash.finalize());
    record_audit(
        root,
        json!({"event_kind":"attachment_upload_file_completed","mailbox_id":mailbox_value(args),"message_id":attachment_message_id(args)?,"name":attachment_name,"size":file_size,"sha256":sha256,"chunk_count":chunk_count}),
    )?;
    Ok(json!({
        "schema":"narada.graph_mail_mcp.attachment_upload_file.v1",
        "status":"uploaded",
        "draft_id":optional_string(args, "draft_id"),
        "message_id":attachment_message_id(args)?,
        "name":attachment_name,
        "content_type":content_type,
        "size":file_size,
        "chunk_size":chunk_size,
        "chunk_count":chunk_count,
        "sha256":sha256,
        "attachment":final_result
    }))
}

fn draft_create(
    policy: &Policy,
    args: &Map<String, Value>,
    root: &Path,
) -> Result<Value, Value> {
    let message = message_patch(args);
    record_audit(
        root,
        json!({
            "event_kind":"draft_create_requested",
            "mailbox_id":mailbox_value(args),
            "subject":message.get("subject").cloned().unwrap_or(Value::Null)
        }),
    )?;
    let result = policy.adapter.request(
        "POST",
        mailbox(args),
        "messages",
        &Map::new(),
        Some(&Value::Object(message)),
    )?;
    record_audit(
        root,
        json!({
            "event_kind":"draft_create_completed",
            "mailbox_id":mailbox_value(args),
            "draft_id":result.get("id").cloned().unwrap_or(Value::Null)
        }),
    )?;
    Ok(json!({
        "schema":"narada.graph_mail_mcp.draft.v1",
        "status":"created",
        "draft":result
    }))
}

fn derived_draft_create(
    policy: &Policy,
    args: &Map<String, Value>,
    root: &Path,
    action: &str,
) -> Result<Value, Value> {
    let message_id = required_string(args, "message_id")?;
    if optional_string(args, "comment_html").is_some() {
        if action == "createForward" {
            return Err(unavailable(
                "comment_html_reply_only",
                "comment_html is supported only for reply and reply-all",
            ));
        }
        return html_reply_draft_create(policy, args, root, action);
    }
    let body = derived_draft_body(args, action)?;
    let suffix = format!(
        "messages/{}/{}",
        encode_component(&message_id),
        action
    );
    record_audit(
        root,
        json!({
            "event_kind":format!("{action}_requested"),
            "mailbox_id":mailbox_value(args),
            "message_id":message_id
        }),
    )?;
    let result = policy.adapter.request(
        "POST",
        mailbox(args),
        &suffix,
        &Map::new(),
        Some(&Value::Object(body)),
    )?;
    record_audit(
        root,
        json!({
            "event_kind":format!("{action}_completed"),
            "mailbox_id":mailbox_value(args),
            "message_id":message_id,
            "draft_id":result.get("id").cloned().unwrap_or(Value::Null)
        }),
    )?;
    Ok(json!({
        "schema":"narada.graph_mail_mcp.draft.v1",
        "status":"created",
        "draft":result
    }))
}

