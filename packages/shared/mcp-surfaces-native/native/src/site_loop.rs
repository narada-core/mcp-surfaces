use serde_json::{json, Map, Value};
use rusqlite::{params, types::ValueRef, Connection, OpenFlags, OptionalExtension};
use std::fs;
use std::path::{Path, PathBuf};

const SERVER_NAME: &str = "narada-site-loop-mcp";
const DB_RELATIVE: &str = ".ai/task-lifecycle.db";
const MAX_TEXT_BYTES: u64 = 512_000;
const READ_TOOLS: &[&str] = &[
    "site_loop_doctor", "site_docs_list", "site_docs_show", "site_test_list",
    "site_loop_config_validate", "site_loop_operator_affordances", "site_loop_status",
    "site_loop_unified_status", "site_loop_recovery_plan", "site_loop_health",
    "site_loop_operating_status", "site_loop_proof_status", "site_loop_readiness",
    "site_loop_coherence", "site_loop_runs_list", "site_loop_run_show",
    "site_loop_output_show", "site_loop_attention_list", "site_loop_attention_show",
];
const MUTATING_TOOLS: &[&str] = &[
    "site_test_run", "site_loop_proof_run", "site_loop_recovery_drill",
    "site_loop_attention_ack", "site_loop_control_set", "site_loop_run_once",
];

pub fn list_tools() -> Vec<Value> {
    let mut tools = vec![guidance_tool()];
    for name in READ_TOOLS {
        let description = match *name {
            "site_loop_doctor" => "Inspect configured Site Loop MCP readiness.",
            "site_docs_list" => "List configured read-only documentation paths exposed to agents.",
            "site_docs_show" => "Show a configured allowlisted documentation file.",
            "site_test_list" => "List approved local test selectors.",
            "site_loop_config_validate" => "Validate the site-loop config without running the loop.",
            "site_loop_operator_affordances" => "Return UI-neutral operator affordances for Site Loop.",
            "site_loop_status" => "Show configured Site Operating Loop status.",
            "site_loop_unified_status" => "Show scheduler, supervisor, logical loop, and health posture.",
            "site_loop_recovery_plan" => "Return a safe operator recovery plan without mutating state.",
            "site_loop_health" => "Show configured Site Operating Loop health.",
            "site_loop_operating_status" => "Show composed operating-layer status.",
            "site_loop_proof_status" => "Show proof freshness and configured proof commands.",
            "site_loop_readiness" => "Evaluate unattended-operation readiness gates.",
            "site_loop_coherence" => "Evaluate strict coherence blockers.",
            "site_loop_runs_list" => "List recent configured Site Operating Loop runs.",
            "site_loop_run_show" => "Show one Site Operating Loop run by run id.",
            "site_loop_output_show" => "Read a materialized Site Loop output ref with paging.",
            "site_loop_attention_list" => "List configured loop attention records.",
            "site_loop_attention_show" => "Show one loop attention record.",
            _ => "Read Site Loop state.",
        };
        let schema = match *name {
            "site_docs_show" => json!({"type":"object","properties":{"path":{"type":"string"}},"required":["path"],"additionalProperties":false}),
            "site_loop_runs_list" => json!({"type":"object","properties":{"loop_id":{"type":"string"},"limit":{"type":"integer","minimum":1,"maximum":500}},"additionalProperties":false}),
            "site_loop_run_show" => json!({"type":"object","properties":{"run_id":{"type":"string"}},"required":["run_id"],"additionalProperties":false}),
            "site_loop_attention_list" => json!({"type":"object","properties":{"loop_id":{"type":"string"},"status":{"type":"string"},"limit":{"type":"integer","minimum":1,"maximum":500}},"additionalProperties":false}),
            "site_loop_attention_show" => json!({"type":"object","properties":{"attention_id":{"type":"string"}},"required":["attention_id"],"additionalProperties":false}),
            "site_loop_output_show" => output_schema(),
            _ => json!({"type":"object","properties":{},"additionalProperties":false}),
        };
        tools.push(tool(name, description, schema, true));
    }
    for name in MUTATING_TOOLS {
        tools.push(tool(name, "Site Loop mutation remains owned by the configured JS authority until a complete native adapter is admitted.", json!({"type":"object","additionalProperties":true}), false));
    }
    tools
}

pub fn auxiliary(method: &str, params: &Map<String, Value>) -> Result<Value, Value> {
    match method {
        "prompts/list" => Ok(json!({"prompts":[{"name":"site_loop_workflow","title":"Site Loop Workflow","description":"Inspect status, proof, and recovery posture before changing loop state.","arguments":[]}]})),
        "prompts/get" => {
            if params.get("name").and_then(Value::as_str) != Some("site_loop_workflow") { return Err(error("unknown_prompt", "unknown_prompt")); }
            Ok(json!({"description":"Inspect status, proof, and recovery posture before changing loop state.","messages":[{"role":"user","content":{"type":"text","text":"Use site_loop_doctor and site_loop_unified_status before a recovery or control action. Read back health and proof after any owner-authorized mutation."}}]}))
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
        "site_loop_guidance" => Ok(guidance(args)),
        "site_loop_doctor" => doctor(root),
        "site_loop_config_validate" => config_validate(root),
        "site_docs_list" => docs_list(root),
        "site_docs_show" => docs_show(args, root),
        "site_test_list" => test_list(root),
        "site_loop_operator_affordances" => Ok(affordances()),
        "site_loop_status" => status(root),
        "site_loop_unified_status" => unified_status(root),
        "site_loop_recovery_plan" => recovery_plan(root),
        "site_loop_health" => health(root),
        "site_loop_operating_status" => operating_status(root),
        "site_loop_proof_status" => proof_status(root),
        "site_loop_readiness" => readiness(root),
        "site_loop_coherence" => coherence(root),
        "site_loop_runs_list" => runs_list(args, root),
        "site_loop_run_show" => run_show(args, root),
        "site_loop_attention_list" => attention_list(args, root),
        "site_loop_attention_show" => attention_show(args, root),
        "site_loop_output_show" => output_show(args, root),
        name if MUTATING_TOOLS.contains(&name) => Err(authority_boundary(name)),
        _ => Err(error("unknown_tool", &format!("unknown_tool:{name}"))),
    }
}

fn guidance_tool() -> Value {
    tool("site_loop_guidance", "Show model-facing operating guidance for Site Loop workflows.", json!({"type":"object","properties":{"workflow":{"type":"string"},"tool":{"type":"string"}},"additionalProperties":false}), true)
}

fn guidance(args: &Map<String, Value>) -> Value {
    json!({"schema":"narada.mcp_surface.guidance.v0","status":"ok","surface_id":"site-loop","guidance_tool":"site_loop_guidance","purpose":"Inspect site-loop configuration, status, proof, and recovery posture.","requested":{"workflow":args.get("workflow").cloned().unwrap_or(Value::Null),"tool":args.get("tool").cloned().unwrap_or(Value::Null)},"first_use":["Call site_loop_doctor first.","Use site_loop_unified_status and site_loop_health before recovery.","Use site_loop_proof_status/readiness/coherence to distinguish freshness from coherence.","Keep scheduler, resident carrier, task lifecycle, and control mutations with their owning authorities."],"boundaries":["This native slice is read-only except for no local writes.","Configured commands are reported, never executed.","Resident launch and loop control remain explicit authority boundaries."]})
}

fn config_path(root: &Path) -> PathBuf { root.join(".narada").join("capabilities").join("site-loop-config.json") }
fn db_path(root: &Path) -> PathBuf { root.join(DB_RELATIVE) }

fn open_db(root: &Path) -> Result<Option<Connection>, Value> {
    let path = db_path(root);
    if !path.exists() { return Ok(None); }
    Connection::open_with_flags(&path, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .map(Some)
        .map_err(|e| error("site_loop_store_open_failed", &e.to_string()))
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

fn json_member(value: &Value, name: &str) -> Value { value.as_object().and_then(|object| object.get(name)).cloned().unwrap_or(Value::Null) }

fn run_record(value: Value) -> Value {
    let dry_run = json_member(&value, "dry_run").as_i64().unwrap_or(0) != 0;
    json!({
        "run_id": json_member(&value, "run_id"),
        "loop_id": json_member(&value, "loop_id"),
        "status": json_member(&value, "status"),
        "dry_run": dry_run,
        "started_at": json_member(&value, "started_at"),
        "finished_at": json_member(&value, "finished_at"),
        "summary": json_member(&value, "summary_json"),
        "error": json_member(&value, "error_json"),
        "evidence_ref": json_member(&value, "evidence_ref"),
        "evidence_sha256": json_member(&value, "evidence_sha256"),
        "evidence_bytes": json_member(&value, "evidence_bytes"),
        "evidence_available": false,
        "native_hydration": "not_enabled"
    })
}

fn step_record(value: Value) -> Value {
    json!({
        "step_run_id": json_member(&value, "step_run_id"),
        "run_id": json_member(&value, "run_id"),
        "step_id": json_member(&value, "step_id"),
        "status": json_member(&value, "status"),
        "started_at": json_member(&value, "started_at"),
        "finished_at": json_member(&value, "finished_at"),
        "input_refs": json_member(&value, "input_refs_json"),
        "output_refs": json_member(&value, "output_refs_json"),
        "input_ref_count": json_member(&value, "input_ref_count"),
        "output_ref_count": json_member(&value, "output_ref_count"),
        "input_refs_digest": json_member(&value, "input_refs_digest"),
        "output_refs_digest": json_member(&value, "output_refs_digest"),
        "evidence": json_member(&value, "evidence_json"),
        "evidence_summary": json_member(&value, "evidence_json"),
        "error": json_member(&value, "error_json"),
        "evidence_ref": json_member(&value, "evidence_ref"),
        "evidence_sha256": json_member(&value, "evidence_sha256"),
        "evidence_bytes": json_member(&value, "evidence_bytes"),
        "evidence_available": false,
        "native_hydration": "not_enabled"
    })
}

fn configured_loop_id(args: &Map<String, Value>, root: &Path) -> String {
    args.get("loop_id").and_then(Value::as_str).filter(|value| !value.is_empty()).map(ToOwned::to_owned)
        .or_else(|| load_config(root).ok().flatten().and_then(|value| value.get("loop_id").and_then(Value::as_str).map(ToOwned::to_owned)))
        .unwrap_or_else(|| "narada.site.operating.loop".to_string())
}

fn bounded_limit(args: &Map<String, Value>, default: u64) -> u64 {
    args.get("limit").and_then(Value::as_u64).unwrap_or(default).clamp(1, 500)
}

fn runs_list(args: &Map<String, Value>, root: &Path) -> Result<Value, Value> {
    let loop_id = configured_loop_id(args, root);
    let limit = bounded_limit(args, 50);
    let Some(connection) = open_db(root)? else { return Ok(json!({"schema":"narada.site_operating_loop.loop_runs.v1","status":"missing","loop_id":loop_id,"runs":[],"count":0,"db_path":db_path(root).to_string_lossy()})); };
    let mut statement = connection.prepare("SELECT * FROM site_loop_runs WHERE loop_id = ? ORDER BY started_at DESC LIMIT ?").map_err(|e| error("site_loop_runs_query_failed", &e.to_string()))?;
    let rows = statement.query_map(params![loop_id, limit], row_value).map_err(|e| error("site_loop_runs_query_failed", &e.to_string()))?;
    let mut runs = Vec::new();
    for row in rows.take(500) { runs.push(run_record(row.map_err(|e| error("site_loop_runs_row_failed", &e.to_string()))?)); }
    Ok(json!({"schema":"narada.site_operating_loop.loop_runs.v1","status":"ok","loop_id":loop_id,"runs":runs,"count":runs.len(),"db_path":db_path(root).to_string_lossy(),"native_hydration":"not_enabled"}))
}

fn run_show(args: &Map<String, Value>, root: &Path) -> Result<Value, Value> {
    let run_id = args.get("run_id").and_then(Value::as_str).filter(|value| !value.is_empty()).ok_or_else(|| error("run_id_required", "run_id_required"))?;
    let Some(connection) = open_db(root)? else { return Ok(json!({"schema":"narada.site_operating_loop.loop_run.v1","status":"not_found","run_id":run_id})); };
    let run = connection.query_row("SELECT * FROM site_loop_runs WHERE run_id = ? LIMIT 1", params![run_id], row_value).optional().map_err(|e| error("site_loop_run_query_failed", &e.to_string()))?;
    let Some(run) = run else { return Ok(json!({"schema":"narada.site_operating_loop.loop_run.v1","status":"not_found","run_id":run_id})); };
    let mut statement = connection.prepare("SELECT * FROM site_loop_step_runs WHERE run_id = ? ORDER BY rowid ASC LIMIT 500").map_err(|e| error("site_loop_steps_query_failed", &e.to_string()))?;
    let rows = statement.query_map(params![run_id], row_value).map_err(|e| error("site_loop_steps_query_failed", &e.to_string()))?;
    let mut steps = Vec::new();
    for row in rows { steps.push(step_record(row.map_err(|e| error("site_loop_step_row_failed", &e.to_string()))?)); }
    Ok(json!({"schema":"narada.site_operating_loop.loop_run.v1","status":"ok","run":run_record(run),"steps":steps,"native_hydration":"not_enabled"}))
}

fn attention_record(value: Value) -> Value {
    let escalation = json_member(&value, "escalation_summary_json");
    let attention_id = match json_member(&value, "envelope_id") { Value::String(value) if !value.is_empty() => Value::String(value), _ => json_member(&value, "escalation_id") };
    let severity = escalation.get("severity").cloned().or_else(|| escalation.get("fields").and_then(Value::as_object).and_then(|fields| fields.get("severity")).cloned()).unwrap_or_else(|| json!("warning"));
    json!({
        "schema":"narada.site_operating_loop.attention.v1",
        "attention_id":attention_id,
        "escalation_id":json_member(&value, "escalation_id"),
        "loop_id":json_member(&value, "loop_id"),
        "directive_id":json_member(&value, "directive_id"),
        "classification":json_member(&value, "classification"),
        "status":json_member(&value, "status"),
        "envelope_id":json_member(&value, "envelope_id"),
        "created_at":json_member(&value, "created_at"),
        "acknowledged_at":json_member(&value, "acknowledged_at"),
        "acknowledged_by":json_member(&value, "acknowledged_by"),
        "ack_reason":json_member(&value, "ack_reason"),
        "severity":severity,
        "escalation":escalation,
        "escalation_ref":json_member(&value, "escalation_ref"),
        "escalation_sha256":json_member(&value, "escalation_sha256"),
        "escalation_bytes":json_member(&value, "escalation_bytes")
    })
}

fn attention_summary(connection: &Connection, loop_id: &str) -> Result<Value, Value> {
    let mut statement = connection.prepare("SELECT status, COUNT(*) AS count FROM site_loop_escalations WHERE loop_id = ? GROUP BY status").map_err(|e| error("site_loop_attention_summary_failed", &e.to_string()))?;
    let rows = statement.query_map(params![loop_id], row_value).map_err(|e| error("site_loop_attention_summary_failed", &e.to_string()))?;
    let mut counts = Map::new();
    for row in rows { let value = row.map_err(|e| error("site_loop_attention_summary_failed", &e.to_string()))?; counts.insert(json_member(&value, "status").as_str().unwrap_or("unknown").to_string(), json_member(&value, "count")); }
    let open_count = counts.get("opened").and_then(Value::as_i64).unwrap_or(0);
    let acknowledged_count = counts.get("acknowledged").and_then(Value::as_i64).unwrap_or(0);
    Ok(json!({"schema":"narada.site_operating_loop.attention_summary.v1","loop_id":loop_id,"counts":counts,"open_count":open_count,"acknowledged_count":acknowledged_count,"open_by_severity":{},"native_hydration":"not_enabled"}))
}

fn attention_list(args: &Map<String, Value>, root: &Path) -> Result<Value, Value> {
    let loop_id = configured_loop_id(args, root);
    let limit = bounded_limit(args, 50);
    let status = args.get("status").and_then(Value::as_str).map(|value| if value == "open" { "opened" } else { value });
    let Some(connection) = open_db(root)? else { return Ok(json!({"schema":"narada.site_operating_loop.loop_attention_list.v1","status":"missing","loop_id":loop_id,"summary":{"loop_id":loop_id,"counts":{},"open_count":0,"acknowledged_count":0,"open_by_severity":{}},"attention":[],"count":0,"db_path":db_path(root).to_string_lossy()})); };
    let mut attention = Vec::new();
    if let Some(status) = status {
        let mut statement = connection.prepare("SELECT * FROM site_loop_escalations WHERE loop_id = ? AND status = ? ORDER BY created_at DESC, escalation_id DESC LIMIT ?").map_err(|e| error("site_loop_attention_query_failed", &e.to_string()))?;
        let rows = statement.query_map(params![loop_id, status, limit], row_value).map_err(|e| error("site_loop_attention_query_failed", &e.to_string()))?;
        for row in rows.take(500) { attention.push(attention_record(row.map_err(|e| error("site_loop_attention_row_failed", &e.to_string()))?)); }
    } else {
        let mut statement = connection.prepare("SELECT * FROM site_loop_escalations WHERE loop_id = ? ORDER BY created_at DESC, escalation_id DESC LIMIT ?").map_err(|e| error("site_loop_attention_query_failed", &e.to_string()))?;
        let rows = statement.query_map(params![loop_id, limit], row_value).map_err(|e| error("site_loop_attention_query_failed", &e.to_string()))?;
        for row in rows.take(500) { attention.push(attention_record(row.map_err(|e| error("site_loop_attention_row_failed", &e.to_string()))?)); }
    }
    Ok(json!({"schema":"narada.site_operating_loop.loop_attention_list.v1","status":"ok","loop_id":loop_id,"summary":attention_summary(&connection, &loop_id)?,"attention":attention,"count":attention.len(),"db_path":db_path(root).to_string_lossy()}))
}

fn attention_show(args: &Map<String, Value>, root: &Path) -> Result<Value, Value> {
    let attention_id = args.get("attention_id").and_then(Value::as_str).filter(|value| !value.is_empty()).ok_or_else(|| error("attention_id_required", "attention_id_required"))?;
    let Some(connection) = open_db(root)? else { return Ok(json!({"schema":"narada.site_operating_loop.loop_attention_show.v1","status":"not_found","attention":Value::Null})); };
    let row = connection.query_row("SELECT * FROM site_loop_escalations WHERE envelope_id = ? OR escalation_id = ? LIMIT 1", params![attention_id, attention_id], row_value).optional().map_err(|e| error("site_loop_attention_query_failed", &e.to_string()))?;
    Ok(json!({"schema":"narada.site_operating_loop.loop_attention_show.v1","status":if row.is_some(){"ok"}else{"not_found"},"attention":row.map(attention_record)}))
}

fn load_config(root: &Path) -> Result<Option<Value>, Value> {
    let path = config_path(root);
    if !path.exists() { return Ok(None); }
    if fs::metadata(&path).map_err(|e| error("site_loop_config_read_failed", &e.to_string()))?.len() > MAX_TEXT_BYTES { return Err(error("site_loop_config_too_large", "site_loop_config_too_large")); }
    let text = fs::read_to_string(&path).map_err(|e| error("site_loop_config_read_failed", &e.to_string()))?;
    let value = serde_json::from_str(&text).map_err(|e| error("site_loop_config_invalid_json", &e.to_string()))?;
    Ok(Some(value))
}

fn doctor(root: &Path) -> Result<Value, Value> {
    let config = load_config(root)?;
    let object = config.as_ref().and_then(Value::as_object).cloned().unwrap_or_default();
    Ok(json!({"schema":"narada.site_loop.doctor.v1","status":"ok","site_root":root.to_string_lossy(),"config_path":config_path(root).to_string_lossy(),"config_status":if config.is_some(){"loaded"}else{"missing"},"config_schema":object.get("schema").cloned().unwrap_or(Value::Null),"site_id":object.get("site_id").cloned().unwrap_or(Value::Null),"loop_id":object.get("loop_id").cloned().unwrap_or(Value::Null),"resident_agent_id":object.get("resident").and_then(Value::as_object).and_then(|v|v.get("agent_id")).cloned().unwrap_or(Value::Null),"native_adapter":"read_only_local","mutations":"authority_boundary","server_name":SERVER_NAME}))
}

fn config_validate(root: &Path) -> Result<Value, Value> {
    let config = load_config(root)?;
    let Some(value) = config else { return Ok(json!({"schema":"narada.site_loop.config_validation.v1","status":"invalid","valid":false,"site_root":root.to_string_lossy(),"path":config_path(root).to_string_lossy(),"schema_id":"narada:site-loop-config.v2.schema.json","config_schema":Value::Null,"loop_id":Value::Null,"site_id":Value::Null,"display_name":Value::Null,"config_path":config_path(root).to_string_lossy(),"errors":["config_missing"],"active_tools_refuse":true})); };
    let object = value.as_object();
    let mut errors = Vec::new();
    if object.is_none() { errors.push("config_object_required"); }
    let object = object.cloned().unwrap_or_default();
    for key in ["schema", "loop_id", "site_id", "display_name", "resident", "scheduler", "policy", "persistence"] {
        if !object.contains_key(key) { errors.push(key); }
    }
    if object.get("schema").and_then(Value::as_str) != Some("narada.site_loop.config.v2") { errors.push("schema_must_be_narada.site_loop.config.v2"); }
    let valid = errors.is_empty();
    Ok(json!({"schema":"narada.site_loop.config_validation.v1","status":if valid{"ok"}else{"invalid"},"valid":valid,"site_root":root.to_string_lossy(),"path":config_path(root).to_string_lossy(),"schema_id":"narada:site-loop-config.v2.schema.json","config_schema":object.get("schema").cloned().unwrap_or(Value::Null),"loop_id":object.get("loop_id").cloned().unwrap_or(Value::Null),"site_id":object.get("site_id").cloned().unwrap_or(Value::Null),"display_name":object.get("display_name").cloned().unwrap_or(Value::Null),"config_path":config_path(root).to_string_lossy(),"errors":errors,"active_tools_refuse":!valid}))
}

fn docs_list(root: &Path) -> Result<Value, Value> {
    let config = load_config(root)?;
    let docs = config.as_ref().and_then(Value::as_object).and_then(|v| v.get("docs")).and_then(Value::as_array).cloned().unwrap_or_default();
    let entries = docs.into_iter().take(100).collect::<Vec<_>>();
    Ok(json!({"status":"ok","site_root":root.to_string_lossy(),"docs":entries}))
}

fn docs_show(args: &Map<String, Value>, root: &Path) -> Result<Value, Value> {
    let requested = args.get("path").and_then(Value::as_str).ok_or_else(|| error("path_required", "path_required"))?;
    let config = load_config(root)?;
    let docs = config.as_ref().and_then(Value::as_object).and_then(|v|v.get("docs")).and_then(Value::as_array).cloned().unwrap_or_default();
    let allowed = docs.iter().filter_map(|item| item.as_object()).filter_map(|item| item.get("path").and_then(Value::as_str)).any(|path| path == requested);
    if !allowed { return Err(error("doc_not_allowlisted", "doc_not_allowlisted")); }
    let path = root.join(requested);
    if path.components().any(|component| matches!(component, std::path::Component::ParentDir)) { return Err(error("doc_path_invalid", "doc_path_invalid")); }
    let Some(metadata) = fs::metadata(&path).ok() else { return Ok(json!({"status":"missing","site_root":root.to_string_lossy(),"path":requested})); };
    if metadata.len() > MAX_TEXT_BYTES { return Err(error("doc_too_large", "doc_too_large")); }
    let text = fs::read_to_string(&path).map_err(|_| error("doc_not_found", "doc_not_found"))?;
    Ok(json!({"status":"ok","site_root":root.to_string_lossy(),"path":requested,"content":text}))
}

fn test_list(root: &Path) -> Result<Value, Value> {
    let config = load_config(root)?;
    let tests = config.as_ref().and_then(Value::as_object).and_then(|v|v.get("tests")).and_then(Value::as_object).cloned().unwrap_or_default();
    let entries = tests.into_iter().take(100).filter_map(|(name, value)| {
        let object = value.as_object()?;
        let command = object.get("command").and_then(Value::as_str)?;
        let args = object.get("args").and_then(Value::as_array).cloned().unwrap_or_default();
        let command_line = std::iter::once(command.to_string())
            .chain(args.into_iter().filter_map(|value| value.as_str().map(ToOwned::to_owned)))
            .collect::<Vec<_>>()
            .join(" ");
        Some(json!({"selector":name,"command":command_line}))
    }).collect::<Vec<_>>();
    Ok(json!({"status":"ok","site_root":root.to_string_lossy(),"tests":entries}))
}

fn status(root: &Path) -> Result<Value, Value> {
    let doctor = doctor(root)?;
    Ok(json!({"schema":"narada.site_loop.status.v1","status":"ok","site_root":root.to_string_lossy(),"config_status":doctor.get("config_status"),"loop_id":doctor.get("loop_id"),"site_id":doctor.get("site_id"),"execution":"not_probed_by_native_read_slice"}))
}

fn unified_status(root: &Path) -> Result<Value, Value> {
    let config = load_config(root)?;
    let object = config.as_ref().and_then(Value::as_object).cloned().unwrap_or_default();
    let scheduler = object.get("scheduler").and_then(Value::as_object).cloned().unwrap_or_default();
    let pid_files = scheduler.get("pid_files").and_then(Value::as_array).cloned().unwrap_or_default();
    let pids = pid_files.into_iter().take(20).filter_map(|value| value.as_str().map(ToOwned::to_owned)).map(|relative| { let path = root.join(&relative); json!({"path":relative,"exists":path.exists()}) }).collect::<Vec<_>>();
    Ok(json!({"schema":"narada.site_loop.unified_status.v1","status":"ok","config_status":if config.is_some(){"loaded"}else{"missing"},"scheduler_task":scheduler.get("default_task_name"),"pid_files":pids,"resident":"not_probed_by_native_read_slice","control":"not_probed_by_native_read_slice","authority_boundary":"scheduler_and_resident_owner"}))
}

fn recovery_plan(root: &Path) -> Result<Value, Value> {
    let config = load_config(root)?;
    let recovery = config.as_ref().and_then(Value::as_object).and_then(|v|v.get("recovery_plan")).cloned().unwrap_or_else(|| json!({"steps":[],"guardrails":[]}));
    Ok(json!({"schema":"narada.site_loop.recovery_plan.v1","status":"ok","plan":recovery,"commands":"reported_only","site_root":root.to_string_lossy()}))
}

fn health(root: &Path) -> Result<Value, Value> {
    let validation = config_validate(root)?;
    Ok(json!({"schema":"narada.site_loop.health.v1","status":if validation.get("valid").and_then(Value::as_bool).unwrap_or(false){"ok"}else{"attention"},"config_valid":validation.get("valid"),"proof":"not_probed_by_native_read_slice","resident":"not_probed_by_native_read_slice","scheduler":"not_probed_by_native_read_slice"}))
}

fn operating_status(root: &Path) -> Result<Value, Value> {
    Ok(json!({"schema":"narada.site_loop.operating_layer_status.v1","status":"ok","unified_status":unified_status(root)?,"health":health(root)?,"task_lifecycle":"delegated_authority","sop":"delegated_authority","scheduler":"delegated_authority"}))
}

fn proof_status(root: &Path) -> Result<Value, Value> {
    let config = load_config(root)?;
    let proof = config.as_ref().and_then(Value::as_object).and_then(|v|v.get("production_proof")).cloned().unwrap_or_else(|| json!({}));
    Ok(json!({"schema":"narada.site_loop.proof_status.v1","status":"ok","production_proof":proof,"freshness":"not_probed_by_native_read_slice","execution":"not_run"}))
}

fn readiness(root: &Path) -> Result<Value, Value> {
    let validation = config_validate(root)?;
    Ok(json!({"schema":"narada.site_loop.readiness.v1","status":if validation.get("valid").and_then(Value::as_bool).unwrap_or(false){"attention"}else{"blocked"},"config_valid":validation.get("valid"),"production_proof":"unverified","resident":"unverified","scheduler":"unverified","native_read_slice":true}))
}

fn coherence(root: &Path) -> Result<Value, Value> {
    let readiness = readiness(root)?;
    Ok(json!({"schema":"narada.site_loop.coherence.v1","status":"attention","blockers":["native_read_slice_does_not_probe_resident_scheduler_or_production_proof"],"readiness":readiness}))
}

fn affordances() -> Value { json!({"schema":"narada.site_loop.operator_affordances.v1","status":"ok","actions":[{"id":"inspect_status","tool":"site_loop_unified_status","read_only":true},{"id":"inspect_health","tool":"site_loop_health","read_only":true},{"id":"inspect_recovery","tool":"site_loop_recovery_plan","read_only":true},{"id":"run_controlled_mutation","tool":"site_loop_run_once","read_only":false,"authority":"site_loop_owner"}]}) }

fn output_schema() -> Value { json!({"type":"object","properties":{"ref":{"type":"string"},"output_ref":{"type":"string"},"offset":{"type":"integer","minimum":0},"limit":{"type":"integer","minimum":0}},"additionalProperties":false}) }

fn output_show(args: &Map<String, Value>, root: &Path) -> Result<Value, Value> {
    let reference = args.get("ref").or_else(||args.get("output_ref")).and_then(Value::as_str).ok_or_else(||error("output_ref_required","output_ref_required"))?;
    let id = reference.strip_prefix("mcp_output:").ok_or_else(||error("output_ref_invalid","output_ref_invalid"))?;
    if id.is_empty() || id.len() > 80 || !id.chars().all(|c|c.is_ascii_alphanumeric() || c == '_' || c == '-') { return Err(error("output_ref_invalid","output_ref_invalid")); }
    let path = root.join(".ai/tmp/mcp-outputs/workspace").join(format!("{id}.json"));
    if fs::metadata(&path).map_err(|_|error("output_ref_not_found","output_ref_not_found"))?.len() > MAX_TEXT_BYTES { return Err(error("output_ref_too_large","output_ref_too_large")); }
    let text = fs::read_to_string(&path).map_err(|_|error("output_ref_not_found","output_ref_not_found"))?;
    let record: Value = serde_json::from_str(&text).map_err(|e|error("output_ref_invalid_json",&e.to_string()))?;
    if record.get("schema").and_then(Value::as_str) != Some("narada.mcp_output_ref.v1") { return Err(error("output_ref_schema_unsupported","output_ref_schema_unsupported")); }
    let full = record.get("full_output").cloned().unwrap_or(Value::Null);
    let presentation = serde_json::to_string_pretty(&full).unwrap_or_else(|_|full.to_string());
    let offset = args.get("offset").and_then(Value::as_u64).unwrap_or(0) as usize;
    let limit = args.get("limit").and_then(Value::as_u64).unwrap_or(4000).min(10000) as usize;
    let chars = presentation.chars().collect::<Vec<_>>(); let start = offset.min(chars.len()); let chunk = chars.iter().skip(start).take(limit).collect::<String>(); let end = start + chunk.chars().count();
    Ok(json!({"schema":"narada.mcp_output_page.v1","status":"ok","ref":reference,"tool_name":record.get("tool_name"),"full_output_char_length":chars.len(),"byte_size":text.len(),"offset":start,"limit":limit,"next_offset":if end<chars.len(){json!(end)}else{Value::Null},"output_truncated":end<chars.len(),"output_text":chunk}))
}

fn authority_boundary(name: &str) -> Value { json!({"schema":"narada.site_loop.authority_boundary.v1","status":"unavailable","tool_name":name,"reason":"site_loop_mutation_not_enabled_in_native_read_slice","remediation":"Use the configured Site Loop authority for scheduler, resident, proof, control, and run mutations."}) }
fn error(code: &str, message: &str) -> Value { json!({"schema":"narada.site_loop.error.v1","code":code,"message":message}) }
fn tool(name: &str, description: &str, input_schema: Value, read_only: bool) -> Value { json!({"name":name,"description":description,"annotations":{"title":name,"readOnlyHint":read_only,"destructiveHint":!read_only,"idempotentHint":read_only,"openWorldHint":true},"inputSchema":input_schema,"outputSchema":{"type":"object","additionalProperties":true}}) }

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn native_site_loop_read_slice_is_bounded_and_refuses_mutation() {
        let root = std::env::temp_dir().join(format!("narada-site-loop-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(root.join(".narada/capabilities")).expect("root");
        fs::write(config_path(&root), r#"{"schema":"narada.site_loop.config.v2","loop_id":"site.loop","site_id":"test","display_name":"Test","resident":{},"scheduler":{},"policy":{},"persistence":{}}"#).expect("config");
        assert_eq!(config_validate(&root).expect("validation")["valid"], true);
        assert_eq!(call_tool("site_loop_run_once", &Map::new(), &root).expect_err("boundary")["status"], "unavailable");
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn native_site_loop_reads_bounded_runs_and_attention() {
        let root = std::env::temp_dir().join(format!("narada-site-loop-store-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(root.join(".ai")).expect("root");
        let connection = Connection::open(db_path(&root)).expect("db");
        connection.execute_batch(
            "CREATE TABLE site_loop_runs (run_id TEXT, loop_id TEXT, status TEXT, dry_run INTEGER, started_at TEXT, finished_at TEXT, summary_json TEXT, error_json TEXT, evidence_ref TEXT, evidence_sha256 TEXT, evidence_bytes INTEGER);\
             CREATE TABLE site_loop_step_runs (step_run_id TEXT, run_id TEXT, step_id TEXT, status TEXT, started_at TEXT, finished_at TEXT, input_refs_json TEXT, output_refs_json TEXT, input_ref_count INTEGER, output_ref_count INTEGER, input_refs_digest TEXT, output_refs_digest TEXT, evidence_json TEXT, error_json TEXT, evidence_ref TEXT, evidence_sha256 TEXT, evidence_bytes INTEGER);\
             CREATE TABLE site_loop_escalations (escalation_id TEXT, loop_id TEXT, directive_id TEXT, classification TEXT, status TEXT, envelope_id TEXT, created_at TEXT, acknowledged_at TEXT, acknowledged_by TEXT, ack_reason TEXT, escalation_summary_json TEXT, escalation_ref TEXT, escalation_sha256 TEXT, escalation_bytes INTEGER);\
             INSERT INTO site_loop_runs VALUES ('run-1','loop-1','completed',0,'2026-01-01T00:00:00Z',NULL,'{\"worked\":true}',NULL,NULL,NULL,NULL);\
             INSERT INTO site_loop_step_runs VALUES ('step-1','run-1','prepare','completed','2026-01-01T00:00:00Z',NULL,'[]','{\"ok\":true}',0,1,NULL,NULL,'{\"evidence\":{}}',NULL,NULL,NULL,NULL);\
             INSERT INTO site_loop_escalations VALUES ('esc-1','loop-1','directive-1','proof','opened','env-1','2026-01-01T00:00:00Z',NULL,NULL,NULL,'{\"severity\":\"error\"}',NULL,NULL,NULL);",
        ).expect("seed");
        drop(connection);
        let mut args = Map::new();
        args.insert("loop_id".into(), json!("loop-1"));
        assert_eq!(runs_list(&args, &root).expect("runs")["count"], 1);
        assert_eq!(run_show(&json_map(json!({"run_id":"run-1"})), &root).expect("run")["steps"][0]["step_id"], "prepare");
        assert_eq!(attention_list(&args, &root).expect("attention")["attention"][0]["severity"], "error");
        assert_eq!(attention_show(&json_map(json!({"attention_id":"env-1"})), &root).expect("attention show")["status"], "ok");
        fs::remove_dir_all(root).expect("cleanup");
    }

    fn json_map(value: Value) -> Map<String, Value> { value.as_object().cloned().expect("object") }
}
