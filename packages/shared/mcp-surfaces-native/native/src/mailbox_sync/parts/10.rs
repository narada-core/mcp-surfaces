fn normalize_recipient(value: Option<&Value>) -> Option<Value> {
    let address = value
        .and_then(Value::as_object)
        .and_then(|value| value.get("emailAddress"))
        .and_then(Value::as_object)?;
    let display_name = address
        .get("name")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let email = address
        .get("address")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_lowercase);
    if display_name.is_none() && email.is_none() {
        return None;
    }
    let mut result = Map::new();
    if let Some(value) = display_name {
        result.insert("display_name".to_string(), json!(value));
    }
    if let Some(value) = email {
        result.insert("email".to_string(), json!(value));
    }
    Some(Value::Object(result))
}

fn normalize_recipients(value: Option<&Value>) -> Value {
    Value::Array(
        value
            .and_then(Value::as_array)
            .map(|values| {
                values
                    .iter()
                    .filter_map(|value| normalize_recipient(Some(value)))
                    .collect()
            })
            .unwrap_or_default(),
    )
}

fn normalize_headers(value: Option<&Value>) -> Option<Value> {
    let mut headers: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for header in value.and_then(Value::as_array)? {
        let Some(object) = header.as_object() else {
            continue;
        };
        let Some(name) = object
            .get("name")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_lowercase)
        else {
            continue;
        };
        let Some(value) = object.get("value").and_then(Value::as_str).map(str::trim) else {
            continue;
        };
        headers.entry(name).or_default().push(value.to_string());
    }
    if headers.is_empty() {
        return None;
    }
    Some(json!({"values":headers}))
}

fn normalize_body(body: Option<&Value>, policy: &str, preview: Option<&Value>) -> Value {
    let body = body.and_then(Value::as_object);
    let content_type = body
        .and_then(|value| value.get("contentType"))
        .and_then(Value::as_str);
    let content = body
        .and_then(|value| value.get("content"))
        .and_then(Value::as_str)
        .unwrap_or("");
    let preview = preview
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(normalize_line_endings);
    if content_type.is_none() || content.is_empty() {
        let mut value = json!({"body_kind":"empty"});
        if let Some(preview) = preview {
            value
                .as_object_mut()
                .expect("object")
                .insert("preview".to_string(), json!(preview));
        }
        return value;
    }
    let content = normalize_line_endings(content);
    let (kind, field, hash_field) = if content_type == Some("text")
        || (content_type == Some("html") && policy == "text_only")
    {
        ("text", "text", "text_sha256")
    } else if content_type == Some("html") {
        ("html", "html", "html_sha256")
    } else {
        let mut value = json!({"body_kind":"empty"});
        if let Some(preview) = preview {
            value
                .as_object_mut()
                .expect("object")
                .insert("preview".to_string(), json!(preview));
        }
        return value;
    };
    let mut value = json!({"body_kind":kind});
    value
        .as_object_mut()
        .expect("object")
        .insert(field.to_string(), json!(content));
    if let Some(preview) = preview {
        value
            .as_object_mut()
            .expect("object")
            .insert("preview".to_string(), json!(preview));
    }
    value.as_object_mut().expect("object").insert(
        "content_hashes".to_string(),
        json!({hash_field:sha256_hex(content.as_bytes())}),
    );
    value
}

fn normalize_attachments(value: Option<&Value>, policy: &str) -> Result<Value, Value> {
    if policy == "exclude" {
        return Ok(json!([]));
    }
    let mut normalized = Vec::new();
    for (ordinal, raw) in value
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .enumerate()
    {
        let attachment = raw.as_object().cloned().unwrap_or_default();
        let kind = attachment
            .get("@odata.type")
            .and_then(Value::as_str)
            .unwrap_or("");
        let display_name = attachment
            .get("name")
            .and_then(Value::as_str)
            .map(str::trim)
            .unwrap_or("")
            .to_string();
        let id = optional_trimmed(attachment.get("id"));
        let content_type = optional_trimmed(attachment.get("contentType"));
        let content_id = optional_trimmed(attachment.get("contentId"));
        let size = attachment.get("size").and_then(Value::as_i64);
        let inline = attachment
            .get("isInline")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let mut material = vec![
            if kind == "#microsoft.graph.fileAttachment" {
                json!("file")
            } else if kind == "#microsoft.graph.referenceAttachment" {
                json!("reference")
            } else {
                json!("item")
            },
            id.clone().map(Value::String).unwrap_or(Value::Null),
            json!(display_name),
        ];
        let mut result = Map::new();
        let content_hash = if kind == "#microsoft.graph.fileAttachment" {
            attachment
                .get("contentBytes")
                .and_then(Value::as_str)
                .filter(|value| !value.is_empty())
                .map(decode_base64)
                .transpose()?
                .map(|bytes| sha256_hex(&bytes))
        } else {
            None
        };
        if kind == "#microsoft.graph.fileAttachment" {
            material.extend([
                content_type
                    .clone()
                    .map(Value::String)
                    .unwrap_or(Value::Null),
                size.map(|value| json!(value)).unwrap_or(Value::Null),
                content_hash
                    .clone()
                    .map(Value::String)
                    .unwrap_or(Value::Null),
                json!(ordinal),
            ]);
        } else if kind == "#microsoft.graph.referenceAttachment" {
            material.extend([
                attachment.get("sourceUrl").cloned().unwrap_or(Value::Null),
                attachment
                    .get("providerType")
                    .cloned()
                    .unwrap_or(Value::Null),
                json!(ordinal),
            ]);
        } else {
            material.extend([
                content_type
                    .clone()
                    .map(Value::String)
                    .unwrap_or(Value::Null),
                size.map(|value| json!(value)).unwrap_or(Value::Null),
                json!(ordinal),
            ]);
        }
        let key_material = serde_json::to_string(&material)
            .map_err(|e| error("mailbox_attachment_identity_failed", &e.to_string()))?;
        let attachment_key = format!("att_{}", sha256_hex(key_material.as_bytes()));
        result.insert("attachment_key".to_string(), json!(attachment_key));
        if let Some(value) = id {
            result.insert("source_attachment_id".to_string(), json!(value));
        }
        result.insert("ordinal".to_string(), json!(ordinal));
        result.insert("display_name".to_string(), json!(display_name));
        if let Some(value) = content_type {
            result.insert("content_type".to_string(), json!(value));
        }
        if let Some(value) = size {
            result.insert("size_bytes".to_string(), json!(value));
        }
        result.insert("inline".to_string(), json!(inline));
        if let Some(value) = content_id {
            result.insert("content_id".to_string(), json!(value));
        }
        if let Some(value) = content_hash {
            result.insert("content_hash".to_string(), json!(value));
        }
        if kind == "#microsoft.graph.fileAttachment" && policy == "include_content" {
            if let Some(value) = attachment
                .get("contentBytes")
                .and_then(Value::as_str)
                .filter(|value| !value.is_empty())
            {
                result.insert(
                    "content_ref".to_string(),
                    json!(format!("inline-base64:{value}")),
                );
            }
        } else if kind == "#microsoft.graph.referenceAttachment" {
            if let Some(value) = optional_trimmed(attachment.get("sourceUrl")) {
                result.insert("content_ref".to_string(), json!(value));
            }
        }
        if let Some(extensions) = attachment_extensions(&attachment, kind) {
            result.insert("source_extensions".to_string(), extensions);
        }
        normalized.push(Value::Object(result));
    }
    normalized.sort_by(|left, right| {
        left.get("attachment_key")
            .and_then(Value::as_str)
            .cmp(&right.get("attachment_key").and_then(Value::as_str))
    });
    Ok(Value::Array(normalized))
}

