fn auth_posture(root: &Path) -> (bool, &'static str) {
    let mut values = HashMap::new();
    if let Some(parent) = root.parent() {
        load_env_file(&mut values, &parent.join(".env"));
    }
    load_env_file(&mut values, &root.join(".env"));
    for key in [
        "MS_GRAPH_ACCESS_TOKEN",
        "GRAPH_ACCESS_TOKEN",
        "GRAPH_TENANT_ID",
        "GRAPH_CLIENT_ID",
        "GRAPH_CLIENT_SECRET",
    ] {
        if let Ok(value) = std::env::var(key) {
            values.insert(key.to_string(), value);
        }
    }
    let graph_access_token = non_empty_value(&values, "GRAPH_ACCESS_TOKEN");
    let client_credentials = ["GRAPH_TENANT_ID", "GRAPH_CLIENT_ID", "GRAPH_CLIENT_SECRET"]
        .iter()
        .all(|key| non_empty_value(&values, key));
    let ms_graph_access_token = non_empty_value(&values, "MS_GRAPH_ACCESS_TOKEN");
    if graph_access_token || (!client_credentials && ms_graph_access_token) {
        (true, "access_token")
    } else if client_credentials {
        (true, "client_credentials")
    } else {
        (false, "missing")
    }
}

fn load_env_file(values: &mut HashMap<String, String>, path: &Path) {
    let Ok(metadata) = fs::metadata(path) else {
        return;
    };
    if metadata.len() > MAX_TEXT_BYTES {
        return;
    }
    let Ok(text) = fs::read_to_string(path) else {
        return;
    };
    for line in text.lines() {
        let Some((key, raw_value)) = line.split_once('=') else {
            continue;
        };
        let key = key.trim();
        if key.is_empty() || key.starts_with('#') {
            continue;
        }
        let mut value = raw_value.trim().to_string();
        if value.len() >= 2
            && ((value.starts_with('"') && value.ends_with('"'))
                || (value.starts_with('\'') && value.ends_with('\'')))
        {
            value = value[1..value.len() - 1].to_string();
        }
        values.insert(key.to_string(), value);
    }
}

fn non_empty_value(values: &HashMap<String, String>, key: &str) -> bool {
    values
        .get(key)
        .is_some_and(|value| !value.trim().is_empty())
}

fn output_show(args: &Map<String, Value>, root: &Path) -> Result<Value, Value> {
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
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        return Err(error("output_ref_invalid", "output_ref_invalid"));
    }
    let path = root
        .join(".ai/tmp/mcp-outputs/workspace")
        .join(format!("{id}.json"));
    if fs::metadata(&path)
        .map_err(|_| error("output_ref_not_found", "output_ref_not_found"))?
        .len()
        > MAX_TEXT_BYTES
    {
        return Err(error("output_ref_too_large", "output_ref_too_large"));
    }
    let text = fs::read_to_string(&path)
        .map_err(|_| error("output_ref_not_found", "output_ref_not_found"))?;
    let record: Value = serde_json::from_str(&text)
        .map_err(|e| error("output_ref_invalid_json", &e.to_string()))?;
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
    let full = record.get("full_output").cloned().unwrap_or(Value::Null);
    let presentation = serde_json::to_string_pretty(&full).unwrap_or_else(|_| full.to_string());
    let offset = args.get("offset").and_then(Value::as_u64).unwrap_or(0) as usize;
    let limit = args
        .get("limit")
        .or_else(|| args.get("output_limit"))
        .and_then(Value::as_u64)
        .unwrap_or(10000)
        .min(10000) as usize;
    let chars = presentation.chars().collect::<Vec<_>>();
    let start = offset.min(chars.len());
    let chunk = chars.iter().skip(start).take(limit).collect::<String>();
    let end = start + chunk.chars().count();
    Ok(
        json!({"schema":"narada.mcp_output_page.v1","status":"ok","ref":reference,"tool_name":record.get("tool_name"),"full_output_char_length":record.get("full_output_char_length").cloned().unwrap_or_else(|| json!(chars.len())),"byte_size":text.len(),"original_truncated":record.get("truncated").and_then(Value::as_bool).unwrap_or(false),"path":format!(".ai/tmp/mcp-outputs/workspace/{id}.json"),"offset":start,"limit":limit,"next_offset":if end<chars.len(){json!(end)}else{Value::Null},"output_limit":limit,"output_truncated":end<chars.len(),"output_text":chunk}),
    )
}

fn write_schema(create: bool, update: bool) -> Value {
    let mut properties = Map::new();
    properties.insert("mailbox_id".into(), json!({"type":"string","default":"me"}));
    for key in [
        "subject",
        "body_text",
        "body_html",
        "start_datetime",
        "end_datetime",
        "time_zone",
        "location",
        "online_meeting_provider",
        "show_as",
        "sensitivity",
        "approval_token",
    ] {
        properties.insert(key.into(), json!({"type":"string"}));
    }
    properties.insert("attendees".into(), json!({"type":"array","items":{"oneOf":[{"type":"string"},{"type":"object","additionalProperties":false,"properties":{"emailAddress":{"type":"object","additionalProperties":false,"properties":{"address":{"type":"string"},"name":{"type":"string"}},"required":["address"]},"type":{"type":"string","enum":["required","optional","resource"]}},"required":["emailAddress"]}]}}));
    properties.insert("is_online_meeting".into(), json!({"type":"boolean"}));
    properties.insert(
        "confirm_write".into(),
        json!({"type":"boolean","default":false}),
    );
    if create {
        properties.insert("calendar_id".into(), json!({"type":"string"}));
    }
    if update {
        properties.insert("event_id".into(), json!({"type":"string"}));
    }
    json!({"type":"object","properties":properties,"required":if create {json!(["subject","start_datetime","end_datetime","time_zone"])} else {json!(["event_id"])},"additionalProperties":false})
}

fn authority_boundary(tool_name: &str) -> Value {
    json!({"schema":"narada.calendar_mcp.authority_boundary.v1","status":"unavailable","reason":"native_calendar_external_authority_not_enabled","tool_name":tool_name,"remediation":"Use the existing calendar Graph adapter, or explicitly approve a native adapter that transmits credentials and performs external calendar operations."})
}

fn error(code: &str, message: &str) -> Value {
    json!({"schema":"narada.calendar_mcp.error.v1","code":code,"message":message})
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_contract_keeps_external_authority_explicit() {
        let root = std::env::temp_dir().join(format!("narada-calendar-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&root).expect("root");
        let tools = list_tools();
        assert!(tools
            .iter()
            .any(|tool| tool["name"] == "calendar_event_query"));
        let doctor = call_tool("calendar_doctor", &Map::new(), &root).expect("doctor");
        assert_eq!(doctor["has_access_token"], false);
        assert_eq!(doctor["auth_mode"], "missing");
        let refusal = call_tool("calendar_event_query", &Map::new(), &root).expect_err("boundary");
        assert_eq!(refusal["status"], "unavailable");
        fs::remove_dir_all(root).expect("cleanup");
    }
}
