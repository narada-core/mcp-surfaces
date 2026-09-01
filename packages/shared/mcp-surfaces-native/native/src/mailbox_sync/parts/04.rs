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

