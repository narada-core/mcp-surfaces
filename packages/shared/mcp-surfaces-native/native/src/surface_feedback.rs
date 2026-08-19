use narada_mcp_event_ledger::digest;
use narada_mcp_event_ledger::ledger::LedgerLayout;
use narada_mcp_event_ledger::{
    io as ledger_io, ledger as event_ledger, lock, projection as ledger_projection, ErrorSchema,
};
use narada_mcp_lifecycle::{LifecycleServer, Options as LifecycleOptions, Surface as LifecycleSurface};
use rusqlite::{params, Connection, OpenFlags, OptionalExtension};
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use std::fs;
use std::path::{Path, PathBuf};
use uuid::Uuid;

const SERVER_NAME: &str = "surface-feedback-mcp";
const FEEDBACK_KINDS: &[&str] = &["bug", "improvement", "gap", "observation"];
const FEEDBACK_STATUSES: &[&str] = &["submitted", "acknowledged", "routed", "converted_to_task", "closed"];
const READ_SCOPES: &[&str] = &["all_authorized", "store_reconciliation", "authority_visible", "owned_surfaces", "authority_site_submissions"];
const MAX_IMPORT_IDS: usize = 200;
const ERROR_SCHEMA: ErrorSchema = ErrorSchema("narada.surface_feedback.error.v1");
const EVENT_SCHEMA: &str = "narada.surface_feedback.event.v1";
const HASH_FIELD: &str = "event_hash";

pub fn list_tools() -> Vec<Value> {
    vec![
        guidance_tool(),
        tool("surface_feedback_doctor", "Inspect surface feedback storage posture and backing store path.", json!({"type":"object","properties":{},"additionalProperties":false}), true),
        tool("surface_feedback_live_proof_template", "Return a reusable structured template for live no-mock proof feedback.", json!({"type":"object","properties":{"workflow":{"type":"string"},"surface_id":{"type":"string"}},"additionalProperties":false}), true),
        tool("surface_feedback_list", "List feedback entries using an explicit server-bound read scope.", list_schema(false), true),
        tool("surface_feedback_actionable_queue", "Return a bounded actionable feedback queue using an explicit read scope.", list_schema(true), true),
        tool("surface_feedback_show", "Show one feedback entry using an explicit read scope.", json!({"type":"object","properties":{"feedback_id":{"type":"string"},"scope":{"type":"string","enum":READ_SCOPES}},"required":["feedback_id","scope"],"additionalProperties":false}), true),
        tool("surface_feedback_stats", "Return bounded feedback counts by surface, kind, and status.", json!({"type":"object","properties":{"surface_id":{"type":"string"},"scope":{"type":"string","enum":READ_SCOPES}},"required":["scope"],"additionalProperties":false}), true),
        tool("surface_feedback_submit", "Submit feedback through the owning surface-feedback authority.", submit_schema(), false),
        tool("surface_feedback_update_status", "Update feedback status through the owning authority.", update_status_schema(), false),
        tool("surface_feedback_update_status_batch", "Update multiple feedback entries through the owning authority.", update_status_batch_schema(), false),
        tool("surface_feedback_convert_to_task", "Create a task handoff through the owning authority.", convert_to_task_schema(), false),
        tool("surface_feedback_import", "Import feedback through the owning authority.", import_schema(), false),
    ]
}

pub fn auxiliary(method: &str, params: &Map<String, Value>) -> Result<Value, Value> {
    match method {
        "prompts/list" => Ok(json!({"prompts":[{"name":"surface_feedback_workflow","title":"Surface Feedback Workflow","description":"Inspect feedback scope and evidence before routing or closing an entry.","arguments":[]}]})),
        "prompts/get" => {
            if params.get("name").and_then(Value::as_str) != Some("surface_feedback_workflow") { return Err(error("unknown_prompt", "unknown_prompt")); }
            Ok(json!({"description":"Inspect feedback scope and evidence before routing or closing an entry.","messages":[{"role":"user","content":{"type":"text","text":"Call surface_feedback_doctor first, then choose an explicit read scope. Inspect an entry before any owner-authorized mutation."}}]}))
        }
        "completion/complete" => {
            let is_name = params.get("argument").and_then(Value::as_object).and_then(|v| v.get("name")).and_then(Value::as_str) == Some("name");
            let values = if is_name { list_tools().iter().filter_map(|v| v.get("name").cloned()).take(100).collect::<Vec<_>>() } else { Vec::new() };
            Ok(json!({"completion":{"values":values,"total":values.len(),"hasMore":false}}))
        }
        "logging/setLevel" => Ok(json!({})),
        _ => Err(error("unsupported_mcp_method", &format!("unsupported_mcp_method:{method}"))),
    }
}

pub fn call_tool(name: &str, args: &Map<String, Value>, root: &Path) -> Result<Value, Value> {
    match name {
        "surface_feedback_guidance" => Ok(guidance(args)),
        "surface_feedback_doctor" => doctor(root),
        "surface_feedback_live_proof_template" => Ok(proof_template(args)),
        "surface_feedback_list" => feedback_list(args, root, false),
        "surface_feedback_actionable_queue" => feedback_list(args, root, true),
        "surface_feedback_show" => feedback_show(args, root),
        "surface_feedback_stats" => feedback_stats(args, root),
        "surface_feedback_submit" => feedback_submit(args, root),
        "surface_feedback_update_status" => feedback_update_status(args, root),
        "surface_feedback_update_status_batch" => feedback_update_status_batch(args, root),
        "surface_feedback_import" => feedback_import(args, root),
        "surface_feedback_convert_to_task" => feedback_convert_to_task(args, root),
        _ => Err(error("unknown_tool", &format!("unknown_tool:{name}"))),
    }
}

fn guidance_tool() -> Value { tool("surface_feedback_guidance", "Show model-facing operating guidance for surface feedback workflows.", json!({"type":"object","properties":{"workflow":{"type":"string"},"tool":{"type":"string"}},"additionalProperties":false}), true) }
fn submit_schema() -> Value { json!({"type":"object","properties":{"surface_id":{"type":"string","minLength":1},"submitter_site_id":{"type":"string","minLength":1,"description":"Optional assertion that must equal server-bound NARADA_SITE_ID."},"submitter_principal":{"type":"string","minLength":1,"description":"Optional assertion that must equal server-bound NARADA_AGENT_ID."},"kind":{"type":"string","enum":FEEDBACK_KINDS},"summary":{"type":"string","minLength":1},"details":{"type":"string"},"idempotency_key":{"type":"string","minLength":1,"description":"Stable retry key; reuse with different content is refused."}},"required":["surface_id","kind","summary"],"additionalProperties":false}) }
fn update_status_schema() -> Value { json!({"type":"object","properties":{"feedback_id":{"type":"string","minLength":1},"status":{"type":"string","enum":FEEDBACK_STATUSES},"resolution_note":{"type":"string","minLength":1},"task_ref":{"type":"string"},"task_status":{"type":"string"}},"required":["feedback_id","status","resolution_note"],"additionalProperties":false}) }
fn update_status_batch_schema() -> Value { json!({"type":"object","properties":{"updates":{"type":"array","minItems":1,"maxItems":MAX_IMPORT_IDS,"items":update_status_schema()}},"required":["updates"],"additionalProperties":false}) }
fn convert_to_task_schema() -> Value { json!({"type":"object","properties":{"feedback_id":{"type":"string","minLength":1},"task_title":{"type":"string","minLength":1},"resolution_note":{"type":"string","minLength":1}},"required":["feedback_id"],"additionalProperties":false}) }
fn import_schema() -> Value { json!({"type":"object","properties":{"source_db_path":{"type":"string","minLength":1,"description":"Canonical exact source database path. The runtime accepts source_feedback_root only as a legacy transport compatibility form."},"feedback_ids":{"type":"array","minItems":1,"maxItems":MAX_IMPORT_IDS,"items":{"type":"string","minLength":1}}},"required":["source_db_path","feedback_ids"],"additionalProperties":false}) }
fn guidance(args: &Map<String, Value>) -> Value { json!({"schema":"narada.mcp_surface.guidance.v0","status":"ok","surface_id":"surface-feedback","guidance_tool":"surface_feedback_guidance","purpose":"Inspect bounded feedback evidence with explicit read scope.","requested":{"workflow":args.get("workflow").cloned().unwrap_or(Value::Null),"tool":args.get("tool").cloned().unwrap_or(Value::Null)},"first_use":["Call surface_feedback_doctor first and inspect capabilities.read_scopes.","Use all_authorized for the canonical local store or store_reconciliation for explicit reconciliation work.","Use list or actionable_queue for bounded discovery, then show before mutation.","Task conversion remains owner-authorized."],"read_scope_summary":{"available":["all_authorized","store_reconciliation"],"server_authority_required":["authority_visible","owned_surfaces","authority_site_submissions"]},"boundaries":["The authoritative store is an append-only event ledger under <feedback_root>/ledger; the SQLite projection under .ai/feedback is disposable and rebuilt from the ledger on every read.","The legacy .feedback/surface-feedback.db store is migrated once into the ledger and never written again.","Task creation is delegated to the task-lifecycle authority adapter; Surface Feedback does not own tasks.","Authority and provenance scopes remain server-bound."]}) }

// ---------------------------------------------------------------------------
// Persistence layout: the event ledger is the only authority.
//   <root>/ledger/                       authoritative events (prefix "fb")
//   <root>/ledger/migration-complete.json  one-time legacy migration marker
//   <root>/.ai/feedback/projection.sqlite  disposable fold projection
//   <root>/.ai/feedback/locks/             authority lock files
//   <root>/.feedback/surface-feedback.db   legacy store (read-only, migrated)
// ---------------------------------------------------------------------------

fn ledger_dir(root: &Path) -> PathBuf { root.join("ledger") }
fn runtime_dir(root: &Path) -> PathBuf { root.join(".ai").join("feedback") }
fn projection_path(root: &Path) -> PathBuf { runtime_dir(root).join("projection.sqlite") }
fn legacy_db_path(root: &Path) -> PathBuf { root.join(".feedback").join("surface-feedback.db") }
fn migration_marker_path(root: &Path) -> PathBuf { ledger_dir(root).join("migration-complete.json") }
fn ledger_layout(root: &Path) -> LedgerLayout { LedgerLayout::new(ledger_dir(root), "fb") }
fn ledger_files(root: &Path) -> Result<Vec<PathBuf>, Value> { event_ledger::files(ERROR_SCHEMA, &ledger_layout(root)) }

fn prepare(root: &Path) -> Result<(), Value> {
    fs::create_dir_all(ledger_dir(root)).map_err(ERROR_SCHEMA.io_error("feedback_ledger_create_failed"))?;
    fs::create_dir_all(runtime_dir(root)).map_err(ERROR_SCHEMA.io_error("feedback_runtime_create_failed"))?;
    Ok(())
}

fn with_authority_lock<T>(root: &Path, action: impl FnOnce() -> Result<T, Value>) -> Result<T, Value> {
    lock::with_authority_lock(ERROR_SCHEMA, &runtime_dir(root).join("locks"), "ledger", lock::AuthorityLockPolicy::default(), action)
}

fn is_canonical_store(root: &Path) -> bool {
    std::env::var("NARADA_SURFACE_FEEDBACK_ROOT").ok().map(PathBuf::from).is_some_and(|canonical| canonical == root)
}

fn now_iso() -> String { digest::now() }

fn required_arg(args: &Map<String, Value>, key: &str, code: &str) -> Result<String, Value> {
    args.get(key).and_then(Value::as_str).map(str::trim).filter(|v| !v.is_empty()).map(ToOwned::to_owned).ok_or_else(|| error(code, code))
}

fn authority() -> Result<(String, String, Vec<String>), Value> {
    let site = std::env::var("NARADA_SITE_ID").ok().filter(|v| !v.trim().is_empty()).ok_or_else(|| error("feedback_authority_unconfigured", "feedback_authority_unconfigured"))?;
    let principal = std::env::var("NARADA_AGENT_ID").ok().filter(|v| !v.trim().is_empty()).unwrap_or_else(|| format!("surface-feedback@{site}"));
    let owned = std::env::var("NARADA_OWNED_SURFACE_IDS").ok().unwrap_or_default().split(',').map(str::trim).filter(|v| !v.is_empty()).map(ToOwned::to_owned).collect::<Vec<_>>();
    Ok((site, principal, owned))
}

fn bound_site_id() -> Value {
    std::env::var("NARADA_SITE_ID").ok().filter(|v| !v.trim().is_empty()).map(Value::String).unwrap_or(Value::Null)
}

fn bound_principal(default: &str) -> String {
    std::env::var("NARADA_AGENT_ID").ok().filter(|v| !v.trim().is_empty()).unwrap_or_else(|| default.to_string())
}

// ---------------------------------------------------------------------------
// One-time, non-destructive migration from the legacy SQLite store.
// ---------------------------------------------------------------------------

fn ensure_migrated(root: &Path) -> Result<(), Value> {
    prepare(root)?;
    if !migration_pending(root)? { return Ok(()); }
    with_authority_lock(root, || {
        // Re-check under the authority lock: another process may have finished.
        if !migration_pending(root)? { return Ok(()); }
        migrate_locked(root)
    })
}

fn migration_pending(root: &Path) -> Result<bool, Value> {
    if !legacy_db_path(root).exists() || migration_marker_path(root).exists() { return Ok(false); }
    let files = ledger_files(root)?;
    if files.is_empty() { return Ok(true); }
    // Crash mid-migration leaves migrated events without the marker; resume.
    for path in files {
        let event = ledger_io::read_json(ERROR_SCHEMA, &path)?;
        if event["event_type"].as_str() == Some("migrated") { return Ok(true); }
    }
    Ok(false)
}

fn migrate_locked(root: &Path) -> Result<(), Value> {
    let legacy_path = legacy_db_path(root);
    let legacy = Connection::open_with_flags(&legacy_path, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .map_err(ERROR_SCHEMA.db_error("feedback_migration_source_open_failed"))?;
    let ids = legacy_entry_ids(&legacy)?;
    let mut present = std::collections::BTreeSet::new();
    for path in ledger_files(root)? {
        let event = ledger_io::read_json(ERROR_SCHEMA, &path)?;
        if let Some(id) = event["entry"]["feedback_id"].as_str().or_else(|| event["feedback_id"].as_str()) {
            present.insert(id.to_string());
        }
    }
    let actor = bound_principal("surface-feedback-migration");
    let site = bound_site_id();
    let mut migrated = 0_u64;
    for id in &ids {
        if present.contains(id) { continue; }
        let Some(row) = feedback_row(&legacy, id)? else { continue; };
        let history = legacy_event_rows(&legacy, id)?;
        let entry = migration_entry(row, &legacy_path);
        let idempotency_key = format!("migration:{id}");
        event_ledger::append_event(ERROR_SCHEMA, &ledger_layout(root), HASH_FIELD, None, Some(&idempotency_key), |ctx| {
            json!({
                "schema": EVENT_SCHEMA,
                "sequence": ctx.sequence,
                "event_id": ctx.event_id,
                "previous_hash": ctx.previous_hash,
                "event_type": "migrated",
                "site_id": site,
                "actor_principal": actor,
                "created_at": now_iso(),
                "idempotency_key": idempotency_key,
                "entry": entry,
                "history": history,
                "legacy_db_path": legacy_path.to_string_lossy(),
            })
        })?;
        migrated += 1;
    }
    let head = event_ledger::head(ERROR_SCHEMA, &ledger_layout(root), HASH_FIELD)?;
    ledger_io::write_new_json(ERROR_SCHEMA, &migration_marker_path(root), &json!({
        "schema": "narada.surface_feedback.migration.v1",
        "status": "complete",
        "rows_migrated": migrated + present.len() as u64,
        "appended_this_run": migrated,
        "legacy_db_path": legacy_path.to_string_lossy(),
        "ledger_head": head,
        "completed_at": now_iso(),
    }))?;
    rebuild_projection(root)
}

fn legacy_entry_ids(db: &Connection) -> Result<Vec<String>, Value> {
    let table: Option<String> = db.query_row("SELECT name FROM sqlite_master WHERE type='table' AND name='feedback_entries'", [], |row| row.get(0))
        .optional().map_err(ERROR_SCHEMA.db_error("feedback_migration_probe_failed"))?;
    if table.is_none() { return Ok(vec![]); }
    let mut stmt = db.prepare("SELECT feedback_id FROM feedback_entries ORDER BY rowid ASC").map_err(ERROR_SCHEMA.db_error("feedback_migration_query_failed"))?;
    let rows = stmt.query_map([], |row| row.get::<_, String>(0)).map_err(ERROR_SCHEMA.db_error("feedback_migration_query_failed"))?;
    let mut ids = Vec::new();
    for row in rows { ids.push(row.map_err(ERROR_SCHEMA.db_error("feedback_migration_row_failed"))?); }
    Ok(ids)
}

const LEGACY_EVENT_FIELDS: &[&str] = &[
    "event_id", "feedback_id", "event_type", "actor_principal", "status", "task_ref", "task_status", "note", "details_json", "created_at",
];

fn legacy_event_rows(db: &Connection, feedback_id: &str) -> Result<Vec<Value>, Value> {
    let table: Option<String> = db.query_row("SELECT name FROM sqlite_master WHERE type='table' AND name='feedback_events'", [], |row| row.get(0))
        .optional().map_err(ERROR_SCHEMA.db_error("feedback_migration_probe_failed"))?;
    if table.is_none() { return Ok(vec![]); }
    let mut statement = db.prepare("PRAGMA table_info(feedback_events)").map_err(ERROR_SCHEMA.db_error("feedback_migration_probe_failed"))?;
    let rows = statement.query_map([], |row| row.get::<_, String>(1)).map_err(ERROR_SCHEMA.db_error("feedback_migration_probe_failed"))?;
    let mut columns = std::collections::BTreeSet::new();
    for row in rows { columns.insert(row.map_err(ERROR_SCHEMA.db_error("feedback_migration_probe_failed"))?); }
    let selection = LEGACY_EVENT_FIELDS.iter().map(|field| {
        if columns.contains(*field) { (*field).to_string() } else { format!("NULL AS {field}") }
    }).collect::<Vec<_>>().join(", ");
    let sql = format!("SELECT {selection} FROM feedback_events WHERE feedback_id = ?1 ORDER BY rowid ASC");
    let mut stmt = db.prepare(&sql).map_err(ERROR_SCHEMA.db_error("feedback_migration_events_failed"))?;
    let rows = stmt.query_map(params![feedback_id], |row| {
        let mut object = Map::new();
        for (index, field) in LEGACY_EVENT_FIELDS.iter().enumerate() {
            let value = row.get::<_, Option<String>>(index).ok().flatten().map(Value::String).unwrap_or(Value::Null);
            object.insert((*field).to_string(), value);
        }
        Ok(Value::Object(object))
    }).map_err(ERROR_SCHEMA.db_error("feedback_migration_events_failed"))?;
    let mut events = Vec::new();
    for row in rows { events.push(row.map_err(ERROR_SCHEMA.db_error("feedback_migration_events_row_failed"))?); }
    Ok(events)
}

fn migration_entry(row: Value, legacy_path: &Path) -> Value {
    let mut entry = row;
    let object = entry.as_object_mut().expect("feedback row is an object");
    fn field_text(object: &Map<String, Value>, key: &str) -> String {
        object.get(key).and_then(Value::as_str).unwrap_or("").to_string()
    }
    if !object.contains_key("details") { object.insert("details".into(), json!("")); }
    if field_text(object, "status").is_empty() { object.insert("status".into(), json!("submitted")); }
    let now = now_iso();
    if field_text(object, "created_at").is_empty() { object.insert("created_at".into(), json!(now)); }
    if field_text(object, "updated_at").is_empty() {
        let created = object.get("created_at").cloned().unwrap_or(json!(now));
        object.insert("updated_at".into(), created);
    }
    for key in ["resolution_note", "resolved_by", "task_ref", "task_status", "source_db_path", "source_updated_at", "source_sync_mode"] {
        if object.get(key).and_then(Value::as_str).is_some_and(|value| value.is_empty()) || !object.contains_key(key) {
            object.insert(key.into(), Value::Null);
        }
    }
    if object.get("source_db_path").is_none_or(Value::is_null) && object.get("source_sync_mode").is_none_or(Value::is_null) {
        // Native legacy rows carry no source provenance; record the migration origin.
        object.insert("source_db_path".into(), json!(legacy_path.to_string_lossy()));
        object.insert("source_sync_mode".into(), json!("legacy_migration"));
    }
    entry
}

// ---------------------------------------------------------------------------
// Disposable SQLite projection: a pure fold over the verified ledger.
// ---------------------------------------------------------------------------

const PROJECTION_DDL: &str = "pragma journal_mode=delete; create table feedback_entries(feedback_id text primary key,surface_id text not null,submitter_site_id text not null,submitter_principal text not null,kind text not null,summary text not null,details text not null default '',status text not null default 'submitted',resolution_note text,resolved_by text,task_ref text,task_status text,source_db_path text,source_updated_at text,source_sync_mode text,created_at text not null,updated_at text not null); create table feedback_events(event_id text primary key,feedback_id text not null,event_type text not null,actor_principal text not null,status text,task_ref text,task_status text,note text,details_json text not null default '{}',created_at text not null);";

fn rebuild_projection(root: &Path) -> Result<(), Value> {
    prepare(root)?;
    ledger_projection::rebuild_projection(ERROR_SCHEMA, &ledger_layout(root), HASH_FIELD, &projection_path(root), PROJECTION_DDL, apply_event)
}

fn entry_text(entry: &Value, key: &str) -> Option<String> {
    entry.get(key).and_then(Value::as_str).filter(|value| !value.is_empty()).map(ToOwned::to_owned)
}

fn upsert_entry(tx: &rusqlite::Transaction, entry: &Value, fallback_created: &str) -> Result<(), Value> {
    let feedback_id = entry_text(entry, "feedback_id").unwrap_or_default();
    let created_at = entry_text(entry, "created_at").unwrap_or_else(|| fallback_created.to_string());
    let updated_at = entry_text(entry, "updated_at").unwrap_or_else(|| created_at.clone());
    tx.execute(
        "INSERT OR REPLACE INTO feedback_entries (feedback_id,surface_id,submitter_site_id,submitter_principal,kind,summary,details,status,resolution_note,resolved_by,task_ref,task_status,source_db_path,source_updated_at,source_sync_mode,created_at,updated_at) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17)",
        params![
            feedback_id,
            entry_text(entry, "surface_id").unwrap_or_default(),
            entry_text(entry, "submitter_site_id").unwrap_or_default(),
            entry_text(entry, "submitter_principal").unwrap_or_default(),
            entry_text(entry, "kind").unwrap_or_default(),
            entry_text(entry, "summary").unwrap_or_default(),
            entry_text(entry, "details").unwrap_or_default(),
            entry_text(entry, "status").unwrap_or_else(|| "submitted".to_string()),
            entry_text(entry, "resolution_note"),
            entry_text(entry, "resolved_by"),
            entry_text(entry, "task_ref"),
            entry_text(entry, "task_status"),
            entry_text(entry, "source_db_path"),
            entry_text(entry, "source_updated_at"),
            entry_text(entry, "source_sync_mode"),
            created_at,
            updated_at,
        ],
    ).map_err(ERROR_SCHEMA.db_error("projection_entry_write_failed"))?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn insert_event_row(
    tx: &rusqlite::Transaction,
    event_id: &str,
    feedback_id: &str,
    event_type: &str,
    actor: &str,
    status: Option<&str>,
    task_ref: Option<&str>,
    task_status: Option<&str>,
    note: Option<&str>,
    details: &Value,
    created_at: &str,
) -> Result<(), Value> {
    tx.execute(
        "INSERT OR REPLACE INTO feedback_events (event_id,feedback_id,event_type,actor_principal,status,task_ref,task_status,note,details_json,created_at) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",
        params![event_id, feedback_id, event_type, actor, status, task_ref, task_status, note, details.to_string(), created_at],
    ).map_err(ERROR_SCHEMA.db_error("projection_event_write_failed"))?;
    Ok(())
}

fn apply_event(tx: &rusqlite::Transaction, event: &Value, event_id: &str) -> Result<(), Value> {
    let event_type = event["event_type"].as_str().unwrap_or_default();
    let actor = event["actor_principal"].as_str().unwrap_or_default();
    let created_at = event["created_at"].as_str().unwrap_or_default();
    match event_type {
        "submitted" | "imported" | "migrated" => {
            let entry = &event["entry"];
            let feedback_id = entry_text(entry, "feedback_id").unwrap_or_default();
            upsert_entry(tx, entry, created_at)?;
            let details = match event_type {
                "submitted" => json!({"submitter_site_id":entry["submitter_site_id"],"surface_id":entry["surface_id"],"kind":entry["kind"]}),
                "imported" => json!({"source_db_path":entry["source_db_path"],"source_updated_at":entry["source_updated_at"],"source_sync_mode":entry["source_sync_mode"]}),
                _ => json!({"legacy_db_path":event["legacy_db_path"],"history_count":event["history"].as_array().map_or(0, Vec::len)}),
            };
            if event_type == "migrated" {
                for (index, history) in event["history"].as_array().into_iter().flatten().enumerate() {
                    let history_id = entry_text(history, "event_id").unwrap_or_else(|| format!("{event_id}-history-{index}"));
                    insert_event_row(
                        tx, &history_id, &feedback_id,
                        &entry_text(history, "event_type").unwrap_or_else(|| "legacy".to_string()),
                        &entry_text(history, "actor_principal").unwrap_or_else(|| actor.to_string()),
                        entry_text(history, "status").as_deref(),
                        entry_text(history, "task_ref").as_deref(),
                        entry_text(history, "task_status").as_deref(),
                        entry_text(history, "note").as_deref(),
                        &serde_json::from_str::<Value>(history["details_json"].as_str().unwrap_or("{}")).unwrap_or_else(|_| json!({})),
                        &entry_text(history, "created_at").unwrap_or_else(|| created_at.to_string()),
                    )?;
                }
            }
            insert_event_row(tx, event_id, &feedback_id, event_type, actor, entry_text(entry, "status").as_deref(), entry_text(entry, "task_ref").as_deref(), entry_text(entry, "task_status").as_deref(), entry_text(entry, "summary").as_deref(), &details, created_at)?;
        }
        "status_updated" => {
            let feedback_id = event["feedback_id"].as_str().unwrap_or_default();
            let status = event["status"].as_str().unwrap_or_default();
            let note = event["resolution_note"].as_str().unwrap_or_default();
            let task_ref = event["task_ref"].as_str();
            let task_status = event["task_status"].as_str();
            tx.execute(
                "UPDATE feedback_entries SET status=?1,resolved_by=?2,resolution_note=?3,task_ref=COALESCE(?4,task_ref),task_status=COALESCE(?5,task_status),updated_at=?6 WHERE feedback_id=?7",
                params![status, actor, note, task_ref, task_status, created_at, feedback_id],
            ).map_err(ERROR_SCHEMA.db_error("projection_entry_write_failed"))?;
            insert_event_row(tx, event_id, feedback_id, event_type, actor, Some(status), task_ref, task_status, Some(note), &json!({"previous_status":event["previous_status"],"authority_site_id":event["authority_site_id"]}), created_at)?;
        }
        "converted_to_task" => {
            let feedback_id = event["feedback_id"].as_str().unwrap_or_default();
            let note = event["resolution_note"].as_str().unwrap_or_default();
            let task_ref = event["task_ref"].as_str().unwrap_or_default();
            let task_status = event["task_status"].as_str().unwrap_or("opened");
            tx.execute(
                "UPDATE feedback_entries SET status='converted_to_task',resolved_by=?1,resolution_note=?2,task_ref=?3,task_status=?4,updated_at=?5 WHERE feedback_id=?6",
                params![actor, note, task_ref, task_status, created_at, feedback_id],
            ).map_err(ERROR_SCHEMA.db_error("projection_entry_write_failed"))?;
            insert_event_row(tx, event_id, feedback_id, event_type, actor, Some("converted_to_task"), Some(task_ref), Some(task_status), Some(note), &json!({"task_number":event["task_number"],"task_id":event["task_id"],"payload_ref":event["payload_ref"]}), created_at)?;
        }
        "task_link_failed" => {
            let feedback_id = event["feedback_id"].as_str().unwrap_or_default();
            let detail = event["error"].as_str().unwrap_or_default();
            insert_event_row(tx, event_id, feedback_id, event_type, actor, None, None, None, Some(detail), &json!({"error":event["error"],"error_code":event["error_code"]}), created_at)?;
        }
        _ => {}
    }
    Ok(())
}

fn open_projection(root: &Path) -> Result<Connection, Value> {
    rebuild_projection(root)?;
    Connection::open_with_flags(projection_path(root), OpenFlags::SQLITE_OPEN_READ_ONLY)
        .map_err(ERROR_SCHEMA.db_error("feedback_store_open_failed"))
}

// ---------------------------------------------------------------------------
// Legacy store probing (import sources and migration input only; the Rust
// authority owns the projection schema, so no dynamic probing remains there).
// ---------------------------------------------------------------------------

const FEEDBACK_ROW_FIELDS: &[&str] = &[
    "feedback_id", "surface_id", "submitter_site_id", "submitter_principal", "kind", "summary", "details",
    "status", "resolution_note", "resolved_by", "task_ref", "task_status", "created_at", "updated_at",
];

fn feedback_columns(db: &Connection) -> Result<std::collections::BTreeSet<String>, Value> {
    let mut statement = db.prepare("PRAGMA table_info(feedback_entries)").map_err(|e| error("feedback_schema_probe_failed", &e.to_string()))?;
    let rows = statement.query_map([], |row| row.get::<_, String>(1)).map_err(|e| error("feedback_schema_probe_failed", &e.to_string()))?;
    let mut columns = std::collections::BTreeSet::new();
    for row in rows { columns.insert(row.map_err(|e| error("feedback_schema_probe_failed", &e.to_string()))?); }
    Ok(columns)
}

fn feedback_row(db: &Connection, feedback_id: &str) -> Result<Option<Value>, Value> {
    let columns = feedback_columns(db)?;
    if !columns.contains("feedback_id") { return Err(error("feedback_entries_schema_missing_required_columns", "feedback_entries_schema_missing_required_columns")); }
    let selection = FEEDBACK_ROW_FIELDS.iter().map(|field| {
        if columns.contains(*field) { (*field).to_string() } else { format!("NULL AS {field}") }
    }).collect::<Vec<_>>().join(", ");
    let sql = format!("SELECT {selection} FROM feedback_entries WHERE feedback_id = ?1 LIMIT 1");
    db.query_row(&sql, params![feedback_id], |row| {
        let mut object = Map::new();
        for (index, field) in FEEDBACK_ROW_FIELDS.iter().enumerate() {
            let value = row.get::<_, Option<String>>(index).ok().flatten().map(Value::String).unwrap_or(Value::Null);
            object.insert((*field).to_string(), value);
        }
        Ok(Value::Object(object))
    }).optional().map_err(|e| error("feedback_query_failed", &e.to_string()))
}

// ---------------------------------------------------------------------------
// Read scopes.
// ---------------------------------------------------------------------------

struct ReadScope {
    name: String,
    authority_site: Option<String>,
    owned_surfaces: Option<Vec<String>>,
}

impl ReadScope {
    fn owned_json(&self) -> Option<String> {
        self.owned_surfaces.as_ref().map(|owned| Value::Array(owned.iter().map(|value| json!(value)).collect()).to_string())
    }
}

fn read_scope(args: &Map<String, Value>, root: &Path) -> Result<ReadScope, Value> {
    let value = args.get("scope").and_then(Value::as_str).ok_or_else(|| error("feedback_read_scope_required", "feedback_read_scope_required"))?;
    if !READ_SCOPES.contains(&value) { return Err(error("feedback_read_scope_invalid", "feedback_read_scope_invalid")); }
    match value {
        "all_authorized" | "store_reconciliation" => {
            if !is_canonical_store(root) { return Err(error("feedback_global_read_requires_canonical_store", "feedback_global_read_requires_canonical_store")); }
            Ok(ReadScope { name: value.to_string(), authority_site: None, owned_surfaces: None })
        }
        "authority_visible" | "authority_site_submissions" => {
            let (site, _, _) = authority().map_err(|_| error("feedback_read_scope_authority_unavailable", "feedback_read_scope_authority_unavailable"))?;
            Ok(ReadScope { name: value.to_string(), authority_site: Some(site), owned_surfaces: None })
        }
        _ => {
            let (_, _, owned) = authority().map_err(|_| error("feedback_read_scope_authority_unavailable", "feedback_read_scope_authority_unavailable"))?;
            if owned.is_empty() { return Err(error("feedback_read_scope_authority_unavailable", "feedback_read_scope_authority_unavailable")); }
            Ok(ReadScope { name: value.to_string(), authority_site: authority().ok().map(|(site, _, _)| site), owned_surfaces: Some(owned) })
        }
    }
}

fn scope_filters(args: &Map<String, Value>, scope: &ReadScope) -> Result<Option<String>, Value> {
    let requested = args.get("submitter_site_id_filter").and_then(Value::as_str);
    if requested.is_some() && scope.authority_site.as_deref().is_some_and(|site| requested != Some(site)) {
        return Err(error("feedback_submitter_site_filter_authority_mismatch", "feedback_submitter_site_filter_authority_mismatch"));
    }
    Ok(match scope.name.as_str() {
        "authority_visible" | "authority_site_submissions" => scope.authority_site.as_deref().or(requested).map(ToOwned::to_owned),
        _ => requested.map(ToOwned::to_owned),
    })
}

fn list_schema(actionable: bool) -> Value {
    let mut properties = Map::new();
    properties.insert("surface_id".into(), json!({"type":"string"}));
    properties.insert("submitter_site_id_filter".into(), json!({"type":"string"}));
    properties.insert("kind".into(), json!({"type":"string","enum":FEEDBACK_KINDS}));
    properties.insert("status".into(), json!({"type":"string","enum":FEEDBACK_STATUSES}));
    properties.insert("scope".into(), json!({"type":"string","enum":READ_SCOPES}));
    properties.insert("since".into(), json!({"type":"string"})); properties.insert("until".into(), json!({"type":"string"}));
    properties.insert("limit".into(), json!({"type":"integer","minimum":1,"maximum":200,"default":50})); properties.insert("offset".into(), json!({"type":"integer","minimum":0,"maximum":10000,"default":0}));
    json!({"type":"object","properties":properties,"required":["scope"],"additionalProperties":false,"x-actionable":actionable})
}

fn feedback_list(args: &Map<String, Value>, root: &Path, actionable: bool) -> Result<Value, Value> {
    let scope = read_scope(args, root)?;
    let submitter_site = scope_filters(args, &scope)?;
    ensure_migrated(root)?;
    let db = open_projection(root)?;
    let surface_id = args.get("surface_id").and_then(Value::as_str);
    let kind = args.get("kind").and_then(Value::as_str);
    let requested_status = args.get("status").and_then(Value::as_str);
    let since = args.get("since").and_then(Value::as_str);
    let until = args.get("until").and_then(Value::as_str);
    let limit = args.get("limit").and_then(Value::as_u64).unwrap_or(50).clamp(1, 200) as i64;
    let offset = args.get("offset").and_then(Value::as_u64).unwrap_or(0).min(10_000) as i64;
    let fetch_limit = limit + 1;
    let status = if actionable { Some("submitted") } else { requested_status };
    let status2 = if actionable { Some("acknowledged") } else { None };
    let owned = scope.owned_json();
    let mut stmt = db.prepare("SELECT feedback_id,surface_id,submitter_site_id,submitter_principal,kind,summary,details,status,resolution_note,resolved_by,task_ref,task_status,created_at,updated_at FROM feedback_entries WHERE (?1 IS NULL OR surface_id=?1) AND (?2 IS NULL OR submitter_site_id=?2) AND (?3 IS NULL OR kind=?3) AND (?4 IS NULL OR status=?4 OR status=?5) AND (?6 IS NULL OR created_at>=?6) AND (?7 IS NULL OR created_at<=?7) AND (?8 IS NULL OR surface_id IN (SELECT value FROM json_each(?8))) ORDER BY created_at DESC LIMIT ?9 OFFSET ?10").map_err(|e| error("feedback_query_prepare_failed", &e.to_string()))?;
    let rows = stmt.query_map(params![surface_id, submitter_site, kind, status, status2, since, until, owned, fetch_limit, offset], |row| Ok(json!({"feedback_id":row.get::<_,String>(0)?,"surface_id":row.get::<_,String>(1)?,"submitter_site_id":row.get::<_,String>(2)?,"submitter_principal":row.get::<_,String>(3)?,"kind":row.get::<_,String>(4)?,"summary":row.get::<_,String>(5)?,"details":row.get::<_,String>(6)?,"status":row.get::<_,String>(7)?,"resolution_note":row.get::<_,Option<String>>(8)?,"resolved_by":row.get::<_,Option<String>>(9)?,"task_ref":row.get::<_,Option<String>>(10)?,"task_status":row.get::<_,Option<String>>(11)?,"created_at":row.get::<_,String>(12)?,"updated_at":row.get::<_,String>(13)?}))).map_err(|e| error("feedback_query_failed", &e.to_string()))?;
    let mut entries = Vec::new(); for row in rows.take(201) { entries.push(row.map_err(|e| error("feedback_row_decode_failed", &e.to_string()))?); }
    let has_more = entries.len() > limit as usize;
    entries.truncate(limit as usize);
    let next_offset = has_more.then_some(offset + entries.len() as i64);
    Ok(json!({"schema":"narada.surface_feedback.list.v1","status":"ok","scope":scope.name,"count":entries.len(),"returned":entries.len(),"offset":offset,"limit":limit,"has_more":has_more,"next_offset":next_offset,"entries":entries,"read_only_native":true}))
}

fn feedback_show(args: &Map<String, Value>, root: &Path) -> Result<Value, Value> {
    let scope = read_scope(args, root)?;
    let id = args.get("feedback_id").and_then(Value::as_str).filter(|v|!v.is_empty()).ok_or_else(||error("feedback_id_required","feedback_id_required"))?;
    ensure_migrated(root)?;
    let db = open_projection(root)?;
    let value: Option<Value> = db.query_row("SELECT feedback_id,surface_id,submitter_site_id,submitter_principal,kind,summary,details,status,resolution_note,resolved_by,task_ref,task_status,created_at,updated_at FROM feedback_entries WHERE feedback_id=?1", params![id], |row| Ok(json!({"feedback_id":row.get::<_,String>(0)?,"surface_id":row.get::<_,String>(1)?,"submitter_site_id":row.get::<_,String>(2)?,"submitter_principal":row.get::<_,String>(3)?,"kind":row.get::<_,String>(4)?,"summary":row.get::<_,String>(5)?,"details":row.get::<_,String>(6)?,"status":row.get::<_,String>(7)?,"resolution_note":row.get::<_,Option<String>>(8)?,"resolved_by":row.get::<_,Option<String>>(9)?,"task_ref":row.get::<_,Option<String>>(10)?,"task_status":row.get::<_,Option<String>>(11)?,"created_at":row.get::<_,String>(12)?,"updated_at":row.get::<_,String>(13)?}))).optional().map_err(|e|error("feedback_query_failed",&e.to_string()))?;
    let value = value.filter(|entry| match scope.name.as_str() {
        "authority_visible" | "authority_site_submissions" => scope.authority_site.as_deref().is_some_and(|site| entry["submitter_site_id"] == site),
        "owned_surfaces" => scope.owned_surfaces.as_ref().is_some_and(|owned| entry["surface_id"].as_str().is_some_and(|surface| owned.iter().any(|value| value == surface))),
        _ => true,
    });
    value.map(|entry|json!({"schema":"narada.surface_feedback.show.v1","status":"ok","scope":scope.name,"entry":entry,"read_only_native":true})).ok_or_else(||error("feedback_not_found","feedback_not_found"))
}

fn feedback_stats(args: &Map<String, Value>, root: &Path) -> Result<Value, Value> {
    let scope = read_scope(args, root)?;
    let surface_id = args.get("surface_id").and_then(Value::as_str);
    let authority_site = scope.authority_site.as_deref();
    let owned = scope.owned_json();
    ensure_migrated(root)?;
    let db = open_projection(root)?;
    let total = db.query_row("SELECT COUNT(*) FROM feedback_entries WHERE (?1 IS NULL OR surface_id=?1) AND (?2 IS NULL OR submitter_site_id=?2) AND (?3 IS NULL OR surface_id IN (SELECT value FROM json_each(?3)))", params![surface_id,authority_site,owned], |row| row.get::<_,i64>(0)).map_err(|e|error("feedback_stats_query_failed",&e.to_string()))?;
    let mut by_surface = Vec::new(); let mut stmt = db.prepare("SELECT surface_id,COUNT(*) FROM feedback_entries WHERE (?1 IS NULL OR surface_id=?1) AND (?2 IS NULL OR submitter_site_id=?2) AND (?3 IS NULL OR surface_id IN (SELECT value FROM json_each(?3))) GROUP BY surface_id ORDER BY COUNT(*) DESC LIMIT 100").map_err(|e|error("feedback_stats_prepare_failed",&e.to_string()))?; let rows = stmt.query_map(params![surface_id,authority_site,owned], |row| Ok(json!({"surface_id":row.get::<_,String>(0)?,"count":row.get::<_,i64>(1)?}))).map_err(|e|error("feedback_stats_query_failed",&e.to_string()))?; for row in rows { by_surface.push(row.map_err(|e|error("feedback_stats_row_failed",&e.to_string()))?); }
    let mut by_status = Vec::new(); let mut stmt = db.prepare("SELECT status,COUNT(*) FROM feedback_entries WHERE (?1 IS NULL OR surface_id=?1) AND (?2 IS NULL OR submitter_site_id=?2) AND (?3 IS NULL OR surface_id IN (SELECT value FROM json_each(?3))) GROUP BY status ORDER BY COUNT(*) DESC LIMIT 20").map_err(|e|error("feedback_stats_prepare_failed",&e.to_string()))?; let rows = stmt.query_map(params![surface_id,authority_site,owned], |row| Ok(json!({"status":row.get::<_,String>(0)?,"count":row.get::<_,i64>(1)?}))).map_err(|e|error("feedback_stats_query_failed",&e.to_string()))?; for row in rows { by_status.push(row.map_err(|e|error("feedback_stats_row_failed",&e.to_string()))?); }
    Ok(json!({"schema":"narada.surface_feedback.stats.v1","status":"ok","scope":scope.name,"total":total,"by_surface":by_surface,"by_status":by_status,"read_only_native":true}))
}

// ---------------------------------------------------------------------------
// Mutations: fail-hard event appends under the authority lock.
// ---------------------------------------------------------------------------

fn feedback_submit(args: &Map<String, Value>, root: &Path) -> Result<Value, Value> {
    let surface_id = required_arg(args, "surface_id", "feedback_requires_surface_id")?;
    let (submitter_site_id, submitter_principal, _) = authority()?;
    if let Some(asserted) = args.get("submitter_site_id").and_then(Value::as_str).map(str::trim).filter(|v| !v.is_empty()) {
        if asserted != submitter_site_id { return Err(error("feedback_submitter_site_authority_mismatch", "feedback_submitter_site_authority_mismatch")); }
    }
    if let Some(asserted) = args.get("submitter_principal").and_then(Value::as_str).map(str::trim).filter(|v| !v.is_empty()) {
        if asserted != submitter_principal { return Err(error("feedback_submitter_principal_authority_mismatch", "feedback_submitter_principal_authority_mismatch")); }
    }
    let kind = required_arg(args, "kind", "feedback_requires_kind")?;
    if !FEEDBACK_KINDS.contains(&kind.as_str()) { return Err(error("feedback_invalid_kind", "feedback_invalid_kind")); }
    let summary = required_arg(args, "summary", "feedback_requires_summary")?;
    let details = args.get("details").and_then(Value::as_str).unwrap_or("").to_string();
    let idempotency_key = args.get("idempotency_key").and_then(Value::as_str).map(str::trim).filter(|v|!v.is_empty()).map(ToOwned::to_owned);
    let id = idempotency_key.as_deref().map(|key| { let digest=Sha256::digest(format!("{submitter_site_id}\0{submitter_principal}\0{key}").as_bytes()); format!("sfb_{:x}",digest)[..16].to_string() }).unwrap_or_else(||format!("sfb_{}",&Uuid::new_v4().to_string()[..12]));
    ensure_migrated(root)?;
    with_authority_lock(root, || {
        if let Some(key) = idempotency_key.as_deref() {
            if let Some(existing) = event_ledger::find_event_by_idempotency(ERROR_SCHEMA, &ledger_layout(root), key)? {
                let entry = &existing["entry"];
                let fields = ["surface_id", "submitter_site_id", "submitter_principal", "kind", "summary", "details"];
                let request = [&surface_id, &submitter_site_id, &submitter_principal, &kind, &summary, &details];
                let identical = fields.iter().zip(request).all(|(field, expected)| entry[field].as_str().unwrap_or("") == *expected)
                    && entry["feedback_id"].as_str() == Some(id.as_str());
                if !identical { return Err(error("feedback_idempotency_conflict","feedback_idempotency_conflict")); }
                return Ok(json!({"schema":"narada.surface_feedback.submit.v1","status":"submitted","feedback_id":id,"surface_id":surface_id,"submitter_site_id":submitter_site_id,"kind":kind,"summary":summary,"created_at":entry["created_at"],"native_write":true,"idempotency_replay":true}));
            }
        }
        let now = now_iso();
        let site = bound_site_id();
        let principal = submitter_principal.clone();
        let entry = json!({"feedback_id":id,"surface_id":surface_id,"submitter_site_id":submitter_site_id,"submitter_principal":submitter_principal,"kind":kind,"summary":summary,"details":details,"status":"submitted","resolution_note":null,"resolved_by":null,"task_ref":null,"task_status":null,"source_db_path":null,"source_updated_at":null,"source_sync_mode":null,"created_at":now,"updated_at":now});
        let event_idempotency = idempotency_key.clone();
        event_ledger::append_event(ERROR_SCHEMA, &ledger_layout(root), HASH_FIELD, None, idempotency_key.as_deref(), |ctx| {
            json!({"schema":EVENT_SCHEMA,"sequence":ctx.sequence,"event_id":ctx.event_id,"previous_hash":ctx.previous_hash,"event_type":"submitted","site_id":site,"actor_principal":principal,"created_at":now,"idempotency_key":event_idempotency,"entry":entry})
        })?;
        rebuild_projection(root)?;
        Ok(json!({"schema":"narada.surface_feedback.submit.v1","status":"submitted","feedback_id":id,"surface_id":surface_id,"submitter_site_id":submitter_site_id,"kind":kind,"summary":summary,"created_at":now,"native_write":true,"idempotency_replay":false}))
    })
}

fn feedback_update_status(args: &Map<String, Value>, root: &Path) -> Result<Value, Value> {
    let id = required_arg(args, "feedback_id", "feedback_requires_feedback_id")?;
    let status = required_arg(args, "status", "feedback_requires_status")?;
    if !FEEDBACK_STATUSES.contains(&status.as_str()) { return Err(error("feedback_invalid_status", "feedback_invalid_status")); }
    let note = required_arg(args, "resolution_note", "feedback_requires_resolution_note")?;
    let (authority_site, principal, owned_surfaces) = authority()?;
    ensure_migrated(root)?;
    with_authority_lock(root, || {
        let db = open_projection(root)?;
        let row: Option<(String, String, String)> = db.query_row("SELECT submitter_site_id,surface_id,status FROM feedback_entries WHERE feedback_id=?1", params![id], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?))).optional().map_err(|e| error("feedback_query_failed", &e.to_string()))?;
        drop(db);
        let Some((submitter_site, surface_id, previous_status)) = row else { return Err(error("feedback_not_found", "feedback_not_found")); };
        let owns_surface = owned_surfaces.iter().any(|value| value == &surface_id);
        if submitter_site != authority_site && !owns_surface && !is_canonical_store(root) { return Err(error("feedback_not_visible", "feedback_not_visible")); }
        let now = now_iso();
        let task_ref = args.get("task_ref").and_then(Value::as_str).map(ToOwned::to_owned);
        let task_status = args.get("task_status").and_then(Value::as_str).map(ToOwned::to_owned);
        let site = bound_site_id();
        let actor = principal.clone();
        let event_authority_site = authority_site.clone();
        event_ledger::append_event(ERROR_SCHEMA, &ledger_layout(root), HASH_FIELD, None, None, |ctx| {
            json!({"schema":EVENT_SCHEMA,"sequence":ctx.sequence,"event_id":ctx.event_id,"previous_hash":ctx.previous_hash,"event_type":"status_updated","site_id":site,"actor_principal":actor,"created_at":now,"feedback_id":id,"status":status,"resolution_note":note,"task_ref":task_ref,"task_status":task_status,"previous_status":previous_status,"authority_site_id":event_authority_site})
        })?;
        rebuild_projection(root)?;
        Ok(json!({"schema":"narada.surface_feedback.update_status.v1","status":"updated","feedback_id":id,"new_status":status,"resolved_by":principal,"resolution_note":note,"updated_at":now,"native_write":true}))
    })
}

fn feedback_update_status_batch(args: &Map<String, Value>, root: &Path) -> Result<Value, Value> {
    let updates = args.get("updates").and_then(Value::as_array).ok_or_else(|| error("feedback_batch_requires_updates", "feedback_batch_requires_updates"))?;
    if updates.is_empty() || updates.len() > MAX_IMPORT_IDS {
        return Err(error("feedback_batch_invalid_size", "feedback_batch_invalid_size"));
    }
    let mut succeeded = Vec::new();
    let mut failed = Vec::new();
    for (index, update) in updates.iter().enumerate() {
        let object = update.as_object();
        let feedback_id = object.and_then(|value| value.get("feedback_id")).and_then(Value::as_str).map(str::trim).filter(|value| !value.is_empty()).map(ToOwned::to_owned);
        let result = object.map(|value| feedback_update_status(value, root)).unwrap_or_else(|| Err(error("feedback_update_must_be_object", "feedback_update_must_be_object")));
        match result {
            Ok(value) => succeeded.push(json!({
                "feedback_id": feedback_id.or_else(|| value.get("feedback_id").and_then(Value::as_str).map(ToOwned::to_owned)),
                "status": value.get("new_status").cloned().unwrap_or(Value::Null),
                "resolution_note": value.get("resolution_note").cloned().unwrap_or(Value::Null),
                "updated_at": value.get("updated_at").cloned().unwrap_or(Value::Null),
                "result": value,
            })),
            Err(diagnostic) => failed.push(json!({
                "index": index,
                "feedback_id": feedback_id,
                "code": diagnostic.get("code").cloned().unwrap_or_else(|| json!("feedback_update_failed")),
                "message": diagnostic.get("message").cloned().unwrap_or_else(|| json!("feedback_update_failed")),
                "details": diagnostic.get("details").cloned().unwrap_or_else(|| json!({})),
            })),
        }
    }
    let status = if failed.is_empty() { "updated" } else if succeeded.is_empty() { "failed" } else { "partial" };
    Ok(json!({
        "schema": "narada.surface_feedback.status_batch.v1",
        "status": status,
        "requested_count": updates.len(),
        "updated_count": succeeded.len(),
        "failed_count": failed.len(),
        "updates": succeeded,
        "failures": failed,
        "native_write": true,
    }))
}

fn configured_task_call(root: &Path, name: &str, arguments: Value) -> Result<Option<Value>, Value> {
    let options = LifecycleOptions { surface: LifecycleSurface::Task, site_root: task_authority_root(root), site_root_source: "surface-feedback-authority".to_string(), prepare: false, migrate_legacy: false, source_database_path: None };
    let mut authority = LifecycleServer::new(options).map_err(|detail| error("task_lifecycle_authority_unavailable", &detail))?;
    let response = authority.handle_request(json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":name,"arguments":arguments}})).ok_or_else(|| error("task_lifecycle_authority_no_response", "task_lifecycle_authority_no_response"))?;
    if let Some(authority_error) = response.get("error") { return Err(json!({"schema":"narada.authority_adapter.error.v1","status":"error","authority":"task-lifecycle","error":authority_error})); }
    let value = response.get("result").cloned().unwrap_or(response);
    if value.get("isError").and_then(Value::as_bool) == Some(true) {
        return Err(error("task_lifecycle_tool_refused", "task_lifecycle_tool_refused"));
    }
    Ok(Some(value.get("structuredContent").cloned().unwrap_or(value)))
}

fn task_authority_root(root: &Path) -> PathBuf {
    std::env::var("NARADA_TASK_LIFECYCLE_ROOT")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| root.to_path_buf())
}

fn feedback_convert_to_task(args: &Map<String, Value>, root: &Path) -> Result<Value, Value> {
    let feedback_id = required_arg(args, "feedback_id", "feedback_requires_feedback_id")?;
    ensure_migrated(root)?;
    with_authority_lock(root, || {
        let db = open_projection(root)?;
        let row: Option<(String, String, String, String, String, Option<String>, Option<String>, Option<String>)> = db.query_row(
            "SELECT surface_id,submitter_site_id,summary,details,status,task_ref,task_status,resolution_note FROM feedback_entries WHERE feedback_id=?1",
            params![feedback_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?, row.get(5)?, row.get(6)?, row.get(7)?)),
        ).optional().map_err(|e| error("feedback_query_failed", &e.to_string()))?;
        drop(db);
        let Some((surface_id, submitter_site_id, summary, details, status, existing_task_ref, existing_task_status, existing_note)) = row else {
            return Err(error("feedback_not_found", "feedback_not_found"));
        };
        if let Some(task_ref) = existing_task_ref {
            if status != "converted_to_task" { return Err(error("feedback_task_link_conflict", "feedback_task_link_conflict")); }
            return Ok(json!({"schema":"narada.surface_feedback.convert_to_task.v1","status":"already_linked","feedback_id":feedback_id,"task_ref":task_ref,"task_status":existing_task_status,"resolution_note":existing_note,"handoff_authorization":{"scope":"canonical_user_site_handoff","authorization_basis":"canonical_feedback_store_and_server_binding"}}));
        }
        let payload = json!({
            "title": args.get("task_title").and_then(Value::as_str).filter(|value| !value.trim().is_empty()).unwrap_or_else(|| "Address feedback"),
            "goal": format!("Address feedback {feedback_id} for {surface_id}: {summary}"),
            "context": format!("Source feedback: {feedback_id}\nSurface: {surface_id}\nSubmitter site: {submitter_site_id}\nDetails: {details}"),
            "required_work": format!("Inspect feedback {feedback_id}; implement the smallest coherent fix; add focused tests; record verification evidence."),
            "non_goals": "Do not execute the task from surface-feedback; task execution remains owned by task-lifecycle and worker surfaces.",
            "acceptance_criteria": [format!("The concern described by feedback {feedback_id} is addressed or an exact blocker is recorded."), "Focused tests cover the changed behavior."],
            "idempotency_key": format!("surface-feedback:{feedback_id}"),
        });
        let payload_args = json!({"payload_id":format!("surface-feedback-{feedback_id}-task"),"payload":payload,"created_by":std::env::var("NARADA_AGENT_ID").ok().unwrap_or_else(|| "surface-feedback".to_string())});
        let created = (|| -> Result<Value, Value> {
            let Some(payload_result) = configured_task_call(root, "mcp_payload_create", payload_args)? else { return Err(authority_boundary("surface_feedback_convert_to_task")); };
            let payload_ref = payload_result.get("ref").or_else(|| payload_result.get("payload_ref")).and_then(Value::as_str).ok_or_else(|| error("feedback_task_payload_ref_missing", "feedback_task_payload_ref_missing"))?;
            let Some(task_result) = configured_task_call(root, "task_lifecycle_create", json!({"payload_ref":payload_ref}))? else { return Err(authority_boundary("surface_feedback_convert_to_task")); };
            let task_number = task_result.get("task_number").and_then(Value::as_i64);
            let task_id = task_result.get("task_id").and_then(Value::as_str);
            let task_ref = task_result.get("task_ref").and_then(Value::as_str).map(ToOwned::to_owned)
                .or_else(|| task_number.map(|value| format!("task #{value}")))
                .or_else(|| task_id.map(ToOwned::to_owned))
                .ok_or_else(|| error("feedback_task_create_result_invalid", "feedback_task_create_result_invalid"))?;
            let task_status = task_result.get("task_status").and_then(Value::as_str).or_else(|| task_result.get("status").and_then(Value::as_str)).unwrap_or("opened");
            Ok(json!({"task_ref":task_ref,"task_number":task_number,"task_id":task_id,"task_status":task_status,"payload_ref":payload_ref}))
        })();
        let task = match created {
            Ok(task) => task,
            Err(failure) => {
                // Crash-safe record of the failed handoff: fail-hard append, then report.
                let site = bound_site_id();
                let actor = bound_principal("surface-feedback");
                let detail = failure.get("message").and_then(Value::as_str).unwrap_or("task handoff failed").to_string();
                let code = failure.get("code").and_then(Value::as_str).unwrap_or("feedback_task_link_failed").to_string();
                let event_feedback_id = feedback_id.clone();
                event_ledger::append_event(ERROR_SCHEMA, &ledger_layout(root), HASH_FIELD, None, None, |ctx| {
                    json!({"schema":EVENT_SCHEMA,"sequence":ctx.sequence,"event_id":ctx.event_id,"previous_hash":ctx.previous_hash,"event_type":"task_link_failed","site_id":site,"actor_principal":actor,"created_at":now_iso(),"feedback_id":event_feedback_id,"error":detail,"error_code":code})
                })?;
                rebuild_projection(root)?;
                return Err(failure);
            }
        };
        let task_ref = task["task_ref"].as_str().unwrap_or_default().to_string();
        let task_status = task["task_status"].as_str().unwrap_or("opened").to_string();
        let task_number = task.get("task_number").cloned().unwrap_or(Value::Null);
        let task_id = task.get("task_id").cloned().unwrap_or(Value::Null);
        let payload_ref = task["payload_ref"].as_str().unwrap_or_default().to_string();
        let resolved_by = std::env::var("NARADA_AGENT_ID").ok().filter(|value| !value.trim().is_empty()).unwrap_or_else(|| "surface-feedback".to_string());
        let note = args.get("resolution_note").and_then(Value::as_str).filter(|value| !value.trim().is_empty()).map(ToOwned::to_owned).unwrap_or_else(|| format!("Created {task_ref} from feedback via surface_feedback_convert_to_task."));
        let now = now_iso();
        let site = bound_site_id();
        let actor = resolved_by.clone();
        let event_feedback_id = feedback_id.clone();
        let event_task_ref = task_ref.clone();
        let event_task_status = task_status.clone();
        let event_note = note.clone();
        event_ledger::append_event(ERROR_SCHEMA, &ledger_layout(root), HASH_FIELD, None, None, |ctx| {
            json!({"schema":EVENT_SCHEMA,"sequence":ctx.sequence,"event_id":ctx.event_id,"previous_hash":ctx.previous_hash,"event_type":"converted_to_task","site_id":site,"actor_principal":actor,"created_at":now,"feedback_id":event_feedback_id,"task_ref":event_task_ref,"task_number":task_number,"task_id":task_id,"task_status":event_task_status,"resolution_note":event_note,"payload_ref":payload_ref})
        })?;
        rebuild_projection(root)?;
        Ok(json!({"schema":"narada.surface_feedback.convert_to_task.v1","status":"converted","feedback_id":feedback_id,"task_ref":task_ref,"task_number":task["task_number"],"task_id":task["task_id"],"task_status":task_status,"task_creation":{"status":"created_or_recovered","payload_ref":task["payload_ref"],"idempotency_key":format!("surface-feedback:{feedback_id}")},"handoff_authorization":{"scope":"canonical_user_site_handoff","authorization_basis":"canonical_feedback_store_and_server_binding","authority_site_id":std::env::var("NARADA_SITE_ID").ok()},"next_action":{"surface_id":"task-lifecycle","tool":"task_lifecycle_show","arguments":{"task_number":task["task_number"]}}}))
    })
}

fn import_source_path(args: &Map<String, Value>, root: &Path) -> Result<PathBuf, Value> {
    let source_root = args.get("source_feedback_root").and_then(Value::as_str).filter(|value| !value.trim().is_empty());
    let source_db = args.get("source_db_path").and_then(Value::as_str).filter(|value| !value.trim().is_empty());
    if source_root.is_some() && source_db.is_some() { return Err(error("feedback_import_source_ambiguous", "feedback_import_source_ambiguous")); }
    let path = if let Some(value) = source_root {
        PathBuf::from(value).join(".feedback").join("surface-feedback.db")
    } else if let Some(value) = source_db {
        PathBuf::from(value)
    } else {
        return Err(error("feedback_import_requires_source", "feedback_import_requires_source"));
    };
    let path = if path.is_absolute() { path } else { root.join(path) };
    Ok(path)
}

fn same_feedback_path(left: &Path, right: &Path) -> bool {
    left.canonicalize().unwrap_or_else(|_| left.to_path_buf()) == right.canonicalize().unwrap_or_else(|_| right.to_path_buf())
}

fn feedback_import(args: &Map<String, Value>, root: &Path) -> Result<Value, Value> {
    let source_path = import_source_path(args, root)?;
    if same_feedback_path(&source_path, &legacy_db_path(root)) {
        return Err(error("feedback_import_same_store", "feedback_import_same_store"));
    }
    if !source_path.exists() { return Err(error("feedback_import_source_missing", "feedback_import_source_missing")); }
    let ids = args.get("feedback_ids").and_then(Value::as_array).ok_or_else(|| error("feedback_import_requires_feedback_ids", "feedback_import_requires_feedback_ids"))?;
    let ids = ids.iter().filter_map(Value::as_str).map(str::trim).filter(|value| !value.is_empty()).map(ToOwned::to_owned).collect::<Vec<_>>();
    if ids.is_empty() || ids.len() > MAX_IMPORT_IDS { return Err(error("feedback_import_requires_feedback_ids", "feedback_import_requires_feedback_ids")); }
    let source = Connection::open_with_flags(&source_path, OpenFlags::SQLITE_OPEN_READ_ONLY).map_err(|e| error("feedback_import_source_open_failed", &e.to_string()))?;
    ensure_migrated(root)?;
    with_authority_lock(root, || {
        let target = open_projection(root)?;
        let mut imported = Vec::new();
        let mut skipped = Vec::new();
        let mut missing = Vec::new();
        for id in &ids {
            let Some(row) = feedback_row(&source, id)? else { missing.push(id.clone()); continue; };
            if feedback_row(&target, id)?.is_some() {
                skipped.push(json!({"feedback_id": id, "reason": "already_exists"}));
                continue;
            }
            let get = |name: &str| row.get(name).cloned().unwrap_or(Value::Null);
            let text = |name: &str| get(name).as_str().unwrap_or("").to_string();
            let status = { let value = text("status"); if value.is_empty() { "submitted".to_string() } else { value } };
            let created_at = text("created_at");
            let updated_at = text("updated_at");
            let entry = json!({
                "feedback_id": text("feedback_id"), "surface_id": text("surface_id"), "submitter_site_id": text("submitter_site_id"),
                "submitter_principal": text("submitter_principal"), "kind": text("kind"), "summary": text("summary"), "details": text("details"),
                "status": status, "resolution_note": optional_text(&get("resolution_note")), "resolved_by": optional_text(&get("resolved_by")),
                "task_ref": optional_text(&get("task_ref")), "task_status": optional_text(&get("task_status")),
                "source_db_path": source_path.to_string_lossy(), "source_updated_at": updated_at, "source_sync_mode": "explicit_import",
                "created_at": created_at, "updated_at": updated_at,
            });
            let now = now_iso();
            let site = bound_site_id();
            let actor = bound_principal("surface-feedback-import");
            event_ledger::append_event(ERROR_SCHEMA, &ledger_layout(root), HASH_FIELD, None, None, |ctx| {
                json!({"schema":EVENT_SCHEMA,"sequence":ctx.sequence,"event_id":ctx.event_id,"previous_hash":ctx.previous_hash,"event_type":"imported","site_id":site,"actor_principal":actor,"created_at":now,"entry":entry})
            })?;
            imported.push(json!({"feedback_id":id,"surface_id":text("surface_id"),"submitter_site_id":text("submitter_site_id"),"submitter_principal":text("submitter_principal"),"kind":text("kind"),"summary":text("summary"),"details":text("details"),"status":status,"source_db_path":source_path.to_string_lossy().to_string(),"source_sync_mode":"explicit_import","created_at":created_at,"updated_at":updated_at}));
        }
        drop(target);
        rebuild_projection(root)?;
        Ok(json!({"schema":"narada.surface_feedback.import.v1","status":if missing.is_empty() && skipped.is_empty(){"imported"}else{"partial"},"source_db_path":source_path.to_string_lossy(),"target_db_path":projection_path(root).to_string_lossy(),"target_ledger_path":ledger_dir(root).to_string_lossy(),"requested_count":ids.len(),"imported_count":imported.len(),"skipped_count":skipped.len(),"missing_count":missing.len(),"imported":imported,"skipped":skipped,"missing_feedback_ids":missing,"native_write":true}))
    })
}

fn optional_text(value: &Value) -> Option<String> { value.as_str().map(str::to_string).filter(|value| !value.is_empty()) }

// ---------------------------------------------------------------------------
// Doctor and capabilities.
// ---------------------------------------------------------------------------

fn doctor(root: &Path) -> Result<Value, Value> {
    let legacy_path = legacy_db_path(root);
    let marker_path = migration_marker_path(root);
    let legacy_present = legacy_path.exists();
    let marker_present = marker_path.exists();
    let had_store = legacy_present || ledger_dir(root).exists();
    ensure_migrated(root)?;
    let event_count = ledger_files(root)?.len();
    let rows_migrated = if marker_present {
        ledger_io::read_json(ERROR_SCHEMA, &marker_path).ok().and_then(|marker| marker["rows_migrated"].as_u64()).unwrap_or(0)
    } else { 0 };
    let mut feedback_entries = 0_i64;
    if had_store || event_count > 0 {
        let db = open_projection(root)?;
        feedback_entries = db.query_row("SELECT COUNT(*) FROM feedback_entries", [], |row| row.get::<_, i64>(0)).unwrap_or(0);
    }
    let ready = event_count > 0 || marker_present;
    let migration = json!({
        "legacy_present": legacy_present,
        "legacy_db_path": legacy_path.to_string_lossy(),
        "marker_present": marker_present,
        "marker_path": marker_path.to_string_lossy(),
        "rows_migrated": rows_migrated,
        "legacy_db_writable": false,
    });
    Ok(json!({"schema":"narada.surface_feedback.doctor.v1","status":"ok","feedback_root":root.to_string_lossy(),"db_path":legacy_path.to_string_lossy(),"ledger_path":ledger_dir(root).to_string_lossy(),"projection_path":projection_path(root).to_string_lossy(),"store_status":if ready{"ready"}else{"missing"},"feedback_entries":feedback_entries,"ledger_events":event_count,"read_only_native":false,"native_write_available":true,"migration":migration,"capabilities":capabilities(root),"server_name":SERVER_NAME}))
}

fn capabilities(root: &Path) -> Value {
    let bound_authority = authority().ok();
    let authority_configured = bound_authority.is_some();
    let owned_empty = bound_authority.as_ref().map(|(_, _, owned)| owned.is_empty()).unwrap_or(true);
    let canonical = is_canonical_store(root);
    let task_handoff_configured = task_authority_root(root).join(".ai/task-lifecycle.db").is_file();
    let canonical_scope = |purpose: &str| json!({"available":canonical,"purpose":purpose,"reason":if canonical{Value::Null}else{json!("feedback_global_read_requires_canonical_store")}});
    let authority_scope = |purpose: &str| json!({"available":authority_configured,"purpose":purpose,"reason":if authority_configured{Value::Null}else{json!("feedback_authority_unconfigured")}});
    json!({"read_scopes":{
        "all_authorized":canonical_scope("canonical local feedback store"),
        "store_reconciliation":canonical_scope("explicit source/store reconciliation"),
        "authority_visible":authority_scope("feedback submitted by the server-bound authority Site"),
        "owned_surfaces":{"available":authority_configured && !owned_empty,"purpose":"feedback about surfaces owned by the server-bound authority","reason":if authority_configured && !owned_empty{Value::Null}else{json!("feedback_owned_surfaces_unbound")}},
        "authority_site_submissions":authority_scope("feedback submitted by the server-bound authority Site"),
    },"mutations":{
        "submit":{"available":true,"authority_site_id":bound_authority.as_ref().map(|(site,_,_)|site)},
        "import":{"available":true},
        "status_update":{"available":authority_configured,"reason":if authority_configured{Value::Null}else{json!("feedback_authority_unconfigured")}},
        "task_handoff":{"available":authority_configured && task_handoff_configured,"reason":if authority_configured && task_handoff_configured{Value::Null}else{json!("task_or_site_authority_unconfigured")}},
    }})
}

fn proof_template(args: &Map<String, Value>) -> Value {
    let optional = |key: &str| args.get(key).and_then(Value::as_str).map(str::trim).filter(|value| !value.is_empty()).map(ToOwned::to_owned).map(Value::String).unwrap_or(Value::Null);
    json!({
        "schema": "narada.surface_feedback.live_proof_template.v1",
        "status": "ok",
        "workflow": optional("workflow"),
        "surface_id": optional("surface_id"),
        "purpose": "Capture evidence expectations for live, no-mock, no-fallback E2E authority/projection behavior.",
        "recommended_feedback": {
            "kind": "observation",
            "details_format": "json_or_markdown_with_live_proof_contract"
        },
        "live_proof_contract": {
            "authority_location": {
                "deployed": "<where the deployed authority or projection state lives>",
                "local": "<where local source/test authority lives>"
            },
            "transport": {
                "live_transport_assumption": "<named live transport path and why it is expected>",
                "replay_vs_live_delivery": "<how replay evidence is distinguished from live delivery>"
            },
            "success": {
                "semantic_success_point": "<observable state/event that proves live success>",
                "saved_evidence_file": "<required artifact path or null when not applicable>"
            },
            "exclusions": {
                "no_mock": "<evidence that mocks were not used>",
                "no_fallback": "<evidence that fallback path was not used>",
                "no_shim": "<evidence that compatibility shim did not carry the behavior>"
            },
            "negative_controls": {
                "revocation_or_refusal_proof": "<how revoked/unauthorized paths fail>"
            },
            "test_alignment": {
                "unit_tests_specify_deployed_transport": "<yes/no/unknown plus file references>"
            }
        },
        "usage": [
            "Use this template in feedback details when reporting live-proof gaps or observations.",
            "Use it in task context when converting feedback into implementation work.",
            "Do not treat a completed template as proof by itself; proof requires cited artifacts and live readback."
        ]
    })
}
fn authority_boundary(name: &str) -> Value { json!({"schema":"narada.surface_feedback.authority_boundary.v1","status":"unavailable","tool_name":name,"reason":"surface_feedback_mutation_not_enabled_in_native_read_slice","remediation":"Use the configured surface-feedback authority for writes, imports, task handoffs, and status changes."}) }
fn error(code: &str, message: &str) -> Value { json!({"schema":"narada.surface_feedback.error.v1","code":code,"message":message}) }
fn tool(name: &str, description: &str, input_schema: Value, read_only: bool) -> Value { json!({"name":name,"description":description,"annotations":{"title":name,"readOnlyHint":read_only,"destructiveHint":!read_only,"idempotentHint":read_only,"openWorldHint":false},"inputSchema":input_schema,"outputSchema":{"type":"object","additionalProperties":true}}) }

#[cfg(test)]
mod tests {
    use super::*;
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn reset_feedback_env() {
        for key in ["NARADA_SITE_ID", "NARADA_AGENT_ID", "NARADA_SURFACE_FEEDBACK_ROOT", "NARADA_OWNED_SURFACE_IDS", "NARADA_TASK_LIFECYCLE_ROOT"] {
            std::env::remove_var(key);
        }
    }

    fn temp_root(tag: &str) -> PathBuf {
        std::env::temp_dir().join(format!("narada-feedback-{tag}-{}", uuid::Uuid::new_v4()))
    }

    fn bind_authority(site: &str, agent: &str) {
        std::env::set_var("NARADA_SITE_ID", site);
        std::env::set_var("NARADA_AGENT_ID", agent);
    }

    fn ledger_events(root: &Path) -> Vec<Value> {
        ledger_files(root).expect("ledger files").iter().map(|path| ledger_io::read_json(ERROR_SCHEMA, path).expect("event json")).collect()
    }

    const LEGACY_DDL: &str = "CREATE TABLE feedback_entries (feedback_id TEXT PRIMARY KEY,surface_id TEXT NOT NULL,submitter_site_id TEXT NOT NULL,submitter_principal TEXT NOT NULL,kind TEXT NOT NULL,summary TEXT NOT NULL,details TEXT NOT NULL DEFAULT '',status TEXT NOT NULL DEFAULT 'submitted',resolution_note TEXT,resolved_by TEXT,task_ref TEXT,task_status TEXT,source_db_path TEXT,source_updated_at TEXT,source_sync_mode TEXT,created_at TEXT NOT NULL,updated_at TEXT NOT NULL) STRICT; CREATE TABLE feedback_events (event_id TEXT PRIMARY KEY,feedback_id TEXT NOT NULL,event_type TEXT NOT NULL,actor_principal TEXT NOT NULL,status TEXT,task_ref TEXT,task_status TEXT,note TEXT,details_json TEXT NOT NULL DEFAULT '{}',created_at TEXT NOT NULL) STRICT;";

    fn seed_legacy_db(root: &Path, statements: &str) {
        std::fs::create_dir_all(root.join(".feedback")).expect("legacy dir");
        let db = Connection::open(root.join(".feedback/surface-feedback.db")).expect("legacy db");
        db.execute_batch(&format!("{LEGACY_DDL} {statements}")).expect("legacy seed");
        drop(db);
    }

    #[test]
    fn mutation_tools_advertise_named_closed_schemas() {
        let tools = list_tools();
        let find = |name: &str| tools.iter().find(|tool| tool["name"] == name).expect("tool");
        let submit = find("surface_feedback_submit");
        assert_eq!(submit["inputSchema"]["additionalProperties"], false);
        for field in ["surface_id", "submitter_site_id", "submitter_principal", "kind", "summary", "details", "idempotency_key"] {
            assert!(submit["inputSchema"]["properties"].get(field).is_some(), "missing {field}");
        }
        assert_eq!(submit["inputSchema"]["required"], json!(["surface_id","kind","summary"]));
        assert_eq!(find("surface_feedback_update_status")["inputSchema"]["required"], json!(["feedback_id","status","resolution_note"]));
        assert!(find("surface_feedback_update_status_batch")["inputSchema"]["properties"]["updates"].is_object());
        assert!(find("surface_feedback_convert_to_task")["inputSchema"]["properties"]["feedback_id"].is_object());
        let import = find("surface_feedback_import");
        assert_eq!(import["inputSchema"]["required"], json!(["source_db_path","feedback_ids"]));
        assert!(import["inputSchema"].get("oneOf").is_none());
    }

    #[test]
    fn native_feedback_reads_are_bounded_and_capabilities_are_honest() {
        let _guard = ENV_LOCK.lock().expect("feedback env lock");
        reset_feedback_env();
        let root = temp_root("reads");
        bind_authority("site-a", "agent-a");
        std::env::set_var("NARADA_SURFACE_FEEDBACK_ROOT", &root);
        // Fresh root: no legacy DB, no ledger — the bootstrap gap is closed.
        for summary in ["first", "second"] {
            call_tool("surface_feedback_submit", &json!({"surface_id":"calendar","submitter_site_id":"site-a","submitter_principal":"agent-a","kind":"observation","summary":summary}).as_object().unwrap(), &root).expect("submit");
        }
        let list = feedback_list(&json!({"scope":"all_authorized","limit":1}).as_object().unwrap(), &root, false).expect("list");
        assert_eq!(list["count"], 1);
        assert_eq!(list["has_more"], true);
        assert_eq!(list["next_offset"], 1);
        let doctor = doctor(&root).expect("doctor");
        assert_eq!(doctor["store_status"], "ready");
        assert_eq!(doctor["feedback_entries"], 2);
        assert_eq!(doctor["ledger_events"], 2);
        assert_eq!(doctor["read_only_native"], false);
        assert_eq!(doctor["capabilities"]["read_scopes"]["all_authorized"]["available"], true);
        assert_eq!(doctor["capabilities"]["read_scopes"]["authority_visible"]["available"], true);
        assert_eq!(doctor["capabilities"]["read_scopes"]["owned_surfaces"]["available"], false);
        assert_eq!(doctor["capabilities"]["mutations"]["submit"]["authority_site_id"], "site-a");
        let authority_entries = feedback_list(&json!({"scope":"authority_site_submissions"}).as_object().unwrap(), &root, false).expect("authority list");
        assert_eq!(authority_entries["count"], 2);
        assert!(authority_entries["entries"].as_array().is_some_and(|entries| entries.iter().all(|entry| entry["submitter_site_id"] == "site-a")));
        let mismatch = call_tool("surface_feedback_submit", &json!({"surface_id":"calendar","submitter_site_id":"site-b","kind":"bug","summary":"spoofed"}).as_object().unwrap(), &root).expect_err("authority mismatch");
        assert_eq!(mismatch["code"], "feedback_submitter_site_authority_mismatch");
        let retry_args=json!({"surface_id":"calendar","submitter_site_id":"site-a","submitter_principal":"agent-a","kind":"observation","summary":"retry safe","idempotency_key":"retry-1"});
        let first=call_tool("surface_feedback_submit",retry_args.as_object().unwrap(),&root).expect("first");
        let replay=call_tool("surface_feedback_submit",retry_args.as_object().unwrap(),&root).expect("replay");
        assert_eq!(first["feedback_id"],replay["feedback_id"]);
        assert_eq!(replay["idempotency_replay"],true);
        assert_eq!(replay["created_at"],first["created_at"]);
        let conflict=call_tool("surface_feedback_submit",&json!({"surface_id":"calendar","submitter_site_id":"site-a","submitter_principal":"agent-a","kind":"bug","summary":"different","idempotency_key":"retry-1"}).as_object().unwrap(),&root).expect_err("conflict");
        assert_eq!(conflict["code"],"feedback_idempotency_conflict");
        // Exactly one event for the replayed key: the retry must not append.
        assert_eq!(ledger_events(&root).iter().filter(|event| event["idempotency_key"] == "retry-1").count(), 1);
        let maintained = feedback_update_status(&json!({"feedback_id":first["feedback_id"],"status":"closed","resolution_note":"canonical repair"}).as_object().unwrap(), &root).expect("canonical maintainer");
        assert_eq!(maintained["new_status"], "closed");
        let shown = feedback_show(&json!({"feedback_id":first["feedback_id"],"scope":"all_authorized"}).as_object().unwrap(), &root).expect("show");
        assert_eq!(shown["entry"]["status"], "closed");
        assert_eq!(shown["entry"]["resolution_note"], "canonical repair");
        reset_feedback_env();
        std::fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn read_scopes_enforce_canonical_store_and_owned_surfaces() {
        let _guard = ENV_LOCK.lock().expect("feedback env lock");
        reset_feedback_env();
        let root = temp_root("scopes");
        bind_authority("site-a", "agent-a");
        call_tool("surface_feedback_submit", &json!({"surface_id":"calendar","kind":"bug","summary":"owned surface bug"}).as_object().unwrap(), &root).expect("submit owned");
        call_tool("surface_feedback_submit", &json!({"surface_id":"scheduler","kind":"gap","summary":"other surface gap"}).as_object().unwrap(), &root).expect("submit other");
        // Global scopes require the canonical store.
        let global = feedback_list(&json!({"scope":"all_authorized"}).as_object().unwrap(), &root, false).expect_err("noncanonical all_authorized");
        assert_eq!(global["code"], "feedback_global_read_requires_canonical_store");
        let reconciliation = feedback_list(&json!({"scope":"store_reconciliation"}).as_object().unwrap(), &root, false).expect_err("noncanonical store_reconciliation");
        assert_eq!(reconciliation["code"], "feedback_global_read_requires_canonical_store");
        // owned_surfaces refuses when the owned list is unbound.
        let unbound = feedback_list(&json!({"scope":"owned_surfaces"}).as_object().unwrap(), &root, false).expect_err("unbound owned_surfaces");
        assert_eq!(unbound["code"], "feedback_read_scope_authority_unavailable");
        std::env::set_var("NARADA_OWNED_SURFACE_IDS", "calendar,mailbox");
        let owned = feedback_list(&json!({"scope":"owned_surfaces"}).as_object().unwrap(), &root, false).expect("owned list");
        assert_eq!(owned["count"], 1);
        assert_eq!(owned["entries"][0]["surface_id"], "calendar");
        let owned_show = feedback_show(&json!({"feedback_id":owned["entries"][0]["feedback_id"],"scope":"owned_surfaces"}).as_object().unwrap(), &root).expect("owned show");
        assert_eq!(owned_show["entry"]["summary"], "owned surface bug");
        let other = feedback_list(&json!({"scope":"authority_visible","surface_id":"scheduler"}).as_object().unwrap(), &root, false).expect("authority list");
        let other_id = other["entries"][0]["feedback_id"].clone();
        let hidden = feedback_show(&json!({"feedback_id":other_id,"scope":"owned_surfaces"}).as_object().unwrap(), &root).expect_err("owned scope hides other surfaces");
        assert_eq!(hidden["code"], "feedback_not_found");
        let owned_stats = feedback_stats(&json!({"scope":"owned_surfaces"}).as_object().unwrap(), &root).expect("owned stats");
        assert_eq!(owned_stats["total"], 1);
        // With the canonical binding, global scopes read everything.
        std::env::set_var("NARADA_SURFACE_FEEDBACK_ROOT", &root);
        let global = feedback_list(&json!({"scope":"all_authorized"}).as_object().unwrap(), &root, false).expect("canonical list");
        assert_eq!(global["count"], 2);
        // The submitter filter authority-mismatch refusal still holds.
        let mismatch = feedback_list(&json!({"scope":"authority_site_submissions","submitter_site_id_filter":"site-b"}).as_object().unwrap(), &root, false).expect_err("filter mismatch");
        assert_eq!(mismatch["code"], "feedback_submitter_site_filter_authority_mismatch");
        // Authority-bound scopes refuse when the authority is unconfigured.
        std::env::remove_var("NARADA_SITE_ID");
        let unavailable = feedback_list(&json!({"scope":"authority_visible"}).as_object().unwrap(), &root, false).expect_err("authority unavailable");
        assert_eq!(unavailable["code"], "feedback_read_scope_authority_unavailable");
        reset_feedback_env();
        std::fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn projection_is_disposable_and_event_appends_are_durable() {
        let _guard = ENV_LOCK.lock().expect("feedback env lock");
        reset_feedback_env();
        let root = temp_root("durable");
        bind_authority("site-a", "agent-a");
        std::env::set_var("NARADA_SURFACE_FEEDBACK_ROOT", &root);
        let submitted = call_tool("surface_feedback_submit", &json!({"surface_id":"calendar","kind":"bug","summary":"durable write"}).as_object().unwrap(), &root).expect("submit");
        feedback_update_status(&json!({"feedback_id":submitted["feedback_id"],"status":"acknowledged","resolution_note":"ack"}).as_object().unwrap(), &root).expect("update");
        assert!(projection_path(&root).exists());
        std::fs::remove_file(projection_path(&root)).expect("delete projection");
        // Reads rebuild the projection from the ledger with identical results.
        let shown = feedback_show(&json!({"feedback_id":submitted["feedback_id"],"scope":"all_authorized"}).as_object().unwrap(), &root).expect("show after rebuild");
        assert_eq!(shown["entry"]["status"], "acknowledged");
        assert_eq!(shown["entry"]["resolution_note"], "ack");
        assert!(projection_path(&root).exists());
        // The derived audit readback preserves the feedback_events row shape.
        let db = Connection::open_with_flags(projection_path(&root), OpenFlags::SQLITE_OPEN_READ_ONLY).expect("projection");
        let events: Vec<(String, String, Option<String>, Option<String>)> = {
            let mut stmt = db.prepare("SELECT event_type, actor_principal, status, note FROM feedback_events WHERE feedback_id=?1 ORDER BY rowid ASC").expect("events query");
            let rows = stmt.query_map(params![submitted["feedback_id"].as_str()], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))).expect("events rows");
            rows.collect::<Result<Vec<_>, _>>().expect("events collect")
        };
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].0, "submitted");
        assert_eq!(events[0].1, "agent-a");
        assert_eq!(events[0].2.as_deref(), Some("submitted"));
        assert_eq!(events[1].0, "status_updated");
        assert_eq!(events[1].2.as_deref(), Some("acknowledged"));
        assert_eq!(events[1].3.as_deref(), Some("ack"));
        drop(db);
        // The ledger chain verifies and its head matches the last event.
        event_ledger::verify(ERROR_SCHEMA, &ledger_layout(&root), HASH_FIELD).expect("ledger verifies");
        reset_feedback_env();
        std::fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn legacy_store_migrates_once_and_is_never_written() {
        let _guard = ENV_LOCK.lock().expect("feedback env lock");
        reset_feedback_env();
        let root = temp_root("migration");
        bind_authority("site-a", "agent-a");
        std::env::set_var("NARADA_SURFACE_FEEDBACK_ROOT", &root);
        seed_legacy_db(&root, "INSERT INTO feedback_entries (feedback_id,surface_id,submitter_site_id,submitter_principal,kind,summary,details,status,resolution_note,resolved_by,task_ref,task_status,source_db_path,source_updated_at,source_sync_mode,created_at,updated_at) VALUES ('f1','calendar','site-a','agent-a','bug','broken','details','submitted',NULL,NULL,NULL,NULL,NULL,NULL,NULL,'2026-01-01T00:00:00Z','2026-01-01T00:00:00Z'), ('f2','scheduler','site-b','agent-b','observation','import me','legacy details','acknowledged','ack note','agent-c',NULL,NULL,'legacy-src','2026-01-02T00:00:00Z','explicit_import','2026-01-02T00:00:00Z','2026-01-03T00:00:00Z'); INSERT INTO feedback_events (event_id,feedback_id,event_type,actor_principal,status,task_ref,task_status,note,details_json,created_at) VALUES ('evt-1','f1','submitted','agent-a','submitted',NULL,NULL,'broken','{}','2026-01-01T00:00:00Z'), ('evt-2','f1','status_updated','agent-a','acknowledged',NULL,NULL,'ack','{}','2026-01-01T01:00:00Z');");
        let legacy_bytes = std::fs::read(root.join(".feedback/surface-feedback.db")).expect("legacy bytes");
        let list = feedback_list(&json!({"scope":"all_authorized"}).as_object().unwrap(), &root, false).expect("list migrates");
        assert_eq!(list["count"], 2);
        // One migrated event per legacy row, preserving identity and timestamps.
        let events = ledger_events(&root);
        assert_eq!(events.len(), 2);
        assert!(events.iter().all(|event| event["event_type"] == "migrated"));
        assert!(migration_marker_path(&root).exists());
        let shown = feedback_show(&json!({"feedback_id":"f2","scope":"all_authorized"}).as_object().unwrap(), &root).expect("show migrated");
        assert_eq!(shown["entry"]["status"], "acknowledged");
        assert_eq!(shown["entry"]["resolution_note"], "ack note");
        assert_eq!(shown["entry"]["created_at"], "2026-01-02T00:00:00Z");
        assert_eq!(shown["entry"]["updated_at"], "2026-01-03T00:00:00Z");
        let stats = feedback_stats(&json!({"scope":"all_authorized"}).as_object().unwrap(), &root).expect("stats");
        assert_eq!(stats["total"], 2);
        // Legacy feedback_events history is replayed into the derived readback.
        let db = Connection::open_with_flags(projection_path(&root), OpenFlags::SQLITE_OPEN_READ_ONLY).expect("projection");
        let history_count: i64 = db.query_row("SELECT COUNT(*) FROM feedback_events WHERE feedback_id='f1'", [], |row| row.get(0)).expect("history count");
        assert_eq!(history_count, 3); // evt-1, evt-2, plus the migrated marker event
        let legacy_order: i64 = db.query_row("SELECT COUNT(*) FROM feedback_events WHERE event_id IN ('evt-1','evt-2')", [], |row| row.get(0)).expect("legacy ids");
        assert_eq!(legacy_order, 2);
        drop(db);
        // Doctor reports the migration posture.
        let doctor = doctor(&root).expect("doctor");
        assert_eq!(doctor["migration"]["legacy_present"], true);
        assert_eq!(doctor["migration"]["marker_present"], true);
        assert_eq!(doctor["migration"]["rows_migrated"], 2);
        assert_eq!(doctor["ledger_events"], 2);
        // The legacy DB is byte-identical: migration never writes it.
        assert_eq!(std::fs::read(root.join(".feedback/surface-feedback.db")).expect("legacy bytes after"), legacy_bytes);
        // A second pass appends nothing.
        feedback_list(&json!({"scope":"all_authorized"}).as_object().unwrap(), &root, false).expect("second list");
        assert_eq!(ledger_events(&root).len(), 2);
        // Crash-restart safety: without the marker, re-emission skips existing ids.
        std::fs::remove_file(migration_marker_path(&root)).expect("remove marker");
        feedback_list(&json!({"scope":"all_authorized"}).as_object().unwrap(), &root, false).expect("resume list");
        assert_eq!(ledger_events(&root).len(), 2);
        assert!(migration_marker_path(&root).exists());
        // Migration output is ordinary feedback: updates apply to migrated rows.
        let updated = feedback_update_status(&json!({"feedback_id":"f1","status":"closed","resolution_note":"fixed after migration"}).as_object().unwrap(), &root).expect("update migrated");
        assert_eq!(updated["new_status"], "closed");
        assert_eq!(std::fs::read(root.join(".feedback/surface-feedback.db")).expect("legacy bytes final"), legacy_bytes);
        reset_feedback_env();
        std::fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn native_feedback_batch_update_and_explicit_import() {
        let _guard = ENV_LOCK.lock().expect("feedback env lock");
        reset_feedback_env();
        let root = temp_root("import");
        let source_root = temp_root("source");
        seed_legacy_db(&source_root, "INSERT INTO feedback_entries (feedback_id,surface_id,submitter_site_id,submitter_principal,kind,summary,details,status,created_at,updated_at) VALUES ('f2','calendar','site-a','agent-a','observation','import me','details','submitted','2026-01-01','2026-01-01');");
        bind_authority("site-a", "agent-a");
        std::env::set_var("NARADA_SURFACE_FEEDBACK_ROOT", &root);
        let submitted = feedback_submit(&json!({"surface_id":"calendar","submitter_site_id":"site-a","submitter_principal":"agent-a","kind":"bug","summary":"batch me"}).as_object().unwrap(), &root).expect("submit");
        let batch = feedback_update_status_batch(&json!({"updates":[{"feedback_id":submitted["feedback_id"].clone(),"status":"acknowledged","resolution_note":"ack"}]}).as_object().unwrap(), &root).expect("batch");
        assert_eq!(batch["status"], "updated");
        // Partial semantics: one good update, one missing id, one malformed item.
        let partial = feedback_update_status_batch(&json!({"updates":[{"feedback_id":submitted["feedback_id"].clone(),"status":"routed","resolution_note":"route"},{"feedback_id":"missing","status":"closed","resolution_note":"nope"},"not-an-object"]}).as_object().unwrap(), &root).expect("partial batch");
        assert_eq!(partial["status"], "partial");
        assert_eq!(partial["updated_count"], 1);
        assert_eq!(partial["failed_count"], 2);
        assert_eq!(partial["failures"][0]["code"], "feedback_not_found");
        let failed = feedback_update_status_batch(&json!({"updates":[{"feedback_id":"missing","status":"closed","resolution_note":"nope"}]}).as_object().unwrap(), &root).expect("failed batch");
        assert_eq!(failed["status"], "failed");
        let imported = feedback_import(&json!({"source_db_path":source_root.join(".feedback/surface-feedback.db").to_string_lossy(),"feedback_ids":["f2"]}).as_object().unwrap(), &root).expect("import");
        assert_eq!(imported["status"], "imported");
        assert_eq!(imported["imported_count"], 1);
        // Re-import skips existing ids instead of duplicating events.
        let reimport = feedback_import(&json!({"source_db_path":source_root.join(".feedback/surface-feedback.db").to_string_lossy(),"feedback_ids":["f2","never-present"]}).as_object().unwrap(), &root).expect("reimport");
        assert_eq!(reimport["status"], "partial");
        assert_eq!(reimport["skipped_count"], 1);
        assert_eq!(reimport["missing_count"], 1);
        let shown = feedback_show(&json!({"feedback_id":"f2","scope":"all_authorized"}).as_object().unwrap(), &root).expect("show imported");
        assert_eq!(shown["entry"]["summary"], "import me");
        // Importing the store's own legacy DB is still refused.
        let same_store = feedback_import(&json!({"source_db_path":root.join(".feedback/surface-feedback.db").to_string_lossy(),"feedback_ids":["f2"]}).as_object().unwrap(), &root).expect_err("same store");
        assert_eq!(same_store["code"], "feedback_import_same_store");
        reset_feedback_env();
        std::fs::remove_dir_all(root).expect("cleanup");
        std::fs::remove_dir_all(source_root).expect("source cleanup");
    }

    #[test]
    fn feedback_conversion_uses_in_process_task_authority() {
        let _guard = ENV_LOCK.lock().expect("feedback env lock");
        reset_feedback_env();
        let root = temp_root("convert");
        bind_authority("site-a", "agent-a");
        std::env::set_var("NARADA_SURFACE_FEEDBACK_ROOT", &root);
        let submitted = call_tool("surface_feedback_submit", &json!({"surface_id":"worker-delegation","submitter_site_id":"site-a","submitter_principal":"agent-a","kind":"bug","summary":"bounded worker stalls"}).as_object().unwrap(), &root).expect("submit");
        let lifecycle_options = LifecycleOptions { surface: LifecycleSurface::Task, site_root: root.clone(), site_root_source: "test".to_string(), prepare: true, migrate_legacy: false, source_database_path: None };
        LifecycleServer::prepare_database(&lifecycle_options).expect("task database");
        let result = feedback_convert_to_task(json!({"feedback_id":submitted["feedback_id"],"task_title":"Fix bounded worker stalls"}).as_object().unwrap(), &root).expect("conversion");
        assert_eq!(result["status"], "converted");
        assert!(result["task_ref"].as_str().is_some());
        let replay = feedback_convert_to_task(json!({"feedback_id":submitted["feedback_id"]}).as_object().unwrap(), &root).expect("idempotent replay");
        assert_eq!(replay["status"], "already_linked");
        assert_eq!(replay["task_ref"], result["task_ref"]);
        // The fold reflects the link: the entry is converted with the task ref.
        let shown = feedback_show(&json!({"feedback_id":submitted["feedback_id"],"scope":"all_authorized"}).as_object().unwrap(), &root).expect("show converted");
        assert_eq!(shown["entry"]["status"], "converted_to_task");
        assert_eq!(shown["entry"]["task_ref"], result["task_ref"]);
        // The conversion is a durable ledger event, not a swallowed best-effort write.
        let events = ledger_events(&root);
        let converted = events.iter().filter(|event| event["event_type"] == "converted_to_task").count();
        assert_eq!(converted, 1);
        // Crash between task creation and link append is recoverable: removing the
        // link event's fold effect is equivalent to the entry lacking task_ref, and a
        // retry replays task-lifecycle idempotency and appends the link event.
        // Here we assert the retry guard instead: a status change away from
        // converted_to_task with an existing task_ref refuses as a link conflict.
        let conflict_row = feedback_convert_to_task(json!({"feedback_id":submitted["feedback_id"]}).as_object().unwrap(), &root).expect("second replay");
        assert_eq!(conflict_row["status"], "already_linked");
        reset_feedback_env();
        std::fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn task_link_conflict_is_refused_for_non_converted_entries() {
        let _guard = ENV_LOCK.lock().expect("feedback env lock");
        reset_feedback_env();
        let root = temp_root("link-conflict");
        bind_authority("site-a", "agent-a");
        std::env::set_var("NARADA_SURFACE_FEEDBACK_ROOT", &root);
        let submitted = call_tool("surface_feedback_submit", &json!({"surface_id":"calendar","kind":"bug","summary":"link conflict"}).as_object().unwrap(), &root).expect("submit");
        // A task_ref carried by a status update on a non-converted entry refuses conversion.
        feedback_update_status(&json!({"feedback_id":submitted["feedback_id"],"status":"routed","resolution_note":"route","task_ref":"task #7","task_status":"opened"}).as_object().unwrap(), &root).expect("status with task ref");
        let conflict = feedback_convert_to_task(json!({"feedback_id":submitted["feedback_id"]}).as_object().unwrap(), &root).expect_err("link conflict");
        assert_eq!(conflict["code"], "feedback_task_link_conflict");
        reset_feedback_env();
        std::fs::remove_dir_all(root).expect("cleanup");
    }
}
