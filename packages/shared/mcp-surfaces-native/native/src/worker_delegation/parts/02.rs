fn preflight_paths(
    constraints: Option<&Map<String, Value>>,
    cwd: &Path,
    allowed_roots: &[PathBuf],
) -> Result<Value, Value> {
    let Some(items) = constraints
        .and_then(|value| value.get("preflight_paths"))
        .and_then(Value::as_array)
    else {
        return Ok(json!({"status":"not_requested","items":[]}));
    };
    let mut checked = Vec::with_capacity(items.len());
    let mut native_read_files = 0usize;
    for item in items {
        let Some(object) = item.as_object() else {
            return Err(error("worker_preflight_path_invalid", "worker_preflight_path_invalid"));
        };
        let raw_path = object
            .get("path")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| error("worker_preflight_path_required", "worker_preflight_path_required"))?;
        let access = object
            .get("access")
            .and_then(Value::as_str)
            .unwrap_or("read");
        if !matches!(access, "read" | "write" | "create") {
            return Err(error("worker_preflight_access_invalid", "worker_preflight_access_invalid"));
        }
        let path = {
            let candidate = PathBuf::from(raw_path);
            if candidate.is_absolute() { candidate } else { cwd.join(candidate) }
        };
        let scope_path = if path.exists() {
            path.as_path()
        } else {
            path.parent().unwrap_or(path.as_path())
        };
        if !allowed_roots.iter().any(|allowed| is_within(scope_path, allowed)) {
            return Err(json!({
                "schema":"narada.worker.error.v1",
                "code":"worker_preflight_path_outside_allowed_roots",
                "message":"worker_preflight_path_outside_allowed_roots",
                "path":path.to_string_lossy(),
                "access":access,
                "preflight_status":"failed",
                "remediation":"Use a path under the admitted worker root."
            }));
        }
        let exists = path.exists();
        if matches!(access, "read" | "write") && !exists {
            return Err(json!({
                "schema":"narada.worker.error.v1",
                "code":"worker_preflight_path_missing",
                "message":"worker_preflight_path_missing",
                "path":path.to_string_lossy(),
                "access":access,
                "preflight_status":"failed",
                "remediation":"Correct constraints.preflight_paths or remove the stale path before retrying."
            }));
        }
        if access == "create" && !exists && path.parent().is_some_and(|parent| !parent.exists()) {
            return Err(json!({
                "schema":"narada.worker.error.v1",
                "code":"worker_preflight_parent_missing",
                "message":"worker_preflight_parent_missing",
                "path":path.to_string_lossy(),
                "access":access,
                "preflight_status":"failed",
                "remediation":"Create or select an existing parent directory before retrying."
            }));
        }
        let native_read = if access == "read" && exists {
            if native_read_files >= MAX_NATIVE_READ_FILES {
                json!({"status":"not_read","reason":"native_read_budget_exhausted","max_files":MAX_NATIVE_READ_FILES})
            } else {
                native_read_files += 1;
                match fs::read(&path) {
                    Ok(bytes) => {
                        let total_bytes = bytes.len();
                        let returned_bytes = total_bytes.min(MAX_NATIVE_READ_BYTES);
                        let content = String::from_utf8_lossy(&bytes[..returned_bytes]).to_string();
                        json!({
                            "status":"passed",
                            "encoding":"utf8_lossy",
                            "content":content,
                            "bytes":total_bytes,
                            "returned_bytes":returned_bytes,
                            "truncated":total_bytes > returned_bytes
                        })
                    }
                    Err(error) => json!({
                        "status":"failed",
                        "error_kind":format!("{:?}", error.kind()),
                        "message":"native file read failed"
                    }),
                }
            }
        } else {
            Value::Null
        };
        checked.push(json!({
            "path":path.to_string_lossy(),
            "access":access,
            "exists":exists,
            "status":"passed",
            "native_read":native_read
        }));
    }
    Ok(json!({"status":"passed","items":checked}))
}

fn native_read_evidence_prompt(preflight: &Value) -> String {
    let items = preflight
        .get("items")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut sections = Vec::new();
    for item in items {
        let path = item.get("path").and_then(Value::as_str).unwrap_or("unknown");
        let Some(read) = item.get("native_read").and_then(Value::as_object) else {
            continue;
        };
        match read.get("status").and_then(Value::as_str) {
            Some("passed") => sections.push(format!(
                "PATH: {path}\nCONTENT (native, bounded; authoritative; do not call shell or command tools for this path):\n{}",
                read.get("content").and_then(Value::as_str).unwrap_or_default()
            )),
            Some(status) => sections.push(format!(
                "PATH: {path}\nNATIVE READ STATUS: {status}. Do not retry this read through a shell."
            )),
            _ => {}
        }
    }
    if sections.is_empty() {
        "NATIVE FILE READ EVIDENCE: none supplied. Only issue a bounded shell read when no native evidence was requested.".to_string()
    } else {
        format!(
            "NATIVE FILE READ EVIDENCE (authoritative worker-boundary evidence):\n{}",
            sections.join("\n\n")
        )
    }
}
fn worker_prompt(instruction_text: &str, preflight: &Value) -> String {
    // Evidence is context, not the terminal instruction. Keep the caller-owned
    // output contract last so it remains the worker's final obligation after
    // what may be a comparatively large native evidence packet.
    format!(
        "{READ_ONLY_COMMAND_CONTRACT}\n\n{}\n\nWORKER INTENT AND TERMINAL OUTPUT CONTRACT:\n{instruction_text}",
        native_read_evidence_prompt(preflight)
    )
}
#[cfg(windows)]
fn path_components_equal_or_child(path: &Path, root: &Path) -> bool {
    let path = path
        .components()
        .map(|component| component.as_os_str().to_string_lossy().to_lowercase())
        .collect::<Vec<_>>();
    let root = root
        .components()
        .map(|component| component.as_os_str().to_string_lossy().to_lowercase())
        .collect::<Vec<_>>();
    path.len() >= root.len()
        && path
            .iter()
            .zip(root.iter())
            .all(|(left, right)| left == right)
}
#[cfg(not(windows))]
fn path_components_equal_or_child(path: &Path, root: &Path) -> bool {
    path == root || path.starts_with(root)
}
fn safe_run_id(value: &str) -> Result<&str, Value> {
    if value.len() < 5
        || value.len() > 160
        || !value.starts_with("run-")
        || !value[4..]
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        Err(error("worker_run_id_invalid", "worker_run_id_invalid"))
    } else {
        Ok(value)
    }
}
fn run_id(args: &Map<String, Value>) -> Result<String, Value> {
    let id = args
        .get("run_id")
        .and_then(Value::as_str)
        .filter(|v| !v.trim().is_empty())
        .ok_or_else(|| error("worker_run_id_required", "worker_run_id_required"))?;
    safe_run_id(id.trim())?;
    Ok(id.trim().to_string())
}
fn read_json(path: &Path) -> Result<Value, Value> {
    let meta =
        fs::metadata(path).map_err(|_| error("worker_run_not_found", "worker_run_not_found"))?;
    if meta.len() > MAX_FILE_BYTES as u64 {
        return Err(error("worker_record_too_large", "worker_record_too_large"));
    }
    let text = fs::read_to_string(path)
        .map_err(|_| error("worker_record_read_failed", "worker_record_read_failed"))?;
    serde_json::from_str(&text)
        .map_err(|_| error("worker_record_invalid_json", "worker_record_invalid_json"))
}
fn run_path(root: &Path, id: &str) -> Result<PathBuf, Value> {
    safe_run_id(id)?;
    Ok(run_root(root).join(id).join("result.json"))
}
fn read_run(root: &Path, id: &str) -> Result<Value, Value> {
    read_json(&run_path(root, id)?)
}
fn read_reconciled_run(root: &Path, id: &str) -> Result<Value, Value> {
    let path = run_path(root, id)?;
    let mut run = read_json(&path)?;
    if run.get("status").and_then(Value::as_str) == Some("running") {
        let expected = run
            .pointer("/resolved_invocation/provider_broker_generation")
            .and_then(Value::as_str);
        let expected_owned = expected.map(str::to_string);
        let current = crate::codex_app_server_broker::current_generation();
        if let (Some(expected), Some(current)) = (expected_owned.as_deref(), current.as_deref()) {
            if expected != current {
                let at = now();
                run["status"] = json!("orphaned");
                run["completion_state"] = json!("partial");
                run["phase"] = json!("orphaned");
                run["heartbeat_at"] = json!(at.clone());
                run["error"] = json!("worker_orphaned:broker_generation_mismatch");
                run["orphaned"] = json!({"reason":"broker_generation_mismatch","expected":expected,"current":current,"at":at});
                run["timing"]["finished_at"] = json!(at);
                write_json_atomic(&path, &run)?;
            }
        }
    }
    Ok(run)
}

fn policy(root: &Path, allowed_roots: &[PathBuf]) -> Value {
    json!({"schema":"narada.worker.policy.v1","status":"ok","server_name":SERVER_NAME,"run_root":run_root(root).to_string_lossy(),"site_root":root.to_string_lossy(),"allowed_roots":allowed_roots.iter().map(|allowed|allowed.to_string_lossy()).collect::<Vec<_>>(),"allowed_runtimes":["narada-agent-runtime-server"],"allowed_authorities":["read","write","command"],"default_cognition":DEFAULT_COGNITION,"native_execution":"rust_authority","secret_projection":"secret_store_reference_only","provider_transports":{"codex-subscription":{"transport":"codex_app_server_broker","host_lifecycle":"owned_by_surface_process","thread_policy":"fresh_ephemeral_per_turn","fallback":"none","capacity":{"lanes":1,"queue_limit":64,"scheduling":"fifo"},"timing":{"queue_timeout_field":"constraints.queue_timeout_ms","execution_timeout_field":"constraints.max_run_ms","execution_clock_starts":"provider_admitted"}},"deepseek-api":{"transport":"native_http"},"openrouter-api":{"transport":"native_http"}},"windows_msvc_environment":{"inherited":true,"automatic_discovery":false,"remediation":"Initialize VsDevCmd or use Developer PowerShell before launching the carrier."}})
}
