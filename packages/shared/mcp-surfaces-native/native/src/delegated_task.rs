use serde_json::{json, Map, Value};
use std::fs;
use std::path::{Path, PathBuf};

const SERVER_NAME: &str = "delegated-task-mcp";
const MAX_ITEMS: usize = 200;
const MUTATING: &[&str] = &[
    "delegated_task_run", "delegated_task_advance", "delegated_task_cancel", "delegated_task_acknowledge", "delegated_task_parent_takeover",
];

pub fn list_tools() -> Vec<Value> {
    let mut tools = vec![guidance_tool()];
    for (name, desc, schema) in [
        ("delegated_task_policy_inspect", "Inspect delegated task orchestration policy and defaults.", json!({"type":"object","properties":{},"additionalProperties":false})),
        ("delegated_task_template_catalog", "List built-in delegated workflow templates and contracts.", json!({"type":"object","properties":{"template_id":{"type":"string"}},"additionalProperties":false})),
        ("delegated_task_validate", "Validate delegated task input without creating or running a task.", json!({"type":"object","properties":{"objective":{"type":"string"},"workflow":{"type":"object"},"constraints":{"type":"object"},"acceptance":{"type":"object"},"execution":{"type":"object"},"execution_binding":{"type":"object"}},"additionalProperties":false})),
        ("delegated_task_status", "Return compact durable delegated task status.", id_schema(true)),
        ("delegated_tasks_list", "List bounded delegated tasks by lifecycle and site scope.", json!({"type":"object","properties":{"limit":{"type":"integer","minimum":1,"maximum":200,"default":20},"view":{"type":"string"},"site_scope":{"type":"string"}},"additionalProperties":false})),
        ("delegated_task_result", "Return a delegated task result handoff from durable state.", id_schema(true)),
        ("delegated_task_summary", "Return a compact human review summary from durable state.", id_schema(true)),
        ("delegated_task_events", "List bounded delegated task events.", json!({"type":"object","properties":{"task_id":{"type":"string"},"limit":{"type":"integer","minimum":1,"maximum":100,"default":50},"offset":{"type":"integer","minimum":0,"maximum":10000}},"required":["task_id"],"additionalProperties":false})),
    ] { tools.push(tool(name, desc, schema, true)); }
    for name in MUTATING { tools.push(tool(name, "Delegated task mutation remains owned by the worker/task authority.", json!({"type":"object","additionalProperties":true}), false)); }
    tools.push(tool("delegated_task_wait", "Wait for a delegated task to advance toward terminal status; native mode only reports durable state.", id_schema(true), true));
    tools
}

pub fn auxiliary(method: &str, params: &Map<String, Value>) -> Result<Value, Value> {
    match method {
        "prompts/list" => Ok(json!({"prompts":[{"name":"delegated_task_workflow","title":"Delegated Task Workflow","description":"Validate and inspect durable delegated tasks before execution or disposition.","arguments":[]}]})),
        "prompts/get" => { if params.get("name").and_then(Value::as_str) != Some("delegated_task_workflow") { return Err(error("unknown_prompt","unknown_prompt")); } Ok(json!({"description":"Validate and inspect durable delegated tasks before execution or disposition.","messages":[{"role":"user","content":{"type":"text","text":"Use delegated_task_validate before creation, delegated_tasks_list/status/result/events for readback, and keep worker execution with the owning worker authority."}}]})) }
        "completion/complete" => { let is_name = params.get("argument").and_then(Value::as_object).and_then(|v|v.get("name")).and_then(Value::as_str) == Some("name"); let values = if is_name { list_tools().iter().filter_map(|v|v.get("name").cloned()).take(100).collect::<Vec<_>>() } else { Vec::new() }; Ok(json!({"completion":{"values":values,"total":values.len(),"hasMore":false}})) }
        "logging/setLevel" => Ok(json!({})),
        _ => Err(error("unsupported_mcp_method", &format!("unsupported_mcp_method:{method}"))),
    }
}

pub fn call_tool(name: &str, args: &Map<String, Value>, root: &Path) -> Result<Value, Value> {
    match name {
        "delegated_task_guidance" => Ok(guidance(args)),
        "delegated_task_policy_inspect" => Ok(policy(root)),
        "delegated_task_template_catalog" => Ok(template_catalog(args)),
        "delegated_task_validate" => validate(args, root),
        "delegated_tasks_list" => tasks_list(args, root),
        "delegated_task_status" => task_status(args, root),
        "delegated_task_result" => task_result(args, root),
        "delegated_task_summary" => task_summary(args, root),
        "delegated_task_events" => task_events(args, root),
        "delegated_task_wait" => task_wait(args, root),
        name if MUTATING.contains(&name) => Err(authority_boundary(name)),
        _ => Err(error("unknown_tool", &format!("unknown_tool:{name}"))),
    }
}

fn guidance_tool() -> Value { tool("delegated_task_guidance", "Show model-facing operating guidance for delegated task workflows.", json!({"type":"object","properties":{"workflow":{"type":"string"},"tool":{"type":"string"}},"additionalProperties":false}), true) }
fn guidance(args: &Map<String, Value>) -> Value { json!({"schema":"narada.mcp_surface.guidance.v0","status":"ok","surface_id":"delegated-task","guidance_tool":"delegated_task_guidance","purpose":"Validate and inspect durable delegated task workflows without silently launching workers.","requested":{"workflow":args.get("workflow").cloned().unwrap_or(Value::Null),"tool":args.get("tool").cloned().unwrap_or(Value::Null)},"first_use":["Call delegated_task_policy_inspect first.","Validate an input before creating a task.","Use bounded list/status/result/events readback.","Keep worker execution, cancellation, and disposition with the owning authority."],"boundaries":["Native code reads task.json/events.jsonl under the bounded task root.","It never spawns a worker or changes task state.","Cross-site ownership remains server-bound authority."]}) }

fn task_root(root: &Path) -> PathBuf {
    if root.join("tasks").is_dir() {
        root.to_path_buf()
    } else if root.join(".ai/delegated-tasks/tasks").is_dir() {
        root.join(".ai/delegated-tasks")
    } else if root.join(".narada/tasks").is_dir() {
        root.join(".narada")
    } else {
        root.to_path_buf()
    }
}
fn tasks_dir(root: &Path) -> PathBuf { task_root(root).join("tasks") }
fn safe_id(id: &str) -> Result<String, Value> { if id.is_empty() || id.len()>120 || !id.chars().all(|c|c.is_ascii_alphanumeric() || c=='-' || c=='_') { return Err(error("delegated_task_id_invalid","delegated_task_id_invalid")); } Ok(id.to_string()) }
fn task_path(root: &Path, id: &str) -> Result<PathBuf, Value> { let id = safe_id(id)?; Ok(tasks_dir(root).join(id).join("task.json")) }
fn read_task(root: &Path, id: &str) -> Result<Value, Value> { let path = task_path(root,id)?; let text = fs::read_to_string(&path).map_err(|_|error("delegated_task_not_found","delegated_task_not_found"))?; serde_json::from_str(&text).map_err(|e|error("delegated_task_invalid_json",&e.to_string())) }
fn task_id(args: &Map<String, Value>) -> Result<String, Value> { args.get("task_id").and_then(Value::as_str).filter(|v|!v.trim().is_empty()).map(|v|v.trim().to_string()).ok_or_else(||error("task_id_required","task_id_required")) }

fn policy(root: &Path) -> Value { json!({"schema":"narada.delegated_task.policy.v1","status":"ok","server_name":SERVER_NAME,"task_root":task_root(root).to_string_lossy(),"site_root":root.to_string_lossy(),"allowed_roots":[root.to_string_lossy()],"list_defaults":{"view":"active_queue","site_scope":"current_site"},"workflow_engine":"native_read_slice","worker_execution":"authority_boundary","result_compaction":{"max_worker_refs":50,"max_list_items":200}}) }
fn template_catalog(args: &Map<String, Value>) -> Value { let id = args.get("template_id").and_then(Value::as_str); let templates = vec![json!({"template_id":"task_executability_assessment","description":"Assess whether a delegated task is executable under current authority and binding constraints."}),json!({"template_id":"bounded_worker_workflow","description":"Run bounded worker steps with explicit acceptance and result policy."})]; let filtered = templates.into_iter().filter(|v|id.is_none() || v.get("template_id").and_then(Value::as_str)==id).collect::<Vec<_>>(); json!({"schema":"narada.delegated_task.template_catalog.v1","status":if id.is_some()&&filtered.is_empty(){"not_found"}else{"ok"},"template_id":id,"count":filtered.len(),"templates":filtered}) }

fn validate(args: &Map<String, Value>, root: &Path) -> Result<Value, Value> { let objective = args.get("objective").and_then(Value::as_str).map(str::trim).filter(|v|!v.is_empty()); let mut errors = Vec::new(); if objective.is_none(){errors.push("objective_required");} if let Some(binding)=args.get("execution_binding").and_then(Value::as_object) { if let Some(workspace)=binding.get("workspace_root").and_then(Value::as_str) { if !is_within(Path::new(workspace), root) { errors.push("execution_binding_workspace_outside_site_root"); } } } Ok(json!({"schema":"narada.delegated_task.validate.v1","status":if errors.is_empty(){"ok"}else{"invalid"},"valid":errors.is_empty(),"task_root":task_root(root).to_string_lossy(),"errors":errors,"objective":objective,"worker_execution":"not_run"})) }
fn is_within(path: &Path, root: &Path) -> bool { let p=path.canonicalize().unwrap_or_else(|_|path.to_path_buf()); let r=root.canonicalize().unwrap_or_else(|_|root.to_path_buf()); p==r || p.starts_with(&r) }

fn tasks_list(args: &Map<String, Value>, root: &Path) -> Result<Value, Value> { let limit=args.get("limit").and_then(Value::as_u64).unwrap_or(20).clamp(1,MAX_ITEMS as u64) as usize; let mut tasks=Vec::new(); if let Ok(entries)=fs::read_dir(tasks_dir(root)) { for entry in entries.filter_map(Result::ok).take(MAX_ITEMS) { if !entry.path().is_dir(){continue;} let Some(id)=entry.file_name().to_str().map(ToOwned::to_owned) else {continue;}; if let Ok(task)=read_task(root,&id) { tasks.push(compact_task(&task)); } } } tasks.sort_by(|a,b| b.get("updated_at").and_then(Value::as_str).cmp(&a.get("updated_at").and_then(Value::as_str))); tasks.truncate(limit); Ok(json!({"schema":"narada.delegated_task.list.v1","status":"ok","view":args.get("view").and_then(Value::as_str).unwrap_or("active_queue"),"site_scope":args.get("site_scope").and_then(Value::as_str).unwrap_or("current_site"),"count":tasks.len(),"limit":limit,"tasks":tasks,"native_read_only":true})) }
fn compact_task(task: &Value) -> Value { let obj=task.as_object().cloned().unwrap_or_default(); json!({"task_id":obj.get("task_id"),"task_status":obj.get("status"),"objective":obj.get("objective"),"owner_site_id":obj.get("owner_site_id"),"created_by_site_id":obj.get("created_by_site_id"),"visibility_scope":obj.get("visibility_scope"),"updated_at":obj.get("updated_at"),"summary":obj.get("summary"),"execution_binding":obj.get("execution_binding")}) }
fn task_status(args: &Map<String, Value>, root: &Path) -> Result<Value, Value> { let id=task_id(args)?; let task=read_task(root,&id)?; let obj=task.as_object().cloned().unwrap_or_default(); Ok(json!({"schema":"narada.delegated_task.status.v1","status":"ok","task_id":id,"task_status":obj.get("status"),"objective":obj.get("objective"),"ownership":{"owner_site_id":obj.get("owner_site_id"),"owner_site_root":obj.get("owner_site_root"),"visibility_scope":obj.get("visibility_scope")},"execution_binding":obj.get("execution_binding"),"request_fingerprint":obj.get("request_fingerprint"),"created_at":obj.get("created_at"),"updated_at":obj.get("updated_at"),"result":obj.get("result"),"native_read_only":true})) }
fn task_result(args: &Map<String, Value>, root: &Path) -> Result<Value, Value> { let id=task_id(args)?; let task=read_task(root,&id)?; Ok(json!({"schema":"narada.delegated_task.result.v1","status":"ok","task_id":id,"task_status":task.get("status"),"result":task.get("result"),"summary":task.get("summary"),"native_read_only":true})) }
fn task_summary(args: &Map<String, Value>, root: &Path) -> Result<Value, Value> { let id=task_id(args)?; let task=read_task(root,&id)?; let result=task.get("result").and_then(Value::as_object).cloned().unwrap_or_default(); Ok(json!({"schema":"narada.delegated_task.summary.v1","status":"ok","task_id":id,"task_status":task.get("status"),"objective":task.get("objective"),"summary":task.get("summary"),"acceptance_verdict":result.get("acceptance_verdict").cloned().unwrap_or(Value::String("pending".into())),"residual_risks":result.get("residual_risks").cloned().unwrap_or_else(||json!([])),"progress":result.get("progress"),"native_read_only":true})) }
fn task_wait(args: &Map<String, Value>, root: &Path) -> Result<Value, Value> { let id=task_id(args)?; let task=read_task(root,&id)?; Ok(json!({"schema":"narada.delegated_task.wait.v1","status":"ok","task_id":id,"task_status":task.get("status"),"waited":false,"refresh_performed":false,"worker_execution":"authority_boundary","task":compact_task(&task),"native_read_only":true})) }
fn task_events(args: &Map<String, Value>, root: &Path) -> Result<Value, Value> { let id=task_id(args)?; let path=task_path(root,&id)?.with_file_name("events.jsonl"); let limit=args.get("limit").and_then(Value::as_u64).unwrap_or(50).clamp(1,100) as usize; let offset=args.get("offset").and_then(Value::as_u64).unwrap_or(0).min(10000) as usize; let mut events=Vec::new(); if let Ok(text)=fs::read_to_string(path) { for line in text.lines().skip(offset).take(limit) { if let Ok(value)=serde_json::from_str::<Value>(line) { events.push(value); } } } Ok(json!({"schema":"narada.delegated_task.events.v1","status":"ok","task_id":id,"offset":offset,"limit":limit,"count":events.len(),"events":events,"native_read_only":true})) }

fn id_schema(required: bool) -> Value { json!({"type":"object","properties":{"task_id":{"type":"string"},"refresh":{"type":"boolean","default":false}},"required":if required {json!(["task_id"])} else {json!([])},"additionalProperties":false}) }
fn authority_boundary(name: &str) -> Value { json!({"schema":"narada.delegated_task.authority_boundary.v1","status":"unavailable","tool_name":name,"reason":"delegated_task_worker_authority_not_enabled_in_native_read_slice","remediation":"Use the configured delegated-task/worker authority for creation, execution, waiting, cancellation, acknowledgement, and takeover."}) }
fn error(code: &str, message: &str) -> Value { json!({"schema":"narada.delegated_task.error.v1","code":code,"message":message}) }
fn tool(name: &str, description: &str, schema: Value, read_only: bool) -> Value { json!({"name":name,"description":description,"annotations":{"title":name,"readOnlyHint":read_only,"destructiveHint":!read_only,"idempotentHint":read_only,"openWorldHint":false},"inputSchema":schema,"outputSchema":{"type":"object","additionalProperties":true}}) }

#[cfg(test)]
mod tests { use super::*; #[test] fn native_delegated_task_reads_durable_json_without_execution() { let root=std::env::temp_dir().join(format!("narada-delegated-task-{}",uuid::Uuid::new_v4())); fs::create_dir_all(root.join("tasks/task_a")).expect("root"); fs::write(root.join("tasks/task_a/task.json"),r#"{"task_id":"task_a","status":"completed","objective":"demo","updated_at":"2026-01-01T00:00:00Z","result":{"acceptance_verdict":"accepted"}}"#).expect("task"); let listed=tasks_list(&json!({"limit":1}).as_object().unwrap(),&root).expect("list"); assert_eq!(listed["count"],1); assert_eq!(task_status(&json!({"task_id":"task_a"}).as_object().unwrap(),&root).expect("status")["task_status"],"completed"); fs::remove_dir_all(root).expect("cleanup"); } }
