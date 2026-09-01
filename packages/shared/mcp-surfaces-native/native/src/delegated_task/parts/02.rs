fn write_validated_request(root: &Path, record: &Value) -> Result<(), Value> {
    let reference = record
        .get("validated_request_ref")
        .and_then(Value::as_str)
        .ok_or_else(|| error("validated_request_ref_invalid", "validated_request_ref_invalid"))?;
    let path = validated_request_path(root, reference)?;
    let bytes = serde_json::to_vec_pretty(record)
        .map_err(|_| error("validated_request_write_failed", "validated_request_write_failed"))?;
    if bytes.len() > MAX_VALIDATED_REQUEST_BYTES {
        return Err(error(
            "validated_request_too_large",
            "validated_request_too_large",
        ));
    }
    fs::create_dir_all(path.parent().unwrap())
        .map_err(|_| error("validated_request_write_failed", "validated_request_write_failed"))?;
    fs::write(path, bytes)
        .map_err(|_| error("validated_request_write_failed", "validated_request_write_failed"))
}
fn read_validated_request(root: &Path, reference: &str) -> Result<Value, Value> {
    let path = validated_request_path(root, reference)?;
    let size = fs::metadata(&path)
        .map_err(|_| error("validated_request_not_found", "validated_request_not_found"))?
        .len();
    if size > MAX_VALIDATED_REQUEST_BYTES as u64 {
        return Err(error(
            "validated_request_too_large",
            "validated_request_too_large",
        ));
    }
    let text = fs::read_to_string(path)
        .map_err(|_| error("validated_request_not_found", "validated_request_not_found"))?;
    let record: Value = serde_json::from_str(&text)
        .map_err(|_| error("validated_request_invalid_json", "validated_request_invalid_json"))?;
    if record.get("schema").and_then(Value::as_str)
        != Some("narada.delegated_task.validated_request.v1")
        || record.get("validated_request_ref").and_then(Value::as_str) != Some(reference)
    {
        return Err(error(
            "validated_request_invalid",
            "validated_request_invalid",
        ));
    }
    Ok(record)
}
fn materialize_validated_request(
    args: &Map<String, Value>,
    root: &Path,
) -> Result<Map<String, Value>, Value> {
    let Some(reference) = args
        .get("validated_request_ref")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return Ok(args.clone());
    };
    let record = read_validated_request(root, reference)?;
    let mut merged = record
        .get("request")
        .and_then(Value::as_object)
        .cloned()
        .ok_or_else(|| error("validated_request_invalid", "validated_request_invalid"))?;
    for (key, value) in args {
        if key == "validated_request_ref" {
            continue;
        }
        if !matches!(key.as_str(), "task_id" | "idempotency_key" | "expected_owner_site_id" | "allow_cross_site") {
            return Err(json!({"schema":"narada.delegated_task.error.v1","code":"validated_request_drift","message":"validated_request_drift","validated_request_ref":reference,"field":key,"remediation":"Pass only validated_request_ref and optional task identity/scope fields to delegated_task_run."}));
        }
        if let Some(existing) = merged.get(key) {
            if existing != value {
                return Err(json!({"schema":"narada.delegated_task.error.v1","code":"validated_request_drift","message":"validated_request_drift","validated_request_ref":reference,"field":key}));
            }
        } else {
            merged.insert(key.clone(), value.clone());
        }
    }
    merged.insert("validated_request_ref".into(), json!(reference));
    Ok(merged)
}
fn current_site_id(root: &Path) -> Option<String> {
    for key in ["NARADA_SITE_ID", "SITE_ID", "NARADA_SITE"] {
        if let Ok(value) = std::env::var(key) {
            if !value.trim().is_empty() {
                return Some(value);
            }
        }
    }
    for path in [root.join(".narada/site.json"), root.join(".ai/site.json")] {
        if let Ok(text) = fs::read_to_string(path) {
            if let Ok(value) = serde_json::from_str::<Value>(&text) {
                if let Some(id) = value
                    .get("site_id")
                    .or_else(|| value.get("id"))
                    .and_then(Value::as_str)
                    .filter(|id| !id.trim().is_empty())
                {
                    return Some(id.to_string());
                }
            }
        }
    }
    root.file_name()
        .and_then(|name| name.to_str())
        .filter(|name| *name != ".")
        .map(str::to_string)
}
fn ownership(task: &Value) -> Value {
    let owner = task.get("owner_site_id").and_then(Value::as_str);
    let has = owner.is_some() || task.get("visibility_scope").is_some();
    if !has {
        return json!({"owner_site_id":"unknown","owner_site_root":null,"created_by_site_id":"unknown","visibility_scope":"user_global_legacy","task_root_scope":"unknown","ownership_resolution":"legacy_missing_metadata"});
    }
    json!({"owner_site_id":owner.unwrap_or("unknown"),"owner_site_root":task.get("owner_site_root"),"created_by_site_id":task.get("created_by_site_id").and_then(Value::as_str).unwrap_or("unknown"),"visibility_scope":task.get("visibility_scope").and_then(Value::as_str).unwrap_or(if owner.is_some(){"site"}else{"user_global"}),"task_root_scope":task.get("task_root_scope").and_then(Value::as_str).unwrap_or("unknown"),"ownership_resolution":if owner.is_some(){"explicit"}else{"unknown_owner"}})
}
fn assert_mutation_scope(
    task: &Value,
    args: &Map<String, Value>,
    root: &Path,
) -> Result<Value, Value> {
    let projected = ownership(task);
    let owner = projected
        .get("owner_site_id")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    if let Some(expected) = args.get("expected_owner_site_id").and_then(Value::as_str) {
        if expected != owner {
            return Err(error(
                "delegated_task_owner_site_mismatch",
                "delegated_task_owner_site_mismatch",
            ));
        }
    }
    let current = current_site_id(root);
    let cross = current.as_deref().is_some_and(|site| site != owner);
    let legacy = owner == "unknown"
        || projected.get("visibility_scope").and_then(Value::as_str) == Some("user_global_legacy");
    if (cross || legacy) && args.get("allow_cross_site").and_then(Value::as_bool) != Some(true) {
        return Err(error(
            "delegated_task_cross_site_mutation_denied",
            "delegated_task_cross_site_mutation_denied",
        ));
    }
    Ok(projected)
}
fn safe_id(id: &str) -> Result<String, Value> {
    if id.is_empty()
        || id.len() > 120
        || !id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Err(error(
            "delegated_task_id_invalid",
            "delegated_task_id_invalid",
        ));
    }
    Ok(id.to_string())
}
fn task_path(root: &Path, id: &str) -> Result<PathBuf, Value> {
    let id = safe_id(id)?;
    Ok(tasks_dir(root).join(id).join("task.json"))
}
struct TaskLock {
    path: PathBuf,
    token: String,
    stop: Arc<AtomicBool>,
    heartbeat: Option<JoinHandle<()>>,
}
impl Drop for TaskLock {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(handle) = self.heartbeat.take() {
            handle.thread().unpark();
            let _ = handle.join();
        }
        if lock_owner_matches(&self.path, &self.token) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }
}
fn lock_owner_matches(path: &Path, token: &str) -> bool {
    fs::read_to_string(path.join("owner.json"))
        .ok()
        .and_then(|text| serde_json::from_str::<Value>(&text).ok())
        .and_then(|owner| {
            owner
                .get("token")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .as_deref()
        == Some(token)
}
fn lock_stale(path: &Path, stale_ms: u64) -> bool {
    let heartbeat = path.join("owner.json");
    let target = if heartbeat.exists() { &heartbeat } else { path };
    fs::metadata(target)
        .and_then(|metadata| metadata.modified())
        .ok()
        .and_then(|modified| modified.elapsed().ok())
        .is_some_and(|elapsed| elapsed > std::time::Duration::from_millis(stale_ms))
}
fn reclaim_stale_lock(path: &Path) -> bool {
    let claim = path.with_extension("lockdir.reclaim");
    let claim_file = match OpenOptions::new().write(true).create_new(true).open(&claim) {
        Ok(file) => file,
        Err(_) => return false,
    };
    let suffix = format!(
        "abandoned-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    );
    let abandoned = path.with_extension(suffix);
    let won = fs::rename(path, &abandoned).is_ok();
    if won {
        let _ = fs::remove_dir_all(abandoned);
    }
    drop(claim_file);
    let _ = fs::remove_file(claim);
    won
}
