use serde_json::{json, Map, Value};
use std::collections::HashMap;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

const MAX_BYTES: u64 = 256_000;
const MAX_OUTPUT_BYTES: u64 = 10 * 1024 * 1024;
const DEFAULT_OUTPUT_LIMIT: u64 = 10_000;
const MAX_OUTPUT_LIMIT: u64 = 20_000;
const DEFAULT_GRAPH_BASE_URL: &str = "https://graph.microsoft.com/v1.0";
const OPERATOR: &[(&str, bool)] = &[
    ("operator_console_overlay_status", true),
    ("operator_console_overlay_open", false),
    ("operator_console_overlay_refresh", false),
    ("operator_console_overlay_close", false),
];
const SCHEDULER: &[(&str, bool)] = &[
    ("scheduler_runtime_status", true),
    ("scheduler_task_list", true),
    ("scheduler_task_show", true),
    ("scheduler_task_create", false),
    ("scheduler_task_delete", false),
    ("scheduler_task_update_action", false),
    ("scheduler_task_enable", false),
    ("scheduler_task_disable", false),
    ("scheduler_task_stop", false),
    ("scheduler_task_run", false),
    ("scheduler_task_history", true),
    ("scheduler_activation_doctor", true),
    ("scheduler_activation_prepare", false),
    ("scheduler_binding_list", true),
    ("scheduler_binding_show", true),
    ("scheduler_binding_upsert", false),
    ("scheduler_event_show", true),
    ("scheduler_event_admit", false),
    ("scheduler_activation_list", true),
    ("scheduler_activation_claim", false),
    ("scheduler_activation_admit_sop", false),
    ("scheduler_activation_fail", false),
    ("scheduler_activation_resolve", false),
    ("scheduler_activation_unblock", false),
];
const GRAPH_MAIL: &[(&str, bool)] = &[
    ("graph_mail_doctor", true),
    ("graph_mail_auth_device_code_start", false),
    ("graph_mail_auth_device_code_poll", false),
    ("graph_mail_auth_status", true),
    ("graph_mail_auth_clear", false),
    ("graph_mail_query", true),
    ("graph_mail_message_show", true),
    ("graph_mail_folder_list", true),
    ("graph_mail_folder_create", false),
    ("graph_mail_message_move", false),
    ("graph_mail_message_mark_read", false),
    ("graph_mail_attachment_list", true),
    ("graph_mail_attachment_get", true),
    ("graph_mail_attachment_download_file", false),
    ("graph_mail_attachment_add", false),
    ("graph_mail_attachment_upload_session_create", false),
    ("graph_mail_attachment_upload_chunk", false),
    ("graph_mail_attachment_upload_file", false),
    ("graph_mail_attachment_delete", false),
    ("graph_mail_draft_create", false),
    ("graph_mail_reply_draft_create", false),
    ("graph_mail_reply_all_draft_create", false),
    ("graph_mail_forward_draft_create", false),
    ("graph_mail_reply_all_to_last_in_thread_draft_create", false),
    ("graph_mail_ticket_draft_upsert", false),
    ("graph_mail_ticket_draft_discard", false),
    ("graph_mail_ticket_draft_disposition_scan", false),
    ("graph_mail_ticket_draft_disposition_list", true),
    ("graph_mail_ticket_draft_disposition_ack", false),
    ("graph_mail_draft_update", false),
    ("graph_mail_draft_discard", false),
    ("graph_mail_draft_send", false),
    ("graph_mail_output_show", true),
];

pub fn list_tools(surface_id: &str) -> Vec<Value> {
    if surface_id == "operator-console-overlay" {
        return crate::operator_console::list_tools();
    }
    let mut tools = vec![guidance(surface_id)];
    for (name, read_only) in entries(surface_id) {
        tools.push(if surface_id == "graph-mail" {
            graph_mail_tool(name)
        } else {
            tool(name, description(surface_id, name), *read_only)
        });
    }
    tools
}

pub fn auxiliary(
    surface_id: &str,
    method: &str,
    params: &Map<String, Value>,
) -> Result<Value, Value> {
    match method {
        "prompts/list" => Ok(
            json!({"prompts":[{"name":format!("{}_workflow", surface_id.replace('-', "_")),"title":format!("{} Workflow", surface_id),"description":"Inspect native status and authority posture before host/provider operations.","arguments":[]}]}),
        ),
        "prompts/get" => {
            let expected = format!("{}_workflow", surface_id.replace('-', "_"));
            if params.get("name").and_then(Value::as_str) != Some(expected.as_str()) {
                return Err(error("unknown_prompt", "unknown_prompt"));
            }
            Ok(
                json!({"description":"Inspect native status and authority posture before host/provider operations.","messages":[{"role":"user","content":{"type":"text","text":"Use the read-only status/doctor tool first. Native contract mode never transmits credentials, drives an external browser/provider, or mutates the host scheduler/process."}}]}),
            )
        }
        "completion/complete" => {
            let values = if params
                .get("argument")
                .and_then(Value::as_object)
                .and_then(|v| v.get("name"))
                .and_then(Value::as_str)
                == Some("name")
            {
                list_tools(surface_id)
                    .iter()
                    .filter_map(|v| v.get("name").cloned())
                    .take(100)
                    .collect::<Vec<_>>()
            } else {
                Vec::new()
            };
            Ok(json!({"completion":{"values":values,"total":values.len(),"hasMore":false}}))
        }
        "logging/setLevel" => Ok(json!({})),
        _ => Err(error(
            "unsupported_mcp_method",
            &format!("unsupported_mcp_method:{method}"),
        )),
    }
}

pub fn call_tool(
    surface_id: &str,
    name: &str,
    args: &Map<String, Value>,
    root: &Path,
) -> Result<Value, Value> {
    if surface_id == "operator-console-overlay" {
        return crate::operator_console::call(name, args, root);
    }
    if name.ends_with("_guidance") {
        return Ok(if surface_id == "graph-mail" {
            graph_mail_guidance(args)
        } else {
            guidance_result(surface_id, args)
        });
    }
    match (surface_id, name) {
        ("operator-console-overlay", "operator_console_overlay_status") => {
            Ok(operator_status(root))
        }
        ("scheduler", "scheduler_runtime_status") => Ok(
            json!({"schema":"narada.scheduler.runtime_status.v1","status":"authority_boundary","implementation":"rust-native-contract","native_task_scheduler":false,"native_read_only":true}),
        ),
        ("graph-mail", "graph_mail_doctor") => Ok(graph_mail_doctor(root)),
        ("graph-mail", "graph_mail_auth_status") => Ok(graph_mail_auth_status(root)),
        ("graph-mail", "graph_mail_output_show") => output_show(args, root),
        _ => Err(boundary(
            surface_id,
            name,
            "external_or_host_authority_not_enabled_in_native_contract",
            "Use the configured owning surface authority for this operation.",
        )),
    }
}

fn entries(surface_id: &str) -> &'static [(&'static str, bool)] {
    match surface_id {
        "operator-console-overlay" => OPERATOR,
        "scheduler" => SCHEDULER,
        "graph-mail" => GRAPH_MAIL,
        _ => &[],
    }
}
fn guidance(surface_id: &str) -> Value {
    let name = format!("{}_guidance", surface_id.replace('-', "_"));
    json!({"name":name,"description":format!("Show model-facing operating guidance for {surface_id} MCP workflows."),"inputSchema":{"type":"object","properties":{"workflow":{"type":"string","maxLength":256},"tool":{"type":"string","maxLength":256}},"additionalProperties":false},"annotations":{"title":name,"readOnlyHint":true,"destructiveHint":false,"idempotentHint":true,"openWorldHint":false},"outputSchema":{"type":"object","additionalProperties":true}})
}
fn guidance_result(surface_id: &str, args: &Map<String, Value>) -> Value {
    json!({"schema":"narada.host_surface.guidance.v1","status":"ok","surface_id":surface_id,"requested":args,"native_contract":"status probes and explicit authority boundaries","native_read_only":true})
}
fn graph_mail_guidance(args: &Map<String, Value>) -> Value {
    let requested = |key: &str| {
        args.get(key)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(|value| Value::String(value.to_string()))
            .unwrap_or(Value::Null)
    };
    json!({
        "schema":"narada.mcp_surface.guidance.v0",
        "status":"ok",
        "surface_id":"graph-mail",
        "guidance_tool":"graph_mail_guidance",
        "purpose":"Policy-gated Microsoft Graph mail live reads and draft lifecycle.",
        "requested":{"workflow":requested("workflow"),"tool":requested("tool")},
        "first_use":[
            "Call this guidance command when the surface is unfamiliar, when a refusal/error is unclear, or before composing a multi-step workflow.",
            "Inspect policy/doctor/status tools before mutation or open-world operations.",
            "Use bounded list/search/query tools for discovery, then show/read/detail tools before acting on a specific object.",
            "Preserve structuredContent as authoritative evidence; text content is for assistant readability."
        ],
        "tool_preference":[
            {"step":"orient","guidance":"Use *_guidance first when uncertain, then policy/doctor/status tools."},
            {"step":"discover","guidance":"Use bounded list/search/query commands with explicit limits and filters."},
            {"step":"inspect","guidance":"Use show/read/detail commands for exact targets before mutation."},
            {"step":"mutate","guidance":"Only call mutation tools after policy allows it and intent, target, and expected result are explicit."},
            {"step":"ticket_draft_discard","guidance":"Use graph_mail_ticket_draft_discard for Work-linked drafts so Graph deletion and Work Lifecycle terminalization are connected by a durable disposition receipt."},
            {"step":"verify","guidance":"Read back state with the owning surface after any mutation."}
        ],
        "examples":[
            {"intent":"First use","call":"graph_mail_guidance({})"},
            {"intent":"Tool-specific help","call":"graph_mail_guidance({ tool: \"<tool_name>\" })"},
            {"intent":"Workflow-specific help","call":"graph_mail_guidance({ workflow: \"<workflow_name>\" })"}
        ],
        "anti_patterns":[
            "Do not guess hidden state from a tool name; use doctor/status/list/show tools for evidence.",
            "Do not treat assistant text as the durable record when structuredContent is present.",
            "Do not bypass the owning surface with shell scripts when a governed MCP tool exists.",
            "Do not use graph_mail_draft_discard for a Work-linked ticket draft; the generic path refuses tracked drafts because deletion without a disposition receipt would strand the ticket.",
            "Do not continue after malformed payloads, empty refs, or ambiguous target identifiers; stop and repair the input."
        ],
        "recovery":[
            "For unknown_tool, call tools/list and this guidance command again after restart.",
            "For policy refusal, inspect the surface policy/doctor output and report the exact refusal reason.",
            "For oversized inputs, use the surface payload_ref or output_ref convention when it exists; otherwise reduce scope.",
            "For unclear behavior, submit surface_feedback_submit with surface_id, kind, summary, reproduction steps, expected behavior, and impact."
        ],
        "feedback":{"surface_id":"graph-mail","tool":"surface_feedback_submit","when":["guidance is missing, stale, or contradicted by live behavior","schema shape makes correct usage hard","errors hide the actionable refusal or recovery path"]},
        "boundaries":[
            "Guidance is read-only model-facing operating advice.",
            "Guidance does not weaken policy, authorize mutation, or replace tool schemas.",
            "The owning MCP surface remains authoritative for state and enforcement."
        ]
    })
}
fn description(surface_id: &str, name: &str) -> String {
    format!("Native {surface_id} contract for {name}; external authority remains explicit.")
}

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

pub(crate) fn output_show(args: &Map<String, Value>, root: &Path) -> Result<Value, Value> {
    let ref_value = args.get("ref").and_then(Value::as_str).map(str::trim);
    let output_ref_value = args
        .get("output_ref")
        .and_then(Value::as_str)
        .map(str::trim);
    if let (Some(reference), Some(output_ref)) = (ref_value, output_ref_value) {
        if reference != output_ref {
            return Err(error(
                "output_show_ref_alias_conflict",
                "output_show_ref_alias_conflict",
            ));
        }
    }
    let reference = ref_value
        .or(output_ref_value)
        .ok_or_else(|| error("output_show_requires_ref", "output_show_requires_ref"))?;
    let id = reference
        .strip_prefix("mcp_output:")
        .ok_or_else(|| error("output_ref_invalid", "output_ref_invalid"))?;
    if id.len() < 3
        || id.len() > 64
        || !id
            .chars()
            .all(|value| value.is_ascii_alphanumeric() || matches!(value, '_' | '-'))
    {
        return Err(error("output_ref_invalid", "output_ref_invalid"));
    }
    let path = root
        .join(".ai/tmp/mcp-outputs/workspace")
        .join(format!("{id}.json"));
    let metadata =
        fs::metadata(&path).map_err(|_| error("output_ref_not_found", "output_ref_not_found"))?;
    if !metadata.is_file() {
        return Err(error("output_ref_not_file", "output_ref_not_file"));
    }
    if metadata.len() > MAX_OUTPUT_BYTES {
        return Err(error("output_ref_too_large", "output_ref_too_large"));
    }
    let text = fs::read_to_string(&path)
        .map_err(|_| error("output_ref_not_found", "output_ref_not_found"))?;
    let record: Value = serde_json::from_str(&text)
        .map_err(|parse_error| error("output_ref_invalid_json", &parse_error.to_string()))?;
    if record.get("schema").and_then(Value::as_str) != Some("narada.mcp_output_ref.v1") {
        return Err(error(
            "output_ref_schema_unsupported",
            "output_ref_schema_unsupported",
        ));
    }
    if record.get("ref").and_then(Value::as_str) != Some(reference)
        || record.get("output_id").and_then(Value::as_str) != Some(id)
    {
        return Err(error(
            "output_ref_metadata_mismatch",
            "output_ref_metadata_mismatch",
        ));
    }
    let full_output = record.get("full_output").cloned().unwrap_or(Value::Null);
    let presentation =
        serde_json::to_string_pretty(&full_output).unwrap_or_else(|_| full_output.to_string());
    let offset = match args.get("offset") {
        None => 0,
        Some(value) => value.as_u64().ok_or_else(|| {
            error(
                "offset_must_be_non_negative_integer",
                "offset_must_be_non_negative_integer",
            )
        })?,
    };
    let limit = match args.get("limit").or_else(|| args.get("output_limit")) {
        None => DEFAULT_OUTPUT_LIMIT,
        Some(value) => {
            let value = value.as_u64().ok_or_else(|| {
                error(
                    "output_limit_must_be_positive_integer",
                    "output_limit_must_be_positive_integer",
                )
            })?;
            if value == 0 {
                return Err(error(
                    "output_limit_must_be_positive_integer",
                    "output_limit_must_be_positive_integer",
                ));
            }
            if value > MAX_OUTPUT_LIMIT {
                return Err(error(
                    "output_limit_exceeds_transport_maximum",
                    "output_limit_exceeds_transport_maximum",
                ));
            }
            value
        }
    };
    let chars = presentation.chars().collect::<Vec<_>>();
    let start = (offset as usize).min(chars.len());
    let chunk = chars
        .iter()
        .skip(start)
        .take(limit as usize)
        .collect::<String>();
    let end = start + chunk.chars().count();
    Ok(json!({
        "schema":"narada.mcp_output_page.v1",
        "status":"ok",
        "ref":reference,
        "tool_name":record.get("tool_name").cloned().unwrap_or(Value::Null),
        "full_output_char_length":record.get("full_output_char_length").cloned().unwrap_or_else(|| json!(chars.len())),
        "byte_size":metadata.len(),
        "original_truncated":record.get("truncated").and_then(Value::as_bool).unwrap_or(false),
        "path":format!(".ai/tmp/mcp-outputs/workspace/{id}.json"),
        "offset":start,
        "limit":limit,
        "next_offset":if end < chars.len() { json!(end) } else { Value::Null },
        "output_limit":limit,
        "output_truncated":end < chars.len(),
        "output_text":chunk
    }))
}

fn graph_string(object: &Map<String, Value>, snake: &str, camel: &str) -> Option<String> {
    object
        .get(snake)
        .or_else(|| object.get(camel))
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .map(ToOwned::to_owned)
}
fn graph_string_array(object: &Map<String, Value>, snake: &str, camel: &str) -> Vec<Value> {
    object
        .get(snake)
        .or_else(|| object.get(camel))
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|value| {
                    value
                        .as_str()
                        .filter(|item| !item.trim().is_empty())
                        .map(|item| Value::String(item.to_string()))
                })
                .collect()
        })
        .unwrap_or_default()
}
fn graph_bool(object: &Map<String, Value>, snake: &str, camel: &str) -> bool {
    object
        .get(snake)
        .or_else(|| object.get(camel))
        .and_then(Value::as_bool)
        .unwrap_or(false)
}
fn graph_token_configured(object: &Map<String, Value>, snake: &str, camel: &str) -> bool {
    graph_string(object, snake, camel).is_some()
}
fn resolve_graph_path(root: &Path, value: &str) -> String {
    let path = PathBuf::from(value);
    if path.is_absolute() {
        path.to_string_lossy().to_string()
    } else {
        root.join(path).to_string_lossy().to_string()
    }
}
fn graph_auth_posture(
    root: &Path,
    allow_device_code: bool,
    scopes: &[Value],
) -> (bool, &'static str) {
    let delegated = graph_delegated_token_summary(root);
    let delegated_allowed = allow_device_code
        && delegated
            .get("status")
            .and_then(Value::as_str)
            .map(|status| status == "available" || status == "refreshable")
            .unwrap_or(false)
        && delegated
            .get("scope")
            .and_then(Value::as_str)
            .map(|scope| scopes.iter().any(|value| value.as_str() == Some(scope)))
            .unwrap_or(false);
    if delegated_allowed {
        return (true, "delegated_device_code");
    }
    let graph_access = graph_non_empty_env(root, "GRAPH_ACCESS_TOKEN");
    let client_credentials = graph_non_empty_env(root, "GRAPH_TENANT_ID")
        && graph_non_empty_env(root, "GRAPH_CLIENT_ID")
        && graph_non_empty_env(root, "GRAPH_CLIENT_SECRET");
    let ms_access = graph_non_empty_env(root, "MS_GRAPH_ACCESS_TOKEN");
    if graph_access || (!client_credentials && ms_access) {
        (true, "access_token")
    } else if client_credentials {
        (true, "client_credentials")
    } else {
        (false, "missing")
    }
}
fn graph_delegated_token_summary(root: &Path) -> Value {
    let value = read_json_file(&root.join(".ai/runtime/graph-mail-mcp/delegated-token.json"));
    let Some(object) = value.as_object() else {
        return json!({"status":"missing","fresh":false});
    };
    if object.get("schema").and_then(Value::as_str)
        != Some("narada.graph_mail_mcp.delegated_token.v1")
    {
        return json!({"status":"missing","fresh":false});
    }
    let expires = object
        .get("expires_at_ms")
        .and_then(Value::as_i64)
        .unwrap_or(0);
    let fresh = expires > chrono_now_ms() + 60_000;
    let refreshable = object
        .get("refresh_token")
        .and_then(Value::as_str)
        .map(|value| !value.trim().is_empty())
        .unwrap_or(false);
    json!({"status":if fresh{"available"}else if refreshable{"refreshable"}else{"expired"},"fresh":fresh,"refreshable":refreshable,"auth_mode":object.get("auth_mode").cloned().unwrap_or(Value::Null),"tenant_id":object.get("tenant_id").cloned().unwrap_or(Value::Null),"client_id":object.get("client_id").cloned().unwrap_or(Value::Null),"scope":object.get("scope").cloned().unwrap_or(Value::Null),"acquired_at":object.get("acquired_at").cloned().unwrap_or(Value::Null),"expires_at_ms":object.get("expires_at_ms").cloned().unwrap_or(Value::Null)})
}
fn chrono_now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or(0)
}
fn graph_non_empty_env(root: &Path, key: &str) -> bool {
    let mut values = HashMap::new();
    for path in [
        root.parent().map(|parent| parent.join(".env")),
        Some(root.join(".env")),
    ]
    .into_iter()
    .flatten()
    {
        if let Ok(text) = fs::read_to_string(path) {
            for line in text.lines() {
                if let Some((name, value)) = line.split_once('=') {
                    values.insert(
                        name.trim().to_string(),
                        value.trim().trim_matches(['\'', '"']).to_string(),
                    );
                }
            }
        }
    }
    if let Ok(value) = env::var(key) {
        values.insert(key.to_string(), value);
    }
    values
        .get(key)
        .map(|value| !value.trim().is_empty())
        .unwrap_or(false)
}
fn read_json_file(path: &Path) -> Value {
    let Ok(meta) = fs::metadata(path) else {
        return Value::Object(Map::new());
    };
    if !meta.is_file() || meta.len() > MAX_BYTES {
        return Value::Object(Map::new());
    }
    fs::read_to_string(path)
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_else(|| Value::Object(Map::new()))
}
fn boundary(surface_id: &str, name: &str, reason: &str, remediation: &str) -> Value {
    json!({"schema":format!("narada.{surface_id}.authority_boundary.v1"),"status":"unavailable","tool_name":name,"reason":reason,"remediation":remediation})
}
fn error(code: &str, message: &str) -> Value {
    json!({"schema":"narada.host_surface.error.v1","code":code,"message":message})
}
fn tool(name: &str, description: String, read_only: bool) -> Value {
    json!({"name":name,"description":description,"inputSchema":{"type":"object","additionalProperties":true},"annotations":{"title":name,"readOnlyHint":read_only,"destructiveHint":!read_only,"idempotentHint":read_only,"openWorldHint":!read_only},"outputSchema":{"type":"object","additionalProperties":true}})
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_root(label: &str) -> PathBuf {
        let suffix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        std::env::temp_dir().join(format!("narada-host-contracts-{label}-{suffix}"))
    }

    #[test]
    fn operator_status_projects_persisted_overlay_state() {
        let root = temp_root("operator");
        let state_root = root.join("overlay-state");
        let state_directory = state_root.join("operator-console");
        fs::create_dir_all(&state_directory).expect("directory");
        fs::write(state_directory.join("document.json"), r#"{"schema":"narada.window_surface_overlay.document.v1","id":"operator-console","title":"Fixture","title_tone":"default","subtitle":null,"rows":[],"actions":[],"updated_at":"2026-08-09T00:00:00Z"}"#).expect("document");
        fs::write(state_directory.join("action-state.json"), r#"{"schema":"narada.window_surface_overlay.action_state.v1","action_id":"refresh","request_id":"request-1","status":"succeeded"}"#).expect("action");
        fs::write(
            state_directory.join("visibility.state.json"),
            r#"{"schema":"narada.window_surface_overlay.visibility_state.v1","state":"visible"}"#,
        )
        .expect("visibility");
        fs::write(
            state_root.join("surface.snapshot.json"),
            r#"{"schema":"narada.window_surface_overlay.surface_snapshot.v1","status":"ready"}"#,
        )
        .expect("snapshot");
        fs::write(
            state_root.join("focus.owner.json"),
            r#"{"schema":"narada.window_surface_overlay.focus_owner.v1","owner":"fixture"}"#,
        )
        .expect("focus");
        let response = operator_status_at(&root, &state_root);
        assert_eq!(
            response["schema"],
            "narada.operator_console_overlay.mcp_result.v1"
        );
        assert_eq!(
            response["overlay"]["schema"],
            "narada.window_surface_overlay.result.v1"
        );
        assert_eq!(response["overlay"]["state"], "stopped");
        assert_eq!(response["overlay"]["document"]["title"], "Fixture");
        assert_eq!(response["overlay"]["action_state"]["status"], "succeeded");
        let _ = fs::remove_dir_all(root);
    }
}
