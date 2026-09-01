fn handle_request(request: &Value, options: &Options) -> Option<Value> {
    let object = request.as_object()?;
    let method = object
        .get("method")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if method.starts_with("notifications/") {
        return None;
    }
    let id = object.get("id").cloned().unwrap_or(Value::Null);
    let params = object
        .get("params")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    let modern = is_modern_request(&params);
    let result = if modern {
        validate_modern_request(&params).and_then(|_| match method {
            "server/discover" => Ok(server_discover_result(options)),
            "tools/list" => Ok(modern_result(
                json!({
                    "tools": list_tools(&options.surface_id),
                    "ttlMs": 300_000,
                    "cacheScope": "public"
                }),
                options,
            )),
            "tools/call" => call_tool(&options.surface_id, &params, options)
                .map(|value| modern_result(value, options)),
            method
                if options.surface_id == "site-inbox"
                    && matches!(
                        method,
                        "prompts/list" | "prompts/get" | "completion/complete" | "logging/setLevel"
                    ) =>
            {
                site_inbox::auxiliary(method, &params).map(|value| modern_result(value, options))
            }
            method
                if options.surface_id == "calendar"
                    && matches!(
                        method,
                        "prompts/list" | "prompts/get" | "completion/complete" | "logging/setLevel"
                    ) =>
            {
                calendar::auxiliary(method, &params).map(|value| modern_result(value, options))
            }
            method
                if options.surface_id == "surface-feedback"
                    && matches!(
                        method,
                        "prompts/list" | "prompts/get" | "completion/complete" | "logging/setLevel"
                    ) =>
            {
                surface_feedback::auxiliary(method, &params)
                    .map(|value| modern_result(value, options))
            }
            method
                if options.surface_id == "sop"
                    && matches!(
                        method,
                        "prompts/list" | "prompts/get" | "completion/complete" | "logging/setLevel"
                    ) =>
            {
                sop::auxiliary(method, &params).map(|value| modern_result(value, options))
            }
            method
                if options.surface_id == "delegated-task"
                    && matches!(
                        method,
                        "prompts/list" | "prompts/get" | "completion/complete" | "logging/setLevel"
                    ) =>
            {
                delegated_task::auxiliary(method, &params)
                    .map(|value| modern_result(value, options))
            }
            method
                if options.surface_id == "worker-delegation"
                    && matches!(
                        method,
                        "prompts/list" | "prompts/get" | "completion/complete" | "logging/setLevel"
                    ) =>
            {
                worker_delegation::auxiliary(method, &params)
                    .map(|value| modern_result(value, options))
            }
            method
                if options.surface_id == "speech"
                    && matches!(
                        method,
                        "prompts/list" | "prompts/get" | "completion/complete" | "logging/setLevel"
                    ) =>
            {
                speech_authority::auxiliary(method, &params)
                    .map(|value| modern_result(value, options))
            }
            method
                if matches!(
                    options.surface_id.as_str(),
                    "artifacts" | "nars-session" | "quota-meter"
                ) && matches!(
                    method,
                    "prompts/list" | "prompts/get" | "completion/complete" | "logging/setLevel"
                ) =>
            {
                local_admin::auxiliary(&options.surface_id, method, &params)
                    .map(|value| modern_result(value, options))
            }
            method
                if options.surface_id == "mailbox"
                    && matches!(
                        method,
                        "prompts/list" | "prompts/get" | "completion/complete" | "logging/setLevel"
                    ) =>
            {
                mailbox::auxiliary(method, &params).map(|value| modern_result(value, options))
            }
            method
                if options.surface_id == "scheduler"
                    && matches!(
                        method,
                        "prompts/list" | "prompts/get" | "completion/complete" | "logging/setLevel"
                    ) =>
            {
                scheduler::auxiliary(method, &params).map(|value| modern_result(value, options))
            }
            method
                if options.surface_id == "browser-control"
                    && matches!(
                        method,
                        "prompts/list" | "prompts/get" | "completion/complete" | "logging/setLevel"
                    ) =>
            {
                browser_control_authority::auxiliary(method, &params)
                    .map(|value| modern_result(value, options))
            }
            method
                if options.surface_id == "cloudflare-carrier"
                    && matches!(
                        method,
                        "prompts/list" | "prompts/get" | "completion/complete" | "logging/setLevel"
                    ) =>
            {
                cloudflare_carrier_authority::auxiliary(method, &params)
                    .map(|value| modern_result(value, options))
            }
            method
                if matches!(
                    options.surface_id.as_str(),
                    "operator-console-overlay" | "graph-mail"
                ) && matches!(
                    method,
                    "prompts/list" | "prompts/get" | "completion/complete" | "logging/setLevel"
                ) =>
            {
                host_contracts::auxiliary(&options.surface_id, method, &params)
                    .map(|value| modern_result(value, options))
            }
            "initialize" => Err(diagnostic(
                "initialize_removed",
                "The 2026-07-28 protocol has no initialize handshake.",
                json!({ "protocolVersion": MODERN_PROTOCOL_VERSION }),
            )),
            _ => Err(diagnostic(
                "unsupported_mcp_method",
                &format!("unsupported_mcp_method:{method}"),
                json!({ "method": method }),
            )),
        })
    } else {
        match method {
            method
                if options.surface_id == "site-inbox"
                    && matches!(
                        method,
                        "prompts/list" | "prompts/get" | "completion/complete" | "logging/setLevel"
                    ) =>
            {
                site_inbox::auxiliary(method, &params)
            }
            method
                if options.surface_id == "calendar"
                    && matches!(
                        method,
                        "prompts/list" | "prompts/get" | "completion/complete" | "logging/setLevel"
                    ) =>
            {
                calendar::auxiliary(method, &params)
            }
            method
                if options.surface_id == "surface-feedback"
                    && matches!(
                        method,
                        "prompts/list" | "prompts/get" | "completion/complete" | "logging/setLevel"
                    ) =>
            {
                surface_feedback::auxiliary(method, &params)
            }
            method
                if options.surface_id == "sop"
                    && matches!(
                        method,
                        "prompts/list" | "prompts/get" | "completion/complete" | "logging/setLevel"
                    ) =>
            {
                sop::auxiliary(method, &params)
            }
            method
                if options.surface_id == "delegated-task"
                    && matches!(
                        method,
                        "prompts/list" | "prompts/get" | "completion/complete" | "logging/setLevel"
                    ) =>
            {
                delegated_task::auxiliary(method, &params)
            }
            method
                if options.surface_id == "worker-delegation"
                    && matches!(
                        method,
                        "prompts/list" | "prompts/get" | "completion/complete" | "logging/setLevel"
                    ) =>
            {
                worker_delegation::auxiliary(method, &params)
            }
            method
                if options.surface_id == "speech"
                    && matches!(
                        method,
                        "prompts/list" | "prompts/get" | "completion/complete" | "logging/setLevel"
                    ) =>
            {
                speech_authority::auxiliary(method, &params)
            }
            method
                if matches!(
                    options.surface_id.as_str(),
                    "artifacts" | "nars-session" | "quota-meter"
                ) && matches!(
                    method,
                    "prompts/list" | "prompts/get" | "completion/complete" | "logging/setLevel"
                ) =>
            {
                local_admin::auxiliary(&options.surface_id, method, &params)
            }
            method
                if options.surface_id == "mailbox"
                    && matches!(
                        method,
                        "prompts/list" | "prompts/get" | "completion/complete" | "logging/setLevel"
                    ) =>
            {
                mailbox::auxiliary(method, &params)
            }
            method
                if options.surface_id == "scheduler"
                    && matches!(
                        method,
                        "prompts/list" | "prompts/get" | "completion/complete" | "logging/setLevel"
                    ) =>
            {
                scheduler::auxiliary(method, &params)
            }
            method
                if options.surface_id == "browser-control"
                    && matches!(
                        method,
                        "prompts/list" | "prompts/get" | "completion/complete" | "logging/setLevel"
                    ) =>
            {
                browser_control_authority::auxiliary(method, &params)
            }
            method
                if options.surface_id == "cloudflare-carrier"
                    && matches!(
                        method,
                        "prompts/list" | "prompts/get" | "completion/complete" | "logging/setLevel"
                    ) =>
            {
                cloudflare_carrier_authority::auxiliary(method, &params)
            }
            method
                if matches!(
                    options.surface_id.as_str(),
                    "operator-console-overlay" | "graph-mail"
                ) && matches!(
                    method,
                    "prompts/list" | "prompts/get" | "completion/complete" | "logging/setLevel"
                ) =>
            {
                host_contracts::auxiliary(&options.surface_id, method, &params)
            }
            "initialize" => Ok(initialize_result(options)),
            "tools/list" => Ok(json!({ "tools": list_tools(&options.surface_id) })),
            "tools/call" => call_tool(&options.surface_id, &params, options),
            "server/discover" => Err(diagnostic(
                "modern_metadata_required",
                "server/discover requires 2026-07-28 request metadata.",
                json!({ "protocolVersion": MODERN_PROTOCOL_VERSION }),
            )),
            _ => Err(diagnostic(
                "unsupported_mcp_method",
                &format!("unsupported_mcp_method:{method}"),
                json!({ "method": method }),
            )),
        }
    };
    Some(match result {
        Ok(result) => json!({ "jsonrpc": "2.0", "id": id, "result": result }),
        Err(error) => {
            json!({ "jsonrpc": "2.0", "id": id, "error": { "code": -32000, "message": error["message"], "data": error } })
        }
    })
}

