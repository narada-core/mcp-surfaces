use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{
    atomic::{AtomicBool, AtomicUsize, Ordering},
    Arc, Mutex,
};
use std::thread::JoinHandle;

const SERVER_NAME: &str = "delegated-task-mcp";
const DEFAULT_COGNITION: &str = "low";
const MAX_ITEMS: usize = 200;
const MAX_FILE_BYTES: u64 = 256_000;
const MAX_WORKER_OUTPUT_BYTES: usize = 32_000;
const MAX_IMPORTED_TASK_OUTPUT_BYTES: usize = 32_000;
const MAX_VALIDATED_REQUEST_BYTES: usize = 64_000;
const MUTATING: &[&str] = &[
    "delegated_task_execute",
    "delegated_task_execute_batch",
    "delegated_task_run",
    "delegated_task_advance",
    "delegated_task_cancel",
    "delegated_task_acknowledge",
    "delegated_task_parent_takeover",
];
// Loader-mediated child calls may have a 181s transport lifetime. Keep every
// synchronous lifecycle wait comfortably below it; workers may continue under
// their independent max_run_ms and are recovered by durable task_id.
const MAX_TRANSPORT_SAFE_WAIT_MS: u64 = 120_000;

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
            json!({"type":"object","properties":{"template_id":{"type":"string"},"mode":{"type":"string","enum":["compact","detail"],"default":"compact"}},"additionalProperties":false}),
        ),
        (
            "delegated_task_validate",
            "Validate delegated task input and persist a reusable validated-request reference without running a task.",
            json!({"type":"object","properties":{"objective":{"type":"string"},"workflow":{"type":"object"},"constraints":constraints_schema(),"acceptance":{"type":"object"},"execution":{"type":"object"},"execution_binding":{"type":"object"}},"additionalProperties":false}),
        ),
        (
            "delegated_task_execute",
            "Validate, run, and wait through the same durable gates in one bounded call.",
            mutation_schema("delegated_task_execute"),
        ),
        (
            "delegated_task_execute_batch",
            "Execute several independent delegated tasks with bounded concurrency and ordered results.",
            batch_execute_schema(),
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
            "Return a secondary durable readback; delegated_task_wait is the canonical terminal handoff.",
            id_schema(true),
        ),
        (
            "delegated_task_summary",
            "Return a compact derived human review summary and acceptance evidence from durable state.",
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
    if let Some(validate_tool) = tools.iter_mut().find(|tool| tool["name"] == "delegated_task_validate") {
        validate_tool["annotations"]["readOnlyHint"] = json!(false);
        validate_tool["annotations"]["destructiveHint"] = json!(false);
        validate_tool["annotations"]["stateChangingHint"] = json!(true);
    }
    for name in ["delegated_task_execute", "delegated_task_execute_batch"] {
        if let Some(execute_tool) = tools.iter_mut().find(|tool| tool["name"] == name) {
            execute_tool["annotations"]["readOnlyHint"] = json!(false);
            execute_tool["annotations"]["destructiveHint"] = json!(false);
            execute_tool["annotations"]["stateChangingHint"] = json!(true);
        }
    }
    for name in MUTATING.iter().filter(|name| !matches!(**name, "delegated_task_execute" | "delegated_task_execute_batch")) {
        tools.push(tool(
            name,
            "Delegated task mutation remains owned by the worker/task authority.",
            mutation_schema(name),
            false,
        ));
    }
    tools.push(tool(
        "delegated_task_wait",
        "Advance and wait for a delegated task; on terminal status this is the canonical complete handoff.",
        json!({"type":"object","properties":{"task_id":{"type":"string"},"timeout_ms":{"type":"integer","minimum":0,"maximum":MAX_TRANSPORT_SAFE_WAIT_MS,"default":30000,"description":"Bounded synchronous wait below the loader transport lifetime. A timeout returns durable task_id for repeated wait/status recovery; worker max_run_ms is independent."},"poll_ms":{"type":"integer","minimum":50,"maximum":30000,"default":5000},"expected_owner_site_id":{"type":"string"},"allow_cross_site":{"type":"boolean","default":false}},"required":["task_id"],"additionalProperties":false}),
        false,
    ));
    tools
}

fn batch_execute_schema() -> Value {
    json!({
        "type":"object",
        "properties":{
            "items":{
                "type":"array",
                "minItems":1,
                "maxItems":16,
                "items":mutation_schema("delegated_task_execute")
            },
            "max_concurrency":{"type":"integer","minimum":1,"maximum":8,"default":2},
            "compact":{"type":"boolean","default":false,"description":"Return one lean terminal projection per task; use details_ref for complete readback."}
        },
        "required":["items"],
        "additionalProperties":false
    })
}

fn mutation_schema(name: &str) -> Value {
    let scope = json!({"expected_owner_site_id":{"type":"string"},"allow_cross_site":{"type":"boolean","default":false}});
    let mut properties = scope.as_object().cloned().unwrap_or_default();
    match name {
        "delegated_task_execute" => {
            for field in ["objective","idempotency_key"] { properties.insert(field.into(),json!({"type":"string"})); }
            for field in ["workflow","acceptance","execution","execution_binding"] { properties.insert(field.into(),json!({"type":"object"})); }
            properties.insert("constraints".into(), constraints_schema());
            properties.insert("timeout_ms".into(),json!({"type":"integer","minimum":0,"maximum":MAX_TRANSPORT_SAFE_WAIT_MS,"default":30000,"description":"Transport-safe synchronous wait. Longer worker executions continue durably and return task_id for recovery."}));
            properties.insert("poll_ms".into(),json!({"type":"integer","minimum":50,"maximum":30000,"default":5000}));
            json!({"type":"object","properties":properties,"required":["objective","idempotency_key"],"additionalProperties":false})
        }
        "delegated_task_run" => {
            for field in ["objective","idempotency_key","task_id"] { properties.insert(field.into(),json!({"type":"string"})); }
            for field in ["intent","workflow","acceptance","result_policy","execution","execution_binding","source_task_ref"] { properties.insert(field.into(),json!({"type":"object"})); }
            properties.insert("validated_request_ref".into(),json!({"type":"string","minLength":6,"maxLength":256,"pattern":"^vr_[A-Za-z0-9-]+$"}));
            properties.insert("constraints".into(), constraints_schema());
            for field in ["depends_on_task_ids","import_task_outputs","import_worker_refs"] { properties.insert(field.into(),json!({"type":"array","items":{"type":"string"}})); }
            json!({"type":"object","properties":properties,"anyOf":[{"required":["objective"]},{"required":["intent"]},{"required":["task_id"]},{"required":["validated_request_ref"]}],"additionalProperties":false})
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

pub fn call_tool(
    name: &str,
    args: &Map<String, Value>,
    root: &Path,
    allowed_roots: &[PathBuf],
) -> Result<Value, Value> {
    match name {
        "delegated_task_guidance" => Ok(guidance(args)),
        "delegated_task_policy_inspect" => Ok(policy_with_roots(root, allowed_roots)),
        "delegated_task_template_catalog" => Ok(template_catalog(args)),
        "delegated_task_validate" => validate(args, root),
        "delegated_task_execute" => task_execute(args, root, allowed_roots),
        "delegated_task_execute_batch" => task_execute_batch(args, root, allowed_roots),
        "delegated_tasks_list" => tasks_list(args, root),
        "delegated_task_status" => task_status_with_roots(args, root, allowed_roots),
        "delegated_task_result" => task_result(args, root),
        "delegated_task_summary" => task_summary(args, root),
        "delegated_task_events" => task_events(args, root),
        "delegated_task_wait" => task_wait_with_roots(args, root, allowed_roots),
        "delegated_task_run" => task_run_with_roots(args, root, allowed_roots),
        "delegated_task_advance" => task_advance_with_roots(args, root, allowed_roots),
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
    json!({"schema":"narada.mcp_surface.guidance.v0","status":"ok","surface_id":"delegated-task","guidance_tool":"delegated_task_guidance","purpose":"Validate, execute, and inspect durable delegated task workflows through native authority.","requested":{"workflow":args.get("workflow").cloned().unwrap_or(Value::Null),"tool":args.get("tool").cloned().unwrap_or(Value::Null)},"cognition":{"default":"low","omitted_constraint_behavior":"constraints.cognition resolves to low","mapping_surface":"worker-delegation","mapping_tool":"worker_cognition_defaults_inspect"},"first_use":["Call delegated_task_policy_inspect first.","Omitted constraints.cognition resolves to low; inspect worker-delegation's worker_cognition_defaults_inspect for the current model and reasoning-effort mapping.","Use delegated_task_execute for one bounded validate-run-wait workflow, or delegated_task_execute_batch for up to 16 independent tasks with explicit bounded concurrency; use the explicit validate, run, and wait tools when an agent must inspect or retain each lifecycle transition separately. Cross-task DAGs use delegated_task_run with depends_on_task_ids and explicit import_task_outputs/import_worker_refs; waiting or refreshing a descendant reconciles its dependency closure automatically, while failed or malformed predecessors durably block descendants.","Call delegated_task_validate once, then pass its durable validated_request_ref to delegated_task_run so objective, constraints, workflow, and binding fields are not duplicated. request_valid proves only structural validity. execution_preflight_pending=true means worker-delegation.worker_run still must perform the authoritative existence, scope, and bounded native read check immediately before launch. For read-only file evidence, include each target path with access=read; the native authority injects its bounded content and the worker must not fall back to a shell read.","Prefer the site-scoped mcp_loader_call_binding_tool(binding_id, site_root, surface_id, tool_name, arguments) path when reopening a surface after a loader restart.","delegated_task_wait is the canonical terminal handoff and includes the complete compact result; delegated_task_result is a secondary durable readback.","Use bounded list/status/result/events readback.","Use explicit cancellation and disposition tools for lifecycle mutations."],"binding_reopen":{"tool_name":"mcp_loader_call_binding_tool","arguments":{"site_root":"<site_root>","binding_id":"<binding_id>","surface_id":"delegated-task","tool_name":"<child_tool>","arguments":{}}},"boundaries":["Native authority owns task.json/events.jsonl and validated request records under the bounded task root.","Worker launches cross the native worker-delegation authority boundary.","Cross-site ownership remains server-bound authority."]})
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
fn validated_requests_dir(root: &Path) -> PathBuf {
    task_root(root).join("validated-requests")
}
fn validated_request_path(root: &Path, reference: &str) -> Result<PathBuf, Value> {
    let reference = reference.trim();
    if !reference.starts_with("vr_") {
        return Err(error(
            "validated_request_ref_invalid",
            "validated_request_ref_invalid",
        ));
    }
    safe_id(reference)?;
    Ok(validated_requests_dir(root).join(format!("{reference}.json")))
}
fn write_validated_request(root: &Path, record: &Value) -> Result<(), Value> {
    let reference = record
        .get("validated_request_ref")
        .and_then(Value::as_str)
        .ok_or_else(|| error("validated_request_ref_invalid", "validated_request_ref_invalid"))?;
    let path = validated_request_path(root, reference)?;
    let bytes = serde_json::to_vec_pretty(record)
        .map_err(|_| error("validated_request_write_failed", "validated_request_write_failed"))?;
    if bytes.len() > MAX_VALIDATED_REQUEST_BYTES {
        return Err(error(
            "validated_request_too_large",
            "validated_request_too_large",
        ));
    }
    fs::create_dir_all(path.parent().unwrap())
        .map_err(|_| error("validated_request_write_failed", "validated_request_write_failed"))?;
    fs::write(path, bytes)
        .map_err(|_| error("validated_request_write_failed", "validated_request_write_failed"))
}
fn read_validated_request(root: &Path, reference: &str) -> Result<Value, Value> {
    let path = validated_request_path(root, reference)?;
    let size = fs::metadata(&path)
        .map_err(|_| error("validated_request_not_found", "validated_request_not_found"))?
        .len();
    if size > MAX_VALIDATED_REQUEST_BYTES as u64 {
        return Err(error(
            "validated_request_too_large",
            "validated_request_too_large",
        ));
    }
    let text = fs::read_to_string(path)
        .map_err(|_| error("validated_request_not_found", "validated_request_not_found"))?;
    let record: Value = serde_json::from_str(&text)
        .map_err(|_| error("validated_request_invalid_json", "validated_request_invalid_json"))?;
    if record.get("schema").and_then(Value::as_str)
        != Some("narada.delegated_task.validated_request.v1")
        || record.get("validated_request_ref").and_then(Value::as_str) != Some(reference)
    {
        return Err(error(
            "validated_request_invalid",
            "validated_request_invalid",
        ));
    }
    Ok(record)
}
fn materialize_validated_request(
    args: &Map<String, Value>,
    root: &Path,
) -> Result<Map<String, Value>, Value> {
    let Some(reference) = args
        .get("validated_request_ref")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return Ok(args.clone());
    };
    let record = read_validated_request(root, reference)?;
    let mut merged = record
        .get("request")
        .and_then(Value::as_object)
        .cloned()
        .ok_or_else(|| error("validated_request_invalid", "validated_request_invalid"))?;
    for (key, value) in args {
        if key == "validated_request_ref" {
            continue;
        }
        if !matches!(key.as_str(), "task_id" | "idempotency_key" | "expected_owner_site_id" | "allow_cross_site") {
            return Err(json!({"schema":"narada.delegated_task.error.v1","code":"validated_request_drift","message":"validated_request_drift","validated_request_ref":reference,"field":key,"remediation":"Pass only validated_request_ref and optional task identity/scope fields to delegated_task_run."}));
        }
        if let Some(existing) = merged.get(key) {
            if existing != value {
                return Err(json!({"schema":"narada.delegated_task.error.v1","code":"validated_request_drift","message":"validated_request_drift","validated_request_ref":reference,"field":key}));
            }
        } else {
            merged.insert(key.clone(), value.clone());
        }
    }
    merged.insert("validated_request_ref".into(), json!(reference));
    Ok(merged)
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

#[cfg(test)]
fn policy(root: &Path) -> Value {
    policy_with_roots(root, &[root.to_path_buf()])
}
fn policy_with_roots(root: &Path, allowed_roots: &[PathBuf]) -> Value {
    json!({"schema":"narada.delegated_task.policy.v1","status":"ok","server_name":SERVER_NAME,"task_root":task_root(root).to_string_lossy(),"site_root":root.to_string_lossy(),"allowed_roots":allowed_roots.iter().map(|path| path.to_string_lossy().to_string()).collect::<Vec<_>>(),"default_cognition":DEFAULT_COGNITION,"list_defaults":{"view":"active_queue","site_scope":"current_site"},"workflow_engine":"native_authority","worker_execution":"native_worker_authority","result_compaction":{"max_worker_refs":50,"max_list_items":200}})
}

fn assessment_output_schema() -> Value {
    json!({"schema":"narada.delegated_task.output_schema.v1","name":"task_executability_assessment_v1","version":1,"required":["dimensions","first_actions","reference_resolutions","acceptance_mappings","required_decisions","findings","assessment_result","evaluator_provenance"],"fields":{"dimensions":"array<object>","first_actions":"array<object>","reference_resolutions":"array<object>","acceptance_mappings":"array<object>","required_decisions":"array<object>","findings":"array<object>","assessment_result":"object {status: executable|blocked|not_executable, implementation_ready: boolean, blockers: array<object>, reason: string when not_executable}","evaluator_provenance":"object"},"conditional_rules":[{"when":"assessment_result.status=executable","requires":["assessment_result.implementation_ready=true","assessment_result.blockers=[]"]},{"when":"assessment_result.status=blocked","requires":["assessment_result.implementation_ready=false","assessment_result.blockers nonempty"]},{"when":"assessment_result.status=not_executable","requires":["assessment_result.implementation_ready=false","assessment_result.reason nonempty"]}],"provenance_required":["runtime","provider","model","cognition","profile_version"],"rejection_rules":["missing_required_field","prose_only","invalid_schema","invalid_provenance"]})
}

fn assessment_template() -> Value {
    let output_schema = assessment_output_schema();
    json!({"template_id":"task_executability_assessment_v1","strategy":"task_executability_assessment_v1","title":"Bounded Shoshin task executability assessment","profile_version":"shoshin-task-executability-v1","purpose":"Assess one canonical task snapshot without changing it.","idempotency":{"schema":"narada.task.executability.idempotency.v1","inputs":["request_id","task_digest","environment_digest","profile_version"],"formula":"sha256(canonical_json({request_id, task_digest, environment_digest, profile_version}))"},"bounds":{"authority":"read","cognition":"low","runtime":"narada-agent-runtime-server","max_worker_runs":1,"max_run_ms":300000,"max_retries":0,"max_result_items":32,"max_events":32,"write_set":[]},"result_policy":{"expose_worker_refs":true,"compact_completed_worker_refs":true,"max_events":32,"max_worker_refs":1,"max_result_items":32},"output_schema":output_schema,"milestones":[{"id":"assessment","title":"Assess canonical task snapshot","step_ids":["assessment"]}],"steps":[{"id":"assessment","kind":"worker","profile":"shoshin-task-executability-v1","milestone_id":"assessment","write_set":[],"constraints":{"authority":"read","cognition":"low","runtime":"narada-agent-runtime-server","max_run_ms":300000,"max_retries":0,"max_concurrency":1,"wait_for_completion":false,"resumable":false,"required_mcp_tools":[],"preflight_paths":[],"overrides":{"skip_git_repo_check":true}},"output_schema":output_schema}],"worker_delegation_contract":{"surface_id":"worker-delegation","caller_sets_worker_constraints":true,"worker_run_is_child_execution":true,"required_worker_output_fields":["summary","structured_outputs","verification","target_state_changed"],"forbidden_authorities":["write","command"],"required_structured_output":"task_executability_assessment_v1"}})
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

fn compact_template(template: &Value) -> Value {
    let stages = template
        .get("milestones")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .map(|item| {
                    item.get("title")
                        .or_else(|| item.get("id"))
                        .cloned()
                        .unwrap_or(Value::Null)
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    json!({
        "template_id":template.get("template_id"),
        "title":template.get("title"),
        "stages":stages,
        "authority":if template.get("authority_gates").is_some(){"native task authority with explicit publication gates"}else{"native worker authority"},
        "best_for":template_fit(template.get("template_id").and_then(Value::as_str)).0,
        "avoid_when":template_fit(template.get("template_id").and_then(Value::as_str)).1,
        "detail_available":true
    })
}

fn template_fit(id: Option<&str>) -> (Value, Value) {
    match id.unwrap_or_default() {
        "task_executability_assessment_v1" => (json!(["bounded pre-implementation feasibility and blocker assessment"]), json!(["the objective is already approved for direct implementation"])),
        "implement" => (json!(["one bounded implementation or verification step"]), json!(["independent review or repair loops are required"])),
        "implement_review" => (json!(["implementation requiring an independent review gate"]), json!(["a single trivial worker result is sufficient"])),
        "research_synthesize" => (json!(["evidence gathering followed by synthesis and review"]), json!(["the task is a deterministic code edit"])),
        "implement_review_repair_verify" => (json!(["high-risk changes needing review, repair, and final verification"]), json!(["latency matters more than redundant assurance"])),
        "commit_push_guarded" => (json!(["explicitly authorized commit and push workflows"]), json!(["publication authority has not been granted"])),
        _ => (json!([]), json!([])),
    }
}

fn template_catalog(args: &Map<String, Value>) -> Value {
    let id = args
        .get("template_id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let mode = args
        .get("mode")
        .and_then(Value::as_str)
        .unwrap_or(if id.is_some() { "detail" } else { "compact" });
    let templates = workflow_templates()
        .into_iter()
        .filter(|value| id.is_none() || value.get("template_id").and_then(Value::as_str) == id)
        .collect::<Vec<_>>();
    let details = mode == "detail" || id.is_some();
    let projected = if details {
        templates.into_iter().map(|mut template| {
            let (best_for, avoid_when) = template_fit(template.get("template_id").and_then(Value::as_str));
            template["best_for"] = best_for;
            template["avoid_when"] = avoid_when;
            template
        }).collect()
    } else {
        templates.iter().map(compact_template).collect::<Vec<_>>()
    };
    json!({"schema":"narada.delegated_task.template_catalog.v1","status":if id.is_some() && projected.is_empty(){"not_found"}else{"ok"},"mode":mode,"template_id":id,"count":projected.len(),"templates":projected})
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
    validate_with_options(args, root, true)
}
fn validate_with_options(
    args: &Map<String, Value>,
    root: &Path,
    persist_reference: bool,
) -> Result<Value, Value> {
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
    diagnostics.extend(external_dependency_diagnostics(args, root));
    let preflight_requested = args
        .get("constraints")
        .and_then(Value::as_object)
        .and_then(|constraints| constraints.get("preflight_paths"))
        .and_then(Value::as_array)
        .is_some_and(|paths| !paths.is_empty())
        || workflow
            .get("steps")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .any(|step| {
                step.get("constraints")
                    .and_then(Value::as_object)
                    .and_then(|constraints| constraints.get("preflight_paths"))
                    .and_then(Value::as_array)
                    .is_some_and(|paths| !paths.is_empty())
            });
    let errors = diagnostics
        .iter()
        .filter_map(|item| item.get("code").and_then(Value::as_str))
        .collect::<Vec<_>>();
    let valid = errors.is_empty();
    let mut response = json!({
        "schema":"narada.delegated_task.validate.v1",
        "status":if diagnostics.is_empty(){"ok"}else{"rejected"},
        "dry_run":true,
        "validation_persisted":false,
        "diagnostics":diagnostics,
        "valid":valid,
        "request_valid":valid,
        "execution_preflight_pending":preflight_requested && valid,
        "task_root":task_root(root).to_string_lossy(),
        "errors":errors,
        "objective":objective,
        "resolved_constraints":normalized_constraints(args.get("constraints")),
        "worker_execution":"not_run",
        "preflight_status":if preflight_requested{"deferred"}else{"not_requested"},
        "preflight_authority":if preflight_requested{"worker-delegation.worker_run"}else{"none"},
        "preflight_remediation":if preflight_requested{"worker_run enforces path existence and scope immediately before launch; validation does not inspect the filesystem"}else{"No preflight paths were requested."}
    });
    if valid && persist_reference {
        let request = Value::Object(args.clone());
        let digest = sha256_json(&request);
        let reference = format!("vr_{}", &digest[..32]);
        let record = json!({
            "schema":"narada.delegated_task.validated_request.v1",
            "validated_request_ref":reference,
            "created_at":now(),
            "site_root":root.to_string_lossy(),
            "owner_site_id":current_site_id(root),
            "request":request,
            "request_digest":digest
            ,"preflight_status":if preflight_requested{"deferred"}else{"not_requested"}
        });
        write_validated_request(root, &record)?;
        response["validated_request_ref"] = json!(reference);
        response["request_digest"] = json!(digest);
        response["validation_persisted"] = json!(true);
    }
    Ok(response)
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
    let tasks = records
        .iter()
        .map(|task| compact_task(task, root))
        .collect::<Vec<_>>();
    Ok(
        json!({"schema":"narada.delegated_task.list.v1","status":"ok","view":view,"site_scope":site_scope,"current_site_id":current,"owner_site_id":owner_filter,"count":tasks.len(),"total_scoped_count":total,"limit":limit,"include_active":include_active,"include_terminal":include_terminal,"include_acknowledged":include_ack,"tasks":tasks}),
    )
}
fn concise_value(value: &Value) -> String {
    let text = match value {
        Value::String(value) => value.clone(),
        Value::Null => "null".into(),
        Value::Bool(value) => value.to_string(),
        Value::Number(value) => value.to_string(),
        Value::Array(values) => format!("[{} items]", values.len()),
        Value::Object(values) => format!("{{{} fields}}", values.len()),
    };
    truncate_summary(&text, 160)
}
fn structured_output_summary(value: &Value) -> String {
    if let Some(summary) = value
        .get("summary")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|summary| !summary.is_empty())
    {
        return truncate_summary(summary, 512);
    }
    match value {
        Value::Object(fields) => {
            let parts = fields
                .iter()
                .take(4)
                .map(|(key, value)| format!("{key}={}", concise_value(value)))
                .collect::<Vec<_>>();
            if parts.is_empty() {
                format!("{} fields", fields.len())
            } else {
                parts.join(", ")
            }
        }
        Value::Array(values) => format!("{} items", values.len()),
        _ => concise_value(value),
    }
}
fn diagnostics_prefix(text: &str) -> Option<String> {
    let trimmed = text.trim();
    let boundary = trimmed
        .find("```")
        .or_else(|| trimmed.char_indices().find_map(|(index, character)| {
            matches!(character, '{' | '[').then_some(index)
        }))?;
    if boundary == 0 {
        return None;
    }
    let prefix = trimmed[..boundary].trim();
    (!prefix.is_empty()).then(|| prefix.chars().take(2000).collect())
}
fn final_step_projection(task: &Value) -> Value {
    let outputs = task
        .pointer("/result/worker_outputs")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let steps = task
        .pointer("/workflow/steps")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let terminal = |value: &Value| {
        matches!(
            value.get("status").and_then(Value::as_str),
            Some("completed" | "failed" | "cancelled" | "completed_with_errors")
        )
    };
    let mut selected: Option<(usize, Value)> = None;
    for prefer_review in [true, false] {
        for step in steps.iter().rev() {
            let Some(step_id) = step.get("id").and_then(Value::as_str) else {
                continue;
            };
            let is_review = step.get("kind").and_then(Value::as_str) == Some("review");
            if is_review != prefer_review {
                continue;
            }
            if let Some((index, output)) = outputs
                .iter()
                .enumerate()
                .rev()
                .find(|(_, output)| output.get("step_id").and_then(Value::as_str) == Some(step_id) && terminal(output))
            {
                selected = Some((index, output.clone()));
                break;
            }
        }
        if selected.is_some() {
            break;
        }
    }
    if selected.is_none() {
        selected = outputs
            .iter()
            .enumerate()
            .rev()
            .find(|(_, output)| terminal(output))
            .map(|(index, output)| (index, output.clone()));
    }
    let Some((index, output)) = selected else {
        return json!({"final_step":Value::Null,"final_structured_output":Value::Null,"final_summary":Value::Null,"prior_step_outputs_ref":Value::Null});
    };
    let task_id = task.get("task_id").and_then(Value::as_str).unwrap_or("unknown");
    let prior_ref = if index > 0 {
        json!(format!("delegated-task://{task_id}/prior-step-outputs"))
    } else {
        Value::Null
    };
    json!({
        "final_step":output.get("step_id"),
        "final_structured_output":output.pointer("/output/structured_output").cloned().unwrap_or(Value::Null),
        "final_summary":output.pointer("/output/summary_text").cloned().unwrap_or(Value::Null),
        "prior_step_outputs_ref":prior_ref
    })
}

fn derived_task_summary(task: &Value) -> Option<Value> {
    let projection = final_step_projection(task);
    let base = projection
        .get("final_structured_output")
        .filter(|value| !value.is_null())
        .map(structured_output_summary)
        .or_else(|| projection
        .get("final_summary")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(str::to_string));
    let (objective, _) = objective_verdict(task);
    if objective == "not_applicable" {
        return base.map(|text| json!(truncate_summary(&text, 512)));
    }
    let label = if is_executability_assessment(task) {
        "assessment_result"
    } else {
        "objective_result"
    };
    let body = base.unwrap_or_else(|| "No substantive objective result was reported.".to_string());
    Some(json!(truncate_summary(
        &format!("{label}: {objective}. {body}"),
        512,
    )))
}

fn truncate_summary(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    let prefix = text.chars().take(max_chars).collect::<String>();
    let boundary = prefix
        .char_indices()
        .rev()
        .find(|(_, ch)| ch.is_whitespace())
        .map(|(index, _)| index)
        .unwrap_or(prefix.len());
    format!("{}…", prefix[..boundary].trim_end())
}

fn timing_projection(task: &Value) -> Value {
    let created = task.get("created_at_ms").and_then(Value::as_i64);
    let started = task.get("started_at_ms").and_then(Value::as_i64);
    let finished = task.get("finished_at_ms").and_then(Value::as_i64);
    let queue_ms = created.zip(started).map(|(created, started)| started.saturating_sub(created));
    let worker_ms = task
        .pointer("/result/worker_refs")
        .and_then(Value::as_array)
        .map(|refs| refs.iter().filter_map(|reference| reference.get("duration_ms").and_then(Value::as_i64)).sum::<i64>());
    let active_ms = task.get("duration_ms").and_then(Value::as_i64);
    let orchestration_ms = active_ms.zip(worker_ms).map(|(active, worker)| active.saturating_sub(worker));
    let total_ms = created.zip(finished).map(|(created, finished)| finished.saturating_sub(created));
    json!({"queue_ms":queue_ms,"worker_ms":worker_ms,"orchestration_ms":orchestration_ms,"total_ms":total_ms})
}
fn task_summary_value(task: &Value) -> Option<Value> {
    derived_task_summary(task).or_else(|| {
        task.get("summary")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|summary| !summary.is_empty())
        .map(|summary| json!(summary))
    })
}
fn refresh_task_summary(task: &mut Value) {
    if let Some(summary) = derived_task_summary(task) {
        task["summary"] = summary;
    }
}
fn acceptance_checks_or_derive(
    task: &Value,
    root: &Path,
    result: Option<&Map<String, Value>>,
) -> Value {
    let (_, derived_checks) = acceptance_verdict(task, root);
    let derived_requested_fields = derived_checks
        .iter()
        .find(|check| check["kind"] == "requested_fields")
        .filter(|check| {
            check["requested"]
                .as_array()
                .is_some_and(|fields| !fields.is_empty())
        });
    let derived_outcome_checks = derived_checks
        .iter()
        .filter(|check| {
            matches!(
                check["kind"].as_str(),
                Some("output_contract" | "objective_outcome" | "assessment_consistency")
            )
        })
        .cloned()
        .collect::<Vec<_>>();
    if let Some(checks) = result.and_then(|value| value.get("acceptance_checks")) {
        if checks.as_array().is_some_and(|items| !items.is_empty()) {
            let mut refreshed = checks.as_array().cloned().unwrap_or_default();
            if let Some(derived_requested_fields) = derived_requested_fields {
                if let Some(existing) = refreshed
                    .iter_mut()
                    .find(|check| check["kind"] == "requested_fields")
                {
                    *existing = derived_requested_fields.clone();
                } else {
                    refreshed.push(derived_requested_fields.clone());
                }
            }
            for derived in derived_outcome_checks {
                if let Some(existing) = refreshed
                    .iter_mut()
                    .find(|check| check["kind"] == derived["kind"])
                {
                    *existing = derived;
                } else {
                    refreshed.push(derived);
                }
            }
            return json!(refreshed);
        }
    }
    json!(derived_checks)
}

fn compact_task(task: &Value, root: &Path) -> Value {
    let obj = task.as_object().cloned().unwrap_or_default();
    let result = obj.get("result").and_then(Value::as_object);
    let (derived_verdict, _) = acceptance_verdict(task, root);
    let acceptance_checks = acceptance_checks_or_derive(task, root, result);
    let output_contract = output_contract_verdict(task);
    let objective_verdict_value = objective_verdict(task).0;
    let final_projection = final_step_projection(task);
    let has_final_output = final_projection
        .get("final_structured_output")
        .is_some_and(|value| !value.is_null());
    let prior_ref = final_projection.get("prior_step_outputs_ref").filter(|value| !value.is_null()).or_else(|| result.and_then(|value| value.get("prior_step_outputs_ref")));
    json!({"task_id":obj.get("task_id"),"task_status":obj.get("status"),"objective":obj.get("objective"),"owner_site_id":obj.get("owner_site_id"),"created_by_site_id":obj.get("created_by_site_id"),"visibility_scope":obj.get("visibility_scope"),"updated_at":obj.get("updated_at"),"summary":task_summary_value(task),"output_contract_verdict":output_contract,"objective_verdict":objective_verdict_value,"acceptance_verdict":result.and_then(|v|v.get("acceptance_verdict")).cloned().unwrap_or_else(||json!(derived_verdict)),"acceptance_checks":acceptance_checks,"final_step":final_projection.get("final_step"),"final_structured_output":final_projection.get("final_structured_output"),"prior_step_outputs_ref":prior_ref,"depends_on_task_ids":obj.get("depends_on_task_ids"),"import_task_outputs":obj.get("import_task_outputs"),"external_dependencies":obj.get("external_dependencies"),"imported_task_outputs":result.and_then(|value| value.get("imported_task_outputs")),"execution_binding":obj.get("execution_binding"),"timing":timing_projection(task),"worker_refs":if has_final_output{None}else{result.and_then(|v|v.get("worker_refs"))},"worker_outputs":if has_final_output{None}else{result.and_then(|v|v.get("worker_outputs"))}})
}

fn prior_outputs_ref<'a>(task: &'a Value, projection: &'a Value) -> Option<&'a Value> {
    projection.get("prior_step_outputs_ref").filter(|value| !value.is_null())
        .or_else(|| task.pointer("/result/prior_step_outputs_ref").filter(|value| !value.is_null()))
}

fn parse_embedded_structured_output(text: &str) -> Option<Value> {
    if let Ok(value) = serde_json::from_str::<Value>(text) {
        return Some(value);
    }
    for block in text.split("```").skip(1).step_by(2) {
        let body = block.trim();
        let body = body
            .strip_prefix("json")
            .or_else(|| body.strip_prefix("JSON"))
            .unwrap_or(body)
            .trim();
        if let Ok(value) = serde_json::from_str::<Value>(body) {
            return Some(value);
        }
    }
    text.char_indices()
        .filter(|(_, character)| matches!(character, '{' | '['))
        .find_map(|(start, _)| {
            let candidate = text[start..].trim();
            let candidate = candidate
                .strip_suffix("```")
                .map(str::trim)
                .unwrap_or(candidate);
            serde_json::from_str::<Value>(candidate).ok().or_else(|| {
                let mut stream = serde_json::Deserializer::from_str(candidate).into_iter::<Value>();
                stream.next().and_then(Result::ok)
            })
        })
}

fn required_field_names(value: Option<&Value>) -> Vec<String> {
    let mut fields = Vec::new();
    if let Some(items) = value.and_then(Value::as_array) {
        for field in items.iter().filter_map(|item| {
            item.as_str()
                .or_else(|| item.get("name").and_then(Value::as_str))
        }) {
            if !fields.iter().any(|known| known == field) {
                fields.push(field.to_string());
            }
        }
    }
    fields
}

fn acceptance_required_fields(task: &Value) -> Vec<String> {
    for path in [
        "/acceptance/required_fields",
        "/acceptance/requested_fields",
        "/acceptance/required",
    ] {
        let fields = required_field_names(task.pointer(path));
        if !fields.is_empty() {
            return fields;
        }
    }
    Vec::new()
}

fn required_contract_fields(task: &Value, step_id: Option<&str>) -> Vec<String> {
    let mut fields = acceptance_required_fields(task);
    if let Some(steps) = task.pointer("/workflow/steps").and_then(Value::as_array) {
        for step in steps {
            if step_id.is_some_and(|wanted| step.get("id").and_then(Value::as_str) != Some(wanted)) {
                continue;
            }
            for field in required_field_names(step.pointer("/output_schema/required")) {
                if !fields.iter().any(|known| known == &field) {
                    fields.push(field);
                }
            }
        }
    }
    fields
}

fn is_executability_assessment(task: &Value) -> bool {
    task.pointer("/workflow/steps")
        .and_then(Value::as_array)
        .is_some_and(|steps| {
            steps.iter().any(|step| {
                step.pointer("/output_schema/name").and_then(Value::as_str)
                    == Some("task_executability_assessment_v1")
                    || step.get("profile").and_then(Value::as_str)
                        == Some("shoshin-task-executability-v1")
            })
        })
}

fn assessment_consistency_check(task: &Value) -> Option<Value> {
    if !is_executability_assessment(task) {
        return None;
    }
    let projection = final_step_projection(task);
    let output = projection
        .get("final_structured_output")
        .and_then(Value::as_object)?;
    let assessment = output.get("assessment_result").and_then(Value::as_object)?;
    let assessment_status = assessment
        .get("status")
        .and_then(Value::as_str)
        .map(|value| value.trim().to_ascii_lowercase());
    let implementation_ready = assessment.get("implementation_ready").and_then(Value::as_bool);
    let blocker_count = assessment
        .get("blockers")
        .and_then(Value::as_array)
        .map(Vec::len)
        .unwrap_or(0);
    let blocking_decision_count = output
        .get("required_decisions")
        .and_then(Value::as_array)
        .map(|decisions| {
            decisions
                .iter()
                .filter(|decision| decision.get("blocking").and_then(Value::as_bool) == Some(true))
                .count()
        })
        .unwrap_or(0);
    let executable_status = matches!(
        assessment_status.as_deref(),
        Some("executable" | "passed" | "pass" | "ready" | "complete" | "completed" | "success")
    );
    let strict_blocked_status =
        matches!(assessment_status.as_deref(), Some("blocked" | "not_executable"));
    let explicit_reason_present = assessment
        .get("reason")
        .and_then(Value::as_str)
        .map(str::trim)
        .is_some_and(|reason| !reason.is_empty());
    let mut reasons = Vec::new();
    if assessment_status.is_none() {
        reasons.push("assessment_status_missing".to_string());
    } else if !executable_status
        && !matches!(
            assessment_status.as_deref(),
            Some("blocked" | "not_executable" | "failed" | "failure" | "undetermined" | "inconclusive" | "unavailable")
        )
    {
        reasons.push("assessment_status_unknown".to_string());
    }
    if executable_status && implementation_ready != Some(true) {
        reasons.push("executable_status_requires_implementation_ready_true".to_string());
    }
    if executable_status && blocker_count > 0 {
        reasons.push("executable_status_has_blockers".to_string());
    }
    if executable_status && blocking_decision_count > 0 {
        reasons.push("executable_status_has_blocking_required_decisions".to_string());
    }
    if strict_blocked_status && implementation_ready != Some(false) {
        reasons.push("blocked_status_requires_implementation_ready_false".to_string());
    }
    if assessment_status.as_deref() == Some("blocked") && blocker_count == 0 {
        reasons.push("blocked_status_requires_blockers".to_string());
    }
    if assessment_status.as_deref() == Some("not_executable") && !explicit_reason_present {
        reasons.push("not_executable_status_requires_reason".to_string());
    }
    Some(json!({
        "kind":"assessment_consistency",
        "status":if reasons.is_empty(){"passed"}else{"failed"},
        "verdict":if reasons.is_empty(){"consistent"}else{"inconsistent"},
        "assessment_status":assessment_status,
        "implementation_ready":implementation_ready,
        "blocker_count":blocker_count,
        "blocking_decision_count":blocking_decision_count,
        "reasons":reasons
    }))
}

fn assessment_consistency_failed(task: &Value) -> bool {
    assessment_consistency_check(task)
        .is_some_and(|check| check.get("status").and_then(Value::as_str) == Some("failed"))
}

fn objective_signal(task: &Value) -> Option<String> {
    let projection = final_step_projection(task);
    if projection.get("final_step").and_then(Value::as_str).is_some()
        && projection.get("final_structured_output").is_none_or(Value::is_null)
        && projection.get("final_summary").is_none_or(|value| {
            value.is_null() || value.as_str().is_none_or(|text| text.trim().is_empty())
        })
    {
        return Some("missing_terminal_result".to_string());
    }
    let output = projection.get("final_structured_output")?;
    let object = output.as_object()?;
    for key in ["objective_verdict", "assessment_result", "objective_status"] {
        let Some(value) = object.get(key) else {
            continue;
        };
        if let Some(text) = value.as_str().map(str::trim).filter(|text| !text.is_empty()) {
            return Some(text.to_ascii_lowercase());
        }
        if let Some(nested) = value.as_object() {
            if key == "assessment_result" {
                if assessment_consistency_failed(task) {
                    return Some("inconsistent".to_string());
                }
                if let Some(text) = nested
                    .get("status")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|text| !text.is_empty())
                {
                    return Some(text.to_ascii_lowercase());
                }
                if nested.get("implementation_ready").and_then(Value::as_bool) == Some(true)
                    && nested
                        .get("blockers")
                        .and_then(Value::as_array)
                        .is_some_and(Vec::is_empty)
                {
                    return Some("passed".to_string());
                }
            }
            for nested_key in ["verdict", "status", "result"] {
                if let Some(text) = nested
                    .get(nested_key)
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|text| !text.is_empty())
                {
                    return Some(text.to_ascii_lowercase());
                }
            }
        }
    }
    None
}

fn objective_verdict(task: &Value) -> (&'static str, Option<String>) {
    let signal = objective_signal(task);
    let verdict = match signal.as_deref() {
        Some("passed" | "pass" | "achieved" | "success" | "succeeded" | "completed" | "complete" | "coherent" | "executable" | "ready") => "passed",
        Some("pending" | "running") => "pending",
        Some("failed" | "failure" | "missing_terminal_result") => "failed",
        Some("blocked" | "not_executable" | "undetermined" | "inconclusive" | "unavailable" | "not_found") => "blocked",
        Some("inconsistent") => "blocked",
        Some(_) => "blocked",
        None if !task_is_terminal(task) => "pending",
        None if is_executability_assessment(task) => "blocked",
        None if task.get("status").and_then(Value::as_str) == Some("completed") => "passed",
        None if task.get("status").and_then(Value::as_str) == Some("failed") => "failed",
        None => "blocked",
    };
    (verdict, signal)
}

fn output_contract_verdict(task: &Value) -> &'static str {
    if task
        .pointer("/result/step_states")
        .and_then(Value::as_object)
        .is_some_and(|states| {
            states.values().any(|state| {
                state.get("worker_output_contract").and_then(Value::as_str) == Some("failed")
            })
        })
    {
        return "failed";
    }
    if assessment_consistency_failed(task) {
        return "failed";
    }
    let final_step = final_step_projection(task);
    let step_id = final_step.get("final_step").and_then(Value::as_str);
    let fields = required_contract_fields(task, step_id);
    if fields.is_empty() {
        return "not_applicable";
    }
    let Some(output) = final_step.get("final_structured_output").filter(|value| !value.is_null()) else {
        return "pending";
    };
    if fields.iter().all(|field| output.get(field).is_some()) {
        "passed"
    } else {
        "failed"
    }
}

fn set_outcome_verdicts(task: &mut Value, acceptance: &str) {
    let output_contract = output_contract_verdict(task);
    let objective = objective_verdict(task).0;
    task["result"]["output_contract_verdict"] = json!(output_contract);
    task["result"]["objective_verdict"] = json!(objective);
    task["result"]["acceptance_verdict"] = json!(acceptance);
}

fn structured_output_instruction(task: &Value) -> Option<String> {
    structured_output_instruction_for_step(task, None)
}

fn structured_output_instruction_for_step(
    task: &Value,
    step: Option<&Value>,
) -> Option<String> {
    let mut fields = acceptance_required_fields(task);
    for field in required_field_names(step.and_then(|value| value.pointer("/output_schema/required"))) {
        if !fields.iter().any(|known| known == &field) {
            fields.push(field);
        }
    }
    if fields.is_empty() {
        return None;
    }
    let assessment_contract = step
        .and_then(|value| value.pointer("/output_schema/name"))
        .and_then(Value::as_str)
        .filter(|name| *name == "task_executability_assessment_v1")
        .map(|_| {
            " EXECUTABILITY ASSESSMENT SUBSCHEMA: assessment_result MUST be an object, not a string, with status, implementation_ready, blockers, and (for not_executable) reason. Rules: executable => implementation_ready=true and blockers=[]; blocked => implementation_ready=false and blockers nonempty; not_executable => implementation_ready=false and reason nonempty.".to_string()
        })
        .unwrap_or_default();
    Some(format!(
        "\n\nMANDATORY TERMINAL OUTPUT CONTRACT: return exactly one JSON object with these required top-level keys: {}. The JSON object must be the entire final answer: no Markdown fence, preamble, narration, or trailing explanation. Complete every required field before returning.{}\nREAD-ONLY PROBE RULE: use supplied preflight evidence for path checks; if a probe is necessary, issue one executable with literal arguments and no shell operators, pipes, redirection, or generated scripts.",
        fields.join(", "),
        assessment_contract
    ))
}

fn markdown_field_value(line: &str, field: &str) -> Option<Value> {
    let mut candidate = line.trim();
    for prefix in ["- ", "* ", "+ ", "• "] {
        if let Some(rest) = candidate.strip_prefix(prefix) {
            candidate = rest.trim_start();
            break;
        }
    }
    if let Some(rest) = candidate.strip_prefix("**") {
        let end = rest.find("**")?;
        if rest[..end].trim() != field {
            return None;
        }
        candidate = &rest[end + 2..];
    } else if let Some(rest) = candidate.strip_prefix('`') {
        let end = rest.find('`')?;
        if rest[..end].trim() != field {
            return None;
        }
        candidate = &rest[end + 1..];
    } else {
        candidate = candidate.strip_prefix(field)?;
    }
    let separator = candidate.chars().next()?;
    if !matches!(separator, ':' | '=') {
        return None;
    }
    let value = candidate[separator.len_utf8()..]
        .trim()
        .trim_matches('`')
        .trim_matches('*')
        .trim();
    if value.is_empty() {
        return None;
    }
    serde_json::from_str(value)
        .ok()
        .filter(|parsed: &Value| !parsed.is_object() && !parsed.is_array())
        .or_else(|| Some(Value::String(value.to_string())))
}

fn parse_markdown_structured_output(text: &str, required_fields: &[String]) -> Option<Value> {
    if required_fields.is_empty() {
        return None;
    }
    let mut fields = Map::new();
    for field in required_fields {
        let value = text
            .lines()
            .flat_map(|line| std::iter::once(line).chain(line.split(", ")))
            .find_map(|line| markdown_field_value(line, field))?;
        fields.insert(field.clone(), value);
    }
    Some(Value::Object(fields))
}

fn worker_output_from_run_with_required_fields(
    run: &Value,
    required_fields: &[String],
) -> Option<Value> {
    let summary = run
        .get("summary")
        .or_else(|| run.get("summary_preview"))
        .filter(|value| !value.is_null())?;
    match summary {
        Value::String(text) => {
            let bounded = text.len() <= MAX_WORKER_OUTPUT_BYTES;
            if bounded {
                if let Some(structured) = parse_embedded_structured_output(text) {
                    let encoded_len = serde_json::to_vec(&structured)
                        .map(|bytes| bytes.len())
                        .unwrap_or(MAX_WORKER_OUTPUT_BYTES + 1);
                    if encoded_len <= MAX_WORKER_OUTPUT_BYTES {
                        return Some(json!({"summary_text":structured_output_summary(&structured),"diagnostics_text":diagnostics_prefix(text),"structured_output":structured,"truncated":false}));
                    }
                }
                if let Some(structured) = parse_markdown_structured_output(text, required_fields) {
                    return Some(json!({"summary_text":structured_output_summary(&structured),"diagnostics_text":diagnostics_prefix(text),"raw_summary_text":text.chars().take(MAX_WORKER_OUTPUT_BYTES).collect::<String>(),"structured_output":structured,"structured_output_normalization":"markdown_summary","truncated":false}));
                }
            }
            Some(if required_fields.is_empty() {
                json!({"summary_text":text.chars().take(MAX_WORKER_OUTPUT_BYTES).collect::<String>(),"truncated":!bounded || text.chars().count()>MAX_WORKER_OUTPUT_BYTES})
            } else {
                json!({"summary_text":text.chars().take(MAX_WORKER_OUTPUT_BYTES).collect::<String>(),"structured_output_required":true,"structured_output_error":{"code":"worker_structured_output_required","required_fields":required_fields},"truncated":!bounded || text.chars().count()>MAX_WORKER_OUTPUT_BYTES})
            })
        }
        value => {
            let encoded_len = serde_json::to_vec(value)
                .map(|bytes| bytes.len())
                .unwrap_or(MAX_WORKER_OUTPUT_BYTES + 1);
            (encoded_len <= MAX_WORKER_OUTPUT_BYTES)
                .then(|| json!({"summary_text":structured_output_summary(value),"structured_output":value,"truncated":false}))
        }
    }
}

fn structured_output_contract_failed(output: Option<&Value>, required_fields: &[String]) -> bool {
    !required_fields.is_empty()
        && output.is_none_or(|value| value.get("structured_output_required") == Some(&json!(true)))
}

fn record_worker_terminal(
    task: &mut Value,
    step_id: &str,
    run_id: &str,
    status: &str,
    run: &Value,
) {
    let required_fields = required_contract_fields(task, Some(step_id));
    let runtime_terminal_confirmed = run.get("terminal_event").and_then(Value::as_bool) == Some(true)
        || run.get("completion_state").and_then(Value::as_str) == Some("complete")
        || run.get("phase").and_then(Value::as_str) == Some("completed");
    let runtime_terminal_missing = run.get("phase").is_some() && !runtime_terminal_confirmed;
    let output = if runtime_terminal_missing {
        Some(json!({
            "summary_text":Value::Null,
            "worker_runtime_incomplete":true,
            "structured_output_error":{"code":"worker_runtime_incomplete_output","message":"terminal worker event was not observed"},
            "truncated":false
        }))
    } else {
        worker_output_from_run_with_required_fields(run, &required_fields)
    }.or_else(|| {
        (!required_fields.is_empty()).then(|| {
            json!({
                "summary_text":Value::Null,
                "structured_output_required":true,
                "structured_output_error":{"code":"worker_structured_output_required","required_fields":required_fields},
                "truncated":false
            })
        })
    });
    let contract_failed = structured_output_contract_failed(output.as_ref(), &required_fields);
    let effective_status = if contract_failed || runtime_terminal_missing { "failed" } else { status };
    if let Some(refs) = task["result"]["worker_refs"].as_array_mut() {
        if let Some(reference) = refs.iter_mut().find(|reference| {
            reference.get("run_id").and_then(Value::as_str) == Some(run_id)
        }) {
            reference["status"] = json!(effective_status);
            reference["finished_at"] = json!(now());
            if let Some(duration_ms) = run.get("duration_ms").or_else(|| run.pointer("/timing/duration_ms")) {
                reference["duration_ms"] = duration_ms.clone();
            }
            if let Some(value) = output.clone() {
                reference["output"] = value;
            }
            if let Some(error) = run.get("error").filter(|value| !value.is_null()) {
                reference["error"] = error.clone();
            }
        }
    }
    let step_state = &mut task["result"]["step_states"][step_id];
    step_state["worker_status"] = json!(effective_status);
    step_state["worker_output_contract"] = json!(if contract_failed { "failed" } else { "passed" });
    if let Some(value) = output.clone() {
        step_state["worker_output"] = value;
    }
    if !task["result"].get("worker_outputs").is_some_and(Value::is_array) {
        task["result"]["worker_outputs"] = json!([]);
    }
    if let Some(outputs) = task["result"]["worker_outputs"].as_array_mut() {
        outputs.retain(|value| value.get("run_id").and_then(Value::as_str) != Some(run_id));
        outputs.push(json!({"step_id":step_id,"run_id":run_id,"status":effective_status,"output":output,"error":run.get("error")}));
    }
    refresh_task_summary(task);
}
#[cfg(test)]
fn task_status(args: &Map<String, Value>, root: &Path) -> Result<Value, Value> {
    task_status_with_roots(args, root, &[root.to_path_buf()])
}
fn task_status_with_roots(
    args: &Map<String, Value>,
    root: &Path,
    allowed_roots: &[PathBuf],
) -> Result<Value, Value> {
    let id = task_id(args)?;
    let current = read_task(root, &id)?;
    let has_unresolved_dependencies = !task_is_terminal(&current)
        && current
            .get("depends_on_task_ids")
            .and_then(Value::as_array)
            .is_some_and(|dependencies| !dependencies.is_empty());
    let task = if args.get("refresh").and_then(Value::as_bool) == Some(true) {
        assert_mutation_scope(&current, args, root)?;
        advance_task_closure(root, &id, allowed_roots, &mut std::collections::BTreeSet::new())?
    } else if has_unresolved_dependencies {
        // Dependency transitions are lifecycle-owned projections. Reading a
        // dependent task must not preserve stale waiting state after a
        // predecessor has reached a terminal outcome.
        advance_task_closure(root, &id, allowed_roots, &mut std::collections::BTreeSet::new())?
    } else {
        current
    };
    let obj = task.as_object().cloned().unwrap_or_default();
    Ok(
        json!({"schema":"narada.delegated_task.status.v1","status":"ok","task_id":id,"task_status":obj.get("status"),"objective":obj.get("objective"),"ownership":ownership(&task),"execution_binding":obj.get("execution_binding"),"request_fingerprint":obj.get("request_fingerprint"),"created_at":obj.get("created_at"),"updated_at":obj.get("updated_at"),"timing":timing_projection(&task),"result":compact_task(&task, root)}),
    )
}
fn task_is_terminal(task: &Value) -> bool {
    matches!(
        task.get("status").and_then(Value::as_str),
        Some("completed" | "failed" | "cancelled")
    )
}
fn task_result(args: &Map<String, Value>, root: &Path) -> Result<Value, Value> {
    let id = task_id(args)?;
    let task = read_task(root, &id)?;
    let terminal = task_is_terminal(&task);
    let result = task.get("result").cloned().unwrap_or_else(|| json!({}));
    let (derived_verdict, _) = acceptance_verdict(&task, root);
    let acceptance_checks = acceptance_checks_or_derive(&task, root, task.get("result").and_then(Value::as_object));
    let output_contract = output_contract_verdict(&task);
    let objective_verdict_value = objective_verdict(&task).0;
    let final_projection = final_step_projection(&task);
    Ok(
        json!({"schema":"narada.delegated_task.result.v1","status":"ok","task_id":id,"task_status":task.get("status"),"result":result,"summary":task_summary_value(&task),"output_contract_verdict":output_contract,"objective_verdict":objective_verdict_value,"acceptance_verdict":task.pointer("/result/acceptance_verdict").cloned().unwrap_or_else(||json!(derived_verdict)),"acceptance_checks":acceptance_checks,"final_step":final_projection.get("final_step"),"final_structured_output":final_projection.get("final_structured_output"),"prior_step_outputs_ref":prior_outputs_ref(&task, &final_projection),"canonical_terminal_handoff":terminal,"canonical_readback_tool":"delegated_task_wait","readback_role":"secondary_durable_readback"}),
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
    let terminal = task_is_terminal(&task);
    let (derived_verdict, _) = acceptance_verdict(&task, root);
    let acceptance_checks = acceptance_checks_or_derive(&task, root, Some(&result));
    let output_contract = output_contract_verdict(&task);
    let objective_verdict_value = objective_verdict(&task).0;
    let final_projection = final_step_projection(&task);
    Ok(
        json!({"schema":"narada.delegated_task.summary.v1","status":"ok","task_id":id,"task_status":task.get("status"),"objective":task.get("objective"),"summary":task_summary_value(&task),"output_contract_verdict":output_contract,"objective_verdict":objective_verdict_value,"acceptance_verdict":result.get("acceptance_verdict").cloned().unwrap_or_else(||json!(derived_verdict)),"acceptance_checks":acceptance_checks,"final_step":final_projection.get("final_step"),"final_structured_output":final_projection.get("final_structured_output"),"prior_step_outputs_ref":prior_outputs_ref(&task, &final_projection),"residual_risks":result.get("residual_risks").cloned().unwrap_or_else(||json!([])),"progress":result.get("progress"),"timing":timing_projection(&task),"canonical_terminal_handoff":terminal,"canonical_readback_tool":"delegated_task_wait"}),
    )
}
fn terminal_handoff(task: &Value, root: &Path) -> Value {
    let result = task.get("result").and_then(Value::as_object);
    let (derived_verdict, _) = acceptance_verdict(task, root);
    let output_contract = output_contract_verdict(task);
    let objective_verdict_value = objective_verdict(task).0;
    let final_projection = final_step_projection(task);
    let task_id = task.get("task_id").and_then(Value::as_str).unwrap_or("unknown");
    json!({
        "task_id":task.get("task_id"),
        "task_status":task.get("status"),
        "summary":task_summary_value(task),
        "output_contract_verdict":output_contract,
        "objective_verdict":objective_verdict_value,
        "acceptance_verdict":result.and_then(|value| value.get("acceptance_verdict")).cloned().unwrap_or_else(||json!(derived_verdict)),
        "acceptance_checks":acceptance_checks_or_derive(task, root, result),
        "final_step":final_projection.get("final_step"),
        "final_structured_output":final_projection.get("final_structured_output"),
        "prior_step_outputs_ref":prior_outputs_ref(task, &final_projection),
        "created_at":task.get("created_at"),
        "started_at":task.get("started_at"),
        "finished_at":task.get("finished_at"),
        "duration_ms":task.get("duration_ms"),
        "timing":timing_projection(task),
        "details_ref":format!("delegated-task://{task_id}/result"),
        "details_tool":"delegated_task_result"
    })
}
fn task_execute(args: &Map<String, Value>, root: &Path, allowed_roots: &[PathBuf]) -> Result<Value, Value> {
    let validation = validate(args, root)?;
    if validation.get("request_valid").and_then(Value::as_bool) != Some(true) {
        let mut failure = error("delegated_task_validation_failed", "delegated_task_validation_failed");
        failure["validation"] = validation;
        return Err(failure);
    }
    let validated_request_ref = validation.get("validated_request_ref").cloned()
        .ok_or_else(|| error("validated_request_ref_missing", "validated_request_ref_missing"))?;
    let mut run_args = Map::new();
    run_args.insert("validated_request_ref".into(), validated_request_ref);
    run_args.insert("idempotency_key".into(), args.get("idempotency_key").cloned().unwrap_or(Value::Null));
    let run = task_run_with_roots(&run_args, root, allowed_roots)?;
    let task_id = run.get("task_id").cloned().ok_or_else(|| error("task_id_required", "task_id_required"))?;
    let mut wait_args = Map::new();
    wait_args.insert("task_id".into(), task_id);
    for field in ["timeout_ms", "poll_ms"] {
        if let Some(value) = args.get(field) { wait_args.insert(field.into(), value.clone()); }
    }
    let wait = task_wait_with_roots(&wait_args, root, allowed_roots)?;
    let idempotency_replay = run.get("created").and_then(Value::as_bool) == Some(false);
    Ok(json!({"schema":"narada.delegated_task.execute.v1","status":wait.get("status"),"idempotency_replay":idempotency_replay,"validation":validation,"run":{"task_id":run.get("task_id"),"created":run.get("created")},"terminal":wait}))
}

fn task_execute_batch(
    args: &Map<String, Value>,
    root: &Path,
    allowed_roots: &[PathBuf],
) -> Result<Value, Value> {
    let items = args
        .get("items")
        .and_then(Value::as_array)
        .filter(|items| !items.is_empty() && items.len() <= 16)
        .ok_or_else(|| error("delegated_task_batch_items_invalid", "delegated_task_batch_items_invalid"))?
        .iter()
        .map(|item| {
            item.as_object()
                .cloned()
                .ok_or_else(|| error("delegated_task_batch_item_invalid", "delegated_task_batch_item_invalid"))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let max_concurrency = args
        .get("max_concurrency")
        .and_then(Value::as_u64)
        .unwrap_or(2)
        .clamp(1, 8) as usize;
    let compact = args.get("compact").and_then(Value::as_bool).unwrap_or(false);
    let worker_count = max_concurrency.min(items.len());
    let next = AtomicUsize::new(0);
    let results = Mutex::new(vec![Value::Null; items.len()]);
    let started = std::time::Instant::now();
    std::thread::scope(|scope| {
        for _ in 0..worker_count {
            scope.spawn(|| loop {
                let index = next.fetch_add(1, Ordering::Relaxed);
                let Some(item) = items.get(index) else { break };
                let value = match task_execute(item, root, allowed_roots) {
                    Ok(result) => {
                        let result = if compact { compact_batch_result(&result) } else { result };
                        json!({"index":index,"status":"ok","result":result})
                    },
                    Err(failure) => json!({"index":index,"status":"failed","error":failure}),
                };
                results.lock().expect("batch result lock")[index] = value;
            });
        }
    });
    let results = results.into_inner().expect("batch result lock");
    let failed_count = results.iter().filter(|item| item["status"] == "failed").count();
    Ok(json!({
        "schema":"narada.delegated_task.execute_batch.v1",
        "status":if failed_count == 0 {"completed"} else {"partial_failure"},
        "requested_count":results.len(),
        "completed_count":results.len() - failed_count,
        "failed_count":failed_count,
        "max_concurrency":max_concurrency,
        "compact":compact,
        "elapsed_ms":started.elapsed().as_millis() as u64,
        "results":results
    }))
}

fn compact_batch_result(result: &Value) -> Value {
    let handoff = result.pointer("/terminal/terminal_handoff").unwrap_or(&Value::Null);
    json!({
        "task_id":handoff.get("task_id").or_else(|| result.pointer("/run/task_id")),
        "task_status":handoff.get("task_status"),
        "summary":handoff.get("summary"),
        "output_contract_verdict":handoff.get("output_contract_verdict"),
        "objective_verdict":handoff.get("objective_verdict"),
        "acceptance_verdict":handoff.get("acceptance_verdict"),
        "timing":handoff.get("timing"),
        "details_ref":handoff.get("details_ref"),
        "idempotency_replay":result.get("idempotency_replay")
    })
}
#[cfg(test)]
fn task_wait(args: &Map<String, Value>, root: &Path) -> Result<Value, Value> {
    task_wait_with_roots(args, root, &[root.to_path_buf()])
}
fn task_wait_with_roots(
    args: &Map<String, Value>,
    root: &Path,
    allowed_roots: &[PathBuf],
) -> Result<Value, Value> {
    let id = task_id(args)?;
    let requested_timeout = args
        .get("timeout_ms")
        .and_then(Value::as_u64)
        .unwrap_or(30000);
    let timeout = requested_timeout.min(MAX_TRANSPORT_SAFE_WAIT_MS);
    let poll = args
        .get("poll_ms")
        .and_then(Value::as_u64)
        .unwrap_or(5000)
        .clamp(50, 30000);
    let started = std::time::Instant::now();
    let task = loop {
        let scope_task = read_task(root, &id)?;
        assert_mutation_scope(&scope_task, args, root)?;
        let current = advance_task_closure(root, &id, allowed_roots, &mut std::collections::BTreeSet::new())?;
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
    let handoff = terminal_handoff(&task, root);
    let task_identity = json!({
        "task_id": task.get("task_id"),
        "task_status": task.get("status"),
        "details_ref": handoff.get("details_ref"),
        "details_tool": handoff.get("details_tool"),
        "role": "identity_only"
    });
    Ok(
        json!({"schema":"narada.delegated_task.wait.v1","status":if task_is_terminal(&task){"finished"}else{"timeout"},"elapsed_ms":started.elapsed().as_millis() as u64,"requested_timeout_ms":requested_timeout,"timeout_ms":timeout,"timeout_clamped_for_transport":requested_timeout > timeout,"poll_ms":poll,"task_id":id,"task_status":task.get("status"),"refresh_performed":true,"worker_execution":"native_worker_authority","canonical_terminal_handoff":task_is_terminal(&task),"readback_tool":"delegated_task_wait","recovery":{"durable":true,"task_id":id,"status_tool":"delegated_task_status","wait_tool":"delegated_task_wait","events_tool":"delegated_task_events"},"result_readback_redundant":task_is_terminal(&task),"terminal_handoff":handoff,"task":task_identity}),
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
fn now_ms() -> i128 {
    time::OffsetDateTime::now_utc().unix_timestamp_nanos() / 1_000_000
}
fn finalize_timing(task: &mut Value) {
    if !task_is_terminal(task) || task.get("finished_at_ms").and_then(Value::as_i64).is_some() { return; }
    let finished_ms = now_ms();
    let started_ms = task.get("started_at_ms").and_then(Value::as_i64)
        .or_else(|| task.get("created_at_ms").and_then(Value::as_i64))
        .map(i128::from).unwrap_or(finished_ms);
    task["finished_at"] = json!(now());
    task["finished_at_ms"] = json!(finished_ms);
    task["duration_ms"] = json!(finished_ms.saturating_sub(started_ms));
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
    // A caller-supplied idempotency key is the durable operation identity.
    // Validation references identify payload records and must not fork retries.
    if let Some(key) = args.get("idempotency_key").and_then(Value::as_str) {
        let digest = Sha256::digest(key.as_bytes());
        let prefix = digest[..8]
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        return format!("task_{prefix}");
    }
    if let Some(reference) = args.get("validated_request_ref").and_then(Value::as_str) {
        let digest = Sha256::digest(reference.as_bytes());
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
    json!({"start":input.and_then(|v|v.get("start")).and_then(Value::as_bool)!=Some(false),"wait_for_completion":wait,"timeout_ms":input.and_then(|v|v.get("timeout_ms")).and_then(Value::as_u64).unwrap_or(if wait{30000}else{0}).min(600000),"poll_ms":input.and_then(|v|v.get("poll_ms")).and_then(Value::as_u64).unwrap_or(5000).clamp(50,30000),"resumable":input.and_then(|v|v.get("resumable")).and_then(Value::as_bool)!=Some(false),"exit_interview":input.and_then(|v|v.get("exit_interview")).and_then(Value::as_bool)==Some(true),"max_concurrency":input.and_then(|v|v.get("max_concurrency")).and_then(Value::as_u64).unwrap_or(10).clamp(1,32),"max_retries":input.and_then(|v|v.get("max_retries")).and_then(Value::as_u64).unwrap_or(0).min(10)})
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

fn merged_step_constraints(task: &Value, step: &Value) -> Value {
    let mut merged = normalized_constraints(task.get("constraints"));
    let Some(step_constraints) = step.get("constraints").and_then(Value::as_object) else {
        return merged;
    };
    let Some(target) = merged.as_object_mut() else {
        return merged;
    };
    for (key, value) in step_constraints {
        let preserve_task_preflight = key == "preflight_paths"
            && value.as_array().is_some_and(Vec::is_empty)
            && target
                .get(key)
                .and_then(Value::as_array)
                .is_some_and(|paths| !paths.is_empty());
        if !preserve_task_preflight {
            target.insert(key.clone(), value.clone());
        }
    }
    normalized_constraints(Some(&merged))
}
fn asynchronous_worker_constraints(task: &Value, step: &Value) -> Value {
    let mut constraints = merged_step_constraints(task, step);
    if let Some(constraints) = constraints.as_object_mut() {
        constraints.insert("wait_for_completion".into(), json!(false));
        constraints.remove("wait_timeout_ms");
    }
    constraints
}
fn worker_status_args(run_id: &str) -> Map<String, Value> {
    json!({"run_id":run_id,"compact":false})
        .as_object()
        .cloned()
        .expect("worker status arguments are an object")
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
    "queue_timeout_ms",
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
            "queue_timeout_ms":{"type":"integer","minimum":1,"maximum":1800000,"default":300000,"description":"Maximum provider-admission wait; max_run_ms begins only after provider admission."},
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

fn string_array(value: Option<&Value>) -> Option<Vec<String>> {
    value?.as_array().map(|items| {
        items.iter().filter_map(Value::as_str).map(str::to_string).collect()
    })
}

fn external_dependency_diagnostics(args: &Map<String, Value>, root: &Path) -> Vec<Value> {
    let mut diagnostics = Vec::new();
    let task_id = stable_task_id(args);
    let dependencies = match args.get("depends_on_task_ids") {
        None => Vec::new(),
        Some(value) => match string_array(Some(value)) {
            Some(items) if items.len() == value.as_array().map(Vec::len).unwrap_or(0) => items,
            _ => {
                diagnostics.push(json!({"severity":"error","code":"depends_on_task_ids_must_be_string_array"}));
                Vec::new()
            }
        },
    };
    let mut unique = std::collections::BTreeSet::new();
    for dependency in &dependencies {
        if safe_id(dependency).is_err() {
            diagnostics.push(json!({"severity":"error","code":"dependency_task_id_invalid","task_id":dependency}));
        } else if dependency == &task_id {
            diagnostics.push(json!({"severity":"error","code":"task_dependency_cycle","task_id":task_id}));
        } else if !unique.insert(dependency.clone()) {
            diagnostics.push(json!({"severity":"error","code":"duplicate_dependency_task_id","task_id":dependency}));
        } else if !task_path(root, dependency).is_ok_and(|path| path.is_file()) {
            diagnostics.push(json!({"severity":"error","code":"dependency_task_not_found","task_id":dependency}));
        } else if let Ok(predecessor) = read_task(root, dependency) {
            if dependency_reaches(root, dependency, &task_id, &mut std::collections::BTreeSet::new()) {
                diagnostics.push(json!({"severity":"error","code":"task_dependency_cycle","task_id":task_id,"via":dependency}));
            }
            let authority_rank = |value: Option<&str>| match value { Some("read") => 0, Some("write") => 1, Some("command") => 2, _ => 0 };
            let predecessor_rank = authority_rank(predecessor.pointer("/constraints/authority").and_then(Value::as_str));
            let downstream_rank = authority_rank(args.get("constraints").and_then(|value| value.get("authority")).and_then(Value::as_str));
            if downstream_rank > predecessor_rank {
                diagnostics.push(json!({"severity":"error","code":"dependency_authority_escalation","task_id":dependency,"predecessor_authority":predecessor.pointer("/constraints/authority"),"downstream_authority":args.get("constraints").and_then(|value| value.get("authority"))}));
            }
        }
    }
    for field in ["import_task_outputs", "import_worker_refs"] {
        let imports = match args.get(field) {
            None => Vec::new(),
            Some(value) => match string_array(Some(value)) {
                Some(items) if items.len() == value.as_array().map(Vec::len).unwrap_or(0) => items,
                _ => {
                    diagnostics.push(json!({"severity":"error","code":format!("{field}_must_be_string_array")}));
                    Vec::new()
                }
            },
        };
        for imported in imports {
            if !dependencies.contains(&imported) {
                diagnostics.push(json!({"severity":"error","code":"task_import_must_name_declared_dependency","field":field,"task_id":imported}));
            }
        }
    }
    diagnostics
}

fn dependency_reaches(root: &Path, start: &str, target: &str, seen: &mut std::collections::BTreeSet<String>) -> bool {
    if start == target { return true; }
    if !seen.insert(start.to_string()) { return false; }
    read_task(root, start).ok()
        .and_then(|task| task.get("depends_on_task_ids").and_then(Value::as_array).cloned())
        .is_some_and(|dependencies| dependencies.iter().filter_map(Value::as_str).any(|dependency| dependency_reaches(root, dependency, target, seen)))
}

fn external_dependency_gate(task: &mut Value, root: &Path) -> Result<bool, Value> {
    let dependencies = task.get("depends_on_task_ids").and_then(Value::as_array).cloned().unwrap_or_default();
    if dependencies.is_empty() {
        task["external_dependencies"]["status"] = json!("resolved");
        return Ok(true);
    }
    let id = task.get("task_id").and_then(Value::as_str).unwrap_or("unknown").to_string();
    let mut waiting = Vec::new();
    let mut blocked = Vec::new();
    for dependency in &dependencies {
        let Some(dependency_id) = dependency.as_str() else { continue };
        let predecessor = read_task(root, dependency_id)?;
        match predecessor.get("status").and_then(Value::as_str) {
            Some("completed") => {}
            Some("failed" | "cancelled") => blocked.push(dependency_id.to_string()),
            _ => waiting.push(dependency_id.to_string()),
        }
    }
    if !blocked.is_empty() {
        task["external_dependencies"] = json!({"status":"blocked","resolved_at":null,"blocked_by":blocked});
        for state in task["result"]["step_states"].as_object_mut().into_iter().flat_map(|states| states.values_mut()) {
            if state.get("status").and_then(Value::as_str) == Some("pending") {
                state["status"] = json!("blocked");
                state["blocked_by_external_tasks"] = json!(blocked);
                state["finished_at"] = json!(now());
            }
        }
        task["status"] = json!("failed");
        set_outcome_verdicts(task, "failed");
        append_event(root, &id, "task_dependency_blocked", json!({"blocked_by":blocked}))?;
        return Ok(false);
    }
    if !waiting.is_empty() {
        task["external_dependencies"] = json!({"status":"waiting","resolved_at":null,"waiting_for":waiting,"blocked_by":[]});
        return Ok(false);
    }
    if task.pointer("/external_dependencies/status").and_then(Value::as_str) != Some("resolved") {
        let imports = task.get("import_task_outputs").and_then(Value::as_array).cloned().unwrap_or_default();
        let mut resolved = Vec::new();
        let mut total_bytes = 0usize;
        for imported in imports {
            let dependency_id = imported.as_str().unwrap_or_default();
            let predecessor = read_task(root, dependency_id)?;
            let output = final_step_projection(&predecessor).get("final_structured_output").cloned().unwrap_or(Value::Null);
            if output.is_null() {
                task["external_dependencies"] = json!({"status":"blocked","resolved_at":null,"blocked_by":[dependency_id],"reason":"predecessor_structured_output_missing"});
                task["status"] = json!("failed");
                set_outcome_verdicts(task, "failed");
                append_event(root, &id, "task_dependency_blocked", json!({"blocked_by":[dependency_id],"reason":"predecessor_structured_output_missing"}))?;
                return Ok(false);
            }
            total_bytes = total_bytes.saturating_add(serde_json::to_vec(&output).map(|bytes| bytes.len()).unwrap_or(MAX_IMPORTED_TASK_OUTPUT_BYTES + 1));
            if total_bytes > MAX_IMPORTED_TASK_OUTPUT_BYTES {
                return Err(error("imported_task_outputs_too_large", "imported_task_outputs_too_large"));
            }
            resolved.push(json!({"task_id":dependency_id,"source_ref":format!("delegated-task://{dependency_id}/result#final_structured_output"),"structured_output":output}));
        }
        let worker_imports = task.get("import_worker_refs").and_then(Value::as_array).cloned().unwrap_or_default();
        let mut resolved_worker_refs = Vec::new();
        for imported in worker_imports {
            let dependency_id = imported.as_str().unwrap_or_default();
            let predecessor = read_task(root, dependency_id)?;
            let refs = predecessor.pointer("/result/worker_refs").and_then(Value::as_array).cloned().unwrap_or_default();
            total_bytes = total_bytes.saturating_add(serde_json::to_vec(&refs).map(|bytes| bytes.len()).unwrap_or(MAX_IMPORTED_TASK_OUTPUT_BYTES + 1));
            if total_bytes > MAX_IMPORTED_TASK_OUTPUT_BYTES {
                return Err(error("imported_task_outputs_too_large", "imported_task_outputs_too_large"));
            }
            resolved_worker_refs.push(json!({"task_id":dependency_id,"source_ref":format!("delegated-task://{dependency_id}/result#worker_refs"),"worker_refs":refs}));
        }
        task["result"]["imported_task_outputs"] = json!(resolved);
        task["result"]["imported_worker_refs"] = json!(resolved_worker_refs);
        task["result"]["prior_step_outputs_ref"] = json!(format!("delegated-task://{id}/imported-task-outputs"));
        task["external_dependencies"] = json!({"status":"resolved","resolved_at":now(),"blocked_by":[]});
        append_event(root, &id, "task_dependencies_resolved", json!({"depends_on_task_ids":dependencies,"imported_task_ids":task["import_task_outputs"],"imported_worker_ref_task_ids":task["import_worker_refs"],"prior_step_outputs_ref":task["result"]["prior_step_outputs_ref"]}))?;
    }
    Ok(true)
}

fn imported_output_instruction(task: &Value) -> Option<String> {
    let imports = task.pointer("/result/imported_task_outputs").and_then(Value::as_array).filter(|items| !items.is_empty())?;
    let payload = serde_json::to_string(imports).ok()?;
    Some(format!("\n\nDECLARED PREDECESSOR OUTPUTS (the only cross-task context; consume these typed structured outputs, not predecessor transcripts):\n{payload}"))
}
#[cfg(test)]
fn task_run(args: &Map<String, Value>, root: &Path) -> Result<Value, Value> {
    task_run_with_roots(args, root, &[root.to_path_buf()])
}
fn task_run_with_roots(
    args: &Map<String, Value>,
    root: &Path,
    allowed_roots: &[PathBuf],
) -> Result<Value, Value> {
    let original_validation_ref = args.get("validated_request_ref").cloned();
    let resolved_args = materialize_validated_request(args, root)?;
    let args = &resolved_args;
    let id = stable_task_id(args);
    safe_id(&id)?;
    let _lock = lock_task(root, &id)?;
    if task_path(root, &id)?.is_file() {
        let mut task = read_task(root, &id)?;
        if args.get("objective").is_none() && args.get("intent").is_none() {
            assert_mutation_scope(&task, args, root)?;
            task = advance_value_with_roots(task, root, allowed_roots)?;
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
            json!({"schema":"narada.delegated_task.run.v1","status":"existing","request_status":"existing","execution_status":task["status"],"created":false,"task_id":id,"task_status":task["status"],"validated_request_ref":task.get("validated_request_ref"),"summary":task_summary_value(&task)}),
        );
    }
    let objective = objective(args)?;
    let admission = validate_with_options(args, root, false)?;
    if admission.get("status").and_then(Value::as_str) != Some("ok") {
        return Err(
            json!({"schema":"narada.delegated_task.error.v1","code":"delegated_task_validation_failed","message":"delegated_task_validation_failed","diagnostics":admission["diagnostics"]}),
        );
    }
    let created = now();
    let created_ms = now_ms();
    let workflow = normalize_workflow(args.get("workflow"));
    let step_states = initial_step_states(&workflow);
    let site = current_site_id(root);
    let fingerprint = request_fingerprint(args, root, &id);
    let dependencies = args.get("depends_on_task_ids").cloned().unwrap_or_else(||json!([]));
    let imports = args.get("import_task_outputs").cloned().unwrap_or_else(||json!([]));
    let worker_imports = args.get("import_worker_refs").cloned().unwrap_or_else(||json!([]));
    let result = json!({"schema":"narada.delegated_task.handoff.v1","output_contract_verdict":"pending","objective_verdict":"pending","acceptance_verdict":"pending","step_states":step_states,"worker_refs":[],"worker_outputs":[],"imported_task_outputs":[],"prior_step_outputs_ref":null,"residual_risks":[],"observed_incoherencies":[],"verification":[],"changed_files":[]});
    let mut task = json!({"schema":"narada.delegated_task.task.v1","task_id":id,"owner_site_id":site,"owner_site_root":if site.is_some(){json!(root.to_string_lossy())}else{Value::Null},"created_by_site_id":site,"visibility_scope":if site.is_some(){"site"}else{"user_global"},"task_root_scope":"site_root","status":"accepted_for_execution","objective":objective,"request_fingerprint":fingerprint,"validated_request_ref":original_validation_ref,"created_at":created,"created_at_ms":created_ms,"started_at":null,"started_at_ms":null,"finished_at":null,"finished_at_ms":null,"duration_ms":null,"updated_at":created,"cancelled_at":null,"idempotency_key":args.get("idempotency_key"),"constraints":normalized_constraints(args.get("constraints")),"workflow":workflow,"execution":normalized_execution(args.get("execution")),"acceptance":args.get("acceptance").cloned().unwrap_or_else(||json!({})),"depends_on_task_ids":dependencies,"import_task_outputs":imports,"import_worker_refs":worker_imports,"external_dependencies":{"status":"pending","resolved_at":null,"blocked_by":[]},"result":result,"summary":null});
    write_task(root, &task)?;
    append_event(root, &id, "task_created", json!({"objective":objective,"depends_on_task_ids":task["depends_on_task_ids"],"import_task_outputs":task["import_task_outputs"]}))?;
    if task.pointer("/execution/start").and_then(Value::as_bool) != Some(false) {
        task = advance_value_with_roots(task, root, allowed_roots)?;
    }
    Ok(
        json!({"schema":"narada.delegated_task.run.v1","status":"accepted_for_execution","request_status":"accepted_for_execution","execution_status":task["status"],"created":true,"task_id":id,"task_status":task["status"],"validated_request_ref":task.get("validated_request_ref"),"summary":task_summary_value(&task)}),
    )
}
#[cfg(test)]
fn task_advance(args: &Map<String, Value>, root: &Path) -> Result<Value, Value> {
    task_advance_with_roots(args, root, &[root.to_path_buf()])
}
fn task_advance_with_roots(
    args: &Map<String, Value>,
    root: &Path,
    allowed_roots: &[PathBuf],
) -> Result<Value, Value> {
    let id = task_id(args)?;
    let _lock = lock_task(root, &id)?;
    let current = read_task(root, &id)?;
    assert_mutation_scope(&current, args, root)?;
    let task = advance_value_with_roots(current, root, allowed_roots)?;
    Ok(
        json!({"schema":"narada.delegated_task.advance.v1","status":"ok","task_id":id,"task_status":task["status"],"task":compact_task(&task, root)}),
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
        .unwrap_or(1)
        .clamp(1, 32) as usize
}
fn acceptance_verdict(task: &Value, root: &Path) -> (&'static str, Vec<Value>) {
    let mut checks = Vec::new();
    let result = task.get("result").cloned().unwrap_or_else(|| json!({}));
    let result_text = result.to_string();
    let objective_present = task
        .get("objective")
        .and_then(Value::as_str)
        .is_some_and(|value| !value.trim().is_empty());
    checks.push(json!({
        "kind":"objective_present",
        "status":if objective_present {"passed"} else {"failed"}
    }));
    let owner_site_id = task.get("owner_site_id").and_then(Value::as_str);
    let owner_site_root = task.get("owner_site_root").and_then(Value::as_str);
    let provenance_status = match (owner_site_id, owner_site_root) {
        (Some(site), Some(site_root)) if !site.trim().is_empty() && !site_root.trim().is_empty() => {
            if is_within(Path::new(site_root), root) { "passed" } else { "failed" }
        }
        _ => "not_applicable",
    };
    checks.push(json!({
        "kind":"site_provenance",
        "owner_site_id":owner_site_id,
        "owner_site_root":owner_site_root,
        "status":provenance_status
    }));
    let requested_fields = acceptance_required_fields(task);
    let mut returned_fields = Vec::new();
    for list_name in ["worker_outputs", "worker_refs"] {
        if let Some(items) = result.get(list_name).and_then(Value::as_array) {
            for item in items {
                if let Some(fields) = item
                    .pointer("/output/structured_output")
                    .or_else(|| item.pointer("/structured_output"))
                    .and_then(Value::as_object)
                {
                    for field in fields.keys() {
                        if !returned_fields.contains(field) {
                            returned_fields.push(field.clone());
                        }
                    }
                }
            }
        }
    }
    if requested_fields.is_empty() {
        checks.push(json!({"kind":"requested_fields","requested":[],"returned":returned_fields,"missing":[],"status":"not_applicable"}));
    } else {
        let missing = requested_fields
            .iter()
            .filter(|field| !returned_fields.contains(field))
            .cloned()
            .collect::<Vec<_>>();
        checks.push(json!({"kind":"requested_fields","requested":requested_fields,"returned":returned_fields,"missing":missing,"status":if missing.is_empty(){"passed"}else{"failed"}}));
    }
    let changed_files = result
        .get("changed_files")
        .and_then(Value::as_array)
        .map(Vec::len)
        .unwrap_or(0);
    let mut changes_made = false;
    for list_name in ["worker_outputs", "worker_refs"] {
        if let Some(items) = result.get(list_name).and_then(Value::as_array) {
            changes_made |= items.iter().any(|item| {
                let output = item
                    .pointer("/output/structured_output")
                    .or_else(|| item.pointer("/structured_output"));
                output.is_some_and(|value| {
                    value.pointer("/verification/changes_made").and_then(Value::as_bool) == Some(true)
                        || value.get("changes_made").and_then(Value::as_bool) == Some(true)
                        || value.pointer("/verification/target_state_changed").and_then(Value::as_bool) == Some(true)
                })
            });
        }
    }
    let authority = task
        .pointer("/constraints/authority")
        .and_then(Value::as_str)
        .unwrap_or("read");
    checks.push(json!({
        "kind":"no_write",
        "authority":authority,
        "changed_files":changed_files,
        "changes_made":changes_made,
        "status":if authority == "read" {if changed_files == 0 && !changes_made {"passed"} else {"failed"}} else {"not_applicable"}
    }));
    if task.pointer("/acceptance/strict_clean_run").and_then(Value::as_bool) == Some(true) {
        let terminal = task_is_terminal(task);
        let states = result.get("step_states").and_then(Value::as_object);
        let clean = states.is_some_and(|states| !states.is_empty() && states.values().all(|state| {
            state.get("attempts").and_then(Value::as_u64).unwrap_or(0) <= 1
                && state.get("error").is_none_or(Value::is_null)
                && matches!(state.get("status").and_then(Value::as_str), Some("completed" | "skipped" | "noted"))
        }));
        checks.push(json!({"kind":"strict_clean_run","requested":true,"attempts_at_most_one":clean,"no_step_errors":clean,"status":if !terminal {"pending"} else if clean {"passed"} else {"failed"}}));
    }
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
    let output_contract = output_contract_verdict(task);
    checks.push(json!({
        "kind":"output_contract",
        "verdict":output_contract,
        "status":output_contract
    }));
    if let Some(check) = assessment_consistency_check(task) {
        checks.push(check);
    }
    let (objective, signal) = objective_verdict(task);
    checks.push(json!({
        "kind":"objective_outcome",
        "verdict":objective,
        "signal":signal,
        "status":objective
    }));
    let verdict = if output_contract == "failed"
        || checks.iter().any(|check| check.get("status").and_then(Value::as_str) == Some("failed"))
    {
        "failed"
    } else if objective == "failed" {
        "failed"
    } else if objective == "blocked" {
        "blocked"
    } else if output_contract == "pending"
        || checks.iter().any(|check| check.get("status").and_then(Value::as_str) == Some("pending"))
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
fn advance_value_with_roots(
    mut task: Value,
    root: &Path,
    allowed_roots: &[PathBuf],
) -> Result<Value, Value> {
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
    if !external_dependency_gate(&mut task, root)? {
        finalize_timing(&mut task);
        task["updated_at"] = json!(now());
        write_task(root, &task)?;
        return Ok(task);
    }
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
            &worker_status_args(&run_id),
            root,
            allowed_roots,
        )?;
        let worker = status
            .pointer("/run/status")
            .and_then(Value::as_str)
            .unwrap_or("running")
            .to_string();
        let worker_run = status.get("run").cloned().unwrap_or(Value::Null);
        if worker == "completed" {
            record_worker_terminal(&mut task, step_id, &run_id, "completed", &worker_run);
            if task
                .pointer(&format!("/result/step_states/{step_id}/worker_output_contract"))
                .and_then(Value::as_str)
                == Some("failed")
            {
                task["status"] = json!("failed");
                task["result"]["step_states"][step_id]["status"] = json!("failed");
                task["result"]["step_states"][step_id]["finished_at"] = json!(now());
                set_outcome_verdicts(&mut task, "failed");
                append_event(
                    root,
                    &id,
                    "worker_output_contract_failed",
                    json!({"step_id":step_id,"run_id":run_id,"code":"worker_structured_output_required"}),
                )?;
            } else {
                task["result"]["step_states"][step_id]["status"] = json!("completed");
                task["result"]["step_states"][step_id]["finished_at"] = json!(now());
                append_event(
                    root,
                    &id,
                    "worker_completed",
                    json!({"step_id":step_id,"run_id":run_id}),
                )?;
            }
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
                set_outcome_verdicts(&mut task, "failed");
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
    if task_is_terminal(&task) {
        set_outcome_verdicts(&mut task, current_acceptance);
    } else {
        set_outcome_verdicts(&mut task, "pending");
    }
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
            let instruction = structured_output_instruction_for_step(&task, Some(step))
                .map(|contract| format!("{instruction}{contract}"))
                .unwrap_or_else(|| instruction.to_string());
            let instruction = imported_output_instruction(&task)
                .map(|imports| format!("{instruction}{imports}"))
                .unwrap_or(instruction);
            // The lifecycle authority owns polling and durable recovery. Never
            // let a worker child synchronously occupy this MCP request.
            let constraints = asynchronous_worker_constraints(&task, step);
            let worker_args =
                json!({"intent":{"instruction":instruction},"constraints":constraints});
            let run = crate::worker_delegation::call_tool(
                "worker_run",
                worker_args.as_object().unwrap(),
                root,
                allowed_roots,
            )?;
            let run_id = run
                .get("run_id")
                .and_then(Value::as_str)
                .unwrap_or_default();
            if task.get("started_at").is_none_or(Value::is_null) {
                task["started_at"] = json!(now());
                task["started_at_ms"] = json!(now_ms());
            }
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
            append_event(
                root,
                &id,
                "evidence_resolution_completed",
                json!({
                    "step_id":step_id,
                    "run_id":run_id,
                    "preflight_evidence_ref":run.pointer("/resolved_invocation/preflight_evidence_ref"),
                    "native_evidence_count":run.pointer("/capability_snapshot/preflight/items").and_then(Value::as_array).map(Vec::len).unwrap_or(0)
                }),
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
            // Acceptance checks such as strict_clean_run are terminal-state
            // predicates. Mark the candidate outcome terminal before deriving
            // them, then downgrade to failed if a check or output contract fails.
            task["status"] = json!("completed");
            let (verdict, checks) = acceptance_verdict(&task, root);
            set_outcome_verdicts(&mut task, verdict);
            task["result"]["acceptance_checks"] = json!(checks);
            let terminal_failed = verdict == "failed"
                || output_contract_verdict(&task) == "failed";
            task["status"] = json!(if terminal_failed {
                "failed"
            } else {
                "completed"
            });
            append_event(
                root,
                &id,
                if terminal_failed {
                    "task_failed"
                } else {
                    "task_completed"
                },
                json!({
                    "output_contract_verdict":task["result"]["output_contract_verdict"],
                    "objective_verdict":task["result"]["objective_verdict"],
                    "acceptance_verdict":verdict
                }),
            )?;
        } else if step_ids
            .iter()
            .any(|step_id| matches!(step_status(&task, step_id), Some("failed" | "blocked")))
            && !step_ids
                .iter()
                .any(|step_id| matches!(step_status(&task, step_id), Some("pending" | "running")))
        {
            task["status"] = json!("failed");
            set_outcome_verdicts(&mut task, "failed");
            append_event(
                root,
                &id,
                "task_failed",
                json!({"reason":"blocked_or_failed_steps"}),
            )?;
        }
    }
    finalize_timing(&mut task);
    task["updated_at"] = json!(now());
    write_task(root, &task)?;
    Ok(task)
}

fn advance_task_closure(
    root: &Path,
    id: &str,
    allowed_roots: &[PathBuf],
    visiting: &mut std::collections::BTreeSet<String>,
) -> Result<Value, Value> {
    if !visiting.insert(id.to_string()) {
        return Err(json!({"schema":"narada.delegated_task.error.v1","code":"task_dependency_cycle","message":"task_dependency_cycle","task_id":id}));
    }
    let snapshot = read_task(root, id)?;
    let dependencies = snapshot.get("depends_on_task_ids").and_then(Value::as_array).cloned().unwrap_or_default();
    for dependency in dependencies.iter().filter_map(Value::as_str) {
        let _ = advance_task_closure(root, dependency, allowed_roots, visiting)?;
    }
    visiting.remove(id);
    let _lock = lock_task(root, id)?;
    let current = read_task(root, id)?;
    advance_value_with_roots(current, root, allowed_roots)
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
    finalize_timing(&mut task);
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
    let destructive = matches!(name, "delegated_task_cancel" | "delegated_task_parent_takeover");
    json!({"name":name,"description":description,"annotations":{"title":name,"readOnlyHint":read_only,"destructiveHint":destructive,"stateChangingHint":!read_only,"idempotentHint":read_only,"openWorldHint":false},"inputSchema":schema,"outputSchema":{"type":"object","additionalProperties":true}})
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
        assert_eq!(
            list_tools()
                .iter()
                .find(|tool| tool["name"] == "delegated_task_wait")
                .expect("wait tool")["inputSchema"]["properties"]["poll_ms"]["default"],
            5000
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
        let output = worker_output_from_run_with_required_fields(&run, &[]).expect("worker output");
        assert_eq!(output["structured_output"]["repository"], "marici");
        assert_eq!(output["structured_output"]["branch"], "main");
        assert_eq!(output["truncated"], false);
    }

    #[test]
    fn completed_native_worker_shape_is_terminal_without_terminal_event_field() {
        let mut task = json!({
            "acceptance":{"required":["items"]},
            "workflow":{"steps":[{"id":"extract","output_schema":{"required":["items"]}}]},
            "result":{"worker_refs":[{"step_id":"extract","run_id":"run-live","status":"running"}],"worker_outputs":[],"step_states":{"extract":{"status":"running"}}}
        });
        let run = json!({
            "run_id":"run-live",
            "status":"completed",
            "completion_state":"complete",
            "phase":"completed",
            "summary":"{\"items\":[3,1,2]}",
            "error":null
        });
        record_worker_terminal(&mut task, "extract", "run-live", "completed", &run);
        assert_eq!(task["result"]["step_states"]["extract"]["worker_status"], "completed");
        assert_eq!(task["result"]["step_states"]["extract"]["worker_output_contract"], "passed");
        assert_eq!(task["result"]["worker_outputs"][0]["output"]["structured_output"]["items"], json!([3,1,2]));
        assert!(task["result"]["worker_outputs"][0]["output"].get("worker_runtime_incomplete").is_none());
    }

    #[test]
    fn completed_worker_output_extracts_json_after_prose() {
        let run = json!({"summary":"I checked the repository. {\"repository\":\"marici\",\"branch\":\"main\"}"});
        let output = worker_output_from_run_with_required_fields(&run, &[]).expect("worker output");
        assert_eq!(output["structured_output"]["repository"], "marici");
        assert_eq!(output["structured_output"]["branch"], "main");
        assert_eq!(output["summary_text"], "repository=marici, branch=main");
        assert_eq!(output["diagnostics_text"], "I checked the repository.");
    }

    #[test]
    fn completed_worker_output_extracts_fenced_json_after_prose() {
        let run = json!({"summary":"The checks are complete.```json\n{\"repository\":\"marici\",\"branch\":\"main\"}\n```"});
        let output = worker_output_from_run_with_required_fields(&run, &[]).expect("worker output");
        assert_eq!(output["structured_output"]["repository"], "marici");
        assert_eq!(output["structured_output"]["branch"], "main");
    }

    #[test]
    fn completed_worker_output_extracts_multiline_fenced_json_after_prose() {
        let run = json!({"summary":"I’ll perform two read-only checks.```json\n{\n  \"directory_name\": \"marici\",\n  \"current_git_branch\": \"main\",\n  \"verification\": {\n    \"path\": \"C:\\\\Users\\\\andrey\\\\src\\\\marici\",\n    \"changes_made\": false\n  }\n}\n```"});
        let output = worker_output_from_run_with_required_fields(&run, &[]).expect("worker output");
        assert_eq!(output["structured_output"]["directory_name"], "marici");
        assert_eq!(output["structured_output"]["current_git_branch"], "main");
        assert_eq!(output["structured_output"]["verification"]["changes_made"], false);
    }

    #[test]
    fn completed_worker_output_normalizes_required_markdown_fields() {
        let run = json!({"summary":"- **repository_name**: marici\n- current_branch: main\n- verification: read-only check confirmed the branch."});
        let required = vec![
            "repository_name".to_string(),
            "current_branch".to_string(),
            "verification".to_string(),
        ];
        let output = worker_output_from_run_with_required_fields(&run, &required)
            .expect("worker output");
        assert_eq!(output["structured_output"]["repository_name"], "marici");
        assert_eq!(output["structured_output"]["current_branch"], "main");
        assert_eq!(output["structured_output"]["verification"], "read-only check confirmed the branch.");
        assert_eq!(output["structured_output_normalization"], "markdown_summary");
    }

    #[test]
    fn completed_worker_output_marks_missing_structured_output_explicitly() {
        let run = json!({"summary":"The repository is marici and the branch is main."});
        let required = vec!["repository_name".to_string(), "current_branch".to_string()];
        let output = worker_output_from_run_with_required_fields(&run, &required)
            .expect("worker output");
        assert_eq!(output["structured_output_required"], true);
        assert_eq!(output["structured_output_error"]["code"], "worker_structured_output_required");
    }

    #[test]
    fn structured_output_instruction_names_acceptance_fields() {
        let task = json!({"acceptance":{"required":["repository_name","current_branch"]}});
        let instruction = structured_output_instruction(&task).expect("contract");
        assert!(instruction.contains("repository_name, current_branch"));
        assert!(instruction.contains("exactly one JSON object"));
        assert!(instruction.contains("entire final answer"));
        assert!(!instruction.contains("explanation may follow"));
    }

    #[test]
    fn terminal_worker_poll_requests_full_durable_result() {
        let args = worker_status_args("run-test");
        assert_eq!(args.get("run_id"), Some(&json!("run-test")));
        assert_eq!(args.get("compact"), Some(&json!(false)));
    }

    #[test]
    fn structured_output_instruction_uses_step_schema_and_probe_contract() {
        let task = json!({"objective":"assess"});
        let step = json!({"output_schema":{"required":["dimensions","findings"]}});
        let instruction =
            structured_output_instruction_for_step(&task, Some(&step)).expect("contract");
        assert!(instruction.contains("dimensions, findings"));
        assert!(instruction.contains("READ-ONLY PROBE RULE"));
    }

    #[test]
    fn executability_instruction_requires_conditional_assessment_result() {
        let task = json!({"objective":"assess"});
        let step = json!({
            "output_schema": {
                "name": "task_executability_assessment_v1",
                "required": ["assessment_result"]
            }
        });
        let instruction =
            structured_output_instruction_for_step(&task, Some(&step)).expect("contract");
        assert!(instruction.contains("assessment_result MUST be an object"));
        assert!(instruction.contains("executable => implementation_ready=true"));
        assert!(instruction.contains("blocked => implementation_ready=false"));
    }

    #[test]
    fn executability_template_has_bounded_five_minute_worker_deadline() {
        let template = assessment_template();
        assert_eq!(template["bounds"]["max_run_ms"], 300_000);
        assert_eq!(template["steps"][0]["constraints"]["max_run_ms"], 300_000);
        assert!(template["output_schema"]["fields"]["assessment_result"]
            .as_str()
            .is_some_and(|description| description.contains("implementation_ready")));
        assert_eq!(
            template["output_schema"]["conditional_rules"]
                .as_array()
                .map(Vec::len),
            Some(3)
        );
    }

    #[test]
    fn merged_step_constraints_preserves_caller_read_preflight() {
        let task = json!({
            "constraints":{"preflight_paths":[{"path":"README.md","access":"read"}],"cwd":"C:/site"}
        });
        let step = json!({
            "constraints":{"authority":"read","preflight_paths":[],"max_run_ms":300_000}
        });
        let merged = merged_step_constraints(&task, &step);
        assert_eq!(merged["authority"], "read");
        assert_eq!(merged["max_run_ms"], 300_000);
        assert_eq!(merged["cwd"], "C:/site");
        assert_eq!(merged["preflight_paths"][0]["path"], "README.md");
    }

    #[test]
    fn lifecycle_worker_launch_is_always_asynchronous() {
        let task = json!({"constraints":{"authority":"read","wait_for_completion":true,"wait_timeout_ms":180000}});
        let step = json!({"constraints":{"max_run_ms":600000}});
        let constraints = asynchronous_worker_constraints(&task, &step);
        assert_eq!(constraints["wait_for_completion"], false);
        assert!(constraints.get("wait_timeout_ms").is_none());
        assert_eq!(constraints["max_run_ms"], 600000);
    }

    #[test]
    fn validation_reports_deferred_preflight_without_inspecting_filesystem() {
        let root = std::env::temp_dir().join(format!("narada-delegated-task-preflight-{}", uuid::Uuid::new_v4()));
        let response = validate(
            json!({"objective":"inspect","constraints":{"preflight_paths":[{"path":"does-not-exist.txt","access":"read"}]}})
                .as_object()
                .unwrap(),
            &root,
        )
        .expect("validation");
        assert_eq!(response["valid"], true);
        assert_eq!(response["request_valid"], true);
        assert_eq!(response["execution_preflight_pending"], true);
        assert_eq!(response["preflight_status"], "deferred");
        assert_eq!(response["preflight_authority"], "worker-delegation.worker_run");
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn template_catalog_defaults_to_compact_and_supports_detail_lookup() {
        let compact = template_catalog(&Map::new());
        assert_eq!(compact["mode"], "compact");
        assert!(compact["templates"][0].get("stages").is_some());
        assert!(compact["templates"][0].get("detail_available").is_some());
        assert!(compact["templates"][0].get("best_for").is_some());
        assert!(compact["templates"][0].get("avoid_when").is_some());
        let detail = template_catalog(
            json!({"template_id":"implement_review"})
                .as_object()
                .unwrap(),
        );
        assert_eq!(detail["mode"], "detail");
        assert!(detail["templates"][0].get("steps").is_some());
        assert!(detail["templates"][0]["best_for"].as_array().is_some_and(|items| !items.is_empty()));
        assert!(detail["templates"][0]["avoid_when"].as_array().is_some_and(|items| !items.is_empty()));
    }

    #[test]
    fn final_projection_uses_review_and_preserves_prior_output_reference() {
        let task = json!({
            "task_id":"task-review",
            "status":"completed",
            "workflow":{"steps":[
                {"id":"implement","kind":"worker"},
                {"id":"review","kind":"review","depends_on":["implement"]}
            ]},
            "result":{"worker_outputs":[
                {"step_id":"implement","status":"completed","output":{"summary_text":"implementation"}},
                {"step_id":"review","status":"completed","output":{"summary_text":"review","structured_output":{"verdict":"passed"}}}
            ]}
        });
        let projection = final_step_projection(&task);
        assert_eq!(projection["final_step"], "review");
        assert_eq!(projection["final_structured_output"]["verdict"], "passed");
        assert_eq!(
            derived_task_summary(&task),
            Some(json!("objective_result: passed. verdict=passed"))
        );
        assert!(projection["prior_step_outputs_ref"].as_str().is_some());
    }

    #[test]
    fn executability_blocked_separates_contract_and_objective_verdicts() {
        let task = json!({
            "task_id":"task-assessment",
            "status":"completed",
            "objective":"read the target file",
            "workflow":{"steps":[{"id":"assessment","kind":"worker","profile":"shoshin-task-executability-v1","output_schema":{"name":"task_executability_assessment_v1","required":["findings"]}}]},
            "result":{"worker_outputs":[{"step_id":"assessment","status":"completed","output":{"summary_text":"The target could not be read.","structured_output":{"findings":[],"assessment_result":"undetermined"}}}],"step_states":{"assessment":{"kind":"worker","status":"completed","worker_output_contract":"passed"}}}
        });
        let (acceptance, checks) = acceptance_verdict(&task, Path::new("."));
        assert_eq!(output_contract_verdict(&task), "passed");
        assert_eq!(objective_verdict(&task).0, "blocked");
        assert_eq!(acceptance, "blocked");
        assert!(checks.iter().any(|check| check["kind"] == "objective_outcome" && check["verdict"] == "blocked"));
        assert_eq!(
            derived_task_summary(&task),
            Some(json!(
                "assessment_result: blocked. findings=[0 items], assessment_result=undetermined"
            ))
        );
    }

    #[test]
    fn running_executability_assessment_reports_pending_not_blocked() {
        let task = json!({
            "task_id":"task-assessment-running",
            "status":"accepted_for_execution",
            "objective":"read the target file",
            "workflow":{"steps":[{"id":"assessment","kind":"worker","profile":"shoshin-task-executability-v1","output_schema":{"name":"task_executability_assessment_v1","required":["findings"]}}]},
            "result":{"worker_outputs":[],"step_states":{"assessment":{"kind":"worker","status":"running"}}}
        });
        assert_eq!(objective_verdict(&task).0, "pending");
        assert_eq!(
            derived_task_summary(&task),
            Some(json!("assessment_result: pending. No substantive objective result was reported."))
        );
        let (acceptance, checks) = acceptance_verdict(&task, Path::new("."));
        assert_eq!(acceptance, "pending");
        assert!(checks.iter().any(|check| {
            check["kind"] == "objective_outcome" && check["verdict"] == "pending"
        }));
    }

    #[test]
    fn strict_clean_run_is_pending_then_auditable_at_terminal_state() {
        let mut task = json!({
            "status":"running",
            "objective":"inspect",
            "acceptance":{"strict_clean_run":true},
            "result":{"worker_outputs":[],"step_states":{"inspect":{"status":"running","attempts":1,"error":null}}}
        });
        let (pending, pending_checks) = acceptance_verdict(&task, Path::new("."));
        assert_eq!(pending, "pending");
        assert!(pending_checks.iter().any(|check| check["kind"] == "strict_clean_run" && check["status"] == "pending"));
        task["status"] = json!("completed");
        task["result"]["step_states"]["inspect"]["status"] = json!("completed");
        let (passed, terminal_checks) = acceptance_verdict(&task, Path::new("."));
        assert_eq!(passed, "passed");
        assert!(terminal_checks.iter().any(|check| check["kind"] == "strict_clean_run" && check["status"] == "passed"));
    }

    #[test]
    fn task_advance_recomputes_terminal_acceptance_after_marking_terminal() {
        let root = std::env::temp_dir().join(format!(
            "narada-delegated-task-terminal-verdict-{}",
            uuid::Uuid::new_v4()
        ));
        task_run(
            json!({
                "task_id":"task_terminal_verdict",
                "objective":"return status ok",
                "execution":{"start":false},
                "constraints":{"authority":"read"},
                "acceptance":{"strict_clean_run":true},
                "workflow":{"steps":[{"id":"implement","kind":"worker"}]}
            })
            .as_object()
            .unwrap(),
            &root,
        )
        .expect("create");
        let mut task = read_task(&root, "task_terminal_verdict").expect("task");
        task["result"]["step_states"]["implement"]["status"] = json!("completed");
        task["result"]["step_states"]["implement"]["attempts"] = json!(1);
        task["result"]["step_states"]["implement"]["error"] = Value::Null;
        write_task(&root, &task).expect("persist worker result");
        let terminal = task_advance(
            json!({"task_id":"task_terminal_verdict"})
                .as_object()
                .unwrap(),
            &root,
        )
        .expect("advance");
        assert_eq!(terminal["status"], "ok");
        assert_eq!(terminal["task_status"], "completed");
        assert_eq!(terminal["task"]["acceptance_verdict"], "passed");
        assert!(terminal["task"]["acceptance_checks"]
            .as_array()
            .is_some_and(|checks| checks.iter().any(|check| {
                check["kind"] == "strict_clean_run" && check["status"] == "passed"
            })));
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn terminal_handoff_exposes_task_timing() {
        let task = json!({"task_id":"timed","status":"completed","created_at":"2026-01-01T00:00:00Z","created_at_ms":1000,"started_at":"2026-01-01T00:00:01Z","started_at_ms":1200,"finished_at":"2026-01-01T00:00:02Z","finished_at_ms":2500,"duration_ms":1300,"result":{"acceptance_verdict":"passed","step_states":{},"worker_refs":[{"duration_ms":1000}]}});
        let handoff = terminal_handoff(&task, Path::new("."));
        assert_eq!(handoff["duration_ms"], 1300);
        assert_eq!(handoff["started_at"], "2026-01-01T00:00:01Z");
        assert_eq!(handoff["finished_at"], "2026-01-01T00:00:02Z");
        assert_eq!(handoff["timing"]["queue_ms"], 200);
        assert_eq!(handoff["timing"]["worker_ms"], 1000);
        assert_eq!(handoff["timing"]["orchestration_ms"], 300);
        assert_eq!(handoff["timing"]["total_ms"], 1500);
    }

    #[test]
    fn generic_completed_work_has_a_passed_objective_and_compact_result() {
        let task = json!({
            "task_id":"generic",
            "status":"completed",
            "objective":"say ok",
            "result":{
                "acceptance_verdict":"passed",
                "step_states":{},
                "worker_refs":[{"run_id":"run-1","output":{"structured_output":{"answer":"ok"}}}],
                "worker_outputs":[{"step_id":"implement","status":"completed","output":{"summary_text":"ok","structured_output":{"answer":"ok"}}}]
            }
        });
        assert_eq!(objective_verdict(&task).0, "passed");
        let compact = compact_task(&task, Path::new("."));
        assert_eq!(compact["final_structured_output"]["answer"], "ok");
        assert!(compact["worker_refs"].is_null());
        assert!(compact["worker_outputs"].is_null());
    }

    #[test]
    fn terminal_worker_without_substantive_result_cannot_pass() {
        let task = json!({
            "task_id":"progress-only",
            "status":"completed",
            "objective":"compute the two invariants",
            "result":{
                "worker_outputs":[{"step_id":"implement","status":"completed","output":{"summary_text":null}}],
                "step_states":{"implement":{"kind":"worker","status":"completed"}}
            }
        });
        assert_eq!(objective_verdict(&task).0, "failed");
        let (_, checks) = acceptance_verdict(&task, Path::new("."));
        assert!(checks.iter().any(|check| {
            check["kind"] == "objective_outcome"
                && check["signal"] == "missing_terminal_result"
                && check["status"] == "failed"
        }));
    }

    #[test]
    fn terminal_summary_replaces_persisted_pending_projection() {
        let task = json!({
            "task_id":"generic",
            "status":"completed",
            "summary":"objective_result: pending. waiting",
            "result":{"worker_outputs":[{"step_id":"implement","status":"completed","output":{"summary_text":"done"}}]}
        });
        assert_eq!(task_summary_value(&task), Some(json!("objective_result: passed. done")));
    }

    #[test]
    fn terminal_summary_prefers_complete_structured_output_over_clipped_worker_summary() {
        let task = json!({
            "task_id":"generic",
            "status":"completed",
            "result":{"worker_outputs":[{"step_id":"implement","status":"completed","output":{
                "summary_text":"topic=cross-sector falsifi",
                "structured_output":{"topic":"cross-sector falsification"}
            }}]}
        });
        assert_eq!(
            task_summary_value(&task),
            Some(json!("objective_result: passed. topic=cross-sector falsification"))
        );
    }

    #[test]
    fn summary_truncation_preserves_word_boundaries() {
        assert_eq!(truncate_summary("alpha beta gamma", 11), "alpha beta…");
        let long_summary = format!("{} falsifiability", "word ".repeat(102));
        let summary = structured_output_summary(&json!({"summary":long_summary}));
        assert!(summary.ends_with('…'));
        assert!(!summary.ends_with("falsifi…"));
        let concise = concise_value(&json!(format!("{}falsification", "word ".repeat(32))));
        assert!(concise.ends_with('…'));
        assert!(!concise.ends_with("falsifi…"));
    }

    #[test]
    fn batch_execute_is_bounded_ordered_and_failure_isolated() {
        let root = std::env::temp_dir().join(format!(
            "narada-delegated-task-batch-{}",
            uuid::Uuid::new_v4()
        ));
        let batch = task_execute_batch(
            json!({"items":[{"idempotency_key":"a"},{"idempotency_key":"b"}],"max_concurrency":2})
                .as_object()
                .unwrap(),
            &root,
            &[root.clone()],
        )
        .expect("batch response");
        assert_eq!(batch["status"], "partial_failure");
        assert_eq!(batch["requested_count"], 2);
        assert_eq!(batch["failed_count"], 2);
        assert_eq!(batch["results"][0]["index"], 0);
        assert_eq!(batch["results"][1]["index"], 1);
        assert_eq!(batch["results"][0]["error"]["code"], "delegated_task_validation_failed");
        assert_eq!(batch["results"][0]["error"]["validation"]["request_valid"], false);
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn compact_batch_result_keeps_terminal_verdicts_and_durable_readback() {
        let compact = compact_batch_result(&json!({
            "idempotency_replay":false,
            "run":{"task_id":"task-compact"},
            "terminal":{"terminal_handoff":{
                "task_id":"task-compact","task_status":"completed","summary":"objective_result: passed. ok",
                "output_contract_verdict":"passed","objective_verdict":"passed","acceptance_verdict":"passed",
                "timing":{"total_ms":42},"details_ref":"delegated-task://task-compact/result",
                "final_structured_output":{"large":"omitted"},"acceptance_checks":[{"large":"omitted"}]
            }}
        }));
        assert_eq!(compact["task_id"], "task-compact");
        assert_eq!(compact["objective_verdict"], "passed");
        assert_eq!(compact["timing"]["total_ms"], 42);
        assert_eq!(compact["details_ref"], "delegated-task://task-compact/result");
        assert!(compact.get("final_structured_output").is_none());
        assert!(compact.get("acceptance_checks").is_none());
    }

    #[test]
    fn assessment_result_object_is_canonical_and_rejects_contradictory_blocking_decisions() {
        let workflow = json!({"steps":[{"id":"assessment","kind":"worker","profile":"shoshin-task-executability-v1","output_schema":{"name":"task_executability_assessment_v1","required":["assessment_result","required_decisions"]}}]});
        let mut task = json!({
            "task_id":"task-assessment-object",
            "status":"completed",
            "objective":"assess the task",
            "workflow":workflow,
            "result":{"worker_outputs":[{"step_id":"assessment","status":"completed","output":{"summary_text":"assessment complete","structured_output":{"assessment_result":{"status":"executable","implementation_ready":true,"blockers":[]},"required_decisions":[]}}}],"step_states":{"assessment":{"kind":"worker","status":"completed","worker_output_contract":"passed"}}}
        });
        let (acceptance, checks) = acceptance_verdict(&task, Path::new("."));
        assert_eq!(output_contract_verdict(&task), "passed");
        assert_eq!(objective_verdict(&task).0, "passed");
        assert_eq!(acceptance, "passed");
        assert!(checks.iter().any(|check| {
            check["kind"] == "assessment_consistency" && check["status"] == "passed"
        }));

        task["result"]["worker_outputs"][0]["output"]["structured_output"]["required_decisions"] =
            json!([{"decision":"resolve dirty edits","blocking":true}]);
        let (acceptance, checks) = acceptance_verdict(&task, Path::new("."));
        assert_eq!(output_contract_verdict(&task), "failed");
        assert_eq!(objective_verdict(&task).0, "blocked");
        assert_eq!(acceptance, "failed");
        assert!(checks.iter().any(|check| {
            check["kind"] == "assessment_consistency"
                && check["status"] == "failed"
                && check["reasons"]
                    .as_array()
                    .is_some_and(|reasons| reasons.iter().any(|reason| {
                        reason == "executable_status_has_blocking_required_decisions"
                    }))
        }));
    }

    #[test]
    fn wait_inlines_terminal_handoff_fields() {
        let root = std::env::temp_dir().join(format!("narada-delegated-task-handoff-{}", uuid::Uuid::new_v4()));
        let task = json!({
            "schema":"narada.delegated_task.task.v1",
            "task_id":"task-terminal",
            "owner_site_id":root.file_name().and_then(|value| value.to_str()),
            "owner_site_root":root.to_string_lossy(),
            "status":"completed",
            "objective":"done",
            "result":{"acceptance_verdict":"passed","worker_outputs":[
                {"step_id":"review","status":"completed","output":{"summary_text":"done","structured_output":{"ok":true}}}
            ]},
            "workflow":{"steps":[{"id":"review","kind":"review"}]},
            "acceptance":{}
        });
        write_task(&root, &task).expect("task");
        let response = task_wait(
            json!({"task_id":"task-terminal","timeout_ms":0})
                .as_object()
                .unwrap(),
            &root,
        )
        .expect("wait");
        assert_eq!(response["terminal_handoff"]["task_status"], "completed");
        assert_eq!(response["terminal_handoff"]["final_structured_output"]["ok"], true);
        assert_eq!(response["terminal_handoff"]["details_tool"], "delegated_task_result");
        assert_eq!(response["task"]["role"], "identity_only");
        assert_eq!(response["task"]["details_ref"], response["terminal_handoff"]["details_ref"]);
        assert!(response["task"].get("final_structured_output").is_none());
        fs::remove_dir_all(root).expect("cleanup");
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
    fn required_structured_output_is_a_terminal_worker_contract() {
        let mut task = json!({
            "acceptance":{"required":["repository_name"]},
            "result":{"step_states":{"inspect":{"status":"running"}},"worker_refs":[{"step_id":"inspect","run_id":"run-1","status":"running"}],"worker_outputs":[]}
        });
        record_worker_terminal(
            &mut task,
            "inspect",
            "run-1",
            "completed",
            &json!({"summary":"repository_name is marici"}),
        );
        assert_eq!(task["result"]["step_states"]["inspect"]["worker_output_contract"], "failed");
        assert_eq!(task["result"]["worker_outputs"][0]["status"], "failed");
        assert_eq!(task["result"]["worker_outputs"][0]["output"]["structured_output_required"], true);
    }

    #[test]
    fn runtime_progress_without_terminal_event_cannot_pass() {
        let mut task = json!({
            "objective":"compute the two invariants",
            "result":{"step_states":{"implement":{"status":"running"}},"worker_refs":[{"step_id":"implement","run_id":"run-1","status":"running"}],"worker_outputs":[]}
        });
        record_worker_terminal(
            &mut task,
            "implement",
            "run-1",
            "completed",
            &json!({"runtime":"narada-agent-runtime-server","phase":"formatting_output","status":"completed","summary":"I am starting the computation now"}),
        );
        assert_eq!(task["result"]["worker_outputs"][0]["status"], "failed");
        assert_eq!(task["result"]["worker_outputs"][0]["output"]["worker_runtime_incomplete"], true);
        assert_eq!(objective_verdict(&task).0, "failed");
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
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn validated_request_reference_prevents_drift_and_reuses_request() {
        let root = std::env::temp_dir().join(format!(
            "narada-delegated-task-validated-request-{}",
            uuid::Uuid::new_v4()
        ));
        let validation = validate(
            json!({
                "objective":"inspect repository",
                "constraints":{"authority":"read"},
                "execution":{"start":false}
            })
            .as_object()
            .unwrap(),
            &root,
        )
        .expect("validation");
        let reference = validation["validated_request_ref"]
            .as_str()
            .expect("validated request reference")
            .to_string();
        assert_eq!(validation["validation_persisted"], true);
        assert!(root.join(format!("validated-requests/{reference}.json")).is_file());
        let run = task_run(
            json!({"validated_request_ref":reference})
                .as_object()
                .unwrap(),
            &root,
        )
        .expect("run from validated request");
        assert_eq!(run["created"], true);
        let task = read_task(&root, run["task_id"].as_str().unwrap()).expect("task");
        assert_eq!(task["objective"], "inspect repository");
        assert_eq!(task["validated_request_ref"], reference);
        let drift = task_run(
            json!({"validated_request_ref":reference,"constraints":{"authority":"write"}})
                .as_object()
                .unwrap(),
            &root,
        )
        .expect_err("drift must be refused");
        assert_eq!(drift["code"], "validated_request_drift");
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn execute_identity_reuses_identical_validation_and_prioritizes_idempotency_key() {
        let root = std::env::temp_dir().join(format!(
            "narada-delegated-task-execute-idempotency-{}",
            uuid::Uuid::new_v4()
        ));
        let request = json!({
            "objective":"return ok",
            "idempotency_key":"execute-retry",
            "execution":{"start":false}
        });
        let first_validation =
            validate(request.as_object().unwrap(), &root).expect("first validation");
        let replay_validation =
            validate(request.as_object().unwrap(), &root).expect("replay validation");
        assert_eq!(
            first_validation["validated_request_ref"],
            replay_validation["validated_request_ref"]
        );
        let reference = first_validation["validated_request_ref"].clone();
        let first = task_run(
            json!({
                "validated_request_ref":reference,
                "idempotency_key":"execute-retry"
            })
            .as_object()
            .unwrap(),
            &root,
        )
        .expect("first run");
        let replay = task_run(
            json!({
                "validated_request_ref":replay_validation["validated_request_ref"],
                "idempotency_key":"execute-retry"
            })
            .as_object()
            .unwrap(),
            &root,
        )
        .expect("replay");
        assert_eq!(first["task_id"], replay["task_id"]);
        assert_eq!(first["created"], true);
        assert_eq!(replay["created"], false);

        let changed_validation = validate(
            json!({
                "objective":"different payload",
                "idempotency_key":"execute-retry",
                "execution":{"start":false}
            })
            .as_object()
            .unwrap(),
            &root,
        )
        .expect("changed validation");
        let conflict = task_run(
            json!({
                "validated_request_ref":changed_validation["validated_request_ref"],
                "idempotency_key":"execute-retry"
            })
            .as_object()
            .unwrap(),
            &root,
        )
        .expect_err("changed payload under the same key must conflict");
        assert_eq!(conflict["code"], "delegated_task_idempotency_conflict");
        fs::remove_dir_all(root).expect("cleanup");
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
        let validate = tools
            .iter()
            .find(|tool| tool["name"] == "delegated_task_validate")
            .expect("validate tool");
        assert_eq!(validate["annotations"]["readOnlyHint"], false);
        assert_eq!(validate["annotations"]["destructiveHint"], false);
        assert_eq!(validate["annotations"]["stateChangingHint"], true);
        let wait = tools
            .iter()
            .find(|tool| tool["name"] == "delegated_task_wait")
            .expect("wait tool");
        assert_eq!(wait["annotations"]["destructiveHint"], false);
        assert_eq!(wait["annotations"]["stateChangingHint"], true);
        let execute = tools
            .iter()
            .find(|tool| tool["name"] == "delegated_task_execute")
            .expect("execute tool");
        assert_eq!(execute["annotations"]["readOnlyHint"], false);
        assert_eq!(execute["annotations"]["destructiveHint"], false);
        assert_eq!(execute["annotations"]["stateChangingHint"], true);
        assert_eq!(
            execute["inputSchema"]["required"],
            json!(["objective", "idempotency_key"])
        );
        let cancel = tools
            .iter()
            .find(|tool| tool["name"] == "delegated_task_cancel")
            .expect("cancel tool");
        assert_eq!(cancel["annotations"]["destructiveHint"], true);
        assert_eq!(cancel["annotations"]["stateChangingHint"], true);
        let run = tools
            .iter()
            .find(|tool| tool["name"] == "delegated_task_run")
            .expect("run tool");
        assert!(run["inputSchema"]["anyOf"]
            .as_array()
            .is_some_and(|items| items.iter().any(|item| item["required"] == json!(["validated_request_ref"]))));
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
        assert_eq!(
            wait["inputSchema"]["properties"]["timeout_ms"]["maximum"],
            MAX_TRANSPORT_SAFE_WAIT_MS
        );
        assert_eq!(
            execute["inputSchema"]["properties"]["timeout_ms"]["maximum"],
            MAX_TRANSPORT_SAFE_WAIT_MS
        );
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
        let task = json!({"status":"completed","objective":"demo","owner_site_id":"site-test","owner_site_root":root.to_string_lossy(),"result":{"acceptance_verdict":"passed","residual_risks":[],"verification":[{"command":"cargo test","status":"passed"}],"tools":["filesystem_search"],"step_states":{"review":{"kind":"review","status":"completed"}}},"acceptance":{"required_files":[{"path":"proof.txt","contains":"evidence"}],"required_tests":["cargo test"],"focused_tests":[{"command":"cargo test","status":"passed"}],"required_tools":["filesystem_search"],"forbidden_patterns":["forbidden-secret"],"verification_budget":{"max_attempts":2,"max_commands":2},"review_quorum":{"min_passed":1,"max_failed":0},"residual_risk_policy":"none_allowed"}});
        assert!(condition_passes(
            Some("all(step:review:completed,no_residual_risks)"),
            &task
        ));
        let (verdict, checks) = acceptance_verdict(&task, &root);
        assert_eq!(verdict, "passed");
        assert!(checks.len() >= 14);
        assert!(checks.iter().any(|check| check["kind"] == "output_contract"));
        assert!(checks.iter().any(|check| check["kind"] == "objective_outcome"));
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn acceptance_required_alias_reports_returned_fields() {
        let root = std::env::temp_dir().join(format!(
            "narada-delegated-task-required-fields-{}",
            uuid::Uuid::new_v4()
        ));
        let task = json!({
            "objective":"demo",
            "owner_site_id":"site-test",
            "owner_site_root":root.to_string_lossy(),
            "constraints":{"authority":"read"},
            "acceptance":{"required":["repository_name","current_branch","verification"]},
            "result":{"changed_files":[],"worker_outputs":[{"output":{"structured_output":{"repository_name":"marici","current_branch":"main","verification":"confirmed"}}}]}
        });
        let (_, checks) = acceptance_verdict(&task, &root);
        let fields = checks
            .iter()
            .find(|check| check["kind"] == "requested_fields")
            .expect("requested fields check");
        assert_eq!(
            fields["requested"],
            json!(["repository_name", "current_branch", "verification"])
        );
        assert_eq!(fields["status"], "passed");
    }

    #[test]
    fn acceptance_readback_refreshes_stale_requested_fields_check() {
        let root = std::env::temp_dir().join(format!(
            "narada-delegated-task-stale-acceptance-{}",
            uuid::Uuid::new_v4()
        ));
        let task = json!({
            "objective":"demo",
            "owner_site_id":"site-test",
            "owner_site_root":root.to_string_lossy(),
            "constraints":{"authority":"read"},
            "acceptance":{"required":["repository_name","current_branch","verification"]},
            "result":{
                "acceptance_checks":[
                    {"kind":"objective_present","status":"passed"},
                    {"kind":"requested_fields","requested":[],"returned":["repository_name","current_branch","verification"],"missing":[],"status":"not_applicable"}
                ],
                "worker_outputs":[{"output":{"structured_output":{"repository_name":"marici","current_branch":"main","verification":"confirmed"}}}]
            }
        });
        let result = task["result"].as_object().expect("result object");
        let checks = acceptance_checks_or_derive(&task, &root, Some(result));
        let fields = checks
            .as_array()
            .and_then(|checks| checks.iter().find(|check| check["kind"] == "requested_fields"))
            .expect("requested fields check");
        assert_eq!(
            fields["requested"],
            json!(["repository_name", "current_branch", "verification"])
        );
        assert_eq!(fields["status"], "passed");
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
        assert_eq!(waited["canonical_terminal_handoff"], true);
        assert_eq!(waited["result_readback_redundant"], true);
        let result = task_result(
            json!({"task_id":"task_terminal"}).as_object().unwrap(),
            &root,
        )
        .expect("result");
        assert_eq!(result["canonical_terminal_handoff"], true);
        assert_eq!(result["readback_role"], "secondary_durable_readback");
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

    fn complete_fixture_task(root: &Path, id: &str, output: Option<Value>) {
        let mut task = read_task(root, id).expect("fixture task");
        task["status"] = json!("completed");
        task["result"]["step_states"]["primary"]["status"] = json!("completed");
        task["result"]["step_states"]["primary"]["finished_at"] = json!(now());
        if let Some(output) = output {
            task["result"]["worker_outputs"] = json!([{"step_id":"primary","run_id":"fixture","status":"completed","output":{"structured_output":output,"summary_text":"fixture","truncated":false}}]);
        }
        write_task(root, &task).expect("complete fixture task");
    }

    #[test]
    fn cross_task_dependency_is_persisted_waits_and_imports_bounded_output() {
        let root = std::env::temp_dir().join(format!("narada-delegated-task-cross-dag-{}", uuid::Uuid::new_v4()));
        task_run(json!({"task_id":"task_a","objective":"extract typed list","constraints":{"authority":"read"},"execution":{"start":false}}).as_object().unwrap(), &root).expect("A");
        let b_args = json!({"task_id":"task_b","objective":"reduce typed list","constraints":{"authority":"read"},"depends_on_task_ids":["task_a"],"import_task_outputs":["task_a"],"workflow":{"steps":[{"id":"record","kind":"note"}]}});
        let b = task_run(b_args.as_object().unwrap(), &root).expect("B");
        assert_eq!(b["task_status"], "accepted_for_execution");
        let waiting = read_task(&root, "task_b").expect("persisted B");
        assert_eq!(waiting["depends_on_task_ids"], json!(["task_a"]));
        assert_eq!(waiting["import_task_outputs"], json!(["task_a"]));
        assert_eq!(waiting["external_dependencies"]["status"], "waiting");
        assert_eq!(waiting["result"]["step_states"]["record"]["status"], "pending");
        complete_fixture_task(&root, "task_a", Some(json!({"items":[3,1,2]})));
        let resolved = advance_task_closure(&root, "task_b", &[root.clone()], &mut std::collections::BTreeSet::new()).expect("automatic dependency closure");
        assert_eq!(resolved["status"], "completed");
        assert_eq!(resolved["external_dependencies"]["status"], "resolved");
        assert_eq!(resolved["result"]["imported_task_outputs"][0]["task_id"], "task_a");
        assert_eq!(resolved["result"]["imported_task_outputs"][0]["structured_output"]["items"], json!([3,1,2]));
        assert!(resolved["result"]["prior_step_outputs_ref"].as_str().is_some());
        let replay = task_run(b_args.as_object().unwrap(), &root).expect("idempotent replay");
        assert_eq!(replay["created"], false);
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn malformed_or_failed_predecessor_blocks_descendant_without_worker_start() {
        let root = std::env::temp_dir().join(format!("narada-delegated-task-cross-block-{}", uuid::Uuid::new_v4()));
        task_run(json!({"task_id":"task_a","objective":"extract","constraints":{"authority":"read"},"execution":{"start":false}}).as_object().unwrap(), &root).expect("A");
        task_run(json!({"task_id":"task_b","objective":"consume","constraints":{"authority":"read"},"depends_on_task_ids":["task_a"],"import_task_outputs":["task_a"],"execution":{"start":false}}).as_object().unwrap(), &root).expect("B");
        complete_fixture_task(&root, "task_a", None);
        let blocked = advance_task_closure(&root, "task_b", &[root.clone()], &mut std::collections::BTreeSet::new()).expect("blocked result");
        assert_eq!(blocked["status"], "failed");
        assert_eq!(blocked["external_dependencies"]["reason"], "predecessor_structured_output_missing");
        assert!(blocked["result"]["worker_refs"].as_array().is_some_and(Vec::is_empty));
        let events = task_events(json!({"task_id":"task_b","limit":20}).as_object().unwrap(), &root).expect("events");
        assert!(events["events"].as_array().is_some_and(|items| items.iter().any(|event| event["event_kind"] == "task_dependency_blocked")));
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn ordinary_status_reconciles_terminal_predecessor_failure() {
        let root = std::env::temp_dir().join(format!("narada-delegated-task-status-reconcile-{}", uuid::Uuid::new_v4()));
        task_run(json!({"task_id":"task_a","objective":"extract","constraints":{"authority":"read"},"execution":{"start":false}}).as_object().unwrap(), &root).expect("A");
        task_run(json!({"task_id":"task_b","objective":"consume","constraints":{"authority":"read"},"depends_on_task_ids":["task_a"],"execution":{"start":false}}).as_object().unwrap(), &root).expect("B");
        let mut predecessor = read_task(&root, "task_a").expect("predecessor");
        predecessor["status"] = json!("failed");
        write_task(&root, &predecessor).expect("fail predecessor");
        let status = task_status(json!({"task_id":"task_b"}).as_object().unwrap(), &root).expect("status reconciliation");
        assert_eq!(status["task_status"], "failed");
        assert_eq!(read_task(&root, "task_b").expect("persisted descendant")["external_dependencies"]["status"], "blocked");
        let events = task_events(json!({"task_id":"task_b","limit":20}).as_object().unwrap(), &root).expect("events");
        assert!(events["events"].as_array().is_some_and(|items| items.iter().any(|event| event["event_kind"] == "task_dependency_blocked")));
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn cross_task_dependencies_reject_missing_imports_cycles_and_authority_escalation() {
        let root = std::env::temp_dir().join(format!("narada-delegated-task-cross-invalid-{}", uuid::Uuid::new_v4()));
        task_run(json!({"task_id":"task_a","objective":"A","constraints":{"authority":"read"},"execution":{"start":false}}).as_object().unwrap(), &root).expect("A");
        let missing = task_run(json!({"task_id":"task_missing","objective":"missing","constraints":{"authority":"read"},"depends_on_task_ids":["absent"]}).as_object().unwrap(), &root).expect_err("missing predecessor");
        assert_eq!(missing["code"], "delegated_task_validation_failed");
        let undeclared = task_run(json!({"task_id":"task_import","objective":"import","constraints":{"authority":"read"},"import_task_outputs":["task_a"]}).as_object().unwrap(), &root).expect_err("undeclared import");
        assert_eq!(undeclared["code"], "delegated_task_validation_failed");
        let escalation = task_run(json!({"task_id":"task_write","objective":"write","constraints":{"authority":"write"},"depends_on_task_ids":["task_a"]}).as_object().unwrap(), &root).expect_err("authority escalation");
        assert_eq!(escalation["code"], "delegated_task_validation_failed");
        let mut a = read_task(&root, "task_a").expect("A task");
        a["depends_on_task_ids"] = json!(["task_cycle"]);
        write_task(&root, &a).expect("legacy cyclic edge fixture");
        let cycle = task_run(json!({"task_id":"task_cycle","objective":"cycle","constraints":{"authority":"read"},"depends_on_task_ids":["task_a"]}).as_object().unwrap(), &root).expect_err("cycle");
        assert_eq!(cycle["code"], "delegated_task_validation_failed");
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn delegated_tasks_propagate_queue_budget_and_default_to_provider_capacity() {
        let schema = constraints_schema();
        assert_eq!(schema["properties"]["queue_timeout_ms"]["default"], 300_000);
        assert!(CONSTRAINT_FIELDS.contains(&"queue_timeout_ms"));
        assert_eq!(max_concurrency(&json!({})), 1);
        assert_eq!(asynchronous_worker_constraints(
            &json!({"constraints":{"queue_timeout_ms":600000}}),
            &json!({})
        )["queue_timeout_ms"], 600_000);
    }
}
