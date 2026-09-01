fn registry_surface(
    contract: &Value,
    site: &Value,
    server_name: &str,
    server: &Value,
    file: &str,
) -> Result<Value, String> {
    let site_id = site["site_id"].as_str().unwrap_or("");
    let prefix = if site_id == "andrey-user" {
        "narada-site-andrey-user".to_string()
    } else if site_id.starts_with("narada-") {
        site_id.to_string()
    } else {
        format!("narada-{site_id}")
    };
    let inferred = server_name
        .strip_prefix(&(prefix + "-"))
        .unwrap_or(server_name);
    let surface_id = server["surface_id"].as_str().unwrap_or(inferred);
    let items = contract
        .pointer("/read_models/registrar_surface_list/items")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let catalog = items
        .iter()
        .find(|surface| surface["id"] == surface_id)
        .cloned()
        .or_else(|| embedded_site_local_catalog(server, surface_id))
        .ok_or_else(|| format!("registrar_site_local_descriptor_missing:{surface_id}"))?;
    let projection_id = server["projection_id"]
        .as_str()
        .or_else(|| {
            server
                .pointer("/surface_projection/projection_id")
                .and_then(Value::as_str)
        })
        .unwrap_or("default");
    let projection = catalog["projections"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|projection| projection["id"] == projection_id)
        .ok_or_else(|| {
            format!("registrar_unknown_surface_projection:{surface_id}:{projection_id}")
        })?;
    let tool_source = projection
        .get("exposed_tools")
        .filter(|value| value.is_array())
        .unwrap_or(&catalog["tools"]);
    let registered = unique(
        tool_source
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str),
    );
    let descriptor = &catalog["descriptor"];
    let mut read_only = vec![];
    let mut refused = vec![];
    for tool in descriptor["tools"].as_array().into_iter().flatten() {
        let Some(name) = tool["name"].as_str() else {
            continue;
        };
        if !registered.iter().any(|value| value == name) {
            continue;
        }
        if tool.pointer("/effect/class").and_then(Value::as_str) == Some("read")
            || tool
                .pointer("/annotations/readOnlyHint")
                .and_then(Value::as_bool)
                == Some(true)
        {
            read_only.push(name.to_string());
        }
        if tool
            .pointer("/annotations/legacy_policy")
            .and_then(Value::as_str)
            == Some("refused")
        {
            refused.push(name.to_string());
        }
    }
    let mut classified = read_only.clone();
    classified.extend(refused.clone());
    let mutating = registered
        .iter()
        .filter(|name| !classified.contains(name))
        .cloned()
        .collect::<Vec<_>>();
    let raw_command = server["command"].as_str().unwrap_or("node").to_string();
    let mut raw_args = server["args"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(str::to_string)
        .collect::<Vec<_>>();
    let mut launch = unwrap_launch(&raw_command, &raw_args);
    if matches!(
        launch.invocation.as_deref(),
        Some("native_applet" | "native_entrypoint")
    ) {
        if let Some(canonical) = canonical_native_surface_entrypoint(surface_id, projection_id) {
            for flag in ["--child-command", "--entrypoint"] {
                if let Some(index) = raw_args.iter().position(|value| value == flag) {
                    if let Some(value) = raw_args.get_mut(index + 1) {
                        *value = canonical.clone();
                    }
                }
            }
            launch.entrypoint = canonical.clone();
            launch.child_command = canonical;
        }
    }
    let runtime_kind = if matches!(
        launch.invocation.as_deref(),
        Some("native_applet" | "native_entrypoint")
    ) {
        "rust-stdio"
    } else if executable_name(&launch.child_command) == "bun" {
        "bun-stdio"
    } else {
        "node-stdio"
    };
    let mut surface_projection = json!({"surface_id":surface_id,"projection_id":projection_id,"injection_scope":projection["injection_scope"],"runtime_requirements":projection.get("runtime_requirements").cloned().unwrap_or_else(||json!([])),"exposed_tools":registered,"execution":projection["execution"],"descriptor_digest":catalog["descriptor_digest"],"tool_contract_digest":catalog["tool_contract_digest"],"surface_descriptor":descriptor});
    if let Some(value) = projection.get("default_injection") {
        surface_projection
            .as_object_mut()
            .unwrap()
            .insert("default_injection".into(), value.clone());
    }
    if let Some(value) = server.pointer("/surface_projection/runtime_kind") {
        surface_projection
            .as_object_mut()
            .unwrap()
            .insert("runtime_kind".into(), value.clone());
    }
    if let Some(value) = descriptor["projections"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|candidate| candidate["id"] == projection_id)
        .and_then(|candidate| candidate.get("lifecycle"))
    {
        surface_projection
            .as_object_mut()
            .unwrap()
            .insert("lifecycle".into(), value.clone());
    }
    let transport_command = if launch.proxied {
        native_proxy_entrypoint().unwrap_or(raw_command.clone())
    } else {
        raw_command.clone()
    };
    let transport_args = if !launch.proxied && raw_args.is_empty() {
        vec![String::new()]
    } else {
        raw_args
    };
    Ok(
        json!({"surface_id":format!("{server_name}.local"),"surface_projection":surface_projection,"surface_type":catalog["kind"],"display_name":server_name,"server_name":server_name,"runtime_binding":{"runtime_kind":runtime_kind,"proxy_implementation":if launch.proxied{json!("native")}else{Value::Null},"entrypoint":launch.entrypoint,"owner_site_id":site_id,"transport":{"type":"stdio","command":transport_command,"args":transport_args}},"authority_boundary":{"posture":"registrar_generated_runtime_surface_registry","grants_tool_authority":true,"granted_tool_authority_kind":"declared_enabled_mcp_surface_tools","source":"site_mcp_fabric_and_registrar_catalog"},"client_config":{"generated_path":format!(".ai/mcp/{file}"),"generated_file":file},"tool_contract":{"exposed_tools":registered,"semantic_operations":[],"deprecated_aliases":{},"read_only_tools":read_only,"mutating_tools":mutating,"refused_tools":refused},"registered_live_tools":registered,"catalog_surface_id":descriptor["surface_id"],"evidence":{"source":"site_mcp_fabric","path":format!(".ai/mcp/{file}"),"projection_kind":"site_fabric"}}),
    )
}

fn canonical_native_surface_entrypoint(surface_id: &str, projection_id: &str) -> Option<String> {
    let (package, artifact) = native_surface_artifact(surface_id, projection_id)?;
    native_artifact_entrypoint(package, artifact)
}

fn native_surface_artifact(
    surface_id: &str,
    projection_id: &str,
) -> Option<(&'static str, &'static str)> {
    if ["local-filesystem", "structured-command", "git"].contains(&surface_id) {
        return Some(("shared/mcp-runtime-proxy", "narada-mcp-runtime.exe"));
    }
    match surface_id {
        "mcp-loader" => return Some(("mcp-loader-mcp", "narada-mcp-loader.exe")),
        "task-lifecycle" => {
            return Some((
                "shared/mcp-lifecycle-native",
                "narada-task-lifecycle-mcp.exe",
            ))
        }
        "work-lifecycle" => {
            return Some((
                "shared/mcp-lifecycle-native",
                "narada-work-lifecycle-mcp.exe",
            ))
        }
        "agent-context" if projection_id == "default" => {
            return Some(("agent-context-mcp", "narada-agent-context-mcp.exe"))
        }
        "epistemic-graph" => return Some(("ledger-domain-mcp", "narada-ledger-domain.exe")),
        "mcp-registrar" => return Some(("mcp-registrar", "narada-mcp-registrar.exe")),
        _ => {}
    }
    if [
        "catalog-observation",
        "operator-routing",
        "site-inbox",
        "site-lifecycle",
        "site-registry",
        "project-state",
        "runtime-introspection",
        "site-coherence",
        "launcher",
        "mailbox",
        "graph-mail",
        "calendar",
        "worker-delegation",
        "delegated-task",
        "sop",
        "scheduler",
        "surface-feedback",
        "speech",
        "artifacts",
        "nars-session",
        "quota-meter",
        "operator-console-overlay",
        "browser-control",
        "cloudflare-carrier",
    ]
    .contains(&surface_id)
    {
        return Some(("shared/mcp-surfaces-native", "narada-mcp-surfaces.exe"));
    }
    None
}

struct Launch {
    entrypoint: String,
    child_command: String,
    proxied: bool,
    invocation: Option<String>,
}
fn unwrap_launch(command: &str, args: &[String]) -> Launch {
    if args.first().map(String::as_str) == Some("proxy") {
        let value = |flag: &str| {
            args.iter()
                .position(|item| item == flag)
                .and_then(|index| args.get(index + 1))
                .cloned()
                .unwrap_or_default()
        };
        return Launch {
            entrypoint: value("--entrypoint"),
            child_command: value("--child-command"),
            proxied: true,
            invocation: Some(value("--child-invocation-kind")).filter(|value| !value.is_empty()),
        };
    }
    Launch {
        entrypoint: args.first().cloned().unwrap_or_default(),
        child_command: command.to_string(),
        proxied: false,
        invocation: None,
    }
}
fn executable_name(command: &str) -> String {
    Path::new(command)
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or(command)
        .to_ascii_lowercase()
}
fn native_proxy_entrypoint() -> Option<String> {
    native_artifact_entrypoint("shared/mcp-runtime-proxy", "narada-mcp-runtime.exe")
}
