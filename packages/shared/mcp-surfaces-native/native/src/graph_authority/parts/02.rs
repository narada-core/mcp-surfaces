impl CalendarGraphAdapter {
    pub fn from_site_root(root: &Path) -> Result<Self, Value> {
        Self::from_config(root, ".ai/calendar-mcp.json")
    }

    pub fn from_config(root: &Path, relative_config_path: &str) -> Result<Self, Value> {
        let config_path = root.join(relative_config_path);
        let config = if config_path.exists() {
            let metadata = fs::metadata(&config_path)
                .map_err(|error| unavailable("calendar_config_read_failed", &error.to_string()))?;
            if metadata.len() > MAX_CONFIG_BYTES {
                return Err(unavailable(
                    "calendar_config_too_large",
                    "calendar policy exceeds bounded size",
                ));
            }
            let text = fs::read_to_string(&config_path)
                .map_err(|error| unavailable("calendar_config_read_failed", &error.to_string()))?;
            serde_json::from_str::<Value>(&text)
                .map_err(|error| unavailable("calendar_config_invalid", &error.to_string()))?
        } else {
            json!({})
        };
        let object = config
            .as_object()
            .cloned()
            .ok_or_else(|| unavailable("graph_config_invalid", "policy must be a JSON object"))?;
        let base_url = object
            .get("graph_base_url")
            .and_then(Value::as_str)
            .filter(|value| !value.trim().is_empty())
            .unwrap_or(DEFAULT_GRAPH_BASE_URL)
            .trim_end_matches('/')
            .to_string();
        validate_base_url(&base_url)?;
        let allowed_mailboxes: Vec<String> = object
            .get("allowed_mailboxes")
            .or_else(|| object.get("allowedMailboxes"))
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(Value::as_str)
                    .filter(|value| !value.trim().is_empty())
                    .map(ToOwned::to_owned)
                    .collect()
            })
            .unwrap_or_default();
        if allowed_mailboxes.len() > 32 || allowed_mailboxes.iter().any(|value| value.len() > 320) {
            return Err(unavailable(
                "graph_allowed_mailboxes_invalid",
                "allowed_mailboxes permits at most 32 values of at most 320 bytes",
            ));
        }
        let request_timeout_ms = object
            .get("request_timeout_ms")
            .or_else(|| object.get("requestTimeoutMs"))
            .and_then(Value::as_u64)
            .unwrap_or(30_000);
        if !(100..=60_000).contains(&request_timeout_ms) {
            return Err(unavailable(
                "graph_request_timeout_invalid",
                "request_timeout_ms must be between 100 and 60000",
            ));
        }
        let allow_event_writes = object
            .get("allow_event_writes")
            .and_then(Value::as_bool)
            .unwrap_or(false)
            || object
                .get("allowEventWrites")
                .and_then(Value::as_bool)
                .unwrap_or(false);
        let write_approval_token = object
            .get("write_approval_token")
            .or_else(|| object.get("writeApprovalToken"))
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .map(ToOwned::to_owned);
        let environment = load_environment(root);
        let auth = if relative_config_path == ".ai/graph-mail-mcp.json" {
            resolve_auth_with_delegated_token(root, &environment)
        } else {
            resolve_auth(&environment)
        };
        Ok(Self {
            base_url,
            allowed_mailboxes,
            allow_event_writes,
            write_approval_token,
            auth,
            request_timeout: Duration::from_millis(request_timeout_ms),
        })
    }

    pub fn write_allowed(&self, args: &Map<String, Value>) -> Result<(), &'static str> {
        if !self.allow_event_writes {
            return Err("event_writes_disallowed_by_policy");
        }
        let confirmed = args
            .get("confirm_write")
            .and_then(Value::as_bool)
            .or_else(|| args.get("confirmWrite").and_then(Value::as_bool))
            .unwrap_or(false);
        if !confirmed {
            return Err("confirm_write_required");
        }
        if let Some(expected) = self.write_approval_token.as_deref() {
            if args.get("approval_token").and_then(Value::as_str) != Some(expected) {
                return Err("write_approval_token_required");
            }
        }
        Ok(())
    }

    pub fn request(
        &self,
        method: &str,
        mailbox_id: Option<&str>,
        suffix: &str,
        query: &Map<String, Value>,
        body: Option<&Value>,
    ) -> Result<Value, Value> {
        self.request_with_headers(method, mailbox_id, suffix, query, body, &Map::new())
    }

    pub fn request_with_headers(
        &self,
        method: &str,
        mailbox_id: Option<&str>,
        suffix: &str,
        query: &Map<String, Value>,
        body: Option<&Value>,
        headers: &Map<String, Value>,
    ) -> Result<Value, Value> {
        let method = method.to_ascii_uppercase();
        if !matches!(method.as_str(), "GET" | "POST" | "PATCH" | "DELETE") {
            return Err(unavailable("graph_method_not_allowed", &method));
        }
        let url = self.build_url(mailbox_id, suffix, query)?;
        let agent = ureq::AgentBuilder::new()
            .timeout(self.request_timeout)
            .build();
        let request = agent
            .request(&method, &url)
            .set(
                "Authorization",
                &format!("Bearer {}", self.auth.access_token()?),
            )
            .set("Accept", "application/json");
        let mut request = request;
        for (key, value) in headers {
            if let Some(value) = value.as_str() {
                request = request.set(key, value);
            }
        }
        let response = if let Some(body) = body {
            let encoded = serde_json::to_vec(body)
                .map_err(|error| unavailable("graph_request_encode_failed", &error.to_string()))?;
            if encoded.len() > MAX_REQUEST_BYTES {
                return Err(unavailable(
                    "graph_request_too_large",
                    &MAX_REQUEST_BYTES.to_string(),
                ));
            }
            request
                .set("Content-Type", "application/json")
                .send_bytes(&encoded)
        } else {
            request.call()
        };
        match response {
            Ok(response) => parse_response(response.status(), response),
            Err(ureq::Error::Status(code, response)) => Err(http_error(code, response)),
            Err(error) => Err(unavailable("graph_request_failed", &error.to_string())),
        }
    }

    pub fn request_upload_bytes(
        &self,
        method: &str,
        upload_url: &str,
        body: &[u8],
        headers: &Map<String, Value>,
    ) -> Result<(u16, Value), Value> {
        if method.to_ascii_uppercase() != "PUT" {
            return Err(unavailable("graph_upload_method_not_allowed", method));
        }
        validate_upload_url(upload_url)?;
        if body.len() > MAX_UPLOAD_REQUEST_BYTES {
            return Err(unavailable(
                "graph_upload_request_too_large",
                &MAX_UPLOAD_REQUEST_BYTES.to_string(),
            ));
        }
        let agent = ureq::AgentBuilder::new()
            .timeout(self.request_timeout)
            .build();
        let mut request = agent
            .request("PUT", upload_url)
            .set("Accept", "application/json");
        for (key, value) in headers {
            if let Some(value) = value.as_str() {
                request = request.set(key, value);
            }
        }
        let response = match request.send_bytes(body) {
            Ok(response) => response,
            Err(ureq::Error::Status(code, response)) => {
                return Err(http_error(code, response));
            }
            Err(error) => {
                return Err(unavailable(
                    "graph_upload_request_failed",
                    &error.to_string(),
                ))
            }
        };
        let status = response.status();
        let (_, body) = read_response_body(response)?;
        let value = if body.trim().is_empty() || status == 202 || status == 204 {
            json!({"status":"accepted","http_status":status})
        } else {
            serde_json::from_str::<Value>(&body)
                .unwrap_or_else(|_| json!({"status":"ok","text":body}))
        };
        Ok((status, value))
    }

    pub fn build_url(
        &self,
        mailbox_id: Option<&str>,
        suffix: &str,
        query: &Map<String, Value>,
    ) -> Result<String, Value> {
        let mailbox = mailbox_id
            .filter(|value| !value.trim().is_empty())
            .or_else(|| {
                if self.allowed_mailboxes.len() == 1 {
                    self.allowed_mailboxes.first().map(String::as_str)
                } else {
                    None
                }
            })
            .unwrap_or("me");
        if !self.allowed_mailboxes.is_empty()
            && !self.allowed_mailboxes.iter().any(|value| value == mailbox)
        {
            return Err(unavailable("mailbox_not_allowed", mailbox));
        }
        if suffix.contains("..") || suffix.starts_with('/') {
            return Err(unavailable("graph_path_not_allowed", suffix));
        }
        let prefix = if mailbox == "me" {
            "/me".to_string()
        } else {
            format!("/users/{}", encode_component(mailbox))
        };
        let mut url = format!("{}{}/{}", self.base_url, prefix, suffix.trim_matches('/'));
        let mut first = true;
        for (key, value) in query {
            let Some(value) = scalar_query_value(value) else {
                continue;
            };
            if value.is_empty() {
                continue;
            }
            url.push(if first { '?' } else { '&' });
            first = false;
            url.push_str(&encode_component(key));
            url.push('=');
            url.push_str(&encode_component(&value));
        }
        if url.len() > MAX_URL_BYTES {
            return Err(unavailable(
                "graph_url_too_large",
                &MAX_URL_BYTES.to_string(),
            ));
        }
        Ok(url)
    }
}

