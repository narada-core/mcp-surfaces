use rusqlite::{params, Connection, OpenFlags, OptionalExtension};
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use narada_mcp_lifecycle::{LifecycleServer, Options as LifecycleOptions, Surface as LifecycleSurface};
use std::path::{Path, PathBuf};
use time::OffsetDateTime;
use uuid::Uuid;

const SERVER_NAME: &str = "surface-feedback-mcp";
const FEEDBACK_KINDS: &[&str] = &["bug", "improvement", "gap", "observation"];
const FEEDBACK_STATUSES: &[&str] = &["submitted", "acknowledged", "routed", "converted_to_task", "closed"];
const READ_SCOPES: &[&str] = &["all_authorized", "store_reconciliation", "authority_visible", "owned_surfaces", "authority_site_submissions"];
const MAX_IMPORT_IDS: usize = 200;

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
fn guidance(args: &Map<String, Value>) -> Value { json!({"schema":"narada.mcp_surface.guidance.v0","status":"ok","surface_id":"surface-feedback","guidance_tool":"surface_feedback_guidance","purpose":"Inspect bounded feedback evidence with explicit read scope.","requested":{"workflow":args.get("workflow").cloned().unwrap_or(Value::Null),"tool":args.get("tool").cloned().unwrap_or(Value::Null)},"first_use":["Call surface_feedback_doctor first and inspect capabilities.read_scopes.","Use all_authorized for the canonical local store or store_reconciliation for explicit reconciliation work.","Use list or actionable_queue for bounded discovery, then show before mutation.","Task conversion remains owner-authorized."],"read_scope_summary":{"available":["all_authorized","store_reconciliation"],"server_authority_required":["authority_visible","owned_surfaces","authority_site_submissions"]},"boundaries":["The native surface reads and writes only the configured feedback SQLite store.","Task creation is delegated to the task-lifecycle authority adapter; Surface Feedback does not own tasks.","Authority and provenance scopes remain server-bound."]}) }

fn feedback_path(root: &Path) -> std::path::PathBuf { root.join(".feedback/surface-feedback.db") }

fn is_canonical_store(root: &Path) -> bool {
    std::env::var("NARADA_SURFACE_FEEDBACK_ROOT").ok().map(PathBuf::from).is_some_and(|canonical| canonical == root)
}

fn open_db(root: &Path) -> Result<Connection, Value> {
    let path = feedback_path(root);
    if !path.exists() { return Err(error("feedback_store_missing", "feedback_store_missing")); }
    Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY).map_err(|e| error("feedback_store_open_failed", &e.to_string()))
}

fn open_db_rw(root: &Path) -> Result<Connection, Value> {
    let path = feedback_path(root);
    if !path.exists() { return Err(error("feedback_store_missing", "feedback_store_missing")); }
    Connection::open(path).map_err(|e| error("feedback_store_open_failed", &e.to_string()))
}

fn now_iso() -> String {
    OffsetDateTime::now_utc().format(&time::format_description::well_known::Rfc3339).unwrap_or_else(|_| "1970-01-01T00:00:00Z".to_string())
}

fn required_arg(args: &Map<String, Value>, key: &str, code: &str) -> Result<String, Value> {
    args.get(key).and_then(Value::as_str).map(str::trim).filter(|v| !v.is_empty()).map(ToOwned::to_owned).ok_or_else(|| error(code, code))
}

fn authority() -> Result<(String, String, Vec<String>), Value> {
    let site = std::env::var("NARADA_SITE_ID").ok().filter(|v| !v.trim().is_empty()).ok_or_else(|| error("feedback_authority_unconfigured", "feedback_authority_unconfigured"))?;
    let principal = std::env::var("NARADA_AGENT_ID").ok().filter(|v| !v.trim().is_empty()).unwrap_or_else(|| format!("surface-feedback@{site}"));
    let owned = std::env::var("NARADA_OWNED_SURFACE_IDS").ok().unwrap_or_default().split(',').map(str::trim).filter(|v| !v.is_empty()).map(ToOwned::to_owned).collect::<Vec<_>>();
    Ok((site, principal, owned))
}

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
    let idempotency_key = args.get("idempotency_key").and_then(Value::as_str).map(str::trim).filter(|v|!v.is_empty());
    let id = idempotency_key.map(|key| { let digest=Sha256::digest(format!("{submitter_site_id}\0{submitter_principal}\0{key}").as_bytes()); format!("sfb_{:x}",digest)[..16].to_string() }).unwrap_or_else(||format!("sfb_{}",&Uuid::new_v4().to_string()[..12]));
    let now = now_iso();
    let db = open_db_rw(root)?;
    if let Some(existing)=db.query_row("SELECT surface_id,submitter_site_id,submitter_principal,kind,summary,details,created_at FROM feedback_entries WHERE feedback_id=?1",params![id],|row|Ok((row.get::<_,String>(0)?,row.get::<_,String>(1)?,row.get::<_,String>(2)?,row.get::<_,String>(3)?,row.get::<_,String>(4)?,row.get::<_,String>(5)?,row.get::<_,String>(6)?))).optional().map_err(|e|error("feedback_query_failed",&e.to_string()))? {
        if existing.0!=surface_id||existing.1!=submitter_site_id||existing.2!=submitter_principal||existing.3!=kind||existing.4!=summary||existing.5!=details { return Err(error("feedback_idempotency_conflict","feedback_idempotency_conflict")); }
        return Ok(json!({"schema":"narada.surface_feedback.submit.v1","status":"submitted","feedback_id":id,"surface_id":surface_id,"submitter_site_id":submitter_site_id,"kind":kind,"summary":summary,"created_at":existing.6,"native_write":true,"idempotency_replay":true}));
    }
    db.execute("INSERT INTO feedback_entries (feedback_id,surface_id,submitter_site_id,submitter_principal,kind,summary,details,status,created_at,updated_at) VALUES (?1,?2,?3,?4,?5,?6,?7,'submitted',?8,?8)", params![id, surface_id, submitter_site_id, submitter_principal, kind, summary, details, now]).map_err(|e| error("feedback_submit_failed", &e.to_string()))?;
    let _ = db.execute("INSERT INTO feedback_events (event_id,feedback_id,event_type,actor_principal,status,note,details_json,created_at) VALUES (?1,?2,'submitted',?3,'submitted',?4,?5,?6)", params![format!("sfb_evt_{}", Uuid::new_v4()), id, submitter_principal, summary, json!({"submitter_site_id":submitter_site_id,"surface_id":surface_id,"kind":kind}).to_string(), now]);
    Ok(json!({"schema":"narada.surface_feedback.submit.v1","status":"submitted","feedback_id":id,"surface_id":surface_id,"submitter_site_id":submitter_site_id,"kind":kind,"summary":summary,"created_at":now,"native_write":true,"idempotency_replay":false}))
}

fn feedback_update_status(args: &Map<String, Value>, root: &Path) -> Result<Value, Value> {
    let id = required_arg(args, "feedback_id", "feedback_requires_feedback_id")?;
    let status = required_arg(args, "status", "feedback_requires_status")?;
    if !FEEDBACK_STATUSES.contains(&status.as_str()) { return Err(error("feedback_invalid_status", "feedback_invalid_status")); }
    let note = required_arg(args, "resolution_note", "feedback_requires_resolution_note")?;
    let (authority_site, principal, owned_surfaces) = authority()?;
    let db = open_db_rw(root)?;
    let row: Option<(String, String, String)> = db.query_row("SELECT submitter_site_id,surface_id,status FROM feedback_entries WHERE feedback_id=?1", params![id], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?))).optional().map_err(|e| error("feedback_query_failed", &e.to_string()))?;
    let Some((submitter_site, surface_id, previous_status)) = row else { return Err(error("feedback_not_found", "feedback_not_found")); };
    let owns_surface = owned_surfaces.iter().any(|value| value == &surface_id);
    if submitter_site != authority_site && !owns_surface && !is_canonical_store(root) { return Err(error("feedback_not_visible", "feedback_not_visible")); }
    let now = now_iso();
    let task_ref = args.get("task_ref").and_then(Value::as_str);
    let task_status = args.get("task_status").and_then(Value::as_str);
    db.execute("UPDATE feedback_entries SET status=?1,resolved_by=?2,resolution_note=?3,task_ref=COALESCE(?4,task_ref),task_status=COALESCE(?5,task_status),updated_at=?6 WHERE feedback_id=?7", params![status, principal, note, task_ref, task_status, now, id]).map_err(|e| error("feedback_update_failed", &e.to_string()))?;
    let _ = db.execute("INSERT INTO feedback_events (event_id,feedback_id,event_type,actor_principal,status,task_ref,task_status,note,details_json,created_at) VALUES (?1,?2,'status_updated',?3,?4,?5,?6,?7,?8,?9)", params![format!("sfb_evt_{}", Uuid::new_v4()), id, principal, status, task_ref, task_status, note, json!({"previous_status":previous_status,"authority_site_id":authority_site}).to_string(), now]);
    Ok(json!({"schema":"narada.surface_feedback.update_status.v1","status":"updated","feedback_id":id,"new_status":status,"resolved_by":principal,"resolution_note":note,"updated_at":now,"native_write":true}))
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
    let db = open_db_rw(root)?;
    let row: Option<(String, String, String, String, String, Option<String>, Option<String>, Option<String>)> = db.query_row(
        "SELECT surface_id,submitter_site_id,summary,details,status,task_ref,task_status,resolution_note FROM feedback_entries WHERE feedback_id=?1",
        params![feedback_id],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?, row.get(5)?, row.get(6)?, row.get(7)?)),
    ).optional().map_err(|e| error("feedback_query_failed", &e.to_string()))?;
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
    let resolved_by = std::env::var("NARADA_AGENT_ID").ok().filter(|value| !value.trim().is_empty()).unwrap_or_else(|| "surface-feedback".to_string());
    let note = args.get("resolution_note").and_then(Value::as_str).filter(|value| !value.trim().is_empty()).map(ToOwned::to_owned).unwrap_or_else(|| format!("Created {task_ref} from feedback via surface_feedback_convert_to_task."));
    db.execute("UPDATE feedback_entries SET status='converted_to_task',resolved_by=?1,resolution_note=?2,task_ref=?3,task_status=?4,updated_at=?5 WHERE feedback_id=?6", params![resolved_by, note, task_ref, task_status, now_iso(), feedback_id]).map_err(|e| error("feedback_task_link_failed", &e.to_string()))?;
    Ok(json!({"schema":"narada.surface_feedback.convert_to_task.v1","status":"converted","feedback_id":feedback_id,"task_ref":task_ref,"task_number":task_number,"task_id":task_id,"task_status":task_status,"task_creation":{"status":"created_or_recovered","payload_ref":payload_ref,"idempotency_key":format!("surface-feedback:{feedback_id}")},"handoff_authorization":{"scope":"canonical_user_site_handoff","authorization_basis":"canonical_feedback_store_and_server_binding","authority_site_id":std::env::var("NARADA_SITE_ID").ok()},"next_action":{"surface_id":"task-lifecycle","tool":"task_lifecycle_show","arguments":{"task_number":task_number}}}))
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
    let target_path = feedback_path(root);
    if same_feedback_path(&source_path, &target_path) {
        return Err(error("feedback_import_same_store", "feedback_import_same_store"));
    }
    if !source_path.exists() { return Err(error("feedback_import_source_missing", "feedback_import_source_missing")); }
    let ids = args.get("feedback_ids").and_then(Value::as_array).ok_or_else(|| error("feedback_import_requires_feedback_ids", "feedback_import_requires_feedback_ids"))?;
    let ids = ids.iter().filter_map(Value::as_str).map(str::trim).filter(|value| !value.is_empty()).map(ToOwned::to_owned).collect::<Vec<_>>();
    if ids.is_empty() || ids.len() > MAX_IMPORT_IDS { return Err(error("feedback_import_requires_feedback_ids", "feedback_import_requires_feedback_ids")); }
    let source = Connection::open_with_flags(&source_path, OpenFlags::SQLITE_OPEN_READ_ONLY).map_err(|e| error("feedback_import_source_open_failed", &e.to_string()))?;
    let target = open_db_rw(root)?;
    let mut imported = Vec::new();
    let mut skipped = Vec::new();
    let mut missing = Vec::new();
    target.execute_batch("BEGIN IMMEDIATE").map_err(|e| error("feedback_import_begin_failed", &e.to_string()))?;
    let result = (|| -> Result<(), Value> {
        for id in &ids {
            let Some(row) = feedback_row(&source, id)? else { missing.push(id.clone()); continue; };
            if feedback_row(&target, id)?.is_some() {
                skipped.push(json!({"feedback_id": id, "reason": "already_exists"}));
                continue;
            }
            let get = |name: &str| row.get(name).cloned().unwrap_or(Value::Null);
            let text = |name: &str| get(name).as_str().unwrap_or("").to_string();
            target.execute("INSERT INTO feedback_entries (feedback_id,surface_id,submitter_site_id,submitter_principal,kind,summary,details,status,resolution_note,resolved_by,task_ref,task_status,source_db_path,source_updated_at,source_sync_mode,created_at,updated_at) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,'explicit_import',?15,?16)", params![
                text("feedback_id"), text("surface_id"), text("submitter_site_id"), text("submitter_principal"), text("kind"), text("summary"), text("details"),
                { let value = text("status"); if value.is_empty() { "submitted".to_string() } else { value } },
                optional_text(&get("resolution_note")), optional_text(&get("resolved_by")), optional_text(&get("task_ref")), optional_text(&get("task_status")),
                source_path.to_string_lossy().to_string(), text("updated_at"), text("created_at"), text("updated_at")
            ]).map_err(|e| error("feedback_import_insert_failed", &e.to_string()))?;
            imported.push(json!({"feedback_id":id,"surface_id":text("surface_id"),"submitter_site_id":text("submitter_site_id"),"submitter_principal":text("submitter_principal"),"kind":text("kind"),"summary":text("summary"),"details":text("details"),"status":text("status"),"source_db_path":source_path.to_string_lossy().to_string(),"source_sync_mode":"explicit_import","created_at":text("created_at"),"updated_at":text("updated_at")}));
        }
        Ok(())
    })();
    match result {
        Ok(()) => target.execute_batch("COMMIT").map_err(|e| error("feedback_import_commit_failed", &e.to_string()))?,
        Err(error) => { let _ = target.execute_batch("ROLLBACK"); return Err(error); }
    }
    Ok(json!({"schema":"narada.surface_feedback.import.v1","status":if missing.is_empty() && skipped.is_empty(){"imported"}else{"partial"},"source_db_path":source_path.to_string_lossy(),"target_db_path":target_path.to_string_lossy(),"requested_count":ids.len(),"imported_count":imported.len(),"skipped_count":skipped.len(),"missing_count":missing.len(),"imported":imported,"skipped":skipped,"missing_feedback_ids":missing,"native_write":true}))
}

fn optional_text(value: &Value) -> Option<String> { value.as_str().map(str::to_string).filter(|value| !value.is_empty()) }

fn doctor(root: &Path) -> Result<Value, Value> {
    let path = feedback_path(root);
    if !path.exists() { return Ok(json!({"schema":"narada.surface_feedback.doctor.v1","status":"ok","feedback_root":root.to_string_lossy(),"db_path":path.to_string_lossy(),"store_status":"missing","feedback_entries":0,"read_only_native":false,"native_write_available":false,"capabilities":capabilities(root, false),"server_name":SERVER_NAME})); }
    let db = open_db(root)?;
    let table: Option<String> = db.query_row("SELECT name FROM sqlite_master WHERE type='table' AND name='feedback_entries'", [], |row| row.get(0)).optional().map_err(|e| error("feedback_store_probe_failed", &e.to_string()))?;
    let rows = if table.is_some() { db.query_row("SELECT COUNT(*) FROM feedback_entries", [], |row| row.get::<_, i64>(0)).unwrap_or(0) } else { 0 };
    let ready = table.is_some();
    Ok(json!({"schema":"narada.surface_feedback.doctor.v1","status":"ok","feedback_root":root.to_string_lossy(),"db_path":path.to_string_lossy(),"store_status":if ready{"ready"}else{"schema_missing"},"feedback_entries":rows,"read_only_native":false,"native_write_available":ready,"capabilities":capabilities(root, ready),"server_name":SERVER_NAME}))
}

fn capabilities(root: &Path, store_ready: bool) -> Value {
    let authority_configured = authority().is_ok();
    let task_handoff_configured = task_authority_root(root).join(".ai/task-lifecycle.db").is_file();
    let unavailable = |reason: &str| json!({"available":false,"reason":reason});
    json!({"read_scopes":{"all_authorized":{"available":store_ready,"purpose":"canonical local feedback store"},"store_reconciliation":{"available":store_ready,"purpose":"explicit source/store reconciliation"},"authority_visible":unavailable("native_authority_scope_projection_not_implemented"),"owned_surfaces":unavailable("native_authority_scope_projection_not_implemented"),"authority_site_submissions":unavailable("native_authority_scope_projection_not_implemented")},"mutations":{"submit":{"available":store_ready},"import":{"available":store_ready},"status_update":{"available":store_ready && authority_configured,"reason":if authority_configured{Value::Null}else{json!("feedback_authority_unconfigured")}},"task_handoff":{"available":store_ready && authority_configured && task_handoff_configured,"reason":if authority_configured && task_handoff_configured{Value::Null}else{json!("task_or_site_authority_unconfigured")}}}})
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
    let fetch_limit = limit + 1;
    let status = if actionable { Some("submitted") } else { requested_status };
    let status2 = if actionable { Some("acknowledged") } else { None };
    let mut stmt = db.prepare("SELECT feedback_id,surface_id,submitter_site_id,submitter_principal,kind,summary,details,status,resolution_note,resolved_by,task_ref,task_status,created_at,updated_at FROM feedback_entries WHERE (?1 IS NULL OR surface_id=?1) AND (?2 IS NULL OR submitter_site_id=?2) AND (?3 IS NULL OR kind=?3) AND (?4 IS NULL OR status=?4 OR status=?5) AND (?6 IS NULL OR created_at>=?6) AND (?7 IS NULL OR created_at<=?7) ORDER BY created_at DESC LIMIT ?8 OFFSET ?9").map_err(|e| error("feedback_query_prepare_failed", &e.to_string()))?;
    let rows = stmt.query_map(params![surface_id, submitter_site, kind, status, status2, since, until, fetch_limit, offset], |row| Ok(json!({"feedback_id":row.get::<_,String>(0)?,"surface_id":row.get::<_,String>(1)?,"submitter_site_id":row.get::<_,String>(2)?,"submitter_principal":row.get::<_,String>(3)?,"kind":row.get::<_,String>(4)?,"summary":row.get::<_,String>(5)?,"details":row.get::<_,String>(6)?,"status":row.get::<_,String>(7)?,"resolution_note":row.get::<_,Option<String>>(8)?,"resolved_by":row.get::<_,Option<String>>(9)?,"task_ref":row.get::<_,Option<String>>(10)?,"task_status":row.get::<_,Option<String>>(11)?,"created_at":row.get::<_,String>(12)?,"updated_at":row.get::<_,String>(13)?}))).map_err(|e| error("feedback_query_failed", &e.to_string()))?;
    let mut entries = Vec::new(); for row in rows.take(201) { entries.push(row.map_err(|e| error("feedback_row_decode_failed", &e.to_string()))?); }
    let has_more = entries.len() > limit as usize;
    entries.truncate(limit as usize);
    let next_offset = has_more.then_some(offset + entries.len() as i64);
    Ok(json!({"schema":"narada.surface_feedback.list.v1","status":"ok","scope":read_scope,"count":entries.len(),"returned":entries.len(),"offset":offset,"limit":limit,"has_more":has_more,"next_offset":next_offset,"entries":entries,"read_only_native":true}))
}

fn feedback_show(args: &Map<String, Value>, root: &Path) -> Result<Value, Value> {
    let read_scope = scope(args)?; let id = args.get("feedback_id").and_then(Value::as_str).filter(|v|!v.is_empty()).ok_or_else(||error("feedback_id_required","feedback_id_required"))?; let db = open_db(root)?;
    let value: Option<Value> = db.query_row("SELECT feedback_id,surface_id,submitter_site_id,submitter_principal,kind,summary,details,status,resolution_note,resolved_by,task_ref,task_status,created_at,updated_at FROM feedback_entries WHERE feedback_id=?1", params![id], |row| Ok(json!({"feedback_id":row.get::<_,String>(0)?,"surface_id":row.get::<_,String>(1)?,"submitter_site_id":row.get::<_,String>(2)?,"submitter_principal":row.get::<_,String>(3)?,"kind":row.get::<_,String>(4)?,"summary":row.get::<_,String>(5)?,"details":row.get::<_,String>(6)?,"status":row.get::<_,String>(7)?,"resolution_note":row.get::<_,Option<String>>(8)?,"resolved_by":row.get::<_,Option<String>>(9)?,"task_ref":row.get::<_,Option<String>>(10)?,"task_status":row.get::<_,Option<String>>(11)?,"created_at":row.get::<_,String>(12)?,"updated_at":row.get::<_,String>(13)?}))).optional().map_err(|e|error("feedback_query_failed",&e.to_string()))?;
    value.map(|entry|json!({"schema":"narada.surface_feedback.show.v1","status":"ok","scope":read_scope,"entry":entry,"read_only_native":true})).ok_or_else(||error("feedback_not_found","feedback_not_found"))
}

fn feedback_stats(args: &Map<String, Value>, root: &Path) -> Result<Value, Value> {
    let read_scope = scope(args)?; let surface_id = args.get("surface_id").and_then(Value::as_str); let db = open_db(root)?;
    let total = db.query_row("SELECT COUNT(*) FROM feedback_entries WHERE (?1 IS NULL OR surface_id=?1)", params![surface_id], |row| row.get::<_,i64>(0)).map_err(|e|error("feedback_stats_query_failed",&e.to_string()))?;
    let mut by_surface = Vec::new(); let mut stmt = db.prepare("SELECT surface_id,COUNT(*) FROM feedback_entries WHERE (?1 IS NULL OR surface_id=?1) GROUP BY surface_id ORDER BY COUNT(*) DESC LIMIT 100").map_err(|e|error("feedback_stats_prepare_failed",&e.to_string()))?; let rows = stmt.query_map(params![surface_id], |row| Ok(json!({"surface_id":row.get::<_,String>(0)?,"count":row.get::<_,i64>(1)?}))).map_err(|e|error("feedback_stats_query_failed",&e.to_string()))?; for row in rows { by_surface.push(row.map_err(|e|error("feedback_stats_row_failed",&e.to_string()))?); }
    let mut by_status = Vec::new(); let mut stmt = db.prepare("SELECT status,COUNT(*) FROM feedback_entries WHERE (?1 IS NULL OR surface_id=?1) GROUP BY status ORDER BY COUNT(*) DESC LIMIT 20").map_err(|e|error("feedback_stats_prepare_failed",&e.to_string()))?; let rows = stmt.query_map(params![surface_id], |row| Ok(json!({"status":row.get::<_,String>(0)?,"count":row.get::<_,i64>(1)?}))).map_err(|e|error("feedback_stats_query_failed",&e.to_string()))?; for row in rows { by_status.push(row.map_err(|e|error("feedback_stats_row_failed",&e.to_string()))?); }
    Ok(json!({"schema":"narada.surface_feedback.stats.v1","status":"ok","scope":read_scope,"total":total,"by_surface":by_surface,"by_status":by_status,"read_only_native":true}))
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
        let root = std::env::temp_dir().join(format!("narada-feedback-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(root.join(".feedback")).expect("root");
        let db = Connection::open(root.join(".feedback/surface-feedback.db")).expect("db");
        db.execute_batch("CREATE TABLE feedback_entries (feedback_id TEXT PRIMARY KEY,surface_id TEXT,submitter_site_id TEXT,submitter_principal TEXT,kind TEXT,summary TEXT,details TEXT,status TEXT,resolution_note TEXT,resolved_by TEXT,task_ref TEXT,task_status TEXT,created_at TEXT,updated_at TEXT); INSERT INTO feedback_entries VALUES ('f1','calendar','site-a','p','bug','broken','details','submitted',NULL,NULL,NULL,NULL,'2026-01-01','2026-01-01');").expect("schema");
        drop(db);
        let list = feedback_list(&json!({"scope":"all_authorized","limit":1}).as_object().unwrap(), &root, false).expect("list");
        assert_eq!(list["count"], 1);
        assert_eq!(list["has_more"], false);
        assert_eq!(list["next_offset"], Value::Null);
        let doctor = doctor(&root).expect("doctor");
        assert_eq!(doctor["read_only_native"], false);
        assert_eq!(doctor["capabilities"]["read_scopes"]["all_authorized"]["available"], true);
        assert_eq!(doctor["capabilities"]["read_scopes"]["authority_visible"]["available"], false);
        std::env::set_var("NARADA_SITE_ID", "site-a");
        std::env::set_var("NARADA_AGENT_ID", "agent-a");
        let submitted = call_tool("surface_feedback_submit", &json!({"surface_id":"calendar","submitter_site_id":"site-a","submitter_principal":"agent-a","kind":"observation","summary":"native write"}).as_object().unwrap(), &root).expect("submit");
        assert_eq!(submitted["status"], "submitted");
        let mismatch = call_tool("surface_feedback_submit", &json!({"surface_id":"calendar","submitter_site_id":"site-b","kind":"bug","summary":"spoofed"}).as_object().unwrap(), &root).expect_err("authority mismatch");
        assert_eq!(mismatch["code"], "feedback_submitter_site_authority_mismatch");
        let retry_args=json!({"surface_id":"calendar","submitter_site_id":"site-a","submitter_principal":"agent-a","kind":"observation","summary":"retry safe","idempotency_key":"retry-1"});
        let first=call_tool("surface_feedback_submit",retry_args.as_object().unwrap(),&root).expect("first");
        let replay=call_tool("surface_feedback_submit",retry_args.as_object().unwrap(),&root).expect("replay");
        assert_eq!(first["feedback_id"],replay["feedback_id"]); assert_eq!(replay["idempotency_replay"],true);
        let conflict=call_tool("surface_feedback_submit",&json!({"surface_id":"calendar","submitter_site_id":"site-a","submitter_principal":"agent-a","kind":"bug","summary":"different","idempotency_key":"retry-1"}).as_object().unwrap(),&root).expect_err("conflict");
        assert_eq!(conflict["code"],"feedback_idempotency_conflict");
        std::env::set_var("NARADA_SITE_ID", "canonical-maintainer");
        std::env::set_var("NARADA_AGENT_ID", "canonical-maintainer-agent");
        std::env::set_var("NARADA_SURFACE_FEEDBACK_ROOT", &root);
        let maintained = feedback_update_status(&json!({"feedback_id":"f1","status":"closed","resolution_note":"canonical repair"}).as_object().unwrap(), &root).expect("canonical maintainer");
        assert_eq!(maintained["new_status"], "closed");
        std::fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn native_feedback_batch_update_and_explicit_import() {
        let root = std::env::temp_dir().join(format!("narada-feedback-import-{}", uuid::Uuid::new_v4()));
        let source_root = std::env::temp_dir().join(format!("narada-feedback-source-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(root.join(".feedback")).expect("root");
        std::fs::create_dir_all(source_root.join(".feedback")).expect("source");
        let schema = "CREATE TABLE feedback_entries (feedback_id TEXT PRIMARY KEY,surface_id TEXT,submitter_site_id TEXT,submitter_principal TEXT,kind TEXT,summary TEXT,details TEXT,status TEXT,resolution_note TEXT,resolved_by TEXT,task_ref TEXT,task_status TEXT,source_db_path TEXT,source_updated_at TEXT,source_sync_mode TEXT,created_at TEXT,updated_at TEXT);";
        let db = Connection::open(root.join(".feedback/surface-feedback.db")).expect("db");
        db.execute_batch(&format!("{schema} CREATE TABLE feedback_events (event_id TEXT PRIMARY KEY,feedback_id TEXT,event_type TEXT,actor_principal TEXT,status TEXT,note TEXT,details_json TEXT,created_at TEXT);")) .expect("schema");
        drop(db);
        let source = Connection::open(source_root.join(".feedback/surface-feedback.db")).expect("source db");
        source.execute_batch(schema).expect("source schema");
        source.execute("INSERT INTO feedback_entries (feedback_id,surface_id,submitter_site_id,submitter_principal,kind,summary,details,status,created_at,updated_at) VALUES ('f2','calendar','site-a','agent-a','observation','import me','details','submitted','2026-01-01','2026-01-01')", []).expect("source row");
        drop(source);
        std::env::set_var("NARADA_SITE_ID", "site-a");
        std::env::set_var("NARADA_AGENT_ID", "agent-a");
        let submitted = feedback_submit(&json!({"surface_id":"calendar","submitter_site_id":"site-a","submitter_principal":"agent-a","kind":"bug","summary":"batch me"}).as_object().unwrap(), &root).expect("submit");
        let batch = feedback_update_status_batch(&json!({"updates":[{"feedback_id":submitted["feedback_id"].clone(),"status":"acknowledged","resolution_note":"ack"}]}).as_object().unwrap(), &root).expect("batch");
        assert_eq!(batch["status"], "updated");
        let imported = feedback_import(&json!({"source_db_path":source_root.join(".feedback/surface-feedback.db").to_string_lossy(),"feedback_ids":["f2"]}).as_object().unwrap(), &root).expect("import");
        assert_eq!(imported["status"], "imported");
        assert_eq!(imported["imported_count"], 1);
        std::fs::remove_dir_all(root).expect("cleanup");
        std::fs::remove_dir_all(source_root).expect("source cleanup");
    }

    #[test]
    fn feedback_conversion_uses_in_process_task_authority() {
        let root = std::env::temp_dir().join(format!("narada-feedback-convert-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(root.join(".feedback")).expect("feedback root");
        let feedback = Connection::open(root.join(".feedback/surface-feedback.db")).expect("db");
        feedback.execute_batch("CREATE TABLE feedback_entries (feedback_id TEXT PRIMARY KEY,surface_id TEXT,submitter_site_id TEXT,submitter_principal TEXT,kind TEXT,summary TEXT,details TEXT,status TEXT,resolution_note TEXT,resolved_by TEXT,task_ref TEXT,task_status TEXT,created_at TEXT,updated_at TEXT); INSERT INTO feedback_entries VALUES ('f-convert','worker-delegation','site-a','agent-a','bug','bounded worker stalls','details','submitted',NULL,NULL,NULL,NULL,'2026-01-01','2026-01-01');").expect("feedback schema");
        drop(feedback);
        let lifecycle_options = LifecycleOptions { surface: LifecycleSurface::Task, site_root: root.clone(), site_root_source: "test".to_string(), prepare: true, migrate_legacy: false, source_database_path: None };
        LifecycleServer::prepare_database(&lifecycle_options).expect("task database");
        let result = feedback_convert_to_task(json!({"feedback_id":"f-convert","task_title":"Fix bounded worker stalls"}).as_object().unwrap(), &root).expect("conversion");
        assert_eq!(result["status"], "converted");
        assert!(result["task_ref"].as_str().is_some());
        let replay = feedback_convert_to_task(json!({"feedback_id":"f-convert"}).as_object().unwrap(), &root).expect("idempotent replay");
        assert_eq!(replay["status"], "already_linked");
        assert_eq!(replay["task_ref"], result["task_ref"]);
        std::fs::remove_dir_all(root).expect("cleanup");
    }
}
