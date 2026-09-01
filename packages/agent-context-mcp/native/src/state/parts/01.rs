use chrono::{SecondsFormat, Utc};
use rusqlite::{params, Connection, OptionalExtension, Row};
use serde_json::{json, Value};
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use uuid::Uuid;

const MIGRATION_001: &str = include_str!("../../../../migrations/001-agent-context-materializations.sql");
const MIGRATION_002: &str = include_str!("../../../../migrations/002-agent-events.sql");
const MIGRATION_003: &str = include_str!("../../../migrations/003-agent-context-compatibility.sql");

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
        db.execute_batch("CREATE TABLE IF NOT EXISTS codex_session_admissions (admission_id TEXT PRIMARY KEY, agent_id TEXT NOT NULL, runtime TEXT NOT NULL DEFAULT 'codex', cwd TEXT NOT NULL, status TEXT NOT NULL CHECK (status IN ('creating','admitted','suspect','retired')), agent_start_event_id TEXT, codex_session_id TEXT, codex_session_file TEXT, evidence_json TEXT NOT NULL DEFAULT '{}', created_at TEXT NOT NULL, verified_at TEXT); CREATE INDEX IF NOT EXISTS idx_codex_session_admissions_agent ON codex_session_admissions(agent_id,cwd,status,created_at DESC); CREATE INDEX IF NOT EXISTS idx_codex_session_admissions_session ON codex_session_admissions(codex_session_id); CREATE TABLE IF NOT EXISTS identity_state_records (record_id TEXT PRIMARY KEY, event_id TEXT, session_id TEXT, claimed_identity_json TEXT NOT NULL, authentication_json TEXT NOT NULL, authority_json TEXT NOT NULL, recorded_at TEXT NOT NULL); CREATE TRIGGER IF NOT EXISTS identity_state_records_no_update BEFORE UPDATE ON identity_state_records BEGIN SELECT RAISE(ABORT, 'identity_state_records_append_only_no_update'); END; CREATE TRIGGER IF NOT EXISTS identity_state_records_no_delete BEFORE DELETE ON identity_state_records BEGIN SELECT RAISE(ABORT, 'identity_state_records_append_only_no_delete'); END;").map_err(db_error)?;
        ensure_checkpoint_tables(&db)?;
        Ok(db)
    }

    pub(crate) fn prepare(&self) -> Result<(), String> {
        self.open_db().map(drop)
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
        ("claimed_identity_json", "TEXT"),
        ("authentication_json", "TEXT"),
        ("authority_json", "TEXT"),
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
    name: &str,
    args: &Value,
) -> Result<Value, String> {
    match name {
        "agent_context_doctor" => doctor(context),
        "agent_context_guidance" => guidance(args),
        "agent_context_whoami" => crate::orientation::whoami(context, args),
        "agent_context_hydrate_current" => hydrate_current(context, args),
        "agent_orientation_read" => crate::orientation::read(context, projection, args),
        "agent_orientation_acknowledge" => crate::orientation::acknowledge_tool(context, args),
        "agent_context_startup_sequence" => crate::orientation::startup(context, args),
        "mcp_output_show" => output_show(context, args),
        "agent_context_checkpoint" => checkpoint(context, args),
        "agent_context_rehydrate" => rehydrate(context, args),
        "agent_context_continuation_export" => continuation_export(context, args),
        "agent_context_continuation_read" => continuation_read(context, args),
        "agent_context_list_sessions" => list_sessions(context, args),
        "agent_context_start_session" => start_session(context, args),
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
            args.get("claimed_identity"),
        );
    }
    Ok(
        json!({"schema":"narada.agent_context.session_start.v1","status":"dry_run","authority_claimed":false,"identity":identity,"claimed_identity":{"identity":identity,"status":"claimed","source":"caller_assertion","asserted_at":null,"evidence_refs":[],"authority_granted":false},"authenticated_identity":null,"authentication":{"status":"missing","authenticated_identity":null,"evidence_refs":[]},"authority":{"status":"not_evaluated","operation":null,"granted":false,"evidence_refs":[]},"identity_state":{"schema":"narada.agent.identity_state.v1","claimed_identity":{"identity":identity,"status":"claimed","source":"caller_assertion","asserted_at":null,"evidence_refs":[],"authority_granted":false},"authentication":{"status":"missing","authenticated_identity":null,"evidence_refs":[]},"authority":{"status":"not_evaluated","operation":null,"granted":false,"evidence_refs":[]}},"role":roster["role"],"role_binding":roster["role_binding"],"runtime_request":runtime,"root_dir":path_text(&context.site_root),"cwd_request":cwd,"db_path":path_text(&context.db_path),"would_validate":{"roster_or_identity_projection":true,"exact_admission_receipt":true,"orientation_manifest":true},"would_write":["orientation_manifest_generations","agent_start_events_downstream_trace"],"orientation_manifest":null,"required_for_materialization":["site_id","carrier_session_admission_receipt"]}),
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

