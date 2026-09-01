fn tool(name: &str, description: &str, input_schema: Value, read_only: bool) -> Value {
    json!({
        "name": name,
        "description": description,
        "inputSchema": input_schema,
        "annotations": { "title": name, "readOnlyHint": read_only, "destructiveHint": !read_only, "idempotentHint": true, "openWorldHint": false },
        "outputSchema": { "type": "object", "additionalProperties": true }
    })
}

fn compact_guidance_result(mut result: Value) -> Value {
    if result.get("schema").and_then(Value::as_str) != Some("narada.mcp_surface.guidance.v0") {
        return result;
    }
    if let Some(object) = result.as_object_mut() {
        for key in ["path_resolution", "workflows", "tool_inventory", "examples", "anti_patterns", "recovery", "feedback", "tool_call_timeout"] {
            object.remove(key);
        }
        for key in ["first_use", "tool_preference", "boundaries"] {
            if let Some(values) = object.get_mut(key).and_then(Value::as_array_mut) {
                values.truncate(3);
            }
        }
        object.insert("compact".to_string(), Value::Bool(true));
    }
    result
}

fn call_tool(
    surface_id: &str,
    params: &Map<String, Value>,
    options: &Options,
) -> Result<Value, Value> {
    let name = params.get("name").and_then(Value::as_str).ok_or_else(|| {
        diagnostic(
            "invalid_request",
            "tools/call requires a tool name.",
            Value::Null,
        )
    })?;
    let args = params
        .get("arguments")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    let tool = list_tools(surface_id)
        .into_iter()
        .find(|tool| tool.get("name").and_then(Value::as_str) == Some(name))
        .ok_or_else(|| diagnostic("unknown_tool", &format!("unknown_tool:{name}"), Value::Null))?;
    validate_input_schema(
        tool.get("inputSchema").unwrap_or(&Value::Null),
        &Value::Object(args.clone()),
        "/arguments",
    )?;
    let result = match (surface_id, name) {
        ("catalog-observation", "catalog_observation_guidance") => catalog_guidance(&args),
        ("catalog-observation", "catalog_observation_observe") => catalog_observation(&args),
        ("operator-routing", "operator_routing_guidance") => operator_guidance(&args),
        ("operator-routing", "operator_route_doctor") => operator_route_doctor(options),
        ("operator-routing", "operator_route_request") => operator_route_request(&args, options),
        ("site-inbox", name) => site_inbox::call_tool(name, &args, &options.site_root),
        ("calendar", name) => calendar::call_tool(name, &args, &options.site_root),
        ("surface-feedback", name) => surface_feedback::call_tool(name, &args, &options.site_root),
        ("sop", name) => sop::call_tool(name, &args, &options.site_root),
        ("delegated-task", name) => delegated_task::call_tool(
            name,
            &args,
            &options.site_root,
            &options.allowed_roots,
        ),
        ("worker-delegation", name) => {
            worker_delegation::call_tool(name, &args, &options.site_root, &options.allowed_roots)
        }
        ("artifacts", name) | ("nars-session", name) | ("quota-meter", name) => {
            local_admin::call_tool(surface_id, name, &args, &options.site_root)
        }
        ("mailbox", name) => mailbox::call_tool(name, &args, &options.site_root),
        ("speech", name) => speech_authority::call_tool(name, &args, &options.site_root),
        ("browser-control", name) => {
            browser_control_authority::call_tool(name, &args, &options.site_root)
        }
        ("cloudflare-carrier", name) => {
            cloudflare_carrier_authority::call_tool(name, &args, &options.site_root)
        }
        ("graph-mail", name)
            if graph_mail_authority::enabled() && graph_mail_authority::supports(name) =>
        {
            graph_mail_authority::call_tool(name, &args, &options.site_root)
        }
        ("scheduler", name) => scheduler::call_tool(name, &args, &options.site_root),
        ("operator-console-overlay", name) | ("graph-mail", name) => {
            host_contracts::call_tool(surface_id, name, &args, &options.site_root)
        }
        ("site-lifecycle", name) | ("site-registry", name) | ("project-state", name) => {
            simple_surfaces::call_tool(surface_id, name, &args, &options.site_root)
        }
        ("runtime-introspection", name) => {
            runtime_introspection::call_tool(name, &args, &options.site_root)
        }
        ("site-coherence", name) => site_coherence::call_tool(name, &args, &options.site_root),
        ("launcher", name) => launcher::call_tool(
            name,
            &args,
            &options.site_root,
            options.registry_path.as_deref(),
        ),
        (_, unknown) => {
            return Err(diagnostic(
                "unknown_tool",
                &format!("unknown_tool:{unknown}"),
                json!({ "tool_name": unknown }),
            ))
        }
    }?;
    let result = compact_guidance_result(result);
    let is_error = result.get("status").and_then(Value::as_str) == Some("unavailable");
    let mut response = json!({ "content": [{ "type": "text", "text": serde_json::to_string_pretty(&result).unwrap_or_else(|_| result.to_string()) }], "structuredContent": result });
    if is_error {
        response["isError"] = json!(true);
    }
    Ok(response)
}

fn validate_input_schema(schema: &Value, value: &Value, path: &str) -> Result<(), Value> {
    let validator = validator_for(schema).map_err(|error| {
        diagnostic(
            "input_schema_invalid",
            "input_schema_invalid",
            json!({"path":path,"message":error.to_string()}),
        )
    })?;
    let error = validator.iter_errors(value).next();
    match error {
        None => Ok(()),
        Some(error) => Err(diagnostic(
            "input_schema_validation_failed",
            &format!("input_schema_validation_failed:{path}"),
            json!({"path":path,"message":error.to_string()}),
        )),
    }
}

fn catalog_guidance(_args: &Map<String, Value>) -> Result<Value, Value> {
    Ok(json!({
        "schema": "narada.catalog-observation.guidance.v1",
        "status": "ok",
        "capability_status": "contract_only_until_observation_port_installed",
        "authority": "Narada management owns catalog observation and credential resolution.",
        "boundary": "This MCP surface is read-only and forwards typed observation requests only.",
        "credentials": "Credential values never cross this MCP boundary and never appear in observations.",
        "unavailable": "Without an installed Narada observation port, the surface returns a typed unavailable observation and never infers model availability.",
        "retry": "Retry only after the Site installs a provider authority port; repeated calls without one are deterministically unavailable."
    }))
}

fn catalog_observation(args: &Map<String, Value>) -> Result<Value, Value> {
    let provider_id = required_string(args, "provider_id")?;
    let observed_at = required_string(args, "observed_at")?;
    let access_mode = args
        .get("access_mode")
        .and_then(Value::as_str)
        .unwrap_or("public");
    if !matches!(access_mode, "public" | "credentialed" | "operator_attested") {
        return Err(diagnostic(
            "catalog_observation_access_mode_invalid",
            "access_mode must be public, credentialed, or operator_attested.",
            json!({"field":"access_mode","received":access_mode,"allowed":["public","credentialed","operator_attested"]}),
        ));
    }
    if OffsetDateTime::parse(&observed_at, &Rfc3339).is_err() {
        return Err(diagnostic(
            "catalog_observation_observed_at_invalid",
            "observed_at must be an explicit RFC 3339 instant.",
            json!({"field":"observed_at","received":observed_at}),
        ));
    }
    Ok(json!({
        "schema": "narada.invokable-intelligence.catalog-observation.v1",
        "id": format!("catalog-observation:unavailable-{provider_id}"),
        "observed_at": observed_at,
        "inference_provider": { "kind": "inference-provider", "id": provider_id },
        "requested_access_mode": access_mode,
        "access_mode": "unavailable",
        "authority": { "kind": "unavailable", "authority_ref": "narada-observation-port:not-injected" },
        "source": { "kind": "unavailable", "reference": "narada-observation-port:not-injected" },
        "status": "unavailable",
        "models": [],
        "diagnostics": [{ "code": "provider-authority-unavailable", "message": "No Narada catalog observation port was injected into this surface process.", "retryable": false }],
        "digest": format!("sha256:{}", "0".repeat(64))
    }))
}

fn operator_guidance(args: &Map<String, Value>) -> Result<Value, Value> {
    let workflow = args
        .get("workflow")
        .and_then(Value::as_str)
        .filter(|v| !v.trim().is_empty());
    let tool = args
        .get("tool")
        .and_then(Value::as_str)
        .filter(|v| !v.trim().is_empty());
    Ok(json!({
        "schema": "narada.mcp_surface.guidance.v0",
        "status": "ok",
        "surface_id": "operator-routing",
        "guidance_tool": "operator_routing_guidance",
        "purpose": "User Site operator transcript-to-target routing and inbox fallback packaging.",
        "requested": { "workflow": workflow, "tool": tool },
        "first_use": ["Call this guidance command when the surface is unfamiliar, when a refusal/error is unclear, or before composing a multi-step workflow.", "Inspect policy/doctor/status tools before mutation or open-world operations.", "Use bounded list/search/query tools for discovery, then show/read/detail tools before acting on a specific object.", "Preserve structuredContent as authoritative evidence; text content is for assistant readability."],
        "boundaries": ["Guidance is read-only model-facing operating advice.", "Guidance does not weaken policy, authorize mutation, or replace tool schemas.", "The owning MCP surface remains authoritative for state and enforcement."]
    }))
}

fn operator_route_doctor(options: &Options) -> Result<Value, Value> {
    Ok(json!({
        "schema": "narada.operator_routing.doctor.v1",
        "status": "ok",
        "server_name": "operator-routing-mcp",
        "site_root": options.site_root.to_string_lossy(),
        "direct_delivery_supported": false,
        "fallback_channel": "site-inbox",
        "handoff_contract": {
            "schema": "narada.mcp_handoff.v1",
            "role_admission": { "target_surface": "site-lifecycle", "tool": "site_admit_role", "authority": "explicit project Site authority root" },
            "runtime_binding": { "target_surface": "site-lifecycle", "tool": "site_bind_runtime", "authority": "owning runtime locus with observed handle" },
            "default_fallback": { "target_surface": "site-inbox", "channel": "site-inbox", "mutation": "deferred until owning surface admits it" }
        },
        "suggested_speech": { "provider": "openai_api", "model": "tts-1", "voice": "nova", "text": "Request recorded. Direct delivery to that runtime is not available from this surface. I can route it through the admitted inbox path." }
    }))
}

