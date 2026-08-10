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
            | "graph_mail_folder_list"
            | "graph_mail_folder_create"
            | "graph_mail_message_move"
            | "graph_mail_message_mark_read"
            | "graph_mail_attachment_list"
            | "graph_mail_attachment_get"
            | "graph_mail_attachment_add"
            | "graph_mail_attachment_delete"
            | "graph_mail_draft_create"
            | "graph_mail_reply_draft_create"
            | "graph_mail_reply_all_draft_create"
            | "graph_mail_forward_draft_create"
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
        "graph_mail_folder_list" => folder_list(&policy, args),
        "graph_mail_folder_create" => folder_create(&policy, args, root),
        "graph_mail_message_move" => message_move(&policy, args, root),
        "graph_mail_message_mark_read" => mark_read(&policy, args, root),
        "graph_mail_attachment_list" => attachment_list(&policy, args),
        "graph_mail_attachment_get" => attachment_get(&policy, args),
        "graph_mail_attachment_add" => attachment_add(&policy, args),
        "graph_mail_attachment_delete" => attachment_delete(&policy, args),
        "graph_mail_draft_create" => draft_create(&policy, args, root),
        "graph_mail_reply_draft_create" => derived_draft_create(&policy, args, root, "createReply"),
        "graph_mail_reply_all_draft_create" => derived_draft_create(&policy, args, root, "createReplyAll"),
        "graph_mail_forward_draft_create" => derived_draft_create(&policy, args, root, "createForward"),
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
        return Err(boundary(
            &format!("graph_mail_{action}_draft_create"),
            "graph_mail_html_reply_requires_quote_preservation_port",
        ));
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
    fn operation_support_is_limited_to_ported_provider_slice() {
        assert!(supports("graph_mail_query"));
        assert!(supports("graph_mail_message_mark_read"));
        assert!(supports("graph_mail_draft_send"));
        assert!(!supports("graph_mail_attachment_upload_file"));
        assert!(!supports("graph_mail_ticket_draft_upsert"));
    }
}
