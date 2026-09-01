use crate::full::*;

pub(crate) fn normalize_input_schema(schema: &mut Value, field: Option<&str>) {
    let Some(object) = schema.as_object_mut() else {
        return;
    };
    match object.get("type").and_then(Value::as_str) {
        Some("string") if !object.contains_key("maxLength") && !object.contains_key("enum") => {
            let field = field.unwrap_or_default().to_ascii_lowercase();
            let maximum =
                if field.contains("path") || field.contains("root") || field.contains("entrypoint")
                {
                    4096
                } else if field.contains("reason") || field.contains("digest") {
                    8192
                } else {
                    1024
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
            normalize_input_schema(child, Some(name));
        }
    }
    if let Some(items) = object.get_mut("items") {
        normalize_input_schema(items, field);
    }
}

pub(crate) fn validate_input_schema(
    schema: &Value,
    value: &Value,
    path: &str,
) -> Result<(), Diagnostic> {
    let invalid = |reason: String| {
        let keyword = reason.split(':').next().unwrap_or(reason.as_str());
        let expected = schema
            .get(keyword)
            .cloned()
            .unwrap_or_else(|| match keyword {
                "type_mismatch" => schema.get("type").cloned().unwrap_or(Value::Null),
                "enum_mismatch" => schema.get("enum").cloned().unwrap_or(Value::Null),
                _ => Value::Null,
            });
        let property = path.rsplit(['.', '/']).next().unwrap_or(path);
        let corrected_value = match keyword {
            "maximum" | "minimum" => expected.clone(),
            "maxLength" => json!("x".repeat(expected.as_u64().unwrap_or(0) as usize)),
            "minLength" => json!("x".repeat(expected.as_u64().unwrap_or(0) as usize)),
            "maxItems" => json!([]),
            "maxProperties" => json!({}),
            _ => Value::Null,
        };
        let corrected = json!({"operation":"replace","path":path,"value":corrected_value,"merge_into_original_arguments":true});
        Diagnostic::new(
            "input_schema_validation_failed",
            format!(
                "input_schema_validation_failed:{path}:{reason}: expected {}; received {}",
                expected, value
            ),
        )
        .with_details(json!({
            "path":path,
            "violated_property":property,
            "constraint":keyword,
            "expected":expected,
            "received":value,
            "reason":reason,
            "corrected_call_template":corrected
        }))
    };
    let type_matches = |kind: &str| match kind {
        "object" => value.is_object(),
        "array" => value.is_array(),
        "string" => value.is_string(),
        "integer" => value.as_i64().is_some() || value.as_u64().is_some(),
        "number" => value.is_number(),
        "boolean" => value.is_boolean(),
        "null" => value.is_null(),
        _ => true,
    };
    let matches = match schema.get("type") {
        Some(Value::String(expected)) => type_matches(expected),
        Some(Value::Array(expected)) => expected.iter().filter_map(Value::as_str).any(type_matches),
        _ => true,
    };
    if !matches {
        return Err(invalid("type_mismatch".into()));
    }
    if let Some(allowed) = schema.get("enum").and_then(Value::as_array) {
        if !allowed.contains(value) {
            return Err(invalid("enum_mismatch".into()));
        }
    }
    match value {
        Value::Object(arguments) => {
            if schema.get("additionalProperties") == Some(&Value::Bool(false)) {
                let properties = schema.get("properties").and_then(Value::as_object);
                if let Some(key) = arguments
                    .keys()
                    .find(|key| !properties.is_some_and(|map| map.contains_key(*key)))
                {
                    return Err(invalid(format!("unknown_field:{key}")));
                }
            }
            if let Some(required) = schema.get("required").and_then(Value::as_array) {
                if let Some(field) = required
                    .iter()
                    .filter_map(Value::as_str)
                    .find(|field| !arguments.contains_key(*field))
                {
                    return Err(invalid(format!("required:{field}")));
                }
            }
            validate_bound(schema, arguments.len(), "maxProperties", &invalid)?;
            if let Some(properties) = schema.get("properties").and_then(Value::as_object) {
                for (name, value) in arguments {
                    if let Some(child) = properties.get(name) {
                        validate_input_schema(child, value, &format!("{path}.{name}"))?;
                    }
                }
            }
        }
        Value::Array(items) => {
            validate_bound(schema, items.len(), "maxItems", &invalid)?;
            if let Some(item_schema) = schema.get("items") {
                for (index, item) in items.iter().enumerate() {
                    validate_input_schema(item_schema, item, &format!("{path}[{index}]"))?;
                }
            }
        }
        Value::String(text) => {
            validate_bound(schema, text.chars().count(), "maxLength", &invalid)?;
            if schema
                .get("minLength")
                .and_then(Value::as_u64)
                .is_some_and(|minimum| text.chars().count() < minimum as usize)
            {
                return Err(invalid("minLength".into()));
            }
        }
        Value::Number(number) => {
            let number = number.as_f64().unwrap_or_default();
            if schema
                .get("minimum")
                .and_then(Value::as_f64)
                .is_some_and(|minimum| number < minimum)
            {
                return Err(invalid("minimum".into()));
            }
            if schema
                .get("maximum")
                .and_then(Value::as_f64)
                .is_some_and(|maximum| number > maximum)
            {
                return Err(invalid("maximum".into()));
            }
        }
        _ => {}
    }
    Ok(())
}

pub(crate) fn validate_bound<F>(
    schema: &Value,
    actual: usize,
    key: &str,
    invalid: &F,
) -> Result<(), Diagnostic>
where
    F: Fn(String) -> Diagnostic,
{
    if schema
        .get(key)
        .and_then(Value::as_u64)
        .is_some_and(|maximum| actual > maximum as usize)
    {
        return Err(invalid(key.to_string()));
    }
    Ok(())
}
