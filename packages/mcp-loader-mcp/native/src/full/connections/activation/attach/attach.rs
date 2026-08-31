use crate::full::*;

pub(crate) fn attach_surface(
    arguments: &JsonObject,
    state: &mut LoaderState,
) -> Result<Value, Diagnostic> {
    let explicit_entrypoint = value_string(arguments.get("entrypoint"));
    let site_root = value_string(arguments.get("site_root")).map(|value| normalize_path(&value));
    let standalone =
        state.standalone_ambient_attachment && site_root.is_none() && explicit_entrypoint.is_some();
    let site_root = site_root.unwrap_or_else(|| normalize_path("."));
    let binding_id = value_string(arguments.get("binding_id"));
    let admitted = if state.binding_admission.is_some() {
        let id = binding_id
            .as_deref()
            .ok_or_else(|| Diagnostic::new("missing_binding_id", "missing_binding_id"))?;
        admitted_binding(state, &site_root, id, "attach")?
    } else {
        assert_binding_admission_available(state)?;
        None
    };
    let surface_id = admitted
        .as_ref()
        .and_then(|(entry, _)| {
            entry
                .get("surface_id")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned)
        })
        .or_else(|| value_string(arguments.get("surface_id")))
        .unwrap_or_else(|| "native-loader-child".to_string());
    if admitted.is_some()
        && value_string(arguments.get("surface_id")).is_some_and(|asserted| asserted != surface_id)
    {
        return Err(Diagnostic::new(
            "mcp_binding_surface_assertion_mismatch",
            format!(
                "mcp_binding_surface_assertion_mismatch:{}",
                binding_id.clone().unwrap_or_default()
            ),
        ));
    }
    if !standalone {
        ensure_site_root_allowed(&site_root, &state.policy)?;
        ensure_surface_allowed(&surface_id, &site_root, &state.policy, state)?;
    }
    let runtime_kind = value_string(arguments.get("runtime_kind"));
    let (
        server_name,
        projection_id,
        execution,
        lifecycle,
        descriptor,
        descriptor_digest,
        declared_digest,
        runtime_requirements,
    ) = if standalone {
        (
            surface_id.clone(),
            "default".to_string(),
            json!({"adapter":"stdio","tenancy":"session_isolated","replacement":"manual"}),
            json!({"mode":"replayable"}),
            None,
            None,
            None,
            Vec::new(),
        )
    } else {
        runtime_metadata(&site_root, &surface_id)?
    };
    if !runtime_matches(&runtime_requirements, runtime_kind.as_deref()) {
        if runtime_kind.is_none() {
            return Err(Diagnostic::new(
                "surface_runtime_required",
                format!("surface_runtime_required:{}", surface_id),
            )
            .with_details(
                json!({"surface_id":surface_id,"runtime_requirements":runtime_requirements}),
            ));
        }
        return Err(Diagnostic::new("surface_runtime_not_supported", format!("surface_runtime_not_supported:{}:{}", surface_id, runtime_kind.clone().unwrap_or_default()))
            .with_details(json!({"surface_id":surface_id,"runtime_kind":runtime_kind,"runtime_requirements":runtime_requirements})));
    }
    if execution
        .get("adapter")
        .and_then(Value::as_str)
        .unwrap_or("stdio")
        != "stdio"
    {
        return Err(Diagnostic::new("surface_execution_adapter_not_supported_by_loader", format!("surface_execution_adapter_not_supported_by_loader:{}:{}", surface_id, execution.get("adapter").and_then(Value::as_str).unwrap_or_default()))
            .with_details(json!({"surface_id":surface_id,"projection_id":projection_id,"execution":execution,"responsible_actuator":"pc_site_surface_runtime","remediation":"Route this admitted binding through the PC Site surface runtime; mcp-loader remains the stdio compatibility adapter."})));
    }
    if state.connections.len() >= state.policy.max_connections {
        let inventory = connection_inventory(&Map::new(), state);
        return Err(Diagnostic::new("max_connections_reached", format!("max_connections_reached:{}", state.connections.len()))
            .with_details(json!({"max_connections":inventory["max_connections"],"connection_count":inventory["connection_count"],"available_slots":inventory["available_slots"],"closed_connection_ids":inventory["closed_connection_ids"],"reclaimable_connections":inventory["connections"],"recovery":inventory["recovery"]})));
    }
    let extra_args = string_array(arguments.get("args"))?.unwrap_or_default();
    if admitted.is_some() && (explicit_entrypoint.is_some() || !extra_args.is_empty()) {
        return Err(Diagnostic::new(
            "mcp_binding_invocation_override_not_allowed",
            format!(
                "mcp_binding_invocation_override_not_allowed:{}",
                binding_id.clone().unwrap_or_default()
            ),
        )
        .with_details(json!({"child_spawned":false})));
    }
    if explicit_entrypoint.is_none() && !extra_args.is_empty() {
        return Err(Diagnostic::new(
            "site_fabric_invocation_override_not_allowed",
            format!("site_fabric_invocation_override_not_allowed:{}", surface_id),
        )
        .with_details(json!({
            "surface_id": surface_id,
            "remediation": "Change and rematerialize the authoritative Site fabric instead of supplying per-call arguments."
        })));
    }
    let (entrypoint, resolved_args, command, child_invocation_kind) = if let Some(explicit) =
        explicit_entrypoint.clone()
    {
        (
            normalize_path(&explicit),
            extra_args.clone(),
            normalize_path(&explicit),
            "native_entrypoint".to_string(),
        )
    } else {
        let bundle = read_site_fabric(&site_root)?;
        let servers = bundle
            .fabric
            .get("mcpServers")
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();
        if let Some((_, server)) = find_site_server(&servers, &surface_id)? {
            let command = server
                .get("command")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            let raw_args = server
                .get("args")
                .and_then(Value::as_array)
                .map(|values| {
                    values
                        .iter()
                        .filter_map(|value| value.as_str().map(ToOwned::to_owned))
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            let is_proxy_wrapped = is_runtime_proxy_command(&command)
                && raw_args.first().map(String::as_str) == Some("proxy");
            if is_proxy_wrapped {
                let child_command = extract_proxy_child_command(&raw_args)
                    .map(|cmd| resolve_child_command(&cmd))
                    .ok_or_else(|| {
                        Diagnostic::new(
                            "surface_command_unsupported",
                            format!("surface_command_unsupported:runtime-proxy:{}", command),
                        )
                    })?;
                let child_entrypoint =
                    extract_proxy_child_entrypoint(&raw_args).ok_or_else(|| {
                        Diagnostic::new(
                            "surface_command_unsupported",
                            format!("surface_command_unsupported:runtime-proxy:{}", command),
                        )
                    })?;
                let child_invocation_kind = extract_proxy_child_invocation_kind(&raw_args);
                let child_args = extract_proxy_child_args(&raw_args).ok_or_else(|| {
                    Diagnostic::new(
                        "surface_command_unsupported",
                        format!("surface_command_unsupported:runtime-proxy:{}", command),
                    )
                })?;
                match child_invocation_kind.as_str() {
                    "entrypoint" => (
                        normalize_path(&child_entrypoint),
                        child_args.into_iter().chain(extra_args.clone()).collect(),
                        child_command,
                        child_invocation_kind,
                    ),
                    "native_entrypoint" => {
                        let native_entrypoint = normalize_path(&child_command);
                        (
                            native_entrypoint,
                            child_args.into_iter().chain(extra_args.clone()).collect(),
                            child_command,
                            child_invocation_kind,
                        )
                    }
                    "native_applet" => {
                        let child_applet =
                            extract_proxy_child_applet(&raw_args).ok_or_else(|| {
                                Diagnostic::new(
                                    "surface_native_child_unsupported",
                                    format!(
                                        "surface_native_child_unsupported:{}",
                                        child_invocation_kind
                                    ),
                                )
                            })?;
                        let native_entrypoint = normalize_path(&child_command);
                        let mut native_args = vec![child_applet];
                        native_args.extend(child_args);
                        native_args.extend(extra_args.clone());
                        (
                            native_entrypoint,
                            native_args,
                            child_command,
                            child_invocation_kind,
                        )
                    }
                    _ => {
                        return Err(Diagnostic::new(
                            "surface_native_child_unsupported",
                            format!("surface_native_child_unsupported:{}", child_invocation_kind),
                        )
                        .with_details(json!({
                            "surface_id": surface_id,
                            "child_command": child_command,
                            "child_entrypoint": child_entrypoint,
                        })));
                    }
                }
            } else {
                let declared =
                    extract_runtime_entrypoint(&command, &raw_args).ok_or_else(|| {
                        Diagnostic::new(
                            "surface_command_unsupported",
                            format!("surface_command_unsupported:{}:{}", surface_id, command),
                        )
                    })?;
                (
                    normalize_path(&declared),
                    remove_entrypoint_arg(&raw_args, &declared)
                        .into_iter()
                        .chain(extra_args.clone())
                        .collect(),
                    command,
                    "entrypoint".to_string(),
                )
            }
        } else if let Some((entrypoint, args)) =
            shared_surface_registry(&surface_id, &state.surface_root)
        {
            let args = args
                .into_iter()
                .map(|value| interpolate_site_arg(&value, &site_root))
                .collect::<Result<Vec<_>, _>>()?;
            (
                normalize_path(&entrypoint),
                args.into_iter().chain(extra_args.clone()).collect(),
                normalize_path(&entrypoint),
                "native_entrypoint".to_string(),
            )
        } else {
            return Err(Diagnostic::new(
                "surface_not_found",
                format!("surface_not_found:{}", surface_id),
            ));
        }
    };
    // A Site-fabric launch is admitted by its exact materialized declaration.
    // Prefix policy remains the authority for caller-supplied entrypoints only.
    if explicit_entrypoint.is_some() {
        ensure_entrypoint_allowed(&site_root, &entrypoint, &state.policy)?;
    }
    if !Path::new(&entrypoint).exists() {
        return Err(Diagnostic::new(
            "entrypoint_not_found",
            format!("entrypoint_not_found:{}", entrypoint),
        ));
    }
    let connection = open_connection(
        state,
        site_root,
        surface_id,
        runtime_kind,
        runtime_requirements,
        entrypoint,
        resolved_args,
        command,
        child_invocation_kind,
        explicit_entrypoint,
        extra_args,
        server_name,
        projection_id,
        execution,
        lifecycle,
        descriptor,
        descriptor_digest,
        declared_digest,
        admitted.as_ref().map(|(entry, _)| entry.clone()),
        arguments
            .get("binding_id")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned),
    )?;
    let id = connection.connection_id.clone();
    let response = attached_response(&connection, state);
    state.connections.insert(id, connection);
    Ok(response)
}
