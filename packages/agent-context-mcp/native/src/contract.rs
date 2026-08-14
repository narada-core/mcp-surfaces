use serde_json::{json, Map, Value};

const CONTRACT: &str = include_str!("../tool-catalog.json");

pub fn tools(projection: &str) -> Result<Vec<Value>, String> {
    let catalog: Value = serde_json::from_str(CONTRACT)
        .map_err(|error| format!("agent_context_native_catalog_invalid:{error}"))?;
    let mut tools = catalog
        .pointer(&format!("/projections/{projection}"))
        .and_then(Value::as_array)
        .cloned()
        .ok_or_else(|| format!("agent_context_native_projection_invalid:{projection}"))?;
    for tool in &mut tools {
        normalize_tool(projection, tool)?;
    }
    Ok(tools)
}

pub fn guidance() -> Result<Value, String> {
    serde_json::from_str::<Value>(CONTRACT)
        .map_err(|error| format!("agent_context_native_catalog_invalid:{error}"))?
        .get("guidance")
        .cloned()
        .ok_or_else(|| "agent_context_native_guidance_missing".to_string())
}

pub fn validate_call(tools: &[Value], params: &Value) -> Result<(String, Value), String> {
    let object = params
        .as_object()
        .ok_or("agent_context_tools_call_params_must_be_object")?;
    for key in object.keys() {
        if key != "name" && key != "arguments" && key != "_meta" {
            return Err(format!("agent_context_tools_call_parameter_unknown:{key}"));
        }
    }
    let name = object
        .get("name")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or("agent_context_tool_name_required")?
        .to_string();
    let arguments = object
        .get("arguments")
        .cloned()
        .unwrap_or_else(|| json!({}));
    let tool = tools
        .iter()
        .find(|tool| tool.get("name").and_then(Value::as_str) == Some(&name))
        .ok_or_else(|| format!("agent_context_native_tool_not_exposed:{name}"))?;
    validate_schema(
        tool.get("inputSchema").unwrap_or(&Value::Null),
        &arguments,
        "arguments",
    )?;
    Ok((name, arguments))
}

fn normalize_tool(projection: &str, tool: &mut Value) -> Result<(), String> {
    let name = tool
        .get("name")
        .and_then(Value::as_str)
        .ok_or("agent_context_native_catalog_tool_name_missing")?
        .to_string();
    let schema = tool
        .get_mut("inputSchema")
        .ok_or_else(|| format!("agent_context_native_catalog_schema_missing:{name}"))?;
    normalize_schema(schema, Some(&name));
    let object = schema
        .as_object_mut()
        .ok_or_else(|| format!("agent_context_native_catalog_schema_not_object:{name}"))?;
    object.insert("title".into(), json!(format!("{name}.input")));
    object.insert("additionalProperties".into(), Value::Bool(false));
    if name == "agent_context_list_sessions" {
        let properties = object
            .entry("properties")
            .or_insert_with(|| json!({}))
            .as_object_mut()
            .ok_or("agent_context_list_sessions_properties_invalid")?;
        properties.insert(
            "substrate".into(),
            json!({"type":"string","minLength":1,"maxLength":256}),
        );
        properties.insert(
            "date_from".into(),
            json!({"type":"string","minLength":1,"maxLength":64,"format":"date-time"}),
        );
        properties.insert(
            "date_to".into(),
            json!({"type":"string","minLength":1,"maxLength":64,"format":"date-time"}),
        );
        properties.insert(
            "offset".into(),
            json!({"type":"integer","minimum":0,"maximum":10000,"default":0}),
        );
    }
    if name == "mcp_output_show" {
        object
            .get_mut("properties")
            .and_then(Value::as_object_mut)
            .ok_or("agent_context_output_show_properties_invalid")?
            .remove("output_ref");
    }
    if projection == "admin"
        && matches!(
            name.as_str(),
            "agent_orientation_read"
                | "agent_orientation_acknowledge"
                | "agent_context_startup_sequence"
        )
    {
        let properties = object
            .entry("properties")
            .or_insert_with(|| json!({}))
            .as_object_mut()
            .ok_or("agent_context_orientation_properties_invalid")?;
        properties.insert(
            "manifest_id".into(),
            json!({"type":"string","minLength":1,"maxLength":512}),
        );
        properties.insert(
            "admission_receipt".into(),
            json!({"type":"object","maxProperties":128}),
        );
        properties.insert(
            "delivery_receipt".into(),
            json!({"type":"object","maxProperties":128}),
        );
        if name == "agent_context_startup_sequence" {
            properties.remove("activation_receipt");
            properties.remove("output");
        }
    }
    Ok(())
}

fn normalize_schema(schema: &mut Value, field: Option<&str>) {
    let Some(object) = schema.as_object_mut() else {
        return;
    };
    match object.get("type").and_then(Value::as_str) {
        Some("string") if !object.contains_key("maxLength") && !object.contains_key("enum") => {
            let field = field.unwrap_or_default().to_ascii_lowercase();
            let maximum =
                if field.contains("path") || field.contains("root") || field.contains("file") {
                    4096
                } else if field.contains("context")
                    || field.contains("summary")
                    || field.contains("notes")
                    || field.contains("output")
                {
                    65_536
                } else {
                    8192
                };
            object.insert("maxLength".into(), json!(maximum));
        }
        Some("array") if !object.contains_key("maxItems") => {
            object.insert("maxItems".into(), json!(128));
        }
        Some("object") if !object.contains_key("maxProperties") => {
            object.insert("maxProperties".into(), json!(128));
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
    for keyword in ["anyOf", "oneOf", "allOf"] {
        if let Some(branches) = object.get_mut(keyword).and_then(Value::as_array_mut) {
            for branch in branches {
                normalize_schema(branch, field);
            }
        }
    }
}

fn validate_schema(schema: &Value, value: &Value, path: &str) -> Result<(), String> {
    if let Some(branches) = schema.get("anyOf").and_then(Value::as_array) {
        if branches
            .iter()
            .any(|branch| validate_schema(branch, value, path).is_ok())
        {
            return Ok(());
        }
        return Err(format!(
            "agent_context_input_invalid:{path}:no_anyOf_branch_matched"
        ));
    }
    if let Some(expected) = schema.get("const") {
        if value != expected {
            return Err(format!("agent_context_input_invalid:{path}:const_mismatch"));
        }
    }
    if let Some(allowed) = schema.get("enum").and_then(Value::as_array) {
        if !allowed.contains(value) {
            return Err(format!("agent_context_input_invalid:{path}:enum_mismatch"));
        }
    }
    let types = schema_types(schema);
    if !types.is_empty() && !types.iter().any(|kind| type_matches(kind, value)) {
        return Err(format!("agent_context_input_invalid:{path}:type_mismatch"));
    }
    match value {
        Value::Object(object) => validate_object(schema, object, path)?,
        Value::Array(items) => {
            validate_length(schema, items.len(), "minItems", "maxItems", path)?;
            if let Some(item_schema) = schema.get("items") {
                for (index, item) in items.iter().enumerate() {
                    validate_schema(item_schema, item, &format!("{path}[{index}]"))?;
                }
            }
        }
        Value::String(text) => {
            validate_length(schema, text.chars().count(), "minLength", "maxLength", path)?;
        }
        Value::Number(number) => {
            if let Some(value) = number.as_f64() {
                if schema
                    .get("minimum")
                    .and_then(Value::as_f64)
                    .is_some_and(|minimum| value < minimum)
                {
                    return Err(format!("agent_context_input_invalid:{path}:below_minimum"));
                }
                if schema
                    .get("maximum")
                    .and_then(Value::as_f64)
                    .is_some_and(|maximum| value > maximum)
                {
                    return Err(format!("agent_context_input_invalid:{path}:above_maximum"));
                }
            }
        }
        _ => {}
    }
    Ok(())
}

fn validate_object(schema: &Value, object: &Map<String, Value>, path: &str) -> Result<(), String> {
    validate_length(schema, object.len(), "minProperties", "maxProperties", path)?;
    let properties = schema.get("properties").and_then(Value::as_object);
    if schema.get("additionalProperties") == Some(&Value::Bool(false)) {
        for key in object.keys() {
            if !properties.is_some_and(|properties| properties.contains_key(key)) {
                return Err(format!(
                    "agent_context_input_invalid:{path}.{key}:unknown_field"
                ));
            }
        }
    }
    if let Some(required) = schema.get("required").and_then(Value::as_array) {
        for name in required.iter().filter_map(Value::as_str) {
            if !object.contains_key(name) {
                return Err(format!(
                    "agent_context_input_invalid:{path}.{name}:required"
                ));
            }
        }
    }
    if let Some(properties) = properties {
        for (name, value) in object {
            if let Some(child) = properties.get(name) {
                validate_schema(child, value, &format!("{path}.{name}"))?;
            }
        }
    }
    Ok(())
}

fn validate_length(
    schema: &Value,
    length: usize,
    minimum: &str,
    maximum: &str,
    path: &str,
) -> Result<(), String> {
    if schema
        .get(minimum)
        .and_then(Value::as_u64)
        .is_some_and(|bound| length < bound as usize)
    {
        return Err(format!("agent_context_input_invalid:{path}:{minimum}"));
    }
    if schema
        .get(maximum)
        .and_then(Value::as_u64)
        .is_some_and(|bound| length > bound as usize)
    {
        return Err(format!("agent_context_input_invalid:{path}:{maximum}"));
    }
    Ok(())
}

fn schema_types(schema: &Value) -> Vec<&str> {
    match schema.get("type") {
        Some(Value::String(kind)) => vec![kind.as_str()],
        Some(Value::Array(kinds)) => kinds.iter().filter_map(Value::as_str).collect(),
        _ => Vec::new(),
    }
}

fn type_matches(kind: &str, value: &Value) -> bool {
    match kind {
        "object" => value.is_object(),
        "array" => value.is_array(),
        "string" => value.is_string(),
        "integer" => value.as_i64().is_some() || value.as_u64().is_some(),
        "number" => value.is_number(),
        "boolean" => value.is_boolean(),
        "null" => value.is_null(),
        _ => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_projection_schema_is_named_closed_bounded_and_enforced() {
        for projection in ["occupant", "admin"] {
            let tools = tools(projection).unwrap();
            for tool in &tools {
                let name = tool["name"].as_str().unwrap();
                let schema = &tool["inputSchema"];
                assert_eq!(schema["title"], json!(format!("{name}.input")));
                assert_eq!(schema["additionalProperties"], false);
                assert!(schema["maxProperties"].as_u64().is_some());
                assert!(validate_call(
                    &tools,
                    &json!({"name":name,"arguments":{"unexpected":true}})
                )
                .is_err());
            }
        }
    }

    #[test]
    fn list_sessions_contract_matches_native_filters() {
        let tools = tools("admin").unwrap();
        let (_, args) = validate_call(
            &tools,
            &json!({"name":"agent_context_list_sessions","arguments":{
                "identity":"agent","substrate":"codex",
                "date_from":"2026-01-01T00:00:00Z","date_to":"2026-02-01T00:00:00Z",
                "limit":10
            }}),
        )
        .unwrap();
        assert_eq!(args["substrate"], "codex");
    }
}
