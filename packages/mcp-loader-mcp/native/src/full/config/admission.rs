use crate::full::*;

pub(crate) fn load_binding_admission(options: &Options) -> Result<Option<Value>, Diagnostic> {
    let required = env::var("NARADA_MCP_BINDING_ADMISSION_REQUIRED")
        .ok()
        .as_deref()
        == Some("1");
    let path = options
        .binding_admission_path
        .clone()
        .or_else(|| env::var("NARADA_MCP_BINDING_ADMISSION_PATH").ok())
        .filter(|value| !value.trim().is_empty());
    let Some(path) = path else {
        if required {
            return Err(Diagnostic::new(
                "mcp_binding_admission_required",
                "mcp_binding_admission_required",
            ));
        }
        return Ok(None);
    };
    let text = read_to_string(&path).map_err(|error| {
        Diagnostic::new(
            "mcp_binding_admission_unreadable",
            format!("mcp_binding_admission_unreadable:{error}"),
        )
    })?;
    let envelope: Value = serde_json::from_str(&text).map_err(|error| {
        Diagnostic::new(
            "mcp_binding_admission_invalid",
            format!("mcp_binding_admission_invalid:{error}"),
        )
    })?;
    if envelope.get("schema").and_then(Value::as_str)
        != Some("narada.mcp.binding_admission_envelope.v1")
    {
        return Err(Diagnostic::new(
            "mcp_binding_admission_schema_invalid",
            "mcp_binding_admission_schema_invalid",
        ));
    }
    let digest = envelope
        .get("envelope_digest")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let mut unsigned = envelope.clone();
    if let Some(object) = unsigned.as_object_mut() {
        object.remove("envelope_digest");
    }
    if sha256(&stable_json(&unsigned)) != digest {
        return Err(Diagnostic::new(
            "mcp_binding_admission_envelope_digest_mismatch",
            "mcp_binding_admission_envelope_digest_mismatch",
        ));
    }
    let expected_digest = options
        .binding_admission_digest
        .clone()
        .or_else(|| env::var("NARADA_MCP_BINDING_ADMISSION_DIGEST").ok());
    if expected_digest
        .as_deref()
        .is_some_and(|expected| expected != digest)
    {
        return Err(Diagnostic::new(
            "mcp_binding_admission_digest_mismatch",
            "mcp_binding_admission_digest_mismatch",
        ));
    }
    let expected_session = env::var("NARADA_NARS_SESSION_ID")
        .or_else(|_| env::var("NARADA_RUNTIME_SESSION_ID"))
        .or_else(|_| env::var("NARADA_CARRIER_SESSION_ID"))
        .ok();
    if expected_session.as_deref().is_some_and(|expected| {
        envelope.get("carrier_session_id").and_then(Value::as_str) != Some(expected)
    }) {
        return Err(Diagnostic::new(
            "mcp_binding_admission_session_mismatch",
            "mcp_binding_admission_session_mismatch",
        ));
    }
    if let Ok(expected) = env::var("NARADA_SESSION_AUTHORITY_PRINCIPAL_KEY") {
        if envelope.get("principal_key").and_then(Value::as_str) != Some(expected.as_str()) {
            return Err(Diagnostic::new(
                "mcp_binding_admission_principal_mismatch",
                "mcp_binding_admission_principal_mismatch",
            ));
        }
    }
    if let Some(expected) = env::var("NARADA_SESSION_AUTHORITY_EPOCH")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
    {
        if envelope.get("authority_epoch").and_then(Value::as_u64) != Some(expected) {
            return Err(Diagnostic::new(
                "mcp_binding_admission_epoch_mismatch",
                "mcp_binding_admission_epoch_mismatch",
            ));
        }
    }
    let now = OffsetDateTime::now_utc();
    if envelope
        .get("issued_at")
        .and_then(Value::as_str)
        .and_then(|value| OffsetDateTime::parse(value, &Rfc3339).ok())
        .is_some_and(|issued| issued > now)
    {
        return Err(Diagnostic::new(
            "mcp_binding_admission_not_yet_issued",
            "mcp_binding_admission_not_yet_issued",
        ));
    }
    if envelope
        .get("valid_until")
        .and_then(Value::as_str)
        .and_then(|value| OffsetDateTime::parse(value, &Rfc3339).ok())
        .is_some_and(|expiry| expiry <= now)
    {
        return Err(Diagnostic::new(
            "mcp_binding_admission_expired",
            "mcp_binding_admission_expired",
        ));
    }
    Ok(Some(envelope))
}
