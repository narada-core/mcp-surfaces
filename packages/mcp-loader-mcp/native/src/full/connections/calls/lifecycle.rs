use crate::full::*;

pub(crate) fn detach_connection(
    arguments: &JsonObject,
    state: &mut LoaderState,
) -> Result<Value, Diagnostic> {
    let id = required_string(arguments, "connection_id", "missing_connection_id")?;
    let mut connection = state.connections.remove(&id).ok_or_else(|| {
        Diagnostic::new(
            "connection_not_found",
            format!("connection_not_found:{}", id),
        )
    })?;
    connection.detached = true;
    connection.detached_ms = Some(now_ms());
    let termination = connection.session.terminate();
    let termination_observation = json!({
        "classification":"expected_protocol_detach",
        "requested":true,
        "child_exit_is_crash":false,
        "mechanism":if termination.get("forced").and_then(Value::as_bool)==Some(true){"process_kill_after_protocol_detach"}else{"already_exited"},
        "raw":termination
    });
    Ok(
        json!({"schema":"narada.mcp_loader.detached.v1","connection_id":connection.connection_id,"surface_id":connection.surface_id,"status":"detached","termination":termination_observation}),
    )
}

pub(crate) fn restart_connection(
    arguments: &JsonObject,
    state: &mut LoaderState,
) -> Result<Value, Diagnostic> {
    let id = required_string(arguments, "connection_id", "missing_connection_id")?;
    let admitted = if let Some(existing) = state.connections.get(&id) {
        if let Some(binding_id) = existing.binding_id.as_deref() {
            admitted_binding(state, &existing.site_root, binding_id, "restart")?
        } else {
            assert_binding_admission_available(state)?;
            None
        }
    } else {
        None
    };
    let mut previous = state.connections.remove(&id).ok_or_else(|| {
        Diagnostic::new(
            "connection_not_found",
            format!("connection_not_found:{}", id),
        )
    })?;
    if lifecycle_mode(&previous) != Some("replayable") {
        let runtime = runtime_lifecycle(Some(&previous.connection_id), Some(&previous.lifecycle));
        let recovery = connection_recovery_actions(&previous);
        state.connections.insert(id.clone(), previous);
        return Err(Diagnostic::new("surface_restart_not_loader_replayable", format!("surface_restart_not_loader_replayable:{}", id))
            .with_details(json!({"connection_id":id,"surface_id":state.connections.get(&id).map(|value| value.surface_id.clone()),"lifecycle":state.connections.get(&id).map(|value| value.lifecycle.clone()),"runtime_lifecycle":runtime,"recovery_actions":recovery})));
    }
    let previous_status = connection_status(&previous, state);
    let termination = previous.session.terminate();
    previous.detached = true;
    previous.detached_ms = Some(now_ms());
    let replacement = match open_connection(
        state,
        previous.site_root.clone(),
        previous.surface_id.clone(),
        previous.runtime_kind.clone(),
        previous.runtime_requirements.clone(),
        previous.entrypoint.clone(),
        previous.args.clone(),
        previous.session.spec.command.clone(),
        previous.child_invocation_kind.clone(),
        previous.requested_entrypoint.clone(),
        previous.extra_args.clone(),
        previous.server_name.clone(),
        previous.projection_id.clone(),
        previous.execution.clone(),
        previous.lifecycle.clone(),
        previous.descriptor.clone(),
        previous.descriptor_digest.clone(),
        previous.declared_tool_contract_digest.clone(),
        admitted.map(|(entry, _)| entry),
        previous.binding_id.clone(),
    ) {
        Ok(mut connection) => {
            connection.logical_connection_id = previous.logical_connection_id.clone();
            connection
        }
        Err(error) => {
            state.connections.insert(id.clone(), previous);
            return Err(error);
        }
    };
    let response = json!({
        "schema":"narada.mcp_loader.surface_restarted.v1","status":"restarted","reason":value_string(arguments.get("reason")),
        "previous_connection":previous_status,"replacement_connection":connection_status(&replacement,state),
        "connection_id":replacement.connection_id,"previous_connection_id":id,"surface_id":replacement.surface_id,
        "runtime_lifecycle":runtime_lifecycle(Some(&replacement.connection_id),Some(&replacement.lifecycle)),
        "entrypoint":replacement.entrypoint,"args":replacement.args,"termination":termination,
        "server_info":replacement.server_info,"tool_count":replacement.tools.len(),
        "tool_discovery":{"tool_name":"mcp_loader_list_tools","arguments":{"connection_id":replacement.connection_id}},
        "tool_inspection":{"tool_name":"mcp_loader_inspect_tool","required_arguments":["connection_id","tool_name"]}
    });
    state
        .connections
        .insert(replacement.connection_id.clone(), replacement);
    Ok(response)
}

pub(crate) fn runtime_observation(
    arguments: &JsonObject,
    state: &mut LoaderState,
) -> Result<Value, Diagnostic> {
    let id = required_string(arguments, "connection_id", "missing_connection_id")?;
    let carrier_kind = required_string(arguments, "carrier_kind", "missing_carrier_kind")?;
    let connection = state.connections.get_mut(&id).ok_or_else(|| {
        Diagnostic::new(
            "connection_not_found",
            format!("connection_not_found:{}", id),
        )
    })?;
    let live = connection_live(connection);
    if live {
        touch_connection(connection);
    }
    let site_id = derive_site_id(&connection.site_root)?;
    let manifest_digest = value_string(arguments.get("manifest_digest"));
    Ok(json!({
        "schema_version":"2.0","observation_id":format!("observation-{}-{}",now_ms(),&connection.logical_connection_id[..connection.logical_connection_id.len().min(12)]),
        "observed_at":now_iso(),"site_id":site_id,"carrier_kind":carrier_kind,"runtime_state_root":Value::Null,
        "manifest_digest":manifest_digest,
        "servers":[{
            "server_name":connection.server_name,"surface_id":connection.surface_id,"projection_id":connection.projection_id,
            "logical_connection_id":connection.logical_connection_id,"lifecycle":connection.lifecycle,
            "active_generation":if live {runtime_generation(connection,now_ms())} else {Value::Null},
            "draining_generations":[],
            "recovery_actions":connection_recovery_actions(connection),
            "detail":if live {"mcp-loader owns this active generation; use the bounded loader restart action for replacement."} else {"The loader child is no longer live; inspect the status and use the bounded loader restart action if lifecycle permits."}
        }]
    }))
}
