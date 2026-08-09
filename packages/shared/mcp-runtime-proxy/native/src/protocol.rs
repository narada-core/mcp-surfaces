use serde_json::{json, Map, Value};

pub const LEGACY_PROTOCOL_VERSION: &str = "2024-11-05";
pub const MODERN_PROTOCOL_VERSION: &str = "2026-07-28";

pub fn is_modern_request(request: &Value) -> bool {
    request
        .get("params")
        .and_then(Value::as_object)
        .and_then(|params| params.get("_meta"))
        .and_then(Value::as_object)
        .and_then(|meta| meta.get("io.modelcontextprotocol/protocolVersion"))
        .and_then(Value::as_str)
        == Some(MODERN_PROTOCOL_VERSION)
}

fn rpc_error(request: &Value, code: &str, message: &str, data: Value) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": request.get("id").cloned().unwrap_or(Value::Null),
        "error": { "code": -32000, "message": message, "data": {
            "schema": "narada.mcp_protocol.error.v1",
            "code": code,
            "message": message,
            "details": data
        }}
    })
}

fn valid_client_identity(value: Option<&Value>) -> bool {
    let Some(identity) = value.and_then(Value::as_object) else {
        return false;
    };
    identity.get("name").and_then(Value::as_str).is_some()
        && identity.get("version").and_then(Value::as_str).is_some()
}

fn validate_modern_request(request: &Value) -> Result<(), Value> {
    let params = request
        .get("params")
        .and_then(Value::as_object)
        .ok_or_else(|| {
            rpc_error(
                request,
                "modern_metadata_required",
                "Modern MCP requests require params._meta.",
                Value::Null,
            )
        })?;
    let meta = params
        .get("_meta")
        .and_then(Value::as_object)
        .ok_or_else(|| {
            rpc_error(
                request,
                "modern_metadata_required",
                "Modern MCP requests require params._meta.",
                Value::Null,
            )
        })?;
    if !valid_client_identity(meta.get("io.modelcontextprotocol/clientInfo")) {
        return Err(rpc_error(
            request,
            "modern_metadata_required",
            "Modern MCP requests require clientInfo metadata.",
            json!({ "key": "io.modelcontextprotocol/clientInfo" }),
        ));
    }
    if meta
        .get("io.modelcontextprotocol/clientCapabilities")
        .and_then(Value::as_object)
        .is_none()
    {
        return Err(rpc_error(
            request,
            "modern_metadata_required",
            "Modern MCP requests require clientCapabilities metadata.",
            json!({ "key": "io.modelcontextprotocol/clientCapabilities" }),
        ));
    }
    Ok(())
}

fn server_info(server_name: &str) -> Value {
    json!({ "name": server_name, "version": "0.1.0" })
}

pub fn preflight_response(request: &Value, server_name: &str) -> Option<Value> {
    let method = request
        .get("method")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if is_modern_request(request) {
        if let Err(response) = validate_modern_request(request) {
            return Some(response);
        }
        if method == "initialize" {
            return Some(rpc_error(
                request,
                "initialize_removed",
                "The 2026-07-28 protocol has no initialize handshake.",
                json!({ "protocolVersion": MODERN_PROTOCOL_VERSION }),
            ));
        }
        if method == "server/discover" {
            return Some(json!({
                "jsonrpc": "2.0",
                "id": request.get("id").cloned().unwrap_or(Value::Null),
                "result": {
                    "resultType": "complete",
                    "supportedVersions": [MODERN_PROTOCOL_VERSION, LEGACY_PROTOCOL_VERSION],
                    "capabilities": { "tools": {}, "resources": {}, "prompts": {}, "completions": {}, "logging": {} },
                    "_meta": { "io.modelcontextprotocol/serverInfo": server_info(server_name) },
                    "ttlMs": 3_600_000,
                    "cacheScope": "public"
                }
            }));
        }
        return None;
    }
    if method == "server/discover" {
        return Some(rpc_error(
            request,
            "modern_metadata_required",
            "server/discover requires 2026-07-28 request metadata.",
            json!({ "protocolVersion": MODERN_PROTOCOL_VERSION }),
        ));
    }
    None
}

pub fn modernize_response(request: &Value, response: Value, server_name: &str) -> Value {
    if !is_modern_request(request) {
        return response;
    }
    let method = request
        .get("method")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let Some(response_object) = response.as_object().cloned() else {
        return response;
    };
    let Some(result) = response_object.get("result") else {
        return response;
    };
    let mut result_object = result.as_object().cloned().unwrap_or_else(|| {
        let mut value = Map::new();
        value.insert("value".to_string(), result.clone());
        value
    });
    result_object.insert("resultType".to_string(), json!("complete"));
    let mut metadata = result_object
        .remove("_meta")
        .and_then(|value| value.as_object().cloned())
        .unwrap_or_default();
    metadata.insert(
        "io.modelcontextprotocol/serverInfo".to_string(),
        server_info(server_name),
    );
    result_object.insert("_meta".to_string(), Value::Object(metadata));
    if matches!(method, "tools/list" | "resources/list" | "resources/read") {
        result_object.insert("ttlMs".to_string(), json!(300_000));
        result_object.insert("cacheScope".to_string(), json!("public"));
    }
    let mut modern_response = response_object;
    modern_response.insert("result".to_string(), Value::Object(result_object));
    Value::Object(modern_response)
}
