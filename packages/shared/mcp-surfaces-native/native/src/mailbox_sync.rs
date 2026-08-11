use rusqlite::{params, Connection, OptionalExtension, Transaction, TransactionBehavior};
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashSet};
use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::thread;
use std::time::{Duration, Instant};
use time::{format_description::well_known::Rfc3339, OffsetDateTime};
use uuid::Uuid;

const DOMAIN_SCHEMA: &str = "narada.domain_operation.v1";
const ARTIFACT_SCHEMA: &str = "narada.mailbox.sync_generation_artifact.v1";
const DOMAIN_DB_RELATIVE: &str = ".narada/runtime/mailbox-domain/mailbox-domain.db";
const MAX_CONFIG_BYTES: u64 = 10 * 1024 * 1024;
const MAX_RESPONSE_BYTES: u64 = 16 * 1024 * 1024;
const MAX_GRAPH_PAGES: usize = 100;
const MAX_GRAPH_RECORDS: usize = 10_000;
const LEASE_MS: i128 = 30_000;

#[derive(Clone)]
struct GraphConfig {
    auth_mode: Option<String>,
    mailbox_id: Option<String>,
    tenant_id: Option<String>,
    client_id: Option<String>,
    client_secret: Option<String>,
    user_id: String,
    base_url: String,
    configured_base_url: Option<String>,
    prefer_immutable_ids: bool,
}

#[derive(Clone)]
struct ScopeConfig {
    scope_id: String,
    root_dir: PathBuf,
    root_dir_text: String,
    graph: GraphConfig,
    included_container_refs: Vec<String>,
    included_item_kinds: Vec<String>,
    attachment_policy: String,
    body_policy: String,
    include_headers: bool,
    tombstones_enabled: bool,
    acquire_lock_timeout_ms: u64,
    cleanup_tmp_on_startup: bool,
}

#[derive(Clone)]
struct SourceRecord {
    record_id: String,
    ordinal: Option<String>,
    payload: Value,
    provenance: Value,
}

#[derive(Clone)]
struct SourceBatch {
    records: Vec<SourceRecord>,
    prior_checkpoint: Option<String>,
    next_checkpoint: Option<String>,
    has_more: bool,
    fetched_at: String,
}

#[derive(Clone)]
struct Generation {
    generation_id: String,
    _idempotency_key: String,
    request_fingerprint: String,
    scope_id: String,
    config_fingerprint: String,
    status: String,
    parent_cursor: Option<String>,
    next_cursor: Option<String>,
    batch_path: Option<String>,
    batch_sha256: Option<String>,
    batch_record_count: i64,
    receipt: Option<Value>,
    error_message: Option<String>,
    lease_token: Option<String>,
    _created_at: String,
    _updated_at: String,
    _completed_at: Option<String>,
}

#[derive(Clone)]
struct StagedRecord {
    record_id: String,
    ordinal: Option<String>,
    fact_id: String,
    event_kind: String,
    message_id: Option<String>,
    mailbox_id: Option<String>,
    conversation_id: Option<String>,
    source_version: Option<String>,
    application_status: String,
}

pub fn sync_generation(args: &Map<String, Value>, site_root: &Path) -> Result<Value, Value> {
    let idempotency_key = required_bounded(
        args,
        "idempotency_key",
        "mailbox_sync_idempotency_key_required",
        512,
    )?;
    let scope = load_scope(args, site_root)?;
    let config_fingerprint = sync_config_fingerprint(&scope);
    let request_fingerprint = fingerprint(&json!({
        "schema":"narada.mailbox.sync_generation_request.v1",
        "scope_id":scope.scope_id,
        "config_fingerprint":config_fingerprint,
    }));
    let generation_id = stable_id("mbg_", &idempotency_key);
    let mut db = open_domain_db(site_root)?;
    let now = now_iso_millis();
    let (claimed, lease_token) = claim_generation(
        &mut db,
        &generation_id,
        &idempotency_key,
        &request_fingerprint,
        &scope.scope_id,
        &config_fingerprint,
        &now,
    )?;
    if claimed.status == "completed" {
        return generation_operation(&claimed, true);
    }
    if claimed.status == "failed" {
        return Ok(blocked_generation_operation(&claimed, true));
    }
    let lease_token = lease_token
        .ok_or_else(|| error("mailbox_sync_lease_missing", "mailbox_sync_lease_missing"))?;

    let result = run_claimed_generation(&mut db, site_root, &scope, &generation_id, &lease_token);
    if result.is_err() {
        let _ = release_lease(
            &mut db,
            &scope.scope_id,
            &generation_id,
            &lease_token,
            &now_iso_millis(),
        );
    }
    result
}

fn run_claimed_generation(
    db: &mut Connection,
    site_root: &Path,
    scope: &ScopeConfig,
    generation_id: &str,
    lease_token: &str,
) -> Result<Value, Value> {
    let artifact_path = site_root
        .join(".narada/runtime/mailbox-domain/generations")
        .join(format!("{generation_id}.json"));
    let current_cursor = read_cursor(scope)?;
    let mut generation = require_generation(db, generation_id)?;
    if generation.status == "staged" {
        let _ = read_generation_artifact(&generation, &artifact_path)?;
        if generation.next_cursor.is_some() && current_cursor == generation.next_cursor {
            assert_lease(db, &scope.scope_id, generation_id, lease_token)?;
            db.execute(
                "UPDATE mailbox_sync_generation_records SET application_status='reconciled' WHERE generation_id=? AND application_status='staged'",
                params![generation_id],
            )
            .map_err(|e| error("mailbox_sync_reconcile_failed", &e.to_string()))?;
            generation = finalize_generation(db, generation_id, lease_token, &now_iso_millis())?;
            return generation_operation(&generation, true);
        }
        if generation.next_cursor.is_none() && generation_ready(db, generation_id)? {
            generation = finalize_generation(db, generation_id, lease_token, &now_iso_millis())?;
            return generation_operation(&generation, true);
        }
        if current_cursor != generation.parent_cursor {
            let code = format!("mailbox_sync_cursor_conflict:{generation_id}");
            return Err(error(&code, &code));
        }
    }

    let lock = match SyncLock::acquire(scope) {
        Ok(lock) => lock,
        Err(reason) => {
            release_lease(
                db,
                &scope.scope_id,
                generation_id,
                lease_token,
                &now_iso_millis(),
            )?;
            let code = format!("mailbox_sync_retryable:{reason}");
            return Err(error(&code, &code));
        }
    };
    if scope.cleanup_tmp_on_startup {
        cleanup_tmp(&scope.root_dir)?;
    }
    ensure_root_layout(&scope.root_dir)?;

    let batch = if generation.status == "staged" {
        read_generation_artifact(&generation, &artifact_path)?
    } else if artifact_path.is_file() {
        let batch = read_artifact_at(&artifact_path, generation_id)?;
        stage_batch(
            db,
            generation_id,
            lease_token,
            current_cursor.as_deref(),
            &batch,
            &artifact_path,
        )?;
        batch
    } else {
        renew_lease(db, &scope.scope_id, generation_id, lease_token)?;
        let batch = fetch_graph_batch(scope, current_cursor.as_deref(), || {
            renew_lease(db, &scope.scope_id, generation_id, lease_token)
        })?;
        if batch.prior_checkpoint.as_deref() != current_cursor.as_deref() {
            let code = format!("mailbox_sync_source_parent_cursor_mismatch:{generation_id}");
            return Err(error(&code, &code));
        }
        write_generation_artifact(&artifact_path, generation_id, &batch)?;
        stage_batch(
            db,
            generation_id,
            lease_token,
            current_cursor.as_deref(),
            &batch,
            &artifact_path,
        )?;
        batch
    };

    let processing = process_batch(db, scope, generation_id, lease_token, &batch);
    drop(lock);
    if let Err(failure) = processing {
        let message = failure
            .get("message")
            .and_then(Value::as_str)
            .or_else(|| failure.get("code").and_then(Value::as_str))
            .unwrap_or("mailbox_sync_fatal_failure");
        let failed = fail_generation(
            db,
            generation_id,
            lease_token,
            &bounded_error(message),
            &now_iso_millis(),
        )?;
        return Ok(blocked_generation_operation(&failed, false));
    }

    let staged = require_generation(db, generation_id)?;
    if staged.status != "staged" {
        let code = format!("mailbox_sync_batch_not_staged:{generation_id}");
        return Err(error(&code, &code));
    }
    let committed_cursor = read_cursor(scope)?;
    if staged.next_cursor.is_some() && committed_cursor != staged.next_cursor {
        let code = format!("mailbox_sync_cursor_not_committed:{generation_id}");
        return Err(error(&code, &code));
    }
    generation = finalize_generation(db, generation_id, lease_token, &now_iso_millis())?;
    generation_operation(&generation, false)
}

fn load_scope(args: &Map<String, Value>, site_root: &Path) -> Result<ScopeConfig, Value> {
    let config_argument = args
        .get("config_path")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("config/config.json");
    if config_argument.chars().count() > 1024 {
        return Err(error(
            "mailbox_string_argument_too_long",
            "mailbox_string_argument_too_long",
        ));
    }
    let config_candidate = PathBuf::from(config_argument);
    let config_path = if config_candidate.is_absolute() {
        config_candidate
    } else {
        site_root.join(config_candidate)
    };
    let site_canonical = fs::canonicalize(site_root)
        .map_err(|e| error("mailbox_site_root_invalid", &e.to_string()))?;
    let config_canonical = fs::canonicalize(&config_path)
        .map_err(|e| error("mailbox_config_read_failed", &e.to_string()))?;
    if !config_canonical.starts_with(&site_canonical) {
        return Err(error(
            "mailbox_config_path_outside_site",
            &format!(
                "mailbox_config_path_outside_site:{}",
                config_path.to_string_lossy()
            ),
        ));
    }
    if fs::metadata(&config_canonical)
        .map_err(|e| error("mailbox_config_read_failed", &e.to_string()))?
        .len()
        > MAX_CONFIG_BYTES
    {
        return Err(error(
            "mailbox_config_too_large",
            "mailbox_config_too_large",
        ));
    }
    let document: Value = serde_json::from_str(
        &fs::read_to_string(&config_canonical)
            .map_err(|e| error("mailbox_config_read_failed", &e.to_string()))?,
    )
    .map_err(|e| error("mailbox_config_invalid", &e.to_string()))?;
    let scopes = document
        .get("scopes")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            error(
                "mailbox_config_scopes_invalid",
                "mailbox_config_scopes_invalid",
            )
        })?;
    let requested = args
        .get("scope_id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let raw = if let Some(requested) = requested {
        scopes.iter().find(|scope| {
            scope
                .get("scope_id")
                .or_else(|| scope.get("id"))
                .or_else(|| scope.get("mailbox_id"))
                .and_then(Value::as_str)
                == Some(requested)
        })
    } else if scopes.len() == 1 {
        scopes.first()
    } else {
        None
    }
    .ok_or_else(|| {
        if let Some(requested) = requested {
            let code = format!("mailbox_scope_not_found:{requested}");
            error(&code, &code)
        } else {
            error("mailbox_scope_id_required", "mailbox_scope_id_required")
        }
    })?;
    normalize_scope(raw, site_root, &site_canonical)
}

fn normalize_scope(
    raw: &Value,
    site_root: &Path,
    site_canonical: &Path,
) -> Result<ScopeConfig, Value> {
    let object = raw
        .as_object()
        .ok_or_else(|| error("mailbox_scope_invalid", "mailbox_scope_invalid"))?;
    let scope_id = object
        .get("scope_id")
        .or_else(|| object.get("id"))
        .or_else(|| object.get("mailbox_id"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| error("mailbox_scope_id_required", "mailbox_scope_id_required"))?
        .to_string();
    if scope_id.chars().count() > 256 {
        return Err(error(
            "mailbox_string_argument_too_long",
            "mailbox_string_argument_too_long",
        ));
    }
    let root_text = object
        .get("root_dir")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| error("mailbox_scope_root_required", "mailbox_scope_root_required"))?;
    let candidate = PathBuf::from(root_text);
    let root_dir = if candidate.is_absolute() {
        candidate
    } else {
        site_root.join(candidate)
    };
    fs::create_dir_all(&root_dir)
        .map_err(|e| error("mailbox_scope_root_invalid", &e.to_string()))?;
    let root_canonical = fs::canonicalize(&root_dir)
        .map_err(|e| error("mailbox_scope_root_invalid", &e.to_string()))?;
    if !root_canonical.starts_with(site_canonical) {
        return Err(error(
            "mailbox_scope_root_outside_site",
            &format!(
                "mailbox_scope_root_outside_site:{}",
                root_dir.to_string_lossy()
            ),
        ));
    }

    let legacy_graph = object.get("graph").and_then(Value::as_object);
    let source_graph = object
        .get("sources")
        .and_then(Value::as_array)
        .and_then(|sources| {
            sources
                .iter()
                .find(|source| source.get("type").and_then(Value::as_str) == Some("graph"))
        })
        .and_then(Value::as_object);
    let graph = legacy_graph.or(source_graph).ok_or_else(|| {
        let code = format!("mailbox_scope_graph_source_required:{scope_id}");
        error(&code, &code)
    })?;
    let graph_string = |key: &str| {
        graph
            .get(key)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToString::to_string)
    };
    let user_id = graph_string("user_id").ok_or_else(|| {
        let code = format!("mailbox_scope_graph_source_required:{scope_id}");
        error(&code, &code)
    })?;
    let configured_base_url = graph_string("base_url");
    let base_url = configured_base_url
        .clone()
        .unwrap_or_else(|| "https://graph.microsoft.com/v1.0".to_string())
        .trim_end_matches('/')
        .to_string();
    validate_graph_base_url(&base_url)?;
    let filter = object.get("scope").and_then(Value::as_object);
    let included_container_refs = string_array(
        filter.and_then(|value| value.get("included_container_refs")),
        &["inbox", "sentitems", "drafts", "archive"],
        "mailbox_scope_container_refs_invalid",
    )?;
    if included_container_refs.is_empty() {
        return Err(error(
            "mailbox_scope_container_refs_invalid",
            "mailbox_scope_container_refs_invalid",
        ));
    }
    let included_item_kinds = string_array(
        filter.and_then(|value| value.get("included_item_kinds")),
        &["message"],
        "mailbox_scope_item_kinds_invalid",
    )?;
    let normalize = object.get("normalize").and_then(Value::as_object);
    let attachment_policy =
        optional_string(normalize.and_then(|value| value.get("attachment_policy")))
            .unwrap_or_else(|| "metadata_only".to_string());
    if !matches!(
        attachment_policy.as_str(),
        "exclude" | "metadata_only" | "include_content"
    ) {
        return Err(error(
            "mailbox_attachment_policy_invalid",
            "mailbox_attachment_policy_invalid",
        ));
    }
    let body_policy = optional_string(normalize.and_then(|value| value.get("body_policy")))
        .unwrap_or_else(|| "text_only".to_string());
    if !matches!(
        body_policy.as_str(),
        "original"
            | "best_effort"
            | "plain_text_only"
            | "text_only"
            | "html_only"
            | "text_and_html"
    ) {
        return Err(error(
            "mailbox_body_policy_invalid",
            "mailbox_body_policy_invalid",
        ));
    }
    let runtime = object.get("runtime").and_then(Value::as_object);
    let acquire_lock_timeout_ms = runtime
        .and_then(|value| value.get("acquire_lock_timeout_ms"))
        .and_then(Value::as_u64)
        .unwrap_or(30_000)
        .min(300_000);
    Ok(ScopeConfig {
        scope_id,
        root_dir_text: normalized_path_text(&root_dir),
        root_dir,
        graph: GraphConfig {
            auth_mode: graph_string("auth_mode"),
            mailbox_id: graph_string("mailbox_id"),
            tenant_id: graph_string("tenant_id"),
            client_id: graph_string("client_id"),
            client_secret: graph_string("client_secret"),
            user_id,
            base_url,
            configured_base_url,
            prefer_immutable_ids: graph
                .get("prefer_immutable_ids")
                .and_then(Value::as_bool)
                .unwrap_or(true),
        },
        included_container_refs,
        included_item_kinds,
        attachment_policy,
        body_policy,
        include_headers: normalize
            .and_then(|value| value.get("include_headers"))
            .and_then(Value::as_bool)
            .unwrap_or(false),
        tombstones_enabled: normalize
            .and_then(|value| value.get("tombstones_enabled"))
            .and_then(Value::as_bool)
            .unwrap_or(true),
        acquire_lock_timeout_ms,
        cleanup_tmp_on_startup: runtime
            .and_then(|value| value.get("cleanup_tmp_on_startup"))
            .and_then(Value::as_bool)
            .unwrap_or(true),
    })
}

fn sync_config_fingerprint(scope: &ScopeConfig) -> String {
    let mut source = Map::new();
    source.insert("type".to_string(), json!("graph"));
    if let Some(value) = &scope.graph.mailbox_id {
        source.insert("mailbox_id".to_string(), json!(value));
    }
    source.insert("user_id".to_string(), json!(scope.graph.user_id));
    if let Some(value) = &scope.graph.configured_base_url {
        source.insert("base_url".to_string(), json!(value));
    }
    source.insert(
        "prefer_immutable_ids".to_string(),
        json!(scope.graph.prefer_immutable_ids),
    );
    fingerprint(&json!({
        "schema":"narada.mailbox.sync_config.v1",
        "scope_id":scope.scope_id,
        "root_dir":scope.root_dir_text,
        "source":Value::Object(source),
        "scope":{
            "included_container_refs":scope.included_container_refs,
            "included_item_kinds":scope.included_item_kinds,
        },
        "normalize":{
            "attachment_policy":scope.attachment_policy,
            "body_policy":scope.body_policy,
            "include_headers":scope.include_headers,
            "tombstones_enabled":scope.tombstones_enabled,
        },
    }))
}

fn open_domain_db(site_root: &Path) -> Result<Connection, Value> {
    let path = site_root.join(DOMAIN_DB_RELATIVE);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|e| error("mailbox_domain_store_directory_failed", &e.to_string()))?;
    }
    let db = Connection::open(path)
        .map_err(|e| error("mailbox_domain_store_open_failed", &e.to_string()))?;
    db.busy_timeout(Duration::from_millis(5_000))
        .map_err(|e| error("mailbox_domain_store_pragma_failed", &e.to_string()))?;
    db.pragma_update(None, "journal_mode", "WAL")
        .map_err(|e| error("mailbox_domain_store_pragma_failed", &e.to_string()))?;
    db.pragma_update(None, "foreign_keys", true)
        .map_err(|e| error("mailbox_domain_store_pragma_failed", &e.to_string()))?;
    init_domain_schema(&db)?;
    Ok(db)
}

fn init_domain_schema(db: &Connection) -> Result<(), Value> {
    db.execute_batch(
        r#"
        CREATE TABLE IF NOT EXISTS mailbox_sync_generations(
          generation_id TEXT PRIMARY KEY,
          idempotency_key TEXT NOT NULL UNIQUE,
          request_fingerprint TEXT NOT NULL,
          scope_id TEXT NOT NULL,
          config_fingerprint TEXT NOT NULL,
          status TEXT NOT NULL CHECK(status IN ('accepted','staged','completed','failed')),
          parent_cursor TEXT,
          next_cursor TEXT,
          batch_path TEXT,
          batch_sha256 TEXT,
          batch_record_count INTEGER NOT NULL DEFAULT 0,
          staged_at TEXT,
          receipt_json TEXT,
          error_message TEXT,
          lease_token TEXT,
          lease_expires_at TEXT,
          created_at TEXT NOT NULL,
          updated_at TEXT NOT NULL,
          completed_at TEXT
        );
        CREATE TABLE IF NOT EXISTS mailbox_sync_generation_records(
          generation_id TEXT NOT NULL REFERENCES mailbox_sync_generations(generation_id),
          record_id TEXT NOT NULL,
          ordinal TEXT,
          fact_id TEXT NOT NULL,
          event_kind TEXT NOT NULL,
          message_id TEXT,
          mailbox_id TEXT,
          conversation_id TEXT,
          source_version TEXT,
          application_status TEXT NOT NULL CHECK(application_status IN ('staged','already_applied','projected','not_applied','reconciled')),
          PRIMARY KEY(generation_id, record_id)
        );
        CREATE TABLE IF NOT EXISTS mailbox_sync_scope_leases(
          scope_id TEXT PRIMARY KEY,
          generation_id TEXT NOT NULL REFERENCES mailbox_sync_generations(generation_id),
          lease_token TEXT NOT NULL,
          expires_at TEXT NOT NULL,
          updated_at TEXT NOT NULL
        );
        CREATE TABLE IF NOT EXISTS mailbox_message_observations(
          observation_id TEXT PRIMARY KEY,
          mailbox_id TEXT NOT NULL,
          message_id TEXT NOT NULL,
          first_generation_id TEXT NOT NULL REFERENCES mailbox_sync_generations(generation_id),
          first_fact_id TEXT NOT NULL,
          observed_at TEXT NOT NULL,
          UNIQUE(mailbox_id, message_id)
        );
        CREATE TABLE IF NOT EXISTS mailbox_outbox(
          event_id TEXT PRIMARY KEY,
          scope_id TEXT NOT NULL,
          topic TEXT NOT NULL,
          aggregate_id TEXT NOT NULL,
          aggregate_revision INTEGER NOT NULL,
          schema_version INTEGER NOT NULL,
          causation_id TEXT NOT NULL,
          idempotency_key TEXT NOT NULL UNIQUE,
          partition_key TEXT NOT NULL,
          occurred_at TEXT NOT NULL,
          payload_json TEXT NOT NULL
        );
        CREATE TABLE IF NOT EXISTS mailbox_outbox_consumers(
          consumer_id TEXT PRIMARY KEY,
          scope_id TEXT,
          topics_json TEXT,
          start_at TEXT NOT NULL,
          created_at TEXT NOT NULL
        );
        CREATE TABLE IF NOT EXISTS mailbox_outbox_receipts(
          consumer_id TEXT NOT NULL REFERENCES mailbox_outbox_consumers(consumer_id),
          event_id TEXT NOT NULL REFERENCES mailbox_outbox(event_id),
          receipt_fingerprint TEXT NOT NULL,
          receipt_json TEXT NOT NULL,
          acknowledged_at TEXT NOT NULL,
          PRIMARY KEY(consumer_id, event_id)
        );
        CREATE TABLE IF NOT EXISTS mailbox_admission_receipts(
          admission_id TEXT PRIMARY KEY,
          idempotency_key TEXT NOT NULL UNIQUE,
          request_fingerprint TEXT NOT NULL,
          scope_id TEXT NOT NULL,
          fact_id TEXT NOT NULL,
          policy_version TEXT NOT NULL,
          decision_json TEXT NOT NULL,
          created_at TEXT NOT NULL
        );
        CREATE TABLE IF NOT EXISTS mailbox_reconciliation_operations(
          operation_id TEXT PRIMARY KEY,
          idempotency_key TEXT NOT NULL UNIQUE,
          request_fingerprint TEXT NOT NULL,
          scope_id TEXT NOT NULL,
          generation_id TEXT NOT NULL REFERENCES mailbox_sync_generations(generation_id),
          result_json TEXT NOT NULL,
          created_at TEXT NOT NULL
        );
        CREATE INDEX IF NOT EXISTS mailbox_outbox_order_idx ON mailbox_outbox(occurred_at,event_id);
        CREATE INDEX IF NOT EXISTS mailbox_outbox_subscription_idx ON mailbox_outbox(scope_id,topic,occurred_at,event_id);
        CREATE INDEX IF NOT EXISTS mailbox_generation_scope_idx ON mailbox_sync_generations(scope_id,created_at);
        CREATE UNIQUE INDEX IF NOT EXISTS mailbox_admission_scope_fact_idx ON mailbox_admission_receipts(scope_id,fact_id);
        PRAGMA user_version=2;
        "#,
    )
    .map_err(|e| error("mailbox_domain_schema_failed", &e.to_string()))
}

fn claim_generation(
    db: &mut Connection,
    generation_id: &str,
    idempotency_key: &str,
    request_fingerprint: &str,
    scope_id: &str,
    config_fingerprint: &str,
    now: &str,
) -> Result<(Generation, Option<String>), Value> {
    let tx = db
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|e| error("mailbox_domain_transaction_failed", &e.to_string()))?;
    let existing_id: Option<String> = tx
        .query_row(
            "SELECT generation_id FROM mailbox_sync_generations WHERE idempotency_key=?",
            params![idempotency_key],
            |row| row.get(0),
        )
        .optional()
        .map_err(|e| error("mailbox_sync_generation_query_failed", &e.to_string()))?;
    if existing_id.is_none() {
        tx.execute(
            "INSERT INTO mailbox_sync_generations(generation_id,idempotency_key,request_fingerprint,scope_id,config_fingerprint,status,created_at,updated_at) VALUES (?,?,?,?,?,'accepted',?,?)",
            params![generation_id,idempotency_key,request_fingerprint,scope_id,config_fingerprint,now,now],
        )
        .map_err(|e| error("mailbox_sync_generation_insert_failed", &e.to_string()))?;
    }
    let generation = require_generation_tx(&tx, existing_id.as_deref().unwrap_or(generation_id))?;
    if generation.generation_id != generation_id
        || generation.request_fingerprint != request_fingerprint
        || generation.scope_id != scope_id
        || generation.config_fingerprint != config_fingerprint
    {
        let code = format!("mailbox_sync_idempotency_conflict:{idempotency_key}");
        return Err(error(&code, &code));
    }
    if matches!(generation.status.as_str(), "completed" | "failed") {
        tx.commit()
            .map_err(|e| error("mailbox_domain_transaction_commit_failed", &e.to_string()))?;
        return Ok((generation, None));
    }
    let active: Option<(String, String)> = tx
        .query_row(
            "SELECT generation_id,expires_at FROM mailbox_sync_scope_leases WHERE scope_id=?",
            params![scope_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .map_err(|e| error("mailbox_sync_lease_query_failed", &e.to_string()))?;
    if let Some((active_generation, expires_at)) = &active {
        if expires_at.as_str() > now {
            let code = format!("mailbox_sync_scope_busy:{scope_id}:{active_generation}");
            return Err(error(&code, &code));
        }
        tx.execute(
            "DELETE FROM mailbox_sync_scope_leases WHERE scope_id=?",
            params![scope_id],
        )
        .map_err(|e| error("mailbox_sync_lease_delete_failed", &e.to_string()))?;
    }
    let token = Uuid::new_v4().to_string();
    let expires_at = add_millis_iso(now, LEASE_MS)?;
    tx.execute(
        "INSERT INTO mailbox_sync_scope_leases(scope_id,generation_id,lease_token,expires_at,updated_at) VALUES (?,?,?,?,?)",
        params![scope_id,generation.generation_id,token,expires_at,now],
    )
    .map_err(|e| error("mailbox_sync_lease_insert_failed", &e.to_string()))?;
    tx.execute(
        "UPDATE mailbox_sync_generations SET lease_token=?,lease_expires_at=?,updated_at=? WHERE generation_id=?",
        params![token,expires_at,now,generation.generation_id],
    )
    .map_err(|e| error("mailbox_sync_generation_update_failed", &e.to_string()))?;
    let claimed = require_generation_tx(&tx, &generation.generation_id)?;
    tx.commit()
        .map_err(|e| error("mailbox_domain_transaction_commit_failed", &e.to_string()))?;
    Ok((claimed, Some(token)))
}

fn renew_lease(
    db: &mut Connection,
    scope_id: &str,
    generation_id: &str,
    token: &str,
) -> Result<(), Value> {
    let now = now_iso_millis();
    let expires_at = add_millis_iso(&now, LEASE_MS)?;
    let tx = db
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|e| error("mailbox_domain_transaction_failed", &e.to_string()))?;
    let changes = tx
        .execute(
            "UPDATE mailbox_sync_scope_leases SET expires_at=?,updated_at=? WHERE scope_id=? AND generation_id=? AND lease_token=?",
            params![expires_at,now,scope_id,generation_id,token],
        )
        .map_err(|e| error("mailbox_sync_lease_update_failed", &e.to_string()))?;
    if changes != 1 {
        let code = format!("mailbox_sync_lease_lost:{scope_id}");
        return Err(error(&code, &code));
    }
    tx.execute(
        "UPDATE mailbox_sync_generations SET lease_expires_at=?,updated_at=? WHERE generation_id=? AND lease_token=?",
        params![expires_at,now,generation_id,token],
    )
    .map_err(|e| error("mailbox_sync_generation_update_failed", &e.to_string()))?;
    tx.commit()
        .map_err(|e| error("mailbox_domain_transaction_commit_failed", &e.to_string()))
}

fn assert_lease(
    db: &Connection,
    scope_id: &str,
    generation_id: &str,
    token: &str,
) -> Result<(), Value> {
    let row: Option<(String, String, String)> = db
        .query_row(
            "SELECT generation_id,lease_token,expires_at FROM mailbox_sync_scope_leases WHERE scope_id=?",
            params![scope_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()
        .map_err(|e| error("mailbox_sync_lease_query_failed", &e.to_string()))?;
    let valid = row
        .as_ref()
        .is_some_and(|(current_generation, current_token, expires_at)| {
            current_generation == generation_id
                && current_token == token
                && expires_at > &now_iso_millis()
        });
    if !valid {
        let code = format!("mailbox_sync_lease_lost:{scope_id}");
        return Err(error(&code, &code));
    }
    Ok(())
}

fn release_lease(
    db: &mut Connection,
    scope_id: &str,
    generation_id: &str,
    token: &str,
    now: &str,
) -> Result<(), Value> {
    let tx = db
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|e| error("mailbox_domain_transaction_failed", &e.to_string()))?;
    tx.execute(
        "DELETE FROM mailbox_sync_scope_leases WHERE scope_id=? AND generation_id=? AND lease_token=?",
        params![scope_id,generation_id,token],
    )
    .map_err(|e| error("mailbox_sync_lease_delete_failed", &e.to_string()))?;
    tx.execute(
        "UPDATE mailbox_sync_generations SET lease_token=NULL,lease_expires_at=NULL,updated_at=? WHERE generation_id=? AND lease_token=?",
        params![now,generation_id,token],
    )
    .map_err(|e| error("mailbox_sync_generation_update_failed", &e.to_string()))?;
    tx.commit()
        .map_err(|e| error("mailbox_domain_transaction_commit_failed", &e.to_string()))
}

fn require_generation(db: &Connection, generation_id: &str) -> Result<Generation, Value> {
    require_generation_query(db, generation_id)
}

fn require_generation_tx(tx: &Transaction<'_>, generation_id: &str) -> Result<Generation, Value> {
    require_generation_query(tx, generation_id)
}

fn require_generation_query(db: &Connection, generation_id: &str) -> Result<Generation, Value> {
    db.query_row(
            "SELECT generation_id,idempotency_key,request_fingerprint,scope_id,config_fingerprint,status,parent_cursor,next_cursor,batch_path,batch_sha256,batch_record_count,receipt_json,error_message,lease_token,created_at,updated_at,completed_at FROM mailbox_sync_generations WHERE generation_id=?",
            params![generation_id],
            |row| {
                let receipt_json: Option<String> = row.get(11)?;
                Ok(Generation {
                    generation_id: row.get(0)?,
                    _idempotency_key: row.get(1)?,
                    request_fingerprint: row.get(2)?,
                    scope_id: row.get(3)?,
                    config_fingerprint: row.get(4)?,
                    status: row.get(5)?,
                    parent_cursor: row.get(6)?,
                    next_cursor: row.get(7)?,
                    batch_path: row.get(8)?,
                    batch_sha256: row.get(9)?,
                    batch_record_count: row.get(10)?,
                    receipt: receipt_json.and_then(|value| serde_json::from_str(&value).ok()),
                    error_message: row.get(12)?,
                    lease_token: row.get(13)?,
                    _created_at: row.get(14)?,
                    _updated_at: row.get(15)?,
                    _completed_at: row.get(16)?,
                })
            },
        )
        .optional()
        .map_err(|e| error("mailbox_sync_generation_query_failed", &e.to_string()))?
        .ok_or_else(|| {
            let code = format!("mailbox_sync_generation_not_found:{generation_id}");
            error(&code, &code)
        })
}

fn write_generation_artifact(
    path: &Path,
    generation_id: &str,
    batch: &SourceBatch,
) -> Result<String, Value> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| {
            error(
                "mailbox_sync_generation_artifact_write_failed",
                &e.to_string(),
            )
        })?;
    }
    let document = json!({
        "schema":ARTIFACT_SCHEMA,
        "generation_id":generation_id,
        "batch":batch_to_value(batch),
    });
    let bytes = format!(
        "{}\n",
        serde_json::to_string(&document).map_err(|e| error(
            "mailbox_sync_generation_artifact_encode_failed",
            &e.to_string()
        ))?
    );
    let digest = sha256_hex(bytes.as_bytes());
    if path.is_file() {
        let existing = fs::read(path).map_err(|e| {
            error(
                "mailbox_sync_generation_artifact_read_failed",
                &e.to_string(),
            )
        })?;
        if sha256_hex(&existing) != digest {
            return Err(error(
                "mailbox_sync_generation_artifact_conflict",
                "mailbox_sync_generation_artifact_conflict",
            ));
        }
        return Ok(digest);
    }
    let temporary = path.with_extension(format!(
        "json.{}.{}.tmp",
        std::process::id(),
        Uuid::new_v4()
    ));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary)
        .map_err(|e| {
            error(
                "mailbox_sync_generation_artifact_write_failed",
                &e.to_string(),
            )
        })?;
    if let Err(failure) = file
        .write_all(bytes.as_bytes())
        .and_then(|_| file.sync_all())
    {
        let _ = fs::remove_file(&temporary);
        return Err(error(
            "mailbox_sync_generation_artifact_write_failed",
            &failure.to_string(),
        ));
    }
    if let Err(failure) = fs::rename(&temporary, path) {
        if path.is_file() {
            let existing = fs::read(path).unwrap_or_default();
            if sha256_hex(&existing) == digest {
                let _ = fs::remove_file(&temporary);
                return Ok(digest);
            }
        }
        let _ = fs::remove_file(&temporary);
        return Err(error(
            "mailbox_sync_generation_artifact_write_failed",
            &failure.to_string(),
        ));
    }
    Ok(digest)
}

fn read_artifact_at(path: &Path, generation_id: &str) -> Result<SourceBatch, Value> {
    let bytes = fs::read(path).map_err(|e| {
        error(
            "mailbox_sync_generation_artifact_read_failed",
            &e.to_string(),
        )
    })?;
    if bytes.len() as u64 > MAX_RESPONSE_BYTES {
        return Err(error(
            "mailbox_sync_generation_artifact_too_large",
            "mailbox_sync_generation_artifact_too_large",
        ));
    }
    parse_generation_artifact(&bytes, generation_id)
}

fn read_generation_artifact(
    generation: &Generation,
    expected_path: &Path,
) -> Result<SourceBatch, Value> {
    let path = generation.batch_path.as_deref().ok_or_else(|| {
        let code = format!(
            "mailbox_sync_generation_artifact_missing:{}",
            generation.generation_id
        );
        error(&code, &code)
    })?;
    let expected_hash = generation.batch_sha256.as_deref().ok_or_else(|| {
        let code = format!(
            "mailbox_sync_generation_artifact_missing:{}",
            generation.generation_id
        );
        error(&code, &code)
    })?;
    if normalized_path_text(Path::new(path)) != normalized_path_text(expected_path) {
        let code = format!(
            "mailbox_sync_generation_artifact_path_mismatch:{}",
            generation.generation_id
        );
        return Err(error(&code, &code));
    }
    let bytes = fs::read(path).map_err(|e| {
        error(
            "mailbox_sync_generation_artifact_read_failed",
            &e.to_string(),
        )
    })?;
    if sha256_hex(&bytes) != expected_hash {
        let code = format!(
            "mailbox_sync_generation_artifact_hash_mismatch:{}",
            generation.generation_id
        );
        return Err(error(&code, &code));
    }
    parse_generation_artifact(&bytes, &generation.generation_id)
}

fn parse_generation_artifact(bytes: &[u8], generation_id: &str) -> Result<SourceBatch, Value> {
    let document: Value = serde_json::from_slice(bytes)
        .map_err(|e| error("mailbox_sync_generation_artifact_invalid", &e.to_string()))?;
    if document.get("schema").and_then(Value::as_str) != Some(ARTIFACT_SCHEMA)
        || document.get("generation_id").and_then(Value::as_str) != Some(generation_id)
    {
        let code = format!("mailbox_sync_generation_artifact_identity_mismatch:{generation_id}");
        return Err(error(&code, &code));
    }
    batch_from_value(document.get("batch").unwrap_or(&Value::Null))
}

fn stage_batch(
    db: &mut Connection,
    generation_id: &str,
    lease_token: &str,
    checkpoint: Option<&str>,
    batch: &SourceBatch,
    artifact_path: &Path,
) -> Result<(), Value> {
    let parent_cursor = batch.prior_checkpoint.as_deref().or(checkpoint);
    if parent_cursor != checkpoint {
        let code = format!("mailbox_sync_source_parent_cursor_mismatch:{generation_id}");
        return Err(error(&code, &code));
    }
    let bytes = fs::read(artifact_path).map_err(|e| {
        error(
            "mailbox_sync_generation_artifact_read_failed",
            &e.to_string(),
        )
    })?;
    let artifact_hash = sha256_hex(&bytes);
    let staged = batch
        .records
        .iter()
        .map(|record| staged_record(record, batch.next_checkpoint.as_deref()))
        .collect::<Result<Vec<_>, _>>()?;
    let now = now_iso_millis();
    let tx = db
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|e| error("mailbox_domain_transaction_failed", &e.to_string()))?;
    let generation = require_generation_tx(&tx, generation_id)?;
    if generation.status == "staged" {
        if generation.parent_cursor.as_deref() != parent_cursor
            || generation.next_cursor != batch.next_checkpoint
            || generation.batch_path.as_deref()
                != Some(normalized_path_text(artifact_path).as_str())
            || generation.batch_sha256.as_deref() != Some(artifact_hash.as_str())
            || generation.batch_record_count != staged.len() as i64
        {
            let code = format!("mailbox_sync_staged_batch_conflict:{generation_id}");
            return Err(error(&code, &code));
        }
        tx.commit()
            .map_err(|e| error("mailbox_domain_transaction_commit_failed", &e.to_string()))?;
        return Ok(());
    }
    if generation.status != "accepted" {
        let code = format!(
            "mailbox_sync_generation_not_stageable:{}",
            generation.status
        );
        return Err(error(&code, &code));
    }
    if generation.lease_token.as_deref() != Some(lease_token) {
        let code = format!("mailbox_sync_lease_lost:{}", generation.scope_id);
        return Err(error(&code, &code));
    }
    tx.execute(
        "UPDATE mailbox_sync_generations SET status='staged',parent_cursor=?,next_cursor=?,batch_path=?,batch_sha256=?,batch_record_count=?,staged_at=?,updated_at=? WHERE generation_id=?",
        params![parent_cursor,batch.next_checkpoint,normalized_path_text(artifact_path),artifact_hash,staged.len() as i64,now,now,generation_id],
    )
    .map_err(|e| error("mailbox_sync_generation_update_failed", &e.to_string()))?;
    for record in staged {
        tx.execute(
            "INSERT INTO mailbox_sync_generation_records(generation_id,record_id,ordinal,fact_id,event_kind,message_id,mailbox_id,conversation_id,source_version,application_status) VALUES (?,?,?,?,?,?,?,?,?,'staged')",
            params![generation_id,record.record_id,record.ordinal,record.fact_id,record.event_kind,record.message_id,record.mailbox_id,record.conversation_id,record.source_version],
        )
        .map_err(|e| error("mailbox_sync_generation_record_insert_failed", &e.to_string()))?;
    }
    tx.commit()
        .map_err(|e| error("mailbox_domain_transaction_commit_failed", &e.to_string()))
}

fn generation_records(db: &Connection, generation_id: &str) -> Result<Vec<StagedRecord>, Value> {
    let mut statement = db
        .prepare("SELECT record_id,ordinal,fact_id,event_kind,message_id,mailbox_id,conversation_id,source_version,application_status FROM mailbox_sync_generation_records WHERE generation_id=? ORDER BY rowid")
        .map_err(|e| error("mailbox_sync_generation_record_query_failed", &e.to_string()))?;
    let rows = statement
        .query_map(params![generation_id], |row| {
            Ok(StagedRecord {
                record_id: row.get(0)?,
                ordinal: row.get(1)?,
                fact_id: row.get(2)?,
                event_kind: row.get(3)?,
                message_id: row.get(4)?,
                mailbox_id: row.get(5)?,
                conversation_id: row.get(6)?,
                source_version: row.get(7)?,
                application_status: row.get(8)?,
            })
        })
        .map_err(|e| {
            error(
                "mailbox_sync_generation_record_query_failed",
                &e.to_string(),
            )
        })?;
    let mut records = Vec::new();
    for row in rows {
        records.push(
            row.map_err(|e| error("mailbox_sync_generation_record_row_failed", &e.to_string()))?,
        );
    }
    Ok(records)
}

fn generation_ready(db: &Connection, generation_id: &str) -> Result<bool, Value> {
    Ok(generation_records(db, generation_id)?
        .iter()
        .all(|record| record.application_status != "staged"))
}

fn mark_record_application(
    db: &Connection,
    generation_id: &str,
    record_id: &str,
    status: &str,
) -> Result<(), Value> {
    let changes = db
        .execute(
            "UPDATE mailbox_sync_generation_records SET application_status=? WHERE generation_id=? AND record_id=?",
            params![status,generation_id,record_id],
        )
        .map_err(|e| error("mailbox_sync_generation_record_update_failed", &e.to_string()))?;
    if changes != 1 {
        let code = format!("mailbox_sync_record_unknown:{record_id}");
        return Err(error(&code, &code));
    }
    Ok(())
}

fn finalize_generation(
    db: &mut Connection,
    generation_id: &str,
    lease_token: &str,
    now: &str,
) -> Result<Generation, Value> {
    let tx = db
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|e| error("mailbox_domain_transaction_failed", &e.to_string()))?;
    let generation = require_generation_tx(&tx, generation_id)?;
    if generation.status == "completed" {
        tx.commit()
            .map_err(|e| error("mailbox_domain_transaction_commit_failed", &e.to_string()))?;
        return Ok(generation);
    }
    if generation.status != "staged" {
        let code = format!(
            "mailbox_sync_generation_not_finalizable:{}",
            generation.status
        );
        return Err(error(&code, &code));
    }
    let records = generation_records(&tx, generation_id)?;
    if let Some(incomplete) = records
        .iter()
        .find(|record| record.application_status == "staged")
    {
        let code = format!(
            "mailbox_sync_generation_incomplete:{}",
            incomplete.record_id
        );
        return Err(error(&code, &code));
    }
    let mut first_observation_count = 0_u64;
    let mut observed: Vec<Value> = Vec::new();
    let mut observed_keys = HashSet::new();
    let mut tombstone_count = 0_u64;
    for record in &records {
        if record.application_status == "not_applied" {
            continue;
        }
        if matches!(record.event_kind.as_str(), "delete" | "deleted") {
            tombstone_count += 1;
            continue;
        }
        let (Some(message_id), Some(mailbox_id)) = (&record.message_id, &record.mailbox_id) else {
            continue;
        };
        let identity = format!("{mailbox_id}\0{message_id}");
        let mut reference = json!({
            "mailbox_id":mailbox_id,
            "message_id":message_id,
            "fact_id":record.fact_id,
        });
        if let Some(conversation_id) = &record.conversation_id {
            reference
                .as_object_mut()
                .expect("object")
                .insert("conversation_id".to_string(), json!(conversation_id));
        }
        if observed_keys.insert(identity.clone()) {
            observed.push(reference);
        } else if let Some(slot) = observed.iter_mut().find(|value| {
            value.get("mailbox_id").and_then(Value::as_str) == Some(mailbox_id)
                && value.get("message_id").and_then(Value::as_str) == Some(message_id)
        }) {
            *slot = reference;
        }
        let observation_id = stable_id("mobs_", &identity);
        let inserted = tx
            .execute(
                "INSERT OR IGNORE INTO mailbox_message_observations(observation_id,mailbox_id,message_id,first_generation_id,first_fact_id,observed_at) VALUES (?,?,?,?,?,?)",
                params![observation_id,mailbox_id,message_id,generation_id,record.fact_id,now],
            )
            .map_err(|e| error("mailbox_sync_observation_insert_failed", &e.to_string()))?;
        if inserted != 1 {
            continue;
        }
        first_observation_count += 1;
        let event_id = stable_id("mbe_", &format!("first-observed\0{identity}"));
        let mut payload = json!({
            "schema":"narada.mailbox.message_first_observed.v1",
            "generation_id":generation_id,
            "observation_id":observation_id,
            "mailbox_id":mailbox_id,
            "message_id":message_id,
            "fact_id":record.fact_id,
        });
        if let Some(conversation_id) = &record.conversation_id {
            payload
                .as_object_mut()
                .expect("object")
                .insert("conversation_id".to_string(), json!(conversation_id));
        }
        tx.execute(
            "INSERT INTO mailbox_outbox(event_id,scope_id,topic,aggregate_id,aggregate_revision,schema_version,causation_id,idempotency_key,partition_key,occurred_at,payload_json) VALUES (?,?,'mailbox.message.first_observed',?,1,1,?,?,?,?,?)",
            params![event_id,generation.scope_id,observation_id,generation_id,event_id,observation_id,now,serde_json::to_string(&payload).unwrap_or_else(|_| "{}".to_string())],
        )
        .map_err(|e| error("mailbox_sync_outbox_insert_failed", &e.to_string()))?;
    }
    let status = if first_observation_count > 0 {
        "synced"
    } else {
        "no_change"
    };
    let receipt = json!({
        "schema":"narada.mailbox.sync_generation_receipt.v1",
        "generation_id":generation_id,
        "scope_id":generation.scope_id,
        "status":status,
        "config_fingerprint":generation.config_fingerprint,
        "parent_cursor_sha256":nullable_hash(generation.parent_cursor.as_deref()),
        "next_cursor_sha256":nullable_hash(generation.next_cursor.as_deref()),
        "record_count":records.len(),
        "observed_message_count":observed.len(),
        "first_observation_count":first_observation_count,
        "tombstone_count":tombstone_count,
        "observed_message_refs":observed.iter().take(100).cloned().collect::<Vec<_>>(),
        "observed_message_refs_truncated":observed.len()>100,
        "completed_at":now,
    });
    let changes = tx
        .execute(
            "UPDATE mailbox_sync_generations SET status='completed',receipt_json=?,error_message=NULL,completed_at=?,updated_at=?,lease_token=NULL,lease_expires_at=NULL WHERE generation_id=? AND lease_token=?",
            params![serde_json::to_string(&receipt).unwrap_or_else(|_| "{}".to_string()),now,now,generation_id,lease_token],
        )
        .map_err(|e| error("mailbox_sync_generation_update_failed", &e.to_string()))?;
    if changes != 1 {
        let code = format!("mailbox_sync_lease_lost:{}", generation.scope_id);
        return Err(error(&code, &code));
    }
    tx.execute(
        "DELETE FROM mailbox_sync_scope_leases WHERE scope_id=? AND generation_id=? AND lease_token=?",
        params![generation.scope_id,generation_id,lease_token],
    )
    .map_err(|e| error("mailbox_sync_lease_delete_failed", &e.to_string()))?;
    let completed = require_generation_tx(&tx, generation_id)?;
    tx.commit()
        .map_err(|e| error("mailbox_domain_transaction_commit_failed", &e.to_string()))?;
    Ok(completed)
}

fn fail_generation(
    db: &mut Connection,
    generation_id: &str,
    lease_token: &str,
    message: &str,
    now: &str,
) -> Result<Generation, Value> {
    let tx = db
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|e| error("mailbox_domain_transaction_failed", &e.to_string()))?;
    let generation = require_generation_tx(&tx, generation_id)?;
    let bounded = message.chars().take(2048).collect::<String>();
    let changes = tx
        .execute(
            "UPDATE mailbox_sync_generations SET status='failed',error_message=?,completed_at=?,updated_at=?,lease_token=NULL,lease_expires_at=NULL WHERE generation_id=? AND lease_token=?",
            params![bounded,now,now,generation_id,lease_token],
        )
        .map_err(|e| error("mailbox_sync_generation_update_failed", &e.to_string()))?;
    if changes != 1 {
        let code = format!("mailbox_sync_lease_lost:{}", generation.scope_id);
        return Err(error(&code, &code));
    }
    tx.execute(
        "DELETE FROM mailbox_sync_scope_leases WHERE scope_id=? AND generation_id=? AND lease_token=?",
        params![generation.scope_id,generation_id,lease_token],
    )
    .map_err(|e| error("mailbox_sync_lease_delete_failed", &e.to_string()))?;
    let failed = require_generation_tx(&tx, generation_id)?;
    tx.commit()
        .map_err(|e| error("mailbox_domain_transaction_commit_failed", &e.to_string()))?;
    Ok(failed)
}

fn generation_operation(generation: &Generation, replayed: bool) -> Result<Value, Value> {
    let receipt = generation.receipt.as_ref().ok_or_else(|| {
        let code = format!("mailbox_sync_receipt_missing:{}", generation.generation_id);
        error(&code, &code)
    })?;
    let serialized = canonical_json(receipt);
    let observed_count = receipt
        .get("observed_message_refs")
        .and_then(Value::as_array)
        .map(Vec::len)
        .unwrap_or(0);
    Ok(json!({
        "schema":DOMAIN_SCHEMA,
        "operation_ref":format!("mailbox-sync:{}",generation.generation_id),
        "outcome":"completed",
        "result":{
            "schema":receipt.get("schema").and_then(Value::as_str).unwrap_or("narada.mailbox.sync_generation_receipt.v1"),
            "generation_id":receipt.get("generation_id").cloned().unwrap_or(Value::Null),
            "scope_id":receipt.get("scope_id").cloned().unwrap_or(Value::Null),
            "status":receipt.get("status").cloned().unwrap_or(Value::Null),
            "config_fingerprint":receipt.get("config_fingerprint").cloned().unwrap_or(Value::Null),
            "parent_cursor_sha256":receipt.get("parent_cursor_sha256").cloned().unwrap_or(Value::Null),
            "next_cursor_sha256":receipt.get("next_cursor_sha256").cloned().unwrap_or(Value::Null),
            "record_count":receipt.get("record_count").cloned().unwrap_or(Value::Null),
            "observed_message_count":receipt.get("observed_message_count").cloned().unwrap_or(Value::Null),
            "first_observation_count":receipt.get("first_observation_count").cloned().unwrap_or(Value::Null),
            "tombstone_count":receipt.get("tombstone_count").cloned().unwrap_or(Value::Null),
            "observed_message_refs_available_count":observed_count,
            "observed_message_refs_omitted":true,
            "observed_message_refs_truncated":receipt.get("observed_message_refs_truncated").and_then(Value::as_bool).unwrap_or(false),
            "completed_at":receipt.get("completed_at").cloned().unwrap_or(Value::Null),
            "idempotency_replayed":replayed,
        },
        "result_ref":{
            "ref":format!("mailbox-generation-receipt:{}",generation.generation_id),
            "sha256":sha256_hex(serialized.as_bytes()),
            "byte_length":serialized.len(),
            "media_type":"application/json",
        },
    }))
}

fn blocked_generation_operation(generation: &Generation, replayed: bool) -> Value {
    json!({
        "schema":DOMAIN_SCHEMA,
        "operation_ref":format!("mailbox-sync:{}",generation.generation_id),
        "outcome":"completed",
        "result":{
            "schema":"narada.mailbox.sync_generation_failure.v1",
            "generation_id":generation.generation_id,
            "scope_id":generation.scope_id,
            "status":"blocked",
            "error_message":generation.error_message.as_deref().unwrap_or("mailbox_sync_failed"),
            "idempotency_replayed":replayed,
        },
    })
}

fn fetch_graph_batch<F>(
    scope: &ScopeConfig,
    checkpoint: Option<&str>,
    mut heartbeat: F,
) -> Result<SourceBatch, Value>
where
    F: FnMut() -> Result<(), Value>,
{
    let token = graph_access_token(scope)?;
    let fetched_at = now_iso_millis();
    let mut messages = Vec::new();
    let mut next_by_folder = Map::new();
    let composite = checkpoint
        .and_then(|value| serde_json::from_str::<Value>(value).ok())
        .and_then(|value| value.as_object().cloned());
    for folder in &scope.included_container_refs {
        if folder.trim().is_empty() {
            return Err(error(
                "mailbox_graph_folder_ref_invalid",
                "Configured folder ref is empty",
            ));
        }
        heartbeat()?;
        let folder_cursor = if scope.included_container_refs.len() == 1 {
            checkpoint.map(ToString::to_string)
        } else {
            composite
                .as_ref()
                .and_then(|value| value.get(folder))
                .and_then(Value::as_str)
                .map(ToString::to_string)
        };
        let base = format!(
            "{}{}/mailFolders/{}/messages/delta",
            scope.graph.base_url,
            graph_mailbox_path(&scope.graph.user_id),
            encode_component(folder)
        );
        let walked = match walk_delta(
            scope,
            &token,
            folder_cursor.as_deref().unwrap_or(&base),
            folder,
            &mut heartbeat,
        ) {
            Ok(value) => value,
            Err(GraphWalkError::Stale) if folder_cursor.is_some() => {
                walk_delta(scope, &token, &base, folder, &mut heartbeat)
                    .map_err(GraphWalkError::into_value)?
            }
            Err(value) => return Err(value.into_value()),
        };
        if messages.len() + walked.0.len() > MAX_GRAPH_RECORDS {
            return Err(error(
                "mailbox_graph_record_limit_exceeded",
                "mailbox_graph_record_limit_exceeded",
            ));
        }
        messages.extend(walked.0);
        next_by_folder.insert(folder.clone(), json!(walked.1));
    }

    let next_checkpoint = if scope.included_container_refs.len() == 1 {
        next_by_folder
            .values()
            .next()
            .and_then(Value::as_str)
            .map(ToString::to_string)
    } else {
        Some(
            serde_json::to_string(&Value::Object(next_by_folder))
                .map_err(|e| error("mailbox_graph_cursor_encode_failed", &e.to_string()))?,
        )
    };
    let mut events = BTreeMap::new();
    for message in messages {
        let event = normalize_graph_event(scope, &message, &fetched_at)?;
        let event_id = event
            .get("event_id")
            .and_then(Value::as_str)
            .expect("normalized event id")
            .to_string();
        events.insert(event_id, event);
    }
    let records = events
        .into_values()
        .map(|event| {
            let record_id = event
                .get("event_id")
                .and_then(Value::as_str)
                .expect("normalized event id")
                .to_string();
            let ordinal = event
                .get("observed_at")
                .or_else(|| event.get("received_at"))
                .and_then(Value::as_str)
                .map(ToString::to_string)
                .or_else(|| Some(fetched_at.clone()));
            let source_version = event
                .get("source_version")
                .and_then(Value::as_str)
                .map(ToString::to_string);
            let mut provenance = json!({
                "sourceId":scope.scope_id,
                "observedAt":event.get("observed_at").and_then(Value::as_str).unwrap_or(&fetched_at),
            });
            if let Some(source_version) = source_version {
                provenance
                    .as_object_mut()
                    .expect("object")
                    .insert("sourceVersion".to_string(), json!(source_version));
            }
            SourceRecord {
                record_id,
                ordinal,
                payload: event,
                provenance,
            }
        })
        .collect();
    Ok(SourceBatch {
        records,
        prior_checkpoint: checkpoint.map(ToString::to_string),
        next_checkpoint,
        has_more: false,
        fetched_at,
    })
}

enum GraphWalkError {
    Stale,
    Failure(Value),
}

impl GraphWalkError {
    fn into_value(self) -> Value {
        match self {
            Self::Stale => error(
                "mailbox_graph_delta_cursor_stale",
                "mailbox_graph_delta_cursor_stale",
            ),
            Self::Failure(value) => value,
        }
    }
}

fn walk_delta<F>(
    scope: &ScopeConfig,
    token: &str,
    start_url: &str,
    folder: &str,
    heartbeat: &mut F,
) -> Result<(Vec<Value>, String), GraphWalkError>
where
    F: FnMut() -> Result<(), Value>,
{
    let mut url = start_url.to_string();
    let mut values = Vec::new();
    let mut delta_link = None;
    for _ in 0..MAX_GRAPH_PAGES {
        validate_graph_page_url(&url, &scope.graph.base_url).map_err(GraphWalkError::Failure)?;
        heartbeat().map_err(GraphWalkError::Failure)?;
        let page = match graph_get(scope, token, &url) {
            Ok(value) => value,
            Err(GraphHttpError::Status(410, _)) => return Err(GraphWalkError::Stale),
            Err(value) => return Err(GraphWalkError::Failure(value.into_value())),
        };
        let page_values = page.get("value").and_then(Value::as_array).ok_or_else(|| {
            GraphWalkError::Failure(error(
                "mailbox_graph_delta_response_invalid",
                "mailbox_graph_delta_response_invalid",
            ))
        })?;
        if values.len() + page_values.len() > MAX_GRAPH_RECORDS {
            return Err(GraphWalkError::Failure(error(
                "mailbox_graph_record_limit_exceeded",
                "mailbox_graph_record_limit_exceeded",
            )));
        }
        for raw in page_values {
            let mut message = raw.as_object().cloned().ok_or_else(|| {
                GraphWalkError::Failure(error(
                    "mailbox_graph_delta_message_invalid",
                    "mailbox_graph_delta_message_invalid",
                ))
            })?;
            message.insert("sourceQueriedFolderRef".to_string(), json!(folder));
            if scope.attachment_policy != "exclude"
                && message.get("@removed").is_none()
                && message.get("hasAttachments").and_then(Value::as_bool) == Some(true)
                && message
                    .get("attachments")
                    .and_then(Value::as_array)
                    .map(|value| value.is_empty())
                    .unwrap_or(true)
            {
                if let Some(message_id) = message.get("id").and_then(Value::as_str) {
                    let attachments = fetch_attachments(scope, token, message_id, heartbeat)
                        .map_err(GraphWalkError::Failure)?;
                    message.insert("attachments".to_string(), Value::Array(attachments));
                }
            }
            values.push(Value::Object(message));
        }
        delta_link = page
            .get("@odata.deltaLink")
            .and_then(Value::as_str)
            .map(ToString::to_string)
            .or(delta_link);
        if let Some(next) = page.get("@odata.nextLink").and_then(Value::as_str) {
            url = next.to_string();
        } else {
            return delta_link.map(|value| (values, value)).ok_or_else(|| {
                GraphWalkError::Failure(error(
                    "mailbox_graph_delta_link_missing",
                    "Delta query did not return @odata.deltaLink",
                ))
            });
        }
    }
    Err(GraphWalkError::Failure(error(
        "mailbox_graph_page_limit_exceeded",
        "mailbox_graph_page_limit_exceeded",
    )))
}

fn fetch_attachments<F>(
    scope: &ScopeConfig,
    token: &str,
    message_id: &str,
    heartbeat: &mut F,
) -> Result<Vec<Value>, Value>
where
    F: FnMut() -> Result<(), Value>,
{
    let mut url = format!(
        "{}{}/messages/{}/attachments",
        scope.graph.base_url,
        graph_mailbox_path(&scope.graph.user_id),
        encode_component(message_id)
    );
    let mut attachments = Vec::new();
    for _ in 0..MAX_GRAPH_PAGES {
        validate_graph_page_url(&url, &scope.graph.base_url)?;
        heartbeat()?;
        let page = graph_get(scope, token, &url).map_err(GraphHttpError::into_value)?;
        let values = page.get("value").and_then(Value::as_array).ok_or_else(|| {
            error(
                "mailbox_graph_attachment_response_invalid",
                "mailbox_graph_attachment_response_invalid",
            )
        })?;
        if attachments.len() + values.len() > MAX_GRAPH_RECORDS {
            return Err(error(
                "mailbox_graph_attachment_limit_exceeded",
                "mailbox_graph_attachment_limit_exceeded",
            ));
        }
        attachments.extend(values.iter().cloned());
        if let Some(next) = page.get("@odata.nextLink").and_then(Value::as_str) {
            url = next.to_string();
        } else {
            return Ok(attachments);
        }
    }
    Err(error(
        "mailbox_graph_page_limit_exceeded",
        "mailbox_graph_page_limit_exceeded",
    ))
}

enum GraphHttpError {
    Status(u16, String),
    Failure(String),
}

impl GraphHttpError {
    fn into_value(self) -> Value {
        match self {
            Self::Status(status, body) => error(
                "mailbox_graph_request_failed",
                &format!("Graph API error ({status}): {}", bounded_error(&body)),
            ),
            Self::Failure(message) => error("mailbox_graph_request_failed", &message),
        }
    }
}

fn graph_get(scope: &ScopeConfig, token: &str, url: &str) -> Result<Value, GraphHttpError> {
    let agent = ureq::AgentBuilder::new()
        .timeout(Duration::from_secs(20))
        .build();
    let mut request = agent
        .get(url)
        .set("Authorization", &format!("Bearer {token}"))
        .set("Accept", "application/json");
    if scope.graph.prefer_immutable_ids {
        request = request.set("Prefer", "IdType=\"ImmutableId\"");
    }
    let response = match request.call() {
        Ok(response) => response,
        Err(ureq::Error::Status(status, response)) => {
            let body = read_ureq_body(response).unwrap_or_default();
            return Err(GraphHttpError::Status(status, body));
        }
        Err(value) => return Err(GraphHttpError::Failure(value.to_string())),
    };
    let body = read_ureq_body(response).map_err(GraphHttpError::Failure)?;
    serde_json::from_str(&body)
        .map_err(|e| GraphHttpError::Failure(format!("mailbox_graph_response_invalid:{e}")))
}

fn graph_access_token(scope: &ScopeConfig) -> Result<String, Value> {
    if scope.graph.auth_mode.as_deref() == Some("delegated_token_store") {
        return delegated_graph_access_token(scope);
    }
    if let Some(token) = non_empty_env("GRAPH_ACCESS_TOKEN") {
        return Ok(token);
    }
    let tenant = non_empty_env("GRAPH_TENANT_ID").or_else(|| scope.graph.tenant_id.clone());
    let client_id = non_empty_env("GRAPH_CLIENT_ID").or_else(|| scope.graph.client_id.clone());
    let client_secret =
        non_empty_env("GRAPH_CLIENT_SECRET").or_else(|| scope.graph.client_secret.clone());
    if let (Some(tenant), Some(client_id), Some(client_secret)) = (tenant, client_id, client_secret)
    {
        return client_credentials_token(&tenant, &client_id, &client_secret);
    }
    azure_cli_token(scope.graph.tenant_id.as_deref())
}

fn delegated_graph_access_token(scope: &ScopeConfig) -> Result<String, Value> {
    let site_root = scope
        .root_dir
        .ancestors()
        .find(|candidate| {
            candidate
                .join(".ai/runtime/graph-mail-mcp/delegated-token.json")
                .is_file()
        })
        .ok_or_else(|| {
            error(
                "mailbox_graph_delegated_token_missing",
                "mailbox_graph_delegated_token_missing",
            )
        })?;
    let path = site_root.join(".ai/runtime/graph-mail-mcp/delegated-token.json");
    let text = fs::read_to_string(&path)
        .map_err(|e| error("mailbox_graph_delegated_token_missing", &e.to_string()))?;
    let mut token: Value = serde_json::from_str(&text)
        .map_err(|e| error("mailbox_graph_delegated_token_invalid", &e.to_string()))?;
    if token.get("schema").and_then(Value::as_str)
        != Some("narada.graph_mail_mcp.delegated_token.v1")
    {
        return Err(error(
            "mailbox_graph_delegated_token_invalid",
            "mailbox_graph_delegated_token_invalid",
        ));
    }
    let expires_at_ms = token
        .get("expires_at_ms")
        .and_then(Value::as_i64)
        .unwrap_or(0);
    let now_ms = (OffsetDateTime::now_utc().unix_timestamp_nanos() / 1_000_000) as i64;
    if expires_at_ms > now_ms + 60_000 {
        return token
            .get("access_token")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToString::to_string)
            .ok_or_else(|| {
                error(
                    "mailbox_graph_delegated_token_invalid",
                    "mailbox_graph_delegated_token_invalid",
                )
            });
    }
    let tenant =
        required_value_string(&token, "tenant_id", "mailbox_graph_delegated_token_invalid")?;
    let client_id =
        required_value_string(&token, "client_id", "mailbox_graph_delegated_token_invalid")?;
    let scope_value =
        required_value_string(&token, "scope", "mailbox_graph_delegated_token_invalid")?;
    let refresh = required_value_string(
        &token,
        "refresh_token",
        "mailbox_graph_delegated_token_expired_reauthorization_required",
    )?;
    let endpoint = format!(
        "https://login.microsoftonline.com/{}/oauth2/v2.0/token",
        encode_component(&tenant)
    );
    let form = format!(
        "grant_type=refresh_token&client_id={}&refresh_token={}&scope={}",
        encode_component(&client_id),
        encode_component(&refresh),
        encode_component(&scope_value)
    );
    let response = ureq::AgentBuilder::new()
        .timeout(Duration::from_secs(20))
        .build()
        .post(&endpoint)
        .set("Content-Type", "application/x-www-form-urlencoded")
        .send_string(&form);
    let response = response.map_err(|value| {
        error(
            "mailbox_graph_delegated_token_refresh_failed",
            &value.to_string(),
        )
    })?;
    let payload: Value = serde_json::from_str(
        &read_ureq_body(response)
            .map_err(|value| error("mailbox_graph_delegated_token_refresh_failed", &value))?,
    )
    .map_err(|e| {
        error(
            "mailbox_graph_delegated_token_refresh_response_invalid",
            &e.to_string(),
        )
    })?;
    let access_token = required_value_string(
        &payload,
        "access_token",
        "mailbox_graph_delegated_token_refresh_response_invalid",
    )?;
    let expires_in = payload
        .get("expires_in")
        .and_then(Value::as_i64)
        .unwrap_or(3599)
        .max(60);
    if let Some(object) = token.as_object_mut() {
        object.insert("access_token".to_string(), json!(access_token));
        if let Some(value) = payload.get("refresh_token").and_then(Value::as_str) {
            object.insert("refresh_token".to_string(), json!(value));
        }
        object.insert(
            "expires_at_ms".to_string(),
            json!(now_ms + expires_in * 1000),
        );
        object.insert("acquired_at".to_string(), json!(now_iso_millis()));
    }
    atomic_write_json(&path, &token)?;
    Ok(access_token)
}

fn client_credentials_token(
    tenant: &str,
    client_id: &str,
    client_secret: &str,
) -> Result<String, Value> {
    let endpoint = format!(
        "https://login.microsoftonline.com/{}/oauth2/v2.0/token",
        encode_component(tenant)
    );
    let form = format!(
        "grant_type=client_credentials&client_id={}&client_secret={}&scope={}",
        encode_component(client_id),
        encode_component(client_secret),
        encode_component("https://graph.microsoft.com/.default")
    );
    let response = ureq::AgentBuilder::new()
        .timeout(Duration::from_secs(20))
        .build()
        .post(&endpoint)
        .set("Content-Type", "application/x-www-form-urlencoded")
        .send_string(&form)
        .map_err(|e| error("mailbox_graph_token_request_failed", &e.to_string()))?;
    let payload: Value = serde_json::from_str(
        &read_ureq_body(response).map_err(|e| error("mailbox_graph_token_request_failed", &e))?,
    )
    .map_err(|e| error("mailbox_graph_token_response_invalid", &e.to_string()))?;
    required_value_string(
        &payload,
        "access_token",
        "mailbox_graph_token_response_invalid",
    )
}

fn azure_cli_token(tenant: Option<&str>) -> Result<String, Value> {
    let mut command = Command::new("az");
    command.args([
        "account",
        "get-access-token",
        "--resource",
        "https://graph.microsoft.com",
        "--output",
        "json",
    ]);
    if let Some(tenant) = tenant {
        command.args(["--tenant", tenant]);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000);
    }
    let output = command.output().map_err(|e| {
        error(
            "mailbox_graph_login_unavailable",
            &format!("Graph delegated Microsoft login unavailable: {e}"),
        )
    })?;
    if !output.status.success() || output.stdout.len() as u64 > MAX_RESPONSE_BYTES {
        return Err(error(
            "mailbox_graph_login_unavailable",
            "Graph delegated Microsoft login unavailable: Azure CLI token request failed",
        ));
    }
    let payload: Value = serde_json::from_slice(&output.stdout)
        .map_err(|e| error("mailbox_graph_login_unavailable", &e.to_string()))?;
    required_value_string(&payload, "accessToken", "mailbox_graph_login_unavailable")
}

fn normalize_graph_event(
    scope: &ScopeConfig,
    raw: &Value,
    observed_at: &str,
) -> Result<Value, Value> {
    let message = raw.as_object().ok_or_else(|| {
        error(
            "mailbox_graph_delta_message_invalid",
            "mailbox_graph_delta_message_invalid",
        )
    })?;
    let message_id = message
        .get("id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            error(
                "mailbox_graph_message_id_missing",
                "Graph delta entry is missing id",
            )
        })?;
    let source_version = message
        .get("changeKey")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty());
    if message.get("@removed").is_some() {
        let mut identity = json!({
            "scope_id":scope.scope_id,
            "message_id":message_id,
            "event_kind":"delete",
            "source_version":source_version,
        });
        let event_id = format!("evt_{}", sha256_hex(canonical_json(&identity).as_bytes()));
        let mut event = json!({
            "schema_version":1,
            "event_id":event_id,
            "mailbox_id":scope.scope_id,
            "message_id":message_id,
        });
        insert_optional_string(&mut event, "conversation_id", message.get("conversationId"));
        event
            .as_object_mut()
            .expect("object")
            .insert("source_item_id".to_string(), json!(message_id));
        if let Some(value) = source_version {
            event
                .as_object_mut()
                .expect("object")
                .insert("source_version".to_string(), json!(value));
        } else if let Some(object) = identity.as_object_mut() {
            object.insert("source_version".to_string(), Value::Null);
        }
        event
            .as_object_mut()
            .expect("object")
            .insert("event_kind".to_string(), json!("delete"));
        event
            .as_object_mut()
            .expect("object")
            .insert("observed_at".to_string(), json!(observed_at));
        return Ok(event);
    }
    let payload = normalize_message_payload(scope, message)?;
    let mut identity = json!({
        "scope_id":scope.scope_id,
        "message_id":message_id,
        "event_kind":"upsert",
    });
    if let Some(value) = source_version {
        identity
            .as_object_mut()
            .expect("object")
            .insert("source_version".to_string(), json!(value));
    } else {
        identity.as_object_mut().expect("object").insert(
            "payload_hash".to_string(),
            json!(sha256_hex(canonical_json(&payload).as_bytes())),
        );
    }
    let event_id = format!("evt_{}", sha256_hex(canonical_json(&identity).as_bytes()));
    let mut event = json!({
        "schema_version":1,
        "event_id":event_id,
        "mailbox_id":scope.scope_id,
        "message_id":message_id,
    });
    insert_optional_string(&mut event, "conversation_id", message.get("conversationId"));
    event
        .as_object_mut()
        .expect("object")
        .insert("source_item_id".to_string(), json!(message_id));
    if let Some(value) = source_version {
        event
            .as_object_mut()
            .expect("object")
            .insert("source_version".to_string(), json!(value));
    }
    event
        .as_object_mut()
        .expect("object")
        .insert("event_kind".to_string(), json!("upsert"));
    event
        .as_object_mut()
        .expect("object")
        .insert("observed_at".to_string(), json!(observed_at));
    event
        .as_object_mut()
        .expect("object")
        .insert("payload".to_string(), payload);
    Ok(event)
}

fn normalize_message_payload(
    scope: &ScopeConfig,
    message: &Map<String, Value>,
) -> Result<Value, Value> {
    let message_id = message.get("id").and_then(Value::as_str).ok_or_else(|| {
        error(
            "mailbox_graph_message_id_missing",
            "mailbox_graph_message_id_missing",
        )
    })?;
    let mut payload = json!({
        "schema_version":1,
        "mailbox_id":scope.scope_id,
        "message_id":message_id,
    });
    insert_optional_string(
        &mut payload,
        "conversation_id",
        message.get("conversationId"),
    );
    insert_optional_string(
        &mut payload,
        "internet_message_id",
        message.get("internetMessageId"),
    );
    payload.as_object_mut().expect("object").insert(
        "subject".to_string(),
        message
            .get("subject")
            .filter(|value| value.is_string())
            .cloned()
            .unwrap_or_else(|| json!("")),
    );
    if let Some(value) = normalize_recipient(message.get("from")) {
        payload
            .as_object_mut()
            .expect("object")
            .insert("from".to_string(), value);
    }
    if let Some(value) = normalize_recipient(message.get("sender")) {
        payload
            .as_object_mut()
            .expect("object")
            .insert("sender".to_string(), value);
    }
    payload.as_object_mut().expect("object").insert(
        "reply_to".to_string(),
        normalize_recipients(message.get("replyTo")),
    );
    payload.as_object_mut().expect("object").insert(
        "to".to_string(),
        normalize_recipients(message.get("toRecipients")),
    );
    payload.as_object_mut().expect("object").insert(
        "cc".to_string(),
        normalize_recipients(message.get("ccRecipients")),
    );
    payload.as_object_mut().expect("object").insert(
        "bcc".to_string(),
        normalize_recipients(message.get("bccRecipients")),
    );
    for (target, source) in [
        ("sent_at", "sentDateTime"),
        ("received_at", "receivedDateTime"),
        ("created_at", "createdDateTime"),
        ("last_modified_at", "lastModifiedDateTime"),
    ] {
        insert_optional_string(&mut payload, target, message.get(source));
    }
    let folders = message
        .get("parentFolderId")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| vec![json!(value)])
        .unwrap_or_default();
    payload
        .as_object_mut()
        .expect("object")
        .insert("folder_refs".to_string(), Value::Array(folders));
    let mut categories = message
        .get("categories")
        .and_then(Value::as_array)
        .map(|values| {
            values
                .iter()
                .filter_map(Value::as_str)
                .map(ToString::to_string)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    categories.sort();
    payload
        .as_object_mut()
        .expect("object")
        .insert("category_refs".to_string(), json!(categories));
    let importance = message
        .get("importance")
        .and_then(Value::as_str)
        .filter(|value| matches!(*value, "low" | "normal" | "high"));
    let flagged = message
        .get("flag")
        .and_then(Value::as_object)
        .and_then(|flag| flag.get("flagStatus"))
        .and_then(Value::as_str)
        .is_some_and(|value| matches!(value, "flagged" | "complete"));
    let mut flags = json!({
        "is_read":message.get("isRead").and_then(Value::as_bool).unwrap_or(false),
        "is_draft":message.get("isDraft").and_then(Value::as_bool).unwrap_or(false),
        "is_flagged":flagged,
        "has_attachments":message.get("hasAttachments").and_then(Value::as_bool).unwrap_or(false),
    });
    if let Some(value) = importance {
        flags
            .as_object_mut()
            .expect("object")
            .insert("importance".to_string(), json!(value));
    }
    payload
        .as_object_mut()
        .expect("object")
        .insert("flags".to_string(), flags);
    if scope.include_headers {
        if let Some(headers) = normalize_headers(message.get("internetMessageHeaders")) {
            payload
                .as_object_mut()
                .expect("object")
                .insert("headers".to_string(), headers);
        }
    }
    payload.as_object_mut().expect("object").insert(
        "body".to_string(),
        normalize_body(
            message.get("body"),
            &scope.body_policy,
            message.get("bodyPreview"),
        ),
    );
    payload.as_object_mut().expect("object").insert(
        "attachments".to_string(),
        normalize_attachments(message.get("attachments"), &scope.attachment_policy)?,
    );
    if let Some(extensions) = graph_message_extensions(message) {
        payload
            .as_object_mut()
            .expect("object")
            .insert("source_extensions".to_string(), extensions);
    }
    Ok(payload)
}

fn normalize_recipient(value: Option<&Value>) -> Option<Value> {
    let address = value
        .and_then(Value::as_object)
        .and_then(|value| value.get("emailAddress"))
        .and_then(Value::as_object)?;
    let display_name = address
        .get("name")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let email = address
        .get("address")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_lowercase);
    if display_name.is_none() && email.is_none() {
        return None;
    }
    let mut result = Map::new();
    if let Some(value) = display_name {
        result.insert("display_name".to_string(), json!(value));
    }
    if let Some(value) = email {
        result.insert("email".to_string(), json!(value));
    }
    Some(Value::Object(result))
}

fn normalize_recipients(value: Option<&Value>) -> Value {
    Value::Array(
        value
            .and_then(Value::as_array)
            .map(|values| {
                values
                    .iter()
                    .filter_map(|value| normalize_recipient(Some(value)))
                    .collect()
            })
            .unwrap_or_default(),
    )
}

fn normalize_headers(value: Option<&Value>) -> Option<Value> {
    let mut headers: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for header in value.and_then(Value::as_array)? {
        let Some(object) = header.as_object() else {
            continue;
        };
        let Some(name) = object
            .get("name")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_lowercase)
        else {
            continue;
        };
        let Some(value) = object.get("value").and_then(Value::as_str).map(str::trim) else {
            continue;
        };
        headers.entry(name).or_default().push(value.to_string());
    }
    if headers.is_empty() {
        return None;
    }
    Some(json!({"values":headers}))
}

fn normalize_body(body: Option<&Value>, policy: &str, preview: Option<&Value>) -> Value {
    let body = body.and_then(Value::as_object);
    let content_type = body
        .and_then(|value| value.get("contentType"))
        .and_then(Value::as_str);
    let content = body
        .and_then(|value| value.get("content"))
        .and_then(Value::as_str)
        .unwrap_or("");
    let preview = preview
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(normalize_line_endings);
    if content_type.is_none() || content.is_empty() {
        let mut value = json!({"body_kind":"empty"});
        if let Some(preview) = preview {
            value
                .as_object_mut()
                .expect("object")
                .insert("preview".to_string(), json!(preview));
        }
        return value;
    }
    let content = normalize_line_endings(content);
    let (kind, field, hash_field) = if content_type == Some("text")
        || (content_type == Some("html") && policy == "text_only")
    {
        ("text", "text", "text_sha256")
    } else if content_type == Some("html") {
        ("html", "html", "html_sha256")
    } else {
        let mut value = json!({"body_kind":"empty"});
        if let Some(preview) = preview {
            value
                .as_object_mut()
                .expect("object")
                .insert("preview".to_string(), json!(preview));
        }
        return value;
    };
    let mut value = json!({"body_kind":kind});
    value
        .as_object_mut()
        .expect("object")
        .insert(field.to_string(), json!(content));
    if let Some(preview) = preview {
        value
            .as_object_mut()
            .expect("object")
            .insert("preview".to_string(), json!(preview));
    }
    value.as_object_mut().expect("object").insert(
        "content_hashes".to_string(),
        json!({hash_field:sha256_hex(content.as_bytes())}),
    );
    value
}

fn normalize_attachments(value: Option<&Value>, policy: &str) -> Result<Value, Value> {
    if policy == "exclude" {
        return Ok(json!([]));
    }
    let mut normalized = Vec::new();
    for (ordinal, raw) in value
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .enumerate()
    {
        let attachment = raw.as_object().cloned().unwrap_or_default();
        let kind = attachment
            .get("@odata.type")
            .and_then(Value::as_str)
            .unwrap_or("");
        let display_name = attachment
            .get("name")
            .and_then(Value::as_str)
            .map(str::trim)
            .unwrap_or("")
            .to_string();
        let id = optional_trimmed(attachment.get("id"));
        let content_type = optional_trimmed(attachment.get("contentType"));
        let content_id = optional_trimmed(attachment.get("contentId"));
        let size = attachment.get("size").and_then(Value::as_i64);
        let inline = attachment
            .get("isInline")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let mut material = vec![
            if kind == "#microsoft.graph.fileAttachment" {
                json!("file")
            } else if kind == "#microsoft.graph.referenceAttachment" {
                json!("reference")
            } else {
                json!("item")
            },
            id.clone().map(Value::String).unwrap_or(Value::Null),
            json!(display_name),
        ];
        let mut result = Map::new();
        let content_hash = if kind == "#microsoft.graph.fileAttachment" {
            attachment
                .get("contentBytes")
                .and_then(Value::as_str)
                .filter(|value| !value.is_empty())
                .map(decode_base64)
                .transpose()?
                .map(|bytes| sha256_hex(&bytes))
        } else {
            None
        };
        if kind == "#microsoft.graph.fileAttachment" {
            material.extend([
                content_type
                    .clone()
                    .map(Value::String)
                    .unwrap_or(Value::Null),
                size.map(|value| json!(value)).unwrap_or(Value::Null),
                content_hash
                    .clone()
                    .map(Value::String)
                    .unwrap_or(Value::Null),
                json!(ordinal),
            ]);
        } else if kind == "#microsoft.graph.referenceAttachment" {
            material.extend([
                attachment.get("sourceUrl").cloned().unwrap_or(Value::Null),
                attachment
                    .get("providerType")
                    .cloned()
                    .unwrap_or(Value::Null),
                json!(ordinal),
            ]);
        } else {
            material.extend([
                content_type
                    .clone()
                    .map(Value::String)
                    .unwrap_or(Value::Null),
                size.map(|value| json!(value)).unwrap_or(Value::Null),
                json!(ordinal),
            ]);
        }
        let key_material = serde_json::to_string(&material)
            .map_err(|e| error("mailbox_attachment_identity_failed", &e.to_string()))?;
        let attachment_key = format!("att_{}", sha256_hex(key_material.as_bytes()));
        result.insert("attachment_key".to_string(), json!(attachment_key));
        if let Some(value) = id {
            result.insert("source_attachment_id".to_string(), json!(value));
        }
        result.insert("ordinal".to_string(), json!(ordinal));
        result.insert("display_name".to_string(), json!(display_name));
        if let Some(value) = content_type {
            result.insert("content_type".to_string(), json!(value));
        }
        if let Some(value) = size {
            result.insert("size_bytes".to_string(), json!(value));
        }
        result.insert("inline".to_string(), json!(inline));
        if let Some(value) = content_id {
            result.insert("content_id".to_string(), json!(value));
        }
        if let Some(value) = content_hash {
            result.insert("content_hash".to_string(), json!(value));
        }
        if kind == "#microsoft.graph.fileAttachment" && policy == "include_content" {
            if let Some(value) = attachment
                .get("contentBytes")
                .and_then(Value::as_str)
                .filter(|value| !value.is_empty())
            {
                result.insert(
                    "content_ref".to_string(),
                    json!(format!("inline-base64:{value}")),
                );
            }
        } else if kind == "#microsoft.graph.referenceAttachment" {
            if let Some(value) = optional_trimmed(attachment.get("sourceUrl")) {
                result.insert("content_ref".to_string(), json!(value));
            }
        }
        if let Some(extensions) = attachment_extensions(&attachment, kind) {
            result.insert("source_extensions".to_string(), extensions);
        }
        normalized.push(Value::Object(result));
    }
    normalized.sort_by(|left, right| {
        left.get("attachment_key")
            .and_then(Value::as_str)
            .cmp(&right.get("attachment_key").and_then(Value::as_str))
    });
    Ok(Value::Array(normalized))
}

fn attachment_extensions(attachment: &Map<String, Value>, kind: &str) -> Option<Value> {
    let mut graph = Map::new();
    if kind != "#microsoft.graph.fileAttachment" {
        graph.insert("odata_type".to_string(), json!(kind));
    }
    if kind == "#microsoft.graph.referenceAttachment" {
        for (target, source) in [
            ("source_url", "sourceUrl"),
            ("provider_type", "providerType"),
            ("permission", "permission"),
            ("is_folder", "isFolder"),
        ] {
            if let Some(value) = attachment.get(source) {
                graph.insert(target.to_string(), value.clone());
            }
        }
    }
    if let Some(value) = attachment.get("lastModifiedDateTime") {
        graph.insert("last_modified_at".to_string(), value.clone());
    }
    if graph.is_empty() {
        None
    } else {
        Some(json!({"namespaces":{"graph":Value::Object(graph)}}))
    }
}

fn graph_message_extensions(message: &Map<String, Value>) -> Option<Value> {
    let mut graph = Map::new();
    for (target, source) in [
        ("raw_id", "id"),
        ("change_key", "changeKey"),
        ("parent_folder_id", "parentFolderId"),
        ("queried_folder_ref", "sourceQueriedFolderRef"),
        ("web_link", "webLink"),
        ("inference_classification", "inferenceClassification"),
        ("flag", "flag"),
        ("unique_body", "uniqueBody"),
    ] {
        if let Some(value) = message.get(source) {
            graph.insert(target.to_string(), value.clone());
        }
    }
    if graph.is_empty() {
        None
    } else {
        Some(json!({"namespaces":{"graph":Value::Object(graph)}}))
    }
}

fn insert_optional_string(target: &mut Value, key: &str, value: Option<&Value>) {
    if let Some(value) = value
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
    {
        target
            .as_object_mut()
            .expect("object")
            .insert(key.to_string(), json!(value));
    }
}

fn process_batch(
    domain_db: &mut Connection,
    scope: &ScopeConfig,
    generation_id: &str,
    lease_token: &str,
    batch: &SourceBatch,
) -> Result<(), Value> {
    let facts_path = scope.root_dir.join(".narada/facts.db");
    if let Some(parent) = facts_path.parent() {
        fs::create_dir_all(parent)
            .map_err(|e| error("mailbox_fact_store_directory_failed", &e.to_string()))?;
    }
    let facts = Connection::open(facts_path)
        .map_err(|e| error("mailbox_fact_store_open_failed", &e.to_string()))?;
    facts
        .busy_timeout(Duration::from_millis(5_000))
        .map_err(|e| error("mailbox_fact_store_pragma_failed", &e.to_string()))?;
    facts
        .pragma_update(None, "journal_mode", "WAL")
        .map_err(|e| error("mailbox_fact_store_pragma_failed", &e.to_string()))?;
    init_fact_schema(&facts)?;
    for (index, record) in batch.records.iter().enumerate() {
        if index % 10 == 0 {
            renew_lease(domain_db, &scope.scope_id, generation_id, lease_token)?;
        }
        let marker = apply_marker_path(&scope.root_dir, &record.record_id);
        if marker.is_file() {
            validate_apply_marker(&marker)?;
            mark_record_application(
                domain_db,
                generation_id,
                &record.record_id,
                "already_applied",
            )?;
            continue;
        }
        let (fact_id, fact_type, provenance, payload_json) =
            fact_for_record(record, batch.next_checkpoint.as_deref())?;
        ingest_fact(&facts, &fact_id, &fact_type, &provenance, &payload_json)?;
        assert_lease(domain_db, &scope.scope_id, generation_id, lease_token)?;
        let applied = project_record(scope, record)?;
        assert_lease(domain_db, &scope.scope_id, generation_id, lease_token)?;
        mark_record_application(
            domain_db,
            generation_id,
            &record.record_id,
            if applied { "projected" } else { "not_applied" },
        )?;
        if applied {
            write_apply_marker(&marker, record)?;
        }
    }
    if let Some(next) = &batch.next_checkpoint {
        renew_lease(domain_db, &scope.scope_id, generation_id, lease_token)?;
        commit_cursor(scope, next)?;
        assert_lease(domain_db, &scope.scope_id, generation_id, lease_token)?;
    }
    Ok(())
}

fn init_fact_schema(db: &Connection) -> Result<(), Value> {
    db.execute_batch(
        r#"
        CREATE TABLE IF NOT EXISTS facts(
          fact_id TEXT PRIMARY KEY,
          fact_type TEXT NOT NULL,
          source_id TEXT NOT NULL,
          source_record_id TEXT NOT NULL,
          source_version TEXT,
          source_cursor TEXT,
          provenance_json TEXT NOT NULL,
          payload_json TEXT NOT NULL,
          created_at TEXT NOT NULL DEFAULT (datetime('now')),
          admitted_at TEXT
        );
        CREATE INDEX IF NOT EXISTS idx_facts_source_record ON facts(source_id,source_record_id);
        CREATE INDEX IF NOT EXISTS idx_facts_source_cursor ON facts(source_id,source_cursor,created_at);
        CREATE INDEX IF NOT EXISTS idx_facts_type ON facts(fact_type,created_at);
        CREATE INDEX IF NOT EXISTS idx_facts_admitted ON facts(source_id,admitted_at,created_at);
        "#,
    )
    .map_err(|e| error("mailbox_fact_schema_failed", &e.to_string()))
}

fn ingest_fact(
    db: &Connection,
    fact_id: &str,
    fact_type: &str,
    provenance: &Value,
    payload_json: &str,
) -> Result<(), Value> {
    if db
        .query_row(
            "SELECT 1 FROM facts WHERE fact_id=?",
            params![fact_id],
            |row| row.get::<_, i64>(0),
        )
        .optional()
        .map_err(|e| error("mailbox_fact_query_failed", &e.to_string()))?
        .is_some()
    {
        return Ok(());
    }
    db.execute(
        "INSERT INTO facts(fact_id,fact_type,source_id,source_record_id,source_version,source_cursor,provenance_json,payload_json,created_at) VALUES (?,?,?,?,?,?,?,?,datetime('now'))",
        params![
            fact_id,
            fact_type,
            provenance.get("source_id").and_then(Value::as_str),
            provenance.get("source_record_id").and_then(Value::as_str),
            provenance.get("source_version").and_then(Value::as_str),
            provenance.get("source_cursor").and_then(Value::as_str),
            serde_json::to_string(provenance).unwrap_or_else(|_| "{}".to_string()),
            payload_json,
        ],
    )
    .map_err(|e| error("mailbox_fact_insert_failed", &e.to_string()))?;
    Ok(())
}

fn project_record(scope: &ScopeConfig, record: &SourceRecord) -> Result<bool, Value> {
    let event = record.payload.as_object().ok_or_else(|| {
        error(
            "mailbox_projection_event_invalid",
            "mailbox_projection_event_invalid",
        )
    })?;
    let kind = event
        .get("event_kind")
        .and_then(Value::as_str)
        .unwrap_or("");
    let message_id = event
        .get("message_id")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            error(
                "mailbox_projection_message_id_missing",
                "mailbox_projection_message_id_missing",
            )
        })?;
    match kind {
        "upsert" | "created" | "updated" => {
            let payload = event.get("payload").ok_or_else(|| {
                error(
                    "mailbox_projection_payload_missing",
                    &format!("Upsert event {} is missing payload", record.record_id),
                )
            })?;
            install_blobs(&scope.root_dir, payload)?;
            write_message_projection(&scope.root_dir, payload)?;
            if scope.tombstones_enabled {
                remove_path(
                    &scope
                        .root_dir
                        .join("tombstones")
                        .join(format!("{}.json", safe_segment(message_id))),
                )?;
            }
            mark_views(&scope.root_dir, payload)?;
            Ok(true)
        }
        "delete" | "deleted" => {
            if scope.tombstones_enabled {
                write_tombstone(&scope.root_dir, event)?;
            }
            let message_path = scope
                .root_dir
                .join("messages")
                .join(safe_segment(message_id));
            if message_path.exists() {
                fs::remove_dir_all(&message_path).map_err(|e| {
                    error("mailbox_projection_message_remove_failed", &e.to_string())
                })?;
            }
            unlink_view(
                &scope
                    .root_dir
                    .join("views/unread")
                    .join(safe_segment(message_id)),
            )?;
            unlink_view(
                &scope
                    .root_dir
                    .join("views/flagged")
                    .join(safe_segment(message_id)),
            )?;
            Ok(true)
        }
        _ => Err(error(
            "mailbox_projection_event_kind_unknown",
            &format!("Unknown event kind: {kind}"),
        )),
    }
}

fn install_blobs(root: &Path, payload: &Value) -> Result<(), Value> {
    for attachment in payload
        .get("attachments")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let Some(content) = attachment
            .get("content_ref")
            .and_then(Value::as_str)
            .and_then(|value| value.strip_prefix("inline-base64:"))
        else {
            continue;
        };
        let bytes = decode_base64(content)?;
        let hash = sha256_hex(&bytes);
        let destination = root
            .join("blobs/sha256")
            .join(&hash[..2])
            .join(&hash[2..4])
            .join(&hash);
        if destination.is_file() {
            continue;
        }
        if let Some(parent) = destination.parent() {
            fs::create_dir_all(parent)
                .map_err(|e| error("mailbox_blob_directory_failed", &e.to_string()))?;
        }
        let temporary = root
            .join("tmp")
            .join(format!("blob.{hash}.{}.tmp", Uuid::new_v4()));
        fs::write(&temporary, &bytes)
            .map_err(|e| error("mailbox_blob_write_failed", &e.to_string()))?;
        match fs::rename(&temporary, &destination) {
            Ok(()) => {}
            Err(_) if destination.is_file() => {
                let _ = fs::remove_file(&temporary);
            }
            Err(value) => {
                let _ = fs::remove_file(&temporary);
                return Err(error("mailbox_blob_install_failed", &value.to_string()));
            }
        }
    }
    Ok(())
}

fn write_message_projection(root: &Path, payload: &Value) -> Result<(), Value> {
    let message_id = payload
        .get("message_id")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            error(
                "mailbox_projection_message_id_missing",
                "mailbox_projection_message_id_missing",
            )
        })?;
    let destination = root.join("messages").join(safe_segment(message_id));
    let existing = if destination.join("record.json").is_file() {
        let text = fs::read_to_string(destination.join("record.json"))
            .map_err(|e| error("mailbox_projection_message_read_failed", &e.to_string()))?;
        serde_json::from_str::<Value>(&text)
            .map_err(|e| error("mailbox_projection_message_invalid", &e.to_string()))?
    } else {
        Value::Null
    };
    let merged = merge_message_payload(&existing, payload);
    let nonce = format!("{}.{}", std::process::id(), Uuid::new_v4());
    let staging = root
        .join("tmp")
        .join(format!("message.{}.{nonce}", safe_segment(message_id)));
    let prior = destination.with_extension(format!("prior.{nonce}"));
    for relative in ["body", "attachments/by-id", "attachments/by-name"] {
        fs::create_dir_all(staging.join(relative)).map_err(|e| {
            error(
                "mailbox_projection_message_directory_failed",
                &e.to_string(),
            )
        })?;
    }
    if let Some(text) = merged
        .get("body")
        .and_then(|value| value.get("text"))
        .and_then(Value::as_str)
    {
        fs::write(staging.join("body/body.txt"), text)
            .map_err(|e| error("mailbox_projection_message_write_failed", &e.to_string()))?;
    }
    if let Some(html) = merged
        .get("body")
        .and_then(|value| value.get("html"))
        .and_then(Value::as_str)
    {
        fs::write(staging.join("body/body.html"), html)
            .map_err(|e| error("mailbox_projection_message_write_failed", &e.to_string()))?;
    }
    let attachments = merged
        .get("attachments")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let manifest = attachment_manifest(&attachments);
    atomic_write_json_pretty(
        &staging.join("attachments/manifest.json"),
        &Value::Array(manifest),
    )?;
    for attachment in &attachments {
        let Some(encoded) = attachment
            .get("content_ref")
            .and_then(Value::as_str)
            .and_then(|value| value.strip_prefix("inline-base64:"))
        else {
            continue;
        };
        let bytes = decode_base64(encoded)?;
        let key = attachment
            .get("attachment_key")
            .and_then(Value::as_str)
            .unwrap_or("attachment");
        let name = attachment
            .get("display_name")
            .and_then(Value::as_str)
            .filter(|value| !value.trim().is_empty())
            .unwrap_or(key);
        fs::write(
            staging.join("attachments/by-id").join(safe_segment(key)),
            &bytes,
        )
        .map_err(|e| error("mailbox_projection_attachment_write_failed", &e.to_string()))?;
        fs::write(
            staging.join("attachments/by-name").join(safe_segment(name)),
            &bytes,
        )
        .map_err(|e| error("mailbox_projection_attachment_write_failed", &e.to_string()))?;
    }
    let mut record = merged.as_object().cloned().unwrap_or_default();
    let mut body_refs = Map::new();
    if merged
        .get("body")
        .and_then(|value| value.get("text"))
        .is_some()
    {
        body_refs.insert("text".to_string(), json!("body/body.txt"));
    }
    if merged
        .get("body")
        .and_then(|value| value.get("html"))
        .is_some()
    {
        body_refs.insert("html".to_string(), json!("body/body.html"));
    }
    record.insert("body_refs".to_string(), Value::Object(body_refs));
    record.insert(
        "attachment_manifest_ref".to_string(),
        json!("attachments/manifest.json"),
    );
    record.insert("_checksum".to_string(), json!(""));
    let checksum = &sha256_hex(
        serde_json::to_string(&Value::Object(record.clone()))
            .map_err(|e| error("mailbox_projection_message_encode_failed", &e.to_string()))?
            .as_bytes(),
    )[..16];
    record.insert("_checksum".to_string(), json!(checksum));
    atomic_write_json_pretty(&staging.join("record.json"), &Value::Object(record))?;
    let existed = destination.exists();
    if existed {
        fs::rename(&destination, &prior)
            .map_err(|e| error("mailbox_projection_message_replace_failed", &e.to_string()))?;
    }
    if let Err(value) = fs::rename(&staging, &destination) {
        if existed && !destination.exists() && prior.exists() {
            let _ = fs::rename(&prior, &destination);
        }
        let _ = fs::remove_dir_all(&staging);
        return Err(error(
            "mailbox_projection_message_replace_failed",
            &value.to_string(),
        ));
    }
    if prior.exists() {
        fs::remove_dir_all(&prior)
            .map_err(|e| error("mailbox_projection_message_cleanup_failed", &e.to_string()))?;
    }
    Ok(())
}

fn merge_message_payload(existing: &Value, incoming: &Value) -> Value {
    let Some(existing) = existing.as_object() else {
        return incoming.clone();
    };
    let Some(incoming) = incoming.as_object() else {
        return incoming.clone();
    };
    if existing.get("message_id") != incoming.get("message_id") {
        return Value::Object(incoming.clone());
    }
    let mut merged = existing.clone();
    for (key, value) in incoming {
        merged.insert(key.clone(), value.clone());
    }
    for key in [
        "conversation_id",
        "internet_message_id",
        "subject",
        "from",
        "sender",
        "received_at",
        "sent_at",
        "created_at",
        "last_modified_at",
    ] {
        let incoming_has = incoming.get(key).is_some_and(has_meaningful_value);
        if !incoming_has {
            if let Some(value) = existing.get(key) {
                merged.insert(key.to_string(), value.clone());
            }
        }
    }
    for key in ["reply_to", "to", "cc", "bcc"] {
        if incoming
            .get(key)
            .and_then(Value::as_array)
            .map(|value| value.is_empty())
            .unwrap_or(true)
        {
            if let Some(value) = existing
                .get(key)
                .and_then(Value::as_array)
                .filter(|value| !value.is_empty())
            {
                merged.insert(key.to_string(), Value::Array(value.clone()));
            }
        }
    }
    if incoming
        .get("attachments")
        .and_then(Value::as_array)
        .map(|value| value.is_empty())
        .unwrap_or(true)
    {
        if let Some(value) = existing
            .get("attachments")
            .and_then(Value::as_array)
            .filter(|value| !value.is_empty())
        {
            merged.insert("attachments".to_string(), Value::Array(value.clone()));
        }
    }
    if incoming
        .get("body")
        .and_then(|value| value.get("body_kind"))
        .and_then(Value::as_str)
        == Some("empty")
    {
        if let Some(old_body) = existing.get("body").and_then(Value::as_object) {
            if old_body.get("text").is_some() || old_body.get("html").is_some() {
                let mut body = incoming
                    .get("body")
                    .and_then(Value::as_object)
                    .cloned()
                    .unwrap_or_default();
                for key in ["body_kind", "text", "html", "content_hashes"] {
                    if let Some(value) = old_body.get(key) {
                        body.insert(key.to_string(), value.clone());
                    }
                }
                merged.insert("body".to_string(), Value::Object(body));
            }
        }
    }
    if let (Some(old), Some(new)) = (
        existing
            .get("source_extensions")
            .and_then(|value| value.get("namespaces"))
            .and_then(|value| value.get("graph"))
            .and_then(Value::as_object),
        incoming
            .get("source_extensions")
            .and_then(|value| value.get("namespaces"))
            .and_then(|value| value.get("graph"))
            .and_then(Value::as_object),
    ) {
        let mut graph = old.clone();
        graph.extend(new.clone());
        merged.insert(
            "source_extensions".to_string(),
            json!({"namespaces":{"graph":Value::Object(graph)}}),
        );
    }
    Value::Object(merged)
}

fn has_meaningful_value(value: &Value) -> bool {
    match value {
        Value::String(value) => !value.trim().is_empty(),
        Value::Array(value) => !value.is_empty(),
        Value::Object(value) => !value.is_empty(),
        Value::Null => false,
        _ => true,
    }
}

fn attachment_manifest(attachments: &[Value]) -> Vec<Value> {
    attachments
        .iter()
        .map(|attachment| {
            let mut value = Map::new();
            for key in [
                "attachment_key",
                "source_attachment_id",
                "ordinal",
                "display_name",
                "content_type",
                "size_bytes",
                "inline",
                "content_id",
                "content_hash",
                "content_ref",
                "source_extensions",
            ] {
                if let Some(field) = attachment.get(key) {
                    value.insert(key.to_string(), field.clone());
                }
            }
            if attachment
                .get("content_ref")
                .and_then(Value::as_str)
                .is_some_and(|value| value.starts_with("inline-base64:"))
            {
                if let Some(key) = attachment.get("attachment_key").and_then(Value::as_str) {
                    value.insert(
                        "content_file_ref".to_string(),
                        json!(format!("attachments/by-id/{}", safe_segment(key))),
                    );
                }
            }
            Value::Object(value)
        })
        .collect()
}

fn mark_views(root: &Path, payload: &Value) -> Result<(), Value> {
    let message_id = payload
        .get("message_id")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            error(
                "mailbox_projection_message_id_missing",
                "mailbox_projection_message_id_missing",
            )
        })?;
    let message_path = root.join("messages").join(safe_segment(message_id));
    if let Some(conversation) = payload
        .get("conversation_id")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
    {
        link_view(
            &root
                .join("views/by-thread")
                .join(safe_segment(conversation))
                .join("members")
                .join(safe_segment(message_id)),
            &message_path,
        )?;
    }
    for folder in payload
        .get("folder_refs")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
    {
        link_view(
            &root
                .join("views/by-folder")
                .join(safe_segment(folder))
                .join("members")
                .join(safe_segment(message_id)),
            &message_path,
        )?;
    }
    let unread = root.join("views/unread").join(safe_segment(message_id));
    if payload
        .get("flags")
        .and_then(|value| value.get("is_read"))
        .and_then(Value::as_bool)
        != Some(true)
    {
        link_view(&unread, &message_path)?;
    } else {
        unlink_view(&unread)?;
    }
    let flagged = root.join("views/flagged").join(safe_segment(message_id));
    if payload
        .get("flags")
        .and_then(|value| value.get("is_flagged"))
        .and_then(Value::as_bool)
        == Some(true)
    {
        link_view(&flagged, &message_path)?;
    } else {
        unlink_view(&flagged)?;
    }
    Ok(())
}

fn link_view(path: &Path, target: &Path) -> Result<(), Value> {
    unlink_view(path)?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|e| error("mailbox_projection_view_directory_failed", &e.to_string()))?;
    }
    #[cfg(windows)]
    let result = create_windows_junction(target, path);
    #[cfg(unix)]
    let result = std::os::unix::fs::symlink(target, path);
    result.map_err(|e| error("mailbox_projection_view_link_failed", &e.to_string()))
}

#[cfg(windows)]
fn create_windows_junction(target: &Path, link: &Path) -> std::io::Result<()> {
    use std::ffi::c_void;
    use std::os::windows::ffi::OsStrExt;
    use std::ptr::null_mut;

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn CreateFileW(
            file_name: *const u16,
            desired_access: u32,
            share_mode: u32,
            security_attributes: *mut c_void,
            creation_disposition: u32,
            flags_and_attributes: u32,
            template_file: *mut c_void,
        ) -> *mut c_void;
        fn DeviceIoControl(
            device: *mut c_void,
            control_code: u32,
            input: *mut c_void,
            input_size: u32,
            output: *mut c_void,
            output_size: u32,
            bytes_returned: *mut u32,
            overlapped: *mut c_void,
        ) -> i32;
        fn CloseHandle(handle: *mut c_void) -> i32;
    }

    const GENERIC_WRITE: u32 = 0x4000_0000;
    const OPEN_EXISTING: u32 = 3;
    const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
    const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
    const IO_REPARSE_TAG_MOUNT_POINT: u32 = 0xA000_0003;
    const FSCTL_SET_REPARSE_POINT: u32 = 0x0009_00A4;

    fs::create_dir(link)?;
    let target = fs::canonicalize(target)?;
    let printable = normalized_path_text(&target);
    let substitute = format!(r"\??\{printable}");
    let substitute_wide = std::ffi::OsStr::new(&substitute)
        .encode_wide()
        .collect::<Vec<_>>();
    let printable_wide = std::ffi::OsStr::new(&printable)
        .encode_wide()
        .collect::<Vec<_>>();
    let substitute_bytes = (substitute_wide.len() * 2) as u16;
    let printable_bytes = (printable_wide.len() * 2) as u16;
    let path_bytes = substitute_bytes as usize + 2 + printable_bytes as usize + 2;
    let data_length = (8 + path_bytes) as u16;
    let mut buffer = Vec::with_capacity(8 + data_length as usize);
    buffer.extend_from_slice(&IO_REPARSE_TAG_MOUNT_POINT.to_le_bytes());
    buffer.extend_from_slice(&data_length.to_le_bytes());
    buffer.extend_from_slice(&0_u16.to_le_bytes());
    buffer.extend_from_slice(&0_u16.to_le_bytes());
    buffer.extend_from_slice(&substitute_bytes.to_le_bytes());
    buffer.extend_from_slice(&(substitute_bytes + 2).to_le_bytes());
    buffer.extend_from_slice(&printable_bytes.to_le_bytes());
    for value in substitute_wide {
        buffer.extend_from_slice(&value.to_le_bytes());
    }
    buffer.extend_from_slice(&0_u16.to_le_bytes());
    for value in printable_wide {
        buffer.extend_from_slice(&value.to_le_bytes());
    }
    buffer.extend_from_slice(&0_u16.to_le_bytes());

    let mut link_wide = link.as_os_str().encode_wide().collect::<Vec<_>>();
    link_wide.push(0);
    let handle = unsafe {
        CreateFileW(
            link_wide.as_ptr(),
            GENERIC_WRITE,
            0,
            null_mut(),
            OPEN_EXISTING,
            FILE_FLAG_OPEN_REPARSE_POINT | FILE_FLAG_BACKUP_SEMANTICS,
            null_mut(),
        )
    };
    if handle as isize == -1 {
        let failure = std::io::Error::last_os_error();
        let _ = fs::remove_dir(link);
        return Err(failure);
    }
    let mut returned = 0_u32;
    let success = unsafe {
        DeviceIoControl(
            handle,
            FSCTL_SET_REPARSE_POINT,
            buffer.as_mut_ptr().cast(),
            buffer.len() as u32,
            null_mut(),
            0,
            &mut returned,
            null_mut(),
        )
    };
    unsafe {
        CloseHandle(handle);
    }
    if success == 0 {
        let failure = std::io::Error::last_os_error();
        let _ = fs::remove_dir(link);
        return Err(failure);
    }
    Ok(())
}

fn unlink_view(path: &Path) -> Result<(), Value> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(value) => value,
        Err(value) if value.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(value) => {
            return Err(error(
                "mailbox_projection_view_stat_failed",
                &value.to_string(),
            ))
        }
    };
    if metadata.file_type().is_symlink() || metadata.is_file() {
        fs::remove_file(path)
    } else {
        fs::remove_dir_all(path)
    }
    .map_err(|e| error("mailbox_projection_view_remove_failed", &e.to_string()))
}

fn write_tombstone(root: &Path, event: &Map<String, Value>) -> Result<(), Value> {
    let message_id = event
        .get("message_id")
        .and_then(Value::as_str)
        .unwrap_or("");
    let mut value = json!({
        "message_id":message_id,
        "mailbox_id":event.get("mailbox_id").cloned().unwrap_or(Value::Null),
        "deleted_by_event_id":event.get("event_id").cloned().unwrap_or(Value::Null),
    });
    if let Some(source_version) = event.get("source_version") {
        value
            .as_object_mut()
            .expect("object")
            .insert("source_version".to_string(), source_version.clone());
    }
    value.as_object_mut().expect("object").insert(
        "observed_at".to_string(),
        event.get("observed_at").cloned().unwrap_or(Value::Null),
    );
    atomic_write_json_pretty(
        &root
            .join("tombstones")
            .join(format!("{}.json", safe_segment(message_id))),
        &value,
    )
}

fn apply_marker_path(root: &Path, record_id: &str) -> PathBuf {
    let shard = record_id.get(..2).unwrap_or("00");
    root.join("state/apply-log")
        .join(shard)
        .join(format!("{record_id}.json"))
}

fn validate_apply_marker(path: &Path) -> Result<(), Value> {
    let value: Value = serde_json::from_str(
        &fs::read_to_string(path)
            .map_err(|e| error("mailbox_apply_marker_read_failed", &e.to_string()))?,
    )
    .map_err(|e| error("mailbox_apply_marker_invalid", &e.to_string()))?;
    let valid = ["event_id", "message_id", "event_kind", "applied_at"]
        .iter()
        .all(|key| {
            value
                .get(*key)
                .and_then(Value::as_str)
                .is_some_and(|value| !value.trim().is_empty())
        });
    if !valid {
        return Err(error(
            "mailbox_apply_marker_invalid",
            "mailbox_apply_marker_invalid",
        ));
    }
    Ok(())
}

fn write_apply_marker(path: &Path, record: &SourceRecord) -> Result<(), Value> {
    if path.is_file() {
        return validate_apply_marker(path);
    }
    atomic_write_json_pretty(
        path,
        &json!({
            "event_id":record.record_id,
            "message_id":record.payload.get("message_id").and_then(Value::as_str).unwrap_or(""),
            "event_kind":record.payload.get("event_kind").and_then(Value::as_str).unwrap_or("upsert"),
            "applied_at":now_iso_millis(),
        }),
    )
}

fn safe_segment(value: &str) -> String {
    encode_component(value)
}

fn remove_path(path: &Path) -> Result<(), Value> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(value) if value.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(value) => Err(error(
            "mailbox_projection_remove_failed",
            &value.to_string(),
        )),
    }
}

fn batch_to_value(batch: &SourceBatch) -> Value {
    let records = batch
        .records
        .iter()
        .map(|record| {
            let mut value = json!({"recordId":record.record_id});
            if let Some(ordinal) = &record.ordinal {
                value
                    .as_object_mut()
                    .expect("object")
                    .insert("ordinal".to_string(), json!(ordinal));
            }
            value
                .as_object_mut()
                .expect("object")
                .insert("payload".to_string(), record.payload.clone());
            value
                .as_object_mut()
                .expect("object")
                .insert("provenance".to_string(), record.provenance.clone());
            value
        })
        .collect::<Vec<_>>();
    let mut value = json!({"records":records});
    value.as_object_mut().expect("object").insert(
        "priorCheckpoint".to_string(),
        batch
            .prior_checkpoint
            .clone()
            .map(Value::String)
            .unwrap_or(Value::Null),
    );
    if let Some(next) = &batch.next_checkpoint {
        value
            .as_object_mut()
            .expect("object")
            .insert("nextCheckpoint".to_string(), json!(next));
    }
    value
        .as_object_mut()
        .expect("object")
        .insert("hasMore".to_string(), json!(batch.has_more));
    value
        .as_object_mut()
        .expect("object")
        .insert("fetchedAt".to_string(), json!(batch.fetched_at));
    value
}

fn batch_from_value(value: &Value) -> Result<SourceBatch, Value> {
    let object = value.as_object().ok_or_else(|| {
        error(
            "mailbox_sync_generation_batch_invalid",
            "mailbox_sync_generation_batch_invalid",
        )
    })?;
    let raw_records = object
        .get("records")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            error(
                "mailbox_sync_generation_records_invalid",
                "mailbox_sync_generation_records_invalid",
            )
        })?;
    if raw_records.len() > MAX_GRAPH_RECORDS {
        return Err(error(
            "mailbox_sync_generation_records_too_many",
            "mailbox_sync_generation_records_too_many",
        ));
    }
    let mut records = Vec::with_capacity(raw_records.len());
    for raw in raw_records {
        let record = raw.as_object().ok_or_else(|| {
            error(
                "mailbox_sync_generation_record_invalid",
                "mailbox_sync_generation_record_invalid",
            )
        })?;
        let record_id = record
            .get("recordId")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| {
                error(
                    "mailbox_sync_generation_record_invalid",
                    "mailbox_sync_generation_record_invalid",
                )
            })?
            .to_string();
        records.push(SourceRecord {
            record_id,
            ordinal: record
                .get("ordinal")
                .and_then(Value::as_str)
                .map(ToString::to_string),
            payload: record.get("payload").cloned().unwrap_or(Value::Null),
            provenance: record
                .get("provenance")
                .cloned()
                .unwrap_or_else(|| json!({})),
        });
    }
    Ok(SourceBatch {
        records,
        prior_checkpoint: object
            .get("priorCheckpoint")
            .and_then(Value::as_str)
            .map(ToString::to_string),
        next_checkpoint: object
            .get("nextCheckpoint")
            .and_then(Value::as_str)
            .map(ToString::to_string),
        has_more: object
            .get("hasMore")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        fetched_at: object
            .get("fetchedAt")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
    })
}

fn staged_record(
    record: &SourceRecord,
    source_cursor: Option<&str>,
) -> Result<StagedRecord, Value> {
    let event = record.payload.as_object().ok_or_else(|| {
        let code = format!("mailbox_sync_record_payload_invalid:{}", record.record_id);
        error(&code, &code)
    })?;
    Ok(StagedRecord {
        record_id: record.record_id.clone(),
        ordinal: record.ordinal.clone(),
        fact_id: fact_for_record(record, source_cursor)?.0,
        event_kind: event
            .get("event_kind")
            .and_then(Value::as_str)
            .unwrap_or("unknown")
            .chars()
            .take(64)
            .collect(),
        message_id: optional_trimmed(event.get("message_id"))
            .map(|value| value.chars().take(512).collect()),
        mailbox_id: optional_trimmed(event.get("mailbox_id"))
            .map(|value| value.chars().take(512).collect()),
        conversation_id: optional_trimmed(event.get("conversation_id"))
            .map(|value| value.chars().take(1024).collect()),
        source_version: optional_trimmed(event.get("source_version"))
            .map(|value| value.chars().take(1024).collect()),
        application_status: "staged".to_string(),
    })
}

fn fact_for_record(
    record: &SourceRecord,
    source_cursor: Option<&str>,
) -> Result<(String, String, Value, String), Value> {
    let event_kind = record.payload.get("event_kind").and_then(Value::as_str);
    let fact_type = match event_kind {
        Some("created" | "upsert") => "mail.message.discovered",
        Some("updated") => "mail.message.changed",
        Some("deleted" | "delete") => "mail.message.removed",
        _ => "mail.message.discovered",
    };
    let source_id = record
        .provenance
        .get("sourceId")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            error(
                "mailbox_sync_record_provenance_invalid",
                "mailbox_sync_record_provenance_invalid",
            )
        })?;
    let observed_at = record
        .provenance
        .get("observedAt")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            error(
                "mailbox_sync_record_provenance_invalid",
                "mailbox_sync_record_provenance_invalid",
            )
        })?;
    let source_version = record
        .provenance
        .get("sourceVersion")
        .cloned()
        .unwrap_or(Value::Null);
    let provenance = json!({
        "source_id":source_id,
        "source_record_id":record.record_id,
        "source_version":source_version,
        "source_cursor":source_cursor,
        "observed_at":observed_at,
    });
    let payload = json!({
        "record_id":record.record_id,
        "ordinal":record.ordinal,
        "event":record.payload,
    });
    let identity = json!({
        "fact_type":fact_type,
        "source_id":source_id,
        "source_record_id":record.record_id,
        "payload":payload,
    });
    let fact_id = format!(
        "fact_{}",
        &sha256_hex(canonical_json(&identity).as_bytes())[..32]
    );
    let payload_json = serde_json::to_string(&payload)
        .map_err(|e| error("mailbox_fact_payload_encode_failed", &e.to_string()))?;
    Ok((fact_id, fact_type.to_string(), provenance, payload_json))
}

struct SyncLock {
    path: PathBuf,
}

impl SyncLock {
    fn acquire(scope: &ScopeConfig) -> Result<Self, String> {
        let path = scope.root_dir.join("state/sync.lock");
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        let started = Instant::now();
        loop {
            match fs::create_dir(&path) {
                Ok(()) => {
                    let metadata = json!({
                        "pid":std::process::id(),
                        "acquired_at":now_iso_millis(),
                        "platform":std::env::consts::OS,
                    });
                    if let Err(value) = atomic_write_json(&path.join("meta.json"), &metadata) {
                        let _ = fs::remove_dir_all(&path);
                        return Err(value
                            .get("message")
                            .and_then(Value::as_str)
                            .unwrap_or("sync_lock_metadata_failed")
                            .to_string());
                    }
                    return Ok(Self { path });
                }
                Err(value) if value.kind() == std::io::ErrorKind::AlreadyExists => {
                    let stale = fs::metadata(&path)
                        .and_then(|metadata| metadata.modified())
                        .ok()
                        .and_then(|modified| modified.elapsed().ok())
                        .is_some_and(|age| age > Duration::from_secs(3600));
                    if stale {
                        let _ = fs::remove_dir_all(&path);
                        continue;
                    }
                    if started.elapsed() >= Duration::from_millis(scope.acquire_lock_timeout_ms) {
                        return Err(format!(
                            "Failed to acquire lock within {}ms timeout",
                            scope.acquire_lock_timeout_ms
                        ));
                    }
                    thread::sleep(Duration::from_millis(250));
                }
                Err(value) => return Err(value.to_string()),
            }
        }
    }
}

impl Drop for SyncLock {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

fn ensure_root_layout(root: &Path) -> Result<(), Value> {
    for relative in ["state", "messages", "tombstones", "views", "blobs", "tmp"] {
        fs::create_dir_all(root.join(relative))
            .map_err(|e| error("mailbox_projection_layout_failed", &e.to_string()))?;
    }
    Ok(())
}

fn cleanup_tmp(root: &Path) -> Result<(), Value> {
    let path = root.join("tmp");
    if !path.is_dir() {
        return Ok(());
    }
    for entry in fs::read_dir(&path)
        .map_err(|e| error("mailbox_projection_tmp_read_failed", &e.to_string()))?
        .take(10_000)
    {
        let path = entry
            .map_err(|e| error("mailbox_projection_tmp_read_failed", &e.to_string()))?
            .path();
        if path.is_dir() {
            fs::remove_dir_all(&path)
        } else {
            fs::remove_file(&path)
        }
        .map_err(|e| error("mailbox_projection_tmp_cleanup_failed", &e.to_string()))?;
    }
    Ok(())
}

fn read_cursor(scope: &ScopeConfig) -> Result<Option<String>, Value> {
    let path = scope.root_dir.join("state/cursor.json");
    if !path.is_file() {
        return Ok(None);
    }
    let value: Value = serde_json::from_str(
        &fs::read_to_string(&path)
            .map_err(|e| error("mailbox_cursor_read_failed", &e.to_string()))?,
    )
    .map_err(|e| error("mailbox_cursor_invalid", &e.to_string()))?;
    if value.get("scope_id").and_then(Value::as_str) != Some(&scope.scope_id) {
        return Err(error(
            "mailbox_cursor_scope_mismatch",
            "mailbox_cursor_scope_mismatch",
        ));
    }
    let cursor = value
        .get("committed_cursor")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| error("mailbox_cursor_invalid", "mailbox_cursor_invalid"))?;
    Ok(Some(cursor.to_string()))
}

fn commit_cursor(scope: &ScopeConfig, cursor: &str) -> Result<(), Value> {
    if cursor.trim().is_empty() {
        return Err(error(
            "mailbox_cursor_invalid",
            "Cannot commit empty cursor",
        ));
    }
    atomic_write_json_pretty(
        &scope.root_dir.join("state/cursor.json"),
        &json!({
            "scope_id":scope.scope_id,
            "committed_cursor":cursor,
            "committed_at":now_iso_millis(),
        }),
    )
}

fn required_bounded(
    args: &Map<String, Value>,
    key: &str,
    code: &str,
    max: usize,
) -> Result<String, Value> {
    let value = args
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| error(code, code))?;
    if value.chars().count() > max {
        return Err(error(
            "mailbox_string_argument_too_long",
            "mailbox_string_argument_too_long",
        ));
    }
    Ok(value.to_string())
}

fn required_value_string(value: &Value, key: &str, code: &str) -> Result<String, Value> {
    value
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
        .ok_or_else(|| error(code, code))
}

fn optional_string(value: Option<&Value>) -> Option<String> {
    value
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
}

fn optional_trimmed(value: Option<&Value>) -> Option<String> {
    optional_string(value)
}

fn string_array(
    value: Option<&Value>,
    fallback: &[&str],
    code: &str,
) -> Result<Vec<String>, Value> {
    let Some(value) = value else {
        return Ok(fallback.iter().map(|value| value.to_string()).collect());
    };
    let values = value.as_array().ok_or_else(|| error(code, code))?;
    values
        .iter()
        .map(|value| {
            value
                .as_str()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(ToString::to_string)
                .ok_or_else(|| error(code, code))
        })
        .collect()
}

fn stable_id(prefix: &str, value: &str) -> String {
    format!("{prefix}{}", &sha256_hex(value.as_bytes())[..40])
}

fn fingerprint(value: &Value) -> String {
    sha256_hex(canonical_json(value).as_bytes())
}

fn nullable_hash(value: Option<&str>) -> Value {
    value
        .map(|value| json!(sha256_hex(value.as_bytes())))
        .unwrap_or(Value::Null)
}

fn canonical_json(value: &Value) -> String {
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

fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn now_iso_millis() -> String {
    iso_millis(OffsetDateTime::now_utc())
}

fn iso_millis(value: OffsetDateTime) -> String {
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

fn add_millis_iso(value: &str, milliseconds: i128) -> Result<String, Value> {
    let parsed = OffsetDateTime::parse(value, &Rfc3339)
        .map_err(|e| error("mailbox_timestamp_invalid", &e.to_string()))?;
    Ok(iso_millis(
        parsed + time::Duration::milliseconds(milliseconds as i64),
    ))
}

fn normalize_line_endings(value: &str) -> String {
    value.replace("\r\n", "\n").replace('\r', "\n")
}

fn normalized_path_text(path: &Path) -> String {
    let value = path.to_string_lossy();
    let value = value.strip_prefix(r"\\?\").unwrap_or(&value);
    #[cfg(windows)]
    {
        value.replace('/', r"\")
    }
    #[cfg(not(windows))]
    {
        value.to_string()
    }
}

fn graph_mailbox_path(user_id: &str) -> String {
    if user_id == "me" {
        "/me".to_string()
    } else {
        format!("/users/{}", encode_component(user_id))
    }
}

fn validate_graph_base_url(value: &str) -> Result<(), Value> {
    if value.starts_with("https://")
        || (std::env::var("NARADA_NATIVE_GRAPH_ALLOW_INSECURE_TEST")
            .ok()
            .as_deref()
            == Some("1")
            && value.starts_with("http://127.0.0.1:"))
    {
        return Ok(());
    }
    Err(error(
        "mailbox_graph_base_url_not_allowed",
        "Graph mailbox sync requires HTTPS or an explicit loopback test override",
    ))
}

fn validate_graph_page_url(value: &str, base_url: &str) -> Result<(), Value> {
    let prefix = graph_origin_prefix(base_url);
    if value.starts_with(&prefix) {
        Ok(())
    } else {
        Err(error(
            "mailbox_graph_page_url_not_allowed",
            "Graph continuation URL changed authority",
        ))
    }
}

fn graph_origin_prefix(value: &str) -> String {
    let scheme_end = value.find("://").map(|index| index + 3).unwrap_or(0);
    let path = value[scheme_end..]
        .find('/')
        .map(|index| scheme_end + index);
    path.map(|index| value[..index].to_string())
        .unwrap_or_else(|| value.to_string())
}

fn encode_component(value: &str) -> String {
    let mut encoded = String::new();
    for byte in value.as_bytes() {
        if byte.is_ascii_alphanumeric()
            || matches!(
                *byte,
                b'-' | b'_' | b'.' | b'!' | b'~' | b'*' | b'\'' | b'(' | b')'
            )
        {
            encoded.push(*byte as char);
        } else {
            encoded.push_str(&format!("%{byte:02X}"));
        }
    }
    encoded
}

fn non_empty_env(key: &str) -> Option<String> {
    std::env::var(key)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn read_ureq_body(response: ureq::Response) -> Result<String, String> {
    let mut bytes = Vec::new();
    response
        .into_reader()
        .take(MAX_RESPONSE_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() as u64 > MAX_RESPONSE_BYTES {
        return Err("mailbox_graph_response_too_large".to_string());
    }
    String::from_utf8(bytes).map_err(|e| e.to_string())
}

fn atomic_write_json(path: &Path, value: &Value) -> Result<(), Value> {
    let bytes = format!(
        "{}\n",
        serde_json::to_string(value)
            .map_err(|e| error("mailbox_json_encode_failed", &e.to_string()))?
    );
    atomic_write(path, bytes.as_bytes())
}

fn atomic_write_json_pretty(path: &Path, value: &Value) -> Result<(), Value> {
    let bytes = format!(
        "{}\n",
        serde_json::to_string_pretty(value)
            .map_err(|e| error("mailbox_json_encode_failed", &e.to_string()))?
    );
    atomic_write(path, bytes.as_bytes())
}

fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), Value> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|e| error("mailbox_file_write_failed", &e.to_string()))?;
    }
    let temporary = path.with_extension(format!("tmp.{}.{}", std::process::id(), Uuid::new_v4()));
    fs::write(&temporary, bytes).map_err(|e| error("mailbox_file_write_failed", &e.to_string()))?;
    if path.exists() {
        fs::remove_file(path).map_err(|e| error("mailbox_file_replace_failed", &e.to_string()))?;
    }
    fs::rename(&temporary, path).map_err(|e| error("mailbox_file_replace_failed", &e.to_string()))
}

fn decode_base64(value: &str) -> Result<Vec<u8>, Value> {
    let mut output = Vec::with_capacity(value.len() * 3 / 4);
    let mut quartet = [0_u8; 4];
    let mut count = 0;
    for byte in value.bytes().filter(|byte| !byte.is_ascii_whitespace()) {
        let decoded = match byte {
            b'A'..=b'Z' => byte - b'A',
            b'a'..=b'z' => byte - b'a' + 26,
            b'0'..=b'9' => byte - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            b'=' => 64,
            _ => {
                return Err(error(
                    "mailbox_attachment_base64_invalid",
                    "mailbox_attachment_base64_invalid",
                ))
            }
        };
        quartet[count] = decoded;
        count += 1;
        if count == 4 {
            output.push((quartet[0] << 2) | (quartet[1] >> 4));
            if quartet[2] != 64 {
                output.push((quartet[1] << 4) | (quartet[2] >> 2));
            }
            if quartet[3] != 64 {
                output.push((quartet[2] << 6) | quartet[3]);
            }
            count = 0;
        }
    }
    if count != 0 {
        return Err(error(
            "mailbox_attachment_base64_invalid",
            "mailbox_attachment_base64_invalid",
        ));
    }
    Ok(output)
}

fn bounded_error(value: &str) -> String {
    value
        .replace(['\r', '\n'], " ")
        .chars()
        .take(2048)
        .collect()
}

fn error(code: &str, message: &str) -> Value {
    json!({"schema":"narada.mailbox.error.v1","code":code,"message":message})
}
