fn build_bind_config(
    _contract: &Value,
    site: &Value,
    surface: &Value,
    projection: &Value,
    runtime_kind: Option<&str>,
    server_key: &str,
) -> Result<Value, String> {
    let site_id = site["site_id"].as_str().unwrap_or("");
    let surface_id = surface["id"].as_str().unwrap_or("");
    let root = canonical_root(PathBuf::from(site["root"].as_str().unwrap_or("")));
    let workspace = site_workspace_root(site);
    let source_args = projection
        .get("args")
        .and_then(Value::as_array)
        .or_else(|| surface["args"].as_array())
        .cloned()
        .unwrap_or_default();
    let mut child_args = source_args
        .iter()
        .filter_map(Value::as_str)
        .map(|value| interpolate(value, site_id, &root, &workspace))
        .collect::<Vec<_>>();
    append_durable_delegation_allowed_roots(surface_id, &root, &mut child_args)?;
    if projection["id"] == "user-site-operator" {
        child_args.extend(
            [
                "--projection",
                "user-site-operator",
                "--user-site-root",
                &path_text(&user_site_root()),
                "--source-kind",
                "operator",
                "--operator-id",
                &default_operator_id(),
            ]
            .map(str::to_string),
        );
    }
    let entrypoint_template = projection["entrypoint"]
        .as_str()
        .or_else(|| surface["entrypoint"].as_str())
        .unwrap_or("");
    let child_entrypoint = canonical_root(PathBuf::from(interpolate(
        entrypoint_template,
        site_id,
        &root,
        &workspace,
    )));
    let implementation = site
        .pointer(&format!(
            "/surface_overrides/{surface_id}/surface_implementation"
        ))
        .and_then(Value::as_str);
    let launch = site_launch(
        surface_id,
        projection,
        implementation,
        &path_text(&child_entrypoint),
        &child_args,
    )?;
    let exposed = projection["exposed_tools"]
        .as_array()
        .cloned()
        .unwrap_or_else(|| surface["tools"].as_array().cloned().unwrap_or_default());
    let scope = scope_metadata(projection, &root);
    let mut envs = surface["env_vars"].as_array().cloned().unwrap_or_default();
    envs.extend(
        projection["env_vars"]
            .as_array()
            .cloned()
            .unwrap_or_default(),
    );
    let envs = unique(envs.iter().filter_map(Value::as_str));
    let projection_metadata = projection_metadata(surface, projection, runtime_kind);
    Ok(
        json!({"schema":"narada.mcp.client_config.v0","site_id":site_id,"description":format!("{} MCP surface bound by registrar.",surface["package"].as_str().unwrap_or("")),"mcpServers":{server_key:{"transport":"stdio","command":launch.0,"args":launch.1,"tools":exposed,"env_vars":envs,"surface_id":surface_id,"projection_id":projection["id"],"surface_projection":projection_metadata,"authority_posture":if scope["injection_scope"]=="local_site"{"site_local_mcp_surface"}else{"injected_mcp_surface"},"injection_scope":scope["injection_scope"],"authority_locus":scope["authority_locus"],"mutation_locus":scope["mutation_locus"],"restart_owner":scope["restart_owner"],"bound_into_site":site_id,"narada_scope":{"injection_scope":scope["injection_scope"],"authority_locus":scope["authority_locus"],"mutation_locus":scope["mutation_locus"],"restart_owner":scope["restart_owner"],"bound_into_site":site_id,"scope_source":"registrar_surface_catalog"}}}}),
    )
}

fn site_workspace_root(site: &Value) -> PathBuf {
    let config = site["config_path"]
        .as_str()
        .and_then(|path| fs::read_to_string(path).ok())
        .and_then(|text| serde_json::from_str::<Value>(&text).ok());
    let configured = config.as_ref().and_then(|value| {
        value["workspace_root"].as_str().or_else(|| {
            value
                .pointer("/site/workspace_root")
                .and_then(Value::as_str)
        })
    });
    canonical_root(PathBuf::from(
        configured.unwrap_or_else(|| site["root"].as_str().unwrap_or("")),
    ))
}

fn append_durable_delegation_allowed_roots(
    surface_id: &str,
    site_root: &Path,
    child_args: &mut Vec<String>,
) -> Result<(), String> {
    if !matches!(surface_id, "worker-delegation" | "delegated-task") {
        return Ok(());
    }
    let extras = durable_extra_allowed_roots(site_root)?;
    if extras.is_empty() {
        return Ok(());
    }
    let site_root_key = comparable_root(site_root);
    let mut filtered = Vec::with_capacity(child_args.len());
    let mut index = 0;
    while index < child_args.len() {
        if index + 1 < child_args.len()
            && child_args[index] == "--allowed-root"
            && comparable_root(Path::new(&child_args[index + 1])) == site_root_key
        {
            index += 2;
            continue;
        }
        filtered.push(child_args[index].clone());
        index += 1;
    }
    *child_args = filtered;
    let existing = child_args
        .windows(2)
        .filter(|pair| pair[0] == "--allowed-root")
        .map(|pair| comparable_root(Path::new(&pair[1])))
        .collect::<std::collections::BTreeSet<_>>();
    for extra in extras {
        if existing.contains(&comparable_root(&extra)) {
            continue;
        }
        child_args.push("--allowed-root".to_string());
        child_args.push(path_text(&extra));
    }
    Ok(())
}

fn durable_extra_allowed_roots(site_root: &Path) -> Result<Vec<PathBuf>, String> {
    let path = site_root.join(".narada").join("allowed-roots.json");
    if !path.exists() {
        return Ok(Vec::new());
    }
    let bytes = fs::read(&path).map_err(|error| {
        format!(
            "registrar_allowed_roots_read_failed:{}:{error}",
            path_text(&path)
        )
    })?;
    let document: Value = serde_json::from_slice(&bytes).map_err(|error| {
        format!(
            "registrar_allowed_roots_invalid_json:{}:{error}",
            path_text(&path)
        )
    })?;
    let Some(entries) = document.get("extra_allowed_roots") else {
        return Ok(Vec::new());
    };
    let entries = entries.as_array().ok_or_else(|| {
        format!(
            "registrar_allowed_roots_invalid_extra_allowed_roots:{}",
            path_text(&path)
        )
    })?;
    let mut roots = Vec::new();
    let mut seen = std::collections::BTreeSet::new();
    for (index, entry) in entries.iter().enumerate() {
        let value = entry
            .as_str()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| {
                format!(
                    "registrar_allowed_roots_invalid_entry:{}:{index}",
                    path_text(&path)
                )
            })?;
        let root = PathBuf::from(value);
        if !root.is_absolute() {
            return Err(format!(
                "registrar_allowed_roots_entry_not_absolute:{}:{index}",
                path_text(&path)
            ));
        }
        let root = canonical_root(root);
        if seen.insert(comparable_root(&root)) {
            roots.push(root);
        }
    }
    Ok(roots)
}

fn interpolate(value: &str, site_id: &str, root: &Path, workspace: &Path) -> String {
    let control = if root
        .file_name()
        .is_some_and(|name| name.to_string_lossy().eq_ignore_ascii_case(".narada"))
    {
        root.to_path_buf()
    } else {
        root.join(".narada")
    };
    let user_root = user_site_root();
    let user_control = user_root.join(".narada");
    value
        .replace(
            "{mcp_surfaces_root}",
            &workspace_repo_root()
                .map(|root| path_text(&root.join("packages")))
                .unwrap_or_default(),
        )
        .replace("{site_root}", &path_text(root))
        .replace("{site_control_root}", &path_text(&control))
        .replace("{user_site_root}", &path_text(&user_root))
        .replace("{user_site_control_root}", &path_text(&user_control))
        .replace("{site_runtime_root}", &path_text(&control.join("runtime")))
        .replace("{workspace_root}", &path_text(workspace))
        .replace("{site_id}", site_id)
}
fn default_operator_id() -> String {
    user_site_root()
        .parent()
        .and_then(Path::file_name)
        .and_then(|value| value.to_str())
        .unwrap_or("operator")
        .to_ascii_lowercase()
}
fn workspace_repo_root() -> Option<PathBuf> {
    let executable = env::current_exe().ok()?;
    executable
        .ancestors()
        .find(|root| root.join("packages").join("mcp-registrar").exists())
        .map(Path::to_path_buf)
}

fn runtime_implementation_matrix_path(workspace: &Path) -> Result<PathBuf, String> {
    const MATRIX_RELATIVE_PATH: &str =
        "narada/packages/operator-surface-runtime-contract/contracts/runtime-implementation-matrix.json";
    workspace
        .ancestors()
        .find(|candidate| candidate.join(MATRIX_RELATIVE_PATH).is_file())
        .map(|candidate| candidate.join(MATRIX_RELATIVE_PATH))
        .ok_or_else(|| {
            format!(
                "registrar_runtime_matrix_unavailable:{}",
                path_text(workspace)
            )
        })
}

fn scope_metadata(projection: &Value, root: &Path) -> Value {
    let injection = projection["injection_scope"]
        .as_str()
        .unwrap_or("local_site");
    let locus = if injection == "host" {
        json!({"kind":"host"})
    } else if injection == "user_site" {
        json!({"kind":"user_site","site_root":path_text(&user_site_root())})
    } else {
        json!({"kind":"local_site","site_root":path_text(root)})
    };
    json!({"injection_scope":injection,"authority_locus":locus,"mutation_locus":locus,"restart_owner":projection["restart_owner"].as_str().unwrap_or(injection)})
}
