fn dispatch(request: &Value) -> Value {
    let id = request.get("id").cloned().unwrap_or(Value::Null);
    let method = request.get("method").and_then(Value::as_str).unwrap_or("");
    let params = request.get("params").cloned().unwrap_or_else(|| json!({}));
    let protocol_marker = params.pointer("/_meta/io.modelcontextprotocol~1protocolVersion");
    if protocol_marker.is_some_and(|value| value.as_str() != Some(MODERN_PROTOCOL_VERSION)) {
        return error(id, "protocol_version_unsupported".into());
    }
    let modern = protocol_marker.is_some();
    if modern {
        let meta = params.get("_meta").and_then(Value::as_object);
        if meta
            .and_then(|value| value.get("io.modelcontextprotocol/clientInfo"))
            .and_then(Value::as_object)
            .is_none()
            || meta
                .and_then(|value| value.get("io.modelcontextprotocol/clientCapabilities"))
                .and_then(Value::as_object)
                .is_none()
        {
            return error(id, "modern_metadata_required".into());
        }
    }
    if method == "initialize" && !modern {
        return json!({
            "jsonrpc":"2.0",
            "id":id,
            "result":{
                "protocolVersion":LEGACY_PROTOCOL_VERSION,
                "capabilities":{"tools":{}},
                "serverInfo":{"name":"mcp-registrar","version":"0.1.0"}
            }
        });
    }
    if method == "initialize" {
        return error(id, "initialize_removed".into());
    }
    if modern && method == "server/discover" {
        return json!({"jsonrpc":"2.0","id":id,"result":{"resultType":"complete","supportedVersions":[MODERN_PROTOCOL_VERSION,LEGACY_PROTOCOL_VERSION],"capabilities":{"tools":{}},"serverInfo":{"name":"mcp-registrar","version":"0.1.0"},"_meta":{"io.modelcontextprotocol/serverInfo":{"name":"mcp-registrar","version":"0.1.0"}}}});
    }
    let mut contract: Value = match decode_contract() {
        Ok(v) => v,
        Err(e) => return error(id, format!("mcp_registrar_native_contract_invalid:{e}")),
    };
    extend_epistemic_catalog(&mut contract);
    admit_structured_command_python(&mut contract);
    align_native_surface_descriptor_schemas(&mut contract);
    if let Err(e) = validate_contract(&contract) {
        return error(id, format!("mcp_registrar_native_contract_invalid:{e}"));
    }
    if let Err(e) = rebind_native_registrar(&mut contract) {
        return error(id, format!("mcp_registrar_native_contract_invalid:{e}"));
    }
    normalize_tool_schemas(&mut contract);
    if method == "tools/call" {
        if let Err(message) = validate_tool_call(&contract, &params) {
            return error(id, message);
        }
    }
    let mut response = match method {
        "tools/list" => json!({"jsonrpc":"2.0","id":id,"result":{"tools":contract["tools"]}}),
        "tools/call" => {
            let name = request
                .pointer("/params/name")
                .and_then(Value::as_str)
                .unwrap_or("");
            if matches!(
                name,
                "registrar_guidance"
                    | "registrar_surface_list"
                    | "registrar_carrier_list"
                    | "registrar_site_list"
            ) {
                let mut guidance = if name == "registrar_guidance" {
                    contract["guidance"].clone()
                } else if name == "registrar_surface_list" {
                    let args = request
                        .pointer("/params/arguments")
                        .cloned()
                        .unwrap_or_else(|| json!({}));
                    surface_list(&contract, &args)
                } else if name == "registrar_site_list" {
                    let args = request
                        .pointer("/params/arguments")
                        .cloned()
                        .unwrap_or_else(|| json!({}));
                    site_list(&contract, &args)
                } else if name == "registrar_carrier_list" {
                    let args = request
                        .pointer("/params/arguments")
                        .cloned()
                        .unwrap_or_else(|| json!({}));
                    carrier_list(&contract, &args)
                } else {
                    contract["read_models"][name].clone()
                };
                let args = request
                    .pointer("/params/arguments")
                    .cloned()
                    .unwrap_or_else(|| json!({}));
                if name == "registrar_guidance" {
                    guidance.as_object_mut().unwrap().insert("requested".into(),json!({"workflow":args.get("workflow").and_then(Value::as_str).filter(|v|!v.trim().is_empty()).map(str::trim),"tool":args.get("tool").and_then(Value::as_str).filter(|v|!v.trim().is_empty()).map(str::trim)}));
                }
                json!({"jsonrpc":"2.0","id":id,"result":{"content":[{"type":"text","text":serde_json::to_string_pretty(&guidance).unwrap()}],"structuredContent":guidance}})
            } else if name == "registrar_surface_tool_inventory_check" {
                let args = request
                    .pointer("/params/arguments")
                    .cloned()
                    .unwrap_or_else(|| json!({}));
                let result = surface_tool_inventory(&contract, &args);
                json!({"jsonrpc":"2.0","id":id,"result":{"content":[{"type":"text","text":serde_json::to_string_pretty(&result).unwrap()}],"structuredContent":result}})
            } else if name == "registrar_site_surfaces" {
                let args = request
                    .pointer("/params/arguments")
                    .cloned()
                    .unwrap_or_else(|| json!({}));
                match site_surfaces(&contract, &args) {
                    Ok(result) => {
                        json!({"jsonrpc":"2.0","id":id,"result":{"content":[{"type":"text","text":serde_json::to_string_pretty(&result).unwrap()}],"structuredContent":result}})
                    }
                    Err(message) => error(id, message),
                }
            } else if name == "registrar_site_output_reader_closure_check" {
                let args = request
                    .pointer("/params/arguments")
                    .cloned()
                    .unwrap_or_else(|| json!({}));
                match site_output_reader_closure_check(&contract, &args) {
                    Ok(result) => {
                        json!({"jsonrpc":"2.0","id":id,"result":{"content":[{"type":"text","text":serde_json::to_string_pretty(&result).unwrap()}],"structuredContent":result}})
                    }
                    Err(message) => error(id, message),
                }
            } else if name == "registrar_surface_usage" {
                let args = request
                    .pointer("/params/arguments")
                    .cloned()
                    .unwrap_or_else(|| json!({}));
                match surface_usage(&contract, &args) {
                    Ok(result) => {
                        json!({"jsonrpc":"2.0","id":id,"result":{"content":[{"type":"text","text":serde_json::to_string_pretty(&result).unwrap()}],"structuredContent":result}})
                    }
                    Err(message) => error(id, message),
                }
            } else if name == "registrar_site_surface_registry_sync" {
                let args = request
                    .pointer("/params/arguments")
                    .cloned()
                    .unwrap_or_else(|| json!({}));
                match site_surface_registry_sync(&contract, &args) {
                    Ok(result) => {
                        json!({"jsonrpc":"2.0","id":id,"result":{"content":[{"type":"text","text":serde_json::to_string_pretty(&result).unwrap()}],"structuredContent":result}})
                    }
                    Err(message) => error(id, message),
                }
            } else if name == "registrar_site_bind" || name == "registrar_site_unbind" {
                let args = request
                    .pointer("/params/arguments")
                    .cloned()
                    .unwrap_or_else(|| json!({}));
                let result = if name == "registrar_site_bind" {
                    site_bind(&contract, &args)
                } else {
                    site_unbind(&contract, &args)
                };
                match result {
                    Ok(value) => {
                        json!({"jsonrpc":"2.0","id":id,"result":{"content":[{"type":"text","text":serde_json::to_string_pretty(&value).unwrap()}],"structuredContent":value}})
                    }
                    Err(message) => error(id, message),
                }
            } else if name == "registrar_site_mcp_fabric_validate" {
                let args = request
                    .pointer("/params/arguments")
                    .cloned()
                    .unwrap_or_else(|| json!({}));
                match site_mcp_fabric_validate(&contract, &args) {
                    Ok(value) => {
                        json!({"jsonrpc":"2.0","id":id,"result":{"content":[{"type":"text","text":serde_json::to_string_pretty(&value).unwrap()}],"structuredContent":value}})
                    }
                    Err(message) => error(id, message),
                }
            } else if name == "registrar_carrier_validate" {
                let args = request
                    .pointer("/params/arguments")
                    .cloned()
                    .unwrap_or_else(|| json!({}));
                match carrier_validate(&contract, &args) {
                    Ok(value) => {
                        json!({"jsonrpc":"2.0","id":id,"result":{"content":[{"type":"text","text":serde_json::to_string_pretty(&value).unwrap()}],"structuredContent":value}})
                    }
                    Err(message) => error(id, message),
                }
            } else if name == "registrar_carrier_diff" {
                let args = request
                    .pointer("/params/arguments")
                    .cloned()
                    .unwrap_or_else(|| json!({}));
                match carrier_diff(&contract, &args) {
                    Ok(value) => {
                        json!({"jsonrpc":"2.0","id":id,"result":{"content":[{"type":"text","text":serde_json::to_string_pretty(&value).unwrap()}],"structuredContent":value}})
                    }
                    Err(message) => error(id, message),
                }
            } else if name == "registrar_site_registry_conformance_check" {
                let args = request
                    .pointer("/params/arguments")
                    .cloned()
                    .unwrap_or_else(|| json!({}));
                match site_registry_conformance_check(&contract, &args) {
                    Ok(value) => {
                        json!({"jsonrpc":"2.0","id":id,"result":{"content":[{"type":"text","text":serde_json::to_string_pretty(&value).unwrap()}],"structuredContent":value}})
                    }
                    Err(message) => error(id, message),
                }
            } else if name == "registrar_carrier_bind" || name == "registrar_carrier_unbind" {
                let args = request
                    .pointer("/params/arguments")
                    .cloned()
                    .unwrap_or_else(|| json!({}));
                let result = if name == "registrar_carrier_bind" {
                    carrier_bind(&contract, &args)
                } else {
                    carrier_unbind(&contract, &args)
                };
                match result {
                    Ok(value) => {
                        json!({"jsonrpc":"2.0","id":id,"result":{"content":[{"type":"text","text":serde_json::to_string_pretty(&value).unwrap()}],"structuredContent":value}})
                    }
                    Err(failure) => carrier_mutation_error(id, failure),
                }
            } else if name == "registrar_sync" {
                let args = request
                    .pointer("/params/arguments")
                    .cloned()
                    .unwrap_or_else(|| json!({}));
                match registrar_sync(&contract, &args) {
                    Ok(value) => {
                        json!({"jsonrpc":"2.0","id":id,"result":{"content":[{"type":"text","text":serde_json::to_string_pretty(&value).unwrap()}],"structuredContent":value}})
                    }
                    Err(message) => sync_error(id, message),
                }
            } else {
                error(
                    id,
                    format!("mcp_registrar_native_tool_not_implemented:{name}"),
                )
            }
        }
        method => error(id, format!("unsupported_mcp_method:{method}")),
    };
    if modern {
        if let Some(result) = response.get_mut("result").and_then(Value::as_object_mut) {
            result.insert("resultType".into(), json!("complete"));
            result.insert("cacheScope".into(), json!("private"));
            result.insert("ttlMs".into(), json!(0));
        }
    }
    response
}

fn ensure_python_command_admission(args: &mut Value) {
    let Some(values) = args.as_array_mut() else {
        return;
    };
    if values
        .windows(2)
        .any(|pair| pair[0] == "--allow-command" && pair[1] == "python")
    {
        return;
    }
    values.push(json!("--allow-command"));
    values.push(json!("python"));
}

