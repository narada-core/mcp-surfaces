fn delegated_graph_access_token(scope: &ScopeConfig) -> Result<String, Value> {
    let site_root = scope
        .root_dir
        .ancestors()
        .find(|candidate| {
            candidate
                .join(".ai/runtime/graph-mail-mcp/delegated-token.json")
                .is_file()
        })
        .ok_or_else(|| {
            error(
                "mailbox_graph_delegated_token_missing",
                "mailbox_graph_delegated_token_missing",
            )
        })?;
    let path = site_root.join(".ai/runtime/graph-mail-mcp/delegated-token.json");
    let text = fs::read_to_string(&path)
        .map_err(|e| error("mailbox_graph_delegated_token_missing", &e.to_string()))?;
    let mut token: Value = serde_json::from_str(&text)
        .map_err(|e| error("mailbox_graph_delegated_token_invalid", &e.to_string()))?;
    if token.get("schema").and_then(Value::as_str)
        != Some("narada.graph_mail_mcp.delegated_token.v1")
    {
        return Err(error(
            "mailbox_graph_delegated_token_invalid",
            "mailbox_graph_delegated_token_invalid",
        ));
    }
    let expires_at_ms = token
        .get("expires_at_ms")
        .and_then(Value::as_i64)
        .unwrap_or(0);
    let now_ms = (OffsetDateTime::now_utc().unix_timestamp_nanos() / 1_000_000) as i64;
    if expires_at_ms > now_ms + 60_000 {
        return token
            .get("access_token")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToString::to_string)
            .ok_or_else(|| {
                error(
                    "mailbox_graph_delegated_token_invalid",
                    "mailbox_graph_delegated_token_invalid",
                )
            });
    }
    let tenant =
        required_value_string(&token, "tenant_id", "mailbox_graph_delegated_token_invalid")?;
    let client_id =
        required_value_string(&token, "client_id", "mailbox_graph_delegated_token_invalid")?;
    let scope_value =
        required_value_string(&token, "scope", "mailbox_graph_delegated_token_invalid")?;
    let refresh = required_value_string(
        &token,
        "refresh_token",
        "mailbox_graph_delegated_token_expired_reauthorization_required",
    )?;
    let endpoint = format!(
        "https://login.microsoftonline.com/{}/oauth2/v2.0/token",
        encode_component(&tenant)
    );
    let form = format!(
        "grant_type=refresh_token&client_id={}&refresh_token={}&scope={}",
        encode_component(&client_id),
        encode_component(&refresh),
        encode_component(&scope_value)
    );
    let response = ureq::AgentBuilder::new()
        .timeout(Duration::from_secs(20))
        .build()
        .post(&endpoint)
        .set("Content-Type", "application/x-www-form-urlencoded")
        .send_string(&form);
    let response = response.map_err(|value| {
        error(
            "mailbox_graph_delegated_token_refresh_failed",
            &value.to_string(),
        )
    })?;
    let payload: Value = serde_json::from_str(
        &read_ureq_body(response)
            .map_err(|value| error("mailbox_graph_delegated_token_refresh_failed", &value))?,
    )
    .map_err(|e| {
        error(
            "mailbox_graph_delegated_token_refresh_response_invalid",
            &e.to_string(),
        )
    })?;
    let access_token = required_value_string(
        &payload,
        "access_token",
        "mailbox_graph_delegated_token_refresh_response_invalid",
    )?;
    let expires_in = payload
        .get("expires_in")
        .and_then(Value::as_i64)
        .unwrap_or(3599)
        .max(60);
    if let Some(object) = token.as_object_mut() {
        object.insert("access_token".to_string(), json!(access_token));
        if let Some(value) = payload.get("refresh_token").and_then(Value::as_str) {
            object.insert("refresh_token".to_string(), json!(value));
        }
        object.insert(
            "expires_at_ms".to_string(),
            json!(now_ms + expires_in * 1000),
        );
        object.insert("acquired_at".to_string(), json!(now_iso_millis()));
    }
    atomic_write_json(&path, &token)?;
    Ok(access_token)
}

fn client_credentials_token(
    tenant: &str,
    client_id: &str,
    client_secret: &str,
) -> Result<String, Value> {
    let endpoint = format!(
        "https://login.microsoftonline.com/{}/oauth2/v2.0/token",
        encode_component(tenant)
    );
    let form = format!(
        "grant_type=client_credentials&client_id={}&client_secret={}&scope={}",
        encode_component(client_id),
        encode_component(client_secret),
        encode_component("https://graph.microsoft.com/.default")
    );
    let response = ureq::AgentBuilder::new()
        .timeout(Duration::from_secs(20))
        .build()
        .post(&endpoint)
        .set("Content-Type", "application/x-www-form-urlencoded")
        .send_string(&form)
        .map_err(|e| error("mailbox_graph_token_request_failed", &e.to_string()))?;
    let payload: Value = serde_json::from_str(
        &read_ureq_body(response).map_err(|e| error("mailbox_graph_token_request_failed", &e))?,
    )
    .map_err(|e| error("mailbox_graph_token_response_invalid", &e.to_string()))?;
    required_value_string(
        &payload,
        "access_token",
        "mailbox_graph_token_response_invalid",
    )
}

fn azure_cli_token(tenant: Option<&str>) -> Result<String, Value> {
    let mut command = Command::new("az");
    command.args([
        "account",
        "get-access-token",
        "--resource",
        "https://graph.microsoft.com",
        "--output",
        "json",
    ]);
    if let Some(tenant) = tenant {
        command.args(["--tenant", tenant]);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000);
    }
    let output = command.output().map_err(|e| {
        error(
            "mailbox_graph_login_unavailable",
            &format!("Graph delegated Microsoft login unavailable: {e}"),
        )
    })?;
    if !output.status.success() || output.stdout.len() as u64 > MAX_RESPONSE_BYTES {
        return Err(error(
            "mailbox_graph_login_unavailable",
            "Graph delegated Microsoft login unavailable: Azure CLI token request failed",
        ));
    }
    let payload: Value = serde_json::from_slice(&output.stdout)
        .map_err(|e| error("mailbox_graph_login_unavailable", &e.to_string()))?;
    required_value_string(&payload, "accessToken", "mailbox_graph_login_unavailable")
}

