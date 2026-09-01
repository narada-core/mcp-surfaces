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

