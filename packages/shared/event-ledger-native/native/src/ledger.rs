//! Event-ledger layout and append core. Event files live directly in the
//! ledger directory, named `<prefix>-<sequence:012>-<uuid v4>.json`;
//! idempotency markers are `idem-<safe_name>.txt` files containing the
//! admitted `event_id`.

use serde_json::{json, Value};
use std::path::PathBuf;
use uuid::Uuid;

use crate::chain::{self, ChainSpec, Refusal};
use crate::digest;
use crate::error::ErrorSchema;
use crate::io;

/// Filesystem layout of one event ledger.
#[derive(Clone, Debug)]
pub struct LedgerLayout {
    pub directory: PathBuf,
    pub file_prefix: String,
}

impl LedgerLayout {
    pub fn new(directory: impl Into<PathBuf>, file_prefix: impl Into<String>) -> Self {
        Self {
            directory: directory.into(),
            file_prefix: file_prefix.into(),
        }
    }

    /// Path of one event file by event id.
    pub fn event_path(&self, event_id: &str) -> PathBuf {
        self.directory.join(format!("{event_id}.json"))
    }

    /// Path of the disposable idempotency marker for one key.
    pub fn idempotency_marker_path(&self, key: &str) -> PathBuf {
        self.directory.join(chain::idempotency_marker_name(key))
    }
}

/// Sorted list of event files (`<prefix>-*.json`) in the ledger directory.
pub fn files(schema: ErrorSchema, layout: &LedgerLayout) -> Result<Vec<PathBuf>, Value> {
    if !layout.directory.exists() {
        return Ok(vec![]);
    }
    let prefix = format!("{}-", layout.file_prefix);
    let mut files = std::fs::read_dir(&layout.directory)
        .map_err(schema.io_error("ledger_read_failed"))?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.extension().and_then(|value| value.to_str()) == Some("json")
                && path
                    .file_name()
                    .and_then(|value| value.to_str())
                    .map(|name| name.starts_with(&prefix))
                    .unwrap_or(false)
        })
        .collect::<Vec<_>>();
    files.sort();
    Ok(files)
}

/// Current ledger head: the hash field of the last event, or `None` for an
/// empty ledger.
pub fn head(
    schema: ErrorSchema,
    layout: &LedgerLayout,
    hash_field: &str,
) -> Result<Option<String>, Value> {
    let Some(path) = files(schema, layout)?.last().cloned() else {
        return Ok(None);
    };
    Ok(io::read_json(schema, &path)?[hash_field]
        .as_str()
        .map(str::to_string))
}

/// The standard event-ledger chain shape: contiguous `sequence` from 1,
/// `previous_hash` links, and the given hash field, with the standard
/// `ledger_*` refusal codes.
pub fn standard_chain_spec(hash_field: &'static str) -> ChainSpec {
    ChainSpec {
        hash_field,
        previous_hash_field: "previous_hash",
        ordinal_field: "sequence",
        ordinal_start: 1,
        missing_hash: Refusal::new(
            "ledger_hash_missing",
            format!("ledger {hash_field} is missing"),
        ),
        ordinal_invalid: Refusal::new(
            "ledger_sequence_invalid",
            "ledger sequence is not contiguous",
        ),
        link_invalid: Refusal::new(
            "ledger_chain_invalid",
            "ledger previous_hash does not match",
        ),
        hash_invalid: Refusal::new(
            "ledger_hash_invalid",
            format!("ledger {hash_field} does not match content"),
        ),
    }
}

/// Verify the full ledger hash chain.
pub fn verify(
    schema: ErrorSchema,
    layout: &LedgerLayout,
    hash_field: &'static str,
) -> Result<(), Value> {
    chain::verify_files(
        schema,
        &files(schema, layout)?,
        &standard_chain_spec(hash_field),
    )?;
    Ok(())
}

/// Scan the ledger for the event carrying one idempotency key.
pub fn find_event_by_idempotency(
    schema: ErrorSchema,
    layout: &LedgerLayout,
    key: &str,
) -> Result<Option<Value>, Value> {
    chain::find_by_idempotency(schema, &files(schema, layout)?, key)
}

/// Ledger-owned envelope fields supplied to the append callback.
#[derive(Clone, Debug)]
pub struct EnvelopeContext {
    pub sequence: u64,
    pub event_id: String,
    pub previous_hash: Option<String>,
}

/// Result of one durable event append.
#[derive(Clone, Debug)]
pub struct AppendOutcome {
    pub event: Value,
    pub event_id: String,
    pub event_hash: String,
    pub sequence: u64,
}

/// Append one immutable event: derive `sequence = last_event.sequence + 1` and
/// `event_id = <prefix>-{sequence:012}-{uuid v4}`, let `build` supply the
/// domain envelope around the ledger-owned fields, compute and append the
/// hash field, write the event `create_new` + `sync_all`, and write the
/// idempotency marker when a key is present.
///
/// `expected_head` is the optional head-CAS boundary: `Some(expected)`
/// requires the current head to equal `expected` (`None` expects an empty
/// ledger) and refuses with `ledger_head_conflict`; `None` skips the check.
/// Callers needing richer conflict details should pre-check the head under
/// the same authority lock and pass `None`.
pub fn append_event(
    schema: ErrorSchema,
    layout: &LedgerLayout,
    hash_field: &str,
    expected_head: Option<Option<&str>>,
    idempotency_key: Option<&str>,
    build: impl FnOnce(EnvelopeContext) -> Value,
) -> Result<AppendOutcome, Value> {
    let existing = files(schema, layout)?;
    let last_event = existing
        .last()
        .map(|path| io::read_json(schema, path))
        .transpose()?;
    let previous_hash = last_event
        .as_ref()
        .and_then(|event| event[hash_field].as_str())
        .map(str::to_string);
    if let Some(expected) = expected_head {
        if expected != previous_hash.as_deref() {
            return Err(schema.error(
                "ledger_head_conflict",
                "expected ledger head does not match",
                json!({"expected":expected,"actual":previous_hash}),
            ));
        }
    }
    let sequence = match last_event {
        Some(event) => event["sequence"].as_u64().ok_or_else(|| {
            schema.error(
                "ledger_sequence_invalid",
                "last ledger event has no valid sequence",
                json!({"path":existing.last().map(|path|path.to_string_lossy())}),
            )
        })? + 1,
        None => 1,
    };
    let event_id = format!("{}-{sequence:012}-{}", layout.file_prefix, Uuid::new_v4());
    let context = EnvelopeContext {
        sequence,
        event_id: event_id.clone(),
        previous_hash,
    };
    let mut event = build(context);
    let event_hash = digest::digest_value(schema, &event)?;
    event
        .as_object_mut()
        .unwrap()
        .insert(hash_field.into(), json!(event_hash));
    io::write_new_json(schema, &layout.event_path(&event_id), &event)?;
    if let Some(key) = idempotency_key {
        io::write_new(
            schema,
            &layout.idempotency_marker_path(key),
            event_id.as_bytes(),
        )?;
    }
    Ok(AppendOutcome {
        event,
        event_id,
        event_hash,
        sequence,
    })
}
