use chrono::{SecondsFormat, Utc};
use rusqlite::{params, Connection, OptionalExtension, Row};
use serde_json::{json, Value};
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use uuid::Uuid;

const MIGRATION_001: &str = include_str!("../../migrations/001-agent-context-materializations.sql");
const MIGRATION_002: &str = include_str!("../../migrations/002-agent-events.sql");
const MIGRATION_003: &str = include_str!("../migrations/003-agent-context-compatibility.sql");
const NATIVE_CONTRACT: &str = include_str!("../tool-catalog.json");

pub struct Context {
    pub site_root: PathBuf,
    pub site_id: String,
    pub server_name: String,
    pub db_path: PathBuf,
}

impl Context {
    pub fn new(site_root: PathBuf, site_id: Option<String>) -> Result<Self, String> {
        let site_root = site_root
            .canonicalize()
            .map_err(|error| format!("site_root_not_found:{}:{error}", site_root.display()))?;
        let site_id = site_id
            .or_else(|| env::var("NARADA_SITE_ID").ok())
            .unwrap_or_else(|| {
                site_root
                    .file_name()
                    .and_then(|v| v.to_str())
                    .unwrap_or("site")
                    .to_string()
            });
        let server_name = format!("{}-agent-context-mcp", sanitize_site_id(&site_id));
        let db_path = env::var_os("NARADA_AGENT_CONTEXT_DB")
            .map(PathBuf::from)
            .unwrap_or_else(|| site_root.join(".ai/state/agent-context.sqlite"));
        Ok(Self {
            site_root,
            site_id,
            server_name,
            db_path,
        })
    }

    pub(crate) fn open_db(&self) -> Result<Connection, String> {
        if let Some(parent) = self.db_path.parent() {
            fs::create_dir_all(parent)
                .map_err(|error| format!("agent_context_db_directory_failed:{error}"))?;
        }
        let db = Connection::open(&self.db_path).map_err(db_error)?;
        db.busy_timeout(std::time::Duration::from_millis(5000))
            .map_err(db_error)?;
        db.execute_batch(MIGRATION_001).map_err(db_error)?;
        db.execute_batch(MIGRATION_002).map_err(db_error)?;
        db.execute_batch(MIGRATION_003).map_err(db_error)?;
        ensure_agent_start_event_columns(&db)?;
        db.execute_batch("CREATE TABLE IF NOT EXISTS codex_session_admissions (admission_id TEXT PRIMARY KEY, agent_id TEXT NOT NULL, runtime TEXT NOT NULL DEFAULT 'codex', cwd TEXT NOT NULL, status TEXT NOT NULL CHECK (status IN ('creating','admitted','suspect','retired')), agent_start_event_id TEXT, codex_session_id TEXT, codex_session_file TEXT, evidence_json TEXT NOT NULL DEFAULT '{}', created_at TEXT NOT NULL, verified_at TEXT); CREATE INDEX IF NOT EXISTS idx_codex_session_admissions_agent ON codex_session_admissions(agent_id,cwd,status,created_at DESC); CREATE INDEX IF NOT EXISTS idx_codex_session_admissions_session ON codex_session_admissions(codex_session_id);").map_err(db_error)?;
        ensure_checkpoint_tables(&db)?;
        Ok(db)
    }
}

fn ensure_agent_start_event_columns(db: &Connection) -> Result<(), String> {
    let mut statement = db
        .prepare("PRAGMA table_info(agent_start_events)")
        .map_err(db_error)?;
    let columns = statement
        .query_map([], |row| row.get::<_, String>(1))
        .map_err(db_error)?
        .collect::<Result<std::collections::HashSet<_>, _>>()
        .map_err(db_error)?;
    drop(statement);
    for (name, kind) in [
        ("identity_id", "TEXT"),
        ("runtime", "TEXT"),
        ("created_at", "TEXT"),
        ("status", "TEXT"),
        ("resume_command", "TEXT"),
        ("bootstrap_artifact_uri", "TEXT"),
        ("carrier_session_id", "TEXT"),
        ("admission_receipt_ref", "TEXT"),
        ("authority_epoch", "INTEGER"),
        ("orientation_manifest_id", "TEXT"),
    ] {
        if !columns.contains(name) {
            db.execute_batch(&format!(
                "ALTER TABLE agent_start_events ADD COLUMN {name} {kind}"
            ))
            .map_err(db_error)?;
        }
    }
    Ok(())
}

pub fn call_tool(
    context: &Context,
    projection: &str,
    params_value: &Value,
) -> Result<Value, String> {
    let name = params_value
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or("");
    let args = params_value
        .get("arguments")
        .cloned()
        .unwrap_or_else(|| json!({}));
    if projection != "admin" && !matches!(name, "agent_orientation_read" | "mcp_output_show") {
        return Err(format!(
            "agent_context_tool_not_exposed_in_{projection}_projection:{name}"
        ));
    }
    match name {
        "agent_context_doctor" => doctor(context),
        "agent_context_guidance" => guidance(&args),
        "agent_context_whoami" => crate::orientation::whoami(context, &args),
        "agent_context_hydrate_current" => hydrate_current(context, &args),
        "agent_orientation_read" => crate::orientation::read(context, projection, &args),
        "agent_orientation_acknowledge" => crate::orientation::acknowledge_tool(context, &args),
        "agent_context_startup_sequence" => crate::orientation::startup(context, &args),
        "mcp_output_show" => output_show(context, &args),
        "agent_context_checkpoint" => checkpoint(context, &args),
        "agent_context_rehydrate" => rehydrate(context, &args),
        "agent_context_continuation_export" => continuation_export(context, &args),
        "agent_context_continuation_read" => continuation_read(context, &args),
        "agent_context_list_sessions" => list_sessions(context, &args),
        "agent_context_start_session" => start_session(context, &args),
        _ => Err(format!("agent_context_native_tool_not_implemented:{name}")),
    }
}

fn start_session(context: &Context, args: &Value) -> Result<Value, String> {
    let identity = required_string(args, "identity")?;
    validate_identity(context, &identity)?;
    let runtime = args
        .get("runtime")
        .and_then(Value::as_str)
        .unwrap_or("codex");
    let cwd = args
        .get("cwd")
        .and_then(Value::as_str)
        .map(str::to_string)
        .unwrap_or_else(|| path_text(&context.site_root));
    let roster = roster_projection(context, &identity);
    if args.get("dry_run") != Some(&Value::Bool(true)) {
        let admission = exact_evidence(
            args,
            "admission_receipt",
            "NARADA_CARRIER_SESSION_ADMISSION_RECEIPT",
        )?
        .ok_or("agent_context_exact_admission_receipt_required")?;
        if admission
            .pointer("/agent_identity/local_agent_id")
            .and_then(Value::as_str)
            != Some(&identity)
        {
            return Err("agent_context_admission_identity_mismatch".into());
        }
        if let Ok(session) = env::var("NARADA_CARRIER_SESSION_ID") {
            if admission
                .pointer("/coordinate/carrier_session_id")
                .and_then(Value::as_str)
                != Some(session.as_str())
            {
                return Err("agent_context_admission_session_mismatch".into());
            }
        }
        let activation = exact_evidence(
            args,
            "activation_receipt",
            "NARADA_CARRIER_SESSION_ACTIVATION_RECEIPT",
        )?;
        let generated_at = match args.get("generated_at").and_then(Value::as_str) {
            Some(value) => chrono::DateTime::parse_from_rfc3339(value)
                .map_err(|_| "agent_context_invalid_generated_at")?
                .with_timezone(&Utc)
                .to_rfc3339_opts(SecondsFormat::Millis, true),
            None => timestamp(),
        };
        let compiled = crate::materialization::compile(
            context,
            &admission,
            activation.as_ref(),
            &roster["role_binding"],
            &generated_at,
            None,
            None,
        )?;
        return persist_session_materialization(
            context, &identity, runtime, &cwd, &roster, &admission, compiled,
        );
    }
    Ok(
        json!({"schema":"narada.agent_context.session_start.v1","status":"dry_run","authority_claimed":false,"identity":identity,"role":roster["role"],"role_binding":roster["role_binding"],"runtime_request":runtime,"root_dir":path_text(&context.site_root),"cwd_request":cwd,"db_path":path_text(&context.db_path),"would_validate":{"roster_or_identity_projection":true,"exact_admission_receipt":true,"orientation_manifest":true},"would_write":["orientation_manifest_generations","agent_start_events_downstream_trace"],"orientation_manifest":null,"required_for_materialization":["site_id","carrier_session_admission_receipt"]}),
    )
}

fn exact_evidence(args: &Value, field: &str, variable: &str) -> Result<Option<Value>, String> {
    let supplied = args.get(field).filter(|v| !v.is_null()).cloned();
    let inherited = env::var(variable)
        .ok()
        .filter(|v| !v.trim().is_empty())
        .map(|raw| {
            serde_json::from_str::<Value>(&raw)
                .map_err(|error| format!("orientation_environment_json_invalid:{variable}:{error}"))
        })
        .transpose()?;
    if supplied.is_some() && inherited.is_some() && supplied != inherited {
        return Err(format!("agent_context_conflicting_{}s", field));
    }
    Ok(supplied.or(inherited))
}

fn persist_session_materialization(
    context: &Context,
    identity: &str,
    runtime: &str,
    cwd: &str,
    roster: &Value,
    admission: &Value,
    compiled: crate::materialization::Materialization,
) -> Result<Value, String> {
    let manifest = compiled.manifest;
    let brief = compiled.brief;
    let now = manifest["generated_at"]
        .as_str()
        .ok_or("agent_context_native_manifest_generated_at_missing")?;
    let event_id = format!(
        "evt-{}_{}",
        now.replace([':', '.'], "-")
            .replace('T', "_")
            .chars()
            .take(19)
            .collect::<String>(),
        &Uuid::new_v4().to_string()[..8]
    );
    let event_status = if manifest["delivery"] == "deliverable" {
        "materialized"
    } else {
        "orientation_blocked"
    };
    let manifest_json = serde_json::to_string(&manifest).map_err(|e| e.to_string())?;
    let brief_json = brief
        .as_ref()
        .map(serde_json::to_string)
        .transpose()
        .map_err(|e| e.to_string())?;
    let mut db = context.open_db()?;
    let tx = db.transaction().map_err(db_error)?;
    let existing: Option<String> = tx
        .query_row(
            "SELECT manifest_json FROM orientation_manifest_generations WHERE manifest_id=?1",
            [manifest["manifest_id"].as_str()],
            |r| r.get(0),
        )
        .optional()
        .map_err(db_error)?;
    if existing.as_deref().is_some_and(|v| v != manifest_json) {
        return Err("agent_context_orientation_manifest_generation_conflict".into());
    }
    if existing.is_none() {
        tx.execute("INSERT INTO orientation_manifest_generations (manifest_id,admission_receipt_ref,carrier_session_id,authority_epoch,readiness,delivery,manifest_json,generated_at) VALUES (?1,?2,?3,?4,?5,?6,?7,?8)",params![manifest["manifest_id"].as_str(),admission["receipt_id"].as_str(),admission.pointer("/coordinate/carrier_session_id").and_then(Value::as_str),admission.pointer("/coordinate/authority_epoch").and_then(Value::as_i64),manifest["readiness"].as_str(),manifest["delivery"].as_str(),manifest_json,now]).map_err(db_error)?;
    }
    if let (Some(value), Some(text)) = (&brief, &brief_json) {
        let existing: Option<String> = tx
            .query_row(
                "SELECT brief_json FROM orientation_brief_generations WHERE brief_id=?1",
                [value["brief_id"].as_str()],
                |r| r.get(0),
            )
            .optional()
            .map_err(db_error)?;
        if existing.as_deref().is_some_and(|v| v != text) {
            return Err("agent_context_orientation_brief_generation_conflict".into());
        }
        if existing.is_none() {
            tx.execute("INSERT INTO orientation_brief_generations (brief_id,manifest_id,brief_digest,brief_json,generated_at) VALUES (?1,?2,?3,?4,?5)",params![value["brief_id"].as_str(),manifest["manifest_id"].as_str(),value["brief_digest"].as_str(),text,value["generated_at"].as_str()]).map_err(db_error)?;
        }
    }
    tx.execute("INSERT INTO agent_start_events (event_id,identity_id,runtime,created_at,status,resume_command,bootstrap_artifact_uri,carrier_session_id,admission_receipt_ref,authority_epoch,orientation_manifest_id) VALUES (?1,?2,?3,?4,?5,NULL,NULL,?6,?7,?8,?9)",params![event_id,identity,runtime,now,event_status,admission.pointer("/coordinate/carrier_session_id").and_then(Value::as_str),admission["receipt_id"].as_str(),admission.pointer("/coordinate/authority_epoch").and_then(Value::as_i64),manifest["manifest_id"].as_str()]).map_err(db_error)?;
    tx.commit().map_err(db_error)?;
    let persisted = if brief.is_some() {
        json!([
            "orientation_manifest_generations",
            "orientation_brief_generations",
            "agent_start_events"
        ])
    } else {
        json!(["orientation_manifest_generations", "agent_start_events"])
    };
    let manifest_ref=brief.as_ref().map(|v|v["manifest_ref"].clone()).unwrap_or_else(||json!({"source_authority_ref":"agent-context:orientation-manifest-store","artifact_ref":format!("agent-context:orientation_manifest_generations:{}",manifest["manifest_id"].as_str().unwrap_or("")),"revision":manifest["manifest_digest"],"manifest_id":manifest["manifest_id"],"manifest_digest":manifest["manifest_digest"]}));
    let entry_procedure = manifest["entries"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|v| v["compartment"] == "entry_procedure")
        .cloned()
        .collect::<Vec<_>>();
    Ok(
        json!({"schema":"narada.agent_context.session_start.v1","status":if manifest["delivery"]=="deliverable"{"materialized"}else{"blocked"},"compatibility_facade":{"authority":"none","event_posture":"downstream_trace","source_authority_mutation":false,"local_persistence":true,"persisted_records":persisted},"agent_start_event":event_id,"identity":identity,"role":roster["role"],"role_binding":roster["role_binding"],"runtime_request":runtime,"cwd_request":cwd,"db_path":path_text(&context.db_path),"carrier_session":admission["coordinate"],"admission_receipt":admission,"admission_receipt_ref":admission["receipt_id"],"orientation_manifest":manifest,"orientation_brief":brief,"orientation_manifest_ref":manifest_ref,"entry_procedure":entry_procedure}),
    )
}
fn roster_projection(context: &Context, identity: &str) -> Value {
    let path = context.site_root.join(".ai/agents/roster.json");
    if let Ok(bytes) = fs::read(path) {
        if let Ok(roster) = serde_json::from_slice::<Value>(&bytes) {
            if let Some(agent) = roster
                .get("agents")
                .and_then(Value::as_array)
                .and_then(|items| {
                    items
                        .iter()
                        .find(|item| item.get("agent_id").and_then(Value::as_str) == Some(identity))
                })
            {
                let role = agent.get("role").cloned().unwrap_or(Value::Null);
                return json!({"role":role,"role_binding":role_binding(identity,&role,"static_roster_config","agent_roster")});
            }
            if roster.get("enforce_session_roster") == Some(&Value::Bool(true)) {
                return json!({"role":null,"role_binding":role_binding(identity,&Value::Null,"unavailable","unavailable")});
            }
        }
    }
    let suffix = identity
        .rsplit('.')
        .next()
        .filter(|v| matches!(*v, "architect" | "builder" | "builder2" | "resident"));
    let role = suffix.map(Value::from).unwrap_or(Value::Null);
    json!({"role":role,"role_binding":role_binding(identity,&role,"identity_inference_non_authoritative","identity_inference_non_authoritative")})
}
fn role_binding(agent: &str, role: &Value, source: &str, authority: &str) -> Value {
    let semantics=match authority{"agent_roster"=>"Roster role binding is used for identity read models, routing, and eligibility; it is not activation authority or a capability grant.","identity_inference_non_authoritative"=>"Role was inferred from identity shape because the Site has not opted into session roster enforcement; this is a read-model hint, not activation authority or a capability grant.",_=>"No authoritative role binding was available. This residual projection cannot create identity, block an owner-issued admission, or grant capability."};
    json!({"schema":"narada.agent.role_binding.v0","agent_id":agent,"role_name":role,"binding_source":source,"binding_authority":authority,"semantics":semantics,"capability_policy_ref":"capability_policy"})
}

fn list_sessions(context: &Context, args: &Value) -> Result<Value, String> {
    let db = context.open_db()?;
    let identity = args.get("identity").and_then(Value::as_str);
    let substrate = args.get("substrate").and_then(Value::as_str);
    let date_from = parse_date_filter(args.get("date_from"), "date_from")?;
    let date_to = parse_date_filter(args.get("date_to"), "date_to")?;
    let limit = args
        .get("limit")
        .and_then(Value::as_i64)
        .unwrap_or(100)
        .clamp(1, 500) as usize;
    let mut stmt=db.prepare("SELECT event_id,identity_id,runtime,created_at,status,resume_command,bootstrap_artifact_uri FROM agent_start_events ORDER BY created_at DESC,event_id DESC").map_err(db_error)?;
    let rows = stmt
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, Option<String>>(5)?,
                row.get::<_, Option<String>>(6)?,
            ))
        })
        .map_err(db_error)?;
    let now = Utc::now();
    let generated = now.to_rfc3339_opts(SecondsFormat::Millis, true);
    let mut sessions = Vec::new();
    for row in rows {
        let (event_id, agent, runtime, created, status, resume, bootstrap) =
            row.map_err(db_error)?;
        if identity.is_some_and(|v| v != agent)
            || substrate.is_some_and(|v| v != runtime)
            || date_from
                .as_ref()
                .is_some_and(|v| created.as_str() < v.as_str())
            || date_to
                .as_ref()
                .is_some_and(|v| created.as_str() > v.as_str())
        {
            continue;
        }
        let seconds = chrono::DateTime::parse_from_rfc3339(&created)
            .ok()
            .map(|start| (now.timestamp() - start.timestamp()).max(0));
        sessions.push(json!({"event_id":event_id,"identity":agent,"substrate":runtime,"runtime":runtime,"status":status,"created_at":created,"resume_command":resume,"bootstrap_artifact_uri":bootstrap,"duration_estimate":{"seconds":seconds,"basis":"elapsed_since_start_no_end_event","as_of":generated}}));
        if sessions.len() == limit {
            break;
        }
    }
    let mut latest = serde_json::Map::new();
    for session in &sessions {
        if let Some(agent) = session.get("identity").and_then(Value::as_str) {
            if !latest.contains_key(agent) {
                latest.insert(agent.into(), session.clone());
            }
        }
    }
    Ok(
        json!({"status":"ok","schema":"narada.agent_context.sessions.v0","authority":"agent_context_sqlite","generated_at":generated,"filters":{"identity":args.get("identity").cloned().unwrap_or(Value::Null),"date_from":args.get("date_from").cloned().unwrap_or(Value::Null),"date_to":args.get("date_to").cloned().unwrap_or(Value::Null),"substrate":args.get("substrate").cloned().unwrap_or(Value::Null),"limit":limit},"session_count":sessions.len(),"sessions":sessions,"latest_session_per_identity":latest,"duration_estimate_note":"agent_start_events has no end timestamp; duration is elapsed time from created_at to generated_at."}),
    )
}
fn parse_date_filter(value: Option<&Value>, field: &str) -> Result<Option<String>, String> {
    let Some(value) = value.filter(|v| !v.is_null()) else {
        return Ok(None);
    };
    let text = value.as_str().unwrap_or(&value.to_string()).to_string();
    chrono::DateTime::parse_from_rfc3339(&text)
        .map(|v| Some(v.to_utc().to_rfc3339_opts(SecondsFormat::Millis, true)))
        .map_err(|_| format!("invalid_{field}: {text}"))
}

pub fn protocol_request(
    context: &Context,
    projection: &str,
    method: &str,
    params: &Value,
) -> Result<Value, String> {
    match method {
        "resources/list" => {
            if projection == "occupant" {
                return Ok(json!({"resources":[]}));
            }
            let directory = context.site_root.join(".ai/tmp/mcp-outputs/workspace");
            let mut resources = if directory.exists() {
                fs::read_dir(directory).map_err(|e|format!("output_resource_list_failed:{e}"))?.filter_map(Result::ok).filter_map(|entry|{let name=entry.file_name().to_string_lossy().to_string();let id=name.strip_suffix(".json")?;let reference=format!("mcp_output:{id}");Some(json!({"uri":format!("mcp-output:{}",percent_encode(&reference)),"name":reference,"title":reference,"description":"Materialized MCP output ref.","mimeType":"application/json"}))}).collect::<Vec<_>>()
            } else {
                vec![]
            };
            resources.sort_by_key(|v| v["name"].as_str().unwrap_or("").to_string());
            Ok(
                json!({"resources":resources,"offset":0,"limit":100,"next_offset":null,"nextCursor":null,"has_more":false}),
            )
        }
        "resources/read" => {
            if projection == "occupant" {
                return Err("agent_context_resources_not_exposed_in_occupant_projection".into());
            }
            let uri = params.get("uri").and_then(Value::as_str).unwrap_or("");
            let encoded = uri
                .strip_prefix("mcp-output:")
                .ok_or_else(|| format!("output_resource_uri_invalid: {uri}"))?;
            let reference = percent_decode(encoded)?;
            let page = output_show(context, &json!({"ref":reference,"offset":0,"limit":10000}))?;
            Ok(
                json!({"contents":[{"uri":uri,"mimeType":"application/json","text":serde_json::to_string_pretty(&page).unwrap()}]}),
            )
        }
        "prompts/list" => Ok(
            json!({"prompts":if projection=="occupant"{vec![]}else{vec![json!({"name":"agent_context_startup","title":"Agent Context Startup","description":"Guidance for exact admitted Orientation Manifest delivery and bounded continuity.","arguments":[]})]}}),
        ),
        "prompts/get" => {
            if projection == "occupant" {
                return Err("agent_context_prompts_not_exposed_in_occupant_projection".into());
            }
            let name = params.get("name").and_then(Value::as_str).unwrap_or("");
            if name != "agent_context_startup" {
                return Err(format!("unknown_prompt: {name}"));
            }
            Ok(
                json!({"description":"Guidance for exact admitted Orientation Manifest delivery and bounded continuity.","messages":[{"role":"user","content":{"type":"text","text":"This is the enforced Carrier-entry orientation turn. Call agent_orientation_read({}) and then execute each returned next_call exactly. A continuation is opaque: never inspect or alter it. Stop only when status=ready and ordinary_work_gate=open. Agent Context retains required-read and acknowledgement evidence. The inline brief names exact continuity and work entry snapshots or explicit omissions and carries one canonical manifest_ref. Acknowledgement proves delivery and completed reads, not comprehension or authority for a later action."}}]}),
            )
        }
        "completion/complete" => {
            let argument = params
                .pointer("/argument/name")
                .and_then(Value::as_str)
                .unwrap_or("");
            let values = if argument == "name" {
                let contract: Value =
                    serde_json::from_str(NATIVE_CONTRACT).map_err(|e| e.to_string())?;
                contract
                    .pointer(&format!("/projections/{projection}"))
                    .and_then(Value::as_array)
                    .unwrap_or(&vec![])
                    .iter()
                    .filter_map(|v| v.get("name").and_then(Value::as_str))
                    .take(100)
                    .map(str::to_string)
                    .collect::<Vec<_>>()
            } else {
                vec![]
            };
            Ok(json!({"completion":{"values":values,"total":values.len(),"hasMore":false}}))
        }
        "logging/setLevel" => Ok(json!({})),
        _ => Err(format!("unsupported_method: {method}")),
    }
}

fn percent_encode(value: &str) -> String {
    value
        .bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) {
                (b as char).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect()
}
fn percent_decode(value: &str) -> Result<String, String> {
    let bytes = value.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            if i + 2 >= bytes.len() {
                return Err("output_resource_uri_invalid_encoding".into());
            }
            let hex = std::str::from_utf8(&bytes[i + 1..i + 3])
                .map_err(|_| "output_resource_uri_invalid_encoding")?;
            out.push(
                u8::from_str_radix(hex, 16).map_err(|_| "output_resource_uri_invalid_encoding")?,
            );
            i += 3
        } else {
            out.push(bytes[i]);
            i += 1
        }
    }
    String::from_utf8(out).map_err(|_| "output_resource_uri_invalid_encoding".into())
}

pub fn bounded_tool_result(context: &Context, tool: &str, value: Value) -> Result<Value, String> {
    let text = serde_json::to_string_pretty(&value).map_err(|e| e.to_string())?;
    let structured = if text.chars().count() <= 6000 {
        value
    } else {
        materialize_output(context, tool, value, &text)?
    };
    let content = serde_json::to_string_pretty(&structured).map_err(|e| e.to_string())?;
    Ok(
        json!({"resultType":"complete","content":[{"type":"text","text":content,"annotations":{"audience":["assistant"]}}],"structuredContent":structured}),
    )
}

fn materialize_output(
    context: &Context,
    tool: &str,
    value: Value,
    full_text: &str,
) -> Result<Value, String> {
    use sha2::{Digest, Sha256};
    let output_id = format!(
        "o_{}",
        Uuid::new_v4()
            .simple()
            .to_string()
            .chars()
            .take(24)
            .collect::<String>()
    );
    let reference = format!("mcp_output:{output_id}");
    let created_at = timestamp();
    let record = json!({"schema":"narada.mcp_output_ref.v1","ref":reference,"output_id":output_id,"tool_name":tool,"created_at":created_at,"created_by":env::var("NARADA_AGENT_ID").ok(),"content_type":"application/json","inline_char_limit":6000,"full_output_char_length":full_text.chars().count(),"truncated":true,"sha256":format!("{:x}",Sha256::digest(stable_json(&value).as_bytes())),"max_bytes":10*1024*1024,"full_output":value});
    let serialized = format!(
        "{}\n",
        serde_json::to_string(&record).map_err(|e| e.to_string())?
    );
    if serialized.len() > 10 * 1024 * 1024 {
        return Err(format!(
            "mcp_output_too_large: {} > {}",
            serialized.len(),
            10 * 1024 * 1024
        ));
    }
    let directory = context.site_root.join(".ai/tmp/mcp-outputs/workspace");
    fs::create_dir_all(&directory).map_err(|e| format!("mcp_output_write_failed:{e}"))?;
    fs::write(directory.join(format!("{output_id}.json")), serialized)
        .map_err(|e| format!("mcp_output_write_failed:{e}"))?;
    let status = record["full_output"]
        .get("status")
        .and_then(Value::as_str)
        .filter(|v| v.len() <= 32)
        .unwrap_or("ok");
    let mut preview = take_chars(full_text, 6000);
    loop {
        let next = if preview.chars().count() < full_text.chars().count() {
            Some(preview.chars().count())
        } else {
            None
        };
        let envelope = json!({"schema":"narada.producer_output_page.v1","status":status,"truncated":true,"output_ref":reference,"ref":reference,"result_materialized":true,"tool_name":tool,"offset":0,"limit":6000,"next_offset":next,"transport_offset":0,"transport_limit":6000,"transport_next_offset":next,"output_text":preview,"output_truncated":next.is_some(),"reader_tool":"mcp_output_show","site_root":path_text(&context.site_root),"read_command":format!("mcp_output_show({{ \"ref\": \"{reference}\", \"offset\": 0, \"limit\": 10000 }})"),"remediation":format!("Use mcp_output_show with output_ref/ref={reference} to read the bounded produced JSON pages; continue with the returned next_offset."),"inline_limit":6000,"full_output_char_length":full_text.chars().count()});
        let compact = serde_json::to_string(&envelope).map_err(|e| e.to_string())?;
        if compact.chars().count() <= 6000
            && compact.len() + serde_json::to_vec(&envelope).unwrap().len() <= 32768
        {
            return Ok(envelope);
        }
        let next_len = ((preview.chars().count() as f64) * 0.75).floor() as usize;
        if next_len == 0 {
            return Err("inline_output_envelope_limit_too_small".into());
        }
        preview = take_chars(full_text, next_len)
    }
}

fn output_show(context: &Context, args: &Value) -> Result<Value, String> {
    let reference = args
        .get("ref")
        .or_else(|| args.get("output_ref"))
        .and_then(Value::as_str)
        .filter(|v| !v.trim().is_empty())
        .ok_or("output_show_requires_ref")?;
    let output_id = reference
        .strip_prefix("mcp_output:")
        .filter(|v| {
            v.len() >= 3
                && v.chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
        })
        .ok_or_else(|| format!("output_ref_invalid: {reference}"))?;
    let path = context
        .site_root
        .join(format!(".ai/tmp/mcp-outputs/workspace/{output_id}.json"));
    let bytes = fs::read(&path).map_err(|_| format!("output_ref_not_found: {reference}"))?;
    let record: Value =
        serde_json::from_slice(&bytes).map_err(|e| format!("output_ref_invalid_json: {e}"))?;
    if record.get("schema").and_then(Value::as_str) != Some("narada.mcp_output_ref.v1")
        || record.get("ref").and_then(Value::as_str) != Some(reference)
    {
        return Err(format!("output_ref_metadata_mismatch: {reference}"));
    }
    let full = serde_json::to_string_pretty(&record["full_output"]).map_err(|e| e.to_string())?;
    let offset = args.get("offset").and_then(Value::as_u64).unwrap_or(0) as usize;
    let limit = args
        .get("limit")
        .or_else(|| args.get("output_limit"))
        .and_then(Value::as_u64)
        .unwrap_or(10000) as usize;
    if limit == 0 {
        return Err("output_limit_must_be_positive_integer".into());
    }
    if limit > 20000 {
        return Err(format!(
            "output_limit_exceeds_transport_maximum: {limit} > 20000"
        ));
    }
    let total = full.chars().count();
    let chunk = take_chars_from(&full, offset, limit);
    let end = (offset + chunk.chars().count()).min(total);
    Ok(
        json!({"schema":"narada.mcp_output_page.v1","status":"ok","ref":reference,"tool_name":record["tool_name"],"full_output_char_length":total,"byte_size":bytes.len(),"original_truncated":true,"path":format!(".ai/tmp/mcp-outputs/workspace/{output_id}.json"),"offset":offset,"limit":limit,"next_offset":if end<total{Some(end)}else{None},"output_limit":limit,"output_truncated":end<total,"output_text":chunk}),
    )
}

fn take_chars(value: &str, count: usize) -> String {
    value.chars().take(count).collect()
}
fn take_chars_from(value: &str, offset: usize, count: usize) -> String {
    value.chars().skip(offset).take(count).collect()
}
fn stable_json(value: &Value) -> String {
    match value {
        Value::Array(v) => {
            serde_json::to_string(&v.iter().map(sort_json).collect::<Vec<_>>()).unwrap()
        }
        Value::Object(_) => serde_json::to_string(&sort_json(value)).unwrap(),
        _ => serde_json::to_string(value).unwrap(),
    }
}
fn sort_json(value: &Value) -> Value {
    match value {
        Value::Array(v) => Value::Array(v.iter().map(sort_json).collect()),
        Value::Object(v) => {
            let mut keys = v.keys().collect::<Vec<_>>();
            keys.sort();
            let mut out = serde_json::Map::new();
            for key in keys {
                out.insert(key.clone(), sort_json(&v[key]));
            }
            Value::Object(out)
        }
        _ => value.clone(),
    }
}

fn guidance(args: &Value) -> Result<Value, String> {
    let contract: Value = serde_json::from_str(NATIVE_CONTRACT)
        .map_err(|e| format!("agent_context_native_contract_invalid:{e}"))?;
    let mut result = contract
        .get("guidance")
        .cloned()
        .ok_or("agent_context_native_guidance_missing")?;
    let requested = json!({
        "workflow": args.get("workflow").and_then(Value::as_str).filter(|v|!v.trim().is_empty()).map(str::trim),
        "tool": args.get("tool").and_then(Value::as_str).filter(|v|!v.trim().is_empty()).map(str::trim)
    });
    result
        .as_object_mut()
        .ok_or("agent_context_native_guidance_invalid")?
        .insert("requested".into(), requested);
    Ok(result)
}

fn hydrate_current(context: &Context, args: &Value) -> Result<Value, String> {
    if args.get("checkpoint_startup") == Some(&Value::Bool(true)) {
        return Ok(
            json!({"schema":"narada.agent_context.orientation_hydration.v1","status":"blocked","reason":"orientation_assembly_read_only","required_next_step":"Use agent_context_checkpoint as a separate explicit mutation."}),
        );
    }
    let admission = match exact_evidence(
        args,
        "admission_receipt",
        "NARADA_CARRIER_SESSION_ADMISSION_RECEIPT",
    )? {
        Some(value) => value,
        None => {
            return Ok(
                json!({"schema":"narada.agent_context.orientation_hydration.v1","status":"blocked","reason":"agent_context_exact_admission_receipt_required","rejected_fallbacks":["latest_checkpoint","latest_start_event","identity_name_inference"]}),
            )
        }
    };
    let identity = admission
        .pointer("/agent_identity/local_agent_id")
        .and_then(Value::as_str)
        .ok_or("agent_context_admission_identity_mismatch")?;
    if let Ok(expected) = env::var("NARADA_AGENT_ID") {
        if expected != identity {
            return Err("agent_context_admission_identity_mismatch".into());
        }
    }
    let generated_at = match args.get("generated_at").and_then(Value::as_str) {
        Some(value) => chrono::DateTime::parse_from_rfc3339(value)
            .map_err(|_| "agent_context_invalid_generated_at")?
            .with_timezone(&Utc)
            .to_rfc3339_opts(SecondsFormat::Millis, true),
        None => timestamp(),
    };
    let roster = roster_projection(context, identity);
    let activation = exact_evidence(
        args,
        "activation_receipt",
        "NARADA_CARRIER_SESSION_ACTIVATION_RECEIPT",
    )?;
    let checkpoint_id = optional_string(args, "checkpoint_id")?;
    let (checkpoint, portable) = if let Some(id) = checkpoint_id.as_deref() {
        let selection = json!({"agent_id":identity,"checkpoint_id":id});
        (
            Some(rehydrate(context, &selection)?),
            Some(continuation_read(context, &selection)?),
        )
    } else {
        (None, None)
    };
    let compiled = crate::materialization::compile(
        context,
        &admission,
        activation.as_ref(),
        &roster["role_binding"],
        &generated_at,
        checkpoint.as_ref(),
        portable.as_ref(),
    )?;
    let whoami = json!({"schema":"narada.agent_context.identity_resolution.v1","status":"ok","identity":identity,"canonical_agent_id":admission.pointer("/agent_identity/canonical_agent_id"),"confidence":"exact","source":"carrier_session_admission_receipt","admission_receipt_ref":admission["receipt_id"],"carrier_session":admission["coordinate"],"authority_readback_ref":admission["authority_readback_ref"],"hint_match":true});
    let omitted =
        json!({"status":"omitted","reason":"exact_checkpoint_not_selected","checkpoint_id":null});
    let checkpoint_result = checkpoint.unwrap_or_else(|| omitted.clone());
    let portable_result = portable.unwrap_or_else(|| omitted.clone());
    let advisory = checkpoint_result
        .get("next_intended_action")
        .cloned()
        .unwrap_or(Value::Null);
    Ok(
        json!({"schema":"narada.agent_context.orientation_hydration.v1","status":if compiled.manifest["delivery"]=="deliverable"{"ok"}else{"blocked"},"source_mutation":false,"site_id":context.site_id,"site_root":path_text(&context.site_root),"hydrated_at":compiled.manifest["generated_at"],"whoami":whoami,"admission_receipt_ref":admission["receipt_id"],"orientation_manifest":compiled.manifest,"continuity_selection":if let Some(id)=checkpoint_id{json!({"mode":"exact","checkpoint_id":id})}else{json!({"mode":"omitted","checkpoint_id":null})},"checkpoint":checkpoint_result,"portable_continuation":portable_result,"continuity_advisory_next_action":advisory}),
    )
}

fn doctor(context: &Context) -> Result<Value, String> {
    let db = context.open_db()?;
    let tables = [
        "agent_start_events",
        "agent_events",
        "agent_checkpoints",
        "agent_checkpoint_history",
        "orientation_manifest_generations",
    ]
    .iter()
    .map(|name| {
        let exists = db
            .query_row(
                "SELECT 1 FROM sqlite_master WHERE type='table' AND name=?1",
                [name],
                |_| Ok(true),
            )
            .optional()
            .unwrap_or(None)
            .unwrap_or(false);
        json!({"table":name,"exists":exists})
    })
    .collect::<Vec<_>>();
    let ok = tables
        .iter()
        .all(|v| v.get("exists") == Some(&Value::Bool(true)));
    Ok(
        json!({"status":if ok {"ok"} else {"degraded"},"site_id":context.site_id,"server_name":context.server_name,"site_root":path_text(&context.site_root),"db_path":path_text(&context.db_path),"tables":tables}),
    )
}

fn checkpoint(context: &Context, args: &Value) -> Result<Value, String> {
    let agent_id = args
        .get("agent_id")
        .and_then(Value::as_str)
        .map(str::to_string)
        .or_else(|| env::var("NARADA_AGENT_ID").ok())
        .ok_or("agent_id_required")?;
    validate_identity(context, &agent_id)?;
    let mut db = context.open_db()?;
    let now = timestamp();
    let checkpoint_id = id("chk");
    let existing = db
        .query_row(
            "SELECT * FROM agent_checkpoints WHERE agent_id=?1 ORDER BY checkpoint_at DESC LIMIT 1",
            [&agent_id],
            row_to_checkpoint,
        )
        .optional()
        .map_err(db_error)?;
    let continuation = normalize_continuation(args.get("continuation"), &checkpoint_id, &now)?;
    let continuation_ref = args
        .get("continuation_ref")
        .cloned()
        .filter(|v| !v.is_null());
    let projection = continuation
        .as_ref()
        .map(|_| continuation_projection(&agent_id, continuation_ref.as_ref(), existing.as_ref()));
    let payload = json!({
        "schema":"narada.agent_context.checkpoint.v1","site_id":context.site_id,"site_root":path_text(&context.site_root),"agent_id":agent_id,"checkpoint_at":now,
        "active_task":field_or_null(args,"active_task"),"files_touched":array(args,"files_touched"),"key_decisions":array(args,"key_decisions"),"open_questions":array(args,"open_questions"),
        "git_head":field_or_null(args,"git_head"),"last_workboard_check_at":field_or_null(args,"last_workboard_check_at"),"next_intended_action":field_or_null(args,"next_intended_action"),
        "authority_basis":field_or_null(args,"authority_basis"),"continuation_blockers":array(args,"continuation_blockers"),"evidence_refs":array(args,"evidence_refs"),
        "worktree_state":field_or_null(args,"worktree_state"),"tactical_resume_notes":array(args,"tactical_resume_notes"),"continuation":continuation,"continuation_ref":continuation_ref,"continuation_projection":projection
    });
    let transaction = db.transaction().map_err(db_error)?;
    if let Some(previous) = &existing {
        transaction.execute("INSERT INTO agent_checkpoint_history (history_id,checkpoint_id,agent_id,session_id,checkpoint_at,active_task_json,files_touched_json,key_decisions_json,open_questions_json,git_head,payload_json,archived_at) SELECT ?1,checkpoint_id,agent_id,session_id,checkpoint_at,active_task_json,files_touched_json,key_decisions_json,open_questions_json,git_head,payload_json,?2 FROM agent_checkpoints WHERE checkpoint_id=?3", params![id("hist"),now,previous["checkpoint_id"].as_str()]).map_err(db_error)?;
        transaction
            .execute(
                "DELETE FROM agent_checkpoints WHERE checkpoint_id=?1",
                [previous["checkpoint_id"].as_str()],
            )
            .map_err(db_error)?;
    }
    let session_id = args
        .get("session_id")
        .and_then(Value::as_str)
        .map(str::to_string)
        .or_else(|| env::var("NARADA_AGENT_START_EVENT_ID").ok());
    transaction.execute("INSERT INTO agent_checkpoints (checkpoint_id,agent_id,session_id,checkpoint_at,active_task_json,files_touched_json,key_decisions_json,open_questions_json,git_head,payload_json) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)", params![checkpoint_id,agent_id,session_id,now,json_db(args.get("active_task")),json_text(array(args,"files_touched")),json_text(array(args,"key_decisions")),json_text(array(args,"open_questions")),args.get("git_head").and_then(Value::as_str),json_text(payload.clone())]).map_err(db_error)?;
    transaction.commit().map_err(db_error)?;
    Ok(
        json!({"status":"checkpointed","checkpoint_id":checkpoint_id,"archived_prior":existing.as_ref().and_then(|v|v["checkpoint_id"].as_str()),"agent_id":agent_id,"checkpoint_at":now,"db_path":path_text(&context.db_path),"site_root":path_text(&context.site_root),"continuation":payload["continuation"],"continuation_ref":payload["continuation_ref"],"continuation_projection":payload["continuation_projection"]}),
    )
}

fn rehydrate(context: &Context, args: &Value) -> Result<Value, String> {
    let agent_id = required_string(args, "agent_id")?;
    validate_identity(context, &agent_id)?;
    let db = context.open_db()?;
    let checkpoint_id = optional_string(args, "checkpoint_id")?;
    let limit = args
        .get("limit")
        .and_then(Value::as_i64)
        .unwrap_or(1)
        .clamp(1, 50);
    if let Some(ref requested) = checkpoint_id {
        let found = checkpoint_by_id(&db, &agent_id, requested)?;
        return Ok(found.map(|v| merge(json!({"status":"ok"}),v)).unwrap_or_else(||json!({"status":"checkpoint_not_found","agent_id":agent_id,"checkpoint_id":requested,"message":"No site-local current or archived checkpoint found for the requested checkpoint_id."})));
    }
    if args.get("history") == Some(&Value::Bool(true)) || limit > 1 {
        let mut stmt=db.prepare("SELECT checkpoint_id,agent_id,session_id,checkpoint_at,active_task_json,files_touched_json,key_decisions_json,open_questions_json,git_head,payload_json FROM agent_checkpoint_history WHERE agent_id=?1 ORDER BY archived_at DESC LIMIT ?2").map_err(db_error)?;
        let rows = stmt
            .query_map(params![agent_id, limit], row_to_checkpoint)
            .map_err(db_error)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(db_error)?;
        return Ok(
            json!({"status":if rows.is_empty(){"no_checkpoint_history"}else{"ok"},"agent_id":agent_id,"count":rows.len(),"checkpoints":rows}),
        );
    }
    let row = db
        .query_row(
            "SELECT * FROM agent_checkpoints WHERE agent_id=?1 ORDER BY checkpoint_at DESC LIMIT 1",
            [&agent_id],
            row_to_checkpoint,
        )
        .optional()
        .map_err(db_error)?;
    Ok(row.map(|v|merge(json!({"status":"ok"}),v)).unwrap_or_else(||json!({"status":"no_checkpoint","agent_id":agent_id,"message":"No site-local checkpoint found."})))
}

fn continuation_export(context: &Context, args: &Value) -> Result<Value, String> {
    let agent_id = args
        .get("agent_id")
        .and_then(Value::as_str)
        .map(str::to_string)
        .or_else(|| env::var("NARADA_AGENT_ID").ok())
        .ok_or("agent_id_required")?;
    validate_identity(context, &agent_id)?;
    let db = context.open_db()?;
    let checkpoint = db
        .query_row(
            "SELECT * FROM agent_checkpoints WHERE agent_id=?1 ORDER BY checkpoint_at DESC LIMIT 1",
            [&agent_id],
            row_to_checkpoint,
        )
        .optional()
        .map_err(db_error)?;
    let Some(checkpoint) = checkpoint else {
        return Ok(
            json!({"status":"no_checkpoint","agent_id":agent_id,"message":"No site-local checkpoint found."}),
        );
    };
    let continuation = checkpoint.get("continuation").filter(|v| !v.is_null());
    let Some(continuation) = continuation else {
        return Ok(
            json!({"status":"no_continuation","agent_id":agent_id,"checkpoint_id":checkpoint["checkpoint_id"],"message":"The latest checkpoint has no canonical continuation state."}),
        );
    };
    let relative = continuation_export_path(
        context,
        args.get("path"),
        &agent_id,
        checkpoint["checkpoint_id"].as_str().unwrap_or(""),
    )?;
    let artifact_path = context.site_root.join(relative.replace('/', "\\"));
    let markdown = render_continuation(&agent_id, &checkpoint, continuation);
    if let Some(parent) = artifact_path.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("continuation_export_write_failed:{e}"))?;
    }
    let overwrite = args.get("overwrite") == Some(&Value::Bool(true));
    let wrote = if artifact_path.exists() {
        let prior =
            fs::read(&artifact_path).map_err(|e| format!("continuation_export_read_failed:{e}"))?;
        if prior == markdown.as_bytes() {
            false
        } else if !overwrite {
            return Err("continuation_export_target_exists".into());
        } else {
            fs::write(&artifact_path, markdown.as_bytes())
                .map_err(|e| format!("continuation_export_write_failed:{e}"))?;
            true
        }
    } else {
        fs::write(&artifact_path, markdown.as_bytes())
            .map_err(|e| format!("continuation_export_write_failed:{e}"))?;
        true
    };
    use sha2::{Digest, Sha256};
    let reference = json!({"schema":"narada.continuation.handoff.v1","path":relative,"sha256":format!("{:x}",Sha256::digest(markdown.as_bytes())),"created_at":timestamp()});
    let projection = continuation_projection(&agent_id, Some(&reference), None);
    let mut payload = checkpoint["payload"].clone();
    payload
        .as_object_mut()
        .ok_or("checkpoint_payload_invalid")?
        .insert("continuation_ref".into(), reference.clone());
    payload
        .as_object_mut()
        .unwrap()
        .insert("continuation_projection".into(), projection.clone());
    db.execute(
        "UPDATE agent_checkpoints SET payload_json=?1 WHERE checkpoint_id=?2",
        params![json_text(payload), checkpoint["checkpoint_id"].as_str()],
    )
    .map_err(db_error)?;
    Ok(
        json!({"status":"exported","site_id":context.site_id,"site_root":path_text(&context.site_root),"agent_id":agent_id,"checkpoint_id":checkpoint["checkpoint_id"],"checkpoint_at":checkpoint["checkpoint_at"],"continuation":continuation,"continuation_ref":reference,"continuation_projection":projection,"artifact":{"path":relative,"bytes":markdown.len(),"wrote":wrote}}),
    )
}

fn continuation_read(context: &Context, args: &Value) -> Result<Value, String> {
    let agent_id = args
        .get("agent_id")
        .and_then(Value::as_str)
        .map(str::to_string)
        .or_else(|| env::var("NARADA_AGENT_ID").ok())
        .ok_or("agent_id_required")?;
    validate_identity(context, &agent_id)?;
    let checkpoint_id = optional_string(args, "checkpoint_id")?;
    let db = context.open_db()?;
    let checkpoint = match checkpoint_id.as_ref() { Some(id) => checkpoint_by_id(&db,&agent_id,id)?, None => db.query_row("SELECT * FROM agent_checkpoints WHERE agent_id=?1 ORDER BY checkpoint_at DESC LIMIT 1",[&agent_id],row_to_checkpoint).optional().map_err(db_error)? };
    let Some(checkpoint) = checkpoint else {
        return Ok(match checkpoint_id {
            Some(id) => {
                json!({"status":"checkpoint_not_found","agent_id":agent_id,"checkpoint_id":id,"message":"No site-local current or archived checkpoint found for the requested checkpoint_id."})
            }
            None => {
                json!({"status":"no_checkpoint","agent_id":agent_id,"message":"No site-local checkpoint found."})
            }
        });
    };
    let mut base = json!({"site_id":context.site_id,"site_root":path_text(&context.site_root),"agent_id":agent_id,"checkpoint_id":checkpoint["checkpoint_id"],"checkpoint_at":checkpoint["checkpoint_at"],"continuation":checkpoint["continuation"],"continuation_ref":checkpoint["continuation_ref"],"continuation_projection":checkpoint["continuation_projection"]});
    let reference = checkpoint.get("continuation_ref").filter(|v| !v.is_null());
    if reference.is_none() {
        let has_continuation = checkpoint.get("continuation").is_some_and(|v| !v.is_null());
        let selected = checkpoint_id.as_deref();
        let message = match (has_continuation, selected) {
            (true, Some(id)) => format!("Canonical continuation exists in the checkpoint {id} but has no portable Markdown reference."),
            (true, None) => "Canonical continuation exists in the latest checkpoint but has no portable Markdown reference.".into(),
            (false, Some(id)) => format!("The checkpoint {id} has no canonical continuation state."),
            (false, None) => "The latest checkpoint has no canonical continuation state.".into(),
        };
        base.as_object_mut().unwrap().extend(json!({"status":if has_continuation{"unlinked"}else{"no_continuation"},"message":message,"next_action":checkpoint.pointer("/continuation_projection/next_action").cloned().unwrap_or(Value::Null)}).as_object().unwrap().clone());
        return Ok(base);
    }
    let reference = reference.unwrap();
    let path = reference
        .get("path")
        .and_then(Value::as_str)
        .ok_or("continuation_ref_path_must_be_site_relative")?;
    let artifact_path = context.site_root.join(path.replace('/', "\\"));
    let result = fs::read_to_string(&artifact_path);
    match result {
        Ok(markdown)=>{
            let expected=checkpoint.pointer("/continuation/content_hash").and_then(Value::as_str);
            if expected.is_some_and(|hash|!markdown.contains("<!-- narada.continuation.handoff.v1 -->")||!markdown.contains(&format!("<!-- narada.continuation.content-hash: {hash} -->"))){base.as_object_mut().unwrap().extend(json!({"continuation_ref":reference,"status":"stale","reason":"continuation_artifact_content_hash_mismatch","artifact":{"path":path,"verified":false}}).as_object().unwrap().clone())}else{base.as_object_mut().unwrap().extend(json!({"continuation_ref":reference,"status":"ok","artifact":{"path":path,"sha256":reference["sha256"],"created_at":reference["created_at"],"bytes":markdown.len(),"verified":true,"markdown":markdown}}).as_object().unwrap().clone())}
        }
        Err(error)=>base.as_object_mut().unwrap().extend(json!({"status":"stale","reason":format!("continuation_ref_unreadable: {error}"),"artifact":{"path":path,"verified":false}}).as_object().unwrap().clone()),
    }
    Ok(base)
}

fn continuation_export_path(
    context: &Context,
    value: Option<&Value>,
    agent: &str,
    checkpoint: &str,
) -> Result<String, String> {
    let default = format!(".ai/continuations/{}-{checkpoint}.md", safe_segment(agent));
    let raw = match value {
        None | Some(Value::Null) => default,
        Some(Value::String(v)) => v.clone(),
        _ => return Err("continuation_export_path_must_be_site_relative".into()),
    };
    if raw.trim().is_empty()
        || raw.contains('\0')
        || raw.contains(':')
        || Path::new(&raw).is_absolute()
    {
        return Err("continuation_export_path_must_be_site_relative".into());
    }
    let normalized = raw.replace('\\', "/");
    if !normalized.to_ascii_lowercase().ends_with(".md") {
        return Err("continuation_export_path_must_be_markdown".into());
    }
    let parts = normalized
        .split('/')
        .filter(|p| !p.is_empty() && *p != ".")
        .collect::<Vec<_>>();
    if parts.iter().any(|p| *p == "..")
        || parts.get(0) != Some(&".ai")
        || parts.get(1) != Some(&"continuations")
    {
        return Err("continuation_export_path_outside_export_root".into());
    }
    let _ = &context.site_root;
    Ok(parts.join("/"))
}
fn safe_segment(value: &str) -> String {
    let value = value
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || "_.-".contains(c) {
                c
            } else {
                '-'
            }
        })
        .collect::<String>();
    let trimmed = value.trim_matches('-');
    if trimmed.is_empty() {
        "agent".into()
    } else {
        trimmed.chars().take(80).collect()
    }
}
fn render_continuation(agent: &str, checkpoint: &Value, c: &Value) -> String {
    let mut lines = vec![
        "<!-- narada.continuation.handoff.v1 -->".into(),
        format!(
            "<!-- narada.continuation.content-hash: {} -->",
            c["content_hash"].as_str().unwrap_or("")
        ),
        format!(
            "<!-- narada.continuation.source-checkpoint-ref: {} -->",
            c["source_checkpoint_ref"].as_str().unwrap_or("")
        ),
        "".into(),
        format!("# Continuation: {}", inline(&c["objective"])),
        "".into(),
        "- **Schema:** `narada.continuation.v1`".into(),
        format!("- **Continuation ID:** `{}`", inline(&c["continuation_id"])),
        format!("- **Agent:** `{}`", inline(&json!(agent))),
        format!(
            "- **Checkpoint:** `{}`",
            inline(&checkpoint["checkpoint_id"])
        ),
        format!(
            "- **Checkpointed:** {}",
            inline(&checkpoint["checkpoint_at"])
        ),
        format!("- **Created:** {}", inline(&c["created_at"])),
        format!("- **Resume mode:** `{}`", inline(&c["resume_mode"])),
        "".into(),
        "## Current state".into(),
        "".into(),
        block(&c["current_state"]),
        "".into(),
        "## Next action".into(),
        "".into(),
        if c["next_action"].is_null() {
            "No next action recorded.".into()
        } else {
            block(&c["next_action"])
        },
        "".into(),
    ];
    for (title, key) in [
        ("Completed work", "completed_work"),
        ("Decisions", "decisions"),
        ("Evidence references", "evidence_refs"),
        ("Open blockers", "open_blockers"),
        ("Canonical sources", "canonical_sources"),
        ("Constraints", "constraints"),
    ] {
        lines.push(format!("## {title}"));
        lines.push("".into());
        if let Some(items) = c[key].as_array() {
            if items.is_empty() {
                lines.push("_None._".into())
            } else {
                for item in items {
                    lines.push(format!("- {}", inline(item)))
                }
            }
        }
        lines.push("".into())
    }
    lines.push("> This file is a bounded projection of agent-context checkpoint state. Verify live Git, task, and agent-context state before acting.".into());
    lines.push("".into());
    lines.join("\n")
}
fn inline(v: &Value) -> String {
    value_text(v).replace(['\r', '\n'], " ").trim().into()
}
fn block(v: &Value) -> String {
    value_text(v).replace("\r\n", "\n").trim().into()
}
fn value_text(v: &Value) -> String {
    v.as_str().map(str::to_string).unwrap_or_else(|| {
        if v.is_null() {
            "".into()
        } else {
            v.to_string()
        }
    })
}

fn ensure_checkpoint_tables(db: &Connection) -> Result<(), String> {
    db.execute_batch("CREATE TABLE IF NOT EXISTS agent_checkpoints (checkpoint_id TEXT PRIMARY KEY,agent_id TEXT NOT NULL,session_id TEXT,checkpoint_at TEXT NOT NULL,active_task_json TEXT,files_touched_json TEXT,key_decisions_json TEXT,open_questions_json TEXT,git_head TEXT,payload_json TEXT); CREATE INDEX IF NOT EXISTS idx_agent_checkpoints_agent ON agent_checkpoints(agent_id,checkpoint_at DESC); CREATE TABLE IF NOT EXISTS agent_checkpoint_history (history_id TEXT PRIMARY KEY,checkpoint_id TEXT NOT NULL,agent_id TEXT NOT NULL,session_id TEXT,checkpoint_at TEXT NOT NULL,active_task_json TEXT,files_touched_json TEXT,key_decisions_json TEXT,open_questions_json TEXT,git_head TEXT,payload_json TEXT,archived_at TEXT NOT NULL); CREATE INDEX IF NOT EXISTS idx_checkpoint_history_agent ON agent_checkpoint_history(agent_id,archived_at DESC);").map_err(db_error)
}

fn checkpoint_by_id(db: &Connection, agent: &str, id: &str) -> Result<Option<Value>, String> {
    let current = db
        .query_row(
            "SELECT * FROM agent_checkpoints WHERE agent_id=?1 AND checkpoint_id=?2 LIMIT 1",
            params![agent, id],
            row_to_checkpoint,
        )
        .optional()
        .map_err(db_error)?;
    if current.is_some() {
        return Ok(current);
    }
    db.query_row("SELECT checkpoint_id,agent_id,session_id,checkpoint_at,active_task_json,files_touched_json,key_decisions_json,open_questions_json,git_head,payload_json FROM agent_checkpoint_history WHERE agent_id=?1 AND checkpoint_id=?2 ORDER BY archived_at DESC LIMIT 1",params![agent,id],row_to_checkpoint).optional().map_err(db_error)
}

fn row_to_checkpoint(row: &Row) -> rusqlite::Result<Value> {
    let payload: Value = parse_json(row.get::<_, Option<String>>(9)?.as_deref(), json!({}));
    Ok(
        json!({"checkpoint_id":row.get::<_,String>(0)?,"agent_id":row.get::<_,String>(1)?,"session_id":row.get::<_,Option<String>>(2)?,"checkpoint_at":row.get::<_,String>(3)?,"active_task":parse_json(row.get::<_,Option<String>>(4)?.as_deref(),Value::Null),"files_touched":parse_json(row.get::<_,Option<String>>(5)?.as_deref(),json!([])),"key_decisions":parse_json(row.get::<_,Option<String>>(6)?.as_deref(),json!([])),"open_questions":parse_json(row.get::<_,Option<String>>(7)?.as_deref(),json!([])),"git_head":row.get::<_,Option<String>>(8)?,"last_workboard_check_at":payload.get("last_workboard_check_at").cloned().unwrap_or(Value::Null),"next_intended_action":payload.get("next_intended_action").cloned().unwrap_or(Value::Null),"authority_basis":payload.get("authority_basis").cloned().unwrap_or(Value::Null),"continuation_blockers":payload.get("continuation_blockers").cloned().unwrap_or_else(||json!([])),"evidence_refs":payload.get("evidence_refs").cloned().unwrap_or_else(||json!([])),"worktree_state":payload.get("worktree_state").cloned().unwrap_or(Value::Null),"tactical_resume_notes":payload.get("tactical_resume_notes").cloned().unwrap_or_else(||json!([])),"continuation":payload.get("continuation").cloned().unwrap_or(Value::Null),"continuation_ref":payload.get("continuation_ref").cloned().unwrap_or(Value::Null),"continuation_projection":payload.get("continuation_projection").cloned().unwrap_or(Value::Null),"payload":payload}),
    )
}

fn validate_identity(context: &Context, agent: &str) -> Result<(), String> {
    if agent.trim().is_empty() {
        return Err(format!("agent_context_identity_invalid: {agent}"));
    }
    let roster = context.site_root.join(".ai/agents/roster.json");
    if !roster.exists() {
        return Ok(());
    }
    let value: Value =
        serde_json::from_slice(&fs::read(&roster).map_err(|e| format!("roster_read_error: {e}"))?)
            .map_err(|e| format!("roster_parse_error: {e}"))?;
    let found = value
        .get("agents")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .any(|v| v.get("agent_id").and_then(Value::as_str) == Some(agent))
        })
        .unwrap_or(false);
    if found || value.get("enforce_session_roster") != Some(&Value::Bool(true)) {
        Ok(())
    } else {
        Err(format!("identity_not_in_roster: {agent}"))
    }
}

fn normalize_continuation(
    value: Option<&Value>,
    checkpoint_id: &str,
    checkpoint_at: &str,
) -> Result<Option<Value>, String> {
    let Some(value) = value.filter(|v| !v.is_null()) else {
        return Ok(None);
    };
    let object = value
        .as_object()
        .ok_or("continuation_invalid: expected an object")?;
    let allowed = [
        "schema",
        "continuation_id",
        "objective",
        "current_state",
        "completed_work",
        "decisions",
        "evidence_refs",
        "open_blockers",
        "next_action",
        "canonical_sources",
        "constraints",
        "resume_mode",
        "created_at",
    ];
    if let Some(key) = object.keys().find(|k| !allowed.contains(&k.as_str())) {
        return Err(format!("continuation_field_unknown: {key}"));
    }
    if value.get("schema").and_then(Value::as_str) != Some("narada.continuation.v1") {
        return Err("continuation_schema_invalid".into());
    }
    let objective =
        required_string(value, "objective").map_err(|_| "continuation_objective_invalid")?;
    let current_state = required_string(value, "current_state")
        .map_err(|_| "continuation_current_state_invalid")?;
    let resume = value
        .get("resume_mode")
        .and_then(Value::as_str)
        .unwrap_or("fresh_session");
    if !matches!(resume, "fresh_session" | "same_session") {
        return Err("continuation_resume_mode_invalid".into());
    }
    let mut canonical = json!({"schema":"narada.continuation.v1","continuation_id":value.get("continuation_id").and_then(Value::as_str).map(str::to_string).unwrap_or_else(||id("cont")),"objective":objective,"current_state":current_state,"completed_work":array(value,"completed_work"),"decisions":array(value,"decisions"),"evidence_refs":array(value,"evidence_refs"),"open_blockers":array(value,"open_blockers"),"next_action":field_or_null(value,"next_action"),"canonical_sources":array(value,"canonical_sources"),"constraints":array(value,"constraints"),"resume_mode":resume,"source_checkpoint_ref":format!("agent_context_checkpoint:{checkpoint_id}"),"created_at":value.get("created_at").cloned().unwrap_or_else(||Value::String(checkpoint_at.into()))});
    let mut content = canonical.clone();
    content
        .as_object_mut()
        .unwrap()
        .remove("source_checkpoint_ref");
    use sha2::{Digest, Sha256};
    canonical.as_object_mut().unwrap().insert(
        "content_hash".into(),
        Value::String(format!(
            "{:x}",
            Sha256::digest(serde_json::to_vec(&content).unwrap())
        )),
    );
    Ok(Some(canonical))
}

fn continuation_projection(
    agent: &str,
    reference: Option<&Value>,
    previous: Option<&Value>,
) -> Value {
    if let Some(reference) = reference {
        return json!({"status":"linked","reason":null,"continuation_ref":reference,"previous_checkpoint_id":null,"next_action":null});
    }
    let previous_ref = previous
        .and_then(|v| v.get("continuation_ref"))
        .filter(|v| !v.is_null());
    let mut arguments = json!({"agent_id":agent});
    if let Some(path) = previous_ref
        .and_then(|v| v.get("path"))
        .and_then(Value::as_str)
    {
        arguments
            .as_object_mut()
            .unwrap()
            .insert("path".into(), json!(path));
        arguments
            .as_object_mut()
            .unwrap()
            .insert("overwrite".into(), json!(true));
    }
    json!({"status":if previous_ref.is_some(){"stale"}else{"unlinked"},"reason":if previous_ref.is_some(){"checkpoint_supersedes_linked_projection"}else{"continuation_projection_not_exported"},"continuation_ref":previous_ref.cloned().unwrap_or(Value::Null),"previous_checkpoint_id":if previous_ref.is_some(){previous.and_then(|v|v.get("checkpoint_id")).cloned().unwrap_or(Value::Null)}else{Value::Null},"next_action":{"tool":"agent_context_continuation_export","arguments":arguments}})
}

fn required_string(v: &Value, key: &str) -> Result<String, String> {
    v.get(key)
        .and_then(Value::as_str)
        .filter(|s| !s.trim().is_empty())
        .map(str::to_string)
        .ok_or_else(|| format!("{key}_required"))
}
fn optional_string(v: &Value, key: &str) -> Result<Option<String>, String> {
    match v.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) if !s.trim().is_empty() => Ok(Some(s.trim().into())),
        _ => Err(format!("{key}_invalid")),
    }
}
fn field_or_null(v: &Value, key: &str) -> Value {
    v.get(key).cloned().unwrap_or(Value::Null)
}
fn array(v: &Value, key: &str) -> Value {
    v.get(key)
        .filter(|v| v.is_array())
        .cloned()
        .unwrap_or_else(|| json!([]))
}
fn json_text(v: Value) -> String {
    serde_json::to_string(&v).unwrap()
}
fn json_db(v: Option<&Value>) -> Option<String> {
    v.filter(|v| !v.is_null()).map(|v| json_text(v.clone()))
}
fn parse_json(v: Option<&str>, fallback: Value) -> Value {
    v.and_then(|s| serde_json::from_str(s).ok())
        .unwrap_or(fallback)
}
fn merge(mut a: Value, b: Value) -> Value {
    a.as_object_mut()
        .unwrap()
        .extend(b.as_object().unwrap().clone());
    a
}
fn timestamp() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true)
}
fn id(prefix: &str) -> String {
    format!("{prefix}_{}", Uuid::new_v4().simple())
}
fn path_text(path: &Path) -> String {
    let text = path.to_string_lossy();
    text.strip_prefix(r"\\?\").unwrap_or(&text).to_string()
}
fn sanitize_site_id(v: &str) -> String {
    v.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || "_.-".contains(c) {
                c
            } else {
                '-'
            }
        })
        .collect()
}
fn db_error(e: rusqlite::Error) -> String {
    format!("agent_context_db_error:{e}")
}
