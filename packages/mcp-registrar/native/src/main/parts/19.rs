fn projection_metadata(surface: &Value, projection: &Value, runtime_kind: Option<&str>) -> Value {
    let tools = projection["exposed_tools"]
        .as_array()
        .cloned()
        .unwrap_or_else(|| surface["tools"].as_array().cloned().unwrap_or_default());
    let descriptor = &surface["descriptor"];
    let mut value = json!({"surface_id":surface["id"],"projection_id":projection["id"],"injection_scope":projection["injection_scope"],"runtime_requirements":projection.get("runtime_requirements").cloned().unwrap_or_else(||json!([])),"exposed_tools":tools,"execution":projection["execution"],"descriptor_digest":surface["descriptor_digest"],"tool_contract_digest":surface["tool_contract_digest"],"surface_descriptor":descriptor});
    for key in ["default_injection"] {
        if let Some(item) = projection.get(key) {
            value
                .as_object_mut()
                .unwrap()
                .insert(key.into(), item.clone());
        }
    }
    if let Some(kind) = runtime_kind {
        value
            .as_object_mut()
            .unwrap()
            .insert("runtime_kind".into(), json!(kind));
    }
    if let Some(lifecycle) = descriptor["projections"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|candidate| candidate["id"] == projection["id"])
        .and_then(|candidate| candidate.get("lifecycle"))
    {
        value
            .as_object_mut()
            .unwrap()
            .insert("lifecycle".into(), lifecycle.clone());
    }
    value
}

fn site_launch(
    surface_id: &str,
    projection: &Value,
    implementation: Option<&str>,
    entrypoint: &str,
    args: &[String],
) -> Result<(String, Vec<String>), String> {
    let component = component_kind(surface_id);
    let engine = runtime_engine(&component, implementation)?;
    let proxy = native_proxy_entrypoint().ok_or("registrar_native_runtime_proxy_missing")?;
    let mut effective_command = if engine == "rust" {
        projection["command"]
            .as_str()
            .filter(|value| !value.trim().is_empty())
            .ok_or("registrar_native_projection_command_missing")?
            .to_string()
    } else {
        return Err(format!(
            "registrar_non_native_runtime_retired:{engine}"
        ));
    };
    let mut effective_entrypoint = entrypoint.to_string();
    let mut effective_args = args.to_vec();
    let mut invocation = None;
    let mut applet = None;
    let shared = [
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
    .contains(&surface_id);
    if engine == "rust" {
        if ["local-filesystem", "structured-command", "git"].contains(&surface_id) {
            effective_command = proxy.clone();
            effective_entrypoint = proxy.clone();
            invocation = Some("native_applet");
            applet = Some(if surface_id == "local-filesystem" {
                "filesystem"
            } else {
                surface_id
            });
        } else if surface_id == "mcp-loader" {
            let path = native_artifact_entrypoint("mcp-loader-mcp", "narada-mcp-loader.exe")
                .ok_or("registrar_native_mcp_loader_missing")?;
            effective_command = path.clone();
            effective_entrypoint = path;
            invocation = Some("native_entrypoint");
        } else if surface_id == "task-lifecycle" || surface_id == "work-lifecycle" {
            let artifact = if surface_id == "task-lifecycle" {
                "narada-task-lifecycle-mcp.exe"
            } else {
                "narada-work-lifecycle-mcp.exe"
            };
            let path = native_artifact_entrypoint("shared/mcp-lifecycle-native", artifact)
                .ok_or("registrar_native_lifecycle_missing")?;
            effective_command = path.clone();
            effective_entrypoint = path;
            invocation = Some("native_entrypoint");
        } else if surface_id == "agent-context" && projection["id"] == "default" {
            let path =
                native_artifact_entrypoint("agent-context-mcp", "narada-agent-context-mcp.exe")
                    .ok_or("registrar_native_agent_context_missing")?;
            effective_command = path.clone();
            effective_entrypoint = path;
            invocation = Some("native_entrypoint");
        } else if surface_id == "epistemic-graph" {
            let path = native_artifact_entrypoint("ledger-domain-mcp", "narada-ledger-domain.exe")
                .ok_or("registrar_native_ledger_domain_missing")?;
            effective_command = path.clone();
            effective_entrypoint = path;
            invocation = Some("native_entrypoint");
        } else if shared {
            let path =
                native_artifact_entrypoint("shared/mcp-surfaces-native", "narada-mcp-surfaces.exe")
                    .ok_or("registrar_native_shared_surface_missing")?;
            effective_command = path.clone();
            effective_entrypoint = path;
            effective_args = native_shared_args(surface_id, args);
            invocation = Some("native_entrypoint");
        } else if surface_id == "mcp-registrar" {
            let path = native_artifact_entrypoint("mcp-registrar", "narada-mcp-registrar.exe")
                .ok_or("registrar_native_registrar_missing")?;
            effective_command = path.clone();
            effective_entrypoint = path;
            invocation = Some("native_entrypoint");
        }
    }
    let mut proxy_args = vec![
        "proxy".into(),
        "--surface-id".into(),
        surface_id.into(),
        "--child-command".into(),
        effective_command,
        "--artifact-manifest".into(),
        workspace_repo_root()
            .map(|root| {
                root.join(".ai/runtime/workspace-artifact-manifest.json")
                    .to_string_lossy()
                    .replace('\\', "/")
            })
            .unwrap_or_default(),
        "--runtime-contract-version".into(),
        CONTRACT_VERSION.to_string(),
        "--entrypoint".into(),
        effective_entrypoint,
    ];
    if let Some(kind) = invocation {
        proxy_args.extend(["--child-invocation-kind", kind].map(str::to_string));
        if kind == "native_applet" {
            proxy_args
                .extend(["--child-applet", applet.unwrap_or("filesystem")].map(str::to_string));
        }
    }
    proxy_args.push("--".into());
    proxy_args.extend(effective_args);
    Ok((proxy, proxy_args))
}

fn native_shared_args(surface_id: &str, args: &[String]) -> Vec<String> {
    let mut result = vec!["--surface-id".into(), surface_id.into()];
    if surface_id == "calendar" || surface_id == "graph-mail" {
        result.push("--native-authority".into())
    }
    let roots = [
        "--site-root",
        "--narada-root",
        "--feedback-root",
        "--output-root",
        "--user-site-root",
        "--repo-root",
        "--sop-root",
        "--task-root",
        "--allowed-root",
    ];
    let forwarded = [
        "--log-root",
        "--registry-path",
        "--projection-id",
        "--canonical-feedback-root",
        "--task-lifecycle-root",
        "--feedback-discovery-root",
        "--site-id",
        "--owned-surface-id",
        "--projection",
        "--source-kind",
        "--operator-id",
        "--run-root",
        "--sops-dir",
        "--provider-registry-path",
        "--server-name",
    ];
    let mut index = 0;
    while index < args.len() {
        let key = &args[index];
        if (roots.contains(&key.as_str()) || forwarded.contains(&key.as_str()))
            && index + 1 < args.len()
            && !args[index + 1].starts_with("--")
        {
            result.push(key.clone());
            result.push(args[index + 1].clone());
            index += 2
        } else {
            index += 1
        }
    }
    result
}
fn component_kind(surface: &str) -> String {
    match surface {
        "mcp-loader" => "mcp-loader-mcp",
        "local-filesystem" => "filesystem-mcp",
        "structured-command" => "structured-command-mcp",
        "git" => "git-mcp",
        "agent-context" => "agent-context-mcp",
        "mcp-registrar" => "mcp-registrar",
        "task-lifecycle" => "task-lifecycle-mcp",
        "work-lifecycle" => "work-lifecycle-mcp",
        value => return format!("{value}-mcp"),
    }
    .into()
}
fn runtime_engine(component: &str, implementation: Option<&str>) -> Result<String, String> {
    if implementation == Some("js") {
        return Err("registrar_legacy_javascript_runtime_retired".into());
    }
    let workspace = workspace_repo_root().ok_or("registrar_workspace_root_unavailable")?;
    let path = runtime_implementation_matrix_path(&workspace)?;
    let matrix: Value = serde_json::from_str(
        &fs::read_to_string(&path)
            .map_err(|error| format!("registrar_runtime_matrix_read_failed:{}:{error}", path_text(&path)))?,
    )
    .map_err(|error| format!("registrar_runtime_matrix_invalid:{}:{error}", path_text(&path)))?;
    let row = matrix["rows"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|row| row["component_kind"] == component)
        .ok_or_else(|| format!("registrar_runtime_implementation_unavailable:{component}"))?;
    let engine = if implementation == Some("native") {
        "rust"
    } else {
        row.pointer("/profile_runtime_engine_kinds/native")
            .and_then(Value::as_str)
            .ok_or_else(|| format!("registrar_runtime_profile_engine_missing:{component}"))?
    };
    if engine != "rust" {
        return Err(format!("registrar_non_native_runtime_retired:{engine}"));
    }
    if row
        .pointer(&format!("/implementations/{engine}/status"))
        .and_then(Value::as_str)
        != Some("admitted")
    {
        return Err(format!(
            "registrar_runtime_implementation_unavailable:{component}"
        ));
    }
    Ok(engine.into())
}

