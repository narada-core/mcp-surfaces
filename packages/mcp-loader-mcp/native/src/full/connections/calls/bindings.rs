use crate::full::*;

pub(crate) fn call_surface_handle_tool(
    arguments: &JsonObject,
    state: &mut LoaderState,
) -> Result<Value, Diagnostic> {
    let handle_name = required_string(arguments, "surface_handle", "missing_surface_handle")?;
    let handle = state.handles.get(&handle_name).ok_or_else(|| {
        Diagnostic::new(
            "surface_handle_not_found",
            format!("surface_handle_not_found:{}", handle_name),
        )
    })?;
    let connection_id = find_connection_for_handle(handle, state)
        .filter(|connection| connection_live(connection))
        .map(|connection| connection.connection_id.clone());
    let Some(connection_id) = connection_id else {
        return Err(Diagnostic::new("surface_handle_connection_unavailable", format!("surface_handle_connection_unavailable:{}", handle_name))
            .with_details(json!({"surface_handle":handle_name,"logical_connection_id":handle.logical_connection_id,"binding_id":handle.binding_id,"site_root":handle.site_root,"surface_id":handle.surface_id,"recovery":unavailable_handle_recovery(handle)})));
    };
    let mut delegated = arguments.clone();
    delegated.insert("connection_id".to_string(), json!(connection_id));
    call_attached_tool(&delegated, state)
}

pub(crate) fn inspect_binding_tool(
    arguments: &JsonObject,
    state: &mut LoaderState,
) -> Result<Value, Diagnostic> {
    let opened = resume_or_open_surface(arguments, state)?;
    let handle_name = opened
        .get("surface_handle")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            Diagnostic::new(
                "surface_handle_missing",
                "surface_handle_missing_after_resume_or_open",
            )
        })?;
    let handle = state.handles.get(handle_name).ok_or_else(|| {
        Diagnostic::new(
            "surface_handle_not_found",
            format!("surface_handle_not_found:{handle_name}"),
        )
    })?;
    let connection = find_connection_for_handle(handle, state)
        .filter(|connection| connection_live(connection))
        .ok_or_else(|| {
            Diagnostic::new(
                "surface_handle_connection_unavailable",
                format!("surface_handle_connection_unavailable:{handle_name}"),
            )
        })?;
    let mut delegated = Map::new();
    delegated.insert("connection_id".into(), json!(connection.connection_id));
    delegated.insert(
        "tool_name".into(),
        arguments.get("tool_name").cloned().unwrap_or(Value::Null),
    );
    let mut result = inspect_attached_tool(&delegated, state)?;
    result["binding_resolution"] = json!({
        "status": opened.get("status").cloned().unwrap_or_else(|| json!("opened")),
        "binding_id": arguments.get("binding_id").cloned().unwrap_or(Value::Null),
        "canonical_binding_id": opened.get("canonical_binding_id").cloned().unwrap_or_else(|| arguments.get("binding_id").cloned().unwrap_or(Value::Null)),
        "binding_id_canonicalized": opened.get("binding_id_canonicalized").cloned().unwrap_or_else(|| json!(false)),
        "surface_handle": handle_name,
        "handle_scope": "loader_process"
    });
    Ok(result)
}

pub(crate) fn inspect_binding_tools(
    arguments: &JsonObject,
    state: &mut LoaderState,
) -> Result<Value, Diagnostic> {
    let opened = resume_or_open_surface(arguments, state)?;
    let handle_name = opened
        .get("surface_handle")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            Diagnostic::new(
                "surface_handle_missing",
                "surface_handle_missing_after_resume_or_open",
            )
        })?;
    let handle = state.handles.get(handle_name).ok_or_else(|| {
        Diagnostic::new(
            "surface_handle_not_found",
            format!("surface_handle_not_found:{handle_name}"),
        )
    })?;
    let connection_id = find_connection_for_handle(handle, state)
        .filter(|connection| connection_live(connection))
        .map(|connection| connection.connection_id.clone())
        .ok_or_else(|| {
            Diagnostic::new(
                "surface_handle_connection_unavailable",
                format!("surface_handle_connection_unavailable:{handle_name}"),
            )
        })?;
    let names = arguments
        .get("tool_names")
        .and_then(Value::as_array)
        .ok_or_else(|| Diagnostic::new("tool_names_required", "tool_names_required"))?;
    let mut leases = Vec::with_capacity(names.len());
    for name in names {
        let mut delegated = Map::new();
        delegated.insert("connection_id".into(), json!(connection_id));
        delegated.insert("tool_name".into(), name.clone());
        delegated.insert(
            "include_tool_contract".into(),
            arguments
                .get("include_tool_contract")
                .cloned()
                .unwrap_or_else(|| json!(false)),
        );
        leases.push(inspect_attached_tool(&delegated, state)?);
    }
    let result = json!({
        "schema":"narada.mcp_loader.schema_lease_batch.v1",
        "status":"issued",
        "connection_id":connection_id,
        "surface_handle":handle_name,
        "lease_count":leases.len(),
        "leases":leases,
        "binding_resolution":{
            "status":opened.get("status").cloned().unwrap_or_else(|| json!("opened")),
            "binding_id":arguments.get("binding_id").cloned().unwrap_or(Value::Null),
            "canonical_binding_id":opened.get("canonical_binding_id").cloned().unwrap_or_else(|| arguments.get("binding_id").cloned().unwrap_or(Value::Null)),
            "binding_id_canonicalized":opened.get("binding_id_canonicalized").cloned().unwrap_or_else(|| json!(false))
        }
    });
    if utf16_len(&pretty_json(&result)) > DEFAULT_LOADER_RESULT_INLINE_LIMIT {
        return Ok(build_bounded_result(
            state,
            &connection_id,
            "mcp_loader_inspect_binding_tools",
            &result,
            false,
        )?["structuredContent"]
            .clone());
    }
    Ok(result)
}

pub(crate) fn call_binding_tool(
    arguments: &JsonObject,
    state: &mut LoaderState,
) -> Result<Value, Diagnostic> {
    let opened = resume_or_open_surface(arguments, state)?;
    let handle = opened
        .get("surface_handle")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            Diagnostic::new(
                "surface_handle_missing",
                "surface_handle_missing_after_resume_or_open",
            )
        })?;
    let mut delegated = Map::new();
    delegated.insert("surface_handle".into(), json!(handle));
    delegated.insert(
        "tool_name".into(),
        arguments.get("tool_name").cloned().unwrap_or(Value::Null),
    );
    delegated.insert(
        "arguments".into(),
        arguments
            .get("arguments")
            .cloned()
            .unwrap_or_else(|| json!({})),
    );
    delegated.insert(
        "schema_lease".into(),
        arguments
            .get("schema_lease")
            .cloned()
            .unwrap_or(Value::Null),
    );
    if let Some(value) = arguments
        .get("tool_schema_digest")
        .or_else(|| arguments.get("tool_contract_digest"))
    {
        delegated.insert("tool_schema_digest".into(), value.clone());
    }
    if let Some(value) = arguments.get("include_runtime_metadata") {
        delegated.insert("include_runtime_metadata".into(), value.clone());
    }
    let mut result = call_surface_handle_tool(&delegated, state)?;
    result["binding_resolution"] = json!({
        "status": opened.get("status").cloned().unwrap_or_else(|| json!("opened")),
        "binding_id": arguments.get("binding_id").cloned().unwrap_or(Value::Null),
        "surface_handle": handle,
        "handle_scope": "loader_process",
        "caller_must_retain_handle": false
    });
    Ok(result)
}
