use serde_json::{json, Map, Value};
use std::fs;
use std::path::Path;

const SERVER_NAME: &str = "narada-calendar-mcp";
const MAX_TEXT_BYTES: u64 = 512_000;

// Calendar remains an explicit authority boundary.  This native module owns the
// wire contract, local policy inspection, and output paging; the Graph adapter
// is not silently reimplemented here because it transmits credentials and can
// mutate external calendar state.
pub fn list_tools() -> Vec<Value> {
    vec![
        guidance_tool(),
        tool("calendar_doctor", "Inspect local Microsoft Graph calendar policy posture.", json!({"type":"object","properties":{},"additionalProperties":false}), true),
        tool("calendar_list", "List calendars for an allowed mailbox.", json!({"type":"object","properties":{"mailbox_id":{"type":"string","default":"me"},"limit":{"type":"integer","minimum":1,"maximum":100,"default":20}},"additionalProperties":false}), true),
        tool("calendar_event_query", "Query calendar view events over an explicit time window.", json!({"type":"object","properties":{"mailbox_id":{"type":"string","default":"me"},"calendar_id":{"type":"string"},"start_datetime":{"type":"string"},"end_datetime":{"type":"string"},"select":{"type":"string"},"filter":{"type":"string"},"orderby":{"type":"string","default":"start/dateTime"},"limit":{"type":"integer","minimum":1,"maximum":100,"default":20}},"required":["start_datetime","end_datetime"],"additionalProperties":false}), true),
        tool("calendar_event_show", "Read one calendar event.", json!({"type":"object","properties":{"mailbox_id":{"type":"string","default":"me"},"event_id":{"type":"string"},"select":{"type":"string"}},"required":["event_id"],"additionalProperties":false}), true),
        tool("calendar_event_create", "Create an event when the approved Graph adapter is available.", write_schema(true, false), false),
        tool("calendar_event_update", "Update an event when the approved Graph adapter is available.", write_schema(false, true), false),
        tool("calendar_event_delete", "Delete an event when the approved Graph adapter is available.", json!({"type":"object","properties":{"mailbox_id":{"type":"string","default":"me"},"event_id":{"type":"string"},"confirm_write":{"type":"boolean","default":false},"approval_token":{"type":"string"}},"required":["event_id"],"additionalProperties":false}), false),
        tool("calendar_output_show", "Read a materialized Calendar MCP output ref with offset/limit paging.", json!({"type":"object","properties":{"ref":{"type":"string"},"output_ref":{"type":"string"},"offset":{"type":"integer","minimum":0},"limit":{"type":"integer","minimum":0}},"additionalProperties":false}), true),
    ]
}

pub fn auxiliary(method: &str, params: &Map<String, Value>) -> Result<Value, Value> {
    match method {
        "prompts/list" => Ok(json!({"prompts":[{"name":"calendar_workflow","title":"Calendar Workflow","description":"Live calendar reads and guarded event writes.","arguments":[]}]})),
        "prompts/get" => {
            if params.get("name").and_then(Value::as_str) != Some("calendar_workflow") { return Err(error("unknown_prompt", "unknown_prompt")); }
            Ok(json!({"description":"Live calendar reads and guarded event writes.","messages":[{"role":"user","content":{"type":"text","text":"Use calendar_event_query with explicit start and end timestamps. Event writes require site policy opt-in, confirm_write=true, and an approved external adapter."}}]}))
        }
        "completion/complete" => {
            let is_name = params.get("argument").and_then(Value::as_object).and_then(|v| v.get("name")).and_then(Value::as_str) == Some("name");
            let values = if is_name { list_tools().iter().filter_map(|v| v.get("name").cloned()).take(100).collect::<Vec<_>>() } else { Vec::new() };
            Ok(json!({"completion":{"values":values,"total":values.len(),"hasMore":false}}))
        }
        "logging/setLevel" => Ok(json!({})),
        _ => Err(error("unsupported_mcp_method", &format!("unsupported_mcp_method:{method}"))),
    }
}

pub fn call_tool(name: &str, args: &Map<String, Value>, root: &Path) -> Result<Value, Value> {
    match name {
        "calendar_guidance" => Ok(guidance(args)),
        "calendar_doctor" => doctor(root),
        "calendar_output_show" => output_show(args, root),
        "calendar_list" | "calendar_event_query" | "calendar_event_show" | "calendar_event_create" | "calendar_event_update" | "calendar_event_delete" => Err(authority_boundary(name)),
        _ => Err(error("unknown_tool", &format!("unknown_tool:{name}"))),
    }
}

fn guidance_tool() -> Value {
    tool("calendar_guidance", "Show model-facing operating guidance for calendar MCP workflows.", json!({"type":"object","properties":{"workflow":{"type":"string"},"tool":{"type":"string"}},"additionalProperties":false}), true)
}

fn tool(name: &str, description: &str, input_schema: Value, read_only: bool) -> Value {
    json!({"name":name,"description":description,"annotations":{"title":name,"readOnlyHint":read_only,"destructiveHint":name == "calendar_event_delete","idempotentHint":read_only,"openWorldHint":true},"inputSchema":input_schema,"outputSchema":{"type":"object","additionalProperties":true}})
}

fn guidance(args: &Map<String, Value>) -> Value {
    json!({"schema":"narada.mcp_surface.guidance.v0","status":"ok","surface_id":"calendar","guidance_tool":"calendar_guidance","purpose":"Policy-bounded calendar reads and explicitly approved event writes.","requested":{"workflow":args.get("workflow").cloned().unwrap_or(Value::Null),"tool":args.get("tool").cloned().unwrap_or(Value::Null)},"first_use":["Call calendar_doctor first.","Use explicit start_datetime and end_datetime for event queries.","Use show/read tools before any mutation.","Keep the external Graph adapter authority explicit."],"boundaries":["This native layer does not transmit Graph credentials or mutate external calendar state.","The existing Graph adapter remains authoritative until a separately approved native adapter is implemented.","Materialized output refs are local and bounded."]})
}

fn doctor(root: &Path) -> Result<Value, Value> {
    let path = root.join(".ai/calendar-mcp.json");
    let mut policy = Value::Object(Map::new());
    let mut policy_status = "missing";
    if path.exists() {
        if fs::metadata(&path).map_err(|e| error("calendar_policy_read_failed", &e.to_string()))?.len() > MAX_TEXT_BYTES { return Err(error("calendar_policy_too_large", "calendar_policy_too_large")); }
        let text = fs::read_to_string(&path).map_err(|e| error("calendar_policy_read_failed", &e.to_string()))?;
        policy = serde_json::from_str(&text).map_err(|e| error("calendar_policy_invalid_json", &e.to_string()))?;
        policy_status = "loaded";
    }
    let object = policy.as_object().cloned().unwrap_or_default();
    let allowed = object.get("allowed_mailboxes").or_else(|| object.get("allowedMailboxes")).cloned().unwrap_or_else(|| json!([]));
    let writes = object.get("allow_event_writes").and_then(Value::as_bool).unwrap_or(false) || object.get("allowEventWrites").and_then(Value::as_bool).unwrap_or(false);
    Ok(json!({"schema":"narada.calendar_mcp.doctor.v1","status":"ok","site_root":root.to_string_lossy(),"policy_path":path.to_string_lossy(),"policy_status":policy_status,"graph_base_url":object.get("graph_base_url").cloned().unwrap_or_else(|| json!("https://graph.microsoft.com/v1.0")),"allowed_mailboxes":allowed,"allow_event_writes":writes,"write_approval_token_configured":object.get("write_approval_token").or_else(|| object.get("writeApprovalToken")).and_then(Value::as_str).is_some(),"native_adapter":"contract_only","native_adapter_status":"awaiting_explicit_external-authority_approval","server_name":SERVER_NAME}))
}

fn output_show(args: &Map<String, Value>, root: &Path) -> Result<Value, Value> {
    let reference = args.get("ref").or_else(|| args.get("output_ref")).and_then(Value::as_str).ok_or_else(|| error("output_ref_required", "output_ref_required"))?;
    let id = reference.strip_prefix("mcp_output:").ok_or_else(|| error("output_ref_invalid", "output_ref_invalid"))?;
    if id.is_empty() || id.len() > 80 || !id.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-') { return Err(error("output_ref_invalid", "output_ref_invalid")); }
    let path = root.join(".ai/tmp/mcp-outputs/workspace").join(format!("{id}.json"));
    if fs::metadata(&path).map_err(|_| error("output_ref_not_found", "output_ref_not_found"))?.len() > MAX_TEXT_BYTES { return Err(error("output_ref_too_large", "output_ref_too_large")); }
    let text = fs::read_to_string(&path).map_err(|_| error("output_ref_not_found", "output_ref_not_found"))?;
    let record: Value = serde_json::from_str(&text).map_err(|e| error("output_ref_invalid_json", &e.to_string()))?;
    if record.get("schema").and_then(Value::as_str) != Some("narada.mcp_output_ref.v1") { return Err(error("output_ref_schema_unsupported", "output_ref_schema_unsupported")); }
    let full = record.get("full_output").cloned().unwrap_or(Value::Null);
    let presentation = serde_json::to_string_pretty(&full).unwrap_or_else(|_| full.to_string());
    let offset = args.get("offset").and_then(Value::as_u64).unwrap_or(0) as usize;
    let limit = args.get("limit").and_then(Value::as_u64).unwrap_or(4000).min(10000) as usize;
    let chars = presentation.chars().collect::<Vec<_>>();
    let start = offset.min(chars.len());
    let chunk = chars.iter().skip(start).take(limit).collect::<String>();
    let end = start + chunk.chars().count();
    Ok(json!({"schema":"narada.mcp_output_page.v1","status":"ok","ref":reference,"tool_name":record.get("tool_name"),"full_output_char_length":chars.len(),"byte_size":text.len(),"original_truncated":record.get("truncated").and_then(Value::as_bool).unwrap_or(false),"path":path.to_string_lossy(),"offset":start,"limit":limit,"next_offset":if end<chars.len(){json!(end)}else{Value::Null},"output_limit":limit,"output_truncated":end<chars.len(),"output_text":chunk}))
}

fn write_schema(create: bool, update: bool) -> Value {
    let mut properties = Map::new();
    properties.insert("mailbox_id".into(), json!({"type":"string","default":"me"}));
    for key in ["subject","body_text","body_html","start_datetime","end_datetime","time_zone","location","online_meeting_provider","approval_token"] { properties.insert(key.into(), json!({"type":"string"})); }
    properties.insert("attendees".into(), json!({"type":"array","items":{"type":["string","object"]}}));
    properties.insert("is_online_meeting".into(), json!({"type":"boolean"}));
    properties.insert("confirm_write".into(), json!({"type":"boolean","default":false}));
    if create { properties.insert("calendar_id".into(), json!({"type":"string"})); }
    if update { properties.insert("event_id".into(), json!({"type":"string"})); }
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
        assert!(tools.iter().any(|tool| tool["name"] == "calendar_event_query"));
        let doctor = call_tool("calendar_doctor", &Map::new(), &root).expect("doctor");
        assert_eq!(doctor["native_adapter"], "contract_only");
        let refusal = call_tool("calendar_event_query", &Map::new(), &root).expect_err("boundary");
        assert_eq!(refusal["status"], "unavailable");
        fs::remove_dir_all(root).expect("cleanup");
    }
}
