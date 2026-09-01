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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn immutable_fact_identity_does_not_depend_on_batch_ordinal() {
        let record = SourceRecord {
            record_id: "event-1".to_string(),
            ordinal: Some("1".to_string()),
            payload: json!({
                "event_kind":"upsert",
                "message_id":"message-1"
            }),
            provenance: json!({
                "sourceId":"graph",
                "sourceVersion":"v1",
                "observedAt":"2026-08-18T00:00:00.000Z"
            }),
        };
        let mut reordered = record.clone();
        reordered.ordinal = Some("246".to_string());

        let first = fact_for_record(&record, Some("cursor-a")).expect("first fact");
        let second = fact_for_record(&reordered, Some("cursor-b")).expect("second fact");

        assert_eq!(first.0, second.0);
        assert_eq!(first.3, second.3);
        assert!(!first.3.contains("ordinal"));
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

