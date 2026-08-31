use crate::full::*;

pub(crate) fn run_server(options: Options) -> Result<(), Diagnostic> {
    let binding_admission = load_binding_admission(&options)?;
    let executable = env::current_exe()
        .map_err(|error| Diagnostic::new("runtime_path_unavailable", error.to_string()))?;
    let native_dir = executable
        .parent()
        .and_then(Path::parent)
        .and_then(Path::parent)
        .ok_or_else(|| Diagnostic::new("runtime_path_unavailable", "runtime_path_unavailable"))?;
    let package_root = native_dir
        .parent()
        .and_then(Path::parent)
        .unwrap_or(native_dir);
    let surface_root = normalize_path(
        &package_root
            .parent()
            .unwrap_or(package_root)
            .to_string_lossy(),
    );
    let workspace_root = normalize_path(
        &Path::new(&surface_root)
            .parent()
            .unwrap_or(Path::new(&surface_root))
            .to_string_lossy(),
    );
    let started_ms = now_ms();
    let run_id = new_id("loader");
    let owner_pid = std::process::id();
    let ownership_marker = format!("narada.mcp.loader/{}", run_id);
    let policy = build_policy(&options, &surface_root, &workspace_root);
    let standalone_ambient_attachment = options.standalone_ambient_attachment;
    let mut state = LoaderState {
        policy,
        surface_root,
        workspace_root,
        started_ms,
        run_id,
        owner_pid,
        ownership_marker,
        schema_lease_secret: new_id("schema-lease-secret"),
        connections: HashMap::new(),
        handles: HashMap::new(),
        binding_admission,
        standalone_ambient_attachment,
    };
    let stdin = io::stdin();
    let mut reader = WireReader::new(stdin.lock(), state.policy.max_request_bytes);
    let stdout = io::stdout();
    let mut writer = stdout.lock();
    while let Some((request, framed)) = reader.next().map_err(|error| {
        Diagnostic::new(
            "parent_read_failed",
            format!("parent_read_failed:{}", error),
        )
    })? {
        let method = request
            .get("method")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if request.get("id").is_none() && method.starts_with("notifications/") {
            continue;
        }
        let id = request.get("id").cloned().unwrap_or(Value::Null);
        let response = match dispatch(&request, &mut state) {
            Ok(result) => json!({"jsonrpc":"2.0","id":id,"result":result}),
            Err(error) => {
                json!({"jsonrpc":"2.0","id":id,"error":{"code":-32000,"message":error.message,"data":error.value()}})
            }
        };
        write_wire(&mut writer, &response, framed).map_err(|error| {
            Diagnostic::new(
                "parent_write_failed",
                format!("parent_write_failed:{}", error),
            )
        })?;
    }
    let connections = std::mem::take(&mut state.connections);
    for (_, connection) in connections {
        connection.session.terminate();
    }
    Ok(())
}

pub(crate) fn build_policy(options: &Options, surface_root: &str, workspace_root: &str) -> Policy {
    let user_profile = env::var("USERPROFILE").or_else(|_| env::var("HOME")).ok();
    let mut site_roots = options
        .allowed_site_roots
        .clone()
        .unwrap_or_else(|| vec![workspace_root.to_string()]);
    if let Ok(configured) = env::var("NARADA_MCP_ALLOWED_SITE_ROOTS") {
        site_roots.extend(configured.split(',').filter_map(optional_str));
    }
    if let Some(profile) = user_profile.as_deref() {
        site_roots.push(normalize_path(&join_path(profile, "Narada")));
    }
    let mut prefixes = options
        .allowed_entrypoint_prefixes
        .clone()
        .unwrap_or_else(|| vec![surface_root.to_string(), "{site_root}/tools/".to_string()]);
    if let Ok(configured) = env::var("NARADA_MCP_ALLOWED_ENTRYPOINT_PREFIXES") {
        prefixes.extend(configured.split(',').filter_map(optional_str));
    }
    if let Some(profile) = user_profile.as_deref() {
        prefixes.push(normalize_path(&join_path(profile, "Narada/tools")));
    }
    let mut allowed_prefixes: Vec<String> = prefixes
        .into_iter()
        .map(|prefix| normalize_policy_prefix(&prefix))
        .collect();
    allowed_prefixes.sort_by_key(|value| std::cmp::Reverse(value.len()));
    allowed_prefixes.dedup();
    let mut allowed_site_roots: Vec<String> = site_roots
        .into_iter()
        .map(|value| normalize_path(&value))
        .collect();
    allowed_site_roots.sort_by_key(|value| {
        if cfg!(windows) {
            value.to_lowercase()
        } else {
            value.clone()
        }
    });
    allowed_site_roots.dedup_by(|left, right| {
        if cfg!(windows) {
            left.to_lowercase() == right.to_lowercase()
        } else {
            left == right
        }
    });
    Policy {
        allowed_site_roots,
        allowed_entrypoint_prefixes: allowed_prefixes,
        allowed_surface_ids: options.allowed_surface_ids.clone(),
        allowed_env_vars: options.allowed_env_vars.clone().unwrap_or_else(|| {
            vec![
                "USERPROFILE",
                "HOME",
                "NODE_OPTIONS",
                "PATH",
                "PROCESSOR_ARCHITECTURE",
                "SystemRoot",
                "NARADA_AGENT_ID",
                "NARADA_OPERATOR_ID",
                "NARADA_NARS_SESSION_SOURCE_KIND",
                "NARADA_CARRIER_SESSION_ID",
                "NARADA_SITE_ID",
                "NARADA_ROOT",
                "NARADA_SRC_ROOT",
            ]
            .into_iter()
            .map(String::from)
            .collect()
        }),
        max_connections: options.max_connections,
        max_request_bytes: options.max_request_bytes,
        max_response_bytes: options.max_response_bytes,
        attach_timeout_ms: options.attach_timeout_ms,
        tool_call_timeout_ms: options.tool_call_timeout_ms,
        tool_call_grace_ms: options.tool_call_grace_ms,
    }
}
