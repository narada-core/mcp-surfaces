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

