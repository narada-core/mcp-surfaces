//! Hash-chain verification and idempotency-index helpers shared by the event
//! ledger and auxiliary authorities (for example sequence-claim chains).
//! Everything is parameterized by hash-field name so chains using
//! `event_hash`, `claim_hash`, or `creation_hash` share one algorithm.

use serde_json::{json, Map, Value};
use std::path::PathBuf;

use crate::digest;
use crate::error::ErrorSchema;
use crate::io;

/// (code, message) pair for one refusal class.
#[derive(Clone, Debug)]
pub struct Refusal {
    pub code: String,
    pub message: String,
}

impl Refusal {
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
        }
    }
}

/// Verification shape of one hash chain over sorted record files: a
/// contiguous ordinal field, a previous-hash link field, and a recomputed
/// hash field.
#[derive(Clone, Debug)]
pub struct ChainSpec {
    pub hash_field: &'static str,
    pub previous_hash_field: &'static str,
    pub ordinal_field: &'static str,
    pub ordinal_start: u64,
    pub missing_hash: Refusal,
    pub ordinal_invalid: Refusal,
    pub link_invalid: Refusal,
    pub hash_invalid: Refusal,
}

/// Verify a hash chain over already-sorted record files and return the
/// verified records in chain order. Checks, per record, in order: contiguous
/// ordinal starting at `spec.ordinal_start`, previous-hash link equality,
/// presence of the hash field, and exact hash recomputation.
pub fn verify_files(
    schema: ErrorSchema,
    files: &[PathBuf],
    spec: &ChainSpec,
) -> Result<Vec<Value>, Value> {
    let mut expected_previous: Option<String> = None;
    let mut expected_ordinal = spec.ordinal_start;
    let mut items = Vec::with_capacity(files.len());
    for path in files {
        let item = io::read_json(schema, path)?;
        if item.get(spec.ordinal_field).and_then(Value::as_u64) != Some(expected_ordinal) {
            let mut details = Map::new();
            details.insert("path".into(), json!(path.to_string_lossy()));
            details.insert(
                format!("expected_{}", spec.ordinal_field),
                json!(expected_ordinal),
            );
            details.insert(
                format!("actual_{}", spec.ordinal_field),
                json!(item.get(spec.ordinal_field)),
            );
            return Err(schema.error(
                &spec.ordinal_invalid.code,
                &spec.ordinal_invalid.message,
                Value::Object(details),
            ));
        }
        if item.get(spec.previous_hash_field).and_then(Value::as_str)
            != expected_previous.as_deref()
        {
            return Err(schema.error(
                &spec.link_invalid.code,
                &spec.link_invalid.message,
                json!({"path":path.to_string_lossy(),"expected_previous":expected_previous,"actual_previous":item.get(spec.previous_hash_field)}),
            ));
        }
        let Some(recomputed) = recompute_hash(schema, &item, spec.hash_field)? else {
            return Err(schema.error(
                &spec.missing_hash.code,
                &spec.missing_hash.message,
                json!({"path":path.to_string_lossy()}),
            ));
        };
        let RecomputedHash { stored, computed } = recomputed;
        if stored != computed {
            return Err(schema.error(
                &spec.hash_invalid.code,
                &spec.hash_invalid.message,
                json!({"path":path.to_string_lossy(),"expected_hash":computed,"actual_hash":stored}),
            ));
        }
        expected_previous = Some(stored);
        expected_ordinal += 1;
        items.push(item);
    }
    Ok(items)
}

/// Stored and recomputed hash of one chain record.
#[derive(Clone, Debug)]
pub struct RecomputedHash {
    pub stored: String,
    pub computed: String,
}

/// Recompute the digest of one record after removing its hash field
/// (clone, remove `hash_field`, digest the rest). Returns `Ok(None)` when the
/// record lacks the hash field; the caller decides the missing-hash refusal.
pub fn recompute_hash(
    schema: ErrorSchema,
    item: &Value,
    hash_field: &str,
) -> Result<Option<RecomputedHash>, Value> {
    let Some(stored) = item.get(hash_field).and_then(Value::as_str) else {
        return Ok(None);
    };
    let mut unhashed = item.clone();
    unhashed.as_object_mut().unwrap().remove(hash_field);
    let computed = digest::digest_value(schema, &unhashed)?;
    Ok(Some(RecomputedHash {
        stored: stored.to_string(),
        computed,
    }))
}

/// Scan chain files for the first record carrying `idempotency_key == key`.
/// This is the recovery path for the disposable idempotency-marker index.
pub fn find_by_idempotency(
    schema: ErrorSchema,
    files: &[PathBuf],
    key: &str,
) -> Result<Option<Value>, Value> {
    for path in files {
        let item = io::read_json(schema, path)?;
        if item.get("idempotency_key").and_then(Value::as_str) == Some(key) {
            return Ok(Some(item));
        }
    }
    Ok(None)
}

/// Disposable idempotency-marker filename: `idem-<safe_name(key)>.txt`.
pub fn idempotency_marker_name(key: &str) -> String {
    format!("idem-{}.txt", digest::safe_name(key))
}
