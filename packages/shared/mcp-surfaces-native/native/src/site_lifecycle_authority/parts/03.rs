pub(crate) fn preflight(args: &Map<String, Value>) -> Result<Value, Value> {
    let name = required(args, "kind")?;
    let Some(kind) = KINDS.iter().find(|entry| entry.kind == name) else {
        return Ok(
            json!({"status":"error","mutation_performed":false,"error":format!("Unsupported Site lifecycle transformation: \"{name}\""),"allowed_kinds":KINDS.iter().map(|entry|entry.kind).collect::<Vec<_>>() }),
        );
    };
    let source = text(args, "source_site");
    let target = text(args, "target_site");
    let mode = text(args, "authority_mode");
    let checks = vec![
        check(
            "source_site_declared",
            source.is_some() || !kind.source_required,
            source
                .as_deref()
                .map(|v| format!("Source Site: {v}"))
                .unwrap_or_else(|| "Source Site is required".to_string()),
            format!("Provide source_site for {}", kind.kind),
        ),
        check(
            "target_site_declared",
            target.is_some() || !kind.target_required,
            target
                .as_deref()
                .map(|v| format!("Target Site: {v}"))
                .unwrap_or_else(|| {
                    if kind.target_required {
                        "Target Site is required".to_string()
                    } else {
                        "Target Site is not required".to_string()
                    }
                }),
            format!("Provide target_site for {}", kind.kind),
        ),
        check(
            "authority_mode_declared",
            mode.is_some(),
            mode.as_deref()
                .map(|v| format!("Authority mode: {v}"))
                .unwrap_or_else(|| "Authority mode is required".to_string()),
            format!("Choose one of: {}", kind.authority_modes.join(", ")),
        ),
        check(
            "authority_mode_supported",
            mode.as_deref()
                .is_some_and(|v| kind.authority_modes.contains(&v)),
            mode.as_deref()
                .map(|v| {
                    format!(
                        "Authority mode {v} {} supported",
                        if kind.authority_modes.contains(&v) {
                            "is"
                        } else {
                            "is not"
                        }
                    )
                })
                .unwrap_or_else(|| "Authority mode was not provided".to_string()),
            format!("Choose one of: {}", kind.authority_modes.join(", ")),
        ),
    ];
    let ready = checks.iter().all(|entry| entry["status"] == "pass");
    Ok(
        json!({"status":if ready{"ready"}else{"blocked"},"mutation_performed":false,"kind":kind.kind,"purpose":kind.purpose,"source_site":source,"target_site":target,"authority_mode":mode,"required_artifacts":kind.artifacts,"checks":checks,"next_step":if ready{"Create a governed transformation plan artifact before any Site filesystem, registry, config, inbox, task, or authority mutation."}else{"Resolve failed checks before creating a transformation plan."}}),
    )
}

pub(crate) fn relation_list(args: &Map<String, Value>, bound_root: &Path) -> Result<Value, Value> {
    let root = requested_root(args, bound_root)?;
    let registry_path = root.join(".ai/site-relation-registry.json");
    let relations = read_relations(&registry_path)?;
    let limit = integer(args, "limit", 20, 1, 500)? as usize;
    let filtered = relations
        .into_iter()
        .filter(|relation| {
            matches_filter(relation, args, "relation_kind", "kind")
                && matches_filter(relation, args, "source_site_ref", "source_site")
                && matches_filter(relation, args, "target_site_ref", "target_site")
                && matches_filter(relation, args, "status", "status")
        })
        .take(limit)
        .collect::<Vec<_>>();
    Ok(
        json!({"status":"success","mutation_performed":false,"registry_path":registry_path.to_string_lossy(),"count":filtered.len(),"limit":limit,"relations":filtered}),
    )
}

pub(crate) fn relation_validate(
    args: &Map<String, Value>,
    bound_root: &Path,
) -> Result<Value, Value> {
    let root = requested_root(args, bound_root)?;
    let registry_path = root.join(".ai/site-relation-registry.json");
    let relations = read_relations(&registry_path)?;
    let active = relations
        .iter()
        .filter(|relation| relation["status"] == "active")
        .collect::<Vec<_>>();
    let mut issues = Vec::new();
    for relation in &active {
        let id = relation["relation_id"].as_str().unwrap_or_default();
        for (field, code, message) in [
            (
                "source_site_ref",
                "missing_source_site",
                "Relation source_site_ref is required.",
            ),
            (
                "target_site_ref",
                "missing_target_site",
                "Relation target_site_ref is required.",
            ),
            (
                "authority_effect",
                "missing_authority_effect",
                "Relation authority_effect is required.",
            ),
        ] {
            if relation[field]
                .as_str()
                .map(str::trim)
                .unwrap_or_default()
                .is_empty()
            {
                issues.push(
                    json!({"relation_id":id,"severity":"error","code":code,"message":message}),
                );
            }
        }
        if let Some(reciprocal) = relation["reciprocal_relation_id"].as_str() {
            if !reciprocal.is_empty()
                && !active
                    .iter()
                    .any(|candidate| candidate["relation_id"] == reciprocal)
            {
                issues.push(json!({"relation_id":id,"severity":"error","code":"missing_named_reciprocal","message":format!("Reciprocal relation is not active: {reciprocal}")}));
            }
        }
        if relation["reciprocal_required"] == true && !has_reciprocal(relation, &active) {
            issues.push(json!({"relation_id":id,"severity":"error","code":"missing_required_reciprocal","message":format!("Missing active reciprocal relation {} -> {}.",relation["target_site_ref"].as_str().unwrap_or_default(),relation["source_site_ref"].as_str().unwrap_or_default())}));
        }
    }
    let valid = issues.is_empty();
    Ok(
        json!({"status":if valid{"success"}else{"error"},"mutation_performed":false,"registry_path":registry_path.to_string_lossy(),"relation_count":relations.len(),"valid":valid,"issues":issues}),
    )
}

pub(crate) fn authority_preflight(
    args: &Map<String, Value>,
    bound_root: &Path,
) -> Result<Value, Value> {
    let root = requested_root(args, bound_root)?;
    let family = text(args, "mutation_family").unwrap_or_else(|| "task_lifecycle".to_string());
    let supported =
        ["task_lifecycle", "inbox", "publication", "secret", "site"].contains(&family.as_str());
    let files = json!({"task_lifecycle_db":root.join(".ai/task-lifecycle.db").is_file(),"task_snapshot":root.join(".ai/task-lifecycle-snapshot.json").is_file(),"tasks_dir":root.join(".ai/do-not-open/tasks").is_dir(),"inbox_db":root.join(".ai/inbox.db").is_file(),"inbox_exports":root.join(".ai/inbox-envelopes").is_dir(),"publication_dir":root.join(".ai/repo-publications").is_dir(),"site_config":root.join("config.json").is_file()||root.join(".narada-site.json").is_file(),"read_only_marker":root.join(".ai/read-only-embodiment.json").is_file()});
    let read_only = files["read_only_marker"] == true;
    let has_authority = [
        "task_lifecycle_db",
        "task_snapshot",
        "tasks_dir",
        "inbox_db",
        "inbox_exports",
        "publication_dir",
        "site_config",
    ]
    .iter()
    .any(|key| files[*key] == true);
    let repo = git_posture(&root);
    let behind = repo
        .as_ref()
        .and_then(|v| v["behind"].as_i64())
        .unwrap_or(0)
        > 0;
    let (locus, safety, next, reason) = if !supported {
        (
            "unsupported",
            "inspect_only",
            "Use a supported mutation_family.",
            format!("Unsupported mutation family: {family}."),
        )
    } else if read_only {
        ("read_only_embodiment","refuse","Run this mutation at the declared authority locus, or submit an inbox observation from this embodiment.","This checkout declares itself as a read-only embodiment.".to_string())
    } else if behind {
        ("stale_clone","inspect_only","git pull --ff-only && narada mutation-evidence reconcile --apply","The local branch is behind its upstream; mutation would risk writing against stale authority.".to_string())
    } else if has_authority {
        (
            "authority_locus",
            "allowed_with_command",
            recommended(&family),
            "Authority-bearing Narada state surfaces are present at this locus.".to_string(),
        )
    } else {
        (
            "unknown",
            "refuse",
            "Run authority preflight at the authority Site.",
            "No authority-bearing Narada state surface was found.".to_string(),
        )
    };
    Ok(
        json!({"status":"success","cwd":root.to_string_lossy(),"mutation_family":family,"locus_state":locus,"mutation_safety":safety,"next_safe_command":next,"reason":reason,"repo":repo,"authority_files":files,"embodiments":[],"embodiment_warnings":[],"integration_hooks":integration_hooks()}),
    )
}

pub(crate) fn dependency_posture(bound_root: &Path) -> Result<Value, Value> {
    let executable =
        std::env::current_exe().map_err(io_error("native_executable_resolution_failed"))?;
    let packages = [
        "agent-cli",
        "mcp-transport",
        "task-governance-core",
        "task-lifecycle-mcp",
    ];
    let legacy_links = packages.iter().filter_map(|name| {
        let path = bound_root.join("node_modules/@narada-core").join(name);
        path.symlink_metadata().ok().map(|metadata| json!({"package_name":format!("@narada-core/{name}"),"path":path.to_string_lossy(),"is_symlink":metadata.file_type().is_symlink(),"is_directory":metadata.is_dir()}))
    }).collect::<Vec<_>>();
    Ok(
        json!({"schema":"narada.site_native_dependency_posture.v1","status":if legacy_links.is_empty(){"native_self_contained"}else{"legacy_links_present"},"implementation":"rust-native","runtime_dependencies":[],"node_required":false,"bun_required":false,"typescript_required":false,"native_executable":executable.to_string_lossy(),"site_root":bound_root.to_string_lossy(),"legacy_package_links":legacy_links,"legacy_package_link_count":legacy_links.len(),"mutation_performed":false,"next_action":if legacy_links.is_empty(){Value::Null}else{Value::String("Review and remove legacy node_modules links through an explicit filesystem authority after confirming no legacy runtime consumes them.".to_string())}}),
    )
}

pub(crate) fn retired_dependency_sync(bound_root: &Path) -> Value {
    json!({"schema":"narada.site_deps_sync.retired.v1","status":"retired","implementation":"rust-native","mutation_attempted":false,"mutation_performed":false,"site_root":bound_root.to_string_lossy(),"reason":"legacy_node_package_link_synchronization_removed_from_native_runtime","replacement_tool":"site_dependency_posture","node_modules_modified":false,"remediation":"Call site_dependency_posture. Native MCP surfaces are self-contained and do not synchronize JavaScript package links."})
}

