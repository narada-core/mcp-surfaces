use crate::full::*;

pub(crate) fn stable_value(value: &Value) -> Value {
    match value {
        Value::Object(object) => {
            let mut entries: Vec<_> = object.iter().collect();
            entries.sort_by(|left, right| left.0.cmp(right.0));
            let mut output = Map::new();
            for (key, value) in entries {
                output.insert(key.clone(), stable_value(value));
            }
            Value::Object(output)
        }
        Value::Array(values) => Value::Array(values.iter().map(stable_value).collect()),
        _ => value.clone(),
    }
}

pub(crate) fn stable_json(value: &Value) -> String {
    serde_json::to_string(&stable_value(value)).unwrap_or_else(|_| "null".to_string())
}

pub(crate) fn pretty_json(value: &Value) -> String {
    serde_json::to_string_pretty(value).unwrap_or_else(|_| "null".to_string())
}

pub(crate) fn sha256(value: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(value.as_bytes());
    hex::encode(hasher.finalize())
}

pub(crate) fn utf16_len(value: &str) -> usize {
    value.encode_utf16().count()
}

pub(crate) fn json_byte_len(value: &Value) -> usize {
    serde_json::to_vec(value)
        .map(|bytes| bytes.len())
        .unwrap_or(usize::MAX)
}

pub(crate) fn connection_ownership(connection: &Connection) -> Value {
    json!({
        "owner":"mcp-loader","owner_run_id":connection.owner_run_id,"owner_pid":connection.owner_pid,
        "parent_pid":connection.parent_pid,"ownership_marker":connection.ownership_marker,
        "cleanup_scope":"loader_owned_child_only"
    })
}

pub(crate) fn connection_live(connection: &Connection) -> bool {
    !connection.detached && connection.session.alive()
}

pub(crate) fn touch_connection(connection: &mut Connection) {
    connection.heartbeat_ms = now_ms();
    connection.lease_expires_ms = connection.heartbeat_ms + DEFAULT_RUNTIME_LEASE_MS as u128;
}

pub(crate) fn lifecycle_mode(connection: &Connection) -> Option<&str> {
    connection.lifecycle.get("mode").and_then(Value::as_str)
}

pub(crate) fn connection_recovery_actions(connection: &Connection) -> Vec<Value> {
    if lifecycle_mode(connection) != Some("replayable") {
        vec![json!({
            "actuator":"carrier-supervisor","tool_name":Value::Null,
            "arguments":{"connection_id":connection.connection_id,"logical_connection_id":connection.logical_connection_id,"capability":"restart_mcp_loader_process"},
            "guidance":"This projection is not loader-replayable. Ask the carrier supervisor to invoke restart_mcp_loader_process for the attached MCP loader before reconnecting the session."
        })]
    } else {
        vec![json!({
            "actuator":"mcp-loader","tool_name":"mcp_loader_surface_restart",
            "arguments":{"connection_id":connection.connection_id},
            "guidance":"Invoke mcp_loader_surface_restart with the connection_id to replace this child generation; this does not restart the agent session or loader process."
        })]
    }
}

pub(crate) fn runtime_generation(connection: &Connection, observed_at_ms: u128) -> Value {
    let fresh = connection.lease_expires_ms > observed_at_ms;
    json!({
        "generation_id":connection.generation_id,"state":"active","started_at":ms_to_iso(connection.attached_ms),
        "activated_at":ms_to_iso(connection.attached_ms),"heartbeat_at":ms_to_iso(connection.heartbeat_ms),
        "lease_expires_at":ms_to_iso(connection.lease_expires_ms),
        "freshness":if fresh {"current"} else {"stale"},
        "health":if connection_live(connection) {"healthy"} else {"unreachable"},
        "descriptor_digest":connection.descriptor_digest,
        "tool_contract_digest":connection.tool_contract_digest,
        "inflight":connection.session.pending.lock().map(|pending| pending.len()).unwrap_or(0)
    })
}

pub(crate) fn connection_status(connection: &Connection, state: &LoaderState) -> Value {
    let live = connection_live(connection);
    let value = json!({
        "connection_id":connection.connection_id,"ownership":connection_ownership(connection),
        "logical_connection_id":connection.logical_connection_id,"generation_id":connection.generation_id,
        "site_root":connection.site_root,"surface_id":connection.surface_id,"server_name":connection.server_name,
        "projection_id":connection.projection_id,"execution":connection.execution,
        "runtime_kind":connection.runtime_kind,"runtime_requirements":connection.runtime_requirements,
        "lifecycle":connection.lifecycle,
        "runtime_lifecycle":runtime_lifecycle(Some(&connection.connection_id),Some(&connection.lifecycle)),
        "runtime_freshness":runtime_freshness(state),"runtime_command":connection.runtime_command,"entrypoint":connection.entrypoint,"args":connection.args,"child_invocation_kind":connection.child_invocation_kind,
        "status":if live {"live"} else {"closed"},"detached":connection.detached,"initialized":connection.initialized,
        "pid":connection.session.pid,"exit_code":connection.session.exit_code(),"signal_code":connection.session.signal_code(),
        "killed":connection.session.killed(),"pending_count":connection.session.pending.lock().map(|pending| pending.len()).unwrap_or(0),
        "attached_at":ms_to_iso(connection.attached_ms),"detached_at":connection.detached_ms.map(ms_to_iso),
        "stderr_tail":connection.session.stderr_tail(),"server_info":connection.server_info,
        "tool_count":connection.tools.len(),"descriptor_digest":connection.descriptor_digest,
        "declared_tool_contract_digest":connection.declared_tool_contract_digest,"tool_contract_digest":connection.tool_contract_digest,
        "heartbeat_at":ms_to_iso(connection.heartbeat_ms),"lease_expires_at":ms_to_iso(connection.lease_expires_ms),
        "active_generation":if live {runtime_generation(connection,now_ms())} else {Value::Null},
        "draining_generations":[],"recovery_actions":connection_recovery_actions(connection)
    });
    value
}

pub(crate) fn observed_tool_digest(tools: &[Value], _descriptor: Option<&Value>) -> Option<String> {
    let mut canonical = Vec::new();
    for tool in tools {
        let Some(object) = tool.as_object() else {
            continue;
        };
        if object.get("name").and_then(Value::as_str) == Some(RUNTIME_PROXY_STATUS_TOOL_NAME) {
            continue;
        }
        let mut entry = Map::new();
        entry.insert(
            "name".to_string(),
            object.get("name").cloned().unwrap_or(Value::Null),
        );
        entry.insert(
            "description".to_string(),
            object.get("description").cloned().unwrap_or(Value::Null),
        );
        entry.insert(
            "input_schema".to_string(),
            object
                .get("inputSchema")
                .or_else(|| object.get("input_schema"))
                .cloned()
                .unwrap_or_else(|| json!({})),
        );
        entry.insert(
            "output_schema".to_string(),
            object
                .get("outputSchema")
                .or_else(|| object.get("output_schema"))
                .cloned()
                .unwrap_or(Value::Null),
        );
        entry.insert(
            "annotations".to_string(),
            object.get("annotations").cloned().unwrap_or(Value::Null),
        );
        canonical.push(Value::Object(entry));
    }
    if canonical.is_empty() {
        None
    } else {
        canonical.sort_by(|left, right| {
            left.get("name")
                .and_then(Value::as_str)
                .cmp(&right.get("name").and_then(Value::as_str))
        });
        Some(sha256(&stable_json(&Value::Array(canonical))))
    }
}
