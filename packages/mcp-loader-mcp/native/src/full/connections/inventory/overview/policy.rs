use crate::full::*;

pub(crate) fn policy_inspect(state: &LoaderState) -> Value {
    let admission = state.binding_admission.as_ref().map(|envelope| json!({
        "status":"admitted","envelope_id":envelope.get("envelope_id"),"envelope_digest":envelope.get("envelope_digest"),
        "binding_count":envelope.get("bindings").and_then(Value::as_array).map(Vec::len).unwrap_or(0),
        "authority_epoch":envelope.get("authority_epoch"),"carrier_session_id":envelope.get("carrier_session_id")
    })).unwrap_or_else(|| json!({"status":if state.standalone_ambient_attachment{"standalone_ambient"}else{"required_missing"}}));
    json!({"schema":"narada.mcp_loader.policy.v1","binding_admission":admission,"policy":{
        "allowedSiteRoots":state.policy.allowed_site_roots,"allowedEntrypointPrefixes":state.policy.allowed_entrypoint_prefixes,
        "allowedSurfaceIds":state.policy.allowed_surface_ids.as_ref().map(|ids| json!(ids)).unwrap_or_else(|| json!("site_fabric")),
        "allowedEnvVars":state.policy.allowed_env_vars,"maxConnections":state.policy.max_connections,
        "maxRequestBytes":state.policy.max_request_bytes,"maxResponseBytes":state.policy.max_response_bytes,
        "attachTimeoutMs":state.policy.attach_timeout_ms,"toolCallTimeoutMs":state.policy.tool_call_timeout_ms,
        "toolCallGraceMs":state.policy.tool_call_grace_ms
    }})
}

pub(crate) fn connection_inventory(arguments: &JsonObject, state: &LoaderState) -> Value {
    let compact = arguments
        .get("compact")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let mut connections = state.connections.values().collect::<Vec<_>>();
    connections.sort_by(|left, right| left.connection_id.cmp(&right.connection_id));
    let entries: Vec<Value> = connections.iter().map(|connection| {
        let live = connection_live(connection);
        let mut entry = if compact { Map::new() } else { connection_status(connection, state).as_object().cloned().unwrap_or_default() };
        entry.insert("connection_id".to_string(), json!(connection.connection_id));
        entry.insert("binding_id".to_string(), json!(connection.binding_id));
        entry.insert("generation_id".to_string(), json!(connection.generation_id));
        entry.insert("surface_id".to_string(), json!(connection.surface_id));
        entry.insert("liveness".to_string(), json!(if live {"live"} else {"closed"}));
        entry.insert("age_ms".to_string(), json!(now_ms().saturating_sub(connection.attached_ms)));
        entry.insert("pending_request_count".to_string(), json!(connection.session.pending.lock().map(|pending| pending.len()).unwrap_or(0)));
        entry.insert("actions".to_string(), json!({
            "inspect":{"tool_name":"mcp_loader_surface_status","arguments":{"connection_id":connection.connection_id}},
            "detach":{"tool_name":"mcp_loader_detach","arguments":{"connection_id":connection.connection_id}},
            "restart":connection_recovery_actions(connection).first().cloned().unwrap_or(Value::Null),
            "ownership":{"tool_name":"mcp_loader_process_ownership","arguments":{}}
        }));
        Value::Object(entry)
    }).collect();
    let live_ids: Vec<String> = connections
        .iter()
        .filter(|connection| connection_live(connection))
        .map(|connection| connection.connection_id.clone())
        .collect();
    let closed_ids: Vec<String> = connections
        .iter()
        .filter(|connection| !connection_live(connection))
        .map(|connection| connection.connection_id.clone())
        .collect();
    json!({
        "schema":"narada.mcp_loader.connection_inventory.v1","status":"ok","compact":compact,
        "max_connections":state.policy.max_connections,"connection_count":entries.len(),
        "available_slots":state.policy.max_connections.saturating_sub(entries.len()),"live_count":live_ids.len(),"closed_count":closed_ids.len(),
        "live_connection_ids":live_ids,"closed_connection_ids":closed_ids,"connections":entries,
        "runtime_freshness":if compact { Value::Null } else { runtime_freshness(state) },
        "recovery":if compact { Value::Null } else { json!({
            "when_full":"Inspect this inventory, then detach closed or no-longer-needed connections. Use surface restart only for an intentionally live replacement.",
            "inspect_tool":"mcp_loader_surface_status","detach_tool":"mcp_loader_detach","restart_tool":"mcp_loader_surface_restart",
            "ownership_tool":"mcp_loader_process_ownership","note":"The inventory is read-only and does not reap children or free slots automatically."
        }) }
    })
}

pub(crate) fn process_ownership(state: &LoaderState) -> Value {
    let mut processes = Vec::new();
    for connection in state.connections.values() {
        let live = connection_live(connection);
        let mut entry = connection_ownership(connection)
            .as_object()
            .cloned()
            .unwrap_or_default();
        entry.insert("connection_id".to_string(), json!(connection.connection_id));
        entry.insert(
            "logical_connection_id".to_string(),
            json!(connection.logical_connection_id),
        );
        entry.insert("generation_id".to_string(), json!(connection.generation_id));
        entry.insert("pid".to_string(), json!(connection.session.pid));
        entry.insert(
            "status".to_string(),
            json!(if live { "live" } else { "closed" }),
        );
        entry.insert("ownership_status".to_string(), json!("loader_owned"));
        entry.insert(
            "descendant_scope".to_string(),
            json!("direct_child_process_only"),
        );
        entry.insert("cleanup".to_string(), if live {
            json!({"status":"not_eligible","action":{"tool_name":"mcp_loader_detach","arguments":{"connection_id":connection.connection_id}}})
        } else {
            json!({"status":"safe_to_reconcile","action":{"tool_name":"mcp_loader_detach","arguments":{"connection_id":connection.connection_id}}})
        });
        processes.push(Value::Object(entry));
    }
    processes.sort_by(|left, right| {
        left.get("connection_id")
            .and_then(Value::as_str)
            .cmp(&right.get("connection_id").and_then(Value::as_str))
    });
    let safe_closed: Vec<Value> = processes
        .iter()
        .filter(|process| process.get("status").and_then(Value::as_str) == Some("closed"))
        .filter_map(|process| process.get("connection_id").cloned())
        .collect();
    json!({
        "schema":"narada.mcp_loader.process_ownership.v1","status":"ok",
        "loader":{"run_id":state.run_id,"pid":state.owner_pid,"ownership_marker":state.ownership_marker,"started_at":ms_to_iso(state.started_ms)},
        "scope":"known_direct_children_spawned_by_this_loader_run","processes":processes,
        "safe_reconciliation_connection_ids":safe_closed,
        "external_process_policy":"unowned_or_unobserved_processes_are_not_enumerated_or_terminated",
        "host_process_reconciliation":{"status":"not_available","reason":"mcp-loader has no authority to enumerate arbitrary host processes or conhost descendants","conhost_descendants":"not_enumerated","remediation":"Use the host/runtime supervisor for external process inspection; use mcp_loader_detach for a known loader-owned connection."}
    })
}

pub(crate) fn owned_port_lookup(
    arguments: &JsonObject,
    state: &LoaderState,
) -> Result<Value, Diagnostic> {
    let port = arguments
        .get("port")
        .and_then(Value::as_u64)
        .filter(|value| (1..=65535).contains(value))
        .ok_or_else(|| Diagnostic::new("invalid_port", "invalid_port"))?;
    let owned: HashMap<u32, &Connection> = state
        .connections
        .values()
        .map(|connection| (connection.session.pid, connection))
        .collect();
    #[cfg(windows)]
    let output = Command::new("netstat")
        .args(["-ano", "-p", "tcp"])
        .creation_flags(0x0800_0000)
        .output();
    #[cfg(not(windows))]
    let output = Command::new("ss").args(["-ltnp"]).output();
    let stdout = output
        .map_err(|error| Diagnostic::new("owned_port_lookup_failed", error.to_string()))?
        .stdout;
    let text = String::from_utf8_lossy(&stdout);
    let needle = format!(":{port}");
    let mut owners = Vec::new();
    for line in text.lines().filter(|line| line.contains(&needle)) {
        let Some(pid) = line
            .split_whitespace()
            .last()
            .and_then(|value| value.parse::<u32>().ok())
        else {
            continue;
        };
        let Some(connection) = owned.get(&pid) else {
            continue;
        };
        owners.push(json!({
            "port":port,
            "pid":pid,
            "connection_id":connection.connection_id,
            "binding_id":connection.binding_id,
            "surface_id":connection.surface_id,
            "ownership_status":"loader_owned_direct_child",
            "actions":{
                "inspect":{"tool_name":"mcp_loader_surface_status","arguments":{"connection_id":connection.connection_id}},
                "restart":{"tool_name":"mcp_loader_surface_restart","arguments":{"connection_id":connection.connection_id,"reason":"owned_port_restart"}},
                "detach":{"tool_name":"mcp_loader_detach","arguments":{"connection_id":connection.connection_id}}
            }
        }));
    }
    Ok(
        json!({"schema":"narada.mcp_loader.owned_port_lookup.v1","status":"ok","port":port,"owner_count":owners.len(),"owners":owners,"scope":"loader_owned_direct_children_only"}),
    )
}
