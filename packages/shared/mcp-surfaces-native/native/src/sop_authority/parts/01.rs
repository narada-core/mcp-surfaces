use jsonschema::validator_for;
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use time::format_description::well_known::Rfc3339;
use time::{Duration, OffsetDateTime, UtcOffset};
use uuid::Uuid;

const DB_RELATIVE: &str = ".sop/sop.db";
pub(crate) const MAX_INLINE_VALUE_BYTES: usize = 16 * 1024;
pub(crate) const MAX_RUN_STATE_BYTES: usize = 128 * 1024;
pub(crate) const MAX_TEMPLATE_DEFINITION_BYTES: usize = 128 * 1024;
const MAX_TEMPLATE_FILE_BYTES: u64 = 512 * 1024;
const MAX_STEPS: usize = 128;
pub(crate) const MAX_OUTBOX_PAYLOAD_BYTES: usize = 16 * 1024;
const MAX_OUTBOX_RECEIPT_BYTES: usize = 8 * 1024;
const MIN_LEASE_MS: i64 = 1_000;
const MAX_LEASE_MS: i64 = 5 * 60_000;
pub(crate) const SOP_TERMINAL_TOPIC: &str = "sop.run.terminal.v1";
const TEMPLATE_SCHEMA: &str = include_str!("../../../../../../sop-mcp/sops/sop-template.schema.json");

pub fn call_tool(name: &str, args: &Map<String, Value>, root: &Path) -> Result<Value, Value> {
    match name {
        "sop_template_create" => template_create(args, root),
        "sop_template_update" => template_update(args, root),
        "sop_template_deprecate" => template_deprecate(args, root),
        "sop_template_unimport" => template_unimport(args, root),
        "sop_template_import_yaml" => template_import_yaml(args, root),
        "sop_handoff_claim" => handoff_claim(args, root),
        "sop_handoff_claim_and_advance" => handoff_claim_and_advance(args, root),
        "sop_handoff_renew" => handoff_renew(args, root),
        "sop_handoff_release" => handoff_release(args, root),
        "sop_outbox_consumer_register" => outbox_consumer_register(args, root),
        "sop_outbox_ack" => outbox_ack(args, root),
        "sop_outbox_compact" => outbox_compact(args, root),
        "sop_run_start" | "sop_run_refresh" | "sop_run_advance" | "sop_handoff_retry"
        | "sop_action_resolve" | "sop_run_cancel" => crate::sop_engine::call_tool(name, args, root),
        _ => Err(diagnostic(
            "unknown_tool",
            &format!("unknown_tool:{name}"),
            json!({"tool_name":name}),
        )),
    }
}

pub(crate) fn open_db(root: &Path) -> Result<Connection, Value> {
    let path = root.join(DB_RELATIVE);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|error| {
            diagnostic(
                "sop_registry_directory_failed",
                &error.to_string(),
                json!({}),
            )
        })?;
    }
    let connection = Connection::open(&path).map_err(|error| {
        diagnostic(
            "sop_registry_open_failed",
            &error.to_string(),
            json!({"db_path":path}),
        )
    })?;
    connection
        .pragma_update(None, "journal_mode", "WAL")
        .map_err(|error| diagnostic("sop_registry_pragma_failed", &error.to_string(), json!({})))?;
    connection
        .pragma_update(None, "foreign_keys", true)
        .map_err(|error| diagnostic("sop_registry_pragma_failed", &error.to_string(), json!({})))?;
    connection
        .busy_timeout(std::time::Duration::from_millis(5_000))
        .map_err(|error| diagnostic("sop_registry_pragma_failed", &error.to_string(), json!({})))?;
    prepare_schema(&connection)?;
    Ok(connection)
}

fn prepare_schema(db: &Connection) -> Result<(), Value> {
    db.execute_batch(
        r#"
        CREATE TABLE IF NOT EXISTS sop_templates (
          sop_id TEXT NOT NULL,
          version INTEGER NOT NULL DEFAULT 1,
          title TEXT NOT NULL,
          status TEXT NOT NULL DEFAULT 'draft',
          description TEXT NOT NULL DEFAULT '',
          steps_json TEXT NOT NULL DEFAULT '[]',
          trigger_kind TEXT NOT NULL DEFAULT 'manual',
          input_schema_json TEXT,
          output_mapping_json TEXT,
          output_ref_mapping_json TEXT,
          output_schema_json TEXT,
          acceptance_criteria_json TEXT NOT NULL DEFAULT '[]',
          evidence_requirements_json TEXT NOT NULL DEFAULT '[]',
          created_at TEXT NOT NULL,
          updated_at TEXT NOT NULL,
          PRIMARY KEY (sop_id, version)
        ) STRICT;
        CREATE TABLE IF NOT EXISTS sop_runs (
          run_id TEXT PRIMARY KEY,
          sop_id TEXT NOT NULL,
          sop_version INTEGER NOT NULL,
          sop_title TEXT NOT NULL,
          status TEXT NOT NULL DEFAULT 'pending',
          occurrence_key TEXT NOT NULL DEFAULT '',
          request_fingerprint TEXT NOT NULL DEFAULT '',
          definition_fingerprint TEXT NOT NULL DEFAULT '',
          definition_json TEXT NOT NULL DEFAULT '{}',
          input_json TEXT NOT NULL DEFAULT '{}',
          input_ref_json TEXT,
          output_json TEXT NOT NULL DEFAULT '{}',
          output_ref_json TEXT,
          step_states_json TEXT NOT NULL DEFAULT '[]',
          trigger_source_kind TEXT NOT NULL DEFAULT 'manual',
          trigger_source_ref TEXT NOT NULL DEFAULT '',
          triggered_by TEXT NOT NULL DEFAULT '',
          parent_run_id TEXT,
          parent_step_id TEXT,
          created_at TEXT NOT NULL,
          updated_at TEXT NOT NULL,
          completed_at TEXT
        ) STRICT;
        CREATE TABLE IF NOT EXISTS sop_events (
          event_id TEXT PRIMARY KEY,
          run_id TEXT NOT NULL,
          step_id TEXT NOT NULL,
          event_kind TEXT NOT NULL,
          details_json TEXT NOT NULL DEFAULT '{}',
          recorded_at TEXT NOT NULL
        ) STRICT;
        CREATE TABLE IF NOT EXISTS sop_actions (
          action_id TEXT PRIMARY KEY,
          run_id TEXT NOT NULL,
          step_id TEXT NOT NULL,
          occurrence_key TEXT NOT NULL,
          surface_id TEXT NOT NULL,
          tool_name TEXT NOT NULL,
          arguments_json TEXT NOT NULL,
          request_fingerprint TEXT NOT NULL,
          status TEXT NOT NULL DEFAULT 'pending',
          completion_key TEXT,
          completion_fingerprint TEXT,
          operation_ref TEXT,
          result_json TEXT NOT NULL DEFAULT '{}',
          result_ref_json TEXT,
          error_message TEXT,
          created_at TEXT NOT NULL,
          updated_at TEXT NOT NULL,
          completed_at TEXT,
          UNIQUE (run_id, step_id),
          UNIQUE (occurrence_key)
        ) STRICT;
        CREATE UNIQUE INDEX IF NOT EXISTS sop_runs_occurrence_unique
          ON sop_runs (sop_id, occurrence_key) WHERE occurrence_key <> '';
        CREATE INDEX IF NOT EXISTS sop_runs_status_idx ON sop_runs (status, updated_at);
        CREATE INDEX IF NOT EXISTS sop_runs_parent_idx ON sop_runs (parent_run_id, parent_step_id);
        CREATE INDEX IF NOT EXISTS sop_actions_status_idx ON sop_actions (status, created_at);
        CREATE TABLE IF NOT EXISTS sop_handoffs (
          handoff_id TEXT PRIMARY KEY,
          run_id TEXT NOT NULL REFERENCES sop_runs(run_id),
          step_id TEXT NOT NULL,
          occurrence_key TEXT NOT NULL UNIQUE,
          sop_id TEXT NOT NULL,
          sop_version INTEGER NOT NULL,
          executor TEXT NOT NULL CHECK (executor IN ('agent', 'operator')),
          title TEXT NOT NULL,
          instructions TEXT NOT NULL CHECK (length(CAST(instructions AS BLOB)) <= 16384),
          input_json TEXT NOT NULL CHECK (length(CAST(input_json AS BLOB)) <= 16384),
          input_ref_json TEXT,
          result_schema_json TEXT,
          request_fingerprint TEXT NOT NULL,
          status TEXT NOT NULL DEFAULT 'pending' CHECK (status IN ('pending', 'leased', 'completed', 'failed', 'cancelled')),
          lease_owner TEXT,
          lease_token TEXT,
          lease_expires_at TEXT,
          attempt_count INTEGER NOT NULL DEFAULT 0,
          last_error TEXT,
          completion_key TEXT,
          completion_fingerprint TEXT,
          principal TEXT,
          result_json TEXT NOT NULL DEFAULT '{}' CHECK (length(CAST(result_json AS BLOB)) <= 16384),
          result_ref_json TEXT,
          error_message TEXT,
          created_at TEXT NOT NULL,
          updated_at TEXT NOT NULL,
          completed_at TEXT,
          UNIQUE (run_id, step_id)
        ) STRICT;
        CREATE INDEX IF NOT EXISTS sop_handoffs_delivery_idx
          ON sop_handoffs(status, lease_expires_at, created_at);
        CREATE TABLE IF NOT EXISTS sop_outbox (
          event_id TEXT PRIMARY KEY,
          topic TEXT NOT NULL,
          partition_key TEXT NOT NULL,
          run_id TEXT NOT NULL UNIQUE REFERENCES sop_runs(run_id),
          sop_id TEXT NOT NULL,
          sop_version INTEGER NOT NULL,
          occurrence_key TEXT NOT NULL,
          outcome TEXT NOT NULL CHECK (outcome IN ('completed', 'failed', 'cancelled')),
          payload_json TEXT NOT NULL CHECK (length(CAST(payload_json AS BLOB)) <= 16384),
          created_at TEXT NOT NULL,
          available_at TEXT NOT NULL,
          compacted_at TEXT
        ) STRICT;
        CREATE INDEX IF NOT EXISTS sop_outbox_delivery_idx
          ON sop_outbox(topic, available_at, created_at);
        CREATE TABLE IF NOT EXISTS sop_outbox_consumer_requirements (
          topic TEXT NOT NULL,
          consumer_id TEXT NOT NULL,
          start_at TEXT NOT NULL,
          registered_at TEXT NOT NULL,
          PRIMARY KEY(topic, consumer_id)
        ) STRICT;
        CREATE TABLE IF NOT EXISTS sop_outbox_receipts (
          event_id TEXT NOT NULL REFERENCES sop_outbox(event_id),
          consumer_id TEXT NOT NULL,
          processed_at TEXT NOT NULL,
          receipt_json TEXT NOT NULL CHECK (length(CAST(receipt_json AS BLOB)) <= 8192),
          PRIMARY KEY(event_id, consumer_id)
        ) STRICT;
        "#,
    )
    .map_err(|error| diagnostic("sop_registry_schema_failed", &error.to_string(), json!({})))?;
    Ok(())
}

