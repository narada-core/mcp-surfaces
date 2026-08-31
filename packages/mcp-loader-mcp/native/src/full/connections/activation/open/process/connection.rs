use crate::full::*;

pub(crate) fn open_connection(
    state: &LoaderState,
    site_root: String,
    surface_id: String,
    runtime_kind: Option<String>,
    runtime_requirements: Vec<String>,
    entrypoint: String,
    resolved_args: Vec<String>,
    command: String,
    child_invocation_kind: String,
    requested_entrypoint: Option<String>,
    extra_args: Vec<String>,
    server_name: String,
    projection_id: String,
    execution: Value,
    lifecycle: Value,
    descriptor: Option<Value>,
    descriptor_digest: Option<String>,
    declared_digest: Option<String>,
    admitted_binding: Option<Value>,
    requested_binding_id: Option<String>,
) -> Result<Connection, Diagnostic> {
    let connection_id = new_id("connection");
    let logical_connection_id = connection_id.clone();
    let generation_id = new_id("generation");
    let owner_run_id = state.run_id.clone();
    let owner_pid = state.owner_pid;
    let ownership_marker = state.ownership_marker.clone();
    let child_spec = build_child_spec(
        &command,
        &entrypoint,
        &resolved_args,
        &child_invocation_kind,
    );
    let env_map = build_child_env(
        &site_root,
        &state.policy,
        &connection_id,
        &logical_connection_id,
        &generation_id,
        &ownership_marker,
    );
    let session = ChildSession::spawn(child_spec, &env_map, state.policy.max_response_bytes)?;
    let (server_info, tools_result) = match session.request(
        "server/discover",
        modern_request_params(),
        state.policy.attach_timeout_ms,
    ) {
        Ok(discovery) if modern_discovery_is_valid(&discovery) => {
            let server_info = discovery
                .get("_meta")
                .and_then(|meta| meta.get("io.modelcontextprotocol/serverInfo"))
                .cloned()
                .unwrap_or_else(|| json!({}));
            let tools_result = match session.request(
                "tools/list",
                modern_request_params(),
                state.policy.attach_timeout_ms,
            ) {
                Ok(value) => value,
                Err(error) => {
                    session.terminate();
                    return Err(error.with_details(json!({
                        "connection_id":connection_id,
                        "surface_id":surface_id,
                        "entrypoint":entrypoint,
                        "args":resolved_args,
                        "exit_code":session.exit_code(),
                        "signal_code":session.signal_code(),
                        "stderr_tail":session.stderr_tail(),
                        "runtime_lifecycle":runtime_lifecycle(Some(&connection_id),Some(&lifecycle))
                    })));
                }
            };
            (server_info, tools_result)
        }
        Ok(_) | Err(_) => {
            let init = match session.request(
                    "initialize",
                    json!({"protocolVersion":PROTOCOL_VERSION,"capabilities":{},"clientInfo":{"name":SERVER_NAME,"version":SERVER_VERSION}}),
                    state.policy.attach_timeout_ms,
                ) {
                    Ok(value) => value,
                    Err(error) => {
                        session.terminate();
                        return Err(error.with_details(json!({
                            "connection_id":connection_id,
                            "surface_id":surface_id,
                            "entrypoint":entrypoint,
                            "args":resolved_args,
                            "exit_code":session.exit_code(),
                            "signal_code":session.signal_code(),
                            "stderr_tail":session.stderr_tail(),
                            "runtime_lifecycle":runtime_lifecycle(Some(&connection_id),Some(&lifecycle))
                        })));
                    }
                };
            if let Err(error) = session.notify("notifications/initialized", json!({})) {
                session.terminate();
                return Err(error);
            }
            let tools_result =
                match session.request("tools/list", json!({}), state.policy.attach_timeout_ms) {
                    Ok(value) => value,
                    Err(error) => {
                        session.terminate();
                        return Err(error.with_details(json!({
                        "connection_id":connection_id,
                        "surface_id":surface_id,
                        "entrypoint":entrypoint,
                        "args":resolved_args,
                        "exit_code":session.exit_code(),
                        "signal_code":session.signal_code(),
                        "stderr_tail":session.stderr_tail(),
                        "runtime_lifecycle":runtime_lifecycle(Some(&connection_id),Some(&lifecycle))
                    })));
                    }
                };
            (
                init.get("serverInfo").cloned().unwrap_or_else(|| json!({})),
                tools_result,
            )
        }
    };
    let tools = tools_result
        .get("tools")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let attached_ms = now_ms();
    let mut connection = Connection {
        session,
        connection_id,
        owner_run_id,
        owner_pid,
        parent_pid: owner_pid,
        ownership_marker,
        logical_connection_id,
        generation_id,
        server_name,
        projection_id,
        execution,
        lifecycle,
        descriptor,
        descriptor_digest,
        declared_tool_contract_digest: declared_digest,
        tool_contract_digest: observed_tool_digest(&tools, None),
        heartbeat_ms: attached_ms,
        lease_expires_ms: attached_ms + DEFAULT_RUNTIME_LEASE_MS as u128,
        site_root,
        surface_id,
        binding_id: admitted_binding
            .as_ref()
            .and_then(|entry| entry.get("binding_id"))
            .and_then(Value::as_str)
            .map(ToOwned::to_owned)
            .or(requested_binding_id),
        admission_envelope_id: state
            .binding_admission
            .as_ref()
            .and_then(|value| value.get("envelope_id"))
            .and_then(Value::as_str)
            .map(ToOwned::to_owned),
        admitted_binding_digest: admitted_binding
            .as_ref()
            .and_then(|entry| entry.get("binding_digest"))
            .and_then(Value::as_str)
            .map(ToOwned::to_owned),
        authority_epoch: state
            .binding_admission
            .as_ref()
            .and_then(|value| value.get("authority_epoch"))
            .and_then(Value::as_u64),
        runtime_kind,
        runtime_requirements,
        runtime_command: command,
        entrypoint,
        args: resolved_args,
        child_invocation_kind,
        requested_entrypoint,
        extra_args,
        initialized: true,
        server_info,
        tools,
        detached: false,
        attached_ms,
        detached_ms: None,
    };
    touch_connection(&mut connection);
    Ok(connection)
}
