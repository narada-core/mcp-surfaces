use crate::full::*;

pub(crate) fn child_tool_contract<'a>(
    connection: &'a Connection,
    tool_name: &str,
) -> Result<&'a Value, Diagnostic> {
    connection
        .tools
        .iter()
        .find(|tool| tool.get("name").and_then(Value::as_str) == Some(tool_name))
        .ok_or_else(|| {
            Diagnostic::new(
                "child_tool_not_found",
                format!("child_tool_not_found:{tool_name}"),
            )
            .with_details(json!({
                "connection_id": connection.connection_id,
                "surface_id": connection.surface_id,
                "tool_name": tool_name
            }))
        })
}

pub(crate) fn child_tool_schema_digest(tool: &Value) -> String {
    sha256(&stable_json(tool))
}

pub(crate) fn schema_lease_token(
    state: &LoaderState,
    connection: &Connection,
    tool_name: &str,
    schema_digest: &str,
) -> String {
    schema_lease_digest(
        &state.schema_lease_secret,
        &connection.connection_id,
        &connection.generation_id,
        tool_name,
        schema_digest,
    )
}

pub(crate) fn schema_lease_digest(
    secret: &str,
    connection_id: &str,
    generation_id: &str,
    tool_name: &str,
    schema_digest: &str,
) -> String {
    sha256(&stable_json(&json!({
        "secret": secret,
        "connection_id": connection_id,
        "generation_id": generation_id,
        "tool_name": tool_name,
        "tool_schema_digest": schema_digest
    })))
}

pub(crate) fn inspect_attached_tool(
    arguments: &JsonObject,
    state: &LoaderState,
) -> Result<Value, Diagnostic> {
    let connection = get_connection(arguments, state)?;
    let tool_name = required_string(arguments, "tool_name", "missing_tool_name")?;
    let tool = child_tool_contract(connection, &tool_name)?;
    let tool_schema_digest = child_tool_schema_digest(tool);
    let input_schema = tool
        .get("inputSchema")
        .or_else(|| tool.get("input_schema"))
        .cloned()
        .unwrap_or(Value::Null);
    let output_schema = tool
        .get("outputSchema")
        .or_else(|| tool.get("output_schema"))
        .cloned()
        .unwrap_or(Value::Null);
    let mut result = json!({
        "schema": "narada.mcp_loader.schema_lease.v1",
        "status": "issued",
        "connection_id": connection.connection_id,
        "logical_connection_id": connection.logical_connection_id,
        "generation_id": connection.generation_id,
        "surface_id": connection.surface_id,
        "tool_name": tool_name,
        "tool_schema_digest": tool_schema_digest,
        "tool_contract_digest": tool_schema_digest,
        "input_schema_digest": sha256(&stable_json(&input_schema)),
        "output_schema_digest": sha256(&stable_json(&output_schema)),
        "description": tool.get("description").cloned().unwrap_or(Value::Null),
        "annotations": tool.get("annotations").cloned().unwrap_or(Value::Null),
        "input_contract": compact_input_contract(&input_schema),
        "schema_lease": schema_lease_token(state, connection, &tool_name, &tool_schema_digest),
        "lease_scope": "loader_process_child_generation",
        "transferable": false
    });
    result["authorization_resolution"] = json!("lease_renewed");
    if arguments
        .get("include_tool_contract")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        result["tool_contract"] = tool.clone();
    }
    Ok(result)
}
