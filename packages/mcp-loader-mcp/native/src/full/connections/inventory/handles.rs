use crate::full::*;

pub(crate) fn surface_handle_inventory(state: &LoaderState) -> Value {
    let mut handles = Vec::new();
    for handle in state.handles.values() {
        let connection = find_connection_for_handle(handle, state);
        handles.push(json!({
            "surface_handle":handle.handle,"handle_scope":"loader_process","logical_connection_id":handle.logical_connection_id,
            "binding_id":handle.binding_id,"site_root":handle.site_root,"surface_id":handle.surface_id,"runtime_kind":handle.runtime_kind,
            "created_at":handle.created_at,"connection_id":connection.as_ref().map(|value| value.connection_id.clone()),
            "generation_id":connection.as_ref().map(|value| value.generation_id.clone()),
            "status":if connection.as_ref().is_some_and(|value| connection_live(value)) {"live"} else {"unavailable"},
            "recovery":if let Some(connection) = connection {
                json!({"tool_name":"mcp_loader_surface_restart","arguments":{"connection_id":connection.connection_id}})
            } else {
                unavailable_handle_recovery(handle)
            }
        }));
    }
    handles.sort_by(|left, right| {
        left.get("surface_handle")
            .and_then(Value::as_str)
            .cmp(&right.get("surface_handle").and_then(Value::as_str))
    });
    json!({"schema":"narada.mcp_loader.surface_handle_inventory.v1","status":"ok","handle_scope":"loader_process","handle_count":handles.len(),"handles":handles})
}

pub(crate) fn find_connection_for_handle<'a>(
    handle: &SurfaceHandle,
    state: &'a LoaderState,
) -> Option<&'a Connection> {
    let mut matches: Vec<&Connection> = state
        .connections
        .values()
        .filter(|connection| connection.logical_connection_id == handle.logical_connection_id)
        .collect();
    matches.sort_by_key(|connection| std::cmp::Reverse(connection.attached_ms));
    matches
        .iter()
        .find(|connection| connection_live(connection))
        .copied()
        .or_else(|| matches.first().copied())
}

pub(crate) fn unavailable_handle_recovery(handle: &SurfaceHandle) -> Value {
    if let Some(binding_id) = &handle.binding_id {
        json!({"tool_name":"mcp_loader_resume_or_open_surface","arguments":{"site_root":handle.site_root,"binding_id":binding_id,"surface_id":handle.surface_id,"runtime_kind":handle.runtime_kind}})
    } else {
        json!({"status":"unavailable","reason":"surface_handle_binding_id_unavailable","instruction":"Open the surface again from a canonical Site binding; this legacy handle cannot be resumed safely."})
    }
}

pub(crate) fn list_attached_tools(
    arguments: &JsonObject,
    state: &LoaderState,
) -> Result<Value, Diagnostic> {
    let connection = get_connection(arguments, state)?;
    let tools = connection
        .tools
        .iter()
        .map(compact_tool_contract)
        .collect::<Vec<_>>();
    let mut result = json!({"schema":"narada.mcp_loader.tools.v1","connection_id":connection.connection_id,"surface_id":connection.surface_id,"compact":true,"tool_count":tools.len(),"tools":tools});
    if arguments
        .get("include_runtime_metadata")
        .and_then(Value::as_bool)
        == Some(true)
    {
        result["runtime_lifecycle"] =
            runtime_lifecycle(Some(&connection.connection_id), Some(&connection.lifecycle));
        result["runtime_freshness"] = runtime_freshness(state);
    }
    Ok(result)
}

pub(crate) fn compact_tool_contract(tool: &Value) -> Value {
    json!({
        "name": tool.get("name").cloned().unwrap_or(Value::Null),
        "description": tool.get("description").cloned().unwrap_or(Value::Null),
        "annotations": tool.get("annotations").cloned().unwrap_or(Value::Null)
    })
}

pub(crate) fn compact_input_contract(input_schema: &Value) -> Value {
    let properties = input_schema
        .get("properties")
        .and_then(Value::as_object)
        .map(|properties| properties.keys().cloned().collect::<Vec<_>>())
        .unwrap_or_default();
    json!({
        "required": input_schema.get("required").cloned().unwrap_or_else(|| json!([])),
        "properties": properties,
        "additional_properties": input_schema.get("additionalProperties").cloned().unwrap_or(Value::Null)
    })
}

pub(crate) fn surface_status(
    arguments: &JsonObject,
    state: &LoaderState,
) -> Result<Value, Diagnostic> {
    let connection = get_connection(arguments, state)?;
    let mut result = connection_status(connection, state)
        .as_object()
        .cloned()
        .unwrap_or_default();
    result.insert(
        "schema".to_string(),
        json!("narada.mcp_loader.surface_status.v1"),
    );
    Ok(Value::Object(result))
}
pub(crate) fn tool_discovery_manifest(
    arguments: &JsonObject,
    state: &LoaderState,
) -> Result<Value, Diagnostic> {
    let connection = get_connection(arguments, state)?;
    let tools: Vec<Value> = connection
        .tools
        .iter()
        .map(|tool| {
            json!({
                "canonical_name":tool.get("name").and_then(Value::as_str).unwrap_or_default(),
                "callable_name":tool.get("name").and_then(Value::as_str).unwrap_or_default(),
                "generated_aliases":[]
            })
        })
        .collect();
    let mut result = json!({"schema":"narada.mcp_loader.tool_discovery_manifest.v1","connection_id":connection.connection_id,"surface_id":connection.surface_id,"compact":true,"tool_count":tools.len(),"alias_policy":{"canonical_name_source":"tools/list.name","generated_aliases_authoritative":false,"guidance":"Use canonical_name/callable_name for directives and tool calls. Client-generated aliases should be treated as compatibility UI labels only. Obtain an exact schema only by inspecting one named tool and receiving its schema lease."},"tools":tools});
    if arguments
        .get("include_runtime_metadata")
        .and_then(Value::as_bool)
        == Some(true)
    {
        result["runtime_lifecycle"] =
            runtime_lifecycle(Some(&connection.connection_id), Some(&connection.lifecycle));
        result["runtime_freshness"] = runtime_freshness(state);
    }
    Ok(result)
}
