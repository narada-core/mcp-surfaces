fn is_modern_request(params: &Map<String, Value>) -> bool {
    params
        .get("_meta")
        .and_then(Value::as_object)
        .and_then(|meta| meta.get("io.modelcontextprotocol/protocolVersion"))
        .and_then(Value::as_str)
        == Some(MODERN_PROTOCOL_VERSION)
}

fn validate_modern_request(params: &Map<String, Value>) -> Result<(), Value> {
    let meta = params
        .get("_meta")
        .and_then(Value::as_object)
        .ok_or_else(|| {
            diagnostic(
                "modern_metadata_required",
                "Modern MCP requests require _meta.",
                Value::Null,
            )
        })?;
    if meta
        .get("io.modelcontextprotocol/clientInfo")
        .and_then(Value::as_object)
        .is_none()
    {
        return Err(diagnostic(
            "modern_metadata_required",
            "Modern MCP requests require clientInfo metadata.",
            json!({ "key": "io.modelcontextprotocol/clientInfo" }),
        ));
    }
    if meta
        .get("io.modelcontextprotocol/clientCapabilities")
        .and_then(Value::as_object)
        .is_none()
    {
        return Err(diagnostic(
            "modern_metadata_required",
            "Modern MCP requests require clientCapabilities metadata.",
            json!({ "key": "io.modelcontextprotocol/clientCapabilities" }),
        ));
    }
    Ok(())
}

fn server_name(options: &Options) -> String {
    match options.surface_id.as_str() {
        "site-inbox" => "narada-site-inbox-mcp".to_string(),
        "calendar" => "narada-calendar-mcp".to_string(),
        "surface-feedback" => "surface-feedback-mcp".to_string(),
        "sop" => "sop-mcp".to_string(),
        "delegated-task" => "delegated-task-mcp".to_string(),
        "worker-delegation" => "worker-delegation-mcp".to_string(),
        "artifacts" => "artifacts-mcp".to_string(),
        "nars-session" => "nars-session-mcp".to_string(),
        "quota-meter" => "quota-meter-mcp".to_string(),
        "mailbox" => "mailbox-mcp".to_string(),
        "browser-control" => "browser-control-mcp".to_string(),
        "operator-console-overlay" => "operator-console-overlay-mcp".to_string(),
        "cloudflare-carrier" => "cloudflare-carrier-mcp".to_string(),
        "speech" => "speech-mcp".to_string(),
        "scheduler" => "scheduler-mcp".to_string(),
        "graph-mail" => "graph-mail-mcp".to_string(),
        "site-lifecycle" => "site-lifecycle-mcp".to_string(),
        "site-registry" => "site-registry-mcp".to_string(),
        "project-state" => "project-state-mcp".to_string(),
        "runtime-introspection" => "runtime-introspection-mcp".to_string(),
        "site-coherence" => "site-coherence-mcp".to_string(),
        "launcher" => "launcher-mcp".to_string(),
        _ => format!("{}-mcp", options.surface_id),
    }
}

fn capabilities(surface_id: &str) -> Value {
    if matches!(
        surface_id,
        "site-inbox"
            | "calendar"
            | "surface-feedback"
            | "sop"
            | "delegated-task"
            | "worker-delegation"
            | "artifacts"
            | "nars-session"
            | "quota-meter"
            | "mailbox"
            | "browser-control"
            | "operator-console-overlay"
            | "cloudflare-carrier"
            | "speech"
            | "scheduler"
            | "graph-mail"
    ) {
        json!({"tools":{},"prompts":{},"completions":{},"logging":{}})
    } else {
        json!({"tools":{}})
    }
}

fn initialize_result(options: &Options) -> Value {
    json!({
        "protocolVersion": LEGACY_PROTOCOL_VERSION,
        "capabilities": capabilities(&options.surface_id),
        "serverInfo": { "name": server_name(options), "version": "0.1.0" }
    })
}

fn server_discover_result(options: &Options) -> Value {
    modern_result(
        json!({
            "supportedVersions": [MODERN_PROTOCOL_VERSION, LEGACY_PROTOCOL_VERSION],
            "capabilities": capabilities(&options.surface_id),
            "ttlMs": 3_600_000,
            "cacheScope": "public"
        }),
        options,
    )
}

fn modern_result(value: Value, options: &Options) -> Value {
    let mut result = value.as_object().cloned().unwrap_or_default();
    result.insert("resultType".to_string(), json!("complete"));
    let mut meta = result
        .remove("_meta")
        .and_then(|value| value.as_object().cloned())
        .unwrap_or_default();
    meta.insert(
        "io.modelcontextprotocol/serverInfo".to_string(),
        json!({ "name": server_name(options), "version": "0.1.0" }),
    );
    result.insert("_meta".to_string(), Value::Object(meta));
    Value::Object(result)
}
fn list_tools(surface_id: &str) -> Vec<Value> {
    let mut tools = raw_list_tools(surface_id);
    for tool in &mut tools {
        let name = tool
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or("mcp_tool")
            .to_string();
        if let Some(schema) = tool.get_mut("inputSchema") {
            normalize_input_schema(schema, Some(&name));
            if let Some(object) = schema.as_object_mut() {
                object.insert("title".to_string(), json!(format!("{name}.input")));
                object.insert("additionalProperties".to_string(), Value::Bool(false));
            }
        }
    }
    tools
}

fn raw_list_tools(surface_id: &str) -> Vec<Value> {
    match surface_id {
        "site-inbox" => site_inbox::list_tools(),
        "calendar" => calendar::list_tools(),
        "surface-feedback" => surface_feedback::list_tools(),
        "sop" => sop::list_tools(),
        "delegated-task" => delegated_task::list_tools(),
        "worker-delegation" => worker_delegation::list_tools(),
        "artifacts" | "nars-session" | "quota-meter" => local_admin::list_tools(surface_id),
        "mailbox" => mailbox::list_tools(),
        "scheduler" => scheduler::list_tools(),
        "speech" => speech_authority::list_tools(),
        "browser-control" => browser_control_authority::list_tools(),
        "cloudflare-carrier" => cloudflare_carrier_authority::list_tools(),
        "operator-console-overlay" | "graph-mail" => host_contracts::list_tools(surface_id),
        "catalog-observation" => vec![
            guidance_tool("catalog-observation"),
            tool("catalog_observation_observe", "Observe a provider model catalog through an installed Narada-owned observation port. Without that port, return a typed unavailable result rather than inferred catalog data.", json!({
                "type": "object",
                "properties": {
                    "provider_id": { "type": "string", "minLength": 1, "maxLength": 512, "description": "Canonical inference-provider resource id." },
                    "observed_at": { "type": "string", "format": "date-time", "description": "Explicit RFC 3339 observation instant." },
                    "access_mode": { "type": "string", "enum": ["public", "credentialed", "operator_attested"], "default": "public" }
                },
                "required": ["provider_id", "observed_at"],
                "additionalProperties": false
            }), true),
        ],
        "operator-routing" => vec![
            guidance_tool("operator-routing"),
            tool("operator_route_doctor", "Report operator routing posture, fallback policy, and the suggested spoken acknowledgement shape.", json!({ "type": "object", "properties": {}, "additionalProperties": false }), true),
            tool("operator_route_request", "Compile a transcript into a routing decision and a site-inbox-compatible fallback envelope.", json!({
                "type": "object",
                "properties": {
                    "transcript": { "type": "string", "minLength": 1, "maxLength": 65536, "description": "Transcript text to route." },
                    "target_runtime": { "type": "string", "minLength": 1, "maxLength": 256, "description": "Target runtime or runtime family to receive the command." },
                    "target_identity": { "type": "string", "minLength": 1, "maxLength": 512, "description": "Optional target agent identity." },
                    "intent_kind": { "type": "string", "minLength": 1, "maxLength": 256, "description": "Optional intent classification." },
                    "speaker_agent_id": { "type": "string", "minLength": 1, "maxLength": 512, "description": "Optional speaker identity to preserve in the route record." },
                    "target_site_id": { "type": "string", "minLength": 1, "maxLength": 512, "description": "Canonical target Site id for a project-scoped handoff." },
                    "target_site_root": { "type": "string", "minLength": 1, "maxLength": 4096, "description": "Explicit target Site workspace root for a project-scoped handoff." },
                    "operation_kind": { "type": "string", "enum": ["role_admission", "runtime_binding"], "description": "Typed handoff operation; inferred from intent_kind when omitted." },
                    "role": { "type": "string", "minLength": 1, "maxLength": 256 },
                    "agent_kind": { "type": "string", "minLength": 1, "maxLength": 256 },
                    "principal": { "type": "string", "minLength": 1, "maxLength": 512 },
                    "runtime_locus": { "type": "string", "minLength": 1, "maxLength": 4096 },
                    "runtime_handle": { "type": "string", "minLength": 1, "maxLength": 4096 },
                    "allow_inbox_fallback": { "type": "boolean", "default": true, "description": "Allow a site-inbox fallback envelope when direct delivery is unavailable." },
                    "request_id": { "type": "string", "minLength": 1, "maxLength": 512, "description": "Optional stable request identifier." }
                },
                "required": ["transcript", "target_runtime"],
                "additionalProperties": false
            }), false),
        ],
        "site-lifecycle" | "site-registry" | "project-state" => simple_surfaces::list_tools(surface_id),
        "runtime-introspection" => runtime_introspection::list_tools(),
        "site-coherence" => site_coherence::list_tools(),
        "launcher" => launcher::list_tools(),
        _ => Vec::new(),
    }
}

fn normalize_input_schema(schema: &mut Value, field: Option<&str>) {
    let Some(object) = schema.as_object_mut() else {
        return;
    };
    match object.get("type").and_then(Value::as_str) {
        Some("string") if !object.contains_key("maxLength") && !object.contains_key("enum") => {
            let name = field.unwrap_or_default().to_ascii_lowercase();
            let maximum = if name.contains("path") || name.contains("root") || name.contains("file")
            {
                4096
            } else if name.contains("summary")
                || name.contains("body")
                || name.contains("context")
                || name.contains("output")
            {
                32768
            } else {
                8192
            };
            object.insert("maxLength".to_string(), json!(maximum));
        }
        Some("array") if !object.contains_key("maxItems") => {
            object.insert("maxItems".to_string(), json!(500));
        }
        Some("object") if !object.contains_key("maxProperties") => {
            object.insert("maxProperties".to_string(), json!(256));
        }
        _ => {}
    }
    if let Some(properties) = object.get_mut("properties").and_then(Value::as_object_mut) {
        for (name, child) in properties {
            normalize_input_schema(child, Some(name));
        }
    }
    if let Some(items) = object.get_mut("items") {
        normalize_input_schema(items, field);
    }
    for keyword in ["allOf", "anyOf", "oneOf"] {
        if let Some(branches) = object.get_mut(keyword).and_then(Value::as_array_mut) {
            for branch in branches {
                normalize_input_schema(branch, field);
            }
        }
    }
}

fn guidance_tool(surface_id: &str) -> Value {
    let tool_name = surface_id.replace("-", "_") + "_guidance";
    tool(
        &tool_name,
        &format!("Show model-facing operating guidance for {surface_id} MCP workflows."),
        json!({
            "type": "object",
            "properties": {
                "workflow": { "type": "string", "description": "Optional workflow name or area to focus guidance on." },
                "tool": { "type": "string", "description": "Optional tool name for tool-specific guidance." }
            },
            "additionalProperties": false
        }),
        true,
    )
}

