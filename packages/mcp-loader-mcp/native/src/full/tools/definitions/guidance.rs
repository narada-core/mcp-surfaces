use crate::full::*;

fn guidance_next_call(workflow: Option<&str>, tool: Option<&str>) -> Value {
    let requested = tool.or(workflow).unwrap_or_default().to_ascii_lowercase();
    if requested.contains("site") || requested.contains("discover") || requested.contains("attach") || requested.contains("activate") || requested.contains("git") {
        return json!({
            "tool_name":"mcp_loader_list_site_surfaces",
            "arguments":{"site_root":"<site_root>"},
            "reason":"Resolve the admitted binding before opening or calling a surface."
        });
    }
    if requested.contains("call") || requested.contains("operate") {
        return json!({
            "tool_name":"mcp_loader_call_binding_tool",
            "arguments":{"site_root":"<site_root>","binding_id":"<binding_id>","tool_name":"<child_tool>","arguments":{}},
            "reason":"Use the canonical binding path for a reconnect-safe child call."
        });
    }
    json!({
        "tool_name":"mcp_loader_policy_inspect",
        "arguments":{},
        "reason":"Inspect loader policy before selecting or attaching a surface."
    })
}

pub(crate) fn guidance_result(arguments: &JsonObject, state: &LoaderState) -> Value {
    let workflow = value_string(arguments.get("workflow"));
    let tool = value_string(arguments.get("tool"));
    let include_details = arguments
        .get("include_details")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let include_runtime = arguments
        .get("include_runtime_metadata")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let mut result = json!({
        "schema":"narada.mcp_surface.guidance.v0",
        "status":"ok",
        "surface_id":"mcp-loader",
        "guidance_tool":"mcp_loader_guidance",
        "purpose":"Policy-gated runtime attachment and proxying for MCP surfaces admitted by a Site fabric.",
        "requested":{"workflow":workflow,"tool":tool},
        "runtime_lifecycle":runtime_lifecycle(None,None),
        "runtime_freshness":runtime_freshness(state),
        "compact":!include_details,
        "next_call":guidance_next_call(workflow.as_deref(),tool.as_deref()),
        "tool_call_timeout":{
            "tool":"mcp_loader_call_tool","nested_argument":"arguments.timeout_ms",
            "policy_default_ms":DEFAULT_TOOL_CALL_TIMEOUT_MS,"request_max_ms":MAX_TOOL_TIMEOUT_MS,
            "grace_flag":"--tool-timeout-grace-ms","default_grace_ms":DEFAULT_TOOL_TIMEOUT_GRACE_MS,
            "grace_max_ms":MAX_TOOL_TIMEOUT_GRACE_MS,
            "semantics":"When nested timeout_ms is present, it is forwarded to the child and the loader waits timeout_ms plus bounded grace for the child timeout result. When absent, the loader policy default is the outer deadline and no grace is added."
        },
        "first_use":[
            "Call mcp_loader_policy_inspect before relying on loader capabilities or allowed roots.",
            "Call mcp_loader_connection_inventory before attachment when recovering from capacity errors or an earlier interrupted session.",
            "Call mcp_loader_process_ownership when reconciling child processes after an interrupted attach; it reports only this loader run's direct children and safe known-connection cleanup actions.",
            "Call mcp_loader_list_site_surfaces and mcp_loader_site_fabric_diagnostics for the explicit Site root.",
            "Use mcp_loader_call_binding_tool with canonical binding_id for reconnect-safe calls; use resume_or_open only when several calls deliberately share one child handle.",
            "Inspect surface_projection.execution before attachment. mcp-loader accepts stdio projections only; surface_factory projections belong to the PC Site surface runtime.",
            "Use mcp_loader_list_tools or mcp_loader_tool_discovery_manifest after attachment; the child tools/list response owns exact tool schemas.",
            "Call mcp_loader_runtime_observation with connection_id and carrier_kind to obtain the V2 normalized observation.",
            "For mcp_loader_call_tool, place timeout_ms inside the nested arguments object.",
            "Call mcp_loader_runtime_status when the loader process may have out-of-date source, dependency, or build-configuration evidence.",
            "Preserve structuredContent as authoritative evidence; text content is for assistant readability."
        ],
        "tool_preference":[
            {"step":"orient","guidance":"Use mcp_loader_guidance, mcp_loader_runtime_status, and mcp_loader_policy_inspect before attachment or proxy calls."},
            {"step":"recover","guidance":"For a stale or transport-closed child, inspect inventory or status, then call mcp_loader_surface_restart."},
            {"step":"reconcile_processes","guidance":"Use mcp_loader_process_ownership to distinguish loader-owned direct children from unobserved host processes."},
            {"step":"resolve_site","guidance":"Use mcp_loader_list_site_surfaces and mcp_loader_site_fabric_diagnostics against the same explicit Site root."},
            {"step":"attach","guidance":"Prefer mcp_loader_call_binding_tool for each reconnect-safe call. It resumes or reopens the admitted binding and invokes the child atomically; no process-local handle must be retained."},
            {"step":"discover","guidance":"Use compact mcp_loader_list_tools by default; request include_runtime_metadata only when lifecycle or freshness evidence is material."},
            {"step":"observe_live","guidance":"Use mcp_loader_site_tool_inventory_check to compare declared tools with fresh child tools/list responses."},
            {"step":"observe_runtime","guidance":"Call mcp_loader_runtime_observation after attachment."},
            {"step":"operate","guidance":"Call a child tool only after selecting the intended connection and honoring the child surface policy."},
            {"step":"finish","guidance":"Use mcp_loader_detach or mcp_loader_surface_restart deliberately and inspect returned evidence."}
        ],
        "examples":[
            {"intent":"First use","call":"mcp_loader_guidance({})"},
            {"intent":"Inspect a workflow","call":"mcp_loader_guidance({ workflow: \"discover\", tool: \"mcp_loader_list_tools\" })"},
            {"intent":"Recover capacity","call":"mcp_loader_connection_inventory({})"},
            {"intent":"Inspect a Site","call":"mcp_loader_list_site_surfaces({ site_root: \"<site_root>\" })"},
            {"intent":"Inspect loader freshness","call":"mcp_loader_runtime_status({})"},
            {"intent":"Observe live tools","call":"mcp_loader_site_tool_inventory_check({ site_root: \"<site_root>\" })"},
            {"intent":"Observe a generation","call":"mcp_loader_runtime_observation({ connection_id: \"<connection_id>\", carrier_kind: \"codex\" })"}
        ],
        "anti_patterns":[
            "Do not infer a Site or runtime from the current directory, process name, server name, or entrypoint path.",
            "Do not attach an undeclared surface or use an entrypoint outside the allowed policy prefixes.",
            "Do not reinterpret a surface_factory projection as stdio.",
            "Do not copy child inputSchema or outputSchema into loader guidance.",
            "Do not treat loader attachment as authorization for the child surface domain.",
            "Do not enumerate or terminate arbitrary host processes, conhost descendants, or processes lacking this loader run's ownership marker."
        ],
        "recovery":[
            "For unknown_tool, call tools/list and mcp_loader_guidance again after restart.",
            "For surface_runtime_required or surface_runtime_not_supported, inspect the declared projection and retry only with an explicit compatible runtime_kind.",
            "For surface_execution_adapter_not_supported_by_loader, route the admitted binding through the PC Site surface runtime.",
            "For child failures, inspect mcp_loader_surface_status and stderr evidence, then use mcp_loader_surface_restart when eligible.",
            "For max_connections_reached, inspect inventory and detach stale or closed connections.",
            "For stale loader runtime, use runtime_freshness.reload_action as a carrier/runtime-supervisor request."
        ],
        "feedback":{"surface_id":"mcp-loader","tool":"surface_feedback_submit","when":["guidance is missing, stale, or contradicted by live loader behavior","schema shape makes correct usage hard","errors hide the actionable refusal or recovery path"]},
        "boundaries":[
            "MCP Loader owns child attachment, initialization, tool discovery, call proxying, and detachment.",
            "MCP Loader does not own attached-surface domain policy, action admission, or child tool semantics.",
            "MCP Loader is the stdio compatibility adapter. It does not host surface factories or own authority-shared instances.",
            "The loader binds children to the requested Site root and does not let an ambient caller Site root override it.",
            "Process ownership is limited to direct children spawned by this loader run."
        ]
    });
    if !include_details {
        let object = result.as_object_mut().expect("guidance result object");
        for key in ["runtime_lifecycle", "runtime_freshness", "tool_call_timeout", "tool_preference", "examples", "anti_patterns", "recovery", "feedback"] {
            object.remove(key);
        }
        object.insert("first_use".into(), json!([
            "Inspect policy, then resolve the explicit Site binding.",
            "Prefer mcp_loader_call_binding_tool with the canonical binding_id.",
            "Inspect child tools and lease one exact contract before calling."
        ]));
        object.insert("boundaries".into(), json!([
            "Site discovery does not create authority.",
            "The loader supervises children but does not own their domain policy.",
            "Use only explicitly admitted bindings and declared execution adapters."
        ]));
    }
    if include_runtime {
        result["runtime_lifecycle"] = runtime_lifecycle(None, None);
        result["runtime_freshness"] = runtime_freshness(state);
    }
    result
}
