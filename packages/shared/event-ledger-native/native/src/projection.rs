//! Disposable SQLite projection rebuild: verify the ledger, apply every
//! event into `projection.sqlite.next` inside one transaction, then remove
//! the old file and atomically rename `.next` into place. The consuming
//! surface owns the DDL and the per-event fold.

use rusqlite::Connection;
use serde_json::Value;
use std::fs;
use std::path::Path;

use crate::error::ErrorSchema;
use crate::io;
use crate::ledger::{self, LedgerLayout};

/// Rebuild the projection from the authoritative ledger. `ddl` is the full
/// schema batch executed on the fresh `.next` database before the fold;
/// `apply` folds one event (with its `event_id`) into the open transaction
/// and may iterate any domain operation arrays inside the event.
pub fn rebuild_projection(
    schema: ErrorSchema,
    layout: &LedgerLayout,
    hash_field: &'static str,
    projection_path: &Path,
    ddl: &str,
    apply: impl Fn(&rusqlite::Transaction, &Value, &str) -> Result<(), Value>,
) -> Result<(), Value> {
    ledger::verify(schema, layout, hash_field)?;
    let temporary = projection_path.with_extension("sqlite.next");
    let _ = fs::remove_file(&temporary);
    let mut db = Connection::open(&temporary).map_err(schema.db_error("projection_create_failed"))?;
    db.execute_batch(ddl)
        .map_err(schema.db_error("projection_schema_failed"))?;
    let tx = db
        .transaction()
        .map_err(schema.db_error("projection_transaction_failed"))?;
    for path in ledger::files(schema, layout)? {
        let event = io::read_json(schema, &path)?;
        let event_id = event["event_id"].as_str().unwrap_or_default();
        apply(&tx, &event, event_id)?;
    }
    tx.commit().map_err(schema.db_error("projection_commit_failed"))?;
    drop(db);
    let _ = fs::remove_file(projection_path);
    fs::rename(&temporary, projection_path).map_err(schema.io_error("projection_replace_failed"))?;
    Ok(())
}
