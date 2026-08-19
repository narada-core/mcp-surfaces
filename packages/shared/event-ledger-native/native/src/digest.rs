//! Digest, naming, and timestamp helpers.
//!
//! `digest_value` is the load-bearing convention: hex SHA-256 of
//! `serde_json::to_vec(value)` with `preserve_order`, so the digest depends on
//! JSON object key insertion order. This is deliberate (see
//! `docs/event-ledger-format.md`); changing it invalidates every existing
//! ledger hash.

use serde_json::Value;
use sha2::{Digest, Sha256};
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;

use crate::error::ErrorSchema;

/// Hex-encoded SHA-256 of raw bytes.
pub fn sha256(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

/// Hex-encoded SHA-256 of the compact `serde_json` encoding of a value.
/// Insertion-order dependent by design.
pub fn digest_value(schema: ErrorSchema, value: &Value) -> Result<String, Value> {
    let encoded = serde_json::to_vec(value)
        .map_err(|source| schema.error("json_encode_failed", &source.to_string(), Value::Null))?;
    Ok(sha256(&encoded))
}

/// Filesystem-safe name fragment: ASCII alphanumerics, `-`, and `_`, bounded
/// to 120 characters.
pub fn safe_name(value: &str) -> String {
    value
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
        .take(120)
        .collect()
}

/// Current UTC time formatted as RFC 3339.
pub fn now() -> String {
    OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .unwrap_or_default()
}
