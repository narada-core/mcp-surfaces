use rusqlite::{params, types::ValueRef, Connection, OpenFlags, OptionalExtension};
use serde_json::{json, Map, Value};
use std::fs;
use std::path::{Path, PathBuf};
use time::{format_description::well_known::Rfc3339, OffsetDateTime};

const SERVER_NAME: &str = "sop-mcp";
const DB_RELATIVE: &str = ".sop/sop.db";
const MAX_CANDIDATES: usize = 100;
const MAX_TEMPLATE_CHARS: usize = 32_000;
const MAX_TEMPLATE_BYTES: u64 = 512_000;
const RUN_STATUSES: &[&str] = &["pending", "running", "completed", "failed", "cancelled", "awaiting_confirmation"];
const MUTATING: &[&str] = &[
    "sop_template_create", "sop_template_update", "sop_template_deprecate", "sop_template_unimport", "sop_template_import_yaml",
    "sop_run_start", "sop_run_refresh", "sop_run_advance", "sop_handoff_claim", "sop_handoff_claim_and_advance", "sop_handoff_renew", "sop_handoff_release", "sop_handoff_retry",
    "sop_action_resolve", "sop_run_cancel", "sop_outbox_consumer_register", "sop_outbox_ack", "sop_outbox_compact",
];

pub fn list_tools() -> Vec<Value> {
    let mut tools = vec![guidance_tool()];
    for (name, description, schema) in [
        ("sop_doctor", "Inspect configured SOP directories and native read posture.", json!({"type":"object","properties":{},"additionalProperties":false})),
        ("sop_template_candidate_list", "List bounded SOP YAML template candidates from configured directories.", json!({"type":"object","properties":{"limit":{"type":"integer","minimum":1,"maximum":100,"default":50}},"additionalProperties":false})),
        ("sop_template_candidate_show", "Show one bounded SOP YAML template candidate.", json!({"type":"object","properties":{"sop_id":{"type":"string"}},"required":["sop_id"],"additionalProperties":false})),
        ("sop_template_list", "List imported SOP templates from the local SOP registry.", json!({"type":"object","properties":{"status":{"type":"string"},"limit":{"type":"integer","minimum":1,"maximum":100,"default":50}},"additionalProperties":false})),
        ("sop_template_show", "Show one imported SOP template from the local SOP registry.", json!({"type":"object","properties":{"sop_id":{"type":"string"},"version":{"type":"integer","minimum":1}},"required":["sop_id"],"additionalProperties":false})),
        ("sop_template_search", "Search local SOP registry templates by title or description.", json!({"type":"object","properties":{"query":{"type":"string"},"status":{"type":"string"},"limit":{"type":"integer","minimum":1,"maximum":100,"default":50}},"required":["query"],"additionalProperties":false})),
        ("sop_run_status", "Get the durable status and step state for one SOP occurrence.", json!({"type":"object","properties":{"run_id":{"type":"string"}},"required":["run_id"],"additionalProperties":false})),
        ("sop_run_list", "List SOP runs with optional filters.", json!({"type":"object","properties":{"sop_id":{"type":"string"},"status":{"type":"string","enum":["pending","running","completed","failed","cancelled","awaiting_confirmation"]},"include_terminal":{"type":"boolean"},"limit":{"type":"integer","minimum":1,"maximum":200,"default":50}},"additionalProperties":false})),
        ("sop_handoff_list", "List durable SOP handoffs without exposing lease tokens.", json!({"type":"object","properties":{"run_id":{"type":"string"},"executor":{"type":"string","enum":["agent","operator"]},"status":{"type":"string","enum":["pending","leased","completed","failed","cancelled"]},"limit":{"type":"integer","minimum":1,"maximum":100,"default":50}},"additionalProperties":false})),
        ("sop_handoff_show", "Show one durable SOP handoff without exposing its lease token.", json!({"type":"object","properties":{"handoff_id":{"type":"string"}},"required":["handoff_id"],"additionalProperties":false})),
        ("sop_action_list", "List SOP actions from the owning SOP store.", json!({"type":"object","properties":{"run_id":{"type":"string"},"status":{"type":"string"},"limit":{"type":"integer","minimum":1,"maximum":100,"default":50}},"additionalProperties":false})),
        ("sop_action_show", "Show one SOP action from the owning SOP store.", json!({"type":"object","properties":{"action_id":{"type":"string"}},"required":["action_id"],"additionalProperties":false})),
        ("sop_run_coverage_since", "List latest SOP run coverage since a supplied timestamp.", json!({"type":"object","properties":{"since":{"type":"string"},"limit":{"type":"integer","minimum":1,"maximum":500,"default":200},"template_status":{"type":"string","enum":["draft","active","deprecated"],"default":"active"},"status":{"type":"string","enum":["pending","running","completed","failed","cancelled","awaiting_confirmation"]},"include_terminal":{"type":"boolean","default":true}},"required":["since"],"additionalProperties":false})),
        ("sop_run_events", "List bounded SOP run events in reverse insertion order.", json!({"type":"object","properties":{"run_id":{"type":"string"},"limit":{"type":"integer","minimum":1,"maximum":500,"default":50},"offset":{"type":"integer","minimum":0,"maximum":100000,"default":0}},"required":["run_id"],"additionalProperties":false})),
        ("sop_outbox_list", "List unacknowledged SOP terminal events for a registered consumer.", json!({"type":"object","properties":{"consumer_id":{"type":"string"},"topic":{"type":"string","const":"sop.run.terminal.v1"},"limit":{"type":"integer","minimum":1,"maximum":500,"default":100}},"required":["consumer_id"],"additionalProperties":false})),
    ] {
        tools.push(tool(name, description, schema, true));
    }
    for name in MUTATING { tools.push(tool(name, mutation_description(name), mutation_schema(name), false)); }
    tools
}

pub fn auxiliary(method: &str, params: &Map<String, Value>) -> Result<Value, Value> {
    match method {
        "prompts/list" => Ok(json!({"prompts":[{"name":"sop_workflow","title":"SOP Workflow","description":"Inspect templates and run posture before starting or advancing a governed SOP.","arguments":[]}]})),
        "prompts/get" => { if params.get("name").and_then(Value::as_str) != Some("sop_workflow") { return Err(error("unknown_prompt","unknown_prompt")); } Ok(json!({"description":"Inspect templates and run posture before starting or advancing a governed SOP.","messages":[{"role":"user","content":{"type":"text","text":"Use sop_doctor and sop_template_candidate_list/show before importing or running an SOP. Native Rust owns template, run, handoff, action, and outbox durability."}}]})) }
        "completion/complete" => { let is_name = params.get("argument").and_then(Value::as_object).and_then(|v|v.get("name")).and_then(Value::as_str) == Some("name"); let values = if is_name { list_tools().iter().filter_map(|v|v.get("name").cloned()).take(100).collect::<Vec<_>>() } else { Vec::new() }; Ok(json!({"completion":{"values":values,"total":values.len(),"hasMore":false}})) }
        "logging/setLevel" => Ok(json!({})),
        _ => Err(error("unsupported_mcp_method", &format!("unsupported_mcp_method:{method}"))),
    }
}

pub fn call_tool(name: &str, args: &Map<String, Value>, root: &Path) -> Result<Value, Value> {
    match name {
        "sop_guidance" => Ok(guidance(args)),
        "sop_doctor" => doctor(root),
        "sop_template_candidate_list" => candidate_list(args, root),
        "sop_template_candidate_show" => candidate_show(args, root),
        "sop_template_search" => template_search(args, root),
        "sop_template_list" => template_list(args, root),
        "sop_template_show" => template_show(args, root),
        "sop_outbox_list" => outbox_list(args, root),
        "sop_run_status" => run_status(args, root),
        "sop_run_list" => run_list(args, root),
        "sop_run_events" => run_events(args, root),
        "sop_run_coverage_since" => run_coverage_since(args, root),
        "sop_handoff_list" => handoff_list(args, root),
        "sop_handoff_show" => handoff_show(args, root),
        "sop_action_list" => action_list(args, root),
        "sop_action_show" => action_show(args, root),
        name if MUTATING.contains(&name) => crate::sop_authority::call_tool(name, args, root),
        _ => Err(error("unknown_tool", &format!("unknown_tool:{name}"))),
    }
}

fn guidance_tool() -> Value { tool("sop_guidance", "Show model-facing operating guidance for SOP workflows.", json!({"type":"object","properties":{"workflow":{"type":"string"},"tool":{"type":"string"}},"additionalProperties":false}), true) }
fn guidance(args: &Map<String, Value>) -> Value { json!({"schema":"narada.mcp_surface.guidance.v0","status":"ok","surface_id":"sop","guidance_tool":"sop_guidance","purpose":"Inspect and execute bounded, durable SOP workflows.","requested":{"workflow":args.get("workflow").cloned().unwrap_or(Value::Null),"tool":args.get("tool").cloned().unwrap_or(Value::Null)},"first_use":["Call sop_doctor first.","Use candidate list/show/search for local template discovery.","Inspect a template before import or execution.","Use occurrence and completion keys for replay-safe mutations.","For immediate completion of an already-produced result, prefer sop_handoff_claim_and_advance so lease acquisition and completion occur in one MCP call. Use claim, renew, and advance separately for work performed between calls."],"boundaries":["Template candidates are bounded local YAML.","Legacy command effects are refused; effects require governed action bindings.","Native Rust owns template, run, handoff, action, and outbox durability."]}) }

fn mutation_description(name: &str) -> &'static str {
    match name {
        "sop_handoff_claim_and_advance" => "Claim and immediately complete one handoff in the same MCP call, without cross-call lease-token threading.",
        _ => "Apply a durable SOP mutation through the native Rust authority.",
    }
}

fn mutation_schema(name: &str) -> Value {
    let string = || json!({"type":"string","minLength":1});
    let object = || json!({"type":"object","additionalProperties":true});
    let (properties, required): (Map<String, Value>, Vec<&str>) = match name {
        "sop_template_create" => (json!({"sop_id":string(),"title":string(),"description":{"type":"string"},"steps":{"type":"array","minItems":1},"trigger_kind":{"type":"string"},"input_schema":object(),"output":object(),"output_ref":object(),"output_schema":object(),"acceptance_criteria":{"type":"array"},"evidence_requirements":{"type":"array"},"principal":string()}).as_object().unwrap().clone(), vec!["sop_id","title","steps"]),
        "sop_template_update" => (json!({"sop_id":string(),"title":string(),"description":{"type":"string"},"steps":{"type":"array","minItems":1},"trigger_kind":{"type":"string"},"input_schema":object(),"output":object(),"output_ref":object(),"output_schema":object(),"acceptance_criteria":{"type":"array"},"evidence_requirements":{"type":"array"},"status":{"type":"string","enum":["draft","active","deprecated"]},"principal":string()}).as_object().unwrap().clone(), vec!["sop_id"]),
        "sop_template_deprecate" => (json!({"sop_id":string(),"reason":string(),"principal":string()}).as_object().unwrap().clone(), vec!["sop_id","reason"]),
        "sop_template_unimport" => (json!({"sop_id":string(),"version":{"type":"integer","minimum":1},"reason":string(),"principal":string()}).as_object().unwrap().clone(), vec!["sop_id","version","reason","principal"]),
        "sop_template_import_yaml" => (json!({"sop_id":string(),"path":string(),"yaml":string(),"status":{"type":"string","enum":["draft","active"]},"principal":string()}).as_object().unwrap().clone(), vec!["sop_id"]),
        "sop_run_start" => (json!({"sop_id":string(),"version":{"type":"integer","minimum":1},"occurrence_key":string(),"input":object(),"input_ref":object(),"trigger_source_kind":{"type":"string"},"trigger_source_ref":{"type":"string"},"triggered_by":string(),"parent_run_id":{"type":"string"},"parent_step_id":{"type":"string"}}).as_object().unwrap().clone(), vec!["sop_id","occurrence_key","triggered_by"]),
        "sop_run_refresh" => (json!({"run_id":string(),"principal":string()}).as_object().unwrap().clone(), vec!["run_id"]),
        "sop_run_advance" => (json!({"handoff_id":string(),"run_id":string(),"step_id":string(),"consumer_id":string(),"lease_token":string(),"completion_key":string(),"outcome":{"type":"string","enum":["completed","failed"]},"result":object(),"result_ref":object(),"error_message":{"type":"string"},"principal":string()}).as_object().unwrap().clone(), vec!["handoff_id","run_id","step_id","consumer_id","lease_token","completion_key","outcome","principal"]),
        "sop_handoff_claim" => (json!({"consumer_id":string(),"handoff_id":string(),"executor":{"type":"string","enum":["agent","operator"]},"lease_ms":{"type":"integer","minimum":1000,"maximum":300000,"default":60000}}).as_object().unwrap().clone(), vec!["consumer_id"]),
        "sop_handoff_claim_and_advance" => (json!({"consumer_id":string(),"handoff_id":string(),"executor":{"type":"string","enum":["agent","operator"]},"lease_ms":{"type":"integer","minimum":1000,"maximum":300000,"default":60000},"completion_key":string(),"outcome":{"type":"string","enum":["completed","failed"]},"result":object(),"result_ref":object(),"error_message":{"type":"string"},"principal":string()}).as_object().unwrap().clone(), vec!["consumer_id","completion_key","outcome","principal"]),
        "sop_handoff_renew" => (json!({"handoff_id":string(),"consumer_id":string(),"lease_token":string(),"lease_ms":{"type":"integer","minimum":1000,"maximum":300000,"default":60000}}).as_object().unwrap().clone(), vec!["handoff_id","consumer_id","lease_token"]),
        "sop_handoff_release" => (json!({"handoff_id":string(),"consumer_id":string(),"lease_token":string(),"error_message":{"type":"string"}}).as_object().unwrap().clone(), vec!["handoff_id","consumer_id","lease_token"]),
        "sop_handoff_retry" => (json!({"handoff_id":string(),"principal":string(),"reason":{"type":"string"}}).as_object().unwrap().clone(), vec!["handoff_id","principal"]),
        "sop_action_resolve" => (json!({"action_id":string(),"completion_key":string(),"outcome":{"type":"string","enum":["completed","failed"]},"operation_ref":string(),"result":object(),"result_ref":object(),"error_message":{"type":"string"}}).as_object().unwrap().clone(), vec!["action_id","completion_key","outcome","operation_ref"]),
        "sop_run_cancel" => (json!({"run_id":string(),"reason":string()}).as_object().unwrap().clone(), vec!["run_id","reason"]),
        "sop_outbox_consumer_register" => (json!({"consumer_id":string(),"topic":{"type":"string","const":"sop.run.terminal.v1"},"start_at":{"type":"string"}}).as_object().unwrap().clone(), vec!["consumer_id"]),
        "sop_outbox_ack" => (json!({"consumer_id":string(),"event_id":string(),"receipt":object()}).as_object().unwrap().clone(), vec!["consumer_id","event_id","receipt"]),
        "sop_outbox_compact" => (json!({"before":string()}).as_object().unwrap().clone(), vec!["before"]),
        _ => (Map::new(), vec![]),
    };
    json!({"type":"object","properties":properties,"required":required,"additionalProperties":false})
}

fn sops_dirs(root: &Path) -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(value) = std::env::var_os("NARADA_SOPS_DIR") { dirs.push(PathBuf::from(value)); }
    dirs.push(root.join("sops")); dirs.push(root.join(".ai/sops"));
    dirs.into_iter().filter(|path| path.is_dir()).take(10).collect()
}

fn db_path(root: &Path) -> PathBuf { root.join(DB_RELATIVE) }

fn open_db(root: &Path) -> Result<Option<Connection>, Value> {
    let path = db_path(root);
    if !path.exists() { return Ok(None); }
    Connection::open_with_flags(&path, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .map(Some)
        .map_err(|e| error("sop_registry_open_failed", &e.to_string()))
}

fn row_value(row: &rusqlite::Row<'_>) -> rusqlite::Result<Value> {
    let mut object = Map::new();
    for index in 0..row.as_ref().column_count() {
        let name = row.as_ref().column_name(index).unwrap_or("column").to_string();
        let value = match row.get_ref(index)? {
            ValueRef::Null => Value::Null,
            ValueRef::Integer(value) => json!(value),
            ValueRef::Real(value) => json!(value),
            ValueRef::Text(value) => {
                let text = String::from_utf8_lossy(value).to_string();
                if name.ends_with("_json") { serde_json::from_str(&text).unwrap_or(Value::String(text)) } else { Value::String(text) }
            }
            ValueRef::Blob(value) => json!({"byte_length": value.len()}),
        };
        object.insert(name, value);
    }
    Ok(Value::Object(object))
}

fn member(value: &Value, name: &str) -> Value { value.as_object().and_then(|object| object.get(name)).cloned().unwrap_or(Value::Null) }

fn template_summary(value: Value) -> Value {
    let step_count = member(&value, "steps_json").as_array().map(|steps| steps.len()).unwrap_or(0);
    json!({"schema":"narada.sop.template_summary.v2","sop_id":member(&value,"sop_id"),"version":member(&value,"version"),"title":member(&value,"title"),"status":member(&value,"status"),"description":member(&value,"description"),"trigger_kind":member(&value,"trigger_kind"),"step_count":step_count,"updated_at":member(&value,"updated_at")})
}

fn template_record(value: Value) -> Value {
    json!({"schema":"narada.sop.template.v2","render_mode":"summary_text_with_full_structured_content","full_step_definitions_path":"structuredContent.steps","sop_id":member(&value,"sop_id"),"version":member(&value,"version"),"title":member(&value,"title"),"status":member(&value,"status"),"description":member(&value,"description"),"steps":member(&value,"steps_json"),"trigger_kind":member(&value,"trigger_kind"),"input_schema":member(&value,"input_schema_json"),"output":member(&value,"output_mapping_json"),"output_ref":member(&value,"output_ref_mapping_json"),"output_schema":member(&value,"output_schema_json"),"acceptance_criteria":member(&value,"acceptance_criteria_json"),"evidence_requirements":member(&value,"evidence_requirements_json"),"created_at":member(&value,"created_at"),"updated_at":member(&value,"updated_at"),"native_hydration":"bounded_sqlite_read"})
}

fn template_list(args: &Map<String, Value>, root: &Path) -> Result<Value, Value> {
    let limit = args.get("limit").and_then(Value::as_u64).unwrap_or(50).clamp(1, 100);
    let status = args.get("status").and_then(Value::as_str);
    let Some(connection) = open_db(root)? else { return Ok(json!({"schema":"narada.sop.template_list.v2","status":"missing","items":[],"count":0,"db_path":db_path(root).to_string_lossy()})); };
    let mut items = Vec::new();
    if let Some(status) = status {
        let mut statement = connection.prepare("SELECT t.* FROM sop_templates t JOIN (SELECT sop_id, MAX(version) AS mv FROM sop_templates GROUP BY sop_id) latest ON t.sop_id = latest.sop_id AND t.version = latest.mv WHERE t.status = ? ORDER BY t.updated_at DESC LIMIT ?").map_err(|e| error("sop_template_query_failed", &e.to_string()))?;
        let rows = statement.query_map(params![status, limit], row_value).map_err(|e| error("sop_template_query_failed", &e.to_string()))?;
        for row in rows.take(100) { items.push(template_summary(row.map_err(|e| error("sop_template_row_failed", &e.to_string()))?)); }
    } else {
        let mut statement = connection.prepare("SELECT t.* FROM sop_templates t JOIN (SELECT sop_id, MAX(version) AS mv FROM sop_templates GROUP BY sop_id) latest ON t.sop_id = latest.sop_id AND t.version = latest.mv ORDER BY t.updated_at DESC LIMIT ?").map_err(|e| error("sop_template_query_failed", &e.to_string()))?;
        let rows = statement.query_map(params![limit], row_value).map_err(|e| error("sop_template_query_failed", &e.to_string()))?;
        for row in rows.take(100) { items.push(template_summary(row.map_err(|e| error("sop_template_row_failed", &e.to_string()))?)); }
    }
    Ok(json!({"schema":"narada.sop.template_list.v2","status":"ok","items":items,"count":items.len(),"db_path":db_path(root).to_string_lossy()}))
}

fn template_show(args: &Map<String, Value>, root: &Path) -> Result<Value, Value> {
    let sop_id = args.get("sop_id").and_then(Value::as_str).filter(|value| !value.is_empty()).ok_or_else(|| error("sop_id_required", "sop_id_required"))?;
    let Some(connection) = open_db(root)? else { return Err(error("sop_not_found", "sop_not_found")); };
    let row = if let Some(version) = args.get("version").and_then(Value::as_i64) {
        connection.query_row("SELECT * FROM sop_templates WHERE sop_id = ? AND version = ? LIMIT 1", params![sop_id, version], row_value).optional().map_err(|e| error("sop_template_query_failed", &e.to_string()))?
    } else {
        connection.query_row("SELECT * FROM sop_templates WHERE sop_id = ? ORDER BY version DESC LIMIT 1", params![sop_id], row_value).optional().map_err(|e| error("sop_template_query_failed", &e.to_string()))?
    };
    row.map(|value| template_record(value)).ok_or_else(|| error("sop_not_found", "sop_not_found"))
}

fn template_search(args: &Map<String, Value>, root: &Path) -> Result<Value, Value> {
    let query = args.get("query").and_then(Value::as_str).filter(|value| !value.trim().is_empty()).ok_or_else(|| error("query_required", "query_required"))?.to_string();
    let limit = args.get("limit").and_then(Value::as_u64).unwrap_or(50).clamp(1, 100);
    let like = format!("%{query}%");
    let Some(connection) = open_db(root)? else { return Ok(json!({"schema":"narada.sop.template_search.v2","status":"missing","query":query,"items":[],"count":0,"db_path":db_path(root).to_string_lossy()})); };
    let mut statement = connection.prepare("SELECT t.* FROM sop_templates t JOIN (SELECT sop_id, MAX(version) AS mv FROM sop_templates GROUP BY sop_id) latest ON t.sop_id = latest.sop_id AND t.version = latest.mv WHERE (t.title LIKE ? OR t.description LIKE ?) ORDER BY t.updated_at DESC LIMIT ?").map_err(|e| error("sop_template_query_failed", &e.to_string()))?;
    let rows = statement.query_map(params![like, like, limit], row_value).map_err(|e| error("sop_template_query_failed", &e.to_string()))?;
    let mut items = Vec::new();
    for row in rows.take(100) { items.push(template_summary(row.map_err(|e| error("sop_template_row_failed", &e.to_string()))?)); }
    Ok(json!({"schema":"narada.sop.template_search.v2","status":"ok","query":query,"items":items,"count":items.len(),"db_path":db_path(root).to_string_lossy()}))
}

fn action_list(args: &Map<String, Value>, root: &Path) -> Result<Value, Value> {
    let limit = args.get("limit").and_then(Value::as_u64).unwrap_or(50).clamp(1, 100);
    let run_id = args.get("run_id").and_then(Value::as_str);
    let status = args.get("status").and_then(Value::as_str);
    let Some(connection) = open_db(root)? else { return Ok(json!({"schema":"narada.sop.action_list.v1","status":"missing","items":[],"count":0,"db_path":db_path(root).to_string_lossy()})); };
    let items = match (run_id, status) {
        (Some(run_id), Some(status)) => query_action_rows(&connection, "SELECT action_id, run_id, step_id, occurrence_key, surface_id, tool_name, status, operation_ref, created_at, updated_at, completed_at FROM sop_actions WHERE run_id = ? AND status = ? ORDER BY created_at ASC LIMIT ?", params![run_id, status, limit])?,
        (Some(run_id), None) => query_action_rows(&connection, "SELECT action_id, run_id, step_id, occurrence_key, surface_id, tool_name, status, operation_ref, created_at, updated_at, completed_at FROM sop_actions WHERE run_id = ? ORDER BY created_at ASC LIMIT ?", params![run_id, limit])?,
        (None, Some(status)) => query_action_rows(&connection, "SELECT action_id, run_id, step_id, occurrence_key, surface_id, tool_name, status, operation_ref, created_at, updated_at, completed_at FROM sop_actions WHERE status = ? ORDER BY created_at ASC LIMIT ?", params![status, limit])?,
        (None, None) => query_action_rows(&connection, "SELECT action_id, run_id, step_id, occurrence_key, surface_id, tool_name, status, operation_ref, created_at, updated_at, completed_at FROM sop_actions ORDER BY created_at ASC LIMIT ?", params![limit])?,
    };
    Ok(json!({"schema":"narada.sop.action_list.v1","status":"ok","items":items,"count":items.len(),"db_path":db_path(root).to_string_lossy()}))
}

fn action_show(args: &Map<String, Value>, root: &Path) -> Result<Value, Value> {
    let action_id = args.get("action_id").and_then(Value::as_str).filter(|value| !value.trim().is_empty()).ok_or_else(|| error("sop_requires_action_id", "sop_requires_action_id"))?;
    let Some(connection) = open_db(root)? else { return Err(error("sop_action_not_found", "sop_action_not_found")); };
    let row = connection.query_row("SELECT * FROM sop_actions WHERE action_id = ?", params![action_id], row_value).optional().map_err(|e| error("sop_action_query_failed", &e.to_string()))?.ok_or_else(|| error("sop_action_not_found", "sop_action_not_found"))?;
    Ok(action_record(row))
}

fn query_action_rows<P: rusqlite::Params>(connection: &Connection, sql: &str, params: P) -> Result<Vec<Value>, Value> {
    let mut statement = connection.prepare(sql).map_err(|e| error("sop_action_query_failed", &e.to_string()))?;
    let rows = statement.query_map(params, row_value).map_err(|e| error("sop_action_query_failed", &e.to_string()))?;
    rows.take(100).map(|row| row.map_err(|e| error("sop_action_row_failed", &e.to_string()))).collect()
}

fn action_record(value: Value) -> Value {
    json!({"schema":"narada.sop.action.v1","action_id":member(&value,"action_id"),"run_id":member(&value,"run_id"),"step_id":member(&value,"step_id"),"occurrence_key":member(&value,"occurrence_key"),"surface_id":member(&value,"surface_id"),"tool_name":member(&value,"tool_name"),"arguments":member(&value,"arguments_json"),"request_fingerprint":member(&value,"request_fingerprint"),"status":member(&value,"status"),"completion_key":member(&value,"completion_key"),"completion_fingerprint":member(&value,"completion_fingerprint"),"operation_ref":member(&value,"operation_ref"),"result":member(&value,"result_json"),"result_ref":member(&value,"result_ref_json"),"error_message":member(&value,"error_message"),"created_at":member(&value,"created_at"),"updated_at":member(&value,"updated_at"),"completed_at":member(&value,"completed_at")})
}

fn run_list(args: &Map<String, Value>, root: &Path) -> Result<Value, Value> {
    let limit = args.get("limit").and_then(Value::as_u64).unwrap_or(50).clamp(1, 200);
    let mut sql = String::from("SELECT * FROM sop_runs");
    let mut values = Vec::<String>::new();
    let mut conditions = Vec::<&str>::new();
    if let Some(sop_id) = args.get("sop_id").and_then(Value::as_str).filter(|value| !value.trim().is_empty()) { conditions.push("sop_id = ?"); values.push(sop_id.to_string()); }
    if let Some(status) = args.get("status").and_then(Value::as_str).filter(|value| !value.trim().is_empty()) {
        if !RUN_STATUSES.contains(&status) { return Err(json!({"schema":"narada.sop_mcp.error.v1","code":"sop_run_status_unsupported","message":format!("sop_run_status_unsupported:{status}"),"details":{"status":status,"allowed":RUN_STATUSES}})); }
        conditions.push("status = ?"); values.push(status.to_string());
    }
    if args.get("include_terminal").and_then(Value::as_bool) == Some(false) { conditions.push("status NOT IN ('completed','failed','cancelled')"); }
    if !conditions.is_empty() { sql.push_str(" WHERE "); sql.push_str(&conditions.join(" AND ")); }
    sql.push_str(" ORDER BY created_at DESC LIMIT ?");
    values.push(limit.to_string());
    let Some(connection) = open_db(root)? else { return Ok(json!({"schema":"narada.sop.run_list.v2","status":"missing","items":[],"count":0,"db_path":db_path(root).to_string_lossy()})); };
    let mut statement = connection.prepare(&sql).map_err(|e| error("sop_run_query_failed", &e.to_string()))?;
    let rows = statement.query_map(rusqlite::params_from_iter(values.iter()), row_value).map_err(|e| error("sop_run_query_failed", &e.to_string()))?;
    let items = rows.take(200).map(|row| row.map(|value| run_summary(&value)).map_err(|e| error("sop_run_row_failed", &e.to_string()))).collect::<Result<Vec<_>, _>>()?;
    Ok(json!({"schema":"narada.sop.run_list.v2","status":"ok","items":items,"count":items.len(),"db_path":db_path(root).to_string_lossy()}))
}

fn run_status(args: &Map<String, Value>, root: &Path) -> Result<Value, Value> {
    let run_id = args.get("run_id").and_then(Value::as_str).filter(|value| !value.trim().is_empty()).ok_or_else(|| error("sop_requires_run_id", "sop_requires_run_id"))?;
    let Some(connection) = open_db(root)? else { return Err(error("sop_run_not_found", &format!("sop_run_not_found:{run_id}"))); };
    let row = connection.query_row("SELECT * FROM sop_runs WHERE run_id = ? LIMIT 1", params![run_id], row_value).optional().map_err(|e| error("sop_run_query_failed", &e.to_string()))?;
    let Some(row) = row else { return Err(error("sop_run_not_found", &format!("sop_run_not_found:{run_id}"))); };
    let result = run_detail(row);
    let encoded = serde_json::to_vec(&result).map_err(|e| error("sop_run_encode_failed", &e.to_string()))?;
    if encoded.len() > MAX_TEMPLATE_BYTES as usize { return Err(error("sop_run_result_too_large", "sop_run_result_too_large")); }
    Ok(result)
}

fn run_summary(value: &Value) -> Value {
    json!({"schema":"narada.sop.run_summary.v2","run_id":member(value,"run_id"),"sop_id":member(value,"sop_id"),"sop_version":member(value,"sop_version"),"sop_title":member(value,"sop_title"),"occurrence_key":member(value,"occurrence_key"),"status":member(value,"status"),"parent_run_id":member(value,"parent_run_id"),"parent_step_id":member(value,"parent_step_id"),"created_at":member(value,"created_at"),"updated_at":member(value,"updated_at"),"completed_at":member(value,"completed_at")})
}

