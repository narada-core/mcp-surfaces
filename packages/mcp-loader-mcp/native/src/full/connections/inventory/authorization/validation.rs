use crate::full::*;

pub(crate) fn validate_schema_lease(
    arguments: &JsonObject,
    state: &LoaderState,
    connection_id: &str,
    tool_name: &str,
) -> Result<&'static str, Diagnostic> {
    let connection = state.connections.get(connection_id).ok_or_else(|| {
        Diagnostic::new(
            "connection_not_found",
            format!("connection_not_found:{connection_id}"),
        )
        .with_details(json!({
            "connection_id":connection_id,
            "recovery":{
                "inventory":{"tool_name":"mcp_loader_connection_inventory","arguments":{}},
                "resume_or_open":{"tool_name":"mcp_loader_resume_or_open_surface","required_arguments":["site_root","binding_id"]},
                "note":"Use the stable surface handle or binding identity retained from attach/inspection; a missing raw connection id cannot recreate authority by itself."
            }
        }))
    })?;
    let tool = child_tool_contract(connection, tool_name)?;
    let digest = child_tool_schema_digest(tool);
    let supplied_digest = arguments
        .get("tool_schema_digest")
        .or_else(|| arguments.get("tool_contract_digest"))
        .and_then(Value::as_str);
    if supplied_digest == Some(digest.as_str()) {
        return Ok("digest_reused");
    }
    if let Some(supplied_digest) = supplied_digest {
        return Err(Diagnostic::new(
            "tool_contract_digest_obsolete_generation",
            "tool_contract_digest_obsolete_generation: supplied digest does not authorize the current child generation",
        ).with_details(json!({
            "connection_id":connection_id,
            "tool_name":tool_name,
            "supplied_tool_contract_digest":supplied_digest,
            "supplied_digest_prefix":supplied_digest.chars().take(12).collect::<String>(),
            "current_tool_contract_digest":digest,
            "expected_contract_identity":{"connection_id":connection_id,"generation_id":connection.generation_id,"tool_name":tool_name},
            "current_generation_id":connection.generation_id,
            "next_call":{"tool_name":"mcp_loader_inspect_tool","arguments":{"connection_id":connection_id,"tool_name":tool_name,"include_tool_contract":"verbose"}}
        })));
    }
    let supplied =
        required_string(arguments, "schema_lease", "schema_lease_required").map_err(|_| {
            Diagnostic::new(
                "schema_lease_required",
                "schema_lease_or_contract_digest_required",
            )
            .with_details(json!({
                "connection_id": connection_id,
                "tool_name": tool_name,
                "accepted_authorization": ["schema_lease", "tool_schema_digest"],
                "next_call": {
                    "tool_name": "mcp_loader_inspect_tool",
                    "arguments": {"connection_id": connection_id, "tool_name": tool_name}
                }
            }))
        })?;
    let expected = schema_lease_token(state, connection, tool_name, &digest);
    if supplied != expected {
        return Err(
            Diagnostic::new("schema_lease_stale", "schema_lease_stale").with_details(json!({
                "connection_id": connection_id,
                "generation_id": connection.generation_id,
                "tool_name": tool_name,
                "tool_schema_digest": digest,
                "next_call": {
                    "tool_name": "mcp_loader_inspect_tool",
                    "arguments": {"connection_id": connection_id, "tool_name": tool_name}
                }
            })),
        );
    }
    Ok("lease_reused")
}

pub(crate) fn get_connection<'a>(
    arguments: &JsonObject,
    state: &'a LoaderState,
) -> Result<&'a Connection, Diagnostic> {
    let id = required_string(arguments, "connection_id", "missing_connection_id")?;
    state.connections.get(&id).ok_or_else(|| {
        Diagnostic::new(
            "connection_not_found",
            format!("connection_not_found:{}", id),
        )
        .with_details(json!({
            "connection_id":id,
            "recovery":{
                "inventory":{"tool_name":"mcp_loader_connection_inventory","arguments":{}},
                "resume_or_open":{"tool_name":"mcp_loader_resume_or_open_surface","required_arguments":["site_root","binding_id"]}
            }
        }))
    })
}

pub(crate) fn resolve_timeout(
    arguments: &Value,
    policy: &Policy,
) -> Result<(u64, bool, u64), Diagnostic> {
    let requested = arguments.get("timeout_ms");
    let Some(requested) = requested else {
        return Ok((policy.tool_call_timeout_ms, false, 0));
    };
    let requested = requested
        .as_u64()
        .or_else(|| {
            requested
                .as_i64()
                .and_then(|value| u64::try_from(value).ok())
        })
        .ok_or_else(|| Diagnostic::new("invalid_tool_call_timeout", "invalid_tool_call_timeout"))?;
    if requested == 0 || requested > MAX_TOOL_TIMEOUT_MS {
        return Err(Diagnostic::new(
            "tool_call_timeout_exceeds_loader_max",
            format!("tool_call_timeout_exceeds_loader_max:{}", requested),
        )
        .with_details(
            json!({"requested_timeout_ms":requested,"max_timeout_ms":MAX_TOOL_TIMEOUT_MS}),
        ));
    }
    Ok((
        requested.saturating_add(policy.tool_call_grace_ms),
        true,
        requested,
    ))
}
