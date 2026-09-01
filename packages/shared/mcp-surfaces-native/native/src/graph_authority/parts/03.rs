/// Execute a provider operation through the native Graph authority.  This is
/// called only after the explicit native-authority switch has been checked by
/// the calendar surface.
pub fn call_calendar(name: &str, args: &Map<String, Value>, root: &Path) -> Result<Value, Value> {
    let adapter = CalendarGraphAdapter::from_site_root(root)?;
    let is_write = matches!(
        name,
        "calendar_event_create" | "calendar_event_update" | "calendar_event_delete"
    );
    // The TS authority validates event_id before applying the write policy for
    // update/delete, while create applies policy before validating its body.
    let prevalidated_request = if matches!(name, "calendar_event_update" | "calendar_event_delete")
    {
        Some(build_request(name, args)?)
    } else {
        None
    };
    if is_write {
        if let Err(reason) = adapter.write_allowed(args) {
            return refused_write(root, name, args, reason);
        }
    }
    let request = prevalidated_request.unwrap_or(build_request(name, args)?);
    let request_url = adapter.build_url(
        request.mailbox_id.as_deref(),
        &request.suffix,
        &request.query,
    )?;
    if is_write {
        let requested = match name {
            "calendar_event_create" => json!({
                "event_kind":"event_create_requested",
                "mailbox_id":args.get("mailbox_id").cloned().unwrap_or_else(|| json!("me")),
                "subject":request.body.as_ref().and_then(|body| body.get("subject")).cloned().unwrap_or(Value::Null)
            }),
            "calendar_event_update" => json!({
                "event_kind":"event_update_requested",
                "mailbox_id":args.get("mailbox_id").cloned().unwrap_or_else(|| json!("me")),
                "event_id":args.get("event_id").cloned().unwrap_or(Value::Null)
            }),
            "calendar_event_delete" => json!({
                "event_kind":"event_delete_requested",
                "mailbox_id":args.get("mailbox_id").cloned().unwrap_or_else(|| json!("me")),
                "event_id":args.get("event_id").cloned().unwrap_or(Value::Null)
            }),
            _ => Value::Null,
        };
        record_calendar_audit(root, requested)?;
    }
    let response = adapter.request(
        request.method,
        request.mailbox_id.as_deref(),
        &request.suffix,
        &request.query,
        request.body.as_ref(),
    )?;
    let result = wrap_result(name, request_url, response)?;
    if is_write {
        let completed = match name {
            "calendar_event_create" => json!({
                "event_kind":"event_create_completed",
                "mailbox_id":args.get("mailbox_id").cloned().unwrap_or_else(|| json!("me")),
                "event_id":result.get("event").and_then(|event| event.get("id")).cloned().unwrap_or(Value::Null)
            }),
            "calendar_event_update" => json!({
                "event_kind":"event_update_completed",
                "mailbox_id":args.get("mailbox_id").cloned().unwrap_or_else(|| json!("me")),
                "event_id":args.get("event_id").cloned().unwrap_or(Value::Null)
            }),
            "calendar_event_delete" => json!({
                "event_kind":"event_delete_completed",
                "mailbox_id":args.get("mailbox_id").cloned().unwrap_or_else(|| json!("me")),
                "event_id":args.get("event_id").cloned().unwrap_or(Value::Null)
            }),
            _ => Value::Null,
        };
        record_calendar_audit(root, completed)?;
    }
    Ok(result)
}

fn refused_write(
    root: &Path,
    name: &str,
    args: &Map<String, Value>,
    reason: &str,
) -> Result<Value, Value> {
    let event_kind = match name {
        "calendar_event_create" => "event_create_refused",
        "calendar_event_update" => "event_update_refused",
        "calendar_event_delete" => "event_delete_refused",
        _ => "event_write_refused",
    };
    record_calendar_audit(
        root,
        json!({
            "event_kind":event_kind,
            "mailbox_id":args.get("mailbox_id").cloned().unwrap_or_else(|| json!("me")),
            "event_id":args.get("event_id").cloned().unwrap_or(Value::Null),
            "reason":reason
        }),
    )?;
    Ok(json!({
        "schema":"narada.calendar_mcp.event_write.v1",
        "status":"refused",
        "reason":reason,
        "event_id":args.get("event_id").cloned().unwrap_or(Value::Null)
    }))
}

fn resolve_auth(environment: &HashMap<String, String>) -> GraphAuth {
    if let Some(value) = non_empty(environment, "GRAPH_ACCESS_TOKEN") {
        return GraphAuth::AccessToken(value.to_string());
    }
    let tenant = non_empty(environment, "GRAPH_TENANT_ID");
    let client_id = non_empty(environment, "GRAPH_CLIENT_ID");
    let client_secret = non_empty(environment, "GRAPH_CLIENT_SECRET");
    if let (Some(tenant), Some(client_id), Some(client_secret)) = (tenant, client_id, client_secret)
    {
        let endpoint = environment
            .get("GRAPH_TOKEN_ENDPOINT")
            .filter(|value| !value.trim().is_empty())
            .cloned()
            .unwrap_or_else(|| {
                format!(
                    "https://login.microsoftonline.com/{}/oauth2/v2.0/token",
                    encode_component(tenant)
                )
            });
        return GraphAuth::ClientCredentials {
            endpoint,
            client_id: client_id.to_string(),
            client_secret: client_secret.to_string(),
        };
    }
    non_empty(environment, "MS_GRAPH_ACCESS_TOKEN")
        .map(|value| GraphAuth::AccessToken(value.to_string()))
        .unwrap_or(GraphAuth::Missing)
}

fn resolve_auth_with_delegated_token(
    root: &Path,
    environment: &HashMap<String, String>,
) -> GraphAuth {
    let configured = resolve_auth(environment);
    if !matches!(configured, GraphAuth::Missing) {
        return configured;
    }
    let path = root.join(".ai/runtime/graph-mail-mcp/delegated-token.json");
    let Ok(metadata) = fs::metadata(&path) else {
        return GraphAuth::Missing;
    };
    if metadata.len() > MAX_CONFIG_BYTES {
        return GraphAuth::Missing;
    }
    let Ok(text) = fs::read_to_string(&path) else {
        return GraphAuth::Missing;
    };
    let Ok(value) = serde_json::from_str::<Value>(&text) else {
        return GraphAuth::Missing;
    };
    if value.get("schema").and_then(Value::as_str)
        != Some("narada.graph_mail_mcp.delegated_token.v1")
    {
        return GraphAuth::Missing;
    }
    let expires_at_ms = value
        .get("expires_at_ms")
        .and_then(Value::as_i64)
        .unwrap_or(0);
    let now_ms = (OffsetDateTime::now_utc().unix_timestamp_nanos() / 1_000_000) as i64;
    if expires_at_ms <= now_ms + 60_000 {
        return refresh_delegated_token(root, &path, &value, now_ms, request_delegated_refresh);
    }
    value
        .get("access_token")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .map(|value| GraphAuth::AccessToken(value.to_string()))
        .unwrap_or(GraphAuth::Missing)
}

fn refresh_delegated_token<F>(
    root: &Path,
    path: &Path,
    token: &Value,
    now_ms: i64,
    refresh: F,
) -> GraphAuth
where
    F: FnOnce(&str, &str, &str, &str) -> Result<Value, Value>,
{
    let required = |name: &str| {
        token
            .get(name)
            .and_then(Value::as_str)
            .filter(|value| !value.trim().is_empty())
            .map(ToOwned::to_owned)
    };
    let Some(tenant_id) = required("tenant_id") else {
        return GraphAuth::Unavailable(unavailable(
            "graph_delegated_token_refresh_unavailable",
            "persisted delegated token has no tenant_id",
        ));
    };
    let Some(client_id) = required("client_id") else {
        return GraphAuth::Unavailable(unavailable(
            "graph_delegated_token_refresh_unavailable",
            "persisted delegated token has no client_id",
        ));
    };
    let Some(scope) = required("scope") else {
        return GraphAuth::Unavailable(unavailable(
            "graph_delegated_token_refresh_unavailable",
            "persisted delegated token has no scope",
        ));
    };
    let Some(refresh_token) = required("refresh_token") else {
        return GraphAuth::Unavailable(unavailable(
            "graph_delegated_token_refresh_unavailable",
            "persisted delegated token has no refresh_token",
        ));
    };
    let payload = match refresh(&tenant_id, &client_id, &scope, &refresh_token) {
        Ok(payload) => payload,
        Err(error) => return GraphAuth::Unavailable(error),
    };
    let Some(access_token) = payload
        .get("access_token")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .map(ToOwned::to_owned)
    else {
        return GraphAuth::Unavailable(unavailable(
            "ms_graph_delegated_token_refresh_invalid",
            "refresh response has no access_token",
        ));
    };
    let expires_in = payload
        .get("expires_in")
        .and_then(Value::as_u64)
        .unwrap_or(3599)
        .max(60);
    let rotated_refresh_token = payload
        .get("refresh_token")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .unwrap_or(&refresh_token);
    let refreshed = json!({
        "schema":"narada.graph_mail_mcp.delegated_token.v1",
        "auth_mode":"delegated_device_code",
        "tenant_id":tenant_id,
        "client_id":client_id,
        "scope":scope,
        "access_token":access_token,
        "refresh_token":rotated_refresh_token,
        "expires_at_ms":now_ms.saturating_add((expires_in as i64).saturating_mul(1000)),
        "acquired_at":OffsetDateTime::now_utc().format(&Rfc3339).unwrap_or_else(|_| "unknown".to_string())
    });
    if let Err(error) = persist_delegated_token(path, &refreshed) {
        return GraphAuth::Unavailable(error);
    }
    let _ = crate::graph_mail_authority::record_audit(root, json!({
        "event_kind":"delegated_token_refreshed",
        "expires_at_ms":refreshed.get("expires_at_ms")
    }));
    GraphAuth::AccessToken(access_token)
}

