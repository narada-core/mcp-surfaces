
fn expected_metadata_schema() -> Value {
    json!({"type":"object","maxProperties":5,"additionalProperties":false,"properties":{
        "mtime":{"type":"string","maxLength":128},"size":{"type":"integer","minimum":0,"maximum":9_007_199_254_740_991_i64},
        "sha256":{"type":"string","pattern":"^[0-9a-fA-F]{64}$","maxLength":64},"tree_sha256":{"type":"string","pattern":"^[0-9a-fA-F]{64}$","maxLength":64},
        "entry_count":{"type":"integer","minimum":0,"maximum":5_000}
    }})
}

fn tool_has_write_effect(name: &str) -> bool {
    is_write_tool(name) || name == "fs_patch_outcome_show"
}

fn bound_tool_properties(properties: &mut Map<String, Value>) {
    for (name, schema) in properties.iter_mut() {
        let Some(object) = schema.as_object_mut() else {
            continue;
        };
        match object.get("type").and_then(Value::as_str) {
            Some("string") if !object.contains_key("maxLength") => {
                let limit = if matches!(name.as_str(), "content" | "replacement" | "old" | "new") {
                    8_388_608
                } else {
                    32_768
                };
                object.insert("maxLength".into(), json!(limit));
            }
            Some("array") => {
                object.entry("maxItems").or_insert_with(|| json!(256));
                if let Some(items) = object.get_mut("items").and_then(Value::as_object_mut) {
                    if items.get("type").and_then(Value::as_str) == Some("string") {
                        items.entry("maxLength").or_insert_with(|| json!(32_768));
                    }
                }
            }
            Some("integer") => {
                object.entry("minimum").or_insert_with(|| json!(0));
                let maximum = if name == "limit" {
                    1_000_i64
                } else if name == "timeout_ms" {
                    300_000
                } else if name.contains("bytes") {
                    1_073_741_824
                } else if name.contains("entry_count") {
                    5_000_000
                } else if matches!(name.as_str(), "offset" | "start_line" | "end_line") {
                    10_000_000
                } else {
                    9_007_199_254_740_991
                };
                object.entry("maximum").or_insert_with(|| json!(maximum));
            }
            Some("object") => {
                object.entry("maxProperties").or_insert_with(|| json!(256));
            }
            _ => {}
        }
    }
}

fn validate_tool_arguments(mode: &str, name: &str, args: &Value) -> Result<(), FsError> {
    let tool = list_tools(mode)
        .into_iter()
        .find(|tool| tool.get("name").and_then(Value::as_str) == Some(name))
        .ok_or_else(|| {
            FsError::new(
                format!("tool_not_available_in_{mode}_mode"),
                format!("tool_not_available_in_{mode}_mode: {name}"),
                json!({"tool_name":name,"mode":mode}),
            )
        })?;
    let schema = tool
        .get("inputSchema")
        .and_then(Value::as_object)
        .expect("tool schema");
    let object = args.as_object().ok_or_else(|| {
        FsError::new(
            "tool_arguments_must_be_object",
            "tool_arguments_must_be_object",
            json!({"tool_name":name,"actual_type":json_type(args)}),
        )
    })?;
    let properties = schema
        .get("properties")
        .and_then(Value::as_object)
        .expect("properties");
    let unknown: Vec<&String> = object
        .keys()
        .filter(|key| !properties.contains_key(*key))
        .collect();
    if !unknown.is_empty() {
        return Err(FsError::new(
            "tool_argument_unknown",
            "tool_argument_unknown",
            json!({"tool_name":name,"fields":unknown}),
        ));
    }
    for required in schema
        .get("required")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
    {
        if !object.contains_key(required) {
            return Err(FsError::new(
                "tool_argument_required",
                "tool_argument_required",
                json!({"tool_name":name,"field":required}),
            ));
        }
    }
    for (field, value) in object {
        validate_schema_value(name, field, value, &properties[field])?;
    }
    Ok(())
}

fn validate_schema_value(
    tool_name: &str,
    field: &str,
    value: &Value,
    schema: &Value,
) -> Result<(), FsError> {
    let expected = schema.get("type").and_then(Value::as_str).unwrap_or("any");
    let valid = match expected {
        "string" => value.is_string(),
        "integer" => value.as_i64().is_some() || value.as_u64().is_some(),
        "boolean" => value.is_boolean(),
        "array" => value.is_array(),
        "object" => value.is_object(),
        _ => true,
    };
    if !valid {
        return Err(FsError::new(
            "tool_argument_type_invalid",
            "tool_argument_type_invalid",
            json!({"tool_name":tool_name,"field":field,"expected":expected,"actual":json_type(value)}),
        ));
    }
    if let Some(text) = value.as_str() {
        if let Some(max) = schema.get("maxLength").and_then(Value::as_u64) {
            if text.chars().count() as u64 > max {
                return Err(FsError::new(
                    "tool_argument_too_long",
                    "tool_argument_too_long",
                    json!({"tool_name":tool_name,"field":field,"maximum":max}),
                ));
            }
        }
        if let Some(values) = schema.get("enum").and_then(Value::as_array) {
            if !values
                .iter()
                .any(|candidate| candidate.as_str() == Some(text))
            {
                return Err(FsError::new(
                    "tool_argument_enum_invalid",
                    "tool_argument_enum_invalid",
                    json!({"tool_name":tool_name,"field":field,"allowed":values}),
                ));
            }
        }
        if field == "operation_id" && !valid_operation_id(text) {
            return Err(FsError::new(
                "patch_operation_id_invalid",
                "patch_operation_id_invalid",
                json!({"operation_id":text}),
            ));
        }
        if matches!(
            field,
            "expected_sha256" | "expected_from_sha256" | "expected_to_sha256"
        ) && !text.is_empty()
            && !valid_sha256(text)
        {
            return Err(FsError::new(
                "tool_argument_sha256_invalid",
                "tool_argument_sha256_invalid",
                json!({"tool_name":tool_name,"field":field}),
            ));
        }
    }
    if let Some(items) = value.as_array() {
        let max = schema
            .get("maxItems")
            .and_then(Value::as_u64)
            .unwrap_or(256);
        if items.len() as u64 > max {
            return Err(FsError::new(
                "tool_argument_array_too_large",
                "tool_argument_array_too_large",
                json!({"tool_name":tool_name,"field":field,"maximum":max}),
            ));
        }
        if let Some(item_schema) = schema.get("items") {
            for item in items {
                validate_schema_value(tool_name, field, item, item_schema)?;
            }
        }
    }
    if let Some(number) = value.as_i64() {
        if schema
            .get("minimum")
            .and_then(Value::as_i64)
            .is_some_and(|min| number < min)
            || schema
                .get("maximum")
                .and_then(Value::as_i64)
                .is_some_and(|max| number > max)
        {
            return Err(FsError::new(
                "tool_argument_integer_out_of_range",
                "tool_argument_integer_out_of_range",
                json!({"tool_name":tool_name,"field":field,"value":number,"minimum":schema.get("minimum"),"maximum":schema.get("maximum")}),
            ));
        }
    }
    if let Some(object) = value.as_object() {
        let max = schema
            .get("maxProperties")
            .and_then(Value::as_u64)
            .unwrap_or(256);
        if object.len() as u64 > max {
            return Err(FsError::new(
                "tool_argument_object_too_large",
                "tool_argument_object_too_large",
                json!({"tool_name":tool_name,"field":field,"maximum":max}),
            ));
        }
        let declared = schema.get("properties").and_then(Value::as_object);
        if schema.get("additionalProperties") == Some(&Value::Bool(false)) {
            let unknown: Vec<_> = object
                .keys()
                .filter(|key| declared.is_none_or(|properties| !properties.contains_key(*key)))
                .collect();
            if !unknown.is_empty() {
                return Err(FsError::new(
                    "tool_argument_nested_unknown",
                    "tool_argument_nested_unknown",
                    json!({"tool_name":tool_name,"field":field,"fields":unknown}),
                ));
            }
        }
        for (key, item) in object {
            if key.chars().count() > 32_768 {
                return Err(FsError::new(
                    "tool_argument_key_too_long",
                    "tool_argument_key_too_long",
                    json!({"tool_name":tool_name,"field":field}),
                ));
            }
            if let Some(child) = declared
                .and_then(|properties| properties.get(key))
                .or_else(|| {
                    schema
                        .get("additionalProperties")
                        .filter(|value| value.is_object())
                })
            {
                validate_schema_value(tool_name, key, item, child)?;
            }
        }
    }
    Ok(())
}
