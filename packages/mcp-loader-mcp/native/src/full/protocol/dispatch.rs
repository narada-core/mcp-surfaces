use crate::full::*;

pub(crate) fn dispatch(request: &Value, state: &mut LoaderState) -> Result<Value, Diagnostic> {
    let method = request
        .get("method")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let params = request.get("params").cloned().unwrap_or_else(|| json!({}));
    if let Some(version) = params.pointer("/_meta/io.modelcontextprotocol~1protocolVersion") {
        if version.as_str() != Some(MODERN_PROTOCOL_VERSION) {
            return Err(Diagnostic::new(
                "protocol_version_unsupported",
                format!("protocol_version_unsupported:{version}"),
            ));
        }
    }
    if is_modern_request(&params) {
        validate_modern_request(&params)?;
        return match method {
            "server/discover" => Ok(modernize_result(modern_discover_result(), method)),
            "initialize" => Err(Diagnostic::new(
                "initialize_removed",
                "The 2026-07-28 protocol has no initialize handshake.",
            )),
            _ => {
                dispatch_legacy(method, &params, state).map(|value| modernize_result(value, method))
            }
        };
    }
    dispatch_legacy(method, &params, state)
}

pub(crate) fn dispatch_legacy(
    method: &str,
    params: &Value,
    state: &mut LoaderState,
) -> Result<Value, Diagnostic> {
    match method {
        "initialize" => Ok(json!({
            "protocolVersion": PROTOCOL_VERSION,
            "capabilities": {"tools": {}},
            "serverInfo": {"name": SERVER_NAME, "version": SERVER_VERSION}
        })),
        "notifications/initialized" | "ping" => Ok(json!({})),
        "tools/list" => Ok(json!({"tools": list_tools()})),
        "tools/call" => {
            let object = params
                .as_object()
                .cloned()
                .ok_or_else(|| Diagnostic::new("invalid_tool_call", "invalid_tool_call"))?;
            for key in object.keys() {
                if key != "name" && key != "arguments" && key != "_meta" {
                    return Err(Diagnostic::new(
                        "invalid_tool_call_parameter",
                        format!("invalid_tool_call_parameter:{key}"),
                    ));
                }
            }
            let name = required_string(&object, "name", "missing_tool_name")?;
            let args = object
                .get("arguments")
                .cloned()
                .unwrap_or_else(|| json!({}));
            let tool = list_tools()
                .into_iter()
                .find(|tool| tool.get("name").and_then(Value::as_str) == Some(name.as_str()))
                .ok_or_else(|| Diagnostic::new("unknown_tool", format!("unknown_tool:{name}")))?;
            validate_input_schema(
                tool.get("inputSchema").unwrap_or(&Value::Null),
                &args,
                "arguments",
            )?;
            let result = call_tool(&name, args, state)?;
            Ok(call_tool_result(result))
        }
        _ => Err(Diagnostic::new(
            "unsupported_mcp_method",
            format!("unsupported_mcp_method:{}", method),
        )),
    }
}
pub(crate) fn call_tool_result(result: Value) -> Value {
    let text = render_result(&result);
    json!({"content":[{"type":"text","text":text,"annotations":{"audience":["assistant"]}}],"structuredContent":result})
}

pub(crate) fn required_string(
    object: &JsonObject,
    key: &str,
    code: &str,
) -> Result<String, Diagnostic> {
    object
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .ok_or_else(|| Diagnostic::new(code, code))
}

pub(crate) fn optional_str(value: impl AsRef<str>) -> Option<String> {
    let text = value.as_ref().trim();
    if text.is_empty() {
        None
    } else {
        Some(text.to_string())
    }
}

pub(crate) fn value_string(value: Option<&Value>) -> Option<String> {
    value.and_then(Value::as_str).and_then(optional_str)
}

pub(crate) fn string_array(value: Option<&Value>) -> Result<Option<Vec<String>>, Diagnostic> {
    let Some(value) = value else {
        return Ok(None);
    };
    let array = value
        .as_array()
        .ok_or_else(|| Diagnostic::new("invalid_string_array", "invalid_string_array"))?;
    Ok(Some(
        array
            .iter()
            .filter_map(|item| item.as_str().map(ToOwned::to_owned))
            .collect(),
    ))
}
