use serde_json::{json, Map, Value};
use std::collections::BTreeMap;
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};
use time::OffsetDateTime;

const SERVER_NAME: &str = "worker-delegation-mcp";
const DEFAULT_COGNITION: &str = "low";
const MAX_RUNS: usize = 200;
const MAX_FILE_BYTES: usize = 256_000;
const MAX_NATIVE_READ_BYTES: usize = 64 * 1024;
const MAX_NATIVE_READ_FILES: usize = 8;
const MAX_INLINE_WAIT_MS: u64 = 100_000;
const READ_ONLY_COMMAND_CONTRACT: &str = "READ-ONLY COMMAND CONTRACT (apply before acting): use one executable with literal argv per probe; use supplied native preflight evidence for path existence/readability instead of probing again; when constraints.preflight_paths contains access=read, the native authority injects bounded file evidence before launch; never combine probes with &&, ;, pipes, redirection, $(), backticks, or generated scripts. If a probe is refused, stop that probe and report the refusal; do not retry by bundling commands or changing shells. WORKER OUTPUT CONTRACT (apply before acting): return only the requested result; no preamble, progress narration, tool narration, or post-hoc workflow story. Keep the final response within 4096 characters unless the requested structured schema requires less. Put observed refusals in a refusals array and keep execution details out of the result. WINDOWS READ-ONLY PROBE: when a filesystem read probe is actually needed, use pwsh with literal argv [-NoProfile, -NonInteractive, -Command, Get-Content -LiteralPath <literal-path> -TotalCount 1]. Do not start with dotnet File.OpenRead, C# scripts, generated scripts, or alternate shells. If supplied native_read status is passed, command probes for that path are prohibited: do not call shell, structured-command, pwsh, dotnet, or any command tool; use the evidence above as authoritative and do not retry. Prefer supplied native preflight evidence and do not probe again when it is present.";
const READ_TOOLS: &[(&str, &str)] = &[
    (
        "worker_output_show",
        "Read a bounded materialized worker artifact.",
    ),
    (
        "worker_result_show",
        "Read a bounded final worker result directly, with durable raw-output and execution-log references.",
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
        "Read one durable worker run with bounded state-file polling; native mode does not launch a child.",
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
const COMMAND_TOOLS: &[(&str, &str)] = &[
    (
        "worker_command_run",
        "Run one bounded literal-argv command without starting a reasoning worker.",
    ),
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
    for (name, description) in COMMAND_TOOLS {
        tools.push(command_tool(name, description, input_schema(name)));
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

pub fn call_tool(
    name: &str,
    args: &Map<String, Value>,
    root: &Path,
    allowed_roots: &[PathBuf],
) -> Result<Value, Value> {
    match name {
        "worker_guidance" => Ok(guidance(args)),
        "worker_policy_inspect" => Ok(policy(root, allowed_roots)),
        "worker_cognition_defaults_inspect" => Ok(cognition_defaults(root)),
        "worker_config_resolve" => config_resolve(args, root, allowed_roots),
        "worker_run_status" => run_status(args, root),
        "worker_runs_list" => runs_list(args, root),
        "worker_run_wait" => run_wait(args, root),
        "worker_run_wait_batch" => run_wait_batch(args, root),
        "worker_runs_synthesize" => runs_synthesize(args, root),
        "worker_dashboard_describe" => dashboard(args, root),
        "worker_output_show" => output_show(args, root),
        "worker_result_show" => result_show(args, root),
        "worker_operator_affordances" => Ok(affordances()),
        "worker_cognition_defaults_update" => cognition_defaults_update(args, root),
        "worker_run" => worker_run(args, root, allowed_roots, None, "worker_run"),
        "worker_edit" => worker_edit(args, root, allowed_roots),
        "worker_resume" => worker_resume(args, root, allowed_roots),
        "worker_run_reap" => worker_run_reap(args, root),
        "worker_run_batch" => worker_run_batch(args, root, allowed_roots),
        "worker_command_run" => command_run(args, root, allowed_roots),
        "worker_delegate_batch" => {
            let mut failure = error(
                "worker_tool_renamed",
                "worker_delegate_batch was renamed to worker_run_batch",
            );
            failure["migration"] = json!({
                "from":"worker_delegate_batch",
                "to":"worker_run_batch",
                "replacement_tool":"worker_run_batch",
                "reason":"The native worker surface uses worker_run_batch as the canonical batch entrypoint."
            });
            Err(failure)
        }
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
    json!({"schema":"narada.worker.guidance.v1","status":"ok","server_name":SERVER_NAME,"requested":{"workflow":args.get("workflow").cloned().unwrap_or(Value::Null),"tool":args.get("tool").cloned().unwrap_or(Value::Null)},"cognition":{"default":"low","omitted_constraint_behavior":"constraints.cognition resolves to low","mapping_tool":"worker_cognition_defaults_inspect","mapping_semantics":"cognition selects the admitted model tier; reasoning_effort is disclosed separately and is not a promise of low latency"},"first_use":["Inspect worker_policy_inspect.","Omitted constraints.cognition resolves to low; use worker_cognition_defaults_inspect for the current model and reasoning-effort mapping.","Resolve worker inputs without launching with worker_config_resolve.","For a simple literal-argv probe, use worker_command_run; it does not start a reasoning worker and returns separate execution and objective verdicts.","Launch with worker_run or worker_edit. Set constraints.wait_for_completion=true with a bounded wait_timeout_ms when one-call completion is preferred; omitted or false returns the accepted running record immediately.","Read durable runs with worker_run_status or worker_run_wait; worker_run_wait is the canonical bounded state-file poll and does not launch a child.","Use worker_result_show for direct bounded final-result readback, or worker_output_show for any artifact reference.","For read-only file evidence, supply constraints.preflight_paths entries with access=read; native authority injects bounded content before launch.",READ_ONLY_COMMAND_CONTRACT],"windows_rust_toolchain":{"status":"caller_environment_required","remediation":"For MSVC Rust linking, launch the carrier from a Developer PowerShell or initialize VsDevCmd before starting the carrier so link.exe is inherited by workers.","probe":"worker_policy_inspect reports the inherited PATH boundary; workers do not perform open-ended Visual Studio discovery."},"boundaries":["The native Rust surface launches only the native Rust narada-agent-runtime-server.","Credentials remain SecretStore-referenced and are never returned.","Run records are bounded to the site worker-delegation root."]})
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
    path_components_equal_or_child(&p, &r)
}
