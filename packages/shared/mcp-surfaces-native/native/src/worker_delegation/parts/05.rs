fn minimal_run(run: &Value) -> Value {
    let o = run.as_object().cloned().unwrap_or_default();
    let id = o.get("run_id").and_then(Value::as_str).unwrap_or_default();
    json!({
        "run_id":o.get("run_id"),
        "task_label":o.get("task_label"),
        "status":o.get("status"),
        "completion_state":o.get("completion_state"),
        "phase":o.get("phase"),
        "heartbeat_at":o.get("heartbeat_at"),
        "started_at":o.get("timing").and_then(|v|v.get("started_at")),
        "finished_at":o.get("timing").and_then(|v|v.get("finished_at")),
        "duration_ms":o.get("timing").and_then(|v|v.get("duration_ms")),
        "updated_at":o.get("updated_at").or_else(||o.get("timing").and_then(|v|v.get("finished_at"))),
        "summary":compact_text(o.get("summary").or_else(||o.get("last_message"))),
        "result":compact_text(o.get("result").or_else(||o.get("summary")).or_else(||o.get("last_message"))),
        "result_ref":artifact_ref(id, "last_message.json"),
        "execution_log":execution_log_refs(id, o.get("artifacts")),
        "refusals":refusals_value(&o),
        "error":compact_text(o.get("error"))
    })
}
fn compact_run(run: &Value) -> Value {
    let o = run.as_object().cloned().unwrap_or_default();
    let id = o.get("run_id").and_then(Value::as_str).unwrap_or_default();
    json!({"run_id":o.get("run_id"),"status":o.get("status"),"completion_state":o.get("completion_state"),"phase":o.get("phase"),"heartbeat_at":o.get("heartbeat_at"),"authority":o.get("authority"),"resolved_invocation":o.get("resolved_invocation"),"capability_snapshot":o.get("capability_snapshot"),"worker_session_id":o.get("worker_session_id"),"started_at":o.get("timing").and_then(|v|v.get("started_at")),"finished_at":o.get("timing").and_then(|v|v.get("finished_at")),"duration_ms":o.get("timing").and_then(|v|v.get("duration_ms")),"summary":o.get("summary").or_else(||o.get("last_message")),"summary_preview":compact_text(o.get("summary").or_else(||o.get("last_message"))),"result_ref":artifact_ref(id, "last_message.json"),"execution_log":execution_log_refs(id, o.get("artifacts")),"refusals":refusals_value(&o),"error":o.get("error"),"error_preview":compact_text(o.get("error")),"failure":o.get("failure"),"updated_at":o.get("updated_at").or_else(||o.get("timing").and_then(|v|v.get("finished_at")))})
}

fn artifact_ref(id: &str, name: &str) -> Value {
    if id.is_empty() { Value::Null } else { json!(format!("worker-artifact:{id}/{name}")) }
}
fn execution_log_refs(id: &str, artifacts: Option<&Value>) -> Value {
    json!({
        "events_ref":artifacts.and_then(|v|v.get("events")).cloned().unwrap_or_else(||artifact_ref(id,"events.jsonl")),
        "diagnostic_ref":artifacts.and_then(|v|v.get("diagnostic")).cloned().unwrap_or_else(||artifact_ref(id,"diagnostic.log"))
    })
}
fn refusals_value(object: &Map<String, Value>) -> Value {
    object.get("refusals")
        .or_else(|| object.get("failure").and_then(|v|v.get("refusals")))
        .cloned()
        .unwrap_or_else(|| json!([]))
}

fn resolved_invocation(
    cognition: &str,
    plan_ref: &str,
    provider_mode: &str,
    provider_model: &str,
    preflight_evidence_ref: &str,
    reasoning_effort: Option<&str>,
    provider_binding: Option<&Value>,
    provider_binding_path: Option<&Path>,
) -> Value {
    json!({
        "cognition": cognition,
        "invocation_plan_ref": plan_ref,
        "provider_mode": provider_mode,
        "provider_model": provider_model,
        "reasoning_effort": reasoning_effort,
        "provider_binding": provider_binding,
        "provider_binding_path": provider_binding_path.map(|path| path.to_string_lossy().to_string()),
        "preflight_evidence_ref": preflight_evidence_ref,
        "resolution_source":"worker_intelligence_preflight"
    })
}

fn now() -> String {
    OffsetDateTime::now_utc()
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_else(|_| "1970-01-01T00:00:00Z".into())
}
fn required_string<'a>(
    args: &'a Map<String, Value>,
    key: &str,
    code: &str,
) -> Result<&'a str, Value> {
    args.get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .ok_or_else(|| error(code, code))
}
fn write_json_atomic(path: &Path, value: &Value) -> Result<(), Value> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|_| error("worker_write_failed", "worker_write_failed"))?;
    }
    let temp = path.with_extension(format!("tmp-{}", uuid::Uuid::new_v4()));
    fs::write(
        &temp,
        serde_json::to_vec_pretty(value)
            .map_err(|_| error("worker_json_failed", "worker_json_failed"))?,
    )
    .map_err(|_| error("worker_write_failed", "worker_write_failed"))?;
    fs::rename(&temp, path).map_err(|_| error("worker_write_failed", "worker_write_failed"))
}

fn provider_registry_candidates(root: &Path) -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    if let Some(path) = std::env::var_os("NARADA_PROVIDER_REGISTRY_PATH") {
        candidates.push(PathBuf::from(path));
    }
    candidates.push(root.join(".narada/provider-registry.json"));
    let source_root = narada_source_root(root);
    candidates.push(source_root.join(
        "narada/packages/invokable-intelligence-management/assets/provider-registry.bootstrap.json",
    ));
    candidates
}

fn provider_models_from_registry(value: &Value) -> BTreeMap<String, Vec<String>> {
    let mut result = BTreeMap::new();
    let Some(providers) = value.get("providers").and_then(Value::as_object) else {
        return result;
    };
    for (provider, record) in providers {
        let mut models = record
            .get("available_models")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .map(str::to_string)
            .collect::<Vec<_>>();
        if models.is_empty() {
            if let Some(model_map) = record.get("models").and_then(Value::as_object) {
                models.extend(model_map.keys().cloned());
            }
        }
        models.sort();
        models.dedup();
        if !models.is_empty() {
            result.insert(provider.clone(), models);
        }
    }
    result
}

fn canonical_provider_models(root: &Path) -> Result<BTreeMap<String, Vec<String>>, Value> {
    for path in provider_registry_candidates(root) {
        if !path.is_file() {
            continue;
        }
        let value = read_json(&path).map_err(|_| {
            error(
                "worker_provider_registry_invalid",
                "worker_provider_registry_invalid",
            )
        })?;
        let models = provider_models_from_registry(&value);
        if models.is_empty() {
            return Err(error(
                "worker_provider_registry_invalid",
                "worker_provider_registry_invalid",
            ));
        }
        return Ok(models);
    }
    Err(error(
        "worker_provider_registry_unavailable",
        "worker_provider_registry_unavailable",
    ))
}

fn cognition_defaults_update(args: &Map<String, Value>, root: &Path) -> Result<Value, Value> {
    let provider = required_string(args, "provider", "worker_cognition_provider_required")?;
    let cognition = required_string(args, "cognition", "worker_cognition_required")?;
    if !matches!(cognition, "low" | "medium" | "high") {
        return Err(error(
            "worker_cognition_invalid",
            "worker_cognition_invalid",
        ));
    }
    let model = required_string(args, "model", "worker_model_required")?;
    let effort = required_string(args, "reasoning_effort", "worker_reasoning_effort_required")?;
    let path = defaults_path(root);
    let mut record = read_json(&path).unwrap_or_else(|_| json!({"schema":"narada.worker.cognition_defaults.v1","version":0,"provider_cognition_defaults":{},"effective_cognition_defaults":empty_defaults()}));
    let provider_models = canonical_provider_models(root)?;
    let allowed_models = provider_models.get(provider).ok_or_else(|| {
        error(
            "worker_cognition_provider_not_allowed",
            "worker_cognition_provider_not_allowed",
        )
    })?;
    if !allowed_models.iter().any(|candidate| candidate == model) {
        return Err(error(
            "worker_cognition_model_not_allowed",
            "worker_cognition_model_not_allowed",
        ));
    }
    record["version"] = json!(record.get("version").and_then(Value::as_u64).unwrap_or(0) + 1);
    record["updated_at"] = json!(now());
    record["updated_by"] = args.get("actor").cloned().unwrap_or(Value::Null);
    record["provider_cognition_defaults"][provider][cognition] =
        json!({"model":model,"reasoning_effort":effort});
    record["effective_cognition_defaults"][cognition] =
        json!({"provider":provider,"model":model,"reasoning_effort":effort});
    write_json_atomic(&path, &record)?;
    Ok(
        json!({"schema":"narada.worker.cognition_defaults.v1","status":"updated","cognition":cognition,"default":record["effective_cognition_defaults"][cognition],"defaults":record["effective_cognition_defaults"],"source":"native_rust_authority"}),
    )
}

fn narada_source_root(root: &Path) -> PathBuf {
    if let Some(path) = std::env::var_os("NARADA_SRC_ROOT").map(PathBuf::from) {
        return path;
    }
    let parent = root.parent().unwrap_or(root);
    let conventional = parent.join("src");
    if conventional.join("narada").is_dir() {
        return conventional;
    }
    if parent.join("narada").is_dir() {
        return parent.to_path_buf();
    }
    conventional
}
fn runtime_command(root: &Path) -> Result<PathBuf, Value> {
    if let Some(path) = std::env::var_os("NARADA_AGENT_RUNTIME_SERVER_NATIVE")
        .map(PathBuf::from)
        .filter(|p| p.is_file())
    {
        return Ok(path);
    }
    let src = narada_source_root(root);
    let candidates=[src.join("narada/packages/agent-runtime-server/native/target/release/narada-agent-runtime-server-rust.exe"),src.join("narada/packages/agent-runtime-server/native/target/release/narada-agent-runtime-server-rust")];
    candidates.into_iter().find(|p| p.is_file()).ok_or_else(|| {
        error(
            "worker_runtime_unavailable",
            "worker_runtime_unavailable:narada-agent-runtime-server-rust",
        )
    })
}
fn preflight_command(root: &Path) -> Result<PathBuf, Value> {
    if let Some(path) = std::env::var_os("NARADA_INTELLIGENCE_PREFLIGHT_NATIVE")
        .map(PathBuf::from)
        .filter(|path| path.is_file())
    {
        return Ok(path);
    }
    let src = narada_source_root(root);
    [
        src.join("narada/packages/invokable-intelligence-runtime/native/target/release/narada-intelligence-preflight.exe"),
        src.join("narada/packages/invokable-intelligence-runtime/native/target/release/narada-intelligence-preflight"),
    ]
    .into_iter()
    .find(|path| path.is_file())
    .ok_or_else(|| error("worker_intelligence_preflight_unavailable", "worker_intelligence_preflight_unavailable"))
}
