fn optional_config_string(object: &Map<String, Value>, snake: &str, camel: &str) -> Option<String> {
    object
        .get(snake)
        .or_else(|| object.get(camel))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

fn string_array(object: &Map<String, Value>, snake: &str, camel: &str) -> Vec<String> {
    object
        .get(snake)
        .or_else(|| object.get(camel))
        .and_then(Value::as_array)
        .map(|values| {
            values
                .iter()
                .filter_map(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(ToOwned::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

fn confirmed(args: &Map<String, Value>, snake: &str, camel: &str) -> bool {
    args.get(snake).and_then(Value::as_bool).or_else(|| args.get(camel).and_then(Value::as_bool)).unwrap_or(false)
}

fn mailbox<'a>(args: &'a Map<String, Value>) -> Option<&'a str> { args.get("mailbox_id").and_then(Value::as_str).filter(|value| !value.trim().is_empty()) }
fn mailbox_value(args: &Map<String, Value>) -> Value { mailbox(args).map(|value| json!(value)).unwrap_or_else(|| json!("me")) }
fn optional_string(args: &Map<String, Value>, key: &str) -> Option<String> { args.get(key).and_then(Value::as_str).map(str::trim).filter(|value| !value.is_empty()).map(ToOwned::to_owned) }
fn required_string(args: &Map<String, Value>, key: &str) -> Result<String, Value> { optional_string(args, key).ok_or_else(|| invalid(key)) }
fn required_positive_number(args: &Map<String, Value>, key: &str) -> Result<u64, Value> {
    let value = args
        .get(key)
        .and_then(Value::as_u64)
        .filter(|value| *value > 0)
        .ok_or_else(|| invalid(key))?;
    Ok(value)
}
fn required_nonnegative_number(args: &Map<String, Value>, key: &str) -> Result<u64, Value> {
    args.get(key)
        .and_then(Value::as_u64)
        .ok_or_else(|| invalid(key))
}
fn bounded_top(value: Option<&Value>, fallback: u64) -> u64 { value.and_then(Value::as_u64).unwrap_or(fallback).clamp(1, 100) }

fn encode_component(value: &str) -> String {
    value.bytes().map(|byte| match byte { b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => (byte as char).to_string(), _ => format!("%{byte:02X}") }).collect()
}

fn hex_lower(bytes: &[u8]) -> String { bytes.iter().map(|byte| format!("{byte:02x}")).collect() }
fn invalid(key: &str) -> Value { json!({"schema":"narada.graph_mail_mcp.error.v1","status":"invalid","reason":format!("{key}_required")}) }
fn boundary(name: &str, reason: &str) -> Value { json!({"schema":"narada.graph_mail_mcp.authority_boundary.v1","status":"unavailable","tool_name":name,"reason":reason,"remediation":"Use a supported tool through the configured native Graph Mail authority."}) }
fn unavailable(reason: &str, detail: &str) -> Value { json!({"schema":"narada.graph_mail_mcp.authority_error.v1","status":"unavailable","reason":reason,"detail":detail}) }

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn graph_mail_native_authority_is_explicit() {
        std::env::remove_var("NARADA_NATIVE_GRAPH_MAIL_AUTHORITY");
        assert!(!enabled());
    }

    #[test]
    fn operation_support_is_limited_to_ported_provider_slice() {
        assert!(supports("graph_mail_query"));
        assert!(supports("graph_mail_message_mark_read"));
        assert!(supports("graph_mail_attachment_upload_session_create"));
        assert!(supports("graph_mail_draft_send"));
        assert!(supports("graph_mail_reply_all_to_last_in_thread_draft_create"));
        assert!(supports("graph_mail_attachment_upload_chunk"));
        assert!(supports("graph_mail_attachment_upload_file"));
        assert!(supports("graph_mail_ticket_draft_upsert"));
    }

    #[test]
    fn bounded_base64_decoder_matches_attachment_bytes() {
        assert_eq!(decode_base64("SGVsbG8=").unwrap(), b"Hello");
        assert!(decode_base64("not-base64").is_err());
    }

    #[test]
    fn governed_html_reply_applies_escaped_signature_before_quote() {
        let html = compose_reply_html("<p>Done.</p>", "<p>Original</p>", Some("Ezra & Team"));
        assert_eq!(
            html,
            "<p>Done.</p><p>Thanks,<br>Ezra &amp; Team</p><div data-narada-quoted-history=\"true\"><p>Original</p></div>"
        );
    }
}
