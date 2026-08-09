use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use time::OffsetDateTime;
use uuid::Uuid;

const SERVER_NAME: &str = "narada-site-inbox-mcp";
const STATUSES: &[&str] = &["received", "acknowledged", "dismissed", "promoted"];
const ACTIONS: &[&str] = &[
    "acknowledge",
    "acknowledge_duplicate",
    "archive",
    "materialize",
    "review",
    "review_capa_request",
    "triage",
];
const ROLES: &[&str] = &["architect", "builder", "operator"];
const KINDS: &[&str] = &[
    "proposal",
    "observation",
    "command_request",
    "question",
    "knowledge_candidate",
    "task_candidate",
    "incident",
    "upstream_task_candidate",
];

pub fn list_tools() -> Vec<Value> {
    vec![
        guidance_tool(),
        tool(
            "inbox_doctor",
            "Inspect site-local inbox MCP readiness.",
            schema(json!({"properties":{}}), &[]),
            true,
        ),
        tool(
            "inbox_list",
            "List site-local inbox envelopes ordered by actionability.",
            schema(
                json!({"properties":{
                    "status":{"type":"string","enum":STATUSES,"default":"received"},
                    "kind":{"type":"string","enum":KINDS},
                    "target_role":{"type":"string","enum":ROLES},
                    "action":{"type":"string","enum":ACTIONS},
                    "limit":{"type":"integer","minimum":1,"maximum":100,"default":20}
                }}),
                &[],
            ),
            true,
        ),
        tool(
            "inbox_show",
            "Show one site-local inbox envelope by envelope_id.",
            schema(
                json!({"properties":{"envelope_id":{"type":"string"}}}),
                &["envelope_id"],
            ),
            true,
        ),
        tool(
            "inbox_submit",
            "Submit one site-local inbox envelope and admit it to the local inbox log.",
            schema(
                json!({"properties":{
                    "kind":{"type":"string","enum":KINDS},"title":{"type":"string"},"summary":{"type":"string","default":Value::Null},
                    "principal":{"type":"string"},"target_role":{"type":"string","enum":ROLES},
                    "severity":{"type":"integer","minimum":0,"maximum":100},
                    "authority_level":{"type":"string","enum":["agent_reported","operator_confirmed","operator_directed"],"default":"agent_reported"},
                    "payload":{"type":"object","default":{}}
                }}),
                &["kind", "title", "principal"],
            ),
            false,
        ),
        tool(
            "inbox_acknowledge",
            "Acknowledge an envelope.",
            schema(
                json!({"properties":{"envelope_id":{"type":"string"},"principal":{"type":"string"},"reason":{"type":"string"}}}),
                &["envelope_id", "principal"],
            ),
            false,
        ),
        tool(
            "inbox_dismiss",
            "Dismiss an envelope.",
            schema(
                json!({"properties":{"envelope_id":{"type":"string"},"principal":{"type":"string"},"reason":{"type":"string"}}}),
                &["envelope_id", "principal", "reason"],
            ),
            false,
        ),
        tool(
            "inbox_promote_capa",
            "Promote an envelope to CAPA review status.",
            schema(
                json!({"properties":{"envelope_id":{"type":"string"},"principal":{"type":"string"},"reason":{"type":"string"}}}),
                &["envelope_id", "principal"],
            ),
            false,
        ),
        tool(
            "inbox_audit",
            "Read recent admission log entries.",
            schema(
                json!({"properties":{"limit":{"type":"integer","minimum":1,"maximum":200,"default":50},"envelope_id":{"type":"string"}}}),
                &[],
            ),
            true,
        ),
        tool(
            "inbox_next",
            "Return the next site-local inbox envelope for triage.",
            schema(
                json!({"properties":{"target_role":{"type":"string","enum":ROLES}}}),
                &[],
            ),
            true,
        ),
        tool(
            "capa_queue",
            "List inbox envelopes classified as CAPA review candidates.",
            schema(
                json!({"properties":{"limit":{"type":"integer","minimum":1,"maximum":100,"default":20}}}),
                &[],
            ),
            true,
        ),
        tool(
            "inbox_output_show",
            "Read a materialized Inbox MCP output ref with offset/limit paging.",
            schema(
                json!({"properties":{"ref":{"type":"string"},"output_ref":{"type":"string"},"offset":{"type":"integer","minimum":0},"limit":{"type":"integer","minimum":0}}}),
                &[],
            ),
            true,
        ),
    ]
}

pub fn auxiliary(method: &str, params: &Map<String, Value>) -> Result<Value, Value> {
    match method {
        "prompts/list" => Ok(
            json!({"prompts":[{"name":"inbox_triage_workflow","title":"Inbox Triage Workflow","description":"Guidance for inbox intake and triage.","arguments":[]}]}),
        ),
        "prompts/get" => {
            if params.get("name").and_then(Value::as_str) != Some("inbox_triage_workflow") {
                return Err(error("unknown_prompt", "unknown_prompt"));
            }
            Ok(
                json!({"description":"Guidance for inbox intake and triage.","messages":[{"role":"user","content":{"type":"text","text":"Use inbox_list or inbox_next to inspect actionable envelopes, then use inbox_show before disposition or follow-up workflows."}}]}),
            )
        }
        "completion/complete" => {
            let is_name = params
                .get("argument")
                .and_then(Value::as_object)
                .and_then(|v| v.get("name"))
                .and_then(Value::as_str)
                == Some("name");
            let values = if is_name {
                list_tools()
                    .iter()
                    .filter_map(|v| v.get("name").cloned())
                    .take(100)
                    .collect::<Vec<_>>()
            } else {
                Vec::new()
            };
            Ok(json!({"completion":{"values":values,"total":values.len(),"hasMore":false}}))
        }
        "logging/setLevel" => Ok(json!({})),
        _ => Err(error(
            "unsupported_mcp_method",
            &format!("unsupported_mcp_method:{method}"),
        )),
    }
}

pub fn call_tool(name: &str, args: &Map<String, Value>, root: &Path) -> Result<Value, Value> {
    match name {
        "inbox_guidance" => Ok(guidance(args)),
        "inbox_doctor" => doctor(root),
        "inbox_list" => list(args, root),
        "inbox_show" => show(args, root),
        "inbox_submit" => submit(args, root),
        "inbox_acknowledge" => disposition(args, root, "acknowledged"),
        "inbox_dismiss" => disposition(args, root, "dismissed"),
        "inbox_promote_capa" => disposition(args, root, "promoted"),
        "inbox_audit" => audit(args, root),
        "inbox_next" => next(args, root),
        "capa_queue" => capa(args, root),
        "inbox_output_show" => output_show(args, root),
        _ => Err(error("unknown_tool", &format!("unknown_tool:{name}"))),
    }
}

fn schema(properties: Value, required: &[&str]) -> Value {
    let mut result = properties.as_object().cloned().unwrap_or_default();
    result.insert("type".into(), Value::String("object".into()));
    result.insert("additionalProperties".into(), Value::Bool(false));
    if !required.is_empty() {
        result.insert("required".into(), json!(required));
    }
    Value::Object(result)
}

fn guidance_tool() -> Value {
    tool(
        "inbox_guidance",
        "Show model-facing operating guidance for site-inbox MCP workflows.",
        schema(
            json!({"properties":{"workflow":{"type":"string"},"tool":{"type":"string"}}}),
            &[],
        ),
        true,
    )
}

fn tool(name: &str, description: &str, schema: Value, read_only: bool) -> Value {
    json!({
        "name": name, "description": description,
        "annotations": {
            "title": name, "readOnlyHint": read_only, "destructiveHint": name == "inbox_dismiss",
            "idempotentHint": name == "inbox_guidance" || name.contains("doctor") || name.contains("list") || name.contains("show") || name.contains("queue") || name.contains("audit"),
            "openWorldHint": false
        },
        "inputSchema": schema, "outputSchema": {"type":"object","additionalProperties":true}
    })
}

fn guidance(args: &Map<String, Value>) -> Value {
    json!({
        "schema":"narada.mcp_surface.guidance.v0","status":"ok","surface_id":"site-inbox",
        "guidance_tool":"inbox_guidance","purpose":"Governed site inbox intake and triage.",
        "requested":{"workflow":optional_string(args,"workflow"),"tool":optional_string(args,"tool")},
        "first_use":[
            "Call this guidance command when the surface is unfamiliar, when a refusal/error is unclear, or before composing a multi-step workflow.",
            "Inspect policy/doctor/status tools before mutation or open-world operations.",
            "Use bounded list/search/query tools for discovery, then show/read/detail tools before acting on a specific object.",
            "Preserve structuredContent as authoritative evidence; text content is for assistant readability."
        ],
        "tool_preference":[
            {"step":"orient","guidance":"Use *_guidance first when uncertain, then policy/doctor/status tools."},
            {"step":"discover","guidance":"Use bounded list/search/query commands with explicit limits and filters."},
            {"step":"inspect","guidance":"Use show/read/detail commands for exact targets before mutation."},
            {"step":"mutate","guidance":"Only call mutation tools after policy allows it and intent, target, and expected result are explicit."},
            {"step":"verify","guidance":"Read back state with the owning surface after any mutation."}
        ],
        "boundaries":[
            "Guidance is read-only model-facing operating advice.",
            "Guidance does not weaken policy, authorize mutation, or replace tool schemas.",
            "The owning MCP surface remains authoritative for state and enforcement."
        ]
    })
}

fn submit(args: &Map<String, Value>, root: &Path) -> Result<Value, Value> {
    let kind = required(args, "kind")?;
    if !KINDS.contains(&kind.as_str()) {
        return Err(error(
            "invalid_envelope_kind",
            &format!("invalid_envelope_kind:{kind}; allowed={}", KINDS.join(",")),
        ));
    }
    let title = required(args, "title")?;
    let principal = required(args, "principal")?;
    if let Some(role) = optional_string(args, "target_role") {
        if !ROLES.contains(&role.as_str()) {
            return Err(error(
                "invalid_request",
                "target_role_must_be_architect_builder_or_operator",
            ));
        }
    }
    let mut payload = args
        .get("payload")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    payload.insert("title".into(), Value::String(title.clone()));
    payload.insert(
        "summary".into(),
        args.get("summary").cloned().unwrap_or(Value::Null),
    );
    payload.insert("principal".into(), Value::String(principal.clone()));
    let authority = json!({
        "level": optional_string(args,"authority_level").unwrap_or_else(||"agent_reported".into()),
        "principal": principal
    });
    let source = json!({"kind":"inbox_mcp_submit","principal":principal});
    let envelope = json!({
        "kind":kind, "title":title, "summary":args.get("summary").cloned().unwrap_or(Value::Null),
        "status":"received", "target_role":args.get("target_role").cloned().unwrap_or(Value::Null),
        "severity":args.get("severity").cloned().unwrap_or(Value::Null),
        "authority":authority, "source":source, "payload":payload
    });
    let (id, path, event) = admit(root, envelope)?;
    refresh(root)?;
    Ok(json!({
        "status":"admitted","site_root":root.to_string_lossy(),"envelope_id":id,
        "envelope_path":path,"event_id":event.get("event_id"),"event_sequence":event.get("event_sequence")
    }))
}

fn disposition(args: &Map<String, Value>, root: &Path, status: &str) -> Result<Value, Value> {
    let id = required(args, "envelope_id")?;
    let principal = required(args, "principal")?;
    if read_row(root, &id)?.is_none() {
        return Ok(json!({"status":"not_found","envelope_id":id}));
    }
    let reason = optional_string(args, "reason");
    if status == "dismissed" && reason.is_none() {
        return Err(error("reason_required", "reason_required"));
    }
    let event = append(
        root,
        json!({
            "envelope_id":id, "event_kind":format!("envelope_{status}"), "principal":principal,
            "authority_level":"agent_reported", "event_payload":{"reason":reason}
        }),
    )?;
    refresh(root)?;
    Ok(json!({
        "status":status,"envelope_id":id,"event_id":event.get("event_id"),
        "event_sequence":event.get("event_sequence"),"reason":reason
    }))
}

fn doctor(root: &Path) -> Result<Value, Value> {
    let (indexed, invalid) = refresh(root)?;
    let rows = rows_after_refresh(root)?;
    let mut counts = Map::new();
    for row in rows
        .iter()
        .filter(|r| r.get("status").and_then(Value::as_str) == Some("received"))
    {
        inc(&mut counts, "total");
        if row.get("severity").and_then(Value::as_i64).unwrap_or(0) >= 70 {
            inc(&mut counts, "high_severity");
        }
        if row.get("kind").and_then(Value::as_str) == Some("incident") {
            inc(&mut counts, "incidents");
        }
        if row.get("action").and_then(Value::as_str) == Some("review_capa_request") {
            inc(&mut counts, "capa_requests");
        }
        if row.get("kind").and_then(Value::as_str) == Some("observation") {
            inc(&mut counts, "observations");
        }
        if row.get("kind").and_then(Value::as_str) == Some("proposal") {
            inc(&mut counts, "proposals");
        }
    }
    for key in [
        "total",
        "high_severity",
        "incidents",
        "capa_requests",
        "observations",
        "proposals",
    ] {
        counts.entry(key).or_insert(Value::from(0));
    }
    Ok(json!({
        "status":"ok","site_root":root.to_string_lossy(),
        "db_path":root.join(".ai/state/inbox-index.sqlite").to_string_lossy(),
        "storage_mode":"node_sqlite","indexed_count":indexed,"invalid_count":invalid,
        "counts":counts,"server_name":SERVER_NAME
    }))
}

fn inc(map: &mut Map<String, Value>, key: &str) {
    let n = map.get(key).and_then(Value::as_i64).unwrap_or(0) + 1;
    map.insert(key.into(), Value::from(n));
}

fn list(args: &Map<String, Value>, root: &Path) -> Result<Value, Value> {
    let status = enum_arg(args, "status", Some("received"), STATUSES)?;
    let kind = enum_arg(args, "kind", None, KINDS)?;
    let role = enum_arg(args, "target_role", None, ROLES)?;
    let action = enum_arg(args, "action", None, ACTIONS)?;
    let limit = bounded(args.get("limit"), 20, 100);
    let mut rows = rows(root)?;
    rows.retain(|r| {
        status
            .as_deref()
            .map(|v| r.get("status").and_then(Value::as_str) == Some(v))
            .unwrap_or(true)
    });
    rows.retain(|r| {
        kind.as_deref()
            .map(|v| r.get("kind").and_then(Value::as_str) == Some(v))
            .unwrap_or(true)
    });
    rows.retain(|r| {
        role.as_deref()
            .map(|v| r.get("target_role").and_then(Value::as_str) == Some(v))
            .unwrap_or(true)
    });
    rows.retain(|r| {
        action
            .as_deref()
            .map(|v| r.get("action").and_then(Value::as_str) == Some(v))
            .unwrap_or(true)
    });
    Ok(json!({
        "status":"ok","site_root":root.to_string_lossy(),"storage_mode":"node_sqlite",
        "filters":{"status":status,"kind":kind,"target_role":role,"action":action},
        "count":rows.len(),"envelopes":rows.iter().take(limit).map(summary).collect::<Vec<_>>()
    }))
}

fn show(args: &Map<String, Value>, root: &Path) -> Result<Value, Value> {
    let id = required(args, "envelope_id")?;
    let Some(row) = read_row(root, &id)? else {
        return Ok(json!({"status":"not_found","envelope_id":id}));
    };
    let mut envelope = summary(&row).as_object().cloned().unwrap_or_default();
    envelope.insert(
        "payload".into(),
        row.get("payload_json")
            .and_then(Value::as_str)
            .and_then(|v| serde_json::from_str(v).ok())
            .unwrap_or(Value::Null),
    );
    Ok(json!({"status":"ok","site_root":root.to_string_lossy(),"envelope":envelope}))
}

fn next(args: &Map<String, Value>, root: &Path) -> Result<Value, Value> {
    let role = optional_string(args, "target_role");
    let rows = rows(root)?
        .into_iter()
        .filter(|r| r.get("status").and_then(Value::as_str) == Some("received"))
        .filter(|r| {
            role.as_deref()
                .map(|v| r.get("target_role").and_then(Value::as_str) == Some(v))
                .unwrap_or(true)
        })
        .collect::<Vec<_>>();
    Ok(json!({
        "status":if rows.is_empty(){"empty"}else{"ok"},
        "site_root":root.to_string_lossy(),"envelope":rows.first().map(summary)
    }))
}

fn capa(args: &Map<String, Value>, root: &Path) -> Result<Value, Value> {
    let limit = bounded(args.get("limit"), 20, 100);
    let rows = rows(root)?
        .into_iter()
        .filter(|r| r.get("status").and_then(Value::as_str) == Some("received"))
        .filter(|r| {
            r.get("action").and_then(Value::as_str) == Some("review_capa_request")
                || r.get("kind").and_then(Value::as_str) == Some("incident")
        })
        .collect::<Vec<_>>();
    Ok(json!({
        "status":"ok","site_root":root.to_string_lossy(),"count":rows.len(),
        "envelopes":rows.iter().take(limit).map(summary).collect::<Vec<_>>()
    }))
}

fn audit(args: &Map<String, Value>, root: &Path) -> Result<Value, Value> {
    let limit = bounded(args.get("limit"), 50, 200);
    let id = optional_string(args, "envelope_id");
    let mut entries = read_log(root)?;
    if let Some(id) = id {
        entries.retain(|e| e.get("envelope_id").and_then(Value::as_str) == Some(id.as_str()));
    }
    let total = entries.len();
    let entries = entries
        .into_iter()
        .rev()
        .take(limit)
        .map(|e| {
            json!({
                "event_id":e.get("event_id"),"event_sequence":e.get("event_sequence"),
                "event_kind":e.get("event_kind"),"envelope_id":e.get("envelope_id"),
                "principal":e.get("principal"),"timestamp":e.get("timestamp"),
                "payload":e.get("event_payload")
            })
        })
        .collect::<Vec<_>>();
    Ok(
        json!({"status":"ok","site_root":root.to_string_lossy(),"total_entries":total,"count":entries.len(),"entries":entries}),
    )
}

fn summary(row: &Map<String, Value>) -> Value {
    json!({
        "envelope_id":row.get("envelope_id"),"status":row.get("status"),"kind":row.get("kind"),
        "title":row.get("title"),"summary":row.get("summary"),"received_at":row.get("received_at"),
        "target_role":row.get("target_role"),"severity":row.get("severity"),
        "severity_reason":row.get("severity_reason"),"action":row.get("action"),
        "file_path":row.get("file_path")
    })
}

fn output_show(args: &Map<String, Value>, root: &Path) -> Result<Value, Value> {
    let reference = optional_string(args, "ref")
        .or_else(|| optional_string(args, "output_ref"))
        .ok_or_else(|| error("output_ref_required", "output_ref_required"))?;
    let id = reference.strip_prefix("mcp_output:").ok_or_else(|| {
        error(
            "output_ref_invalid",
            &format!("output_ref_invalid:{reference}"),
        )
    })?;
    if id.is_empty()
        || id.len() > 80
        || !id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        return Err(error(
            "output_ref_invalid",
            &format!("output_ref_invalid:{reference}"),
        ));
    }
    let path = root
        .join(".ai/tmp/mcp-outputs/workspace")
        .join(format!("{id}.json"));
    let text = fs::read_to_string(&path).map_err(|_| {
        error(
            "output_ref_not_found",
            &format!("output_ref_not_found:{reference}"),
        )
    })?;
    let record: Value = serde_json::from_str(&text)
        .map_err(|e| error("output_ref_invalid_json", &e.to_string()))?;
    if record.get("schema").and_then(Value::as_str) != Some("narada.mcp_output_ref.v1") {
        return Err(error(
            "output_ref_schema_unsupported",
            "output_ref_schema_unsupported",
        ));
    }
    let full = record.get("full_output").cloned().unwrap_or(Value::Null);
    let presentation = serde_json::to_string_pretty(&full).unwrap_or_else(|_| full.to_string());
    let offset = args.get("offset").and_then(Value::as_u64).unwrap_or(0) as usize;
    let limit = args
        .get("limit")
        .and_then(Value::as_u64)
        .unwrap_or(4000)
        .min(10000) as usize;
    let chars = presentation.chars().collect::<Vec<_>>();
    let start = offset.min(chars.len());
    let chunk = chars.iter().skip(start).take(limit).collect::<String>();
    let end = start + chunk.chars().count();
    Ok(json!({
        "schema":"narada.mcp_output_page.v1","status":"ok","ref":reference,
        "tool_name":record.get("tool_name"),"full_output_char_length":chars.len(),
        "byte_size":text.len(),"original_truncated":record.get("truncated").and_then(Value::as_bool).unwrap_or(false),
        "path":path.to_string_lossy(),"offset":start,"limit":limit,
        "next_offset":if end<chars.len(){json!(end)}else{Value::Null},
        "output_limit":limit,"output_truncated":end<chars.len(),"output_text":chunk
    }))
}

fn refresh(root: &Path) -> Result<(i64, usize), Value> {
    let mut db = open_db(root)?;
    let now = now_iso();
    let latest = latest(root);
    let files = envelope_files(root);
    let tx = db
        .transaction()
        .map_err(|e| db_err("inbox_index_transaction_failed", e))?;
    tx.execute("DELETE FROM inbox_envelopes", [])
        .map_err(|e| db_err("inbox_index_clear_failed", e))?;
    let mut invalid = 0usize;
    for path in files {
        let text = match fs::read_to_string(&path) {
            Ok(v) => v.trim_start_matches('\u{feff}').to_string(),
            Err(_) => {
                invalid += 1;
                continue;
            }
        };
        let value: Value = match serde_json::from_str(&text) {
            Ok(v) => v,
            Err(_) => {
                invalid += 1;
                continue;
            }
        };
        let Some(envelope) = value.as_object() else {
            invalid += 1;
            continue;
        };
        let Some(id) = envelope.get("envelope_id").and_then(Value::as_str) else {
            invalid += 1;
            continue;
        };
        if !valid_id(id) {
            invalid += 1;
            continue;
        }
        let sev = severity(envelope);
        let auth = envelope.get("authority").and_then(Value::as_object);
        let payload = envelope.get("payload").and_then(Value::as_object);
        let source = envelope.get("source").and_then(Value::as_object);
        let status = effective(envelope, latest.get(id));
        tx.execute(
            "INSERT INTO inbox_envelopes(envelope_id,file_path,status,kind,authority_level,title,summary,principal,source_ref,received_at,target_role,severity,severity_reason,action,payload_json,indexed_at) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16)",
            params![
                id, path.to_string_lossy().to_string(), status,
                envelope.get("kind").and_then(Value::as_str).unwrap_or("observation"),
                auth.and_then(|a| a.get("level")).and_then(Value::as_str).unwrap_or("agent_reported"),
                envelope.get("title").and_then(Value::as_str).or_else(|| payload.and_then(|p| p.get("title")).and_then(Value::as_str)).unwrap_or("(untitled)"),
                envelope.get("summary").and_then(Value::as_str).or_else(|| payload.and_then(|p| p.get("summary")).and_then(Value::as_str)),
                envelope.get("principal").and_then(Value::as_str).or_else(|| auth.and_then(|a| a.get("principal")).and_then(Value::as_str)).or_else(|| payload.and_then(|p| p.get("principal")).and_then(Value::as_str)),
                source.and_then(|s| s.get("ref")).and_then(Value::as_str),
                envelope.get("received_at").and_then(Value::as_str),
                sev.role, sev.value, sev.reason, sev.action, text, now
            ],
        ).map_err(|e| db_err("inbox_index_insert_failed", e))?;
    }
    tx.execute(
        "INSERT INTO inbox_index_meta(key,value,updated_at) VALUES('last_refreshed_at',?1,?1) ON CONFLICT(key) DO UPDATE SET value=excluded.value,updated_at=excluded.updated_at",
        params![now.clone()],
    ).map_err(|e| db_err("inbox_index_meta_failed", e))?;
    tx.commit()
        .map_err(|e| db_err("inbox_index_commit_failed", e))?;
    let count = db
        .query_row("SELECT COUNT(*) FROM inbox_envelopes", [], |r| {
            r.get::<_, i64>(0)
        })
        .map_err(|e| db_err("inbox_index_count_failed", e))?;
    Ok((count, invalid))
}

fn open_db(root: &Path) -> Result<Connection, Value> {
    let path = root.join(".ai/state/inbox-index.sqlite");
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|e| error("inbox_index_directory_failed", &e.to_string()))?;
    }
    let db = Connection::open(path).map_err(|e| db_err("inbox_index_open_failed", e))?;
    db.execute_batch(
        "PRAGMA journal_mode=WAL; PRAGMA user_version=1;
         CREATE TABLE IF NOT EXISTS inbox_index_meta(key TEXT PRIMARY KEY,value TEXT NOT NULL,updated_at TEXT NOT NULL);
         CREATE TABLE IF NOT EXISTS inbox_envelopes(
           envelope_id TEXT PRIMARY KEY,file_path TEXT NOT NULL,status TEXT NOT NULL,kind TEXT NOT NULL,
           authority_level TEXT,title TEXT,summary TEXT,principal TEXT,source_ref TEXT,received_at TEXT,
           target_role TEXT,severity INTEGER,severity_reason TEXT,action TEXT,payload_json TEXT NOT NULL,indexed_at TEXT NOT NULL
         );
         CREATE INDEX IF NOT EXISTS idx_inbox_envelopes_status_received ON inbox_envelopes(status,received_at);
         CREATE INDEX IF NOT EXISTS idx_inbox_envelopes_severity ON inbox_envelopes(status,severity DESC,received_at);",
    ).map_err(|e| db_err("inbox_index_schema_failed", e))?;
    db.execute(
        "INSERT INTO inbox_index_meta(key,value,updated_at) VALUES('schema_version','1',?1) ON CONFLICT(key) DO UPDATE SET value=excluded.value,updated_at=excluded.updated_at",
        params![now_iso()],
    ).map_err(|e| db_err("inbox_index_meta_failed", e))?;
    Ok(db)
}

fn rows(root: &Path) -> Result<Vec<Map<String, Value>>, Value> {
    refresh(root)?;
    rows_after_refresh(root)
}

fn rows_after_refresh(root: &Path) -> Result<Vec<Map<String, Value>>, Value> {
    let db = open_db(root)?;
    let mut statement = db.prepare(
        "SELECT envelope_id,file_path,status,kind,authority_level,title,summary,principal,source_ref,
         received_at,target_role,severity,severity_reason,action,payload_json,indexed_at
         FROM inbox_envelopes ORDER BY COALESCE(severity,0) DESC,COALESCE(received_at,'') ASC",
    ).map_err(|e| db_err("inbox_index_query_failed", e))?;
    let result = statement
        .query_map([], row_record)
        .map_err(|e| db_err("inbox_index_query_failed", e))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| db_err("inbox_index_query_failed", e));
    result
}

fn read_row(root: &Path, id: &str) -> Result<Option<Map<String, Value>>, Value> {
    refresh(root)?;
    let db = open_db(root)?;
    db.query_row(
        "SELECT envelope_id,file_path,status,kind,authority_level,title,summary,principal,source_ref,
         received_at,target_role,severity,severity_reason,action,payload_json,indexed_at
         FROM inbox_envelopes WHERE envelope_id=?1",
        params![id], row_record,
    ).optional().map_err(|e| db_err("inbox_index_query_failed", e))
}

fn row_record(row: &rusqlite::Row<'_>) -> rusqlite::Result<Map<String, Value>> {
    let fields = [
        (0, "envelope_id"),
        (1, "file_path"),
        (2, "status"),
        (3, "kind"),
        (4, "authority_level"),
        (5, "title"),
        (6, "summary"),
        (7, "principal"),
        (8, "source_ref"),
        (9, "received_at"),
        (10, "target_role"),
        (12, "severity_reason"),
        (13, "action"),
        (14, "payload_json"),
        (15, "indexed_at"),
    ];
    let mut out = Map::new();
    for (index, key) in fields {
        let value: Option<String> = row.get(index)?;
        out.insert(key.into(), value.map(Value::String).unwrap_or(Value::Null));
    }
    let severity: Option<i64> = row.get(11)?;
    out.insert(
        "severity".into(),
        severity.map(Value::from).unwrap_or(Value::Null),
    );
    Ok(out)
}

#[derive(Clone)]
struct Severity {
    role: Option<String>,
    value: Option<i64>,
    reason: Option<String>,
    action: Option<String>,
}

fn severity(e: &Map<String, Value>) -> Severity {
    if let Some(role) = e.get("target_role").and_then(Value::as_str) {
        return Severity {
            role: Some(role.into()),
            value: Some(e.get("severity").and_then(Value::as_i64).unwrap_or(50)),
            reason: Some("explicit_target_role".into()),
            action: Some("materialize".into()),
        };
    }
    let kind = e
        .get("kind")
        .and_then(Value::as_str)
        .unwrap_or("observation");
    let authority = e
        .get("authority")
        .and_then(Value::as_object)
        .and_then(|a| a.get("level"))
        .and_then(Value::as_str)
        .unwrap_or("agent_reported");
    let payload = e.get("payload").and_then(Value::as_object);
    if kind == "incident" {
        return Severity {
            role: Some("architect".into()),
            value: Some(90),
            reason: Some("incident_always_materializes".into()),
            action: Some("materialize".into()),
        };
    }
    if payload
        .and_then(|p| p.get("capa_request"))
        .and_then(Value::as_object)
        .is_some()
    {
        let value = if authority == "operator_confirmed" || authority == "operator_directed" {
            75
        } else {
            60
        };
        return Severity {
            role: Some("architect".into()),
            value: Some(value),
            reason: Some("capa_request_requires_promotion_review".into()),
            action: Some("review_capa_request".into()),
        };
    }
    if kind == "observation" {
        let recommendation = payload
            .and_then(|p| p.get("recommendation"))
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_lowercase();
        let proposals = payload
            .and_then(|p| p.get("proposal"))
            .and_then(Value::as_array)
            .map(Vec::len)
            .unwrap_or(0);
        let (value, reason) = if recommendation.contains("address before next operational cycle") {
            (70, "observation_urgent_recommendation")
        } else if proposals >= 3 {
            (50, "observation_many_proposals")
        } else if proposals >= 1 {
            (30, "observation_some_proposals")
        } else {
            (20, "observation_low_severity")
        };
        return Severity {
            role: Some("architect".into()),
            value: Some(value),
            reason: Some(reason.into()),
            action: Some("materialize".into()),
        };
    }
    let (value, reason) = match kind {
        "proposal" => (40, "proposal_architect_triage"),
        "command_request" => (45, "command_request_architect_triage"),
        _ => (20, "default_architect_triage"),
    };
    Severity {
        role: Some("architect".into()),
        value: Some(value),
        reason: Some(reason.into()),
        action: Some("materialize".into()),
    }
}

fn effective(e: &Map<String, Value>, latest: Option<&Value>) -> String {
    match latest
        .and_then(Value::as_object)
        .and_then(|v| v.get("event_kind"))
        .and_then(Value::as_str)
    {
        Some("envelope_acknowledged") => "acknowledged",
        Some("envelope_dismissed") => "dismissed",
        Some("envelope_promoted") => "promoted",
        _ => e
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or("received"),
    }
    .into()
}

fn latest(root: &Path) -> std::collections::HashMap<String, Value> {
    let mut out = std::collections::HashMap::new();
    if let Ok(entries) = read_log(root) {
        for entry in entries {
            if let Some(id) = entry.get("envelope_id").and_then(Value::as_str) {
                out.insert(id.into(), entry);
            }
        }
    }
    out
}

fn envelope_files(root: &Path) -> Vec<PathBuf> {
    root.join(".ai/inbox-envelopes")
        .read_dir()
        .ok()
        .into_iter()
        .flatten()
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| path.extension().and_then(|s| s.to_str()) == Some("json"))
        .collect()
}

fn valid_id(value: &str) -> bool {
    value
        .strip_prefix("env_")
        .map(|rest| {
            !rest.is_empty()
                && rest
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
        })
        .unwrap_or(false)
}

fn admit(root: &Path, envelope: Value) -> Result<(String, String, Value), Value> {
    let mut object = envelope
        .as_object()
        .cloned()
        .ok_or_else(|| error("invalid_envelope", "invalid_envelope"))?;
    let id = object
        .get("envelope_id")
        .and_then(Value::as_str)
        .map(str::to_string)
        .unwrap_or_else(|| format!("env_{}", Uuid::new_v4()));
    let received = object
        .get("received_at")
        .and_then(Value::as_str)
        .map(str::to_string)
        .unwrap_or_else(now_iso);
    object.insert("envelope_id".into(), Value::String(id.clone()));
    object.insert("received_at".into(), Value::String(received.clone()));
    let directory = root.join(".ai/inbox-envelopes");
    fs::create_dir_all(&directory)
        .map_err(|e| error("inbox_envelope_directory_failed", &e.to_string()))?;
    let name = format!(
        "{}-{}.json",
        received.replace(':', "-").replace('.', "-"),
        id
    );
    let path = directory.join(name);
    let text = serde_json::to_string_pretty(&Value::Object(object.clone()))
        .map_err(|e| error("inbox_envelope_encode_failed", &e.to_string()))?;
    fs::write(&path, text).map_err(|e| error("inbox_envelope_write_failed", &e.to_string()))?;
    let authority = object.get("authority").and_then(Value::as_object);
    let source = object.get("source").and_then(Value::as_object);
    let payload_uri = format!(
        ".ai/inbox-envelopes/{}",
        path.file_name().and_then(|v| v.to_str()).unwrap_or("")
    );
    append(
        root,
        json!({
            "envelope_id":id,"event_kind":"envelope_received",
            "principal":authority.and_then(|a|a.get("principal")).and_then(Value::as_str).unwrap_or("unknown"),
            "authority_level":authority.and_then(|a|a.get("level")).and_then(Value::as_str).unwrap_or("agent_reported"),
            "payload_hash":hash(&Value::Object(object.clone())),"payload_uri":payload_uri,
            "event_payload":{"source_ref":source.and_then(|s|s.get("ref")),"source_kind":source.and_then(|s|s.get("kind")),"target_locus":"local_site","transport":"mcp_cli"}
        }),
    )?;
    let event = append(
        root,
        json!({
            "envelope_id":id,"event_kind":"envelope_admitted","principal":"inbox_mcp",
            "authority_level":"system_detected","payload_hash":hash(&Value::Object(object)),
            "payload_uri":payload_uri,"event_payload":{"admission_gate":"inbox_mcp_submit","validation_result":"passed","routing_decision":"local_site"}
        }),
    )?;
    Ok((id, path.to_string_lossy().to_string(), event))
}

fn append(root: &Path, event: Value) -> Result<Value, Value> {
    let directory = root.join(".ai/state");
    fs::create_dir_all(&directory)
        .map_err(|e| error("inbox_log_directory_failed", &e.to_string()))?;
    let path = directory.join("inbox-admission.log");
    if path.metadata().map(|m| m.len()).unwrap_or(0) >= 10 * 1024 * 1024 {
        let rotated = directory.join(format!("inbox-admission-{}.log", &now_iso()[..10]));
        let old = fs::read_to_string(&path).unwrap_or_default();
        fs::write(rotated, old).map_err(|e| error("inbox_log_rotation_failed", &e.to_string()))?;
        fs::write(&path, "").map_err(|e| error("inbox_log_rotation_failed", &e.to_string()))?;
    }
    let sequence = read_log(root)?
        .last()
        .and_then(|e| e.get("event_sequence"))
        .and_then(Value::as_i64)
        .unwrap_or(0)
        + 1;
    let mut result = Map::new();
    result.insert(
        "schema".into(),
        Value::String("narada.inbox.admission_log.entry.v0".into()),
    );
    result.insert(
        "event_id".into(),
        Value::String(format!(
            "evt_{}",
            Uuid::new_v4().to_string().replace('-', "")
        )),
    );
    result.insert("event_sequence".into(), Value::from(sequence));
    result.insert("timestamp".into(), Value::String(now_iso()));
    if let Some(input) = event.as_object() {
        for (key, value) in input {
            result.insert(key.clone(), value.clone());
        }
    }
    let line = serde_json::to_string(&Value::Object(result.clone()))
        .map_err(|e| error("inbox_log_encode_failed", &e.to_string()))?;
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|e| error("inbox_log_open_failed", &e.to_string()))?;
    writeln!(file, "{line}").map_err(|e| error("inbox_log_write_failed", &e.to_string()))?;
    Ok(Value::Object(result))
}

fn read_log(root: &Path) -> Result<Vec<Value>, Value> {
    let path = root.join(".ai/state/inbox-admission.log");
    let text = match fs::read_to_string(path) {
        Ok(value) => value,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(error("inbox_log_read_failed", &e.to_string())),
    };
    Ok(text
        .lines()
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect())
}

fn hash(value: &Value) -> String {
    let mut digest = Sha256::new();
    digest.update(serde_json::to_vec(value).unwrap_or_default());
    let encoded = digest
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    format!("sha256:{encoded}")
}

fn required(args: &Map<String, Value>, key: &str) -> Result<String, Value> {
    optional_string(args, key)
        .ok_or_else(|| error("required_argument_missing", &format!("{key}_required")))
}
fn optional_string(args: &Map<String, Value>, key: &str) -> Option<String> {
    args.get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(str::to_string)
}
fn enum_arg(
    args: &Map<String, Value>,
    key: &str,
    default: Option<&str>,
    allowed: &[&str],
) -> Result<Option<String>, Value> {
    let value = if let Some(raw) = args.get(key) {
        raw.as_str().map(str::to_string).ok_or_else(|| {
            error(
                "invalid_request",
                &format!("{key}_must_be_one_of: {}", allowed.join(",")),
            )
        })?
    } else {
        default.map(str::to_string).unwrap_or_default()
    };
    if value.is_empty() {
        return Ok(None);
    }
    if !allowed.contains(&value.as_str()) {
        return Err(error(
            "invalid_request",
            &format!("{key}_must_be_one_of: {}", allowed.join(",")),
        ));
    }
    Ok(Some(value))
}
fn bounded(value: Option<&Value>, default: usize, maximum: usize) -> usize {
    value
        .and_then(Value::as_i64)
        .filter(|v| *v > 0)
        .map(|v| (v as usize).min(maximum))
        .unwrap_or(default)
}
fn error(code: &str, message: &str) -> Value {
    json!({"code":code,"message":message})
}
fn db_err(code: &str, cause: rusqlite::Error) -> Value {
    error(code, &format!("{code}:{cause}"))
}
fn now_iso() -> String {
    OffsetDateTime::now_utc()
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_else(|_| "1970-01-01T00:00:00Z".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn submit_next_ack_show_roundtrip() {
        let root =
            std::env::temp_dir().join(format!("narada-site-inbox-native-{}", Uuid::new_v4()));
        fs::create_dir_all(&root).expect("root");
        let args: Map<String, Value> = serde_json::from_value(json!({
            "kind":"incident","title":"Native inbox test","principal":"native-test","payload":{"summary":"test"}
        })).expect("args");
        let submitted = submit(&args, &root).expect("submit");
        let id = submitted
            .get("envelope_id")
            .and_then(Value::as_str)
            .expect("id")
            .to_string();
        let next_value = next(&Map::new(), &root).expect("next");
        assert_eq!(next_value["status"], "ok");
        assert_eq!(next_value["envelope"]["envelope_id"], id);
        let ack_args: Map<String, Value> =
            serde_json::from_value(json!({"envelope_id":id,"principal":"native-test"}))
                .expect("ack args");
        let acked = disposition(&ack_args, &root, "acknowledged").expect("ack");
        assert_eq!(acked["status"], "acknowledged");
        let show_args: Map<String, Value> =
            serde_json::from_value(json!({"envelope_id":id})).expect("show args");
        let shown = show(&show_args, &root).expect("show");
        assert_eq!(shown["envelope"]["status"], "acknowledged");
        assert_eq!(shown["envelope"]["payload"]["title"], "Native inbox test");
        fs::remove_dir_all(&root).expect("cleanup");
    }
}
