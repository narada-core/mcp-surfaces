
fn file_snapshot(path: &Path) -> FileSnapshot {
    match fs::metadata(path) {
        Ok(metadata) => FileSnapshot {
            path: absolute(path.to_path_buf()),
            exists: true,
            modified_ms: metadata
                .modified()
                .ok()
                .and_then(|value| value.duration_since(UNIX_EPOCH).ok())
                .map(|value| value.as_millis()),
            size: Some(metadata.len()),
            sha256: fs::read(path).ok().map(|bytes| sha256_bytes(&bytes)),
        },
        Err(_) => FileSnapshot {
            path: absolute(path.to_path_buf()),
            exists: false,
            modified_ms: None,
            size: None,
            sha256: None,
        },
    }
}

fn snapshot_json(snapshot: &FileSnapshot) -> Value {
    json!({
        "path": snapshot.path,
        "exists": snapshot.exists,
        "mtime_ms": snapshot.modified_ms,
        "size": snapshot.size,
        "sha256": snapshot.sha256,
    })
}

fn evaluate_freshness(
    options: &Options,
    proxy_pid: u32,
    child_pid: u32,
    manifest_fingerprint: Option<&str>,
    tracker: &FreshnessTracker,
) -> Value {
    let current_proxy = file_snapshot(&tracker.proxy_runtime.path);
    let current_child = file_snapshot(&tracker.child_runtime.path);
    let mut reasons = Vec::<Value>::new();
    let mut evidence_unknown = false;
    for (name, started, current) in [
        ("proxy_runtime", &tracker.proxy_runtime, &current_proxy),
        ("child_runtime", &tracker.child_runtime, &current_child),
    ] {
        if !current.exists {
            evidence_unknown = true;
            reasons.push(json!({ "code": "runtime_file_missing", "evidence": "unknown", "name": name, "path": current.path }));
        } else if started.sha256 != current.sha256 {
            reasons.push(json!({
                "code": "runtime_changed_since_process_start",
                "name": name,
                "path": current.path,
                "started_sha256": started.sha256,
                "current_sha256": current.sha256,
                "started_size": started.size,
                "current_size": current.size,
            }));
        }
    }
    let stale = reasons
        .iter()
        .any(|reason| reason.get("evidence").and_then(Value::as_str) != Some("unknown"));
    let status = if stale {
        "stale"
    } else if evidence_unknown {
        "unknown"
    } else {
        "current"
    };
    json!({
        "schema": "narada.mcp_runtime_proxy.runtime_freshness.v2",
        "status": status,
        "observed_at": now_iso(),
        "process_started_at": tracker.started_at,
        "proxy_pid": proxy_pid,
        "child_pid": child_pid,
        "surface_id": options.surface_id,
        "proxy_implementation": "native",
        "artifact_manifest_fingerprint": manifest_fingerprint,
        "runtime_files": [
            { "name": "proxy_runtime", "started": snapshot_json(&tracker.proxy_runtime), "current": snapshot_json(&current_proxy) },
            { "name": "child_runtime", "started": snapshot_json(&tracker.child_runtime), "current": snapshot_json(&current_child) },
        ],
        "source_files": [],
        "reasons": reasons,
        "reload_action": {
            "schema": "narada.mcp_runtime_proxy.supervisor_restart_action.v1",
            "kind": "restart_carrier_bound_surface",
            "operation": "restart",
            "owner": "carrier_or_runtime_supervisor",
            "target": { "scope": "carrier_bound_surface", "surface_id": options.surface_id, "proxy_pid": proxy_pid, "child_pid": child_pid },
            "automatic": false,
            "guidance": "Restart this carrier-bound proxy/server pair through the carrier or runtime supervisor. Restarting an mcp-loader child does not replace this process.",
        },
    })
}

fn send_cancel(stdin: &Arc<Mutex<Option<ChildStdin>>>, id: &str) -> Result<(), String> {
    let value = json!({ "jsonrpc": "2.0", "method": "notifications/cancelled", "params": { "requestId": id_value(id), "reason": "request timed out in mcp runtime proxy" } });
    if stdin.lock().map_err(lock_error)?.is_none() {
        return Ok(());
    }
    write_child(stdin, &value)
}

fn proxy_error(
    id: &str,
    request: &Pending,
    options: &Options,
    code: &str,
    message: String,
    exit_code: Option<i32>,
    stderr_tail: &[u8],
) -> Value {
    json!({ "jsonrpc": "2.0", "id": id_value(id), "error": {
        "code": -32000,
        "message": message,
        "data": {
            "schema": "narada.mcp_runtime_proxy.error.v1",
            "code": code,
            "method": request.method,
            "surface_id": options.surface_id,
            "entrypoint": options.entrypoint,
            "exit_code": exit_code,
            "signal": Value::Null,
            "stderr_tail": String::from_utf8_lossy(stderr_tail),
            "stdout_tail": "",
            "proxy_request_timeout_ms": options.request_timeout_ms,
            "effective_request_timeout_ms": request.effective_timeout_ms,
            "requested_transport_timeout_ms": request.requested_transport_timeout_ms,
            "tool_timeout_grace_ms": options.tool_timeout_grace_ms
        }
    }})
}

#[derive(Clone)]
struct Refusal {
    code: String,
    reason: String,
    details: Value,
}

fn refusal(code: &str, reason: &str, details: Value) -> Refusal {
    Refusal {
        code: code.to_string(),
        reason: reason.to_string(),
        details,
    }
}

fn preflight_workspace(options: &Options) -> Result<Option<String>, Refusal> {
    let Some(path) = &options.artifact_manifest else {
        return Err(refusal(
            "workspace_manifest_missing",
            "The launch did not provide an existing workspace artifact manifest.",
            json!({}),
        ));
    };
    let parsed = read_json(path).map_err(|error| {
        refusal(
            "workspace_manifest_stale",
            "The workspace artifact manifest is unreadable.",
            json!({ "error": error }),
        )
    })?;
    if parsed.get("schema").and_then(Value::as_str) != Some("narada.workspace_artifact_manifest.v1")
    {
        return Err(refusal(
            "workspace_manifest_stale",
            "The workspace artifact manifest has an unsupported schema or missing fingerprint.",
            json!({}),
        ));
    }
    let expected = parsed
        .get("manifest_fingerprint")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            refusal(
                "workspace_manifest_stale",
                "The workspace artifact manifest has an unsupported schema or missing fingerprint.",
                json!({}),
            )
        })?;
    let mut unsigned = parsed.clone();
    unsigned
        .as_object_mut()
        .map(|object| object.shift_remove("manifest_fingerprint"));
    let actual = sha256_bytes(
        &serde_json::to_vec(&strip_volatile_manifest_metadata(&unsigned)).unwrap_or_default(),
    );
    if actual != expected {
        return Err(refusal(
            "workspace_manifest_stale",
            "The workspace artifact manifest fingerprint does not match its contents.",
            json!({ "expected_fingerprint": expected, "actual_fingerprint": actual }),
        ));
    }
    let entrypoint = normalized_path(&options.entrypoint);
    let package = parsed
        .get("packages")
        .and_then(Value::as_array)
        .and_then(|packages| {
            packages.iter().find(|package| {
                package
                    .get("root")
                    .and_then(Value::as_str)
                    .map(|root| is_path_inside(root, &entrypoint))
                    .unwrap_or(false)
            })
        });
    if let Some(package) = package {
        verify_fingerprint(
            package.get("package_json"),
            "workspace_manifest_stale",
            "The package manifest changed after artifact generation.",
        )?;
        for (field, code, reason) in [
            (
                "build_configs",
                "workspace_manifest_stale",
                "The package build configuration changed after artifact generation.",
            ),
            (
                "source_files",
                "workspace_manifest_stale",
                "A source file changed after artifact generation.",
            ),
        ] {
            for fingerprint in package
                .get(field)
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                verify_fingerprint(Some(fingerprint), code, reason)?;
            }
        }
        let targets = package
            .get("export_targets")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        for target in &targets {
            let Some(fingerprint) = target.get("fingerprint") else {
                return Err(refusal(
                    "workspace_export_target_missing",
                    "A declared package export target is missing.",
                    json!({ "path": target.get("path") }),
                ));
            };
            verify_fingerprint(
                Some(fingerprint),
                "workspace_manifest_stale",
                "A declared package export target changed after artifact generation.",
            )?;
        }
        if !targets.iter().any(|target| {
            target
                .get("path")
                .and_then(Value::as_str)
                .map(|path| same_path(path, &entrypoint))
                .unwrap_or(false)
        }) {
            return Err(refusal(
                "workspace_artifact_missing",
                "The requested entrypoint is not a declared runtime artifact.",
                json!({ "path": entrypoint }),
            ));
        }
        for dependency in package
            .get("dependency_fingerprints")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            verify_fingerprint(
                dependency.get("package_json"),
                "workspace_dependency_unverified",
                "A local workspace dependency changed after artifact generation.",
            )?;
        }
    } else {
        let artifact = parsed
            .get("artifacts")
            .and_then(Value::as_array)
            .and_then(|items| {
                items.iter().find(|item| {
                    item.get("path")
                        .and_then(Value::as_str)
                        .map(|path| same_path(path, &entrypoint))
                        .unwrap_or(false)
                })
            })
            .ok_or_else(|| {
                refusal(
                    "workspace_artifact_missing",
                    "The entrypoint is not present in the workspace artifact manifest.",
                    json!({ "path": entrypoint }),
                )
            })?;
        verify_fingerprint(
            Some(artifact),
            "workspace_manifest_stale",
            "The manifest entrypoint changed after manifest generation.",
        )?;
    }
    Ok(Some(expected.to_string()))
}
