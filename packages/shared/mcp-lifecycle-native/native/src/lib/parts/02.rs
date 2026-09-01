
fn resource_page(params: &Value) -> Result<(usize, usize), String> {
    let has_cursor = params
        .get("cursor")
        .and_then(Value::as_str)
        .map(|value| !value.is_empty())
        .unwrap_or(false);
    if has_cursor && params.get("offset").is_some() {
        return Err("resource_page_cursor_and_offset_are_mutually_exclusive".to_string());
    }
    let offset = if has_cursor {
        params
            .get("cursor")
            .and_then(Value::as_str)
            .ok_or("resource_cursor_invalid")?
            .parse::<usize>()
            .map_err(|_| "resource_cursor_invalid".to_string())?
    } else {
        params
            .get("offset")
            .and_then(Value::as_u64)
            .map(|value| value as usize)
            .unwrap_or(0)
    };
    let limit = params
        .get("limit")
        .and_then(Value::as_u64)
        .map(|value| value as usize)
        .unwrap_or(100);
    if limit == 0 || limit > 1_000 {
        return Err("resource_limit_invalid".to_string());
    }
    Ok((offset, limit))
}

fn valid_output_id(value: &str) -> bool {
    let mut chars = value.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    let length = value.len();
    (3..=64).contains(&length)
        && first.is_ascii_alphanumeric()
        && chars.all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_'))
}

fn output_id_from_reference(reference: &str) -> Result<String, String> {
    let id = reference
        .strip_prefix("mcp_output:")
        .ok_or_else(|| format!("output_ref_invalid: {reference}"))?;
    if !valid_output_id(id) {
        return Err(format!("output_ref_invalid: {reference}"));
    }
    Ok(id.to_string())
}

fn percent_encode(value: &str) -> String {
    value
        .as_bytes()
        .iter()
        .map(|byte| match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (*byte as char).to_string()
            }
            _ => format!("%{byte:02X}"),
        })
        .collect()
}

fn percent_decode(value: &str) -> Result<String, String> {
    let bytes = value.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] != b'%' {
            decoded.push(bytes[index]);
            index += 1;
            continue;
        }
        if index + 2 >= bytes.len() {
            return Err(format!("output_resource_uri_invalid: {value}"));
        }
        let high = (bytes[index + 1] as char)
            .to_digit(16)
            .ok_or_else(|| format!("output_resource_uri_invalid: {value}"))?;
        let low = (bytes[index + 2] as char)
            .to_digit(16)
            .ok_or_else(|| format!("output_resource_uri_invalid: {value}"))?;
        decoded.push(((high << 4) | low) as u8);
        index += 3;
    }
    String::from_utf8(decoded).map_err(|_| format!("output_resource_uri_invalid: {value}"))
}

fn utf16_len(value: &str) -> usize {
    value.encode_utf16().count()
}
fn valid_payload_id(value: &str) -> bool {
    let mut chars = value.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    (3..=64).contains(&value.len())
        && first.is_ascii_alphanumeric()
        && chars.all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_'))
}

fn parse_payload_reference(reference: &str) -> Result<(String, i64), String> {
    let body = reference
        .strip_prefix("mcp_payload:")
        .ok_or_else(|| format!("payload_ref_invalid: {reference}"))?;
    let (id, revision) = body
        .split_once("@v")
        .ok_or_else(|| format!("payload_ref_invalid: {reference}"))?;
    if !valid_payload_id(id) {
        return Err(format!("payload_ref_invalid: {reference}"));
    }
    let revision = revision
        .parse::<i64>()
        .ok()
        .filter(|value| *value > 0)
        .ok_or_else(|| format!("payload_ref_invalid: {reference}"))?;
    Ok((id.to_string(), revision))
}

fn payload_revision_path(root: &Path, id: &str, revision: i64) -> PathBuf {
    root.join(".ai")
        .join("tmp")
        .join("mcp-payloads")
        .join("workspace")
        .join(id)
        .join(format!("v{revision}.json"))
}

fn payload_stable_json(value: &Value) -> String {
    serde_json::to_string(&native_canonical_value(value)).unwrap_or_else(|_| "null".to_string())
}

fn payload_byte_size(value: &Value) -> usize {
    payload_stable_json(value).len()
}

fn payload_object_from_args(
    args: &Value,
    object_key: &str,
    json_key: &str,
) -> Result<Value, String> {
    let object = args.get(object_key);
    let json_text = args.get(json_key).and_then(Value::as_str);
    if object.is_some() && json_text.is_some() {
        let placeholder = object
            .and_then(Value::as_object)
            .map(|value| value.is_empty())
            .unwrap_or(false);
        if !placeholder {
            return Err(format!("payload_{object_key}_and_{json_key}_ambiguous"));
        }
    }
    let value = if let Some(text) = json_text {
        serde_json::from_str::<Value>(text)
            .map_err(|e| format!("payload_{json_key}_must_be_object: {e}"))?
    } else {
        object.cloned().unwrap_or_else(|| json!({}))
    };
    if !value.is_object() {
        return Err(format!("payload_{object_key}_must_be_object"));
    }
    Ok(value)
}

fn merge_json_objects(base: &mut Value, overlay: &Value) -> Result<(), String> {
    let Some(base_object) = base.as_object_mut() else {
        return Err("payload_derive_overlay_parent_not_object".to_string());
    };
    let Some(overlay_object) = overlay.as_object() else {
        return Err("payload_derive_overlay_must_be_object".to_string());
    };
    for (key, value) in overlay_object {
        if let Some(existing) = base_object.get_mut(key) {
            if existing.is_object() && value.is_object() {
                merge_json_objects(existing, value)?;
                continue;
            }
        }
        base_object.insert(key.clone(), value.clone());
    }
    Ok(())
}

fn delete_json_pointer(value: &mut Value, pointer: &str) -> Result<(), String> {
    if !pointer.starts_with('/') {
        return Err(format!("payload_derive_delete_path_invalid: {pointer}"));
    }
    let segments = pointer[1..]
        .split('/')
        .map(|segment| {
            let mut decoded = String::new();
            let bytes = segment.as_bytes();
            let mut index = 0;
            while index < bytes.len() {
                if bytes[index] == b'~' {
                    if index + 1 >= bytes.len() || !matches!(bytes[index + 1], b'0' | b'1') {
                        return Err(format!(
                            "payload_derive_delete_path_invalid_escape: {pointer}"
                        ));
                    }
                    decoded.push(if bytes[index + 1] == b'0' { '~' } else { '/' });
                    index += 2;
                } else {
                    decoded.push(bytes[index] as char);
                    index += 1;
                }
            }
            Ok(decoded)
        })
        .collect::<Result<Vec<_>, _>>()?;
    let Some(mut current) = value.as_object_mut() else {
        return Err(format!(
            "payload_derive_delete_path_parent_not_object: {pointer}"
        ));
    };
    for segment in &segments[..segments.len().saturating_sub(1)] {
        let Some(next) = current.get_mut(segment).and_then(Value::as_object_mut) else {
            return Err(format!(
                "payload_derive_delete_path_parent_not_object: {pointer}"
            ));
        };
        current = next;
    }
    let Some(last) = segments.last() else {
        return Err(format!("payload_derive_delete_path_not_found: {pointer}"));
    };
    if current.remove(last).is_none() {
        return Err(format!("payload_derive_delete_path_not_found: {pointer}"));
    }
    Ok(())
}
