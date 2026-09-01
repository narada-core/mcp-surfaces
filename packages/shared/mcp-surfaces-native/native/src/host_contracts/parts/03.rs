pub(crate) fn output_show(args: &Map<String, Value>, root: &Path) -> Result<Value, Value> {
    let ref_value = args.get("ref").and_then(Value::as_str).map(str::trim);
    let output_ref_value = args
        .get("output_ref")
        .and_then(Value::as_str)
        .map(str::trim);
    if let (Some(reference), Some(output_ref)) = (ref_value, output_ref_value) {
        if reference != output_ref {
            return Err(error(
                "output_show_ref_alias_conflict",
                "output_show_ref_alias_conflict",
            ));
        }
    }
    let reference = ref_value
        .or(output_ref_value)
        .ok_or_else(|| error("output_show_requires_ref", "output_show_requires_ref"))?;
    let id = reference
        .strip_prefix("mcp_output:")
        .ok_or_else(|| error("output_ref_invalid", "output_ref_invalid"))?;
    if id.len() < 3
        || id.len() > 64
        || !id
            .chars()
            .all(|value| value.is_ascii_alphanumeric() || matches!(value, '_' | '-'))
    {
        return Err(error("output_ref_invalid", "output_ref_invalid"));
    }
    let path = root
        .join(".ai/tmp/mcp-outputs/workspace")
        .join(format!("{id}.json"));
    let metadata =
        fs::metadata(&path).map_err(|_| error("output_ref_not_found", "output_ref_not_found"))?;
    if !metadata.is_file() {
        return Err(error("output_ref_not_file", "output_ref_not_file"));
    }
    if metadata.len() > MAX_OUTPUT_BYTES {
        return Err(error("output_ref_too_large", "output_ref_too_large"));
    }
    let text = fs::read_to_string(&path)
        .map_err(|_| error("output_ref_not_found", "output_ref_not_found"))?;
    let record: Value = serde_json::from_str(&text)
        .map_err(|parse_error| error("output_ref_invalid_json", &parse_error.to_string()))?;
    if record.get("schema").and_then(Value::as_str) != Some("narada.mcp_output_ref.v1") {
        return Err(error(
            "output_ref_schema_unsupported",
            "output_ref_schema_unsupported",
        ));
    }
    if record.get("ref").and_then(Value::as_str) != Some(reference)
        || record.get("output_id").and_then(Value::as_str) != Some(id)
    {
        return Err(error(
            "output_ref_metadata_mismatch",
            "output_ref_metadata_mismatch",
        ));
    }
    let full_output = record.get("full_output").cloned().unwrap_or(Value::Null);
    let presentation =
        serde_json::to_string_pretty(&full_output).unwrap_or_else(|_| full_output.to_string());
    let offset = match args.get("offset") {
        None => 0,
        Some(value) => value.as_u64().ok_or_else(|| {
            error(
                "offset_must_be_non_negative_integer",
                "offset_must_be_non_negative_integer",
            )
        })?,
    };
    let limit = match args.get("limit").or_else(|| args.get("output_limit")) {
        None => DEFAULT_OUTPUT_LIMIT,
        Some(value) => {
            let value = value.as_u64().ok_or_else(|| {
                error(
                    "output_limit_must_be_positive_integer",
                    "output_limit_must_be_positive_integer",
                )
            })?;
            if value == 0 {
                return Err(error(
                    "output_limit_must_be_positive_integer",
                    "output_limit_must_be_positive_integer",
                ));
            }
            if value > MAX_OUTPUT_LIMIT {
                return Err(error(
                    "output_limit_exceeds_transport_maximum",
                    "output_limit_exceeds_transport_maximum",
                ));
            }
            value
        }
    };
    let chars = presentation.chars().collect::<Vec<_>>();
    let start = (offset as usize).min(chars.len());
    let chunk = chars
        .iter()
        .skip(start)
        .take(limit as usize)
        .collect::<String>();
    let end = start + chunk.chars().count();
    Ok(json!({
        "schema":"narada.mcp_output_page.v1",
        "status":"ok",
        "ref":reference,
        "tool_name":record.get("tool_name").cloned().unwrap_or(Value::Null),
        "full_output_char_length":record.get("full_output_char_length").cloned().unwrap_or_else(|| json!(chars.len())),
        "byte_size":metadata.len(),
        "original_truncated":record.get("truncated").and_then(Value::as_bool).unwrap_or(false),
        "path":format!(".ai/tmp/mcp-outputs/workspace/{id}.json"),
        "offset":start,
        "limit":limit,
        "next_offset":if end < chars.len() { json!(end) } else { Value::Null },
        "output_limit":limit,
        "output_truncated":end < chars.len(),
        "output_text":chunk
    }))
}

fn graph_string(object: &Map<String, Value>, snake: &str, camel: &str) -> Option<String> {
    object
        .get(snake)
        .or_else(|| object.get(camel))
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .map(ToOwned::to_owned)
}
fn graph_string_array(object: &Map<String, Value>, snake: &str, camel: &str) -> Vec<Value> {
    object
        .get(snake)
        .or_else(|| object.get(camel))
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|value| {
                    value
                        .as_str()
                        .filter(|item| !item.trim().is_empty())
                        .map(|item| Value::String(item.to_string()))
                })
                .collect()
        })
        .unwrap_or_default()
}
fn graph_bool(object: &Map<String, Value>, snake: &str, camel: &str) -> bool {
    object
        .get(snake)
        .or_else(|| object.get(camel))
        .and_then(Value::as_bool)
        .unwrap_or(false)
}
fn graph_token_configured(object: &Map<String, Value>, snake: &str, camel: &str) -> bool {
    graph_string(object, snake, camel).is_some()
}
fn resolve_graph_path(root: &Path, value: &str) -> String {
    let path = PathBuf::from(value);
    if path.is_absolute() {
        path.to_string_lossy().to_string()
    } else {
        root.join(path).to_string_lossy().to_string()
    }
}
fn graph_auth_posture(
    root: &Path,
    allow_device_code: bool,
    scopes: &[Value],
) -> (bool, &'static str) {
    let delegated = graph_delegated_token_summary(root);
    let delegated_allowed = allow_device_code
        && delegated
            .get("status")
            .and_then(Value::as_str)
            .map(|status| status == "available" || status == "refreshable")
            .unwrap_or(false)
        && delegated
            .get("scope")
            .and_then(Value::as_str)
            .map(|scope| scopes.iter().any(|value| value.as_str() == Some(scope)))
            .unwrap_or(false);
    if delegated_allowed {
        return (true, "delegated_device_code");
    }
    let graph_access = graph_non_empty_env(root, "GRAPH_ACCESS_TOKEN");
    let client_credentials = graph_non_empty_env(root, "GRAPH_TENANT_ID")
        && graph_non_empty_env(root, "GRAPH_CLIENT_ID")
        && graph_non_empty_env(root, "GRAPH_CLIENT_SECRET");
    let ms_access = graph_non_empty_env(root, "MS_GRAPH_ACCESS_TOKEN");
    if graph_access || (!client_credentials && ms_access) {
        (true, "access_token")
    } else if client_credentials {
        (true, "client_credentials")
    } else {
        (false, "missing")
    }
}
fn graph_delegated_token_summary(root: &Path) -> Value {
    let value = read_json_file(&root.join(".ai/runtime/graph-mail-mcp/delegated-token.json"));
    let Some(object) = value.as_object() else {
        return json!({"status":"missing","fresh":false});
    };
    if object.get("schema").and_then(Value::as_str)
        != Some("narada.graph_mail_mcp.delegated_token.v1")
    {
        return json!({"status":"missing","fresh":false});
    }
    let expires = object
        .get("expires_at_ms")
        .and_then(Value::as_i64)
        .unwrap_or(0);
    let fresh = expires > chrono_now_ms() + 60_000;
    let refreshable = object
        .get("refresh_token")
        .and_then(Value::as_str)
        .map(|value| !value.trim().is_empty())
        .unwrap_or(false);
    json!({"status":if fresh{"available"}else if refreshable{"refreshable"}else{"expired"},"fresh":fresh,"refreshable":refreshable,"auth_mode":object.get("auth_mode").cloned().unwrap_or(Value::Null),"tenant_id":object.get("tenant_id").cloned().unwrap_or(Value::Null),"client_id":object.get("client_id").cloned().unwrap_or(Value::Null),"scope":object.get("scope").cloned().unwrap_or(Value::Null),"acquired_at":object.get("acquired_at").cloned().unwrap_or(Value::Null),"expires_at_ms":object.get("expires_at_ms").cloned().unwrap_or(Value::Null)})
}
fn chrono_now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or(0)
}
fn graph_non_empty_env(root: &Path, key: &str) -> bool {
    let mut values = HashMap::new();
    for path in [
        root.parent().map(|parent| parent.join(".env")),
        Some(root.join(".env")),
    ]
    .into_iter()
    .flatten()
    {
        if let Ok(text) = fs::read_to_string(path) {
            for line in text.lines() {
                if let Some((name, value)) = line.split_once('=') {
                    values.insert(
                        name.trim().to_string(),
                        value.trim().trim_matches(['\'', '"']).to_string(),
                    );
                }
            }
        }
    }
    if let Ok(value) = env::var(key) {
        values.insert(key.to_string(), value);
    }
    values
        .get(key)
        .map(|value| !value.trim().is_empty())
        .unwrap_or(false)
}
fn read_json_file(path: &Path) -> Value {
    let Ok(meta) = fs::metadata(path) else {
        return Value::Object(Map::new());
    };
    if !meta.is_file() || meta.len() > MAX_BYTES {
        return Value::Object(Map::new());
    }
    fs::read_to_string(path)
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_else(|| Value::Object(Map::new()))
}
fn boundary(surface_id: &str, name: &str, reason: &str, remediation: &str) -> Value {
    json!({"schema":format!("narada.{surface_id}.authority_boundary.v1"),"status":"unavailable","tool_name":name,"reason":reason,"remediation":remediation})
}
fn error(code: &str, message: &str) -> Value {
    json!({"schema":"narada.host_surface.error.v1","code":code,"message":message})
}
fn tool(name: &str, description: String, read_only: bool) -> Value {
    json!({"name":name,"description":description,"inputSchema":{"type":"object","additionalProperties":true},"annotations":{"title":name,"readOnlyHint":read_only,"destructiveHint":!read_only,"idempotentHint":read_only,"openWorldHint":!read_only},"outputSchema":{"type":"object","additionalProperties":true}})
}

