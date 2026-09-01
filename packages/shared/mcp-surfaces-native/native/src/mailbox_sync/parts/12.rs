fn install_blobs(root: &Path, payload: &Value) -> Result<(), Value> {
    for attachment in payload
        .get("attachments")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let Some(content) = attachment
            .get("content_ref")
            .and_then(Value::as_str)
            .and_then(|value| value.strip_prefix("inline-base64:"))
        else {
            continue;
        };
        let bytes = decode_base64(content)?;
        let hash = sha256_hex(&bytes);
        let destination = root
            .join("blobs/sha256")
            .join(&hash[..2])
            .join(&hash[2..4])
            .join(&hash);
        if destination.is_file() {
            continue;
        }
        if let Some(parent) = destination.parent() {
            fs::create_dir_all(parent)
                .map_err(|e| error("mailbox_blob_directory_failed", &e.to_string()))?;
        }
        let temporary = root
            .join("tmp")
            .join(format!("blob.{hash}.{}.tmp", Uuid::new_v4()));
        fs::write(&temporary, &bytes)
            .map_err(|e| error("mailbox_blob_write_failed", &e.to_string()))?;
        match fs::rename(&temporary, &destination) {
            Ok(()) => {}
            Err(_) if destination.is_file() => {
                let _ = fs::remove_file(&temporary);
            }
            Err(value) => {
                let _ = fs::remove_file(&temporary);
                return Err(error("mailbox_blob_install_failed", &value.to_string()));
            }
        }
    }
    Ok(())
}

fn write_message_projection(root: &Path, payload: &Value) -> Result<(), Value> {
    let message_id = payload
        .get("message_id")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            error(
                "mailbox_projection_message_id_missing",
                "mailbox_projection_message_id_missing",
            )
        })?;
    let destination = root.join("messages").join(safe_segment(message_id));
    let existing = if destination.join("record.json").is_file() {
        let text = fs::read_to_string(destination.join("record.json"))
            .map_err(|e| error("mailbox_projection_message_read_failed", &e.to_string()))?;
        serde_json::from_str::<Value>(&text)
            .map_err(|e| error("mailbox_projection_message_invalid", &e.to_string()))?
    } else {
        Value::Null
    };
    let merged = merge_message_payload(&existing, payload);
    let nonce = format!("{}.{}", std::process::id(), Uuid::new_v4());
    let staging = root
        .join("tmp")
        .join(format!("message.{}.{nonce}", safe_segment(message_id)));
    let prior = destination.with_extension(format!("prior.{nonce}"));
    for relative in ["body", "attachments/by-id", "attachments/by-name"] {
        fs::create_dir_all(staging.join(relative)).map_err(|e| {
            error(
                "mailbox_projection_message_directory_failed",
                &e.to_string(),
            )
        })?;
    }
    if let Some(text) = merged
        .get("body")
        .and_then(|value| value.get("text"))
        .and_then(Value::as_str)
    {
        fs::write(staging.join("body/body.txt"), text)
            .map_err(|e| error("mailbox_projection_message_write_failed", &e.to_string()))?;
    }
    if let Some(html) = merged
        .get("body")
        .and_then(|value| value.get("html"))
        .and_then(Value::as_str)
    {
        fs::write(staging.join("body/body.html"), html)
            .map_err(|e| error("mailbox_projection_message_write_failed", &e.to_string()))?;
    }
    let attachments = merged
        .get("attachments")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let manifest = attachment_manifest(&attachments);
    atomic_write_json_pretty(
        &staging.join("attachments/manifest.json"),
        &Value::Array(manifest),
    )?;
    for attachment in &attachments {
        let Some(encoded) = attachment
            .get("content_ref")
            .and_then(Value::as_str)
            .and_then(|value| value.strip_prefix("inline-base64:"))
        else {
            continue;
        };
        let bytes = decode_base64(encoded)?;
        let key = attachment
            .get("attachment_key")
            .and_then(Value::as_str)
            .unwrap_or("attachment");
        let name = attachment
            .get("display_name")
            .and_then(Value::as_str)
            .filter(|value| !value.trim().is_empty())
            .unwrap_or(key);
        fs::write(
            staging.join("attachments/by-id").join(safe_segment(key)),
            &bytes,
        )
        .map_err(|e| error("mailbox_projection_attachment_write_failed", &e.to_string()))?;
        fs::write(
            staging.join("attachments/by-name").join(safe_segment(name)),
            &bytes,
        )
        .map_err(|e| error("mailbox_projection_attachment_write_failed", &e.to_string()))?;
    }
    let mut record = merged.as_object().cloned().unwrap_or_default();
    let mut body_refs = Map::new();
    if merged
        .get("body")
        .and_then(|value| value.get("text"))
        .is_some()
    {
        body_refs.insert("text".to_string(), json!("body/body.txt"));
    }
    if merged
        .get("body")
        .and_then(|value| value.get("html"))
        .is_some()
    {
        body_refs.insert("html".to_string(), json!("body/body.html"));
    }
    record.insert("body_refs".to_string(), Value::Object(body_refs));
    record.insert(
        "attachment_manifest_ref".to_string(),
        json!("attachments/manifest.json"),
    );
    record.insert("_checksum".to_string(), json!(""));
    let checksum = &sha256_hex(
        serde_json::to_string(&Value::Object(record.clone()))
            .map_err(|e| error("mailbox_projection_message_encode_failed", &e.to_string()))?
            .as_bytes(),
    )[..16];
    record.insert("_checksum".to_string(), json!(checksum));
    atomic_write_json_pretty(&staging.join("record.json"), &Value::Object(record))?;
    let existed = destination.exists();
    if existed {
        fs::rename(&destination, &prior)
            .map_err(|e| error("mailbox_projection_message_replace_failed", &e.to_string()))?;
    }
    if let Err(value) = fs::rename(&staging, &destination) {
        if existed && !destination.exists() && prior.exists() {
            let _ = fs::rename(&prior, &destination);
        }
        let _ = fs::remove_dir_all(&staging);
        return Err(error(
            "mailbox_projection_message_replace_failed",
            &value.to_string(),
        ));
    }
    if prior.exists() {
        fs::remove_dir_all(&prior)
            .map_err(|e| error("mailbox_projection_message_cleanup_failed", &e.to_string()))?;
    }
    Ok(())
}

