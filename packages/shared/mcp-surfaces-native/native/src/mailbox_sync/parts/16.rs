fn string_array(
    value: Option<&Value>,
    fallback: &[&str],
    code: &str,
) -> Result<Vec<String>, Value> {
    let Some(value) = value else {
        return Ok(fallback.iter().map(|value| value.to_string()).collect());
    };
    let values = value.as_array().ok_or_else(|| error(code, code))?;
    values
        .iter()
        .map(|value| {
            value
                .as_str()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(ToString::to_string)
                .ok_or_else(|| error(code, code))
        })
        .collect()
}

fn stable_id(prefix: &str, value: &str) -> String {
    format!("{prefix}{}", &sha256_hex(value.as_bytes())[..40])
}

fn fingerprint(value: &Value) -> String {
    sha256_hex(canonical_json(value).as_bytes())
}

fn nullable_hash(value: Option<&str>) -> Value {
    value
        .map(|value| json!(sha256_hex(value.as_bytes())))
        .unwrap_or(Value::Null)
}

fn canonical_json(value: &Value) -> String {
    match value {
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {
            serde_json::to_string(value).unwrap_or_else(|_| "null".to_string())
        }
        Value::Array(values) => format!(
            "[{}]",
            values
                .iter()
                .map(canonical_json)
                .collect::<Vec<_>>()
                .join(",")
        ),
        Value::Object(object) => {
            let mut keys = object.keys().collect::<Vec<_>>();
            keys.sort();
            format!(
                "{{{}}}",
                keys.into_iter()
                    .map(|key| format!(
                        "{}:{}",
                        serde_json::to_string(key).unwrap_or_else(|_| "\"\"".to_string()),
                        canonical_json(object.get(key).unwrap_or(&Value::Null))
                    ))
                    .collect::<Vec<_>>()
                    .join(",")
            )
        }
    }
}

fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn now_iso_millis() -> String {
    iso_millis(OffsetDateTime::now_utc())
}

fn iso_millis(value: OffsetDateTime) -> String {
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:03}Z",
        value.year(),
        u8::from(value.month()),
        value.day(),
        value.hour(),
        value.minute(),
        value.second(),
        value.nanosecond() / 1_000_000
    )
}

fn add_millis_iso(value: &str, milliseconds: i128) -> Result<String, Value> {
    let parsed = OffsetDateTime::parse(value, &Rfc3339)
        .map_err(|e| error("mailbox_timestamp_invalid", &e.to_string()))?;
    Ok(iso_millis(
        parsed + time::Duration::milliseconds(milliseconds as i64),
    ))
}

fn normalize_line_endings(value: &str) -> String {
    value.replace("\r\n", "\n").replace('\r', "\n")
}

fn normalized_path_text(path: &Path) -> String {
    let value = path.to_string_lossy();
    let value = value.strip_prefix(r"\\?\").unwrap_or(&value);
    #[cfg(windows)]
    {
        value.replace('/', r"\")
    }
    #[cfg(not(windows))]
    {
        value.to_string()
    }
}

fn graph_mailbox_path(user_id: &str) -> String {
    if user_id == "me" {
        "/me".to_string()
    } else {
        format!("/users/{}", encode_component(user_id))
    }
}

fn validate_graph_base_url(value: &str) -> Result<(), Value> {
    if value.starts_with("https://")
        || (std::env::var("NARADA_NATIVE_GRAPH_ALLOW_INSECURE_TEST")
            .ok()
            .as_deref()
            == Some("1")
            && value.starts_with("http://127.0.0.1:"))
    {
        return Ok(());
    }
    Err(error(
        "mailbox_graph_base_url_not_allowed",
        "Graph mailbox sync requires HTTPS or an explicit loopback test override",
    ))
}

fn validate_graph_page_url(value: &str, base_url: &str) -> Result<(), Value> {
    let prefix = graph_origin_prefix(base_url);
    if value.starts_with(&prefix) {
        Ok(())
    } else {
        Err(error(
            "mailbox_graph_page_url_not_allowed",
            "Graph continuation URL changed authority",
        ))
    }
}

fn graph_origin_prefix(value: &str) -> String {
    let scheme_end = value.find("://").map(|index| index + 3).unwrap_or(0);
    let path = value[scheme_end..]
        .find('/')
        .map(|index| scheme_end + index);
    path.map(|index| value[..index].to_string())
        .unwrap_or_else(|| value.to_string())
}

fn encode_component(value: &str) -> String {
    let mut encoded = String::new();
    for byte in value.as_bytes() {
        if byte.is_ascii_alphanumeric()
            || matches!(
                *byte,
                b'-' | b'_' | b'.' | b'!' | b'~' | b'*' | b'\'' | b'(' | b')'
            )
        {
            encoded.push(*byte as char);
        } else {
            encoded.push_str(&format!("%{byte:02X}"));
        }
    }
    encoded
}

fn non_empty_env(key: &str) -> Option<String> {
    std::env::var(key)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn read_ureq_body(response: ureq::Response) -> Result<String, String> {
    let mut bytes = Vec::new();
    response
        .into_reader()
        .take(MAX_RESPONSE_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() as u64 > MAX_RESPONSE_BYTES {
        return Err("mailbox_graph_response_too_large".to_string());
    }
    String::from_utf8(bytes).map_err(|e| e.to_string())
}

fn atomic_write_json(path: &Path, value: &Value) -> Result<(), Value> {
    let bytes = format!(
        "{}\n",
        serde_json::to_string(value)
            .map_err(|e| error("mailbox_json_encode_failed", &e.to_string()))?
    );
    atomic_write(path, bytes.as_bytes())
}

fn atomic_write_json_pretty(path: &Path, value: &Value) -> Result<(), Value> {
    let bytes = format!(
        "{}\n",
        serde_json::to_string_pretty(value)
            .map_err(|e| error("mailbox_json_encode_failed", &e.to_string()))?
    );
    atomic_write(path, bytes.as_bytes())
}

fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), Value> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|e| error("mailbox_file_write_failed", &e.to_string()))?;
    }
    let temporary = path.with_extension(format!("tmp.{}.{}", std::process::id(), Uuid::new_v4()));
    fs::write(&temporary, bytes).map_err(|e| error("mailbox_file_write_failed", &e.to_string()))?;
    if path.exists() {
        fs::remove_file(path).map_err(|e| error("mailbox_file_replace_failed", &e.to_string()))?;
    }
    fs::rename(&temporary, path).map_err(|e| error("mailbox_file_replace_failed", &e.to_string()))
}

fn decode_base64(value: &str) -> Result<Vec<u8>, Value> {
    let mut output = Vec::with_capacity(value.len() * 3 / 4);
    let mut quartet = [0_u8; 4];
    let mut count = 0;
    for byte in value.bytes().filter(|byte| !byte.is_ascii_whitespace()) {
        let decoded = match byte {
            b'A'..=b'Z' => byte - b'A',
            b'a'..=b'z' => byte - b'a' + 26,
            b'0'..=b'9' => byte - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            b'=' => 64,
            _ => {
                return Err(error(
                    "mailbox_attachment_base64_invalid",
                    "mailbox_attachment_base64_invalid",
                ))
            }
        };
        quartet[count] = decoded;
        count += 1;
        if count == 4 {
            output.push((quartet[0] << 2) | (quartet[1] >> 4));
            if quartet[2] != 64 {
                output.push((quartet[1] << 4) | (quartet[2] >> 2));
            }
            if quartet[3] != 64 {
                output.push((quartet[2] << 6) | quartet[3]);
            }
            count = 0;
        }
    }
    if count != 0 {
        return Err(error(
            "mailbox_attachment_base64_invalid",
            "mailbox_attachment_base64_invalid",
        ));
    }
    Ok(output)
}

fn bounded_error(value: &str) -> String {
    value
        .replace(['\r', '\n'], " ")
        .chars()
        .take(2048)
        .collect()
}

