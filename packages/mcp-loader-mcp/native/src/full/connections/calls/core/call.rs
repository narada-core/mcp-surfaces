use crate::full::*;

pub(crate) fn call_attached_tool(
    arguments: &JsonObject,
    state: &mut LoaderState,
) -> Result<Value, Diagnostic> {
    let connection_id = required_string(arguments, "connection_id", "missing_connection_id")?;
    let tool_name = required_string(arguments, "tool_name", "missing_tool_name")?;
    let authorization_resolution =
        validate_schema_lease(arguments, state, &connection_id, &tool_name)?;
    if let Some(connection) = state.connections.get(&connection_id) {
        if let Some(binding_id) = connection.binding_id.as_deref() {
            admitted_binding(state, &connection.site_root, binding_id, "attach")?;
        } else {
            assert_binding_admission_available(state)?;
        }
    }
    let tool_arguments = arguments
        .get("arguments")
        .cloned()
        .unwrap_or_else(|| json!({}));
    let tool_object = tool_arguments.as_object().cloned().unwrap_or_default();
    if json_byte_len(&tool_arguments) > state.policy.max_request_bytes {
        return Err(Diagnostic::new(
            "request_too_large",
            format!(
                "request_too_large:{}:{}",
                json_byte_len(&tool_arguments),
                state.policy.max_request_bytes
            ),
        ));
    }
    let target_site_root = state
        .connections
        .get(&connection_id)
        .map(|connection| connection.site_root.clone())
        .ok_or_else(|| Diagnostic::new("connection_not_found", "connection_not_found"))?;
    let payload_transport =
        stage_admitted_payload_ref(&target_site_root, &tool_arguments, &state.policy)?;
    let (outer_timeout, explicit_timeout, request_timeout) =
        resolve_timeout(&tool_arguments, &state.policy)?;
    let request_params = {
        let mut object = Map::new();
        object.insert("name".to_string(), Value::String(tool_name.clone()));
        object.insert("arguments".to_string(), Value::Object(tool_object));
        if explicit_timeout {
            object.insert(
                "_meta".to_string(),
                json!({"narada_request_timeout_ms":request_timeout}),
            );
        }
        Value::Object(object)
    };
    let child_result = {
        let connection = state.connections.get_mut(&connection_id).ok_or_else(|| {
            Diagnostic::new(
                "connection_not_found",
                format!("connection_not_found:{}", connection_id),
            )
        })?;
        if connection.detached {
            return Err(Diagnostic::new(
                "connection_detached",
                format!("connection_detached:{}", connection_id),
            ));
        }
        match connection
            .session
            .request("tools/call", request_params, outer_timeout)
        {
            Ok(result) => {
                touch_connection(connection);
                result
            }
            Err(mut error) => {
                let mut domain_details =
                    request_error_details(&error.details, "tools/call", outer_timeout);
                if error.code.contains("timeout") || error.message.contains("timed out") {
                    let child_alive = connection.session.alive();
                    if let Some(details) = domain_details.as_object_mut() {
                        details.insert(
                            "process_disposition".into(),
                            json!({
                                "child_mcp_process":if child_alive {"observed_running_after_timeout"} else {"observed_exited_after_timeout"},
                                "invoked_command_process":"unknown_to_loader",
                                "execution_ref":"not_observed_before_timeout"
                            }),
                        );
                        details.insert("recovery_action".into(), json!({
                            "first":"If the child tool supports durable execution, query it by the execution_ref from an earlier response.",
                            "otherwise":{"tool_name":"mcp_loader_surface_status","arguments":{"connection_id":connection.connection_id}},
                            "warning":"Do not retry a non-idempotent call until child-side execution disposition is known."
                        }));
                    }
                }
                error.details = child_runtime_diagnostic(connection, domain_details);
                return Err(error);
            }
        }
    };
    let include_runtime = arguments
        .get("include_runtime_metadata")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let enriched = if include_runtime
        && (tool_name.ends_with("_guidance") || tool_name == "guidance")
    {
        let mut result = child_result.clone();
        if let Some(object) = result.as_object_mut() {
            let mut structured = object
                .get("structuredContent")
                .cloned()
                .unwrap_or_else(|| json!({}));
            if let Some(structured_object) = structured.as_object_mut() {
                let connection = state.connections.get(&connection_id).ok_or_else(|| {
                    Diagnostic::new("connection_not_found", "connection_not_found")
                })?;
                structured_object.insert(
                    "loader_runtime_lifecycle".to_string(),
                    runtime_lifecycle(Some(&connection.connection_id), Some(&connection.lifecycle)),
                );
                structured_object.insert(
                    "loader_runtime_freshness".to_string(),
                    runtime_freshness(state),
                );
            }
            object.insert("structuredContent".to_string(), structured);
        }
        result
    } else {
        child_result
    };
    let is_error = enriched
        .get("isError")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let compacted = compact_child_result(&enriched);
    let bounded = build_bounded_result(
        state,
        &connection_id,
        &format!(
            "mcp_loader_call_tool:{}:{}",
            state
                .connections
                .get(&connection_id)
                .map(|value| value.surface_id.as_str())
                .unwrap_or("surface"),
            tool_name
        ),
        &compacted,
        is_error,
    )?;
    let connection = state
        .connections
        .get(&connection_id)
        .ok_or_else(|| Diagnostic::new("connection_not_found", "connection_not_found"))?;
    let bounded_object = bounded
        .get("structuredContent")
        .cloned()
        .unwrap_or(Value::Null);
    let result_bounded = bounded_object.get("schema").and_then(Value::as_str)
        == Some("narada.producer_output_page.v1");
    let mut response = json!({
        "schema":"narada.mcp_loader.tool_result.v1","connection_id":connection.connection_id,"surface_id":connection.surface_id,
        "result":bounded_object,"result_summary":typed_result_summary(&enriched),"result_bounded":result_bounded
    });
    response["authorization_resolution"] = json!(authorization_resolution);
    if let Some(transport) = payload_transport {
        response["payload_transport"] = transport;
    }
    if let Some(output_ref) = bounded
        .get("structuredContent")
        .and_then(|value| value.get("output_ref"))
        .and_then(Value::as_str)
    {
        response["details_ref"] = json!(output_ref);
        response["details_reader"] = json!("mcp_loader_read_result");
    }
    if include_runtime {
        response["runtime_lifecycle"] =
            runtime_lifecycle(Some(&connection.connection_id), Some(&connection.lifecycle));
        response["runtime_freshness"] = runtime_freshness(state);
    }
    if json_byte_len(&response) > state.policy.max_response_bytes {
        return Err(Diagnostic::new(
            "response_too_large",
            format!(
                "response_too_large:{}:{}",
                json_byte_len(&response),
                state.policy.max_response_bytes
            ),
        ));
    }
    Ok(response)
}
