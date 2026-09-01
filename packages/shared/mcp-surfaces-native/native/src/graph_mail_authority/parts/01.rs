use crate::graph_authority::CalendarGraphAdapter;
use serde_json::{json, Map, Value};
use rusqlite::{params, Connection, OptionalExtension};
use sha2::{Digest, Sha256};
use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;
use uuid::Uuid;

const MAX_CONFIG_BYTES: u64 = 512 * 1024;
const MAX_FLOW_BYTES: u64 = 128 * 1024;
const MAX_AUTH_RESPONSE_BYTES: u64 = 512 * 1024;
const MAX_AUDIT_BYTES: usize = 64 * 1024;
const DEFAULT_QUERY_TOP: u64 = 20;
const DEFAULT_FOLDER_TOP: u64 = 50;
const MAX_DOWNLOADED_ATTACHMENT_BYTES: usize = 25 * 1024 * 1024;
const ATTACHMENT_UPLOAD_CHUNK_GRANULARITY: u64 = 320 * 1024;
const DEFAULT_ATTACHMENT_UPLOAD_CHUNK_SIZE: u64 = 10 * ATTACHMENT_UPLOAD_CHUNK_GRANULARITY;
const MAX_ATTACHMENT_UPLOAD_FILE_BYTES: u64 = 512 * 1024 * 1024;
const TICKET_DRAFT_OPERATION_PROPERTY_ID: &str = "String {d700a6f2-79ad-4f44-9df7-3e9b622f09f8} Name NaradaTicketDraftOperation";
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

/// Direct, bounded Microsoft Graph authority for graph-mail.
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
            | "graph_mail_auth_device_code_start"
            | "graph_mail_auth_device_code_poll"
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
            | "graph_mail_ticket_draft_upsert"
            | "graph_mail_ticket_draft_discard"
            | "graph_mail_ticket_draft_disposition_scan"
            | "graph_mail_ticket_draft_disposition_list"
            | "graph_mail_ticket_draft_disposition_ack"
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
        "graph_mail_auth_device_code_start" => auth_device_code_start(&policy, args, root),
        "graph_mail_auth_device_code_poll" => auth_device_code_poll(&policy, args, root),
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
        "graph_mail_ticket_draft_upsert" => ticket_draft_upsert(&policy, args, root),
        "graph_mail_ticket_draft_discard" => ticket_draft_discard(&policy, args, root),
        "graph_mail_ticket_draft_disposition_scan" => ticket_disposition_scan(&policy, args, root),
        "graph_mail_ticket_draft_disposition_list" => ticket_disposition_list(args, root),
        "graph_mail_ticket_draft_disposition_ack" => ticket_disposition_ack(args, root),
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
    reply_signature_name: Option<String>,
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
        let object = object
            .as_object()
            .cloned()
            .ok_or_else(|| unavailable("graph_mail_config_invalid", "policy must be a JSON object"))?;
        let attachment_roots = object
            .get("allowed_attachment_roots")
            .or_else(|| object.get("allowedAttachmentRoots"))
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        if attachment_roots.len() > 32
            || attachment_roots.iter().any(|value| {
                value
                    .as_str()
                    .is_none_or(|value| value.trim().is_empty() || value.len() > 4096)
            })
        {
            return Err(unavailable(
                "graph_mail_attachment_roots_invalid",
                "allowed_attachment_roots permits at most 32 non-empty paths of at most 4096 bytes",
            ));
        }
        let allowed_scopes = string_array(&object, "device_code_allowed_scopes", "deviceCodeAllowedScopes");
        if allowed_scopes.len() > 16 || allowed_scopes.iter().any(|value| value.len() > 4096) {
            return Err(unavailable(
                "graph_mail_device_code_scopes_invalid",
                "device-code policy permits at most 16 scope sets of at most 4096 bytes",
            ));
        }
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
            allowed_attachment_roots: attachment_roots
                .iter()
                .filter_map(Value::as_str)
                .map(|value| root.join(value))
                .collect(),
            allow_device_code_auth: bool_value(&object, "allow_device_code_auth", "allowDeviceCodeAuth"),
            device_code_tenant_id: optional_config_string(&object, "device_code_tenant_id", "deviceCodeTenantId"),
            device_code_client_id: optional_config_string(&object, "device_code_client_id", "deviceCodeClientId"),
            device_code_allowed_scopes: allowed_scopes,
            organization_approval_token,
            reply_signature_name: optional_config_string(
                &object,
                "reply_signature_name",
                "replySignatureName",
            ),
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

