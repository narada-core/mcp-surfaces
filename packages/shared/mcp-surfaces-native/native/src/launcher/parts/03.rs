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

