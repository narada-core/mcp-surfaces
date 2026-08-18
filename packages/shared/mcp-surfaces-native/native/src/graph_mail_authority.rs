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

fn read_auth_response(response: ureq::Response) -> Result<(u16, Value), Value> {
    let status = response.status();
    let mut reader = response.into_reader().take(MAX_AUTH_RESPONSE_BYTES + 1);
    let mut bytes = Vec::new();
    reader
        .read_to_end(&mut bytes)
        .map_err(|error| unavailable("graph_auth_response_read_failed", &error.to_string()))?;
    if bytes.len() as u64 > MAX_AUTH_RESPONSE_BYTES {
        return Err(unavailable("graph_auth_response_too_large", &MAX_AUTH_RESPONSE_BYTES.to_string()));
    }
    let text = String::from_utf8_lossy(&bytes);
    let payload = serde_json::from_str::<Value>(&text).unwrap_or_else(|_| json!({}));
    Ok((status, payload))
}

fn required_value_string(value: &Value, key: &str) -> Result<String, Value> {
    value
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .ok_or_else(|| invalid(key))
}

fn flow_path(root: &Path, flow_id: &str) -> PathBuf {
    let safe = flow_id
        .chars()
        .map(|value| if value.is_ascii_alphanumeric() || matches!(value, '-' | '_' | '.') { value } else { '_' })
        .collect::<String>();
    root.join(format!(".ai/runtime/graph-mail-mcp/device-code-flows/{safe}.json"))
}

fn read_flow(root: &Path, flow_id: &str) -> Result<Option<Value>, Value> {
    let path = flow_path(root, flow_id);
    if !path.exists() {
        return Ok(None);
    }
    let metadata = fs::metadata(&path)
        .map_err(|error| unavailable("device_code_flow_read_failed", &error.to_string()))?;
    if metadata.len() > MAX_FLOW_BYTES {
        return Err(unavailable("device_code_flow_too_large", &MAX_FLOW_BYTES.to_string()));
    }
    let text = fs::read_to_string(&path)
        .map_err(|error| unavailable("device_code_flow_read_failed", &error.to_string()))?;
    serde_json::from_str::<Value>(&text)
        .map(Some)
        .map_err(|error| unavailable("device_code_flow_invalid", &error.to_string()))
}

fn write_bounded_json(path: &Path, value: &Value) -> Result<(), Value> {
    let text = serde_json::to_string_pretty(value)
        .map_err(|error| unavailable("graph_auth_state_encode_failed", &error.to_string()))?;
    if text.len() as u64 > MAX_FLOW_BYTES {
        return Err(unavailable("graph_auth_state_too_large", &MAX_FLOW_BYTES.to_string()));
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|error| unavailable("graph_auth_state_directory_failed", &error.to_string()))?;
    }
    fs::write(path, text)
        .map_err(|error| unavailable("graph_auth_state_write_failed", &error.to_string()))
}

fn now_millis() -> i64 {
    (OffsetDateTime::now_utc().unix_timestamp_nanos() / 1_000_000) as i64
}

fn seconds_millis(seconds: u64) -> i64 {
    seconds
        .min(i64::MAX as u64 / 1_000)
        .saturating_mul(1_000) as i64
}

fn now_rfc3339() -> String {
    OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .unwrap_or_else(|_| "unknown".to_string())
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
    let composed_html = compose_reply_html(
        &comment_html,
        &quote_html,
        policy.reply_signature_name.as_deref(),
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
        "reply_signature_name":policy.reply_signature_name,
        "signature_applied":policy.reply_signature_name.is_some(),
        "quote_preserved":true,
        "unsent":patched.get("isDraft").and_then(Value::as_bool) != Some(false)
    }))
}

fn compose_reply_html(comment_html: &str, quote_html: &str, signature_name: Option<&str>) -> String {
    let signature_html = signature_name
        .map(|name| format!("<p>Thanks,<br>{}</p>", escape_html(name)))
        .unwrap_or_default();
    format!(
        "{}{}<div data-narada-quoted-history=\"true\">{}</div>",
        comment_html, signature_html, quote_html
    )
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

fn ticket_draft_upsert(
    policy: &Policy,
    args: &Map<String, Value>,
    root: &Path,
) -> Result<Value, Value> {
    let body_text = optional_string(args, "body_text");
    let body_html = optional_string(args, "body_html");
    if body_text.is_some() == body_html.is_some() {
        return Err(unavailable(
            "graph_ticket_draft_exactly_one_body_required",
            "provide exactly one of body_text or body_html",
        ));
    }
    let reply_mode = required_string(args, "reply_mode")?;
    if reply_mode != "reply" && reply_mode != "reply_all" {
        return Err(unavailable(
            "graph_ticket_draft_reply_mode_invalid",
            "reply_mode must be reply or reply_all",
        ));
    }
    let operation_key = required_string(args, "draft_operation_key")?;
    if operation_key.len() > 256
        || operation_key.is_empty()
        || !operation_key
            .chars()
            .all(|value| value.is_ascii_alphanumeric() || matches!(value, '.' | '_' | ':' | '-'))
    {
        return Err(unavailable(
            "graph_ticket_draft_operation_key_invalid",
            "operation key contains unsupported characters",
        ));
    }
    let admitted_digest = required_string(args, "draft_request_digest")?.to_ascii_lowercase();
    if admitted_digest.len() != 64 || !admitted_digest.chars().all(|value| value.is_ascii_hexdigit()) {
        return Err(unavailable(
            "graph_ticket_draft_request_digest_invalid",
            "draft request digest must be 64 hexadecimal characters",
        ));
    }
    let ticket_id = required_string(args, "ticket_id")?;
    let effect_claim_id = required_string(args, "effect_claim_id")?;
    let draft_source_id = required_string(args, "draft_source_id")?;
    let mailbox_id = required_string(args, "mailbox_id")?;
    let source_message_id = required_string(args, "source_message_id")?;
    let idempotency_key = required_string(args, "idempotency_key")?;
    let mut normalized = Map::new();
    normalized.insert("ticket_id".to_string(), json!(ticket_id));
    normalized.insert("effect_claim_id".to_string(), json!(effect_claim_id));
    normalized.insert("draft_operation_key".to_string(), json!(operation_key));
    normalized.insert("draft_request_digest".to_string(), json!(admitted_digest));
    normalized.insert("draft_source_id".to_string(), json!(draft_source_id));
    normalized.insert("mailbox_id".to_string(), json!(mailbox_id));
    normalized.insert("source_message_id".to_string(), json!(source_message_id));
    normalized.insert("reply_mode".to_string(), json!(reply_mode));
    if let Some(value) = body_text.as_deref() {
        normalized.insert("body_text".to_string(), json!(value));
    }
    if let Some(value) = body_html.as_deref() {
        normalized.insert("body_html".to_string(), json!(value));
    }
    normalized.insert("idempotency_key".to_string(), json!(idempotency_key));
    let mut draft_request = Map::new();
    draft_request.insert("source_id".to_string(), json!(draft_source_id));
    draft_request.insert("mailbox_id".to_string(), json!(mailbox_id));
    draft_request.insert("source_message_id".to_string(), json!(source_message_id));
    draft_request.insert("reply_mode".to_string(), json!(reply_mode));
    if let Some(value) = body_text.as_deref() {
        draft_request.insert("body_text".to_string(), json!(value));
    }
    if let Some(value) = body_html.as_deref() {
        draft_request.insert("body_html".to_string(), json!(value));
    }
    let actual_digest = sha256_canonical(&Value::Object(draft_request));
    if actual_digest != admitted_digest {
        return Err(unavailable(
            "graph_ticket_draft_request_digest_mismatch",
            &format!("{admitted_digest}:{actual_digest}"),
        ));
    }
    let request_digest = sha256_canonical(&Value::Object(normalized.clone()));
    let connection = ticket_store(root)?;
    connection
        .execute_batch("BEGIN IMMEDIATE")
        .map_err(|error| unavailable("graph_ticket_draft_transaction_failed", &error.to_string()))?;
    let outcome = (|| {
        let mut operation = find_ticket_operation(&connection, &operation_key)?;
        let replayed = operation.is_some();
        if let Some(existing) = operation.as_ref() {
            assert_ticket_operation_matches(
                existing,
                &operation_key,
                &request_digest,
                &admitted_digest,
                &ticket_id,
                &effect_claim_id,
                &mailbox_id,
                &source_message_id,
                &reply_mode,
                &idempotency_key,
            )?;
            if existing.status == "completed" {
                return ticket_domain_operation(existing, true);
            }
        } else {
            let now = now_rfc3339();
            connection
                .execute(
                    "insert into graph_ticket_draft_operations(operation_key, action_idempotency_key, request_digest, draft_request_digest, ticket_id, effect_claim_id, mailbox_id, source_message_id, reply_mode, status, draft_id, receipt_id, draft_ref_json, created_at, updated_at, completed_at) values (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, 'pending', null, null, null, ?10, ?10, null)",
                    params![operation_key, idempotency_key, request_digest, admitted_digest, ticket_id, effect_claim_id, mailbox_id, source_message_id, reply_mode, now],
                )
                .map_err(|error| unavailable("graph_ticket_draft_insert_failed", &error.to_string()))?;
            operation = find_ticket_operation(&connection, &operation_key)?;
        }
        let _existing = operation.ok_or_else(|| unavailable("graph_ticket_draft_operation_not_found", &operation_key))?;
        let mut draft = find_ticket_remote_draft(policy, &mailbox_id, &operation_key)?;
        let mut recovered = true;
        if draft.is_none() {
            recovered = false;
            let mut message = Map::new();
            if let Some(value) = normalized.get("body_text").and_then(Value::as_str) {
                message.insert("body".to_string(), json!({"contentType":"Text","content":value}));
            }
            if let Some(value) = normalized.get("body_html").and_then(Value::as_str) {
                message.insert("body".to_string(), json!({"contentType":"HTML","content":value}));
            }
            message.insert(
                "singleValueExtendedProperties".to_string(),
                json!([{"id":TICKET_DRAFT_OPERATION_PROPERTY_ID,"value":operation_key}]),
            );
            let action = if reply_mode == "reply" { "createReply" } else { "createReplyAll" };
            let suffix = format!("messages/{}/{}", encode_component(&source_message_id), action);
            record_audit(root, json!({
                "event_kind":"ticket_draft_create_requested",
                "ticket_id":ticket_id,
                "effect_claim_id":effect_claim_id,
                "draft_operation_key":operation_key,
                "mailbox_id":mailbox_id,
                "source_message_id":source_message_id,
                "reply_mode":reply_mode,
                "draft_request_digest":admitted_digest
            }))?;
            let created = policy.adapter.request(
                "POST",
                Some(&mailbox_id),
                &suffix,
                &Map::new(),
                Some(&json!({"message":Value::Object(message)})),
            )?;
            if created.get("isDraft").and_then(Value::as_bool) == Some(false)
                || created.get("id").and_then(Value::as_str).is_none()
            {
                return Err(unavailable("graph_ticket_draft_create_result_invalid", "Graph did not return an unsent draft"));
            }
            record_audit(root, json!({"event_kind":"ticket_draft_create_completed","ticket_id":ticket_id,"effect_claim_id":effect_claim_id,"draft_operation_key":operation_key,"draft_id":created.get("id").cloned().unwrap_or(Value::Null)}))?;
            draft = Some(created);
        }
        let draft = draft.ok_or_else(|| unavailable("graph_ticket_draft_create_result_invalid", "draft missing"))?;
        let draft_id = required_draft_id(&draft)?;
        let draft_ref = ticket_draft_ref_value(&normalized, &draft, &draft_id);
        let receipt_id = stable_receipt_id(&operation_key, &draft_id);
        let completed_at = now_rfc3339();
        connection
            .execute(
                "update graph_ticket_draft_operations set status='completed', draft_id=?1, receipt_id=?2, draft_ref_json=?3, updated_at=?4, completed_at=?4 where operation_key=?5 and status='pending'",
                params![draft_id, receipt_id, canonical_json(&draft_ref), completed_at, operation_key],
            )
            .map_err(|error| unavailable("graph_ticket_draft_completion_failed", &error.to_string()))?;
        let completed = find_ticket_operation(&connection, &operation_key)?
            .ok_or_else(|| unavailable("graph_ticket_draft_operation_not_found", &operation_key))?;
        ticket_domain_operation(&completed, replayed || recovered)
    })();
    match outcome {
        Ok(value) => {
            connection
                .execute_batch("COMMIT")
                .map_err(|error| unavailable("graph_ticket_draft_commit_failed", &error.to_string()))?;
            Ok(value)
        }
        Err(error) => {
            let _ = connection.execute_batch("ROLLBACK");
            Err(error)
        }
    }
}

fn ticket_draft_discard(
    policy: &Policy,
    args: &Map<String, Value>,
    root: &Path,
) -> Result<Value, Value> {
    if args.get("confirm_discard").and_then(Value::as_bool) != Some(true) {
        return Err(unavailable(
            "graph_ticket_draft_discard_confirmation_required",
            "confirm_discard=true is required",
        ));
    }
    let ticket_id = required_string(args, "ticket_id")?;
    let effect_claim_id = required_string(args, "effect_claim_id")?;
    let operation_key = required_string(args, "draft_operation_key")?;
    let mailbox_id = required_string(args, "mailbox_id")?;
    let draft_id = required_string(args, "draft_id")?;
    let idempotency_key = required_string(args, "idempotency_key")?;
    let request = json!({
        "ticket_id":ticket_id,
        "effect_claim_id":effect_claim_id,
        "draft_operation_key":operation_key,
        "mailbox_id":mailbox_id,
        "draft_id":draft_id,
        "idempotency_key":idempotency_key,
        "confirm_discard":true
    });
    let request_digest = sha256_canonical(&request);
    let connection = ticket_store(root)?;
    connection
        .execute_batch("BEGIN IMMEDIATE")
        .map_err(|error| unavailable("graph_ticket_draft_transaction_failed", &error.to_string()))?;
    let outcome = (|| {
        let operation = find_ticket_operation(&connection, &operation_key)?
            .ok_or_else(|| unavailable("graph_ticket_draft_operation_not_completed", &operation_key))?;
        if operation.status != "completed" {
            return Err(unavailable("graph_ticket_draft_operation_not_completed", &operation_key));
        }
        if operation.ticket_id != ticket_id
            || operation.effect_claim_id != effect_claim_id
            || operation.mailbox_id != mailbox_id
            || operation.draft_id.as_deref() != Some(draft_id.as_str())
        {
            return Err(unavailable("graph_ticket_draft_discard_linkage_mismatch", &operation_key));
        }
        let now = now_rfc3339();
        connection
            .execute(
                "insert into graph_ticket_draft_discard_intents(operation_key, idempotency_key, request_digest, status, verified_etag, verified_at, receipt_json, created_at, updated_at, completed_at) values (?1, ?2, ?3, 'pending', null, null, null, ?4, ?4, null) on conflict(operation_key) do nothing",
                params![operation_key, idempotency_key, request_digest, now],
            )
            .map_err(|error| unavailable("graph_ticket_draft_discard_intent_failed", &error.to_string()))?;
        let mut intent = find_discard_intent(&connection, &operation_key)?
            .ok_or_else(|| unavailable("graph_ticket_draft_discard_intent_not_found", &operation_key))?;
        if intent.idempotency_key != idempotency_key || intent.request_digest != request_digest {
            return Err(unavailable("graph_ticket_draft_discard_idempotency_conflict", &operation_key));
        }
        if intent.status == "completed" {
            let receipt = intent.receipt.take().ok_or_else(|| unavailable("graph_ticket_draft_discard_receipt_missing", &operation_key))?;
            return Ok(json!({"schema":"narada.graph_mail.ticket_draft_discard.v1","status":"discarded","disposition_receipt":receipt,"idempotency_replayed_or_recovered":true}));
        }
        let messages = find_ticket_remote_messages(policy, &mailbox_id, &operation_key)?;
        if messages.len() > 1 {
            return Err(unavailable("graph_ticket_draft_discard_remote_identity_ambiguous", &operation_key));
        }
        let Some(observed) = messages.into_iter().next() else {
            if intent.status != "verified" {
                return Err(unavailable("graph_ticket_draft_discard_absence_not_evidence", &operation_key));
            }
            let receipt = ticket_discard_receipt(&operation, "operator_authorized_graph_absence_after_verified_discard", false, true);
            complete_discard_intent(&connection, &operation_key, &receipt, &operation)?;
            return Ok(json!({"schema":"narada.graph_mail.ticket_draft_discard.v1","status":"discarded","disposition_receipt":receipt,"idempotency_replayed_or_recovered":true}));
        };
        let observed_id = observed.get("id").and_then(Value::as_str).ok_or_else(|| unavailable("graph_ticket_draft_discard_remote_identity_missing", &operation_key))?;
        if observed_id != draft_id {
            return Err(unavailable("graph_ticket_draft_discard_remote_identity_mismatch", &operation_key));
        }
        if observed.get("isDraft").and_then(Value::as_bool) != Some(true) {
            return Err(unavailable("graph_ticket_draft_discard_refused_not_draft", &operation_key));
        }
        let verifier = observed
            .get("@odata.etag")
            .and_then(Value::as_str)
            .or_else(|| observed.get("changeKey").and_then(Value::as_str))
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| unavailable("graph_ticket_draft_discard_remote_verifier_missing", &operation_key))?;
        connection
            .execute(
                "update graph_ticket_draft_discard_intents set status='verified', verified_etag=?1, verified_at=?2, updated_at=?2 where operation_key=?3 and status in ('pending','verified')",
                params![verifier, now_rfc3339(), operation_key],
            )
            .map_err(|error| unavailable("graph_ticket_draft_discard_verify_failed", &error.to_string()))?;
        record_audit(root, json!({"event_kind":"ticket_draft_discard_requested","ticket_id":ticket_id,"effect_claim_id":effect_claim_id,"draft_operation_key":operation_key,"mailbox_id":mailbox_id,"draft_id":draft_id}))?;
        let mut headers = Map::new();
        headers.insert("If-Match".to_string(), json!(verifier));
        policy.adapter.request_with_headers(
            "DELETE",
            Some(&mailbox_id),
            &format!("messages/{}", encode_component(&draft_id)),
            &Map::new(),
            None,
            &headers,
        )?;
        record_audit(root, json!({"event_kind":"ticket_draft_discard_completed","ticket_id":ticket_id,"effect_claim_id":effect_claim_id,"draft_operation_key":operation_key,"mailbox_id":mailbox_id,"draft_id":draft_id}))?;
        let receipt = ticket_discard_receipt(&operation, "operator_confirmed_graph_discard", true, false);
        complete_discard_intent(&connection, &operation_key, &receipt, &operation)?;
        Ok(json!({"schema":"narada.graph_mail.ticket_draft_discard.v1","status":"discarded","disposition_receipt":receipt,"idempotency_replayed_or_recovered":false}))
    })();
    match outcome {
        Ok(value) => {
            connection
                .execute_batch("COMMIT")
                .map_err(|error| unavailable("graph_ticket_draft_commit_failed", &error.to_string()))?;
            Ok(value)
        }
        Err(error) => {
            let _ = connection.execute_batch("ROLLBACK");
            Err(error)
        }
    }
}

#[derive(Clone)]
struct DiscardIntent {
    idempotency_key: String,
    request_digest: String,
    status: String,
    receipt: Option<Value>,
}

fn find_discard_intent(connection: &Connection, operation_key: &str) -> Result<Option<DiscardIntent>, Value> {
    connection
        .query_row(
            "select idempotency_key, request_digest, status, receipt_json from graph_ticket_draft_discard_intents where operation_key=?1",
            params![operation_key],
            |row| {
                let receipt: Option<String> = row.get(3)?;
                Ok(DiscardIntent {
                    idempotency_key: row.get(0)?,
                    request_digest: row.get(1)?,
                    status: row.get(2)?,
                    receipt: receipt.and_then(|value| serde_json::from_str(&value).ok()),
                })
            },
        )
        .optional()
        .map_err(|error| unavailable("graph_ticket_draft_discard_database_read_failed", &error.to_string()))
}

fn find_ticket_remote_messages(
    policy: &Policy,
    mailbox_id: &str,
    operation_key: &str,
) -> Result<Vec<Value>, Value> {
    let property_id = TICKET_DRAFT_OPERATION_PROPERTY_ID.replace('\'', "''");
    let property_value = operation_key.replace('\'', "''");
    let mut query = Map::new();
    query.insert(
        "$filter".to_string(),
        json!(format!("singleValueExtendedProperties/Any(ep: ep/id eq '{property_id}' and ep/value eq '{property_value}')")),
    );
    query.insert(
        "$expand".to_string(),
        json!(format!("singleValueExtendedProperties($filter=id eq '{property_id}')")),
    );
    query.insert(
        "$select".to_string(),
        json!("id,isDraft,changeKey,createdDateTime,lastModifiedDateTime,sentDateTime,parentFolderId"),
    );
    query.insert("$top".to_string(), json!(2));
    let result = policy.adapter.request("GET", Some(mailbox_id), "messages", &query, None)?;
    Ok(result
        .get("value")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .filter(|value| value.get("id").and_then(Value::as_str).is_some())
        .collect())
}

fn ticket_discard_receipt(
    operation: &TicketOperation,
    evidence_kind: &str,
    graph_delete_confirmed: bool,
    graph_absence_confirmed: bool,
) -> Value {
    let draft_id = operation.draft_id.clone().unwrap_or_default();
    let observation_id = stable_disposition_observation_id(&operation.operation_key, "discarded", &draft_id);
    let mut receipt = json!({
        "schema":"narada.graph_mail.ticket_draft_disposition_receipt.v1",
        "observation_id":observation_id,
        "evidence_kind":evidence_kind,
        "evidence_id":observation_id,
        "disposition":"discarded",
        "ticket_id":operation.ticket_id,
        "effect_claim_id":operation.effect_claim_id,
        "draft_operation_key":operation.operation_key,
        "mailbox_id":operation.mailbox_id,
        "draft_id":draft_id,
        "observed_message_id":draft_id,
        "is_draft":true,
        "graph_delete_confirmed":graph_delete_confirmed,
        "graph_absence_confirmed":graph_absence_confirmed,
        "observed_at":now_rfc3339()
    });
    let digest = sha256_canonical(&receipt);
    receipt.as_object_mut().unwrap().insert("receipt_sha256".to_string(), json!(digest));
    receipt
}

fn complete_discard_intent(
    connection: &Connection,
    operation_key: &str,
    receipt: &Value,
    operation: &TicketOperation,
) -> Result<(), Value> {
    let receipt_json = canonical_json(receipt);
    connection
        .execute(
            "insert into graph_ticket_draft_disposition_observations(observation_id, operation_key, ticket_id, mailbox_id, draft_id, disposition, evidence_kind, evidence_id, receipt_json, observed_at) values (?1, ?2, ?3, ?4, ?5, 'discarded', ?6, ?7, ?8, ?9) on conflict(operation_key) do nothing",
            params![receipt.get("observation_id").and_then(Value::as_str).unwrap_or_default(), operation_key, operation.ticket_id, operation.mailbox_id, operation.draft_id.clone().unwrap_or_default(), receipt.get("evidence_kind").and_then(Value::as_str).unwrap_or_default(), receipt.get("evidence_id").and_then(Value::as_str).unwrap_or_default(), receipt_json, receipt.get("observed_at").and_then(Value::as_str).unwrap_or_default()],
        )
        .map_err(|error| unavailable("graph_ticket_draft_disposition_record_failed", &error.to_string()))?;
    connection
        .execute(
            "update graph_ticket_draft_discard_intents set status='completed', receipt_json=?1, updated_at=?2, completed_at=?2 where operation_key=?3 and status in ('verified','pending')",
            params![receipt_json, now_rfc3339(), operation_key],
        )
        .map_err(|error| unavailable("graph_ticket_draft_discard_completion_failed", &error.to_string()))?;
    Ok(())
}

fn stable_disposition_observation_id(operation_key: &str, disposition: &str, observed_message_id: &str) -> String {
    let input = format!("{operation_key}\0{disposition}\0{observed_message_id}");
    format!("graph_draft_disposition_{}", &hex_lower(&Sha256::digest(input.as_bytes()))[..32])
}

fn ticket_disposition_scan(
    policy: &Policy,
    args: &Map<String, Value>,
    root: &Path,
) -> Result<Value, Value> {
    let limit = args
        .get("limit")
        .and_then(Value::as_u64)
        .unwrap_or(5)
        .clamp(1, 5);
    let connection = ticket_store(root)?;
    let mut statement = connection
        .prepare(
            "select operation_key from graph_ticket_draft_operations operation where operation.status='completed' and not exists (select 1 from graph_ticket_draft_disposition_observations observation where observation.operation_key=operation.operation_key) order by operation.completed_at asc, operation.operation_key asc limit ?1",
        )
        .map_err(|error| unavailable("graph_ticket_draft_scan_query_failed", &error.to_string()))?;
    let keys = statement
        .query_map(params![limit], |row| row.get::<_, String>(0))
        .map_err(|error| unavailable("graph_ticket_draft_scan_query_failed", &error.to_string()))?
        .filter_map(Result::ok)
        .collect::<Vec<_>>();
    drop(statement);
    let mut errors = Vec::new();
    let mut observations_recorded = 0u64;
    let mut still_pending = 0u64;
    for operation_key in &keys {
        let result = (|| {
            let operation = find_ticket_operation(&connection, operation_key)?
                .ok_or_else(|| unavailable("graph_ticket_draft_operation_not_found", operation_key))?;
            let messages = find_ticket_remote_messages(policy, &operation.mailbox_id, operation_key)?;
            if messages.len() > 1 {
                return Err(unavailable("graph_ticket_draft_disposition_remote_identity_ambiguous", operation_key));
            }
            let Some(observed) = messages.into_iter().next() else {
                return Ok(false);
            };
            if observed.get("isDraft").and_then(Value::as_bool) != Some(false) {
                return Ok(false);
            }
            let observed_id = observed
                .get("id")
                .and_then(Value::as_str)
                .ok_or_else(|| unavailable("graph_ticket_draft_disposition_message_id_missing", operation_key))?;
            let observation_id = stable_disposition_observation_id(operation_key, "sent", observed_id);
            let mut receipt = json!({
                "schema":"narada.graph_mail.ticket_draft_disposition_receipt.v1",
                "observation_id":observation_id,
                "evidence_kind":"synchronized_graph_observation",
                "evidence_id":observation_id,
                "disposition":"sent",
                "ticket_id":operation.ticket_id,
                "effect_claim_id":operation.effect_claim_id,
                "draft_operation_key":operation_key,
                "mailbox_id":operation.mailbox_id,
                "draft_id":operation.draft_id,
                "observed_message_id":observed_id,
                "is_draft":false,
                "observed_at":now_rfc3339()
            });
            if let Some(value) = observed.get("@odata.etag").and_then(Value::as_str) {
                receipt.as_object_mut().unwrap().insert("etag".to_string(), json!(value));
            }
            if let Some(value) = observed.get("changeKey").and_then(Value::as_str) {
                receipt.as_object_mut().unwrap().insert("change_key".to_string(), json!(value));
            }
            if let Some(value) = observed.get("lastModifiedDateTime").and_then(Value::as_str) {
                receipt.as_object_mut().unwrap().insert("last_modified_at".to_string(), json!(value));
            }
            let digest = sha256_canonical(&receipt);
            receipt.as_object_mut().unwrap().insert("receipt_sha256".to_string(), json!(digest));
            let recorded = insert_disposition_observation(&connection, &operation, &receipt)?;
            connection
                .execute(
                    "insert into graph_ticket_draft_disposition_scan_state(operation_key, last_scanned_at, scan_count) values (?1, ?2, 1) on conflict(operation_key) do update set last_scanned_at=excluded.last_scanned_at, scan_count=graph_ticket_draft_disposition_scan_state.scan_count+1",
                    params![operation_key, now_rfc3339()],
                )
                .map_err(|error| unavailable("graph_ticket_draft_scan_state_failed", &error.to_string()))?;
            Ok(recorded)
        })();
        match result {
            Ok(true) => observations_recorded += 1,
            Ok(false) => {
                still_pending += 1;
                let _ = connection.execute(
                    "insert into graph_ticket_draft_disposition_scan_state(operation_key, last_scanned_at, scan_count) values (?1, ?2, 1) on conflict(operation_key) do update set last_scanned_at=excluded.last_scanned_at, scan_count=graph_ticket_draft_disposition_scan_state.scan_count+1",
                    params![operation_key, now_rfc3339()],
                );
            }
            Err(error) => {
                errors.push(json!({"operation_key":operation_key,"error":error}));
                let _ = connection.execute(
                    "insert into graph_ticket_draft_disposition_scan_state(operation_key, last_scanned_at, scan_count) values (?1, ?2, 1) on conflict(operation_key) do update set last_scanned_at=excluded.last_scanned_at, scan_count=graph_ticket_draft_disposition_scan_state.scan_count+1",
                    params![operation_key, now_rfc3339()],
                );
            }
        }
    }
    Ok(json!({
        "schema":"narada.graph_mail.ticket_draft_disposition_scan.v1",
        "status":if errors.is_empty() { "completed" } else { "completed_with_errors" },
        "operations_scanned":keys.len(),
        "observations_recorded":observations_recorded,
        "still_pending":still_pending,
        "errors":errors
    }))
}

fn insert_disposition_observation(
    connection: &Connection,
    operation: &TicketOperation,
    receipt: &Value,
) -> Result<bool, Value> {
    let receipt_json = canonical_json(receipt);
    let result = connection
        .execute(
            "insert into graph_ticket_draft_disposition_observations(observation_id, operation_key, ticket_id, mailbox_id, draft_id, disposition, evidence_kind, evidence_id, receipt_json, observed_at) values (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10) on conflict(operation_key) do nothing",
            params![receipt.get("observation_id").and_then(Value::as_str).unwrap_or_default(), operation.operation_key, operation.ticket_id, operation.mailbox_id, operation.draft_id.clone().unwrap_or_default(), receipt.get("disposition").and_then(Value::as_str).unwrap_or_default(), receipt.get("evidence_kind").and_then(Value::as_str).unwrap_or_default(), receipt.get("evidence_id").and_then(Value::as_str).unwrap_or_default(), receipt_json, receipt.get("observed_at").and_then(Value::as_str).unwrap_or_default()],
        )
        .map_err(|error| unavailable("graph_ticket_draft_disposition_record_failed", &error.to_string()))?;
    Ok(result == 1)
}

fn ticket_disposition_list(args: &Map<String, Value>, root: &Path) -> Result<Value, Value> {
    let consumer_id = required_string(args, "consumer_id")?;
    let limit = args
        .get("limit")
        .and_then(Value::as_u64)
        .unwrap_or(5)
        .clamp(1, 5);
    let connection = ticket_store(root)?;
    let mut statement = connection
        .prepare("select observation.receipt_json from graph_ticket_draft_disposition_observations observation where not exists (select 1 from graph_ticket_draft_disposition_receipts receipt where receipt.observation_id=observation.observation_id and receipt.consumer_id=?1) order by observation.observed_at asc, observation.observation_id asc limit ?2")
        .map_err(|error| unavailable("graph_ticket_draft_disposition_list_failed", &error.to_string()))?;
    let rows = statement
        .query_map(params![consumer_id, limit], |row| row.get::<_, String>(0))
        .map_err(|error| unavailable("graph_ticket_draft_disposition_list_failed", &error.to_string()))?;
    let mut items = Vec::new();
    for row in rows {
        let text = row.map_err(|error| unavailable("graph_ticket_draft_disposition_list_failed", &error.to_string()))?;
        if let Ok(value) = serde_json::from_str::<Value>(&text) {
            items.push(value);
        }
    }
    Ok(json!({
        "schema":"narada.graph_mail.ticket_draft_disposition_list.v1",
        "consumer_id":consumer_id,
        "items":items,
        "count":items.len()
    }))
}

fn ticket_disposition_ack(args: &Map<String, Value>, root: &Path) -> Result<Value, Value> {
    let observation_id = required_string(args, "observation_id")?;
    let consumer_id = required_string(args, "consumer_id")?;
    let reconciliation_ref = required_string(args, "reconciliation_ref")?;
    let receipt = args
        .get("reconciliation_receipt")
        .cloned()
        .filter(|value| value.is_object())
        .unwrap_or_else(|| json!({}));
    let receipt_json = canonical_json(&receipt);
    let connection = ticket_store(root)?;
    let changes = connection
        .execute(
            "insert into graph_ticket_draft_disposition_receipts(observation_id, consumer_id, reconciliation_ref, receipt_json, acknowledged_at) values (?1, ?2, ?3, ?4, ?5) on conflict(observation_id, consumer_id) do nothing",
            params![observation_id, consumer_id, reconciliation_ref, receipt_json, now_rfc3339()],
        )
        .map_err(|error| unavailable("graph_ticket_draft_disposition_ack_failed", &error.to_string()))?;
    let existing = connection
        .query_row(
            "select reconciliation_ref, receipt_json from graph_ticket_draft_disposition_receipts where observation_id=?1 and consumer_id=?2",
            params![observation_id, consumer_id],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
        )
        .optional()
        .map_err(|error| unavailable("graph_ticket_draft_disposition_ack_failed", &error.to_string()))?;
    let Some((stored_ref, stored_receipt)) = existing else {
        return Err(unavailable("graph_ticket_draft_disposition_ack_not_found", &observation_id));
    };
    if stored_ref != reconciliation_ref || stored_receipt != receipt_json {
        return Err(unavailable("graph_ticket_draft_disposition_ack_conflict", &observation_id));
    }
    Ok(json!({
        "schema":"narada.graph_mail.ticket_draft_disposition_ack.v1",
        "status":if changes == 1 { "acknowledged" } else { "already_acknowledged" },
        "observation_id":observation_id,
        "consumer_id":consumer_id,
        "reconciliation_ref":reconciliation_ref
    }))
}

#[derive(Clone)]
struct TicketOperation {
    operation_key: String,
    action_idempotency_key: String,
    request_digest: String,
    draft_request_digest: String,
    ticket_id: String,
    effect_claim_id: String,
    mailbox_id: String,
    source_message_id: String,
    reply_mode: String,
    status: String,
    draft_id: Option<String>,
    receipt_id: Option<String>,
    draft_ref: Option<Value>,
    completed_at: Option<String>,
}

fn ticket_store(root: &Path) -> Result<Connection, Value> {
    let directory = root.join(".narada/runtime/graph-mail-domain");
    fs::create_dir_all(&directory)
        .map_err(|error| unavailable("graph_ticket_draft_directory_failed", &error.to_string()))?;
    let connection = Connection::open(directory.join("graph-mail-domain.db"))
        .map_err(|error| unavailable("graph_ticket_draft_database_open_failed", &error.to_string()))?;
    connection
        .execute_batch(
            "pragma busy_timeout = 30000; pragma foreign_keys = on; create table if not exists graph_ticket_draft_operations(operation_key text primary key, action_idempotency_key text not null unique, request_digest text not null, draft_request_digest text not null, ticket_id text not null, effect_claim_id text not null, mailbox_id text not null, source_message_id text not null, reply_mode text not null, status text not null, draft_id text, receipt_id text, draft_ref_json text, created_at text not null, updated_at text not null, completed_at text); create table if not exists graph_ticket_draft_disposition_scan_state(operation_key text primary key, last_scanned_at text not null, scan_count integer not null); create table if not exists graph_ticket_draft_disposition_observations(observation_id text primary key, operation_key text not null unique, ticket_id text not null, mailbox_id text not null, draft_id text not null, disposition text not null, evidence_kind text not null, evidence_id text not null unique, receipt_json text not null, observed_at text not null); create table if not exists graph_ticket_draft_disposition_receipts(observation_id text not null, consumer_id text not null, reconciliation_ref text not null, receipt_json text not null, acknowledged_at text not null, primary key(observation_id, consumer_id)); create table if not exists graph_ticket_draft_discard_intents(operation_key text primary key, idempotency_key text not null unique, request_digest text not null, status text not null, verified_etag text, verified_at text, receipt_json text, created_at text not null, updated_at text not null, completed_at text);",
        )
        .map_err(|error| unavailable("graph_ticket_draft_schema_failed", &error.to_string()))?;
    Ok(connection)
}

fn find_ticket_operation(connection: &Connection, operation_key: &str) -> Result<Option<TicketOperation>, Value> {
    connection
        .query_row(
            "select operation_key, action_idempotency_key, request_digest, draft_request_digest, ticket_id, effect_claim_id, mailbox_id, source_message_id, reply_mode, status, draft_id, receipt_id, draft_ref_json, completed_at from graph_ticket_draft_operations where operation_key=?1",
            params![operation_key],
            |row| {
                let draft_ref: Option<String> = row.get(12)?;
                Ok(TicketOperation {
                    operation_key: row.get(0)?,
                    action_idempotency_key: row.get(1)?,
                    request_digest: row.get(2)?,
                    draft_request_digest: row.get(3)?,
                    ticket_id: row.get(4)?,
                    effect_claim_id: row.get(5)?,
                    mailbox_id: row.get(6)?,
                    source_message_id: row.get(7)?,
                    reply_mode: row.get(8)?,
                    status: row.get(9)?,
                    draft_id: row.get(10)?,
                    receipt_id: row.get(11)?,
                    draft_ref: draft_ref.and_then(|value| serde_json::from_str(&value).ok()),
                    completed_at: row.get(13)?,
                })
            },
        )
        .optional()
        .map_err(|error| unavailable("graph_ticket_draft_database_read_failed", &error.to_string()))
}

fn assert_ticket_operation_matches(
    operation: &TicketOperation,
    operation_key: &str,
    request_digest: &str,
    draft_request_digest: &str,
    ticket_id: &str,
    effect_claim_id: &str,
    mailbox_id: &str,
    source_message_id: &str,
    reply_mode: &str,
    idempotency_key: &str,
) -> Result<(), Value> {
    if operation.operation_key != operation_key
        || operation.request_digest != request_digest
        || operation.action_idempotency_key != idempotency_key
        || operation.draft_request_digest != draft_request_digest
        || operation.ticket_id != ticket_id
        || operation.effect_claim_id != effect_claim_id
        || operation.mailbox_id != mailbox_id
        || operation.source_message_id != source_message_id
        || operation.reply_mode != reply_mode
    {
        return Err(unavailable(
            "graph_ticket_draft_idempotency_conflict",
            operation_key,
        ));
    }
    Ok(())
}

fn find_ticket_remote_draft(
    policy: &Policy,
    mailbox_id: &str,
    operation_key: &str,
) -> Result<Option<Value>, Value> {
    let property_id = TICKET_DRAFT_OPERATION_PROPERTY_ID.replace('\'', "''");
    let property_value = operation_key.replace('\'', "''");
    let mut query = Map::new();
    query.insert(
        "$filter".to_string(),
        json!(format!("isDraft eq true and singleValueExtendedProperties/Any(ep: ep/id eq '{property_id}' and ep/value eq '{property_value}')")),
    );
    query.insert(
        "$expand".to_string(),
        json!(format!("singleValueExtendedProperties($filter=id eq '{property_id}')")),
    );
    query.insert(
        "$select".to_string(),
        json!("id,conversationId,subject,isDraft,createdDateTime,lastModifiedDateTime"),
    );
    query.insert("$top".to_string(), json!(2));
    let result = policy
        .adapter
        .request("GET", Some(mailbox_id), "messages", &query, None)?;
    let drafts = result
        .get("value")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .filter(|value| value.get("id").and_then(Value::as_str).is_some() && value.get("isDraft").and_then(Value::as_bool) != Some(false))
        .collect::<Vec<_>>();
    if drafts.len() > 1 {
        return Err(unavailable(
            "graph_ticket_draft_remote_identity_ambiguous",
            operation_key,
        ));
    }
    Ok(drafts.into_iter().next())
}

fn ticket_draft_ref_value(normalized: &Map<String, Value>, draft: &Value, draft_id: &str) -> Value {
    let mut reference = Map::new();
    reference.insert("schema".to_string(), json!("narada.graph_mail.ticket_draft_ref.v1"));
    for key in [
        "ticket_id",
        "effect_claim_id",
        "draft_operation_key",
        "draft_request_digest",
        "mailbox_id",
        "source_message_id",
        "reply_mode",
    ] {
        if let Some(value) = normalized.get(key) {
            reference.insert(key.to_string(), value.clone());
        }
    }
    reference.insert("draft_id".to_string(), json!(draft_id));
    if let Some(value) = draft.get("conversationId").and_then(Value::as_str) {
        reference.insert("conversation_id".to_string(), json!(value));
    }
    if let Some(value) = draft.get("@odata.etag").and_then(Value::as_str) {
        reference.insert("etag".to_string(), json!(value));
    }
    Value::Object(reference)
}

fn stable_receipt_id(operation_key: &str, draft_id: &str) -> String {
    let mut input = operation_key.as_bytes().to_vec();
    input.push(0);
    input.extend_from_slice(draft_id.as_bytes());
    format!("graph_draft_receipt_{}", &hex_lower(&Sha256::digest(input))[..32])
}

fn ticket_domain_operation(operation: &TicketOperation, replayed_or_recovered: bool) -> Result<Value, Value> {
    let (Some(draft_id), Some(receipt_id), Some(draft_ref), Some(completed_at)) = (
        operation.draft_id.as_ref(),
        operation.receipt_id.as_ref(),
        operation.draft_ref.as_ref(),
        operation.completed_at.as_ref(),
    ) else {
        return Err(unavailable("graph_ticket_draft_operation_incomplete", &operation.operation_key));
    };
    Ok(json!({
        "schema":"narada.domain_operation.v1",
        "operation_ref":format!("graph-mail-ticket-draft:{}", operation.operation_key),
        "outcome":"completed",
        "result":{
            "schema":"narada.graph_mail.ticket_draft_receipt.v1",
            "ticket_id":operation.ticket_id,
            "effect_claim_id":operation.effect_claim_id,
            "draft_operation_key":operation.operation_key,
            "draft_request_digest":operation.draft_request_digest,
            "receipt_id":receipt_id,
            "draft_id":draft_id,
            "draft_ref":draft_ref,
            "idempotency_replayed_or_recovered":replayed_or_recovered,
            "completed_at":completed_at
        }
    }))
}

fn canonical_json(value: &Value) -> String {
    serde_json::to_string(&canonical_value(value)).unwrap_or_else(|_| "null".to_string())
}

fn canonical_value(value: &Value) -> Value {
    match value {
        Value::Array(values) => Value::Array(values.iter().map(canonical_value).collect()),
        Value::Object(object) => {
            let mut keys = object.keys().cloned().collect::<Vec<_>>();
            keys.sort();
            let mut output = Map::new();
            for key in keys {
                if let Some(value) = object.get(&key) {
                    output.insert(key, canonical_value(value));
                }
            }
            Value::Object(output)
        }
        other => other.clone(),
    }
}

fn sha256_canonical(value: &Value) -> String {
    hex_lower(&Sha256::digest(canonical_json(value).as_bytes()))
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

pub(crate) fn record_audit(root: &Path, event: Value) -> Result<(), Value> {
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
fn boundary(name: &str, reason: &str) -> Value { json!({"schema":"narada.graph_mail_mcp.authority_boundary.v1","status":"unavailable","tool_name":name,"reason":reason,"remediation":"Use a supported tool through the configured native Graph Mail authority."}) }
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
        assert!(supports("graph_mail_ticket_draft_upsert"));
    }

    #[test]
    fn bounded_base64_decoder_matches_attachment_bytes() {
        assert_eq!(decode_base64("SGVsbG8=").unwrap(), b"Hello");
        assert!(decode_base64("not-base64").is_err());
    }

    #[test]
    fn governed_html_reply_applies_escaped_signature_before_quote() {
        let html = compose_reply_html("<p>Done.</p>", "<p>Original</p>", Some("Ezra & Team"));
        assert_eq!(
            html,
            "<p>Done.</p><p>Thanks,<br>Ezra &amp; Team</p><div data-narada-quoted-history=\"true\"><p>Original</p></div>"
        );
    }
}
