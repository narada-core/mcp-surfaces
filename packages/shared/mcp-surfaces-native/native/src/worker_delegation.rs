use rusqlite::Connection;
use serde_json::{json, Map, Value};
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;
use time::OffsetDateTime;

const SERVER_NAME: &str = "worker-delegation-mcp";
const MAX_RUNS: usize = 200;
const MAX_FILE_BYTES: usize = 256_000;
const READ_TOOLS: &[(&str, &str)] = &[
    (
        "worker_output_show",
        "Read a bounded materialized worker artifact.",
    ),
    (
        "worker_operator_affordances",
        "Return UI-neutral worker inspection affordances.",
    ),
    (
        "worker_policy_inspect",
        "Inspect worker delegation policy without launching a worker.",
    ),
    (
        "worker_cognition_defaults_inspect",
        "Inspect local cognition defaults without changing them.",
    ),
    (
        "worker_config_resolve",
        "Resolve worker inputs and binding checks without launching a worker.",
    ),
    (
        "worker_run_status",
        "Inspect one durable worker run without waiting for completion.",
    ),
    (
        "worker_runs_list",
        "List recent durable worker runs with bounded compact records.",
    ),
    (
        "worker_run_wait",
        "Read one worker run's current state; native mode does not launch or poll a child.",
    ),
    (
        "worker_run_wait_batch",
        "Read bounded current states for several worker runs.",
    ),
    (
        "worker_runs_synthesize",
        "Summarize bounded worker run states.",
    ),
    (
        "worker_dashboard_describe",
        "Describe a bounded local worker dashboard projection.",
    ),
];
const MUTATING_TOOLS: &[&str] = &[
    "worker_cognition_defaults_update",
    "worker_run",
    "worker_edit",
    "worker_resume",
    "worker_run_reap",
    "worker_run_batch",
];

pub fn list_tools() -> Vec<Value> {
    let mut tools = vec![guidance_tool()];
    for (name, description) in READ_TOOLS {
        tools.push(tool(name, description, input_schema(name), true));
    }
    for name in MUTATING_TOOLS {
        tools.push(tool(
            name,
            "Execute or mutate worker state through the native Rust worker authority.",
            input_schema(name),
            false,
        ));
    }
    tools
}

pub fn auxiliary(method: &str, params: &Map<String, Value>) -> Result<Value, Value> {
    match method {
        "prompts/list" => Ok(
            json!({"prompts":[{"name":"worker_delegation_task","title":"Worker Delegation Task","description":"Inspect worker policy and durable run state before delegating execution.","arguments":[]}]}),
        ),
        "prompts/get" => {
            if params.get("name").and_then(Value::as_str) != Some("worker_delegation_task") {
                return Err(error("unknown_prompt", "unknown_prompt"));
            }
            Ok(
                json!({"description":"Inspect worker policy and durable run state before delegating execution.","messages":[{"role":"user","content":{"type":"text","text":"Use worker_policy_inspect and worker_config_resolve before execution; use worker_run_status, worker_runs_list, worker_run_wait, and worker_output_show for bounded readback. Keep worker launch and mutation with the owning authority."}}]}),
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
        "worker_guidance" => Ok(guidance(args)),
        "worker_policy_inspect" => Ok(policy(root)),
        "worker_cognition_defaults_inspect" => Ok(cognition_defaults(root)),
        "worker_config_resolve" => config_resolve(args, root),
        "worker_run_status" => run_status(args, root),
        "worker_runs_list" => runs_list(args, root),
        "worker_run_wait" => run_wait(args, root),
        "worker_run_wait_batch" => run_wait_batch(args, root),
        "worker_runs_synthesize" => runs_synthesize(args, root),
        "worker_dashboard_describe" => dashboard(args, root),
        "worker_output_show" => output_show(args, root),
        "worker_operator_affordances" => Ok(affordances()),
        "worker_cognition_defaults_update" => cognition_defaults_update(args, root),
        "worker_run" => worker_run(args, root, None, "worker_run"),
        "worker_edit" => worker_edit(args, root),
        "worker_resume" => worker_resume(args, root),
        "worker_run_reap" => worker_run_reap(args, root),
        "worker_run_batch" => worker_run_batch(args, root),
        _ => Err(error("unknown_tool", &format!("unknown_tool:{name}"))),
    }
}

fn guidance_tool() -> Value {
    tool(
        "worker_guidance",
        "Show model-facing operating guidance for worker-delegation MCP workflows.",
        json!({"type":"object","properties":{"workflow":{"type":"string"},"tool":{"type":"string"}},"additionalProperties":false}),
        true,
    )
}
fn guidance(args: &Map<String, Value>) -> Value {
    json!({"schema":"narada.worker.guidance.v1","status":"ok","server_name":SERVER_NAME,"requested":{"workflow":args.get("workflow").cloned().unwrap_or(Value::Null),"tool":args.get("tool").cloned().unwrap_or(Value::Null)},"first_use":["Inspect worker_policy_inspect.","Resolve worker inputs without launching with worker_config_resolve.","Launch with worker_run or worker_edit.","Read durable runs with worker_run_status or worker_run_wait.","Use worker_output_show for bounded artifact readback."],"boundaries":["The native Rust surface launches only the native Rust narada-agent-runtime-server.","Credentials remain environment-projected and are never returned.","Run records are bounded to the site worker-delegation root."]})
}

fn run_root(root: &Path) -> PathBuf {
    if let Some(value) = std::env::var_os("NARADA_WORKER_RUN_ROOT") {
        return PathBuf::from(value);
    }
    if root
        .file_name()
        .and_then(|v| v.to_str())
        .map(|v| v.eq_ignore_ascii_case(".narada"))
        .unwrap_or(false)
    {
        root.join("runtime/worker-delegation")
    } else {
        root.join(".narada/runtime/worker-delegation")
    }
}
fn is_within(path: &Path, root: &Path) -> bool {
    let p = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    let r = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    p == r || p.starts_with(&r)
}
fn safe_run_id(value: &str) -> Result<&str, Value> {
    if value.len() < 5
        || value.len() > 160
        || !value.starts_with("run-")
        || !value[4..]
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        Err(error("worker_run_id_invalid", "worker_run_id_invalid"))
    } else {
        Ok(value)
    }
}
fn run_id(args: &Map<String, Value>) -> Result<String, Value> {
    let id = args
        .get("run_id")
        .and_then(Value::as_str)
        .filter(|v| !v.trim().is_empty())
        .ok_or_else(|| error("worker_run_id_required", "worker_run_id_required"))?;
    safe_run_id(id.trim())?;
    Ok(id.trim().to_string())
}
fn read_json(path: &Path) -> Result<Value, Value> {
    let meta =
        fs::metadata(path).map_err(|_| error("worker_run_not_found", "worker_run_not_found"))?;
    if meta.len() > MAX_FILE_BYTES as u64 {
        return Err(error("worker_record_too_large", "worker_record_too_large"));
    }
    let text = fs::read_to_string(path)
        .map_err(|_| error("worker_record_read_failed", "worker_record_read_failed"))?;
    serde_json::from_str(&text)
        .map_err(|_| error("worker_record_invalid_json", "worker_record_invalid_json"))
}
fn run_path(root: &Path, id: &str) -> Result<PathBuf, Value> {
    safe_run_id(id)?;
    Ok(run_root(root).join(id).join("result.json"))
}
fn read_run(root: &Path, id: &str) -> Result<Value, Value> {
    read_json(&run_path(root, id)?)
}

fn policy(root: &Path) -> Value {
    json!({"schema":"narada.worker.policy.v1","status":"ok","server_name":SERVER_NAME,"run_root":run_root(root).to_string_lossy(),"site_root":root.to_string_lossy(),"allowed_roots":[root.to_string_lossy()],"allowed_runtimes":["narada-agent-runtime-server"],"allowed_authorities":["read","write","command"],"native_execution":"rust_authority","secret_projection":"environment_only"})
}
fn defaults_path(root: &Path) -> PathBuf {
    run_root(root).join("cognition-defaults.json")
}
fn empty_defaults() -> Value {
    json!({"low":{"provider":null,"model":null,"reasoning_effort":null},"medium":{"provider":null,"model":null,"reasoning_effort":null},"high":{"provider":null,"model":null,"reasoning_effort":null}})
}
fn cognition_defaults_for(root: &Path) -> Value {
    read_json(&defaults_path(root))
        .ok()
        .and_then(|v| v.get("defaults").cloned())
        .unwrap_or_else(empty_defaults)
}
fn cognition_defaults(root: &Path) -> Value {
    json!({"schema":"narada.worker.cognition_defaults.v1","status":"ok","defaults":cognition_defaults_for(root),"source":"native_contract","canonical_runtime":"narada-agent-runtime-server uses an immutable invocation plan","native_read_only":false})
}
fn config_resolve(args: &Map<String, Value>, root: &Path) -> Result<Value, Value> {
    let cwd = args
        .get("constraints")
        .and_then(Value::as_object)
        .and_then(|v| v.get("cwd"))
        .and_then(Value::as_str)
        .or_else(|| args.get("cwd").and_then(Value::as_str));
    let cwd = cwd.map(PathBuf::from).unwrap_or_else(|| root.to_path_buf());
    if !is_within(&cwd, root) {
        return Err(error(
            "worker_cwd_outside_allowed_roots",
            "worker_cwd_outside_allowed_roots",
        ));
    }
    Ok(
        json!({"schema":"narada.worker.config_resolve.v1","status":"ok","resolved":{"cwd":cwd.to_string_lossy(),"site_root":root.to_string_lossy(),"runtime":"narada-agent-runtime-server","authority":"read","launch":false},"diagnostics":[{"name":"native_execution","status":"boundary","message":"worker launch is delegated to the owning worker authority"}],"native_read_only":true}),
    )
}
fn run_status(args: &Map<String, Value>, root: &Path) -> Result<Value, Value> {
    let id = run_id(args)?;
    let run = read_run(root, &id)?;
    Ok(
        json!({"schema":"narada.worker.run_status.v1","status":"ok","run_id":id,"run":compact_run(&run),"native_read_only":true}),
    )
}
fn runs_list(args: &Map<String, Value>, root: &Path) -> Result<Value, Value> {
    let limit = args
        .get("limit")
        .and_then(Value::as_u64)
        .unwrap_or(20)
        .clamp(1, 200) as usize;
    let include_running = args
        .get("include_running")
        .and_then(Value::as_bool)
        .unwrap_or(true);
    let include_completed = args
        .get("include_completed")
        .and_then(Value::as_bool)
        .unwrap_or(true);
    let mut items = Vec::new();
    if let Ok(entries) = fs::read_dir(run_root(root)) {
        for entry in entries.filter_map(Result::ok).take(MAX_RUNS) {
            if !entry.path().is_dir() {
                continue;
            }
            let Some(id) = entry.file_name().to_str().map(str::to_string) else {
                continue;
            };
            if !id.starts_with("run-") {
                continue;
            }
            if let Ok(run) = read_run(root, &id) {
                let terminal =
                    !matches!(run.get("status").and_then(Value::as_str), Some("running"));
                if (terminal && include_completed) || (!terminal && include_running) {
                    items.push(compact_run(&run));
                }
            }
        }
    }
    items.sort_by(|a, b| {
        b.get("updated_at")
            .and_then(Value::as_str)
            .cmp(&a.get("updated_at").and_then(Value::as_str))
    });
    items.truncate(limit);
    Ok(
        json!({"schema":"narada.worker.runs_list.v1","status":"ok","count":items.len(),"limit":limit,"scanned":items.len(),"scan_limit":MAX_RUNS,"scan_truncated":false,"include_running":include_running,"include_completed":include_completed,"runs":items,"native_read_only":true}),
    )
}
fn run_wait(args: &Map<String, Value>, root: &Path) -> Result<Value, Value> {
    let id = run_id(args)?;
    let run = read_run(root, &id)?;
    let running = run.get("status").and_then(Value::as_str) == Some("running");
    Ok(
        json!({"schema":"narada.worker.run_wait.v1","status":"ok","wait":{"status":if running{"timed_out"}else{"finished"},"waited":false,"timeout_ms":args.get("timeout_ms").cloned().unwrap_or(json!(0)),"native_execution":"not_polled"},"run":compact_run(&run),"native_read_only":true}),
    )
}
fn run_wait_batch(args: &Map<String, Value>, root: &Path) -> Result<Value, Value> {
    let ids = args
        .get("run_ids")
        .and_then(Value::as_array)
        .ok_or_else(|| error("worker_run_ids_required", "worker_run_ids_required"))?;
    let mut runs = Vec::new();
    for id in ids.iter().take(50).filter_map(Value::as_str) {
        let mut item = json!({"run_id":id,"status":"error"});
        if let Ok(run) = read_run(root, id) {
            item = json!({"run_id":id,"status":"ok","run":compact_run(&run)});
        }
        runs.push(item);
    }
    Ok(
        json!({"schema":"narada.worker.run_wait_batch.v1","status":"ok","requested_count":ids.len().min(50),"runs":runs,"native_read_only":true}),
    )
}
fn runs_synthesize(args: &Map<String, Value>, root: &Path) -> Result<Value, Value> {
    let ids = args
        .get("run_ids")
        .and_then(Value::as_array)
        .ok_or_else(|| error("worker_run_ids_required", "worker_run_ids_required"))?;
    let mut counts = Map::new();
    let mut found = Vec::new();
    for id in ids.iter().take(50).filter_map(Value::as_str) {
        if let Ok(run) = read_run(root, id) {
            let status = run
                .get("status")
                .and_then(Value::as_str)
                .unwrap_or("unknown");
            *counts.entry(status.to_string()).or_insert(Value::from(0)) =
                Value::from(counts.get(status).and_then(Value::as_u64).unwrap_or(0) + 1);
            found.push(id);
        }
    }
    Ok(
        json!({"schema":"narada.worker.runs_synthesis.v1","status":"ok","requested_count":ids.len().min(50),"run_ids":found,"synthesis":{"counts":counts,"native_read_only":true}}),
    )
}
fn dashboard(args: &Map<String, Value>, root: &Path) -> Result<Value, Value> {
    let mode = match args
        .get("mode")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        Some("all_active") => "all_active",
        Some("single_run") => "single_run",
        Some(_) => {
            return Err(error(
                "worker_invalid_dashboard_mode",
                "worker_invalid_dashboard_mode",
            ))
        }
        None if args
            .get("run_id")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .is_some() =>
        {
            "single_run"
        }
        None => "all_active",
    };
    let include_terminal = args
        .get("include_terminal")
        .and_then(Value::as_bool)
        .unwrap_or(mode == "single_run");
    let limit = args
        .get("limit")
        .and_then(Value::as_u64)
        .unwrap_or(25)
        .clamp(1, 200) as usize;
    let mut runs = if mode == "single_run" {
        let id = run_id(args)?;
        vec![compact_run(&read_run(root, &id)?)]
    } else {
        let list = runs_list(&json!({"limit":200}).as_object().unwrap(), root)?;
        list.get("runs")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
    };
    if !include_terminal {
        runs.retain(|run| !is_terminal_status(run.get("status").and_then(Value::as_str)));
    }
    runs.truncate(limit);
    let total = runs.len();
    let active = runs
        .iter()
        .filter(|run| !is_terminal_status(run.get("status").and_then(Value::as_str)))
        .count();
    let failed = runs
        .iter()
        .filter(|run| {
            matches!(
                run.get("status").and_then(Value::as_str),
                Some("failed" | "completed_with_errors")
            )
        })
        .count();
    let nodes = runs
        .iter()
        .map(|run| {
            json!({
                "id":run.get("run_id").cloned().unwrap_or(Value::Null),
                "label":run.get("run_id").cloned().unwrap_or(Value::Null),
                "status":run.get("status").cloned().unwrap_or(Value::Null),
                "worker_session_id":run.get("worker_session_id").cloned().unwrap_or(Value::Null),
            })
        })
        .collect::<Vec<_>>();
    let pending = runs.iter().filter(|run| !is_terminal_status(run.get("status").and_then(Value::as_str))).map(|run| json!({
        "gate_id":format!("join:{}", run.get("run_id").and_then(Value::as_str).unwrap_or("")),
        "run_id":run.get("run_id").cloned().unwrap_or(Value::Null),
        "status":"pending",
        "waiting_for":[run.get("run_id").cloned().unwrap_or(Value::Null)],
    })).collect::<Vec<_>>();
    Ok(json!({
        "schema":"narada.worker.dashboard.v1",
        "status":"ok",
        "mode":mode,
        "include_terminal":include_terminal,
        "dashboard":{
            "kind":"read_only_dashboard_descriptor",
            "server":{"started":false,"reason":"mcp_tool_is_request_response; use the listed JSON API tool calls or wrap them in a local HTTP process if a long-lived dashboard is required"},
            "suggested_local_command":Value::Null,
            "api_endpoints":[
                {"path":"mcp://tools/worker_dashboard_describe","method":"tools/call","description":"Read-only compact dashboard payload for one run or all active runs.","arguments":{"mode":"all_active|single_run","run_id":"optional run id","include_terminal":"boolean","limit":"1..200"}},
                {"path":"mcp://tools/worker_runs_list","method":"tools/call","description":"Recent run index with compact status fields.","arguments":{"include_running":true,"include_completed":true,"verbose":false}},
                {"path":"mcp://tools/worker_run_status","method":"tools/call","description":"Full status for one run, including artifact readback and progress.","arguments":{"run_id":"run-*"}},
                {"path":"mcp://resources/worker-artifact","method":"resources/read","description":"Read run artifacts such as events.jsonl and result.json for primary run-root records."}
            ],
            "refresh":{"tool":"worker_dashboard_describe","arguments":{"mode":mode,"include_terminal":include_terminal,"limit":limit}},
        },
        "counts":{"total":total,"active":active,"terminal":total-active,"failed":failed,"runs":total},
        "runs":runs,
        "topology":{"graph_kind":"run_dag","dependency_source":"worker-delegation run records; explicit inter-run dependencies are not currently recorded","nodes":nodes,"edges":[]},
        "steps":[],
        "pending_join_gates":pending,
        "event_stream":[],
        "native_read_only":true
    }))
}

fn is_terminal_status(status: Option<&str>) -> bool {
    matches!(
        status,
        Some("completed" | "completed_with_errors" | "failed" | "cancelled")
    )
}
fn output_show(args: &Map<String, Value>, root: &Path) -> Result<Value, Value> {
    let reference = args
        .get("ref")
        .or_else(|| args.get("output_ref"))
        .and_then(Value::as_str)
        .ok_or_else(|| error("worker_output_ref_required", "worker_output_ref_required"))?;
    let raw = reference
        .strip_prefix("worker-artifact:")
        .ok_or_else(|| error("worker_output_ref_invalid", "worker_output_ref_invalid"))?;
    let (id, name) = raw
        .split_once('/')
        .ok_or_else(|| error("worker_output_ref_invalid", "worker_output_ref_invalid"))?;
    safe_run_id(id)?;
    if name.is_empty()
        || name.len() > 100
        || name.contains('/')
        || name.contains('\\')
        || name.contains("..")
    {
        return Err(error(
            "worker_output_ref_invalid",
            "worker_output_ref_invalid",
        ));
    }
    let path = run_root(root).join(id).join(name);
    let byte_size = fs::metadata(&path)
        .map_err(|_| error("worker_output_not_found", "worker_output_not_found"))?
        .len();
    if byte_size > MAX_FILE_BYTES as u64 {
        return Err(error("worker_output_too_large", "worker_output_too_large"));
    }
    let bytes =
        fs::read(&path).map_err(|_| error("worker_output_not_found", "worker_output_not_found"))?;
    let chars = String::from_utf8_lossy(&bytes).chars().collect::<Vec<_>>();
    let offset = args
        .get("offset")
        .and_then(Value::as_u64)
        .unwrap_or(0)
        .min(chars.len() as u64) as usize;
    let limit = args
        .get("limit")
        .and_then(Value::as_u64)
        .unwrap_or(MAX_FILE_BYTES as u64)
        .min(MAX_FILE_BYTES as u64) as usize;
    let chunk = chars.iter().skip(offset).take(limit).collect::<String>();
    let end = offset + chunk.chars().count();
    Ok(
        json!({"schema":"narada.worker.output_page.v1","status":"ok","ref":reference,"path":path.to_string_lossy(),"byte_size":byte_size,"offset":offset,"limit":limit,"next_offset":if end<chars.len(){json!(end)}else{Value::Null},"output_text":chunk,"output_truncated":end<chars.len(),"native_read_only":true}),
    )
}
fn affordances() -> Value {
    json!({"schema":"narada.worker.operator_affordances.v1","status":"ok","read_tools":READ_TOOLS.iter().map(|(n,_)|*n).collect::<Vec<_>>(),"mutation_tools":MUTATING_TOOLS,"native_read_only":false,"execution_authority":"rust"})
}
fn compact_run(run: &Value) -> Value {
    let o = run.as_object().cloned().unwrap_or_default();
    json!({"run_id":o.get("run_id"),"status":o.get("status"),"completion_state":o.get("completion_state"),"authority":o.get("authority"),"worker_session_id":o.get("worker_session_id"),"started_at":o.get("timing").and_then(|v|v.get("started_at")),"finished_at":o.get("timing").and_then(|v|v.get("finished_at")),"summary_preview":o.get("summary").or_else(||o.get("last_message")),"error_preview":o.get("error"),"updated_at":o.get("updated_at").or_else(||o.get("timing").and_then(|v|v.get("finished_at")))})
}

fn now() -> String {
    OffsetDateTime::now_utc()
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_else(|_| "1970-01-01T00:00:00Z".into())
}
fn required_string<'a>(
    args: &'a Map<String, Value>,
    key: &str,
    code: &str,
) -> Result<&'a str, Value> {
    args.get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .ok_or_else(|| error(code, code))
}
fn write_json_atomic(path: &Path, value: &Value) -> Result<(), Value> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|_| error("worker_write_failed", "worker_write_failed"))?;
    }
    let temp = path.with_extension(format!("tmp-{}", uuid::Uuid::new_v4()));
    fs::write(
        &temp,
        serde_json::to_vec_pretty(value)
            .map_err(|_| error("worker_json_failed", "worker_json_failed"))?,
    )
    .map_err(|_| error("worker_write_failed", "worker_write_failed"))?;
    fs::rename(&temp, path).map_err(|_| error("worker_write_failed", "worker_write_failed"))
}
fn cognition_defaults_update(args: &Map<String, Value>, root: &Path) -> Result<Value, Value> {
    let provider = required_string(args, "provider", "worker_cognition_provider_required")?;
    let cognition = required_string(args, "cognition", "worker_cognition_required")?;
    if !matches!(cognition, "low" | "medium" | "high") {
        return Err(error(
            "worker_cognition_invalid",
            "worker_cognition_invalid",
        ));
    }
    let model = required_string(args, "model", "worker_model_required")?;
    let effort = required_string(args, "reasoning_effort", "worker_reasoning_effort_required")?;
    let mut defaults = cognition_defaults_for(root);
    defaults[cognition] = json!({"provider":provider,"model":model,"reasoning_effort":effort});
    let record = json!({"schema":"narada.worker.cognition_defaults_store.v1","updated_at":now(),"actor":args.get("actor").cloned().unwrap_or(Value::Null),"defaults":defaults});
    write_json_atomic(&defaults_path(root), &record)?;
    Ok(
        json!({"schema":"narada.worker.cognition_defaults.v1","status":"updated","cognition":cognition,"default":record["defaults"][cognition],"defaults":record["defaults"],"source":"native_rust_authority"}),
    )
}

fn runtime_command(root: &Path) -> Result<PathBuf, Value> {
    if let Some(path) = std::env::var_os("NARADA_AGENT_RUNTIME_SERVER_NATIVE")
        .map(PathBuf::from)
        .filter(|p| p.is_file())
    {
        return Ok(path);
    }
    let src = std::env::var_os("NARADA_SRC_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|| root.parent().unwrap_or(root).join("src"));
    let candidates=[src.join("narada/packages/agent-runtime-server/native/target/release/narada-agent-runtime-server-rust.exe"),src.join("narada/packages/agent-runtime-server/native/target/release/narada-agent-runtime-server-rust")];
    candidates.into_iter().find(|p| p.is_file()).ok_or_else(|| {
        error(
            "worker_runtime_unavailable",
            "worker_runtime_unavailable:narada-agent-runtime-server-rust",
        )
    })
}
fn invocation_plan_binding(root: &Path, plan_ref: &str) -> Result<(String, Option<String>), Value> {
    let context =
        read_json(&root.join(".narada/intelligence-launch-context.json")).map_err(|_| {
            error(
                "worker_intelligence_context_required",
                "worker_intelligence_context_required",
            )
        })?;
    let registry = context
        .get("registry_db_path")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| {
            error(
                "worker_intelligence_registry_required",
                "worker_intelligence_registry_required",
            )
        })?;
    let registry = PathBuf::from(registry);
    let registry = if registry.is_absolute() {
        registry
    } else {
        root.join(registry)
    };
    let connection =
        Connection::open_with_flags(registry, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).map_err(
            |_| {
                error(
                    "worker_intelligence_registry_unavailable",
                    "worker_intelligence_registry_unavailable",
                )
            },
        )?;
    let document: String = connection
        .query_row(
            "SELECT doc FROM invocation_plans WHERE id = ?1",
            [plan_ref],
            |row| row.get(0),
        )
        .map_err(|_| {
            error(
                "worker_canonical_invocation_plan_not_found",
                "worker_canonical_invocation_plan_not_found",
            )
        })?;
    let plan: Value = serde_json::from_str(&document).map_err(|_| {
        error(
            "worker_canonical_invocation_plan_invalid",
            "worker_canonical_invocation_plan_invalid",
        )
    })?;
    if plan.get("id").and_then(Value::as_str) != Some(plan_ref) {
        return Err(error(
            "worker_canonical_invocation_plan_mismatch",
            "worker_canonical_invocation_plan_mismatch",
        ));
    }
    let provider = plan
        .pointer("/selected/inference_provider/id")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            error(
                "worker_canonical_invocation_provider_missing",
                "worker_canonical_invocation_provider_missing",
            )
        })?;
    let mode = match provider {
        "inference-provider:codex-subscription" => "codex-subscription",
        _ => {
            return Err(error(
                "worker_native_provider_unsupported",
                "worker_native_provider_unsupported",
            ))
        }
    };
    let model = plan
        .pointer("/selected/model/id")
        .and_then(Value::as_str)
        .and_then(|value| value.strip_prefix("model:"))
        .map(str::to_string);
    Ok((mode.to_string(), model))
}
fn codex_command() -> Option<PathBuf> {
    if let Some(command) = std::env::var_os("NARADA_NATIVE_CODEX_COMMAND") {
        return Some(PathBuf::from(command));
    }
    std::env::var_os("PATH").and_then(|path| {
        std::env::split_paths(&path).find_map(|directory| {
            ["codex.exe", "codex.cmd", "codex"]
                .into_iter()
                .map(|name| directory.join(name))
                .find(|candidate| candidate.is_file())
        })
    })
}
fn instruction(args: &Map<String, Value>) -> Result<String, Value> {
    let intent = args.get("intent").and_then(Value::as_object);
    for key in ["instruction", "task", "goal", "summary"] {
        if let Some(v) = intent
            .and_then(|m| m.get(key))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|v| !v.is_empty())
        {
            return Ok(v.to_string());
        }
    }
    Err(error(
        "worker_intent_instruction_required",
        "worker_intent_instruction_required",
    ))
}
fn authority(args: &Map<String, Value>) -> Result<&str, Value> {
    let value = args
        .get("constraints")
        .and_then(Value::as_object)
        .and_then(|v| v.get("authority"))
        .and_then(Value::as_str)
        .unwrap_or("read");
    if matches!(value, "read" | "write" | "command") {
        Ok(value)
    } else {
        Err(error(
            "worker_authority_invalid",
            "worker_authority_invalid",
        ))
    }
}
fn worker_run(
    args: &Map<String, Value>,
    root: &Path,
    resume: Option<String>,
    tool_name: &str,
) -> Result<Value, Value> {
    let prompt = instruction(args)?;
    let auth = authority(args)?.to_string();
    let constraints = args.get("constraints").and_then(Value::as_object);
    for key in ["provider", "cognition"] {
        if constraints.and_then(|value| value.get(key)).is_some() {
            return Err(error(
                "worker_canonical_invocation_plan_override_rejected",
                "worker_canonical_invocation_plan_override_rejected",
            ));
        }
    }
    let plan_ref = constraints
        .and_then(|value| value.get("invocation_plan_ref"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .or_else(|| {
            std::env::var("NARADA_INTELLIGENCE_PLAN_REF")
                .ok()
                .filter(|value| !value.trim().is_empty())
        })
        .ok_or_else(|| {
            error(
                "worker_canonical_invocation_plan_required",
                "worker_canonical_invocation_plan_required",
            )
        })?;
    if !plan_ref.starts_with("plan:")
        || !plan_ref[5..].chars().all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '.' | '_' | ':' | '-')
        })
    {
        return Err(error(
            "worker_canonical_invocation_plan_invalid",
            "worker_canonical_invocation_plan_invalid",
        ));
    }
    let (provider_mode, provider_model) = invocation_plan_binding(root, &plan_ref)?;
    let cwd = constraints
        .and_then(|v| v.get("cwd"))
        .and_then(Value::as_str)
        .map(PathBuf::from)
        .unwrap_or_else(|| root.to_path_buf());
    if !is_within(&cwd, root) {
        return Err(error(
            "worker_cwd_outside_allowed_roots",
            "worker_cwd_outside_allowed_roots",
        ));
    }
    let runtime = runtime_command(root)?;
    let id = format!("run-{}", uuid::Uuid::new_v4().simple());
    let session = resume.clone().unwrap_or_else(|| id.clone());
    let dir = run_root(root).join(&id);
    fs::create_dir_all(&dir)
        .map_err(|_| error("worker_run_create_failed", "worker_run_create_failed"))?;
    let started = now();
    let request = json!({"schema":"narada.worker.request.v1","run_id":id,"origin_tool":tool_name,"intent":args.get("intent"),"constraints":args.get("constraints"),"resume_worker_session_id":resume});
    write_json_atomic(&dir.join("request.json"), &request)?;
    fs::write(dir.join("worker_prompt.txt"), &prompt)
        .map_err(|_| error("worker_write_failed", "worker_write_failed"))?;
    let running = json!({"schema":"narada.worker.run.v1","run_id":id,"status":"running","completion_state":"pending","runtime":"narada-agent-runtime-server","authority":auth,"worker_session_id":session,"origin_tool":tool_name,"pid":null,"summary":null,"error":null,"timing":{"started_at":started,"finished_at":null,"duration_ms":null},"artifacts":{"request":dir.join("request.json").to_string_lossy(),"events":dir.join("events.jsonl").to_string_lossy(),"diagnostic":dir.join("diagnostic.log").to_string_lossy(),"last_message":dir.join("last_message.json").to_string_lossy()}});
    write_json_atomic(&dir.join("result.json"), &running)?;
    let root_owned = root.to_path_buf();
    let dir_owned = dir.clone();
    let id_owned = id.clone();
    let session_owned = session.clone();
    let resume_owned = resume.clone();
    let auth_owned = auth.clone();
    thread::Builder::new()
        .name(format!("worker-{id}"))
        .spawn(move || {
            complete_native_run(
                runtime,
                cwd,
                root_owned,
                dir_owned,
                id_owned.clone(),
                id_owned,
                session_owned,
                resume_owned,
                auth_owned,
                plan_ref,
                provider_mode,
                provider_model,
                prompt,
            )
        })
        .map_err(|_| error("worker_launch_failed", "worker_launch_failed"))?;
    Ok(running)
}
fn worker_edit(args: &Map<String, Value>, root: &Path) -> Result<Value, Value> {
    let prompt =
        required_string(args, "instruction", "worker_edit_instruction_required")?.to_string();
    let mut constraints = args
        .get("constraints")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    constraints.insert("authority".into(), json!("write"));
    if let Some(cwd) = args.get("cwd") {
        constraints.insert("cwd".into(), cwd.clone());
    }
    if let Some(plan_ref) = args.get("invocation_plan_ref") {
        constraints.insert("invocation_plan_ref".into(), plan_ref.clone());
    }
    worker_run(
        json!({"intent":{"instruction":prompt,"mode":"edit"},"constraints":constraints})
            .as_object()
            .unwrap(),
        root,
        None,
        "worker_edit",
    )
}
fn worker_resume(args: &Map<String, Value>, root: &Path) -> Result<Value, Value> {
    let session =
        required_string(args, "worker_session_id", "worker_session_id_required")?.to_string();
    worker_run(args, root, Some(session), "worker_resume")
}
fn worker_run_batch(args: &Map<String, Value>, root: &Path) -> Result<Value, Value> {
    let requests = args
        .get("requests")
        .and_then(Value::as_array)
        .filter(|v| !v.is_empty())
        .ok_or_else(|| {
            error(
                "worker_run_batch_requests_required",
                "worker_run_batch_requests_required",
            )
        })?;
    if requests.len() > 50 {
        return Err(error(
            "worker_run_batch_too_large",
            "worker_run_batch_too_large",
        ));
    }
    let started = now();
    let mut runs = Vec::new();
    let mut failures = Vec::new();
    for (index, item) in requests.iter().enumerate() {
        match item
            .as_object()
            .ok_or_else(|| {
                error(
                    "worker_run_batch_item_invalid",
                    "worker_run_batch_item_invalid",
                )
            })
            .and_then(|v| worker_run(v, root, None, "worker_run_batch"))
        {
            Ok(run) => {
                runs.push(json!({"index":index,"run_id":run["run_id"],"status":run["status"]}))
            }
            Err(err) => failures.push(json!({"index":index,"error":err})),
        }
    }
    Ok(
        json!({"schema":"narada.worker.run_batch.v1","status":if failures.is_empty(){"ok"}else{"completed_with_errors"},"requested_count":requests.len(),"started_count":runs.len(),"failed_count":failures.len(),"run_ids":runs.iter().map(|v|v["run_id"].clone()).collect::<Vec<_>>(),"runs":runs,"failures":failures,"timing":{"started_at":started,"finished_at":now()}}),
    )
}
fn worker_run_reap(args: &Map<String, Value>, root: &Path) -> Result<Value, Value> {
    let id = run_id(args)?;
    let reason = required_string(args, "reason", "worker_run_reap_reason_required")?;
    let path = run_path(root, &id)?;
    let mut run = read_json(&path)?;
    if is_terminal_status(run.get("status").and_then(Value::as_str)) {
        return Ok(
            json!({"schema":"narada.worker.run_reap.v1","status":"already_terminal","run_id":id,"reaped":false,"run":run}),
        );
    }
    if args.get("force").and_then(Value::as_bool) != Some(true) {
        return Err(error(
            "worker_run_reap_refused_active_run",
            "worker_run_reap_refused_active_run",
        ));
    }
    run["status"] = json!("cancelled");
    run["completion_state"] = json!("partial");
    run["error"] = json!(format!("worker_run_reaped:{reason}"));
    run["timing"]["finished_at"] = json!(now());
    run["reaped"] = json!({"reason":reason,"force":true,"at":now()});
    write_json_atomic(&path, &run)?;
    Ok(
        json!({"schema":"narada.worker.run_reap.v1","status":"reaped","run_id":id,"reaped":true,"run":run}),
    )
}
fn event_text(event: &Value) -> Option<String> {
    for key in ["content", "message", "text", "summary"] {
        if let Some(value) = event
            .get(key)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|v| !v.is_empty())
        {
            return Some(value.to_string());
        }
    }
    event
        .get("message")
        .and_then(Value::as_object)
        .and_then(|message| {
            ["content", "text", "summary"].into_iter().find_map(|key| {
                message
                    .get(key)
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|v| !v.is_empty())
                    .map(str::to_string)
            })
        })
}
fn complete_native_run(
    runtime: PathBuf,
    cwd: PathBuf,
    site_root: PathBuf,
    dir: PathBuf,
    id: String,
    runtime_session: String,
    session: String,
    resume_session: Option<String>,
    authority: String,
    plan_ref: String,
    provider_mode: String,
    provider_model: Option<String>,
    prompt: String,
) {
    let result_path = dir.join("result.json");
    let events_path = dir.join("events.jsonl");
    let diagnostic_path = dir.join("diagnostic.log");
    let started = std::time::Instant::now();
    let mut command = Command::new(&runtime);
    command
        .args([
            "--raw-jsonl",
            "--authority",
            &authority,
            "--session",
            &runtime_session,
        ])
        .current_dir(&cwd)
        .env("NARADA_SITE_ROOT", &site_root)
        .env("NARADA_WORKSPACE_ROOT", &cwd)
        .env("NARADA_CARRIER_SESSION_ID", &runtime_session)
        .env("NARADA_INTELLIGENCE_PLAN_REF", &plan_ref)
        .env("NARADA_NATIVE_PROVIDER_MODE", provider_mode)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(model) = provider_model {
        command.env("NARADA_NATIVE_CODEX_MODEL", model);
    }
    if let Some(codex) = codex_command() {
        command.env("NARADA_NATIVE_CODEX_COMMAND", codex);
    }
    if let Some(resume_session) = resume_session {
        command.env("NARADA_NATIVE_CODEX_RESUME_SESSION_ID", resume_session);
    }
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(err) => {
            let failed = json!({"schema":"narada.worker.run.v1","run_id":id,"status":"failed","completion_state":"absent","runtime":"narada-agent-runtime-server","authority":authority,"worker_session_id":session,"summary":null,"error":format!("worker_launch_failed:{err}"),"timing":{"started_at":now(),"finished_at":now(),"duration_ms":0}});
            let _ = write_json_atomic(&result_path, &failed);
            return;
        }
    };
    if let Ok(mut running) = read_json(&result_path) {
        running["pid"] = json!(child.id());
        let _ = write_json_atomic(&result_path, &running);
    }
    let stderr = child.stderr.take();
    let diagnostics = diagnostic_path.clone();
    thread::spawn(move || {
        if let Some(mut source) = stderr {
            if let Ok(mut target) = fs::File::create(diagnostics) {
                let _ = std::io::copy(&mut source, &mut target);
            }
        }
    });
    if let Some(mut stdin) = child.stdin.take() {
        let frame = json!({"id":format!("worker-conversation-{id}"),"method":"session.submit","params":{"content":prompt,"source":"programmatic_worker","source_id":"worker-delegation-mcp"}});
        let _ = writeln!(stdin, "{frame}");
        let _ = stdin.flush();
        let mut events = fs::File::create(&events_path).ok();
        let mut assistant = None;
        let mut provider_session = None;
        let mut runtime_error = None;
        let mut close_sent = false;
        if let Some(stdout) = child.stdout.take() {
            let (line_tx, line_rx) = mpsc::channel();
            thread::spawn(move || {
                for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                    if line_tx.send(line).is_err() {
                        break;
                    }
                }
            });
            loop {
                if read_json(&result_path)
                    .ok()
                    .and_then(|v| v.get("status").and_then(Value::as_str).map(str::to_string))
                    .as_deref()
                    == Some("cancelled")
                {
                    let _ = child.kill();
                    let _ = child.wait();
                    return;
                }
                let line = match line_rx.recv_timeout(Duration::from_millis(50)) {
                    Ok(line) => line,
                    Err(mpsc::RecvTimeoutError::Timeout) => continue,
                    Err(mpsc::RecvTimeoutError::Disconnected) => break,
                };
                if let Some(file) = events.as_mut() {
                    let _ = writeln!(file, "{line}");
                }
                let Ok(event) = serde_json::from_str::<Value>(&line) else {
                    continue;
                };
                let kind = event
                    .get("event")
                    .or_else(|| event.get("type"))
                    .and_then(Value::as_str)
                    .unwrap_or("");
                if kind == "assistant_message" {
                    assistant = event_text(&event);
                }
                if let Some(value) = event
                    .get("provider_session_id")
                    .and_then(Value::as_str)
                    .filter(|value| !value.trim().is_empty())
                {
                    provider_session = Some(value.to_string());
                }
                if matches!(
                    kind,
                    "error"
                        | "turn_failed"
                        | "carrier_turn_failed"
                        | "carrier_turn_blocked"
                        | "session_control_rejected"
                ) {
                    runtime_error = event_text(&event).or_else(|| Some(kind.into()));
                }
                if matches!(
                    kind,
                    "turn_complete"
                        | "carrier_turn_completed"
                        | "turn_failed"
                        | "carrier_turn_failed"
                        | "carrier_turn_blocked"
                ) && !close_sent
                {
                    close_sent = true;
                    let close = json!({"id":format!("worker-close-{id}"),"method":"session.close","params":{}});
                    let _ = writeln!(stdin, "{close}");
                    let _ = stdin.flush();
                }
                if kind == "session_closed" {
                    break;
                }
            }
        }
        drop(stdin);
        let status = child.wait().ok();
        let finished = now();
        let successful = status.as_ref().is_some_and(|v| v.success())
            && assistant.is_some()
            && runtime_error.is_none();
        if let Some(message) = assistant.as_ref() {
            let _ = write_json_atomic(
                &dir.join("last_message.json"),
                &json!({"summary":message,"deliverables":[],"open_questions":[],"next_actions":[]}),
            );
        }
        let payload = json!({"schema":"narada.worker.run.v1","run_id":id,"status":if successful{"completed"}else{"failed"},"completion_state":if assistant.is_some(){"complete"}else{"absent"},"runtime":"narada-agent-runtime-server","authority":authority,"worker_session_id":provider_session.unwrap_or(session),"pid":child.id(),"summary":assistant,"error":runtime_error.or_else(||if successful{None}else{Some(format!("worker_runtime_exit:{:?}",status.and_then(|v|v.code())))}),"timing":{"started_at":Value::Null,"finished_at":finished,"duration_ms":started.elapsed().as_millis() as u64},"artifacts":{"request":dir.join("request.json").to_string_lossy(),"events":events_path.to_string_lossy(),"diagnostic":diagnostic_path.to_string_lossy(),"last_message":dir.join("last_message.json").to_string_lossy()}});
        let _ = write_json_atomic(&result_path, &payload);
    }
}
fn error(code: &str, message: &str) -> Value {
    json!({"schema":"narada.worker.error.v1","code":code,"message":message})
}
fn input_schema(name: &str) -> Value {
    match name {
        "worker_run_status" | "worker_run_wait" => {
            json!({"type":"object","properties":{"run_id":{"type":"string"},"timeout_ms":{"type":"integer","minimum":0,"maximum":300000}},"required":["run_id"],"additionalProperties":false})
        }
        "worker_run_wait_batch" | "worker_runs_synthesize" => {
            json!({"type":"object","properties":{"run_ids":{"type":"array","items":{"type":"string"}}},"required":["run_ids"],"additionalProperties":false})
        }
        "worker_output_show" => {
            json!({"type":"object","properties":{"ref":{"type":"string"},"output_ref":{"type":"string"},"offset":{"type":"integer","minimum":0},"limit":{"type":"integer","minimum":0}},"additionalProperties":false})
        }
        _ => json!({"type":"object","additionalProperties":true}),
    }
}
fn tool(name: &str, description: &str, schema: Value, read_only: bool) -> Value {
    json!({"name":name,"description":description,"annotations":{"title":name,"readOnlyHint":read_only,"destructiveHint":!read_only,"idempotentHint":read_only,"openWorldHint":false},"inputSchema":schema,"outputSchema":{"type":"object","additionalProperties":true}})
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_worker_reads_bounded_run_records() {
        let root = std::env::temp_dir().join(format!("narada-worker-{}", uuid::Uuid::new_v4()));
        let dir = run_root(&root).join("run-2026-01-01T00-00-00Z");
        fs::create_dir_all(&dir).expect("dir");
        fs::write(
            dir.join("result.json"),
            r#"{"run_id":"run-2026-01-01T00-00-00Z","status":"completed","summary":"done"}"#,
        )
        .expect("record");
        let listed = runs_list(&json!({"limit":1}).as_object().unwrap(), &root).expect("list");
        assert_eq!(listed["count"], 1);
        assert_eq!(
            run_status(
                &json!({"run_id":"run-2026-01-01T00-00-00Z"})
                    .as_object()
                    .unwrap(),
                &root
            )
            .expect("status")["run"]["status"],
            "completed"
        );
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn native_worker_output_supports_bounded_paging() {
        let root =
            std::env::temp_dir().join(format!("narada-worker-output-{}", uuid::Uuid::new_v4()));
        let dir = run_root(&root).join("run-2026-01-01T00-00-00Z");
        fs::create_dir_all(&dir).expect("dir");
        fs::write(dir.join("worker_prompt.txt"), "0123456789").expect("artifact");
        let page = output_show(&json!({"ref":"worker-artifact:run-2026-01-01T00-00-00Z/worker_prompt.txt","offset":3,"limit":4}).as_object().unwrap(), &root).expect("page");
        assert_eq!(page["output_text"], "3456");
        assert_eq!(page["next_offset"], 7);
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn native_worker_dashboard_respects_mode_and_terminal_filter() {
        let root =
            std::env::temp_dir().join(format!("narada-worker-dashboard-{}", uuid::Uuid::new_v4()));
        let completed = run_root(&root).join("run-2026-01-01T00-00-00Z");
        let running = run_root(&root).join("run-2026-01-01T00-00-01Z");
        fs::create_dir_all(&completed).expect("completed dir");
        fs::create_dir_all(&running).expect("running dir");
        fs::write(
            completed.join("result.json"),
            r#"{"run_id":"run-2026-01-01T00-00-00Z","status":"completed","summary":"done"}"#,
        )
        .expect("completed record");
        fs::write(
            running.join("result.json"),
            r#"{"run_id":"run-2026-01-01T00-00-01Z","status":"running","summary":"active"}"#,
        )
        .expect("running record");
        let selected = dashboard(
            &json!({"mode":"single_run","run_id":"run-2026-01-01T00-00-00Z"})
                .as_object()
                .unwrap(),
            &root,
        )
        .expect("single dashboard");
        assert_eq!(selected["mode"], "single_run");
        assert_eq!(selected["counts"]["total"], 1);
        assert_eq!(selected["runs"][0]["run_id"], "run-2026-01-01T00-00-00Z");
        let active = dashboard(
            &json!({"mode":"all_active","include_terminal":false})
                .as_object()
                .unwrap(),
            &root,
        )
        .expect("active dashboard");
        assert_eq!(active["mode"], "all_active");
        assert_eq!(active["counts"]["active"], 1);
        assert_eq!(active["runs"][0]["run_id"], "run-2026-01-01T00-00-01Z");
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn native_worker_updates_cognition_defaults_atomically() {
        let root =
            std::env::temp_dir().join(format!("narada-worker-defaults-{}", uuid::Uuid::new_v4()));
        let updated = cognition_defaults_update(json!({"provider":"fixture","cognition":"high","model":"fixture-model","reasoning_effort":"max","actor":"test"}).as_object().unwrap(), &root).expect("update");
        assert_eq!(updated["status"], "updated");
        assert_eq!(
            cognition_defaults(&root)["defaults"]["high"]["model"],
            "fixture-model"
        );
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn native_worker_reaps_nonterminal_record_with_explicit_force() {
        let root =
            std::env::temp_dir().join(format!("narada-worker-reap-{}", uuid::Uuid::new_v4()));
        let id = "run-fixture";
        let dir = run_root(&root).join(id);
        fs::create_dir_all(&dir).expect("dir");
        fs::write(dir.join("result.json"), format!(r#"{{"run_id":"{id}","status":"running","timing":{{"started_at":"2026-01-01T00:00:00Z"}}}}"#)).expect("record");
        let result = worker_run_reap(
            json!({"run_id":id,"reason":"fixture cleanup","force":true})
                .as_object()
                .unwrap(),
            &root,
        )
        .expect("reap");
        assert_eq!(result["status"], "reaped");
        assert_eq!(read_run(&root, id).expect("read")["status"], "cancelled");
        fs::remove_dir_all(root).expect("cleanup");
    }
}
