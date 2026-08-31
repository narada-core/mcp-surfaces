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
