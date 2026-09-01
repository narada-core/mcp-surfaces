use rusqlite::{params, params_from_iter, types::Value as SqlValue, Connection, OptionalExtension};
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use time::{
    format_description::well_known::Rfc3339, macros::format_description, Duration, OffsetDateTime,
};
use uuid::Uuid;

const SCHEMA_VERSION: i64 = 2;
const DB_RELATIVE: &str = ".ai/scheduler.db";
const MAX_EVENT_BYTES: usize = 16_384;
const MAX_ERROR_BYTES: usize = 2_048;

pub const TOOLS: &[(&str, bool)] = &[
    ("scheduler_activation_doctor", true),
    ("scheduler_activation_prepare", false),
    ("scheduler_binding_list", true),
    ("scheduler_binding_show", true),
    ("scheduler_binding_upsert", false),
    ("scheduler_binding_pause", false),
    ("scheduler_binding_resume", false),
    ("scheduler_binding_retire", false),
    ("scheduler_event_show", true),
    ("scheduler_event_admit", false),
    ("scheduler_activation_list", true),
    ("scheduler_activation_claim", false),
    ("scheduler_activation_admit_sop", false),
    ("scheduler_activation_fail", false),
    ("scheduler_activation_resolve", false),
    ("scheduler_activation_unblock", false),
];

pub fn supports(name: &str) -> bool {
    TOOLS.iter().any(|(candidate, _)| *candidate == name)
}

pub fn is_mutation(name: &str) -> bool {
    TOOLS
        .iter()
        .find(|(candidate, _)| *candidate == name)
        .map(|(_, read_only)| !read_only)
        .unwrap_or(false)
}

pub fn call_tool(name: &str, args: &Map<String, Value>, root: &Path) -> Result<Value, Value> {
    match name {
        "scheduler_activation_doctor" => Ok(doctor(root)),
        "scheduler_activation_prepare" => prepare(root),
        "scheduler_binding_list" => with_prepared(root, |db| binding_list(db, args)),
        "scheduler_binding_show" => with_prepared(root, |db| binding_show(db, args)),
        "scheduler_binding_upsert" => with_prepared(root, |db| binding_upsert(db, args)),
        "scheduler_binding_pause" | "scheduler_binding_resume" | "scheduler_binding_retire" => {
            with_prepared(root, |db| binding_set_status(db, name, args))
        }
        "scheduler_event_show" => with_prepared(root, |db| event_show(db, args)),
        "scheduler_event_admit" => with_prepared(root, |db| event_admit(db, args)),
        "scheduler_activation_list" => with_prepared(root, |db| activation_list(db, args)),
        "scheduler_activation_claim" => with_prepared(root, |db| activation_claim(db, args)),
        "scheduler_activation_admit_sop" => {
            with_prepared(root, |db| activation_admit_sop(db, args))
        }
        "scheduler_activation_fail" => with_prepared(root, |db| activation_fail(db, args)),
        "scheduler_activation_resolve" => with_prepared(root, |db| activation_resolve(db, args)),
        "scheduler_activation_unblock" => with_prepared(root, |db| activation_unblock(db, args)),
        _ => Err(error("unknown_tool", &format!("unknown_tool:{name}"))),
    }
}

fn db_path(root: &Path) -> PathBuf {
    root.join(DB_RELATIVE)
}

fn configure(db: &Connection, mutate_journal: bool) -> Result<(), Value> {
    db.execute_batch("pragma foreign_keys = on; pragma busy_timeout = 30000;")
        .map_err(|cause| db_error("scheduler_activation_store_configure_failed", cause))?;
    if mutate_journal {
        db.execute_batch("pragma journal_mode = wal;")
            .map_err(|cause| db_error("scheduler_activation_store_configure_failed", cause))?;
    }
    let mode: String = db
        .query_row("pragma journal_mode", [], |row| row.get(0))
        .map_err(|cause| db_error("scheduler_activation_store_configure_failed", cause))?;
    if !mode.eq_ignore_ascii_case("wal") {
        return Err(error(
            "scheduler_activation_store_not_prepared",
            &format!("scheduler_activation_store_not_prepared:journal_mode_{mode}"),
        ));
    }
    db.execute_batch("pragma synchronous = normal;")
        .map_err(|cause| db_error("scheduler_activation_store_configure_failed", cause))
}

fn prepare(root: &Path) -> Result<Value, Value> {
    let path = db_path(root);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|cause| {
            error(
                "scheduler_activation_store_directory_failed",
                &cause.to_string(),
            )
        })?;
    }
    let db = Connection::open(&path)
        .map_err(|cause| db_error("scheduler_activation_store_open_failed", cause))?;
    configure(&db, true)?;
    initialize_schema(&db)?;
    Ok(json!({
        "schema":"narada.scheduler.activation_prepare.v1",
        "status":"prepared",
        "db_path":path.to_string_lossy(),
        "schema_version":SCHEMA_VERSION
    }))
}

fn initialize_schema(db: &Connection) -> Result<(), Value> {
    db.execute_batch(
        r#"
        begin immediate;
        create table if not exists scheduler_meta (
          singleton integer primary key check (singleton = 1),
          schema_version integer not null,
          prepared_at text not null
        );
        create table if not exists scheduler_bindings (
          binding_id text primary key,
          trigger_kind text not null check (trigger_kind in ('bootstrap', 'completion', 'domain_event')),
          source_topic text not null,
          source_sop_id text,
          terminal_outcomes_json text not null,
          target_sop_id text not null,
          target_template_version text not null,
          concurrency text not null check (concurrency in ('singleton', 'partitioned')),
          delay_by_outcome_ms_json text not null,
          default_delay_ms integer not null check (default_delay_ms >= 0),
          retry_base_ms integer not null check (retry_base_ms >= 0),
          retry_max_ms integer not null check (retry_max_ms >= retry_base_ms),
          max_attempts integer not null check (max_attempts > 0),
          blocked_policy text not null check (blocked_policy = 'manual_unblock'),
          status text not null check (status in ('active', 'paused', 'retired')),
          revision integer not null check (revision > 0),
          spec_digest text not null,
          created_at text not null,
          updated_at text not null
        );
        create index if not exists idx_scheduler_bindings_topic on scheduler_bindings(source_topic, status);
        create table if not exists scheduler_source_events (
          event_id text primary key,
          topic text not null,
          partition_key text not null,
          aggregate_id text not null,
          aggregate_revision integer not null,
          schema_version integer not null,
          causation_id text not null,
          idempotency_key text not null,
          payload_json text not null check (length(cast(payload_json as blob)) <= 16384),
          event_digest text not null,
          occurred_at text not null,
          admitted_at text not null
        );
        create table if not exists scheduler_activations (
          activation_id text primary key,
          binding_id text not null references scheduler_bindings(binding_id),
          source_event_id text not null references scheduler_source_events(event_id),
          occurrence_key text not null,
          target_sop_id text not null,
          target_template_version text not null,
          partition_key text not null,
          due_at text not null,
          status text not null check (status in ('pending', 'leased', 'admitted', 'terminal', 'blocked')),
          attempt_count integer not null default 0,
          lease_owner text,
          lease_token text,
          lease_expires_at text,
          sop_run_id text,
          terminal_outcome text,
          last_error text,
          created_at text not null,
          updated_at text not null,
          unique(binding_id, source_event_id),
          unique(target_sop_id, occurrence_key)
        );
        create index if not exists idx_scheduler_activations_due on scheduler_activations(status, due_at, binding_id, partition_key);
        create index if not exists idx_scheduler_activations_sop_run on scheduler_activations(sop_run_id);
        create table if not exists scheduler_activation_receipts (
          activation_id text not null references scheduler_activations(activation_id),
          receipt_kind text not null,
          receipt_id text not null,
          receipt_json text not null check (length(cast(receipt_json as blob)) <= 16384),
          recorded_at text not null,
          primary key(activation_id, receipt_kind),
          unique(receipt_id)
        );
        create unique index if not exists idx_scheduler_activations_sop_run_unique
          on scheduler_activations(sop_run_id) where sop_run_id is not null;
        commit;
        "#,
    )
    .map_err(|cause| db_error("scheduler_activation_schema_failed", cause))?;
    let has_lease_token = db
        .prepare("pragma table_info(scheduler_activations)")
        .and_then(|mut statement| {
            let columns = statement
                .query_map([], |row| row.get::<_, String>(1))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(columns.iter().any(|column| column == "lease_token"))
        })
        .map_err(|cause| db_error("scheduler_activation_schema_failed", cause))?;
    if !has_lease_token {
        db.execute_batch(
            r#"
            begin immediate;
            alter table scheduler_activations add column lease_token text;
            update scheduler_activations
               set status = 'pending', lease_owner = null, lease_expires_at = null,
                   attempt_count = attempt_count + 1,
                   last_error = 'schema_upgrade_invalidated_lease'
             where status = 'leased';
            commit;
            "#,
        )
        .map_err(|cause| db_error("scheduler_activation_schema_failed", cause))?;
    }
    db.execute(
        "insert into scheduler_meta(singleton, schema_version, prepared_at) values (1, ?1, ?2) on conflict(singleton) do update set schema_version=excluded.schema_version, prepared_at=excluded.prepared_at",
        params![SCHEMA_VERSION, now_iso()],
    )
    .map_err(|cause| db_error("scheduler_activation_schema_failed", cause))?;
    Ok(())
}

fn doctor(root: &Path) -> Value {
    let path = db_path(root);
    let mut result = json!({
        "schema":"narada.scheduler.activation_doctor.v1",
        "site_root":root.to_string_lossy(),
        "runtime_open":false,
        "preparation":{
            "status":"missing",
            "db_path":path.to_string_lossy(),
            "schema_version":Value::Null,
            "reason":"database_missing"
        }
    });
    if !path.exists() {
        return result;
    }
    let inspection = match Connection::open(&path) {
        Ok(db) => match configure(&db, false).and_then(|_| {
            db.query_row(
                "select schema_version from scheduler_meta where singleton = 1",
                [],
                |row| row.get::<_, i64>(0),
            )
            .optional()
            .map_err(|cause| db_error("scheduler_activation_store_inspect_failed", cause))
        }) {
            Ok(Some(version)) if version == SCHEMA_VERSION => {
                json!({"status":"prepared","db_path":path.to_string_lossy(),"schema_version":version})
            }
            Ok(version) => {
                json!({"status":"stale","db_path":path.to_string_lossy(),"schema_version":version,"reason":format!("schema_version_{}", version.map(|value| value.to_string()).unwrap_or_else(|| "missing".to_string()))})
            }
            Err(reason) => {
                json!({"status":"invalid","db_path":path.to_string_lossy(),"schema_version":Value::Null,"reason":reason.get("message").cloned().unwrap_or(reason)})
            }
        },
        Err(cause) => {
            json!({"status":"invalid","db_path":path.to_string_lossy(),"schema_version":Value::Null,"reason":cause.to_string()})
        }
    };
    result["preparation"] = inspection;
    result
}

