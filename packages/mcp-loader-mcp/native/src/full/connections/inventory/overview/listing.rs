use crate::full::*;

pub(crate) fn list_site_surfaces(
    arguments: &JsonObject,
    state: &LoaderState,
) -> Result<Value, Diagnostic> {
    let site_root = normalize_path(&required_string(
        arguments,
        "site_root",
        "missing_site_root",
    )?);
    ensure_site_root_allowed(&site_root, &state.policy)?;
    assert_binding_admission_available(state)?;
    let bundle = read_site_fabric(&site_root)?;
    let servers = bundle
        .fabric
        .get("mcpServers")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    let site_id = bundle.fabric.get("site_id").and_then(Value::as_str);
    let include_runtime = arguments
        .get("include_runtime_metadata")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let mut surfaces = Vec::new();
    for (server_id, server) in servers {
        let surface_id = server
            .get("surface_id")
            .and_then(Value::as_str)
            .unwrap_or(&server_id)
            .to_string();
        let binding_id = canonical_binding_id(
            site_id,
            &surface_id,
            server.get("binding_id").and_then(Value::as_str),
        );
        if let Some(envelope) = &state.binding_admission {
            let discoverable = envelope
                .get("bindings")
                .and_then(Value::as_array)
                .is_some_and(|bindings| {
                    bindings.iter().any(|binding| {
                        binding.get("binding_id").and_then(Value::as_str)
                            == Some(binding_id.as_str())
                            && binding
                                .get("operations")
                                .and_then(Value::as_array)
                                .is_some_and(|ops| {
                                    ops.iter().any(|op| op.as_str() == Some("discover"))
                                })
                    })
                });
            if !discoverable {
                continue;
            }
        }
        let env_vars: Vec<String> = server
            .get("env")
            .and_then(Value::as_object)
            .map(|object| object.keys().cloned().collect())
            .unwrap_or_default();
        let requirements = surface_requirements(Some(&server));
        let mut surface = json!({
            "binding_id":if binding_id.is_empty(){Value::Null}else{json!(binding_id)},"surface_id":surface_id,"server_name":server_id,"command":server.get("command").cloned().unwrap_or(Value::Null),
            "args":server.get("args").cloned().unwrap_or_else(|| json!([])),"env_vars":env_vars,
            "runtime_requirements":requirements
        });
        if include_runtime {
            surface["runtime_lifecycle"] = runtime_lifecycle(None, None);
        }
        surfaces.push(surface);
    }
    surfaces.sort_by(|left, right| {
        left.get("surface_id")
            .and_then(Value::as_str)
            .cmp(&right.get("surface_id").and_then(Value::as_str))
    });
    let mut result = json!({"schema":"narada.mcp_loader.site_surfaces.v1","site_root":site_root,"surfaces":surfaces});
    if include_runtime {
        result["runtime_freshness"] = runtime_freshness(state);
    }
    Ok(result)
}

pub(crate) fn classify_fabric_entrypoint(
    site_root: &str,
    declared: Option<&str>,
    expected: Option<&str>,
    exists: bool,
) -> (&'static str, Vec<String>) {
    let Some(declared) = declared else {
        return ("entrypoint_unresolved", vec!["Inspect the site fabric command and args; mcp-loader could not determine the declared entrypoint.".to_string()]);
    };
    if !exists {
        return ("stale_entrypoint", vec!["Repair or regenerate the site MCP fabric so the declared entrypoint exists before attach.".to_string()]);
    }
    if expected.is_some_and(|value| value == declared) {
        return ("matches_shared_registry", Vec::new());
    }
    if is_under_path(declared, site_root) {
        return (if expected.is_some() {"site_local_override"} else {"site_local_surface"}, vec!["Treat this as site-local authority; compare expected tools before replacing it with the shared registry entrypoint.".to_string()]);
    }
    if expected.is_some() {
        return ("external_entrypoint_override", vec!["Classify as intentional override or drift at the fabric generator/registrar layer before local repair. Compare tool counts and authority implications against the shared registry entrypoint.".to_string()]);
    }
    (
        "external_site_declared_surface",
        vec![
            "Verify the external entrypoint authority and allowed-entrypoint policy before attach."
                .to_string(),
        ],
    )
}
