use crate::full::*;

#[test]
fn compact_child_result_removes_duplicate_text_when_structured_data_exists() {
    let child = json!({
        "content":[{"type":"text","text":"duplicate"}],
        "structuredContent":{"schema":"example.v1","status":"ok"},
        "isError":false
    });
    let compacted = compact_child_result(&child);
    assert!(compacted.get("content").is_none());
    assert_eq!(compacted["structuredContent"]["schema"], "example.v1");
    assert_eq!(compacted["isError"], false);
    let text_only = json!({"content":[{"type":"text","text":"only"}]});
    assert_eq!(compact_child_result(&text_only), text_only);
}

#[test]
fn site_surface_text_projection_exposes_compact_bindings_and_next_calls() {
    let rendered = render_result(&json!({
        "schema":"narada.mcp_loader.site_surfaces.v1",
        "status":"ok",
        "site_root":"C:/site",
        "compact":true,
        "surface_count":1,
        "surfaces":[{
            "surface_id":"git",
            "binding_id":"site-git",
            "command":"hidden-command",
            "next_call":{"tool_name":"mcp_loader_open_surface","arguments":{"site_root":"C:/site","binding_id":"site-git"}}
        }]
    }));
    assert!(rendered.contains("binding_id: site-git"));
    assert!(rendered.contains("next_call: mcp_loader_open_surface"));
    assert!(!rendered.contains("hidden-command"));
}

#[test]
fn guidance_text_projection_exposes_actionable_next_call() {
    let rendered = render_result(&json!({
        "schema":"narada.mcp_surface.guidance.v0",
        "status":"ok",
        "purpose":"Attach admitted MCP surfaces.",
        "requested":{"workflow":"activate","tool":null},
        "compact":true,
        "next_call":{"tool_name":"mcp_loader_list_site_surfaces","arguments":{"site_root":"<site_root>"}},
        "first_use":["Resolve the explicit Site binding."],
        "boundaries":["Discovery does not create authority."]
    }));
    assert!(rendered.contains("next_call: mcp_loader_list_site_surfaces"));
    assert!(rendered.contains("Resolve the explicit Site binding."));
}

#[test]
fn schema_lease_text_projection_includes_invocation_token() {
    let rendered = render_result(&json!({
        "schema":"narada.mcp_loader.schema_lease.v1",
        "status":"issued",
        "connection_id":"connection-1",
        "surface_id":"epistemic-graph",
        "tool_name":"epistemic_graph_query",
        "generation_id":"generation-2",
        "schema_lease":"schema-lease-token"
    }));
    assert!(rendered.contains("schema_lease: schema-lease-token"));
    assert!(rendered.contains("tool_name: epistemic_graph_query"));
    assert!(rendered.contains("generation_id: generation-2"));
}

#[test]
fn child_tool_discovery_cannot_project_schemas() {
    let tool = json!({
        "name":"large_query",
        "description":"Query safely.",
        "annotations":{"readOnlyHint":true},
        "inputSchema":{"type":"object","required":["participant"],"properties":{"participant":{"type":"string"},"query":{"type":"object","properties":{"large":{"type":"string"}}}},"additionalProperties":false}
    });
    let compact = compact_tool_contract(&tool);
    assert_eq!(compact["name"], "large_query");
    assert!(compact.get("inputSchema").is_none());
    assert!(compact.get("input_schema").is_none());
}

#[test]
fn compact_tool_discovery_bounds_long_descriptions() {
    let compact = compact_tool_contract(&json!({
        "name":"large_tool",
        "description":"x".repeat(256),
        "annotations":{}
    }));
    let description = compact["description"].as_str().expect("description excerpt");
    assert!(description.chars().count() <= COMPACT_TOOL_DESCRIPTION_CHARS + 1);
    assert!(description.ends_with('…'));
}

#[test]
fn schema_lease_contract_modes_control_projection_size() {
    let input_schema = json!({
        "type":"object",
        "properties":{"value":{"type":"string"}},
        "additionalProperties":false
    });
    let tool = json!({
        "name":"echo",
        "description":"Echo a value",
        "inputSchema":input_schema
    });

    for mode in ["false", "compact", "verbose"] {
        let mut result = json!({"schema":"narada.mcp_loader.schema_lease.v1"});
        apply_contract_mode(&mut result, mode, &input_schema, &tool);
        match mode {
            "false" => {
                assert!(result.get("input_contract").is_none());
                assert!(result.get("tool_contract").is_none());
            }
            "compact" => {
                assert_eq!(result["input_contract"]["properties"], json!(["value"]));
                assert!(result.get("tool_contract").is_none());
            }
            "verbose" => {
                assert_eq!(result["tool_contract"], tool);
                assert!(result.get("input_contract").is_none());
            }
            _ => unreachable!(),
        }
    }
}

#[test]
fn schema_lease_summary_names_inputs_without_repeating_the_schema() {
    let schema = json!({
        "type":"object",
        "required":["participant"],
        "properties":{"participant":{"type":"string"},"limit":{"type":"integer"}},
        "additionalProperties":false
    });
    let summary = compact_input_contract(&schema);
    assert_eq!(summary["required"], json!(["participant"]));
    assert_eq!(summary["properties"], json!(["participant", "limit"]));
    assert_eq!(summary["additional_properties"], false);
    assert!(summary.to_string().len() < schema.to_string().len());
}

#[test]
fn result_page_text_projection_is_resumable_and_bounded_by_contract() {
    let rendered = render_result(&json!({
        "schema":"narada.mcp_loader.result_page.v1",
        "connection_id":"connection-1",
        "surface_id":"epistemic-graph",
        "result":{
            "ref":"mcp_output:o_1","offset":0,"limit":4000,"next_offset":4000,
            "full_output_char_length":12000,"output_text":"bounded excerpt"
        }
    }));
    assert!(rendered.contains("next_offset: 4000"));
    assert!(rendered.contains("output_text:\nbounded excerpt"));
    let read_tool = list_tools()
        .into_iter()
        .find(|tool| tool["name"] == "mcp_loader_read_result")
        .unwrap();
    assert_eq!(
        read_tool["inputSchema"]["properties"]["limit"]["maximum"],
        4000
    );
}

#[test]
fn tool_result_text_never_suppresses_inline_result_or_materialized_reference() {
    let inline = render_result(&json!({
        "schema":"narada.mcp_loader.tool_result.v1","connection_id":"c1","surface_id":"s1",
        "result":{"schema":"child.v1","status":"ok","value":7},
        "result_summary":{"schema":"child.v1","status":"ok"}
    }));
    assert!(inline.contains("\"value\": 7"));
    let bounded = render_result(&json!({
        "schema":"narada.mcp_loader.tool_result.v1","connection_id":"c1","surface_id":"s1",
        "details_ref":"mcp_output:o_1","details_reader":"mcp_loader_read_result",
        "result":{"schema":"narada.producer_output_page.v1","status":"ok"}
    }));
    assert!(bounded.contains("details_ref: mcp_output:o_1"));
    assert!(bounded.contains("details_reader: mcp_loader_read_result"));
}
