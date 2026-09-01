fn redacted(url: &Url) -> String {
    let mut value = url.clone();
    let _ = value.set_username("");
    let _ = value.set_password(None);
    value.set_query(None);
    value.set_fragment(None);
    value.to_string()
}
fn sanitize(value: Value) -> Value {
    match value {
        Value::Object(object) => Value::Object(
            object
                .into_iter()
                .map(|(key, value)| {
                    let lower = key.to_ascii_lowercase();
                    let secret = [
                        "cookie",
                        "token",
                        "secret",
                        "password",
                        "authorization",
                        "credential",
                        "api_key",
                        "api-key",
                    ]
                    .iter()
                    .any(|needle| lower.contains(needle));
                    (
                        key,
                        if secret {
                            json!("[redacted]")
                        } else {
                            sanitize(value)
                        },
                    )
                })
                .collect(),
        ),
        Value::Array(values) => Value::Array(values.into_iter().take(500).map(sanitize).collect()),
        other => other,
    }
}
fn now() -> String {
    OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .unwrap_or_else(|_| "1970-01-01T00:00:00Z".into())
}
fn empty() -> Value {
    json!({"type":"object","properties":{},"additionalProperties":false})
}
fn tool(name: &str, description: &str, input: Value, read_only: bool) -> Value {
    json!({"name":name,"description":description,"inputSchema":input,"annotations":{"title":name,"readOnlyHint":read_only,"destructiveHint":false,"idempotentHint":true,"openWorldHint":true},"outputSchema":{"type":"object","additionalProperties":true}})
}
fn error(code: &str, message: &str, details: Value) -> Value {
    json!({"schema":"narada.cloudflare_carrier_mcp.error.v1","status":"unavailable","code":code,"message":message,"details":details})
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn schemas_are_closed_and_server_bound() {
        for tool in list_tools() {
            assert_eq!(tool["inputSchema"]["additionalProperties"], false);
            assert!(tool["inputSchema"]["properties"]
                .get("session_file")
                .is_none());
            assert!(tool["inputSchema"]["properties"]
                .get("health_file")
                .is_none());
        }
    }
    #[test]
    fn projection_ids_are_path_safe() {
        let root = std::env::temp_dir().join(format!("cloudflare-projection-{}", Uuid::new_v4()));
        let state = State {
            repo_root: root.clone(),
            site_root: root.clone(),
            session_file: root.join("s"),
            health_file: root.join("h"),
            projection_root: root.join("p"),
            worker_url: "https://example.com".into(),
        };
        fs::create_dir_all(&state.projection_root).unwrap();
        assert!(projection(&state, "../escape", &now()).is_err());
        assert!(projection(&state, "missing", &now()).unwrap().is_none());
        let _ = fs::remove_dir_all(root);
    }
}
