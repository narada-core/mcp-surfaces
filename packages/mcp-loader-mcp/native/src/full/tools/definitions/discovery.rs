use crate::full::*;

pub(crate) fn guidance_definition() -> Value {
    json!({
        "name":"mcp_loader_guidance",
        "description":"Show model-facing operating guidance for mcp-loader MCP workflows.",
        "inputSchema":{"type":"object","properties":{
            "workflow":{"type":"string","description":"Optional workflow name or area to focus guidance on."},
            "tool":{"type":"string","description":"Optional tool name for tool-specific guidance."}
        },"additionalProperties":false},
        "annotations":{"title":"mcp_loader_guidance","readOnlyHint":true,"destructiveHint":false,"idempotentHint":true,"openWorldHint":false},
        "outputSchema":{"type":"object","additionalProperties":true}
    })
}

pub(crate) fn tool_definition(
    name: &str,
    description: &str,
    properties: Value,
    required: &[&str],
    read_only: bool,
    destructive: bool,
) -> Value {
    json!({
        "name":name,
        "description":description,
        "annotations":{"title":name,"readOnlyHint":read_only,"destructiveHint":destructive,"idempotentHint":read_only,"openWorldHint":true},
        "inputSchema":{"type":"object","properties":properties,"additionalProperties":false,"required":required},
        "outputSchema":{"type":"object","additionalProperties":true}
    })
}

pub(crate) fn list_tools() -> Vec<Value> {
    let mut tools = vec![
        guidance_definition(),
        tool_definition("mcp_loader_runtime_status","Inspect whether this loader process is current relative to its runtime, source, dependency, and build-configuration evidence and whether the loader process itself must be restarted.",json!({}),&[],true,false),
        tool_definition("mcp_loader_policy_inspect","Inspect the policy governing runtime MCP surface loading.",json!({}),&[],true,false),
        tool_definition("mcp_loader_connection_inventory","List attached loader connections. Compact mode omits repeated runtime manifests and recovery guidance.",json!({"compact":{"type":"boolean","default":false}}),&[],true,false),
        tool_definition("mcp_loader_process_ownership","Inspect process ownership for children spawned by this loader run. This is a read-only reconciliation view: it reports loader-owned direct children and safe cleanup actions, but never enumerates or terminates unrelated host processes or conhost descendants.",json!({}),&[],true,false),
        tool_definition("mcp_loader_owned_port_lookup","Look up listeners on one port, returning only exact direct child processes owned by this loader.",json!({"port":{"type":"integer","minimum":1,"maximum":65535}}),&["port"],true,false),
        tool_definition("mcp_loader_runtime_observation","Return the normalized V2 runtime observation for one attached surface, including stable logical identity, generation state, lifecycle eligibility, contract digests, and bounded actuator guidance.",json!({"connection_id":{"type":"string"},"carrier_kind":{"type":"string"},"manifest_digest":{"type":"string"}}),&["connection_id","carrier_kind"],true,false),
        tool_definition("mcp_loader_list_site_surfaces","List resolvable MCP surfaces declared in a site's local fabric. Runtime metadata is opt-in.",json!({"site_root":{"type":"string"},"include_runtime_metadata":{"type":"boolean","default":false}}),&["site_root"],true,false),
        tool_definition("mcp_loader_site_fabric_diagnostics","Inspect site MCP fabric provenance and classify shared-registry drift or intentional entrypoint overrides.",json!({"site_root":{"type":"string"}}),&["site_root"],true,false),
        tool_definition("mcp_loader_site_tool_inventory_check","Compare site fabric declarations with fresh child tools/list responses; compact output includes per-finding status and tool-name deltas, runtime-skipped surfaces produce partial coverage, and an immutable observation_ref is materialized for Registrar conformance checks.",json!({"site_root":{"type":"string"},"surface_ids":{"type":"array","items":{"type":"string"}},"runtime_kind":{"type":"string"},"include_ok":{"type":"boolean"}}),&["site_root"],false,false),
        tool_definition("mcp_loader_attach_surface","Spawn and initialize an exactly admitted stdio MCP binding, return a connection id, and report loader-managed restartability.",json!({"site_root":{"type":"string"},"binding_id":{"type":"string"},"surface_id":{"type":"string"},"runtime_kind":{"type":"string"},"entrypoint":{"type":"string"},"args":{"type":"array","items":{"type":"string"}}}),&["site_root","binding_id"],false,false),
        tool_definition("mcp_loader_open_surface","Open an exactly admitted binding and return a stable logical handle for calls across loader-managed child generations. Runtime metadata is opt-in.",json!({"site_root":{"type":"string"},"binding_id":{"type":"string"},"surface_id":{"type":"string"},"runtime_kind":{"type":"string"},"entrypoint":{"type":"string"},"args":{"type":"array","items":{"type":"string"}},"include_runtime_metadata":{"type":"boolean","default":false}}),&["site_root","binding_id"],false,false),
        tool_definition("mcp_loader_resume_or_open_surface","Resume the current loader-process handle for a binding when present, otherwise reopen the admitted binding and return a fresh handle.",json!({"site_root":{"type":"string"},"binding_id":{"type":"string"},"surface_id":{"type":"string"},"runtime_kind":{"type":"string"},"include_runtime_metadata":{"type":"boolean","default":false}}),&["site_root","binding_id"],false,false),
        tool_definition("mcp_loader_surface_handle_inventory","List stable logical surface handles and the current child generation, without spawning or replacing a surface.",json!({}),&[],true,false),
        tool_definition("mcp_loader_list_tools","List compact tool summaries exposed by an attached MCP surface. Exact schemas are available only through per-tool inspection and schema leases.",json!({"connection_id":{"type":"string"},"include_runtime_metadata":{"type":"boolean","default":false}}),&["connection_id"],true,false),
        tool_definition("mcp_loader_inspect_tool","Issue a generation-bound schema lease and a compact exact-contract summary. The complete child contract is opt-in.",json!({"connection_id":{"type":"string"},"tool_name":{"type":"string"},"include_tool_contract":{"type":"boolean","default":false}}),&["connection_id","tool_name"],true,false),
        tool_definition("mcp_loader_inspect_binding_tool","Resume or open one admitted binding, issue a schema lease, and return a compact exact-contract summary. The complete child contract is opt-in.",json!({"site_root":{"type":"string"},"binding_id":{"type":"string"},"surface_id":{"type":"string"},"runtime_kind":{"type":"string"},"tool_name":{"type":"string"},"include_tool_contract":{"type":"boolean","default":false}}),&["site_root","binding_id","tool_name"],false,false),
        tool_definition("mcp_loader_inspect_binding_tools","Resume or open one admitted binding once and issue leases for a bounded set of exact tool contracts.",json!({"site_root":{"type":"string"},"binding_id":{"type":"string"},"surface_id":{"type":"string"},"runtime_kind":{"type":"string"},"tool_names":{"type":"array","minItems":1,"maxItems":20,"uniqueItems":true,"items":{"type":"string","minLength":1}},"include_tool_contract":{"type":"boolean","default":false}}),&["site_root","binding_id","tool_names"],false,false),
        tool_definition("mcp_loader_surface_status","Inspect the runtime status and loader-managed restartability of an attached MCP surface child process.",json!({"connection_id":{"type":"string"}}),&["connection_id"],true,false),
        tool_definition("mcp_loader_tool_discovery_manifest","Return canonical semantic tool names for an attached surface and flag generated aliases as non-authoritative. Discovery never returns bulk schemas; inspect one tool to obtain its exact leased contract.",json!({"connection_id":{"type":"string"},"include_runtime_metadata":{"type":"boolean","default":false}}),&["connection_id"],true,false),
        tool_definition("mcp_loader_call_tool","Call a child tool using either its generation lease or a previously inspected exact contract digest. An unchanged digest renews authorization without another inspection round trip.",json!({"connection_id":{"type":"string"},"tool_name":{"type":"string"},"schema_lease":{"type":"string"},"tool_contract_digest":{"type":"string"},"arguments":{"type":"object"},"include_runtime_metadata":{"type":"boolean"}}),&["connection_id","tool_name"],false,false),
        tool_definition("mcp_loader_call_surface_tool","Call through a stable logical handle using either its generation lease or a cached exact contract digest.",json!({"surface_handle":{"type":"string"},"tool_name":{"type":"string"},"schema_lease":{"type":"string"},"tool_contract_digest":{"type":"string"},"arguments":{"type":"object"},"include_runtime_metadata":{"type":"boolean"}}),&["surface_handle","tool_name"],false,false),
        tool_definition("mcp_loader_call_binding_tool","Atomically reuse one admitted binding child and call a tool using either its generation lease or a cached exact contract digest.",json!({"site_root":{"type":"string"},"binding_id":{"type":"string"},"surface_id":{"type":"string"},"runtime_kind":{"type":"string"},"tool_name":{"type":"string"},"schema_lease":{"type":"string"},"tool_contract_digest":{"type":"string"},"arguments":{"type":"object"},"include_runtime_metadata":{"type":"boolean","default":false}}),&["site_root","binding_id","tool_name"],false,false),
        tool_definition("mcp_loader_read_result","Read a compact resumable page from a materialized proxied child result. Pages are capped at 4,000 characters so ordinary calls remain transcript-safe.",json!({"connection_id":{"type":"string"},"ref":{"type":"string"},"offset":{"type":"integer","minimum":0},"limit":{"type":"integer","minimum":1,"maximum":4000},"timeout_ms":{"type":"integer","minimum":1,"maximum":15000}}),&["connection_id","ref"],true,false),
        tool_definition("mcp_loader_detach","Detach and terminate an attached MCP surface.",json!({"connection_id":{"type":"string"}}),&["connection_id"],false,true),
        tool_definition("mcp_loader_surface_restart","Replace an attached MCP surface child process with a freshly initialized connection using the same site, surface, entrypoint, and args; this does not restart the agent session.",json!({"connection_id":{"type":"string"},"reason":{"type":"string"}}),&["connection_id"],false,true),
    ];
    for tool in &mut tools {
        let name = tool
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or("mcp_loader_tool")
            .to_string();
        if let Some(schema) = tool.get_mut("inputSchema") {
            normalize_input_schema(schema, Some(&name));
            if let Some(object) = schema.as_object_mut() {
                object.insert("title".into(), json!(format!("{name}.input")));
                object.insert("additionalProperties".into(), Value::Bool(false));
                object.entry("maxProperties").or_insert(json!(64));
            }
        }
    }
    tools
}
