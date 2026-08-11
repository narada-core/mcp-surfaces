use rusqlite::{params, params_from_iter, types::Value as SqlValue, Connection, OptionalExtension};
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use time::{
    format_description::well_known::Rfc3339, macros::format_description, Duration, OffsetDateTime,
};
use uuid::Uuid;

const SCHEMA_VERSION: i64 = 2;
const DB_RELATIVE: &str = ".ai/scheduler.db";
const MAX_EVENT_BYTES: usize = 16_384;
const MAX_ERROR_BYTES: usize = 2_048;

pub const TOOLS: &[(&str, bool)] = &[
    ("scheduler_activation_doctor", true),
    ("scheduler_activation_prepare", false),
    ("scheduler_binding_list", true),
    ("scheduler_binding_show", true),
    ("scheduler_binding_upsert", false),
    ("scheduler_binding_pause", false),
    ("scheduler_binding_resume", false),
    ("scheduler_binding_retire", false),
    ("scheduler_event_show", true),
    ("scheduler_event_admit", false),
    ("scheduler_activation_list", true),
    ("scheduler_activation_claim", false),
    ("scheduler_activation_admit_sop", false),
    ("scheduler_activation_fail", false),
    ("scheduler_activation_resolve", false),
    ("scheduler_activation_unblock", false),
];

pub fn supports(name: &str) -> bool {
    TOOLS.iter().any(|(candidate, _)| *candidate == name)
}

pub fn is_mutation(name: &str) -> bool {
    TOOLS
        .iter()
        .find(|(candidate, _)| *candidate == name)
        .map(|(_, read_only)| !read_only)
        .unwrap_or(false)
}

pub fn call_tool(name: &str, args: &Map<String, Value>, root: &Path) -> Result<Value, Value> {
    match name {
        "scheduler_activation_doctor" => Ok(doctor(root)),
        "scheduler_activation_prepare" => prepare(root),
        "scheduler_binding_list" => with_prepared(root, |db| binding_list(db, args)),
        "scheduler_binding_show" => with_prepared(root, |db| binding_show(db, args)),
        "scheduler_binding_upsert" => with_prepared(root, |db| binding_upsert(db, args)),
        "scheduler_binding_pause" | "scheduler_binding_resume" | "scheduler_binding_retire" => {
            with_prepared(root, |db| binding_set_status(db, name, args))
        }
        "scheduler_event_show" => with_prepared(root, |db| event_show(db, args)),
        "scheduler_event_admit" => with_prepared(root, |db| event_admit(db, args)),
        "scheduler_activation_list" => with_prepared(root, |db| activation_list(db, args)),
        "scheduler_activation_claim" => with_prepared(root, |db| activation_claim(db, args)),
        "scheduler_activation_admit_sop" => {
            with_prepared(root, |db| activation_admit_sop(db, args))
        }
        "scheduler_activation_fail" => with_prepared(root, |db| activation_fail(db, args)),
        "scheduler_activation_resolve" => with_prepared(root, |db| activation_resolve(db, args)),
        "scheduler_activation_unblock" => with_prepared(root, |db| activation_unblock(db, args)),
        _ => Err(error("unknown_tool", &format!("unknown_tool:{name}"))),
    }
}

fn db_path(root: &Path) -> PathBuf {
    root.join(DB_RELATIVE)
}

fn configure(db: &Connection, mutate_journal: bool) -> Result<(), Value> {
    db.execute_batch("pragma foreign_keys = on; pragma busy_timeout = 30000;")
        .map_err(|cause| db_error("scheduler_activation_store_configure_failed", cause))?;
    if mutate_journal {
        db.execute_batch("pragma journal_mode = wal;")
            .map_err(|cause| db_error("scheduler_activation_store_configure_failed", cause))?;
    }
    let mode: String = db
        .query_row("pragma journal_mode", [], |row| row.get(0))
        .map_err(|cause| db_error("scheduler_activation_store_configure_failed", cause))?;
    if !mode.eq_ignore_ascii_case("wal") {
        return Err(error(
            "scheduler_activation_store_not_prepared",
            &format!("scheduler_activation_store_not_prepared:journal_mode_{mode}"),
        ));
    }
    db.execute_batch("pragma synchronous = normal;")
        .map_err(|cause| db_error("scheduler_activation_store_configure_failed", cause))
}

fn prepare(root: &Path) -> Result<Value, Value> {
    let path = db_path(root);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|cause| {
            error(
                "scheduler_activation_store_directory_failed",
                &cause.to_string(),
            )
        })?;
    }
    let db = Connection::open(&path)
        .map_err(|cause| db_error("scheduler_activation_store_open_failed", cause))?;
    configure(&db, true)?;
    initialize_schema(&db)?;
    Ok(json!({
        "schema":"narada.scheduler.activation_prepare.v1",
        "status":"prepared",
        "db_path":path.to_string_lossy(),
        "schema_version":SCHEMA_VERSION
    }))
}

fn initialize_schema(db: &Connection) -> Result<(), Value> {
    db.execute_batch(
        r#"
        begin immediate;
        create table if not exists scheduler_meta (
          singleton integer primary key check (singleton = 1),
          schema_version integer not null,
          prepared_at text not null
        );
        create table if not exists scheduler_bindings (
          binding_id text primary key,
          trigger_kind text not null check (trigger_kind in ('bootstrap', 'completion', 'domain_event')),
          source_topic text not null,
          source_sop_id text,
          terminal_outcomes_json text not null,
          target_sop_id text not null,
          target_template_version text not null,
          concurrency text not null check (concurrency in ('singleton', 'partitioned')),
          delay_by_outcome_ms_json text not null,
          default_delay_ms integer not null check (default_delay_ms >= 0),
          retry_base_ms integer not null check (retry_base_ms >= 0),
          retry_max_ms integer not null check (retry_max_ms >= retry_base_ms),
          max_attempts integer not null check (max_attempts > 0),
          blocked_policy text not null check (blocked_policy = 'manual_unblock'),
          status text not null check (status in ('active', 'paused', 'retired')),
          revision integer not null check (revision > 0),
          spec_digest text not null,
          created_at text not null,
          updated_at text not null
        );
        create index if not exists idx_scheduler_bindings_topic on scheduler_bindings(source_topic, status);
        create table if not exists scheduler_source_events (
          event_id text primary key,
          topic text not null,
          partition_key text not null,
          aggregate_id text not null,
          aggregate_revision integer not null,
          schema_version integer not null,
          causation_id text not null,
          idempotency_key text not null,
          payload_json text not null check (length(cast(payload_json as blob)) <= 16384),
          event_digest text not null,
          occurred_at text not null,
          admitted_at text not null
        );
        create table if not exists scheduler_activations (
          activation_id text primary key,
          binding_id text not null references scheduler_bindings(binding_id),
          source_event_id text not null references scheduler_source_events(event_id),
          occurrence_key text not null,
          target_sop_id text not null,
          target_template_version text not null,
          partition_key text not null,
          due_at text not null,
          status text not null check (status in ('pending', 'leased', 'admitted', 'terminal', 'blocked')),
          attempt_count integer not null default 0,
          lease_owner text,
          lease_token text,
          lease_expires_at text,
          sop_run_id text,
          terminal_outcome text,
          last_error text,
          created_at text not null,
          updated_at text not null,
          unique(binding_id, source_event_id),
          unique(target_sop_id, occurrence_key)
        );
        create index if not exists idx_scheduler_activations_due on scheduler_activations(status, due_at, binding_id, partition_key);
        create index if not exists idx_scheduler_activations_sop_run on scheduler_activations(sop_run_id);
        create table if not exists scheduler_activation_receipts (
          activation_id text not null references scheduler_activations(activation_id),
          receipt_kind text not null,
          receipt_id text not null,
          receipt_json text not null check (length(cast(receipt_json as blob)) <= 16384),
          recorded_at text not null,
          primary key(activation_id, receipt_kind),
          unique(receipt_id)
        );
        create unique index if not exists idx_scheduler_activations_sop_run_unique
          on scheduler_activations(sop_run_id) where sop_run_id is not null;
        commit;
        "#,
    )
    .map_err(|cause| db_error("scheduler_activation_schema_failed", cause))?;
    db.execute(
        "insert into scheduler_meta(singleton, schema_version, prepared_at) values (1, ?1, ?2) on conflict(singleton) do update set schema_version=excluded.schema_version, prepared_at=excluded.prepared_at",
        params![SCHEMA_VERSION, now_iso()],
    )
    .map_err(|cause| db_error("scheduler_activation_schema_failed", cause))?;
    Ok(())
}

fn doctor(root: &Path) -> Value {
    let path = db_path(root);
    let mut result = json!({
        "schema":"narada.scheduler.activation_doctor.v1",
        "site_root":root.to_string_lossy(),
        "runtime_open":false,
        "preparation":{
            "status":"missing",
            "db_path":path.to_string_lossy(),
            "schema_version":Value::Null,
            "reason":"database_missing"
        }
    });
    if !path.exists() {
        return result;
    }
    let inspection = match Connection::open(&path) {
        Ok(db) => match configure(&db, false).and_then(|_| {
            db.query_row(
                "select schema_version from scheduler_meta where singleton = 1",
                [],
                |row| row.get::<_, i64>(0),
            )
            .optional()
            .map_err(|cause| db_error("scheduler_activation_store_inspect_failed", cause))
        }) {
            Ok(Some(version)) if version == SCHEMA_VERSION => {
                json!({"status":"prepared","db_path":path.to_string_lossy(),"schema_version":version})
            }
            Ok(version) => {
                json!({"status":"stale","db_path":path.to_string_lossy(),"schema_version":version,"reason":format!("schema_version_{}", version.map(|value| value.to_string()).unwrap_or_else(|| "missing".to_string()))})
            }
            Err(reason) => {
                json!({"status":"invalid","db_path":path.to_string_lossy(),"schema_version":Value::Null,"reason":reason.get("message").cloned().unwrap_or(reason)})
            }
        },
        Err(cause) => {
            json!({"status":"invalid","db_path":path.to_string_lossy(),"schema_version":Value::Null,"reason":cause.to_string()})
        }
    };
    result["preparation"] = inspection;
    result
}

fn with_prepared<F>(root: &Path, action: F) -> Result<Value, Value>
where
    F: FnOnce(&Connection) -> Result<Value, Value>,
{
    let path = db_path(root);
    if !path.exists() {
        return Err(error(
            "scheduler_activation_store_not_prepared",
            "scheduler_activation_store_not_prepared:database_missing",
        ));
    }
    let db = Connection::open(&path)
        .map_err(|cause| db_error("scheduler_activation_store_open_failed", cause))?;
    configure(&db, false)?;
    let version: Option<i64> = db
        .query_row(
            "select schema_version from scheduler_meta where singleton = 1",
            [],
            |row| row.get(0),
        )
        .optional()
        .map_err(|cause| db_error("scheduler_activation_store_inspect_failed", cause))?;
    if version != Some(SCHEMA_VERSION) {
        return Err(error(
            "scheduler_activation_store_not_prepared",
            &format!(
                "scheduler_activation_store_not_prepared:schema_version_{}",
                version
                    .map(|value| value.to_string())
                    .unwrap_or_else(|| "missing".to_string())
            ),
        ));
    }
    action(&db)
}

fn binding_upsert(db: &Connection, args: &Map<String, Value>) -> Result<Value, Value> {
    let spec = normalize_binding(args)?;
    let binding_id = required_string(&spec, "binding_id")?;
    let spec_digest = digest(&Value::Object(spec.clone()));
    let now = now_iso();
    transaction(db, || {
        let existing = query_binding(db, &binding_id)?;
        if let Some(current) = existing {
            let current_digest = current
                .get("spec_digest")
                .and_then(Value::as_str)
                .unwrap_or("");
            let expected = args.get("expected_revision").and_then(Value::as_i64);
            if expected.is_none() {
                if current_digest == spec_digest {
                    return Ok(json!({"schema":"narada.scheduler.binding.v1","binding":current}));
                }
                return Err(error(
                    "scheduler_binding_expected_revision_required",
                    "scheduler_binding_expected_revision_required",
                ));
            }
            let actual = current.get("revision").and_then(Value::as_i64).unwrap_or(0);
            if expected != Some(actual) {
                return Err(error(
                    "scheduler_binding_revision_conflict",
                    &format!(
                        "scheduler_binding_revision_conflict:expected_{}:actual_{actual}",
                        expected.unwrap_or_default()
                    ),
                ));
            }
            db.execute(
                r#"update scheduler_bindings set trigger_kind=?1,source_topic=?2,source_sop_id=?3,terminal_outcomes_json=?4,target_sop_id=?5,target_template_version=?6,concurrency=?7,delay_by_outcome_ms_json=?8,default_delay_ms=?9,retry_base_ms=?10,retry_max_ms=?11,max_attempts=?12,blocked_policy=?13,revision=revision+1,spec_digest=?14,updated_at=?15 where binding_id=?16"#,
                params![
                    text(&spec,"trigger_kind"), text(&spec,"source_topic"), optional_text(&spec,"source_sop_id"), canonical_json(spec.get("terminal_outcomes").unwrap_or(&Value::Array(Vec::new()))),
                    text(&spec,"target_sop_id"), text(&spec,"target_template_version"), text(&spec,"concurrency"), canonical_json(spec.get("delay_by_outcome_ms").unwrap_or(&Value::Object(Map::new()))),
                    integer(&spec,"default_delay_ms"), integer(&spec,"retry_base_ms"), integer(&spec,"retry_max_ms"), integer(&spec,"max_attempts"), "manual_unblock", spec_digest, now, binding_id
                ],
            ).map_err(|cause| db_error("scheduler_binding_update_failed", cause))?;
        } else {
            if args.get("expected_revision").is_some() {
                return Err(error(
                    "scheduler_binding_not_found",
                    "scheduler_binding_not_found",
                ));
            }
            db.execute(
                r#"insert into scheduler_bindings(binding_id,trigger_kind,source_topic,source_sop_id,terminal_outcomes_json,target_sop_id,target_template_version,concurrency,delay_by_outcome_ms_json,default_delay_ms,retry_base_ms,retry_max_ms,max_attempts,blocked_policy,status,revision,spec_digest,created_at,updated_at) values (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,'manual_unblock','active',1,?14,?15,?15)"#,
                params![
                    binding_id, text(&spec,"trigger_kind"), text(&spec,"source_topic"), optional_text(&spec,"source_sop_id"), canonical_json(spec.get("terminal_outcomes").unwrap_or(&Value::Array(Vec::new()))),
                    text(&spec,"target_sop_id"), text(&spec,"target_template_version"), text(&spec,"concurrency"), canonical_json(spec.get("delay_by_outcome_ms").unwrap_or(&Value::Object(Map::new()))),
                    integer(&spec,"default_delay_ms"), integer(&spec,"retry_base_ms"), integer(&spec,"retry_max_ms"), integer(&spec,"max_attempts"), spec_digest, now
                ],
            ).map_err(|cause| db_error("scheduler_binding_insert_failed", cause))?;
        }
        let binding = require_binding(db, &binding_id)?;
        Ok(json!({"schema":"narada.scheduler.binding.v1","binding":binding}))
    })
}

fn normalize_binding(args: &Map<String, Value>) -> Result<Map<String, Value>, Value> {
    let trigger_kind = required(args, "trigger_kind")?;
    if !matches!(
        trigger_kind.as_str(),
        "bootstrap" | "completion" | "domain_event"
    ) {
        return Err(error(
            "scheduler_binding_trigger_kind_invalid",
            "scheduler_binding_trigger_kind_invalid",
        ));
    }
    let concurrency = required(args, "concurrency")?;
    if !matches!(concurrency.as_str(), "singleton" | "partitioned") {
        return Err(error(
            "scheduler_binding_concurrency_invalid",
            "scheduler_binding_concurrency_invalid",
        ));
    }
    let mut terminal = args
        .get("terminal_outcomes")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .filter_map(|value| {
            value
                .as_str()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(ToString::to_string)
        })
        .collect::<Vec<_>>();
    terminal.sort();
    terminal.dedup();
    let mut delays = BTreeMap::new();
    for (key, value) in args
        .get("delay_by_outcome_ms")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default()
    {
        let Some(delay) = value.as_i64().filter(|delay| *delay >= 0) else {
            return Err(error(
                "scheduler_binding_delay_invalid",
                "scheduler_binding_delay_invalid",
            ));
        };
        delays.insert(key, json!(delay));
    }
    let retry_base = nonnegative(args, "retry_base_ms", 1_000)?;
    let retry_max = nonnegative(args, "retry_max_ms", 300_000)?;
    if retry_max < retry_base {
        return Err(error("retry_max_ms_below_base", "retry_max_ms_below_base"));
    }
    let max_attempts = nonnegative(args, "max_attempts", 5)?;
    if max_attempts < 1 {
        return Err(error("max_attempts_invalid", "max_attempts_invalid"));
    }
    let mut spec = Map::new();
    spec.insert("binding_id".into(), json!(required(args, "binding_id")?));
    spec.insert("trigger_kind".into(), json!(trigger_kind));
    spec.insert(
        "source_topic".into(),
        json!(required(args, "source_topic")?),
    );
    spec.insert(
        "source_sop_id".into(),
        args.get("source_sop_id")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(|value| json!(value))
            .unwrap_or(Value::Null),
    );
    spec.insert("terminal_outcomes".into(), json!(terminal));
    spec.insert(
        "target_sop_id".into(),
        json!(required(args, "target_sop_id")?),
    );
    spec.insert(
        "target_template_version".into(),
        json!(required(args, "target_template_version")?),
    );
    spec.insert("concurrency".into(), json!(concurrency));
    spec.insert(
        "delay_by_outcome_ms".into(),
        Value::Object(delays.into_iter().collect()),
    );
    spec.insert(
        "default_delay_ms".into(),
        json!(nonnegative(args, "default_delay_ms", 0)?),
    );
    spec.insert("retry_base_ms".into(), json!(retry_base));
    spec.insert("retry_max_ms".into(), json!(retry_max));
    spec.insert("max_attempts".into(), json!(max_attempts));
    spec.insert("blocked_policy".into(), json!("manual_unblock"));
    Ok(spec)
}

fn binding_list(db: &Connection, args: &Map<String, Value>) -> Result<Value, Value> {
    let status = args.get("status").and_then(Value::as_str);
    if let Some(value) = status {
        if !matches!(value, "active" | "paused" | "retired") {
            return Err(error(
                "scheduler_binding_status_invalid",
                "scheduler_binding_status_invalid",
            ));
        }
    }
    let mut statement = db
        .prepare(if status.is_some() {
            "select * from scheduler_bindings where status=?1 order by binding_id"
        } else {
            "select * from scheduler_bindings order by binding_id"
        })
        .map_err(|cause| db_error("scheduler_binding_query_failed", cause))?;
    let bindings = if let Some(status) = status {
        statement.query_map(params![status], binding_from_row)
    } else {
        statement.query_map([], binding_from_row)
    }
    .map_err(|cause| db_error("scheduler_binding_query_failed", cause))?
    .collect::<rusqlite::Result<Vec<_>>>()
    .map_err(|cause| db_error("scheduler_binding_query_failed", cause))?;
    Ok(
        json!({"schema":"narada.scheduler.binding_list.v1","count":bindings.len(),"bindings":bindings}),
    )
}

fn binding_show(db: &Connection, args: &Map<String, Value>) -> Result<Value, Value> {
    let binding = require_binding(db, &required(args, "binding_id")?)?;
    Ok(json!({"schema":"narada.scheduler.binding.v1","binding":binding}))
}

fn binding_set_status(
    db: &Connection,
    name: &str,
    args: &Map<String, Value>,
) -> Result<Value, Value> {
    let binding_id = required(args, "binding_id")?;
    let expected = required_integer(args, "expected_revision")?;
    let status = if name.ends_with("_pause") {
        "paused"
    } else if name.ends_with("_resume") {
        "active"
    } else {
        "retired"
    };
    let now = now_iso();
    transaction(db, || {
        let current = require_binding(db, &binding_id)?;
        let actual = current.get("revision").and_then(Value::as_i64).unwrap_or(0);
        if actual != expected {
            return Err(error(
                "scheduler_binding_revision_conflict",
                &format!("scheduler_binding_revision_conflict:expected_{expected}:actual_{actual}"),
            ));
        }
        db.execute("update scheduler_bindings set status=?1,revision=revision+1,updated_at=?2 where binding_id=?3", params![status,now,binding_id])
            .map_err(|cause| db_error("scheduler_binding_status_update_failed", cause))?;
        if status == "paused" {
            db.execute("update scheduler_activations set status='terminal',terminal_outcome='cancelled_binding_paused',lease_owner=null,lease_token=null,lease_expires_at=null,last_error='binding_paused_before_admission',updated_at=?1 where binding_id=?2 and (status='pending' or (status='leased' and lease_expires_at<=?1))", params![now,binding_id])
                .map_err(|cause| db_error("scheduler_binding_quiesce_failed", cause))?;
        }
        Ok(
            json!({"schema":"narada.scheduler.binding.v1","binding":require_binding(db,&binding_id)?}),
        )
    })
}

fn query_binding(db: &Connection, id: &str) -> Result<Option<Value>, Value> {
    db.query_row(
        "select * from scheduler_bindings where binding_id=?1",
        params![id],
        binding_from_row,
    )
    .optional()
    .map_err(|cause| db_error("scheduler_binding_query_failed", cause))
}

fn require_binding(db: &Connection, id: &str) -> Result<Value, Value> {
    query_binding(db, id)?.ok_or_else(|| {
        error(
            "scheduler_binding_not_found",
            &format!("scheduler_binding_not_found:{id}"),
        )
    })
}

fn binding_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Value> {
    let terminal: String = row.get("terminal_outcomes_json")?;
    let delays: String = row.get("delay_by_outcome_ms_json")?;
    Ok(json!({
        "binding_id":row.get::<_,String>("binding_id")?,"trigger_kind":row.get::<_,String>("trigger_kind")?,"source_topic":row.get::<_,String>("source_topic")?,"source_sop_id":row.get::<_,Option<String>>("source_sop_id")?,
        "terminal_outcomes":serde_json::from_str::<Value>(&terminal).unwrap_or_else(|_|json!([])),"target_sop_id":row.get::<_,String>("target_sop_id")?,"target_template_version":row.get::<_,String>("target_template_version")?,
        "concurrency":row.get::<_,String>("concurrency")?,"delay_by_outcome_ms":serde_json::from_str::<Value>(&delays).unwrap_or_else(|_|json!({})),"default_delay_ms":row.get::<_,i64>("default_delay_ms")?,
        "retry_base_ms":row.get::<_,i64>("retry_base_ms")?,"retry_max_ms":row.get::<_,i64>("retry_max_ms")?,"max_attempts":row.get::<_,i64>("max_attempts")?,"blocked_policy":"manual_unblock",
        "status":row.get::<_,String>("status")?,"revision":row.get::<_,i64>("revision")?,"spec_digest":row.get::<_,String>("spec_digest")?,"created_at":row.get::<_,String>("created_at")?,"updated_at":row.get::<_,String>("updated_at")?
    }))
}

fn event_admit(db: &Connection, args: &Map<String, Value>) -> Result<Value, Value> {
    let event = normalize_event(args)?;
    let event_id = required_string(&event, "event_id")?;
    let event_digest = digest(&Value::Object(event.clone()));
    let now = now_iso();
    transaction(db, || {
        let existing_digest: Option<String> = db
            .query_row(
                "select event_digest from scheduler_source_events where event_id=?1",
                params![event_id],
                |row| row.get(0),
            )
            .optional()
            .map_err(|cause| db_error("scheduler_event_query_failed", cause))?;
        if let Some(existing) = existing_digest.as_deref() {
            if existing != event_digest {
                return Err(error(
                    "scheduler_event_idempotency_conflict",
                    &format!("scheduler_event_idempotency_conflict:{event_id}"),
                ));
            }
        } else {
            db.execute(
                "insert into scheduler_source_events(event_id,topic,partition_key,aggregate_id,aggregate_revision,schema_version,causation_id,idempotency_key,payload_json,event_digest,occurred_at,admitted_at) values (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12)",
                params![event_id,text(&event,"topic"),text(&event,"partition_key"),text(&event,"aggregate_id"),integer(&event,"aggregate_revision"),integer(&event,"schema_version"),text(&event,"causation_id"),text(&event,"idempotency_key"),canonical_json(event.get("payload").unwrap_or(&json!({}))),event_digest,text(&event,"occurred_at"),now],
            ).map_err(|cause| db_error("scheduler_event_insert_failed", cause))?;
        }
        let mut statement = db.prepare("select * from scheduler_bindings where source_topic=?1 and status='active' order by binding_id")
            .map_err(|cause| db_error("scheduler_binding_query_failed", cause))?;
        let bindings = statement
            .query_map(params![text(&event, "topic")], binding_from_row)
            .map_err(|cause| db_error("scheduler_binding_query_failed", cause))?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(|cause| db_error("scheduler_binding_query_failed", cause))?;
        for binding in bindings {
            if !binding_matches(&binding, &event) {
                continue;
            }
            let binding_id = binding
                .get("binding_id")
                .and_then(Value::as_str)
                .unwrap_or("");
            let activation_id = stable_id(
                "activation",
                &json!({"binding_id":binding_id,"source_event_id":event_id}),
            );
            if query_activation(db, &activation_id)?.is_some() {
                continue;
            }
            let payload = event
                .get("payload")
                .and_then(Value::as_object)
                .cloned()
                .unwrap_or_default();
            let outcome = payload
                .get("outcome")
                .and_then(Value::as_str)
                .unwrap_or("default");
            let delay = delay_for(&binding, outcome, &payload);
            let occurred = parse_iso(text(&event, "occurred_at").as_str())?;
            let due_at = format_iso(occurred + Duration::milliseconds(delay));
            let blocked = outcome == "blocked";
            let partition_key =
                if binding.get("concurrency").and_then(Value::as_str) == Some("singleton") {
                    binding_id.to_string()
                } else {
                    text(&event, "partition_key")
                };
            db.execute(
                "insert into scheduler_activations(activation_id,binding_id,source_event_id,occurrence_key,target_sop_id,target_template_version,partition_key,due_at,status,attempt_count,lease_owner,lease_token,lease_expires_at,sop_run_id,terminal_outcome,last_error,created_at,updated_at) values (?1,?2,?3,?4,?5,?6,?7,?8,?9,0,null,null,null,null,null,?10,?11,?11)",
                params![activation_id,binding_id,event_id,format!("{binding_id}:{event_id}"),binding.get("target_sop_id").and_then(Value::as_str).unwrap_or(""),binding.get("target_template_version").and_then(Value::as_str).unwrap_or(""),partition_key,due_at,if blocked{"blocked"}else{"pending"},if blocked{Some("blocked_outcome_requires_explicit_unblock")}else{None},now],
            ).map_err(|cause| db_error("scheduler_activation_insert_failed", cause))?;
        }
        let activations = list_activations(db, None, None, Some(&event_id), None, 500)?;
        Ok(
            json!({"schema":"narada.scheduler.event_admission.v1","status":if existing_digest.is_some(){"replayed"}else{"admitted"},"event_id":event_id,"activation_count":activations.len(),"activations":activations}),
        )
    })
}

fn normalize_event(args: &Map<String, Value>) -> Result<Map<String, Value>, Value> {
    let occurred = parse_iso(&required(args, "occurred_at")?)?;
    let aggregate_revision = required_integer(args, "aggregate_revision")?;
    if aggregate_revision < 0 {
        return Err(error(
            "scheduler_event_aggregate_revision_invalid",
            "scheduler_event_aggregate_revision_invalid",
        ));
    }
    let schema_version = required_integer(args, "schema_version")?;
    if schema_version < 1 {
        return Err(error(
            "scheduler_event_schema_version_invalid",
            "scheduler_event_schema_version_invalid",
        ));
    }
    let payload = args
        .get("payload")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    bounded_json(
        &Value::Object(payload.clone()),
        "scheduler_event_payload",
        MAX_EVENT_BYTES,
    )?;
    let mut event = Map::new();
    for field in [
        "event_id",
        "topic",
        "partition_key",
        "aggregate_id",
        "causation_id",
        "idempotency_key",
    ] {
        event.insert(field.into(), json!(required(args, field)?));
    }
    event.insert("aggregate_revision".into(), json!(aggregate_revision));
    event.insert("schema_version".into(), json!(schema_version));
    event.insert("payload".into(), Value::Object(payload));
    event.insert("occurred_at".into(), json!(format_iso(occurred)));
    Ok(event)
}

fn binding_matches(binding: &Value, event: &Map<String, Value>) -> bool {
    let payload = event.get("payload").and_then(Value::as_object);
    if let Some(expected) = binding.get("source_sop_id").and_then(Value::as_str) {
        if payload
            .and_then(|value| value.get("sop_id"))
            .and_then(Value::as_str)
            != Some(expected)
        {
            return false;
        }
    }
    let outcomes = binding.get("terminal_outcomes").and_then(Value::as_array);
    if let Some(outcomes) = outcomes.filter(|values| !values.is_empty()) {
        let outcome = payload
            .and_then(|value| value.get("outcome"))
            .and_then(Value::as_str)
            .unwrap_or("");
        if !outcomes.iter().any(|value| value.as_str() == Some(outcome)) {
            return false;
        }
    }
    true
}

fn delay_for(binding: &Value, outcome: &str, payload: &Map<String, Value>) -> i64 {
    if outcome == "retryable_failure" {
        let attempt = payload
            .get("attempt")
            .and_then(Value::as_i64)
            .unwrap_or(1)
            .max(1)
            .min(31);
        let base = binding
            .get("retry_base_ms")
            .and_then(Value::as_i64)
            .unwrap_or(1_000);
        let cap = binding
            .get("retry_max_ms")
            .and_then(Value::as_i64)
            .unwrap_or(300_000);
        return base.saturating_mul(1_i64 << (attempt - 1)).min(cap);
    }
    binding
        .get("delay_by_outcome_ms")
        .and_then(Value::as_object)
        .and_then(|values| values.get(outcome))
        .and_then(Value::as_i64)
        .or_else(|| binding.get("default_delay_ms").and_then(Value::as_i64))
        .unwrap_or(0)
}

fn event_show(db: &Connection, args: &Map<String, Value>) -> Result<Value, Value> {
    let event_id = required(args, "event_id")?;
    let event = db
        .query_row(
            "select * from scheduler_source_events where event_id=?1",
            params![event_id],
            event_from_row,
        )
        .optional()
        .map_err(|cause| db_error("scheduler_event_query_failed", cause))?
        .ok_or_else(|| {
            error(
                "scheduler_source_event_not_found",
                &format!("scheduler_source_event_not_found:{event_id}"),
            )
        })?;
    Ok(json!({"schema":"narada.scheduler.source_event.v1","event":event}))
}

fn event_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Value> {
    let payload: String = row.get("payload_json")?;
    Ok(
        json!({"event_id":row.get::<_,String>("event_id")?,"topic":row.get::<_,String>("topic")?,"partition_key":row.get::<_,String>("partition_key")?,"aggregate_id":row.get::<_,String>("aggregate_id")?,"aggregate_revision":row.get::<_,i64>("aggregate_revision")?,"schema_version":row.get::<_,i64>("schema_version")?,"causation_id":row.get::<_,String>("causation_id")?,"idempotency_key":row.get::<_,String>("idempotency_key")?,"payload":serde_json::from_str::<Value>(&payload).unwrap_or_else(|_|json!({})),"occurred_at":row.get::<_,String>("occurred_at")?}),
    )
}

fn activation_list(db: &Connection, args: &Map<String, Value>) -> Result<Value, Value> {
    let limit = args
        .get("limit")
        .and_then(Value::as_i64)
        .unwrap_or(100)
        .clamp(1, 500);
    let activations = list_activations(
        db,
        args.get("status").and_then(Value::as_str),
        args.get("binding_id").and_then(Value::as_str),
        args.get("source_event_id").and_then(Value::as_str),
        args.get("sop_run_id").and_then(Value::as_str),
        limit,
    )?;
    Ok(
        json!({"schema":"narada.scheduler.activation_list.v1","count":activations.len(),"activations":activations}),
    )
}

fn list_activations(
    db: &Connection,
    status: Option<&str>,
    binding_id: Option<&str>,
    source_event_id: Option<&str>,
    sop_run_id: Option<&str>,
    limit: i64,
) -> Result<Vec<Value>, Value> {
    let mut clauses = Vec::new();
    let mut values = Vec::<SqlValue>::new();
    for (column, value) in [
        ("status", status),
        ("binding_id", binding_id),
        ("source_event_id", source_event_id),
        ("sop_run_id", sop_run_id),
    ] {
        if let Some(value) = value {
            clauses.push(format!("{column}=?"));
            values.push(SqlValue::Text(value.to_string()));
        }
    }
    values.push(SqlValue::Integer(limit));
    let sql = format!(
        "select * from scheduler_activations {} order by due_at,activation_id limit ?",
        if clauses.is_empty() {
            String::new()
        } else {
            format!("where {}", clauses.join(" and "))
        }
    );
    let mut statement = db
        .prepare(&sql)
        .map_err(|cause| db_error("scheduler_activation_query_failed", cause))?;
    let rows = statement
        .query_map(params_from_iter(values.iter()), activation_from_row)
        .map_err(|cause| db_error("scheduler_activation_query_failed", cause))?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(|cause| db_error("scheduler_activation_query_failed", cause))?;
    Ok(rows)
}

fn query_activation(db: &Connection, id: &str) -> Result<Option<Value>, Value> {
    db.query_row(
        "select * from scheduler_activations where activation_id=?1",
        params![id],
        activation_from_row,
    )
    .optional()
    .map_err(|cause| db_error("scheduler_activation_query_failed", cause))
}
fn require_activation(db: &Connection, id: &str) -> Result<Value, Value> {
    query_activation(db, id)?.ok_or_else(|| {
        error(
            "scheduler_activation_not_found",
            "scheduler_activation_not_found",
        )
    })
}

fn activation_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Value> {
    Ok(json!({
        "activation_id":row.get::<_,String>("activation_id")?,"binding_id":row.get::<_,String>("binding_id")?,"source_event_id":row.get::<_,String>("source_event_id")?,"occurrence_key":row.get::<_,String>("occurrence_key")?,
        "target_sop_id":row.get::<_,String>("target_sop_id")?,"target_template_version":row.get::<_,String>("target_template_version")?,"partition_key":row.get::<_,String>("partition_key")?,"due_at":row.get::<_,String>("due_at")?,
        "status":row.get::<_,String>("status")?,"attempt_count":row.get::<_,i64>("attempt_count")?,"lease_owner":row.get::<_,Option<String>>("lease_owner")?,"lease_token":row.get::<_,Option<String>>("lease_token")?,"lease_expires_at":row.get::<_,Option<String>>("lease_expires_at")?,
        "sop_run_id":row.get::<_,Option<String>>("sop_run_id")?,"terminal_outcome":row.get::<_,Option<String>>("terminal_outcome")?,"last_error":row.get::<_,Option<String>>("last_error")?,"created_at":row.get::<_,String>("created_at")?,"updated_at":row.get::<_,String>("updated_at")?
    }))
}

fn activation_claim(db: &Connection, args: &Map<String, Value>) -> Result<Value, Value> {
    let consumer = required(args, "consumer_id")?;
    let lease_ms = args
        .get("lease_ms")
        .and_then(Value::as_i64)
        .unwrap_or(30_000);
    if !(1_000..=300_000).contains(&lease_ms) {
        return Err(error(
            "scheduler_activation_lease_ms_invalid",
            "scheduler_activation_lease_ms_invalid",
        ));
    }
    let now_time = OffsetDateTime::now_utc();
    let now = format_iso(now_time);
    let expires = format_iso(now_time + Duration::milliseconds(lease_ms));
    transaction(db, || {
        db.execute("update scheduler_activations set status='terminal',lease_owner=null,lease_token=null,lease_expires_at=null,terminal_outcome='cancelled_binding_paused',last_error='binding_paused_after_lease_expiry',updated_at=?1 where status='leased' and lease_expires_at<=?1 and binding_id in (select binding_id from scheduler_bindings where status='paused')",params![now]).map_err(|cause|db_error("scheduler_activation_recovery_failed",cause))?;
        db.execute("update scheduler_activations set status='pending',lease_owner=null,lease_token=null,lease_expires_at=null,attempt_count=attempt_count+1,last_error='lease_expired',updated_at=?1 where status='leased' and lease_expires_at<=?1 and binding_id in (select binding_id from scheduler_bindings where status in ('active','retired'))",params![now]).map_err(|cause|db_error("scheduler_activation_recovery_failed",cause))?;
        let id:Option<String>=db.query_row("select activation.activation_id from scheduler_activations activation join scheduler_bindings binding on binding.binding_id=activation.binding_id where activation.status='pending' and activation.due_at<=?1 and binding.status in ('active','retired') and not exists (select 1 from scheduler_activations active where active.binding_id=activation.binding_id and active.partition_key=activation.partition_key and active.activation_id<>activation.activation_id and active.status in ('leased','admitted')) order by activation.due_at,activation.activation_id limit 1",params![now],|row|row.get(0)).optional().map_err(|cause|db_error("scheduler_activation_claim_failed",cause))?;
        let activation = if let Some(id) = id {
            let token = Uuid::new_v4().to_string();
            db.execute("update scheduler_activations set status='leased',lease_owner=?1,lease_token=?2,lease_expires_at=?3,updated_at=?4 where activation_id=?5 and status='pending'",params![consumer,token,expires,now,id]).map_err(|cause|db_error("scheduler_activation_claim_failed",cause))?;
            query_activation(db, &id)?
        } else {
            None
        };
        Ok(json!({"schema":"narada.scheduler.activation_claim.v1","activation":activation}))
    })
}

fn require_leased(db: &Connection, id: &str, consumer: &str, token: &str) -> Result<Value, Value> {
    let activation = require_activation(db, id)?;
    let status = activation
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or("");
    if status != "leased" {
        return Err(error(
            "scheduler_activation_not_leased",
            &format!("scheduler_activation_not_leased:{status}"),
        ));
    }
    if activation.get("lease_owner").and_then(Value::as_str) != Some(consumer) {
        return Err(error(
            "scheduler_activation_lease_owner_mismatch",
            "scheduler_activation_lease_owner_mismatch",
        ));
    }
    if activation.get("lease_token").and_then(Value::as_str) != Some(token) {
        return Err(error(
            "scheduler_activation_lease_token_mismatch",
            "scheduler_activation_lease_token_mismatch",
        ));
    }
    let expires = activation
        .get("lease_expires_at")
        .and_then(Value::as_str)
        .unwrap_or("");
    if expires <= now_iso().as_str() {
        return Err(error(
            "scheduler_activation_lease_expired",
            "scheduler_activation_lease_expired",
        ));
    }
    Ok(activation)
}

fn record_receipt(
    db: &Connection,
    activation_id: &str,
    kind: &str,
    receipt_id: &str,
    receipt: &Value,
    now: &str,
) -> Result<(), Value> {
    let encoded = bounded_json(receipt, "scheduler_activation_receipt", MAX_EVENT_BYTES)?;
    db.execute("insert into scheduler_activation_receipts(activation_id,receipt_kind,receipt_id,receipt_json,recorded_at) values (?1,?2,?3,?4,?5)",params![activation_id,kind,receipt_id,encoded,now])
        .map_err(|cause|db_error("scheduler_activation_receipt_failed",cause))?;
    Ok(())
}

fn activation_admit_sop(db: &Connection, args: &Map<String, Value>) -> Result<Value, Value> {
    let id = required(args, "activation_id")?;
    let consumer = required(args, "consumer_id")?;
    let token = required(args, "lease_token")?;
    let sop_run_id = required(args, "sop_run_id")?;
    let receipt_id = required(args, "receipt_id")?;
    let receipt = args.get("receipt").cloned().unwrap_or_else(|| json!({}));
    let now = now_iso();
    transaction(db, || {
        require_leased(db, &id, &consumer, &token)?;
        db.execute("update scheduler_activations set status='admitted',sop_run_id=?1,lease_owner=null,lease_token=null,lease_expires_at=null,updated_at=?2 where activation_id=?3",params![sop_run_id,now,id]).map_err(|cause|db_error("scheduler_activation_admit_failed",cause))?;
        record_receipt(db, &id, "sop_admission", &receipt_id, &receipt, &now)?;
        Ok(
            json!({"schema":"narada.scheduler.activation.v1","activation":require_activation(db,&id)?}),
        )
    })
}

fn activation_fail(db: &Connection, args: &Map<String, Value>) -> Result<Value, Value> {
    let id = required(args, "activation_id")?;
    let consumer = required(args, "consumer_id")?;
    let token = required(args, "lease_token")?;
    let retryable = args.get("retryable").and_then(Value::as_bool) == Some(true);
    let failure = required(args, "error")?;
    let now_time = OffsetDateTime::now_utc();
    let now = format_iso(now_time);
    transaction(db, || {
        let activation = require_leased(db, &id, &consumer, &token)?;
        let binding = require_binding(
            db,
            activation
                .get("binding_id")
                .and_then(Value::as_str)
                .unwrap_or(""),
        )?;
        let attempt = activation
            .get("attempt_count")
            .and_then(Value::as_i64)
            .unwrap_or(0)
            + 1;
        let max = binding
            .get("max_attempts")
            .and_then(Value::as_i64)
            .unwrap_or(5);
        let retry = retryable && attempt < max;
        let base = binding
            .get("retry_base_ms")
            .and_then(Value::as_i64)
            .unwrap_or(1_000);
        let cap = binding
            .get("retry_max_ms")
            .and_then(Value::as_i64)
            .unwrap_or(300_000);
        let exponent = (attempt - 1).clamp(0, 30);
        let delay = base.saturating_mul(1_i64 << exponent).min(cap);
        let due = format_iso(now_time + Duration::milliseconds(delay));
        let bounded = failure.chars().take(MAX_ERROR_BYTES).collect::<String>();
        db.execute("update scheduler_activations set status=?1,attempt_count=?2,lease_owner=null,lease_token=null,lease_expires_at=null,due_at=?3,last_error=?4,updated_at=?5 where activation_id=?6",params![if retry{"pending"}else{"blocked"},attempt,due,bounded,now,id]).map_err(|cause|db_error("scheduler_activation_fail_failed",cause))?;
        Ok(
            json!({"schema":"narada.scheduler.activation.v1","activation":require_activation(db,&id)?}),
        )
    })
}

fn activation_resolve(db: &Connection, args: &Map<String, Value>) -> Result<Value, Value> {
    let activation_id = args
        .get("activation_id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|v| !v.is_empty());
    let sop_run_id = args
        .get("sop_run_id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|v| !v.is_empty());
    if activation_id.is_none() && sop_run_id.is_none() {
        return Err(error(
            "scheduler_activation_not_found",
            "scheduler_activation_not_found",
        ));
    }
    let outcome = required(args, "outcome")?;
    let receipt_id = required(args, "receipt_id")?;
    let receipt = args.get("receipt").cloned().unwrap_or_else(|| json!({}));
    let now = now_iso();
    transaction(db, || {
        let activation = if let Some(id) = activation_id {
            query_activation(db, id)?
        } else {
            db.query_row(
                "select * from scheduler_activations where sop_run_id=?1",
                params![sop_run_id],
                activation_from_row,
            )
            .optional()
            .map_err(|cause| db_error("scheduler_activation_query_failed", cause))?
        }
        .ok_or_else(|| {
            error(
                "scheduler_activation_not_found",
                "scheduler_activation_not_found",
            )
        })?;
        let id = activation
            .get("activation_id")
            .and_then(Value::as_str)
            .unwrap_or("");
        let status = activation
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or("");
        if status == "terminal" {
            let existing:Option<String>=db.query_row("select receipt_id from scheduler_activation_receipts where activation_id=?1 and receipt_kind='terminal'",params![id],|row|row.get(0)).optional().map_err(|cause|db_error("scheduler_activation_receipt_query_failed",cause))?;
            if existing.as_deref() == Some(receipt_id.as_str()) {
                return Ok(
                    json!({"schema":"narada.scheduler.activation.v1","activation":activation}),
                );
            }
            return Err(error(
                "scheduler_activation_terminal_conflict",
                "scheduler_activation_terminal_conflict",
            ));
        }
        if status != "admitted" {
            return Err(error(
                "scheduler_activation_not_admitted",
                &format!("scheduler_activation_not_admitted:{status}"),
            ));
        }
        db.execute("update scheduler_activations set status='terminal',terminal_outcome=?1,updated_at=?2 where activation_id=?3",params![outcome,now,id]).map_err(|cause|db_error("scheduler_activation_resolve_failed",cause))?;
        record_receipt(db, id, "terminal", &receipt_id, &receipt, &now)?;
        Ok(
            json!({"schema":"narada.scheduler.activation.v1","activation":require_activation(db,id)?}),
        )
    })
}

fn activation_unblock(db: &Connection, args: &Map<String, Value>) -> Result<Value, Value> {
    let id = required(args, "activation_id")?;
    let now = now_iso();
    let due = if let Some(value) = args.get("due_at").and_then(Value::as_str) {
        format_iso(parse_iso(value)?)
    } else {
        now.clone()
    };
    transaction(db, || {
        let activation = require_activation(db, &id)?;
        let status = activation
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or("");
        if status != "blocked" {
            return Err(error(
                "scheduler_activation_not_blocked",
                &format!("scheduler_activation_not_blocked:{status}"),
            ));
        }
        db.execute("update scheduler_activations set status='pending',due_at=?1,last_error=null,updated_at=?2 where activation_id=?3",params![due,now,id]).map_err(|cause|db_error("scheduler_activation_unblock_failed",cause))?;
        Ok(
            json!({"schema":"narada.scheduler.activation.v1","activation":require_activation(db,&id)?}),
        )
    })
}

fn transaction<F>(db: &Connection, action: F) -> Result<Value, Value>
where
    F: FnOnce() -> Result<Value, Value>,
{
    db.execute_batch("begin immediate")
        .map_err(|cause| db_error("scheduler_activation_transaction_failed", cause))?;
    match action() {
        Ok(value) => {
            db.execute_batch("commit")
                .map_err(|cause| db_error("scheduler_activation_transaction_failed", cause))?;
            Ok(value)
        }
        Err(problem) => {
            let _ = db.execute_batch("rollback");
            Err(problem)
        }
    }
}

fn required(args: &Map<String, Value>, field: &str) -> Result<String, Value> {
    args.get(field)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
        .ok_or_else(|| error(&format!("{field}_required"), &format!("{field}_required")))
}
fn required_string(args: &Map<String, Value>, field: &str) -> Result<String, Value> {
    required(args, field)
}
fn required_integer(args: &Map<String, Value>, field: &str) -> Result<i64, Value> {
    args.get(field)
        .and_then(Value::as_i64)
        .ok_or_else(|| error(&format!("{field}_required"), &format!("{field}_required")))
}
fn nonnegative(args: &Map<String, Value>, field: &str, fallback: i64) -> Result<i64, Value> {
    let value = args.get(field).and_then(Value::as_i64).unwrap_or(fallback);
    if value < 0 {
        Err(error(
            &format!("{field}_invalid"),
            &format!("{field}_invalid"),
        ))
    } else {
        Ok(value)
    }
}
fn text(args: &Map<String, Value>, field: &str) -> String {
    args.get(field)
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string()
}
fn optional_text(args: &Map<String, Value>, field: &str) -> Option<String> {
    args.get(field)
        .and_then(Value::as_str)
        .map(ToString::to_string)
}
fn integer(args: &Map<String, Value>, field: &str) -> i64 {
    args.get(field).and_then(Value::as_i64).unwrap_or(0)
}

fn parse_iso(value: &str) -> Result<OffsetDateTime, Value> {
    OffsetDateTime::parse(value, &Rfc3339)
        .map_err(|_| error("scheduler_datetime_invalid", "scheduler_datetime_invalid"))
}
fn format_iso(value: OffsetDateTime) -> String {
    value
        .to_offset(time::UtcOffset::UTC)
        .format(format_description!(
            "[year]-[month]-[day]T[hour]:[minute]:[second].[subsecond digits:3]Z"
        ))
        .expect("timestamp")
}
fn now_iso() -> String {
    format_iso(OffsetDateTime::now_utc())
}

fn canonicalize(value: &Value) -> Value {
    match value {
        Value::Array(values) => Value::Array(values.iter().map(canonicalize).collect()),
        Value::Object(object) => {
            let mut keys = object.keys().collect::<Vec<_>>();
            keys.sort();
            let mut normalized = Map::new();
            for key in keys {
                normalized.insert(key.clone(), canonicalize(&object[key]));
            }
            Value::Object(normalized)
        }
        other => other.clone(),
    }
}
fn canonical_json(value: &Value) -> String {
    serde_json::to_string(&canonicalize(value)).unwrap_or_else(|_| "null".to_string())
}
fn bounded_json(value: &Value, field: &str, max: usize) -> Result<String, Value> {
    let encoded = canonical_json(value);
    if encoded.len() > max {
        Err(error(
            &format!("{field}_too_large"),
            &format!("{field}_too_large"),
        ))
    } else {
        Ok(encoded)
    }
}
fn digest(value: &Value) -> String {
    let bytes = Sha256::digest(canonical_json(value).as_bytes());
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}
fn stable_id(prefix: &str, value: &Value) -> String {
    format!("{prefix}_{}", &digest(value)[..32])
}
fn db_error(code: &str, cause: rusqlite::Error) -> Value {
    error(code, &format!("{code}:{cause}"))
}
fn error(code: &str, message: &str) -> Value {
    json!({"schema":"narada.scheduler_mcp.error.v1","code":code,"message":message})
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> PathBuf {
        let root = std::env::temp_dir().join(format!("narada-scheduler-native-{}", Uuid::new_v4()));
        fs::create_dir_all(&root).expect("root");
        prepare(&root).expect("prepare");
        root
    }
    fn call(root: &Path, name: &str, args: Value) -> Result<Value, Value> {
        call_tool(name, args.as_object().expect("args"), root)
    }
    fn binding() -> Value {
        json!({"binding_id":"mailbox-sync-continuation","trigger_kind":"completion","source_topic":"sop.run.terminal.v1","source_sop_id":"sonar.mailbox-sync","terminal_outcomes":["synced","retryable_failure","blocked"],"target_sop_id":"sonar.mailbox-sync","target_template_version":"v1","concurrency":"singleton","delay_by_outcome_ms":{"synced":0},"retry_base_ms":0,"retry_max_ms":1000,"max_attempts":3})
    }
    fn event(id: &str, outcome: &str) -> Value {
        json!({"event_id":id,"topic":"sop.run.terminal.v1","partition_key":"sonar.mailbox-sync","aggregate_id":format!("run-{id}"),"aggregate_revision":1,"schema_version":1,"causation_id":id,"idempotency_key":id,"payload":{"sop_id":"sonar.mailbox-sync","outcome":outcome},"occurred_at":now_iso()})
    }
    #[test]
    fn prepare_and_inspect_are_explicit() {
        let root = std::env::temp_dir().join(format!("narada-scheduler-native-{}", Uuid::new_v4()));
        assert_eq!(doctor(&root)["preparation"]["status"], "missing");
        prepare(&root).expect("prepare");
        assert_eq!(doctor(&root)["preparation"]["status"], "prepared");
        let _ = fs::remove_dir_all(root);
    }
    #[test]
    fn events_are_replay_safe_and_revision_guarded() {
        let root = fixture();
        let created = call(&root, "scheduler_binding_upsert", binding()).expect("binding");
        assert_eq!(created["binding"]["revision"], 1);
        let admitted_event = event("event-1", "synced");
        let first = call(&root, "scheduler_event_admit", admitted_event.clone()).expect("admit");
        assert_eq!(first["status"], "admitted");
        assert_eq!(first["activation_count"], 1);
        let replay = call(&root, "scheduler_event_admit", admitted_event).expect("replay");
        assert_eq!(replay["status"], "replayed");
        assert!(call(&root, "scheduler_event_admit", event("event-1", "blocked")).is_err());
        let mut changed = binding().as_object().unwrap().clone();
        changed.insert("default_delay_ms".into(), json!(1));
        assert!(call_tool("scheduler_binding_upsert", &changed, &root).is_err());
        let _ = fs::remove_dir_all(root);
    }
    #[test]
    fn lease_receipts_hold_singleton_until_terminal() {
        let root = fixture();
        call(&root, "scheduler_binding_upsert", binding()).expect("binding");
        call(&root, "scheduler_event_admit", event("event-1", "synced")).expect("event1");
        call(&root, "scheduler_event_admit", event("event-2", "synced")).expect("event2");
        let claim = call(
            &root,
            "scheduler_activation_claim",
            json!({"consumer_id":"dispatcher","lease_ms":30000}),
        )
        .expect("claim")["activation"]
            .clone();
        let id = claim["activation_id"].as_str().unwrap();
        let token = claim["lease_token"].as_str().unwrap();
        call(&root,"scheduler_activation_admit_sop",json!({"activation_id":id,"consumer_id":"dispatcher","lease_token":token,"sop_run_id":"run-1","receipt_id":"admit-1","receipt":{}})).expect("admit sop");
        assert!(call(
            &root,
            "scheduler_activation_claim",
            json!({"consumer_id":"dispatcher"})
        )
        .expect("blocked")["activation"]
            .is_null());
        call(
            &root,
            "scheduler_activation_resolve",
            json!({"sop_run_id":"run-1","outcome":"synced","receipt_id":"terminal-1","receipt":{}}),
        )
        .expect("resolve");
        assert!(!call(
            &root,
            "scheduler_activation_claim",
            json!({"consumer_id":"dispatcher"})
        )
        .expect("next")["activation"]
            .is_null());
        let _ = fs::remove_dir_all(root);
    }
    #[test]
    fn blocked_activation_requires_explicit_unblock() {
        let root = fixture();
        call(&root, "scheduler_binding_upsert", binding()).expect("binding");
        let admitted = call(
            &root,
            "scheduler_event_admit",
            event("event-blocked", "blocked"),
        )
        .expect("event");
        let id = admitted["activations"][0]["activation_id"]
            .as_str()
            .unwrap();
        assert_eq!(admitted["activations"][0]["status"], "blocked");
        let unblocked = call(
            &root,
            "scheduler_activation_unblock",
            json!({"activation_id":id}),
        )
        .expect("unblock");
        assert_eq!(unblocked["activation"]["status"], "pending");
        let _ = fs::remove_dir_all(root);
    }
}
