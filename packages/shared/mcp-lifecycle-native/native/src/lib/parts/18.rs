fn sql_value(v: rusqlite::types::Value) -> Value {
    match v {
        rusqlite::types::Value::Null => Value::Null,
        rusqlite::types::Value::Integer(v) => json!(v),
        rusqlite::types::Value::Real(v) => json!(v),
        rusqlite::types::Value::Text(v) => Value::String(v),
        rusqlite::types::Value::Blob(v) => json!(base64_like(&v)),
    }
}
fn base64_like(v: &[u8]) -> String {
    v.iter().map(|b| format!("{b:02x}")).collect()
}
fn db_error<E: std::fmt::Display>(e: E) -> String {
    format!("sqlite_error:{e}")
}
fn now() -> String {
    OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .unwrap_or_else(|_| "1970-01-01T00:00:00Z".to_string())
}
fn utc_daily_due_key(value: &str) -> Result<String, String> {
    let instant = OffsetDateTime::parse(value, &Rfc3339)
        .map_err(|error| format!("recurring_current_time_invalid:{error}"))?;
    let date = instant.to_offset(time::UtcOffset::UTC).date();
    Ok(format!("{:04}-{:02}-{:02}", date.year(), u8::from(date.month()), date.day()))
}
fn read_json_file(path: &Path) -> Option<Value> {
    let text = fs::read_to_string(path).ok()?;
    serde_json::from_str(&text).ok()
}
fn write_json_file(path: &Path, value: &Value, label: &str) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("{label}_directory_create_failed:{e}"))?;
    }
    let bytes = serde_json::to_vec_pretty(value).map_err(|e| format!("{label}_serialize_failed:{e}"))?;
    fs::write(path, bytes).map_err(|e| format!("{label}_write_failed:{e}"))
}
fn digest(value: &Value) -> String {
    native_canonical_digest(value)
}

fn validate_result_schema(schema: &Value, value: &Value, path: &str, errors: &mut Vec<Value>) {
    if let Some(expected) = schema.get("type").and_then(Value::as_str) {
        let valid = match expected {
            "object" => value.is_object(), "array" => value.is_array(), "string" => value.is_string(),
            "integer" => value.as_i64().is_some() || value.as_u64().is_some(), "number" => value.is_number(),
            "boolean" => value.is_boolean(), "null" => value.is_null(), _ => true,
        };
        if !valid { errors.push(json!({"path":path,"keyword":"type","expected":expected})); return; }
    }
    if let Some(allowed) = schema.get("enum").and_then(Value::as_array) {
        if !allowed.iter().any(|candidate| candidate == value) { errors.push(json!({"path":path,"keyword":"enum","allowed":allowed})); }
    }
    if let Some(object) = value.as_object() {
        if let Some(required) = schema.get("required").and_then(Value::as_array) {
            for field in required.iter().filter_map(Value::as_str) {
                if !object.contains_key(field) { errors.push(json!({"path":format!("{path}/{field}"),"keyword":"required"})); }
            }
        }
        if let Some(properties) = schema.get("properties").and_then(Value::as_object) {
            for (field, child_schema) in properties { if let Some(child) = object.get(field) { validate_result_schema(child_schema, child, &format!("{path}/{field}"), errors); } }
        }
    }
    if let (Some(items), Some(array)) = (schema.get("items"), value.as_array()) {
        for (index, child) in array.iter().enumerate() { validate_result_schema(items, child, &format!("{path}/{index}"), errors); }
    }
}
fn validate_input(schema: &Value, value: &Value, path: &str) -> Result<(), String> {
    if let Some(expected) = schema.get("type").and_then(Value::as_str) {
        let valid = match expected {
            "object" => value.is_object(),
            "array" => value.is_array(),
            "string" => value.is_string(),
            "integer" => value.as_i64().is_some() || value.as_u64().is_some(),
            "number" => value.is_number(),
            "boolean" => value.is_boolean(),
            "null" => value.is_null(),
            _ => true,
        };
        if !valid {
            return Err(format!("input_schema_validation_failed:{path}:type:{expected}"));
        }
    }
    if let Some(allowed) = schema.get("enum").and_then(Value::as_array) {
        if !allowed.iter().any(|candidate| candidate == value) {
            return Err(format!("input_schema_validation_failed:{path}:enum"));
        }
    }
    if let Some(text) = value.as_str() {
        let length = text.chars().count() as u64;
        if schema.get("minLength").and_then(Value::as_u64).is_some_and(|minimum| length < minimum) {
            return Err(format!("input_schema_validation_failed:{path}:minLength"));
        }
        if schema.get("maxLength").and_then(Value::as_u64).is_some_and(|maximum| length > maximum) {
            return Err(format!("input_schema_validation_failed:{path}:maxLength"));
        }
    }
    if let Some(array) = value.as_array() {
        if schema.get("maxItems").and_then(Value::as_u64).is_some_and(|maximum| array.len() as u64 > maximum) {
            return Err(format!("input_schema_validation_failed:{path}:maxItems"));
        }
        if let Some(items) = schema.get("items") {
            for (index, child) in array.iter().enumerate() {
                validate_input(items, child, &format!("{path}/{index}"))?;
            }
        }
    }
    if let Some(object) = value.as_object() {
        if let Some(required) = schema.get("required").and_then(Value::as_array) {
            for field in required.iter().filter_map(Value::as_str) {
                if !object.contains_key(field) {
                    return Err(format!("input_schema_validation_failed:{path}/{field}:required"));
                }
            }
        }
        let properties = schema.get("properties").and_then(Value::as_object);
        if schema.get("additionalProperties").and_then(Value::as_bool) == Some(false) {
            if let Some(unknown) = object.keys().find(|field| properties.is_none_or(|known| !known.contains_key(*field))) {
                return Err(format!("input_schema_validation_failed:{path}/{unknown}:additionalProperties"));
            }
        }
        if let Some(properties) = properties {
            for (field, child_schema) in properties {
                if let Some(child) = object.get(field) {
                    validate_input(child_schema, child, &format!("{path}/{field}"))?;
                }
            }
        }
    }
    for keyword in ["allOf", "anyOf", "oneOf"] {
        if let Some(branches) = schema.get(keyword).and_then(Value::as_array) {
            let matches = branches.iter().filter(|branch| validate_input(branch, value, path).is_ok()).count();
            let valid = match keyword {
                "allOf" => matches == branches.len(),
                "oneOf" => matches == 1,
                _ => matches > 0,
            };
            if !valid {
                return Err(format!("input_schema_validation_failed:{path}:{keyword}"));
            }
        }
    }
    Ok(())
}
fn required_string(args: &Value, key: &str) -> Result<String, String> {
    string_arg(args, key).ok_or_else(|| format!("{key}_required"))
}
fn required_i64(args: &Value, key: &str) -> Result<i64, String> {
    args.get(key)
        .and_then(Value::as_i64)
        .ok_or_else(|| format!("{key}_required"))
}
fn string_arg(args: &Value, key: &str) -> Option<String> {
    args.get(key)
        .and_then(Value::as_str)
        .map(ToString::to_string)
}
fn safe_reference_id(reference: &str, prefix: &str) -> Result<String, String> {
    let id = reference.strip_prefix(prefix).unwrap_or(reference);
    if id.is_empty()
        || id.len() > 200
        || id.contains("..")
        || id.contains('/')
        || id.contains('\\')
        || id.contains('\0')
    {
        return Err(format!("invalid_reference:{reference}"));
    }
    if !id
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | ':' | '.'))
    {
        return Err(format!("invalid_reference:{reference}"));
    }
    Ok(id.to_string())
}
fn normalized_text(args: &Value, key: &str) -> String {
    match args.get(key) {
        Some(Value::Array(v)) => v
            .iter()
            .filter_map(Value::as_str)
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .collect::<Vec<_>>()
            .join("\n"),
        Some(Value::String(v)) => v.clone(),
        _ => String::new(),
    }
}
fn binding_string(input: &Map<String, Value>, key: &str, required: bool) -> Result<Option<String>, String> {
    let Some(value) = input.get(key) else { return Ok(None); };
    if value.is_null() && !required { return Ok(None); }
    let Some(value) = value.as_str() else {
        return Err(format!("execution_binding_{key}_must_be_string"));
    };
    let value = value.trim();
    if value.is_empty() {
        if required { return Err(format!("execution_binding_{key}_required")); }
        return Ok(None);
    }
    Ok(Some(value.to_string()))
}
