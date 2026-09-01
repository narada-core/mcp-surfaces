pub fn protocol_request(
    context: &Context,
    projection: &str,
    method: &str,
    params: &Value,
) -> Result<Value, String> {
    match method {
        "resources/list" => {
            if projection == "occupant" {
                return Ok(json!({"resources":[]}));
            }
            let directory = context.site_root.join(".ai/tmp/mcp-outputs/workspace");
            let offset = params
                .get("cursor")
                .and_then(Value::as_str)
                .unwrap_or("0")
                .parse::<usize>()
                .map_err(|_| "output_resource_cursor_invalid")?;
            if offset > 10_000 {
                return Err("output_resource_cursor_invalid".into());
            }
            let mut resources = if directory.exists() {
                let mut values = Vec::new();
                for entry in fs::read_dir(directory)
                    .map_err(|e| format!("output_resource_list_failed:{e}"))?
                {
                    let entry = entry.map_err(|e| format!("output_resource_list_failed:{e}"))?;
                    let name = entry.file_name().to_string_lossy().to_string();
                    let Some(id) = name.strip_suffix(".json") else {
                        continue;
                    };
                    values.push(json!({"uri":format!("mcp-output:{}",percent_encode(&format!("mcp_output:{id}"))),"name":format!("mcp_output:{id}"),"title":format!("mcp_output:{id}"),"description":"Materialized MCP output ref.","mimeType":"application/json"}));
                    if values.len() > 10_000 {
                        return Err("output_resource_scan_limit_reached".into());
                    }
                }
                values
            } else {
                vec![]
            };
            resources.sort_by_key(|v| v["name"].as_str().unwrap_or("").to_string());
            let page = resources
                .iter()
                .skip(offset)
                .take(100)
                .cloned()
                .collect::<Vec<_>>();
            let has_more = offset + page.len() < resources.len();
            Ok(
                json!({"resources":page,"offset":offset,"limit":100,"returned":page.len(),"next_offset":if has_more{Some(offset+page.len())}else{None},"nextCursor":if has_more{Some((offset+page.len()).to_string())}else{None},"has_more":has_more}),
            )
        }
        "resources/read" => {
            if projection == "occupant" {
                return Err("agent_context_resources_not_exposed_in_occupant_projection".into());
            }
            let uri = params.get("uri").and_then(Value::as_str).unwrap_or("");
            let encoded = uri
                .strip_prefix("mcp-output:")
                .ok_or_else(|| format!("output_resource_uri_invalid: {uri}"))?;
            let reference = percent_decode(encoded)?;
            let page = output_show(context, &json!({"ref":reference,"offset":0,"limit":10000}))?;
            Ok(
                json!({"contents":[{"uri":uri,"mimeType":"application/json","text":serde_json::to_string_pretty(&page).unwrap()}]}),
            )
        }
        "prompts/list" => Ok(
            json!({"prompts":if projection=="occupant"{vec![]}else{vec![json!({"name":"agent_context_startup","title":"Agent Context Startup","description":"Guidance for exact admitted Orientation Manifest delivery and bounded continuity.","arguments":[]})]}}),
        ),
        "prompts/get" => {
            if projection == "occupant" {
                return Err("agent_context_prompts_not_exposed_in_occupant_projection".into());
            }
            let name = params.get("name").and_then(Value::as_str).unwrap_or("");
            if name != "agent_context_startup" {
                return Err(format!("unknown_prompt: {name}"));
            }
            Ok(
                json!({"description":"Guidance for exact admitted Orientation Manifest delivery and bounded continuity.","messages":[{"role":"user","content":{"type":"text","text":"This is the enforced Carrier-entry orientation turn. Call agent_orientation_read({}) and then execute each returned next_call exactly. A continuation is opaque: never inspect or alter it. Stop only when status=ready and ordinary_work_gate=open. Agent Context retains required-read and acknowledgement evidence. The inline brief names exact continuity and work entry snapshots or explicit omissions and carries one canonical manifest_ref. Acknowledgement proves delivery and completed reads, not comprehension or authority for a later action."}}]}),
            )
        }
        "completion/complete" => {
            let argument = params
                .pointer("/argument/name")
                .and_then(Value::as_str)
                .unwrap_or("");
            let values = if argument == "name" {
                crate::contract::tools(projection)?
                    .iter()
                    .filter_map(|v| v.get("name").and_then(Value::as_str))
                    .take(100)
                    .map(str::to_string)
                    .collect::<Vec<_>>()
            } else {
                vec![]
            };
            Ok(json!({"completion":{"values":values,"total":values.len(),"hasMore":false}}))
        }
        "logging/setLevel" => Ok(json!({})),
        _ => Err(format!("unsupported_method: {method}")),
    }
}

fn percent_encode(value: &str) -> String {
    value
        .bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) {
                (b as char).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect()
}
fn percent_decode(value: &str) -> Result<String, String> {
    let bytes = value.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            if i + 2 >= bytes.len() {
                return Err("output_resource_uri_invalid_encoding".into());
            }
            let hex = std::str::from_utf8(&bytes[i + 1..i + 3])
                .map_err(|_| "output_resource_uri_invalid_encoding")?;
            out.push(
                u8::from_str_radix(hex, 16).map_err(|_| "output_resource_uri_invalid_encoding")?,
            );
            i += 3
        } else {
            out.push(bytes[i]);
            i += 1
        }
    }
    String::from_utf8(out).map_err(|_| "output_resource_uri_invalid_encoding".into())
}

pub fn bounded_tool_result(context: &Context, tool: &str, value: Value) -> Result<Value, String> {
    let text = serde_json::to_string_pretty(&value).map_err(|e| e.to_string())?;
    let structured = if text.chars().count() <= 6000 {
        value
    } else {
        materialize_output(context, tool, value, &text)?
    };
    let content = serde_json::to_string_pretty(&structured).map_err(|e| e.to_string())?;
    Ok(
        json!({"resultType":"complete","content":[{"type":"text","text":content,"annotations":{"audience":["assistant"]}}],"structuredContent":structured}),
    )
}

fn materialize_output(
    context: &Context,
    tool: &str,
    value: Value,
    full_text: &str,
) -> Result<Value, String> {
    use sha2::{Digest, Sha256};
    let output_id = format!(
        "o_{}",
        Uuid::new_v4()
            .simple()
            .to_string()
            .chars()
            .take(24)
            .collect::<String>()
    );
    let reference = format!("mcp_output:{output_id}");
    let created_at = timestamp();
    let record = json!({"schema":"narada.mcp_output_ref.v1","ref":reference,"output_id":output_id,"tool_name":tool,"created_at":created_at,"created_by":env::var("NARADA_AGENT_ID").ok(),"content_type":"application/json","inline_char_limit":6000,"full_output_char_length":full_text.chars().count(),"truncated":true,"sha256":format!("{:x}",Sha256::digest(stable_json(&value).as_bytes())),"max_bytes":10*1024*1024,"full_output":value});
    let serialized = format!(
        "{}\n",
        serde_json::to_string(&record).map_err(|e| e.to_string())?
    );
    if serialized.len() > 10 * 1024 * 1024 {
        return Err(format!(
            "mcp_output_too_large: {} > {}",
            serialized.len(),
            10 * 1024 * 1024
        ));
    }
    let directory = context.site_root.join(".ai/tmp/mcp-outputs/workspace");
    fs::create_dir_all(&directory).map_err(|e| format!("mcp_output_write_failed:{e}"))?;
    fs::write(directory.join(format!("{output_id}.json")), serialized)
        .map_err(|e| format!("mcp_output_write_failed:{e}"))?;
    let status = record["full_output"]
        .get("status")
        .and_then(Value::as_str)
        .filter(|v| v.len() <= 32)
        .unwrap_or("ok");
    let mut preview = take_chars(full_text, 6000);
    loop {
        let next = if preview.chars().count() < full_text.chars().count() {
            Some(preview.chars().count())
        } else {
            None
        };
        let envelope = json!({"schema":"narada.producer_output_page.v1","status":status,"truncated":true,"output_ref":reference,"ref":reference,"result_materialized":true,"tool_name":tool,"offset":0,"limit":6000,"next_offset":next,"transport_offset":0,"transport_limit":6000,"transport_next_offset":next,"output_text":preview,"output_truncated":next.is_some(),"reader_tool":"mcp_output_show","site_root":path_text(&context.site_root),"read_command":format!("mcp_output_show({{ \"ref\": \"{reference}\", \"offset\": 0, \"limit\": 10000 }})"),"remediation":format!("Use mcp_output_show with output_ref/ref={reference} to read the bounded produced JSON pages; continue with the returned next_offset."),"inline_limit":6000,"full_output_char_length":full_text.chars().count()});
        let compact = serde_json::to_string(&envelope).map_err(|e| e.to_string())?;
        if compact.chars().count() <= 6000
            && compact.len() + serde_json::to_vec(&envelope).unwrap().len() <= 32768
        {
            return Ok(envelope);
        }
        let next_len = ((preview.chars().count() as f64) * 0.75).floor() as usize;
        if next_len == 0 {
            return Err("inline_output_envelope_limit_too_small".into());
        }
        preview = take_chars(full_text, next_len)
    }
}

fn output_show(context: &Context, args: &Value) -> Result<Value, String> {
    let reference = args
        .get("ref")
        .or_else(|| args.get("output_ref"))
        .and_then(Value::as_str)
        .filter(|v| !v.trim().is_empty())
        .ok_or("output_show_requires_ref")?;
    let output_id = reference
        .strip_prefix("mcp_output:")
        .filter(|v| {
            v.len() >= 3
                && v.chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
        })
        .ok_or_else(|| format!("output_ref_invalid: {reference}"))?;
    let path = context
        .site_root
        .join(format!(".ai/tmp/mcp-outputs/workspace/{output_id}.json"));
    let metadata =
        fs::symlink_metadata(&path).map_err(|_| format!("output_ref_not_found: {reference}"))?;
    if metadata.file_type().is_symlink() {
        return Err(format!("output_ref_symlink_refused: {reference}"));
    }
    if metadata.len() > 10 * 1024 * 1024 {
        return Err(format!("output_ref_too_large: {reference}"));
    }
    let bytes = fs::read(&path).map_err(|_| format!("output_ref_not_found: {reference}"))?;
    let record: Value =
        serde_json::from_slice(&bytes).map_err(|e| format!("output_ref_invalid_json: {e}"))?;
    if record.get("schema").and_then(Value::as_str) != Some("narada.mcp_output_ref.v1")
        || record.get("ref").and_then(Value::as_str) != Some(reference)
    {
        return Err(format!("output_ref_metadata_mismatch: {reference}"));
    }
    let full = serde_json::to_string_pretty(&record["full_output"]).map_err(|e| e.to_string())?;
    let offset = args.get("offset").and_then(Value::as_u64).unwrap_or(0) as usize;
    let limit = args
        .get("limit")
        .or_else(|| args.get("output_limit"))
        .and_then(Value::as_u64)
        .unwrap_or(10000) as usize;
    if limit == 0 {
        return Err("output_limit_must_be_positive_integer".into());
    }
    if limit > 20000 {
        return Err(format!(
            "output_limit_exceeds_transport_maximum: {limit} > 20000"
        ));
    }
    let total = full.chars().count();
    let chunk = take_chars_from(&full, offset, limit);
    let end = (offset + chunk.chars().count()).min(total);
    Ok(
        json!({"schema":"narada.mcp_output_page.v1","status":"ok","ref":reference,"tool_name":record["tool_name"],"full_output_char_length":total,"byte_size":bytes.len(),"original_truncated":true,"path":format!(".ai/tmp/mcp-outputs/workspace/{output_id}.json"),"offset":offset,"limit":limit,"next_offset":if end<total{Some(end)}else{None},"output_limit":limit,"output_truncated":end<total,"output_text":chunk}),
    )
}

fn take_chars(value: &str, count: usize) -> String {
    value.chars().take(count).collect()
}
fn take_chars_from(value: &str, offset: usize, count: usize) -> String {
    value.chars().skip(offset).take(count).collect()
}
