use serde_json::{json, Map, Value};
use std::fs;
use std::path::{Path, PathBuf};

const SERVER_NAME: &str = "sop-mcp";
const MAX_CANDIDATES: usize = 100;
const MAX_TEMPLATE_CHARS: usize = 32_000;
const MUTATING: &[&str] = &[
    "sop_template_create", "sop_template_update", "sop_template_deprecate", "sop_template_unimport", "sop_template_import_yaml",
    "sop_run_start", "sop_run_refresh", "sop_run_advance", "sop_handoff_claim", "sop_handoff_renew", "sop_handoff_release", "sop_handoff_retry",
    "sop_action_resolve", "sop_run_cancel", "sop_outbox_consumer_register", "sop_outbox_ack", "sop_outbox_compact",
];

pub fn list_tools() -> Vec<Value> {
    let mut tools = vec![guidance_tool()];
    for (name, description, schema) in [
        ("sop_doctor", "Inspect configured SOP directories and native read posture.", json!({"type":"object","properties":{},"additionalProperties":false})),
        ("sop_template_candidate_list", "List bounded SOP YAML template candidates from configured directories.", json!({"type":"object","properties":{"limit":{"type":"integer","minimum":1,"maximum":100,"default":50}},"additionalProperties":false})),
        ("sop_template_candidate_show", "Show one bounded SOP YAML template candidate.", json!({"type":"object","properties":{"sop_id":{"type":"string"}},"required":["sop_id"],"additionalProperties":false})),
        ("sop_template_list", "List imported SOP templates when a native registry is available.", json!({"type":"object","properties":{"limit":{"type":"integer","minimum":1,"maximum":100,"default":50}},"additionalProperties":false})),
        ("sop_template_show", "Show one imported SOP template when a native registry is available.", json!({"type":"object","properties":{"sop_id":{"type":"string"}},"required":["sop_id"],"additionalProperties":false})),
        ("sop_template_search", "Search bounded SOP template candidates by text.", json!({"type":"object","properties":{"query":{"type":"string"},"limit":{"type":"integer","minimum":1,"maximum":100,"default":50}},"required":["query"],"additionalProperties":false})),
        ("sop_run_status", "Read one SOP run from the owning SOP store.", json!({"type":"object","additionalProperties":true})),
        ("sop_run_list", "List SOP runs from the owning SOP store.", json!({"type":"object","additionalProperties":true})),
        ("sop_handoff_list", "List SOP handoffs from the owning SOP store.", json!({"type":"object","additionalProperties":true})),
        ("sop_handoff_show", "Show one SOP handoff from the owning SOP store.", json!({"type":"object","additionalProperties":true})),
        ("sop_action_list", "List SOP actions from the owning SOP store.", json!({"type":"object","additionalProperties":true})),
        ("sop_action_show", "Show one SOP action from the owning SOP store.", json!({"type":"object","additionalProperties":true})),
        ("sop_run_coverage_since", "Read SOP coverage evidence from the owning SOP store.", json!({"type":"object","additionalProperties":true})),
        ("sop_run_events", "Read SOP run events from the owning SOP store.", json!({"type":"object","additionalProperties":true})),
        ("sop_outbox_list", "Read SOP outbox entries from the owning SOP store.", json!({"type":"object","additionalProperties":true})),
    ] {
        tools.push(tool(name, description, schema, true));
    }
    for name in MUTATING { tools.push(tool(name, "SOP mutation remains owned by the configured SOP authority.", json!({"type":"object","additionalProperties":true}), false)); }
    tools
}

pub fn auxiliary(method: &str, params: &Map<String, Value>) -> Result<Value, Value> {
    match method {
        "prompts/list" => Ok(json!({"prompts":[{"name":"sop_workflow","title":"SOP Workflow","description":"Inspect templates and run posture before starting or advancing a governed SOP.","arguments":[]}]})),
        "prompts/get" => { if params.get("name").and_then(Value::as_str) != Some("sop_workflow") { return Err(error("unknown_prompt","unknown_prompt")); } Ok(json!({"description":"Inspect templates and run posture before starting or advancing a governed SOP.","messages":[{"role":"user","content":{"type":"text","text":"Use sop_doctor and sop_template_candidate_list/show before importing or running an SOP. Run and handoff mutations remain with the owning SOP authority."}}]})) }
        "completion/complete" => { let is_name = params.get("argument").and_then(Value::as_object).and_then(|v|v.get("name")).and_then(Value::as_str) == Some("name"); let values = if is_name { list_tools().iter().filter_map(|v|v.get("name").cloned()).take(100).collect::<Vec<_>>() } else { Vec::new() }; Ok(json!({"completion":{"values":values,"total":values.len(),"hasMore":false}})) }
        "logging/setLevel" => Ok(json!({})),
        _ => Err(error("unsupported_mcp_method", &format!("unsupported_mcp_method:{method}"))),
    }
}

pub fn call_tool(name: &str, args: &Map<String, Value>, root: &Path) -> Result<Value, Value> {
    match name {
        "sop_guidance" => Ok(guidance(args)),
        "sop_doctor" => doctor(root),
        "sop_template_candidate_list" => candidate_list(args, root),
        "sop_template_candidate_show" => candidate_show(args, root),
        "sop_template_search" => candidate_search(args, root),
        "sop_template_list" | "sop_template_show" | "sop_run_status" | "sop_run_list" | "sop_handoff_list" | "sop_handoff_show" | "sop_action_list" | "sop_action_show" | "sop_run_coverage_since" | "sop_run_events" | "sop_outbox_list" => Err(authority_boundary(name)),
        name if MUTATING.contains(&name) => Err(authority_boundary(name)),
        _ => Err(error("unknown_tool", &format!("unknown_tool:{name}"))),
    }
}

fn guidance_tool() -> Value { tool("sop_guidance", "Show model-facing operating guidance for SOP workflows.", json!({"type":"object","properties":{"workflow":{"type":"string"},"tool":{"type":"string"}},"additionalProperties":false}), true) }
fn guidance(args: &Map<String, Value>) -> Value { json!({"schema":"narada.mcp_surface.guidance.v0","status":"ok","surface_id":"sop","guidance_tool":"sop_guidance","purpose":"Inspect bounded SOP templates and delegated run posture.","requested":{"workflow":args.get("workflow").cloned().unwrap_or(Value::Null),"tool":args.get("tool").cloned().unwrap_or(Value::Null)},"first_use":["Call sop_doctor first.","Use candidate list/show/search for local template discovery.","Inspect a template before import or execution.","Keep run, handoff, action, and outbox state with the owning SOP authority."],"boundaries":["The native slice reads local YAML only.","It does not parse or execute arbitrary commands from YAML.","Durable SOP registry/run state remains an explicit authority boundary."]}) }

fn sops_dirs(root: &Path) -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(value) = std::env::var_os("NARADA_SOPS_DIR") { dirs.push(PathBuf::from(value)); }
    dirs.push(root.join("sops")); dirs.push(root.join(".ai/sops"));
    dirs.into_iter().filter(|path| path.is_dir()).take(10).collect()
}

fn doctor(root: &Path) -> Result<Value, Value> {
    let dirs = sops_dirs(root); let mut counts = Vec::new();
    for dir in &dirs { let count = fs::read_dir(dir).ok().map(|entries| entries.filter_map(Result::ok).filter(|entry| entry.path().file_name().and_then(|v|v.to_str()).map(|v|v.ends_with(".sop.yaml")).unwrap_or(false)).take(MAX_CANDIDATES).count()).unwrap_or(0); counts.push(json!({"path":dir.to_string_lossy(),"candidate_count":count})); }
    Ok(json!({"schema":"narada.sop_mcp.doctor.v1","status":"ok","site_root":root.to_string_lossy(),"sops_dirs":counts,"native_adapter":"template_read_slice","execution":"authority_boundary","server_name":SERVER_NAME}))
}

fn candidate_entries(root: &Path) -> Vec<(String, PathBuf)> {
    let mut entries = Vec::new();
    for dir in sops_dirs(root) { if let Ok(read) = fs::read_dir(dir) { for entry in read.filter_map(Result::ok).take(MAX_CANDIDATES) { let path = entry.path(); if path.file_name().and_then(|v|v.to_str()).map(|v|v.ends_with(".sop.yaml")).unwrap_or(false) { if let Some(name) = path.file_name().and_then(|v|v.to_str()).map(|v|v.trim_end_matches(".sop.yaml").to_string()) { entries.push((name,path)); } } if entries.len() >= MAX_CANDIDATES { break; } } } }
    entries
}

fn candidate_list(args: &Map<String, Value>, root: &Path) -> Result<Value, Value> {
    let limit = args.get("limit").and_then(Value::as_u64).unwrap_or(50).clamp(1, MAX_CANDIDATES as u64) as usize;
    let candidates = candidate_entries(root).into_iter().take(limit).map(|(sop_id,path)| { let meta = fs::metadata(&path).ok(); json!({"sop_id":sop_id,"path":path.to_string_lossy(),"bytes":meta.as_ref().map(|m|m.len()),"modified":meta.and_then(|m|m.modified().ok()).and_then(|v|v.duration_since(std::time::UNIX_EPOCH).ok()).map(|v|v.as_secs()),"import_state":"unverified"}) }).collect::<Vec<_>>();
    Ok(json!({"schema":"narada.sop_mcp.template_candidates.v1","status":"ok","count":candidates.len(),"limit":limit,"candidates":candidates,"native_read_only":true}))
}

fn safe_id(args: &Map<String, Value>) -> Result<String, Value> { let id = args.get("sop_id").and_then(Value::as_str).filter(|v|!v.trim().is_empty()).ok_or_else(||error("sop_id_required","sop_id_required"))?.trim().to_string(); if id.len()>120 || !id.chars().all(|c|c.is_ascii_alphanumeric() || c=='-' || c=='_' || c=='.') { return Err(error("sop_id_invalid","sop_id_invalid")); } Ok(id) }

fn candidate_show(args: &Map<String, Value>, root: &Path) -> Result<Value, Value> {
    let id = safe_id(args)?; let Some((_, path)) = candidate_entries(root).into_iter().find(|(candidate, _)| candidate == &id) else { return Err(error("sop_yaml_not_found","sop_yaml_not_found")); };
    let text = fs::read_to_string(&path).map_err(|e|error("sop_yaml_read_failed",&e.to_string()))?; let truncated = text.chars().count() > MAX_TEMPLATE_CHARS; let bounded = text.chars().take(MAX_TEMPLATE_CHARS).collect::<String>();
    Ok(json!({"schema":"narada.sop_mcp.template_candidate.v1","status":"ok","sop_id":id,"path":path.to_string_lossy(),"raw_yaml":bounded,"truncated":truncated,"import_state":"unverified","native_read_only":true}))
}

fn candidate_search(args: &Map<String, Value>, root: &Path) -> Result<Value, Value> {
    let query = args.get("query").and_then(Value::as_str).filter(|v|!v.trim().is_empty()).ok_or_else(||error("query_required","query_required"))?.to_ascii_lowercase(); let limit = args.get("limit").and_then(Value::as_u64).unwrap_or(50).clamp(1,MAX_CANDIDATES as u64) as usize; let mut matches = Vec::new();
    for (id,path) in candidate_entries(root) { if matches.len() >= limit { break; } if let Ok(text) = fs::read_to_string(&path) { if text.to_ascii_lowercase().contains(&query) || id.to_ascii_lowercase().contains(&query) { matches.push(json!({"sop_id":id,"path":path.to_string_lossy(),"match":"text"})); } } }
    Ok(json!({"schema":"narada.sop_mcp.template_search.v1","status":"ok","query":query,"count":matches.len(),"matches":matches,"native_read_only":true}))
}

fn authority_boundary(name: &str) -> Value { json!({"schema":"narada.sop_mcp.authority_boundary.v1","status":"unavailable","tool_name":name,"reason":"sop_registry_or_execution_not_enabled_in_native_template_slice","remediation":"Use the configured SOP authority for registry, run, handoff, action, and outbox operations."}) }
fn error(code: &str, message: &str) -> Value { json!({"schema":"narada.sop_mcp.error.v1","code":code,"message":message}) }
fn tool(name: &str, description: &str, schema: Value, read_only: bool) -> Value { json!({"name":name,"description":description,"annotations":{"title":name,"readOnlyHint":read_only,"destructiveHint":!read_only,"idempotentHint":read_only,"openWorldHint":false},"inputSchema":schema,"outputSchema":{"type":"object","additionalProperties":true}}) }

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn native_sop_template_read_is_bounded() {
        let root = std::env::temp_dir().join(format!("narada-sop-{}", uuid::Uuid::new_v4())); let dir = root.join("sops"); fs::create_dir_all(&dir).expect("dir"); fs::write(dir.join("demo.sop.yaml"), "schema: narada.sop.v1\nid: demo\n").expect("yaml");
        assert_eq!(candidate_list(&json!({"limit":1}).as_object().unwrap(), &root).expect("list")["count"], 1);
        assert!(candidate_show(&json!({"sop_id":"demo"}).as_object().unwrap(), &root).expect("show")["raw_yaml"].as_str().unwrap().contains("demo"));
        fs::remove_dir_all(root).expect("cleanup");
    }
}
