fn auth_device_code_start(
    policy: &Policy,
    args: &Map<String, Value>,
    root: &Path,
) -> Result<Value, Value> {
    let (tenant_id, client_id, scope) = match device_code_policy(policy, args) {
        Ok(value) => value,
        Err(reason) => {
            record_audit(root, json!({"event_kind":"device_code_start_refused","reason":reason}))?;
            return Ok(json!({
                "schema":"narada.graph_mail_mcp.device_code_start.v1",
                "status":"refused",
                "reason":reason
            }));
        }
    };
    let endpoint = device_code_endpoint(&tenant_id, "devicecode");
    let (status, payload) = post_form(
        &endpoint,
        &[
            ("client_id", client_id.as_str()),
            ("scope", scope.as_str()),
        ],
    )?;
    if !(200..300).contains(&status) {
        return Err(unavailable(
            "ms_graph_device_code_start_failed",
            &format!("http_status={status}"),
        ));
    }
    let device_code = required_value_string(&payload, "device_code")?;
    let user_code = required_value_string(&payload, "user_code")?;
    let verification_uri = payload
        .get("verification_uri")
        .and_then(Value::as_str)
        .or_else(|| payload.get("verification_url").and_then(Value::as_str))
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| unavailable("ms_graph_device_code_response_missing_verification_uri", "verification URI missing"))?;
    let expires_in = payload
        .get("expires_in")
        .and_then(Value::as_u64)
        .unwrap_or(900);
    let interval = payload
        .get("interval")
        .and_then(Value::as_u64)
        .unwrap_or(5);
    let now_ms = now_millis();
    let flow_id = format!("flow_{}", Uuid::new_v4());
    let flow = json!({
        "schema":"narada.graph_mail_mcp.device_code_flow.v1",
        "flow_id":flow_id,
        "tenant_id":tenant_id,
        "client_id":client_id,
        "scope":scope,
        "device_code":device_code,
        "user_code":user_code,
        "verification_uri":verification_uri,
        "expires_at_ms":now_ms.saturating_add(seconds_millis(expires_in)),
        "interval_seconds":interval,
        "created_at":now_rfc3339()
    });
    write_bounded_json(&flow_path(root, &flow_id), &flow)?;
    record_audit(root, json!({"event_kind":"device_code_start_completed","flow_id":flow_id,"scope":scope,"expires_at_ms":now_ms.saturating_add(seconds_millis(expires_in))}))?;
    Ok(json!({
        "schema":"narada.graph_mail_mcp.device_code_start.v1",
        "status":"authorization_pending",
        "flow_id":flow_id,
        "user_code":user_code,
        "verification_uri":verification_uri,
        "expires_in":expires_in,
        "interval":interval,
        "message":payload.get("message").and_then(Value::as_str)
    }))
}

fn auth_device_code_poll(
    policy: &Policy,
    args: &Map<String, Value>,
    root: &Path,
) -> Result<Value, Value> {
    let flow_id = required_string(args, "flow_id")?;
    let flow = read_flow(root, &flow_id)?.ok_or_else(|| json!({
        "schema":"narada.graph_mail_mcp.device_code_poll.v1",
        "status":"refused",
        "reason":"device_code_flow_not_found",
        "flow_id":flow_id
    }))?;
    let flow_object = flow.as_object().ok_or_else(|| unavailable("device_code_flow_invalid", "flow is not an object"))?;
    let tenant_id = required_value_string(&flow, "tenant_id")?;
    let client_id = required_value_string(&flow, "client_id")?;
    let scope = required_value_string(&flow, "scope")?;
    if !policy.allow_device_code_auth {
        return Ok(json!({"schema":"narada.graph_mail_mcp.device_code_poll.v1","status":"refused","reason":"device_code_auth_disallowed_by_policy","flow_id":flow_id}));
    }
    if !policy.device_code_allowed_scopes.iter().any(|value| value == &scope) {
        return Ok(json!({"schema":"narada.graph_mail_mcp.device_code_poll.v1","status":"refused","reason":"device_code_scope_not_allowed","flow_id":flow_id}));
    }
    let expires_at_ms = flow_object.get("expires_at_ms").and_then(Value::as_i64).unwrap_or(0);
    if now_millis() >= expires_at_ms {
        return Ok(json!({"schema":"narada.graph_mail_mcp.device_code_poll.v1","status":"expired","flow_id":flow_id}));
    }
    let device_code = required_value_string(&flow, "device_code")?;
    let endpoint = device_code_endpoint(&tenant_id, "token");
    let (status, payload) = post_form(
        &endpoint,
        &[
            ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
            ("client_id", client_id.as_str()),
            ("device_code", device_code.as_str()),
        ],
    )?;
    if !(200..300).contains(&status) {
        let error_code = payload.get("error").and_then(Value::as_str);
        if error_code == Some("authorization_pending") || error_code == Some("slow_down") {
            let interval = flow_object
                .get("interval_seconds")
                .and_then(Value::as_u64)
                .unwrap_or(5);
            return Ok(json!({
                "schema":"narada.graph_mail_mcp.device_code_poll.v1",
                "status":error_code,
                "flow_id":flow_id,
                "interval":if error_code == Some("slow_down") { interval + 5 } else { interval },
                "expires_at_ms":expires_at_ms
            }));
        }
        if error_code == Some("invalid_client")
            && payload
                .get("error_description")
                .and_then(Value::as_str)
                .map(|value| value.contains("AADSTS7000218"))
                == Some(true)
        {
            record_audit(root, json!({"event_kind":"device_code_poll_refused","flow_id":flow_id,"reason":"device_code_client_must_be_public_client"}))?;
            return Ok(json!({
                "schema":"narada.graph_mail_mcp.device_code_poll.v1",
                "status":"refused",
                "reason":"device_code_client_must_be_public_client",
                "flow_id":flow_id,
                "recovery":"Configure device_code_client_id to an Entra public-client app with device-code/native-client support. Do not use a confidential client or client secret for device-code auth."
            }));
        }
        return Err(unavailable(
            "ms_graph_device_code_poll_failed",
            &format!("http_status={status}"),
        ));
    }
    let access_token = required_value_string(&payload, "access_token")?;
    let expires_in = payload
        .get("expires_in")
        .and_then(Value::as_u64)
        .unwrap_or(3599);
    let token = json!({
        "schema":"narada.graph_mail_mcp.delegated_token.v1",
        "auth_mode":"delegated_device_code",
        "tenant_id":tenant_id,
        "client_id":client_id,
        "scope":scope,
        "access_token":access_token,
        "refresh_token":payload.get("refresh_token").and_then(Value::as_str),
        "expires_at_ms":now_millis().saturating_add(seconds_millis(expires_in.max(60))),
        "acquired_at":now_rfc3339()
    });
    write_bounded_json(&delegated_token_path(root), &token)?;
    let token_expires = token.get("expires_at_ms").cloned().unwrap_or(Value::Null);
    record_audit(root, json!({"event_kind":"device_code_poll_completed","flow_id":flow_id,"scope":scope,"expires_at_ms":token_expires}))?;
    Ok(json!({
        "schema":"narada.graph_mail_mcp.device_code_poll.v1",
        "status":"authorized",
        "flow_id":flow_id,
        "auth_mode":"delegated_device_code",
        "scope":scope,
        "expires_at_ms":token_expires
    }))
}

fn auth_clear(args: &Map<String, Value>, root: &Path) -> Result<Value, Value> {
    if !confirmed(args, "confirm_clear", "confirmClear") {
        return Ok(json!({
            "schema":"narada.graph_mail_mcp.auth_clear.v1",
            "status":"refused",
            "reason":"confirm_clear_required"
        }));
    }
    let path = delegated_token_path(root);
    let mut removed = 0u64;
    if path.exists() {
        fs::remove_file(&path)
            .map_err(|error| unavailable("graph_mail_auth_clear_failed", &error.to_string()))?;
        removed = 1;
    }
    record_audit(root, json!({"event_kind":"device_code_auth_cleared","removed":removed}))?;
    Ok(json!({
        "schema":"narada.graph_mail_mcp.auth_clear.v1",
        "status":"cleared",
        "removed":removed
    }))
}

fn device_code_policy(
    policy: &Policy,
    args: &Map<String, Value>,
) -> Result<(String, String, String), &'static str> {
    if !policy.allow_device_code_auth {
        return Err("device_code_auth_disallowed_by_policy");
    }
    let tenant_id = policy
        .device_code_tenant_id
        .clone()
        .or_else(|| std::env::var("GRAPH_TENANT_ID").ok().filter(|value| !value.trim().is_empty()))
        .ok_or("device_code_tenant_id_required")?;
    let client_id = policy
        .device_code_client_id
        .clone()
        .or_else(|| std::env::var("GRAPH_CLIENT_ID").ok().filter(|value| !value.trim().is_empty()))
        .ok_or("device_code_client_id_required")?;
    let scope = optional_string(args, "scope")
        .or_else(|| (policy.device_code_allowed_scopes.len() == 1).then(|| policy.device_code_allowed_scopes[0].clone()))
        .ok_or("device_code_scope_required")?;
    if !policy.device_code_allowed_scopes.iter().any(|value| value == &scope) {
        return Err("device_code_scope_not_allowed");
    }
    Ok((tenant_id, client_id, scope))
}

fn post_form(endpoint: &str, fields: &[(&str, &str)]) -> Result<(u16, Value), Value> {
    let insecure_test = std::env::var("NARADA_GRAPH_MAIL_ALLOW_INSECURE_TEST").ok().as_deref() == Some("1")
        && endpoint.starts_with("http://127.0.0.1:");
    if !endpoint.starts_with("https://login.microsoftonline.com/") && !insecure_test {
        return Err(unavailable(
            "graph_auth_endpoint_not_allowed",
            "device-code authority requires login.microsoftonline.com",
        ));
    }
    let form = fields
        .iter()
        .map(|(key, value)| format!("{}={}", encode_component(key), encode_component(value)))
        .collect::<Vec<_>>()
        .join("&");
    let response = ureq::AgentBuilder::new()
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .post(endpoint)
        .set("Accept", "application/json")
        .set("Content-Type", "application/x-www-form-urlencoded")
        .send_string(&form);
    match response {
        Ok(response) => read_auth_response(response),
        Err(ureq::Error::Status(_, response)) => read_auth_response(response),
        Err(error) => Err(unavailable("graph_auth_request_failed", &error.to_string())),
    }
}

fn device_code_endpoint(tenant_id: &str, operation: &str) -> String {
    if std::env::var("NARADA_GRAPH_MAIL_ALLOW_INSECURE_TEST").ok().as_deref() == Some("1") {
        if let Ok(base) = std::env::var("NARADA_GRAPH_MAIL_DEVICE_CODE_ENDPOINT") {
            if !base.trim().is_empty() {
                return format!("{}/{}", base.trim_end_matches('/'), operation);
            }
        }
    }
    format!(
        "https://login.microsoftonline.com/{}/oauth2/v2.0/{}",
        encode_component(tenant_id),
        operation
    )
}

