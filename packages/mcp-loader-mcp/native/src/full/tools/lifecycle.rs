use crate::full::*;

pub(crate) fn supervisor_restart_action() -> Value {
    json!({
        "schema":"narada.mcp_loader.supervisor_restart_action.v1",
        "kind":"restart_loader_process",
        "target":"mcp-loader-process",
        "owner":"carrier_or_runtime_supervisor",
        "operation":"restart",
        "capability":"restart_mcp_loader_process",
        "tool_name":"restart_mcp_loader_process",
        "arguments":{},
        "actuator_scope":"external_supervisor_capability",
        "agent_callable":false,
        "availability":"external_supervisor_only",
        "invocation_note":"This is a carrier/runtime-supervisor capability name, not a tool exposed by mcp-loader. The agent must call the carrier supervisor only when that capability is separately present.",
        "next_call":{"tool_name":"restart_mcp_loader_process","arguments":{}},
        "connection_id_required":false,
        "session_restart_required":false
    })
}

pub(crate) fn runtime_lifecycle(connection_id: Option<&str>, lifecycle: Option<&Value>) -> Value {
    let attached = connection_id.is_some();
    let mode = lifecycle
        .and_then(|value| value.get("mode"))
        .and_then(Value::as_str);
    let non_replayable = attached && mode.is_some_and(|value| value != "replayable");
    let restartable = if non_replayable {
        Value::Bool(false)
    } else if attached {
        Value::Bool(true)
    } else {
        Value::Null
    };
    let mut result = json!({
        "schema":"narada.mcp_loader.runtime_lifecycle.v1",
        "managed_by":"mcp-loader",
        "restartable":restartable,
        "restartability_status":if non_replayable {"unavailable_for_lifecycle"} else if attached {"available"} else {"available_after_successful_attach"},
        "restart_scope":if non_replayable {"carrier_supervisor"} else {"attached_child_process"},
        "session_restart_required":false,
        "connection_id_required":true,
        "inventory_tool":"mcp_loader_connection_inventory",
        "status_tool":"mcp_loader_surface_status",
        "restart_tool":if non_replayable {Value::Null} else {json!("mcp_loader_surface_restart")},
        "loader_restart_action":supervisor_restart_action(),
        "guidance":if non_replayable {
            format!("This projection declares lifecycle mode {}; mcp-loader must not replace its child. Ask the carrier or runtime supervisor to invoke restart_mcp_loader_process, then reconnect the surface.", mode.unwrap_or("unknown"))
        } else {
            "Restart replaces only the attached child surface process; it does not restart the agent session or reload the mcp-loader process.".to_string()
        }
    });
    if attached && !non_replayable {
        let id = connection_id.unwrap_or_default();
        result["actions"] = json!({
            "inspect":{"tool_name":"mcp_loader_surface_status","arguments":{"connection_id":id}},
            "restart":{"tool_name":"mcp_loader_surface_restart","arguments":{"connection_id":id}}
        });
    }
    result
}
