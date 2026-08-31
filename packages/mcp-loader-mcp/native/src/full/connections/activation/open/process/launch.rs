use crate::full::*;

pub(crate) fn build_child_spec(
    command: &str,
    entrypoint: &str,
    args: &[String],
    child_invocation_kind: &str,
) -> ChildSpec {
    let base = command
        .replace('\\', "/")
        .rsplit('/')
        .next()
        .unwrap_or(command)
        .to_ascii_lowercase();
    let mut child_args = Vec::new();
    if child_invocation_kind == "native_entrypoint" || child_invocation_kind == "native_applet" {
        child_args.extend(args.iter().cloned());
    } else if [
        "node", "node.exe", "node.cmd", "bun", "bun.exe", "deno", "deno.exe",
    ]
    .contains(&base.as_str())
    {
        child_args.push(entrypoint.to_string());
        child_args.extend(args.iter().cloned());
    } else if normalize_path(command) == normalize_path(entrypoint) {
        child_args.extend(args.iter().cloned());
    } else {
        child_args.push(entrypoint.to_string());
        child_args.extend(args.iter().cloned());
    }
    ChildSpec {
        command: command.to_string(),
        args: child_args,
    }
}

pub(crate) fn build_child_env(
    site_root: &str,
    policy: &Policy,
    connection_id: &str,
    logical_id: &str,
    generation_id: &str,
    marker: &str,
) -> HashMap<String, String> {
    let mut env_map = HashMap::new();
    for key in &policy.allowed_env_vars {
        if let Ok(value) = env::var(key) {
            env_map.insert(key.clone(), value);
        }
    }
    env_map.insert("NARADA_SITE_ROOT".to_string(), site_root.to_string());
    env_map.insert(
        "NARADA_MCP_LOADER_RUN_ID".to_string(),
        marker
            .strip_prefix("narada.mcp.loader/")
            .unwrap_or(marker)
            .to_string(),
    );
    env_map.insert(
        "NARADA_MCP_LOADER_CONNECTION_ID".to_string(),
        connection_id.to_string(),
    );
    env_map.insert(
        "NARADA_MCP_LOADER_LOGICAL_CONNECTION_ID".to_string(),
        logical_id.to_string(),
    );
    env_map.insert(
        "NARADA_MCP_LOADER_GENERATION_ID".to_string(),
        generation_id.to_string(),
    );
    env_map.insert(
        "NARADA_MCP_LOADER_OWNER_PID".to_string(),
        std::process::id().to_string(),
    );
    env_map.insert(
        "NARADA_MCP_LOADER_PARENT_PID".to_string(),
        std::process::id().to_string(),
    );
    env_map.insert(
        "NARADA_MCP_LOADER_OWNERSHIP_MARKER".to_string(),
        marker.to_string(),
    );
    env_map
}

pub(crate) fn attached_response(connection: &Connection, state: &LoaderState) -> Value {
    json!({
        "schema":"narada.mcp_loader.surface_attached.v1",
        "connection_id":connection.connection_id,"logical_connection_id":connection.logical_connection_id,
        "generation_id":connection.generation_id,"site_root":connection.site_root,"surface_id":connection.surface_id,
        "binding_id":connection.binding_id,"admission_envelope_id":connection.admission_envelope_id,
        "binding_digest":connection.admitted_binding_digest,"authority_epoch":connection.authority_epoch,
        "runtime_kind":connection.runtime_kind,"runtime_requirements":connection.runtime_requirements,
        "runtime_lifecycle":runtime_lifecycle(Some(&connection.connection_id),Some(&connection.lifecycle)),
        "runtime_freshness":runtime_freshness(state),"runtime_command":connection.runtime_command,"entrypoint":connection.entrypoint,"args":connection.args,"child_invocation_kind":connection.child_invocation_kind,
        "server_info":connection.server_info,"tool_count":connection.tools.len(),
        "tool_discovery":{"tool_name":"mcp_loader_list_tools","arguments":{"connection_id":connection.connection_id}},
        "tool_inspection":{"tool_name":"mcp_loader_inspect_tool","required_arguments":["connection_id","tool_name"]},
        "descriptor_digest":connection.descriptor_digest,
        "tool_contract_digest":connection.tool_contract_digest,"declared_tool_contract_digest":connection.declared_tool_contract_digest,
        "lifecycle":connection.lifecycle,"ownership":connection_ownership(connection)
    })
}
