use crate::full::*;

#[test]
fn loader_discovery_defaults_to_compact_metadata_and_exposes_resume() {
    let tools = list_tools();
    let find = |name: &str| {
        tools
            .iter()
            .find(|tool| tool["name"] == name)
            .expect("tool must exist")
    };
    assert!(
        find("mcp_loader_resume_or_open_surface")["inputSchema"]["properties"]["binding_id"]
            .is_object()
    );
    assert_eq!(
        find("mcp_loader_call_binding_tool")["inputSchema"]["required"],
        json!(["site_root", "binding_id", "tool_name"])
    );
    assert_eq!(
        find("mcp_loader_call_tool")["inputSchema"]["required"],
        json!(["connection_id", "tool_name"])
    );
    assert_eq!(
        find("mcp_loader_call_surface_tool")["inputSchema"]["required"],
        json!(["surface_handle", "tool_name"])
    );
    for name in [
        "mcp_loader_list_site_surfaces",
        "mcp_loader_open_surface",
        "mcp_loader_list_tools",
        "mcp_loader_tool_discovery_manifest",
    ] {
        assert_eq!(
            find(name)["inputSchema"]["properties"]["include_runtime_metadata"]["default"],
            false
        );
    }
    assert!(find("mcp_loader_list_tools")
        .pointer("/inputSchema/properties/include_schemas")
        .is_none());
    assert!(find("mcp_loader_tool_discovery_manifest")
        .pointer("/inputSchema/properties/compact")
        .is_none());
}

#[test]
fn schema_lease_is_bound_to_generation_tool_and_exact_contract() {
    let lease = schema_lease_digest("secret", "connection", "generation-1", "echo", "schema-a");
    assert_eq!(
        lease,
        schema_lease_digest("secret", "connection", "generation-1", "echo", "schema-a")
    );
    assert_ne!(
        lease,
        schema_lease_digest("secret", "connection", "generation-2", "echo", "schema-a")
    );
    assert_ne!(
        lease,
        schema_lease_digest("secret", "connection", "generation-1", "other", "schema-a")
    );
    assert_ne!(
        lease,
        schema_lease_digest("secret", "connection", "generation-1", "echo", "schema-b")
    );
}
