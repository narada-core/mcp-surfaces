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

