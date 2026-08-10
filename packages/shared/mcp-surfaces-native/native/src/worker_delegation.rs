use serde_json::{json, Map, Value};
use std::fs;
use std::path::{Path, PathBuf};

const SERVER_NAME: &str = "worker-delegation-mcp";
const MAX_RUNS: usize = 200;
const MAX_FILE_BYTES: usize = 256_000;
const READ_TOOLS: &[(&str, &str)] = &[
    ("worker_output_show", "Read a bounded materialized worker artifact."),
    ("worker_operator_affordances", "Return UI-neutral worker inspection affordances."),
    ("worker_policy_inspect", "Inspect worker delegation policy without launching a worker."),
    ("worker_cognition_defaults_inspect", "Inspect local cognition defaults without changing them."),
    ("worker_config_resolve", "Resolve worker inputs and binding checks without launching a worker."),
    ("worker_run_status", "Inspect one durable worker run without waiting for completion."),
    ("worker_runs_list", "List recent durable worker runs with bounded compact records."),
    ("worker_run_wait", "Read one worker run's current state; native mode does not launch or poll a child."),
    ("worker_run_wait_batch", "Read bounded current states for several worker runs."),
    ("worker_runs_synthesize", "Summarize bounded worker run states."),
    ("worker_dashboard_describe", "Describe a bounded local worker dashboard projection."),
];
const MUTATING_TOOLS: &[&str] = &[
    "worker_cognition_defaults_update", "worker_run", "worker_edit", "worker_resume",
    "worker_run_reap", "worker_run_batch",
];

pub fn list_tools() -> Vec<Value> {
    let mut tools = vec![guidance_tool()];
    for (name, description) in READ_TOOLS {
        tools.push(tool(name, description, input_schema(name), true));
    }
    for name in MUTATING_TOOLS {
        tools.push(tool(name, "Worker execution or mutation remains owned by the worker authority.", json!({"type":"object","additionalProperties":true}), false));
    }
    tools
}

pub fn auxiliary(method: &str, params: &Map<String, Value>) -> Result<Value, Value> {
    match method {
        "prompts/list" => Ok(json!({"prompts":[{"name":"worker_delegation_task","title":"Worker Delegation Task","description":"Inspect worker policy and durable run state before delegating execution.","arguments":[]}]})),
        "prompts/get" => {
            if params.get("name").and_then(Value::as_str) != Some("worker_delegation_task") {
                return Err(error("unknown_prompt", "unknown_prompt"));
            }
            Ok(json!({"description":"Inspect worker policy and durable run state before delegating execution.","messages":[{"role":"user","content":{"type":"text","text":"Use worker_policy_inspect and worker_config_resolve before execution; use worker_run_status, worker_runs_list, worker_run_wait, and worker_output_show for bounded readback. Keep worker launch and mutation with the owning authority."}}]}))
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
        "worker_guidance" => Ok(guidance(args)),
        "worker_policy_inspect" => Ok(policy(root)),
        "worker_cognition_defaults_inspect" => Ok(cognition_defaults()),
        "worker_config_resolve" => config_resolve(args, root),
        "worker_run_status" => run_status(args, root),
        "worker_runs_list" => runs_list(args, root),
        "worker_run_wait" => run_wait(args, root),
        "worker_run_wait_batch" => run_wait_batch(args, root),
        "worker_runs_synthesize" => runs_synthesize(args, root),
        "worker_dashboard_describe" => dashboard(args, root),
        "worker_output_show" => output_show(args, root),
        "worker_operator_affordances" => Ok(affordances()),
        name if MUTATING_TOOLS.contains(&name) => Err(authority_boundary(name)),
        _ => Err(error("unknown_tool", &format!("unknown_tool:{name}"))),
    }
}

fn guidance_tool() -> Value { tool("worker_guidance", "Show model-facing operating guidance for worker-delegation MCP workflows.", json!({"type":"object","properties":{"workflow":{"type":"string"},"tool":{"type":"string"}},"additionalProperties":false}), true) }
fn guidance(args: &Map<String, Value>) -> Value { json!({"schema":"narada.worker.guidance.v1","status":"ok","server_name":SERVER_NAME,"requested":{"workflow":args.get("workflow").cloned().unwrap_or(Value::Null),"tool":args.get("tool").cloned().unwrap_or(Value::Null)},"first_use":["Inspect worker_policy_inspect.","Resolve worker inputs without launching with worker_config_resolve.","Read durable runs with worker_run_status or worker_runs_list.","Use worker_output_show for bounded artifact readback."],"boundaries":["Native mode never spawns, resumes, cancels, or reaps a worker.","Credentials and provider secrets never cross this surface.","Run records are read from the bounded site worker-delegation root."]}) }

fn run_root(root: &Path) -> PathBuf {
    if root.file_name().and_then(|v| v.to_str()).map(|v| v.eq_ignore_ascii_case(".narada")).unwrap_or(false) { root.join("runtime/worker-delegation") } else { root.join(".narada/runtime/worker-delegation") }
}
fn is_within(path: &Path, root: &Path) -> bool { let p=path.canonicalize().unwrap_or_else(|_|path.to_path_buf()); let r=root.canonicalize().unwrap_or_else(|_|root.to_path_buf()); p==r || p.starts_with(&r) }
fn safe_run_id(value: &str) -> Result<&str, Value> { if value.len() < 5 || value.len() > 160 || !value.starts_with("run-") || !value[4..].chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') { Err(error("worker_run_id_invalid", "worker_run_id_invalid")) } else { Ok(value) } }
fn run_id(args: &Map<String, Value>) -> Result<String, Value> { let id = args.get("run_id").and_then(Value::as_str).filter(|v| !v.trim().is_empty()).ok_or_else(|| error("worker_run_id_required", "worker_run_id_required"))?; safe_run_id(id.trim())?; Ok(id.trim().to_string()) }
fn read_json(path: &Path) -> Result<Value, Value> { let meta = fs::metadata(path).map_err(|_| error("worker_run_not_found", "worker_run_not_found"))?; if meta.len() > MAX_FILE_BYTES as u64 { return Err(error("worker_record_too_large", "worker_record_too_large")); } let text = fs::read_to_string(path).map_err(|_| error("worker_record_read_failed", "worker_record_read_failed"))?; serde_json::from_str(&text).map_err(|_| error("worker_record_invalid_json", "worker_record_invalid_json")) }
fn run_path(root: &Path, id: &str) -> Result<PathBuf, Value> { safe_run_id(id)?; Ok(run_root(root).join(id).join("result.json")) }
fn read_run(root: &Path, id: &str) -> Result<Value, Value> { read_json(&run_path(root, id)?) }

fn policy(root: &Path) -> Value { json!({"schema":"narada.worker.policy.v1","status":"ok","server_name":SERVER_NAME,"run_root":run_root(root).to_string_lossy(),"site_root":root.to_string_lossy(),"allowed_roots":[root.to_string_lossy()],"allowed_runtimes":["codex","narada-agent-runtime-server"],"allowed_authorities":["read","write","command"],"native_execution":"authority_boundary","secret_projection":"not_available"}) }
fn cognition_defaults() -> Value { json!({"schema":"narada.worker.cognition_defaults.v1","status":"ok","defaults":{"low":{"provider":null,"model":null,"reasoning_effort":null},"medium":{"provider":null,"model":null,"reasoning_effort":null},"high":{"provider":null,"model":null,"reasoning_effort":null}},"source":"native_contract","canonical_runtime":"narada-agent-runtime-server uses an immutable invocation plan","native_read_only":true}) }
fn config_resolve(args: &Map<String, Value>, root: &Path) -> Result<Value, Value> { let cwd = args.get("constraints").and_then(Value::as_object).and_then(|v| v.get("cwd")).and_then(Value::as_str).or_else(|| args.get("cwd").and_then(Value::as_str)); let cwd = cwd.map(PathBuf::from).unwrap_or_else(|| root.to_path_buf()); if !is_within(&cwd, root) { return Err(error("worker_cwd_outside_allowed_roots", "worker_cwd_outside_allowed_roots")); } Ok(json!({"schema":"narada.worker.config_resolve.v1","status":"ok","resolved":{"cwd":cwd.to_string_lossy(),"site_root":root.to_string_lossy(),"runtime":"narada-agent-runtime-server","authority":"read","launch":false},"diagnostics":[{"name":"native_execution","status":"boundary","message":"worker launch is delegated to the owning worker authority"}],"native_read_only":true})) }
fn run_status(args: &Map<String, Value>, root: &Path) -> Result<Value, Value> { let id=run_id(args)?; let run=read_run(root,&id)?; Ok(json!({"schema":"narada.worker.run_status.v1","status":"ok","run_id":id,"run":compact_run(&run),"native_read_only":true})) }
fn runs_list(args: &Map<String, Value>, root: &Path) -> Result<Value, Value> { let limit=args.get("limit").and_then(Value::as_u64).unwrap_or(20).clamp(1,200) as usize; let include_running=args.get("include_running").and_then(Value::as_bool).unwrap_or(true); let include_completed=args.get("include_completed").and_then(Value::as_bool).unwrap_or(true); let mut items=Vec::new(); if let Ok(entries)=fs::read_dir(run_root(root)) { for entry in entries.filter_map(Result::ok).take(MAX_RUNS) { if !entry.path().is_dir(){continue;} let Some(id)=entry.file_name().to_str().map(str::to_string) else {continue;}; if !id.starts_with("run-"){continue;} if let Ok(run)=read_run(root,&id) { let terminal=!matches!(run.get("status").and_then(Value::as_str),Some("running")); if (terminal&&include_completed)||(!terminal&&include_running) { items.push(compact_run(&run)); } } } } items.sort_by(|a,b| b.get("updated_at").and_then(Value::as_str).cmp(&a.get("updated_at").and_then(Value::as_str))); items.truncate(limit); Ok(json!({"schema":"narada.worker.runs_list.v1","status":"ok","count":items.len(),"limit":limit,"scanned":items.len(),"scan_limit":MAX_RUNS,"scan_truncated":false,"include_running":include_running,"include_completed":include_completed,"runs":items,"native_read_only":true})) }
fn run_wait(args: &Map<String, Value>, root: &Path) -> Result<Value, Value> { let id=run_id(args)?; let run=read_run(root,&id)?; let running=run.get("status").and_then(Value::as_str)==Some("running"); Ok(json!({"schema":"narada.worker.run_wait.v1","status":"ok","wait":{"status":if running{"timed_out"}else{"finished"},"waited":false,"timeout_ms":args.get("timeout_ms").cloned().unwrap_or(json!(0)),"native_execution":"not_polled"},"run":compact_run(&run),"native_read_only":true})) }
fn run_wait_batch(args: &Map<String, Value>, root: &Path) -> Result<Value, Value> { let ids=args.get("run_ids").and_then(Value::as_array).ok_or_else(||error("worker_run_ids_required","worker_run_ids_required"))?; let mut runs=Vec::new(); for id in ids.iter().take(50).filter_map(Value::as_str) { let mut item=json!({"run_id":id,"status":"error"}); if let Ok(run)=read_run(root,id) { item=json!({"run_id":id,"status":"ok","run":compact_run(&run)}); } runs.push(item); } Ok(json!({"schema":"narada.worker.run_wait_batch.v1","status":"ok","requested_count":ids.len().min(50),"runs":runs,"native_read_only":true})) }
fn runs_synthesize(args: &Map<String, Value>, root: &Path) -> Result<Value, Value> { let ids=args.get("run_ids").and_then(Value::as_array).ok_or_else(||error("worker_run_ids_required","worker_run_ids_required"))?; let mut counts=Map::new(); let mut found=Vec::new(); for id in ids.iter().take(50).filter_map(Value::as_str) { if let Ok(run)=read_run(root,id) { let status=run.get("status").and_then(Value::as_str).unwrap_or("unknown"); *counts.entry(status.to_string()).or_insert(Value::from(0)) = Value::from(counts.get(status).and_then(Value::as_u64).unwrap_or(0)+1); found.push(id); } } Ok(json!({"schema":"narada.worker.runs_synthesis.v1","status":"ok","requested_count":ids.len().min(50),"run_ids":found,"synthesis":{"counts":counts,"native_read_only":true}})) }
fn dashboard(args: &Map<String, Value>, root: &Path) -> Result<Value, Value> { let list=runs_list(&json!({"limit":args.get("limit").cloned().unwrap_or(json!(25))}).as_object().unwrap(),root)?; let runs=list.get("runs").cloned().unwrap_or_else(||json!([])); Ok(json!({"schema":"narada.worker.dashboard.v1","status":"ok","mode":"all_active","include_terminal":true,"dashboard":{"server_name":SERVER_NAME,"run_root":run_root(root).to_string_lossy()},"counts":{"runs":runs.as_array().map(Vec::len).unwrap_or(0)},"runs":runs,"topology":[],"steps":[],"pending_join_gates":[],"event_stream":[],"native_read_only":true})) }
fn output_show(args: &Map<String, Value>, root: &Path) -> Result<Value, Value> { let reference=args.get("ref").or_else(||args.get("output_ref")).and_then(Value::as_str).ok_or_else(||error("worker_output_ref_required","worker_output_ref_required"))?; let raw=reference.strip_prefix("worker-artifact:").ok_or_else(||error("worker_output_ref_invalid","worker_output_ref_invalid"))?; let (id,name)=raw.split_once('/').ok_or_else(||error("worker_output_ref_invalid","worker_output_ref_invalid"))?; safe_run_id(id)?; if name.is_empty()||name.len()>100||name.contains('/')||name.contains('\\')||name.contains("..") { return Err(error("worker_output_ref_invalid","worker_output_ref_invalid")); } let path=run_root(root).join(id).join(name); let bytes=fs::read(&path).map_err(|_|error("worker_output_not_found","worker_output_not_found"))?; let truncated=bytes.len()>MAX_FILE_BYTES; let text=String::from_utf8_lossy(&bytes[..bytes.len().min(MAX_FILE_BYTES)]).to_string(); Ok(json!({"schema":"narada.worker.output_page.v1","status":"ok","ref":reference,"path":path.to_string_lossy(),"byte_size":bytes.len(),"output_text":text,"output_truncated":truncated,"native_read_only":true})) }
fn affordances() -> Value { json!({"schema":"narada.worker.operator_affordances.v1","status":"ok","read_tools":READ_TOOLS.iter().map(|(n,_)|*n).collect::<Vec<_>>(),"mutation_tools":MUTATING_TOOLS,"native_read_only":true}) }
fn compact_run(run: &Value) -> Value { let o=run.as_object().cloned().unwrap_or_default(); json!({"run_id":o.get("run_id"),"status":o.get("status"),"completion_state":o.get("completion_state"),"authority":o.get("authority"),"worker_session_id":o.get("worker_session_id"),"started_at":o.get("timing").and_then(|v|v.get("started_at")),"finished_at":o.get("timing").and_then(|v|v.get("finished_at")),"summary_preview":o.get("summary").or_else(||o.get("last_message")),"error_preview":o.get("error"),"updated_at":o.get("updated_at").or_else(||o.get("timing").and_then(|v|v.get("finished_at")))}) }
fn authority_boundary(name: &str) -> Value { json!({"schema":"narada.worker.authority_boundary.v1","status":"unavailable","tool_name":name,"reason":"worker_execution_authority_not_enabled_in_native_read_slice","remediation":"Use the configured worker-delegation authority for launch, resume, reaping, cancellation, and cognition-default mutation."}) }
fn error(code: &str, message: &str) -> Value { json!({"schema":"narada.worker.error.v1","code":code,"message":message}) }
fn input_schema(name: &str) -> Value { match name { "worker_run_status"|"worker_run_wait" => json!({"type":"object","properties":{"run_id":{"type":"string"},"timeout_ms":{"type":"integer","minimum":0,"maximum":300000}},"required":["run_id"],"additionalProperties":false}), "worker_run_wait_batch"|"worker_runs_synthesize" => json!({"type":"object","properties":{"run_ids":{"type":"array","items":{"type":"string"}}},"required":["run_ids"],"additionalProperties":false}), "worker_output_show" => json!({"type":"object","properties":{"ref":{"type":"string"},"output_ref":{"type":"string"},"offset":{"type":"integer","minimum":0},"limit":{"type":"integer","minimum":0}},"additionalProperties":false}), _ => json!({"type":"object","additionalProperties":true}) } }
fn tool(name: &str, description: &str, schema: Value, read_only: bool) -> Value { json!({"name":name,"description":description,"annotations":{"title":name,"readOnlyHint":read_only,"destructiveHint":!read_only,"idempotentHint":read_only,"openWorldHint":false},"inputSchema":schema,"outputSchema":{"type":"object","additionalProperties":true}}) }

#[cfg(test)]
mod tests { use super::*; #[test] fn native_worker_reads_bounded_run_records() { let root=std::env::temp_dir().join(format!("narada-worker-{}",uuid::Uuid::new_v4())); let dir=run_root(&root).join("run-2026-01-01T00-00-00Z"); fs::create_dir_all(&dir).expect("dir"); fs::write(dir.join("result.json"),r#"{"run_id":"run-2026-01-01T00-00-00Z","status":"completed","summary":"done"}"#).expect("record"); let listed=runs_list(&json!({"limit":1}).as_object().unwrap(),&root).expect("list"); assert_eq!(listed["count"],1); assert_eq!(run_status(&json!({"run_id":"run-2026-01-01T00-00-00Z"}).as_object().unwrap(),&root).expect("status")["run"]["status"],"completed"); fs::remove_dir_all(root).expect("cleanup"); } }
