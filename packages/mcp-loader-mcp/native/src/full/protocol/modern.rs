use crate::full::*;

pub(crate) fn is_modern_request(params: &Value) -> bool {
    params
        .get("_meta")
        .and_then(Value::as_object)
        .and_then(|meta| meta.get("io.modelcontextprotocol/protocolVersion"))
        .and_then(Value::as_str)
        == Some(MODERN_PROTOCOL_VERSION)
}

pub(crate) fn validate_modern_request(params: &Value) -> Result<(), Diagnostic> {
    let meta = params
        .get("_meta")
        .and_then(Value::as_object)
        .ok_or_else(|| Diagnostic::new("modern_metadata_required", "modern_metadata_required"))?;
    if meta
        .get("io.modelcontextprotocol/clientInfo")
        .and_then(Value::as_object)
        .is_none()
    {
        return Err(Diagnostic::new(
            "modern_metadata_required",
            "modern_metadata_required:clientInfo",
        ));
    }
    if meta
        .get("io.modelcontextprotocol/clientCapabilities")
        .and_then(Value::as_object)
        .is_none()
    {
        return Err(Diagnostic::new(
            "modern_metadata_required",
            "modern_metadata_required:clientCapabilities",
        ));
    }
    Ok(())
}

pub(crate) fn modern_request_params() -> Value {
    json!({
        "_meta": {
            "io.modelcontextprotocol/protocolVersion": MODERN_PROTOCOL_VERSION,
            "io.modelcontextprotocol/clientInfo": {"name": SERVER_NAME, "version": SERVER_VERSION},
            "io.modelcontextprotocol/clientCapabilities": {}
        }
    })
}

pub(crate) fn modernize_result(value: Value, method: &str) -> Value {
    let mut result = value.as_object().cloned().unwrap_or_default();
    result.insert("resultType".to_string(), json!("complete"));
    if matches!(method, "tools/list" | "resources/list" | "resources/read") {
        result.entry("ttlMs".to_string()).or_insert(json!(300_000));
        result
            .entry("cacheScope".to_string())
            .or_insert(json!("public"));
    }
    let mut meta = result
        .remove("_meta")
        .and_then(|entry| entry.as_object().cloned())
        .unwrap_or_default();
    meta.insert(
        "io.modelcontextprotocol/serverInfo".to_string(),
        json!({"name": SERVER_NAME, "version": SERVER_VERSION}),
    );
    result.insert("_meta".to_string(), Value::Object(meta));
    Value::Object(result)
}

pub(crate) fn modern_discover_result() -> Value {
    json!({
        "supportedVersions": [MODERN_PROTOCOL_VERSION, PROTOCOL_VERSION],
        "capabilities": {"tools": {}},
        "ttlMs": 3_600_000,
        "cacheScope": "public"
    })
}

pub(crate) fn modern_discovery_is_valid(value: &Value) -> bool {
    value.get("resultType").and_then(Value::as_str) == Some("complete")
        && value
            .get("supportedVersions")
            .and_then(Value::as_array)
            .is_some_and(|versions| {
                versions
                    .iter()
                    .any(|version| version.as_str() == Some(MODERN_PROTOCOL_VERSION))
            })
}
