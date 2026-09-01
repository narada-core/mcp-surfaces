fn merge_message_payload(existing: &Value, incoming: &Value) -> Value {
    let Some(existing) = existing.as_object() else {
        return incoming.clone();
    };
    let Some(incoming) = incoming.as_object() else {
        return incoming.clone();
    };
    if existing.get("message_id") != incoming.get("message_id") {
        return Value::Object(incoming.clone());
    }
    let mut merged = existing.clone();
    for (key, value) in incoming {
        merged.insert(key.clone(), value.clone());
    }
    for key in [
        "conversation_id",
        "internet_message_id",
        "subject",
        "from",
        "sender",
        "received_at",
        "sent_at",
        "created_at",
        "last_modified_at",
    ] {
        let incoming_has = incoming.get(key).is_some_and(has_meaningful_value);
        if !incoming_has {
            if let Some(value) = existing.get(key) {
                merged.insert(key.to_string(), value.clone());
            }
        }
    }
    for key in ["reply_to", "to", "cc", "bcc"] {
        if incoming
            .get(key)
            .and_then(Value::as_array)
            .map(|value| value.is_empty())
            .unwrap_or(true)
        {
            if let Some(value) = existing
                .get(key)
                .and_then(Value::as_array)
                .filter(|value| !value.is_empty())
            {
                merged.insert(key.to_string(), Value::Array(value.clone()));
            }
        }
    }
    if incoming
        .get("attachments")
        .and_then(Value::as_array)
        .map(|value| value.is_empty())
        .unwrap_or(true)
    {
        if let Some(value) = existing
            .get("attachments")
            .and_then(Value::as_array)
            .filter(|value| !value.is_empty())
        {
            merged.insert("attachments".to_string(), Value::Array(value.clone()));
        }
    }
    if incoming
        .get("body")
        .and_then(|value| value.get("body_kind"))
        .and_then(Value::as_str)
        == Some("empty")
    {
        if let Some(old_body) = existing.get("body").and_then(Value::as_object) {
            if old_body.get("text").is_some() || old_body.get("html").is_some() {
                let mut body = incoming
                    .get("body")
                    .and_then(Value::as_object)
                    .cloned()
                    .unwrap_or_default();
                for key in ["body_kind", "text", "html", "content_hashes"] {
                    if let Some(value) = old_body.get(key) {
                        body.insert(key.to_string(), value.clone());
                    }
                }
                merged.insert("body".to_string(), Value::Object(body));
            }
        }
    }
    if let (Some(old), Some(new)) = (
        existing
            .get("source_extensions")
            .and_then(|value| value.get("namespaces"))
            .and_then(|value| value.get("graph"))
            .and_then(Value::as_object),
        incoming
            .get("source_extensions")
            .and_then(|value| value.get("namespaces"))
            .and_then(|value| value.get("graph"))
            .and_then(Value::as_object),
    ) {
        let mut graph = old.clone();
        graph.extend(new.clone());
        merged.insert(
            "source_extensions".to_string(),
            json!({"namespaces":{"graph":Value::Object(graph)}}),
        );
    }
    Value::Object(merged)
}

fn has_meaningful_value(value: &Value) -> bool {
    match value {
        Value::String(value) => !value.trim().is_empty(),
        Value::Array(value) => !value.is_empty(),
        Value::Object(value) => !value.is_empty(),
        Value::Null => false,
        _ => true,
    }
}

fn attachment_manifest(attachments: &[Value]) -> Vec<Value> {
    attachments
        .iter()
        .map(|attachment| {
            let mut value = Map::new();
            for key in [
                "attachment_key",
                "source_attachment_id",
                "ordinal",
                "display_name",
                "content_type",
                "size_bytes",
                "inline",
                "content_id",
                "content_hash",
                "content_ref",
                "source_extensions",
            ] {
                if let Some(field) = attachment.get(key) {
                    value.insert(key.to_string(), field.clone());
                }
            }
            if attachment
                .get("content_ref")
                .and_then(Value::as_str)
                .is_some_and(|value| value.starts_with("inline-base64:"))
            {
                if let Some(key) = attachment.get("attachment_key").and_then(Value::as_str) {
                    value.insert(
                        "content_file_ref".to_string(),
                        json!(format!("attachments/by-id/{}", safe_segment(key))),
                    );
                }
            }
            Value::Object(value)
        })
        .collect()
}

fn mark_views(root: &Path, payload: &Value) -> Result<(), Value> {
    let message_id = payload
        .get("message_id")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            error(
                "mailbox_projection_message_id_missing",
                "mailbox_projection_message_id_missing",
            )
        })?;
    let message_path = root.join("messages").join(safe_segment(message_id));
    if let Some(conversation) = payload
        .get("conversation_id")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
    {
        link_view(
            &root
                .join("views/by-thread")
                .join(view_segment(conversation))
                .join("members")
                .join(view_segment(message_id)),
            &message_path,
        )?;
    }
    for folder in payload
        .get("folder_refs")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
    {
        link_view(
            &root
                .join("views/by-folder")
                .join(view_segment(folder))
                .join("members")
                .join(view_segment(message_id)),
            &message_path,
        )?;
    }
    let unread = root.join("views/unread").join(view_segment(message_id));
    if payload
        .get("flags")
        .and_then(|value| value.get("is_read"))
        .and_then(Value::as_bool)
        != Some(true)
    {
        link_view(&unread, &message_path)?;
    } else {
        unlink_view(&unread)?;
    }
    let flagged = root.join("views/flagged").join(view_segment(message_id));
    if payload
        .get("flags")
        .and_then(|value| value.get("is_flagged"))
        .and_then(Value::as_bool)
        == Some(true)
    {
        link_view(&flagged, &message_path)?;
    } else {
        unlink_view(&flagged)?;
    }
    Ok(())
}

fn link_view(path: &Path, target: &Path) -> Result<(), Value> {
    unlink_view(path)?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|e| error("mailbox_projection_view_directory_failed", &e.to_string()))?;
    }
    #[cfg(windows)]
    let result = create_windows_view_reference(target, path);
    #[cfg(unix)]
    let result = std::os::unix::fs::symlink(target, path);
    result.map_err(|e| error("mailbox_projection_view_link_failed", &e.to_string()))
}

#[cfg(windows)]
fn create_windows_view_reference(target: &Path, link: &Path) -> std::io::Result<()> {
    // Mailbox views are derived indexes, not filesystem authority. A small
    // reference directory is portable across ordinary Windows sessions and
    // avoids junction privileges and MAX_PATH-sensitive Win32 calls.
    fs::create_dir(link)?;
    let result = fs::canonicalize(target).and_then(|target| {
        fs::write(link.join(".narada-view-target"), normalized_path_text(&target))
    });
    if result.is_err() {
        let _ = fs::remove_dir_all(link);
    }
    result
}

fn view_segment(value: &str) -> String {
    let segment = safe_segment(value);
    if segment.chars().count() <= 64 {
        segment
    } else {
        format!("key_{}", &sha256_hex(value.as_bytes())[..32])
    }
}

fn unlink_view(path: &Path) -> Result<(), Value> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(value) => value,
        Err(value) if value.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(value) => {
            return Err(error(
                "mailbox_projection_view_stat_failed",
                &value.to_string(),
            ))
        }
    };
    if metadata.file_type().is_symlink() || metadata.is_file() {
        fs::remove_file(path)
    } else {
        fs::remove_dir_all(path)
    }
    .map_err(|e| error("mailbox_projection_view_remove_failed", &e.to_string()))
}

