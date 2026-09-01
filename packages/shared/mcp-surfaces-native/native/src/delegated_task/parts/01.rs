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
