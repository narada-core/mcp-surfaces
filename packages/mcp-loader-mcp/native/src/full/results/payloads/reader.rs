use crate::full::*;

pub(crate) fn read_loader_result(
    arguments: &JsonObject,
    state: &LoaderState,
) -> Result<Value, Diagnostic> {
    let connection = get_connection(arguments, state)?;
    let reference = value_string(arguments.get("ref"))
        .or_else(|| value_string(arguments.get("output_ref")))
        .ok_or_else(|| Diagnostic::new("missing_output_ref", "missing_output_ref"))?;
    if !reference.starts_with("mcp_output:") {
        return Err(Diagnostic::new(
            "output_ref_invalid",
            format!("output_ref_invalid:{}", reference),
        ));
    }
    let id = reference.trim_start_matches("mcp_output:");
    if id.is_empty() || id.contains('/') || id.contains('\\') {
        return Err(Diagnostic::new(
            "output_ref_invalid",
            format!("output_ref_invalid:{}", reference),
        ));
    }
    let path = join_path(&output_root(&connection.site_root), &format!("{}.json", id));
    let bytes = fs::read(&path).map_err(|_| {
        Diagnostic::new(
            "output_ref_not_found",
            format!("output_ref_not_found:{}", reference),
        )
    })?;
    if bytes.len() > state.policy.max_response_bytes {
        return Err(Diagnostic::new(
            "output_ref_too_large",
            format!("output_ref_too_large:{}", reference),
        ));
    }
    let record: Value = serde_json::from_slice(&bytes)
        .map_err(|error| Diagnostic::new("output_ref_invalid_json", error.to_string()))?;
    if record.get("schema").and_then(Value::as_str) != Some("narada.mcp_output_ref.v1")
        || record.get("ref").and_then(Value::as_str) != Some(reference.as_str())
    {
        return Err(Diagnostic::new(
            "output_ref_metadata_mismatch",
            format!("output_ref_metadata_mismatch:{}", reference),
        ));
    }
    let full_output = record.get("full_output").cloned().unwrap_or(Value::Null);
    let full_text = pretty_json(&full_output);
    if record
        .get("full_output_char_length")
        .and_then(Value::as_u64)
        .map(usize::try_from)
        .transpose()
        .ok()
        .flatten()
        != Some(utf16_len(&full_text))
    {
        return Err(Diagnostic::new(
            "output_ref_length_mismatch",
            format!("output_ref_length_mismatch:{}", reference),
        ));
    }
    if record.get("sha256").and_then(Value::as_str)
        != Some(sha256(&stable_json(&full_output)).as_str())
    {
        return Err(Diagnostic::new(
            "output_ref_sha256_mismatch",
            format!("output_ref_sha256_mismatch:{}", reference),
        ));
    }
    let offset = arguments.get("offset").and_then(Value::as_u64).unwrap_or(0) as usize;
    let limit = arguments
        .get("limit")
        .and_then(Value::as_u64)
        .unwrap_or(DEFAULT_OUTPUT_SHOW_CHAR_LIMIT as u64) as usize;
    if limit == 0 || limit > MAX_OUTPUT_SHOW_CHAR_LIMIT {
        return Err(Diagnostic::new(
            "output_limit_exceeds_transport_maximum",
            format!("output_limit_exceeds_transport_maximum:{}", limit),
        ));
    }
    let (chunk, end) = bounded_page(&full_text, offset, limit, MAX_OUTPUT_PAGE_BYTES);
    let page = json!({
        "schema":"narada.mcp_output_page.v1","status":"ok","ref":reference,
        "tool_name":record.get("tool_name").cloned().unwrap_or(Value::Null),
        "full_output_char_length":utf16_len(&full_text),"byte_size":bytes.len(),"original_truncated":true,
        "path":format!(".ai/tmp/mcp-outputs/workspace/{}.json",id),
        "offset":offset,"limit":limit,"next_offset":if end < full_text.chars().count() {Value::from(end as u64)} else {Value::Null},
        "output_limit":limit,"output_truncated":end < full_text.chars().count(),"output_text":chunk
    });
    Ok(
        json!({"schema":"narada.mcp_loader.result_page.v1","connection_id":connection.connection_id,"surface_id":connection.surface_id,"result":page}),
    )
}

pub(crate) fn payload_observation(
    site_root: &str,
    observation: &Value,
    state: &LoaderState,
) -> Result<(String, String, usize, Value), Diagnostic> {
    let id = format!(
        "{}{}",
        SITE_TOOL_OBSERVATION_PAYLOAD_PREFIX,
        output_id().trim_start_matches("o_")
    );
    let reference = format!("mcp_payload:{}@v1", id);
    let payload_json = stable_json(observation);
    let payload_size = payload_json.len();
    if payload_size > state.policy.max_response_bytes {
        return Err(Diagnostic::new(
            "payload_too_large",
            format!("payload_too_large:{}", payload_size),
        ));
    }
    let record = json!({
        "schema":"narada.mcp_payload.revision.v1","ref":reference,"payload_id":id,"revision":1,
        "created_at":now_iso(),"created_by":SERVER_NAME,"source":{"kind":"create"},
        "sha256":sha256(&payload_json),"byte_size":payload_size,"max_bytes":state.policy.max_response_bytes,
        "transient_not_authority":true,"immutable_revision":true,"payload":observation
    });
    let path = join_path(
        &join_path(site_root, ".ai/tmp/mcp-payloads/workspace"),
        &format!("{}/v1.json", id),
    );
    let serialized = format!("{}\n", stable_json(&record));
    let written = write_immutable(&path, &serialized)?;
    if !written {
        let existing = read_to_string(&path)
            .map_err(|error| Diagnostic::new("payload_revision_conflict", error.to_string()))?;
        let existing_value: Value = serde_json::from_str(&existing)
            .map_err(|error| Diagnostic::new("payload_revision_conflict", error.to_string()))?;
        if existing_value.get("sha256") != record.get("sha256") {
            return Err(Diagnostic::new("payload_revision_conflict", reference));
        }
    }
    let retention = prune_payloads(site_root)?;
    Ok((reference, sha256(&payload_json), payload_size, retention))
}

pub(crate) fn prune_payloads(site_root: &str) -> Result<Value, Diagnostic> {
    let root = payload_root(site_root);
    if !Path::new(&root).exists() {
        return Ok(
            json!({"status":"ok","payload_id_prefix":SITE_TOOL_OBSERVATION_PAYLOAD_PREFIX,"max_entries":SITE_TOOL_OBSERVATION_MAX_ENTRIES,"max_age_ms":SITE_TOOL_OBSERVATION_MAX_AGE_MS,"considered_count":0,"retained_count":0,"removed_count":0,"retained_payload_ids":[],"removed_payload_ids":[]}),
        );
    }
    let mut entries: Vec<(String, u128, String)> = read_dir(&root)
        .ok()
        .into_iter()
        .flatten()
        .filter_map(|entry| {
            let entry = entry.ok()?;
            let name = entry.file_name().to_string_lossy().to_string();
            if !name.starts_with(SITE_TOOL_OBSERVATION_PAYLOAD_PREFIX) || !entry.path().is_dir() {
                return None;
            }
            let modified = entry
                .metadata()
                .ok()?
                .modified()
                .ok()?
                .duration_since(UNIX_EPOCH)
                .ok()?
                .as_millis();
            Some((name, modified, entry.path().to_string_lossy().to_string()))
        })
        .collect();
    entries.sort_by(|left, right| right.1.cmp(&left.1).then_with(|| right.0.cmp(&left.0)));
    let now = now_ms();
    let mut retained = Vec::new();
    let mut removed = Vec::new();
    for (index, (name, modified, path)) in entries.iter().enumerate() {
        if index >= SITE_TOOL_OBSERVATION_MAX_ENTRIES
            || now.saturating_sub(*modified) > SITE_TOOL_OBSERVATION_MAX_AGE_MS
        {
            remove_dir_all(path).ok();
            removed.push(name.clone());
        } else {
            retained.push(name.clone());
        }
    }
    Ok(
        json!({"status":"ok","payload_id_prefix":SITE_TOOL_OBSERVATION_PAYLOAD_PREFIX,"max_entries":SITE_TOOL_OBSERVATION_MAX_ENTRIES,"max_age_ms":SITE_TOOL_OBSERVATION_MAX_AGE_MS,"considered_count":entries.len(),"retained_count":retained.len(),"removed_count":removed.len(),"retained_payload_ids":retained,"removed_payload_ids":removed}),
    )
}
