use rusqlite::{params, types::ValueRef, Connection, OpenFlags, OptionalExtension};
use serde_json::{json, Map, Value};
use std::fs;
use std::path::{Path, PathBuf};

const SERVER_NAME: &str = "sop-mcp";
const DB_RELATIVE: &str = ".sop/sop.db";
const MAX_CANDIDATES: usize = 100;
const MAX_TEMPLATE_CHARS: usize = 32_000;
const MAX_TEMPLATE_BYTES: u64 = 512_000;
const RUN_STATUSES: &[&str] = &["pending", "running", "completed", "failed", "cancelled", "awaiting_confirmation"];
const MUTATING: &[&str] = &[
    "sop_template_create", "sop_template_update", "sop_template_deprecate", "sop_template_unimport", "sop_template_import_yaml",
    "sop_run_start", "sop_run_refresh", "sop_run_advance", "sop_handoff_claim", "sop_handoff_renew", "sop_handoff_release", "sop_handoff_retry",
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
        ("sop_action_list", "List SOP actions from the owning SOP store.", json!({"type":"object","additionalProperties":true})),
        ("sop_action_show", "Show one SOP action from the owning SOP store.", json!({"type":"object","additionalProperties":true})),
        ("sop_run_coverage_since", "Read SOP coverage evidence from the owning SOP store.", json!({"type":"object","additionalProperties":true})),
        ("sop_run_events", "List bounded SOP run events in reverse insertion order.", json!({"type":"object","properties":{"run_id":{"type":"string"},"limit":{"type":"integer","minimum":1,"maximum":500,"default":50},"offset":{"type":"integer","minimum":0,"maximum":100000,"default":0}},"required":["run_id"],"additionalProperties":false})),
        ("sop_outbox_list", "Read SOP outbox entries from the owning SOP store.", json!({"type":"object","additionalProperties":true})),
    ] {
        tools.push(tool(name, description, schema, true));
    }
    for name in MUTATING { tools.push(tool(name, "SOP mutation remains owned by the configured SOP authority.", json!({"type":"object","additionalProperties":true}), false)); }
    tools
}

pub fn auxiliary(method: &str, params: &Map<String, Value>) -> Result<Value, Value> {
    match method {
        "prompts/list" => Ok(json!({"prompts":[{"name":"sop_workflow","title":"SOP Workflow","description":"Inspect templates and run posture before starting or advancing a governed SOP.","arguments":[]}]})),
        "prompts/get" => { if params.get("name").and_then(Value::as_str) != Some("sop_workflow") { return Err(error("unknown_prompt","unknown_prompt")); } Ok(json!({"description":"Inspect templates and run posture before starting or advancing a governed SOP.","messages":[{"role":"user","content":{"type":"text","text":"Use sop_doctor and sop_template_candidate_list/show before importing or running an SOP. Run and handoff mutations remain with the owning SOP authority."}}]})) }
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
        "sop_run_coverage_since" | "sop_outbox_list" => Err(authority_boundary(name)),
        "sop_run_status" => run_status(args, root),
        "sop_run_list" => run_list(args, root),
        "sop_run_events" => run_events(args, root),
        "sop_handoff_list" => handoff_list(args, root),
        "sop_handoff_show" => handoff_show(args, root),
        "sop_action_list" => action_list(args, root),
        "sop_action_show" => action_show(args, root),
        name if MUTATING.contains(&name) => Err(authority_boundary(name)),
        _ => Err(error("unknown_tool", &format!("unknown_tool:{name}"))),
    }
}

fn guidance_tool() -> Value { tool("sop_guidance", "Show model-facing operating guidance for SOP workflows.", json!({"type":"object","properties":{"workflow":{"type":"string"},"tool":{"type":"string"}},"additionalProperties":false}), true) }
fn guidance(args: &Map<String, Value>) -> Value { json!({"schema":"narada.mcp_surface.guidance.v0","status":"ok","surface_id":"sop","guidance_tool":"sop_guidance","purpose":"Inspect bounded SOP templates and delegated run posture.","requested":{"workflow":args.get("workflow").cloned().unwrap_or(Value::Null),"tool":args.get("tool").cloned().unwrap_or(Value::Null)},"first_use":["Call sop_doctor first.","Use candidate list/show/search for local template discovery.","Inspect a template before import or execution.","Keep run, handoff, action, and outbox state with the owning SOP authority."],"boundaries":["The native slice reads local YAML only.","It does not parse or execute arbitrary commands from YAML.","Durable SOP registry/run state remains an explicit authority boundary."]}) }

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
    if args.get("include_terminal").and_then(Value::as_bool) != Some(true) { conditions.push("status NOT IN ('completed','failed','cancelled')"); }
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

fn run_detail(value: Value) -> Value {
    let steps = member(&value, "step_states_json");
    let parse_error = if steps.is_array() { Value::Null } else { json!("step_states_json is not an array") };
    let step_values = steps.as_array().cloned().unwrap_or_default();
    let next_steps = step_values.iter().filter(|step| member(step, "status").as_str() == Some("running")).map(|step| {
        let action = member(step, "action");
        let action_target = action.as_object().map(|object| json!({"surface_id":object.get("surface_id").cloned().unwrap_or(Value::Null),"tool_name":object.get("tool_name").cloned().unwrap_or(Value::Null)}));
        let result = member(step, "result");
        let instructions = result.as_object().and_then(|object| object.get("instructions")).cloned().filter(|value| !value.is_null()).unwrap_or_else(|| member(step, "instructions"));
        json!({"step_id":member(step,"step_id"),"executor":member(step,"executor"),"title":member(step,"title"),"instructions":instructions,"child_run_id":member(step,"child_run_id"),"child_sop_id":member(step,"sop_id"),"action_id":member(step,"action_id"),"action_target":action_target,"result":result,"result_ref":member(step,"result_ref")})
    }).collect::<Vec<_>>();
    let child_pins = step_values.iter().filter(|step| member(step, "executor").as_str() == Some("sop")).map(|step| json!({"step_id":member(step,"step_id"),"sop_id":member(step,"sop_id"),"sop_version":member(step,"sop_version"),"definition_fingerprint":member(step,"pinned_child_definition_fingerprint")})).collect::<Vec<_>>();
    json!({
        "schema":"narada.sop.run.v2",
        "run_id":member(&value,"run_id"),
        "sop_id":member(&value,"sop_id"),
        "sop_version":member(&value,"sop_version"),
        "sop_title":member(&value,"sop_title"),
        "status":member(&value,"status"),
        "occurrence_key":member(&value,"occurrence_key"),
        "request_fingerprint":member(&value,"request_fingerprint"),
        "definition_fingerprint":member(&value,"definition_fingerprint"),
        "input":member(&value,"input_json"),
        "input_ref":member(&value,"input_ref_json"),
        "output":member(&value,"output_json"),
        "output_ref":member(&value,"output_ref_json"),
        "step_states":step_values,
        "step_states_parse_error":parse_error,
        "trigger_source_kind":member(&value,"trigger_source_kind"),
        "trigger_source_ref":member(&value,"trigger_source_ref"),
        "triggered_by":member(&value,"triggered_by"),
        "parent_run_id":member(&value,"parent_run_id"),
        "parent_step_id":member(&value,"parent_step_id"),
        "created_at":member(&value,"created_at"),
        "updated_at":member(&value,"updated_at"),
        "completed_at":member(&value,"completed_at"),
        "definition_snapshot":{"stored":true,"fingerprint":member(&value,"definition_fingerprint"),"sop_id":member(&value,"sop_id"),"sop_version":member(&value,"sop_version"),"child_pins":child_pins},
        "admission":Value::Null,
        "next_awaits_confirmation":next_steps.iter().any(|step| matches!(step.get("executor").and_then(Value::as_str),Some("agent")|Some("operator"))),
        "next_steps":next_steps,
        "next_step":next_steps.first().cloned().unwrap_or(Value::Null),
        "relationship_reconciliation":{"mode":"automatic","repair_tool":"sop_run_refresh"},
        "native_hydration":"bounded_sqlite_read"
    })
}

fn handoff_list(args: &Map<String, Value>, root: &Path) -> Result<Value, Value> {
    let limit = args.get("limit").and_then(Value::as_u64).unwrap_or(50).clamp(1, 100);
    let run_id = args.get("run_id").and_then(Value::as_str).filter(|value| !value.trim().is_empty());
    let executor = args.get("executor").and_then(Value::as_str).filter(|value| !value.trim().is_empty());
    let status = args.get("status").and_then(Value::as_str).filter(|value| !value.trim().is_empty());
    if let Some(executor) = executor {
        if !matches!(executor, "agent" | "operator") { return Err(error("sop_handoff_executor_invalid", &format!("sop_handoff_executor_invalid:{executor}"))); }
    }
    if let Some(status) = status {
        if !matches!(status, "pending" | "leased" | "completed" | "failed" | "cancelled") { return Err(error("sop_handoff_status_invalid", &format!("sop_handoff_status_invalid:{status}"))); }
    }
    let mut sql = String::from("SELECT * FROM sop_handoffs");
    let mut conditions = Vec::<&str>::new();
    let mut values = Vec::<String>::new();
    if let Some(run_id) = run_id { conditions.push("run_id = ?"); values.push(run_id.to_string()); }
    if let Some(executor) = executor { conditions.push("executor = ?"); values.push(executor.to_string()); }
    if let Some(status) = status { conditions.push("status = ?"); values.push(status.to_string()); }
    if !conditions.is_empty() { sql.push_str(" WHERE "); sql.push_str(&conditions.join(" AND ")); }
    sql.push_str(" ORDER BY created_at, handoff_id LIMIT ?");
    values.push(limit.to_string());
    let Some(connection) = open_db(root)? else { return Ok(json!({"schema":"narada.sop.handoff_list.v1","items":[],"count":0})); };
    let mut statement = connection.prepare(&sql).map_err(|e| error("sop_handoff_query_failed", &e.to_string()))?;
    let rows = statement.query_map(rusqlite::params_from_iter(values.iter()), row_value).map_err(|e| error("sop_handoff_query_failed", &e.to_string()))?;
    let items = rows.take(100).map(|row| row.map(handoff_record).map_err(|e| error("sop_handoff_row_failed", &e.to_string()))).collect::<Result<Vec<_>, _>>()?;
    Ok(json!({"schema":"narada.sop.handoff_list.v1","items":items,"count":items.len()}))
}

fn handoff_show(args: &Map<String, Value>, root: &Path) -> Result<Value, Value> {
    let handoff_id = args.get("handoff_id").and_then(Value::as_str).filter(|value| !value.trim().is_empty()).ok_or_else(|| error("sop_handoff_id_required", "sop_handoff_id_required"))?;
    let Some(connection) = open_db(root)? else { return Err(error("sop_handoff_not_found", &format!("sop_handoff_not_found:{handoff_id}"))); };
    let row = connection.query_row("SELECT * FROM sop_handoffs WHERE handoff_id = ? LIMIT 1", params![handoff_id], row_value).optional().map_err(|e| error("sop_handoff_query_failed", &e.to_string()))?;
    row.map(handoff_record).ok_or_else(|| error("sop_handoff_not_found", &format!("sop_handoff_not_found:{handoff_id}")))
}

fn handoff_record(value: Value) -> Value {
    json!({"schema":"narada.sop.handoff.v1","handoff_id":member(&value,"handoff_id"),"run_id":member(&value,"run_id"),"step_id":member(&value,"step_id"),"occurrence_key":member(&value,"occurrence_key"),"sop_id":member(&value,"sop_id"),"sop_version":member(&value,"sop_version"),"executor":member(&value,"executor"),"title":member(&value,"title"),"instructions":member(&value,"instructions"),"input":member(&value,"input_json"),"input_ref":member(&value,"input_ref_json"),"result_schema":member(&value,"result_schema_json"),"request_fingerprint":member(&value,"request_fingerprint"),"status":member(&value,"status"),"lease_owner":member(&value,"lease_owner"),"lease_expires_at":member(&value,"lease_expires_at"),"attempt_count":member(&value,"attempt_count"),"last_error":member(&value,"last_error"),"completion_key":member(&value,"completion_key"),"completion_fingerprint":member(&value,"completion_fingerprint"),"principal":member(&value,"principal"),"result":member(&value,"result_json"),"result_ref":member(&value,"result_ref_json"),"error_message":member(&value,"error_message"),"created_at":member(&value,"created_at"),"updated_at":member(&value,"updated_at"),"completed_at":member(&value,"completed_at")})
}

fn run_events(args: &Map<String, Value>, root: &Path) -> Result<Value, Value> {
    let run_id = args.get("run_id").and_then(Value::as_str).filter(|value| !value.trim().is_empty()).ok_or_else(|| error("sop_requires_run_id", "sop_requires_run_id"))?;
    let limit = args.get("limit").and_then(Value::as_u64).unwrap_or(50).clamp(1, 500);
    let offset = args.get("offset").and_then(Value::as_u64).unwrap_or(0).min(100_000);
    let Some(connection) = open_db(root)? else { return Ok(json!({"items":[],"count":0,"run_id":run_id})); };
    let mut statement = connection.prepare("SELECT * FROM sop_events WHERE run_id = ? ORDER BY rowid DESC LIMIT ? OFFSET ?").map_err(|e| error("sop_event_query_failed", &e.to_string()))?;
    let rows = statement.query_map(params![run_id, limit, offset], row_value).map_err(|e| error("sop_event_query_failed", &e.to_string()))?;
    let items = rows.take(500).map(|row| row.map(event_record).map_err(|e| error("sop_event_row_failed", &e.to_string()))).collect::<Result<Vec<_>, _>>()?;
    Ok(json!({"items":items,"count":items.len(),"run_id":run_id}))
}

fn event_record(value: Value) -> Value {
    json!({"event_id":member(&value,"event_id"),"run_id":member(&value,"run_id"),"step_id":member(&value,"step_id"),"event_kind":member(&value,"event_kind"),"details":member(&value,"details_json"),"recorded_at":member(&value,"recorded_at")})
}

fn doctor(root: &Path) -> Result<Value, Value> {
    let dirs = sops_dirs(root); let mut counts = Vec::new();
    for dir in &dirs { let count = fs::read_dir(dir).ok().map(|entries| entries.filter_map(Result::ok).filter(|entry| entry.path().file_name().and_then(|v|v.to_str()).map(|v|v.ends_with(".sop.yaml")).unwrap_or(false)).take(MAX_CANDIDATES).count()).unwrap_or(0); counts.push(json!({"path":dir.to_string_lossy(),"candidate_count":count})); }
    Ok(json!({"schema":"narada.sop_mcp.doctor.v1","status":"ok","site_root":root.to_string_lossy(),"sops_dirs":counts,"native_adapter":"template_action_run_status_handoff_event_read","execution":"authority_boundary","server_name":SERVER_NAME}))
}

fn candidate_entries(root: &Path) -> Vec<(String, PathBuf)> {
    let mut entries = Vec::new();
    for dir in sops_dirs(root) { if let Ok(read) = fs::read_dir(dir) { for entry in read.filter_map(Result::ok).take(MAX_CANDIDATES) { let path = entry.path(); if path.file_name().and_then(|v|v.to_str()).map(|v|v.ends_with(".sop.yaml")).unwrap_or(false) { if let Some(name) = path.file_name().and_then(|v|v.to_str()).map(|v|v.trim_end_matches(".sop.yaml").to_string()) { entries.push((name,path)); } } if entries.len() >= MAX_CANDIDATES { break; } } } }
    entries
}

fn candidate_list(args: &Map<String, Value>, root: &Path) -> Result<Value, Value> {
    let limit = args.get("limit").and_then(Value::as_u64).unwrap_or(50).clamp(1, MAX_CANDIDATES as u64) as usize;
    let candidates = candidate_entries(root).into_iter().take(limit).map(|(sop_id,path)| { let meta = fs::metadata(&path).ok(); json!({"sop_id":sop_id,"path":path.to_string_lossy(),"bytes":meta.as_ref().map(|m|m.len()),"modified":meta.and_then(|m|m.modified().ok()).and_then(|v|v.duration_since(std::time::UNIX_EPOCH).ok()).map(|v|v.as_secs()),"import_state":"unverified"}) }).collect::<Vec<_>>();
    Ok(json!({"schema":"narada.sop_mcp.template_candidates.v1","status":"ok","count":candidates.len(),"limit":limit,"candidates":candidates,"native_read_only":true}))
}

fn safe_id(args: &Map<String, Value>) -> Result<String, Value> { let id = args.get("sop_id").and_then(Value::as_str).filter(|v|!v.trim().is_empty()).ok_or_else(||error("sop_id_required","sop_id_required"))?.trim().to_string(); if id.len()>120 || !id.chars().all(|c|c.is_ascii_alphanumeric() || c=='-' || c=='_' || c=='.') { return Err(error("sop_id_invalid","sop_id_invalid")); } Ok(id) }

fn candidate_show(args: &Map<String, Value>, root: &Path) -> Result<Value, Value> {
    let id = safe_id(args)?; let Some((_, path)) = candidate_entries(root).into_iter().find(|(candidate, _)| candidate == &id) else { return Err(error("sop_yaml_not_found","sop_yaml_not_found")); };
    if fs::metadata(&path).map_err(|e|error("sop_yaml_read_failed",&e.to_string()))?.len() > MAX_TEMPLATE_BYTES { return Err(error("sop_yaml_too_large", "sop_yaml_too_large")); }
    let text = fs::read_to_string(&path).map_err(|e|error("sop_yaml_read_failed",&e.to_string()))?; let truncated = text.chars().count() > MAX_TEMPLATE_CHARS; let bounded = text.chars().take(MAX_TEMPLATE_CHARS).collect::<String>();
    Ok(json!({"schema":"narada.sop_mcp.template_candidate.v1","status":"ok","sop_id":id,"path":path.to_string_lossy(),"raw_yaml":bounded,"truncated":truncated,"import_state":"unverified","native_read_only":true}))
}

fn authority_boundary(name: &str) -> Value { json!({"schema":"narada.sop_mcp.authority_boundary.v1","status":"unavailable","tool_name":name,"reason":"sop_registry_or_execution_not_enabled_in_native_read_slice","remediation":"Use the configured SOP authority for registry, run, handoff, action, and outbox operations outside the native read slice."}) }
fn error(code: &str, message: &str) -> Value { json!({"schema":"narada.sop_mcp.error.v1","code":code,"message":message}) }
fn tool(name: &str, description: &str, schema: Value, read_only: bool) -> Value { json!({"name":name,"description":description,"annotations":{"title":name,"readOnlyHint":read_only,"destructiveHint":!read_only,"idempotentHint":read_only,"openWorldHint":false},"inputSchema":schema,"outputSchema":{"type":"object","additionalProperties":true}}) }

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn native_sop_template_read_is_bounded() {
        let root = std::env::temp_dir().join(format!("narada-sop-{}", uuid::Uuid::new_v4())); let dir = root.join("sops"); fs::create_dir_all(&dir).expect("dir"); fs::write(dir.join("demo.sop.yaml"), "schema: narada.sop.v1\nid: demo\n").expect("yaml");
        assert_eq!(candidate_list(&json!({"limit":1}).as_object().unwrap(), &root).expect("list")["count"], 1);
        assert!(candidate_show(&json!({"sop_id":"demo"}).as_object().unwrap(), &root).expect("show")["raw_yaml"].as_str().unwrap().contains("demo"));
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn native_sop_registry_reads_templates_without_execution() {
        let root = std::env::temp_dir().join(format!("narada-sop-db-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(root.join(".sop")).expect("root");
        let connection = Connection::open(db_path(&root)).expect("db");
        connection.execute_batch("CREATE TABLE sop_templates (sop_id TEXT, version INTEGER, title TEXT, status TEXT, description TEXT, steps_json TEXT, trigger_kind TEXT, input_schema_json TEXT, output_mapping_json TEXT, output_ref_mapping_json TEXT, output_schema_json TEXT, acceptance_criteria_json TEXT, evidence_requirements_json TEXT, created_at TEXT, updated_at TEXT); INSERT INTO sop_templates VALUES ('demo',1,'Demo','active','A demo','[{\"id\":\"step-1\"}]','manual',NULL,NULL,NULL,NULL,'[]','[]','2026-01-01','2026-01-01');").expect("schema");
        drop(connection);
        assert_eq!(template_list(&json!({"limit":1}).as_object().unwrap(), &root).expect("list")["count"], 1);
        assert_eq!(template_show(&json!({"sop_id":"demo"}).as_object().unwrap(), &root).expect("show")["steps"][0]["id"], "step-1");
        assert_eq!(template_search(&json!({"query":"Demo"}).as_object().unwrap(), &root).expect("search")["count"], 1);
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn native_sop_action_reads_are_bounded_and_read_only() {
        let root = std::env::temp_dir().join(format!("narada-sop-actions-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(root.join(".sop")).expect("root");
        let connection = Connection::open(db_path(&root)).expect("db");
        connection.execute_batch("CREATE TABLE sop_actions (action_id TEXT, run_id TEXT, step_id TEXT, occurrence_key TEXT, surface_id TEXT, tool_name TEXT, arguments_json TEXT, request_fingerprint TEXT, status TEXT, completion_key TEXT, completion_fingerprint TEXT, operation_ref TEXT, result_json TEXT, result_ref_json TEXT, error_message TEXT, created_at TEXT, updated_at TEXT, completed_at TEXT); INSERT INTO sop_actions VALUES ('action-1','run-1','step-1','occ-1','surface','tool','{}','fingerprint','pending',NULL,NULL,NULL,'{}',NULL,NULL,'2026-01-01','2026-01-01',NULL);").expect("schema");
        drop(connection);
        let list = action_list(&json!({"limit":1}).as_object().unwrap(), &root).expect("list");
        assert_eq!(list["count"], 1);
        assert_eq!(list["items"][0]["action_id"], "action-1");
        let show = action_show(&json!({"action_id":"action-1"}).as_object().unwrap(), &root).expect("show");
        assert_eq!(show["schema"], "narada.sop.action.v1");
        assert_eq!(show["arguments"], json!({}));
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn native_sop_run_list_reads_nonterminal_summaries() {
        let root = std::env::temp_dir().join(format!("narada-sop-runs-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(root.join(".sop")).expect("root");
        let connection = Connection::open(db_path(&root)).expect("db");
        connection.execute_batch("CREATE TABLE sop_runs (run_id TEXT, sop_id TEXT, sop_version INTEGER, sop_title TEXT, status TEXT, occurrence_key TEXT, parent_run_id TEXT, parent_step_id TEXT, created_at TEXT, updated_at TEXT, completed_at TEXT); INSERT INTO sop_runs VALUES ('run-1','demo',1,'Demo','running','occ-1',NULL,NULL,'2026-01-01','2026-01-01',NULL); INSERT INTO sop_runs VALUES ('run-2','demo',1,'Demo','completed','occ-2',NULL,NULL,'2026-01-02','2026-01-02','2026-01-02');").expect("schema");
        drop(connection);
        let list = run_list(&json!({"limit":10}).as_object().unwrap(), &root).expect("list");
        assert_eq!(list["count"], 1);
        assert_eq!(list["items"][0]["run_id"], "run-1");
        let invalid = run_list(&json!({"status":"unknown"}).as_object().unwrap(), &root).expect_err("status validation");
        assert_eq!(invalid["code"], "sop_run_status_unsupported");
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn native_sop_run_status_rehydrates_bounded_public_projection() {
        let root = std::env::temp_dir().join(format!("narada-sop-run-status-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(root.join(".sop")).expect("root");
        let connection = Connection::open(db_path(&root)).expect("db");
        connection.execute_batch("CREATE TABLE sop_runs (run_id TEXT, sop_id TEXT, sop_version INTEGER, sop_title TEXT, status TEXT, occurrence_key TEXT, request_fingerprint TEXT, definition_fingerprint TEXT, definition_json TEXT, input_json TEXT, input_ref_json TEXT, output_json TEXT, output_ref_json TEXT, step_states_json TEXT, trigger_source_kind TEXT, trigger_source_ref TEXT, triggered_by TEXT, parent_run_id TEXT, parent_step_id TEXT, created_at TEXT, updated_at TEXT, completed_at TEXT);").expect("schema");
        let steps = r#"[{"step_id":"step-1","executor":"operator","blocking":true,"title":"Approve","status":"running","depends_on":[],"instructions":"approve","when":null,"input":{},"input_ref":null,"result_schema":null,"action":null,"sop_id":null,"sop_version":null,"wait_policy":null,"pinned_child_definition_fingerprint":null,"child_run_id":null,"action_id":null,"started_at":"2026-01-01","completed_at":null,"result":{"instructions":"approve now"},"result_ref":null,"completion_key":null,"completion_fingerprint":null,"error_message":null}]"#;
        connection.execute("INSERT INTO sop_runs (run_id,sop_id,sop_version,sop_title,status,occurrence_key,request_fingerprint,definition_fingerprint,definition_json,input_json,input_ref_json,output_json,output_ref_json,step_states_json,trigger_source_kind,trigger_source_ref,triggered_by,parent_run_id,parent_step_id,created_at,updated_at,completed_at) VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)", params!["run-1", "demo", 1, "Demo", "awaiting_confirmation", "occ-1", "request-fp", "definition-fp", r#"{"steps":[]}"#, r#"{"input":1}"#, Option::<String>::None, r#"{}"#, Option::<String>::None, steps, "manual", "", "operator", Option::<String>::None, Option::<String>::None, "2026-01-01", "2026-01-01", Option::<String>::None]).expect("run");
        drop(connection);
        let status = run_status(&json!({"run_id":"run-1"}).as_object().unwrap(), &root).expect("status");
        assert_eq!(status["schema"], "narada.sop.run.v2");
        assert_eq!(status["step_states"][0]["status"], "running");
        assert_eq!(status["next_awaits_confirmation"], true);
        assert_eq!(status["next_step"]["instructions"], "approve now");
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn native_sop_handoff_reads_redact_lease_tokens() {
        let root = std::env::temp_dir().join(format!("narada-sop-handoffs-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(root.join(".sop")).expect("root");
        let connection = Connection::open(db_path(&root)).expect("db");
        connection.execute_batch("CREATE TABLE sop_handoffs (handoff_id TEXT, run_id TEXT, step_id TEXT, occurrence_key TEXT, sop_id TEXT, sop_version INTEGER, executor TEXT, title TEXT, instructions TEXT, input_json TEXT, input_ref_json TEXT, result_schema_json TEXT, request_fingerprint TEXT, status TEXT, lease_owner TEXT, lease_token TEXT, lease_expires_at TEXT, attempt_count INTEGER, last_error TEXT, completion_key TEXT, completion_fingerprint TEXT, principal TEXT, result_json TEXT, result_ref_json TEXT, error_message TEXT, created_at TEXT, updated_at TEXT, completed_at TEXT); INSERT INTO sop_handoffs VALUES ('handoff-1','run-1','step-1','occ-1','demo',1,'operator','Approve','approve now','{}',NULL,NULL,'request-fp','leased','consumer','secret-token','2026-01-01T01:00:00Z',1,NULL,NULL,NULL,NULL,'{}',NULL,NULL,'2026-01-01','2026-01-01',NULL);").expect("schema");
        drop(connection);
        let list = handoff_list(&json!({"status":"leased"}).as_object().unwrap(), &root).expect("list");
        assert_eq!(list["count"], 1);
        assert!(list["items"][0].get("lease_token").is_none());
        let show = handoff_show(&json!({"handoff_id":"handoff-1"}).as_object().unwrap(), &root).expect("show");
        assert_eq!(show["schema"], "narada.sop.handoff.v1");
        assert!(show.get("lease_token").is_none());
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn native_sop_run_events_page_durable_records() {
        let root = std::env::temp_dir().join(format!("narada-sop-events-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(root.join(".sop")).expect("root");
        let connection = Connection::open(db_path(&root)).expect("db");
        connection.execute_batch("CREATE TABLE sop_events (event_id TEXT, run_id TEXT, step_id TEXT, event_kind TEXT, details_json TEXT, recorded_at TEXT); INSERT INTO sop_events VALUES ('event-1','run-1','step-1','step_started','{\"detail\":1}','2026-01-01'); INSERT INTO sop_events VALUES ('event-2','run-1','','run_completed','{}','2026-01-02');").expect("schema");
        drop(connection);
        let page = run_events(&json!({"run_id":"run-1","limit":1}).as_object().unwrap(), &root).expect("events");
        assert_eq!(page["count"], 1);
        assert_eq!(page["items"][0]["event_id"], "event-2");
        assert_eq!(page["items"][0]["details"], json!({}));
        fs::remove_dir_all(root).expect("cleanup");
    }
}
