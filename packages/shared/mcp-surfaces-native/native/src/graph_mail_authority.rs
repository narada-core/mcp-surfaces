use crate::graph_authority::CalendarGraphAdapter;
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::Path;
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;

const MAX_CONFIG_BYTES: u64 = 512 * 1024;
const MAX_AUDIT_BYTES: usize = 64 * 1024;
const DEFAULT_QUERY_TOP: u64 = 20;
const DEFAULT_FOLDER_TOP: u64 = 50;

/// Direct, bounded Microsoft Graph authority for the provider-facing part of
/// graph-mail. It intentionally starts with the core mailbox operations; the
/// remaining draft/attachment flows stay behind their existing authority
/// boundary until their Rust policies are ported.
pub fn enabled() -> bool {
    matches!(
        std::env::var("NARADA_NATIVE_GRAPH_MAIL_AUTHORITY")
            .ok()
            .as_deref()
            .map(str::trim),
        Some("1" | "true" | "yes")
    )
}

pub fn supports(name: &str) -> bool {
    matches!(
        name,
        "graph_mail_query"
            | "graph_mail_message_show"
            | "graph_mail_folder_list"
            | "graph_mail_folder_create"
            | "graph_mail_message_move"
            | "graph_mail_message_mark_read"
    )
}

pub fn call_tool(name: &str, args: &Map<String, Value>, root: &Path) -> Result<Value, Value> {
    let policy = Policy::from_site_root(root)?;
    match name {
        "graph_mail_query" => query(&policy, args),
        "graph_mail_message_show" => message_show(&policy, args),
        "graph_mail_folder_list" => folder_list(&policy, args),
        "graph_mail_folder_create" => folder_create(&policy, args, root),
        "graph_mail_message_move" => message_move(&policy, args, root),
        "graph_mail_message_mark_read" => mark_read(&policy, args, root),
        _ => Err(boundary(name, "graph_mail_native_operation_not_implemented")),
    }
}

struct Policy {
    adapter: CalendarGraphAdapter,
    allow_folder_create: bool,
    allow_message_move: bool,
    allow_message_mark_read: bool,
    organization_approval_token: Option<String>,
}

impl Policy {
    fn from_site_root(root: &Path) -> Result<Self, Value> {
        let path = root.join(".ai/graph-mail-mcp.json");
        let object = if path.exists() {
            let metadata = fs::metadata(&path)
                .map_err(|error| unavailable("graph_mail_config_read_failed", &error.to_string()))?;
            if metadata.len() > MAX_CONFIG_BYTES {
                return Err(unavailable("graph_mail_config_too_large", "bounded config exceeded"));
            }
            let text = fs::read_to_string(&path)
                .map_err(|error| unavailable("graph_mail_config_read_failed", &error.to_string()))?;
            serde_json::from_str::<Value>(&text)
                .map_err(|error| unavailable("graph_mail_config_invalid", &error.to_string()))?
        } else {
            json!({})
        };
        let object = object.as_object().cloned().unwrap_or_default();
        let organization_approval_token = object
            .get("mailbox_organization_approval_token")
            .or_else(|| object.get("mailboxOrganizationApprovalToken"))
            .and_then(Value::as_str)
            .filter(|value| !value.trim().is_empty())
            .map(ToOwned::to_owned);
        Ok(Self {
            adapter: CalendarGraphAdapter::from_config(root, ".ai/graph-mail-mcp.json")?,
            allow_folder_create: bool_value(&object, "allow_folder_create", "allowFolderCreate"),
            allow_message_move: bool_value(&object, "allow_message_move", "allowMessageMove"),
            allow_message_mark_read: bool_value(
                &object,
                "allow_message_mark_read",
                "allowMessageMarkRead",
            ),
            organization_approval_token,
        })
    }

    fn organization_write_allowed(
        &self,
        args: &Map<String, Value>,
        operation: &str,
    ) -> Result<(), &'static str> {
        let allowed = match operation {
            "folder_create" => self.allow_folder_create,
            "message_move" => self.allow_message_move,
            "message_mark_read" => self.allow_message_mark_read,
            _ => false,
        };
        if !allowed {
            return Err(match operation {
                "folder_create" => "folder_create_disallowed_by_policy",
                "message_move" => "message_move_disallowed_by_policy",
                _ => "message_mark_read_disallowed_by_policy",
            });
        }
        if !confirmed(args, "confirm_write", "confirmWrite") {
            return Err("confirm_write_required");
        }
        if let Some(expected) = self.organization_approval_token.as_deref() {
            if args.get("approval_token").and_then(Value::as_str) != Some(expected) {
                return Err("mailbox_organization_approval_token_required");
            }
        }
        Ok(())
    }
}

fn query(policy: &Policy, args: &Map<String, Value>) -> Result<Value, Value> {
    let folder_id = optional_string(args, "folder_id");
    let suffix = folder_id
        .as_deref()
        .map(|id| format!("mailFolders/{}/messages", encode_component(id)))
        .unwrap_or_else(|| "messages".to_string());
    let mut query = Map::new();
    query.insert("$top".to_string(), json!(bounded_top(args.get("limit"), DEFAULT_QUERY_TOP)));
    for key in ["$select", "$filter", "$orderby"] {
        let source = key.trim_start_matches('$');
        if let Some(value) = optional_string(args, source) {
            query.insert(key.to_string(), Value::String(value));
        }
    }
    if let Some(value) = optional_string(args, "query") {
        query.insert("$search".to_string(), Value::String(format!("\"{}\"", value.replace('"', "\\\""))));
    } else if !query.contains_key("$orderby") {
        query.insert("$orderby".to_string(), Value::String("receivedDateTime desc".to_string()));
    }
    let url = policy.adapter.build_url(mailbox(args), &suffix, &query)?;
    let result = policy.adapter.request("GET", mailbox(args), &suffix, &query, None)?;
    Ok(json!({"schema":"narada.graph_mail_mcp.query.v1","status":"ok","request_url":url,"result":result}))
}

fn message_show(policy: &Policy, args: &Map<String, Value>) -> Result<Value, Value> {
    let id = required_string(args, "message_id")?;
    let suffix = format!("messages/{}", encode_component(&id));
    let mut query = Map::new();
    if let Some(select) = optional_string(args, "select") {
        query.insert("$select".to_string(), Value::String(select));
    }
    let result = policy.adapter.request("GET", mailbox(args), &suffix, &query, None)?;
    Ok(json!({"schema":"narada.graph_mail_mcp.message.v1","status":"ok","message":result}))
}

fn folder_list(policy: &Policy, args: &Map<String, Value>) -> Result<Value, Value> {
    let parent = optional_string(args, "parent_folder_id");
    let suffix = parent
        .as_deref()
        .map(|id| format!("mailFolders/{}/childFolders", encode_component(id)))
        .unwrap_or_else(|| "mailFolders".to_string());
    let mut query = Map::new();
    query.insert("$top".to_string(), json!(bounded_top(args.get("limit"), DEFAULT_FOLDER_TOP)));
    if let Some(select) = optional_string(args, "select") {
        query.insert("$select".to_string(), Value::String(select));
    }
    let url = policy.adapter.build_url(mailbox(args), &suffix, &query)?;
    let result = policy.adapter.request("GET", mailbox(args), &suffix, &query, None)?;
    Ok(json!({"schema":"narada.graph_mail_mcp.folders.v1","status":"ok","request_url":url,"folders":result}))
}

fn folder_create(policy: &Policy, args: &Map<String, Value>, root: &Path) -> Result<Value, Value> {
    let display_name = required_string(args, "display_name")?;
    if let Err(reason) = policy.organization_write_allowed(args, "folder_create") {
        return refused(root, "folder_create_refused", reason, json!({"display_name":display_name}));
    }
    let parent = optional_string(args, "parent_folder_id");
    let suffix = parent
        .as_deref()
        .map(|id| format!("mailFolders/{}/childFolders", encode_component(id)))
        .unwrap_or_else(|| "mailFolders".to_string());
    record_audit(root, json!({"event_kind":"folder_create_requested","mailbox_id":mailbox_value(args),"display_name":display_name}))?;
    let result = policy.adapter.request("POST", mailbox(args), &suffix, &Map::new(), Some(&json!({"displayName":display_name})))?;
    record_audit(root, json!({"event_kind":"folder_create_completed","mailbox_id":mailbox_value(args),"folder_id":result.get("id").cloned().unwrap_or(Value::Null)}))?;
    Ok(json!({"schema":"narada.graph_mail_mcp.folder.v1","status":"created","folder":result}))
}

fn message_move(policy: &Policy, args: &Map<String, Value>, root: &Path) -> Result<Value, Value> {
    let id = required_string(args, "message_id")?;
    let destination = required_string(args, "destination_folder_id")?;
    if let Err(reason) = policy.organization_write_allowed(args, "message_move") {
        return refused(root, "message_move_refused", reason, json!({"message_id":id,"destination_folder_id":destination}));
    }
    let suffix = format!("messages/{}/move", encode_component(&id));
    record_audit(root, json!({"event_kind":"message_move_requested","mailbox_id":mailbox_value(args),"message_id":id,"destination_folder_id":destination}))?;
    let result = policy.adapter.request("POST", mailbox(args), &suffix, &Map::new(), Some(&json!({"destinationId":destination})))?;
    record_audit(root, json!({"event_kind":"message_move_completed","mailbox_id":mailbox_value(args),"message_id":id,"destination_folder_id":destination}))?;
    Ok(json!({"schema":"narada.graph_mail_mcp.message_move.v1","status":"moved","message":result}))
}

fn mark_read(policy: &Policy, args: &Map<String, Value>, root: &Path) -> Result<Value, Value> {
    let id = required_string(args, "message_id")?;
    let idempotency = required_string(args, "idempotency_key")?;
    let digest = Sha256::digest(idempotency.as_bytes());
    let digest_hex = hex_lower(&digest);
    let operation_ref = format!("graph-mail-mark-read:{}", &digest_hex[..32]);
    if let Err(reason) = policy.organization_write_allowed(args, "message_mark_read") {
        record_audit(root, json!({"event_kind":"message_mark_read_refused","mailbox_id":mailbox_value(args),"message_id":id,"reason":reason}))?;
        return Ok(json!({"schema":"narada.domain_operation.v1","operation_ref":operation_ref,"outcome":"failed","error_message":reason,"result":{"schema":"narada.graph_mail_mcp.message_mark_read.v1","status":"refused","reason":reason,"message_id":id}}));
    }
    let suffix = format!("messages/{}", encode_component(&id));
    record_audit(root, json!({"event_kind":"message_mark_read_requested","mailbox_id":mailbox_value(args),"message_id":id}))?;
    let _ = policy.adapter.request("PATCH", mailbox(args), &suffix, &Map::new(), Some(&json!({"isRead":true})))?;
    record_audit(root, json!({"event_kind":"message_mark_read_completed","mailbox_id":mailbox_value(args),"message_id":id}))?;
    Ok(json!({"schema":"narada.domain_operation.v1","operation_ref":operation_ref,"outcome":"completed","result":{"schema":"narada.graph_mail_mcp.message_mark_read.v1","status":"marked_read","message_id":id}}))
}

fn refused(root: &Path, event_kind: &str, reason: &str, extra: Value) -> Result<Value, Value> {
    record_audit(root, merge(json!({"event_kind":event_kind,"reason":reason}), extra))?;
    Ok(json!({"schema":"narada.graph_mail_mcp.mailbox_organization_write.v1","status":"refused","reason":reason}))
}

fn record_audit(root: &Path, event: Value) -> Result<(), Value> {
    let path = root.join(".ai/audit/graph-mail-mcp.jsonl");
    if let Some(parent) = path.parent() { fs::create_dir_all(parent).map_err(|e| unavailable("graph_mail_audit_write_failed", &e.to_string()))?; }
    let mut object = event.as_object().cloned().unwrap_or_default();
    object.insert("schema".to_string(), json!("narada.graph_mail_mcp.audit.v1"));
    object.insert("recorded_at".to_string(), json!(OffsetDateTime::now_utc().format(&Rfc3339).unwrap_or_else(|_| "unknown".to_string())));
    let line = serde_json::to_string(&Value::Object(object)).map_err(|e| unavailable("graph_mail_audit_encode_failed", &e.to_string()))?;
    if line.len() > MAX_AUDIT_BYTES { return Err(unavailable("graph_mail_audit_record_too_large", "bounded audit record exceeded")); }
    let mut file = OpenOptions::new().create(true).append(true).open(path).map_err(|e| unavailable("graph_mail_audit_write_failed", &e.to_string()))?;
    file.write_all(line.as_bytes()).and_then(|_| file.write_all(b"\n")).map_err(|e| unavailable("graph_mail_audit_write_failed", &e.to_string()))
}

fn merge(left: Value, right: Value) -> Value {
    let mut object = left.as_object().cloned().unwrap_or_default();
    if let Some(extra) = right.as_object() { object.extend(extra.clone()); }
    Value::Object(object)
}

fn bool_value(object: &Map<String, Value>, snake: &str, camel: &str) -> bool {
    object.get(snake).and_then(Value::as_bool).unwrap_or(false) || object.get(camel).and_then(Value::as_bool).unwrap_or(false)
}

fn confirmed(args: &Map<String, Value>, snake: &str, camel: &str) -> bool {
    args.get(snake).and_then(Value::as_bool).or_else(|| args.get(camel).and_then(Value::as_bool)).unwrap_or(false)
}

fn mailbox<'a>(args: &'a Map<String, Value>) -> Option<&'a str> { args.get("mailbox_id").and_then(Value::as_str).filter(|value| !value.trim().is_empty()) }
fn mailbox_value(args: &Map<String, Value>) -> Value { mailbox(args).map(|value| json!(value)).unwrap_or_else(|| json!("me")) }
fn optional_string(args: &Map<String, Value>, key: &str) -> Option<String> { args.get(key).and_then(Value::as_str).map(str::trim).filter(|value| !value.is_empty()).map(ToOwned::to_owned) }
fn required_string(args: &Map<String, Value>, key: &str) -> Result<String, Value> { optional_string(args, key).ok_or_else(|| invalid(key)) }
fn bounded_top(value: Option<&Value>, fallback: u64) -> u64 { value.and_then(Value::as_u64).unwrap_or(fallback).clamp(1, 100) }

fn encode_component(value: &str) -> String {
    value.bytes().map(|byte| match byte { b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => (byte as char).to_string(), _ => format!("%{byte:02X}") }).collect()
}

fn hex_lower(bytes: &[u8]) -> String { bytes.iter().map(|byte| format!("{byte:02x}")).collect() }
fn invalid(key: &str) -> Value { json!({"schema":"narada.graph_mail_mcp.error.v1","status":"invalid","reason":format!("{key}_required")}) }
fn boundary(name: &str, reason: &str) -> Value { json!({"schema":"narada.graph_mail_mcp.authority_boundary.v1","status":"unavailable","tool_name":name,"reason":reason,"remediation":"Use the configured Rust Graph Mail authority or keep the Bun authority selected."}) }
fn unavailable(reason: &str, detail: &str) -> Value { json!({"schema":"narada.graph_mail_mcp.authority_error.v1","status":"unavailable","reason":reason,"detail":detail}) }

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn graph_mail_native_authority_is_explicit() {
        std::env::remove_var("NARADA_NATIVE_GRAPH_MAIL_AUTHORITY");
        assert!(!enabled());
    }

    #[test]
    fn operation_support_is_limited_to_ported_core() {
        assert!(supports("graph_mail_query"));
        assert!(supports("graph_mail_message_mark_read"));
        assert!(!supports("graph_mail_draft_send"));
    }
}
