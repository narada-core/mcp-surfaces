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
            "--target-site-id",
            &record.site,
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

