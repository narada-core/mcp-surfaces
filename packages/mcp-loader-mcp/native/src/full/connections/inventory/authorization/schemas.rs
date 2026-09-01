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

pub(crate) fn apply_contract_mode(
    result: &mut Value,
    mode: &str,
    input_schema: &Value,
    tool: &Value,
) {
    if mode == "compact" {
        result["input_contract"] = compact_input_contract(input_schema);
    } else if mode == "verbose" {
        result["tool_contract"] = tool.clone();
    }
}

fn minimal_schema_value(schema: &Value) -> Value {
    if let Some(value) = schema.get("default") {
        return value.clone();
    }
    if let Some(value) = schema
        .get("enum")
        .and_then(Value::as_array)
        .and_then(|items| items.first())
    {
        return value.clone();
    }
    match schema.get("type").and_then(Value::as_str) {
        Some("object") => {
            let mut result = Map::new();
            if let (Some(required), Some(properties)) = (
                schema.get("required").and_then(Value::as_array),
                schema.get("properties").and_then(Value::as_object),
            ) {
                for name in required.iter().filter_map(Value::as_str) {
                    if let Some(child) = properties.get(name) {
                        result.insert(name.into(), minimal_schema_value(child));
                    }
                }
            }
            let minimum = schema.get("minProperties").and_then(Value::as_u64).unwrap_or(0) as usize;
            while result.len() < minimum {
                result.insert(format!("required_property_{}", result.len() + 1), json!("<required value>"));
            }
            Value::Object(result)
        }
        Some("array") => {
            let item = minimal_schema_value(schema.get("items").unwrap_or(&Value::Null));
            Value::Array(vec![item; schema.get("minItems").and_then(Value::as_u64).unwrap_or(1) as usize])
        }
        Some("string") => json!("<required string>"),
        Some("integer") | Some("number") => {
            schema.get("minimum").cloned().unwrap_or_else(|| json!(0))
        }
        Some("boolean") => json!(false),
        Some("null") => Value::Null,
        _ => Value::Null,
    }
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
    let contract_mode = match arguments.get("include_tool_contract") {
        None => "compact",
        Some(Value::Bool(false)) => "false",
        Some(Value::String(mode)) if mode == "compact" => "compact",
        Some(Value::String(mode)) if mode == "verbose" => "verbose",
        _ => {
            return Err(Diagnostic::new(
                "include_tool_contract_invalid",
                "include_tool_contract must be false, compact, or verbose",
            ))
        }
    };
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
        "minimal_valid_arguments":minimal_schema_value(&input_schema),
        "verbose_contract_call":{"tool_name":"mcp_loader_inspect_tool","arguments":{"connection_id":connection.connection_id,"tool_name":tool_name,"include_tool_contract":"verbose"}},
        "schema_lease": schema_lease_token(state, connection, &tool_name, &tool_schema_digest),
        "lease_scope": "loader_process_child_generation",
        "transferable": false
    });
    result["authorization_resolution"] = json!("lease_renewed");
    apply_contract_mode(&mut result, contract_mode, &input_schema, tool);
    Ok(result)
}
