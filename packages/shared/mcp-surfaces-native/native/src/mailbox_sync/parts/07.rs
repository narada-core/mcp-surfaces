fn walk_delta<F>(
    scope: &ScopeConfig,
    token: &str,
    start_url: &str,
    folder: &str,
    heartbeat: &mut F,
) -> Result<(Vec<Value>, String), GraphWalkError>
where
    F: FnMut() -> Result<(), Value>,
{
    let mut url = start_url.to_string();
    let mut values = Vec::new();
    let mut delta_link = None;
    for _ in 0..MAX_GRAPH_PAGES {
        validate_graph_page_url(&url, &scope.graph.base_url).map_err(GraphWalkError::Failure)?;
        heartbeat().map_err(GraphWalkError::Failure)?;
        let page = match graph_get(scope, token, &url) {
            Ok(value) => value,
            Err(GraphHttpError::Status(410, _)) => return Err(GraphWalkError::Stale),
            Err(value) => return Err(GraphWalkError::Failure(value.into_value())),
        };
        let page_values = page.get("value").and_then(Value::as_array).ok_or_else(|| {
            GraphWalkError::Failure(error(
                "mailbox_graph_delta_response_invalid",
                "mailbox_graph_delta_response_invalid",
            ))
        })?;
        if values.len() + page_values.len() > MAX_GRAPH_RECORDS {
            return Err(GraphWalkError::Failure(error(
                "mailbox_graph_record_limit_exceeded",
                "mailbox_graph_record_limit_exceeded",
            )));
        }
        for raw in page_values {
            let mut message = raw.as_object().cloned().ok_or_else(|| {
                GraphWalkError::Failure(error(
                    "mailbox_graph_delta_message_invalid",
                    "mailbox_graph_delta_message_invalid",
                ))
            })?;
            message.insert("sourceQueriedFolderRef".to_string(), json!(folder));
            if scope.attachment_policy != "exclude"
                && message.get("@removed").is_none()
                && message.get("hasAttachments").and_then(Value::as_bool) == Some(true)
                && message
                    .get("attachments")
                    .and_then(Value::as_array)
                    .map(|value| value.is_empty())
                    .unwrap_or(true)
            {
                if let Some(message_id) = message.get("id").and_then(Value::as_str) {
                    let attachments = fetch_attachments(scope, token, message_id, heartbeat)
                        .map_err(GraphWalkError::Failure)?;
                    message.insert("attachments".to_string(), Value::Array(attachments));
                }
            }
            values.push(Value::Object(message));
        }
        delta_link = page
            .get("@odata.deltaLink")
            .and_then(Value::as_str)
            .map(ToString::to_string)
            .or(delta_link);
        if let Some(next) = page.get("@odata.nextLink").and_then(Value::as_str) {
            url = next.to_string();
        } else {
            return delta_link.map(|value| (values, value)).ok_or_else(|| {
                GraphWalkError::Failure(error(
                    "mailbox_graph_delta_link_missing",
                    "Delta query did not return @odata.deltaLink",
                ))
            });
        }
    }
    Err(GraphWalkError::Failure(error(
        "mailbox_graph_page_limit_exceeded",
        "mailbox_graph_page_limit_exceeded",
    )))
}

fn fetch_attachments<F>(
    scope: &ScopeConfig,
    token: &str,
    message_id: &str,
    heartbeat: &mut F,
) -> Result<Vec<Value>, Value>
where
    F: FnMut() -> Result<(), Value>,
{
    let mut url = format!(
        "{}{}/messages/{}/attachments",
        scope.graph.base_url,
        graph_mailbox_path(&scope.graph.user_id),
        encode_component(message_id)
    );
    let mut attachments = Vec::new();
    for _ in 0..MAX_GRAPH_PAGES {
        validate_graph_page_url(&url, &scope.graph.base_url)?;
        heartbeat()?;
        let page = graph_get(scope, token, &url).map_err(GraphHttpError::into_value)?;
        let values = page.get("value").and_then(Value::as_array).ok_or_else(|| {
            error(
                "mailbox_graph_attachment_response_invalid",
                "mailbox_graph_attachment_response_invalid",
            )
        })?;
        if attachments.len() + values.len() > MAX_GRAPH_RECORDS {
            return Err(error(
                "mailbox_graph_attachment_limit_exceeded",
                "mailbox_graph_attachment_limit_exceeded",
            ));
        }
        attachments.extend(values.iter().cloned());
        if let Some(next) = page.get("@odata.nextLink").and_then(Value::as_str) {
            url = next.to_string();
        } else {
            return Ok(attachments);
        }
    }
    Err(error(
        "mailbox_graph_page_limit_exceeded",
        "mailbox_graph_page_limit_exceeded",
    ))
}

enum GraphHttpError {
    Status(u16, String),
    Failure(String),
}

impl GraphHttpError {
    fn into_value(self) -> Value {
        match self {
            Self::Status(status, body) => error(
                "mailbox_graph_request_failed",
                &format!("Graph API error ({status}): {}", bounded_error(&body)),
            ),
            Self::Failure(message) => error("mailbox_graph_request_failed", &message),
        }
    }
}

fn graph_get(scope: &ScopeConfig, token: &str, url: &str) -> Result<Value, GraphHttpError> {
    let agent = ureq::AgentBuilder::new()
        .timeout(Duration::from_millis(scope.graph.request_timeout_ms))
        .build();
    let mut request = agent
        .get(url)
        .set("Authorization", &format!("Bearer {token}"))
        .set("Accept", "application/json");
    if scope.graph.prefer_immutable_ids {
        request = request.set("Prefer", "IdType=\"ImmutableId\"");
    }
    let response = match request.call() {
        Ok(response) => response,
        Err(ureq::Error::Status(status, response)) => {
            let body = read_ureq_body(response).unwrap_or_default();
            return Err(GraphHttpError::Status(status, body));
        }
        Err(value) => return Err(GraphHttpError::Failure(value.to_string())),
    };
    let body = read_ureq_body(response).map_err(GraphHttpError::Failure)?;
    serde_json::from_str(&body)
        .map_err(|e| GraphHttpError::Failure(format!("mailbox_graph_response_invalid:{e}")))
}

fn graph_access_token(scope: &ScopeConfig) -> Result<String, Value> {
    if scope.graph.auth_mode.as_deref() == Some("delegated_token_store") {
        return delegated_graph_access_token(scope);
    }
    if let Some(token) = non_empty_env("GRAPH_ACCESS_TOKEN") {
        return Ok(token);
    }
    let tenant = non_empty_env("GRAPH_TENANT_ID").or_else(|| scope.graph.tenant_id.clone());
    let client_id = non_empty_env("GRAPH_CLIENT_ID").or_else(|| scope.graph.client_id.clone());
    let client_secret =
        non_empty_env("GRAPH_CLIENT_SECRET").or_else(|| scope.graph.client_secret.clone());
    if let (Some(tenant), Some(client_id), Some(client_secret)) = (tenant, client_id, client_secret)
    {
        return client_credentials_token(&tenant, &client_id, &client_secret);
    }
    azure_cli_token(scope.graph.tenant_id.as_deref())
}

