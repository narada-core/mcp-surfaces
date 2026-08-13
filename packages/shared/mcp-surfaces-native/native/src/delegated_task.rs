use fs2::FileExt;
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

const SERVER_NAME: &str = "delegated-task-mcp";
const MAX_ITEMS: usize = 200;
const MAX_FILE_BYTES: u64 = 256_000;
const MUTATING: &[&str] = &[
    "delegated_task_run",
    "delegated_task_advance",
    "delegated_task_cancel",
    "delegated_task_acknowledge",
    "delegated_task_parent_takeover",
];

pub fn list_tools() -> Vec<Value> {
    let mut tools = vec![guidance_tool()];
    for (name, desc, schema) in [
        (
            "delegated_task_policy_inspect",
            "Inspect delegated task orchestration policy and defaults.",
            json!({"type":"object","properties":{},"additionalProperties":false}),
        ),
        (
            "delegated_task_template_catalog",
            "List built-in delegated workflow templates and contracts.",
            json!({"type":"object","properties":{"template_id":{"type":"string"}},"additionalProperties":false}),
        ),
        (
            "delegated_task_validate",
            "Validate delegated task input without creating or running a task.",
            json!({"type":"object","properties":{"objective":{"type":"string"},"workflow":{"type":"object"},"constraints":{"type":"object"},"acceptance":{"type":"object"},"execution":{"type":"object"},"execution_binding":{"type":"object"}},"additionalProperties":false}),
        ),
        (
            "delegated_task_status",
            "Return compact durable delegated task status.",
            id_schema(true),
        ),
        (
            "delegated_tasks_list",
            "List bounded delegated tasks by lifecycle and site scope.",
            json!({"type":"object","properties":{"limit":{"type":"integer","minimum":1,"maximum":200,"default":20},"view":{"type":"string"},"site_scope":{"type":"string"}},"additionalProperties":false}),
        ),
        (
            "delegated_task_result",
            "Return a delegated task result handoff from durable state.",
            id_schema(true),
        ),
        (
            "delegated_task_summary",
            "Return a compact human review summary from durable state.",
            id_schema(true),
        ),
        (
            "delegated_task_events",
            "List bounded delegated task events.",
            json!({"type":"object","properties":{"task_id":{"type":"string"},"limit":{"type":"integer","minimum":1,"maximum":100,"default":50},"offset":{"type":"integer","minimum":0,"maximum":10000}},"required":["task_id"],"additionalProperties":false}),
        ),
    ] {
        tools.push(tool(name, desc, schema, true));
    }
    for name in MUTATING {
        tools.push(tool(
            name,
            "Delegated task mutation remains owned by the worker/task authority.",
            json!({"type":"object","additionalProperties":true}),
            false,
        ));
    }
    tools.push(tool("delegated_task_wait", "Wait for a delegated task to advance toward terminal status; native mode only reports durable state.", id_schema(true), true));
    tools
}

pub fn auxiliary(method: &str, params: &Map<String, Value>) -> Result<Value, Value> {
    match method {
        "prompts/list" => Ok(
            json!({"prompts":[{"name":"delegated_task_workflow","title":"Delegated Task Workflow","description":"Validate and inspect durable delegated tasks before execution or disposition.","arguments":[]}]}),
        ),
        "prompts/get" => {
            if params.get("name").and_then(Value::as_str) != Some("delegated_task_workflow") {
                return Err(error("unknown_prompt", "unknown_prompt"));
            }
            Ok(
                json!({"description":"Validate and inspect durable delegated tasks before execution or disposition.","messages":[{"role":"user","content":{"type":"text","text":"Use delegated_task_validate before creation, delegated_tasks_list/status/result/events for readback, and keep worker execution with the owning worker authority."}}]}),
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
        "delegated_task_run" => task_run(args, root),
        "delegated_task_advance" => task_advance(args, root),
        "delegated_task_cancel" => task_cancel(args, root, false),
        "delegated_task_parent_takeover" => task_cancel(args, root, true),
        "delegated_task_acknowledge" => task_acknowledge(args, root),
        _ => Err(error("unknown_tool", &format!("unknown_tool:{name}"))),
    }
}

fn guidance_tool() -> Value {
    tool(
        "delegated_task_guidance",
        "Show model-facing operating guidance for delegated task workflows.",
        json!({"type":"object","properties":{"workflow":{"type":"string"},"tool":{"type":"string"}},"additionalProperties":false}),
        true,
    )
}
fn guidance(args: &Map<String, Value>) -> Value {
    json!({"schema":"narada.mcp_surface.guidance.v0","status":"ok","surface_id":"delegated-task","guidance_tool":"delegated_task_guidance","purpose":"Validate, execute, and inspect durable delegated task workflows through native authority.","requested":{"workflow":args.get("workflow").cloned().unwrap_or(Value::Null),"tool":args.get("tool").cloned().unwrap_or(Value::Null)},"first_use":["Call delegated_task_policy_inspect first.","Validate an input before creating a task.","Use bounded list/status/result/events readback.","Use explicit cancellation and disposition tools for lifecycle mutations."],"boundaries":["Native authority owns task.json/events.jsonl under the bounded task root.","Worker launches cross the native worker-delegation authority boundary.","Cross-site ownership remains server-bound authority."]})
}

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
fn tasks_dir(root: &Path) -> PathBuf {
    task_root(root).join("tasks")
}
fn current_site_id(root: &Path) -> Option<String> {
    for key in ["NARADA_SITE_ID", "SITE_ID", "NARADA_SITE"] {
        if let Ok(value) = std::env::var(key) {
            if !value.trim().is_empty() {
                return Some(value);
            }
        }
    }
    for path in [root.join(".narada/site.json"), root.join(".ai/site.json")] {
        if let Ok(text) = fs::read_to_string(path) {
            if let Ok(value) = serde_json::from_str::<Value>(&text) {
                if let Some(id) = value
                    .get("site_id")
                    .or_else(|| value.get("id"))
                    .and_then(Value::as_str)
                    .filter(|id| !id.trim().is_empty())
                {
                    return Some(id.to_string());
                }
            }
        }
    }
    root.file_name()
        .and_then(|name| name.to_str())
        .filter(|name| *name != ".")
        .map(str::to_string)
}
fn ownership(task: &Value) -> Value {
    let owner = task.get("owner_site_id").and_then(Value::as_str);
    let has = owner.is_some() || task.get("visibility_scope").is_some();
    if !has {
        return json!({"owner_site_id":"unknown","owner_site_root":null,"created_by_site_id":"unknown","visibility_scope":"user_global_legacy","task_root_scope":"unknown","ownership_resolution":"legacy_missing_metadata"});
    }
    json!({"owner_site_id":owner.unwrap_or("unknown"),"owner_site_root":task.get("owner_site_root"),"created_by_site_id":task.get("created_by_site_id").and_then(Value::as_str).unwrap_or("unknown"),"visibility_scope":task.get("visibility_scope").and_then(Value::as_str).unwrap_or(if owner.is_some(){"site"}else{"user_global"}),"task_root_scope":task.get("task_root_scope").and_then(Value::as_str).unwrap_or("unknown"),"ownership_resolution":if owner.is_some(){"explicit"}else{"unknown_owner"}})
}
fn assert_mutation_scope(
    task: &Value,
    args: &Map<String, Value>,
    root: &Path,
) -> Result<Value, Value> {
    let projected = ownership(task);
    let owner = projected
        .get("owner_site_id")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    if let Some(expected) = args.get("expected_owner_site_id").and_then(Value::as_str) {
        if expected != owner {
            return Err(error(
                "delegated_task_owner_site_mismatch",
                "delegated_task_owner_site_mismatch",
            ));
        }
    }
    let current = current_site_id(root);
    let cross = current.as_deref().is_some_and(|site| site != owner);
    let legacy = owner == "unknown"
        || projected.get("visibility_scope").and_then(Value::as_str) == Some("user_global_legacy");
    if (cross || legacy) && args.get("allow_cross_site").and_then(Value::as_bool) != Some(true) {
        return Err(error(
            "delegated_task_cross_site_mutation_denied",
            "delegated_task_cross_site_mutation_denied",
        ));
    }
    Ok(projected)
}
fn safe_id(id: &str) -> Result<String, Value> {
    if id.is_empty()
        || id.len() > 120
        || !id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Err(error(
            "delegated_task_id_invalid",
            "delegated_task_id_invalid",
        ));
    }
    Ok(id.to_string())
}
fn task_path(root: &Path, id: &str) -> Result<PathBuf, Value> {
    let id = safe_id(id)?;
    Ok(tasks_dir(root).join(id).join("task.json"))
}
struct TaskLock(std::fs::File);
impl Drop for TaskLock {
    fn drop(&mut self) {
        let _ = self.0.unlock();
    }
}
fn lock_task(root: &Path, id: &str) -> Result<TaskLock, Value> {
    let path = task_path(root, id)?.with_file_name("mutation.lock");
    fs::create_dir_all(path.parent().unwrap())
        .map_err(|_| error("delegated_task_lock_failed", "delegated_task_lock_failed"))?;
    let file = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .open(path)
        .map_err(|_| error("delegated_task_lock_failed", "delegated_task_lock_failed"))?;
    file.lock_exclusive()
        .map_err(|_| error("delegated_task_lock_failed", "delegated_task_lock_failed"))?;
    Ok(TaskLock(file))
}
fn read_task(root: &Path, id: &str) -> Result<Value, Value> {
    let path = task_path(root, id)?;
    let size = fs::metadata(&path)
        .map_err(|_| error("delegated_task_not_found", "delegated_task_not_found"))?
        .len();
    if size > MAX_FILE_BYTES {
        return Err(error(
            "delegated_task_record_too_large",
            "delegated_task_record_too_large",
        ));
    }
    let text = fs::read_to_string(&path)
        .map_err(|_| error("delegated_task_not_found", "delegated_task_not_found"))?;
    serde_json::from_str(&text).map_err(|e| error("delegated_task_invalid_json", &e.to_string()))
}
fn task_id(args: &Map<String, Value>) -> Result<String, Value> {
    args.get("task_id")
        .and_then(Value::as_str)
        .filter(|v| !v.trim().is_empty())
        .map(|v| v.trim().to_string())
        .ok_or_else(|| error("task_id_required", "task_id_required"))
}

fn policy(root: &Path) -> Value {
    json!({"schema":"narada.delegated_task.policy.v1","status":"ok","server_name":SERVER_NAME,"task_root":task_root(root).to_string_lossy(),"site_root":root.to_string_lossy(),"allowed_roots":[root.to_string_lossy()],"list_defaults":{"view":"active_queue","site_scope":"current_site"},"workflow_engine":"native_authority","worker_execution":"native_worker_authority","result_compaction":{"max_worker_refs":50,"max_list_items":200}})
}

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
        json!({"template_id":"commit_push_guarded","strategy":"commit_push_guarded","title":"Review-gated commit and push publication handoff","feedback_ids":["sfb_98a64342-379","sfb_7e043d77-074"],"milestones":[{"id":"prepare","title":"Prepare evidence","step_ids":["prepare"]},{"id":"review","title":"Review publication readiness","depends_on":["prepare"],"step_ids":["review"]},{"id":"publication-gate","title":"Publication authority gate","depends_on":["review"],"step_ids":["commit-gate","push-gate"]}],"authority_gates":authority_gates("commit only after explicit caller authority","push only after explicit command authority"),"steps":[{"id":"prepare","kind":"worker","milestone_id":"prepare"},{"id":"review","kind":"review","milestone_id":"review","depends_on":["prepare"]},{"id":"commit-gate","kind":"gate","milestone_id":"publication-gate","depends_on":["review"],"if":"all(step:review:completed,no_residual_risks)","authority_gate":{"operation":"commit","mode":"requires_explicit_authority","required_authority":"write"}},{"id":"push-gate","kind":"gate","milestone_id":"publication-gate","depends_on":["commit-gate"],"if":"acceptance:passed","authority_gate":{"operation":"push","mode":"requires_explicit_authority","required_authority":"command"}}],"worker_delegation_contract":worker_contract(&["worker","review"])}),
    ]
}

fn template_catalog(args: &Map<String, Value>) -> Value {
    let id = args
        .get("template_id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let templates = workflow_templates()
        .into_iter()
        .filter(|value| id.is_none() || value.get("template_id").and_then(Value::as_str) == id)
        .collect::<Vec<_>>();
    json!({"schema":"narada.delegated_task.template_catalog.v1","status":if id.is_some() && templates.is_empty(){"not_found"}else{"ok"},"template_id":id,"count":templates.len(),"templates":templates})
}

fn workflow_diagnostics(workflow: &Value) -> Vec<Value> {
    let steps = workflow
        .get("steps")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut diagnostics = Vec::new();
    let mut ids = std::collections::HashSet::new();
    for step in &steps {
        let Some(id) = step.get("id").and_then(Value::as_str) else {
            diagnostics.push(json!({"severity":"error","code":"step_id_required"}));
            continue;
        };
        if !ids.insert(id.to_string()) {
            diagnostics.push(json!({"severity":"error","code":"duplicate_step_id","step_id":id}));
        }
    }
    for step in &steps {
        let Some(id) = step.get("id").and_then(Value::as_str) else {
            continue;
        };
        for dependency in step
            .get("depends_on")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
        {
            if !ids.contains(dependency) {
                diagnostics.push(json!({"severity":"error","code":"unknown_dependency","step_id":id,"dependency":dependency}));
            }
        }
    }
    let mut resolved = std::collections::HashSet::new();
    loop {
        let before = resolved.len();
        for step in &steps {
            let Some(id) = step.get("id").and_then(Value::as_str) else {
                continue;
            };
            if resolved.contains(id) {
                continue;
            }
            let ready = step
                .get("depends_on")
                .and_then(Value::as_array)
                .map(|items| {
                    items
                        .iter()
                        .filter_map(Value::as_str)
                        .all(|dependency| resolved.contains(dependency))
                })
                .unwrap_or(true);
            if ready {
                resolved.insert(id.to_string());
            }
        }
        if resolved.len() == before {
            break;
        }
    }
    if resolved.len() < ids.len() {
        diagnostics.push(json!({"severity":"error","code":"workflow_cycle"}));
    }
    diagnostics
}
fn validate(args: &Map<String, Value>, root: &Path) -> Result<Value, Value> {
    let objective = args
        .get("objective")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|v| !v.is_empty());
    let workflow = normalize_workflow(args.get("workflow"));
    let mut diagnostics = workflow_diagnostics(&workflow);
    if objective.is_none() {
        diagnostics.push(json!({"severity":"error","code":"objective_required"}));
    }
    if let Some(binding) = args.get("execution_binding").and_then(Value::as_object) {
        if let Some(workspace) = binding.get("workspace_root").and_then(Value::as_str) {
            if !is_within(Path::new(workspace), root) {
                diagnostics.push(json!({"severity":"error","code":"execution_binding_workspace_outside_site_root"}));
            }
        }
    }
    let errors = diagnostics
        .iter()
        .filter_map(|item| item.get("code").and_then(Value::as_str))
        .collect::<Vec<_>>();
    Ok(
        json!({"schema":"narada.delegated_task.validate.v1","status":if diagnostics.is_empty(){"ok"}else{"rejected"},"dry_run":true,"diagnostics":diagnostics,"valid":errors.is_empty(),"task_root":task_root(root).to_string_lossy(),"errors":errors,"objective":objective,"worker_execution":"not_run"}),
    )
}
fn is_within(path: &Path, root: &Path) -> bool {
    let p = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    let r = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    p == r || p.starts_with(&r)
}

fn tasks_list(args: &Map<String, Value>, root: &Path) -> Result<Value, Value> {
    let limit = args
        .get("limit")
        .and_then(Value::as_u64)
        .unwrap_or(20)
        .clamp(1, MAX_ITEMS as u64) as usize;
    let mut tasks = Vec::new();
    if let Ok(entries) = fs::read_dir(tasks_dir(root)) {
        for entry in entries.filter_map(Result::ok).take(MAX_ITEMS) {
            if !entry.path().is_dir() {
                continue;
            }
            let Some(id) = entry.file_name().to_str().map(ToOwned::to_owned) else {
                continue;
            };
            if let Ok(task) = read_task(root, &id) {
                tasks.push(compact_task(&task));
            }
        }
    }
    tasks.sort_by(|a, b| {
        b.get("updated_at")
            .and_then(Value::as_str)
            .cmp(&a.get("updated_at").and_then(Value::as_str))
    });
    tasks.truncate(limit);
    Ok(
        json!({"schema":"narada.delegated_task.list.v1","status":"ok","view":args.get("view").and_then(Value::as_str).unwrap_or("active_queue"),"site_scope":args.get("site_scope").and_then(Value::as_str).unwrap_or("current_site"),"count":tasks.len(),"limit":limit,"tasks":tasks}),
    )
}
fn compact_task(task: &Value) -> Value {
    let obj = task.as_object().cloned().unwrap_or_default();
    json!({"task_id":obj.get("task_id"),"task_status":obj.get("status"),"objective":obj.get("objective"),"owner_site_id":obj.get("owner_site_id"),"created_by_site_id":obj.get("created_by_site_id"),"visibility_scope":obj.get("visibility_scope"),"updated_at":obj.get("updated_at"),"summary":obj.get("summary"),"execution_binding":obj.get("execution_binding")})
}
fn task_status(args: &Map<String, Value>, root: &Path) -> Result<Value, Value> {
    let id = task_id(args)?;
    let task = read_task(root, &id)?;
    let obj = task.as_object().cloned().unwrap_or_default();
    Ok(
        json!({"schema":"narada.delegated_task.status.v1","status":"ok","task_id":id,"task_status":obj.get("status"),"objective":obj.get("objective"),"ownership":ownership(&task),"execution_binding":obj.get("execution_binding"),"request_fingerprint":obj.get("request_fingerprint"),"created_at":obj.get("created_at"),"updated_at":obj.get("updated_at"),"result":obj.get("result")}),
    )
}
fn task_result(args: &Map<String, Value>, root: &Path) -> Result<Value, Value> {
    let id = task_id(args)?;
    let task = read_task(root, &id)?;
    Ok(
        json!({"schema":"narada.delegated_task.result.v1","status":"ok","task_id":id,"task_status":task.get("status"),"result":task.get("result"),"summary":task.get("summary")}),
    )
}
fn task_summary(args: &Map<String, Value>, root: &Path) -> Result<Value, Value> {
    let id = task_id(args)?;
    let task = read_task(root, &id)?;
    let result = task
        .get("result")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    Ok(
        json!({"schema":"narada.delegated_task.summary.v1","status":"ok","task_id":id,"task_status":task.get("status"),"objective":task.get("objective"),"summary":task.get("summary"),"acceptance_verdict":result.get("acceptance_verdict").cloned().unwrap_or(Value::String("pending".into())),"residual_risks":result.get("residual_risks").cloned().unwrap_or_else(||json!([])),"progress":result.get("progress")}),
    )
}
fn task_wait(args: &Map<String, Value>, root: &Path) -> Result<Value, Value> {
    let id = task_id(args)?;
    let task = read_task(root, &id)?;
    Ok(
        json!({"schema":"narada.delegated_task.wait.v1","status":"ok","task_id":id,"task_status":task.get("status"),"waited":false,"refresh_performed":false,"worker_execution":"native_worker_authority","task":compact_task(&task)}),
    )
}
fn task_events(args: &Map<String, Value>, root: &Path) -> Result<Value, Value> {
    let id = task_id(args)?;
    let path = task_path(root, &id)?.with_file_name("events.jsonl");
    let limit = args
        .get("limit")
        .and_then(Value::as_u64)
        .unwrap_or(50)
        .clamp(1, 100) as usize;
    let offset = args
        .get("offset")
        .and_then(Value::as_u64)
        .unwrap_or(0)
        .min(10000) as usize;
    let mut events = Vec::new();
    if let Ok(metadata) = fs::metadata(&path) {
        if metadata.len() > MAX_FILE_BYTES {
            return Err(error(
                "delegated_task_events_too_large",
                "delegated_task_events_too_large",
            ));
        }
        let text = fs::read_to_string(path).map_err(|_| {
            error(
                "delegated_task_events_read_failed",
                "delegated_task_events_read_failed",
            )
        })?;
        for line in text.lines().skip(offset).take(limit) {
            if let Ok(value) = serde_json::from_str::<Value>(line) {
                events.push(value);
            }
        }
    }
    Ok(
        json!({"schema":"narada.delegated_task.events.v1","status":"ok","task_id":id,"offset":offset,"limit":limit,"count":events.len(),"events":events}),
    )
}

fn now() -> String {
    time::OffsetDateTime::now_utc()
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_default()
}
fn write_task(root: &Path, task: &Value) -> Result<(), Value> {
    let id = task
        .get("task_id")
        .and_then(Value::as_str)
        .ok_or_else(|| error("task_id_required", "task_id_required"))?;
    let path = task_path(root, id)?;
    fs::create_dir_all(path.parent().unwrap())
        .map_err(|_| error("delegated_task_write_failed", "delegated_task_write_failed"))?;
    let temp = path.with_extension(format!("tmp-{}", uuid::Uuid::new_v4()));
    fs::write(
        &temp,
        serde_json::to_vec_pretty(task)
            .map_err(|_| error("delegated_task_write_failed", "delegated_task_write_failed"))?,
    )
    .map_err(|_| error("delegated_task_write_failed", "delegated_task_write_failed"))?;
    fs::rename(temp, path)
        .map_err(|_| error("delegated_task_write_failed", "delegated_task_write_failed"))
}
fn append_event(root: &Path, id: &str, kind: &str, payload: Value) -> Result<Value, Value> {
    let event = json!({"schema":"narada.delegated_task.event.v1","event_id":format!("evt_{}",uuid::Uuid::new_v4().simple()),"task_id":id,"event_kind":kind,"recorded_at":now(),"details":payload});
    let path = task_path(root, id)?.with_file_name("events.jsonl");
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|_| {
            error(
                "delegated_task_event_write_failed",
                "delegated_task_event_write_failed",
            )
        })?;
    writeln!(file, "{}", event).map_err(|_| {
        error(
            "delegated_task_event_write_failed",
            "delegated_task_event_write_failed",
        )
    })?;
    Ok(event)
}
fn objective(args: &Map<String, Value>) -> Result<String, Value> {
    args.get("objective")
        .or_else(|| args.get("intent").and_then(|v| v.get("objective")))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(str::to_string)
        .ok_or_else(|| {
            error(
                "delegated_task_requires_objective",
                "delegated_task_requires_objective",
            )
        })
}
fn normalize_workflow(input: Option<&Value>) -> Value {
    let mut workflow = input
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    if !workflow.get("steps").is_some_and(Value::is_array) {
        let strategy = workflow
            .get("template_id")
            .or_else(|| workflow.get("strategy"))
            .or_else(|| workflow.get("template"))
            .and_then(Value::as_str);
        let template = strategy.and_then(|id| {
            workflow_templates()
                .into_iter()
                .find(|item| item.get("template_id").and_then(Value::as_str) == Some(id))
        });
        let steps = template
            .and_then(|item| item.get("steps").cloned())
            .unwrap_or_else(|| json!([{"id":"primary","kind":"worker"}]));
        workflow.insert("steps".into(), steps);
    }
    Value::Object(workflow)
}
fn initial_step_states(workflow: &Value) -> Value {
    let mut states = Map::new();
    for step in workflow
        .get("steps")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let Some(id) = step.get("id").and_then(Value::as_str) else {
            continue;
        };
        states.insert(id.to_string(),json!({"step_id":id,"kind":step.get("kind").and_then(Value::as_str).unwrap_or("worker"),"status":"pending","attempts":0,"run_ids":[],"current_run_id":null,"started_at":null,"finished_at":null,"error":null,"summary":null}));
    }
    Value::Object(states)
}
fn stable_task_id(args: &Map<String, Value>) -> String {
    if let Some(id) = args.get("task_id").and_then(Value::as_str) {
        return id.to_string();
    }
    if let Some(key) = args.get("idempotency_key").and_then(Value::as_str) {
        let digest = Sha256::digest(key.as_bytes());
        let prefix = digest[..8]
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        return format!("task_{prefix}");
    }
    format!("task_{}", uuid::Uuid::new_v4().simple())
}
fn task_run(args: &Map<String, Value>, root: &Path) -> Result<Value, Value> {
    let objective = objective(args)?;
    let admission = validate(args, root)?;
    if admission.get("status").and_then(Value::as_str) != Some("ok") {
        return Err(
            json!({"schema":"narada.delegated_task.error.v1","code":"delegated_task_validation_failed","message":"delegated_task_validation_failed","diagnostics":admission["diagnostics"]}),
        );
    }
    let id = stable_task_id(args);
    safe_id(&id)?;
    let _lock = lock_task(root, &id)?;
    if task_path(root, &id)?.is_file() {
        let task = read_task(root, &id)?;
        return Ok(
            json!({"schema":"narada.delegated_task.run.v1","status":"existing","request_status":"existing","execution_status":task["status"],"created":false,"task_id":id,"task_status":task["status"],"summary":task["summary"]}),
        );
    }
    let created = now();
    let workflow = normalize_workflow(args.get("workflow"));
    let step_states = initial_step_states(&workflow);
    let site = current_site_id(root);
    let mut task = json!({"schema":"narada.delegated_task.task.v1","task_id":id,"owner_site_id":site,"owner_site_root":if site.is_some(){json!(root.to_string_lossy())}else{Value::Null},"created_by_site_id":site,"visibility_scope":if site.is_some(){"site"}else{"user_global"},"task_root_scope":"site_root","status":"accepted_for_execution","objective":objective,"created_at":created,"updated_at":created,"cancelled_at":null,"idempotency_key":args.get("idempotency_key"),"constraints":args.get("constraints").cloned().unwrap_or_else(||json!({})),"workflow":workflow,"execution":args.get("execution").cloned().unwrap_or_else(||json!({"start":true})),"acceptance":args.get("acceptance").cloned().unwrap_or_else(||json!({})),"result":{"schema":"narada.delegated_task.handoff.v1","acceptance_verdict":"pending","step_states":step_states,"worker_refs":[],"residual_risks":[],"observed_incoherencies":[],"verification":[],"changed_files":[]},"summary":null});
    write_task(root, &task)?;
    append_event(root, &id, "task_created", json!({"objective":objective}))?;
    if task.pointer("/execution/start").and_then(Value::as_bool) != Some(false) {
        task = advance_value(task, root)?;
    }
    Ok(
        json!({"schema":"narada.delegated_task.run.v1","status":"accepted_for_execution","request_status":"accepted_for_execution","execution_status":task["status"],"created":true,"task_id":id,"task_status":task["status"],"summary":task["summary"]}),
    )
}
fn task_advance(args: &Map<String, Value>, root: &Path) -> Result<Value, Value> {
    let id = task_id(args)?;
    let _lock = lock_task(root, &id)?;
    let current = read_task(root, &id)?;
    assert_mutation_scope(&current, args, root)?;
    let task = advance_value(current, root)?;
    Ok(
        json!({"schema":"narada.delegated_task.advance.v1","status":"ok","task_id":id,"task_status":task["status"],"task":compact_task(&task)}),
    )
}
fn step_status<'a>(task: &'a Value, id: &str) -> Option<&'a str> {
    task.pointer(&format!("/result/step_states/{id}/status"))
        .and_then(Value::as_str)
}
fn parse_condition_call(value: &str) -> Option<(&str, Vec<&str>)> {
    let open = value.find('(')?;
    if !value.ends_with(')') {
        return None;
    }
    let name = &value[..open];
    let body = &value[open + 1..value.len() - 1];
    let mut depth = 0;
    let mut start = 0;
    let mut args = Vec::new();
    for (index, ch) in body.char_indices() {
        match ch {
            '(' => depth += 1,
            ')' => depth -= 1,
            ',' if depth == 0 => {
                args.push(body[start..index].trim());
                start = index + 1
            }
            _ => {}
        }
    }
    args.push(body[start..].trim());
    Some((name, args))
}
fn condition_passes(condition: Option<&str>, task: &Value) -> bool {
    let Some(value) = condition.map(str::trim).filter(|value| !value.is_empty()) else {
        return true;
    };
    match value {
        "always" => true,
        "on_failure" => task
            .pointer("/result/step_states")
            .and_then(Value::as_object)
            .is_some_and(|states| {
                states.values().any(|state| {
                    matches!(
                        state.get("status").and_then(Value::as_str),
                        Some("failed" | "blocked")
                    )
                })
            }),
        "on_success" => task
            .pointer("/result/step_states")
            .and_then(Value::as_object)
            .is_none_or(|states| {
                states.values().all(|state| {
                    !matches!(
                        state.get("status").and_then(Value::as_str),
                        Some("failed" | "blocked")
                    )
                })
            }),
        "review_failed" => task
            .pointer("/result/step_states")
            .and_then(Value::as_object)
            .is_some_and(|states| {
                states.values().any(|state| {
                    state.get("kind").and_then(Value::as_str) == Some("review")
                        && state.get("status").and_then(Value::as_str) == Some("failed")
                })
            }),
        "no_residual_risks" => task
            .pointer("/result/residual_risks")
            .and_then(Value::as_array)
            .is_none_or(Vec::is_empty),
        _ if value.starts_with("acceptance:") => {
            task.pointer("/result/acceptance_verdict")
                .and_then(Value::as_str)
                == Some(&value[11..])
        }
        _ if value.starts_with("step:") => {
            let parts = value.split(':').collect::<Vec<_>>();
            parts.len() == 3 && step_status(task, parts[1]) == Some(parts[2])
        }
        _ if value.starts_with("kind:") => {
            let parts = value.split(':').collect::<Vec<_>>();
            parts.len() == 3
                && task
                    .pointer("/result/step_states")
                    .and_then(Value::as_object)
                    .is_some_and(|states| {
                        let matching = states
                            .values()
                            .filter(|state| {
                                state.get("kind").and_then(Value::as_str) == Some(parts[1])
                            })
                            .collect::<Vec<_>>();
                        !matching.is_empty()
                            && matching.iter().all(|state| {
                                state.get("status").and_then(Value::as_str) == Some(parts[2])
                            })
                    })
        }
        _ if value.starts_with("result_has:") => task
            .get("result")
            .is_some_and(|result| result.to_string().contains(&value[11..])),
        _ => parse_condition_call(value).is_some_and(|(name, args)| match name {
            "all" => args.len() >= 2 && args.iter().all(|arg| condition_passes(Some(arg), task)),
            "any" => args.len() >= 2 && args.iter().any(|arg| condition_passes(Some(arg), task)),
            "not" => args.len() == 1 && !condition_passes(Some(args[0]), task),
            _ => false,
        }),
    }
}
fn max_retries(task: &Value) -> u64 {
    task.pointer("/constraints/max_retries")
        .or_else(|| task.pointer("/execution/max_retries"))
        .and_then(Value::as_u64)
        .unwrap_or(0)
        .min(10)
}
fn acceptance_verdict(task: &Value, root: &Path) -> (&'static str, Vec<Value>) {
    let mut checks = Vec::new();
    let result = task.get("result").cloned().unwrap_or_else(|| json!({}));
    let result_text = result.to_string();
    for item in task
        .pointer("/acceptance/required_files")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let target = item
            .as_str()
            .or_else(|| item.get("path").and_then(Value::as_str))
            .unwrap_or_default();
        let path = root.join(target);
        let mut passed = !target.is_empty() && is_within(&path, root) && path.exists();
        if passed {
            if let Some(needle) = item.get("contains").and_then(Value::as_str) {
                passed = fs::read_to_string(&path).is_ok_and(|text| text.contains(needle));
            }
        }
        checks.push(json!({"kind":"required_file","target":target,"status":if passed{"passed"}else{"failed"}}));
    }
    for (field, kind) in [
        ("required_tests", "required_test"),
        ("focused_tests", "focused_test"),
    ] {
        for item in task
            .pointer(&format!("/acceptance/{field}"))
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let target = item
                .as_str()
                .or_else(|| item.get("command").and_then(Value::as_str))
                .unwrap_or_default();
            let required = item
                .get("status")
                .and_then(Value::as_str)
                .unwrap_or("passed");
            let matched = result
                .pointer("/verification")
                .and_then(Value::as_array)
                .is_some_and(|records| {
                    records.iter().any(|record| {
                        record.to_string().contains(target)
                            && record
                                .get("status")
                                .and_then(Value::as_str)
                                .is_some_and(|status| status.contains(required))
                    })
                });
            checks.push(json!({"kind":kind,"target":target,"required_status":required,"status":if matched{"passed"}else{"pending"}}));
        }
    }
    for item in task
        .pointer("/acceptance/required_tools")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let target = item
            .as_str()
            .or_else(|| item.get("name").and_then(Value::as_str))
            .unwrap_or_default();
        checks.push(json!({"kind":"required_tool","target":target,"status":if result_text.contains(target){"passed"}else{"pending"}}));
    }
    for item in task
        .pointer("/acceptance/forbidden_patterns")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let target = item
            .as_str()
            .or_else(|| item.get("pattern").and_then(Value::as_str))
            .unwrap_or_default();
        checks.push(json!({"kind":"forbidden_pattern","target":target,"status":if !target.is_empty()&&result_text.contains(target){"failed"}else{"passed"}}));
    }
    if let Some(budget) = task
        .pointer("/acceptance/verification_budget")
        .and_then(Value::as_object)
    {
        let count = result
            .pointer("/verification")
            .and_then(Value::as_array)
            .map(Vec::len)
            .unwrap_or(0) as u64;
        let attempts = budget
            .get("max_attempts")
            .and_then(Value::as_u64)
            .unwrap_or(u64::MAX);
        let commands = budget
            .get("max_commands")
            .and_then(Value::as_u64)
            .unwrap_or(u64::MAX);
        checks.push(json!({"kind":"verification_budget","verification_count":count,"max_attempts":attempts,"max_commands":commands,"status":if count<=attempts&&count<=commands{"passed"}else{"failed"}}));
    }
    if let Some(quorum) = task
        .pointer("/acceptance/review_quorum")
        .and_then(Value::as_object)
    {
        let states = task
            .pointer("/result/step_states")
            .and_then(Value::as_object);
        let passed = states
            .map(|states| {
                states
                    .values()
                    .filter(|state| {
                        state.get("kind").and_then(Value::as_str) == Some("review")
                            && state.get("status").and_then(Value::as_str) == Some("completed")
                    })
                    .count()
            })
            .unwrap_or(0) as u64;
        let failed = states
            .map(|states| {
                states
                    .values()
                    .filter(|state| {
                        state.get("kind").and_then(Value::as_str) == Some("review")
                            && state.get("status").and_then(Value::as_str) == Some("failed")
                    })
                    .count()
            })
            .unwrap_or(0) as u64;
        let running = states
            .map(|states| {
                states
                    .values()
                    .filter(|state| {
                        state.get("kind").and_then(Value::as_str) == Some("review")
                            && state.get("status").and_then(Value::as_str) == Some("running")
                    })
                    .count()
            })
            .unwrap_or(0) as u64;
        let min = quorum
            .get("min_passed")
            .and_then(Value::as_u64)
            .unwrap_or(1);
        let max = quorum
            .get("max_failed")
            .and_then(Value::as_u64)
            .unwrap_or(0);
        let status = if passed == 0 && failed == 0 && running == 0 {
            "pending"
        } else if passed >= min && failed <= max {
            "passed"
        } else if running > 0 {
            "pending"
        } else {
            "failed"
        };
        checks.push(json!({"kind":"review_quorum","min_passed":min,"max_failed":max,"passed":passed,"failed":failed,"status":status}));
    }
    if task
        .pointer("/acceptance/residual_risk_policy")
        .and_then(Value::as_str)
        == Some("none_allowed")
    {
        let count = task
            .pointer("/result/residual_risks")
            .and_then(Value::as_array)
            .map(Vec::len)
            .unwrap_or(0);
        checks.push(json!({"kind":"residual_risk_policy","status":if count==0{"passed"}else{"failed"},"risk_count":count}));
    }
    let verdict = if checks
        .iter()
        .any(|check| check.get("status").and_then(Value::as_str) == Some("failed"))
    {
        "failed"
    } else if checks
        .iter()
        .any(|check| check.get("status").and_then(Value::as_str) == Some("pending"))
    {
        "pending"
    } else {
        "passed"
    };
    (verdict, checks)
}
fn ready_step_ids(task: &Value) -> Vec<String> {
    task.pointer("/workflow/steps")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|step| {
            let id = step.get("id").and_then(Value::as_str)?;
            if step_status(task, id) != Some("pending") {
                return None;
            }
            let ready = step
                .get("depends_on")
                .and_then(Value::as_array)
                .map(|dependencies| {
                    dependencies
                        .iter()
                        .filter_map(Value::as_str)
                        .all(|dependency| {
                            matches!(step_status(task, dependency), Some("completed" | "skipped"))
                        })
                })
                .unwrap_or(true);
            (ready && condition_passes(step.get("if").and_then(Value::as_str), task))
                .then(|| id.to_string())
        })
        .collect()
}
fn advance_value(mut task: Value, root: &Path) -> Result<Value, Value> {
    if matches!(
        task.get("status").and_then(Value::as_str),
        Some("completed" | "failed" | "cancelled")
    ) {
        return Ok(task);
    }
    let id = task["task_id"].as_str().unwrap_or_default().to_string();
    let step_ids = task
        .pointer("/workflow/steps")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|step| step.get("id").and_then(Value::as_str).map(str::to_string))
        .collect::<Vec<_>>();
    for step_id in &step_ids {
        if step_status(&task, step_id) != Some("running") {
            continue;
        }
        let run_id = task
            .pointer(&format!("/result/step_states/{step_id}/current_run_id"))
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let status = crate::worker_delegation::call_tool(
            "worker_run_status",
            json!({"run_id":run_id}).as_object().unwrap(),
            root,
            &[root.to_path_buf()],
        )?;
        let worker = status
            .pointer("/run/status")
            .and_then(Value::as_str)
            .unwrap_or("running")
            .to_string();
        if worker == "completed" {
            task["result"]["step_states"][step_id]["status"] = json!("completed");
            task["result"]["step_states"][step_id]["finished_at"] = json!(now());
            append_event(
                root,
                &id,
                "worker_completed",
                json!({"step_id":step_id,"run_id":run_id}),
            )?;
        } else if matches!(
            worker.as_str(),
            "failed" | "cancelled" | "completed_with_errors"
        ) {
            let attempts = task["result"]["step_states"][step_id]["attempts"]
                .as_u64()
                .unwrap_or(1);
            if attempts <= max_retries(&task) {
                task["result"]["step_states"][step_id]["status"] = json!("pending");
                task["result"]["step_states"][step_id]["current_run_id"] = Value::Null;
                append_event(
                    root,
                    &id,
                    "step_retry_scheduled",
                    json!({"step_id":step_id,"run_id":run_id,"attempts":attempts,"max_retries":max_retries(&task)}),
                )?;
            } else {
                task["status"] = json!("failed");
                task["result"]["step_states"][step_id]["status"] = json!("failed");
                task["result"]["acceptance_verdict"] = json!("failed");
                append_event(
                    root,
                    &id,
                    "task_failed",
                    json!({"step_id":step_id,"run_id":run_id,"worker_status":worker}),
                )?;
            }
        }
    }
    if task.get("status").and_then(Value::as_str) != Some("failed") {
        let steps = task
            .pointer("/workflow/steps")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        for step in &steps {
            let Some(step_id) = step.get("id").and_then(Value::as_str) else {
                continue;
            };
            if step_status(&task, step_id) != Some("pending") {
                continue;
            }
            let dependencies_ready = step
                .get("depends_on")
                .and_then(Value::as_array)
                .map(|items| {
                    items.iter().filter_map(Value::as_str).all(|dependency| {
                        matches!(
                            step_status(&task, dependency),
                            Some("completed" | "skipped")
                        )
                    })
                })
                .unwrap_or(true);
            if dependencies_ready
                && !condition_passes(step.get("if").and_then(Value::as_str), &task)
            {
                task["result"]["step_states"][step_id]["status"] = json!("skipped");
                task["result"]["step_states"][step_id]["finished_at"] = json!(now());
                append_event(
                    root,
                    &id,
                    "step_skipped",
                    json!({"step_id":step_id,"condition":step.get("if")}),
                )?;
            }
        }
        let ready = ready_step_ids(&task);
        for step_id in ready {
            let Some(step) = steps
                .iter()
                .find(|step| step.get("id").and_then(Value::as_str) == Some(step_id.as_str()))
            else {
                continue;
            };
            let kind = step.get("kind").and_then(Value::as_str).unwrap_or("worker");
            if kind == "gate" {
                task["result"]["step_states"][&step_id]["status"] = json!("completed");
                task["result"]["step_states"][&step_id]["finished_at"] = json!(now());
                append_event(
                    root,
                    &id,
                    "step_gate_evaluated",
                    json!({"step_id":step_id,"authority_gate":step.get("authority_gate"),"executed":false}),
                )?;
                continue;
            }
            let instruction = step
                .get("instruction")
                .and_then(Value::as_str)
                .or_else(|| task.get("objective").and_then(Value::as_str))
                .unwrap_or_default();
            let constraints = step.get("constraints").cloned().unwrap_or_else(|| {
                task.get("constraints")
                    .cloned()
                    .unwrap_or_else(|| json!({}))
            });
            let worker_args =
                json!({"intent":{"instruction":instruction},"constraints":constraints});
            let run = crate::worker_delegation::call_tool(
                "worker_run",
                worker_args.as_object().unwrap(),
                root,
                &[root.to_path_buf()],
            )?;
            let run_id = run
                .get("run_id")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let attempts = task["result"]["step_states"][&step_id]["attempts"]
                .as_u64()
                .unwrap_or(0)
                + 1;
            task["status"] = json!("running");
            task["result"]["step_states"][&step_id]["status"] = json!("running");
            task["result"]["step_states"][&step_id]["attempts"] = json!(attempts);
            task["result"]["step_states"][&step_id]["current_run_id"] = json!(run_id);
            if let Some(run_ids) = task["result"]["step_states"][&step_id]["run_ids"].as_array_mut()
            {
                run_ids.push(json!(run_id));
            }
            task["result"]["step_states"][&step_id]["started_at"] = json!(now());
            if let Some(refs) = task["result"]["worker_refs"].as_array_mut() {
                refs.push(
                    json!({"step_id":step_id,"step_kind":kind,"run_id":run_id,"status":"running"}),
                );
            }
            append_event(
                root,
                &id,
                "worker_started",
                json!({"step_id":step_id,"run_id":run_id,"attempt":attempts}),
            )?;
        }
        if !step_ids.is_empty()
            && step_ids
                .iter()
                .all(|step_id| matches!(step_status(&task, step_id), Some("completed" | "skipped")))
        {
            let (verdict, checks) = acceptance_verdict(&task, root);
            task["result"]["acceptance_verdict"] = json!(verdict);
            task["result"]["acceptance_checks"] = json!(checks);
            task["status"] = json!(if verdict == "passed" {
                "completed"
            } else {
                "failed"
            });
            append_event(
                root,
                &id,
                if verdict == "passed" {
                    "task_completed"
                } else {
                    "task_failed"
                },
                json!({"acceptance_verdict":verdict}),
            )?;
        }
    }
    task["updated_at"] = json!(now());
    write_task(root, &task)?;
    Ok(task)
}
fn task_cancel(args: &Map<String, Value>, root: &Path, takeover: bool) -> Result<Value, Value> {
    let id = task_id(args)?;
    let _lock = lock_task(root, &id)?;
    let mut task = read_task(root, &id)?;
    let ownership = assert_mutation_scope(&task, args, root)?;
    if matches!(
        task.get("status").and_then(Value::as_str),
        Some("completed" | "failed" | "cancelled")
    ) {
        return Err(error(
            "delegated_task_terminal_status",
            "delegated_task_terminal_status",
        ));
    }
    task["status"] = json!("cancelled");
    task["updated_at"] = json!(now());
    let kind = if takeover {
        "task_parent_takeover"
    } else {
        "task_cancelled"
    };
    let detail = if takeover {
        json!({"parent_task_id":args.get("parent_task_id"),"reason":args.get("reason")})
    } else {
        json!({"reason":args.get("reason")})
    };
    task["result"][if takeover {
        "parent_takeover"
    } else {
        "cancellation"
    }] = detail.clone();
    write_task(root, &task)?;
    let event = append_event(root, &id, kind, detail)?;
    Ok(
        json!({"schema":if takeover{"narada.delegated_task.parent_takeover.v1"}else{"narada.delegated_task.cancel.v1"},"status":if takeover{"parent_takeover_recorded"}else{"cancelled"},"task_id":id,"task_status":"cancelled","ownership":ownership,"event":event}),
    )
}
fn task_acknowledge(args: &Map<String, Value>, root: &Path) -> Result<Value, Value> {
    let id = task_id(args)?;
    let _lock = lock_task(root, &id)?;
    let mut task = read_task(root, &id)?;
    let ownership = assert_mutation_scope(&task, args, root)?;
    if !matches!(
        task.get("status").and_then(Value::as_str),
        Some("completed" | "failed" | "cancelled")
    ) {
        return Err(error(
            "delegated_task_not_terminal",
            "delegated_task_not_terminal",
        ));
    }
    let ack = json!({"acknowledged":true,"acknowledged_at":now(),"acknowledged_by":args.get("acknowledged_by"),"note":args.get("note")});
    task["result"]["lifecycle_acknowledgement"] = ack.clone();
    task["updated_at"] = json!(now());
    write_task(root, &task)?;
    let event = append_event(root, &id, "task_acknowledged", ack.clone())?;
    Ok(
        json!({"schema":"narada.delegated_task.acknowledge.v1","status":"acknowledged","task_id":id,"task_status":task["status"],"ownership":ownership,"acknowledgement":ack,"event":event}),
    )
}

fn id_schema(required: bool) -> Value {
    json!({"type":"object","properties":{"task_id":{"type":"string"},"refresh":{"type":"boolean","default":false}},"required":if required {json!(["task_id"])} else {json!([])},"additionalProperties":false})
}
fn error(code: &str, message: &str) -> Value {
    json!({"schema":"narada.delegated_task.error.v1","code":code,"message":message})
}
fn tool(name: &str, description: &str, schema: Value, read_only: bool) -> Value {
    json!({"name":name,"description":description,"annotations":{"title":name,"readOnlyHint":read_only,"destructiveHint":!read_only,"idempotentHint":read_only,"openWorldHint":false},"inputSchema":schema,"outputSchema":{"type":"object","additionalProperties":true}})
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_delegated_task_reads_durable_json_without_execution() {
        let root =
            std::env::temp_dir().join(format!("narada-delegated-task-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(root.join("tasks/task_a")).expect("root");
        fs::write(root.join("tasks/task_a/task.json"), r#"{"task_id":"task_a","status":"completed","objective":"demo","updated_at":"2026-01-01T00:00:00Z","result":{"acceptance_verdict":"accepted"}}"#).expect("task");
        let listed = tasks_list(&json!({"limit":1}).as_object().unwrap(), &root).expect("list");
        assert_eq!(listed["count"], 1);
        assert_eq!(
            task_status(&json!({"task_id":"task_a"}).as_object().unwrap(), &root).expect("status")
                ["task_status"],
            "completed"
        );
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn native_delegated_task_refuses_oversized_records() {
        let root = std::env::temp_dir().join(format!(
            "narada-delegated-task-large-{}",
            uuid::Uuid::new_v4()
        ));
        fs::create_dir_all(root.join("tasks/task_a")).expect("root");
        fs::write(
            root.join("tasks/task_a/task.json"),
            vec![b'x'; MAX_FILE_BYTES as usize + 1],
        )
        .expect("task");
        let error = task_status(&json!({"task_id":"task_a"}).as_object().unwrap(), &root)
            .expect_err("oversized record must refuse");
        assert_eq!(error["code"], "delegated_task_record_too_large");
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn native_delegated_task_owns_durable_lifecycle_mutations() {
        let root = std::env::temp_dir().join(format!(
            "narada-delegated-task-mutate-{}",
            uuid::Uuid::new_v4()
        ));
        let created = task_run(
            json!({"task_id":"task_native","objective":"demo","execution":{"start":false}})
                .as_object()
                .unwrap(),
            &root,
        )
        .expect("run");
        assert_eq!(created["task_status"], "accepted_for_execution");
        let cancelled = task_cancel(
            json!({"task_id":"task_native","reason":"fixture"})
                .as_object()
                .unwrap(),
            &root,
            false,
        )
        .expect("cancel");
        assert_eq!(cancelled["task_status"], "cancelled");
        let acknowledged = task_acknowledge(
            json!({"task_id":"task_native","acknowledged_by":"test"})
                .as_object()
                .unwrap(),
            &root,
        )
        .expect("acknowledge");
        assert_eq!(acknowledged["status"], "acknowledged");
        assert_eq!(
            task_events(json!({"task_id":"task_native"}).as_object().unwrap(), &root)
                .expect("events")["count"],
            3
        );

        task_run(
            json!({"task_id":"task_takeover","objective":"demo","execution":{"start":false}})
                .as_object()
                .unwrap(),
            &root,
        )
        .expect("second run");
        let takeover = task_cancel(
            json!({"task_id":"task_takeover","parent_task_id":"parent"})
                .as_object()
                .unwrap(),
            &root,
            true,
        )
        .expect("takeover");
        assert_eq!(takeover["status"], "parent_takeover_recorded");
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn native_delegated_task_preserves_explicit_dag_state() {
        let root = std::env::temp_dir().join(format!(
            "narada-delegated-task-dag-{}",
            uuid::Uuid::new_v4()
        ));
        task_run(json!({"task_id":"task_dag","objective":"demo","execution":{"start":false},"workflow":{"steps":[{"id":"research","kind":"research"},{"id":"synthesize","kind":"worker","depends_on":["research"]}]}}).as_object().unwrap(), &root).expect("run");
        let task = read_task(&root, "task_dag").expect("task");
        assert_eq!(task["workflow"]["steps"].as_array().map(Vec::len), Some(2));
        assert_eq!(
            task["result"]["step_states"]["research"]["status"],
            "pending"
        );
        assert_eq!(
            task["result"]["step_states"]["synthesize"]["status"],
            "pending"
        );
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn native_delegated_task_rejects_invalid_dags_before_write() {
        let root = std::env::temp_dir().join(format!(
            "narada-delegated-task-invalid-dag-{}",
            uuid::Uuid::new_v4()
        ));
        let invalid = json!({"task_id":"task_cycle","objective":"demo","execution":{"start":false},"workflow":{"steps":[{"id":"a","kind":"worker","depends_on":["b"]},{"id":"b","kind":"worker","depends_on":["a"]}]}});
        let error = task_run(invalid.as_object().unwrap(), &root).expect_err("cycle must refuse");
        assert_eq!(error["code"], "delegated_task_validation_failed");
        assert!(!root.join("tasks/task_cycle/task.json").exists());
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn native_delegated_task_schedules_only_dependency_ready_steps() {
        let mut task = json!({"workflow":{"steps":[{"id":"a","kind":"worker"},{"id":"b","kind":"worker","depends_on":["a"]}]},"result":{"step_states":{"a":{"status":"pending"},"b":{"status":"pending"}}}});
        assert_eq!(ready_step_ids(&task), vec!["a"]);
        task["result"]["step_states"]["a"]["status"] = json!("completed");
        assert_eq!(ready_step_ids(&task), vec!["b"]);
    }

    #[test]
    fn native_delegated_task_evaluates_conditions_and_acceptance() {
        let root = std::env::temp_dir().join(format!(
            "narada-delegated-task-semantics-{}",
            uuid::Uuid::new_v4()
        ));
        fs::create_dir_all(&root).expect("root");
        fs::write(root.join("proof.txt"), "accepted evidence").expect("proof");
        let task = json!({"result":{"acceptance_verdict":"passed","residual_risks":[],"verification":[{"command":"cargo test","status":"passed"}],"tools":["filesystem_search"],"step_states":{"review":{"kind":"review","status":"completed"}}},"acceptance":{"required_files":[{"path":"proof.txt","contains":"evidence"}],"required_tests":["cargo test"],"focused_tests":[{"command":"cargo test","status":"passed"}],"required_tools":["filesystem_search"],"forbidden_patterns":["forbidden-secret"],"verification_budget":{"max_attempts":2,"max_commands":2},"review_quorum":{"min_passed":1,"max_failed":0},"residual_risk_policy":"none_allowed"}});
        assert!(condition_passes(
            Some("all(step:review:completed,no_residual_risks)"),
            &task
        ));
        let (verdict, checks) = acceptance_verdict(&task, &root);
        assert_eq!(verdict, "passed");
        assert_eq!(checks.len(), 8);
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn native_delegated_task_enforces_owner_site_on_mutation() {
        let root = std::env::temp_dir().join(format!("site-current-{}", uuid::Uuid::new_v4()));
        let task =
            json!({"task_id":"task_owned","owner_site_id":"site-other","visibility_scope":"site"});
        let denied = assert_mutation_scope(&task, &Map::new(), &root)
            .expect_err("cross-site mutation denied");
        assert_eq!(denied["code"], "delegated_task_cross_site_mutation_denied");
        let allowed = assert_mutation_scope(
            &task,
            json!({"allow_cross_site":true,"expected_owner_site_id":"site-other"})
                .as_object()
                .unwrap(),
            &root,
        )
        .expect("override");
        assert_eq!(allowed["owner_site_id"], "site-other");
    }
}
