
fn atomic_json(path: &Path, value: &Value) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(io_string)?;
    }
    let temporary = path.with_extension(format!("tmp-{}", std::process::id()));
    fs::write(
        &temporary,
        format!(
            "{}\n",
            serde_json::to_string_pretty(value).map_err(|error| error.to_string())?
        ),
    )
    .map_err(io_string)?;
    if path.exists() {
        fs::remove_file(path).ok();
    }
    fs::rename(&temporary, path).map_err(io_string)
}

fn write_startup_phase_trace(options: &Options, preflight_ms: f64) {
    let path = options.diagnostics_dir.join(format!(
        "startup-phases-{}.json",
        safe_segment(options.surface_id.as_deref().unwrap_or("surface"))
    ));
    let _ = atomic_json(
        &path,
        &json!({
            "schema": "narada.mcp_runtime_proxy.startup_phases.v1",
            "surface_id": options.surface_id,
            "observed_at": now_iso(),
            "preflight_ms": preflight_ms,
            "child_invocation_kind": options.child_invocation_kind,
            "child_applet": options.child_applet,
        }),
    );
}

fn record_startup_event(
    trace: &mut NativeStartupTrace,
    options: &Options,
    event: &str,
    detail: Value,
    completed: bool,
) {
    trace.events.push(json!({
        "at": now_iso(),
        "elapsed_ms": trace.started_clock.elapsed().as_secs_f64() * 1000.0,
        "event": event,
        "detail": detail,
    }));
    trace.completed = trace.completed || completed;
    let value = json!({
        "schema": "narada.mcp_runtime_proxy.startup_trace.v1",
        "surface_id": options.surface_id.clone(),
        "entrypoint": options.entrypoint.clone(),
        "started_at": trace.started_at.clone(),
        "updated_at": now_iso(),
        "completed": trace.completed,
        "runtime_contract_version": options.runtime_contract_version,
        "proxy_implementation": "native",
        "proxy_pid": std::process::id(),
        "events": trace.events.clone(),
    });
    let _ = atomic_json(&trace.path, &value);
}

fn read_json(path: &Path) -> Result<Value, String> {
    let bytes = fs::read(path).map_err(|error| format!("{}:{error}", path.display()))?;
    serde_json::from_slice(&bytes).map_err(|error| format!("{}:{error}", path.display()))
}

fn strip_volatile_manifest_metadata(value: &Value) -> Value {
    match value {
        Value::Array(values) => Value::Array(
            values
                .iter()
                .map(strip_volatile_manifest_metadata)
                .collect(),
        ),
        Value::Object(object) => Value::Object(
            object
                .iter()
                .filter_map(|(key, child)| {
                    if key == "generated_at" || key == "mtime_ms" {
                        None
                    } else {
                        Some((key.clone(), strip_volatile_manifest_metadata(child)))
                    }
                })
                .collect(),
        ),
        _ => value.clone(),
    }
}

fn sha256_file(path: &Path) -> Option<String> {
    fs::read(path).ok().map(|bytes| sha256_bytes(&bytes))
}
fn runtime_plan_fingerprint(plan: &Value) -> Option<String> {
    let mut unsigned = plan.as_object()?.clone();
    unsigned.remove("plan_fingerprint");
    serde_json::to_vec(&Value::Object(unsigned))
        .ok()
        .map(|bytes| sha256_bytes(&bytes))
}
fn sha256_bytes(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}
fn same_path(left: &str, right: &str) -> bool {
    normalize_text_path(left) == normalize_text_path(right)
}
fn normalized_path(path: &Path) -> String {
    normalize_text_path(&absolute(path.to_path_buf()).to_string_lossy())
}
fn normalize_text_path(path: &str) -> String {
    path.replace('/', "\\")
        .trim_end_matches('\\')
        .to_ascii_lowercase()
}
fn is_path_inside(root: &str, path: &str) -> bool {
    let root = normalize_text_path(root);
    path == root || path.starts_with(&(root + "\\"))
}
fn absolute(path: PathBuf) -> PathBuf {
    if path.is_absolute() {
        path
    } else {
        env::current_dir().unwrap_or_default().join(path)
    }
}

fn resolve_child_command(child_command: &str) -> Result<PathBuf, String> {
    let path = PathBuf::from(child_command);
    if path.is_absolute() {
        let base = path
            .file_name()
            .map(|value| value.to_string_lossy().to_ascii_lowercase())
            .unwrap_or_default();
        if matches!(base.as_str(), "node" | "node.exe" | "bun" | "bun.exe") {
            return Err(format!("native_proxy_interpreter_child_refused:{base}"));
        }
        return Ok(path);
    }
    if path.exists() {
        return Ok(absolute(path));
    }

    let base = path
        .file_name()
        .map(|value| value.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    if matches!(base.as_str(), "node" | "node.exe" | "bun" | "bun.exe") {
        return Err(format!("native_proxy_interpreter_child_refused:{base}"));
    }

    if let Some(found) = executable_on_path(&base) {
        return Ok(found);
    }

    // Preserve exact non-interpreter external commands; Command::new owns the
    // final unavailable diagnostic.
    Ok(path)
}

/// The strict native registrar speaks the no-handshake 2026 protocol.  Naked
/// Codex still emits the transport-era initialize pair before its ordinary
/// requests.  Keep that compatibility at the carrier edge only: the registrar
/// remains strict, and a modern request still receives the protocol's explicit
/// initialize_removed response.
fn registrar_carrier_compatibility_response(
    surface_id: Option<&str>,
    carrier_kind: Option<&str>,
    request: &Value,
) -> Option<Value> {
    if surface_id != Some("mcp-registrar")
        || protocol::is_modern_request(request)
        || request.get("method").and_then(Value::as_str) != Some("initialize")
    {
        return None;
    }
    if carrier_kind.is_some_and(|kind| kind.eq_ignore_ascii_case("kimi")) {
        return Some(json!({
            "jsonrpc": "2.0",
            "id": request.get("id").cloned().unwrap_or(Value::Null),
            "result": {
                "protocolVersion": protocol::LEGACY_PROTOCOL_VERSION,
                "capabilities": { "tools": {} },
                "serverInfo": { "name": "mcp-registrar", "version": "0.1.0" }
            }
        }));
    }
    Some(json!({
        "jsonrpc": "2.0",
        "id": request.get("id").cloned().unwrap_or(Value::Null),
        "result": {
            "resultType": "complete",
            "protocolVersion": protocol::MODERN_PROTOCOL_VERSION,
            "capabilities": { "tools": {} },
            "serverInfo": { "name": "mcp-registrar", "version": "0.1.0" },
            "_meta": { "io.modelcontextprotocol/serverInfo": { "name": "mcp-registrar", "version": "0.1.0" } }
        }
    }))
}
