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

