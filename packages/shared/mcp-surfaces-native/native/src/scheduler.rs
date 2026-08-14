use crate::scheduler_activation;
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use std::env;
use std::fs::{self, File};
use std::io::Read;
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::OnceLock;
use std::thread;
use std::time::{Duration, Instant};
use uuid::Uuid;

const SERVER_VERSION: &str = "0.1.0";
const COMMAND_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_COMMAND_OUTPUT: u64 = 1_048_576;
const TASK_TOOLS: &[(&str, bool)] = &[
    ("scheduler_runtime_status", true),
    ("scheduler_task_list", true),
    ("scheduler_task_show", true),
    ("scheduler_task_create", false),
    ("scheduler_task_delete", false),
    ("scheduler_task_update_action", false),
    ("scheduler_task_enable", false),
    ("scheduler_task_disable", false),
    ("scheduler_task_stop", false),
    ("scheduler_task_run", false),
    ("scheduler_task_history", true),
];

static STARTUP_IMPLEMENTATION_ID: OnceLock<String> = OnceLock::new();

pub fn list_tools() -> Vec<Value> {
    let mut tools = vec![tool(
        "scheduler_guidance",
        "Show model-facing operating guidance for Scheduler workflows.",
        json!({"type":"object","properties":{"workflow":{"type":"string"},"tool":{"type":"string"}},"additionalProperties":false}),
        true,
        false,
        true,
    )];
    for (name, read_only) in TASK_TOOLS {
        tools.push(task_tool(name, *read_only));
    }
    for (name, read_only) in scheduler_activation::TOOLS {
        tools.push(activation_tool(name, *read_only));
    }
    tools
}

pub fn auxiliary(method: &str, params: &Map<String, Value>) -> Result<Value, Value> {
    match method {
        "prompts/list" => Ok(
            json!({"prompts":[{"name":"scheduler_workflow","title":"Scheduler Workflow","description":"Inspect runtime and durable scheduling posture before changing tasks or activations.","arguments":[]}]}),
        ),
        "prompts/get" => {
            if params.get("name").and_then(Value::as_str) != Some("scheduler_workflow") {
                return Err(error("unknown_prompt", "unknown_prompt", Value::Null));
            }
            Ok(
                json!({"description":"Inspect runtime and durable scheduling posture before changing tasks or activations.","messages":[{"role":"user","content":{"type":"text","text":"Call scheduler_runtime_status before task or activation mutations and pass its implementation_id unchanged. Use scheduler_activation_doctor before preparing the durable activation store."}}]}),
            )
        }
        "completion/complete" => {
            let values = if params
                .get("argument")
                .and_then(Value::as_object)
                .and_then(|value| value.get("name"))
                .and_then(Value::as_str)
                == Some("name")
            {
                list_tools()
                    .into_iter()
                    .filter_map(|value| value.get("name").cloned())
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
            json!({"method":method}),
        )),
    }
}

pub fn call_tool(name: &str, args: &Map<String, Value>, root: &Path) -> Result<Value, Value> {
    if scheduler_activation::supports(name) {
        if scheduler_activation::is_mutation(name) {
            assert_mutation_ready(args)?;
        }
        return scheduler_activation::call_tool(name, args, root);
    }
    match name {
        "scheduler_guidance" => Ok(guidance(args)),
        "scheduler_runtime_status" => Ok(runtime_status()),
        "scheduler_task_list" => task_list(args),
        "scheduler_task_show" => task_show(args),
        "scheduler_task_create" => {
            assert_mutation_ready(args)?;
            task_create(args, root)
        }
        "scheduler_task_delete" => {
            assert_mutation_ready(args)?;
            task_simple_mutation("delete", args, "/delete", "deleted")
        }
        "scheduler_task_update_action" => {
            assert_mutation_ready(args)?;
            task_update_action(args, root)
        }
        "scheduler_task_enable" => {
            assert_mutation_ready(args)?;
            task_simple_mutation("enable", args, "/change", "enabled")
        }
        "scheduler_task_disable" => {
            assert_mutation_ready(args)?;
            task_simple_mutation("disable", args, "/change", "disabled")
        }
        "scheduler_task_stop" => {
            assert_mutation_ready(args)?;
            task_simple_mutation("stop", args, "/end", "stopped")
        }
        "scheduler_task_run" => {
            assert_mutation_ready(args)?;
            task_simple_mutation("run", args, "/run", "started")
        }
        "scheduler_task_history" => task_history(args),
        _ => Err(error(
            "unknown_tool",
            &format!("unknown_tool:{name}"),
            json!({"tool_name":name}),
        )),
    }
}

fn guidance(args: &Map<String, Value>) -> Value {
    json!({
        "schema":"narada.mcp_surface.guidance.v0","status":"ok","surface_id":"scheduler","guidance_tool":"scheduler_guidance",
        "purpose":"Operate Windows scheduled tasks and the Site-local durable activation ledger from one native Rust authority.",
        "requested":{"workflow":args.get("workflow").cloned().unwrap_or(Value::Null),"tool":args.get("tool").cloned().unwrap_or(Value::Null)},
        "first_use":["Call scheduler_runtime_status and retain its implementation_id.","Use bounded task list/show reads before mutation.","Call scheduler_task_create with dry_run true to validate the complete launch and trigger plan before creating a Windows task.","Call scheduler_activation_doctor before explicitly preparing the activation database."],
        "boundaries":["Task mutations require the exact current native implementation_id.","Scheduled commands run through the native CREATE_NO_WINDOW supervisor.","The activation database is never created or migrated implicitly."]
    })
}

fn runtime_status() -> Value {
    let executable = env::current_exe().ok();
    let current_id = implementation_id();
    let startup_id = STARTUP_IMPLEMENTATION_ID.get_or_init(|| current_id.clone());
    let supervisor = supervisor_path();
    let supervisor_available = supervisor.is_file();
    let unchanged = startup_id == &current_id;
    let status = if executable.is_none() || !supervisor_available {
        "unavailable"
    } else if !unchanged {
        "stale"
    } else {
        "fresh"
    };
    json!({
        "schema":"narada.scheduler_runtime_status.v1","status":status,"implementation_id":current_id,
        "implementation":"rust-native","runtime_entrypoint":executable.as_ref().map(|path|path.to_string_lossy().to_string()),"source_entrypoint":Value::Null,"source_mtime":Value::Null,
        "runtime_mtime":executable.as_ref().and_then(file_modified),
        "components":[
            {"name":"main","runtime_path":executable.as_ref().map(|path|path.to_string_lossy().to_string()),"source_path":Value::Null,"runtime_mtime":executable.as_ref().and_then(file_modified),"source_mtime":Value::Null,"status":if executable.is_some()&&unchanged{"fresh"}else{"unavailable"}},
            {"name":"scheduled_command_launcher","runtime_path":supervisor.to_string_lossy(),"source_path":Value::Null,"runtime_mtime":file_modified(&supervisor),"source_mtime":Value::Null,"status":if supervisor_available{"fresh"}else{"unavailable"}}
        ],
        "remediation":if status=="fresh"{Value::Null}else{json!("Build the native process supervisor and restart the Scheduler MCP before mutating scheduled tasks.")},
        "native_task_scheduler":true,"native_activation_store":true
    })
}

fn assert_mutation_ready(args: &Map<String, Value>) -> Result<(), Value> {
    let status = runtime_status();
    if status.get("status").and_then(Value::as_str) != Some("fresh") {
        return Err(error(
            "scheduler_runtime_stale",
            "scheduler_runtime_stale",
            status,
        ));
    }
    let supplied = required(
        args,
        "implementation_id",
        "scheduler_implementation_id_required",
    )?;
    let expected = status
        .get("implementation_id")
        .and_then(Value::as_str)
        .unwrap_or("");
    if supplied != expected {
        return Err(error(
            "scheduler_implementation_id_mismatch",
            "scheduler_implementation_id_mismatch",
            json!({"supplied_implementation_id":supplied,"expected_implementation_id":expected,"remediation":"Call scheduler_runtime_status and pass its implementation_id unchanged to the mutation."}),
        ));
    }
    Ok(())
}

fn implementation_id() -> String {
    let mut hasher = Sha256::new();
    hasher.update(SERVER_VERSION.as_bytes());
    hasher.update([0]);
    if let Ok(path) = env::current_exe() {
        hasher.update(path.to_string_lossy().as_bytes());
        if let Ok(mut file) = File::open(path) {
            let mut buffer = [0_u8; 64 * 1024];
            loop {
                match file.read(&mut buffer) {
                    Ok(0) | Err(_) => break,
                    Ok(count) => hasher.update(&buffer[..count]),
                }
            }
        }
    }
    hex(&hasher.finalize())
}

fn file_modified(path: &PathBuf) -> Option<String> {
    fs::metadata(path)
        .ok()?
        .modified()
        .ok()?
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .map(|value| format!("epoch-ms:{}", value.as_millis()))
}

fn task_tool(name: &str, read_only: bool) -> Value {
    let schema = match name {
        "scheduler_runtime_status" => {
            json!({"type":"object","properties":{},"additionalProperties":false})
        }
        "scheduler_task_list" => {
            json!({"type":"object","properties":{"folder":{"type":"string"},"limit":{"type":"integer","minimum":1,"maximum":500,"default":50},"offset":{"type":"integer","minimum":0,"maximum":10000,"default":0}},"additionalProperties":false})
        }
        "scheduler_task_show" => {
            json!({"type":"object","properties":{"task_name":{"type":"string"}},"required":["task_name"],"additionalProperties":false})
        }
        "scheduler_task_history" => {
            json!({"type":"object","properties":{"task_name":{"type":"string"},"limit":{"type":"integer","minimum":1,"maximum":200,"default":20}},"required":["task_name"],"additionalProperties":false})
        }
        "scheduler_task_create" => {
            json!({"type":"object","properties":{"task_name":{"type":"string"},"command":{"type":"string"},"arguments":{"type":"string"},"working_dir":{"type":"string"},"schedule":{"type":"string","enum":["daily","hourly","at_startup","at_logon","once"]},"start_time":{"type":"string"},"interval_minutes":{"type":"integer","minimum":1,"maximum":1440},"execution_time_limit_seconds":{"type":"integer","minimum":1,"maximum":86400},"multiple_instances":{"type":"string","enum":["ignore_new","parallel","queue","stop_existing"]},"dry_run":{"type":"boolean","default":false,"description":"Validate and return the complete native launch plan without creating a Windows scheduled task."},"implementation_id":{"type":"string"}},"required":["task_name","command","schedule","implementation_id"],"additionalProperties":false})
        }
        "scheduler_task_update_action" => {
            json!({"type":"object","properties":{"task_name":{"type":"string"},"command":{"type":"string"},"arguments":{"type":"string"},"working_dir":{"type":"string"},"execution_time_limit_seconds":{"type":"integer","minimum":1,"maximum":86400},"multiple_instances":{"type":"string","enum":["ignore_new","parallel","queue","stop_existing"]},"dry_run":{"type":"boolean"},"implementation_id":{"type":"string"}},"required":["task_name","command","implementation_id"],"additionalProperties":false})
        }
        _ => {
            json!({"type":"object","properties":{"task_name":{"type":"string"},"implementation_id":{"type":"string"}},"required":["task_name","implementation_id"],"additionalProperties":false})
        }
    };
    tool(
        name,
        task_description(name),
        schema,
        read_only,
        name == "scheduler_task_delete" || name == "scheduler_task_stop",
        !matches!(
            name,
            "scheduler_task_create" | "scheduler_task_delete" | "scheduler_task_run"
        ),
    )
}

fn activation_tool(name: &str, read_only: bool) -> Value {
    let implementation = json!({"type":"string","description":"Current implementation_id returned by scheduler_runtime_status."});
    let string = json!({"type":"string"});
    let record = json!({"type":"object","additionalProperties":true});
    let schema = match name {
        "scheduler_activation_doctor" => {
            json!({"type":"object","properties":{},"additionalProperties":false})
        }
        "scheduler_activation_prepare" => {
            json!({"type":"object","properties":{"implementation_id":implementation},"required":["implementation_id"],"additionalProperties":false})
        }
        "scheduler_binding_list" => {
            json!({"type":"object","properties":{"status":{"type":"string","enum":["active","paused","retired"]},"limit":{"type":"integer","minimum":1,"maximum":500,"default":100},"offset":{"type":"integer","minimum":0,"maximum":10000,"default":0}},"additionalProperties":false})
        }
        "scheduler_binding_show" => {
            json!({"type":"object","properties":{"binding_id":string},"required":["binding_id"],"additionalProperties":false})
        }
        "scheduler_binding_upsert" => {
            json!({"type":"object","properties":{"binding_id":string,"trigger_kind":{"type":"string","enum":["bootstrap","completion","domain_event"]},"source_topic":string,"source_sop_id":string,"terminal_outcomes":{"type":"array","items":{"type":"string"}},"target_sop_id":string,"target_template_version":string,"concurrency":{"type":"string","enum":["singleton","partitioned"]},"delay_by_outcome_ms":{"type":"object","additionalProperties":{"type":"integer","minimum":0}},"default_delay_ms":{"type":"integer","minimum":0},"retry_base_ms":{"type":"integer","minimum":0},"retry_max_ms":{"type":"integer","minimum":0},"max_attempts":{"type":"integer","minimum":1},"blocked_policy":{"type":"string","enum":["manual_unblock"]},"expected_revision":{"type":"integer","minimum":1},"implementation_id":implementation},"required":["binding_id","trigger_kind","source_topic","target_sop_id","target_template_version","concurrency","implementation_id"],"additionalProperties":false})
        }
        "scheduler_binding_pause" | "scheduler_binding_resume" | "scheduler_binding_retire" => {
            json!({"type":"object","properties":{"binding_id":string,"expected_revision":{"type":"integer","minimum":1},"implementation_id":implementation},"required":["binding_id","expected_revision","implementation_id"],"additionalProperties":false})
        }
        "scheduler_event_show" => {
            json!({"type":"object","properties":{"event_id":string},"required":["event_id"],"additionalProperties":false})
        }
        "scheduler_event_admit" => {
            json!({"type":"object","properties":{"event_id":string,"topic":string,"partition_key":string,"aggregate_id":string,"aggregate_revision":{"type":"integer","minimum":0},"schema_version":{"type":"integer","minimum":1},"causation_id":string,"idempotency_key":string,"payload":record,"occurred_at":string,"implementation_id":implementation},"required":["event_id","topic","partition_key","aggregate_id","aggregate_revision","schema_version","causation_id","idempotency_key","payload","occurred_at","implementation_id"],"additionalProperties":false})
        }
        "scheduler_activation_list" => {
            json!({"type":"object","properties":{"status":{"type":"string","enum":["pending","leased","admitted","terminal","blocked"]},"binding_id":string,"source_event_id":string,"sop_run_id":string,"limit":{"type":"integer","minimum":1,"maximum":500,"default":100},"offset":{"type":"integer","minimum":0,"maximum":10000,"default":0}},"additionalProperties":false})
        }
        "scheduler_activation_claim" => {
            json!({"type":"object","properties":{"consumer_id":string,"lease_ms":{"type":"integer","minimum":1000,"maximum":300000},"implementation_id":implementation},"required":["consumer_id","implementation_id"],"additionalProperties":false})
        }
        "scheduler_activation_admit_sop" => {
            json!({"type":"object","properties":{"activation_id":string,"consumer_id":string,"lease_token":string,"sop_run_id":string,"receipt_id":string,"receipt":record,"implementation_id":implementation},"required":["activation_id","consumer_id","lease_token","sop_run_id","receipt_id","receipt","implementation_id"],"additionalProperties":false})
        }
        "scheduler_activation_fail" => {
            json!({"type":"object","properties":{"activation_id":string,"consumer_id":string,"lease_token":string,"retryable":{"type":"boolean"},"error":string,"implementation_id":implementation},"required":["activation_id","consumer_id","lease_token","retryable","error","implementation_id"],"additionalProperties":false})
        }
        "scheduler_activation_resolve" => {
            json!({"type":"object","properties":{"activation_id":string,"sop_run_id":string,"outcome":string,"receipt_id":string,"receipt":record,"implementation_id":implementation},"required":["outcome","receipt_id","receipt","implementation_id"],"additionalProperties":false})
        }
        "scheduler_activation_unblock" => {
            json!({"type":"object","properties":{"activation_id":string,"due_at":string,"implementation_id":implementation},"required":["activation_id","implementation_id"],"additionalProperties":false})
        }
        _ => json!({"type":"object","properties":{},"additionalProperties":false}),
    };
    tool(
        name,
        "Operate the durable Site-scoped Scheduler activation ledger.",
        schema,
        read_only,
        false,
        true,
    )
}

fn tool(
    name: &str,
    description: &str,
    input_schema: Value,
    read_only: bool,
    destructive: bool,
    idempotent: bool,
) -> Value {
    json!({"name":name,"description":description,"inputSchema":input_schema,"annotations":{"title":name,"readOnlyHint":read_only,"destructiveHint":destructive,"idempotentHint":idempotent,"openWorldHint":false},"outputSchema":{"type":"object","additionalProperties":true}})
}

fn task_description(name: &str) -> &'static str {
    match name {
        "scheduler_runtime_status" => {
            "Report the native Scheduler implementation identity required for safe mutations."
        }
        "scheduler_task_list" => "List scheduled tasks, optionally filtered by folder path.",
        "scheduler_task_show" => {
            "Show full details and the native action definition for one scheduled task."
        }
        "scheduler_task_create" => {
            "Create a scheduled task whose target runs through the native no-console actuator."
        }
        "scheduler_task_delete" => "Delete a scheduled task.",
        "scheduler_task_update_action" => {
            "Update only a task action while preserving triggers and enabled state."
        }
        "scheduler_task_enable" => "Enable a scheduled task.",
        "scheduler_task_disable" => "Disable a scheduled task.",
        "scheduler_task_stop" => "Stop a running task without changing registration.",
        "scheduler_task_run" => "Run a scheduled task immediately.",
        "scheduler_task_history" => "Show bounded run summary history for a scheduled task.",
        _ => "Operate Windows Task Scheduler.",
    }
}

#[derive(Debug)]
struct CommandResult {
    stdout: String,
    stderr: String,
    exit_code: i32,
    timed_out: bool,
}

fn schtasks(arguments: &[String]) -> Result<CommandResult, Value> {
    let executable = env::var_os("NARADA_SCHEDULER_SCHTASKS_PATH")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("schtasks.exe"));
    run_command(&executable, arguments, &[], COMMAND_TIMEOUT)
}

fn powershell(script: &str, environment: &[(String, String)]) -> Result<CommandResult, Value> {
    let executable = env::var_os("NARADA_SCHEDULER_POWERSHELL_PATH")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("powershell.exe"));
    let utf16 = script
        .encode_utf16()
        .flat_map(u16::to_le_bytes)
        .collect::<Vec<_>>();
    let encoded = base64(&utf16, false);
    run_command(
        &executable,
        &[
            "-NoProfile".into(),
            "-NonInteractive".into(),
            "-ExecutionPolicy".into(),
            "Bypass".into(),
            "-EncodedCommand".into(),
            encoded,
        ],
        environment,
        COMMAND_TIMEOUT,
    )
}

fn run_command(
    executable: &Path,
    arguments: &[String],
    environment: &[(String, String)],
    timeout: Duration,
) -> Result<CommandResult, Value> {
    let marker = format!("narada-scheduler-{}", Uuid::new_v4());
    let temp = env::temp_dir();
    let stdout_path = temp.join(format!("{marker}.stdout"));
    let stderr_path = temp.join(format!("{marker}.stderr"));
    let stdout_file = File::create(&stdout_path).map_err(|cause| {
        error(
            "scheduler_command_capture_failed",
            &cause.to_string(),
            Value::Null,
        )
    })?;
    let stderr_file = File::create(&stderr_path).map_err(|cause| {
        error(
            "scheduler_command_capture_failed",
            &cause.to_string(),
            Value::Null,
        )
    })?;
    let mut command = Command::new(executable);
    command
        .args(arguments)
        .stdout(Stdio::from(stdout_file))
        .stderr(Stdio::from(stderr_file))
        .stdin(Stdio::null());
    for (key, value) in environment {
        command.env(key, value);
    }
    hide_window(&mut command);
    let mut child = command.spawn().map_err(|cause| {
        let _ = fs::remove_file(&stdout_path);
        let _ = fs::remove_file(&stderr_path);
        error(
            "scheduler_command_spawn_failed",
            &cause.to_string(),
            json!({"executable":executable.to_string_lossy()}),
        )
    })?;
    let started = Instant::now();
    let mut timed_out = false;
    let exit_code = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status.code().unwrap_or(1),
            Ok(None) if started.elapsed() < timeout => thread::sleep(Duration::from_millis(20)),
            Ok(None) => {
                timed_out = true;
                let _ = child.kill();
                break child
                    .wait()
                    .ok()
                    .and_then(|status| status.code())
                    .unwrap_or(-2);
            }
            Err(cause) => {
                let _ = child.kill();
                let _ = child.wait();
                let _ = fs::remove_file(&stdout_path);
                let _ = fs::remove_file(&stderr_path);
                return Err(error(
                    "scheduler_command_wait_failed",
                    &cause.to_string(),
                    Value::Null,
                ));
            }
        }
    };
    let stdout = read_bounded(&stdout_path);
    let mut stderr = read_bounded(&stderr_path);
    let _ = fs::remove_file(stdout_path);
    let _ = fs::remove_file(stderr_path);
    if timed_out {
        if !stderr.is_empty() {
            stderr.push('\n');
        }
        stderr.push_str(&format!(
            "scheduler command timed out after {}ms",
            timeout.as_millis()
        ));
    }
    Ok(CommandResult {
        stdout,
        stderr,
        exit_code,
        timed_out,
    })
}

#[cfg(windows)]
fn hide_window(command: &mut Command) {
    use std::os::windows::process::CommandExt;
    command.creation_flags(0x08000000);
}
#[cfg(not(windows))]
fn hide_window(_command: &mut Command) {}
fn read_bounded(path: &Path) -> String {
    let Ok(file) = File::open(path) else {
        return String::new();
    };
    let mut bytes = Vec::new();
    let _ = file.take(MAX_COMMAND_OUTPUT).read_to_end(&mut bytes);
    String::from_utf8_lossy(&bytes).to_string()
}

fn task_list(args: &Map<String, Value>) -> Result<Value, Value> {
    let folder = args
        .get("folder")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("\\");
    let limit = args
        .get("limit")
        .and_then(Value::as_u64)
        .unwrap_or(50)
        .clamp(1, 500) as usize;
    let offset = args
        .get("offset")
        .and_then(Value::as_u64)
        .unwrap_or(0)
        .min(10_000) as usize;
    let result = schtasks(&strings(&["/query", "/fo", "CSV", "/v", "/tn", folder]))?;
    if result.exit_code != 0 && result.exit_code != 1 {
        return Err(command_failure(
            "scheduler_query_failed",
            "list",
            &result,
            "",
            "",
        ));
    }
    let all_items = compact_rows(&parse_csv(&result.stdout));
    let observed_total = all_items.len();
    let count_exact = result.stdout.len() < MAX_COMMAND_OUTPUT as usize;
    let items = all_items
        .into_iter()
        .skip(offset)
        .take(limit)
        .collect::<Vec<_>>();
    let returned = items.len();
    let has_more = offset.saturating_add(returned) < observed_total || !count_exact;
    Ok(
        json!({"schema":"narada.scheduler.task_list.v1","status":"ok","items":items,"count":returned,"returned":returned,"total":if count_exact{json!(observed_total)}else{Value::Null},"observed_total":observed_total,"count_exact":count_exact,"folder":folder,"offset":offset,"limit":limit,"has_more":has_more,"next_offset":if has_more && returned>0{json!(offset + returned)}else{Value::Null},"bounded":true}),
    )
}

fn task_show(args: &Map<String, Value>) -> Result<Value, Value> {
    let task_name = required(args, "task_name", "scheduler_requires_task_name")?;
    let result = schtasks(&strings(&["/query", "/fo", "CSV", "/v", "/tn", &task_name]))?;
    if result.exit_code != 0 {
        return Err(command_failure(
            "scheduler_task_not_found",
            "show",
            &result,
            &task_name,
            "",
        ));
    }
    let rows = parse_csv(&result.stdout);
    if rows.is_empty() {
        return Err(error(
            "scheduler_task_not_found",
            &format!("scheduler_task_not_found:{task_name}"),
            Value::Null,
        ));
    }
    let definition = scheduled_task_definition(&task_name)?;
    Ok(
        json!({"task":rows[0],"task_compact":compact_rows(&rows).into_iter().next(),"task_definition":definition}),
    )
}

fn task_history(args: &Map<String, Value>) -> Result<Value, Value> {
    let task_name = required(args, "task_name", "scheduler_requires_task_name")?;
    let limit = args
        .get("limit")
        .and_then(Value::as_u64)
        .unwrap_or(20)
        .clamp(1, 200) as usize;
    let result = schtasks(&strings(&["/query", "/fo", "CSV", "/v", "/tn", &task_name]))?;
    if result.exit_code != 0 && result.exit_code != 1 {
        return Err(command_failure(
            "scheduler_query_failed",
            "history",
            &result,
            &task_name,
            "",
        ));
    }
    let rows = parse_csv(&result.stdout);
    if rows.is_empty() {
        return Err(error(
            "scheduler_task_not_found",
            &format!("scheduler_task_not_found:{task_name}"),
            Value::Null,
        ));
    }
    let items=compact_rows(&rows).into_iter().take(limit).map(|task|json!({"task_name":task["task_name"],"last_run":task["last_run"],"status":task["status"],"last_result":task["last_result"],"next_run":task["next_run"],"schedule":task["schedule"],"trigger_count":task["trigger_count"],"triggers":task["triggers"]})).collect::<Vec<_>>();
    Ok(json!({"task_name":task_name,"count":items.len(),"items":items}))
}

fn scheduled_task_definition(task_name: &str) -> Result<Value, Value> {
    let (name, path) = split_task_path(task_name)?;
    let script = r#"$ErrorActionPreference = "Stop";$taskName = [Environment]::GetEnvironmentVariable("NARADA_SCHEDULER_TASK_NAME");$taskPath = [Environment]::GetEnvironmentVariable("NARADA_SCHEDULER_TASK_PATH");$task = Get-ScheduledTask -TaskName $taskName -TaskPath $taskPath;$action = @($task.Actions)[0];$limitSeconds = [System.Xml.XmlConvert]::ToTimeSpan([string]$task.Settings.ExecutionTimeLimit).TotalSeconds;[ordered]@{ task_name = "$($task.TaskPath)$($task.TaskName)"; state = [string]$task.State; execute = [string]$action.Execute; arguments = [string]$action.Arguments; working_dir = [string]$action.WorkingDirectory; hidden = [bool]$task.Settings.Hidden; execution_time_limit_seconds = [int]$limitSeconds; multiple_instances = [string]$task.Settings.MultipleInstances } | ConvertTo-Json -Compress"#;
    let result = powershell(
        script,
        &[
            ("NARADA_SCHEDULER_TASK_NAME".into(), name),
            ("NARADA_SCHEDULER_TASK_PATH".into(), path),
        ],
    )?;
    if result.exit_code != 0 {
        return Err(command_failure(
            "scheduler_task_definition_query_failed",
            "show",
            &result,
            task_name,
            "",
        ));
    }
    let parsed = serde_json::from_str::<Value>(result.stdout.trim()).map_err(|cause| {
        error(
            "scheduler_task_definition_invalid",
            &format!("scheduler_task_definition_invalid:{cause}"),
            Value::Null,
        )
    })?;
    Ok(normalize_task_definition(parsed))
}

fn normalize_task_definition(mut definition: Value) -> Value {
    let execute = definition
        .get("execute")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let arguments = definition
        .get("arguments")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let supervisor = supervisor_path();
    let object = definition
        .as_object_mut()
        .unwrap_or_else(|| panic!("task definition object"));
    if !same_windows_path(&execute, &supervisor.to_string_lossy()) {
        object.insert(
            "console_window_policy".into(),
            json!("unmanaged_direct_process"),
        );
        object.insert("launcher_execute".into(), Value::Null);
        object.insert("launcher_arguments".into(), Value::Null);
        return definition;
    }
    match decode_launch_arguments(&arguments) {
        Ok((command, args)) => {
            object.insert("execute".into(), json!(command));
            object.insert("arguments".into(), json!(args));
            object.insert(
                "console_window_policy".into(),
                json!("native_create_no_window"),
            );
            object.insert("launcher_execute".into(), json!(execute));
            object.insert("launcher_arguments".into(), json!(arguments));
        }
        Err(problem) => {
            object.insert(
                "console_window_policy".into(),
                json!("native_launcher_contract_invalid"),
            );
            object.insert("launcher_execute".into(), json!(execute));
            object.insert("launcher_arguments".into(), json!(arguments));
            object.insert(
                "launcher_contract_error".into(),
                problem.get("message").cloned().unwrap_or(problem),
            );
        }
    }
    definition
}

#[derive(Debug)]
struct LaunchPlan {
    launcher_path: PathBuf,
    launcher_arguments: String,
    target_command: String,
    target_arguments: String,
}
fn launch_plan(
    command: &str,
    arguments: &str,
    require_available: bool,
) -> Result<LaunchPlan, Value> {
    let target = command.trim();
    let target = if target.len() >= 2 && target.starts_with('"') && target.ends_with('"') {
        &target[1..target.len() - 1]
    } else {
        target
    };
    if target.is_empty() {
        return Err(error(
            "scheduler_requires_command",
            "scheduler_requires_command",
            Value::Null,
        ));
    }
    if target.contains('\0') || arguments.contains('\0') {
        return Err(error(
            "scheduler_no_window_launch_invalid",
            "scheduled_command_nul_refused",
            Value::Null,
        ));
    }
    let launcher = supervisor_path();
    if require_available && !launcher.is_file() {
        return Err(error(
            "scheduler_no_window_launcher_unavailable",
            "scheduler_no_window_launcher_unavailable",
            json!({"launcher_path":launcher.to_string_lossy(),"remediation":"Build @narada-core/process-launch-posture before mutating scheduled tasks."}),
        ));
    }
    let mut payload = target.as_bytes().to_vec();
    payload.push(0);
    payload.extend_from_slice(arguments.as_bytes());
    Ok(LaunchPlan {
        launcher_path: launcher,
        launcher_arguments: format!("--scheduled-v1 {}", base64(&payload, true)),
        target_command: target.to_string(),
        target_arguments: arguments.to_string(),
    })
}

fn decode_launch_arguments(arguments: &str) -> Result<(String, String), Value> {
    let Some(payload) = arguments.trim().strip_prefix("--scheduled-v1 ") else {
        return Err(error(
            "scheduled_command_arguments_invalid",
            "scheduled_command_arguments_invalid",
            Value::Null,
        ));
    };
    if payload.is_empty() || payload.contains(char::is_whitespace) {
        return Err(error(
            "scheduled_command_arguments_invalid",
            "scheduled_command_arguments_invalid",
            Value::Null,
        ));
    }
    let bytes = base64_url_decode(payload)?;
    let separators = bytes
        .iter()
        .enumerate()
        .filter(|(_, byte)| **byte == 0)
        .map(|(index, _)| index)
        .collect::<Vec<_>>();
    if separators.len() != 1 || separators[0] == 0 {
        return Err(error(
            "scheduled_command_payload_invalid",
            "scheduled_command_payload_invalid",
            Value::Null,
        ));
    }
    let index = separators[0];
    let command = String::from_utf8(bytes[..index].to_vec()).map_err(|_| {
        error(
            "scheduled_command_payload_invalid",
            "scheduled_command_payload_invalid",
            Value::Null,
        )
    })?;
    let arguments = String::from_utf8(bytes[index + 1..].to_vec()).map_err(|_| {
        error(
            "scheduled_command_payload_invalid",
            "scheduled_command_payload_invalid",
            Value::Null,
        )
    })?;
    Ok((command, arguments))
}

fn supervisor_path() -> PathBuf {
    if let Some(value) = env::var_os("NARADA_PROCESS_SUPERVISOR_PATH") {
        return absolute(PathBuf::from(value));
    }
    if let Ok(executable) = env::current_exe() {
        if let Some(parent) = executable.parent() {
            let adjacent = parent.join("narada-process-supervisor.exe");
            if adjacent.is_file() {
                return adjacent;
            }
        }
        for ancestor in executable.ancestors().take(12) {
            if ancestor
                .file_name()
                .and_then(|name| name.to_str())
                .map(|name| name.eq_ignore_ascii_case("mcp-surfaces"))
                == Some(true)
            {
                if let Some(src) = ancestor.parent() {
                    return src.join("narada/packages/process-launch-posture/native/target/release/narada-process-supervisor.exe");
                }
            }
        }
    }
    PathBuf::from("narada-process-supervisor.exe")
}

fn task_create(args: &Map<String, Value>, root: &Path) -> Result<Value, Value> {
    let task_name = required(args, "task_name", "scheduler_requires_task_name")?;
    let command = required(args, "command", "scheduler_requires_command")?;
    let arguments = optional(args, "arguments").unwrap_or_default();
    let working_dir = optional(args, "working_dir")
        .map(PathBuf::from)
        .map(absolute);
    let schedule = required(args, "schedule", "scheduler_requires_schedule")?;
    let execution_limit = optional_integer_range(args, "execution_time_limit_seconds", 1, 86_400)?;
    let instances = multiple_instances(args.get("multiple_instances"))?;
    assert_action_allowed(&command, &arguments, working_dir.as_deref(), root)?;
    let dry_run = args.get("dry_run").and_then(Value::as_bool) == Some(true);
    let plan = launch_plan(&command, &arguments, !dry_run)?;
    let placeholder = format!(
        "\"{}\" --scheduled-noop-v1",
        plan.launcher_path.to_string_lossy()
    );
    let mut sch_args = strings(&["/create", "/tn", &task_name, "/tr", &placeholder, "/f"]);
    sch_args.extend(schedule_args(&schedule, args)?);
    if dry_run {
        return Ok(
            json!({"schema":"narada.scheduler.task_create_plan.v1","status":"planned","task_name":task_name,"schedule":schedule,"command":join_command(&plan.target_command,&plan.target_arguments),"execute":plan.target_command,"arguments":plan.target_arguments,"launcher_execute":plan.launcher_path.to_string_lossy(),"launcher_arguments":plan.launcher_arguments,"working_dir":working_dir.as_ref().map(|path|path.to_string_lossy().to_string()),"execution_time_limit_seconds":execution_limit,"multiple_instances":instances,"schtasks_create_args":sch_args,"host_effect":false,"bounded":true}),
        );
    }
    let created = schtasks(&sch_args)?;
    if created.exit_code != 0 {
        return Err(command_failure(
            "scheduler_create_failed",
            "create",
            &created,
            &task_name,
            &placeholder,
        ));
    }
    let changed = set_task_action(
        &task_name,
        &plan,
        working_dir.as_deref(),
        execution_limit,
        instances.as_deref(),
    )?;
    if changed.exit_code != 0 {
        return Err(command_failure(
            "scheduler_create_action_failed",
            "create_action",
            &changed,
            &task_name,
            &command,
        ));
    }
    Ok(
        json!({"status":"created","task_name":task_name,"schedule":schedule,"command":join_command(&plan.target_command,&plan.target_arguments),"execute":plan.target_command,"arguments":plan.target_arguments,"launcher_execute":plan.launcher_path.to_string_lossy(),"launcher_arguments":plan.launcher_arguments,"working_dir":working_dir.as_ref().map(|path|path.to_string_lossy().to_string()),"working_dir_applied":working_dir.is_some(),"task_hidden":true,"execution_time_limit_seconds":execution_limit,"multiple_instances":instances,"console_window_policy":"native_create_no_window","mutation_method":"schtasks_create_then_powershell_set_scheduled_task_native_no_window_action_and_hidden_settings"}),
    )
}

fn task_update_action(args: &Map<String, Value>, root: &Path) -> Result<Value, Value> {
    let task_name = required(args, "task_name", "scheduler_requires_task_name")?;
    let command = required(args, "command", "scheduler_requires_command")?;
    let arguments = optional(args, "arguments").unwrap_or_default();
    let working_dir = optional(args, "working_dir")
        .map(PathBuf::from)
        .map(absolute);
    let execution_limit = optional_integer_range(args, "execution_time_limit_seconds", 1, 86_400)?;
    let instances = multiple_instances(args.get("multiple_instances"))?;
    assert_action_allowed(&command, &arguments, working_dir.as_deref(), root)?;
    let dry_run = args.get("dry_run").and_then(Value::as_bool) == Some(true);
    let plan = launch_plan(&command, &arguments, !dry_run)?;
    let launcher_command = format!(
        "\"{}\" {}",
        plan.launcher_path.to_string_lossy(),
        plan.launcher_arguments
    );
    let preview = strings(&["/change", "/tn", &task_name, "/tr", &launcher_command]);
    if dry_run {
        return Ok(
            json!({"status":"planned","task_name":task_name,"command":join_command(&plan.target_command,&plan.target_arguments),"execute":command,"arguments":arguments,"mutation_method":"powershell_set_scheduled_task_native_no_window_action","console_window_policy":"native_create_no_window","launcher_execute":plan.launcher_path.to_string_lossy(),"launcher_arguments":plan.launcher_arguments,"schtasks_preview_args":preview,"schtasks_preview_not_used_for_mutation":true,"preserves_triggers":true,"preserves_enabled_state":true,"enabled_state_preservation":"scheduled_task_settings_disable_flag","working_dir":working_dir.as_ref().map(|path|path.to_string_lossy().to_string()),"working_dir_applied":false,"working_dir_would_apply":working_dir.is_some(),"execution_time_limit_seconds":execution_limit,"multiple_instances":instances}),
        );
    }
    let changed = set_task_action(
        &task_name,
        &plan,
        working_dir.as_deref(),
        execution_limit,
        instances.as_deref(),
    )?;
    if changed.exit_code != 0 {
        return Err(command_failure(
            "scheduler_update_action_failed",
            "update_action",
            &changed,
            &task_name,
            &command,
        ));
    }
    Ok(
        json!({"status":"updated","task_name":task_name,"command":join_command(&plan.target_command,&plan.target_arguments),"preserves_triggers":true,"preserves_enabled_state":true,"enabled_state_preservation":"scheduled_task_settings_disable_flag","mutation_method":"powershell_set_scheduled_task_native_no_window_action","execute":plan.target_command,"arguments":plan.target_arguments,"launcher_execute":plan.launcher_path.to_string_lossy(),"launcher_arguments":plan.launcher_arguments,"console_window_policy":"native_create_no_window","working_dir":working_dir.as_ref().map(|path|path.to_string_lossy().to_string()),"working_dir_applied":working_dir.is_some(),"execution_time_limit_seconds":execution_limit,"multiple_instances":instances}),
    )
}

fn set_task_action(
    task_name: &str,
    plan: &LaunchPlan,
    working_dir: Option<&Path>,
    execution_limit: Option<i64>,
    instances: Option<&str>,
) -> Result<CommandResult, Value> {
    let (name, path) = split_task_path(task_name)?;
    let script = r#"$ErrorActionPreference = "Stop";$execute = [Environment]::GetEnvironmentVariable("NARADA_SCHEDULER_EXECUTE");$arguments = [Environment]::GetEnvironmentVariable("NARADA_SCHEDULER_ARGUMENTS");$workingDirectory = [Environment]::GetEnvironmentVariable("NARADA_SCHEDULER_WORKING_DIR");$executionLimitSeconds = [Environment]::GetEnvironmentVariable("NARADA_SCHEDULER_EXECUTION_LIMIT_SECONDS");$multipleInstances = [Environment]::GetEnvironmentVariable("NARADA_SCHEDULER_MULTIPLE_INSTANCES");$taskName = [Environment]::GetEnvironmentVariable("NARADA_SCHEDULER_TASK_NAME");$taskPath = [Environment]::GetEnvironmentVariable("NARADA_SCHEDULER_TASK_PATH");$existingTask = Get-ScheduledTask -TaskName $taskName -TaskPath $taskPath;$wasDisabled = [string]$existingTask.State -eq "Disabled";if ([string]::IsNullOrWhiteSpace($workingDirectory)) { if ([string]::IsNullOrWhiteSpace($arguments)) { $action = New-ScheduledTaskAction -Execute $execute } else { $action = New-ScheduledTaskAction -Execute $execute -Argument $arguments } } else { if ([string]::IsNullOrWhiteSpace($arguments)) { $action = New-ScheduledTaskAction -Execute $execute -WorkingDirectory $workingDirectory } else { $action = New-ScheduledTaskAction -Execute $execute -Argument $arguments -WorkingDirectory $workingDirectory } };$settingsArguments = @{ Hidden = $true };if ($wasDisabled) { $settingsArguments.Disable = $true };if (-not [string]::IsNullOrWhiteSpace($executionLimitSeconds)) { $settingsArguments.ExecutionTimeLimit = [TimeSpan]::FromSeconds([int]$executionLimitSeconds) };if (-not [string]::IsNullOrWhiteSpace($multipleInstances)) { $settingsArguments.MultipleInstances = $multipleInstances };$settings = New-ScheduledTaskSettingsSet @settingsArguments;Set-ScheduledTask -TaskName $taskName -TaskPath $taskPath -Action $action -Settings $settings | Out-Null"#;
    powershell(
        script,
        &[
            ("NARADA_SCHEDULER_TASK_NAME".into(), name),
            ("NARADA_SCHEDULER_TASK_PATH".into(), path),
            (
                "NARADA_SCHEDULER_EXECUTE".into(),
                plan.launcher_path.to_string_lossy().to_string(),
            ),
            (
                "NARADA_SCHEDULER_ARGUMENTS".into(),
                plan.launcher_arguments.clone(),
            ),
            (
                "NARADA_SCHEDULER_WORKING_DIR".into(),
                working_dir
                    .map(|path| path.to_string_lossy().to_string())
                    .unwrap_or_default(),
            ),
            (
                "NARADA_SCHEDULER_EXECUTION_LIMIT_SECONDS".into(),
                execution_limit
                    .map(|value| value.to_string())
                    .unwrap_or_default(),
            ),
            (
                "NARADA_SCHEDULER_MULTIPLE_INSTANCES".into(),
                instances.unwrap_or("").to_string(),
            ),
        ],
    )
}

fn task_simple_mutation(
    operation: &str,
    args: &Map<String, Value>,
    verb: &str,
    status: &str,
) -> Result<Value, Value> {
    let task_name = required(args, "task_name", "scheduler_requires_task_name")?;
    let mut values = strings(&[verb, "/tn", &task_name]);
    match operation {
        "delete" => values.push("/f".into()),
        "enable" => values.push("/enable".into()),
        "disable" => values.push("/disable".into()),
        _ => {}
    }
    let result = schtasks(&values)?;
    if result.exit_code != 0 {
        return Err(command_failure(
            &format!("scheduler_{operation}_failed"),
            operation,
            &result,
            &task_name,
            "",
        ));
    }
    Ok(json!({"status":status,"task_name":task_name}))
}

fn schedule_args(schedule: &str, args: &Map<String, Value>) -> Result<Vec<String>, Value> {
    match schedule {
        "daily" => Ok(strings(&[
            "/sc",
            "daily",
            "/st",
            optional(args, "start_time").as_deref().unwrap_or("09:00"),
        ])),
        "hourly" => {
            let interval = args
                .get("interval_minutes")
                .and_then(Value::as_i64)
                .unwrap_or(60)
                .clamp(1, 1440);
            if interval < 60 || interval % 60 != 0 {
                Ok(strings(&["/sc", "minute", "/mo", &interval.to_string()]))
            } else {
                Ok(strings(&[
                    "/sc",
                    "hourly",
                    "/mo",
                    &(interval / 60).max(1).to_string(),
                ]))
            }
        }
        "at_startup" => Ok(strings(&["/sc", "onstart"])),
        "at_logon" => Ok(strings(&["/sc", "onlogon"])),
        "once" => Ok(strings(&[
            "/sc",
            "once",
            "/st",
            optional(args, "start_time").as_deref().unwrap_or("09:00"),
        ])),
        _ => Err(error(
            "scheduler_invalid_schedule",
            &format!("scheduler_invalid_schedule:{schedule}"),
            Value::Null,
        )),
    }
}

fn multiple_instances(value: Option<&Value>) -> Result<Option<String>, Value> {
    match value.and_then(Value::as_str) {
        None => Ok(None),
        Some("ignore_new") => Ok(Some("IgnoreNew".into())),
        Some("parallel") => Ok(Some("Parallel".into())),
        Some("queue") => Ok(Some("Queue".into())),
        Some("stop_existing") => Ok(Some("StopExisting".into())),
        Some(other) => Err(error(
            "scheduler_multiple_instances_invalid",
            &format!("scheduler_multiple_instances_invalid:{other}"),
            Value::Null,
        )),
    }
}

fn assert_action_allowed(
    command: &str,
    arguments: &str,
    working_dir: Option<&Path>,
    root: &Path,
) -> Result<(), Value> {
    let reasons = action_policy_reasons(command, arguments, working_dir, root);
    if reasons.is_empty() {
        Ok(())
    } else {
        Err(error(
            "scheduler_action_refused",
            "scheduler_action_refused",
            json!({"refusal_reasons":reasons,"remediation":"Schedule the owning executable directly from an allowed root. Do not use cmd, transient wrappers, or scripts staged under .ai/tmp or .ai/temp."}),
        ))
    }
}

fn action_policy_reasons(
    command: &str,
    arguments: &str,
    working_dir: Option<&Path>,
    root: &Path,
) -> Vec<String> {
    let mut reasons = Vec::new();
    let executable = command
        .trim_matches(['\'', '"'])
        .replace('\\', "/")
        .rsplit('/')
        .next()
        .unwrap_or("")
        .to_ascii_lowercase();
    if matches!(executable.as_str(), "cmd" | "cmd.exe") {
        reasons.push(format!("scheduler_shell_action_disallowed:{command}"));
    }
    for token in std::iter::once(command.to_string()).chain(action_tokens(arguments)) {
        let normalized = token.replace('\\', "/");
        let clean = normalized.trim_matches(['\'', '"']);
        let extension = Path::new(clean)
            .extension()
            .and_then(|value| value.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        if !matches!(extension.as_str(), "cmd" | "bat") {
            continue;
        }
        let candidate = absolute(if Path::new(clean).is_absolute() {
            PathBuf::from(clean)
        } else {
            working_dir.unwrap_or(root).join(clean)
        });
        let canonical =
            !transient_path(&normalized) && is_within(&candidate, root) && candidate.is_file();
        if !canonical {
            reasons.push(format!("scheduler_transient_wrapper_refused:{token}"));
        }
    }
    for value in [command, arguments] {
        let normalized = value.replace('\\', "/").to_ascii_lowercase();
        if transient_path(&normalized)
            && [".ps1", ".psm1", ".js", ".mjs", ".cjs", ".ts"]
                .iter()
                .any(|suffix| normalized.contains(suffix))
        {
            reasons.push(format!("scheduler_transient_script_path_refused:{value}"));
        }
    }
    if let Some(directory) = working_dir {
        if !is_within(
            &absolute(directory.to_path_buf()),
            &absolute(root.to_path_buf()),
        ) {
            reasons.push(format!(
                "scheduler_working_dir_outside_allowed_root:{}",
                directory.to_string_lossy()
            ));
        }
    }
    reasons.sort();
    reasons.dedup();
    reasons
}

fn transient_path(value: &str) -> bool {
    let normalized = value.replace('\\', "/").to_ascii_lowercase();
    normalized.contains("/.ai/tmp/")
        || normalized.contains("/.ai/temp/")
        || normalized.ends_with("/.ai/tmp")
        || normalized.ends_with("/.ai/temp")
}
fn action_tokens(value: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut quote = None;
    for character in value.chars() {
        match (quote, character) {
            (Some(expected), value) if value == expected => quote = None,
            (Some(_), value) => current.push(value),
            (None, '\'' | '"') => quote = Some(character),
            (None, value) if value.is_whitespace() => {
                if !current.is_empty() {
                    tokens.push(std::mem::take(&mut current));
                }
            }
            (None, value) => current.push(value),
        }
    }
    if !current.is_empty() {
        tokens.push(current);
    }
    tokens
}
fn is_within(candidate: &Path, root: &Path) -> bool {
    let candidate = absolute(candidate.to_path_buf());
    let root = absolute(root.to_path_buf());
    candidate == root || candidate.starts_with(root)
}
fn absolute(path: PathBuf) -> PathBuf {
    if path.is_absolute() {
        normalize_path(path)
    } else {
        normalize_path(env::current_dir().unwrap_or_default().join(path))
    }
}
fn normalize_path(path: PathBuf) -> PathBuf {
    let mut result = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                result.pop();
            }
            other => result.push(other.as_os_str()),
        }
    }
    result
}

fn split_task_path(value: &str) -> Result<(String, String), Value> {
    let mut normalized = value.trim().replace('/', "\\");
    if !normalized.starts_with('\\') {
        normalized.insert(0, '\\');
    }
    let Some(index) = normalized.rfind('\\') else {
        return Err(error(
            "scheduler_requires_task_name",
            "scheduler_requires_task_name",
            Value::Null,
        ));
    };
    let name = normalized[index + 1..].to_string();
    if name.is_empty() {
        return Err(error(
            "scheduler_requires_task_name",
            "scheduler_requires_task_name",
            Value::Null,
        ));
    }
    Ok((name, normalized[..=index].to_string()))
}
fn same_windows_path(left: &str, right: &str) -> bool {
    left.trim()
        .trim_matches('"')
        .replace('/', "\\")
        .eq_ignore_ascii_case(&right.trim().trim_matches('"').replace('/', "\\"))
}

fn parse_csv(csv: &str) -> Vec<Map<String, Value>> {
    let mut lines = csv.trim().lines();
    let Some(header) = lines.next() else {
        return Vec::new();
    };
    let headers = parse_csv_line(header);
    let mut rows = Vec::new();
    for line in lines {
        let values = parse_csv_line(line);
        if values.len() != headers.len() {
            continue;
        }
        let mut row = Map::new();
        for (key, value) in headers.iter().zip(values) {
            row.insert(key.clone(), json!(value));
        }
        rows.push(row);
    }
    rows
}
fn parse_csv_line(line: &str) -> Vec<String> {
    let mut values = Vec::new();
    let mut current = String::new();
    let mut chars = line.chars().peekable();
    let mut quoted = false;
    while let Some(character) = chars.next() {
        if quoted {
            if character == '"' {
                if chars.peek() == Some(&'"') {
                    current.push('"');
                    chars.next();
                } else {
                    quoted = false;
                }
            } else {
                current.push(character);
            }
        } else if character == '"' {
            quoted = true;
        } else if character == ',' {
            values.push(current.trim().to_string());
            current.clear();
        } else {
            current.push(character);
        }
    }
    values.push(current.trim().to_string());
    values
}

fn compact_rows(rows: &[Map<String, Value>]) -> Vec<Value> {
    let mut order = Vec::<String>::new();
    let mut grouped = BTreeTaskMap::new();
    for row in rows {
        let name = row.get("TaskName").and_then(Value::as_str).unwrap_or("");
        if name.is_empty() || name == "TaskName" {
            continue;
        }
        let trigger = json!({"schedule":field(row,"Schedule Type"),"start_time":field(row,"Start Time"),"start_date":field(row,"Start Date"),"end_date":field(row,"End Date"),"days":field(row,"Days"),"months":field(row,"Months"),"repeat_every":field(row,"Repeat: Every"),"repeat_until_time":field(row,"Repeat: Until: Time"),"repeat_until_duration":field(row,"Repeat: Until: Duration"),"repeat_stop_if_still_running":field(row,"Repeat: Stop If Still Running"),"next_run":field(row,"Next Run Time")});
        if let Some(existing) = grouped.get_mut(name) {
            existing
                .get_mut("triggers")
                .and_then(Value::as_array_mut)
                .unwrap()
                .push(trigger);
            let count = existing["triggers"].as_array().map(Vec::len).unwrap_or(0);
            existing["trigger_count"] = json!(count);
        } else {
            order.push(name.to_string());
            grouped.insert(name.to_string(),json!({"task_name":name,"status":field(row,"Status"),"schedule":field(row,"Schedule Type"),"next_run":field(row,"Next Run Time"),"last_run":field(row,"Last Run Time"),"last_result":field(row,"Last Result"),"command":field(row,"Task To Run"),"trigger_count":1,"triggers":[trigger]}));
        }
    }
    order
        .into_iter()
        .filter_map(|key| grouped.remove(&key))
        .collect()
}
type BTreeTaskMap = std::collections::BTreeMap<String, Value>;
fn field(row: &Map<String, Value>, name: &str) -> Value {
    row.get(name).cloned().unwrap_or(Value::Null)
}

fn command_failure(
    code: &str,
    operation: &str,
    result: &CommandResult,
    task_name: &str,
    command: &str,
) -> Value {
    let combined = format!("{}\n{}", result.stdout, result.stderr).to_ascii_lowercase();
    let classification = if result.timed_out {
        "scheduler_command_timed_out"
    } else if combined.contains("access is denied") {
        "requires_elevation"
    } else if combined.contains("folder") || combined.contains("cannot find") {
        "invalid_task_path_or_missing_task"
    } else if combined.contains("invalid") || result.exit_code == 2147500037_i64 as i32 {
        "invalid_arguments_or_unsupported_scheduler_option"
    } else {
        "scheduler_command_failed"
    };
    error(
        code,
        &format!("{code}:{}", result.exit_code),
        json!({"operation":operation,"exit_code":result.exit_code,"classification":classification,"requires_elevation":classification=="requires_elevation","timed_out":result.timed_out,"timeout_ms":COMMAND_TIMEOUT.as_millis(),"task_name":task_name,"stdout":result.stdout,"stderr":result.stderr,"command":command,"remediation":if classification=="requires_elevation"{"Run the equivalent scheduler command from an elevated operator terminal."}else{"Inspect bounded stdout/stderr and retry with a concrete task path."}}),
    )
}

fn required(args: &Map<String, Value>, field: &str, code: &str) -> Result<String, Value> {
    args.get(field)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
        .ok_or_else(|| error(code, code, Value::Null))
}
fn optional(args: &Map<String, Value>, field: &str) -> Option<String> {
    args.get(field)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
}
fn optional_integer_range(
    args: &Map<String, Value>,
    field: &str,
    min: i64,
    max: i64,
) -> Result<Option<i64>, Value> {
    match args.get(field) {
        None => Ok(None),
        Some(value) => value
            .as_i64()
            .filter(|number| (*number >= min) && (*number <= max))
            .map(Some)
            .ok_or_else(|| {
                error(
                    &format!("{field}_invalid"),
                    &format!("{field}_invalid"),
                    json!({"minimum":min,"maximum":max}),
                )
            }),
    }
}
fn strings(values: &[&str]) -> Vec<String> {
    values.iter().map(|value| (*value).to_string()).collect()
}
fn join_command(command: &str, arguments: &str) -> String {
    if arguments.is_empty() {
        command.to_string()
    } else {
        format!("{command} {arguments}")
    }
}
fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}
fn error(code: &str, message: &str, details: Value) -> Value {
    json!({"schema":"narada.scheduler_mcp.error.v1","code":code,"message":message,"details":details})
}

fn base64(bytes: &[u8], url_safe: bool) -> String {
    let alphabet = if url_safe {
        b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_"
    } else {
        b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/"
    };
    let mut output = String::new();
    let mut index = 0;
    while index < bytes.len() {
        let a = bytes[index] as u32;
        let b = bytes.get(index + 1).copied().unwrap_or(0) as u32;
        let c = bytes.get(index + 2).copied().unwrap_or(0) as u32;
        let value = (a << 16) | (b << 8) | c;
        for shift in [18, 12, 6, 0] {
            output.push(alphabet[((value >> shift) & 63) as usize] as char);
        }
        match bytes.len() - index {
            1 => {
                output.pop();
                output.pop();
                if !url_safe {
                    output.push('=');
                    output.push('=');
                }
            }
            2 => {
                output.pop();
                if !url_safe {
                    output.push('=');
                }
            }
            _ => {}
        }
        index += 3;
    }
    output
}
fn base64_url_decode(value: &str) -> Result<Vec<u8>, Value> {
    let mut buffer = 0_u32;
    let mut bits = 0_u8;
    let mut output = Vec::new();
    for byte in value.bytes() {
        let sextet = match byte {
            b'A'..=b'Z' => byte - b'A',
            b'a'..=b'z' => byte - b'a' + 26,
            b'0'..=b'9' => byte - b'0' + 52,
            b'-' => 62,
            b'_' => 63,
            _ => {
                return Err(error(
                    "scheduled_command_payload_invalid",
                    "scheduled_command_payload_invalid",
                    Value::Null,
                ))
            }
        } as u32;
        buffer = (buffer << 6) | sextet;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            output.push(((buffer >> bits) & 0xff) as u8);
            buffer = if bits == 0 {
                0
            } else {
                buffer & ((1_u32 << bits) - 1)
            };
        }
    }
    if bits > 0 && buffer != 0 {
        return Err(error(
            "scheduled_command_payload_invalid",
            "scheduled_command_payload_invalid",
            Value::Null,
        ));
    }
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn tool_contract_is_complete() {
        let tools = list_tools();
        let names = tools
            .iter()
            .filter_map(|value| {
                value
                    .get("name")
                    .and_then(Value::as_str)
                    .map(ToString::to_string)
            })
            .collect::<Vec<_>>();
        for name in [
            "scheduler_task_create",
            "scheduler_binding_pause",
            "scheduler_activation_resolve",
        ] {
            assert!(names.contains(&name.to_string()), "{name}");
        }
        let find = |name: &str| tools.iter().find(|tool| tool["name"] == name).unwrap();
        assert_eq!(
            find("scheduler_task_list")["inputSchema"]["properties"]["offset"]["default"],
            0
        );
        assert_eq!(
            find("scheduler_task_create")["inputSchema"]["properties"]["dry_run"]["default"],
            false
        );
    }
    #[test]
    fn schedule_translation_matches_existing_contract() {
        assert_eq!(
            schedule_args(
                "hourly",
                json!({"interval_minutes":15}).as_object().unwrap()
            )
            .unwrap(),
            strings(&["/sc", "minute", "/mo", "15"])
        );
        assert_eq!(
            schedule_args(
                "hourly",
                json!({"interval_minutes":120}).as_object().unwrap()
            )
            .unwrap(),
            strings(&["/sc", "hourly", "/mo", "2"])
        );
        assert_eq!(
            schedule_args(
                "hourly",
                json!({"interval_minutes":90}).as_object().unwrap()
            )
            .unwrap(),
            strings(&["/sc", "minute", "/mo", "90"])
        );
    }
    #[test]
    fn launch_payload_round_trips() {
        let plan = launch_plan("node.exe", "\"C:\\site\\job.js\" run", false).unwrap();
        assert_eq!(
            decode_launch_arguments(&plan.launcher_arguments).unwrap(),
            ("node.exe".into(), "\"C:\\site\\job.js\" run".into())
        );
    }
    #[test]
    fn csv_rows_compact_multiple_triggers() {
        let csv="\"TaskName\",\"Status\",\"Schedule Type\",\"Next Run Time\",\"Last Run Time\",\"Last Result\",\"Task To Run\"\r\n\"\\\\Narada\",\"Ready\",\"At logon time\",\"N/A\",\"today\",\"0\",\"pwsh.exe\"\r\n\"\\\\Narada\",\"Ready\",\"Minute\",\"soon\",\"today\",\"0\",\"pwsh.exe\"";
        let rows = compact_rows(&parse_csv(csv));
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["trigger_count"], 2);
    }
    #[test]
    fn policy_rejects_shell_transience_and_escape() {
        let root = PathBuf::from("C:\\workspace\\site");
        assert!(action_policy_reasons("cmd.exe", "/c tool.cmd", None, &root)
            .iter()
            .any(|reason| reason.starts_with("scheduler_shell_action_disallowed")));
        assert!(action_policy_reasons(
            "pwsh.exe",
            "-File C:\\workspace\\site\\.ai\\tmp\\tool.ps1",
            None,
            &root
        )
        .iter()
        .any(|reason| reason.starts_with("scheduler_transient_script_path_refused")));
        assert!(
            action_policy_reasons("pwsh.exe", "", Some(Path::new("D:\\other")), &root)
                .iter()
                .any(|reason| reason.starts_with("scheduler_working_dir_outside_allowed_root"))
        );
    }
    #[test]
    fn dry_run_has_no_host_effect() {
        let args=json!({"task_name":"\\Narada\\Fixture","command":"pwsh.exe","arguments":"-NoProfile","implementation_id":implementation_id(),"dry_run":true}).as_object().unwrap().clone();
        let result = task_update_action(&args, Path::new("C:\\workspace"));
        assert_eq!(result.unwrap()["status"], "planned");
        let create=json!({"task_name":"\\Narada\\Fixture","command":"C:\\workspace\\job.exe","schedule":"at_logon","implementation_id":implementation_id(),"dry_run":true}).as_object().unwrap().clone();
        let planned = task_create(&create, Path::new("C:\\workspace")).expect("create plan");
        assert_eq!(planned["status"], "planned");
        assert_eq!(planned["host_effect"], false);
        assert_eq!(planned["schema"], "narada.scheduler.task_create_plan.v1");
    }
}
