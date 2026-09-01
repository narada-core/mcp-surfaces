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
    request_timeout_ms: u64,
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
        let code = failure
            .get("code")
            .and_then(Value::as_str)
            .unwrap_or("mailbox_sync_fatal_failure");
        let detail = failure
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or(code);
        let message = if detail == code {
            code.to_string()
        } else {
            format!("{code}: {detail}")
        };
        let failed = fail_generation(
            db,
            generation_id,
            lease_token,
            &bounded_error(&message),
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

