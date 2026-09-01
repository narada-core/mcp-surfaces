use serde::Serialize;
use serde_json::{json, Map, Value};
use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

const MAX_REGISTRY_BYTES: u64 = 4 * 1024 * 1024;
const MAX_RECORDS: usize = 2_000;
const DEFAULT_LIMIT: usize = 100;
const ADMITTED_SCOPES: [&str; 5] = ["all", "host", "user-site", "local-site", "none"];
const ADMITTED_AUTHORITIES: [&str; 3] = ["auto", "read", "write"];
const DECLARED_OPTIONS: [&str; 20] = [
    "Agent",
    "All",
    "Role",
    "Site",
    "Profile",
    "ConfigPath",
    "RegistryPath",
    "OperatorSurface",
    "Runtime",
    "Authority",
    "IntelligenceProvider",
    "McpScope",
    "LauncherUiPort",
    "LauncherUiPortFallback",
    "EnableNativeShell",
    "NoWaitForEnterBeforeExec",
    "Smoke",
    "DryRun",
    "WorkspaceRoot",
    "SiteRoot",
];

pub fn list_tools() -> Vec<Value> {
    vec![
        tool(
            "launcher_guidance",
            "Show model-facing operating guidance for launcher workflows.",
            true,
        ),
        tool(
            "launcher_doctor",
            "Inspect launcher posture, default root, registry path, and source script presence.",
            true,
        ),
        tool(
            "launcher_options_list",
            "List the workspace launcher option surface and read-only MCP coverage.",
            true,
        ),
        tool(
            "launcher_registry_list",
            "List resolved launcher registry agent records without launching agents.",
            true,
        ),
        tool(
            "launcher_plan",
            "Plan a Windows Terminal launch argv without executing it.",
            true,
        ),
        tool(
            "launcher_option_matrix",
            "Return modeled launcher option coverage without executing PowerShell.",
            true,
        ),
        tool(
            "launcher_coherence_check",
            "Check launcher registry coherence and option coverage metadata.",
            true,
        ),
    ]
}

fn tool(name: &str, description: &str, read_only: bool) -> Value {
    let properties = match name {
        "launcher_registry_list" => json!({
            "registry_path": {"type":"string"}, "agent": {"type":"array", "items":{"type":"string"}},
            "role": {"type":"array", "items":{"type":"string"}}, "site": {"type":"array", "items":{"type":"string"}},
            "profile": {"type":"array", "items":{"type":"string"}}, "limit": {"type":"integer", "default":100}
        }),
        "launcher_plan" => json!({
            "registry_path": {"type":"string"}, "agent": {"type":"array", "items":{"type":"string"}},
            "all": {"type":"boolean"}, "config_path": {"type":"array", "items":{"type":"string"}},
            "role": {"type":"array", "items":{"type":"string"}}, "site": {"type":"array", "items":{"type":"string"}},
            "profile": {"type":"array", "items":{"type":"string"}}, "operator_surface": {"type":"string"},
            "runtime": {"type":"string"}, "authority": {"type":"string", "enum":ADMITTED_AUTHORITIES},
            "mcp_scope": {"type":"string", "enum":ADMITTED_SCOPES}, "launch_profile": {"type":"string"},
            "startup_stagger_seconds": {"type":"integer", "minimum":0, "maximum":300},
            "intelligence_provider": {"type":"string"}, "enable_native_shell": {"type":"boolean"},
            "no_wait_for_enter_before_exec": {"type":"boolean"},
            "orientation_entry_file": {"type":"string"}
        }),
        "launcher_option_matrix" | "launcher_coherence_check" => {
            json!({"registry_path":{"type":"string"}})
        }
        _ => json!({}),
    };
    json!({
        "name": name,
        "description": description,
        "inputSchema": {"type":"object", "properties":properties, "additionalProperties":false},
        "annotations": {"title":name, "readOnlyHint":read_only, "destructiveHint":false, "idempotentHint":true, "openWorldHint":false},
        "outputSchema": {"type":"object", "additionalProperties":true}
    })
}

pub fn call_tool(
    name: &str,
    args: &Map<String, Value>,
    narada_root: &Path,
    configured_registry: Option<&Path>,
) -> Result<Value, Value> {
    let result = match name {
        "launcher_guidance" => guidance(),
        "launcher_doctor" => doctor(narada_root, configured_registry),
        "launcher_options_list" => options_list(),
        "launcher_registry_list" => registry_list(args, narada_root, configured_registry),
        "launcher_plan" => plan(args, narada_root, configured_registry),
        "launcher_option_matrix" => option_matrix(args, narada_root, configured_registry),
        "launcher_coherence_check" => coherence(args, narada_root, configured_registry),
        _ => Err(diagnostic(
            "unknown_tool",
            &format!("unknown_tool:{name}"),
            json!({"tool_name":name}),
        )),
    }?;
    Ok(result)
}

fn guidance() -> Result<Value, Value> {
    Ok(json!({
        "schema":"narada.mcp_surface.guidance.v0", "status":"ok", "surface_id":"launcher",
        "purpose":"Read-only planning and inspection for Narada workspace agent launches.",
        "first_use":["Call launcher_doctor and launcher_registry_list before planning.", "Use launcher_plan for argv inspection; this surface never launches a process."],
        "boundaries":["The launcher surface does not invoke PowerShell, Windows Terminal, or agents.", "Registry state and paths remain the authority."],
        "tools":["launcher_doctor","launcher_options_list","launcher_registry_list","launcher_plan","launcher_option_matrix","launcher_coherence_check"]
    }))
}

fn doctor(root: &Path, configured_registry: Option<&Path>) -> Result<Value, Value> {
    let registry = registry_path(root, configured_registry, None);
    let start = root.join("Start-NaradaWorkspace.ps1");
    let matrix = root
        .join("tools")
        .join("agent-start")
        .join("Test-LauncherOptionMatrix.ps1");
    Ok(json!({
        "schema":"narada.launcher.doctor.v1",
        "status": if registry.exists() && start.exists() {"ok"} else {"degraded"},
        "server_name":"launcher-mcp", "server_version":"0.1.0", "protocol_version":"2024-11-05",
        "narada_root": path_text(root), "registry_path":path_text(&registry), "registry_exists":registry.exists(),
        "start_workspace_script":path_text(&start), "start_workspace_script_exists":start.exists(),
        "option_matrix_script":path_text(&matrix), "option_matrix_script_exists":matrix.exists(),
        "execution_posture":"read_only_no_launch_no_shell",
        "mcp_injection_scope_doctrine":{"scopes":["host","user_site","local_site"], "scope_source":"mcp-registrar"}
    }))
}

fn options_list() -> Result<Value, Value> {
    Ok(json!({
        "schema":"narada.launcher.options.v1", "status":"ok", "declared_options":DECLARED_OPTIONS,
        "covered_options":DECLARED_OPTIONS,
        "options":DECLARED_OPTIONS.iter().map(|name| json!({"name":name,"kind":if ["All","LauncherUiPortFallback","EnableNativeShell","NoWaitForEnterBeforeExec","Smoke","DryRun"].contains(name) {"switch"} else {"value"},"covered_by_mcp":true,"mutates_processes":false})).collect::<Vec<_>>(),
        "tools":["launcher_doctor","launcher_options_list","launcher_registry_list","launcher_plan","launcher_option_matrix","launcher_coherence_check"]
    }))
}

fn registry_list(
    args: &Map<String, Value>,
    root: &Path,
    configured_registry: Option<&Path>,
) -> Result<Value, Value> {
    let selected = select_records(args, root, configured_registry, false)?;
    let limit = clamp(
        args.get("limit").and_then(Value::as_i64),
        DEFAULT_LIMIT,
        1,
        1_000,
    );
    Ok(
        json!({"schema":"narada.launcher.registry.v1","status":"ok","count":selected.records.len().min(limit),"total_count":selected.records.len(),"records":selected.records.into_iter().take(limit).collect::<Vec<_>>() }),
    )
}

