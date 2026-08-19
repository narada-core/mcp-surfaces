//! Durable JSON record IO. Immutable records are written `create_new`-only
//! with `sync_all`; best-effort or swallowed writes are prohibited in this
//! regime, so every failure is a structured refusal.

use serde_json::{json, Value};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::Path;

use crate::error::ErrorSchema;

/// Read one JSON record.
pub fn read_json(schema: ErrorSchema, path: &Path) -> Result<Value, Value> {
    let bytes = fs::read(path).map_err(schema.io_error("record_read_failed"))?;
    serde_json::from_slice(&bytes).map_err(|source| {
        schema.error(
            "record_invalid_json",
            &source.to_string(),
            json!({"path":path.to_string_lossy()}),
        )
    })
}

/// Write a pretty-printed JSON record (trailing newline) with `create_new`;
/// refuses to overwrite.
pub fn write_new_json(schema: ErrorSchema, path: &Path, value: &Value) -> Result<(), Value> {
    write_new(
        schema,
        path,
        format!(
            "{}\n",
            serde_json::to_string_pretty(value).map_err(|source| schema.error(
                "json_encode_failed",
                &source.to_string(),
                Value::Null
            ))?
        )
        .as_bytes(),
    )
}

/// Overwrite a JSON record with pretty-printed content (trailing newline).
/// Reserved for explicitly replaceable records such as review verdicts.
pub fn write_replace_json(schema: ErrorSchema, path: &Path, value: &Value) -> Result<(), Value> {
    fs::write(
        path,
        format!("{}\n", serde_json::to_string_pretty(value).unwrap()),
    )
    .map_err(schema.io_error("record_write_failed"))
}

/// Write raw bytes with `create_new` + `sync_all`; refuses to overwrite.
pub fn write_new(schema: ErrorSchema, path: &Path, bytes: &[u8]) -> Result<(), Value> {
    let mut file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(path)
        .map_err(schema.io_error("immutable_record_exists"))?;
    file.write_all(bytes)
        .map_err(schema.io_error("record_write_failed"))?;
    file.sync_all().map_err(schema.io_error("record_sync_failed"))
}
