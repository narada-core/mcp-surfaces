fn graph_mail_tool(name: &str) -> Value {
    let (description, properties, required) = graph_mail_tool_contract(name);
    let read_only = matches!(
        name,
        "graph_mail_doctor"
            | "graph_mail_auth_status"
            | "graph_mail_query"
            | "graph_mail_message_show"
            | "graph_mail_folder_list"
            | "graph_mail_attachment_list"
            | "graph_mail_attachment_get"
            | "graph_mail_ticket_draft_disposition_list"
            | "graph_mail_output_show"
    );
    let destructive = matches!(
        name,
        "graph_mail_auth_clear"
            | "graph_mail_attachment_delete"
            | "graph_mail_ticket_draft_discard"
            | "graph_mail_draft_discard"
            | "graph_mail_draft_send"
    );
    let idempotent = read_only
        || matches!(
            name,
            "graph_mail_auth_clear"
                | "graph_mail_message_mark_read"
                | "graph_mail_ticket_draft_upsert"
                | "graph_mail_ticket_draft_discard"
                | "graph_mail_ticket_draft_disposition_scan"
                | "graph_mail_ticket_draft_disposition_ack"
                | "graph_mail_draft_update"
                | "graph_mail_draft_discard"
        );
    let mut schema = json!({"type":"object","properties":properties,"additionalProperties":false});
    if !required.is_empty() {
        schema
            .as_object_mut()
            .expect("schema object")
            .insert("required".to_string(), json!(required));
    }
    json!({
        "name":name,
        "description":description,
        "inputSchema":schema,
        "annotations":{
            "title":name,
            "readOnlyHint":read_only,
            "destructiveHint":destructive,
            "idempotentHint":idempotent,
            "openWorldHint":true
        },
        "outputSchema":{"type":"object","additionalProperties":true}
    })
}

fn graph_mail_tool_contract(name: &str) -> (&'static str, Value, Vec<&'static str>) {
    let mailbox = || json!({"type":"string","minLength":1,"maxLength":320,"default":"me","description":"Allowed mailbox id or user principal."});
    let id = |description: &str| json!({"type":"string","minLength":1,"maxLength":4096,"description":description});
    let text =
        |description: &str| json!({"type":"string","maxLength":262144,"description":description});
    let token = |description: &str| json!({"type":"string","minLength":1,"maxLength":4096,"description":description});
    let limit = |default: u64, maximum: u64| json!({"type":"integer","minimum":1,"maximum":maximum,"default":default});
    let draft = || {
        json!({
            "mailbox_id":mailbox(),
            "subject":text("Draft subject."),
            "body_text":text("Plain-text draft body."),
            "body_html":text("HTML draft body."),
            "to_recipients":{"type":"array","maxItems":500,"items":{"type":"string","minLength":1,"maxLength":320}},
            "cc_recipients":{"type":"array","maxItems":500,"items":{"type":"string","minLength":1,"maxLength":320}},
            "bcc_recipients":{"type":"array","maxItems":500,"items":{"type":"string","minLength":1,"maxLength":320}},
            "importance":{"type":"string","enum":["low","normal","high"]}
        })
    };
    let reply = || {
        json!({
            "mailbox_id":mailbox(),
            "message_id":id("Original Graph message id."),
            "comment":text("Optional reply comment."),
            "comment_html":text("Governed HTML reply body."),
            "body_text":text("Optional replacement body text."),
            "body_html":text("Optional replacement body HTML.")
        })
    };
    match name {
        "graph_mail_doctor" => ("Inspect Microsoft Graph mail readiness and policy.", json!({}), vec![]),
        "graph_mail_auth_device_code_start" => ("Start an operator-approved Graph device-code flow when site policy permits it.", json!({"scope":{"type":"string","minLength":1,"maxLength":4096}}), vec![]),
        "graph_mail_auth_device_code_poll" => ("Poll a device-code flow and persist the delegated token after approval.", json!({"flow_id":id("Flow id returned by device-code start.")}), vec!["flow_id"]),
        "graph_mail_auth_status" => ("Inspect delegated Graph authentication metadata without exposing credentials.", json!({}), vec![]),
        "graph_mail_auth_clear" => ("Clear this site's delegated Graph authentication material.", json!({"confirm_clear":{"type":"boolean","const":true}}), vec![]),
        "graph_mail_query" => ("Query live Graph messages for an allowed mailbox.", json!({"mailbox_id":mailbox(),"folder_id":id("Optional mail folder id."),"query":text("Optional Graph search string."),"filter":text("Optional Graph filter expression."),"select":{"type":"string","maxLength":8192},"limit":limit(20,100)}), vec![]),
        "graph_mail_message_show" => ("Read one live Graph message.", json!({"mailbox_id":mailbox(),"message_id":id("Graph message id."),"select":{"type":"string","maxLength":8192}}), vec!["message_id"]),
        "graph_mail_folder_list" => ("List live Graph mail folders for an allowed mailbox.", json!({"mailbox_id":mailbox(),"parent_folder_id":id("Optional parent folder id."),"select":{"type":"string","maxLength":8192},"limit":limit(50,100)}), vec![]),
        "graph_mail_folder_create" => ("Create a mail folder when mailbox-organization policy permits it.", json!({"mailbox_id":mailbox(),"display_name":{"type":"string","minLength":1,"maxLength":256},"parent_folder_id":id("Optional parent folder id."),"confirm_write":{"type":"boolean","const":true},"approval_token":token("Optional configured approval token.")}), vec!["display_name"]),
        "graph_mail_message_move" => ("Move one message when mailbox-organization policy permits it.", json!({"mailbox_id":mailbox(),"message_id":id("Graph message id."),"destination_folder_id":id("Destination folder id or well-known name."),"confirm_write":{"type":"boolean","const":true},"approval_token":token("Optional configured approval token.")}), vec!["message_id","destination_folder_id"]),
        "graph_mail_message_mark_read" => ("Idempotently mark a message read after durable downstream admission.", json!({"mailbox_id":mailbox(),"message_id":id("Graph message id."),"confirm_write":{"type":"boolean","const":true},"approval_token":token("Optional configured mailbox-organization approval token."),"idempotency_key":id("Stable action occurrence key.")}), vec!["message_id","idempotency_key"]),
        "graph_mail_attachment_list" => ("List bounded attachment metadata for a message or draft.", json!({"mailbox_id":mailbox(),"message_id":id("Message id."),"draft_id":id("Draft id alias."),"limit":limit(20,100),"top":limit(20,100)}), vec![]),
        "graph_mail_attachment_get" => ("Read one attachment with content excluded unless explicitly requested and bounded.", json!({"mailbox_id":mailbox(),"message_id":id("Message id."),"draft_id":id("Draft id alias."),"attachment_id":id("Attachment id."),"include_content":{"type":"boolean","default":false}}), vec!["attachment_id"]),
        "graph_mail_attachment_download_file" => ("Download one permitted attachment beneath an allowed local root.", json!({"mailbox_id":mailbox(),"message_id":id("Message id."),"attachment_id":id("Attachment id."),"file_path":{"type":"string","minLength":1,"maxLength":4096}}), vec!["attachment_id","file_path"]),
        "graph_mail_attachment_add" => ("Add a bounded inline file attachment to a message or draft.", json!({"mailbox_id":mailbox(),"message_id":id("Message id."),"draft_id":id("Draft id alias."),"name":{"type":"string","minLength":1,"maxLength":255},"content_type":{"type":"string","minLength":1,"maxLength":255},"content_base64":{"type":"string","minLength":1,"maxLength":4194304},"is_inline":{"type":"boolean"},"content_id":id("Optional inline content id.")}), vec!["name","content_type","content_base64"]),
        "graph_mail_attachment_upload_session_create" => ("Create a Graph upload session for a large attachment.", json!({"mailbox_id":mailbox(),"message_id":id("Message id."),"draft_id":id("Draft id alias."),"name":{"type":"string","minLength":1,"maxLength":255},"size":{"type":"integer","minimum":1,"maximum":157286400},"content_type":{"type":"string","maxLength":255},"is_inline":{"type":"boolean"},"content_id":id("Optional inline content id.")}), vec!["name","size"]),
        "graph_mail_attachment_upload_chunk" => ("Upload one bounded chunk to a guarded Graph upload URL.", json!({"upload_url":{"type":"string","minLength":1,"maxLength":16384},"content_base64":{"type":"string","minLength":1,"maxLength":15000000},"range_start":{"type":"integer","minimum":0,"maximum":157286400},"range_end":{"type":"integer","minimum":0,"maximum":157286400},"total_size":{"type":"integer","minimum":1,"maximum":157286400}}), vec!["upload_url","content_base64","range_start","range_end","total_size"]),
        "graph_mail_attachment_upload_file" => ("Upload an allowed local file through a guarded Graph upload session.", json!({"mailbox_id":mailbox(),"message_id":id("Message id."),"draft_id":id("Draft id alias."),"file_path":{"type":"string","minLength":1,"maxLength":4096},"name":{"type":"string","maxLength":255},"content_type":{"type":"string","maxLength":255},"is_inline":{"type":"boolean"},"content_id":id("Optional inline content id."),"chunk_size":{"type":"integer","minimum":327680,"maximum":10485760,"default":3276800}}), vec!["file_path"]),
        "graph_mail_attachment_delete" => ("Delete one attachment from a message or draft.", json!({"mailbox_id":mailbox(),"message_id":id("Message id."),"draft_id":id("Draft id alias."),"attachment_id":id("Attachment id.")}), vec!["attachment_id"]),
        "graph_mail_draft_create" => ("Create a new unsent draft in an allowed mailbox.", draft(), vec![]),
        "graph_mail_reply_draft_create" | "graph_mail_reply_all_draft_create" => ("Create an unsent reply draft for an existing message.", reply(), vec!["message_id"]),
        "graph_mail_forward_draft_create" => { let mut p=reply(); p.as_object_mut().expect("properties").insert("to_recipients".to_string(),json!({"type":"array","maxItems":500,"items":{"type":"string","minLength":1,"maxLength":320}})); ("Create an unsent forward draft for an existing message.",p,vec!["message_id"]) },
        "graph_mail_reply_all_to_last_in_thread_draft_create" => { let mut p=reply(); let o=p.as_object_mut().expect("properties"); o.remove("message_id"); o.insert("conversation_id".to_string(),id("Conversation id.")); ("Create a reply-all draft to the latest message in a bounded thread lookup.",p,vec!["conversation_id"]) },
        "graph_mail_ticket_draft_upsert" => ("Idempotently create or recover the exact Work-authorized unsent reply draft.", json!({"ticket_id":id("Work ticket id."),"effect_claim_id":id("Revision-bound effect claim id."),"draft_operation_key":id("Stable draft operation key."),"draft_request_digest":{"type":"string","pattern":"^[a-f0-9]{64}$","maxLength":64},"draft_source_id":id("Admitted mailbox source id."),"mailbox_id":mailbox(),"source_message_id":id("Immutable source message id."),"reply_mode":{"type":"string","enum":["reply","reply_all"]},"body_text":text("Plain-text unsent body."),"body_html":text("HTML unsent body."),"idempotency_key":id("Stable action occurrence key.")}), vec!["ticket_id","effect_claim_id","draft_operation_key","draft_request_digest","draft_source_id","mailbox_id","source_message_id","reply_mode","idempotency_key"]),
        "graph_mail_ticket_draft_discard" => ("Idempotently discard the exact tracked unsent draft and emit a durable disposition receipt.", json!({"ticket_id":id("Work ticket id."),"effect_claim_id":id("Effect claim id."),"draft_operation_key":id("Draft operation key."),"mailbox_id":mailbox(),"draft_id":id("Tracked draft id."),"idempotency_key":id("Stable discard occurrence key."),"confirm_discard":{"type":"boolean","const":true}}), vec!["ticket_id","effect_claim_id","draft_operation_key","mailbox_id","draft_id","idempotency_key","confirm_discard"]),
        "graph_mail_ticket_draft_disposition_scan" => ("Observe a bounded set of tracked drafts and durably record proved sent dispositions.", json!({"limit":limit(5,5)}), vec![]),
        "graph_mail_ticket_draft_disposition_list" => ("List unacknowledged durable draft disposition receipts for one consumer.", json!({"consumer_id":id("Stable reconciliation consumer id."),"limit":limit(5,5)}), vec!["consumer_id"]),
        "graph_mail_ticket_draft_disposition_ack" => ("Acknowledge a disposition only after durable Work reconciliation.", json!({"observation_id":id("Disposition observation id."),"consumer_id":id("Reconciliation consumer id."),"reconciliation_ref":id("Durable reconciliation reference."),"reconciliation_receipt":{"type":"object","maxProperties":64,"additionalProperties":true}}), vec!["observation_id","consumer_id","reconciliation_ref","reconciliation_receipt"]),
        "graph_mail_draft_update" => { let mut p=draft(); let o=p.as_object_mut().expect("properties"); o.insert("draft_id".to_string(),id("Draft id.")); o.insert("allow_replace_full_body".to_string(),json!({"type":"boolean","default":false})); o.insert("allow_replace_quoted_body".to_string(),json!({"type":"boolean","default":false})); ("Update an existing unsent draft.",p,vec!["draft_id"]) },
        "graph_mail_draft_discard" => ("Delete an existing unsent draft unless it is Work-linked.", json!({"mailbox_id":mailbox(),"draft_id":id("Draft id.")}), vec!["draft_id"]),
        "graph_mail_draft_send" => ("Send an existing draft only when explicitly allowed and confirmed.", json!({"mailbox_id":mailbox(),"draft_id":id("Draft id."),"confirm_send":{"type":"boolean","const":true},"approval_token":token("Optional configured send approval token.")}), vec!["draft_id"]),
        "graph_mail_output_show" => ("Read a materialized Graph Mail output with bounded paging.", json!({"ref":{"type":"string","minLength":1,"maxLength":4096},"output_ref":{"type":"string","minLength":1,"maxLength":4096},"offset":{"type":"integer","minimum":0,"maximum":1073741824,"default":0},"limit":{"type":"integer","minimum":0,"maximum":20000,"default":10000}}), vec![]),
        _ => ("Unknown Graph Mail operation.", json!({}), vec![]),
    }
}
fn operator_status(root: &Path) -> Value {
    let state_root = operator_state_root();
    operator_status_at(root, &state_root)
}

fn operator_status_at(root: &Path, state_root: &Path) -> Value {
    let state_directory = state_root.join("operator-console");
    let pid_path = state_directory.join("overlay.pid");
    let pid = fs::read_to_string(&pid_path)
        .ok()
        .and_then(|value| value.trim().parse::<u32>().ok())
        .filter(|value| *value > 0);
    let running = pid.map(pid_is_running).unwrap_or(false);
    let overlay = json!({
        "schema":"narada.window_surface_overlay.result.v1",
        "id":"operator-console",
        "state":if running { "running" } else { "stopped" },
        "pid":if running { pid.map(|value| json!(value)).unwrap_or(Value::Null) } else { Value::Null },
        "state_directory":state_directory.to_string_lossy(),
        "document_path":state_directory.join("document.json").to_string_lossy(),
        "document":operator_json(&state_directory.join("document.json")),
        "action_state":operator_json(&state_directory.join("action-state.json")),
        "visibility_state":operator_json(&state_directory.join("visibility.state.json")),
        "surface_snapshot":operator_json(&state_root.join("surface.snapshot.json")),
        "focus_owner":operator_json(&state_root.join("focus.owner.json")),
    });
    json!({
        "schema":"narada.operator_console_overlay.mcp_result.v1",
        "status":"ok",
        "operation":"status",
        "command":"inspect",
        "overlay_id":"operator-console",
        "narada_root":root.to_string_lossy(),
        "overlay":overlay,
    })
}

fn operator_state_root() -> PathBuf {
    if let Ok(value) = env::var("NARADA_WINDOW_SURFACE_OVERLAY_STATE_ROOT") {
        if !value.trim().is_empty() {
            return PathBuf::from(value);
        }
    }
    let local_app_data = env::var("LOCALAPPDATA")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            env::var("USERPROFILE")
                .ok()
                .filter(|value| !value.trim().is_empty())
                .map(|value| PathBuf::from(value).join("AppData/Local"))
        })
        .or_else(|| {
            env::var("HOME")
                .ok()
                .filter(|value| !value.trim().is_empty())
                .map(|value| PathBuf::from(value).join("AppData/Local"))
        })
        .unwrap_or_else(|| PathBuf::from("AppData/Local"));
    local_app_data.join("Narada/window-surface-overlays")
}

fn operator_json(path: &Path) -> Value {
    let value = read_json_file(path);
    if value.as_object().is_some_and(Map::is_empty) {
        Value::Null
    } else {
        value
    }
}

fn pid_is_running(pid: u32) -> bool {
    #[cfg(windows)]
    {
        let filter = format!("PID eq {pid}");
        let needle = format!("\"{pid}\"");
        return Command::new("tasklist")
            .args(["/FI", filter.as_str(), "/FO", "CSV", "/NH"])
            .output()
            .ok()
            .filter(|output| output.status.success())
            .and_then(|output| String::from_utf8(output.stdout).ok())
            .map(|output| output.contains(&needle))
            .unwrap_or(false);
    }
    #[cfg(unix)]
    {
        return Command::new("kill")
            .args(["-0", &pid.to_string()])
            .status()
            .map(|status| status.success())
            .unwrap_or(false);
    }
    #[cfg(not(any(windows, unix)))]
    {
        let _ = pid;
        false
    }
}
fn graph_mail_doctor(root: &Path) -> Value {
    let path = root.join(".ai/graph-mail-mcp.json");
    let policy = read_json_file(&path);
    let object = policy.as_object().cloned().unwrap_or_default();
    let graph_base_url = object
        .get("graph_base_url")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .map(|value| value.trim_end_matches('/').to_string())
        .unwrap_or_else(|| DEFAULT_GRAPH_BASE_URL.to_string());
    let allowed_mailboxes = graph_string_array(&object, "allowed_mailboxes", "allowedMailboxes");
    let allowed_attachment_roots = {
        let values = graph_string_array(
            &object,
            "allowed_attachment_roots",
            "allowedAttachmentRoots",
        );
        if values.is_empty() {
            vec![Value::String(root.to_string_lossy().to_string())]
        } else {
            values
                .into_iter()
                .filter_map(|value| {
                    value
                        .as_str()
                        .map(|path| Value::String(resolve_graph_path(root, path)))
                })
                .collect()
        }
    };
    let allow_device_code_auth =
        graph_bool(&object, "allow_device_code_auth", "allowDeviceCodeAuth");
    let device_code_tenant = graph_string(&object, "device_code_tenant_id", "deviceCodeTenantId");
    let device_code_client = graph_string(&object, "device_code_client_id", "deviceCodeClientId");
    let device_code_allowed_scopes = graph_string_array(
        &object,
        "device_code_allowed_scopes",
        "deviceCodeAllowedScopes",
    );
    let (has_access_token, auth_mode) =
        graph_auth_posture(root, allow_device_code_auth, &device_code_allowed_scopes);
    let delegated_token = graph_delegated_token_summary(root);
    let reply_signature_name = graph_string(&object, "reply_signature_name", "replySignatureName");
    json!({"schema":"narada.graph_mail_mcp.doctor.v1","status":"ok","site_root":root.to_string_lossy(),"graph_base_url":graph_base_url,"has_access_token":has_access_token,"auth_mode":auth_mode,"allowed_mailboxes":allowed_mailboxes,"allowed_attachment_roots":allowed_attachment_roots,"allow_device_code_auth":allow_device_code_auth,"device_code_tenant_configured":device_code_tenant.is_some() || graph_non_empty_env(root, "GRAPH_TENANT_ID"),"device_code_client_configured":device_code_client.is_some() || graph_non_empty_env(root, "GRAPH_CLIENT_ID"),"device_code_allowed_scopes":device_code_allowed_scopes,"delegated_token":delegated_token,"allow_send_draft":graph_bool(&object, "allow_send_draft", "allowSendDraft"),"send_approval_token_configured":graph_token_configured(&object, "send_approval_token", "sendApprovalToken"),"allow_folder_create":graph_bool(&object, "allow_folder_create", "allowFolderCreate"),"allow_message_move":graph_bool(&object, "allow_message_move", "allowMessageMove"),"allow_message_mark_read":graph_bool(&object, "allow_message_mark_read", "allowMessageMarkRead"),"mailbox_organization_approval_token_configured":graph_token_configured(&object, "mailbox_organization_approval_token", "mailboxOrganizationApprovalToken"),"reply_signature_name":reply_signature_name,"server_name":"narada-graph-mail-mcp"})
}
fn graph_mail_auth_status(root: &Path) -> Value {
    let object = read_json_file(&root.join(".ai/graph-mail-mcp.json"))
        .as_object()
        .cloned()
        .unwrap_or_default();
    let scopes = graph_string_array(
        &object,
        "device_code_allowed_scopes",
        "deviceCodeAllowedScopes",
    );
    json!({"schema":"narada.graph_mail_mcp.auth_status.v1","status":"ok","allow_device_code_auth":graph_bool(&object, "allow_device_code_auth", "allowDeviceCodeAuth"),"device_code_tenant_configured":graph_string(&object, "device_code_tenant_id", "deviceCodeTenantId").is_some() || graph_non_empty_env(root, "GRAPH_TENANT_ID"),"device_code_client_configured":graph_string(&object, "device_code_client_id", "deviceCodeClientId").is_some() || graph_non_empty_env(root, "GRAPH_CLIENT_ID"),"device_code_allowed_scopes":scopes,"delegated_token":graph_delegated_token_summary(root)})
}

