impl LifecycleServer {
    fn output_show(&self, args: Value) -> Result<Value, String> {
        let reference = string_arg(&args, "ref")
            .or_else(|| string_arg(&args, "output_ref"))
            .ok_or("output_ref_required")?;
        let id = safe_reference_id(&reference, "mcp_output:")?;
        let candidates = [
            self.options.site_root.join(".ai").join("tmp").join("mcp-outputs").join("workspace").join(format!("{id}.json")),
            self.options
                .site_root
                .join(".ai")
                .join("mcp-outputs")
                .join(format!("{id}.txt")),
            self.options
                .site_root
                .join(".ai")
                .join("outputs")
                .join(format!("{id}.txt")),
            self.options
                .site_root
                .join(".ai")
                .join("mcp-outputs")
                .join(format!("{id}.json")),
        ];
        let path = candidates
            .iter()
            .find(|p| p.exists())
            .ok_or_else(|| format!("output_ref_not_found: {reference}"))?;
        let stored = fs::read_to_string(path).map_err(|e| format!("output_read_failed:{e}"))?;
        let text = serde_json::from_str::<Value>(&stored).ok()
            .filter(|record| record.get("schema").and_then(Value::as_str)==Some("narada.mcp_output_ref.v1"))
            .and_then(|record| record.get("full_output").cloned())
            .and_then(|output| serde_json::to_string_pretty(&output).ok())
            .unwrap_or(stored);
        let chars: Vec<char> = text.chars().take(4_000_000).collect();
        let offset = args.get("offset").and_then(Value::as_u64).unwrap_or(0) as usize;
        let limit = args.get("limit").and_then(Value::as_u64).unwrap_or(4_000) as usize;
        let offset = offset.min(chars.len());
        let end = (offset.saturating_add(limit)).min(chars.len());
        let output: String = chars[offset..end].iter().collect();
        Ok(
            json!({"schema":"narada.producer_output_page.v1","status":"ok","ref":reference,"output_ref":reference,"offset":offset,"limit":limit,"next_offset":if end<chars.len(){json!(end)}else{Value::Null},"output_text":output,"output_truncated":end<chars.len(),"full_output_char_length":chars.len()}),
        )
    }

    fn resources_list(&self, params: &Value) -> Result<Value, String> {
        let (offset, limit) = resource_page(params)?;
        let dir = self
            .options
            .site_root
            .join(".ai")
            .join("tmp")
            .join("mcp-outputs")
            .join("workspace");
        let mut ids = Vec::new();
        if dir.is_dir() {
            for entry in fs::read_dir(&dir)
                .map_err(|e| format!("output_resource_directory_read_failed:{e}"))?
                .take(10_000)
            {
                let entry =
                    entry.map_err(|e| format!("output_resource_directory_read_failed:{e}"))?;
                let path = entry.path();
                if !path.is_file() || path.extension().and_then(|v| v.to_str()) != Some("json") {
                    continue;
                }
                let Some(stem) = path.file_stem().and_then(|v| v.to_str()) else {
                    continue;
                };
                if valid_output_id(stem) {
                    ids.push(stem.to_string());
                }
            }
        }
        ids.sort();
        let start = offset.min(ids.len());
        let end = offset.saturating_add(limit).min(ids.len());
        let resources = ids[start..end]
            .iter()
            .map(|id| {
                let reference = format!("mcp_output:{id}");
                json!({
                    "uri": format!("mcp-output:{}", percent_encode(&reference)),
                    "name": reference,
                    "title": reference,
                    "description": "Materialized MCP output ref.",
                    "mimeType": "application/json"
                })
            })
            .collect::<Vec<_>>();
        let next = if end < ids.len() { Some(end) } else { None };
        Ok(json!({
            "resources": resources,
            "offset": offset,
            "limit": limit,
            "next_offset": next,
            "nextCursor": next.map(|value| value.to_string()),
            "has_more": next.is_some()
        }))
    }

    fn resources_read(&self, params: &Value) -> Result<Value, String> {
        let uri = required_string(params, "uri")?;
        let encoded = uri
            .strip_prefix("mcp-output:")
            .ok_or_else(|| format!("output_resource_uri_invalid: {uri}"))?;
        let reference = percent_decode(encoded)?;
        let id = output_id_from_reference(&reference)?;
        let new_path = self
            .options
            .site_root
            .join(".ai")
            .join("tmp")
            .join("mcp-outputs")
            .join("workspace")
            .join(format!("{id}.json"));
        let legacy_path = self
            .options
            .site_root
            .join(".ai")
            .join("mcp-outputs")
            .join(format!("{id}.json"));
        let path = if new_path.is_file() {
            new_path
        } else {
            legacy_path
        };
        if !path.is_file() {
            return Err(format!("output_ref_not_found: {reference}"));
        }
        let metadata = fs::metadata(&path).map_err(|e| format!("output_ref_stat_failed:{e}"))?;
        if metadata.len() > 10 * 1024 * 1024 {
            return Err(format!(
                "output_ref_too_large: {} > {}",
                metadata.len(),
                10 * 1024 * 1024
            ));
        }
        let text = fs::read_to_string(&path).map_err(|e| format!("output_ref_read_failed:{e}"))?;
        let record: Value =
            serde_json::from_str(&text).map_err(|e| format!("output_ref_invalid_json: {e}"))?;
        let object = record
            .as_object()
            .ok_or_else(|| format!("output_ref_record_must_be_object: {reference}"))?;
        if object.get("schema").and_then(Value::as_str) != Some("narada.mcp_output_ref.v1") {
            return Err(format!(
                "output_ref_schema_unsupported: {}",
                object.get("schema").cloned().unwrap_or(Value::Null)
            ));
        }
        if object.get("ref").and_then(Value::as_str) != Some(reference.as_str())
            || object.get("output_id").and_then(Value::as_str) != Some(id.as_str())
        {
            return Err(format!("output_ref_metadata_mismatch: {reference}"));
        }
        let full_output = object
            .get("full_output")
            .ok_or_else(|| format!("output_ref_full_output_missing: {reference}"))?;
        let output_text = serde_json::to_string_pretty(full_output)
            .map_err(|e| format!("output_ref_presentation_failed: {e}"))?;
        let expected_length = utf16_len(&output_text);
        if object
            .get("full_output_char_length")
            .and_then(Value::as_u64)
            != Some(expected_length as u64)
        {
            return Err(format!("output_ref_length_mismatch: {reference}"));
        }
        if object.get("sha256").and_then(Value::as_str)
            != Some(native_canonical_digest(full_output).as_str())
        {
            return Err(format!("output_ref_sha256_mismatch: {reference}"));
        }
        let limit = 10_000usize;
        let page_end = output_text
            .char_indices()
            .nth(limit)
            .map(|(index, _)| index)
            .unwrap_or(output_text.len());
        let chunk = output_text[..page_end].to_string();
        let output_truncated = page_end < output_text.len();
        let relative_path = path
            .strip_prefix(&self.options.site_root)
            .unwrap_or(&path)
            .to_string_lossy()
            .replace('\\', "/");
        let page = json!({
            "schema": "narada.mcp_output_page.v1",
            "status": "ok",
            "ref": reference,
            "tool_name": object.get("tool_name").cloned().unwrap_or(Value::Null),
            "full_output_char_length": json!(expected_length),
            "byte_size": metadata.len(),
            "original_truncated": object.get("truncated").and_then(Value::as_bool).unwrap_or(false),
            "path": relative_path,
            "offset": 0,
            "limit": limit,
            "next_offset": if output_truncated { json!(page_end) } else { Value::Null },
            "output_limit": limit,
            "output_truncated": output_truncated,
            "output_text": chunk
        });
        Ok(json!({
            "contents": [{
                "uri": uri,
                "mimeType": "application/json",
                "text": serde_json::to_string_pretty(&page).map_err(|e| format!("output_resource_serialize_failed: {e}"))?
            }]
        }))
    }
}
