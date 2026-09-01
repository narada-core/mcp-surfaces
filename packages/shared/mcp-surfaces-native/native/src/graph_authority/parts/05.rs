fn encode_component(value: &str) -> String {
    value
        .bytes()
        .map(|byte| match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (byte as char).to_string()
            }
            _ => format!("%{byte:02X}"),
        })
        .collect()
}

fn http_error(code: u16, response: ureq::Response) -> Value {
    let body = read_response_body(response)
        .map(|(_, body)| redact(&body))
        .unwrap_or_else(|_| "<unreadable>".to_string());
    unavailable(
        "graph_request_failed",
        &format!(
            "http_status={code};body={}",
            body.chars().take(1_000).collect::<String>()
        ),
    )
}

fn redact(value: &str) -> String {
    value
        .replace("access_token", "<redacted-access-token>")
        .replace("client_secret", "<redacted-client-secret>")
        .chars()
        .take(4_000)
        .collect()
}

fn validate_base_url(value: &str) -> Result<(), Value> {
    let allowed = value.starts_with("https://graph.microsoft.com/")
        || (std::env::var("NARADA_NATIVE_GRAPH_ALLOW_INSECURE_TEST")
            .ok()
            .as_deref()
            == Some("1")
            && value.starts_with("http://127.0.0.1:"));
    if !allowed {
        return Err(unavailable(
            "graph_base_url_not_allowed",
            "Graph authority requires https://graph.microsoft.com or an explicit loopback test override",
        ));
    }
    Ok(())
}

fn validate_upload_url(value: &str) -> Result<(), Value> {
    if value.len() > MAX_URL_BYTES {
        return Err(unavailable(
            "attachment_upload_url_too_large",
            &MAX_URL_BYTES.to_string(),
        ));
    }
    let insecure_test = std::env::var("NARADA_GRAPH_MAIL_ALLOW_INSECURE_TEST")
        .ok()
        .as_deref()
        == Some("1")
        && value.starts_with("http://127.0.0.1:");
    let Some(host_and_path) =
        value.strip_prefix(if insecure_test { "http://" } else { "https://" })
    else {
        return Err(unavailable(
            "attachment_upload_url_must_be_https",
            "upload URL must use HTTPS",
        ));
    };
    if insecure_test {
        return Ok(());
    }
    let host = host_and_path
        .split(['/', '?', '#'])
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase();
    if !matches!(
        host.as_str(),
        "outlook.office.com" | "outlook.office365.com" | "graph.microsoft.com"
    ) {
        return Err(unavailable("attachment_upload_url_host_not_allowed", &host));
    }
    Ok(())
}

fn unavailable(reason: &str, detail: &str) -> Value {
    json!({
        "schema":"narada.graph_authority.error.v1",
        "code":reason,
        "message":reason,
        "status":"unavailable",
        "reason":reason,
        "detail":detail,
        "remediation":"Configure the bounded native Graph authority and retry."
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn encoding_is_url_safe() {
        assert_eq!(encode_component("user@example.com"), "user%40example.com");
        assert_eq!(encode_component("a b"), "a%20b");
    }

    #[test]
    fn unavailable_errors_have_actionable_jsonrpc_identity() {
        let failure = unavailable("graph_access_token_missing", "configure credentials");
        assert_eq!(failure["code"], "graph_access_token_missing");
        assert_eq!(failure["message"], "graph_access_token_missing");
        assert_eq!(failure["status"], "unavailable");
    }

    #[test]
    fn expired_delegated_token_refreshes_persists_and_survives_restart() {
        let unique = OffsetDateTime::now_utc().unix_timestamp_nanos();
        let root = std::env::temp_dir().join(format!(
            "narada-graph-refresh-{}-{unique}",
            std::process::id()
        ));
        let path = root.join(".ai/runtime/graph-mail-mcp/delegated-token.json");
        let token = json!({
            "schema":"narada.graph_mail_mcp.delegated_token.v1",
            "auth_mode":"delegated_device_code",
            "tenant_id":"organizations",
            "client_id":"client",
            "scope":"https://graph.microsoft.com/Mail.ReadWrite offline_access",
            "access_token":"expired-access",
            "refresh_token":"refresh-one",
            "expires_at_ms":1
        });
        let refresh_count = AtomicUsize::new(0);
        let now_ms =
            (OffsetDateTime::now_utc().unix_timestamp_nanos() / 1_000_000) as i64;
        let refreshed = refresh_delegated_token(
            &root,
            &path,
            &token,
            now_ms,
            |tenant, client, scope, refresh_token| {
                refresh_count.fetch_add(1, Ordering::SeqCst);
                assert_eq!(tenant, "organizations");
                assert_eq!(client, "client");
                assert!(scope.contains("offline_access"));
                assert_eq!(refresh_token, "refresh-one");
                Ok(json!({
                    "access_token":"fresh-access",
                    "refresh_token":"refresh-two",
                    "expires_in":3600
                }))
            },
        );
        assert_eq!(refreshed.access_token().expect("refreshed token"), "fresh-access");
        assert_eq!(refresh_count.load(Ordering::SeqCst), 1);

        let persisted: Value =
            serde_json::from_str(&fs::read_to_string(&path).expect("persisted token"))
                .expect("persisted token JSON");
        assert_eq!(persisted["refresh_token"], "refresh-two");
        assert_eq!(persisted["access_token"], "fresh-access");

        let restarted = resolve_auth_with_delegated_token(&root, &HashMap::new());
        assert_eq!(restarted.access_token().expect("restart token"), "fresh-access");
        fs::remove_dir_all(root).expect("remove test root");
    }

    #[test]
    fn production_graph_base_is_restricted() {
        assert!(validate_base_url(DEFAULT_GRAPH_BASE_URL).is_ok());
        assert!(validate_base_url("http://example.invalid/v1.0").is_err());
    }

    #[test]
    fn upload_host_allowlist_is_restricted() {
        assert!(validate_upload_url("https://outlook.office.com/upload/fixture").is_ok());
        assert!(validate_upload_url("https://evil.example/upload").is_err());
        assert!(validate_upload_url("http://outlook.office.com/upload").is_err());
    }

    #[test]
    fn calendar_url_respects_mailbox_allowlist_and_bounds() {
        let adapter = CalendarGraphAdapter {
            base_url: DEFAULT_GRAPH_BASE_URL.to_string(),
            allowed_mailboxes: vec!["user@example.com".to_string()],
            allow_event_writes: false,
            write_approval_token: None,
            auth: GraphAuth::AccessToken("test".to_string()),
            request_timeout: Duration::from_secs(30),
        };
        let mut query = Map::new();
        query.insert("$top".to_string(), json!(20));
        let url = adapter
            .build_url(Some("user@example.com"), "calendars", &query)
            .expect("url");
        assert!(url.contains("/users/user%40example.com/calendars"));
        assert!(url.contains("%24top=20"));
        assert!(adapter
            .build_url(Some("other@example.com"), "calendars", &query)
            .is_err());
    }

    #[test]
    fn default_mailbox_matches_calendar_policy() {
        let adapter = CalendarGraphAdapter {
            base_url: DEFAULT_GRAPH_BASE_URL.to_string(),
            allowed_mailboxes: vec![
                "user@example.com".to_string(),
                "other@example.com".to_string(),
            ],
            allow_event_writes: false,
            write_approval_token: None,
            auth: GraphAuth::AccessToken("test".to_string()),
            request_timeout: Duration::from_secs(30),
        };
        assert!(adapter.build_url(None, "calendars", &Map::new()).is_err());
    }

    #[test]
    fn write_policy_requires_explicit_confirmation_and_token() {
        let adapter = CalendarGraphAdapter {
            base_url: DEFAULT_GRAPH_BASE_URL.to_string(),
            allowed_mailboxes: Vec::new(),
            allow_event_writes: true,
            write_approval_token: Some("approve".to_string()),
            auth: GraphAuth::AccessToken("test".to_string()),
            request_timeout: Duration::from_secs(30),
        };
        let mut args = Map::new();
        assert_eq!(adapter.write_allowed(&args), Err("confirm_write_required"));
        args.insert("confirm_write".to_string(), json!(true));
        assert_eq!(
            adapter.write_allowed(&args),
            Err("write_approval_token_required")
        );
        args.insert("approval_token".to_string(), json!("approve"));
        assert_eq!(adapter.write_allowed(&args), Ok(()));
    }
}
