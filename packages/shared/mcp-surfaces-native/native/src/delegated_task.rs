use serde_json::{json, Map, Value};
use std::fs;
use std::path::{Path, PathBuf};

const SERVER_NAME: &str = "delegated-task-mcp";
const MAX_ITEMS: usize = 200;
const MAX_FILE_BYTES: u64 = 256_000;
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
    if let Some(value) = std::env::var_os("NARADA_DELEGATED_TASK_ROOT") {
        return PathBuf::from(value);
    }
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
fn read_task(root: &Path, id: &str) -> Result<Value, Value> { let path = task_path(root,id)?; let size = fs::metadata(&path).map_err(|_|error("delegated_task_not_found","delegated_task_not_found"))?.len(); if size > MAX_FILE_BYTES { return Err(error("delegated_task_record_too_large","delegated_task_record_too_large")); } let text = fs::read_to_string(&path).map_err(|_|error("delegated_task_not_found","delegated_task_not_found"))?; serde_json::from_str(&text).map_err(|e|error("delegated_task_invalid_json",&e.to_string())) }
fn task_id(args: &Map<String, Value>) -> Result<String, Value> { args.get("task_id").and_then(Value::as_str).filter(|v|!v.trim().is_empty()).map(|v|v.trim().to_string()).ok_or_else(||error("task_id_required","task_id_required")) }

fn policy(root: &Path) -> Value { json!({"schema":"narada.delegated_task.policy.v1","status":"ok","server_name":SERVER_NAME,"task_root":task_root(root).to_string_lossy(),"site_root":root.to_string_lossy(),"allowed_roots":[root.to_string_lossy()],"list_defaults":{"view":"active_queue","site_scope":"current_site"},"workflow_engine":"native_read_slice","worker_execution":"authority_boundary","result_compaction":{"max_worker_refs":50,"max_list_items":200}}) }

fn assessment_output_schema() -> Value {
    json!({"schema":"narada.delegated_task.output_schema.v1","name":"task_executability_assessment_v1","version":1,"required":["dimensions","first_actions","reference_resolutions","acceptance_mappings","required_decisions","findings","evaluator_provenance"],"fields":{"dimensions":"array<object>","first_actions":"array<object>","reference_resolutions":"array<object>","acceptance_mappings":"array<object>","required_decisions":"array<object>","findings":"array<object>","evaluator_provenance":"object"},"provenance_required":["runtime","provider","model","cognition","profile_version"],"rejection_rules":["missing_required_field","prose_only","invalid_schema","invalid_provenance"]})
}

fn assessment_template() -> Value {
    let output_schema = assessment_output_schema();
    json!({"template_id":"task_executability_assessment_v1","strategy":"task_executability_assessment_v1","title":"Bounded Shoshin task executability assessment","profile_version":"shoshin-task-executability-v1","purpose":"Assess one canonical task snapshot without changing it.","idempotency":{"schema":"narada.task.executability.idempotency.v1","inputs":["request_id","task_digest","environment_digest","profile_version"],"formula":"sha256(canonical_json({request_id, task_digest, environment_digest, profile_version}))"},"bounds":{"authority":"read","cognition":"low","runtime":"narada-agent-runtime-server","max_worker_runs":1,"max_run_ms":120000,"max_retries":0,"max_result_items":32,"max_events":32,"write_set":[]},"result_policy":{"expose_worker_refs":true,"compact_completed_worker_refs":true,"max_events":32,"max_worker_refs":1,"max_result_items":32},"output_schema":output_schema,"milestones":[{"id":"assessment","title":"Assess canonical task snapshot","step_ids":["assessment"]}],"steps":[{"id":"assessment","kind":"worker","profile":"shoshin-task-executability-v1","milestone_id":"assessment","write_set":[],"constraints":{"authority":"read","cognition":"low","runtime":"narada-agent-runtime-server","max_run_ms":120000,"max_retries":0,"max_concurrency":1,"wait_for_completion":false,"resumable":false,"required_mcp_tools":[],"preflight_paths":[],"overrides":{"skip_git_repo_check":true}},"output_schema":output_schema}],"worker_delegation_contract":{"surface_id":"worker-delegation","caller_sets_worker_constraints":true,"worker_run_is_child_execution":true,"required_worker_output_fields":["summary","structured_outputs","verification","target_state_changed"],"forbidden_authorities":["write","command"],"required_structured_output":"task_executability_assessment_v1"}})
}

fn worker_contract(step_kinds: &[&str]) -> Value {
    json!({"surface_id":"worker-delegation","routed_feedback_ids":["sfb_7e043d77-074"],"caller_sets_worker_constraints":true,"worker_run_is_child_execution":true,"required_worker_output_fields":["summary","changes","verification","residual_risks","observed_incoherencies"],"step_kinds":step_kinds})
}

fn authority_gates(commit_reason: &str, push_reason: &str) -> Value {
    json!({"commit":{"operation":"commit","mode":"requires_explicit_authority","reason":commit_reason,"required_authority":"write"},"push":{"operation":"push","mode":"requires_explicit_authority","reason":push_reason,"required_authority":"command"}})
}

fn workflow_templates() -> Vec<Value> {
    vec![
        assessment_template(),
        json!({"template_id":"implement","strategy":"implement","title":"Single implementation worker","feedback_ids":["sfb_f1ea42cb-062","sfb_ac8a8731-f1c"],"milestones":[{"id":"implement","title":"Implement","step_ids":["implement"]}],"steps":[{"id":"implement","kind":"worker","milestone_id":"implement"}],"worker_delegation_contract":worker_contract(&["worker"])}),
        json!({"template_id":"implement_review","strategy":"implement_review","title":"Implementation with review quorum evidence","feedback_ids":["sfb_f1ea42cb-062","sfb_ac8a8731-f1c","sfb_7e043d77-074"],"milestones":[{"id":"implement","title":"Implement","step_ids":["implement"]},{"id":"review","title":"Review","depends_on":["implement"],"step_ids":["review"]}],"steps":[{"id":"implement","kind":"worker","milestone_id":"implement"},{"id":"review","kind":"review","milestone_id":"review","depends_on":["implement"]}],"worker_delegation_contract":worker_contract(&["worker","review"])}),
        json!({"template_id":"research_synthesize","strategy":"research_synthesize","title":"Research, synthesize, and review","feedback_ids":["sfb_074b9629-4a8","sfb_f1ea42cb-062"],"milestones":[{"id":"research","title":"Research","step_ids":["research"]},{"id":"synthesize","title":"Synthesize","depends_on":["research"],"step_ids":["synthesize","review"]}],"steps":[{"id":"research","kind":"research","milestone_id":"research"},{"id":"synthesize","kind":"worker","milestone_id":"synthesize","depends_on":["research"]},{"id":"review","kind":"review","milestone_id":"synthesize","depends_on":["synthesize"]}],"worker_delegation_contract":worker_contract(&["research","worker","review"])}),
        json!({"template_id":"implement_review_repair_verify","strategy":"implement_review_repair_verify","title":"Implementation, review, conditional repair, and verify","feedback_ids":["sfb_6924c7b3-48f","sfb_074b9629-4a8","sfb_f1ea42cb-062","sfb_ac8a8731-f1c","sfb_7e043d77-074"],"milestones":[{"id":"implement","title":"Implement","step_ids":["implement"]},{"id":"review","title":"Review","depends_on":["implement"],"step_ids":["review"]},{"id":"repair","title":"Repair if needed","depends_on":["review"],"step_ids":["repair"]},{"id":"verify","title":"Verify","depends_on":["repair"],"step_ids":["verify"]}],"steps":[{"id":"implement","kind":"worker","milestone_id":"implement"},{"id":"review","kind":"review","milestone_id":"review","depends_on":["implement"]},{"id":"repair","kind":"repair","milestone_id":"repair","depends_on":["review"],"if":"review_failed"},{"id":"verify","kind":"verify","milestone_id":"verify","depends_on":["repair"]}],"authority_gates":authority_gates("commit is modeled as an explicit gate and is never executed by delegated-task-mcp","push must stay opt-in and owned by caller policy or worker constraints"),"worker_delegation_contract":worker_contract(&["worker","review","repair","verify"])}),
        json!({"template_id":"commit_push_guarded","strategy":"commit_push_guarded","title":"Review-gated commit and push publication handoff","feedback_ids":["sfb_98a64342-379","sfb_7e043d77-074"],"milestones":[{"id":"prepare","title":"Prepare evidence","step_ids":["prepare"]},{"id":"review","title":"Review publication readiness","depends_on":["prepare"],"step_ids":["review"]},{"id":"publication-gate","title":"Publication authority gate","depends_on":["review"],"step_ids":["commit-gate","push-gate"]}],"authority_gates":authority_gates("commit only after explicit caller authority","push only after explicit command authority"),"steps":[{"id":"prepare","kind":"worker","milestone_id":"prepare"},{"id":"review","kind":"review","milestone_id":"review","depends_on":["prepare"]},{"id":"commit-gate","kind":"gate","milestone_id":"publication-gate","depends_on":["review"],"if":"all(step:review:completed,no_residual_risks)","authority_gate":{"operation":"commit","mode":"requires_explicit_authority","required_authority":"write"}},{"id":"push-gate","kind":"gate","milestone_id":"publication-gate","depends_on":["commit-gate"],"if":"acceptance:passed","authority_gate":{"operation":"push","mode":"requires_explicit_authority","required_authority":"command"}}],"worker_delegation_contract":worker_contract(&["worker","review"])})
    ]
}

fn template_catalog(args: &Map<String, Value>) -> Value {
    let id = args.get("template_id").and_then(Value::as_str).map(str::trim).filter(|value| !value.is_empty());
    let templates = workflow_templates().into_iter().filter(|value| id.is_none() || value.get("template_id").and_then(Value::as_str) == id).collect::<Vec<_>>();
    json!({"schema":"narada.delegated_task.template_catalog.v1","status":if id.is_some() && templates.is_empty(){"not_found"}else{"ok"},"template_id":id,"count":templates.len(),"templates":templates})
}

fn validate(args: &Map<String, Value>, root: &Path) -> Result<Value, Value> { let objective = args.get("objective").and_then(Value::as_str).map(str::trim).filter(|v|!v.is_empty()); let mut errors = Vec::new(); if objective.is_none(){errors.push("objective_required");} if let Some(binding)=args.get("execution_binding").and_then(Value::as_object) { if let Some(workspace)=binding.get("workspace_root").and_then(Value::as_str) { if !is_within(Path::new(workspace), root) { errors.push("execution_binding_workspace_outside_site_root"); } } } Ok(json!({"schema":"narada.delegated_task.validate.v1","status":if errors.is_empty(){"ok"}else{"invalid"},"valid":errors.is_empty(),"task_root":task_root(root).to_string_lossy(),"errors":errors,"objective":objective,"worker_execution":"not_run"})) }
fn is_within(path: &Path, root: &Path) -> bool { let p=path.canonicalize().unwrap_or_else(|_|path.to_path_buf()); let r=root.canonicalize().unwrap_or_else(|_|root.to_path_buf()); p==r || p.starts_with(&r) }

fn tasks_list(args: &Map<String, Value>, root: &Path) -> Result<Value, Value> { let limit=args.get("limit").and_then(Value::as_u64).unwrap_or(20).clamp(1,MAX_ITEMS as u64) as usize; let mut tasks=Vec::new(); if let Ok(entries)=fs::read_dir(tasks_dir(root)) { for entry in entries.filter_map(Result::ok).take(MAX_ITEMS) { if !entry.path().is_dir(){continue;} let Some(id)=entry.file_name().to_str().map(ToOwned::to_owned) else {continue;}; if let Ok(task)=read_task(root,&id) { tasks.push(compact_task(&task)); } } } tasks.sort_by(|a,b| b.get("updated_at").and_then(Value::as_str).cmp(&a.get("updated_at").and_then(Value::as_str))); tasks.truncate(limit); Ok(json!({"schema":"narada.delegated_task.list.v1","status":"ok","view":args.get("view").and_then(Value::as_str).unwrap_or("active_queue"),"site_scope":args.get("site_scope").and_then(Value::as_str).unwrap_or("current_site"),"count":tasks.len(),"limit":limit,"tasks":tasks,"native_read_only":true})) }
fn compact_task(task: &Value) -> Value { let obj=task.as_object().cloned().unwrap_or_default(); json!({"task_id":obj.get("task_id"),"task_status":obj.get("status"),"objective":obj.get("objective"),"owner_site_id":obj.get("owner_site_id"),"created_by_site_id":obj.get("created_by_site_id"),"visibility_scope":obj.get("visibility_scope"),"updated_at":obj.get("updated_at"),"summary":obj.get("summary"),"execution_binding":obj.get("execution_binding")}) }
fn task_status(args: &Map<String, Value>, root: &Path) -> Result<Value, Value> { let id=task_id(args)?; let task=read_task(root,&id)?; let obj=task.as_object().cloned().unwrap_or_default(); Ok(json!({"schema":"narada.delegated_task.status.v1","status":"ok","task_id":id,"task_status":obj.get("status"),"objective":obj.get("objective"),"ownership":{"owner_site_id":obj.get("owner_site_id"),"owner_site_root":obj.get("owner_site_root"),"visibility_scope":obj.get("visibility_scope")},"execution_binding":obj.get("execution_binding"),"request_fingerprint":obj.get("request_fingerprint"),"created_at":obj.get("created_at"),"updated_at":obj.get("updated_at"),"result":obj.get("result"),"native_read_only":true})) }
fn task_result(args: &Map<String, Value>, root: &Path) -> Result<Value, Value> { let id=task_id(args)?; let task=read_task(root,&id)?; Ok(json!({"schema":"narada.delegated_task.result.v1","status":"ok","task_id":id,"task_status":task.get("status"),"result":task.get("result"),"summary":task.get("summary"),"native_read_only":true})) }
fn task_summary(args: &Map<String, Value>, root: &Path) -> Result<Value, Value> { let id=task_id(args)?; let task=read_task(root,&id)?; let result=task.get("result").and_then(Value::as_object).cloned().unwrap_or_default(); Ok(json!({"schema":"narada.delegated_task.summary.v1","status":"ok","task_id":id,"task_status":task.get("status"),"objective":task.get("objective"),"summary":task.get("summary"),"acceptance_verdict":result.get("acceptance_verdict").cloned().unwrap_or(Value::String("pending".into())),"residual_risks":result.get("residual_risks").cloned().unwrap_or_else(||json!([])),"progress":result.get("progress"),"native_read_only":true})) }
fn task_wait(args: &Map<String, Value>, root: &Path) -> Result<Value, Value> { let id=task_id(args)?; let task=read_task(root,&id)?; Ok(json!({"schema":"narada.delegated_task.wait.v1","status":"ok","task_id":id,"task_status":task.get("status"),"waited":false,"refresh_performed":false,"worker_execution":"authority_boundary","task":compact_task(&task),"native_read_only":true})) }
fn task_events(args: &Map<String, Value>, root: &Path) -> Result<Value, Value> { let id=task_id(args)?; let path=task_path(root,&id)?.with_file_name("events.jsonl"); let limit=args.get("limit").and_then(Value::as_u64).unwrap_or(50).clamp(1,100) as usize; let offset=args.get("offset").and_then(Value::as_u64).unwrap_or(0).min(10000) as usize; let mut events=Vec::new(); if let Ok(metadata)=fs::metadata(&path) { if metadata.len() > MAX_FILE_BYTES { return Err(error("delegated_task_events_too_large","delegated_task_events_too_large")); } let text=fs::read_to_string(path).map_err(|_|error("delegated_task_events_read_failed","delegated_task_events_read_failed"))?; for line in text.lines().skip(offset).take(limit) { if let Ok(value)=serde_json::from_str::<Value>(line) { events.push(value); } } } Ok(json!({"schema":"narada.delegated_task.events.v1","status":"ok","task_id":id,"offset":offset,"limit":limit,"count":events.len(),"events":events,"native_read_only":true})) }

fn id_schema(required: bool) -> Value { json!({"type":"object","properties":{"task_id":{"type":"string"},"refresh":{"type":"boolean","default":false}},"required":if required {json!(["task_id"])} else {json!([])},"additionalProperties":false}) }
fn authority_boundary(name: &str) -> Value { json!({"schema":"narada.delegated_task.authority_boundary.v1","status":"unavailable","tool_name":name,"reason":"delegated_task_worker_authority_not_enabled_in_native_read_slice","remediation":"Use the configured delegated-task/worker authority for creation, execution, waiting, cancellation, acknowledgement, and takeover."}) }
fn error(code: &str, message: &str) -> Value { json!({"schema":"narada.delegated_task.error.v1","code":code,"message":message}) }
fn tool(name: &str, description: &str, schema: Value, read_only: bool) -> Value { json!({"name":name,"description":description,"annotations":{"title":name,"readOnlyHint":read_only,"destructiveHint":!read_only,"idempotentHint":read_only,"openWorldHint":false},"inputSchema":schema,"outputSchema":{"type":"object","additionalProperties":true}}) }

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_delegated_task_reads_durable_json_without_execution() {
        let root = std::env::temp_dir().join(format!("narada-delegated-task-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(root.join("tasks/task_a")).expect("root");
        fs::write(root.join("tasks/task_a/task.json"), r#"{"task_id":"task_a","status":"completed","objective":"demo","updated_at":"2026-01-01T00:00:00Z","result":{"acceptance_verdict":"accepted"}}"#).expect("task");
        let listed = tasks_list(&json!({"limit":1}).as_object().unwrap(), &root).expect("list");
        assert_eq!(listed["count"], 1);
        assert_eq!(task_status(&json!({"task_id":"task_a"}).as_object().unwrap(), &root).expect("status")["task_status"], "completed");
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn native_delegated_task_refuses_oversized_records() {
        let root = std::env::temp_dir().join(format!("narada-delegated-task-large-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(root.join("tasks/task_a")).expect("root");
        fs::write(root.join("tasks/task_a/task.json"), vec![b'x'; MAX_FILE_BYTES as usize + 1]).expect("task");
        let error = task_status(&json!({"task_id":"task_a"}).as_object().unwrap(), &root).expect_err("oversized record must refuse");
        assert_eq!(error["code"], "delegated_task_record_too_large");
        fs::remove_dir_all(root).expect("cleanup");
    }
}
