use crate::graph_authority::CalendarGraphAdapter;
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;

const MAX_CONFIG_BYTES: u64 = 512 * 1024;
const MAX_AUDIT_BYTES: usize = 64 * 1024;
const DEFAULT_QUERY_TOP: u64 = 20;
const DEFAULT_FOLDER_TOP: u64 = 50;
const MAX_DOWNLOADED_ATTACHMENT_BYTES: usize = 25 * 1024 * 1024;
const ATTACHMENT_UPLOAD_CHUNK_GRANULARITY: u64 = 320 * 1024;
const DEFAULT_ATTACHMENT_UPLOAD_CHUNK_SIZE: u64 = 10 * ATTACHMENT_UPLOAD_CHUNK_GRANULARITY;
const MAX_ATTACHMENT_UPLOAD_FILE_BYTES: u64 = 512 * 1024 * 1024;
const ALLOWED_DOWNLOADED_ATTACHMENT_TYPES: &[&str] = &[
    "application/pdf",
    "application/vnd.openxmlformats-officedocument.presentationml.presentation",
    "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
    "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
    "text/csv",
    "text/plain",
    "image/png",
    "image/jpeg",
];

/// Direct, bounded Microsoft Graph authority for the provider-facing part of
/// graph-mail. Operations remain opt-in until the complete provider contract
/// for this surface has been ported and parity-tested.
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
            | "graph_mail_auth_status"
            | "graph_mail_auth_clear"
            | "graph_mail_folder_list"
            | "graph_mail_folder_create"
            | "graph_mail_message_move"
            | "graph_mail_message_mark_read"
            | "graph_mail_attachment_list"
            | "graph_mail_attachment_get"
            | "graph_mail_attachment_download_file"
            | "graph_mail_attachment_add"
            | "graph_mail_attachment_upload_session_create"
            | "graph_mail_attachment_upload_chunk"
            | "graph_mail_attachment_upload_file"
            | "graph_mail_attachment_delete"
            | "graph_mail_draft_create"
            | "graph_mail_reply_draft_create"
            | "graph_mail_reply_all_draft_create"
            | "graph_mail_forward_draft_create"
            | "graph_mail_reply_all_to_last_in_thread_draft_create"
            | "graph_mail_draft_update"
            | "graph_mail_draft_discard"
            | "graph_mail_draft_send"
    )
}

pub fn call_tool(name: &str, args: &Map<String, Value>, root: &Path) -> Result<Value, Value> {
    let policy = Policy::from_site_root(root)?;
    match name {
        "graph_mail_query" => query(&policy, args),
        "graph_mail_message_show" => message_show(&policy, args),
        "graph_mail_auth_status" => auth_status(&policy, root),
        "graph_mail_auth_clear" => auth_clear(args, root),
        "graph_mail_folder_list" => folder_list(&policy, args),
        "graph_mail_folder_create" => folder_create(&policy, args, root),
        "graph_mail_message_move" => message_move(&policy, args, root),
        "graph_mail_message_mark_read" => mark_read(&policy, args, root),
        "graph_mail_attachment_list" => attachment_list(&policy, args),
        "graph_mail_attachment_get" => attachment_get(&policy, args),
        "graph_mail_attachment_download_file" => attachment_download_file(&policy, args, root),
        "graph_mail_attachment_add" => attachment_add(&policy, args),
        "graph_mail_attachment_upload_session_create" => attachment_upload_session_create(&policy, args),
        "graph_mail_attachment_upload_chunk" => attachment_upload_chunk(&policy, args),
        "graph_mail_attachment_upload_file" => attachment_upload_file(&policy, args, root),
        "graph_mail_attachment_delete" => attachment_delete(&policy, args),
        "graph_mail_draft_create" => draft_create(&policy, args, root),
        "graph_mail_reply_draft_create" => derived_draft_create(&policy, args, root, "createReply"),
        "graph_mail_reply_all_draft_create" => derived_draft_create(&policy, args, root, "createReplyAll"),
        "graph_mail_forward_draft_create" => derived_draft_create(&policy, args, root, "createForward"),
        "graph_mail_reply_all_to_last_in_thread_draft_create" => reply_all_to_last_in_thread(&policy, args, root),
        "graph_mail_draft_update" => draft_update(&policy, args, root),
        "graph_mail_draft_discard" => draft_discard(&policy, args, root),
        "graph_mail_draft_send" => draft_send(&policy, args, root),
        _ => Err(boundary(name, "graph_mail_native_operation_not_implemented")),
    }
}

struct Policy {
    adapter: CalendarGraphAdapter,
    allow_folder_create: bool,
    allow_message_move: bool,
    allow_message_mark_read: bool,
    allow_send_draft: bool,
    send_approval_token: Option<String>,
    allowed_attachment_roots: Vec<PathBuf>,
    allow_device_code_auth: bool,
    device_code_tenant_id: Option<String>,
    device_code_client_id: Option<String>,
    device_code_allowed_scopes: Vec<String>,
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
            allow_send_draft: bool_value(&object, "allow_send_draft", "allowSendDraft"),
            send_approval_token: object
                .get("send_approval_token")
                .or_else(|| object.get("sendApprovalToken"))
                .and_then(Value::as_str)
                .filter(|value| !value.trim().is_empty())
                .map(ToOwned::to_owned),
            allowed_attachment_roots: object
                .get("allowed_attachment_roots")
                .or_else(|| object.get("allowedAttachmentRoots"))
                .and_then(Value::as_array)
                .map(|values| {
                    values
                        .iter()
                        .filter_map(Value::as_str)
                        .filter(|value| !value.trim().is_empty())
                        .map(|value| root.join(value))
                        .collect()
                })
                .unwrap_or_default(),
            allow_device_code_auth: bool_value(&object, "allow_device_code_auth", "allowDeviceCodeAuth"),
            device_code_tenant_id: optional_config_string(&object, "device_code_tenant_id", "deviceCodeTenantId"),
            device_code_client_id: optional_config_string(&object, "device_code_client_id", "deviceCodeClientId"),
            device_code_allowed_scopes: string_array(&object, "device_code_allowed_scopes", "deviceCodeAllowedScopes"),
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

    fn draft_send_allowed(&self, args: &Map<String, Value>) -> Result<(), &'static str> {
        if !self.allow_send_draft {
            return Err("send_draft_disallowed_by_policy");
        }
        if !confirmed(args, "confirm_send", "confirmSend") {
            return Err("confirm_send_required");
        }
        if let Some(expected) = self.send_approval_token.as_deref() {
            if args.get("approval_token").and_then(Value::as_str) != Some(expected) {
                return Err("send_approval_token_required");
            }
        }
        Ok(())
    }
}

fn auth_status(policy: &Policy, root: &Path) -> Result<Value, Value> {
    Ok(json!({
        "schema":"narada.graph_mail_mcp.auth_status.v1",
        "status":"ok",
        "allow_device_code_auth":policy.allow_device_code_auth,
        "device_code_tenant_configured":policy.device_code_tenant_id.is_some(),
        "device_code_client_configured":policy.device_code_client_id.is_some(),
        "device_code_allowed_scopes":policy.device_code_allowed_scopes,
        "delegated_token":delegated_token_summary(root)
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

fn delegated_token_path(root: &Path) -> PathBuf {
    root.join(".ai/runtime/graph-mail-mcp/delegated-token.json")
}

fn delegated_token_summary(root: &Path) -> Value {
    let path = delegated_token_path(root);
    let Ok(metadata) = fs::metadata(&path) else {
        return json!({"status":"missing","fresh":false});
    };
    if metadata.len() > MAX_CONFIG_BYTES {
        return json!({"status":"invalid","fresh":false});
    }
    let Ok(text) = fs::read_to_string(&path) else {
        return json!({"status":"invalid","fresh":false});
    };
    let Ok(value) = serde_json::from_str::<Value>(&text) else {
        return json!({"status":"invalid","fresh":false});
    };
    let Some(object) = value.as_object() else {
        return json!({"status":"invalid","fresh":false});
    };
    if object.get("schema").and_then(Value::as_str) != Some("narada.graph_mail_mcp.delegated_token.v1") {
        return json!({"status":"invalid","fresh":false});
    }
    let expires_at_ms = object.get("expires_at_ms").and_then(Value::as_i64).unwrap_or(0);
    let now_ms = (OffsetDateTime::now_utc().unix_timestamp_nanos() / 1_000_000) as i64;
    let fresh = expires_at_ms > now_ms + 60_000;
    let refreshable = object.get("refresh_token").and_then(Value::as_str).is_some();
    json!({
        "status":if fresh { "available" } else if refreshable { "refreshable" } else { "expired" },
        "fresh":fresh,
        "refreshable":refreshable,
        "auth_mode":object.get("auth_mode").cloned().unwrap_or(Value::Null),
        "tenant_id":object.get("tenant_id").cloned().unwrap_or(Value::Null),
        "client_id":object.get("client_id").cloned().unwrap_or(Value::Null),
        "scope":object.get("scope").cloned().unwrap_or(Value::Null),
        "acquired_at":object.get("acquired_at").cloned().unwrap_or(Value::Null),
        "expires_at_ms":expires_at_ms
    })
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

fn attachment_list(policy: &Policy, args: &Map<String, Value>) -> Result<Value, Value> {
    let message_id = attachment_message_id(args)?;
    let suffix = format!(
        "messages/{}/attachments",
        encode_component(&message_id)
    );
    let mut query = Map::new();
    query.insert(
        "$top".to_string(),
        json!(bounded_top(args.get("top").or_else(|| args.get("limit")), 20)),
    );
    let result = policy
        .adapter
        .request("GET", mailbox(args), &suffix, &query, None)?;
    Ok(json!({
        "schema":"narada.graph_mail_mcp.attachments.v1",
        "status":"ok",
        "attachments":strip_graph_attachment_contents(result)
    }))
}

fn attachment_get(policy: &Policy, args: &Map<String, Value>) -> Result<Value, Value> {
    let message_id = attachment_message_id(args)?;
    let attachment_id = required_string(args, "attachment_id")?;
    let suffix = format!(
        "messages/{}/attachments/{}",
        encode_component(&message_id),
        encode_component(&attachment_id)
    );
    let result = policy
        .adapter
        .request("GET", mailbox(args), &suffix, &Map::new(), None)?;
    let attachment = if args
        .get("include_content")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        result
    } else {
        strip_attachment_content(result)
    };
    Ok(json!({
        "schema":"narada.graph_mail_mcp.attachment.v1",
        "status":"ok",
        "attachment":attachment
    }))
}

fn attachment_download_file(
    policy: &Policy,
    args: &Map<String, Value>,
    root: &Path,
) -> Result<Value, Value> {
    let message_id = attachment_message_id(args)?;
    let attachment_id = required_string(args, "attachment_id")?;
    let destination = resolve_attachment_output_path(
        root,
        args,
        &policy.allowed_attachment_roots,
    )?;
    let suffix = format!(
        "messages/{}/attachments/{}",
        encode_component(&message_id),
        encode_component(&attachment_id)
    );
    let graph = policy
        .adapter
        .request("GET", mailbox(args), &suffix, &Map::new(), None)?;
    let name = graph
        .get("name")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .unwrap_or(&attachment_id)
        .to_string();
    let content_type = graph
        .get("contentType")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| infer_content_type(&name));
    if !ALLOWED_DOWNLOADED_ATTACHMENT_TYPES
        .iter()
        .any(|value| value.eq_ignore_ascii_case(&content_type))
    {
        return Err(unavailable(
            "attachment_download_content_type_not_allowed",
            &content_type,
        ));
    }
    let content_base64 = graph
        .get("contentBytes")
        .and_then(Value::as_str)
        .or_else(|| graph.get("content_base64").and_then(Value::as_str))
        .ok_or_else(|| unavailable("attachment_download_content_missing", "contentBytes missing"))?;
    let bytes = decode_base64(content_base64).map_err(|reason| unavailable(&reason, "invalid attachment content"))?;
    if bytes.is_empty() {
        return Err(unavailable(
            "attachment_download_content_empty",
            "attachment content is empty",
        ));
    }
    if bytes.len() > MAX_DOWNLOADED_ATTACHMENT_BYTES {
        return Err(unavailable(
            "attachment_download_too_large",
            &bytes.len().to_string(),
        ));
    }
    if let Some(size) = graph.get("size").and_then(Value::as_u64) {
        if size != bytes.len() as u64 {
            return Err(unavailable(
                "attachment_download_size_mismatch",
                &format!("{size}:{}", bytes.len()),
            ));
        }
    }
    let digest = hex_lower(&Sha256::digest(&bytes));
    if destination.exists() {
        let existing = fs::read(&destination)
            .map_err(|error| unavailable("attachment_download_read_failed", &error.to_string()))?;
        if hex_lower(&Sha256::digest(&existing)) != digest {
            return Err(unavailable(
                "attachment_download_destination_conflict",
                "existing destination has a different digest",
            ));
        }
        return Ok(json!({
            "schema":"narada.graph_mail_mcp.attachment_download_file.v1",
            "status":"already_materialized",
            "message_id":message_id,
            "attachment_id":attachment_id,
            "file_path":display_path(&destination),
            "name":name,
            "content_type":content_type,
            "size":bytes.len(),
            "sha256":digest
        }));
    }
    if let Some(parent) = destination.parent() {
        fs::create_dir_all(parent)
            .map_err(|error| unavailable("attachment_download_directory_failed", &error.to_string()))?;
    }
    let temporary = PathBuf::from(format!("{}.{}.tmp", destination.display(), std::process::id()));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary)
        .map_err(|error| unavailable("attachment_download_write_failed", &error.to_string()))?;
    if let Err(error) = file.write_all(&bytes) {
        let _ = fs::remove_file(&temporary);
        return Err(unavailable("attachment_download_write_failed", &error.to_string()));
    }
    drop(file);
    if let Err(error) = fs::rename(&temporary, &destination) {
        let _ = fs::remove_file(&temporary);
        return Err(unavailable("attachment_download_materialize_failed", &error.to_string()));
    }
    record_audit(
        root,
        json!({
            "event_kind":"attachment_download_file_completed",
            "mailbox_id":mailbox_value(args),
            "message_id":message_id,
            "attachment_id":attachment_id,
            "name":name,
            "content_type":content_type,
            "size":bytes.len(),
            "sha256":digest
        }),
    )?;
    Ok(json!({
        "schema":"narada.graph_mail_mcp.attachment_download_file.v1",
        "status":"materialized",
        "message_id":message_id,
        "attachment_id":attachment_id,
        "file_path":display_path(&destination),
        "name":name,
        "content_type":content_type,
        "size":bytes.len(),
        "sha256":digest
    }))
}

fn attachment_add(policy: &Policy, args: &Map<String, Value>) -> Result<Value, Value> {
    let message_id = attachment_message_id(args)?;
    let content_base64 = required_string(args, "content_base64")?;
    if !valid_base64(&content_base64) {
        return Err(invalid("content_base64"));
    }
    if base64_decoded_size(&content_base64) > 3 * 1024 * 1024 {
        return Err(unavailable(
            "attachment_small_file_too_large",
            "small attachment limit is 3 MiB",
        ));
    }
    let name = required_string(args, "name")?;
    let content_type = required_string(args, "content_type")?;
    let mut body = Map::new();
    body.insert(
        "@odata.type".to_string(),
        json!("#microsoft.graph.fileAttachment"),
    );
    body.insert("name".to_string(), json!(name));
    body.insert("contentType".to_string(), json!(content_type));
    body.insert("contentBytes".to_string(), json!(content_base64));
    if let Some(value) = args.get("is_inline").and_then(Value::as_bool) {
        body.insert("isInline".to_string(), json!(value));
    }
    if let Some(value) = optional_string(args, "content_id") {
        body.insert("contentId".to_string(), json!(value));
    }
    let suffix = format!(
        "messages/{}/attachments",
        encode_component(&message_id)
    );
    let result = policy.adapter.request(
        "POST",
        mailbox(args),
        &suffix,
        &Map::new(),
        Some(&Value::Object(body)),
    )?;
    Ok(json!({
        "schema":"narada.graph_mail_mcp.attachment.v1",
        "status":"created",
        "attachment":result
    }))
}

fn attachment_delete(policy: &Policy, args: &Map<String, Value>) -> Result<Value, Value> {
    let message_id = attachment_message_id(args)?;
    let attachment_id = required_string(args, "attachment_id")?;
    let suffix = format!(
        "messages/{}/attachments/{}",
        encode_component(&message_id),
        encode_component(&attachment_id)
    );
    let result = policy
        .adapter
        .request("DELETE", mailbox(args), &suffix, &Map::new(), None)?;
    Ok(json!({
        "schema":"narada.graph_mail_mcp.attachment_delete.v1",
        "status":"deleted",
        "result":result
    }))
}

fn attachment_upload_session_create(
    policy: &Policy,
    args: &Map<String, Value>,
) -> Result<Value, Value> {
    let message_id = attachment_message_id(args)?;
    let name = required_string(args, "name")?;
    let size = required_positive_number(args, "size")?;
    let mut attachment_item = Map::new();
    attachment_item.insert("attachmentType".to_string(), json!("file"));
    attachment_item.insert("name".to_string(), json!(name));
    attachment_item.insert("size".to_string(), json!(size));
    if let Some(value) = optional_string(args, "content_type") {
        attachment_item.insert("contentType".to_string(), json!(value));
    }
    if let Some(value) = args.get("is_inline").and_then(Value::as_bool) {
        attachment_item.insert("isInline".to_string(), json!(value));
    }
    if let Some(value) = optional_string(args, "content_id") {
        attachment_item.insert("contentId".to_string(), json!(value));
    }
    let body = json!({"AttachmentItem":Value::Object(attachment_item)});
    let suffix = format!(
        "messages/{}/attachments/createUploadSession",
        encode_component(&message_id)
    );
    let result = policy.adapter.request(
        "POST",
        mailbox(args),
        &suffix,
        &Map::new(),
        Some(&body),
    )?;
    Ok(json!({
        "schema":"narada.graph_mail_mcp.attachment_upload_session.v1",
        "status":"created",
        "upload_session":result
    }))
}

fn attachment_upload_chunk(
    policy: &Policy,
    args: &Map<String, Value>,
) -> Result<Value, Value> {
    let upload_url = required_string(args, "upload_url")?;
    let content_base64 = required_string(args, "content_base64")?;
    let range_start = required_nonnegative_number(args, "range_start")?;
    let range_end = required_nonnegative_number(args, "range_end")?;
    let total_size = required_nonnegative_number(args, "total_size")?;
    let bytes = decode_base64(&content_base64)
        .map_err(|reason| unavailable(reason, "invalid upload chunk content"))?;
    if range_end < range_start
        || total_size <= range_end
        || bytes.len() as u64 != range_end - range_start + 1
    {
        return Err(unavailable(
            "attachment_upload_content_range_invalid",
            "chunk byte count does not match Content-Range",
        ));
    }
    let mut headers = Map::new();
    headers.insert("Content-Length".to_string(), json!(bytes.len()));
    headers.insert(
        "Content-Range".to_string(),
        json!(format!("bytes {range_start}-{range_end}/{total_size}")),
    );
    headers.insert(
        "Content-Type".to_string(),
        json!("application/octet-stream"),
    );
    let (status, result) = policy
        .adapter
        .request_upload_bytes("PUT", &upload_url, &bytes, &headers)?;
    if status == 202 || status == 204 {
        return Ok(json!({
            "schema":"narada.graph_mail_mcp.attachment_upload_chunk.v1",
            "status":"accepted",
            "http_status":status
        }));
    }
    Ok(json!({
        "schema":"narada.graph_mail_mcp.attachment_upload_chunk.v1",
        "status":"ok",
        "http_status":status,
        "result":result
    }))
}

fn attachment_upload_file(
    policy: &Policy,
    args: &Map<String, Value>,
    root: &Path,
) -> Result<Value, Value> {
    let file_path = resolve_attachment_input_path(root, args, &policy.allowed_attachment_roots)?;
    let metadata = fs::metadata(&file_path)
        .map_err(|error| unavailable("attachment_file_stat_failed", &error.to_string()))?;
    let file_size = metadata.len();
    if file_size == 0 {
        return Err(unavailable("attachment_file_empty", "attachment file is empty"));
    }
    if file_size > MAX_ATTACHMENT_UPLOAD_FILE_BYTES {
        return Err(unavailable(
            "attachment_file_too_large",
            &MAX_ATTACHMENT_UPLOAD_FILE_BYTES.to_string(),
        ));
    }
    let attachment_name = optional_string(args, "name").or_else(|| {
        file_path
            .file_name()
            .and_then(|value| value.to_str())
            .map(ToOwned::to_owned)
    }).ok_or_else(|| invalid("name"))?;
    let content_type = optional_string(args, "content_type")
        .unwrap_or_else(|| infer_content_type(&attachment_name));
    let chunk_size = upload_chunk_size(args)?;
    let mut session_args = args.clone();
    session_args.insert("name".to_string(), json!(attachment_name));
    session_args.insert("size".to_string(), json!(file_size));
    session_args.insert("content_type".to_string(), json!(content_type));
    let session = attachment_upload_session_create(policy, &session_args)?;
    let upload_url = session
        .get("upload_session")
        .and_then(|value| value.get("uploadUrl"))
        .and_then(Value::as_str)
        .ok_or_else(|| unavailable("attachment_upload_session_url_missing", "uploadUrl missing"))?;
    let mut file = fs::File::open(&file_path)
        .map_err(|error| unavailable("attachment_file_open_failed", &error.to_string()))?;
    let mut buffer = vec![0u8; chunk_size as usize];
    let mut offset = 0u64;
    let mut chunk_count = 0u64;
    let mut final_result = Value::Null;
    let mut hash = Sha256::new();
    while offset < file_size {
        let remaining = file_size - offset;
        let requested = remaining.min(chunk_size) as usize;
        let mut read = 0usize;
        while read < requested {
            let count = file
                .read(&mut buffer[read..requested])
                .map_err(|error| unavailable("attachment_file_read_failed", &error.to_string()))?;
            if count == 0 {
                return Err(unavailable("attachment_file_read_failed", "unexpected end of file"));
            }
            read += count;
        }
        let bytes = &buffer[..read];
        hash.update(bytes);
        let range_end = offset + read as u64 - 1;
        let mut headers = Map::new();
        headers.insert("Content-Length".to_string(), json!(read));
        headers.insert(
            "Content-Range".to_string(),
            json!(format!("bytes {offset}-{range_end}/{file_size}")),
        );
        headers.insert(
            "Content-Type".to_string(),
            json!("application/octet-stream"),
        );
        let (status, result) = policy
            .adapter
            .request_upload_bytes("PUT", upload_url, bytes, &headers)?;
        if status != 202 && status != 204 {
            final_result = result;
        }
        offset = range_end + 1;
        chunk_count += 1;
    }
    let sha256 = hex_lower(&hash.finalize());
    record_audit(
        root,
        json!({"event_kind":"attachment_upload_file_completed","mailbox_id":mailbox_value(args),"message_id":attachment_message_id(args)?,"name":attachment_name,"size":file_size,"sha256":sha256,"chunk_count":chunk_count}),
    )?;
    Ok(json!({
        "schema":"narada.graph_mail_mcp.attachment_upload_file.v1",
        "status":"uploaded",
        "draft_id":optional_string(args, "draft_id"),
        "message_id":attachment_message_id(args)?,
        "name":attachment_name,
        "content_type":content_type,
        "size":file_size,
        "chunk_size":chunk_size,
        "chunk_count":chunk_count,
        "sha256":sha256,
        "attachment":final_result
    }))
}

fn draft_create(
    policy: &Policy,
    args: &Map<String, Value>,
    root: &Path,
) -> Result<Value, Value> {
    let message = message_patch(args);
    record_audit(
        root,
        json!({
            "event_kind":"draft_create_requested",
            "mailbox_id":mailbox_value(args),
            "subject":message.get("subject").cloned().unwrap_or(Value::Null)
        }),
    )?;
    let result = policy.adapter.request(
        "POST",
        mailbox(args),
        "messages",
        &Map::new(),
        Some(&Value::Object(message)),
    )?;
    record_audit(
        root,
        json!({
            "event_kind":"draft_create_completed",
            "mailbox_id":mailbox_value(args),
            "draft_id":result.get("id").cloned().unwrap_or(Value::Null)
        }),
    )?;
    Ok(json!({
        "schema":"narada.graph_mail_mcp.draft.v1",
        "status":"created",
        "draft":result
    }))
}

fn derived_draft_create(
    policy: &Policy,
    args: &Map<String, Value>,
    root: &Path,
    action: &str,
) -> Result<Value, Value> {
    let message_id = required_string(args, "message_id")?;
    if optional_string(args, "comment_html").is_some() {
        if action == "createForward" {
            return Err(unavailable(
                "comment_html_reply_only",
                "comment_html is supported only for reply and reply-all",
            ));
        }
        return html_reply_draft_create(policy, args, root, action);
    }
    let body = derived_draft_body(args, action)?;
    let suffix = format!(
        "messages/{}/{}",
        encode_component(&message_id),
        action
    );
    record_audit(
        root,
        json!({
            "event_kind":format!("{action}_requested"),
            "mailbox_id":mailbox_value(args),
            "message_id":message_id
        }),
    )?;
    let result = policy.adapter.request(
        "POST",
        mailbox(args),
        &suffix,
        &Map::new(),
        Some(&Value::Object(body)),
    )?;
    record_audit(
        root,
        json!({
            "event_kind":format!("{action}_completed"),
            "mailbox_id":mailbox_value(args),
            "message_id":message_id,
            "draft_id":result.get("id").cloned().unwrap_or(Value::Null)
        }),
    )?;
    Ok(json!({
        "schema":"narada.graph_mail_mcp.draft.v1",
        "status":"created",
        "draft":result
    }))
}

fn html_reply_draft_create(
    policy: &Policy,
    args: &Map<String, Value>,
    root: &Path,
    action: &str,
) -> Result<Value, Value> {
    let message_id = required_string(args, "message_id")?;
    let comment_html = required_string(args, "comment_html")?;
    if args.get("comment").and_then(Value::as_str).is_some()
        || args.get("body_text").and_then(Value::as_str).is_some()
        || args.get("body_html").and_then(Value::as_str).is_some()
    {
        return Err(unavailable(
            "comment_html_body_conflict",
            "provide comment_html alone",
        ));
    }
    let create_suffix = format!("messages/{}/{}", encode_component(&message_id), action);
    record_audit(
        root,
        json!({"event_kind":format!("{action}_html_requested"),"mailbox_id":mailbox_value(args),"message_id":message_id}),
    )?;
    let created = policy.adapter.request(
        "POST",
        mailbox(args),
        &create_suffix,
        &Map::new(),
        Some(&json!({})),
    )?;
    let draft_id = required_draft_id(&created)?;
    let draft_suffix = format!("messages/{}", encode_component(&draft_id));
    let observed = policy
        .adapter
        .request("GET", mailbox(args), &draft_suffix, &Map::new(), None)?;
    let quote_html = graph_body_as_html(observed.get("body").or_else(|| created.get("body")))?;
    if quote_html.trim().is_empty() {
        return Err(unavailable(
            "graph_reply_html_quote_missing",
            "Graph did not return quoted history",
        ));
    }
    let composed_html = format!(
        "{}<div data-narada-quoted-history=\"true\">{}</div>",
        comment_html, quote_html
    );
    let patched = policy.adapter.request(
        "PATCH",
        mailbox(args),
        &draft_suffix,
        &Map::new(),
        Some(&json!({"body":{"contentType":"HTML","content":composed_html}})),
    )?;
    if patched.get("isDraft").and_then(Value::as_bool) == Some(false) {
        return Err(unavailable(
            "graph_reply_html_draft_not_unsent",
            "Graph returned a sent message",
        ));
    }
    record_audit(
        root,
        json!({"event_kind":format!("{action}_html_completed"),"mailbox_id":mailbox_value(args),"message_id":message_id,"draft_id":draft_id,"quote_preserved":true}),
    )?;
    Ok(json!({
        "schema":"narada.graph_mail_mcp.draft.v1",
        "status":"created",
        "draft":patched,
        "reply_body_mode":"comment_html",
        "quote_preserved":true,
        "unsent":patched.get("isDraft").and_then(Value::as_bool) != Some(false)
    }))
}

fn reply_all_to_last_in_thread(
    policy: &Policy,
    args: &Map<String, Value>,
    root: &Path,
) -> Result<Value, Value> {
    let conversation_id = required_string(args, "conversation_id")?;
    let filter = format!(
        "conversationId eq '{}'",
        conversation_id.replace('\'', "''")
    );
    let mut query = Map::new();
    query.insert("$filter".to_string(), json!(filter));
    query.insert(
        "$orderby".to_string(),
        json!("receivedDateTime desc"),
    );
    query.insert("$top".to_string(), json!(1));
    query.insert(
        "$select".to_string(),
        json!("id,conversationId,receivedDateTime"),
    );
    let messages = policy
        .adapter
        .request("GET", mailbox(args), "messages", &query, None)?;
    let last_message = messages
        .get("value")
        .and_then(Value::as_array)
        .and_then(|items| items.first())
        .ok_or_else(|| unavailable("graph_mail_thread_no_messages", "conversation has no messages"))?;
    let message_id = last_message
        .get("id")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| unavailable("graph_mail_thread_last_message_missing_id", "last message has no id"))?
        .to_string();
    let body = derived_draft_body(args, "createReplyAll")?;
    let suffix = format!(
        "messages/{}/createReplyAll",
        encode_component(&message_id)
    );
    record_audit(
        root,
        json!({"event_kind":"createReplyAll_to_last_in_thread_requested","mailbox_id":mailbox_value(args),"conversation_id":conversation_id,"message_id":message_id}),
    )?;
    let draft = policy.adapter.request(
        "POST",
        mailbox(args),
        &suffix,
        &Map::new(),
        Some(&Value::Object(body)),
    )?;
    record_audit(
        root,
        json!({"event_kind":"createReplyAll_to_last_in_thread_completed","mailbox_id":mailbox_value(args),"conversation_id":conversation_id,"message_id":message_id,"draft_id":draft.get("id").cloned().unwrap_or(Value::Null)}),
    )?;
    Ok(json!({
        "schema":"narada.graph_mail_mcp.draft.v1",
        "status":"created",
        "source_message_id":message_id,
        "draft":draft
    }))
}

fn draft_update(
    policy: &Policy,
    args: &Map<String, Value>,
    root: &Path,
) -> Result<Value, Value> {
    let draft_id = required_string(args, "draft_id")?;
    let patch = message_patch(args);
    let suffix = format!("messages/{}", encode_component(&draft_id));
    let body_replacement_requested = patch.contains_key("body");
    let mut reply_reference = None;
    if body_replacement_requested {
        let existing = policy
            .adapter
            .request("GET", mailbox(args), &suffix, &Map::new(), None)?;
        reply_reference = graph_reply_reference(&existing);
        if reply_reference.is_some()
            && args.get("allow_replace_full_body").and_then(Value::as_bool) != Some(true)
            && args.get("allow_replace_quoted_body").and_then(Value::as_bool) != Some(true)
        {
            record_audit(
                root,
                json!({
                    "event_kind":"draft_update_refused",
                    "mailbox_id":mailbox_value(args),
                    "draft_id":draft_id,
                    "reason":"reply_or_forward_body_replacement_requires_explicit_authorization"
                }),
            )?;
            return Ok(json!({
                "schema":"narada.graph_mail_mcp.draft.v1",
                "status":"refused",
                "reason":"reply_or_forward_body_replacement_requires_explicit_authorization",
                "draft_id":draft_id,
                "body_replacement":{"requested":true,"reply_or_forward":true,"authorization_required":true,"remediation":"Pass allow_replace_quoted_body=true or allow_replace_full_body=true, or update non-body fields separately."}
            }));
        }
    }
    record_audit(
        root,
        json!({
            "event_kind":"draft_update_requested",
            "mailbox_id":mailbox_value(args),
            "draft_id":draft_id
        }),
    )?;
    let result = policy.adapter.request(
        "PATCH",
        mailbox(args),
        &suffix,
        &Map::new(),
        Some(&Value::Object(patch)),
    )?;
    record_audit(
        root,
        json!({
            "event_kind":"draft_update_completed",
            "mailbox_id":mailbox_value(args),
            "draft_id":draft_id
        }),
    )?;
    Ok(json!({
        "schema":"narada.graph_mail_mcp.draft.v1",
        "status":"updated",
        "draft":result,
        "body_replacement":{
            "requested":body_replacement_requested,
            "reply_or_forward":reply_reference.is_some(),
            "authorization":if body_replacement_requested && reply_reference.is_some() { if args.get("allow_replace_full_body").and_then(Value::as_bool) == Some(true) { "allow_replace_full_body" } else { "allow_replace_quoted_body" } } else { "not_required" },
            "postcondition":if body_replacement_requested { "patch_accepted_by_graph" } else { "not_applicable" }
        }
    }))
}

fn draft_discard(
    policy: &Policy,
    args: &Map<String, Value>,
    root: &Path,
) -> Result<Value, Value> {
    let draft_id = required_string(args, "draft_id")?;
    let suffix = format!("messages/{}", encode_component(&draft_id));
    let property_id = "String {d700a6f2-79ad-4f44-9df7-3e9b622f09f8} Name NaradaTicketDraftOperation";
    let mut query = Map::new();
    query.insert("$select".to_string(), json!("id,isDraft,changeKey"));
    query.insert(
        "$expand".to_string(),
        json!(format!("singleValueExtendedProperties($filter=id eq '{}')", property_id.replace('\'', "''"))),
    );
    let draft = policy
        .adapter
        .request("GET", mailbox(args), &suffix, &query, None)?;
    if draft.get("isDraft").and_then(Value::as_bool) != Some(true) {
        return Err(unavailable(
            "graph_mail_draft_discard_refused_not_draft",
            "Graph object is not an unsent draft",
        ));
    }
    if draft
        .get("singleValueExtendedProperties")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .any(|property| property.get("id").and_then(Value::as_str) == Some(property_id) && optional_string(property.as_object().unwrap_or(&Map::new()), "value").is_some())
    {
        return Err(unavailable(
            "graph_ticket_draft_requires_ticket_discard_tool",
            "Ticket drafts use the transactional ticket-discard operation",
        ));
    }
    let mut headers = Map::new();
    if let Some(verifier) = draft
        .get("@odata.etag")
        .and_then(Value::as_str)
        .or_else(|| draft.get("changeKey").and_then(Value::as_str))
    {
        headers.insert("If-Match".to_string(), json!(verifier));
    }
    record_audit(
        root,
        json!({"event_kind":"draft_discard_requested","mailbox_id":mailbox_value(args),"draft_id":draft_id}),
    )?;
    let result = policy.adapter.request_with_headers(
        "DELETE",
        mailbox(args),
        &suffix,
        &Map::new(),
        None,
        &headers,
    )?;
    record_audit(
        root,
        json!({"event_kind":"draft_discard_completed","mailbox_id":mailbox_value(args),"draft_id":draft_id}),
    )?;
    Ok(json!({
        "schema":"narada.graph_mail_mcp.draft_discard.v1",
        "status":"discarded",
        "result":result
    }))
}

fn draft_send(
    policy: &Policy,
    args: &Map<String, Value>,
    root: &Path,
) -> Result<Value, Value> {
    let draft_id = required_string(args, "draft_id")?;
    if let Err(reason) = policy.draft_send_allowed(args) {
        record_audit(
            root,
            json!({"event_kind":"draft_send_refused","mailbox_id":mailbox_value(args),"draft_id":draft_id,"reason":reason}),
        )?;
        return Ok(json!({
            "schema":"narada.graph_mail_mcp.draft_send.v1",
            "status":"refused",
            "reason":reason,
            "draft_id":draft_id
        }));
    }
    let suffix = format!("messages/{}/send", encode_component(&draft_id));
    record_audit(
        root,
        json!({"event_kind":"draft_send_requested","mailbox_id":mailbox_value(args),"draft_id":draft_id}),
    )?;
    let result = policy
        .adapter
        .request("POST", mailbox(args), &suffix, &Map::new(), None)?;
    record_audit(
        root,
        json!({"event_kind":"draft_send_completed","mailbox_id":mailbox_value(args),"draft_id":draft_id}),
    )?;
    Ok(json!({
        "schema":"narada.graph_mail_mcp.draft_send.v1",
        "status":"sent",
        "result":result
    }))
}

fn attachment_message_id(args: &Map<String, Value>) -> Result<String, Value> {
    optional_string(args, "draft_id")
        .or_else(|| optional_string(args, "message_id"))
        .ok_or_else(|| invalid("message_id"))
}

fn message_patch(args: &Map<String, Value>) -> Map<String, Value> {
    let mut patch = Map::new();
    if let Some(value) = args.get("subject").and_then(Value::as_str) {
        patch.insert("subject".to_string(), json!(value));
    }
    if let Some(value) = args.get("body_text").and_then(Value::as_str) {
        patch.insert("body".to_string(), json!({"contentType":"Text","content":value}));
    }
    if let Some(value) = args.get("body_html").and_then(Value::as_str) {
        patch.insert("body".to_string(), json!({"contentType":"HTML","content":value}));
    }
    for (source, target) in [
        ("to_recipients", "toRecipients"),
        ("cc_recipients", "ccRecipients"),
        ("bcc_recipients", "bccRecipients"),
    ] {
        if let Some(value) = args.get(source).and_then(Value::as_array) {
            patch.insert(target.to_string(), Value::Array(recipients(value)));
        }
    }
    if let Some(value) = args.get("importance").and_then(Value::as_str) {
        patch.insert("importance".to_string(), json!(value));
    }
    patch
}

fn recipients(values: &[Value]) -> Vec<Value> {
    values
        .iter()
        .map(|value| {
            if let Some(address) = value.as_str() {
                json!({"emailAddress":{"address":address}})
            } else {
                value.clone()
            }
        })
        .collect()
}

fn derived_draft_body(args: &Map<String, Value>, action: &str) -> Result<Map<String, Value>, Value> {
    let message = message_patch(args);
    if args.get("comment").and_then(Value::as_str).is_some() && message.contains_key("body") {
        return Err(unavailable(
            "derived_draft_comment_body_conflict",
            "provide comment or body_text/body_html, not both",
        ));
    }
    let mut body = Map::new();
    if let Some(comment) = args.get("comment").and_then(Value::as_str) {
        body.insert("comment".to_string(), json!(comment));
    }
    let mut message = message;
    if action == "createForward" {
        if let Some(value) = args.get("to_recipients").and_then(Value::as_array) {
            message.insert("toRecipients".to_string(), Value::Array(recipients(value)));
        }
    }
    if !message.is_empty() {
        body.insert("message".to_string(), Value::Object(message));
    }
    Ok(body)
}

fn graph_reply_reference(value: &Value) -> Option<String> {
    value
        .get("inReplyTo")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .or_else(|| {
            value
                .get("inReplyTo")
                .and_then(Value::as_object)
                .and_then(|object| object.get("id"))
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(ToOwned::to_owned)
        })
}

fn required_draft_id(value: &Value) -> Result<String, Value> {
    let draft_id = value
        .get("id")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| unavailable("graph_ticket_draft_id_missing", "Graph draft id is missing"))?;
    if value.get("isDraft").and_then(Value::as_bool) == Some(false) {
        return Err(unavailable(
            "graph_ticket_draft_not_unsent",
            "Graph returned a sent message instead of a draft",
        ));
    }
    Ok(draft_id.to_string())
}

fn graph_body_as_html(value: Option<&Value>) -> Result<String, Value> {
    let Some(body) = value.and_then(Value::as_object) else {
        return Ok(String::new());
    };
    let Some(content) = body.get("content").and_then(Value::as_str) else {
        return Ok(String::new());
    };
    if body
        .get("contentType")
        .and_then(Value::as_str)
        .map(|value| value.eq_ignore_ascii_case("html"))
        == Some(true)
    {
        return Ok(content.to_string());
    }
    Ok(content
        .split(['\r', '\n'])
        .filter(|line| !line.is_empty())
        .map(|line| format!("<p>{}</p>", escape_html(line)))
        .collect::<String>())
}

fn escape_html(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

fn strip_graph_attachment_contents(value: Value) -> Value {
    match value {
        Value::Array(items) => Value::Array(
            items
                .into_iter()
                .map(strip_graph_attachment_contents)
                .collect(),
        ),
        Value::Object(object) => {
            let attachment_like = object.keys().any(|key| {
                matches!(key.to_ascii_lowercase().as_str(), "id" | "name" | "attachmenttype")
            });
            Value::Object(
                object
                    .into_iter()
                    .filter_map(|(key, value)| {
                        if attachment_like
                            && matches!(key.to_ascii_lowercase().as_str(), "contentbytes" | "content_base64" | "content" | "data" | "bytes" | "raw")
                        {
                            None
                        } else if attachment_like {
                            Some((key, value))
                        } else {
                            Some((key, strip_graph_attachment_contents(value)))
                        }
                    })
                    .collect(),
            )
        }
        other => other,
    }
}

fn strip_attachment_content(value: Value) -> Value {
    strip_graph_attachment_contents(value)
}

fn valid_base64(value: &str) -> bool {
    value.len() % 4 == 0
        && value
            .bytes()
            .enumerate()
            .all(|(index, byte)| match byte {
                b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'+' | b'/' => true,
                b'=' => index + 2 >= value.len(),
                _ => false,
            })
}

fn base64_decoded_size(value: &str) -> usize {
    value.len() / 4 * 3 - value.as_bytes().iter().rev().take_while(|byte| **byte == b'=').count()
}

fn decode_base64(value: &str) -> Result<Vec<u8>, &'static str> {
    if !valid_base64(value) {
        return Err("attachment_content_base64_invalid");
    }
    let mut output = Vec::with_capacity(base64_decoded_size(value));
    let bytes = value.as_bytes();
    for chunk in bytes.chunks(4) {
        let a = base64_value(chunk[0]).ok_or("attachment_content_base64_invalid")?;
        let b = base64_value(chunk[1]).ok_or("attachment_content_base64_invalid")?;
        let c = if chunk[2] == b'=' { 0 } else { base64_value(chunk[2]).ok_or("attachment_content_base64_invalid")? };
        let d = if chunk[3] == b'=' { 0 } else { base64_value(chunk[3]).ok_or("attachment_content_base64_invalid")? };
        output.push((a << 2) | (b >> 4));
        if chunk[2] != b'=' {
            output.push((b << 4) | (c >> 2));
        }
        if chunk[3] != b'=' {
            output.push((c << 6) | d);
        }
    }
    Ok(output)
}

fn base64_value(byte: u8) -> Option<u8> {
    match byte {
        b'A'..=b'Z' => Some(byte - b'A'),
        b'a'..=b'z' => Some(byte - b'a' + 26),
        b'0'..=b'9' => Some(byte - b'0' + 52),
        b'+' => Some(62),
        b'/' => Some(63),
        _ => None,
    }
}

fn infer_content_type(file_name: &str) -> String {
    let lower = file_name.to_ascii_lowercase();
    if lower.ends_with(".pdf") {
        "application/pdf".to_string()
    } else if lower.ends_with(".pptx") {
        "application/vnd.openxmlformats-officedocument.presentationml.presentation".to_string()
    } else if lower.ends_with(".docx") {
        "application/vnd.openxmlformats-officedocument.wordprocessingml.document".to_string()
    } else if lower.ends_with(".xlsx") {
        "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet".to_string()
    } else if lower.ends_with(".csv") {
        "text/csv".to_string()
    } else if lower.ends_with(".txt") {
        "text/plain".to_string()
    } else if lower.ends_with(".png") {
        "image/png".to_string()
    } else if lower.ends_with(".jpg") || lower.ends_with(".jpeg") {
        "image/jpeg".to_string()
    } else {
        "application/octet-stream".to_string()
    }
}

fn resolve_attachment_output_path(
    root: &Path,
    args: &Map<String, Value>,
    configured_roots: &[PathBuf],
) -> Result<PathBuf, Value> {
    let input = required_string(args, "file_path")?;
    let candidate = root.join(&input);
    let roots: Vec<PathBuf> = if configured_roots.is_empty() {
        vec![root.to_path_buf()]
    } else {
        configured_roots.to_vec()
    };
    if !roots.iter().any(|parent| path_inside(&candidate, parent)) {
        return Err(unavailable(
            "attachment_file_path_not_allowed",
            "destination is outside the configured attachment roots",
        ));
    }
    if same_path(&candidate, root) {
        return Err(unavailable(
            "attachment_file_path_not_file",
            "destination must be a file",
        ));
    }
    let canonical_roots: Vec<PathBuf> = roots
        .iter()
        .map(|path| {
            fs::canonicalize(path)
                .map_err(|_| unavailable("attachment_file_root_missing", &path.to_string_lossy()))
        })
        .collect::<Result<_, _>>()?;
    let existing = nearest_existing_path(&candidate)?;
    let canonical_existing = fs::canonicalize(&existing)
        .map_err(|error| unavailable("attachment_file_path_parent_missing", &error.to_string()))?;
    if !canonical_roots
        .iter()
        .any(|parent| path_inside(&canonical_existing, parent))
    {
        return Err(unavailable(
            "attachment_file_path_symlink_escape",
            "destination parent escapes the configured attachment roots",
        ));
    }
    if candidate.exists() {
        let canonical_candidate = fs::canonicalize(&candidate)
            .map_err(|error| unavailable("attachment_file_path_symlink_escape", &error.to_string()))?;
        if !canonical_roots
            .iter()
            .any(|parent| path_inside(&canonical_candidate, parent))
        {
            return Err(unavailable(
                "attachment_file_path_symlink_escape",
                "destination escapes the configured attachment roots",
            ));
        }
    }
    Ok(candidate)
}

fn resolve_attachment_input_path(
    root: &Path,
    args: &Map<String, Value>,
    configured_roots: &[PathBuf],
) -> Result<PathBuf, Value> {
    let input = required_string(args, "file_path")?;
    let candidate = root.join(&input);
    let roots: Vec<PathBuf> = if configured_roots.is_empty() {
        vec![root.to_path_buf()]
    } else {
        configured_roots.to_vec()
    };
    if !roots.iter().any(|parent| path_inside(&candidate, parent)) {
        return Err(unavailable(
            "attachment_file_path_not_allowed",
            "file is outside the configured attachment roots",
        ));
    }
    let canonical_candidate = fs::canonicalize(&candidate)
        .map_err(|error| unavailable("attachment_file_path_not_allowed", &error.to_string()))?;
    let canonical_roots: Vec<PathBuf> = roots
        .iter()
        .map(|path| {
            fs::canonicalize(path)
                .map_err(|_| unavailable("attachment_file_root_missing", &path.to_string_lossy()))
        })
        .collect::<Result<_, _>>()?;
    if !canonical_roots
        .iter()
        .any(|parent| path_inside(&canonical_candidate, parent))
    {
        return Err(unavailable(
            "attachment_file_path_symlink_escape",
            "file escapes the configured attachment roots",
        ));
    }
    if !fs::metadata(&canonical_candidate)
        .map_err(|error| unavailable("attachment_file_stat_failed", &error.to_string()))?
        .is_file()
    {
        return Err(unavailable(
            "attachment_file_path_not_file",
            "attachment path is not a file",
        ));
    }
    Ok(canonical_candidate)
}

fn upload_chunk_size(args: &Map<String, Value>) -> Result<u64, Value> {
    let size = args
        .get("chunk_size")
        .and_then(Value::as_u64)
        .unwrap_or(DEFAULT_ATTACHMENT_UPLOAD_CHUNK_SIZE);
    if !(ATTACHMENT_UPLOAD_CHUNK_GRANULARITY..=10 * 1024 * 1024).contains(&size) {
        return Err(unavailable(
            "attachment_upload_chunk_size_invalid",
            "chunk size must be between 320 KiB and 10 MiB",
        ));
    }
    if size % ATTACHMENT_UPLOAD_CHUNK_GRANULARITY != 0 {
        return Err(unavailable(
            "attachment_upload_chunk_size_must_be_multiple_of_320kib",
            "chunk size must be a multiple of 320 KiB",
        ));
    }
    Ok(size)
}

fn nearest_existing_path(candidate: &Path) -> Result<PathBuf, Value> {
    let mut current = candidate.to_path_buf();
    while !current.exists() {
        let Some(parent) = current.parent() else {
            return Err(unavailable(
                "attachment_file_path_parent_missing",
                "destination parent is missing",
            ));
        };
        if parent == current {
            return Err(unavailable(
                "attachment_file_path_parent_missing",
                "destination parent is missing",
            ));
        }
        current = parent.to_path_buf();
    }
    Ok(current)
}

fn same_path(left: &Path, right: &Path) -> bool {
    let left = left.to_string_lossy();
    let right = right.to_string_lossy();
    if cfg!(windows) {
        left.eq_ignore_ascii_case(&right)
    } else {
        left == right
    }
}

fn path_inside(child: &Path, parent: &Path) -> bool {
    let child = child.to_string_lossy();
    let parent = parent.to_string_lossy();
    if cfg!(windows) {
        let child = child.to_ascii_lowercase();
        let parent = parent.to_ascii_lowercase();
        child == parent || child.starts_with(&format!("{}\\", parent)) || child.starts_with(&format!("{}/", parent))
    } else {
        child == parent || child.starts_with(&format!("{}/", parent))
    }
}

fn display_path(path: &Path) -> String {
    let value = path.to_string_lossy().to_string();
    if cfg!(windows) {
        value.replace('/', "\\")
    } else {
        value
    }
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

fn optional_config_string(object: &Map<String, Value>, snake: &str, camel: &str) -> Option<String> {
    object
        .get(snake)
        .or_else(|| object.get(camel))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

fn string_array(object: &Map<String, Value>, snake: &str, camel: &str) -> Vec<String> {
    object
        .get(snake)
        .or_else(|| object.get(camel))
        .and_then(Value::as_array)
        .map(|values| {
            values
                .iter()
                .filter_map(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(ToOwned::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

fn confirmed(args: &Map<String, Value>, snake: &str, camel: &str) -> bool {
    args.get(snake).and_then(Value::as_bool).or_else(|| args.get(camel).and_then(Value::as_bool)).unwrap_or(false)
}

fn mailbox<'a>(args: &'a Map<String, Value>) -> Option<&'a str> { args.get("mailbox_id").and_then(Value::as_str).filter(|value| !value.trim().is_empty()) }
fn mailbox_value(args: &Map<String, Value>) -> Value { mailbox(args).map(|value| json!(value)).unwrap_or_else(|| json!("me")) }
fn optional_string(args: &Map<String, Value>, key: &str) -> Option<String> { args.get(key).and_then(Value::as_str).map(str::trim).filter(|value| !value.is_empty()).map(ToOwned::to_owned) }
fn required_string(args: &Map<String, Value>, key: &str) -> Result<String, Value> { optional_string(args, key).ok_or_else(|| invalid(key)) }
fn required_positive_number(args: &Map<String, Value>, key: &str) -> Result<u64, Value> {
    let value = args
        .get(key)
        .and_then(Value::as_u64)
        .filter(|value| *value > 0)
        .ok_or_else(|| invalid(key))?;
    Ok(value)
}
fn required_nonnegative_number(args: &Map<String, Value>, key: &str) -> Result<u64, Value> {
    args.get(key)
        .and_then(Value::as_u64)
        .ok_or_else(|| invalid(key))
}
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
    fn operation_support_is_limited_to_ported_provider_slice() {
        assert!(supports("graph_mail_query"));
        assert!(supports("graph_mail_message_mark_read"));
        assert!(supports("graph_mail_attachment_upload_session_create"));
        assert!(supports("graph_mail_draft_send"));
        assert!(supports("graph_mail_reply_all_to_last_in_thread_draft_create"));
        assert!(supports("graph_mail_attachment_upload_chunk"));
        assert!(supports("graph_mail_attachment_upload_file"));
        assert!(!supports("graph_mail_ticket_draft_upsert"));
    }

    #[test]
    fn bounded_base64_decoder_matches_attachment_bytes() {
        assert_eq!(decode_base64("SGVsbG8=").unwrap(), b"Hello");
        assert!(decode_base64("not-base64").is_err());
    }
}
