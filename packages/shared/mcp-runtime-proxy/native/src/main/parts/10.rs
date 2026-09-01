
fn classify_instance(mut value: Value) -> Value {
    let state = value
        .get("state")
        .and_then(Value::as_str)
        .unwrap_or("stale")
        .to_string();
    let mut reasons = Vec::<Value>::new();
    let observed = if state == "reclaimed" || state == "closed" {
        state
    } else {
        for (field, reason) in [
            ("proxy_pid", "proxy_pid_not_alive"),
            ("parent_pid", "parent_carrier_pid_not_alive"),
            ("child_pid", "child_pid_not_alive"),
        ] {
            if let Some(pid) = value.get(field).and_then(Value::as_u64) {
                if !process_alive(pid as u32) {
                    reasons.push(Value::String(reason.to_string()));
                }
            }
        }
        let lease_expired = value
            .get("lease_expires_at")
            .and_then(Value::as_str)
            .and_then(|value| OffsetDateTime::parse(value, &Rfc3339).ok())
            .is_some_and(|value| value < OffsetDateTime::now_utc());
        if lease_expired {
            reasons.push(Value::String("heartbeat_lease_expired".to_string()));
        }
        if reasons.is_empty() && state == "live" {
            "live".to_string()
        } else {
            "stale".to_string()
        }
    };
    if let Some(object) = value.as_object_mut() {
        object.insert("observed_state".to_string(), Value::String(observed));
        object.insert("stale_reasons".to_string(), Value::Array(reasons));
    }
    value
}

#[cfg(windows)]
fn process_alive(pid: u32) -> bool {
    use windows_sys::Win32::Foundation::{CloseHandle, WAIT_TIMEOUT};
    use windows_sys::Win32::System::Threading::{OpenProcess, WaitForSingleObject};
    const SYNCHRONIZE_ACCESS: u32 = 0x0010_0000;
    let handle = unsafe { OpenProcess(SYNCHRONIZE_ACCESS, 0, pid) };
    if handle.is_null() {
        return false;
    }
    let result = unsafe { WaitForSingleObject(handle, 0) } == WAIT_TIMEOUT;
    unsafe { CloseHandle(handle) };
    result
}

#[cfg(not(windows))]
fn process_alive(_pid: u32) -> bool {
    false
}

fn write_forensic(
    options: &Options,
    event: &str,
    id: &str,
    method: &str,
    child_pid: u32,
    stderr_tail: &[u8],
) -> Result<(), String> {
    let artifact = json!({
        "schema": "narada.mcp_runtime_proxy.forensic_artifact.v1",
        "event": event,
        "recorded_at": now_iso(),
        "proxy": { "pid": std::process::id(), "implementation": "native", "surface_id": options.surface_id },
        "child_process": { "pid": child_pid, "command": options.child_command, "child_prefix_args": options.child_prefix_args, "entrypoint": options.entrypoint },
        "request": { "id": id_value(id), "method": method },
        "stream_tails": { "stderr_tail": String::from_utf8_lossy(stderr_tail), "stdout_tail": "" }
    });
    atomic_json(
        &options.diagnostics_dir.join(format!(
            "{}-{}-{}.json",
            timestamp_ms(),
            safe_segment(options.surface_id.as_deref().unwrap_or("surface")),
            safe_segment(id)
        )),
        &artifact,
    )
}

fn generation_context(generation: &Value) -> Value {
    json!({
        "carrier_id": generation.get("carrier_id"),
        "carrier_kind": generation.get("carrier_kind"),
        "config_path": generation.get("config_path"),
        "materialization_contract_entrypoint": generation.get("materialization_contract_entrypoint"),
        "materialization_contract_fingerprint": generation.get("materialization_contract_fingerprint"),
        "registrar_entrypoint": generation.get("registrar_entrypoint"),
        "registrar_fingerprint": generation.get("registrar_fingerprint"),
        "proxy_implementation": generation.get("proxy_implementation"),
        "proxy_entrypoint": generation.get("proxy_entrypoint"),
        "proxy_fingerprint": generation.get("proxy_fingerprint"),
        "runtime_profile_kind": generation.get("runtime_profile_kind"),
        "runtime_materialization_plan_path": generation.get("runtime_materialization_plan_path"),
        "runtime_materialization_plan_fingerprint": generation.get("runtime_materialization_plan_fingerprint"),
        "runtime_implementation_matrix_path": generation.get("runtime_implementation_matrix_path"),
        "runtime_implementation_matrix_fingerprint": generation.get("runtime_implementation_matrix_fingerprint"),
        "materialization_generated_at": generation.get("generated_at")
    })
}

fn emit_runtime_start(options: &Options, proxy_pid: u32, child_pid: u32) {
    let site_id = env::var("NARADA_SITE_ID").unwrap_or_else(|_| "unknown-site".to_string());
    let authority_ref =
        env::var("NARADA_AUTHORITY_REF").unwrap_or_else(|_| format!("site:{site_id}:mcp-surfaces"));
    let carrier_session = env::var("NARADA_CARRIER_SESSION_ID")
        .ok()
        .filter(|value| !value.trim().is_empty());
    let observed_at = now_iso();
    let proxy_owner = format!("carrier-proxy-{proxy_pid}");
    let executable = env::current_exe()
        .ok()
        .map(|path| path.to_string_lossy().to_string());
    append_observation(
        options,
        &json!({
            "schema": "narada.mcp_runtime.resource_owner.v1", "owner_id": proxy_owner,
            "site_id": site_id, "authority_ref": authority_ref, "owner_kind": "carrier_proxy",
            "pid": proxy_pid, "process_started_at": Value::Null, "parent_owner_id": Value::Null,
            "surface_id": options.surface_id, "instance_id": Value::Null, "generation_id": Value::Null,
            "carrier_session_id": carrier_session, "executable_name": executable, "observed_at": observed_at
        }),
    );
    append_observation(
        options,
        &json!({
            "schema": "narada.mcp_runtime.resource_owner.v1", "owner_id": format!("proxy-child-{child_pid}"),
            "site_id": site_id, "authority_ref": authority_ref, "owner_kind": "nars_stdio_child",
            "pid": child_pid, "process_started_at": Value::Null, "parent_owner_id": proxy_owner,
            "surface_id": options.surface_id, "instance_id": Value::Null, "generation_id": Value::Null,
            "carrier_session_id": carrier_session, "executable_name": options.child_command, "observed_at": observed_at
        }),
    );
    append_observation(
        options,
        &json!({
            "schema": "narada.mcp_runtime.lifecycle_event.v1", "event_id": format!("event-native-{proxy_pid}-{}", timestamp_ms()),
            "occurred_at": observed_at, "site_id": site_id, "authority_ref": authority_ref,
            "owner_id": format!("proxy-child-{child_pid}"), "event_type": "process_started",
            "surface_id": options.surface_id, "instance_id": Value::Null, "generation_id": Value::Null,
            "request_id": Value::Null, "status": "ok", "inflight": Value::Null
        }),
    );
}

fn emit_runtime_exit(options: &Options, child_pid: u32, status: &str) {
    let site_id = env::var("NARADA_SITE_ID").unwrap_or_else(|_| "unknown-site".to_string());
    let authority_ref =
        env::var("NARADA_AUTHORITY_REF").unwrap_or_else(|_| format!("site:{site_id}:mcp-surfaces"));
    append_observation(
        options,
        &json!({
            "schema": "narada.mcp_runtime.lifecycle_event.v1", "event_id": format!("event-native-{}-{}", std::process::id(), timestamp_ms()),
            "occurred_at": now_iso(), "site_id": site_id, "authority_ref": authority_ref,
            "owner_id": format!("proxy-child-{child_pid}"), "event_type": "process_exited",
            "surface_id": options.surface_id, "instance_id": Value::Null, "generation_id": Value::Null,
            "request_id": Value::Null, "status": status, "inflight": Value::Null
        }),
    );
}

fn append_observation(options: &Options, record: &Value) {
    let Some(site_root) = env::var_os("NARADA_SITE_ROOT") else {
        return;
    };
    let source_id = safe_segment(&format!(
        "carrier-proxy-{}",
        options.surface_id.as_deref().unwrap_or("surface")
    ))
    .to_ascii_lowercase();
    let root = PathBuf::from(site_root)
        .join(".narada")
        .join("runtime")
        .join("mcp-runtime-observer")
        .join("sources");
    let path = root.join(format!("{source_id}.current.jsonl"));
    let line = match serde_json::to_string(record) {
        Ok(value) => format!("{value}\n"),
        Err(_) => return,
    };
    if fs::create_dir_all(&root).is_err() {
        return;
    }
    if fs::metadata(&path)
        .map(|value| value.len().saturating_add(line.len() as u64) > 8 * 1024 * 1024)
        .unwrap_or(false)
    {
        let rotated = root.join(format!(
            "{source_id}.{}.{}.jsonl",
            timestamp_ms(),
            std::process::id()
        ));
        let _ = fs::rename(&path, rotated);
    }
    if let Ok(mut file) = fs::OpenOptions::new().create(true).append(true).open(path) {
        let _ = file.write_all(line.as_bytes());
    }
}
