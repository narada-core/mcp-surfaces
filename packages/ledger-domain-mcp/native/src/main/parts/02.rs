fn server_discover_result(server: &Server) -> Value {
    modern_result(
        json!({
            "supportedVersions": [MODERN_PROTOCOL_VERSION, LEGACY_PROTOCOL_VERSION],
            "capabilities": capabilities(),
            "ttlMs": 3_600_000,
            "cacheScope": "public"
        }),
        server,
    )
}

fn modern_result(value: Value, server: &Server) -> Value {
    let mut result = value.as_object().cloned().unwrap_or_default();
    result.insert("resultType".to_string(), json!("complete"));
    let mut meta = result
        .remove("_meta")
        .and_then(|value| value.as_object().cloned())
        .unwrap_or_default();
    meta.insert(
        "io.modelcontextprotocol/serverInfo".to_string(),
        json!({ "name": server.server_name, "version": "0.1.0" }),
    );
    result.insert("_meta".to_string(), Value::Object(meta));
    Value::Object(result)
}

fn list_tools(engine: &Engine) -> Vec<Value> {
    let mut tools = engine.list_tools();
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

fn call_tool(server: &Server, params: &Map<String, Value>) -> Result<Value, Value> {
    let name = params.get("name").and_then(Value::as_str).ok_or_else(|| {
        diagnostic(
            "invalid_request",
            "tools/call requires a tool name.",
            Value::Null,
        )
    })?;
    let args = match params.get("arguments") {
        None => Map::new(),
        Some(Value::Object(arguments)) => arguments.clone(),
        Some(arguments) => {
            return Err(diagnostic(
                "invalid_request",
                "tools/call arguments must be an object",
                json!({"arguments_type":value_type(arguments)}),
            ));
        }
    };
    let tool = list_tools(&server.engine)
        .into_iter()
        .find(|tool| tool.get("name").and_then(Value::as_str) == Some(name))
        .ok_or_else(|| diagnostic("unknown_tool", &format!("unknown_tool:{name}"), Value::Null))?;
    validate_input_schema(
        tool.get("inputSchema").unwrap_or(&Value::Null),
        &Value::Object(args.clone()),
        "/arguments",
    )?;
    let result = server
        .engine
        .call_tool(name, &args, &server.site_root)
        .map_err(|error| error)?;
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

fn diagnostic(code: &str, message: &str, details: Value) -> Value {
    let mut object = Map::new();
    object.insert("code".to_string(), Value::String(code.to_string()));
    object.insert("message".to_string(), Value::String(message.to_string()));
    if !details.is_null() {
        object.insert("details".to_string(), details);
    }
    Value::Object(object)
}

fn value_type(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}
