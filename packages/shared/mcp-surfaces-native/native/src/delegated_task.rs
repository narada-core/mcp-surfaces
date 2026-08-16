use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::thread::JoinHandle;

const SERVER_NAME: &str = "delegated-task-mcp";
const DEFAULT_COGNITION: &str = "low";
const MAX_ITEMS: usize = 200;
const MAX_FILE_BYTES: u64 = 256_000;
const MAX_WORKER_OUTPUT_BYTES: usize = 32_000;
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
            json!({"type":"object","properties":{"objective":{"type":"string"},"workflow":{"type":"object"},"constraints":constraints_schema(),"acceptance":{"type":"object"},"execution":{"type":"object"},"execution_binding":{"type":"object"}},"additionalProperties":false}),
        ),
        (
            "delegated_task_status",
            "Return compact durable delegated task status.",
            id_schema(true),
        ),
        (
            "delegated_tasks_list",
            "List bounded delegated tasks by lifecycle and site scope.",
            json!({"type":"object","properties":{"limit":{"type":"integer","minimum":1,"maximum":200,"default":20},"view":{"type":"string"},"site_scope":{"type":"string"},"owner_site_id":{"type":"string"},"include_active":{"type":"boolean"},"include_terminal":{"type":"boolean"},"include_acknowledged":{"type":"boolean"}},"additionalProperties":false}),
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
            mutation_schema(name),
            false,
        ));
    }
    tools.push(tool(
        "delegated_task_wait",
        "Advance and wait for a delegated task to approach terminal status.",
        json!({"type":"object","properties":{"task_id":{"type":"string"},"timeout_ms":{"type":"integer","minimum":0,"maximum":600000,"default":30000},"poll_ms":{"type":"integer","minimum":50,"maximum":30000,"default":500},"expected_owner_site_id":{"type":"string"},"allow_cross_site":{"type":"boolean","default":false}},"required":["task_id"],"additionalProperties":false}),
        false,
    ));
    tools
}

fn mutation_schema(name: &str) -> Value {
    let scope = json!({"expected_owner_site_id":{"type":"string"},"allow_cross_site":{"type":"boolean","default":false}});
    let mut properties = scope.as_object().cloned().unwrap_or_default();
    match name {
        "delegated_task_run" => {
            for field in ["objective","idempotency_key","task_id"] { properties.insert(field.into(),json!({"type":"string"})); }
            for field in ["intent","workflow","acceptance","result_policy","execution","execution_binding","source_task_ref"] { properties.insert(field.into(),json!({"type":"object"})); }
            properties.insert("constraints".into(), constraints_schema());
            for field in ["depends_on_task_ids","import_task_outputs","import_worker_refs"] { properties.insert(field.into(),json!({"type":"array","items":{"type":"string"}})); }
            json!({"type":"object","properties":properties,"anyOf":[{"required":["objective"]},{"required":["intent"]},{"required":["task_id"]}],"additionalProperties":false})
        }
        "delegated_task_advance" => { properties.insert("task_id".into(),json!({"type":"string"})); json!({"type":"object","properties":properties,"required":["task_id"],"additionalProperties":false}) }
        "delegated_task_cancel" => { properties.insert("task_id".into(),json!({"type":"string"})); properties.insert("reason".into(),json!({"type":"string"})); json!({"type":"object","properties":properties,"required":["task_id"],"additionalProperties":false}) }
        "delegated_task_parent_takeover" => { for field in ["task_id","parent_task_id","reason"] { properties.insert(field.into(),json!({"type":"string"})); } json!({"type":"object","properties":properties,"required":["task_id","parent_task_id"],"additionalProperties":false}) }
        "delegated_task_acknowledge" => { for field in ["task_id","acknowledged_by","note"] { properties.insert(field.into(),json!({"type":"string"})); } json!({"type":"object","properties":properties,"required":["task_id"],"additionalProperties":false}) }
        _ => json!({"type":"object","properties":{},"additionalProperties":false}),
    }
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
    json!({"schema":"narada.mcp_surface.guidance.v0","status":"ok","surface_id":"delegated-task","guidance_tool":"delegated_task_guidance","purpose":"Validate, execute, and inspect durable delegated task workflows through native authority.","requested":{"workflow":args.get("workflow").cloned().unwrap_or(Value::Null),"tool":args.get("tool").cloned().unwrap_or(Value::Null)},"cognition":{"default":"low","omitted_constraint_behavior":"constraints.cognition resolves to low","mapping_surface":"worker-delegation","mapping_tool":"worker_cognition_defaults_inspect"},"first_use":["Call delegated_task_policy_inspect first.","Omitted constraints.cognition resolves to low; inspect worker-delegation's worker_cognition_defaults_inspect for the current model and reasoning-effort mapping.","Validate an input before creating a task.","Use bounded list/status/result/events readback.","Use explicit cancellation and disposition tools for lifecycle mutations."],"boundaries":["Native authority owns task.json/events.jsonl under the bounded task root.","Worker launches cross the native worker-delegation authority boundary.","Cross-site ownership remains server-bound authority."]})
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
struct TaskLock {
    path: PathBuf,
    token: String,
    stop: Arc<AtomicBool>,
    heartbeat: Option<JoinHandle<()>>,
}
impl Drop for TaskLock {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(handle) = self.heartbeat.take() {
            handle.thread().unpark();
            let _ = handle.join();
        }
        if lock_owner_matches(&self.path, &self.token) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }
}
fn lock_owner_matches(path: &Path, token: &str) -> bool {
    fs::read_to_string(path.join("owner.json"))
        .ok()
        .and_then(|text| serde_json::from_str::<Value>(&text).ok())
        .and_then(|owner| {
            owner
                .get("token")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .as_deref()
        == Some(token)
}
fn lock_stale(path: &Path, stale_ms: u64) -> bool {
    let heartbeat = path.join("owner.json");
    let target = if heartbeat.exists() { &heartbeat } else { path };
    fs::metadata(target)
        .and_then(|metadata| metadata.modified())
        .ok()
        .and_then(|modified| modified.elapsed().ok())
        .is_some_and(|elapsed| elapsed > std::time::Duration::from_millis(stale_ms))
}
fn reclaim_stale_lock(path: &Path) -> bool {
    let claim = path.with_extension("lockdir.reclaim");
    let claim_file = match OpenOptions::new().write(true).create_new(true).open(&claim) {
        Ok(file) => file,
        Err(_) => return false,
    };
    let suffix = format!(
        "abandoned-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    );
    let abandoned = path.with_extension(suffix);
    let won = fs::rename(path, &abandoned).is_ok();
    if won {
        let _ = fs::remove_dir_all(abandoned);
    }
    drop(claim_file);
    let _ = fs::remove_file(claim);
    won
}
fn lock_task(root: &Path, id: &str) -> Result<TaskLock, Value> {
    let path = task_path(root, id)?.with_file_name("mutation.lockdir");
    fs::create_dir_all(path.parent().unwrap())
        .map_err(|_| error("delegated_task_lock_failed", "delegated_task_lock_failed"))?;
    let timeout_ms = std::env::var("NARADA_DELEGATED_TASK_LOCK_TIMEOUT_MS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(30_000)
        .clamp(100, 30_000);
    let stale_ms = std::env::var("NARADA_DELEGATED_TASK_LOCK_STALE_MS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(300_000)
        .clamp(1_000, 86_400_000);
    let started = std::time::Instant::now();
    loop {
        match fs::create_dir(&path) {
            Ok(()) => {
                let owner_path = path.join("owner.json");
                let token = format!(
                    "{}-{}",
                    std::process::id(),
                    std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_nanos()
                );
                fs::write(&owner_path, json!({"schema":"narada.delegated_task.mutation_lock.v1","token":token,"pid":std::process::id(),"heartbeat_at":now()}).to_string())
                    .map_err(|_| error("delegated_task_lock_failed", "delegated_task_lock_failed"))?;
                let stop = Arc::new(AtomicBool::new(false));
                let heartbeat_stop = Arc::clone(&stop);
                let heartbeat_path = path.clone();
                let heartbeat_token = token.clone();
                let heartbeat_interval =
                    std::time::Duration::from_millis((stale_ms / 3).clamp(100, 1_000));
                let heartbeat = std::thread::spawn(move || {
                    while !heartbeat_stop.load(Ordering::Acquire) {
                        std::thread::park_timeout(heartbeat_interval);
                        if heartbeat_stop.load(Ordering::Acquire) {
                            break;
                        }
                        if !lock_owner_matches(&heartbeat_path, &heartbeat_token) {
                            break;
                        }
                        let _ = fs::write(&owner_path, json!({"schema":"narada.delegated_task.mutation_lock.v1","token":heartbeat_token,"pid":std::process::id(),"heartbeat_at":now()}).to_string());
                    }
                });
                return Ok(TaskLock {
                    path,
                    token,
                    stop,
                    heartbeat: Some(heartbeat),
                });
            }
            Err(error_value)
                if error_value.kind() == std::io::ErrorKind::AlreadyExists
                    && lock_stale(&path, stale_ms) =>
            {
                let _ = reclaim_stale_lock(&path);
            }
            Err(error_value)
                if error_value.kind() == std::io::ErrorKind::AlreadyExists
                    && started.elapsed() < std::time::Duration::from_millis(timeout_ms) =>
            {
                std::thread::sleep(std::time::Duration::from_millis(25))
            }
            Err(_) => {
                return Err(error(
                    "delegated_task_lock_failed",
                    "delegated_task_lock_failed",
                ))
            }
        }
    }
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
    json!({"schema":"narada.delegated_task.policy.v1","status":"ok","server_name":SERVER_NAME,"task_root":task_root(root).to_string_lossy(),"site_root":root.to_string_lossy(),"allowed_roots":[root.to_string_lossy()],"default_cognition":DEFAULT_COGNITION,"list_defaults":{"view":"active_queue","site_scope":"current_site"},"workflow_engine":"native_authority","worker_execution":"native_worker_authority","result_compaction":{"max_worker_refs":50,"max_list_items":200}})
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
        let kind = step.get("kind").and_then(Value::as_str).unwrap_or("worker");
        if !matches!(
            kind,
            "worker" | "review" | "repair" | "verify" | "research" | "gate" | "join" | "note"
        ) {
            diagnostics.push(json!({"severity":"error","code":"workflow_policy_violation","step_id":id,"kind":kind}));
        }
        if let Some(condition) = step.get("if").and_then(Value::as_str) {
            if !valid_condition(condition) {
                diagnostics.push(json!({"severity":"error","code":"invalid_condition","step_id":id,"condition":condition}));
            }
        }
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
    diagnostics.extend(constraint_diagnostics(args.get("constraints"), "constraints"));
    if let Some(steps) = workflow.get("steps").and_then(Value::as_array) {
        for (index, step) in steps.iter().enumerate() {
            diagnostics.extend(constraint_diagnostics(
                step.get("constraints"),
                &format!("workflow.steps[{index}].constraints"),
            ));
        }
    }
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
    Ok(json!({
        "schema":"narada.delegated_task.validate.v1",
        "status":if diagnostics.is_empty(){"ok"}else{"rejected"},
        "dry_run":true,
        "diagnostics":diagnostics,
        "valid":errors.is_empty(),
        "task_root":task_root(root).to_string_lossy(),
        "errors":errors,
        "objective":objective,
        "resolved_constraints":normalized_constraints(args.get("constraints")),
        "worker_execution":"not_run"
    }))
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
    let mut records = Vec::new();
    if let Ok(entries) = fs::read_dir(tasks_dir(root)) {
        for entry in entries.filter_map(Result::ok).take(MAX_ITEMS) {
            if !entry.path().is_dir() {
                continue;
            }
            let Some(id) = entry.file_name().to_str().map(ToOwned::to_owned) else {
                continue;
            };
            if let Ok(task) = read_task(root, &id) {
                records.push(task);
            }
        }
    }
    let view = args
        .get("view")
        .and_then(Value::as_str)
        .unwrap_or("active_queue");
    let site_scope = args
        .get("site_scope")
        .and_then(Value::as_str)
        .unwrap_or("current_site");
    let current = current_site_id(root);
    let owner_filter = args.get("owner_site_id").and_then(Value::as_str);
    let include_ack = args.get("include_acknowledged").and_then(Value::as_bool) == Some(true);
    let legacy = args.contains_key("include_terminal") || args.contains_key("include_active");
    let include_terminal = args.get("include_terminal").and_then(Value::as_bool) == Some(true);
    let include_active = args.get("include_active").and_then(Value::as_bool) != Some(false);
    records.retain(|task| {
        let projected = ownership(task);
        let owner = projected
            .get("owner_site_id")
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        if owner_filter.is_some_and(|expected| expected != owner) {
            return false;
        }
        if site_scope == "current_site" && current.as_deref().is_some_and(|site| site != owner) {
            return false;
        }
        if site_scope == "user_global"
            && !matches!(
                projected.get("visibility_scope").and_then(Value::as_str),
                Some("user_global" | "user_global_legacy")
            )
        {
            return false;
        }
        let terminal = matches!(
            task.get("status").and_then(Value::as_str),
            Some("completed" | "failed" | "cancelled")
        );
        let acknowledged = task
            .pointer("/result/lifecycle_acknowledgement/acknowledged")
            .and_then(Value::as_bool)
            == Some(true);
        if legacy {
            return (if terminal {
                include_terminal
            } else {
                include_active
            }) && (include_ack || !acknowledged);
        }
        match view {
            "all" => include_ack || !acknowledged,
            "active_queue" => !terminal,
            "operator_inbox" => terminal && !acknowledged,
            "history" => terminal && (include_ack || !acknowledged),
            "acknowledged_archive" => terminal && acknowledged,
            _ => !terminal,
        }
    });
    records.sort_by(|a, b| {
        b.get("updated_at")
            .and_then(Value::as_str)
            .cmp(&a.get("updated_at").and_then(Value::as_str))
    });
    let total = records.len();
    records.truncate(limit);
    let tasks = records.iter().map(compact_task).collect::<Vec<_>>();
    Ok(
        json!({"schema":"narada.delegated_task.list.v1","status":"ok","view":view,"site_scope":site_scope,"current_site_id":current,"owner_site_id":owner_filter,"count":tasks.len(),"total_scoped_count":total,"limit":limit,"include_active":include_active,"include_terminal":include_terminal,"include_acknowledged":include_ack,"tasks":tasks}),
    )
}
fn compact_task(task: &Value) -> Value {
    let obj = task.as_object().cloned().unwrap_or_default();
    let result = obj.get("result").and_then(Value::as_object);
    json!({"task_id":obj.get("task_id"),"task_status":obj.get("status"),"objective":obj.get("objective"),"owner_site_id":obj.get("owner_site_id"),"created_by_site_id":obj.get("created_by_site_id"),"visibility_scope":obj.get("visibility_scope"),"updated_at":obj.get("updated_at"),"summary":obj.get("summary"),"execution_binding":obj.get("execution_binding"),"worker_refs":result.and_then(|v|v.get("worker_refs")),"worker_outputs":result.and_then(|v|v.get("worker_outputs"))})
}

fn worker_output_from_run(run: &Value) -> Option<Value> {
    let summary = run
        .get("summary")
        .or_else(|| run.get("summary_preview"))
        .filter(|value| !value.is_null())?;
    match summary {
        Value::String(text) => {
            let bounded = text.len() <= MAX_WORKER_OUTPUT_BYTES;
            if bounded {
                if let Ok(structured) = serde_json::from_str::<Value>(text) {
                    let encoded_len = serde_json::to_vec(&structured)
                        .map(|bytes| bytes.len())
                        .unwrap_or(MAX_WORKER_OUTPUT_BYTES + 1);
                    if encoded_len <= MAX_WORKER_OUTPUT_BYTES {
                        return Some(json!({"summary_text":text,"structured_output":structured,"truncated":false}));
                    }
                }
            }
            Some(json!({"summary_text":text.chars().take(MAX_WORKER_OUTPUT_BYTES).collect::<String>(),"truncated":!bounded || text.chars().count()>MAX_WORKER_OUTPUT_BYTES}))
        }
        value => {
            let encoded_len = serde_json::to_vec(value)
                .map(|bytes| bytes.len())
                .unwrap_or(MAX_WORKER_OUTPUT_BYTES + 1);
            (encoded_len <= MAX_WORKER_OUTPUT_BYTES)
                .then(|| json!({"structured_output":value,"truncated":false}))
        }
    }
}

fn record_worker_terminal(
    task: &mut Value,
    step_id: &str,
    run_id: &str,
    status: &str,
    run: &Value,
) {
    let output = worker_output_from_run(run);
    if let Some(refs) = task["result"]["worker_refs"].as_array_mut() {
        if let Some(reference) = refs.iter_mut().find(|reference| {
            reference.get("run_id").and_then(Value::as_str) == Some(run_id)
        }) {
            reference["status"] = json!(status);
            reference["finished_at"] = json!(now());
            if let Some(value) = output.clone() {
                reference["output"] = value;
            }
            if let Some(error) = run.get("error").filter(|value| !value.is_null()) {
                reference["error"] = error.clone();
            }
        }
    }
    let step_state = &mut task["result"]["step_states"][step_id];
    step_state["worker_status"] = json!(status);
    if let Some(value) = output.clone() {
        step_state["worker_output"] = value;
    }
    if !task["result"].get("worker_outputs").is_some_and(Value::is_array) {
        task["result"]["worker_outputs"] = json!([]);
    }
    if let Some(outputs) = task["result"]["worker_outputs"].as_array_mut() {
        outputs.retain(|value| value.get("run_id").and_then(Value::as_str) != Some(run_id));
        outputs.push(json!({"step_id":step_id,"run_id":run_id,"status":status,"output":output,"error":run.get("error")}));
    }
}
fn task_status(args: &Map<String, Value>, root: &Path) -> Result<Value, Value> {
    let id = task_id(args)?;
    let task = if args.get("refresh").and_then(Value::as_bool) == Some(true) {
        let _lock = lock_task(root, &id)?;
        let current = read_task(root, &id)?;
        assert_mutation_scope(&current, args, root)?;
        advance_value(current, root)?
    } else {
        read_task(root, &id)?
    };
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
        json!({"schema":"narada.delegated_task.summary.v1","status":"ok","task_id":id,"task_status":task.get("status"),"objective":task.get("objective"),"summary":task.get("summary"),"acceptance_verdict":result.get("acceptance_verdict").cloned().unwrap_or(Value::String("pending".into())),"residual_risks":result.get("residual_risks").cloned().unwrap_or_else(||json!([])),"progress":result.get("progress"),"worker_refs":result.get("worker_refs"),"worker_outputs":result.get("worker_outputs")}),
    )
}
fn task_wait(args: &Map<String, Value>, root: &Path) -> Result<Value, Value> {
    let id = task_id(args)?;
    let timeout = args
        .get("timeout_ms")
        .and_then(Value::as_u64)
        .unwrap_or(30000)
        .min(600000);
    let poll = args
        .get("poll_ms")
        .and_then(Value::as_u64)
        .unwrap_or(500)
        .clamp(50, 30000);
    let started = std::time::Instant::now();
    let task = loop {
        let current = {
            let _lock = lock_task(root, &id)?;
            let current = read_task(root, &id)?;
            assert_mutation_scope(&current, args, root)?;
            advance_value(current, root)?
        };
        if matches!(
            current.get("status").and_then(Value::as_str),
            Some("completed" | "failed" | "cancelled")
        ) || started.elapsed().as_millis() as u64 >= timeout
        {
            break current;
        }
        std::thread::sleep(std::time::Duration::from_millis(
            poll.min(timeout.saturating_sub(started.elapsed().as_millis() as u64)),
        ))
    };
    Ok(
        json!({"schema":"narada.delegated_task.wait.v1","status":if matches!(task.get("status").and_then(Value::as_str),Some("completed"|"failed"|"cancelled")){"finished"}else{"timeout"},"elapsed_ms":started.elapsed().as_millis() as u64,"timeout_ms":timeout,"poll_ms":poll,"task_id":id,"task_status":task.get("status"),"refresh_performed":true,"worker_execution":"native_worker_authority","task":compact_task(&task)}),
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
fn canonicalize(value: &Value) -> Value {
    match value {
        Value::Array(items) => Value::Array(items.iter().map(canonicalize).collect()),
        Value::Object(object) => {
            let mut keys = object.keys().collect::<Vec<_>>();
            keys.sort();
            let mut out = Map::new();
            for key in keys {
                out.insert(key.clone(), canonicalize(&object[key]));
            }
            Value::Object(out)
        }
        _ => value.clone(),
    }
}
fn sha256_json(value: &Value) -> String {
    let digest = Sha256::digest(
        serde_json::to_string(&canonicalize(value))
            .unwrap_or_default()
            .as_bytes(),
    );
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}
fn normalized_execution(value: Option<&Value>) -> Value {
    let input = value.and_then(Value::as_object);
    let wait = input
        .and_then(|v| v.get("wait_for_completion"))
        .and_then(Value::as_bool)
        == Some(true);
    json!({"start":input.and_then(|v|v.get("start")).and_then(Value::as_bool)!=Some(false),"wait_for_completion":wait,"timeout_ms":input.and_then(|v|v.get("timeout_ms")).and_then(Value::as_u64).unwrap_or(if wait{30000}else{0}).min(600000),"poll_ms":input.and_then(|v|v.get("poll_ms")).and_then(Value::as_u64).unwrap_or(500).clamp(50,30000),"resumable":input.and_then(|v|v.get("resumable")).and_then(Value::as_bool)!=Some(false),"exit_interview":input.and_then(|v|v.get("exit_interview")).and_then(Value::as_bool)==Some(true),"max_concurrency":input.and_then(|v|v.get("max_concurrency")).and_then(Value::as_u64).unwrap_or(10).clamp(1,32),"max_retries":input.and_then(|v|v.get("max_retries")).and_then(Value::as_u64).unwrap_or(0).min(10)})
}
fn normalized_constraints(value: Option<&Value>) -> Value {
    let mut constraints = value
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    let cognition = constraints
        .get("cognition")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty());
    if cognition.is_none() {
        constraints.insert("cognition".into(), json!(DEFAULT_COGNITION));
    }
    Value::Object(constraints)
}
const CONSTRAINT_FIELDS: &[&str] = &[
    "authority",
    "cwd",
    "site_root",
    "provider",
    "profile",
    "cognition",
    "model",
    "sandbox",
    "runtime",
    "invocation_plan_ref",
    "skip_git_repo_check",
    "resumable",
    "wait_for_completion",
    "wait_timeout_ms",
    "max_run_ms",
    "exit_interview",
    "max_concurrency",
    "max_retries",
    "repair_policy",
    "authority_gates",
    "required_mcp_tools",
    "preflight_paths",
    "overrides",
];
fn constraints_schema() -> Value {
    json!({
        "type":"object",
        "properties":{
            "authority":{"type":"string","enum":["read","write","command"]},
            "cwd":{"type":"string","minLength":1,"maxLength":4096},
            "site_root":{"type":"string","minLength":1,"maxLength":4096},
            "provider":{"type":"string","minLength":1,"maxLength":256},
            "profile":{"type":"string","minLength":1,"maxLength":256},
            "cognition":{"type":"string","enum":["low","medium","high"],"default":DEFAULT_COGNITION},
            "model":{"type":"string","minLength":1,"maxLength":256},
            "sandbox":{"type":"string","enum":["read-only","workspace-write","danger-full-access"]},
            "runtime":{"type":"string","minLength":1,"maxLength":256},
            "invocation_plan_ref":{"type":"string","minLength":6,"maxLength":512,"pattern":"^plan:[A-Za-z0-9._:-]+$"},
            "skip_git_repo_check":{"type":"boolean"},
            "resumable":{"type":"boolean"},
            "wait_for_completion":{"type":"boolean"},
            "wait_timeout_ms":{"type":"integer","minimum":1,"maximum":180000},
            "max_run_ms":{"type":"integer","minimum":1,"maximum":1800000},
            "exit_interview":{"type":"boolean"},
            "max_concurrency":{"type":"integer","minimum":1,"maximum":32},
            "max_retries":{"type":"integer","minimum":0,"maximum":10},
            "repair_policy":{"type":"object","properties":{"strategy":{"type":"string","enum":["retry_same_step","named_repair_step"]},"repair_step_id":{"type":"string","minLength":1,"maxLength":256},"require_review_after_repair":{"type":"boolean"}},"additionalProperties":false},
            "authority_gates":{"type":"object","properties":{"commit":{"type":"object","properties":{"operation":{"type":"string","enum":["commit","push"]},"mode":{"type":"string","enum":["disallowed","requires_explicit_authority","allowed"]},"reason":{"type":"string","maxLength":2048},"required_authority":{"type":"string","enum":["write","command"]}},"additionalProperties":false},"push":{"type":"object","properties":{"operation":{"type":"string","enum":["commit","push"]},"mode":{"type":"string","enum":["disallowed","requires_explicit_authority","allowed"]},"reason":{"type":"string","maxLength":2048},"required_authority":{"type":"string","enum":["write","command"]}},"additionalProperties":false}},"additionalProperties":false},
            "required_mcp_tools":{"type":"array","maxItems":64,"items":{"type":"string","minLength":1,"maxLength":256}},
            "preflight_paths":{"type":"array","maxItems":64,"items":{"type":"object","properties":{"path":{"type":"string","minLength":1,"maxLength":4096},"access":{"type":"string","enum":["read","write","create"]},"label":{"type":"string","maxLength":256}},"required":["path","access"],"additionalProperties":false}},
            "overrides":{"type":"object","properties":{"runtime":{"type":"string","minLength":1,"maxLength":256},"sandbox":{"type":"string","enum":["read-only","workspace-write","danger-full-access"]},"model":{"type":"string","minLength":1,"maxLength":256},"reasoning_effort":{"type":"string","minLength":1,"maxLength":64},"config":{"type":"object","additionalProperties":{"oneOf":[{"type":"string"},{"type":"number"},{"type":"boolean"}]}},"skip_git_repo_check":{"type":"boolean"}},"additionalProperties":false}
        },
        "additionalProperties":false
    })
}
fn constraint_diagnostics(value: Option<&Value>, locus: &str) -> Vec<Value> {
    let Some(value) = value else { return Vec::new(); };
    let Some(object) = value.as_object() else {
        return vec![json!({"severity":"error","code":"constraints_must_be_object","locus":locus})];
    };
    let mut diagnostics = Vec::new();
    for key in object.keys() {
        if !CONSTRAINT_FIELDS.contains(&key.as_str()) {
            diagnostics.push(json!({"severity":"error","code":"unknown_constraint","locus":locus,"field":key}));
        }
    }
    if let Some(cognition) = object.get("cognition") {
        if !matches!(cognition.as_str(), Some("low" | "medium" | "high")) {
            diagnostics.push(json!({"severity":"error","code":"constraint_cognition_invalid","locus":format!("{locus}.cognition")}));
        }
    }
    if let Some(paths) = object.get("preflight_paths") {
        match paths.as_array() {
            Some(items) => for (index, item) in items.iter().enumerate() {
                let item_locus = format!("{locus}.preflight_paths[{index}]");
                let Some(path) = item.as_object() else {
                    diagnostics.push(json!({"severity":"error","code":"constraint_preflight_path_must_be_object","locus":item_locus}));
                    continue;
                };
                if path.get("path").and_then(Value::as_str).is_none_or(|value| value.trim().is_empty()) {
                    diagnostics.push(json!({"severity":"error","code":"constraint_preflight_path_requires_path","locus":item_locus}));
                }
                if !matches!(path.get("access").and_then(Value::as_str), Some("read" | "write" | "create")) {
                    diagnostics.push(json!({"severity":"error","code":"constraint_preflight_path_access_invalid","locus":item_locus}));
                }
            },
            None => diagnostics.push(json!({"severity":"error","code":"constraints_preflight_paths_must_be_array","locus":format!("{locus}.preflight_paths")})),
        }
    }
    if let Some(tools) = object.get("required_mcp_tools") {
        match tools.as_array() {
            Some(items) => for (index, item) in items.iter().enumerate() {
                if item.as_str().is_none_or(|value| value.trim().is_empty()) {
                    diagnostics.push(json!({"severity":"error","code":"constraint_required_mcp_tool_invalid","locus":format!("{locus}.required_mcp_tools[{index}]" )}));
                }
            },
            None => diagnostics.push(json!({"severity":"error","code":"constraints_required_mcp_tools_must_be_array","locus":format!("{locus}.required_mcp_tools")})),
        }
    }
    if let Some(overrides) = object.get("overrides") {
        if let Some(overrides) = overrides.as_object() {
            for key in overrides.keys() {
                if !["runtime", "sandbox", "model", "reasoning_effort", "config", "skip_git_repo_check"].contains(&key.as_str()) {
                    diagnostics.push(json!({"severity":"error","code":"unknown_constraint_override","locus":format!("{locus}.overrides"),"field":key}));
                }
            }
        } else {
            diagnostics.push(json!({"severity":"error","code":"constraints_overrides_must_be_object","locus":format!("{locus}.overrides")}));
        }
    }
    diagnostics
}
fn normalize_persisted_constraints(task: &mut Value) -> bool {
    let mut changed = false;
    let normalized = normalized_constraints(task.get("constraints"));
    if task.get("constraints") != Some(&normalized) {
        task["constraints"] = normalized;
        changed = true;
    }
    if let Some(steps) = task.pointer_mut("/workflow/steps").and_then(Value::as_array_mut) {
        for step in steps {
            if step.get("constraints").is_some() {
                let normalized = normalized_constraints(step.get("constraints"));
                if step.get("constraints") != Some(&normalized) {
                    step["constraints"] = normalized;
                    changed = true;
                }
            }
        }
    }
    changed
}
fn request_fingerprint(args: &Map<String, Value>, root: &Path, id: &str) -> String {
    let mut material = Map::new();
    material.insert("objective".into(),json!({"objective":objective(args).unwrap_or_default(),"instructions":args.get("intent").and_then(|v|v.get("instructions")).cloned().unwrap_or(Value::Null),"behavior":args.get("intent").and_then(|v|v.get("behavior")).cloned().unwrap_or(Value::Null),"mode":args.get("intent").and_then(|v|v.get("mode")).cloned().unwrap_or(Value::Null)}));
    material.insert(
        "constraints".into(),
        normalized_constraints(args.get("constraints")),
    );
    for key in ["workflow", "acceptance", "result_policy"] {
        if let Some(value) = args.get(key) {
            material.insert(key.into(), value.clone());
        }
    }
    material.insert(
        "execution".into(),
        normalized_execution(args.get("execution")),
    );
    let binding = args.get("execution_binding").and_then(Value::as_object);
    material.insert("execution_binding".into(),json!({"workspace_root":binding.and_then(|v|v.get("workspace_root")).cloned().unwrap_or_else(||json!(root.to_string_lossy())),"executor_kind":binding.and_then(|v|v.get("executor_kind")).cloned().unwrap_or_else(||json!("delegated_task")),"executor_profile":binding.and_then(|v|v.get("executor_profile")).cloned().unwrap_or(Value::Null),"executor_id":binding.and_then(|v|v.get("executor_id")).cloned().unwrap_or(Value::Null),"repository_root":binding.and_then(|v|v.get("repository_root")).cloned().unwrap_or(Value::Null),"site_root":binding.and_then(|v|v.get("site_root")).cloned().unwrap_or_else(||json!(root.to_string_lossy())),"correlation_key":binding.and_then(|v|v.get("correlation_key")).cloned().unwrap_or_else(||json!(args.get("idempotency_key").and_then(Value::as_str).unwrap_or(id)))}));
    material.insert("external_dependencies".into(),json!({"depends_on_task_ids":args.get("depends_on_task_ids").cloned().unwrap_or_else(||json!([])),"import_task_outputs":args.get("import_task_outputs").cloned().unwrap_or_else(||json!([])),"import_worker_refs":args.get("import_worker_refs").cloned().unwrap_or_else(||json!([])),"source_task_ref":args.get("source_task_ref").cloned().unwrap_or_else(||json!({}))}));
    sha256_json(&Value::Object(material))
}
fn task_run(args: &Map<String, Value>, root: &Path) -> Result<Value, Value> {
    let id = stable_task_id(args);
    safe_id(&id)?;
    let _lock = lock_task(root, &id)?;
    if task_path(root, &id)?.is_file() {
        let mut task = read_task(root, &id)?;
        if args.get("objective").is_none() && args.get("intent").is_none() {
            assert_mutation_scope(&task, args, root)?;
            task = advance_value(task, root)?;
        } else if args.get("idempotency_key").is_some() {
            let fingerprint = request_fingerprint(args, root, &id);
            if task.get("request_fingerprint").and_then(Value::as_str) != Some(fingerprint.as_str())
            {
                return Err(
                    json!({"schema":"narada.delegated_task.error.v1","code":"delegated_task_idempotency_conflict","message":"delegated_task_idempotency_conflict","task_id":id,"existing_request_fingerprint":task.get("request_fingerprint"),"request_fingerprint":fingerprint}),
                );
            }
        }
        return Ok(
            json!({"schema":"narada.delegated_task.run.v1","status":"existing","request_status":"existing","execution_status":task["status"],"created":false,"task_id":id,"task_status":task["status"],"summary":task["summary"]}),
        );
    }
    let objective = objective(args)?;
    let admission = validate(args, root)?;
    if admission.get("status").and_then(Value::as_str) != Some("ok") {
        return Err(
            json!({"schema":"narada.delegated_task.error.v1","code":"delegated_task_validation_failed","message":"delegated_task_validation_failed","diagnostics":admission["diagnostics"]}),
        );
    }
    let created = now();
    let workflow = normalize_workflow(args.get("workflow"));
    let step_states = initial_step_states(&workflow);
    let site = current_site_id(root);
    let fingerprint = request_fingerprint(args, root, &id);
    let mut task = json!({"schema":"narada.delegated_task.task.v1","task_id":id,"owner_site_id":site,"owner_site_root":if site.is_some(){json!(root.to_string_lossy())}else{Value::Null},"created_by_site_id":site,"visibility_scope":if site.is_some(){"site"}else{"user_global"},"task_root_scope":"site_root","status":"accepted_for_execution","objective":objective,"request_fingerprint":fingerprint,"created_at":created,"updated_at":created,"cancelled_at":null,"idempotency_key":args.get("idempotency_key"),"constraints":normalized_constraints(args.get("constraints")),"workflow":workflow,"execution":normalized_execution(args.get("execution")),"acceptance":args.get("acceptance").cloned().unwrap_or_else(||json!({})),"result":{"schema":"narada.delegated_task.handoff.v1","acceptance_verdict":"pending","step_states":step_states,"worker_refs":[],"worker_outputs":[],"residual_risks":[],"observed_incoherencies":[],"verification":[],"changed_files":[]},"summary":null});
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
fn valid_condition(condition: &str) -> bool {
    let value = condition.trim();
    if matches!(
        value,
        "always" | "on_success" | "on_failure" | "review_failed" | "no_residual_risks"
    ) {
        return true;
    }
    if let Some(suffix) = value.strip_prefix("acceptance:") {
        return !suffix.trim().is_empty();
    }
    if let Some(suffix) = value.strip_prefix("result_has:") {
        return !suffix.trim().is_empty();
    }
    let parts = value.split(':').collect::<Vec<_>>();
    if parts.first() == Some(&"step") {
        return parts.len() == 3
            && !parts[1].is_empty()
            && matches!(
                parts[2],
                "pending" | "running" | "completed" | "failed" | "skipped" | "blocked" | "noted"
            );
    }
    if parts.first() == Some(&"kind") {
        return parts.len() == 3 && !parts[1].is_empty() && !parts[2].is_empty();
    }
    parse_condition_call(value).is_some_and(|(name, args)| {
        ((name == "all" || name == "any") && args.len() >= 2 || name == "not" && args.len() == 1)
            && args.into_iter().all(valid_condition)
    })
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
fn max_concurrency(task: &Value) -> usize {
    task.pointer("/constraints/max_concurrency")
        .or_else(|| task.pointer("/execution/max_concurrency"))
        .and_then(Value::as_u64)
        .unwrap_or(10)
        .clamp(1, 32) as usize
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
                            matches!(
                                step_status(task, dependency),
                                Some("completed" | "skipped" | "noted")
                            )
                        })
                })
                .unwrap_or(true);
            (ready && condition_passes(step.get("if").and_then(Value::as_str), task))
                .then(|| id.to_string())
        })
        .collect()
}
fn advance_value(mut task: Value, root: &Path) -> Result<Value, Value> {
    let constraints_changed = normalize_persisted_constraints(&mut task);
    if matches!(
        task.get("status").and_then(Value::as_str),
        Some("completed" | "failed" | "cancelled")
    ) {
        if constraints_changed {
            task["updated_at"] = json!(now());
            write_task(root, &task)?;
        }
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
        let worker_run = status.get("run").cloned().unwrap_or(Value::Null);
        if worker == "completed" {
            record_worker_terminal(&mut task, step_id, &run_id, "completed", &worker_run);
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
            record_worker_terminal(&mut task, step_id, &run_id, &worker, &worker_run);
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
    let (current_acceptance, current_checks) = acceptance_verdict(&task, root);
    task["result"]["acceptance_verdict"] = json!(current_acceptance);
    task["result"]["acceptance_checks"] = json!(current_checks);
    if task.get("status").and_then(Value::as_str) != Some("failed") {
        let steps = task
            .pointer("/workflow/steps")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        loop {
            let mut local_progress = false;
            for step in &steps {
                let Some(step_id) = step.get("id").and_then(Value::as_str) else {
                    continue;
                };
                if step_status(&task, step_id) != Some("pending") {
                    continue;
                }
                let blocked = step
                    .get("depends_on")
                    .and_then(Value::as_array)
                    .map(|items| {
                        items
                            .iter()
                            .filter_map(Value::as_str)
                            .filter(|dependency| {
                                matches!(step_status(&task, dependency), Some("failed" | "blocked"))
                            })
                            .map(str::to_string)
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default();
                if !blocked.is_empty() {
                    task["result"]["step_states"][step_id]["status"] = json!("blocked");
                    task["result"]["step_states"][step_id]["blocked_by"] = json!(blocked);
                    task["result"]["step_states"][step_id]["finished_at"] = json!(now());
                    append_event(
                        root,
                        &id,
                        "step_blocked",
                        json!({"step_id":step_id,"blocked_by":task["result"]["step_states"][step_id]["blocked_by"]}),
                    )?;
                    local_progress = true;
                    continue;
                }
                let dependencies_ready = step
                    .get("depends_on")
                    .and_then(Value::as_array)
                    .map(|items| {
                        items.iter().filter_map(Value::as_str).all(|dependency| {
                            matches!(
                                step_status(&task, dependency),
                                Some("completed" | "skipped" | "noted")
                            )
                        })
                    })
                    .unwrap_or(true);
                if !dependencies_ready {
                    continue;
                }
                if !condition_passes(step.get("if").and_then(Value::as_str), &task) {
                    task["result"]["step_states"][step_id]["status"] = json!("skipped");
                    task["result"]["step_states"][step_id]["finished_at"] = json!(now());
                    append_event(
                        root,
                        &id,
                        "step_skipped",
                        json!({"step_id":step_id,"condition":step.get("if")}),
                    )?;
                    local_progress = true;
                    continue;
                }
                let kind = step.get("kind").and_then(Value::as_str).unwrap_or("worker");
                if matches!(kind, "gate" | "join" | "note") {
                    task["result"]["step_states"][step_id]["status"] =
                        json!(if kind == "note" { "noted" } else { "completed" });
                    task["result"]["step_states"][step_id]["finished_at"] = json!(now());
                    append_event(
                        root,
                        &id,
                        if kind == "gate" {
                            "step_gate_evaluated"
                        } else if kind == "join" {
                            "step_join_completed"
                        } else {
                            "step_recorded"
                        },
                        json!({"step_id":step_id,"kind":kind,"authority_gate":step.get("authority_gate"),"executed":false}),
                    )?;
                    local_progress = true;
                }
            }
            if !local_progress {
                break;
            }
        }
        let ready = ready_step_ids(&task);
        let mut active = step_ids
            .iter()
            .filter(|step_id| step_status(&task, step_id) == Some("running"))
            .count();
        let concurrency = max_concurrency(&task);
        for step_id in ready {
            let Some(step) = steps
                .iter()
                .find(|step| step.get("id").and_then(Value::as_str) == Some(step_id.as_str()))
            else {
                continue;
            };
            let kind = step.get("kind").and_then(Value::as_str).unwrap_or("worker");
            if matches!(kind, "gate" | "join" | "note") {
                continue;
            }
            if active >= concurrency {
                continue;
            }
            let instruction = step
                .get("instruction")
                .and_then(Value::as_str)
                .or_else(|| task.get("objective").and_then(Value::as_str))
                .unwrap_or_default();
            let constraints = if step.get("constraints").is_some() {
                normalized_constraints(step.get("constraints"))
            } else {
                normalized_constraints(task.get("constraints"))
            };
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
            active += 1;
        }
        if !step_ids.is_empty()
            && step_ids.iter().all(|step_id| {
                matches!(
                    step_status(&task, step_id),
                    Some("completed" | "skipped" | "noted")
                )
            })
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
        } else if step_ids
            .iter()
            .any(|step_id| matches!(step_status(&task, step_id), Some("failed" | "blocked")))
            && !step_ids
                .iter()
                .any(|step_id| matches!(step_status(&task, step_id), Some("pending" | "running")))
        {
            task["status"] = json!("failed");
            task["result"]["acceptance_verdict"] = json!("failed");
            append_event(
                root,
                &id,
                "task_failed",
                json!({"reason":"blocked_or_failed_steps"}),
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
    json!({"type":"object","properties":{"task_id":{"type":"string"}},"required":if required {json!(["task_id"])} else {json!([])},"additionalProperties":false})
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
    fn delegated_task_defaults_cognition_to_low() {
        assert_eq!(policy(Path::new("."))["default_cognition"], "low");
        assert_eq!(guidance(&Map::new())["cognition"]["default"], "low");
        assert_eq!(
            list_tools()
                .iter()
                .find(|tool| tool["name"] == "delegated_task_validate")
                .expect("validate tool")["inputSchema"]["properties"]["constraints"]["properties"]["cognition"]["default"],
            "low"
        );
        let root = std::env::temp_dir().join(format!(
            "narada-delegated-task-default-cognition-{}",
            uuid::Uuid::new_v4()
        ));
        task_run(
            json!({"task_id":"task_default_cognition","objective":"demo","execution":{"start":false}})
                .as_object()
                .unwrap(),
            &root,
        )
        .expect("run");
        let task = read_task(&root, "task_default_cognition").expect("task");
        assert_eq!(task["constraints"]["cognition"], "low");
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn completed_worker_output_is_structured_and_bounded() {
        let run = json!({"summary":"{\"repository\":\"marici\",\"branch\":\"main\"}"});
        let output = worker_output_from_run(&run).expect("worker output");
        assert_eq!(output["structured_output"]["repository"], "marici");
        assert_eq!(output["structured_output"]["branch"], "main");
        assert_eq!(output["truncated"], false);
    }

    #[test]
    fn terminal_worker_projection_updates_reference_and_handoff() {
        let mut task = json!({"result":{"step_states":{"inspect":{"status":"running"}},"worker_refs":[{"step_id":"inspect","run_id":"run-1","status":"running"}],"worker_outputs":[]}});
        record_worker_terminal(
            &mut task,
            "inspect",
            "run-1",
            "completed",
            &json!({"summary":"{\"repository\":\"marici\",\"branch\":\"main\"}"}),
        );
        assert_eq!(task["result"]["worker_refs"][0]["status"], "completed");
        assert_eq!(task["result"]["worker_refs"][0]["output"]["structured_output"]["branch"], "main");
        assert_eq!(task["result"]["step_states"]["inspect"]["worker_status"], "completed");
        assert_eq!(task["result"]["worker_outputs"][0]["output"]["structured_output"]["repository"], "marici");
    }

    #[test]
    fn delegated_task_constraints_are_closed_and_validation_reports_resolution() {
        let schema = constraints_schema();
        assert_eq!(schema["additionalProperties"], false);
        assert_eq!(schema["properties"]["preflight_paths"]["items"]["additionalProperties"], false);
        let root = std::env::temp_dir().join(format!("narada-delegated-task-validate-{}", uuid::Uuid::new_v4()));
        let invalid = validate(
            &json!({"objective":"probe","constraints":{"unknown_field":"x"}}).as_object().unwrap(),
            &root,
        ).expect("validation response");
        assert_eq!(invalid["valid"], false);
        assert_eq!(invalid["diagnostics"][0]["code"], "unknown_constraint");
        let defaulted = validate(&json!({"objective":"probe"}).as_object().unwrap(), &root).expect("default validation");
        assert_eq!(defaulted["valid"], true);
        assert_eq!(defaulted["resolved_constraints"]["cognition"], "low");
    }

    #[test]
    fn legacy_task_constraints_are_normalized_on_durable_readback() {
        let root = std::env::temp_dir().join(format!("narada-delegated-task-legacy-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(root.join("tasks/legacy_task")).expect("task root");
        fs::write(
            root.join("tasks/legacy_task/task.json"),
            serde_json::to_vec(&json!({"task_id":"legacy_task","status":"completed","objective":"legacy","constraints":{},"updated_at":"2026-01-01T00:00:00Z","result":{}})).expect("encode legacy task"),
        ).expect("legacy task");
        task_run(&json!({"task_id":"legacy_task","allow_cross_site":true}).as_object().unwrap(), &root).expect("normalize");
        let task = read_task(&root, "legacy_task").expect("readback");
        assert_eq!(task["constraints"]["cognition"], "low");
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn mutating_tool_contracts_are_closed_named_and_callable() {
        let tools = list_tools();
        for name in MUTATING {
            let tool = tools.iter().find(|tool| tool["name"] == *name).expect("tool");
            assert_eq!(tool["inputSchema"]["additionalProperties"], false, "{name}");
            assert!(tool["inputSchema"]["properties"].as_object().is_some_and(|value| !value.is_empty()), "{name}");
        }
        let run = tools.iter().find(|tool| tool["name"] == "delegated_task_run").unwrap();
        for field in ["objective","intent","workflow","execution","execution_binding","idempotency_key"] { assert!(run["inputSchema"]["properties"].get(field).is_some(), "{field}"); }
        let wait = tools.iter().find(|tool| tool["name"] == "delegated_task_wait").unwrap();
        assert_eq!(wait["annotations"]["readOnlyHint"], false);
        assert!(wait["inputSchema"]["properties"].get("timeout_ms").is_some());
        assert!(wait["inputSchema"]["properties"].get("allow_cross_site").is_some());
    }

    #[test]
    fn native_delegated_task_reads_durable_json_without_execution() {
        let root =
            std::env::temp_dir().join(format!("narada-delegated-task-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(root.join("tasks/task_a")).expect("root");
        fs::write(root.join("tasks/task_a/task.json"), r#"{"task_id":"task_a","status":"completed","objective":"demo","updated_at":"2026-01-01T00:00:00Z","result":{"acceptance_verdict":"accepted"}}"#).expect("task");
        let listed = tasks_list(
            &json!({"limit":1,"view":"all","site_scope":"all_sites"})
                .as_object()
                .unwrap(),
            &root,
        )
        .expect("list");
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

    #[test]
    fn native_delegated_task_bounds_concurrency_and_waits_on_terminal_state() {
        let root = std::env::temp_dir().join(format!(
            "narada-delegated-task-wait-{}",
            uuid::Uuid::new_v4()
        ));
        let task = json!({"schema":"narada.delegated_task.task.v1","task_id":"task_terminal","owner_site_id":root.file_name().and_then(|v|v.to_str()),"visibility_scope":"site","status":"completed","objective":"done","constraints":{"max_concurrency":99},"result":{}});
        write_task(&root, &task).expect("task");
        assert_eq!(max_concurrency(&task), 32);
        let waited = task_wait(
            json!({"task_id":"task_terminal","timeout_ms":0})
                .as_object()
                .unwrap(),
            &root,
        )
        .expect("wait");
        assert_eq!(waited["status"], "finished");
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn native_delegated_task_keeps_local_steps_out_of_worker_schedule() {
        let task = json!({"workflow":{"steps":[{"id":"gate","kind":"gate"},{"id":"join","kind":"join"},{"id":"note","kind":"note"}]},"result":{"step_states":{"gate":{"status":"pending"},"join":{"status":"pending"},"note":{"status":"pending"}}}});
        assert_eq!(ready_step_ids(&task), vec!["gate", "join", "note"]);
        for step in task["workflow"]["steps"].as_array().unwrap() {
            assert!(matches!(
                step["kind"].as_str(),
                Some("gate" | "join" | "note")
            ));
        }
    }

    #[test]
    fn native_delegated_task_stale_lock_has_one_reclaim_winner() {
        let root = std::env::temp_dir().join(format!(
            "narada-delegated-task-reclaim-{}",
            uuid::Uuid::new_v4()
        ));
        let lock = root.join("mutation.lockdir");
        fs::create_dir_all(&lock).expect("stale lock");
        fs::write(lock.join("owner.json"), "{}").expect("owner");
        let barrier = Arc::new(std::sync::Barrier::new(3));
        let contenders = (0..2)
            .map(|_| {
                let lock = lock.clone();
                let barrier = Arc::clone(&barrier);
                std::thread::spawn(move || {
                    barrier.wait();
                    reclaim_stale_lock(&lock)
                })
            })
            .collect::<Vec<_>>();
        barrier.wait();
        let winners = contenders
            .into_iter()
            .map(|contender| contender.join().expect("contender"))
            .filter(|won| *won)
            .count();
        assert_eq!(winners, 1);
        assert!(!lock.exists());
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn native_delegated_task_rejects_conflicting_idempotent_replay() {
        let root = std::env::temp_dir().join(format!(
            "narada-delegated-task-idempotency-{}",
            uuid::Uuid::new_v4()
        ));
        let first =
            json!({"objective":"first","execution":{"start":false},"idempotency_key":"stable"});
        task_run(first.as_object().unwrap(), &root).expect("first");
        let conflict =
            json!({"objective":"different","execution":{"start":false},"idempotency_key":"stable"});
        let error = task_run(conflict.as_object().unwrap(), &root).expect_err("conflict");
        assert_eq!(error["code"], "delegated_task_idempotency_conflict");
        let task_id = stable_task_id(first.as_object().unwrap());
        task_cancel(
            json!({"task_id":task_id}).as_object().unwrap(),
            &root,
            false,
        )
        .expect("terminal replay fixture");
        let replay = task_run(
            json!({"idempotency_key":"stable","execution":{"start":false}})
                .as_object()
                .unwrap(),
            &root,
        )
        .expect("identity replay");
        assert_eq!(replay["status"], "existing");
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn native_delegated_task_list_honors_lifecycle_views() {
        let root = std::env::temp_dir().join(format!(
            "narada-delegated-task-list-{}",
            uuid::Uuid::new_v4()
        ));
        task_run(
            json!({"task_id":"active","objective":"active","execution":{"start":false}})
                .as_object()
                .unwrap(),
            &root,
        )
        .expect("active");
        task_run(
            json!({"task_id":"terminal","objective":"terminal","execution":{"start":false}})
                .as_object()
                .unwrap(),
            &root,
        )
        .expect("terminal");
        task_cancel(
            json!({"task_id":"terminal"}).as_object().unwrap(),
            &root,
            false,
        )
        .expect("cancel");
        let active = tasks_list(json!({"view":"active_queue"}).as_object().unwrap(), &root)
            .expect("active list");
        let history =
            tasks_list(json!({"view":"history"}).as_object().unwrap(), &root).expect("history");
        assert_eq!(active["count"], 1);
        assert_eq!(history["count"], 1);
        fs::remove_dir_all(root).expect("cleanup");
    }
}
