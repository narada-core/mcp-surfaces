use jsonschema::validator_for;
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use time::format_description::well_known::Rfc3339;
use time::{Duration, OffsetDateTime, UtcOffset};
use uuid::Uuid;

const DB_RELATIVE: &str = ".sop/sop.db";
pub(crate) const MAX_INLINE_VALUE_BYTES: usize = 16 * 1024;
pub(crate) const MAX_RUN_STATE_BYTES: usize = 128 * 1024;
pub(crate) const MAX_TEMPLATE_DEFINITION_BYTES: usize = 128 * 1024;
const MAX_TEMPLATE_FILE_BYTES: u64 = 512 * 1024;
const MAX_STEPS: usize = 128;
pub(crate) const MAX_OUTBOX_PAYLOAD_BYTES: usize = 16 * 1024;
const MAX_OUTBOX_RECEIPT_BYTES: usize = 8 * 1024;
const MIN_LEASE_MS: i64 = 1_000;
const MAX_LEASE_MS: i64 = 5 * 60_000;
pub(crate) const SOP_TERMINAL_TOPIC: &str = "sop.run.terminal.v1";
const TEMPLATE_SCHEMA: &str = include_str!("../../../../sop-mcp/sops/sop-template.schema.json");

pub fn call_tool(name: &str, args: &Map<String, Value>, root: &Path) -> Result<Value, Value> {
    match name {
        "sop_template_create" => template_create(args, root),
        "sop_template_update" => template_update(args, root),
        "sop_template_deprecate" => template_deprecate(args, root),
        "sop_template_unimport" => template_unimport(args, root),
        "sop_template_import_yaml" => template_import_yaml(args, root),
        "sop_handoff_claim" => handoff_claim(args, root),
        "sop_handoff_claim_and_advance" => handoff_claim_and_advance(args, root),
        "sop_handoff_renew" => handoff_renew(args, root),
        "sop_handoff_release" => handoff_release(args, root),
        "sop_outbox_consumer_register" => outbox_consumer_register(args, root),
        "sop_outbox_ack" => outbox_ack(args, root),
        "sop_outbox_compact" => outbox_compact(args, root),
        "sop_run_start" | "sop_run_refresh" | "sop_run_advance" | "sop_handoff_retry"
        | "sop_action_resolve" | "sop_run_cancel" => crate::sop_engine::call_tool(name, args, root),
        _ => Err(diagnostic(
            "unknown_tool",
            &format!("unknown_tool:{name}"),
            json!({"tool_name":name}),
        )),
    }
}

pub(crate) fn open_db(root: &Path) -> Result<Connection, Value> {
    let path = root.join(DB_RELATIVE);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|error| {
            diagnostic(
                "sop_registry_directory_failed",
                &error.to_string(),
                json!({}),
            )
        })?;
    }
    let connection = Connection::open(&path).map_err(|error| {
        diagnostic(
            "sop_registry_open_failed",
            &error.to_string(),
            json!({"db_path":path}),
        )
    })?;
    connection
        .pragma_update(None, "journal_mode", "WAL")
        .map_err(|error| diagnostic("sop_registry_pragma_failed", &error.to_string(), json!({})))?;
    connection
        .pragma_update(None, "foreign_keys", true)
        .map_err(|error| diagnostic("sop_registry_pragma_failed", &error.to_string(), json!({})))?;
    connection
        .busy_timeout(std::time::Duration::from_millis(5_000))
        .map_err(|error| diagnostic("sop_registry_pragma_failed", &error.to_string(), json!({})))?;
    prepare_schema(&connection)?;
    Ok(connection)
}

fn prepare_schema(db: &Connection) -> Result<(), Value> {
    db.execute_batch(
        r#"
        CREATE TABLE IF NOT EXISTS sop_templates (
          sop_id TEXT NOT NULL,
          version INTEGER NOT NULL DEFAULT 1,
          title TEXT NOT NULL,
          status TEXT NOT NULL DEFAULT 'draft',
          description TEXT NOT NULL DEFAULT '',
          steps_json TEXT NOT NULL DEFAULT '[]',
          trigger_kind TEXT NOT NULL DEFAULT 'manual',
          input_schema_json TEXT,
          output_mapping_json TEXT,
          output_ref_mapping_json TEXT,
          output_schema_json TEXT,
          acceptance_criteria_json TEXT NOT NULL DEFAULT '[]',
          evidence_requirements_json TEXT NOT NULL DEFAULT '[]',
          created_at TEXT NOT NULL,
          updated_at TEXT NOT NULL,
          PRIMARY KEY (sop_id, version)
        ) STRICT;
        CREATE TABLE IF NOT EXISTS sop_runs (
          run_id TEXT PRIMARY KEY,
          sop_id TEXT NOT NULL,
          sop_version INTEGER NOT NULL,
          sop_title TEXT NOT NULL,
          status TEXT NOT NULL DEFAULT 'pending',
          occurrence_key TEXT NOT NULL DEFAULT '',
          request_fingerprint TEXT NOT NULL DEFAULT '',
          definition_fingerprint TEXT NOT NULL DEFAULT '',
          definition_json TEXT NOT NULL DEFAULT '{}',
          input_json TEXT NOT NULL DEFAULT '{}',
          input_ref_json TEXT,
          output_json TEXT NOT NULL DEFAULT '{}',
          output_ref_json TEXT,
          step_states_json TEXT NOT NULL DEFAULT '[]',
          trigger_source_kind TEXT NOT NULL DEFAULT 'manual',
          trigger_source_ref TEXT NOT NULL DEFAULT '',
          triggered_by TEXT NOT NULL DEFAULT '',
          parent_run_id TEXT,
          parent_step_id TEXT,
          created_at TEXT NOT NULL,
          updated_at TEXT NOT NULL,
          completed_at TEXT
        ) STRICT;
        CREATE TABLE IF NOT EXISTS sop_events (
          event_id TEXT PRIMARY KEY,
          run_id TEXT NOT NULL,
          step_id TEXT NOT NULL,
          event_kind TEXT NOT NULL,
          details_json TEXT NOT NULL DEFAULT '{}',
          recorded_at TEXT NOT NULL
        ) STRICT;
        CREATE TABLE IF NOT EXISTS sop_actions (
          action_id TEXT PRIMARY KEY,
          run_id TEXT NOT NULL,
          step_id TEXT NOT NULL,
          occurrence_key TEXT NOT NULL,
          surface_id TEXT NOT NULL,
          tool_name TEXT NOT NULL,
          arguments_json TEXT NOT NULL,
          request_fingerprint TEXT NOT NULL,
          status TEXT NOT NULL DEFAULT 'pending',
          completion_key TEXT,
          completion_fingerprint TEXT,
          operation_ref TEXT,
          result_json TEXT NOT NULL DEFAULT '{}',
          result_ref_json TEXT,
          error_message TEXT,
          created_at TEXT NOT NULL,
          updated_at TEXT NOT NULL,
          completed_at TEXT,
          UNIQUE (run_id, step_id),
          UNIQUE (occurrence_key)
        ) STRICT;
        CREATE UNIQUE INDEX IF NOT EXISTS sop_runs_occurrence_unique
          ON sop_runs (sop_id, occurrence_key) WHERE occurrence_key <> '';
        CREATE INDEX IF NOT EXISTS sop_runs_status_idx ON sop_runs (status, updated_at);
        CREATE INDEX IF NOT EXISTS sop_runs_parent_idx ON sop_runs (parent_run_id, parent_step_id);
        CREATE INDEX IF NOT EXISTS sop_actions_status_idx ON sop_actions (status, created_at);
        CREATE TABLE IF NOT EXISTS sop_handoffs (
          handoff_id TEXT PRIMARY KEY,
          run_id TEXT NOT NULL REFERENCES sop_runs(run_id),
          step_id TEXT NOT NULL,
          occurrence_key TEXT NOT NULL UNIQUE,
          sop_id TEXT NOT NULL,
          sop_version INTEGER NOT NULL,
          executor TEXT NOT NULL CHECK (executor IN ('agent', 'operator')),
          title TEXT NOT NULL,
          instructions TEXT NOT NULL CHECK (length(CAST(instructions AS BLOB)) <= 16384),
          input_json TEXT NOT NULL CHECK (length(CAST(input_json AS BLOB)) <= 16384),
          input_ref_json TEXT,
          result_schema_json TEXT,
          request_fingerprint TEXT NOT NULL,
          status TEXT NOT NULL DEFAULT 'pending' CHECK (status IN ('pending', 'leased', 'completed', 'failed', 'cancelled')),
          lease_owner TEXT,
          lease_token TEXT,
          lease_expires_at TEXT,
          attempt_count INTEGER NOT NULL DEFAULT 0,
          last_error TEXT,
          completion_key TEXT,
          completion_fingerprint TEXT,
          principal TEXT,
          result_json TEXT NOT NULL DEFAULT '{}' CHECK (length(CAST(result_json AS BLOB)) <= 16384),
          result_ref_json TEXT,
          error_message TEXT,
          created_at TEXT NOT NULL,
          updated_at TEXT NOT NULL,
          completed_at TEXT,
          UNIQUE (run_id, step_id)
        ) STRICT;
        CREATE INDEX IF NOT EXISTS sop_handoffs_delivery_idx
          ON sop_handoffs(status, lease_expires_at, created_at);
        CREATE TABLE IF NOT EXISTS sop_outbox (
          event_id TEXT PRIMARY KEY,
          topic TEXT NOT NULL,
          partition_key TEXT NOT NULL,
          run_id TEXT NOT NULL UNIQUE REFERENCES sop_runs(run_id),
          sop_id TEXT NOT NULL,
          sop_version INTEGER NOT NULL,
          occurrence_key TEXT NOT NULL,
          outcome TEXT NOT NULL CHECK (outcome IN ('completed', 'failed', 'cancelled')),
          payload_json TEXT NOT NULL CHECK (length(CAST(payload_json AS BLOB)) <= 16384),
          created_at TEXT NOT NULL,
          available_at TEXT NOT NULL,
          compacted_at TEXT
        ) STRICT;
        CREATE INDEX IF NOT EXISTS sop_outbox_delivery_idx
          ON sop_outbox(topic, available_at, created_at);
        CREATE TABLE IF NOT EXISTS sop_outbox_consumer_requirements (
          topic TEXT NOT NULL,
          consumer_id TEXT NOT NULL,
          start_at TEXT NOT NULL,
          registered_at TEXT NOT NULL,
          PRIMARY KEY(topic, consumer_id)
        ) STRICT;
        CREATE TABLE IF NOT EXISTS sop_outbox_receipts (
          event_id TEXT NOT NULL REFERENCES sop_outbox(event_id),
          consumer_id TEXT NOT NULL,
          processed_at TEXT NOT NULL,
          receipt_json TEXT NOT NULL CHECK (length(CAST(receipt_json AS BLOB)) <= 8192),
          PRIMARY KEY(event_id, consumer_id)
        ) STRICT;
        "#,
    )
    .map_err(|error| diagnostic("sop_registry_schema_failed", &error.to_string(), json!({})))?;
    Ok(())
}

fn template_create(args: &Map<String, Value>, root: &Path) -> Result<Value, Value> {
    let sop_id = required_string(args.get("sop_id"), "sop_requires_sop_id", 512)?;
    let title = required_string(args.get("title"), "sop_requires_title", 512)?;
    let steps = validate_steps(args.get("steps"), Some(&sop_id))?;
    let input_schema = optional_schema(args.get("input_schema"), "input_schema")?;
    let output = optional_value(args.get("output"), "output")?;
    let output_ref = optional_value(args.get("output_ref"), "output_ref")?;
    let output_schema = optional_schema(args.get("output_schema"), "output_schema")?;
    validate_output_references(output.as_ref(), &steps)?;
    validate_output_references(output_ref.as_ref(), &steps)?;
    let acceptance = string_list(args.get("acceptance_criteria"))?;
    let evidence = string_list(args.get("evidence_requirements"))?;
    assert_template_bound(&json!({
        "sop_id":sop_id,"title":title,"steps":steps,"input_schema":input_schema,
        "output":output,"output_ref":output_ref,"output_schema":output_schema,
        "acceptance_criteria":acceptance,"evidence_requirements":evidence
    }))?;
    let description = optional_string(args.get("description")).unwrap_or_default();
    let trigger_kind = normalize_trigger(args.get("trigger_kind"))?;
    let db = open_db(root)?;
    let version = db
        .query_row(
            "SELECT MAX(version) FROM sop_templates WHERE sop_id = ?",
            params![sop_id],
            |row| row.get::<_, Option<i64>>(0),
        )
        .map_err(|error| diagnostic("sop_template_query_failed", &error.to_string(), json!({})))?
        .unwrap_or(0)
        + 1;
    let now = now_iso();
    insert_template(
        &db,
        &sop_id,
        version,
        &title,
        "draft",
        &description,
        &steps,
        &trigger_kind,
        input_schema.as_ref(),
        output.as_ref(),
        output_ref.as_ref(),
        output_schema.as_ref(),
        &acceptance,
        &evidence,
        &now,
    )?;
    append_event(
        &db,
        "template_created",
        json!({"sop_id":sop_id,"version":version}),
    )?;
    let step_count = steps.as_array().map_or(0, Vec::len);
    Ok(
        json!({"status":"created","sop_id":sop_id,"version":version,"title":title,"step_count":step_count}),
    )
}

fn template_update(args: &Map<String, Value>, root: &Path) -> Result<Value, Value> {
    let sop_id = required_string(args.get("sop_id"), "sop_requires_sop_id", 512)?;
    let db = open_db(root)?;
    let current = latest_template(&db, &sop_id)?.ok_or_else(|| {
        diagnostic(
            "sop_not_found",
            &format!("sop_not_found:{sop_id}"),
            json!({}),
        )
    })?;
    let current_object = current
        .as_object()
        .ok_or_else(|| diagnostic("sop_template_corrupt", "sop_template_corrupt", json!({})))?;
    let current_version = current_object
        .get("version")
        .and_then(Value::as_i64)
        .unwrap_or(0);
    let title =
        optional_string(args.get("title")).unwrap_or_else(|| text_member(current_object, "title"));
    let description = optional_string(args.get("description"))
        .unwrap_or_else(|| text_member(current_object, "description"));
    let steps = if args.contains_key("steps") {
        validate_steps(args.get("steps"), Some(&sop_id))?
    } else {
        parse_json_member(current_object, "steps_json", json!([]))?
    };
    let input_schema = if args.contains_key("input_schema") {
        optional_schema(args.get("input_schema"), "input_schema")?
    } else {
        parse_nullable_member(current_object, "input_schema_json")?
    };
    let output = if args.contains_key("output") {
        optional_value(args.get("output"), "output")?
    } else {
        parse_nullable_member(current_object, "output_mapping_json")?
    };
    let output_ref = if args.contains_key("output_ref") {
        optional_value(args.get("output_ref"), "output_ref")?
    } else {
        parse_nullable_member(current_object, "output_ref_mapping_json")?
    };
    let output_schema = if args.contains_key("output_schema") {
        optional_schema(args.get("output_schema"), "output_schema")?
    } else {
        parse_nullable_member(current_object, "output_schema_json")?
    };
    validate_output_references(output.as_ref(), &steps)?;
    validate_output_references(output_ref.as_ref(), &steps)?;
    let trigger_kind = if args.contains_key("trigger_kind") {
        normalize_trigger(args.get("trigger_kind"))?
    } else {
        normalize_trigger(current_object.get("trigger_kind"))?
    };
    let status = normalize_template_status(args.get("status"))?;
    let acceptance = if args.contains_key("acceptance_criteria") {
        string_list(args.get("acceptance_criteria"))?
    } else {
        string_list(Some(&parse_json_member(
            current_object,
            "acceptance_criteria_json",
            json!([]),
        )?))?
    };
    let evidence = if args.contains_key("evidence_requirements") {
        string_list(args.get("evidence_requirements"))?
    } else {
        string_list(Some(&parse_json_member(
            current_object,
            "evidence_requirements_json",
            json!([]),
        )?))?
    };
    assert_template_bound(&json!({
        "sop_id":sop_id,"title":title,"steps":steps,"input_schema":input_schema,
        "output":output,"output_ref":output_ref,"output_schema":output_schema,
        "acceptance_criteria":acceptance,"evidence_requirements":evidence
    }))?;
    let version = current_version + 1;
    let now = now_iso();
    insert_template(
        &db,
        &sop_id,
        version,
        &title,
        &status,
        &description,
        &steps,
        &trigger_kind,
        input_schema.as_ref(),
        output.as_ref(),
        output_ref.as_ref(),
        output_schema.as_ref(),
        &acceptance,
        &evidence,
        &now,
    )?;
    append_event(
        &db,
        "template_updated",
        json!({"sop_id":sop_id,"version":version,"previous_version":current_version}),
    )?;
    Ok(
        json!({"status":"updated","sop_id":sop_id,"version":version,"previous_version":current_version,"title":title,"step_count":steps.as_array().map(Vec::len).unwrap_or(0)}),
    )
}

fn template_deprecate(args: &Map<String, Value>, root: &Path) -> Result<Value, Value> {
    let sop_id = required_string(args.get("sop_id"), "sop_requires_sop_id", 512)?;
    let db = open_db(root)?;
    let current = latest_template(&db, &sop_id)?.ok_or_else(|| {
        diagnostic(
            "sop_not_found",
            &format!("sop_not_found:{sop_id}"),
            json!({}),
        )
    })?;
    let version = current.get("version").and_then(Value::as_i64).unwrap_or(0);
    db.execute(
        "UPDATE sop_templates SET status = 'deprecated' WHERE sop_id = ? AND version = ?",
        params![sop_id, version],
    )
    .map_err(|error| diagnostic("sop_template_update_failed", &error.to_string(), json!({})))?;
    let mut details = Map::new();
    details.insert("sop_id".to_string(), json!(sop_id));
    details.insert("version".to_string(), json!(version));
    if let Some(reason) = optional_string(args.get("reason")) {
        details.insert("reason".to_string(), json!(reason));
    }
    append_event(&db, "template_deprecated", Value::Object(details))?;
    Ok(json!({"status":"deprecated","sop_id":sop_id,"version":version}))
}

fn template_unimport(args: &Map<String, Value>, root: &Path) -> Result<Value, Value> {
    let sop_id = required_string(args.get("sop_id"), "sop_requires_sop_id", 512)?;
    let reason = required_string(args.get("reason"), "sop_unimport_requires_reason", 4096)?;
    let principal = required_string(
        args.get("principal"),
        "sop_unimport_requires_principal",
        512,
    )?;
    let requested_version = match args.get("version") {
        None | Some(Value::Null) => None,
        Some(value) => {
            let version = value.as_i64().ok_or_else(|| {
                diagnostic(
                    "sop_invalid_version",
                    "sop_invalid_version",
                    json!({"sop_id":sop_id}),
                )
            })?;
            if version < 1 {
                return Err(diagnostic(
                    "sop_invalid_version",
                    &format!("sop_invalid_version:{version}"),
                    json!({"sop_id":sop_id}),
                ));
            }
            Some(version)
        }
    };
    let db = open_db(root)?;
    let selected = if let Some(version) = requested_version {
        template_by_version(&db, &sop_id, version)?
    } else {
        latest_template(&db, &sop_id)?
    }
    .ok_or_else(|| {
        let suffix = requested_version
            .map(|version| format!("@v{version}"))
            .unwrap_or_default();
        diagnostic(
            "sop_not_found",
            &format!("sop_not_found:{sop_id}{suffix}"),
            json!({}),
        )
    })?;
    let version = selected.get("version").and_then(Value::as_i64).unwrap_or(0);
    let run_count: i64 = db
        .query_row(
            "SELECT COUNT(*) FROM sop_runs WHERE sop_id = ? AND sop_version = ?",
            params![sop_id, version],
            |row| row.get(0),
        )
        .map_err(|error| diagnostic("sop_run_query_failed", &error.to_string(), json!({})))?;
    let mut run_statement = db
        .prepare("SELECT run_id,status,created_at FROM sop_runs WHERE sop_id = ? AND sop_version = ? ORDER BY created_at DESC LIMIT 10")
        .map_err(|error| diagnostic("sop_run_query_failed", &error.to_string(), json!({})))?;
    let run_refs = run_statement
        .query_map(params![sop_id, version], |row| {
            Ok(json!({"run_id":row.get::<_,String>(0)?,"status":row.get::<_,String>(1)?,"created_at":row.get::<_,String>(2)?}))
        })
        .map_err(|error| diagnostic("sop_run_query_failed", &error.to_string(), json!({})))?
        .take(10)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| diagnostic("sop_run_query_failed", &error.to_string(), json!({})))?;
    let pinned_child_refs = pinned_child_references(&db, &sop_id, version)?;
    if run_count > 0 || !pinned_child_refs.is_empty() {
        return Err(diagnostic(
            "sop_template_has_runs",
            &format!("sop_template_has_runs:{sop_id}@v{version}"),
            json!({"sop_id":sop_id,"version":version,"run_count":run_count,"run_refs":run_refs,"pinned_child_refs":pinned_child_refs}),
        ));
    }
    db.execute(
        "DELETE FROM sop_templates WHERE sop_id = ? AND version = ?",
        params![sop_id, version],
    )
    .map_err(|error| diagnostic("sop_template_delete_failed", &error.to_string(), json!({})))?;
    let mut statement = db
        .prepare("SELECT version FROM sop_templates WHERE sop_id = ? ORDER BY version ASC")
        .map_err(|error| diagnostic("sop_template_query_failed", &error.to_string(), json!({})))?;
    let remaining = statement
        .query_map(params![sop_id], |row| row.get::<_, i64>(0))
        .map_err(|error| diagnostic("sop_template_query_failed", &error.to_string(), json!({})))?
        .take(10_000)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| diagnostic("sop_template_query_failed", &error.to_string(), json!({})))?;
    let event_id = append_event(
        &db,
        "template_unimported",
        json!({"sop_id":sop_id,"version":version,"reason":reason,"principal":principal,"remaining_versions":remaining}),
    )?;
    Ok(
        json!({"status":"unimported","sop_id":sop_id,"version":version,"remaining_versions":remaining,"runs_checked":run_count,"event_id":event_id}),
    )
}

fn template_import_yaml(args: &Map<String, Value>, root: &Path) -> Result<Value, Value> {
    let sop_id = required_string(args.get("sop_id"), "sop_requires_sop_id", 512)?;
    let file_name = format!("{sop_id}.sop.yaml");
    let yaml_path = sops_dirs(root)
        .into_iter()
        .map(|directory| directory.join(&file_name))
        .find(|candidate| candidate.is_file())
        .ok_or_else(|| {
            diagnostic(
                "sop_yaml_not_found",
                &format!("sop_yaml_not_found:{sop_id}"),
                json!({"searched":sops_dirs(root),"file":file_name}),
            )
        })?;
    let parsed = parse_yaml_template(&yaml_path, &sop_id)?;
    let db = open_db(root)?;
    let current = latest_template(&db, &sop_id)?;
    if let Some(current) = current.as_ref() {
        if template_matches(current, &parsed)? {
            return Ok(json!({
                "status":"unchanged","sop_id":sop_id,
                "version":current.get("version").and_then(Value::as_i64).unwrap_or(0),
                "title":parsed.get("title"),
                "step_count":parsed.get("steps").and_then(Value::as_array).map(Vec::len).unwrap_or(0)
            }));
        }
    }
    let previous_version = current
        .as_ref()
        .and_then(|value| value.get("version"))
        .and_then(Value::as_i64);
    let version = previous_version.unwrap_or(0) + 1;
    let object = parsed.as_object().expect("normalized YAML object");
    let now = now_iso();
    insert_template(
        &db,
        &sop_id,
        version,
        object.get("title").and_then(Value::as_str).unwrap_or(""),
        object
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or("draft"),
        object
            .get("description")
            .and_then(Value::as_str)
            .unwrap_or(""),
        object.get("steps").unwrap_or(&Value::Array(Vec::new())),
        object
            .get("trigger_kind")
            .and_then(Value::as_str)
            .unwrap_or("manual"),
        nullable_member(object, "input_schema"),
        nullable_member(object, "output"),
        nullable_member(object, "output_ref"),
        nullable_member(object, "output_schema"),
        object
            .get("acceptance_criteria")
            .and_then(Value::as_array)
            .unwrap_or(&Vec::new()),
        object
            .get("evidence_requirements")
            .and_then(Value::as_array)
            .unwrap_or(&Vec::new()),
        &now,
    )?;
    let event_kind = if previous_version.is_some() {
        "template_updated"
    } else {
        "template_created"
    };
    let mut details = Map::new();
    details.insert("sop_id".to_string(), json!(sop_id));
    details.insert("version".to_string(), json!(version));
    if let Some(previous) = previous_version {
        details.insert("previous_version".to_string(), json!(previous));
    }
    details.insert("source".to_string(), json!("yaml_import"));
    details.insert("yaml_path".to_string(), json!(yaml_path.to_string_lossy()));
    append_event(&db, event_kind, Value::Object(details))?;
    let status = if previous_version.is_some() {
        "updated"
    } else {
        "created"
    };
    let mut response = Map::new();
    response.insert("status".to_string(), json!(status));
    response.insert("sop_id".to_string(), json!(sop_id));
    response.insert("version".to_string(), json!(version));
    if let Some(previous) = previous_version {
        response.insert("previous_version".to_string(), json!(previous));
    }
    response.insert(
        "title".to_string(),
        object.get("title").cloned().unwrap_or(Value::Null),
    );
    response.insert(
        "step_count".to_string(),
        json!(object
            .get("steps")
            .and_then(Value::as_array)
            .map(Vec::len)
            .unwrap_or(0)),
    );
    Ok(Value::Object(response))
}

fn handoff_claim(args: &Map<String, Value>, root: &Path) -> Result<Value, Value> {
    transactional(root, |db| {
        let consumer_id = required_string(
            args.get("consumer_id"),
            "sop_handoff_consumer_id_required",
            512,
        )?;
        let requested_handoff_id = optional_string(args.get("handoff_id"));
        let executor = match optional_string(args.get("executor")) {
            Some(value) => Some(normalize_handoff_executor(&value)?),
            None => None,
        };
        let lease_ms = bounded_integer_arg(
            args.get("lease_ms"),
            60_000,
            MIN_LEASE_MS,
            MAX_LEASE_MS,
            "sop_handoff_lease_ms_invalid",
        )?;
        let now = OffsetDateTime::now_utc();
        let now_text = format_iso(now);
        let mut conditions = vec![
            "(handoff.status = 'pending' OR (handoff.status = 'leased' AND handoff.lease_expires_at <= ?))",
            "run.status NOT IN ('completed', 'failed', 'cancelled')",
        ];
        let mut values = vec![now_text.clone()];
        if let Some(handoff_id) = requested_handoff_id.as_ref() {
            conditions.push("handoff.handoff_id = ?");
            values.push(handoff_id.clone());
        }
        if let Some(executor) = executor.as_ref() {
            conditions.push("handoff.executor = ?");
            values.push(executor.clone());
        }
        let sql = format!(
            "SELECT handoff.* FROM sop_handoffs handoff JOIN sop_runs run ON run.run_id = handoff.run_id WHERE {} ORDER BY handoff.created_at, handoff.handoff_id LIMIT 1",
            conditions.join(" AND ")
        );
        let candidate = db
            .query_row(&sql, rusqlite::params_from_iter(values.iter()), row_json)
            .optional()
            .map_err(|error| {
                diagnostic("sop_handoff_query_failed", &error.to_string(), json!({}))
            })?;
        let Some(candidate) = candidate else {
            return Ok(json!({
                "schema":"narada.sop.handoff_claim.v1",
                "status":"empty",
                "handoff":null
            }));
        };
        let handoff_id = required_string(candidate.get("handoff_id"), "sop_handoff_corrupt", 512)?;
        let lease_token = Uuid::new_v4().to_string();
        let lease_expires_at = format_iso(now + Duration::milliseconds(lease_ms));
        let changes = db
            .execute(
                "UPDATE sop_handoffs SET status = 'leased', lease_owner = ?, lease_token = ?, lease_expires_at = ?, attempt_count = attempt_count + 1, last_error = CASE WHEN status = 'leased' THEN 'lease_expired' ELSE last_error END, updated_at = ? WHERE handoff_id = ? AND (status = 'pending' OR (status = 'leased' AND lease_expires_at <= ?))",
                params![consumer_id, lease_token, lease_expires_at, now_text, handoff_id, now_text],
            )
            .map_err(|error| diagnostic("sop_handoff_claim_failed", &error.to_string(), json!({})))?;
        if changes != 1 {
            return Err(diagnostic(
                "sop_handoff_claim_race",
                "sop_handoff_claim_race",
                json!({"handoff_id":handoff_id}),
            ));
        }
        let handoff = public_handoff(get_handoff(db, &handoff_id)?, true);
        Ok(json!({
            "schema":"narada.sop.handoff_claim.v1",
            "status":"claimed",
            "handoff":handoff,
            "lease_ms":lease_ms,
            "lease_remaining_ms":lease_ms,
            "next":{"tool":"sop_run_advance","required_from_claim":["handoff.handoff_id","handoff.run_id","handoff.step_id","handoff.lease_token"]}
        }))
    })
}

fn handoff_claim_and_advance(args: &Map<String, Value>, root: &Path) -> Result<Value, Value> {
    let claim = handoff_claim(args, root)?;
    if claim.get("status").and_then(Value::as_str) == Some("empty") {
        return Ok(
            json!({"schema":"narada.sop.handoff_claim_and_advance.v1","status":"empty","handoff":null,"advanced":false}),
        );
    }
    let handoff = claim.get("handoff").cloned().ok_or_else(|| {
        diagnostic(
            "sop_handoff_claim_response_corrupt",
            "sop_handoff_claim_response_corrupt",
            json!({}),
        )
    })?;
    let mut advance = Map::new();
    for (target, source) in [
        ("handoff_id", "handoff_id"),
        ("run_id", "run_id"),
        ("step_id", "step_id"),
        ("lease_token", "lease_token"),
    ] {
        advance.insert(
            target.to_string(),
            handoff.get(source).cloned().unwrap_or(Value::Null),
        );
    }
    for key in [
        "consumer_id",
        "completion_key",
        "outcome",
        "result",
        "result_ref",
        "error_message",
        "principal",
    ] {
        if let Some(value) = args.get(key) {
            advance.insert(key.to_string(), value.clone());
        }
    }
    match crate::sop_engine::call_tool("sop_run_advance", &advance, root) {
        Ok(result) => Ok(
            json!({"schema":"narada.sop.handoff_claim_and_advance.v1","status":"advanced","advanced":true,"claim":claim,"result":result}),
        ),
        Err(mut failure) => {
            let cleanup = handoff_release(
                &Map::from_iter([
                    ("handoff_id".into(), handoff["handoff_id"].clone()),
                    (
                        "consumer_id".into(),
                        args.get("consumer_id").cloned().unwrap_or(Value::Null),
                    ),
                    ("lease_token".into(), handoff["lease_token"].clone()),
                    (
                        "error_message".into(),
                        json!("compound_advance_failed_released"),
                    ),
                ]),
                root,
            );
            if let Some(object) = failure.as_object_mut() {
                object.insert(
                    "compound_cleanup".into(),
                    match cleanup {
                        Ok(_) => json!({"status":"released","handoff_id":handoff["handoff_id"]}),
                        Err(error) => json!({"status":"release_failed","error":error}),
                    },
                );
            }
            Err(failure)
        }
    }
}

fn handoff_renew(args: &Map<String, Value>, root: &Path) -> Result<Value, Value> {
    transactional(root, |db| {
        let now = OffsetDateTime::now_utc();
        let handoff = require_lease(db, args, now, false)?;
        let lease_ms = bounded_integer_arg(
            args.get("lease_ms"),
            60_000,
            MIN_LEASE_MS,
            MAX_LEASE_MS,
            "sop_handoff_lease_ms_invalid",
        )?;
        let handoff_id = handoff
            .get("handoff_id")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let lease_expires_at = format_iso(now + Duration::milliseconds(lease_ms));
        db.execute(
            "UPDATE sop_handoffs SET lease_expires_at = ?, updated_at = ? WHERE handoff_id = ?",
            params![lease_expires_at, format_iso(now), handoff_id],
        )
        .map_err(|error| diagnostic("sop_handoff_renew_failed", &error.to_string(), json!({})))?;
        let mut handoff = public_handoff(get_handoff(db, handoff_id)?, true);
        handoff["lease_ms"] = json!(lease_ms);
        handoff["lease_remaining_ms"] = json!(lease_ms);
        Ok(handoff)
    })
}

fn handoff_release(args: &Map<String, Value>, root: &Path) -> Result<Value, Value> {
    transactional(root, |db| {
        let now = OffsetDateTime::now_utc();
        let handoff = require_lease(db, args, now, true)?;
        let handoff_id = handoff
            .get("handoff_id")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let error_message = optional_bounded_string(
            args.get("error_message"),
            "sop_handoff_error_message_too_long",
            4096,
        )?;
        db.execute(
            "UPDATE sop_handoffs SET status = 'pending', lease_owner = NULL, lease_token = NULL, lease_expires_at = NULL, last_error = ?, updated_at = ? WHERE handoff_id = ?",
            params![error_message, format_iso(now), handoff_id],
        )
        .map_err(|error| diagnostic("sop_handoff_release_failed", &error.to_string(), json!({})))?;
        Ok(public_handoff(get_handoff(db, handoff_id)?, false))
    })
}

fn require_lease(
    db: &Connection,
    args: &Map<String, Value>,
    now: OffsetDateTime,
    allow_expired: bool,
) -> Result<Value, Value> {
    let handoff_id = required_string(args.get("handoff_id"), "sop_handoff_id_required", 512)?;
    let consumer_id = required_string(
        args.get("consumer_id"),
        "sop_handoff_consumer_id_required",
        512,
    )?;
    let lease_token = required_string(
        args.get("lease_token"),
        "sop_handoff_lease_token_required",
        512,
    )?;
    let handoff = get_handoff(db, &handoff_id)?;
    let status = handoff
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if status != "leased" {
        return Err(diagnostic(
            "sop_handoff_not_leased",
            "sop_handoff_not_leased",
            json!({"handoff_id":handoff_id,"status":status}),
        ));
    }
    if handoff.get("lease_owner").and_then(Value::as_str) != Some(consumer_id.as_str())
        || handoff.get("lease_token").and_then(Value::as_str) != Some(lease_token.as_str())
    {
        return Err(diagnostic(
            "sop_handoff_lease_mismatch",
            "sop_handoff_lease_mismatch",
            json!({"handoff_id":handoff_id,"lease_owner":handoff.get("lease_owner")}),
        ));
    }
    if !allow_expired {
        let lease_expires_at = handoff
            .get("lease_expires_at")
            .and_then(Value::as_str)
            .ok_or_else(|| {
                diagnostic(
                    "sop_handoff_lease_corrupt",
                    "sop_handoff_lease_corrupt",
                    json!({"handoff_id":handoff_id}),
                )
            })?;
        let expires = parse_iso(lease_expires_at).ok_or_else(|| {
            diagnostic(
                "sop_handoff_lease_corrupt",
                "sop_handoff_lease_corrupt",
                json!({"handoff_id":handoff_id}),
            )
        })?;
        if expires <= now {
            return Err(diagnostic(
                "sop_handoff_lease_expired",
                "sop_handoff_lease_expired",
                json!({"handoff_id":handoff_id,"lease_expires_at":lease_expires_at,"recovery":{"tool":"sop_handoff_claim","arguments":{"handoff_id":handoff_id,"consumer_id":consumer_id},"guidance":"Reclaim the expired handoff with a new lease token; the stale token cannot commit."}}),
            ));
        }
    }
    Ok(handoff)
}

pub(crate) fn get_handoff(db: &Connection, handoff_id: &str) -> Result<Value, Value> {
    let row = db
        .query_row(
            "SELECT * FROM sop_handoffs WHERE handoff_id = ?",
            params![handoff_id],
            row_json,
        )
        .optional()
        .map_err(|error| diagnostic("sop_handoff_query_failed", &error.to_string(), json!({})))?
        .ok_or_else(|| {
            diagnostic(
                "sop_handoff_not_found",
                "sop_handoff_not_found",
                json!({"handoff_id":handoff_id}),
            )
        })?;
    hydrate_handoff(row)
}

pub(crate) fn hydrate_handoff(row: Value) -> Result<Value, Value> {
    let object = row
        .as_object()
        .ok_or_else(|| diagnostic("sop_handoff_corrupt", "sop_handoff_corrupt", json!({})))?;
    let run_id = required_string(object.get("run_id"), "sop_handoff_corrupt", 512)?;
    let step_id = required_string(object.get("step_id"), "sop_handoff_corrupt", 512)?;
    let handoff_id = required_string(object.get("handoff_id"), "sop_handoff_corrupt", 512)?;
    let occurrence_key = required_string(object.get("occurrence_key"), "sop_handoff_corrupt", 512)?;
    let identity = format!("{run_id}\0{step_id}");
    let expected_handoff_id = deterministic_id("soh_", &identity);
    let expected_occurrence_key = deterministic_id("sop_handoff_", &identity);
    if handoff_id != expected_handoff_id || occurrence_key != expected_occurrence_key {
        return Err(diagnostic(
            "sop_handoff_identity_mismatch",
            "sop_handoff_identity_mismatch",
            json!({"handoff_id":handoff_id,"expected_handoff_id":expected_handoff_id}),
        ));
    }
    let sop_id = required_string(object.get("sop_id"), "sop_handoff_corrupt", 512)?;
    let sop_version = positive_integer_member(object.get("sop_version"), "sop_handoff_corrupt")?;
    let executor = normalize_handoff_executor(
        object
            .get("executor")
            .and_then(Value::as_str)
            .unwrap_or_default(),
    )?;
    let title = required_string(object.get("title"), "sop_handoff_corrupt", 512)?;
    let instructions =
        required_string(object.get("instructions"), "sop_handoff_corrupt", 16 * 1024)?;
    let input = object.get("input_json").cloned().unwrap_or(Value::Null);
    let input_ref = object.get("input_ref_json").cloned().unwrap_or(Value::Null);
    let result_schema = object
        .get("result_schema_json")
        .cloned()
        .unwrap_or(Value::Null);
    let request_fingerprint = required_string(
        object.get("request_fingerprint"),
        "sop_handoff_corrupt",
        512,
    )?;
    let actual_request_fingerprint = fingerprint(&json!({
        "run_id":run_id,"step_id":step_id,"sop_id":sop_id,"sop_version":sop_version,
        "executor":executor,"title":title,"instructions":instructions,"input":input,
        "input_ref":input_ref,"result_schema":result_schema
    }));
    if request_fingerprint != actual_request_fingerprint {
        return Err(diagnostic(
            "sop_handoff_request_fingerprint_mismatch",
            "sop_handoff_request_fingerprint_mismatch",
            json!({"handoff_id":handoff_id}),
        ));
    }
    let status = normalize_handoff_status(
        object
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or_default(),
    )?;
    let lease_owner = optional_string(object.get("lease_owner"));
    let lease_token = optional_string(object.get("lease_token"));
    let lease_expires_at = optional_string(object.get("lease_expires_at"));
    if status == "leased" {
        if lease_owner.is_none() || lease_token.is_none() || lease_expires_at.is_none() {
            return Err(diagnostic(
                "sop_handoff_lease_corrupt",
                "sop_handoff_lease_corrupt",
                json!({"handoff_id":handoff_id}),
            ));
        }
    } else if lease_owner.is_some() || lease_token.is_some() || lease_expires_at.is_some() {
        return Err(diagnostic(
            "sop_handoff_lease_corrupt",
            "sop_handoff_lease_corrupt",
            json!({"handoff_id":handoff_id,"status":status}),
        ));
    }
    let attempt_count = nonnegative_integer_member(
        object.get("attempt_count"),
        "sop_handoff_attempt_count_invalid",
    )?;
    let completion_key = optional_string(object.get("completion_key"));
    let completion_fingerprint = optional_string(object.get("completion_fingerprint"));
    let principal = optional_string(object.get("principal"));
    let result = object
        .get("result_json")
        .cloned()
        .unwrap_or_else(|| json!({}));
    if !result.is_object() {
        return Err(diagnostic(
            "sop_handoff_result_corrupt",
            "sop_handoff_result_corrupt",
            json!({"handoff_id":handoff_id}),
        ));
    }
    let result_ref = object
        .get("result_ref_json")
        .cloned()
        .unwrap_or(Value::Null);
    let error_message = optional_string(object.get("error_message"));
    if let Some(recorded) = completion_fingerprint.as_ref() {
        if completion_key.is_none()
            || principal.is_none()
            || !matches!(status.as_str(), "completed" | "failed")
        {
            return Err(diagnostic(
                "sop_handoff_completion_identity_invalid",
                "sop_handoff_completion_identity_invalid",
                json!({"handoff_id":handoff_id,"status":status}),
            ));
        }
        let actual = fingerprint(&json!({
            "completion_key":completion_key,"outcome":status,"principal":principal,
            "result":result,"result_ref":result_ref,"error_message":error_message
        }));
        if recorded != &actual {
            return Err(diagnostic(
                "sop_handoff_completion_fingerprint_mismatch",
                "sop_handoff_completion_fingerprint_mismatch",
                json!({"handoff_id":handoff_id}),
            ));
        }
    } else if completion_key.is_some()
        || principal.is_some()
        || matches!(status.as_str(), "completed" | "failed")
    {
        return Err(diagnostic(
            "sop_handoff_completion_identity_invalid",
            "sop_handoff_completion_identity_invalid",
            json!({"handoff_id":handoff_id,"status":status}),
        ));
    }
    Ok(json!({
        "schema":"narada.sop.handoff.v1","handoff_id":handoff_id,"run_id":run_id,
        "step_id":step_id,"occurrence_key":occurrence_key,"sop_id":sop_id,
        "sop_version":sop_version,"executor":executor,"title":title,
        "instructions":instructions,"input":input,"input_ref":input_ref,
        "result_schema":result_schema,"request_fingerprint":request_fingerprint,
        "status":status,"lease_owner":lease_owner,"lease_token":lease_token,
        "lease_expires_at":lease_expires_at,"attempt_count":attempt_count,
        "last_error":optional_string(object.get("last_error")),"completion_key":completion_key,
        "completion_fingerprint":completion_fingerprint,"principal":principal,"result":result,
        "result_ref":result_ref,"error_message":error_message,
        "created_at":required_string(object.get("created_at"),"sop_handoff_corrupt",512)?,
        "updated_at":required_string(object.get("updated_at"),"sop_handoff_corrupt",512)?,
        "completed_at":optional_string(object.get("completed_at"))
    }))
}

pub(crate) fn public_handoff(mut handoff: Value, include_lease_token: bool) -> Value {
    if !include_lease_token {
        if let Some(object) = handoff.as_object_mut() {
            object.remove("lease_token");
        }
    }
    handoff
}

fn outbox_consumer_register(args: &Map<String, Value>, root: &Path) -> Result<Value, Value> {
    transactional(root, |db| {
        let topic = normalize_outbox_topic(
            optional_string(args.get("topic"))
                .as_deref()
                .unwrap_or(SOP_TERMINAL_TOPIC),
        )?;
        let consumer_id = required_string(
            args.get("consumer_id"),
            "sop_outbox_consumer_id_required",
            512,
        )?;
        let now = OffsetDateTime::now_utc();
        let start_at = match optional_string(args.get("start_at")) {
            Some(value) => normalize_timestamp(&value, "sop_outbox_start_at_invalid")?,
            None => format_iso(now),
        };
        let existing = db
            .query_row(
                "SELECT * FROM sop_outbox_consumer_requirements WHERE topic = ? AND consumer_id = ?",
                params![topic, consumer_id],
                row_json,
            )
            .optional()
            .map_err(|error| {
                diagnostic(
                    "sop_outbox_consumer_query_failed",
                    &error.to_string(),
                    json!({}),
                )
            })?;
        if let Some(existing) = existing {
            let recorded_start_at = existing
                .get("start_at")
                .and_then(Value::as_str)
                .unwrap_or_default();
            if recorded_start_at != start_at {
                return Err(diagnostic(
                    "sop_outbox_consumer_registration_conflict",
                    "sop_outbox_consumer_registration_conflict",
                    json!({
                        "topic":topic,"consumer_id":consumer_id,
                        "recorded_start_at":recorded_start_at,"supplied_start_at":start_at
                    }),
                ));
            }
            return Ok(json!({
                "schema":"narada.sop.outbox_consumer.v1",
                "topic":existing.get("topic").cloned().unwrap_or(Value::Null),
                "consumer_id":existing.get("consumer_id").cloned().unwrap_or(Value::Null),
                "start_at":existing.get("start_at").cloned().unwrap_or(Value::Null),
                "registered_at":existing.get("registered_at").cloned().unwrap_or(Value::Null),
                "registration_replayed":true
            }));
        }
        let compacted = db
            .query_row(
                "SELECT event_id, created_at FROM sop_outbox WHERE topic = ? AND created_at >= ? AND compacted_at IS NOT NULL ORDER BY created_at LIMIT 1",
                params![topic, start_at],
                row_json,
            )
            .optional()
            .map_err(|error| diagnostic("sop_outbox_query_failed", &error.to_string(), json!({})))?;
        if let Some(compacted) = compacted {
            return Err(diagnostic(
                "sop_outbox_registration_history_compacted",
                "sop_outbox_registration_history_compacted",
                json!({
                    "topic":topic,"consumer_id":consumer_id,"start_at":start_at,
                    "first_compacted_event_id":compacted.get("event_id"),
                    "first_compacted_event_created_at":compacted.get("created_at")
                }),
            ));
        }
        let registered_at = format_iso(now);
        db.execute(
            "INSERT INTO sop_outbox_consumer_requirements(topic, consumer_id, start_at, registered_at) VALUES (?, ?, ?, ?)",
            params![topic, consumer_id, start_at, registered_at],
        )
        .map_err(|error| diagnostic("sop_outbox_consumer_insert_failed", &error.to_string(), json!({})))?;
        Ok(json!({
            "schema":"narada.sop.outbox_consumer.v1","topic":topic,
            "consumer_id":consumer_id,"start_at":start_at,"registered_at":registered_at,
            "registration_replayed":false
        }))
    })
}

fn outbox_ack(args: &Map<String, Value>, root: &Path) -> Result<Value, Value> {
    let receipt = args.get("receipt").cloned().ok_or_else(|| {
        diagnostic(
            "sop_outbox_receipt_must_be_object",
            "sop_outbox_receipt_must_be_object",
            json!({}),
        )
    })?;
    if !receipt.is_object() {
        return Err(diagnostic(
            "sop_outbox_receipt_must_be_object",
            "sop_outbox_receipt_must_be_object",
            json!({}),
        ));
    }
    assert_bound(&receipt, "sop_outbox_receipt", MAX_OUTBOX_RECEIPT_BYTES)?;
    transactional(root, |db| {
        let event_id = required_string(args.get("event_id"), "sop_outbox_event_id_required", 512)?;
        let consumer_id = required_string(
            args.get("consumer_id"),
            "sop_outbox_consumer_id_required",
            512,
        )?;
        let receipt_json = canonical_json(&receipt);
        let event = require_outbox_event(db, &event_id)?;
        let topic = event
            .get("topic")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let requirement = db
            .query_row(
                "SELECT * FROM sop_outbox_consumer_requirements WHERE topic = ? AND consumer_id = ?",
                params![topic, consumer_id],
                row_json,
            )
            .optional()
            .map_err(|error| {
                diagnostic(
                    "sop_outbox_consumer_query_failed",
                    &error.to_string(),
                    json!({}),
                )
            })?
            .ok_or_else(|| {
                diagnostic(
                    "sop_outbox_consumer_not_registered",
                    "sop_outbox_consumer_not_registered",
                    json!({"consumer_id":consumer_id,"topic":topic}),
                )
            })?;
        let event_created_at = event
            .get("created_at")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let start_at = requirement
            .get("start_at")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if event_created_at < start_at {
            return Err(diagnostic(
                "sop_outbox_event_before_consumer_start",
                "sop_outbox_event_before_consumer_start",
                json!({"event_id":event_id,"consumer_id":consumer_id,"start_at":start_at}),
            ));
        }
        let existing = db
            .query_row(
                "SELECT * FROM sop_outbox_receipts WHERE event_id = ? AND consumer_id = ?",
                params![event_id, consumer_id],
                row_json,
            )
            .optional()
            .map_err(|error| {
                diagnostic(
                    "sop_outbox_receipt_query_failed",
                    &error.to_string(),
                    json!({}),
                )
            })?;
        if let Some(existing) = existing {
            let recorded = existing.get("receipt_json").cloned().unwrap_or(Value::Null);
            if !recorded.is_object() {
                return Err(diagnostic(
                    "sop_outbox_receipt_corrupt",
                    "sop_outbox_receipt_corrupt",
                    json!({"event_id":event_id,"consumer_id":consumer_id}),
                ));
            }
            if canonical_json(&recorded) != receipt_json {
                return Err(diagnostic(
                    "sop_outbox_receipt_conflict",
                    "sop_outbox_receipt_conflict",
                    json!({"event_id":event_id,"consumer_id":consumer_id}),
                ));
            }
            return Ok(json!({
                "schema":"narada.sop.outbox_ack.v1","event_id":event_id,
                "consumer_id":consumer_id,
                "processed_at":existing.get("processed_at").cloned().unwrap_or(Value::Null),
                "acknowledgement_replayed":true
            }));
        }
        let processed_at = now_iso();
        db.execute(
            "INSERT INTO sop_outbox_receipts(event_id, consumer_id, processed_at, receipt_json) VALUES (?, ?, ?, ?)",
            params![event_id, consumer_id, processed_at, receipt_json],
        )
        .map_err(|error| diagnostic("sop_outbox_receipt_insert_failed", &error.to_string(), json!({})))?;
        Ok(json!({
            "schema":"narada.sop.outbox_ack.v1","event_id":event_id,
            "consumer_id":consumer_id,"processed_at":processed_at,
            "acknowledgement_replayed":false
        }))
    })
}

fn outbox_compact(args: &Map<String, Value>, root: &Path) -> Result<Value, Value> {
    transactional(root, |db| {
        let before = required_string(
            args.get("before"),
            "sop_outbox_compact_before_required",
            512,
        )?;
        let cutoff = normalize_timestamp(&before, "sop_outbox_compact_before_invalid")?;
        let compacted_at = now_iso();
        let compacted = db
            .execute(
                "UPDATE sop_outbox AS outbox SET payload_json = '{}', compacted_at = ? WHERE outbox.compacted_at IS NULL AND outbox.created_at < ? AND EXISTS (SELECT 1 FROM sop_outbox_consumer_requirements requirement WHERE requirement.topic = outbox.topic AND requirement.start_at <= outbox.created_at) AND NOT EXISTS (SELECT 1 FROM sop_outbox_consumer_requirements requirement WHERE requirement.topic = outbox.topic AND requirement.start_at <= outbox.created_at AND NOT EXISTS (SELECT 1 FROM sop_outbox_receipts receipt WHERE receipt.event_id = outbox.event_id AND receipt.consumer_id = requirement.consumer_id))",
                params![compacted_at, cutoff],
            )
            .map_err(|error| diagnostic("sop_outbox_compaction_failed", &error.to_string(), json!({})))?;
        Ok(json!({
            "schema":"narada.sop.outbox_compaction.v1","before":cutoff,
            "compacted_at":compacted_at,"compacted":compacted
        }))
    })
}

pub(crate) fn require_outbox_event(db: &Connection, event_id: &str) -> Result<Value, Value> {
    let row = db
        .query_row(
            "SELECT * FROM sop_outbox WHERE event_id = ?",
            params![event_id],
            row_json,
        )
        .optional()
        .map_err(|error| diagnostic("sop_outbox_query_failed", &error.to_string(), json!({})))?
        .ok_or_else(|| {
            diagnostic(
                "sop_outbox_event_not_found",
                "sop_outbox_event_not_found",
                json!({"event_id":event_id}),
            )
        })?;
    hydrate_outbox_event(row)
}

pub(crate) fn hydrate_outbox_event(row: Value) -> Result<Value, Value> {
    let object = row.as_object().ok_or_else(|| {
        diagnostic(
            "sop_outbox_event_corrupt",
            "sop_outbox_event_corrupt",
            json!({}),
        )
    })?;
    let outcome = required_string(object.get("outcome"), "sop_outbox_event_corrupt", 64)?;
    if !matches!(outcome.as_str(), "completed" | "failed" | "cancelled") {
        return Err(diagnostic(
            "sop_outbox_outcome_corrupt",
            "sop_outbox_outcome_corrupt",
            json!({"outcome":outcome}),
        ));
    }
    let topic = normalize_outbox_topic(
        object
            .get("topic")
            .and_then(Value::as_str)
            .unwrap_or_default(),
    )?;
    let payload = object.get("payload_json").cloned().unwrap_or(Value::Null);
    if !payload.is_object() {
        return Err(diagnostic(
            "sop_outbox_payload_corrupt",
            "sop_outbox_payload_corrupt",
            json!({}),
        ));
    }
    assert_bound(&payload, "sop_outbox_payload", MAX_OUTBOX_PAYLOAD_BYTES)?;
    Ok(json!({
        "schema":"narada.sop.outbox_event.v1",
        "event_id":required_string(object.get("event_id"),"sop_outbox_event_corrupt",512)?,
        "topic":topic,
        "partition_key":required_string(object.get("partition_key"),"sop_outbox_event_corrupt",512)?,
        "run_id":required_string(object.get("run_id"),"sop_outbox_event_corrupt",512)?,
        "sop_id":required_string(object.get("sop_id"),"sop_outbox_event_corrupt",512)?,
        "sop_version":positive_integer_member(object.get("sop_version"),"sop_outbox_event_corrupt")?,
        "occurrence_key":required_string(object.get("occurrence_key"),"sop_outbox_event_corrupt",512)?,
        "outcome":outcome,"payload":payload,
        "created_at":required_string(object.get("created_at"),"sop_outbox_event_corrupt",512)?,
        "available_at":required_string(object.get("available_at"),"sop_outbox_event_corrupt",512)?,
        "compacted_at":optional_string(object.get("compacted_at"))
    }))
}

pub(crate) fn transactional<F>(root: &Path, work: F) -> Result<Value, Value>
where
    F: FnOnce(&Connection) -> Result<Value, Value>,
{
    let mut db = open_db(root)?;
    let transaction = db
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|error| {
            diagnostic(
                "sop_transaction_begin_failed",
                &error.to_string(),
                json!({}),
            )
        })?;
    match work(&transaction) {
        Ok(value) => {
            transaction.commit().map_err(|error| {
                diagnostic(
                    "sop_transaction_commit_failed",
                    &error.to_string(),
                    json!({}),
                )
            })?;
            Ok(value)
        }
        Err(error) => {
            let _ = transaction.rollback();
            Err(error)
        }
    }
}

pub(crate) fn row_json(row: &rusqlite::Row<'_>) -> rusqlite::Result<Value> {
    let mut object = Map::new();
    for index in 0..row.as_ref().column_count() {
        let name = row
            .as_ref()
            .column_name(index)
            .unwrap_or("column")
            .to_string();
        let value = match row.get_ref(index)? {
            rusqlite::types::ValueRef::Null => Value::Null,
            rusqlite::types::ValueRef::Integer(value) => json!(value),
            rusqlite::types::ValueRef::Real(value) => json!(value),
            rusqlite::types::ValueRef::Text(value) => {
                let text = String::from_utf8_lossy(value).to_string();
                if name.ends_with("_json") {
                    serde_json::from_str(&text).unwrap_or(Value::String(text))
                } else {
                    Value::String(text)
                }
            }
            rusqlite::types::ValueRef::Blob(value) => json!({"byte_length":value.len()}),
        };
        object.insert(name, value);
    }
    Ok(Value::Object(object))
}

fn normalize_handoff_executor(value: &str) -> Result<String, Value> {
    if !matches!(value, "agent" | "operator") {
        return Err(diagnostic(
            "sop_handoff_executor_invalid",
            "sop_handoff_executor_invalid",
            json!({"executor":value}),
        ));
    }
    Ok(value.to_string())
}

fn normalize_handoff_status(value: &str) -> Result<String, Value> {
    if !matches!(
        value,
        "pending" | "leased" | "completed" | "failed" | "cancelled"
    ) {
        return Err(diagnostic(
            "sop_handoff_status_invalid",
            "sop_handoff_status_invalid",
            json!({"status":value}),
        ));
    }
    Ok(value.to_string())
}

fn normalize_outbox_topic(value: &str) -> Result<String, Value> {
    let topic = value.trim();
    if topic.is_empty() {
        return Err(diagnostic(
            "sop_outbox_topic_required",
            "sop_outbox_topic_required",
            json!({}),
        ));
    }
    if topic.chars().count() > 256 {
        return Err(diagnostic(
            "sop_outbox_topic_required_too_long",
            "sop_outbox_topic_required_too_long",
            json!({"max_length":256}),
        ));
    }
    if topic != SOP_TERMINAL_TOPIC {
        return Err(diagnostic(
            "sop_outbox_topic_unsupported",
            "sop_outbox_topic_unsupported",
            json!({"topic":topic,"allowed":[SOP_TERMINAL_TOPIC]}),
        ));
    }
    Ok(topic.to_string())
}

fn bounded_integer_arg(
    value: Option<&Value>,
    default: i64,
    min: i64,
    max: i64,
    code: &str,
) -> Result<i64, Value> {
    let parsed = match value {
        None | Some(Value::Null) => default,
        Some(value) => value
            .as_i64()
            .ok_or_else(|| diagnostic(code, code, json!({"value":value,"min":min,"max":max})))?,
    };
    if parsed < min || parsed > max {
        return Err(diagnostic(
            code,
            code,
            json!({"value":value,"min":min,"max":max}),
        ));
    }
    Ok(parsed)
}

fn positive_integer_member(value: Option<&Value>, code: &str) -> Result<i64, Value> {
    let parsed = value
        .and_then(Value::as_i64)
        .ok_or_else(|| diagnostic(code, code, json!({})))?;
    if parsed < 1 {
        return Err(diagnostic(code, code, json!({"value":parsed,"min":1})));
    }
    Ok(parsed)
}

fn nonnegative_integer_member(value: Option<&Value>, code: &str) -> Result<i64, Value> {
    let parsed = value
        .and_then(Value::as_i64)
        .ok_or_else(|| diagnostic(code, code, json!({})))?;
    if parsed < 0 {
        return Err(diagnostic(code, code, json!({"value":parsed,"min":0})));
    }
    Ok(parsed)
}

pub(crate) fn optional_bounded_string(
    value: Option<&Value>,
    code: &str,
    max: usize,
) -> Result<Option<String>, Value> {
    let Some(text) = optional_string(value) else {
        return Ok(None);
    };
    if text.chars().count() > max {
        return Err(diagnostic(
            code,
            code,
            json!({"length":text.chars().count(),"max_length":max}),
        ));
    }
    Ok(Some(text))
}

fn normalize_timestamp(value: &str, code: &str) -> Result<String, Value> {
    parse_iso(value)
        .map(format_iso)
        .ok_or_else(|| diagnostic(code, code, json!({"value":value})))
}

pub(crate) fn parse_iso(value: &str) -> Option<OffsetDateTime> {
    OffsetDateTime::parse(value.trim(), &Rfc3339)
        .ok()
        .map(|timestamp| timestamp.to_offset(UtcOffset::UTC))
}

pub(crate) fn format_iso(value: OffsetDateTime) -> String {
    let value = value.to_offset(UtcOffset::UTC);
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:03}Z",
        value.year(),
        u8::from(value.month()),
        value.day(),
        value.hour(),
        value.minute(),
        value.second(),
        value.nanosecond() / 1_000_000
    )
}

#[allow(clippy::too_many_arguments)]
fn insert_template(
    db: &Connection,
    sop_id: &str,
    version: i64,
    title: &str,
    status: &str,
    description: &str,
    steps: &Value,
    trigger_kind: &str,
    input_schema: Option<&Value>,
    output: Option<&Value>,
    output_ref: Option<&Value>,
    output_schema: Option<&Value>,
    acceptance: &[Value],
    evidence: &[Value],
    now: &str,
) -> Result<(), Value> {
    db.execute(
        "INSERT INTO sop_templates (sop_id,version,title,status,description,steps_json,trigger_kind,input_schema_json,output_mapping_json,output_ref_mapping_json,output_schema_json,acceptance_criteria_json,evidence_requirements_json,created_at,updated_at) VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)",
        params![
            sop_id, version, title, status, description, encode(steps)?, trigger_kind,
            encode_optional(input_schema)?, encode_optional(output)?, encode_optional(output_ref)?,
            encode_optional(output_schema)?, encode(&Value::Array(acceptance.to_vec()))?,
            encode(&Value::Array(evidence.to_vec()))?, now, now
        ],
    )
    .map_err(|error| diagnostic("sop_template_insert_failed", &error.to_string(), json!({"sop_id":sop_id,"version":version})))?;
    Ok(())
}

fn latest_template(db: &Connection, sop_id: &str) -> Result<Option<Value>, Value> {
    query_template(
        db,
        "SELECT * FROM sop_templates WHERE sop_id = ? ORDER BY version DESC LIMIT 1",
        params![sop_id],
    )
}

fn template_by_version(
    db: &Connection,
    sop_id: &str,
    version: i64,
) -> Result<Option<Value>, Value> {
    query_template(
        db,
        "SELECT * FROM sop_templates WHERE sop_id = ? AND version = ? LIMIT 1",
        params![sop_id, version],
    )
}

fn query_template<P: rusqlite::Params>(
    db: &Connection,
    sql: &str,
    params: P,
) -> Result<Option<Value>, Value> {
    db.query_row(sql, params, row_value)
        .optional()
        .map_err(|error| diagnostic("sop_template_query_failed", &error.to_string(), json!({})))
}

fn row_value(row: &rusqlite::Row<'_>) -> rusqlite::Result<Value> {
    let mut object = Map::new();
    for index in 0..row.as_ref().column_count() {
        let name = row
            .as_ref()
            .column_name(index)
            .unwrap_or("column")
            .to_string();
        let value = match row.get_ref(index)? {
            rusqlite::types::ValueRef::Null => Value::Null,
            rusqlite::types::ValueRef::Integer(value) => json!(value),
            rusqlite::types::ValueRef::Real(value) => json!(value),
            rusqlite::types::ValueRef::Text(value) => {
                Value::String(String::from_utf8_lossy(value).to_string())
            }
            rusqlite::types::ValueRef::Blob(value) => json!({"byte_length":value.len()}),
        };
        object.insert(name, value);
    }
    Ok(Value::Object(object))
}

fn parse_yaml_template(path: &Path, expected_sop_id: &str) -> Result<Value, Value> {
    let metadata = fs::metadata(path).map_err(|error| {
        diagnostic(
            "sop_yaml_read_error",
            &format!("sop_yaml_read_error:{expected_sop_id}"),
            json!({"yaml_path":path,"message":error.to_string()}),
        )
    })?;
    if metadata.len() > MAX_TEMPLATE_FILE_BYTES {
        return Err(diagnostic(
            "sop_yaml_too_large",
            &format!("sop_yaml_too_large:{expected_sop_id}"),
            json!({"yaml_path":path,"byte_length":metadata.len(),"max_bytes":MAX_TEMPLATE_FILE_BYTES}),
        ));
    }
    let raw = fs::read_to_string(path).map_err(|error| {
        diagnostic(
            "sop_yaml_read_error",
            &format!("sop_yaml_read_error:{expected_sop_id}"),
            json!({"yaml_path":path,"message":error.to_string()}),
        )
    })?;
    let document: Value = yaml_serde::from_str(&raw).map_err(|error| {
        diagnostic(
            "sop_yaml_parse_error",
            &format!("sop_yaml_parse_error:{expected_sop_id}"),
            json!({"yaml_path":path,"message":error.to_string()}),
        )
    })?;
    let schema: Value = serde_json::from_str(TEMPLATE_SCHEMA)
        .map_err(|error| diagnostic("sop_schema_load_failed", &error.to_string(), json!({})))?;
    let validator = validator_for(&schema)
        .map_err(|error| diagnostic("sop_schema_load_failed", &error.to_string(), json!({})))?;
    let schema_errors = validator
        .iter_errors(&document)
        .take(20)
        .map(|error| error.to_string())
        .collect::<Vec<_>>();
    if !schema_errors.is_empty() {
        return Err(diagnostic(
            "sop_yaml_schema_error",
            &format!("sop_yaml_schema_error:{expected_sop_id}"),
            json!({"yaml_path":path,"errors":schema_errors.join("; ")}),
        ));
    }
    let object = document.as_object().ok_or_else(|| {
        diagnostic(
            "sop_yaml_schema_error",
            &format!("sop_yaml_schema_error:{expected_sop_id}"),
            json!({"yaml_path":path,"errors":"(root) must be object"}),
        )
    })?;
    let yaml_sop_id = required_string(object.get("sop_id"), "sop_yaml_requires_sop_id", 512)?;
    if yaml_sop_id != expected_sop_id {
        return Err(diagnostic(
            "sop_yaml_id_mismatch",
            &format!("sop_yaml_id_mismatch:arg={expected_sop_id} yaml={yaml_sop_id}"),
            json!({"yaml_path":path}),
        ));
    }
    let title = required_string(object.get("title"), "sop_yaml_requires_title", 512)?;
    let description = optional_string(object.get("description")).unwrap_or_default();
    let trigger_kind = normalize_trigger(object.get("trigger_kind"))?;
    let status = normalize_template_status(object.get("status"))?;
    let steps = validate_steps(object.get("steps"), Some(&yaml_sop_id))?;
    let input_schema = optional_schema(object.get("input_schema"), "input_schema")?;
    let output = optional_value(object.get("output"), "output")?;
    let output_ref = optional_value(object.get("output_ref"), "output_ref")?;
    let output_schema = optional_schema(object.get("output_schema"), "output_schema")?;
    validate_output_references(output.as_ref(), &steps)?;
    validate_output_references(output_ref.as_ref(), &steps)?;
    let acceptance = string_list(object.get("acceptance_criteria"))?;
    let evidence = string_list(object.get("evidence_requirements"))?;
    let normalized = json!({
        "sop_id":yaml_sop_id,"title":title,"description":description,"trigger_kind":trigger_kind,
        "status":status,"steps":steps,"input_schema":input_schema,"output":output,
        "output_ref":output_ref,"output_schema":output_schema,"acceptance_criteria":acceptance,
        "evidence_requirements":evidence
    });
    assert_template_bound(&normalized)?;
    Ok(normalized)
}

fn template_matches(current: &Value, next: &Value) -> Result<bool, Value> {
    let current = current
        .as_object()
        .ok_or_else(|| diagnostic("sop_template_corrupt", "sop_template_corrupt", json!({})))?;
    let next = next.as_object().expect("normalized template");
    let comparisons = [
        (
            Value::String(text_member(current, "title")),
            next.get("title").cloned().unwrap_or(Value::Null),
        ),
        (
            Value::String(text_member(current, "status")),
            next.get("status").cloned().unwrap_or(Value::Null),
        ),
        (
            Value::String(text_member(current, "description")),
            next.get("description").cloned().unwrap_or(Value::Null),
        ),
        (
            Value::String(text_member(current, "trigger_kind")),
            next.get("trigger_kind").cloned().unwrap_or(Value::Null),
        ),
        (
            parse_json_member(current, "steps_json", json!([]))?,
            next.get("steps").cloned().unwrap_or(Value::Null),
        ),
        (
            parse_nullable_member(current, "input_schema_json")?.unwrap_or(Value::Null),
            next.get("input_schema").cloned().unwrap_or(Value::Null),
        ),
        (
            parse_nullable_member(current, "output_mapping_json")?.unwrap_or(Value::Null),
            next.get("output").cloned().unwrap_or(Value::Null),
        ),
        (
            parse_nullable_member(current, "output_ref_mapping_json")?.unwrap_or(Value::Null),
            next.get("output_ref").cloned().unwrap_or(Value::Null),
        ),
        (
            parse_nullable_member(current, "output_schema_json")?.unwrap_or(Value::Null),
            next.get("output_schema").cloned().unwrap_or(Value::Null),
        ),
        (
            parse_json_member(current, "acceptance_criteria_json", json!([]))?,
            next.get("acceptance_criteria")
                .cloned()
                .unwrap_or(Value::Null),
        ),
        (
            parse_json_member(current, "evidence_requirements_json", json!([]))?,
            next.get("evidence_requirements")
                .cloned()
                .unwrap_or(Value::Null),
        ),
    ];
    Ok(comparisons.into_iter().all(|(left, right)| left == right))
}

fn validate_steps(value: Option<&Value>, owner_sop_id: Option<&str>) -> Result<Value, Value> {
    let raw = value
        .and_then(Value::as_array)
        .ok_or_else(|| diagnostic("sop_requires_array", "sop_requires_array", json!({})))?;
    if raw.is_empty() || raw.len() > MAX_STEPS {
        return Err(diagnostic(
            "sop_step_count_invalid",
            "sop_step_count_invalid",
            json!({"count":raw.len(),"min":1,"max":MAX_STEPS}),
        ));
    }
    let mut ids = HashSet::new();
    let mut normalized = Vec::with_capacity(raw.len());
    for (index, value) in raw.iter().enumerate() {
        let step = value.as_object().ok_or_else(|| {
            diagnostic(
                "sop_array_entry_must_be_object",
                "sop_array_entry_must_be_object",
                json!({"index":index}),
            )
        })?;
        let id = required_string(step.get("id"), "sop_step_requires_id", 128)?;
        if !valid_step_id(&id) {
            return Err(diagnostic(
                "sop_step_id_invalid",
                &format!("sop_step_id_invalid:{id}"),
                json!({}),
            ));
        }
        if !ids.insert(id.clone()) {
            return Err(diagnostic(
                "sop_duplicate_step_id",
                &format!("sop_duplicate_step_id:{id}"),
                json!({}),
            ));
        }
        let executor = required_string(step.get("executor"), "sop_step_requires_executor", 32)?;
        if !matches!(
            executor.as_str(),
            "engine" | "agent" | "operator" | "sop" | "action"
        ) {
            return Err(diagnostic(
                "sop_invalid_executor",
                &format!("sop_invalid_executor:{executor}"),
                json!({"step_id":id,"allowed":["engine","agent","operator","sop","action"]}),
            ));
        }
        for legacy in ["command", "args", "timeout_ms", "cwd"] {
            if step.contains_key(legacy) {
                return Err(diagnostic(
                    "sop_effect_must_be_governed_action",
                    &format!("sop_effect_must_be_governed_action:{id}"),
                    json!({"step_id":id,"legacy_field":legacy,"remediation":"Use executor=action with an owning MCP surface/tool and idempotency_key_argument."}),
                ));
            }
        }
        let blocking = matches!(executor.as_str(), "agent" | "operator");
        if let Some(declared) = step.get("blocking") {
            if declared.as_bool() != Some(blocking) {
                return Err(diagnostic(
                    "sop_blocking_semantics_fixed",
                    &format!("sop_blocking_semantics_fixed:{id}"),
                    json!({"executor":executor,"required_blocking":blocking}),
                ));
            }
        }
        let child_sop_id = optional_string(step.get("sop_id"));
        let child_version = match step.get("sop_version") {
            None | Some(Value::Null) => None,
            Some(value) => value.as_i64(),
        };
        let wait_policy = optional_string(step.get("wait_policy"))
            .or_else(|| (executor == "sop").then(|| "wait".to_string()));
        if executor == "sop" {
            let child = child_sop_id.as_ref().ok_or_else(|| {
                diagnostic(
                    "sop_step_requires_child_sop_id",
                    &format!("sop_step_requires_child_sop_id:{id}"),
                    json!({"step_id":id}),
                )
            })?;
            if owner_sop_id == Some(child.as_str()) {
                return Err(diagnostic(
                    "sop_recursive_child_definition",
                    &format!("sop_recursive_child_definition:{child}"),
                    json!({"step_id":id}),
                ));
            }
            if wait_policy.as_deref() != Some("wait") {
                return Err(diagnostic(
                    "sop_invalid_wait_policy",
                    "sop_invalid_wait_policy",
                    json!({"step_id":id,"allowed":["wait"]}),
                ));
            }
            if step.contains_key("sop_version")
                && child_version.map(|version| version < 1).unwrap_or(true)
            {
                return Err(diagnostic(
                    "sop_invalid_child_sop_version",
                    "sop_invalid_child_sop_version",
                    json!({"step_id":id}),
                ));
            }
        } else if child_sop_id.is_some()
            || child_version.is_some()
            || step.contains_key("wait_policy")
        {
            return Err(diagnostic(
                "sop_child_fields_require_sop_executor",
                &format!("sop_child_fields_require_sop_executor:{id}"),
                json!({}),
            ));
        }
        let when = normalize_condition(step.get("when"), 0, &mut 0)?;
        let input = optional_value(step.get("input"), &format!("steps.{id}.input"))?;
        let input_ref = optional_value(step.get("input_ref"), &format!("steps.{id}.input_ref"))?;
        let result_schema = optional_schema(
            step.get("result_schema"),
            &format!("steps.{id}.result_schema"),
        )?;
        let action = normalize_action(step.get("action"), &id)?;
        if executor == "action" && action.is_none() {
            return Err(diagnostic(
                "sop_action_binding_required",
                &format!("sop_action_binding_required:{id}"),
                json!({}),
            ));
        }
        if executor != "action" && action.is_some() {
            return Err(diagnostic(
                "sop_action_binding_requires_action_executor",
                &format!("sop_action_binding_requires_action_executor:{id}"),
                json!({}),
            ));
        }
        normalized.push(json!({
            "id":id,
            "executor":executor,
            "blocking":blocking,
            "title":required_string(step.get("title"),"sop_step_requires_title",512)?,
            "depends_on":string_list(step.get("depends_on"))?,
            "instructions":required_string(step.get("instructions"),"sop_step_requires_instructions",16*1024)?,
            "when":when,
            "input":input,
            "input_ref":input_ref,
            "result_schema":result_schema,
            "action":action,
            "sop_id":child_sop_id,
            "sop_version":child_version,
            "wait_policy":wait_policy,
            "legacy_command":Value::Null,
        }));
    }
    validate_dag(&normalized)?;
    validate_step_references(&normalized)?;
    Ok(Value::Array(normalized))
}

fn validate_dag(steps: &[Value]) -> Result<(), Value> {
    let by_id = steps
        .iter()
        .filter_map(|step| Some((step.get("id")?.as_str()?.to_string(), step)))
        .collect::<HashMap<_, _>>();
    for step in steps {
        let id = step.get("id").and_then(Value::as_str).unwrap_or("");
        for dependency in step
            .get("depends_on")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
        {
            if !by_id.contains_key(dependency) {
                return Err(diagnostic(
                    "sop_unknown_dependency",
                    "sop_unknown_dependency",
                    json!({"step_id":id,"dependency":dependency}),
                ));
            }
            if dependency == id {
                return Err(diagnostic(
                    "sop_dependency_cycle",
                    "sop_dependency_cycle",
                    json!({"cycle":[id,id]}),
                ));
            }
        }
    }
    fn visit(
        id: &str,
        by_id: &HashMap<String, &Value>,
        visiting: &mut Vec<String>,
        visited: &mut HashSet<String>,
    ) -> Result<(), Vec<String>> {
        if visited.contains(id) {
            return Ok(());
        }
        if let Some(start) = visiting.iter().position(|value| value == id) {
            let mut cycle = visiting[start..].to_vec();
            cycle.push(id.to_string());
            return Err(cycle);
        }
        visiting.push(id.to_string());
        for dependency in by_id
            .get(id)
            .and_then(|step| step.get("depends_on"))
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
        {
            visit(dependency, by_id, visiting, visited)?;
        }
        visiting.pop();
        visited.insert(id.to_string());
        Ok(())
    }
    let mut visiting = Vec::new();
    let mut visited = HashSet::new();
    for id in by_id.keys() {
        if let Err(cycle) = visit(id, &by_id, &mut visiting, &mut visited) {
            return Err(diagnostic(
                "sop_dependency_cycle",
                "sop_dependency_cycle",
                json!({"cycle":cycle}),
            ));
        }
    }
    Ok(())
}

fn validate_step_references(steps: &[Value]) -> Result<(), Value> {
    let by_id = steps
        .iter()
        .filter_map(|step| Some((step.get("id")?.as_str()?.to_string(), step)))
        .collect::<HashMap<_, _>>();
    fn ancestors<'a>(id: &str, by_id: &'a HashMap<String, &'a Value>, found: &mut HashSet<String>) {
        for dependency in by_id
            .get(id)
            .and_then(|step| step.get("depends_on"))
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
        {
            if found.insert(dependency.to_string()) {
                ancestors(dependency, by_id, found);
            }
        }
    }
    for step in steps {
        let id = step.get("id").and_then(Value::as_str).unwrap_or("");
        let mut allowed = HashSet::new();
        ancestors(id, &by_id, &mut allowed);
        let mut referenced = HashSet::new();
        for field in ["when", "input", "input_ref"] {
            collect_step_references(step.get(field), &mut referenced)?;
        }
        collect_step_references(
            step.get("action").and_then(|value| value.get("arguments")),
            &mut referenced,
        )?;
        collect_instruction_references(
            step.get("instructions")
                .and_then(Value::as_str)
                .unwrap_or(""),
            &mut referenced,
        )?;
        for target in referenced {
            if !by_id.contains_key(&target) {
                return Err(diagnostic(
                    "sop_step_reference_unknown",
                    "sop_step_reference_unknown",
                    json!({"step_id":id,"referenced_step_id":target}),
                ));
            }
            if !allowed.contains(&target) {
                return Err(diagnostic(
                    "sop_step_reference_not_dependency",
                    "sop_step_reference_not_dependency",
                    json!({"step_id":id,"referenced_step_id":target}),
                ));
            }
        }
    }
    Ok(())
}

fn validate_output_references(mapping: Option<&Value>, steps: &Value) -> Result<(), Value> {
    let Some(mapping) = mapping else {
        return Ok(());
    };
    let ids = steps
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|step| step.get("id").and_then(Value::as_str))
        .collect::<HashSet<_>>();
    let mut referenced = HashSet::new();
    collect_step_references(Some(mapping), &mut referenced)?;
    for target in referenced {
        if !ids.contains(target.as_str()) {
            return Err(diagnostic(
                "sop_output_reference_unknown",
                &format!("sop_output_reference_unknown:{target}"),
                json!({}),
            ));
        }
    }
    Ok(())
}

fn collect_step_references(
    value: Option<&Value>,
    output: &mut HashSet<String>,
) -> Result<(), Value> {
    let Some(value) = value else {
        return Ok(());
    };
    match value {
        Value::Array(values) => {
            for value in values {
                collect_step_references(Some(value), output)?;
            }
        }
        Value::Object(object) => {
            if let Some(reference) = object.get("ref").and_then(Value::as_str) {
                validate_reference(reference)?;
                add_step_reference(reference, output);
            }
            if object.len() == 1 {
                if let Some(reference) = object.get("$ref").and_then(Value::as_str) {
                    validate_reference(reference)?;
                    add_step_reference(reference, output);
                }
            }
            for value in object.values() {
                collect_step_references(Some(value), output)?;
            }
        }
        _ => {}
    }
    Ok(())
}

fn collect_instruction_references(text: &str, output: &mut HashSet<String>) -> Result<(), Value> {
    let mut remaining = text;
    while let Some(open) = remaining.find("{{") {
        let after = &remaining[open + 2..];
        let Some(close) = after.find("}}") else { break };
        let reference = after[..close].trim();
        validate_reference(reference)?;
        add_step_reference(reference, output);
        remaining = &after[close + 2..];
    }
    Ok(())
}

fn validate_reference(reference: &str) -> Result<(), Value> {
    let parts = reference.split('.').collect::<Vec<_>>();
    let safe = !parts.is_empty()
        && parts.iter().all(|part| {
            !part.is_empty()
                && *part != "__proto__"
                && *part != "prototype"
                && *part != "constructor"
                && part.chars().all(|character| {
                    character.is_ascii_alphanumeric() || character == '_' || character == '-'
                })
        });
    let valid = reference == "input"
        || reference == "input_ref"
        || (safe && matches!(parts.first(), Some(&"input" | &"input_ref")))
        || (safe
            && parts.len() >= 3
            && parts[0] == "steps"
            && matches!(parts[2], "status" | "result" | "result_ref"));
    if !valid {
        return Err(diagnostic(
            "sop_reference_invalid",
            "sop_reference_invalid",
            json!({"ref":reference}),
        ));
    }
    Ok(())
}

fn add_step_reference(reference: &str, output: &mut HashSet<String>) {
    let parts = reference.split('.').collect::<Vec<_>>();
    if parts.first() == Some(&"steps") && parts.len() >= 2 {
        output.insert(parts[1].to_string());
    }
}

fn normalize_condition(
    value: Option<&Value>,
    depth: usize,
    nodes: &mut usize,
) -> Result<Option<Value>, Value> {
    let Some(value) = value else {
        return Ok(None);
    };
    if value.is_null() {
        return Ok(None);
    }
    *nodes += 1;
    if depth > 12 || *nodes > 64 {
        return Err(diagnostic(
            "sop_condition_too_complex",
            "sop_condition_too_complex",
            json!({"max_depth":12,"max_nodes":64}),
        ));
    }
    let object = value.as_object().ok_or_else(|| {
        diagnostic(
            "sop_condition_invalid",
            "sop_condition_invalid",
            json!({"reason":"condition_must_be_object"}),
        )
    })?;
    if object.len() == 1 {
        for key in ["all", "any"] {
            if let Some(raw) = object.get(key) {
                let values = raw
                    .as_array()
                    .filter(|values| !values.is_empty())
                    .ok_or_else(|| {
                        diagnostic(
                            "sop_condition_invalid",
                            "sop_condition_invalid",
                            json!({"reason":format!("{key}_requires_nonempty_array")}),
                        )
                    })?;
                let mut normalized = Vec::new();
                for value in values {
                    normalized.push(
                        normalize_condition(Some(value), depth + 1, nodes)?.unwrap_or(Value::Null),
                    );
                }
                return Ok(Some(json!({key:normalized})));
            }
        }
        if let Some(raw) = object.get("not") {
            let normalized =
                normalize_condition(Some(raw), depth + 1, nodes)?.ok_or_else(|| {
                    diagnostic("sop_condition_invalid", "sop_condition_invalid", json!({}))
                })?;
            return Ok(Some(json!({"not":normalized})));
        }
    }
    if object
        .keys()
        .any(|key| !matches!(key.as_str(), "ref" | "op" | "value"))
    {
        return Err(diagnostic(
            "sop_condition_invalid",
            "sop_condition_invalid",
            json!({"reason":"unknown_fields"}),
        ));
    }
    let reference = required_string(object.get("ref"), "sop_condition_invalid", 512)?;
    validate_reference(&reference)?;
    let op = required_string(object.get("op"), "sop_condition_invalid", 32)?;
    if !matches!(
        op.as_str(),
        "equals" | "not_equals" | "exists" | "not_exists" | "truthy" | "falsy" | "in" | "contains"
    ) {
        return Err(diagnostic(
            "sop_condition_invalid",
            "sop_condition_invalid",
            json!({"reason":"unsupported_operator","op":op}),
        ));
    }
    if !matches!(op.as_str(), "exists" | "not_exists" | "truthy" | "falsy")
        && !object.contains_key("value")
    {
        return Err(diagnostic(
            "sop_condition_invalid",
            "sop_condition_invalid",
            json!({"reason":"operator_requires_value","op":op}),
        ));
    }
    if op == "in" && !object.get("value").map(Value::is_array).unwrap_or(false) {
        return Err(diagnostic(
            "sop_condition_invalid",
            "sop_condition_invalid",
            json!({"reason":"in_value_must_be_array"}),
        ));
    }
    let mut normalized = Map::new();
    normalized.insert("ref".to_string(), json!(reference));
    normalized.insert("op".to_string(), json!(op));
    if let Some(value) = object.get("value") {
        assert_bound(value, "sop_condition_value", MAX_INLINE_VALUE_BYTES)?;
        normalized.insert("value".to_string(), value.clone());
    }
    Ok(Some(Value::Object(normalized)))
}

fn normalize_action(value: Option<&Value>, step_id: &str) -> Result<Option<Value>, Value> {
    let Some(value) = value else {
        return Ok(None);
    };
    if value.is_null() {
        return Ok(None);
    }
    let object = value.as_object().ok_or_else(|| {
        diagnostic(
            "sop_action_binding_invalid",
            &format!("sop_action_binding_invalid:{step_id}"),
            json!({"reason":"must_be_object"}),
        )
    })?;
    let allowed = [
        "surface_id",
        "tool_name",
        "arguments",
        "idempotency_key_argument",
    ];
    let unknown = object
        .keys()
        .filter(|key| !allowed.contains(&key.as_str()))
        .cloned()
        .collect::<Vec<_>>();
    if !unknown.is_empty() {
        return Err(diagnostic(
            "sop_action_binding_invalid",
            &format!("sop_action_binding_invalid:{step_id}"),
            json!({"reason":"unknown_fields","fields":unknown}),
        ));
    }
    let surface_id = required_string(
        object.get("surface_id"),
        "sop_action_requires_surface_id",
        256,
    )?;
    let tool_name = required_string(
        object.get("tool_name"),
        "sop_action_requires_tool_name",
        256,
    )?;
    let idempotency = required_string(
        object.get("idempotency_key_argument"),
        "sop_action_requires_idempotency_key_argument",
        128,
    )?;
    if !valid_identifier(&idempotency) {
        return Err(diagnostic(
            "sop_action_idempotency_key_argument_invalid",
            &format!("sop_action_idempotency_key_argument_invalid:{idempotency}"),
            json!({}),
        ));
    }
    let arguments = object
        .get("arguments")
        .cloned()
        .unwrap_or_else(|| json!({}));
    if !arguments.is_object() {
        return Err(diagnostic(
            "sop_action_arguments_must_be_object",
            &format!("sop_action_arguments_must_be_object:{step_id}"),
            json!({}),
        ));
    }
    assert_bound(
        &arguments,
        "sop_action_arguments_mapping",
        MAX_INLINE_VALUE_BYTES,
    )?;
    if arguments.get(&idempotency).is_some() {
        return Err(diagnostic(
            "sop_action_idempotency_argument_reserved",
            &format!("sop_action_idempotency_argument_reserved:{step_id}"),
            json!({"field":idempotency}),
        ));
    }
    Ok(Some(json!({
        "surface_id":surface_id,"tool_name":tool_name,"arguments":arguments,
        "idempotency_key_argument":idempotency
    })))
}

fn optional_schema(value: Option<&Value>, field: &str) -> Result<Option<Value>, Value> {
    let Some(value) = value else {
        return Ok(None);
    };
    if value.is_null() {
        return Ok(None);
    }
    if !value.is_object() {
        return Err(diagnostic(
            "sop_json_schema_must_be_object",
            &format!("sop_json_schema_must_be_object:{field}"),
            json!({"field":field}),
        ));
    }
    assert_bound(value, "sop_json_schema", MAX_INLINE_VALUE_BYTES)?;
    validator_for(value).map_err(|error| {
        diagnostic(
            "sop_json_schema_invalid",
            &format!("sop_json_schema_invalid:{field}"),
            json!({"field":field,"message":error.to_string()}),
        )
    })?;
    Ok(Some(value.clone()))
}

fn optional_value(value: Option<&Value>, field: &str) -> Result<Option<Value>, Value> {
    let Some(value) = value else {
        return Ok(None);
    };
    if value.is_null() {
        return Ok(None);
    }
    assert_bound(value, field, MAX_INLINE_VALUE_BYTES)?;
    Ok(Some(value.clone()))
}

pub(crate) fn required_string(
    value: Option<&Value>,
    code: &str,
    max: usize,
) -> Result<String, Value> {
    let text = value
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| diagnostic(code, code, json!({})))?;
    if text.chars().count() > max {
        return Err(diagnostic(
            &format!("{code}_too_long"),
            &format!("{code}_too_long"),
            json!({"length":text.chars().count(),"max_length":max}),
        ));
    }
    Ok(text.to_string())
}

pub(crate) fn optional_string(value: Option<&Value>) -> Option<String> {
    value
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
}

fn string_list(value: Option<&Value>) -> Result<Vec<Value>, Value> {
    let Some(value) = value else {
        return Ok(Vec::new());
    };
    if value.is_null() {
        return Ok(Vec::new());
    }
    let values = value.as_array().ok_or_else(|| {
        diagnostic(
            "sop_string_list_invalid",
            "sop_string_list_invalid",
            json!({"reason":"must_be_array"}),
        )
    })?;
    let mut output = Vec::new();
    let mut seen = HashSet::new();
    for (index, value) in values.iter().enumerate() {
        let text = value
            .as_str()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| {
                diagnostic(
                    "sop_string_list_invalid",
                    "sop_string_list_invalid",
                    json!({"reason":"entry_must_be_nonempty_string","index":index}),
                )
            })?;
        if !seen.insert(text.to_string()) {
            return Err(diagnostic(
                "sop_string_list_invalid",
                "sop_string_list_invalid",
                json!({"reason":"duplicate_entries"}),
            ));
        }
        output.push(json!(text));
    }
    Ok(output)
}

fn normalize_trigger(value: Option<&Value>) -> Result<String, Value> {
    let value = optional_string(value).unwrap_or_else(|| "manual".to_string());
    if !matches!(value.as_str(), "manual" | "inbox_event" | "schedule") {
        return Err(diagnostic(
            "sop_invalid_trigger_kind",
            &format!("sop_invalid_trigger_kind:{value}"),
            json!({"trigger_kind":value,"allowed":["manual","inbox_event","schedule"]}),
        ));
    }
    Ok(value)
}

fn normalize_template_status(value: Option<&Value>) -> Result<String, Value> {
    let value = optional_string(value).unwrap_or_else(|| "draft".to_string());
    if !matches!(value.as_str(), "draft" | "active" | "deprecated") {
        return Err(diagnostic(
            "sop_invalid_template_status",
            &format!("sop_invalid_template_status:{value}"),
            json!({"status":value,"allowed":["draft","active","deprecated"]}),
        ));
    }
    Ok(value)
}

fn append_event(db: &Connection, kind: &str, details: Value) -> Result<String, Value> {
    let event_id = format!("soe_{}", &Uuid::new_v4().to_string()[..12]);
    db.execute(
        "INSERT INTO sop_events(event_id,run_id,step_id,event_kind,details_json,recorded_at) VALUES (?,'','',?,?,?)",
        params![event_id, kind, encode(&details)?, now_iso()],
    )
    .map_err(|error| diagnostic("sop_event_insert_failed", &error.to_string(), json!({})))?;
    Ok(event_id)
}

fn pinned_child_references(
    db: &Connection,
    sop_id: &str,
    version: i64,
) -> Result<Vec<Value>, Value> {
    let mut statement = db
        .prepare(
            "SELECT run_id,step_states_json FROM sop_runs ORDER BY created_at DESC LIMIT 10000",
        )
        .map_err(|error| diagnostic("sop_run_query_failed", &error.to_string(), json!({})))?;
    let rows = statement
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })
        .map_err(|error| diagnostic("sop_run_query_failed", &error.to_string(), json!({})))?;
    let mut references = Vec::new();
    for row in rows.take(10_000) {
        let (run_id, encoded) =
            row.map_err(|error| diagnostic("sop_run_query_failed", &error.to_string(), json!({})))?;
        let Ok(states) = serde_json::from_str::<Value>(&encoded) else {
            continue;
        };
        for step in states.as_array().into_iter().flatten() {
            if step.get("sop_id").and_then(Value::as_str) == Some(sop_id)
                && step.get("sop_version").and_then(Value::as_i64) == Some(version)
            {
                references.push(json!({"run_id":run_id,"step_id":step.get("step_id").cloned().unwrap_or(Value::Null)}));
                if references.len() >= 20 {
                    return Ok(references);
                }
            }
        }
    }
    Ok(references)
}

fn sops_dirs(root: &Path) -> Vec<PathBuf> {
    let mut directories = Vec::new();
    if let Some(value) = std::env::var_os("NARADA_SOPS_DIR") {
        directories.push(PathBuf::from(value));
    }
    directories.push(root.join("sops"));
    directories.push(root.join(".ai/sops"));
    let mut seen = HashSet::new();
    directories
        .into_iter()
        .filter(|path| seen.insert(path.clone()))
        .take(10)
        .collect()
}

fn parse_json_member(
    object: &Map<String, Value>,
    key: &str,
    fallback: Value,
) -> Result<Value, Value> {
    let Some(value) = object.get(key) else {
        return Ok(fallback);
    };
    if value.is_null() {
        return Ok(fallback);
    }
    let text = value.as_str().unwrap_or("");
    if text.is_empty() {
        return Ok(fallback);
    }
    serde_json::from_str(text).map_err(|error| {
        diagnostic(
            "sop_persisted_value_invalid",
            &error.to_string(),
            json!({"field":key}),
        )
    })
}

fn parse_nullable_member(object: &Map<String, Value>, key: &str) -> Result<Option<Value>, Value> {
    let Some(value) = object.get(key) else {
        return Ok(None);
    };
    if value.is_null() {
        return Ok(None);
    }
    let text = value.as_str().unwrap_or("");
    if text.is_empty() {
        return Ok(None);
    }
    serde_json::from_str(text).map(Some).map_err(|error| {
        diagnostic(
            "sop_persisted_value_invalid",
            &error.to_string(),
            json!({"field":key}),
        )
    })
}

fn nullable_member<'a>(object: &'a Map<String, Value>, key: &str) -> Option<&'a Value> {
    object.get(key).filter(|value| !value.is_null())
}

fn text_member(object: &Map<String, Value>, key: &str) -> String {
    object
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string()
}

fn assert_template_bound(value: &Value) -> Result<(), Value> {
    assert_bound(
        value,
        "sop_template_definition",
        MAX_TEMPLATE_DEFINITION_BYTES,
    )
}

pub(crate) fn assert_bound(value: &Value, field: &str, max: usize) -> Result<(), Value> {
    let bytes = canonical_json(value).as_bytes().len();
    if bytes > max {
        return Err(diagnostic(
            &format!("{field}_too_large"),
            &format!("{field}_too_large"),
            json!({"field":field,"byte_length":bytes,"max_bytes":max}),
        ));
    }
    Ok(())
}

fn encode(value: &Value) -> Result<String, Value> {
    serde_json::to_string(value)
        .map_err(|error| diagnostic("sop_json_encode_failed", &error.to_string(), json!({})))
}

fn encode_optional(value: Option<&Value>) -> Result<Option<String>, Value> {
    value.map(encode).transpose()
}

pub(crate) fn canonical_json(value: &Value) -> String {
    match value {
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {
            serde_json::to_string(value).unwrap_or_else(|_| "null".to_string())
        }
        Value::Array(values) => format!(
            "[{}]",
            values
                .iter()
                .map(canonical_json)
                .collect::<Vec<_>>()
                .join(",")
        ),
        Value::Object(object) => {
            let mut keys = object.keys().collect::<Vec<_>>();
            keys.sort();
            format!(
                "{{{}}}",
                keys.into_iter()
                    .map(|key| format!(
                        "{}:{}",
                        serde_json::to_string(key).unwrap_or_else(|_| "\"\"".to_string()),
                        canonical_json(object.get(key).unwrap_or(&Value::Null))
                    ))
                    .collect::<Vec<_>>()
                    .join(",")
            )
        }
    }
}

#[allow(dead_code)]
pub(crate) fn fingerprint(value: &Value) -> String {
    Sha256::digest(canonical_json(value).as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

#[allow(dead_code)]
pub(crate) fn deterministic_id(prefix: &str, value: &str) -> String {
    let digest = Sha256::digest(value.as_bytes());
    let hex = digest
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    format!("{prefix}{}", &hex[..24])
}

pub(crate) fn now_iso() -> String {
    let value = OffsetDateTime::now_utc();
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:03}Z",
        value.year(),
        u8::from(value.month()),
        value.day(),
        value.hour(),
        value.minute(),
        value.second(),
        value.nanosecond() / 1_000_000
    )
}

fn valid_step_id(value: &str) -> bool {
    let mut characters = value.chars();
    characters
        .next()
        .map(|character| character.is_ascii_alphanumeric())
        .unwrap_or(false)
        && characters.all(|character| {
            character.is_ascii_alphanumeric() || character == '_' || character == '-'
        })
}

fn valid_identifier(value: &str) -> bool {
    let mut characters = value.chars();
    characters
        .next()
        .map(|character| character.is_ascii_alphabetic() || character == '_')
        .unwrap_or(false)
        && characters.all(|character| character.is_ascii_alphanumeric() || character == '_')
}

pub(crate) fn diagnostic(code: &str, message: &str, details: Value) -> Value {
    json!({"schema":"narada.sop.error.v1","code":code,"message":message,"details":details})
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn claim_and_advance_completes_without_cross_call_lease_threading() {
        let root = std::env::temp_dir().join(format!("narada-sop-compound-{}", Uuid::new_v4()));
        template_create(
            json!({
                "sop_id":"compound-demo","title":"Compound demo","steps":[{
                    "id":"manual","executor":"agent","title":"Manual","instructions":"Complete"
                }]
            })
            .as_object()
            .unwrap(),
            &root,
        )
        .expect("create");
        template_update(
            json!({"sop_id":"compound-demo","status":"active"})
                .as_object()
                .unwrap(),
            &root,
        )
        .expect("activate");
        crate::sop_engine::call_tool(
            "sop_run_start",
            json!({
                "sop_id":"compound-demo","occurrence_key":"occ-1",
                "triggered_by":"test","input":{}
            })
            .as_object()
            .unwrap(),
            &root,
        )
        .expect("start");

        let completed = handoff_claim_and_advance(
            json!({
                "consumer_id":"test-agent","completion_key":"completion-1",
                "outcome":"completed","result":{"answer":42},"principal":"agent:test"
            })
            .as_object()
            .unwrap(),
            &root,
        )
        .expect("claim and advance");
        assert_eq!(completed["status"], "advanced");
        assert_eq!(completed["advanced"], true);
        assert_eq!(completed["result"]["status"], "completed");
        assert_eq!(completed["claim"]["lease_ms"], 60_000);
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn template_registry_mutations_are_versioned_and_bounded() {
        let root = std::env::temp_dir().join(format!("narada-sop-authority-{}", Uuid::new_v4()));
        let create = template_create(
            json!({
                "sop_id":"demo","title":"Demo","steps":[{
                    "id":"first","executor":"engine","title":"First","instructions":"Record input"
                }]
            })
            .as_object()
            .unwrap(),
            &root,
        )
        .expect("create");
        assert_eq!(create["version"], 1);
        let update = template_update(
            json!({"sop_id":"demo","title":"Demo v2","status":"active"})
                .as_object()
                .unwrap(),
            &root,
        )
        .expect("update");
        assert_eq!(update["version"], 2);
        let deprecated = template_deprecate(
            json!({"sop_id":"demo","reason":"fixture"})
                .as_object()
                .unwrap(),
            &root,
        )
        .expect("deprecate");
        assert_eq!(deprecated["status"], "deprecated");
        let removed = template_unimport(
            json!({"sop_id":"demo","version":1,"reason":"fixture cleanup","principal":"test"})
                .as_object()
                .unwrap(),
            &root,
        )
        .expect("unimport");
        assert_eq!(removed["remaining_versions"], json!([2]));
        fs::remove_dir_all(root).expect("cleanup");
    }
}
