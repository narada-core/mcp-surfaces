use crate::full::*;

pub(crate) fn output_root(site_root: &str) -> String {
    join_path(site_root, ".ai/tmp/mcp-outputs/workspace")
}

pub(crate) fn payload_root(site_root: &str) -> String {
    join_path(site_root, ".ai/tmp/mcp-payloads/workspace")
}

pub(crate) fn payload_revision_location(
    site_root: &str,
    reference: &str,
) -> Result<String, Diagnostic> {
    let body = reference.strip_prefix("mcp_payload:").ok_or_else(|| {
        Diagnostic::new(
            "payload_ref_invalid",
            "payload_ref must use mcp_payload:<id>@v<revision>",
        )
    })?;
    let (payload_id, revision_text) = body.rsplit_once("@v").ok_or_else(|| {
        Diagnostic::new(
            "payload_ref_invalid",
            "payload_ref must include an immutable revision",
        )
    })?;
    if !(3..=64).contains(&payload_id.len())
        || !payload_id
            .chars()
            .next()
            .is_some_and(|ch| ch.is_ascii_alphanumeric())
        || !payload_id
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_'))
    {
        return Err(Diagnostic::new(
            "payload_ref_invalid",
            "payload_ref id is invalid",
        ));
    }
    let revision = revision_text
        .parse::<u64>()
        .ok()
        .filter(|value| *value > 0)
        .ok_or_else(|| {
            Diagnostic::new(
                "payload_ref_invalid",
                "payload_ref revision must be a positive integer",
            )
        })?;
    Ok(join_path(
        &payload_root(site_root),
        &format!("{payload_id}/v{revision}.json"),
    ))
}

pub(crate) fn verified_payload_revision(
    path: &str,
    reference: &str,
) -> Result<(String, Value), Diagnostic> {
    const MAX_PAYLOAD_BYTES: u64 = 256 * 1024;
    let file_metadata = metadata(path).map_err(|error| {
        Diagnostic::new("payload_ref_read_failed", error.to_string())
            .with_details(json!({"payload_ref":reference,"path":path}))
    })?;
    if file_metadata.len() > MAX_PAYLOAD_BYTES {
        return Err(Diagnostic::new("payload_ref_too_large", "immutable payload revision exceeds the transport ceiling")
            .with_details(json!({"payload_ref":reference,"path":path,"byte_size":file_metadata.len(),"max_bytes":MAX_PAYLOAD_BYTES})));
    }
    let serialized = read_to_string(path).map_err(|error| {
        Diagnostic::new("payload_ref_read_failed", error.to_string())
            .with_details(json!({"payload_ref":reference,"path":path}))
    })?;
    let record: Value = serde_json::from_str(&serialized).map_err(|error| {
        Diagnostic::new("payload_ref_invalid_json", error.to_string())
            .with_details(json!({"payload_ref":reference,"path":path}))
    })?;
    let body = reference.trim_start_matches("mcp_payload:");
    let (payload_id, revision_text) = body.rsplit_once("@v").unwrap_or_default();
    let revision = revision_text.parse::<u64>().unwrap_or_default();
    if record.get("schema").and_then(Value::as_str) != Some("narada.mcp_payload.revision.v1")
        || record.get("ref").and_then(Value::as_str) != Some(reference)
        || record.get("payload_id").and_then(Value::as_str) != Some(payload_id)
        || record.get("revision").and_then(Value::as_u64) != Some(revision)
    {
        return Err(Diagnostic::new(
            "payload_ref_metadata_mismatch",
            "immutable payload metadata does not match its reference",
        )
        .with_details(json!({"payload_ref":reference,"path":path})));
    }
    let payload = record.get("payload").ok_or_else(|| {
        Diagnostic::new(
            "payload_ref_payload_missing",
            "immutable payload record has no payload",
        )
        .with_details(json!({"payload_ref":reference,"path":path}))
    })?;
    let canonical = stable_json(payload);
    if record.get("byte_size").and_then(Value::as_u64) != Some(canonical.len() as u64) {
        return Err(Diagnostic::new(
            "payload_ref_byte_size_mismatch",
            "immutable payload byte size verification failed",
        )
        .with_details(json!({"payload_ref":reference,"path":path})));
    }
    if record.get("sha256").and_then(Value::as_str) != Some(sha256(&canonical).as_str()) {
        return Err(Diagnostic::new(
            "payload_ref_sha256_mismatch",
            "immutable payload digest verification failed",
        )
        .with_details(json!({"payload_ref":reference,"path":path})));
    }
    Ok((serialized, record))
}

pub(crate) fn stage_admitted_payload_ref(
    target_site_root: &str,
    arguments: &Value,
    policy: &Policy,
) -> Result<Option<Value>, Diagnostic> {
    let Some(reference) = arguments
        .as_object()
        .and_then(|object| object.get("payload_ref"))
        .and_then(Value::as_str)
    else {
        return Ok(None);
    };
    let target_path = payload_revision_location(target_site_root, reference)?;
    if Path::new(&target_path).is_file() {
        verified_payload_revision(&target_path, reference)?;
        return Ok(Some(
            json!({"status":"target_local","payload_ref":reference,"target_site_root":target_site_root}),
        ));
    }
    let mut matches: Vec<(String, String, Value)> = Vec::new();
    for root in &policy.allowed_site_roots {
        let source_path = payload_revision_location(root, reference)?;
        if Path::new(&source_path).is_file() {
            let (serialized, record) = verified_payload_revision(&source_path, reference)?;
            matches.push((root.clone(), serialized, record));
        }
    }
    if matches.is_empty() {
        return Err(Diagnostic::new("payload_ref_not_found", "immutable payload revision was not found in any admitted Site")
            .with_details(json!({"payload_ref":reference,"target_site_root":target_site_root,"searched_site_roots":policy.allowed_site_roots})));
    }
    let canonical_record = stable_json(&matches[0].2);
    if matches
        .iter()
        .skip(1)
        .any(|(_, _, record)| stable_json(record) != canonical_record)
    {
        return Err(Diagnostic::new("payload_ref_admitted_site_collision", "admitted Sites contain divergent immutable revisions for the same payload_ref")
            .with_details(json!({"payload_ref":reference,"source_site_roots":matches.iter().map(|item| item.0.clone()).collect::<Vec<_>>()})));
    }
    let written = write_immutable(&target_path, &matches[0].1)?;
    if !written {
        let (_, target_record) = verified_payload_revision(&target_path, reference)?;
        if stable_json(&target_record) != canonical_record {
            return Err(Diagnostic::new(
                "payload_ref_target_collision",
                "target Site acquired a divergent immutable revision while payload transport was staging",
            )
            .with_details(json!({
                "payload_ref":reference,"target_site_root":target_site_root
            })));
        }
    }
    Ok(Some(
        json!({"status":"staged_from_admitted_site","payload_ref":reference,"source_site_root":matches[0].0,"target_site_root":target_site_root,"identical_source_count":matches.len()}),
    ))
}

pub(crate) fn write_immutable(path: &str, content: &str) -> Result<bool, Diagnostic> {
    if let Some(parent) = Path::new(path).parent() {
        create_dir_all(parent)
            .map_err(|error| Diagnostic::new("output_directory_failed", error.to_string()))?;
    }
    match OpenOptions::new().write(true).create_new(true).open(path) {
        Ok(mut file) => {
            file.write_all(content.as_bytes())
                .map_err(|error| Diagnostic::new("output_write_failed", error.to_string()))?;
            file.sync_all().ok();
            Ok(true)
        }
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => Ok(false),
        Err(error) => Err(Diagnostic::new("output_write_failed", error.to_string())),
    }
}
