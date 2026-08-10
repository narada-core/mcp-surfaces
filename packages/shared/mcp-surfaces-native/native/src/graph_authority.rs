use serde_json::{json, Map, Value};
use std::fs;
use std::io::Read;
use std::path::Path;
use std::time::Duration;

const DEFAULT_GRAPH_BASE_URL: &str = "https://graph.microsoft.com/v1.0";
const MAX_CONFIG_BYTES: u64 = 512 * 1024;
const MAX_RESPONSE_BYTES: u64 = 4 * 1024 * 1024;

pub struct CalendarGraphAdapter {
    base_url: String,
    allowed_mailboxes: Vec<String>,
    access_token: String,
}

impl CalendarGraphAdapter {
    pub fn from_site_root(root: &Path) -> Result<Self, Value> {
        let config_path = root.join(".ai/calendar-mcp.json");
        let config = if config_path.exists() {
            let metadata = fs::metadata(&config_path).map_err(|e| unavailable("calendar_config_read_failed", &e.to_string()))?;
            if metadata.len() > MAX_CONFIG_BYTES { return Err(unavailable("calendar_config_too_large", "calendar policy exceeds bounded size")); }
            let text = fs::read_to_string(&config_path).map_err(|e| unavailable("calendar_config_read_failed", &e.to_string()))?;
            serde_json::from_str::<Value>(&text).map_err(|e| unavailable("calendar_config_invalid", &e.to_string()))?
        } else { json!({}) };
        let object = config.as_object().cloned().unwrap_or_default();
        let base_url = object.get("graph_base_url").and_then(Value::as_str).filter(|v| !v.trim().is_empty()).unwrap_or(DEFAULT_GRAPH_BASE_URL).trim_end_matches('/').to_string();
        validate_base_url(&base_url)?;
        let allowed_mailboxes = object.get("allowed_mailboxes").or_else(|| object.get("allowedMailboxes")).and_then(Value::as_array).map(|items| items.iter().filter_map(Value::as_str).map(ToOwned::to_owned).collect()).unwrap_or_default();
        let access_token = std::env::var("MS_GRAPH_ACCESS_TOKEN").or_else(|_| std::env::var("GRAPH_ACCESS_TOKEN")).ok().filter(|v| !v.trim().is_empty()).ok_or_else(|| unavailable("graph_access_token_missing", "set MS_GRAPH_ACCESS_TOKEN or GRAPH_ACCESS_TOKEN"))?;
        Ok(Self { base_url, allowed_mailboxes, access_token })
    }

    pub fn get(&self, mailbox_id: Option<&str>, suffix: &str, query: &Map<String, Value>) -> Result<Value, Value> {
        let url = self.build_url(mailbox_id, suffix, query)?;
        let response = ureq::AgentBuilder::new().timeout(Duration::from_secs(30)).build().get(&url).set("Authorization", &format!("Bearer {}", self.access_token)).set("Accept", "application/json").call();
        let response = match response { Ok(response) => response, Err(ureq::Error::Status(code, response)) => return Err(http_error(code, response)), Err(error) => return Err(unavailable("graph_request_failed", &error.to_string())) };
        let status = response.status();
        let mut reader = response.into_reader().take(MAX_RESPONSE_BYTES);
        let mut body = Vec::new();
        reader.read_to_end(&mut body).map_err(|e| unavailable("graph_response_read_failed", &e.to_string()))?;
        if body.len() as u64 >= MAX_RESPONSE_BYTES { return Err(unavailable("graph_response_too_large", &MAX_RESPONSE_BYTES.to_string())); }
        if body.is_empty() || status == 204 { return Ok(json!({"status":"accepted","http_status":status})); }
        serde_json::from_slice(&body).map_err(|e| unavailable("graph_response_invalid_json", &e.to_string()))
    }

    pub fn build_url(&self, mailbox_id: Option<&str>, suffix: &str, query: &Map<String, Value>) -> Result<String, Value> {
        let mailbox = mailbox_id.filter(|v| !v.trim().is_empty()).or_else(|| self.allowed_mailboxes.first().map(String::as_str)).unwrap_or("me");
        if !self.allowed_mailboxes.is_empty() && !self.allowed_mailboxes.iter().any(|value| value == mailbox) { return Err(unavailable("mailbox_not_allowed", mailbox)); }
        let prefix = if mailbox == "me" { "/me".to_string() } else { format!("/users/{}", encode_segment(mailbox)) };
        let mut url = format!("{}{}/{}", self.base_url, prefix, suffix.trim_matches('/'));
        let mut first = true;
        for (key, value) in query {
            let value = if let Some(value) = value.as_str() { value.to_string() } else if let Some(value) = value.as_i64() { value.to_string() } else { continue; };
            if value.is_empty() { continue; }
            url.push(if first { '?' } else { '&' }); first = false;
            url.push_str(&encode_component(key)); url.push('='); url.push_str(&encode_component(&value));
        }
        Ok(url)
    }
}

fn validate_base_url(value: &str) -> Result<(), Value> {
    let allowed = value.starts_with("https://graph.microsoft.com/") || (std::env::var("NARADA_NATIVE_GRAPH_ALLOW_INSECURE_TEST").ok().as_deref() == Some("1") && value.starts_with("http://127.0.0.1:"));
    if !allowed { return Err(unavailable("graph_base_url_not_allowed", "Graph authority requires https://graph.microsoft.com or an explicit loopback test override")); }
    Ok(())
}

fn encode_segment(value: &str) -> String { encode_component(value) }

fn encode_component(value: &str) -> String {
    value.bytes().map(|byte| match byte { b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => (byte as char).to_string(), _ => format!("%{byte:02X}") }).collect()
}

fn http_error(code: u16, response: ureq::Response) -> Value {
    let mut body = String::new(); let _ = response.into_reader().take(MAX_RESPONSE_BYTES).read_to_string(&mut body);
    unavailable("graph_request_failed", &format!("http_status={code};body={}", body.chars().take(1_000).collect::<String>()))
}

fn unavailable(reason: &str, detail: &str) -> Value {
    json!({"schema":"narada.graph_authority.error.v1","status":"unavailable","reason":reason,"detail":detail,"remediation":"Configure the bounded native Graph authority and retry."})
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
        let adapter = CalendarGraphAdapter { base_url: DEFAULT_GRAPH_BASE_URL.to_string(), allowed_mailboxes: vec!["user@example.com".to_string()], access_token: "test".to_string() };
        let mut query = Map::new(); query.insert("$top".to_string(), json!(20));
        let url = adapter.build_url(Some("user@example.com"), "calendars", &query).expect("url");
        assert!(url.contains("/users/user%40example.com/calendars"));
        assert!(url.contains("%24top=20"));
        assert!(adapter.build_url(Some("other@example.com"), "calendars", &query).is_err());
    }
}
