fn native_artifact_entrypoint(package: &str, artifact: &str) -> Option<String> {
    let executable = env::current_exe().ok()?;
    let workspace = executable.ancestors().find(|root| {
        root.join("packages")
            .join("shared")
            .join("mcp-runtime-proxy")
            .exists()
    })?;
    let native_root = package
        .split('/')
        .fold(workspace.join("packages"), |root, part| root.join(part))
        .join("dist")
        .join("native");
    let pointer: Value =
        serde_json::from_str(&fs::read_to_string(native_root.join("current.json")).ok()?).ok()?;
    let requested = artifact.strip_suffix(".exe").unwrap_or(artifact);
    let names = if cfg!(windows) {
        vec![artifact.to_string(), format!("{requested}.exe")]
    } else {
        vec![requested.to_string(), artifact.to_string()]
    };
    let relative = names
        .iter()
        .find_map(|name| pointer.get("artifacts")?.get(name)?.as_str())?;
    Some(
        native_root
            .join(relative)
            .to_string_lossy()
            .replace('\\', "/"),
    )
}

fn surface_tool_inventory(contract: &Value, args: &Value) -> Value {
    let observed = args.get("observed_tools").and_then(Value::as_object);
    let include_ok = args.get("include_ok") == Some(&Value::Bool(true));
    let items = contract
        .pointer("/read_models/registrar_surface_list/items")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut findings = vec![];
    let mut checked = 0;
    for surface in &items {
        let id = surface["id"].as_str().unwrap_or("");
        let Some(input) = observed.and_then(|value| value.get(id)) else {
            continue;
        };
        checked += 1;
        let registered = unique(
            surface["tools"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str),
        );
        let actual = unique(
            input
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str),
        );
        let missing = actual
            .iter()
            .filter(|value| !registered.contains(value))
            .cloned()
            .collect::<Vec<_>>();
        let extra = registered
            .iter()
            .filter(|value| !actual.contains(value))
            .cloned()
            .collect::<Vec<_>>();
        let status = if missing.is_empty() && extra.is_empty() {
            "ok"
        } else {
            "drift"
        };
        if status != "ok" || include_ok {
            findings.push(json!({"surface_id":id,"package":surface["package"],"status":status,"registered_count":registered.len(),"observed_count":actual.len(),"missing_from_registrar":missing,"extra_in_registrar":extra}));
        }
    }
    let without = items
        .iter()
        .filter_map(|value| value["id"].as_str())
        .filter(|id| observed.is_none_or(|value| !value.contains_key(*id)))
        .collect::<Vec<_>>();
    json!({"schema":"narada.registrar.surface_tool_inventory_check.v1","status":if findings.iter().any(|value|value["status"]=="drift"){"drift"}else{"ok"},"checked_count":checked,"surfaces_without_observations":without,"findings":findings})
}
fn unique<'a>(values: impl Iterator<Item = &'a str>) -> Vec<String> {
    let mut result = vec![];
    for value in values {
        if !result.iter().any(|existing| existing == value) {
            result.push(value.to_string());
        }
    }
    result
}

fn normalize_tool_schemas(contract: &mut Value) {
    let Some(tools) = contract.get_mut("tools").and_then(Value::as_array_mut) else {
        return;
    };
    for tool in tools {
        let name = tool
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or("registrar_tool")
            .to_string();
        let Some(schema) = tool.get_mut("inputSchema") else {
            continue;
        };
        normalize_schema(schema, Some(&name));
        if let Some(object) = schema.as_object_mut() {
            object.insert("title".into(), json!(format!("{name}.input")));
            object.insert("additionalProperties".into(), json!(false));
            object.entry("maxProperties").or_insert(json!(64));
        }
    }
}

fn normalize_schema(schema: &mut Value, field: Option<&str>) {
    let Some(object) = schema.as_object_mut() else {
        return;
    };
    match object.get("type").and_then(Value::as_str) {
        Some("string") if !object.contains_key("maxLength") && !object.contains_key("enum") => {
            let field = field.unwrap_or_default().to_ascii_lowercase();
            let maximum = if field.contains("path") || field.contains("root") {
                4096
            } else {
                8192
            };
            object.insert("maxLength".into(), json!(maximum));
        }
        Some("array") if !object.contains_key("maxItems") => {
            object.insert("maxItems".into(), json!(256));
        }
        Some("object") if !object.contains_key("maxProperties") => {
            object.insert("maxProperties".into(), json!(256));
        }
        _ => {}
    }
    if let Some(properties) = object.get_mut("properties").and_then(Value::as_object_mut) {
        for (name, child) in properties {
            normalize_schema(child, Some(name));
        }
    }
    if let Some(items) = object.get_mut("items") {
        normalize_schema(items, field);
    }
}

fn validate_tool_call(contract: &Value, params: &Value) -> Result<(), String> {
    let object = params
        .as_object()
        .ok_or("invalid_tool_call_params:expected_object")?;
    for key in object.keys() {
        if !matches!(key.as_str(), "name" | "arguments" | "_meta") {
            return Err(format!("invalid_tool_call_params:unknown_field:{key}"));
        }
    }
    let name = object
        .get("name")
        .and_then(Value::as_str)
        .ok_or("invalid_tool_call_params:name_required")?;
    let tool = contract["tools"]
        .as_array()
        .and_then(|tools| tools.iter().find(|tool| tool["name"] == name))
        .ok_or_else(|| format!("unknown_tool:{name}"))?;
    let arguments = object
        .get("arguments")
        .cloned()
        .unwrap_or_else(|| json!({}));
    validate_schema(&tool["inputSchema"], &arguments, "$args")
}

fn validate_schema(schema: &Value, value: &Value, path: &str) -> Result<(), String> {
    if let Some(expected) = schema.get("type").and_then(Value::as_str) {
        let matches = match expected {
            "object" => value.is_object(),
            "array" => value.is_array(),
            "string" => value.is_string(),
            "integer" => value.as_i64().is_some() || value.as_u64().is_some(),
            "number" => value.is_number(),
            "boolean" => value.is_boolean(),
            _ => true,
        };
        if !matches {
            return Err(format!("invalid_tool_arguments:{path}:expected_{expected}"));
        }
    }
    if let Some(text) = value.as_str() {
        if schema
            .get("maxLength")
            .and_then(Value::as_u64)
            .is_some_and(|max| text.len() > max as usize)
        {
            return Err(format!("invalid_tool_arguments:{path}:maxLength"));
        }
        if schema
            .get("enum")
            .and_then(Value::as_array)
            .is_some_and(|values| !values.iter().any(|candidate| candidate == value))
        {
            return Err(format!("invalid_tool_arguments:{path}:enum"));
        }
    }
    if let Some(array) = value.as_array() {
        if schema
            .get("maxItems")
            .and_then(Value::as_u64)
            .is_some_and(|max| array.len() > max as usize)
        {
            return Err(format!("invalid_tool_arguments:{path}:maxItems"));
        }
        if let Some(items) = schema.get("items") {
            for (index, item) in array.iter().enumerate() {
                validate_schema(items, item, &format!("{path}[{index}]"))?;
            }
        }
    }
    if let Some(object) = value.as_object() {
        if schema
            .get("maxProperties")
            .and_then(Value::as_u64)
            .is_some_and(|max| object.len() > max as usize)
        {
            return Err(format!("invalid_tool_arguments:{path}:maxProperties"));
        }
        let properties = schema.get("properties").and_then(Value::as_object);
        if schema.get("additionalProperties") == Some(&json!(false)) {
            for key in object.keys() {
                if !properties.is_some_and(|known| known.contains_key(key)) {
                    return Err(format!("invalid_tool_arguments:{path}:unknown_field:{key}"));
                }
            }
        }
        for required in schema
            .get("required")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
        {
            if !object.contains_key(required) {
                return Err(format!("invalid_tool_arguments:{path}:required:{required}"));
            }
        }
        if let Some(properties) = properties {
            for (key, child) in object {
                if let Some(child_schema) = properties.get(key) {
                    validate_schema(child_schema, child, &format!("{path}.{key}"))?;
                }
            }
        }
    }
    Ok(())
}

fn error(id: Value, message: String) -> Value {
    json!({"jsonrpc":"2.0","id":id,"error":{"code":-32000,"message":message}})
}

fn read_line_bounded<R: BufRead>(input: &mut R, maximum: usize) -> Result<Option<String>, String> {
    let mut bytes = Vec::new();
    let count = input
        .take((maximum + 1) as u64)
        .read_until(b'\n', &mut bytes)
        .map_err(|error| error.to_string())?;
    if count == 0 {
        return Ok(None);
    }
    if bytes.len() > maximum || !bytes.ends_with(b"\n") {
        return Err("mcp_line_exceeds_byte_limit".into());
    }
    String::from_utf8(bytes)
        .map(Some)
        .map_err(|_| "mcp_line_invalid_utf8".into())
}

