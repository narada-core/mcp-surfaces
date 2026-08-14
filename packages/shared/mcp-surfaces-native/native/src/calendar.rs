use serde_json::{json, Map, Value};
use std::collections::HashMap;
use std::fs;
use std::path::Path;

#[path = "calendar_provider.rs"]
pub(crate) mod provider;

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

fn auth_posture(root: &Path) -> (bool, &'static str) {
    let mut values = HashMap::new();
    if let Some(parent) = root.parent() {
        load_env_file(&mut values, &parent.join(".env"));
    }
    load_env_file(&mut values, &root.join(".env"));
    for key in [
        "MS_GRAPH_ACCESS_TOKEN",
        "GRAPH_ACCESS_TOKEN",
        "GRAPH_TENANT_ID",
        "GRAPH_CLIENT_ID",
        "GRAPH_CLIENT_SECRET",
    ] {
        if let Ok(value) = std::env::var(key) {
            values.insert(key.to_string(), value);
        }
    }
    let graph_access_token = non_empty_value(&values, "GRAPH_ACCESS_TOKEN");
    let client_credentials = ["GRAPH_TENANT_ID", "GRAPH_CLIENT_ID", "GRAPH_CLIENT_SECRET"]
        .iter()
        .all(|key| non_empty_value(&values, key));
    let ms_graph_access_token = non_empty_value(&values, "MS_GRAPH_ACCESS_TOKEN");
    if graph_access_token || (!client_credentials && ms_graph_access_token) {
        (true, "access_token")
    } else if client_credentials {
        (true, "client_credentials")
    } else {
        (false, "missing")
    }
}

fn load_env_file(values: &mut HashMap<String, String>, path: &Path) {
    let Ok(metadata) = fs::metadata(path) else {
        return;
    };
    if metadata.len() > MAX_TEXT_BYTES {
        return;
    }
    let Ok(text) = fs::read_to_string(path) else {
        return;
    };
    for line in text.lines() {
        let Some((key, raw_value)) = line.split_once('=') else {
            continue;
        };
        let key = key.trim();
        if key.is_empty() || key.starts_with('#') {
            continue;
        }
        let mut value = raw_value.trim().to_string();
        if value.len() >= 2
            && ((value.starts_with('"') && value.ends_with('"'))
                || (value.starts_with('\'') && value.ends_with('\'')))
        {
            value = value[1..value.len() - 1].to_string();
        }
        values.insert(key.to_string(), value);
    }
}

fn non_empty_value(values: &HashMap<String, String>, key: &str) -> bool {
    values
        .get(key)
        .is_some_and(|value| !value.trim().is_empty())
}

fn output_show(args: &Map<String, Value>, root: &Path) -> Result<Value, Value> {
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
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        return Err(error("output_ref_invalid", "output_ref_invalid"));
    }
    let path = root
        .join(".ai/tmp/mcp-outputs/workspace")
        .join(format!("{id}.json"));
    if fs::metadata(&path)
        .map_err(|_| error("output_ref_not_found", "output_ref_not_found"))?
        .len()
        > MAX_TEXT_BYTES
    {
        return Err(error("output_ref_too_large", "output_ref_too_large"));
    }
    let text = fs::read_to_string(&path)
        .map_err(|_| error("output_ref_not_found", "output_ref_not_found"))?;
    let record: Value = serde_json::from_str(&text)
        .map_err(|e| error("output_ref_invalid_json", &e.to_string()))?;
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
    let full = record.get("full_output").cloned().unwrap_or(Value::Null);
    let presentation = serde_json::to_string_pretty(&full).unwrap_or_else(|_| full.to_string());
    let offset = args.get("offset").and_then(Value::as_u64).unwrap_or(0) as usize;
    let limit = args
        .get("limit")
        .or_else(|| args.get("output_limit"))
        .and_then(Value::as_u64)
        .unwrap_or(10000)
        .min(10000) as usize;
    let chars = presentation.chars().collect::<Vec<_>>();
    let start = offset.min(chars.len());
    let chunk = chars.iter().skip(start).take(limit).collect::<String>();
    let end = start + chunk.chars().count();
    Ok(
        json!({"schema":"narada.mcp_output_page.v1","status":"ok","ref":reference,"tool_name":record.get("tool_name"),"full_output_char_length":record.get("full_output_char_length").cloned().unwrap_or_else(|| json!(chars.len())),"byte_size":text.len(),"original_truncated":record.get("truncated").and_then(Value::as_bool).unwrap_or(false),"path":format!(".ai/tmp/mcp-outputs/workspace/{id}.json"),"offset":start,"limit":limit,"next_offset":if end<chars.len(){json!(end)}else{Value::Null},"output_limit":limit,"output_truncated":end<chars.len(),"output_text":chunk}),
    )
}

fn write_schema(create: bool, update: bool) -> Value {
    let mut properties = Map::new();
    properties.insert("mailbox_id".into(), json!({"type":"string","default":"me"}));
    for key in [
        "subject",
        "body_text",
        "body_html",
        "start_datetime",
        "end_datetime",
        "time_zone",
        "location",
        "online_meeting_provider",
        "show_as",
        "sensitivity",
        "approval_token",
    ] {
        properties.insert(key.into(), json!({"type":"string"}));
    }
    properties.insert("attendees".into(), json!({"type":"array","items":{"oneOf":[{"type":"string"},{"type":"object","additionalProperties":false,"properties":{"emailAddress":{"type":"object","additionalProperties":false,"properties":{"address":{"type":"string"},"name":{"type":"string"}},"required":["address"]},"type":{"type":"string","enum":["required","optional","resource"]}},"required":["emailAddress"]}]}}));
    properties.insert("is_online_meeting".into(), json!({"type":"boolean"}));
    properties.insert(
        "confirm_write".into(),
        json!({"type":"boolean","default":false}),
    );
    if create {
        properties.insert("calendar_id".into(), json!({"type":"string"}));
    }
    if update {
        properties.insert("event_id".into(), json!({"type":"string"}));
    }
    json!({"type":"object","properties":properties,"required":if create {json!(["subject","start_datetime","end_datetime","time_zone"])} else {json!(["event_id"])},"additionalProperties":false})
}

fn authority_boundary(tool_name: &str) -> Value {
    json!({"schema":"narada.calendar_mcp.authority_boundary.v1","status":"unavailable","reason":"native_calendar_external_authority_not_enabled","tool_name":tool_name,"remediation":"Use the existing calendar Graph adapter, or explicitly approve a native adapter that transmits credentials and performs external calendar operations."})
}

fn error(code: &str, message: &str) -> Value {
    json!({"schema":"narada.calendar_mcp.error.v1","code":code,"message":message})
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_contract_keeps_external_authority_explicit() {
        let root = std::env::temp_dir().join(format!("narada-calendar-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&root).expect("root");
        let tools = list_tools();
        assert!(tools
            .iter()
            .any(|tool| tool["name"] == "calendar_event_query"));
        let doctor = call_tool("calendar_doctor", &Map::new(), &root).expect("doctor");
        assert_eq!(doctor["has_access_token"], false);
        assert_eq!(doctor["auth_mode"], "missing");
        let refusal = call_tool("calendar_event_query", &Map::new(), &root).expect_err("boundary");
        assert_eq!(refusal["status"], "unavailable");
        fs::remove_dir_all(root).expect("cleanup");
    }
}
