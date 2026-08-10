use crate::calendar::provider::{build_request, wrap_result};
use serde_json::{json, Map, Value};
use std::collections::HashMap;
use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::path::Path;
use std::time::Duration;
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;

const DEFAULT_GRAPH_BASE_URL: &str = "https://graph.microsoft.com/v1.0";
const DEFAULT_TOKEN_SCOPE: &str = "https://graph.microsoft.com/.default";
const MAX_CONFIG_BYTES: u64 = 512 * 1024;
const MAX_ENV_BYTES: u64 = 512 * 1024;
const MAX_REQUEST_BYTES: usize = 512 * 1024;
const MAX_RESPONSE_BYTES: u64 = 4 * 1024 * 1024;
const MAX_URL_BYTES: usize = 16 * 1024;
const MAX_AUDIT_BYTES: usize = 64 * 1024;

enum GraphAuth {
    AccessToken(String),
    ClientCredentials {
        endpoint: String,
        client_id: String,
        client_secret: String,
    },
    Missing,
}

/// Native provider authority for the calendar surface.  It is activated only
/// when the caller explicitly sets `NARADA_NATIVE_GRAPH_AUTHORITY=1`; the
/// runtime matrix therefore keeps Bun in charge until parity is admitted.
pub struct CalendarGraphAdapter {
    base_url: String,
    allowed_mailboxes: Vec<String>,
    allow_event_writes: bool,
    write_approval_token: Option<String>,
    auth: GraphAuth,
}

impl GraphAuth {
    fn access_token(&self) -> Result<String, Value> {
        match self {
            Self::AccessToken(value) => Ok(value.clone()),
            Self::ClientCredentials {
                endpoint,
                client_id,
                client_secret,
            } => request_client_credentials(endpoint, client_id, client_secret),
            Self::Missing => Err(unavailable(
                "graph_access_token_missing",
                "set MS_GRAPH_ACCESS_TOKEN or GRAPH_TENANT_ID/GRAPH_CLIENT_ID/GRAPH_CLIENT_SECRET",
            )),
        }
    }
}

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
        let object = config.as_object().cloned().unwrap_or_default();
        let base_url = object
            .get("graph_base_url")
            .and_then(Value::as_str)
            .filter(|value| !value.trim().is_empty())
            .unwrap_or(DEFAULT_GRAPH_BASE_URL)
            .trim_end_matches('/')
            .to_string();
        validate_base_url(&base_url)?;
        let allowed_mailboxes = object
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
        let auth = resolve_auth(&environment);
        Ok(Self {
            base_url,
            allowed_mailboxes,
            allow_event_writes,
            write_approval_token,
            auth,
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
            .timeout(Duration::from_secs(30))
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

fn unavailable(reason: &str, detail: &str) -> Value {
    json!({
        "schema":"narada.graph_authority.error.v1",
        "status":"unavailable",
        "reason":reason,
        "detail":detail,
        "remediation":"Configure the bounded native Graph authority and retry."
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encoding_is_url_safe() {
        assert_eq!(encode_component("user@example.com"), "user%40example.com");
        assert_eq!(encode_component("a b"), "a%20b");
    }

    #[test]
    fn production_graph_base_is_restricted() {
        assert!(validate_base_url(DEFAULT_GRAPH_BASE_URL).is_ok());
        assert!(validate_base_url("http://example.invalid/v1.0").is_err());
    }

    #[test]
    fn calendar_url_respects_mailbox_allowlist_and_bounds() {
        let adapter = CalendarGraphAdapter {
            base_url: DEFAULT_GRAPH_BASE_URL.to_string(),
            allowed_mailboxes: vec!["user@example.com".to_string()],
            allow_event_writes: false,
            write_approval_token: None,
            auth: GraphAuth::AccessToken("test".to_string()),
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
