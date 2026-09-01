fn request_delegated_refresh(
    tenant_id: &str,
    client_id: &str,
    scope: &str,
    refresh_token: &str,
) -> Result<Value, Value> {
    let endpoint = format!(
        "https://login.microsoftonline.com/{}/oauth2/v2.0/token",
        encode_component(tenant_id)
    );
    validate_token_endpoint(&endpoint)?;
    let form = format!(
        "client_id={}&scope={}&refresh_token={}&grant_type=refresh_token",
        encode_component(client_id),
        encode_component(scope),
        encode_component(refresh_token),
    );
    let response = ureq::AgentBuilder::new()
        .timeout(Duration::from_secs(30))
        .build()
        .post(&endpoint)
        .set("Accept", "application/json")
        .set("Content-Type", "application/x-www-form-urlencoded")
        .send_string(&form);
    let (status, body) = match response {
        Ok(response) => read_response_body(response)?,
        Err(ureq::Error::Status(code, response)) => {
            let (_, body) = read_response_body(response)?;
            return Err(unavailable(
                "ms_graph_delegated_token_refresh_failed",
                &format!("http_status={code};body={}", redact(&body)),
            ));
        }
        Err(error) => {
            return Err(unavailable(
                "ms_graph_delegated_token_refresh_failed",
                &error.to_string(),
            ))
        }
    };
    if !(200..300).contains(&status) {
        return Err(unavailable(
            "ms_graph_delegated_token_refresh_failed",
            &format!("http_status={status};body={}", redact(&body)),
        ));
    }
    serde_json::from_str(&body).map_err(|_| {
        unavailable(
            "ms_graph_delegated_token_refresh_invalid",
            "refresh response is not JSON",
        )
    })
}

fn persist_delegated_token(path: &Path, token: &Value) -> Result<(), Value> {
    let text = serde_json::to_string_pretty(token).map_err(|error| {
        unavailable(
            "graph_delegated_token_persist_failed",
            &error.to_string(),
        )
    })?;
    if text.len() as u64 > MAX_CONFIG_BYTES {
        return Err(unavailable(
            "graph_delegated_token_persist_failed",
            "delegated token exceeds bounded size",
        ));
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|error| {
            unavailable(
                "graph_delegated_token_persist_failed",
                &error.to_string(),
            )
        })?;
    }
    fs::write(path, text).map_err(|error| {
        unavailable(
            "graph_delegated_token_persist_failed",
            &error.to_string(),
        )
    })
}

fn record_calendar_audit(root: &Path, event: Value) -> Result<(), Value> {
    let audit_path = root.join(".ai/audit/calendar-mcp.jsonl");
    if let Some(parent) = audit_path.parent() {
        fs::create_dir_all(parent)
            .map_err(|error| unavailable("calendar_audit_write_failed", &error.to_string()))?;
    }
    let recorded_at = OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .unwrap_or_else(|_| "unknown".to_string());
    let mut object = event.as_object().cloned().unwrap_or_default();
    object.insert("schema".to_string(), json!("narada.calendar_mcp.audit.v1"));
    object.insert("recorded_at".to_string(), json!(recorded_at));
    let line = serde_json::to_string(&Value::Object(object))
        .map_err(|error| unavailable("calendar_audit_encode_failed", &error.to_string()))?;
    if line.len() > MAX_AUDIT_BYTES {
        return Err(unavailable(
            "calendar_audit_record_too_large",
            &MAX_AUDIT_BYTES.to_string(),
        ));
    }
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&audit_path)
        .map_err(|error| unavailable("calendar_audit_write_failed", &error.to_string()))?;
    file.write_all(line.as_bytes())
        .and_then(|_| file.write_all(b"\n"))
        .map_err(|error| unavailable("calendar_audit_write_failed", &error.to_string()))
}

fn request_client_credentials(
    endpoint: &str,
    client_id: &str,
    client_secret: &str,
) -> Result<String, Value> {
    validate_token_endpoint(endpoint)?;
    let form = format!(
        "client_id={}&client_secret={}&scope={}&grant_type=client_credentials",
        encode_component(client_id),
        encode_component(client_secret),
        encode_component(DEFAULT_TOKEN_SCOPE),
    );
    let response = ureq::AgentBuilder::new()
        .timeout(Duration::from_secs(30))
        .build()
        .post(endpoint)
        .set("Accept", "application/json")
        .set("Content-Type", "application/x-www-form-urlencoded")
        .send_string(&form);
    let (status, body) = match response {
        Ok(response) => read_response_body(response)?,
        Err(ureq::Error::Status(code, response)) => {
            let (_, body) = read_response_body(response)?;
            return Err(unavailable(
                "ms_graph_token_request_failed",
                &format!("http_status={code};body={}", redact(&body)),
            ));
        }
        Err(error) => {
            return Err(unavailable(
                "ms_graph_token_request_failed",
                &error.to_string(),
            ))
        }
    };
    if !(200..300).contains(&status) {
        return Err(unavailable(
            "ms_graph_token_request_failed",
            &format!("http_status={status};body={}", redact(&body)),
        ));
    }
    let payload = serde_json::from_str::<Value>(&body).map_err(|_| {
        unavailable(
            "ms_graph_token_response_invalid_json",
            "token response is not JSON",
        )
    })?;
    payload
        .get("access_token")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .map(ToOwned::to_owned)
        .ok_or_else(|| {
            unavailable(
                "ms_graph_token_response_missing_access_token",
                "access_token missing",
            )
        })
}

fn validate_token_endpoint(value: &str) -> Result<(), Value> {
    let allowed = value.starts_with("https://login.microsoftonline.com/")
        || (std::env::var("NARADA_NATIVE_GRAPH_ALLOW_INSECURE_TEST")
            .ok()
            .as_deref()
            == Some("1")
            && value.starts_with("http://127.0.0.1:"));
    if !allowed {
        return Err(unavailable(
            "graph_token_endpoint_not_allowed",
            "token authority requires login.microsoftonline.com or an explicit loopback test override",
        ));
    }
    Ok(())
}

fn load_environment(root: &Path) -> HashMap<String, String> {
    let mut values = HashMap::new();
    if let Some(parent) = root.parent() {
        load_env_file(&mut values, &parent.join(".env"));
    }
    load_env_file(&mut values, &root.join(".env"));
    for (key, value) in std::env::vars() {
        values.insert(key, value);
    }
    values
}

fn load_env_file(values: &mut HashMap<String, String>, path: &Path) {
    let Ok(metadata) = fs::metadata(path) else {
        return;
    };
    if metadata.len() > MAX_ENV_BYTES {
        return;
    }
    let Ok(text) = fs::read_to_string(path) else {
        return;
    };
    for line in text.lines() {
        let Some((key, raw_value)) = line.split_once('=') else {
            continue;
        };
        let key = key.trim();
        if key.is_empty() || key.starts_with('#') {
            continue;
        }
        let mut value = raw_value.trim().to_string();
        if value.len() >= 2
            && ((value.starts_with('"') && value.ends_with('"'))
                || (value.starts_with('\'') && value.ends_with('\'')))
        {
            value = value[1..value.len() - 1].to_string();
        }
        values.insert(key.to_string(), value);
    }
}

fn non_empty<'a>(values: &'a HashMap<String, String>, key: &str) -> Option<&'a str> {
    values
        .get(key)
        .map(String::as_str)
        .filter(|value| !value.trim().is_empty())
}

fn scalar_query_value(value: &Value) -> Option<String> {
    value
        .as_str()
        .map(ToOwned::to_owned)
        .or_else(|| value.as_i64().map(|value| value.to_string()))
        .or_else(|| value.as_u64().map(|value| value.to_string()))
        .or_else(|| value.as_f64().map(|value| value.to_string()))
        .or_else(|| value.as_bool().map(|value| value.to_string()))
}

fn parse_response(status: u16, response: ureq::Response) -> Result<Value, Value> {
    let (_, body) = read_response_body(response)?;
    if body.trim().is_empty() || status == 202 || status == 204 {
        return Ok(json!({"status":"accepted","http_status":status}));
    }
    match serde_json::from_str::<Value>(&body) {
        Ok(value) => Ok(value),
        Err(_) => Ok(json!({"status":"ok","text":body})),
    }
}

fn read_response_body(response: ureq::Response) -> Result<(u16, String), Value> {
    let status = response.status();
    let mut reader = response.into_reader().take(MAX_RESPONSE_BYTES + 1);
    let mut body = Vec::new();
    reader
        .read_to_end(&mut body)
        .map_err(|error| unavailable("graph_response_read_failed", &error.to_string()))?;
    if body.len() as u64 > MAX_RESPONSE_BYTES {
        return Err(unavailable(
            "graph_response_too_large",
            &MAX_RESPONSE_BYTES.to_string(),
        ));
    }
    Ok((status, String::from_utf8_lossy(&body).to_string()))
}

