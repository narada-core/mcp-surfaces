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

fn plan(
    args: &Map<String, Value>,
    root: &Path,
    configured_registry: Option<&Path>,
) -> Result<Value, Value> {
    let selected = select_records(args, root, configured_registry, true)?;
    let stagger = clamp(
        args.get("startup_stagger_seconds").and_then(Value::as_i64),
        0,
        0,
        300,
    );
    let requested_scope = optional_string(args, "mcp_scope");
    if let Some(scope) = requested_scope.as_deref() {
        if !ADMITTED_SCOPES.contains(&scope) {
            return Err(diagnostic(
                "mcp_scope_not_admitted",
                &format!("mcp_scope_not_admitted:{scope}"),
                json!({"admitted_scopes":ADMITTED_SCOPES}),
            ));
        }
    }
    let runtime_override = optional_string(args, "runtime");
    let operator_surface_override = optional_string(args, "operator_surface");
    let authority_override = optional_string(args, "authority");
    if let Some(authority) = authority_override.as_deref() {
        if !ADMITTED_AUTHORITIES.contains(&authority) {
            return Err(diagnostic(
                "authority_not_admitted",
                &format!("authority_not_admitted:{authority}"),
                json!({"admitted_authorities":ADMITTED_AUTHORITIES}),
            ));
        }
    }
    let profile_override = optional_string(args, "launch_profile");
    let orientation_entry_file = optional_string(args, "orientation_entry_file");
    let provider = optional_string(args, "intelligence_provider");
    let compatibility_diagnostics = Vec::<Value>::new();
    let mut wt_args = Vec::<Value>::new();
    let mut native_launches = Vec::<Value>::new();
    let mut scope_plan = Vec::<Value>::new();
    let mut startup = Vec::<Value>::new();
    for (index, record) in selected.records.iter().enumerate() {
        if !wt_args.is_empty() {
            wt_args.push(json!(";"));
        }
        let runtime = runtime_override.as_deref().unwrap_or(&record.runtime);
        if !runtime.eq_ignore_ascii_case("narada-agent-runtime-server") {
            return Err(diagnostic(
                "native_launch_runtime_not_admitted",
                &format!("native_launch_runtime_not_admitted:{runtime}"),
                json!({"required_runtime":"narada-agent-runtime-server"}),
            ));
        }
        let operator_surface = operator_surface_override
            .as_deref()
            .unwrap_or(&record.operator_surface);
        let authority = authority_override.as_deref().unwrap_or(&record.authority);
        let profile = profile_override.as_deref().unwrap_or(&record.profile);
        let scope = requested_scope.as_deref().unwrap_or(&record.mcp_scope);
        let title = if record.title.is_empty() {
            &record.agent
        } else {
            &record.title
        };
        let working_directory = if record.workspace_root.is_empty() {
            &record.narada_root
        } else {
            &record.workspace_root
        };
        let compiler = std::env::current_exe().map_err(|cause| {
            diagnostic(
                "native_launch_compiler_unavailable",
                &format!("native_launch_compiler_unavailable:{cause}"),
                json!({}),
            )
        })?;
        let runtime_binary = native_runtime_binary(record);
        let session_id = format!(
            "carrier_{}_{}",
            record
                .agent
                .chars()
                .map(|ch| if ch.is_ascii_alphanumeric() { ch } else { '_' })
                .collect::<String>(),
            &uuid::Uuid::new_v4().simple().to_string()[..12]
        );
        for arg in [
            "new-tab",
            "--title",
            title,
            "-d",
            working_directory,
            &path_text(&compiler),
            "--resident-runtime-host",
            "--runtime",
            &path_text(&runtime_binary),
            "--site-root",
            &record.site_root,
            "--identity",
            &record.agent,
            "--session",
            &session_id,
        ] {
            wt_args.push(json!(arg));
        }
        wt_args.push(json!("--authority"));
        wt_args.push(json!(authority));
        if !scope.is_empty() {
            wt_args.push(json!("--mcp-scope"));
            wt_args.push(json!(scope));
        }
        if let Some(entry_file) = orientation_entry_file.as_deref() {
            wt_args.push(json!("--orientation-entry-file"));
            wt_args.push(json!(entry_file));
        }
        if let Some(provider) = provider.as_deref() {
            wt_args.push(json!("--intelligence-provider"));
            wt_args.push(json!(provider));
        }
        if args
            .get("enable_native_shell")
            .and_then(Value::as_bool)
            .unwrap_or(false)
            || record.enable_native_shell
        {
            wt_args.push(json!("--enable-native-shell"));
        }
        native_launches.push(json!({
            "schema":"narada.native_launch_compilation.v1", "status":"compiled",
            "compiler":path_text(&compiler), "runtime":path_text(&runtime_binary),
            "runtime_exists":runtime_binary.is_file(), "operator_surface":operator_surface,
            "identity":record.agent, "carrier_session_id":session_id,
            "site_id":record.site, "site_root":record.site_root,
            "workspace_root":working_directory, "authority":authority, "mcp_scope":scope,
            "orientation_entry_file":orientation_entry_file,
            "orientation_required":orientation_entry_file.is_some(),
            "intelligence_provider":provider,
            "native_shell_enabled":args.get("enable_native_shell").and_then(Value::as_bool).unwrap_or(false) || record.enable_native_shell
        }));
        scope_plan.push(json!({"agent":record.agent,"requested":scope,"requested_loci":scope_loci(scope),"registry_default":record.mcp_scope}));
        startup.push(json!({"agent":record.agent,"site":record.site,"role":record.role,"registry_profile":if record.profile.is_empty(){Value::Null}else{json!(&record.profile)},"launch_profile":if profile.is_empty(){Value::Null}else{json!(profile)},"start_after_seconds":index * stagger,"diagnostics":if record.profile.is_empty(){json!(["registry_profile_missing"])}else{json!([])}}));
    }
    let profiles: Vec<String> = startup
        .iter()
        .filter_map(|entry| {
            entry
                .get("launch_profile")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .collect::<HashSet<_>>()
        .into_iter()
        .collect();
    Ok(json!({
        "schema":"narada.workspace_launch.dry_run.v1","status":"planned","count":selected.records.len(),"windows_terminal_invoked":false,
        "registry_paths":selected.registry_paths,"wt_args":wt_args,"command_contract":"narada.native_launch_compilation.v1","native_launches":native_launches,"compatibility_diagnostics":compatibility_diagnostics,"mcp_scope_plan":{"admitted_scopes":ADMITTED_SCOPES,"agents":scope_plan},"records":selected.records,
        "startup_profile_plan":{"schema":"narada.launcher.profile_startup_plan.v1","execution_posture":"planned_not_started_by_mcp","selected_count":startup.len(),"stagger_seconds":stagger,"profile_count":profiles.len(),"profiles":profiles,"entries":startup,"diagnostics":[]}
    }))
}

fn option_matrix(
    args: &Map<String, Value>,
    root: &Path,
    configured_registry: Option<&Path>,
) -> Result<Value, Value> {
    let registry = registry_path(
        root,
        configured_registry,
        args.get("registry_path").and_then(Value::as_str),
    );
    let records = load_records(&registry).unwrap_or_default();
    let representative = records
        .iter()
        .find(|record| record.agent == "smart-scheduling.resident")
        .or_else(|| records.iter().find(|record| record.runtime == "agent-cli"))
        .or_else(|| records.first());
    let names = [
        "selection_required",
        "unknown_agent",
        "missing_role_filter",
        "missing_site_filter",
        "missing_profile_filter",
        "agent_exact_dry_run",
        "all_site_role_dry_run",
        "profile_aware_startup",
        "config_path_selects_without_all",
        "runtime_override",
        "native_shell_flag",
        "intelligence_provider_flag",
        "no_wait_flag",
        "smoke_agent_cli_contract",
        "smoke_site_role_filter_contract",
    ];
    Ok(
        json!({"schema":"narada.launcher.option_matrix_model.v1","status":"modeled","registry_path":path_text(&registry),"execution_posture":"not_executed_by_mcp","representative_agent":representative.map(|record|record.agent.clone()).unwrap_or_default(),"representative_site":representative.map(|record|record.site.clone()).unwrap_or_default(),"representative_role":representative.map(|record|record.role.clone()).unwrap_or_default(),"representative_runtime":representative.map(|record|record.runtime.clone()).unwrap_or_else(||"codex".to_string()),"representative_profile":representative.map(|record|record.profile.clone()).unwrap_or_default(),"declared_options":DECLARED_OPTIONS,"covered_options":DECLARED_OPTIONS,"case_count":names.len(),"cases":names.iter().map(|name|json!({"case":name,"modeled":true})).collect::<Vec<_>>() }),
    )
}

fn coherence(
    args: &Map<String, Value>,
    root: &Path,
    configured_registry: Option<&Path>,
) -> Result<Value, Value> {
    let registry = registry_path(
        root,
        configured_registry,
        args.get("registry_path").and_then(Value::as_str),
    );
    let mut findings = Vec::<Value>::new();
    if !registry.exists() {
        findings.push(finding(
            "error",
            "launcher_registry_missing",
            &format!("Registry path does not exist: {}", path_text(&registry)),
            &registry,
        ));
    }
    let records = if registry.exists() {
        load_records(&registry)?
    } else {
        Vec::new()
    };
    let mut seen = HashMap::<String, String>::new();
    for record in &records {
        if let Some(first) = seen.insert(record.agent.clone(), record.config_path.clone()) {
            findings.push(json!({"severity":"error","code":"launcher_duplicate_agent","message":format!("Duplicate agent in launch registry: {}",record.agent),"path":path_text(&registry),"agent":record.agent,"first_config_path":first}));
        }
        for (field, value) in [
            ("narada_root", &record.narada_root),
            ("site_root", &record.site_root),
            ("workspace_root", &record.workspace_root),
            ("launcher_path", &record.launcher_path),
        ] {
            if !value.is_empty() && !Path::new(value).exists() {
                findings.push(finding(
                    "warning",
                    &format!("launcher_{field}_missing"),
                    &format!("{field} does not exist for {}: {}", record.agent, value),
                    Path::new(value),
                ));
            }
        }
    }
    let errors = findings
        .iter()
        .filter(|item| item.get("severity").and_then(Value::as_str) == Some("error"))
        .count();
    let warnings = findings
        .iter()
        .filter(|item| item.get("severity").and_then(Value::as_str) == Some("warning"))
        .count();
    Ok(
        json!({"schema":"narada.launcher.coherence.v1","status":if errors>0{"invalid"}else if warnings>0{"valid_with_warnings"}else{"valid"},"registry_path":path_text(&registry),"agent_count":records.len(),"errors":errors,"warnings":warnings,"findings":findings}),
    )
}

#[derive(Clone, Debug, Serialize)]
struct AgentRecord {
    agent: String,
    title: String,
    role: String,
    site: String,
    narada_root: String,
    dependency_narada_root: String,
    site_root: String,
    workspace_root: String,
    launcher_path: String,
    operator_surface: String,
    runtime: String,
    authority: String,
    profile: String,
    mcp_scope: String,
    enable_native_shell: bool,
    config_path: String,
}

fn select_records(
    args: &Map<String, Value>,
    root: &Path,
    configured_registry: Option<&Path>,
    require_selection: bool,
) -> Result<Selection, Value> {
    let config_paths = string_array(args.get("config_path"));
    let paths = if config_paths.is_empty() {
        vec![registry_path(
            root,
            configured_registry,
            args.get("registry_path").and_then(Value::as_str),
        )]
    } else {
        config_paths.iter().map(PathBuf::from).collect()
    };
    let mut all = Vec::<AgentRecord>::new();
    for path in &paths {
        all.extend(load_records(path)?);
    }
    let requested_agents = string_array(args.get("agent"));
    let mut selected = if !requested_agents.is_empty() {
        let mut result = Vec::new();
        for agent in &requested_agents {
            let matches: Vec<AgentRecord> = all
                .iter()
                .filter(|record| &record.agent == agent)
                .cloned()
                .collect();
            if matches.len() == 0 {
                return Err(diagnostic(
                    "agent_not_found_in_launch_registry",
                    &format!("agent_not_found_in_launch_registry:{agent}"),
                    json!({}),
                ));
            }
            if matches.len() > 1 {
                return Err(diagnostic(
                    "agent_duplicate_in_launch_registry",
                    &format!("agent_duplicate_in_launch_registry:{agent}"),
                    json!({}),
                ));
            }
            result.push(matches[0].clone());
        }
        result
    } else if args.get("all").and_then(Value::as_bool).unwrap_or(false)
        || !config_paths.is_empty()
        || !require_selection
    {
        all
    } else {
        return Err(diagnostic(
            "launch_selection_required",
            "launch_selection_required: specify agent, all, or config_path",
            json!({}),
        ));
    };
    filter_records(&mut selected, args, "role", |record, value| {
        record.role.eq_ignore_ascii_case(value)
    });
    filter_records(&mut selected, args, "profile", |record, value| {
        !record.profile.is_empty() && record.profile.eq_ignore_ascii_case(value)
    });
    if let Some(values) = nonempty_filter(args.get("site")) {
        selected.retain(|record| {
            values.iter().any(|value| {
                site_aliases(record)
                    .iter()
                    .any(|alias| alias.eq_ignore_ascii_case(value))
            })
        });
        if selected.is_empty() {
            return Err(diagnostic(
                "no_agents_match_site_filter",
                "no_agents_match_site_filter",
                json!({"site":values}),
            ));
        }
    }
    Ok(Selection {
        records: selected,
        registry_paths: paths.iter().map(|path| path_text(path)).collect(),
    })
}

struct Selection {
    records: Vec<AgentRecord>,
    registry_paths: Vec<String>,
}

fn filter_records<F>(
    records: &mut Vec<AgentRecord>,
    args: &Map<String, Value>,
    key: &str,
    predicate: F,
) where
    F: Fn(&AgentRecord, &str) -> bool,
{
    if let Some(values) = nonempty_filter(args.get(key)) {
        records.retain(|record| values.iter().any(|value| predicate(record, value)));
    }
}

fn load_records(path: &Path) -> Result<Vec<AgentRecord>, Value> {
    let source = read_bounded(path)?;
    let parsed = Parser::new(&source).parse().map_err(|message| {
        diagnostic(
            "psd1_parse_error",
            &message,
            json!({"path":path_text(path)}),
        )
    })?;
    let root = parsed.as_object().ok_or_else(|| {
        diagnostic(
            "psd1_parse_error",
            "psd1 root must be a hashtable",
            json!({"path":path_text(path)}),
        )
    })?;
    let agents = root
        .get("Agents")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let narada_root = root
        .get("NaradaRoot")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let site_root = root
        .get("SiteRoot")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let workspace_root = root
        .get("WorkspaceRoot")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let launcher = root
        .get("Launcher")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let operator_surface = root
        .get("OperatorSurface")
        .or_else(|| root.get("Carrier"))
        .and_then(Value::as_str)
        .unwrap_or("codex")
        .to_string();
    let runtime = root
        .get("Runtime")
        .and_then(Value::as_str)
        .unwrap_or("narada-agent-runtime-server")
        .to_string();
    let authority = root
        .get("Authority")
        .and_then(Value::as_str)
        .unwrap_or("auto")
        .to_string();
    let profile = root
        .get("Profile")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let mcp_scope = root
        .get("McpScope")
        .and_then(Value::as_str)
        .unwrap_or("all")
        .to_string();
    let mut records = Vec::new();
    for agent in agents.into_iter().take(MAX_RECORDS) {
        let object = agent.as_object().cloned().unwrap_or_default();
        let agent_id = required_field(&object, "Agent")?;
        let nr = string_field(&object, "NaradaRoot").unwrap_or_else(|| narada_root.clone());
        let dependency_narada_root = string_field(&object, "DependencyNaradaRoot")
            .or_else(|| string_field(&object, "NaradaProperRoot"))
            .or_else(|| {
                root.get("DependencyNaradaRoot")
                    .or_else(|| root.get("NaradaProperRoot"))
                    .and_then(Value::as_str)
                    .map(str::to_string)
            })
            .unwrap_or_else(|| resolve_dependency_narada_root(&nr));
        let sr = string_field(&object, "SiteRoot").unwrap_or_else(|| {
            if site_root.is_empty() {
                nr.clone()
            } else {
                site_root.clone()
            }
        });
        let wr = string_field(&object, "WorkspaceRoot").unwrap_or_else(|| workspace_root.clone());
        let launch = string_field(&object, "LauncherPath")
            .or_else(|| string_field(&object, "Launcher").map(|value| join_path(&nr, &value)))
            .or_else(|| {
                if launcher.is_empty() {
                    None
                } else {
                    Some(join_path(&nr, &launcher))
                }
            })
            .unwrap_or_default();
        let short = agent_id.rsplit('.').next().unwrap_or(&agent_id).to_string();
        let role = string_field(&object, "Role").unwrap_or_else(|| strip_digits(&short));
        let site = string_field(&object, "Site")
            .unwrap_or_else(|| agent_id.split('.').next().unwrap_or(&agent_id).to_string());
        let operator_surface_value = string_field(&object, "OperatorSurface")
            .or_else(|| string_field(&object, "Carrier"))
            .unwrap_or_else(|| operator_surface.clone());
        let runtime_value = string_field(&object, "Runtime").unwrap_or_else(|| runtime.clone());
        let authority_value =
            string_field(&object, "Authority").unwrap_or_else(|| authority.clone());
        if !ADMITTED_AUTHORITIES.contains(&authority_value.as_str()) {
            return Err(diagnostic(
                "authority_not_admitted",
                &format!("authority_not_admitted:{authority_value}"),
                json!({"agent":agent_id,"admitted_authorities":ADMITTED_AUTHORITIES}),
            ));
        }
        let profile_value = string_field(&object, "Profile").unwrap_or_else(|| profile.clone());
        let scope_value = string_field(&object, "McpScope").unwrap_or_else(|| mcp_scope.clone());
        if !ADMITTED_SCOPES.contains(&scope_value.as_str()) {
            return Err(diagnostic(
                "mcp_scope_not_admitted",
                &format!("mcp_scope_not_admitted:{scope_value}"),
                json!({"agent":agent_id,"admitted_scopes":ADMITTED_SCOPES}),
            ));
        }
        records.push(AgentRecord {
            agent: agent_id.clone(),
            title: string_field(&object, "Title").unwrap_or_else(|| short.clone()),
            role,
            site,
            narada_root: normalize_path(&nr),
            dependency_narada_root: normalize_path(&dependency_narada_root),
            site_root: normalize_path(&sr),
            workspace_root: normalize_path(&wr),
            launcher_path: normalize_path(&launch),
            operator_surface: operator_surface_value,
            runtime: runtime_value,
            authority: authority_value,
            profile: profile_value,
            mcp_scope: scope_value,
            enable_native_shell: object
                .get("EnableNativeShell")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            config_path: path_text(path),
        });
    }
    Ok(records)
}

fn read_bounded(path: &Path) -> Result<String, Value> {
    let mut file = File::open(path).map_err(|error| {
        diagnostic(
            "launch_registry_missing",
            &format!("launch_registry_missing:{}", path_text(path)),
            json!({"path":path_text(path),"error":error.to_string()}),
        )
    })?;
    let size = file
        .metadata()
        .map_err(|error| {
            diagnostic(
                "launch_registry_stat_failed",
                &error.to_string(),
                json!({"path":path_text(path)}),
            )
        })?
        .len();
    if size > MAX_REGISTRY_BYTES {
        return Err(diagnostic(
            "launch_registry_too_large",
            "launch registry exceeds bounded parser input",
            json!({"bytes":size,"maximum":MAX_REGISTRY_BYTES}),
        ));
    }
    let mut source = String::with_capacity(size as usize);
    file.seek(SeekFrom::Start(0)).ok();
    file.read_to_string(&mut source).map_err(|error| {
        diagnostic(
            "launch_registry_read_failed",
            &error.to_string(),
            json!({"path":path_text(path)}),
        )
    })?;
    Ok(source)
}

fn registry_path(root: &Path, configured: Option<&Path>, requested: Option<&str>) -> PathBuf {
    requested
        .map(PathBuf::from)
        .or_else(|| configured.map(PathBuf::from))
        .unwrap_or_else(|| root.join("config").join("launch").join("agents.psd1"))
}

fn required_field(object: &Map<String, Value>, key: &str) -> Result<String, Value> {
    string_field(object, key).ok_or_else(|| {
        diagnostic(
            "psd1_field_missing",
            &format!("agent_missing:{key}"),
            json!({"field":key}),
        )
    })
}
fn string_field(object: &Map<String, Value>, key: &str) -> Option<String> {
    object
        .get(key)
        .and_then(Value::as_str)
        .map(str::to_string)
        .filter(|value| !value.is_empty())
}
fn string_array(value: Option<&Value>) -> Vec<String> {
    match value {
        Some(Value::Array(items)) => items
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_string)
            .filter(|v| !v.is_empty())
            .collect(),
        Some(Value::String(value)) if !value.is_empty() => vec![value.clone()],
        _ => Vec::new(),
    }
}
fn nonempty_filter(value: Option<&Value>) -> Option<Vec<String>> {
    let values = string_array(value);
    if values.is_empty() {
        None
    } else {
        Some(values)
    }
}
fn optional_string(args: &Map<String, Value>, key: &str) -> Option<String> {
    args.get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(str::to_string)
}
fn clamp(value: Option<i64>, fallback: usize, min: usize, max: usize) -> usize {
    value
        .map(|v| v.max(min as i64).min(max as i64) as usize)
        .unwrap_or(fallback)
        .clamp(min, max)
}
fn strip_digits(value: &str) -> String {
    value
        .trim_end_matches(|ch: char| ch.is_ascii_digit())
        .to_string()
}
fn normalize_path(value: &str) -> String {
    value.replace('\\', "/")
}
fn resolve_dependency_narada_root(narada_root: &str) -> String {
    let target = PathBuf::from(narada_root);
    let parent = target.parent().unwrap_or_else(|| Path::new(""));
    let sibling = parent.join("narada");
    let user_source = parent.join("src").join("narada");
    for candidate in [&target, &sibling, &user_source] {
        if candidate.join("package.json").is_file()
            && candidate
                .join("packages")
                .join("layers")
                .join("cli")
                .join("package.json")
                .is_file()
        {
            return path_text(candidate);
        }
    }
    if target
        .file_name()
        .map(|name| name.to_string_lossy().eq_ignore_ascii_case("narada"))
        .unwrap_or(false)
    {
        return path_text(&target);
    }
    if parent
        .file_name()
        .map(|name| {
            let value = name.to_string_lossy();
            value.eq_ignore_ascii_case("src") || value.eq_ignore_ascii_case("code")
        })
        .unwrap_or(false)
    {
        return path_text(&sibling);
    }
    path_text(&user_source)
}
fn join_path(root: &str, child: &str) -> String {
    let path = PathBuf::from(child);
    if path.is_absolute() {
        path_text(&path)
    } else {
        path_text(&PathBuf::from(root).join(path))
    }
}
fn path_text(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

fn native_runtime_binary(record: &AgentRecord) -> PathBuf {
    if let Some(path) = std::env::var_os("NARADA_AGENT_RUNTIME_SERVER_NATIVE") {
        return PathBuf::from(path);
    }
    let executable = if cfg!(windows) {
        "narada-agent-runtime-server-rust.exe"
    } else {
        "narada-agent-runtime-server-rust"
    };
    PathBuf::from(&record.dependency_narada_root)
        .join("packages")
        .join("agent-runtime-server")
        .join("native")
        .join("target")
        .join("release")
        .join(executable)
}
fn site_aliases(record: &AgentRecord) -> Vec<String> {
    let prefix = record.agent.split('.').next().unwrap_or(&record.agent);
    vec![
        record.site.clone(),
        record
            .site
            .strip_prefix("narada-")
            .unwrap_or(&record.site)
            .to_string(),
        if record.site.starts_with("narada-") {
            record.site.clone()
        } else {
            format!("narada-{}", record.site)
        },
        prefix.to_string(),
        if prefix.starts_with("narada-") {
            prefix.to_string()
        } else {
            format!("narada-{}", prefix)
        },
    ]
}
fn scope_loci(scope: &str) -> Vec<&'static str> {
    match scope {
        "none" => vec![],
        "host" => vec!["host"],
        "user-site" => vec!["user-site"],
        "local-site" => vec!["local-site"],
        _ => vec!["host", "user-site", "local-site"],
    }
}
fn finding(severity: &str, code: &str, message: &str, path: &Path) -> Value {
    json!({"severity":severity,"code":code,"message":message,"path":path_text(path)})
}
fn diagnostic(code: &str, message: &str, details: Value) -> Value {
    let mut value = json!({"code":code,"message":message});
    if !details.is_null() {
        value["details"] = details;
    }
    value
}

struct Parser<'a> {
    tokens: Vec<Token>,
    position: usize,
    _source: &'a str,
}
#[derive(Clone, Debug)]
enum Token {
    Word(String),
    At,
    OpenBrace,
    CloseBrace,
    OpenParen,
    CloseParen,
    Equals,
    Separator,
}
impl<'a> Parser<'a> {
    fn new(source: &'a str) -> Self {
        Self {
            tokens: tokenize(source),
            position: 0,
            _source: source,
        }
    }
    fn parse(mut self) -> Result<Value, String> {
        self.value()
    }
    fn peek(&self) -> Option<&Token> {
        self.tokens.get(self.position)
    }
    fn take(&mut self) -> Option<Token> {
        let token = self.tokens.get(self.position).cloned();
        self.position += 1;
        token
    }
    fn value(&mut self) -> Result<Value, String> {
        match self.take() {
            Some(Token::At) => match self.take() {
                Some(Token::OpenBrace) => self.object(),
                Some(Token::OpenParen) => self.array(),
                other => Err(format!(
                    "expected hashtable or array after @, got {other:?}"
                )),
            },
            Some(Token::OpenParen) => self.array(),
            Some(Token::Word(value)) => Ok(match value.as_str() {
                "$true" => Value::Bool(true),
                "$false" => Value::Bool(false),
                "$null" => Value::Null,
                _ => Value::String(value),
            }),
            other => Err(format!("unexpected token {other:?}")),
        }
    }
    fn object(&mut self) -> Result<Value, String> {
        let mut map = Map::new();
        loop {
            match self.peek() {
                Some(Token::CloseBrace) => {
                    self.take();
                    break;
                }
                None => return Err("unexpected end of hashtable".to_string()),
                _ => {}
            }
            let key = match self.take() {
                Some(Token::Word(value)) => value,
                other => return Err(format!("expected hashtable key, got {other:?}")),
            };
            match self.take() {
                Some(Token::Equals) => {}
                other => return Err(format!("expected = after key, got {other:?}")),
            };
            map.insert(key, self.value()?);
            if matches!(self.peek(), Some(Token::Separator)) {
                self.take();
            }
        }
        Ok(Value::Object(map))
    }
    fn array(&mut self) -> Result<Value, String> {
        let mut values = Vec::new();
        loop {
            match self.peek() {
                Some(Token::CloseParen) => {
                    self.take();
                    break;
                }
                None => return Err("unexpected end of array".to_string()),
                _ => {}
            }
            values.push(self.value()?);
            if matches!(self.peek(), Some(Token::Separator)) {
                self.take();
            }
        }
        Ok(Value::Array(values))
    }
}

fn tokenize(source: &str) -> Vec<Token> {
    let chars: Vec<char> = source.chars().collect();
    let mut out = Vec::new();
    let mut index = 0;
    while index < chars.len() {
        let ch = chars[index];
        if ch.is_whitespace() {
            index += 1;
            continue;
        }
        if ch == '#' {
            while index < chars.len() && chars[index] != '\n' {
                index += 1;
            }
            continue;
        }
        let token = match ch {
            '@' => Some(Token::At),
            '{' => Some(Token::OpenBrace),
            '}' => Some(Token::CloseBrace),
            '(' => Some(Token::OpenParen),
            ')' => Some(Token::CloseParen),
            '=' => Some(Token::Equals),
            ';' | ',' => Some(Token::Separator),
            _ => None,
        };
        if let Some(token) = token {
            out.push(token);
            index += 1;
            continue;
        }
        if ch == '\'' || ch == '"' {
            let quote = ch;
            index += 1;
            let mut value = String::new();
            while index < chars.len() {
                let current = chars[index];
                index += 1;
                if current == quote {
                    if quote == '\'' && index < chars.len() && chars[index] == '\'' {
                        value.push('\'');
                        index += 1;
                        continue;
                    }
                    break;
                }
                value.push(current);
            }
            out.push(Token::Word(value));
            continue;
        }
        let mut value = String::new();
        while index < chars.len() {
            let current = chars[index];
            if current.is_whitespace() || "@{}()=;,".contains(current) || current == '#' {
                break;
            }
            value.push(current);
            index += 1;
        }
        if !value.is_empty() {
            out.push(Token::Word(value));
        } else {
            index += 1;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn parses_nested_psd1_agents() {
        let source = "@{ NaradaRoot = 'C:/Narada'; Agents = @(@{ Agent = 'site.user'; Role = 'user'; EnableNativeShell = $true }); }";
        let value = Parser::new(source).parse().expect("parse");
        assert_eq!(value["Agents"][0]["Agent"], "site.user");
        assert_eq!(value["Agents"][0]["EnableNativeShell"], true);
    }
    #[test]
    fn scope_loci_are_bounded() {
        assert_eq!(scope_loci("all"), vec!["host", "user-site", "local-site"]);
        assert!(scope_loci("none").is_empty());
    }

    #[test]
    fn plan_uses_direct_operator_surface_runtime_contract() {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let test_root = std::env::temp_dir().join(format!(
            "narada-launcher-native-{}-{nonce}",
            std::process::id()
        ));
        std::fs::create_dir_all(&test_root).expect("create test root");
        let registry = test_root.join("agents.psd1");
        std::fs::write(
            &registry,
            format!(
                "@{{ NaradaRoot = '{}'; WorkspaceRoot = '{}'; SiteRoot = '{}'; Launcher = 'site.ps1'; OperatorSurface = 'agent-cli'; Runtime = 'narada-agent-runtime-server'; Agents = @(@{{ Agent = 'site.architect'; Role = 'architect'; McpScope = 'all'; }}); }}",
                path_text(&test_root),
                path_text(&test_root),
                path_text(&test_root)
            ),
        )
        .expect("write registry");

        let mut args = Map::new();
        args.insert("agent".to_string(), json!(["site.architect"]));
        let result = plan(&args, &test_root, Some(&registry)).expect("plan");
        let argv: Vec<&str> = result["wt_args"]
            .as_array()
            .expect("wt args")
            .iter()
            .filter_map(Value::as_str)
            .collect();

        assert!(argv.iter().any(|value| *value == "--resident-runtime-host"));
        assert!(argv.iter().any(
            |value| value.ends_with("narada-agent-runtime-server-rust.exe")
                || value.ends_with("narada-agent-runtime-server-rust")
        ));
        assert!(!argv
            .iter()
            .any(|value| matches!(*value, "pnpm" | "node" | "bun")));
        assert_eq!(
            result["command_contract"],
            "narada.native_launch_compilation.v1"
        );
        assert_eq!(result["native_launches"][0]["status"], "compiled");
        assert!(argv
            .windows(2)
            .any(|window| window == ["--identity", "site.architect"]));
        assert!(argv
            .windows(2)
            .any(|window| window == ["--authority", "auto"]));
        assert!(argv
            .windows(2)
            .any(|window| window == ["--mcp-scope", "all"]));
        assert!(!argv.iter().any(|arg| arg.contains("Start-NaradaAgent.ps1")));
        assert!(!argv.contains(&"-LauncherPath"));
        assert!(!argv.contains(&"--wait"));

        std::fs::remove_dir_all(test_root).expect("remove test root");
    }
}
