use rusqlite::{params, Connection, OpenFlags, OptionalExtension};
use serde_json::{json, Map, Value};
use std::path::Path;

const SERVER_NAME: &str = "surface-feedback-mcp";
const FEEDBACK_KINDS: &[&str] = &["bug", "improvement", "gap", "observation"];
const FEEDBACK_STATUSES: &[&str] = &["submitted", "acknowledged", "routed", "converted_to_task", "closed"];
const READ_SCOPES: &[&str] = &["all_authorized", "store_reconciliation", "authority_visible", "owned_surfaces", "authority_site_submissions"];

pub fn list_tools() -> Vec<Value> {
    vec![
        guidance_tool(),
        tool("surface_feedback_doctor", "Inspect surface feedback storage posture and backing store path.", json!({"type":"object","properties":{},"additionalProperties":false}), true),
        tool("surface_feedback_live_proof_template", "Return a reusable structured template for live no-mock proof feedback.", json!({"type":"object","properties":{"workflow":{"type":"string"},"surface_id":{"type":"string"}},"additionalProperties":false}), true),
        tool("surface_feedback_list", "List feedback entries using an explicit server-bound read scope.", list_schema(false), true),
        tool("surface_feedback_actionable_queue", "Return a bounded actionable feedback queue using an explicit read scope.", list_schema(true), true),
        tool("surface_feedback_show", "Show one feedback entry using an explicit read scope.", json!({"type":"object","properties":{"feedback_id":{"type":"string"},"scope":{"type":"string","enum":READ_SCOPES}},"required":["feedback_id","scope"],"additionalProperties":false}), true),
        tool("surface_feedback_stats", "Return bounded feedback counts by surface, kind, and status.", json!({"type":"object","properties":{"surface_id":{"type":"string"},"scope":{"type":"string","enum":READ_SCOPES}},"required":["scope"],"additionalProperties":false}), true),
        tool("surface_feedback_submit", "Submit feedback through the owning surface-feedback authority.", json!({"type":"object","additionalProperties":true}), false),
        tool("surface_feedback_update_status", "Update feedback status through the owning authority.", json!({"type":"object","additionalProperties":true}), false),
        tool("surface_feedback_update_status_batch", "Update multiple feedback entries through the owning authority.", json!({"type":"object","additionalProperties":true}), false),
        tool("surface_feedback_convert_to_task", "Create a task handoff through the owning authority.", json!({"type":"object","additionalProperties":true}), false),
        tool("surface_feedback_import", "Import feedback through the owning authority.", json!({"type":"object","additionalProperties":true}), false),
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
        "surface_feedback_submit" | "surface_feedback_update_status" | "surface_feedback_update_status_batch" | "surface_feedback_convert_to_task" | "surface_feedback_import" => Err(authority_boundary(name)),
        _ => Err(error("unknown_tool", &format!("unknown_tool:{name}"))),
    }
}

fn guidance_tool() -> Value { tool("surface_feedback_guidance", "Show model-facing operating guidance for surface feedback workflows.", json!({"type":"object","properties":{"workflow":{"type":"string"},"tool":{"type":"string"}},"additionalProperties":false}), true) }
fn guidance(args: &Map<String, Value>) -> Value { json!({"schema":"narada.mcp_surface.guidance.v0","status":"ok","surface_id":"surface-feedback","guidance_tool":"surface_feedback_guidance","purpose":"Inspect bounded feedback evidence with explicit read scope.","requested":{"workflow":args.get("workflow").cloned().unwrap_or(Value::Null),"tool":args.get("tool").cloned().unwrap_or(Value::Null)},"first_use":["Call surface_feedback_doctor first.","Choose a server-bound read scope explicitly.","Use list or actionable_queue for bounded discovery, then show before mutation.","Task conversion and status changes remain owner-authorized."],"boundaries":["The native slice opens the feedback SQLite store read-only.","It never creates tasks, changes statuses, imports entries, or executes tasks.","Authority and provenance scopes remain with the owning surface-feedback process."]}) }

fn feedback_path(root: &Path) -> std::path::PathBuf { root.join(".feedback/surface-feedback.db") }

fn open_db(root: &Path) -> Result<Connection, Value> {
    let path = feedback_path(root);
    if !path.exists() { return Err(error("feedback_store_missing", "feedback_store_missing")); }
    Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY).map_err(|e| error("feedback_store_open_failed", &e.to_string()))
}

fn doctor(root: &Path) -> Result<Value, Value> {
    let path = feedback_path(root);
    if !path.exists() { return Ok(json!({"schema":"narada.surface_feedback.doctor.v1","status":"ok","feedback_root":root.to_string_lossy(),"db_path":path.to_string_lossy(),"store_status":"missing","read_only_native":true,"server_name":SERVER_NAME})); }
    let db = open_db(root)?;
    let table: Option<String> = db.query_row("SELECT name FROM sqlite_master WHERE type='table' AND name='feedback_entries'", [], |row| row.get(0)).optional().map_err(|e| error("feedback_store_probe_failed", &e.to_string()))?;
    let rows = if table.is_some() { db.query_row("SELECT COUNT(*) FROM feedback_entries", [], |row| row.get::<_, i64>(0)).unwrap_or(0) } else { 0 };
    Ok(json!({"schema":"narada.surface_feedback.doctor.v1","status":"ok","feedback_root":root.to_string_lossy(),"db_path":path.to_string_lossy(),"store_status":if table.is_some(){"ready"}else{"schema_missing"},"feedback_entries":rows,"read_only_native":true,"server_name":SERVER_NAME}))
}

fn scope(args: &Map<String, Value>) -> Result<String, Value> {
    let value = args.get("scope").and_then(Value::as_str).ok_or_else(|| error("feedback_read_scope_required", "feedback_read_scope_required"))?;
    if !READ_SCOPES.contains(&value) { return Err(error("feedback_read_scope_invalid", "feedback_read_scope_invalid")); }
    if !matches!(value, "all_authorized" | "store_reconciliation") { return Err(error("feedback_read_scope_authority_unavailable", "feedback_read_scope_authority_unavailable")); }
    Ok(value.to_string())
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
    let read_scope = scope(args)?;
    let db = open_db(root)?;
    let surface_id = args.get("surface_id").and_then(Value::as_str);
    let submitter_site = args.get("submitter_site_id_filter").and_then(Value::as_str);
    let kind = args.get("kind").and_then(Value::as_str);
    let requested_status = args.get("status").and_then(Value::as_str);
    let since = args.get("since").and_then(Value::as_str);
    let until = args.get("until").and_then(Value::as_str);
    let limit = args.get("limit").and_then(Value::as_u64).unwrap_or(50).clamp(1, 200) as i64;
    let offset = args.get("offset").and_then(Value::as_u64).unwrap_or(0).min(10_000) as i64;
    let status = if actionable { Some("submitted") } else { requested_status };
    let status2 = if actionable { Some("acknowledged") } else { None };
    let mut stmt = db.prepare("SELECT feedback_id,surface_id,submitter_site_id,submitter_principal,kind,summary,details,status,resolution_note,resolved_by,task_ref,task_status,created_at,updated_at FROM feedback_entries WHERE (?1 IS NULL OR surface_id=?1) AND (?2 IS NULL OR submitter_site_id=?2) AND (?3 IS NULL OR kind=?3) AND (?4 IS NULL OR status=?4 OR status=?5) AND (?6 IS NULL OR created_at>=?6) AND (?7 IS NULL OR created_at<=?7) ORDER BY created_at DESC LIMIT ?8 OFFSET ?9").map_err(|e| error("feedback_query_prepare_failed", &e.to_string()))?;
    let rows = stmt.query_map(params![surface_id, submitter_site, kind, status, status2, since, until, limit, offset], |row| Ok(json!({"feedback_id":row.get::<_,String>(0)?,"surface_id":row.get::<_,String>(1)?,"submitter_site_id":row.get::<_,String>(2)?,"submitter_principal":row.get::<_,String>(3)?,"kind":row.get::<_,String>(4)?,"summary":row.get::<_,String>(5)?,"details":row.get::<_,String>(6)?,"status":row.get::<_,String>(7)?,"resolution_note":row.get::<_,Option<String>>(8)?,"resolved_by":row.get::<_,Option<String>>(9)?,"task_ref":row.get::<_,Option<String>>(10)?,"task_status":row.get::<_,Option<String>>(11)?,"created_at":row.get::<_,String>(12)?,"updated_at":row.get::<_,String>(13)?}))).map_err(|e| error("feedback_query_failed", &e.to_string()))?;
    let mut entries = Vec::new(); for row in rows.take(200) { entries.push(row.map_err(|e| error("feedback_row_decode_failed", &e.to_string()))?); }
    Ok(json!({"schema":"narada.surface_feedback.list.v1","status":"ok","scope":read_scope,"count":entries.len(),"offset":offset,"limit":limit,"entries":entries,"read_only_native":true}))
}

fn feedback_show(args: &Map<String, Value>, root: &Path) -> Result<Value, Value> {
    let read_scope = scope(args)?; let id = args.get("feedback_id").and_then(Value::as_str).filter(|v|!v.is_empty()).ok_or_else(||error("feedback_id_required","feedback_id_required"))?; let db = open_db(root)?;
    let value: Option<Value> = db.query_row("SELECT feedback_id,surface_id,submitter_site_id,submitter_principal,kind,summary,details,status,resolution_note,resolved_by,task_ref,task_status,created_at,updated_at FROM feedback_entries WHERE feedback_id=?1", params![id], |row| Ok(json!({"feedback_id":row.get::<_,String>(0)?,"surface_id":row.get::<_,String>(1)?,"submitter_site_id":row.get::<_,String>(2)?,"submitter_principal":row.get::<_,String>(3)?,"kind":row.get::<_,String>(4)?,"summary":row.get::<_,String>(5)?,"details":row.get::<_,String>(6)?,"status":row.get::<_,String>(7)?,"resolution_note":row.get::<_,Option<String>>(8)?,"resolved_by":row.get::<_,Option<String>>(9)?,"task_ref":row.get::<_,Option<String>>(10)?,"task_status":row.get::<_,Option<String>>(11)?,"created_at":row.get::<_,String>(12)?,"updated_at":row.get::<_,String>(13)?}))).optional().map_err(|e|error("feedback_query_failed",&e.to_string()))?;
    value.map(|entry|json!({"schema":"narada.surface_feedback.show.v1","status":"ok","scope":read_scope,"entry":entry,"read_only_native":true})).ok_or_else(||error("feedback_not_found","feedback_not_found"))
}

fn feedback_stats(args: &Map<String, Value>, root: &Path) -> Result<Value, Value> {
    let read_scope = scope(args)?; let surface_id = args.get("surface_id").and_then(Value::as_str); let db = open_db(root)?;
    let mut by_surface = Vec::new(); let mut stmt = db.prepare("SELECT surface_id,COUNT(*) FROM feedback_entries WHERE (?1 IS NULL OR surface_id=?1) GROUP BY surface_id ORDER BY COUNT(*) DESC LIMIT 100").map_err(|e|error("feedback_stats_prepare_failed",&e.to_string()))?; let rows = stmt.query_map(params![surface_id], |row| Ok(json!({"surface_id":row.get::<_,String>(0)?,"count":row.get::<_,i64>(1)?}))).map_err(|e|error("feedback_stats_query_failed",&e.to_string()))?; for row in rows { by_surface.push(row.map_err(|e|error("feedback_stats_row_failed",&e.to_string()))?); }
    let mut by_status = Vec::new(); let mut stmt = db.prepare("SELECT status,COUNT(*) FROM feedback_entries WHERE (?1 IS NULL OR surface_id=?1) GROUP BY status ORDER BY COUNT(*) DESC LIMIT 20").map_err(|e|error("feedback_stats_prepare_failed",&e.to_string()))?; let rows = stmt.query_map(params![surface_id], |row| Ok(json!({"status":row.get::<_,String>(0)?,"count":row.get::<_,i64>(1)?}))).map_err(|e|error("feedback_stats_query_failed",&e.to_string()))?; for row in rows { by_status.push(row.map_err(|e|error("feedback_stats_row_failed",&e.to_string()))?); }
    Ok(json!({"schema":"narada.surface_feedback.stats.v1","status":"ok","scope":read_scope,"by_surface":by_surface,"by_status":by_status,"read_only_native":true}))
}

fn proof_template(args: &Map<String, Value>) -> Value { json!({"schema":"narada.surface_feedback.live_proof_template.v1","status":"ok","workflow":args.get("workflow").cloned().unwrap_or(Value::Null),"surface_id":args.get("surface_id").cloned().unwrap_or(Value::Null),"evidence":[{"kind":"command","command":"<bounded-live-command>","result":"<captured-result>"},{"kind":"readback","surface":"<owning-surface>","result":"<captured-readback>"}],"requirements":["No mock-only claim.","Include the exact command and bounded output.","Read back durable state from the owning surface."]}) }
fn authority_boundary(name: &str) -> Value { json!({"schema":"narada.surface_feedback.authority_boundary.v1","status":"unavailable","tool_name":name,"reason":"surface_feedback_mutation_not_enabled_in_native_read_slice","remediation":"Use the configured surface-feedback authority for writes, imports, task handoffs, and status changes."}) }
fn error(code: &str, message: &str) -> Value { json!({"schema":"narada.surface_feedback.error.v1","code":code,"message":message}) }
fn tool(name: &str, description: &str, input_schema: Value, read_only: bool) -> Value { json!({"name":name,"description":description,"annotations":{"title":name,"readOnlyHint":read_only,"destructiveHint":!read_only,"idempotentHint":read_only,"openWorldHint":false},"inputSchema":input_schema,"outputSchema":{"type":"object","additionalProperties":true}}) }

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn native_feedback_reads_are_bounded_and_mutations_refuse() {
        let root = std::env::temp_dir().join(format!("narada-feedback-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(root.join(".feedback")).expect("root");
        let db = Connection::open(root.join(".feedback/surface-feedback.db")).expect("db");
        db.execute_batch("CREATE TABLE feedback_entries (feedback_id TEXT PRIMARY KEY,surface_id TEXT,submitter_site_id TEXT,submitter_principal TEXT,kind TEXT,summary TEXT,details TEXT,status TEXT,resolution_note TEXT,resolved_by TEXT,task_ref TEXT,task_status TEXT,created_at TEXT,updated_at TEXT); INSERT INTO feedback_entries VALUES ('f1','calendar','site-a','p','bug','broken','details','submitted',NULL,NULL,NULL,NULL,'2026-01-01','2026-01-01');").expect("schema");
        drop(db);
        let list = feedback_list(&json!({"scope":"all_authorized","limit":1}).as_object().unwrap(), &root, false).expect("list");
        assert_eq!(list["count"], 1);
        assert_eq!(call_tool("surface_feedback_submit", &Map::new(), &root).expect_err("boundary")["status"], "unavailable");
        std::fs::remove_dir_all(root).expect("cleanup");
    }
}
