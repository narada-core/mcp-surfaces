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
const READ_ONLY_COMMAND_CONTRACT: &str = "READ-ONLY COMMAND CONTRACT (apply before acting): use one executable with literal argv per probe; use supplied native preflight evidence for path existence/readability instead of probing again; never combine probes with &&, ;, pipes, redirection, $(), backticks, or generated scripts. If a probe is refused, stop that probe and report the refusal; do not retry by bundling commands or changing shells.";
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
        "worker_operator_affordances" => Ok(affordances()),
        "worker_cognition_defaults_update" => cognition_defaults_update(args, root),
        "worker_run" => worker_run(args, root, allowed_roots, None, "worker_run"),
        "worker_edit" => worker_edit(args, root, allowed_roots),
        "worker_resume" => worker_resume(args, root, allowed_roots),
        "worker_run_reap" => worker_run_reap(args, root),
        "worker_run_batch" => worker_run_batch(args, root, allowed_roots),
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
    json!({"schema":"narada.worker.guidance.v1","status":"ok","server_name":SERVER_NAME,"requested":{"workflow":args.get("workflow").cloned().unwrap_or(Value::Null),"tool":args.get("tool").cloned().unwrap_or(Value::Null)},"cognition":{"default":"low","omitted_constraint_behavior":"constraints.cognition resolves to low","mapping_tool":"worker_cognition_defaults_inspect"},"first_use":["Inspect worker_policy_inspect.","Omitted constraints.cognition resolves to low; use worker_cognition_defaults_inspect for the current model and reasoning-effort mapping.","Resolve worker inputs without launching with worker_config_resolve.","Launch with worker_run or worker_edit. Set constraints.wait_for_completion=true with a bounded wait_timeout_ms when one-call completion is preferred; omitted or false returns the accepted running record immediately.","Read durable runs with worker_run_status or worker_run_wait; worker_run_wait is the canonical bounded state-file poll and does not launch a child.","Use worker_output_show for bounded artifact readback.",READ_ONLY_COMMAND_CONTRACT],"windows_rust_toolchain":{"status":"caller_environment_required","remediation":"For MSVC Rust linking, launch the carrier from a Developer PowerShell or initialize VsDevCmd before starting the carrier so link.exe is inherited by workers.","probe":"worker_policy_inspect reports the inherited PATH boundary; workers do not perform open-ended Visual Studio discovery."},"boundaries":["The native Rust surface launches only the native Rust narada-agent-runtime-server.","Credentials remain SecretStore-referenced and are never returned.","Run records are bounded to the site worker-delegation root."]})
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
fn preflight_paths(
    constraints: Option<&Map<String, Value>>,
    cwd: &Path,
    allowed_roots: &[PathBuf],
) -> Result<Value, Value> {
    let Some(items) = constraints
        .and_then(|value| value.get("preflight_paths"))
        .and_then(Value::as_array)
    else {
        return Ok(json!({"status":"not_requested","items":[]}));
    };
    let mut checked = Vec::with_capacity(items.len());
    for item in items {
        let Some(object) = item.as_object() else {
            return Err(error("worker_preflight_path_invalid", "worker_preflight_path_invalid"));
        };
        let raw_path = object
            .get("path")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| error("worker_preflight_path_required", "worker_preflight_path_required"))?;
        let access = object
            .get("access")
            .and_then(Value::as_str)
            .unwrap_or("read");
        if !matches!(access, "read" | "write" | "create") {
            return Err(error("worker_preflight_access_invalid", "worker_preflight_access_invalid"));
        }
        let path = {
            let candidate = PathBuf::from(raw_path);
            if candidate.is_absolute() { candidate } else { cwd.join(candidate) }
        };
        let scope_path = if path.exists() {
            path.as_path()
        } else {
            path.parent().unwrap_or(path.as_path())
        };
        if !allowed_roots.iter().any(|allowed| is_within(scope_path, allowed)) {
            return Err(json!({
                "schema":"narada.worker.error.v1",
                "code":"worker_preflight_path_outside_allowed_roots",
                "message":"worker_preflight_path_outside_allowed_roots",
                "path":path.to_string_lossy(),
                "access":access,
                "preflight_status":"failed",
                "remediation":"Use a path under the admitted worker root."
            }));
        }
        let exists = path.exists();
        if matches!(access, "read" | "write") && !exists {
            return Err(json!({
                "schema":"narada.worker.error.v1",
                "code":"worker_preflight_path_missing",
                "message":"worker_preflight_path_missing",
                "path":path.to_string_lossy(),
                "access":access,
                "preflight_status":"failed",
                "remediation":"Correct constraints.preflight_paths or remove the stale path before retrying."
            }));
        }
        if access == "create" && !exists && path.parent().is_some_and(|parent| !parent.exists()) {
            return Err(json!({
                "schema":"narada.worker.error.v1",
                "code":"worker_preflight_parent_missing",
                "message":"worker_preflight_parent_missing",
                "path":path.to_string_lossy(),
                "access":access,
                "preflight_status":"failed",
                "remediation":"Create or select an existing parent directory before retrying."
            }));
        }
        checked.push(json!({
            "path":path.to_string_lossy(),
            "access":access,
            "exists":exists,
            "status":"passed"
        }));
    }
    Ok(json!({"status":"passed","items":checked}))
}
#[cfg(windows)]
fn path_components_equal_or_child(path: &Path, root: &Path) -> bool {
    let path = path
        .components()
        .map(|component| component.as_os_str().to_string_lossy().to_lowercase())
        .collect::<Vec<_>>();
    let root = root
        .components()
        .map(|component| component.as_os_str().to_string_lossy().to_lowercase())
        .collect::<Vec<_>>();
    path.len() >= root.len()
        && path
            .iter()
            .zip(root.iter())
            .all(|(left, right)| left == right)
}
#[cfg(not(windows))]
fn path_components_equal_or_child(path: &Path, root: &Path) -> bool {
    path == root || path.starts_with(root)
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

fn policy(root: &Path, allowed_roots: &[PathBuf]) -> Value {
    json!({"schema":"narada.worker.policy.v1","status":"ok","server_name":SERVER_NAME,"run_root":run_root(root).to_string_lossy(),"site_root":root.to_string_lossy(),"allowed_roots":allowed_roots.iter().map(|allowed|allowed.to_string_lossy()).collect::<Vec<_>>(),"allowed_runtimes":["narada-agent-runtime-server"],"allowed_authorities":["read","write","command"],"default_cognition":DEFAULT_COGNITION,"native_execution":"rust_authority","secret_projection":"secret_store_reference_only","windows_msvc_environment":{"inherited":true,"automatic_discovery":false,"remediation":"Initialize VsDevCmd or use Developer PowerShell before launching the carrier."}})
}
fn capability_snapshot(
    authority: &str,
    cwd: &Path,
    allowed_roots: &[PathBuf],
    runtime_probe: Option<&Value>,
) -> Value {
    let writable = authority != "read";
    let effective_mode = if writable {
        "workspace_write"
    } else {
        "read_only"
    };
    json!({
        "schema":"narada.worker.capability_snapshot.v1",
        "authority":authority,
        "effective_mode":effective_mode,
        "validated_against_runtime":true,
        "validation_basis":if runtime_probe.is_some(){"scoped_create_read_remove_probe_plus_codex_cli_contract"}else{"native_worker_maps_authority_to_codex_cli_and_approval_contract"},
        "reconciliation":{"authoritative_source":"native_worker_runtime","ambient_profile_is_advisory":true,"conflict_resolution":"effective_mode_and_runtime_probe_win"},
        "runtime_probe":runtime_probe.cloned().unwrap_or(Value::Null),
        "provider_boundary":{"permission_profile":effective_mode,"writable_roots_injected":writable,"source":"native_process_environment_and_codex_cli"},
        "cwd":cwd.to_string_lossy(),
        "allowed_roots":allowed_roots.iter().map(|root|root.to_string_lossy()).collect::<Vec<_>>(),
        "filesystem":{"read":true,"write":writable,"patch":writable},
        "commands":{"execute":true,"write_effects":writable,"direct_file_mutation":writable,"working_directory_scoped":true,"tests_may_write_build_artifacts":writable},
        "approval":{"mode":if writable{"automatic_contained_review"}else{"not_required"},"human_interaction_required":false,"sandbox":if writable{"workspace-write"}else{"read-only"}},
        "tool_bridge":{"kind":"codex_builtin_repo_tools","ordinary_file_mutation_tool":"apply_patch","exact_byte_file_mutation_tool":"bounded_shell_command","mcp_projection":"none","reason":"delegated runs use an isolated config to avoid duplicating the carrier MCP fleet"},
        "workflow_primitives":{"exact_byte_file_lifecycle":{"tool":"bounded_shell_command","expected_commands":1,"operations":["create","read_verify","remove","confirm_absent"],"encoding_must_be_explicit":true,"windows_recipe":"assign literal path and content variables; use IO.File WriteAllBytes and ReadAllBytes; compare hex; delete; test existence; avoid interpolated command strings"}},
        "evaluation_contract":{"schema":"narada.worker.observed_ergonomics.v1","basis":"observed_fresh_run_only","score_5":"no_material_observed_friction","score_reduction_requires":"observed_failure_retry_human_intervention_or_ambiguity_that_changed_execution","automatic_contained_review_is_human_ceremony":false,"speculative_improvements_field":"non_scoring_observations"},
        "refusal_contract":{"schema":"narada.worker.refusal.v1","required_fields":["tool","operation","cwd","target_path","declared_capability","actual_refusal"]}
    })
}
fn scoped_write_probe(cwd: &Path) -> Result<Value, Value> {
    let path = cwd.join(format!(
        ".narada-worker-probe-{}",
        uuid::Uuid::new_v4().simple()
    ));
    fs::write(&path, b"probe").map_err(|failure| {
        error(
            "worker_write_preflight_failed",
            &format!("worker_write_preflight_failed:{failure}"),
        )
    })?;
    let verified = fs::read(&path)
        .map(|value| value == b"probe")
        .unwrap_or(false);
    let removed = fs::remove_file(&path).is_ok() && !path.exists();
    if !verified || !removed {
        return Err(error(
            "worker_write_preflight_failed",
            "worker_write_preflight_failed:verification_or_cleanup",
        ));
    }
    Ok(
        json!({"schema":"narada.worker.runtime_probe.v1","operation":"create_read_remove","status":"passed","cwd":cwd.to_string_lossy(),"cleanup_verified":true}),
    )
}
fn defaults_path(root: &Path) -> PathBuf {
    root.join(".narada/worker-cognition-defaults.json")
}
fn empty_defaults() -> Value {
    json!({"low":{"provider":null,"model":null,"reasoning_effort":null},"medium":{"provider":null,"model":null,"reasoning_effort":null},"high":{"provider":null,"model":null,"reasoning_effort":null}})
}
fn cognition_defaults_for(root: &Path) -> Value {
    read_json(&defaults_path(root))
        .ok()
        .and_then(|v| {
            v.get("effective_cognition_defaults")
                .or_else(|| v.get("defaults"))
                .cloned()
        })
        .unwrap_or_else(empty_defaults)
}
fn cognition_defaults(root: &Path) -> Value {
    json!({"schema":"narada.worker.cognition_defaults.v1","status":"ok","default_cognition":DEFAULT_COGNITION,"defaults":cognition_defaults_for(root),"source":"native_contract","canonical_runtime":"narada-agent-runtime-server uses an immutable invocation plan","native_read_only":true})
}
fn config_resolve(
    args: &Map<String, Value>,
    root: &Path,
    allowed_roots: &[PathBuf],
) -> Result<Value, Value> {
    let resolved_authority = authority(args)?;
    let constraints = args.get("constraints").and_then(Value::as_object);
    let cognition = constraints
        .and_then(|value| value.get("cognition"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or(DEFAULT_COGNITION)
        .to_string();
    if !matches!(cognition.as_str(), "low" | "medium" | "high") {
        return Err(error(
            "worker_cognition_invalid",
            "worker_cognition_invalid",
        ));
    }
    let defaults = cognition_defaults_for(root);
    let selected = defaults.get(&cognition).cloned().unwrap_or(Value::Null);
    let cwd = constraints
        .and_then(|v| v.get("cwd"))
        .and_then(Value::as_str)
        .or_else(|| args.get("cwd").and_then(Value::as_str));
    let cwd = cwd.map(PathBuf::from).unwrap_or_else(|| root.to_path_buf());
    if !allowed_roots.iter().any(|allowed| is_within(&cwd, allowed)) {
        return Err(error(
            "worker_cwd_outside_allowed_roots",
            "worker_cwd_outside_allowed_roots",
        ));
    }
    Ok(json!({
        "schema":"narada.worker.config_resolve.v1",
        "status":"ok",
        "resolved":{
            "cwd":cwd.to_string_lossy(),
            "site_root":root.to_string_lossy(),
            "runtime":"narada-agent-runtime-server",
            "authority":resolved_authority,
            "cognition":cognition,
            "provider":selected.get("provider").cloned().unwrap_or(Value::Null),
            "provider_mode":selected.get("provider").cloned().unwrap_or(Value::Null),
            "model":selected.get("model").cloned().unwrap_or(Value::Null),
            "reasoning_effort":selected.get("reasoning_effort").cloned().unwrap_or(Value::Null),
            "resolution_source":"site_cognition_defaults",
            "canonical_plan_preflight":"deferred_to_worker_run",
            "launch":false
        },
        "capability_snapshot":capability_snapshot(resolved_authority,&cwd,allowed_roots,None),
        "diagnostics":[
            {"name":"native_execution","status":"boundary","message":"worker launch is delegated to the owning worker authority"},
            {"name":"invocation_plan","status":"deferred","message":"canonical provider/model/reasoning binding is finalized by worker_run preflight"}
        ],
        "native_read_only":true
    }))
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
fn wait_for_run(root: &Path, id: &str, timeout_ms: u64) -> Result<(Value, Value), Value> {
    let started = Instant::now();
    let mut run = read_run(root, id)?;
    while run.get("status").and_then(Value::as_str) == Some("running")
        && started.elapsed() < Duration::from_millis(timeout_ms)
    {
        thread::sleep(Duration::from_millis(100).min(Duration::from_millis(
            timeout_ms.saturating_sub(started.elapsed().as_millis() as u64),
        )));
        run = read_run(root, id)?;
    }
    let running = run.get("status").and_then(Value::as_str) == Some("running");
    let waited_ms = started.elapsed().as_millis() as u64;
    let wait = json!({"status":if running{"timed_out"}else{"finished"},"waited":waited_ms>0,"waited_ms":waited_ms,"timeout_ms":timeout_ms,"native_execution":"bounded_state_poll"});
    Ok((run, wait))
}
fn run_wait(args: &Map<String, Value>, root: &Path) -> Result<Value, Value> {
    let id = run_id(args)?;
    let timeout_ms = args
        .get("timeout_ms")
        .and_then(Value::as_u64)
        .unwrap_or(30_000)
        .min(300_000);
    let (run, wait) = wait_for_run(root, &id, timeout_ms)?;
    Ok(
        json!({"schema":"narada.worker.run_wait.v1","status":"ok","wait":wait,"run":compact_run(&run),"native_read_only":true}),
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
    json!({"schema":"narada.worker.operator_affordances.v1","status":"ok","read_tools":READ_TOOLS.iter().map(|(n,_)|*n).collect::<Vec<_>>(),"mutation_tools":MUTATING_TOOLS,"native_read_only":true,"execution_authority":"rust"})
}
fn compact_run(run: &Value) -> Value {
    let o = run.as_object().cloned().unwrap_or_default();
    json!({"run_id":o.get("run_id"),"status":o.get("status"),"completion_state":o.get("completion_state"),"authority":o.get("authority"),"resolved_invocation":o.get("resolved_invocation"),"capability_snapshot":o.get("capability_snapshot"),"worker_session_id":o.get("worker_session_id"),"started_at":o.get("timing").and_then(|v|v.get("started_at")),"finished_at":o.get("timing").and_then(|v|v.get("finished_at")),"duration_ms":o.get("timing").and_then(|v|v.get("duration_ms")),"summary":o.get("summary").or_else(||o.get("last_message")),"summary_preview":o.get("summary").or_else(||o.get("last_message")),"error":o.get("error"),"error_preview":o.get("error"),"failure":o.get("failure"),"updated_at":o.get("updated_at").or_else(||o.get("timing").and_then(|v|v.get("finished_at")))})
}

fn resolved_invocation(
    cognition: &str,
    plan_ref: &str,
    provider_mode: &str,
    provider_model: &str,
    preflight_evidence_ref: &str,
    reasoning_effort: Option<&str>,
    provider_binding: Option<&Value>,
    provider_binding_path: Option<&Path>,
) -> Value {
    json!({
        "cognition": cognition,
        "invocation_plan_ref": plan_ref,
        "provider_mode": provider_mode,
        "provider_model": provider_model,
        "reasoning_effort": reasoning_effort,
        "provider_binding": provider_binding,
        "provider_binding_path": provider_binding_path.map(|path| path.to_string_lossy().to_string()),
        "preflight_evidence_ref": preflight_evidence_ref,
        "resolution_source":"worker_intelligence_preflight"
    })
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

fn provider_registry_candidates(root: &Path) -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    if let Some(path) = std::env::var_os("NARADA_PROVIDER_REGISTRY_PATH") {
        candidates.push(PathBuf::from(path));
    }
    candidates.push(root.join(".narada/provider-registry.json"));
    let source_root = narada_source_root(root);
    candidates.push(source_root.join(
        "narada/packages/invokable-intelligence-management/assets/provider-registry.bootstrap.json",
    ));
    candidates
}

fn provider_models_from_registry(value: &Value) -> BTreeMap<String, Vec<String>> {
    let mut result = BTreeMap::new();
    let Some(providers) = value.get("providers").and_then(Value::as_object) else {
        return result;
    };
    for (provider, record) in providers {
        let mut models = record
            .get("available_models")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .map(str::to_string)
            .collect::<Vec<_>>();
        if models.is_empty() {
            if let Some(model_map) = record.get("models").and_then(Value::as_object) {
                models.extend(model_map.keys().cloned());
            }
        }
        models.sort();
        models.dedup();
        if !models.is_empty() {
            result.insert(provider.clone(), models);
        }
    }
    result
}

fn canonical_provider_models(root: &Path) -> Result<BTreeMap<String, Vec<String>>, Value> {
    for path in provider_registry_candidates(root) {
        if !path.is_file() {
            continue;
        }
        let value = read_json(&path).map_err(|_| {
            error(
                "worker_provider_registry_invalid",
                "worker_provider_registry_invalid",
            )
        })?;
        let models = provider_models_from_registry(&value);
        if models.is_empty() {
            return Err(error(
                "worker_provider_registry_invalid",
                "worker_provider_registry_invalid",
            ));
        }
        return Ok(models);
    }
    Err(error(
        "worker_provider_registry_unavailable",
        "worker_provider_registry_unavailable",
    ))
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
    let path = defaults_path(root);
    let mut record = read_json(&path).unwrap_or_else(|_| json!({"schema":"narada.worker.cognition_defaults.v1","version":0,"provider_cognition_defaults":{},"effective_cognition_defaults":empty_defaults()}));
    let provider_models = canonical_provider_models(root)?;
    let allowed_models = provider_models.get(provider).ok_or_else(|| {
        error(
            "worker_cognition_provider_not_allowed",
            "worker_cognition_provider_not_allowed",
        )
    })?;
    if !allowed_models.iter().any(|candidate| candidate == model) {
        return Err(error(
            "worker_cognition_model_not_allowed",
            "worker_cognition_model_not_allowed",
        ));
    }
    record["version"] = json!(record.get("version").and_then(Value::as_u64).unwrap_or(0) + 1);
    record["updated_at"] = json!(now());
    record["updated_by"] = args.get("actor").cloned().unwrap_or(Value::Null);
    record["provider_cognition_defaults"][provider][cognition] =
        json!({"model":model,"reasoning_effort":effort});
    record["effective_cognition_defaults"][cognition] =
        json!({"provider":provider,"model":model,"reasoning_effort":effort});
    write_json_atomic(&path, &record)?;
    Ok(
        json!({"schema":"narada.worker.cognition_defaults.v1","status":"updated","cognition":cognition,"default":record["effective_cognition_defaults"][cognition],"defaults":record["effective_cognition_defaults"],"source":"native_rust_authority"}),
    )
}

fn narada_source_root(root: &Path) -> PathBuf {
    if let Some(path) = std::env::var_os("NARADA_SRC_ROOT").map(PathBuf::from) {
        return path;
    }
    let parent = root.parent().unwrap_or(root);
    let conventional = parent.join("src");
    if conventional.join("narada").is_dir() {
        return conventional;
    }
    if parent.join("narada").is_dir() {
        return parent.to_path_buf();
    }
    conventional
}
fn runtime_command(root: &Path) -> Result<PathBuf, Value> {
    if let Some(path) = std::env::var_os("NARADA_AGENT_RUNTIME_SERVER_NATIVE")
        .map(PathBuf::from)
        .filter(|p| p.is_file())
    {
        return Ok(path);
    }
    let src = narada_source_root(root);
    let candidates=[src.join("narada/packages/agent-runtime-server/native/target/release/narada-agent-runtime-server-rust.exe"),src.join("narada/packages/agent-runtime-server/native/target/release/narada-agent-runtime-server-rust")];
    candidates.into_iter().find(|p| p.is_file()).ok_or_else(|| {
        error(
            "worker_runtime_unavailable",
            "worker_runtime_unavailable:narada-agent-runtime-server-rust",
        )
    })
}
fn preflight_command(root: &Path) -> Result<PathBuf, Value> {
    if let Some(path) = std::env::var_os("NARADA_INTELLIGENCE_PREFLIGHT_NATIVE")
        .map(PathBuf::from)
        .filter(|path| path.is_file())
    {
        return Ok(path);
    }
    let src = narada_source_root(root);
    [
        src.join("narada/packages/invokable-intelligence-runtime/native/target/release/narada-intelligence-preflight.exe"),
        src.join("narada/packages/invokable-intelligence-runtime/native/target/release/narada-intelligence-preflight"),
    ]
    .into_iter()
    .find(|path| path.is_file())
    .ok_or_else(|| error("worker_intelligence_preflight_unavailable", "worker_intelligence_preflight_unavailable"))
}
fn admitted_plan_binding(
    admission: &Value,
) -> Result<(String, String, String, String, Option<Value>), Value> {
    let plan_ref = admission
        .get("plan_ref")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            error(
                "worker_canonical_invocation_plan_invalid",
                "worker_canonical_invocation_plan_invalid",
            )
        })?;
    let provider = admission
        .pointer("/selected/inference_provider/id")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            error(
                "worker_canonical_invocation_provider_missing",
                "worker_canonical_invocation_provider_missing",
            )
        })?;
    let provider_binding = admission
        .get("provider_binding")
        .filter(|value| !value.is_null())
        .cloned();
    let mode = match provider {
        "inference-provider:codex-subscription" => "codex-subscription",
        "inference-provider:deepseek-api" | "inference-provider:openrouter-api" => {
            validate_native_provider_binding(provider_binding.as_ref())?;
            provider
                .strip_prefix("inference-provider:")
                .unwrap_or(provider)
        }
        _ => {
            return Err(error(
                "worker_native_provider_unsupported",
                "worker_native_provider_unsupported",
            ))
        }
    };
    let model = admission
        .pointer("/selected/model/id")
        .and_then(Value::as_str)
        .and_then(|value| value.strip_prefix("model:"))
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            error(
                "worker_canonical_invocation_model_missing",
                "worker_canonical_invocation_model_missing",
            )
        })?;
    let evidence_ref = admission
        .get("evidence_ref")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    Ok((
        plan_ref.to_string(),
        mode.to_string(),
        model.to_string(),
        evidence_ref,
        provider_binding,
    ))
}

fn validate_native_provider_binding(binding: Option<&Value>) -> Result<(), Value> {
    let binding = binding.ok_or_else(|| {
        error(
            "worker_native_provider_binding_missing",
            "worker_native_provider_binding_missing",
        )
    })?;
    let provider = binding
        .get("provider")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let expected_secret_ref = match provider {
        "deepseek-api" => "narada/provider/deepseek-api/api-key",
        "openrouter-api" => "narada/provider/openrouter-api/api-key",
        _ => {
            return Err(error(
                "worker_native_provider_binding_invalid",
                "worker_native_provider_binding_invalid",
            ))
        }
    };
    let endpoint = binding
        .get("endpoint")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let endpoint_ok = match provider {
        "deepseek-api" => endpoint.starts_with("https://api.deepseek.com/"),
        "openrouter-api" => endpoint.starts_with("https://openrouter.ai/"),
        _ => false,
    };
    if binding.get("schema").and_then(Value::as_str) != Some("narada.native.provider_binding.v1")
        || binding.get("protocol").and_then(Value::as_str) != Some("openai/chat-completions/1")
        || binding.get("credential_secret_ref").and_then(Value::as_str) != Some(expected_secret_ref)
        || !endpoint_ok
        || binding
            .get("model")
            .and_then(Value::as_str)
            .is_none_or(str::is_empty)
    {
        return Err(error(
            "worker_native_provider_binding_invalid",
            "worker_native_provider_binding_invalid",
        ));
    }
    Ok(())
}
fn resolve_intelligence_context_path(
    root: &Path,
    explicit_context: Option<PathBuf>,
    user_site_root: Option<PathBuf>,
    home: Option<PathBuf>,
) -> PathBuf {
    if let Some(path) = explicit_context.filter(|path| path.is_file()) {
        return path;
    }
    if let Some(path) = user_site_root
        .map(|path| path.join(".narada/intelligence-launch-context.json"))
        .filter(|path| path.is_file())
    {
        return path;
    }
    let local = root.join(".narada/intelligence-launch-context.json");
    if local.is_file() {
        return local;
    }
    home.map(|home| home.join("Narada/.narada/intelligence-launch-context.json"))
        .filter(|path| path.is_file())
        .unwrap_or(local)
}
fn intelligence_context_path(root: &Path) -> PathBuf {
    resolve_intelligence_context_path(
        root,
        std::env::var_os("NARADA_INTELLIGENCE_CONTEXT_PATH").map(PathBuf::from),
        std::env::var_os("NARADA_USER_SITE_ROOT").map(PathBuf::from),
        std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" }).map(PathBuf::from),
    )
}
fn invocation_plan_binding(
    root: &Path,
    requested_plan_ref: Option<&str>,
    cognition: Option<&str>,
) -> Result<
    (
        String,
        String,
        String,
        String,
        Option<String>,
        Option<Value>,
    ),
    Value,
> {
    let context_path = intelligence_context_path(root);
    let context = read_json(&context_path).map_err(|_| {
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
    let context_site_root = context_path.parent().and_then(Path::parent).unwrap_or(root);
    let registry = if registry.is_absolute() {
        registry
    } else {
        context_site_root.join(registry)
    };
    let principal = context
        .get("principal_id")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            error(
                "worker_intelligence_principal_required",
                "worker_intelligence_principal_required",
            )
        })?;
    let defaults_path = root.join(".narada/worker-cognition-defaults.json");
    let request = json!({
        "schema":"narada.invokable-intelligence.preflight-request.v1",
        "intent_id":"",
        "purpose":"local-agent-runtime",
        "principal":principal,
        "requested_plan_id":requested_plan_ref,
        "evaluated_at":now(),
        "clock_authority_ref":"execution-site-clock:worker-delegation",
        "mode":"immediate",
        "cognition":cognition,
        "cognition_defaults_path":if cognition.is_some(){json!(defaults_path)}else{Value::Null}
    });
    let executable = preflight_command(root)?;
    let mut child = Command::new(executable)
        .args(["--registry", &registry.to_string_lossy()])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|_| {
            error(
                "worker_intelligence_preflight_launch_failed",
                "worker_intelligence_preflight_launch_failed",
            )
        })?;
    if let Some(mut stdin) = child.stdin.take() {
        writeln!(stdin, "{request}").map_err(|_| {
            error(
                "worker_intelligence_preflight_write_failed",
                "worker_intelligence_preflight_write_failed",
            )
        })?;
    }
    let output = child.wait_with_output().map_err(|_| {
        error(
            "worker_intelligence_preflight_wait_failed",
            "worker_intelligence_preflight_wait_failed",
        )
    })?;
    if output.stdout.len() > MAX_FILE_BYTES {
        return Err(error(
            "worker_intelligence_preflight_response_too_large",
            "worker_intelligence_preflight_response_too_large",
        ));
    }
    let admission: Value = serde_json::from_slice(&output.stdout).map_err(|_| {
        error(
            "worker_intelligence_preflight_response_invalid",
            "worker_intelligence_preflight_response_invalid",
        )
    })?;
    if !output.status.success()
        || admission.get("status").and_then(Value::as_str) != Some("admitted")
    {
        return Err(
            json!({"schema":"narada.worker.error.v1","code":"worker_intelligence_preflight_refused","message":"worker_intelligence_preflight_refused","preflight":admission}),
        );
    }
    let (plan_ref, mode, model, evidence_ref, provider_binding) =
        admitted_plan_binding(&admission)?;
    let reasoning_effort = admission
        .pointer("/options/reasoning_effort")
        .and_then(Value::as_str)
        .map(str::to_string);
    Ok((
        plan_ref,
        mode,
        model,
        evidence_ref,
        reasoning_effort,
        provider_binding,
    ))
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
    allowed_roots: &[PathBuf],
    resume: Option<String>,
    tool_name: &str,
) -> Result<Value, Value> {
    let prompt = format!("{READ_ONLY_COMMAND_CONTRACT}\n\n{}", instruction(args)?);
    let auth = authority(args)?.to_string();
    let constraints = args.get("constraints").and_then(Value::as_object);
    let max_run_ms = constraints
        .and_then(|value| value.get("max_run_ms"))
        .and_then(Value::as_u64)
        .unwrap_or(300_000)
        .clamp(1, 1_800_000);
    let wait_for_completion = constraints
        .and_then(|value| value.get("wait_for_completion"))
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let wait_timeout_ms = constraints
        .and_then(|value| value.get("wait_timeout_ms"))
        .and_then(Value::as_u64)
        .unwrap_or(30_000)
        .min(300_000);
    for key in ["provider"] {
        if constraints.and_then(|value| value.get(key)).is_some() {
            return Err(error(
                "worker_canonical_invocation_plan_override_rejected",
                "worker_canonical_invocation_plan_override_rejected",
            ));
        }
    }
    let cognition = constraints
        .and_then(|value| value.get("cognition"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or(DEFAULT_COGNITION)
        .to_string();
    if !matches!(cognition.as_str(), "low" | "medium" | "high") {
        return Err(error(
            "worker_cognition_invalid",
            "worker_cognition_invalid",
        ));
    }
    let requested_plan_ref = constraints
        .and_then(|value| value.get("invocation_plan_ref"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .or_else(|| {
            std::env::var("NARADA_INTELLIGENCE_PLAN_REF")
                .ok()
                .filter(|value| !value.trim().is_empty())
        });
    if requested_plan_ref.as_deref().is_some_and(|plan_ref| {
        !plan_ref.starts_with("plan:")
            || !plan_ref[5..].chars().all(|character| {
                character.is_ascii_alphanumeric() || matches!(character, '.' | '_' | ':' | '-')
            })
    }) {
        return Err(error(
            "worker_canonical_invocation_plan_invalid",
            "worker_canonical_invocation_plan_invalid",
        ));
    }
    let (
        plan_ref,
        provider_mode,
        provider_model,
        preflight_evidence_ref,
        reasoning_effort,
        provider_binding,
    ) = invocation_plan_binding(root, requested_plan_ref.as_deref(), Some(&cognition))?;
    let cwd = constraints
        .and_then(|v| v.get("cwd"))
        .and_then(Value::as_str)
        .map(PathBuf::from)
        .unwrap_or_else(|| root.to_path_buf());
    if !allowed_roots.iter().any(|allowed| is_within(&cwd, allowed)) {
        return Err(error(
            "worker_cwd_outside_allowed_roots",
            "worker_cwd_outside_allowed_roots",
        ));
    }
    let runtime = runtime_command(root)?;
    let runtime_probe = if auth == "read" {
        None
    } else {
        Some(scoped_write_probe(&cwd)?)
    };
    let preflight = preflight_paths(constraints, &cwd, allowed_roots)?;
    let mut capabilities = capability_snapshot(&auth, &cwd, allowed_roots, runtime_probe.as_ref());
    capabilities["preflight"] = preflight;
    let id = format!("run-{}", uuid::Uuid::new_v4().simple());
    let session = resume.clone().unwrap_or_else(|| id.clone());
    let dir = run_root(root).join(&id);
    fs::create_dir_all(&dir)
        .map_err(|_| error("worker_run_create_failed", "worker_run_create_failed"))?;
    let provider_binding_path = provider_binding
        .as_ref()
        .map(|_| dir.join("provider-binding.json"));
    if let (Some(binding), Some(path)) = (provider_binding.as_ref(), provider_binding_path.as_ref())
    {
        write_json_atomic(path, binding)?;
    }
    let resolved_invocation = resolved_invocation(
        &cognition,
        &plan_ref,
        &provider_mode,
        &provider_model,
        &preflight_evidence_ref,
        reasoning_effort.as_deref(),
        provider_binding.as_ref(),
        provider_binding_path.as_deref(),
    );
    let started = now();
    let request = json!({"schema":"narada.worker.request.v1","run_id":id,"origin_tool":tool_name,"intent":args.get("intent"),"constraints":args.get("constraints"),"resume_worker_session_id":resume,"capability_snapshot":capabilities.clone(),"invocation_plan_ref":plan_ref,"preflight_evidence_ref":preflight_evidence_ref,"resolved_invocation":resolved_invocation.clone()});
    write_json_atomic(&dir.join("request.json"), &request)?;
    fs::write(dir.join("worker_prompt.txt"), &prompt)
        .map_err(|_| error("worker_write_failed", "worker_write_failed"))?;
    let running = json!({"schema":"narada.worker.run.v1","run_id":id,"status":"running","completion_state":"pending","runtime":"narada-agent-runtime-server","authority":auth,"resolved_invocation":resolved_invocation.clone(),"capability_snapshot":capabilities.clone(),"worker_session_id":session,"origin_tool":tool_name,"pid":null,"summary":null,"error":null,"timing":{"started_at":started,"finished_at":null,"duration_ms":null},"artifacts":{"request":dir.join("request.json").to_string_lossy(),"events":dir.join("events.jsonl").to_string_lossy(),"diagnostic":dir.join("diagnostic.log").to_string_lossy(),"last_message":dir.join("last_message.json").to_string_lossy()}});
    write_json_atomic(&dir.join("result.json"), &running)?;
    let root_owned = root.to_path_buf();
    let dir_owned = dir.clone();
    let id_owned = id.clone();
    let session_owned = session.clone();
    let resume_owned = resume.clone();
    let auth_owned = auth.clone();
    let allowed_roots_owned = allowed_roots.to_vec();
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
                cognition,
                resolved_invocation,
                plan_ref,
                provider_mode,
                provider_model,
                reasoning_effort,
                provider_binding_path,
                allowed_roots_owned,
                max_run_ms,
                format!("Effective mode: {}. This reconciled state is injected at the provider process boundary through the permission profile, CLI sandbox, and writable-root arguments; ambient labels are advisory. CWD: {}. Writable roots: {}. Scoped create/read/remove preflight: {}. Command write effects: {}. First-class exact-byte lifecycle: one bounded shell command with explicit encoding for create/read-verify/remove/confirm-absent. On Windows assign literal path/content variables, use IO.File WriteAllBytes/ReadAllBytes, compare hex, delete, and test existence; avoid interpolated command strings. Use apply_patch for ordinary edits. Read-only command policy: issue one executable with literal arguments per probe; do not combine probes with &&, ;, pipes, redirection, $(), backticks, or generated scripts. Use separate bounded commands and report each result. For non-ASCII text, set explicit UTF-8 output before reading. Carrier MCP projection: none. On refusal return narada.worker.refusal.v1 with tool, operation, cwd, target_path, declared_capability, actual_refusal. Ergonomics ratings use narada.worker.observed_ergonomics.v1: lower a score only for observed failure, retry, human intervention, or ambiguity that changed execution; automatic contained review requires no human interaction and is not ceremony; put hypothetical improvements in non_scoring_observations.\n\nTask:\n{prompt}", capabilities["effective_mode"].as_str().unwrap_or("unknown"), capabilities["cwd"].as_str().unwrap_or("unknown"), capabilities["allowed_roots"].as_array().map(|roots| roots.iter().filter_map(Value::as_str).collect::<Vec<_>>().join(", ")).unwrap_or_default(), capabilities["runtime_probe"]["status"].as_str().unwrap_or("not_required"), capabilities["commands"]["write_effects"].as_bool().unwrap_or(false)),
            )
        })
        .map_err(|_| error("worker_launch_failed", "worker_launch_failed"))?;
    if wait_for_completion {
        let (mut run, wait) = wait_for_run(root, &id, wait_timeout_ms)?;
        if let Some(object) = run.as_object_mut() {
            object.insert("wait".into(), wait);
        }
        return Ok(run);
    }
    Ok(running)
}
fn worker_edit(
    args: &Map<String, Value>,
    root: &Path,
    allowed_roots: &[PathBuf],
) -> Result<Value, Value> {
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
        allowed_roots,
        None,
        "worker_edit",
    )
}
fn worker_resume(
    args: &Map<String, Value>,
    root: &Path,
    allowed_roots: &[PathBuf],
) -> Result<Value, Value> {
    let session =
        required_string(args, "worker_session_id", "worker_session_id_required")?.to_string();
    worker_run(args, root, allowed_roots, Some(session), "worker_resume")
}
fn worker_run_batch(
    args: &Map<String, Value>,
    root: &Path,
    allowed_roots: &[PathBuf],
) -> Result<Value, Value> {
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
            .and_then(|v| worker_run(v, root, allowed_roots, None, "worker_run_batch"))
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
fn repair_mojibake(text: &str) -> String {
    text.replace("Â·", "·")
        .replace("â€“", "–")
        .replace("â€”", "—")
        .replace("â€œ", "“")
        .replace("â€\u{009d}", "”")
        .replace("â€˜", "‘")
        .replace("â€™", "’")
        .replace("â€¦", "…")
        .replace("Â ", " ")
}

fn timeout_failure(run_id: &str, max_run_ms: u64, elapsed_ms: u64) -> Value {
    json!({
        "schema":"narada.worker.failure.v1",
        "code":"worker_runtime_timed_out",
        "run_id":run_id,
        "max_run_ms":max_run_ms,
        "elapsed_ms":elapsed_ms,
        "remediation":"Increase constraints.max_run_ms or inspect the worker runtime before retrying."
    })
}

fn event_text(event: &Value) -> Option<String> {
    for key in ["content", "message", "text", "summary"] {
        if let Some(value) = event
            .get(key)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|v| !v.is_empty())
        {
            return Some(repair_mojibake(value));
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
                    .map(repair_mojibake)
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
    cognition: String,
    resolved_invocation: Value,
    plan_ref: String,
    provider_mode: String,
    provider_model: String,
    reasoning_effort: Option<String>,
    provider_binding_path: Option<PathBuf>,
    allowed_roots: Vec<PathBuf>,
    max_run_ms: u64,
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
        .env(
            "NARADA_NATIVE_CODEX_SANDBOX",
            if authority == "read" {
                "read-only"
            } else {
                "workspace-write"
            },
        )
        .env(
            "NARADA_NATIVE_CODEX_WRITABLE_ROOTS",
            serde_json::to_string(
                &allowed_roots
                    .iter()
                    .map(|root| root.to_string_lossy().to_string())
                    .collect::<Vec<_>>(),
            )
            .unwrap_or_else(|_| "[]".to_string()),
        )
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(binding_path) = provider_binding_path {
        command.env("NARADA_NATIVE_PROVIDER_BINDING_PATH", binding_path);
    }
    command.env("NARADA_NATIVE_CODEX_MODEL", provider_model);
    if let Some(reasoning_effort) = reasoning_effort {
        command.env("NARADA_NATIVE_CODEX_REASONING_EFFORT", reasoning_effort);
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
            let failed = json!({"schema":"narada.worker.run.v1","run_id":id,"status":"failed","completion_state":"absent","runtime":"narada-agent-runtime-server","authority":authority,"cognition":cognition,"resolved_invocation":resolved_invocation,"worker_session_id":session,"summary":null,"error":format!("worker_launch_failed:{err}"),"timing":{"started_at":now(),"finished_at":now(),"duration_ms":0}});
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
        let mut failure = Value::Null;
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
                if started.elapsed() >= Duration::from_millis(max_run_ms) {
                    let elapsed_ms = started.elapsed().as_millis() as u64;
                    failure = timeout_failure(&id, max_run_ms, elapsed_ms);
                    runtime_error = Some(format!(
                        "worker_runtime_timed_out:max_run_ms={max_run_ms}:elapsed_ms={elapsed_ms}"
                    ));
                    if let Some(file) = events.as_mut() {
                        let _ = writeln!(
                            file,
                            "{}",
                            json!({"schema":"narada.worker.event.v1","event":"worker_runtime_timed_out","run_id":id,"elapsed_ms":elapsed_ms,"max_run_ms":max_run_ms,"failure":failure,"remediation":"Increase constraints.max_run_ms or inspect the worker runtime before retrying."})
                        );
                    }
                    let _ = child.kill();
                    break;
                }
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
        let snapshot = read_json(&dir.join("request.json"))
            .ok()
            .and_then(|request| request.get("capability_snapshot").cloned())
            .unwrap_or(Value::Null);
        let elapsed_ms = started.elapsed().as_millis() as u64;
        let final_error = runtime_error.or_else(|| {
            if successful {
                None
            } else {
                Some(format!("worker_runtime_exit:{:?}", status.and_then(|v| v.code())))
            }
        });
        let payload = json!({"schema":"narada.worker.run.v1","run_id":id,"status":if successful{"completed"}else{"failed"},"completion_state":if assistant.is_some(){"complete"}else{"absent"},"runtime":"narada-agent-runtime-server","authority":authority,"cognition":cognition,"resolved_invocation":resolved_invocation,"capability_snapshot":snapshot,"worker_session_id":provider_session.unwrap_or(session),"pid":child.id(),"summary":assistant,"error":final_error,"failure":failure,"timing":{"started_at":Value::Null,"finished_at":finished,"duration_ms":elapsed_ms},"artifacts":{"request":dir.join("request.json").to_string_lossy(),"events":events_path.to_string_lossy(),"diagnostic":diagnostic_path.to_string_lossy(),"last_message":dir.join("last_message.json").to_string_lossy()}});
        let _ = write_json_atomic(&result_path, &payload);
    }
}

fn error(code: &str, message: &str) -> Value {
    json!({"schema":"narada.worker.error.v1","code":code,"message":message})
}
fn input_schema(name: &str) -> Value {
    let short_string = || json!({"type":"string","minLength":1,"maxLength":512});
    let run_id =
        || json!({"type":"string","minLength":5,"maxLength":160,"pattern":"^run-[A-Za-z0-9_-]+$"});
    let run_ids = || json!({"type":"array","minItems":1,"maxItems":50,"items":run_id()});
    let intent = || {
        json!({
            "type":"object",
            "properties":{
                "instruction":{"type":"string","minLength":1,"maxLength":65536},
                "task":{"type":"string","minLength":1,"maxLength":65536},
                "goal":{"type":"string","minLength":1,"maxLength":65536},
                "summary":{"type":"string","minLength":1,"maxLength":65536},
                "mode":short_string()
            },
            "additionalProperties":false,
            "anyOf":[{"required":["instruction"]},{"required":["task"]},{"required":["goal"]},{"required":["summary"]}]
        })
    };
    let constraints = || {
        json!({
            "type":"object",
            "properties":{
                "authority":{"type":"string","enum":["read","write","command"]},
                "cognition":{"type":"string","enum":["low","medium","high"],"default":"low"},
                "cwd":{"type":"string","minLength":1,"maxLength":4096},
                "preflight_paths":{"type":"array","maxItems":64,"items":{"type":"object","properties":{"path":{"type":"string","minLength":1,"maxLength":4096},"access":{"type":"string","enum":["read","write","create"],"default":"read"}},"required":["path"],"additionalProperties":false}},
                "invocation_plan_ref":{"type":"string","minLength":6,"maxLength":512,"pattern":"^plan:[A-Za-z0-9._:-]+$"},
                "max_run_ms":{"type":"integer","minimum":1,"maximum":1800000,"default":300000,"description":"Hard worker runtime deadline enforced by the native authority."},
                "wait_for_completion":{"type":"boolean","default":false,"description":"Return after bounded child completion polling when true; false returns the accepted running record immediately."},
                "wait_timeout_ms":{"type":"integer","minimum":0,"maximum":300000,"default":30000,"description":"Maximum inline completion wait when wait_for_completion is true."}
            },
            "additionalProperties":false
        })
    };
    let run_request = || {
        json!({
            "type":"object",
            "properties":{"intent":intent(),"constraints":constraints()},
            "required":["intent"],
            "additionalProperties":false
        })
    };
    match name {
        "worker_guidance" => {
            json!({"type":"object","properties":{"workflow":short_string(),"tool":short_string()},"additionalProperties":false})
        }
        "worker_policy_inspect"
        | "worker_cognition_defaults_inspect"
        | "worker_operator_affordances" => json!({"type":"object","additionalProperties":false}),
        "worker_config_resolve" => {
            json!({"type":"object","properties":{"cwd":{"type":"string","minLength":1,"maxLength":4096},"constraints":constraints()},"additionalProperties":false})
        }
        "worker_run_status" => {
            json!({"type":"object","properties":{"run_id":run_id()},"required":["run_id"],"additionalProperties":false})
        }
        "worker_run_wait" => {
            json!({"type":"object","properties":{"run_id":run_id(),"timeout_ms":{"type":"integer","minimum":0,"maximum":300000,"default":30000,"description":"Maximum bounded state-file polling interval."}},"required":["run_id"],"additionalProperties":false})
        }
        "worker_runs_list" => {
            json!({"type":"object","properties":{"limit":{"type":"integer","minimum":1,"maximum":200},"include_running":{"type":"boolean"},"include_completed":{"type":"boolean"}},"additionalProperties":false})
        }
        "worker_run_wait_batch" | "worker_runs_synthesize" => {
            json!({"type":"object","properties":{"run_ids":run_ids()},"required":["run_ids"],"additionalProperties":false})
        }
        "worker_dashboard_describe" => {
            json!({"type":"object","properties":{"mode":{"type":"string","enum":["all_active","single_run"]},"run_id":run_id(),"include_terminal":{"type":"boolean"},"limit":{"type":"integer","minimum":1,"maximum":200}},"additionalProperties":false})
        }
        "worker_output_show" => {
            json!({"type":"object","properties":{"ref":{"type":"string","minLength":1,"maxLength":512},"output_ref":{"type":"string","minLength":1,"maxLength":512},"offset":{"type":"integer","minimum":0,"maximum":256000},"limit":{"type":"integer","minimum":1,"maximum":256000}},"anyOf":[{"required":["ref"]},{"required":["output_ref"]}],"additionalProperties":false})
        }
        "worker_cognition_defaults_update" => {
            json!({"type":"object","properties":{"provider":short_string(),"cognition":{"type":"string","enum":["low","medium","high"]},"model":short_string(),"reasoning_effort":short_string(),"actor":short_string()},"required":["provider","cognition","model","reasoning_effort"],"additionalProperties":false})
        }
        "worker_run" => run_request(),
        "worker_edit" => {
            json!({"type":"object","properties":{"instruction":{"type":"string","minLength":1,"maxLength":65536},"cwd":{"type":"string","minLength":1,"maxLength":4096},"invocation_plan_ref":{"type":"string","minLength":6,"maxLength":512,"pattern":"^plan:[A-Za-z0-9._:-]+$"},"constraints":constraints()},"required":["instruction"],"additionalProperties":false})
        }
        "worker_resume" => {
            json!({"type":"object","properties":{"worker_session_id":{"type":"string","minLength":1,"maxLength":512},"intent":intent(),"constraints":constraints()},"required":["worker_session_id","intent"],"additionalProperties":false})
        }
        "worker_run_reap" => {
            json!({"type":"object","properties":{"run_id":run_id(),"reason":{"type":"string","minLength":1,"maxLength":2048},"force":{"type":"boolean"}},"required":["run_id","reason","force"],"additionalProperties":false})
        }
        "worker_run_batch" => {
            json!({"type":"object","properties":{"requests":{"type":"array","minItems":1,"maxItems":50,"items":run_request()}},"required":["requests"],"additionalProperties":false})
        }
        _ => json!({"type":"object","additionalProperties":false}),
    }
}
fn tool(name: &str, description: &str, schema: Value, read_only: bool) -> Value {
    json!({"name":name,"description":description,"annotations":{"title":name,"readOnlyHint":read_only,"destructiveHint":!read_only,"idempotentHint":read_only,"openWorldHint":false},"inputSchema":schema,"outputSchema":{"type":"object","additionalProperties":true}})
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn worker_run_schema_declares_low_cognition_default() {
        assert_eq!(
            input_schema("worker_run")["properties"]["constraints"]["properties"]["cognition"]
                ["default"],
            "low"
        );
        assert_eq!(
            cognition_defaults(Path::new("."))["default_cognition"],
            "low"
        );
        assert_eq!(guidance(&Map::new())["cognition"]["default"], "low");
        assert_eq!(
            input_schema("worker_run")["properties"]["constraints"]["properties"]
                ["wait_for_completion"]["default"],
            false
        );
        assert_eq!(
            input_schema("worker_run")["properties"]["constraints"]["properties"]
                ["wait_timeout_ms"]["default"],
            30_000
        );
    }

    #[test]
    fn policy_declares_secret_store_reference_projection() {
        let value = policy(Path::new("."), &[PathBuf::from(".")]);
        assert_eq!(value["secret_projection"], "secret_store_reference_only");
        assert!(guidance(&Map::new())["boundaries"][1]
            .as_str()
            .is_some_and(|text| text.contains("SecretStore-referenced")));
    }

    #[test]
    fn config_resolve_reports_site_cognition_mapping_without_launching() {
        let root =
            std::env::temp_dir().join(format!("narada-worker-config-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(root.join(".narada")).expect("site root");
        fs::write(
            defaults_path(&root),
            serde_json::to_vec(&json!({"effective_cognition_defaults":{"low":{"provider":"codex-subscription","model":"gpt-5.6-luna","reasoning_effort":"max"},"medium":{"provider":"codex-subscription","model":"gpt-5.6-sol","reasoning_effort":"low"},"high":{"provider":"codex-subscription","model":"gpt-5.6-sol","reasoning_effort":"max"}}})).expect("encode defaults"),
        ).expect("defaults");
        let resolved = config_resolve(
            &json!({"constraints":{"cognition":"medium"}})
                .as_object()
                .unwrap(),
            &root,
            std::slice::from_ref(&root),
        )
        .expect("resolve");
        assert_eq!(resolved["resolved"]["cognition"], "medium");
        assert_eq!(resolved["resolved"]["provider_mode"], "codex-subscription");
        assert_eq!(resolved["resolved"]["model"], "gpt-5.6-sol");
        assert_eq!(resolved["resolved"]["reasoning_effort"], "low");
        assert_eq!(resolved["resolved"]["launch"], false);
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn compact_run_preserves_effective_invocation_provenance() {
        let compact = compact_run(
            &json!({"run_id":"run-test","status":"completed","resolved_invocation":{"cognition":"low","provider_model":"gpt-5.6-luna"}}),
        );
        assert_eq!(compact["resolved_invocation"]["cognition"], "low");
        assert_eq!(
            compact["resolved_invocation"]["provider_model"],
            "gpt-5.6-luna"
        );
    }

    #[test]
    fn compact_run_surfaces_failure_and_elapsed_time() {
        let compact = compact_run(&json!({
            "run_id":"run-timeout",
            "status":"failed",
            "error":"worker_runtime_timed_out:max_run_ms=120000:elapsed_ms=120004",
            "failure":{"code":"worker_runtime_timed_out","elapsed_ms":120004},
            "timing":{"duration_ms":120004}
        }));
        assert_eq!(compact["error"], "worker_runtime_timed_out:max_run_ms=120000:elapsed_ms=120004");
        assert_eq!(compact["duration_ms"], 120004);
        assert_eq!(compact["failure"]["code"], "worker_runtime_timed_out");
    }

    #[test]
    fn timeout_failure_contains_remediation_and_elapsed_time() {
        let failure = timeout_failure("run-timeout", 120_000, 120_004);
        assert_eq!(failure["code"], "worker_runtime_timed_out");
        assert_eq!(failure["elapsed_ms"], 120_004);
        assert!(failure["remediation"].as_str().is_some_and(|text| !text.is_empty()));
    }

    #[test]
    fn worker_output_repairs_common_utf8_display_corruption() {
        assert_eq!(repair_mojibake("x Â· y â€“ z"), "x · y – z");
    }

    #[test]
    fn worker_preflight_rejects_missing_read_path() {
        let root = std::env::temp_dir().join(format!("narada-worker-preflight-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&root).expect("root");
        let constraints = json!({"preflight_paths":[{"path":"missing.txt","access":"read"}]});
        let error = preflight_paths(constraints.as_object(), &root, std::slice::from_ref(&root))
            .expect_err("missing path must fail");
        assert_eq!(error["code"], "worker_preflight_path_missing");
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn worker_preflight_accepts_existing_read_path() {
        let root = std::env::temp_dir().join(format!("narada-worker-preflight-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&root).expect("root");
        fs::write(root.join("present.txt"), "ok").expect("file");
        let constraints = json!({"preflight_paths":[{"path":"present.txt","access":"read"}]});
        let result = preflight_paths(constraints.as_object(), &root, std::slice::from_ref(&root))
            .expect("existing path");
        assert_eq!(result["status"], "passed");
        assert_eq!(result["items"][0]["status"], "passed");
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn every_public_tool_has_a_closed_bounded_input_contract() {
        for tool in list_tools() {
            let name = tool["name"].as_str().expect("tool name");
            let schema = &tool["inputSchema"];
            assert_eq!(
                schema["additionalProperties"], false,
                "{name} must be closed"
            );
            if ![
                "worker_policy_inspect",
                "worker_cognition_defaults_inspect",
                "worker_operator_affordances",
            ]
            .contains(&name)
            {
                assert_ne!(
                    schema,
                    &json!({"type":"object","additionalProperties":false}),
                    "{name} unexpectedly has no declared arguments"
                );
            }
        }
        for name in [
            "worker_policy_inspect",
            "worker_cognition_defaults_inspect",
            "worker_operator_affordances",
        ] {
            assert_eq!(
                input_schema(name),
                json!({"type":"object","additionalProperties":false})
            );
        }
    }

    #[test]
    fn containment_is_path_component_aware() {
        assert!(path_components_equal_or_child(
            Path::new("C:/Users/Andrey/Narada/project"),
            Path::new("C:/Users/Andrey/Narada")
        ));
        assert!(!path_components_equal_or_child(
            Path::new("C:/Users/Andrey/Narada-other"),
            Path::new("C:/Users/Andrey/Narada")
        ));
        assert!(path_components_equal_or_child(
            Path::new("C:/Users/Andrey/src/mcp-surfaces"),
            Path::new("C:/Users/Andrey/src")
        ));
        assert!(path_components_equal_or_child(
            Path::new("C:/Users/Andrey/wt/mcp-surfaces"),
            Path::new("C:/Users/Andrey/wt")
        ));
        assert!(!path_components_equal_or_child(
            Path::new("C:/Users/Andrey/src-other/mcp-surfaces"),
            Path::new("C:/Users/Andrey/src")
        ));
    }

    #[test]
    fn wait_and_windows_toolchain_contracts_are_explicit() {
        assert_eq!(
            input_schema("worker_run_wait")["properties"]["timeout_ms"]["maximum"],
            300_000
        );
        assert_eq!(
            input_schema("worker_run")["properties"]["constraints"]["properties"]["wait_timeout_ms"]
                ["maximum"],
            300_000
        );
        assert_eq!(
            guidance(&Map::new())["windows_rust_toolchain"]["status"],
            "caller_environment_required"
        );
        assert!(guidance(&Map::new())["first_use"]
            .as_array()
            .expect("guidance steps")
            .iter()
            .any(|step| step
                .as_str()
                .is_some_and(|text| text.starts_with("READ-ONLY COMMAND CONTRACT"))));
        assert_eq!(
            policy(Path::new("."), &[])["windows_msvc_environment"]["inherited"],
            true
        );
    }

    #[test]
    fn bounded_wait_returns_terminal_run_without_polling_forever() {
        let root =
            std::env::temp_dir().join(format!("narada-worker-wait-{}", uuid::Uuid::new_v4()));
        let run_dir = run_root(&root).join("run-test");
        fs::create_dir_all(&run_dir).expect("run directory");
        fs::write(
            run_dir.join("result.json"),
            serde_json::to_vec(&json!({
                "schema":"narada.worker.run.v1",
                "run_id":"run-test",
                "status":"completed",
                "completion_state":"complete"
            }))
            .expect("run record"),
        )
        .expect("write run");
        let (run, wait) = wait_for_run(&root, "run-test", 30_000).expect("bounded wait");
        assert_eq!(run["status"], "completed");
        assert_eq!(wait["status"], "finished");
        assert_eq!(wait["timeout_ms"], 30_000);
        assert_eq!(wait["native_execution"], "bounded_state_poll");
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn project_site_falls_back_to_user_site_intelligence_context() {
        let base =
            std::env::temp_dir().join(format!("narada-worker-context-{}", uuid::Uuid::new_v4()));
        let project = base.join("src/marici");
        let user_site = base.join("Narada");
        let expected = user_site.join(".narada/intelligence-launch-context.json");
        fs::create_dir_all(expected.parent().expect("context parent")).expect("context dir");
        fs::write(&expected, "{}").expect("context");
        assert_eq!(
            resolve_intelligence_context_path(&project, None, None, Some(base.clone())),
            expected
        );
        fs::remove_dir_all(base).expect("cleanup");
    }

    #[test]
    fn project_site_discovers_sibling_narada_source_root() {
        let base =
            std::env::temp_dir().join(format!("narada-worker-source-{}", uuid::Uuid::new_v4()));
        let source_root = base.join("src");
        let project = source_root.join("marici");
        fs::create_dir_all(source_root.join("narada")).expect("narada source dir");
        fs::create_dir_all(&project).expect("project dir");
        assert_eq!(narada_source_root(&project), source_root);
        fs::remove_dir_all(base).expect("cleanup");
    }

    #[cfg(windows)]
    #[test]
    fn containment_ignores_windows_path_case() {
        assert!(path_components_equal_or_child(
            Path::new("c:/users/andrey/narada/project"),
            Path::new("C:/Users/Andrey/Narada")
        ));
    }

    #[test]
    #[ignore = "requires an explicit deployed Site root and native preflight binary"]
    fn live_native_preflight_resolves_without_caller_plan() {
        let site_root = std::env::var("NARADA_TEST_SITE_ROOT").expect("NARADA_TEST_SITE_ROOT");
        for (cognition, expected_model, expected_effort) in [
            ("low", "gpt-5.6-luna", "max"),
            ("medium", "gpt-5.6-sol", "low"),
            ("high", "gpt-5.6-sol", "max"),
        ] {
            let (plan_ref, provider_mode, model, evidence_ref, reasoning_effort, provider_binding) =
                invocation_plan_binding(Path::new(&site_root), None, Some(cognition))
                    .expect("native preflight");
            assert!(plan_ref.starts_with("plan:cognition:"));
            assert_eq!(provider_mode, "codex-subscription");
            assert_eq!(model, expected_model);
            assert!(evidence_ref.starts_with("preflight-evidence:"));
            assert_eq!(reasoning_effort.as_deref(), Some(expected_effort));
            assert!(provider_binding.is_none() || provider_binding.as_ref().is_some_and(|value| value["schema"] == "narada.native.provider_binding.v1"));
        }
    }

    #[test]
    fn native_worker_refuses_admitted_plan_without_model() {
        let admission = json!({"status":"admitted","plan_ref":"plan:test","selected":{"inference_provider":{"id":"inference-provider:codex-subscription"},"model":null},"evidence_ref":"preflight-evidence:test"});
        let refusal = admitted_plan_binding(&admission).expect_err("missing model must refuse");
        assert_eq!(refusal["code"], "worker_canonical_invocation_model_missing");
    }

    #[test]
    fn native_worker_requires_valid_http_binding_for_api_provider() {
        let admission = json!({
            "status":"admitted",
            "plan_ref":"plan:test",
            "selected":{
                "inference_provider":{"id":"inference-provider:deepseek-api"},
                "model":{"id":"model:deepseek-v4-flash"}
            },
            "evidence_ref":"preflight-evidence:test"
        });
        let refusal = admitted_plan_binding(&admission).expect_err("binding must be required");
        assert_eq!(refusal["code"], "worker_native_provider_binding_missing");

        let mut valid = admission.clone();
        valid["provider_binding"] = json!({
            "schema":"narada.native.provider_binding.v1",
            "provider":"deepseek-api",
            "protocol":"openai/chat-completions/1",
            "endpoint":"https://api.deepseek.com/v1/chat/completions",
            "model":"deepseek-v4-flash",
            "credential_secret_ref":"narada/provider/deepseek-api/api-key"
        });
        assert!(admitted_plan_binding(&valid).is_ok());

        let mut env_binding = valid;
        env_binding["provider_binding"] = json!({
            "schema":"narada.native.provider_binding.v1",
            "provider":"deepseek-api",
            "protocol":"openai/chat-completions/1",
            "endpoint":"https://api.deepseek.com/v1/chat/completions",
            "model":"deepseek-v4-flash",
            "credential_env":"DEEPSEEK_API_KEY"
        });
        let refusal = admitted_plan_binding(&env_binding).expect_err("env binding must refuse");
        assert_eq!(refusal["code"], "worker_native_provider_binding_invalid");
    }

    #[test]
    fn capability_snapshot_reports_effective_write_posture() {
        let cwd = PathBuf::from("C:/workspace/repo");
        let probe = json!({"status":"passed"});
        let snapshot = capability_snapshot("write", &cwd, std::slice::from_ref(&cwd), Some(&probe));
        assert_eq!(snapshot["filesystem"]["write"], true);
        assert_eq!(snapshot["effective_mode"], "workspace_write");
        assert_eq!(snapshot["validated_against_runtime"], true);
        assert_eq!(snapshot["approval"]["mode"], "automatic_contained_review");
        assert_eq!(snapshot["tool_bridge"]["kind"], "codex_builtin_repo_tools");
    }

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
        fs::create_dir_all(root.join(".narada")).expect("site root");
        fs::write(
            root.join(".narada/provider-registry.json"),
            serde_json::to_vec(&json!({
                "schema":"narada.carrier.provider_registry.v1",
                "providers":{"fixture":{"available_models":["fixture-model"]}}
            }))
            .expect("registry"),
        )
        .expect("registry write");
        let updated = cognition_defaults_update(json!({"provider":"fixture","cognition":"high","model":"fixture-model","reasoning_effort":"max","actor":"test"}).as_object().unwrap(), &root).expect("update");
        assert_eq!(updated["status"], "updated");
        assert_eq!(
            cognition_defaults(&root)["defaults"]["high"]["model"],
            "fixture-model"
        );
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn native_worker_rejects_provider_and_model_outside_registry() {
        let root =
            std::env::temp_dir().join(format!("narada-worker-defaults-reject-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(root.join(".narada")).expect("site root");
        fs::write(
            root.join(".narada/provider-registry.json"),
            br#"{"schema":"narada.carrier.provider_registry.v1","providers":{"fixture":{"available_models":["fixture-model"]}}}"#,
        )
        .expect("registry write");
        let unknown_provider = cognition_defaults_update(
            json!({"provider":"unknown","cognition":"low","model":"fixture-model","reasoning_effort":"low"})
                .as_object()
                .unwrap(),
            &root,
        )
        .expect_err("unknown provider must be refused");
        assert_eq!(unknown_provider["code"], "worker_cognition_provider_not_allowed");
        let unknown_model = cognition_defaults_update(
            json!({"provider":"fixture","cognition":"low","model":"unknown-model","reasoning_effort":"low"})
                .as_object()
                .unwrap(),
            &root,
        )
        .expect_err("unknown model must be refused");
        assert_eq!(unknown_model["code"], "worker_cognition_model_not_allowed");
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
