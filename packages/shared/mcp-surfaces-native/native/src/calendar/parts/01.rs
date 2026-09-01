use serde_json::{json, Map, Value};
use std::collections::HashMap;
use std::fs;
use std::path::Path;

const SERVER_NAME: &str = "narada-calendar-mcp";
const DEFAULT_GRAPH_BASE_URL: &str = "https://graph.microsoft.com/v1.0";
const MAX_TEXT_BYTES: u64 = 512_000;

// Calendar keeps Graph credentials server-bound while the native launch profile
// explicitly activates the Rust Graph authority for reads and guarded writes.
pub fn list_tools() -> Vec<Value> {
    vec![
        guidance_tool(),
        tool(
            "calendar_doctor",
            "Inspect local Microsoft Graph calendar policy posture.",
            json!({"type":"object","properties":{},"additionalProperties":false}),
            true,
        ),
        tool(
            "calendar_list",
            "List calendars for an allowed mailbox.",
            json!({"type":"object","properties":{"mailbox_id":{"type":"string","default":"me"},"limit":{"type":"integer","minimum":1,"maximum":100,"default":20}},"additionalProperties":false}),
            true,
        ),
        tool(
            "calendar_event_query",
            "Query calendar view events over an explicit time window.",
            json!({"type":"object","properties":{"mailbox_id":{"type":"string","default":"me"},"calendar_id":{"type":"string"},"start_datetime":{"type":"string"},"end_datetime":{"type":"string"},"select":{"type":"string"},"filter":{"type":"string"},"orderby":{"type":"string","default":"start/dateTime"},"limit":{"type":"integer","minimum":1,"maximum":100,"default":20}},"required":["start_datetime","end_datetime"],"additionalProperties":false}),
            true,
        ),
        tool(
            "calendar_event_show",
            "Read one calendar event.",
            json!({"type":"object","properties":{"mailbox_id":{"type":"string","default":"me"},"event_id":{"type":"string"},"select":{"type":"string"}},"required":["event_id"],"additionalProperties":false}),
            true,
        ),
        tool(
            "calendar_event_create",
            "Create an event through the policy-gated native Graph authority.",
            write_schema(true, false),
            false,
        ),
        tool(
            "calendar_event_update",
            "Update an event through the policy-gated native Graph authority.",
            write_schema(false, true),
            false,
        ),
        tool(
            "calendar_event_delete",
            "Delete an event through the policy-gated native Graph authority.",
            json!({"type":"object","properties":{"mailbox_id":{"type":"string","default":"me"},"event_id":{"type":"string"},"confirm_write":{"type":"boolean","default":false},"approval_token":{"type":"string"}},"required":["event_id"],"additionalProperties":false}),
            false,
        ),
        tool(
            "calendar_output_show",
            "Read a materialized Calendar MCP output ref with offset/limit paging.",
            json!({"type":"object","properties":{"ref":{"type":"string"},"offset":{"type":"integer","minimum":0},"limit":{"type":"integer","minimum":1,"maximum":10000}},"required":["ref"],"additionalProperties":false}),
            true,
        ),
    ]
}

pub fn auxiliary(method: &str, params: &Map<String, Value>) -> Result<Value, Value> {
    match method {
        "prompts/list" => Ok(
            json!({"prompts":[{"name":"calendar_workflow","title":"Calendar Workflow","description":"Live calendar reads and guarded event writes.","arguments":[]}]}),
        ),
        "prompts/get" => {
            if params.get("name").and_then(Value::as_str) != Some("calendar_workflow") {
                return Err(error("unknown_prompt", "unknown_prompt"));
            }
            Ok(
                json!({"description":"Live calendar reads and guarded event writes.","messages":[{"role":"user","content":{"type":"text","text":"Use calendar_event_query with explicit start and end timestamps. Event writes require site policy opt-in and confirm_write=true; credentials remain inside the native Graph authority."}}]}),
            )
        }
        "completion/complete" => {
            let is_name = params
                .get("argument")
                .and_then(Value::as_object)
                .and_then(|v| v.get("name"))
                .and_then(Value::as_str)
                == Some("name");
            let values = if is_name {
                list_tools()
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

pub fn call_tool(name: &str, args: &Map<String, Value>, root: &Path) -> Result<Value, Value> {
    match name {
        "calendar_guidance" => Ok(guidance(args)),
        "calendar_doctor" => doctor(root),
        "calendar_output_show" => output_show(args, root),
        "calendar_list"
        | "calendar_event_query"
        | "calendar_event_show"
        | "calendar_event_create"
        | "calendar_event_update"
        | "calendar_event_delete" => authority_call(name, args, root),
        _ => Err(error("unknown_tool", &format!("unknown_tool:{name}"))),
    }
}

fn authority_call(name: &str, args: &Map<String, Value>, root: &Path) -> Result<Value, Value> {
    if native_graph_authority_enabled() {
        return crate::graph_authority::call_calendar(name, args, root);
    }
    if let Some(result) = crate::authority::call_if_configured("calendar", name, args) {
        return result;
    }
    Err(authority_boundary(name))
}

fn native_graph_authority_enabled() -> bool {
    matches!(
        std::env::var("NARADA_NATIVE_GRAPH_AUTHORITY")
            .ok()
            .as_deref()
            .map(str::trim),
        Some("1" | "true" | "yes")
    )
}

fn guidance_tool() -> Value {
    tool(
        "calendar_guidance",
        "Show model-facing operating guidance for calendar MCP workflows.",
        json!({"type":"object","properties":{"workflow":{"type":"string"},"tool":{"type":"string"}},"additionalProperties":false}),
        true,
    )
}

fn tool(name: &str, description: &str, input_schema: Value, read_only: bool) -> Value {
    json!({"name":name,"description":description,"annotations":{"title":name,"readOnlyHint":read_only,"destructiveHint":name == "calendar_event_delete","idempotentHint":read_only,"openWorldHint":true},"inputSchema":input_schema,"outputSchema":{"type":"object","additionalProperties":true}})
}

fn guidance(args: &Map<String, Value>) -> Value {
    let workflow = trimmed_arg(args, "workflow");
    let tool = trimmed_arg(args, "tool");
    json!({
        "schema": "narada.mcp_surface.guidance.v0",
        "status": "ok",
        "surface_id": "calendar",
        "guidance_tool": "calendar_guidance",
        "purpose": "Policy-gated Microsoft Graph calendar reads and guarded event lifecycle.",
        "requested": {"workflow": workflow, "tool": tool},
        "first_use": [
            "Call this guidance command when the surface is unfamiliar, when a refusal/error is unclear, or before composing a multi-step workflow.",
            "Inspect policy/doctor/status tools before mutation or open-world operations.",
            "Use bounded list/search/query tools for discovery, then show/read/detail tools before acting on a specific object.",
            "Preserve structuredContent as authoritative evidence; text content is for assistant readability."
        ],
        "tool_preference": [
            {"step":"orient","guidance":"Use *_guidance first when uncertain, then policy/doctor/status tools."},
            {"step":"discover","guidance":"Use bounded list/search/query commands with explicit limits and filters."},
            {"step":"inspect","guidance":"Use show/read/detail commands for exact targets before mutation."},
            {"step":"mutate","guidance":"Only call mutation tools after policy allows it and intent, target, and expected result are explicit."},
            {"step":"verify","guidance":"Read back state with the owning surface after any mutation."}
        ],
        "examples": [
            {"intent":"First use","call":"calendar_guidance({})"},
            {"intent":"Tool-specific help","call":"calendar_guidance({ tool: \"<tool_name>\" })"},
            {"intent":"Workflow-specific help","call":"calendar_guidance({ workflow: \"<workflow_name>\" })"}
        ],
        "anti_patterns": [
            "Do not guess hidden state from a tool name; use doctor/status/list/show tools for evidence.",
            "Do not treat assistant text as the durable record when structuredContent is present.",
            "Do not bypass the owning surface with shell scripts when a governed MCP tool exists.",
            "Do not continue after malformed payloads, empty refs, or ambiguous target identifiers; stop and repair the input."
        ],
        "recovery": [
            "For unknown_tool, call tools/list and this guidance command again after restart.",
            "For policy refusal, inspect the surface policy/doctor output and report the exact refusal reason.",
            "For oversized inputs, use the surface payload_ref or output_ref convention when it exists; otherwise reduce scope.",
            "For unclear behavior, submit surface_feedback_submit with surface_id, kind, summary, reproduction steps, expected behavior, and impact."
        ],
        "feedback": {
            "surface_id": "calendar",
            "tool": "surface_feedback_submit",
            "when": [
                "guidance is missing, stale, or contradicted by live behavior",
                "schema shape makes correct usage hard",
                "errors hide the actionable refusal or recovery path"
            ]
        },
        "boundaries": [
            "Guidance is read-only model-facing operating advice.",
            "Guidance does not weaken policy, authorize mutation, or replace tool schemas.",
            "The owning MCP surface remains authoritative for state and enforcement."
        ]
    })
}

fn trimmed_arg(args: &Map<String, Value>, key: &str) -> Value {
    args.get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| Value::String(value.to_string()))
        .unwrap_or(Value::Null)
}

fn doctor(root: &Path) -> Result<Value, Value> {
    let path = root.join(".ai/calendar-mcp.json");
    let mut policy = Value::Object(Map::new());
    if path.exists() {
        if fs::metadata(&path)
            .map_err(|e| error("calendar_policy_read_failed", &e.to_string()))?
            .len()
            > MAX_TEXT_BYTES
        {
            return Err(error(
                "calendar_policy_too_large",
                "calendar_policy_too_large",
            ));
        }
        let text = fs::read_to_string(&path)
            .map_err(|e| error("calendar_policy_read_failed", &e.to_string()))?;
        policy = serde_json::from_str(&text)
            .map_err(|e| error("calendar_policy_invalid_json", &e.to_string()))?;
    }
    let object = policy.as_object().cloned().unwrap_or_default();
    let allowed = object
        .get("allowed_mailboxes")
        .or_else(|| object.get("allowedMailboxes"))
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .filter(|value| !value.trim().is_empty())
                .map(ToOwned::to_owned)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let writes = object
        .get("allow_event_writes")
        .and_then(Value::as_bool)
        .unwrap_or(false)
        || object
            .get("allowEventWrites")
            .and_then(Value::as_bool)
            .unwrap_or(false);
    let graph_base_url = object
        .get("graph_base_url")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .map(|value| value.trim_end_matches('/').to_owned())
        .unwrap_or_else(|| DEFAULT_GRAPH_BASE_URL.to_string());
    let write_approval_token_configured = object
        .get("write_approval_token")
        .or_else(|| object.get("writeApprovalToken"))
        .and_then(Value::as_str)
        .map(|value| !value.is_empty())
        .unwrap_or(false);
    let (has_access_token, auth_mode) = auth_posture(root);
    Ok(
        json!({"schema":"narada.calendar_mcp.doctor.v1","status":"ok","site_root":root.to_string_lossy(),"graph_base_url":graph_base_url,"has_access_token":has_access_token,"auth_mode":auth_mode,"allowed_mailboxes":allowed,"allow_event_writes":writes,"write_approval_token_configured":write_approval_token_configured,"server_name":SERVER_NAME}),
    )
}

