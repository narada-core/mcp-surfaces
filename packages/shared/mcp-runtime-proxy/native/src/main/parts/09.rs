
fn preflight_refusal(options: &Options, mut refusal: Refusal) -> Result<(), String> {
    let mut details = refusal.details.as_object().cloned().unwrap_or_default();
    details.insert(
        "remediation".to_string(),
        Value::String(
            "Run cargo native-release from mcp-surfaces, then restart the carrier session."
                .to_string(),
        ),
    );
    details.insert("recovery".to_string(), recovery(options, &refusal));
    refusal.details = Value::Object(details);
    eprintln!(
        "mcp_runtime_proxy_preflight_refused:{}:{}",
        refusal.code, refusal.reason
    );
    let mut reader = BufReader::new(io::stdin());
    if let Ok(Some(request)) = read_wire(&mut reader) {
        let id = request.value.get("id").cloned().unwrap_or(Value::Null);
        let response = json!({ "jsonrpc": "2.0", "id": id, "error": {
            "code": -32000,
            "message": format!("mcp_runtime_proxy_preflight_refused:{}", refusal.code),
            "data": {
                "schema": "narada.mcp_runtime_proxy.error.v1",
                "code": refusal.code,
                "method": request.value.get("method").cloned().unwrap_or(Value::Null),
                "surface_id": options.surface_id,
                "entrypoint": options.entrypoint,
                "artifact_manifest_path": options.artifact_manifest,
                "reason": refusal.reason,
                "details": refusal.details
            }
        }});
        write_wire(&mut io::stdout().lock(), &response, request.framed)?;
    }
    Err(format!(
        "mcp_runtime_proxy_preflight_refused:{}",
        refusal.code
    ))
}

fn recovery(options: &Options, refusal: &Refusal) -> Value {
    let command = match (
        &options.registrar_command,
        &options.registrar_entrypoint,
        &options.materialization_sidecar,
    ) {
        (Some(executable), Some(entrypoint), Some(sidecar))
            if same_path(executable, &entrypoint.to_string_lossy()) =>
        {
            let args = vec![
                "recover-generation".to_string(),
                "--generation".to_string(),
                sidecar.to_string_lossy().to_string(),
            ];
            json!({
                "executable": executable,
                "args": args,
                "display": format!("\"{}\" recover-generation --generation \"{}\"", executable, sidecar.display())
            })
        }
        _ => Value::Null,
    };
    let materialization = refusal.code.starts_with("materialization_")
        || refusal.code.starts_with("runtime_contract_");
    let prefix = if materialization {
        "materialization"
    } else {
        "workspace-materialization"
    };
    let group_id = format!(
        "{prefix}-{}",
        &sha256_bytes(
            format!(
                "{}:{:?}:{:?}",
                refusal.code, options.artifact_manifest, options.materialization_sidecar
            )
            .as_bytes()
        )[..20]
    );
    if materialization {
        let config_path = options.materialization_sidecar.as_ref().map(|path| {
            let text = path.to_string_lossy();
            PathBuf::from(
                text.strip_suffix(".narada-generation.json")
                    .unwrap_or(&text),
            )
        });
        return json!({
            "schema": "narada.mcp_runtime_proxy.materialization_recovery.v1",
            "recovery_group_id": group_id,
            "deduplication": { "scope": "carrier_materialization", "key": group_id, "guidance": "Report one recovery action for this group; bootstrap surfaces sharing this id describe the same carrier failure." },
            "carrier": { "carrier_id": options.carrier_id, "carrier_kind": options.carrier_kind, "config_path": config_path },
            "regeneration": { "required": true, "available": !command.is_null(), "owner": "narada-mcp-materializer", "command": command, "unavailable_reason": if options.registrar_entrypoint.is_none() { Value::String("The materialization record does not identify the native materializer entrypoint.".to_string()) } else { Value::Null } },
            "restart_required": true,
            "restart": { "owner": options.carrier_kind.as_deref().unwrap_or("carrier"), "automatic": false, "instruction": carrier_restart(options.carrier_kind.as_deref()) }
        });
    }
    let workspace_root = options
        .artifact_manifest
        .as_ref()
        .and_then(|path| path.parent())
        .and_then(Path::parent)
        .and_then(Path::parent);
    json!({
        "schema": "narada.mcp_runtime_proxy.workspace_recovery.v1",
        "recovery_group_id": group_id,
        "deduplication": { "scope": "carrier_materialization", "key": group_id, "guidance": "Report one build/materialization action for this group; bootstrap surfaces sharing this id describe the same carrier failure." },
        "cause": { "code": refusal.code, "reason": refusal.reason, "details": refusal.details },
        "steps": [
            { "order": 1, "action": "build_workspace", "command": { "executable": "cargo", "args": ["native-package"], "cwd": workspace_root, "display": "cargo native-package" } },
            { "order": 2, "action": "materialize_all_carriers", "required": true, "owner": "narada-mcp-materializer", "available": !command.is_null(), "command": command, "unavailable_reason": if options.registrar_entrypoint.is_none() { Value::String("The carrier launch does not identify the native materializer entrypoint.".to_string()) } else { Value::Null } },
            { "order": 3, "action": "restart_carrier", "required": true, "automatic": false, "instruction": carrier_restart(options.carrier_kind.as_deref()) }
        ],
        "restart_required": true
    })
}

fn carrier_restart(kind: Option<&str>) -> &'static str {
    match kind {
        Some("codex") => "Restart Codex or start a new Codex session after materialization.",
        Some("kimi") => "Restart Kimi or start a new Kimi session after materialization.",
        Some("opencode") => {
            "Restart OpenCode or start a new OpenCode session after materialization."
        }
        _ => "Restart the carrier or start a new carrier session after materialization.",
    }
}

fn write_instance(
    options: &Options,
    proxy_pid: u32,
    child_pid: u32,
    started_at: &str,
    state: &str,
    exit_code: Option<i32>,
    freshness: &FreshnessTracker,
) -> Result<(), String> {
    let now = now_iso();
    let lease_expires_at = OffsetDateTime::now_utc()
        .saturating_add(time::Duration::milliseconds(
            (options.liveness_check_ms.saturating_mul(3)) as i64,
        ))
        .format(&Rfc3339)
        .unwrap_or_else(|_| now.clone());
    let record = json!({
        "schema": "narada.mcp_runtime_proxy.instance.v2",
        "surface_id": options.surface_id,
        "proxy_pid": proxy_pid,
        "parent_pid": parent_pid(),
        "child_pid": child_pid,
        "supervisor_pid": Value::Null,
        "managed_child_pid": child_pid,
        "server_pid": child_pid,
        "entrypoint": options.entrypoint,
        "started_at": started_at,
        "heartbeat_at": now,
        "lease_expires_at": lease_expires_at,
        "state": state,
        "liveness_evidence": { "proxy_implementation": "native", "carrier_id": options.carrier_id, "exit_code": exit_code },
        "runtime_freshness": evaluate_freshness(options, proxy_pid, child_pid, None, freshness),
        "artifact_manifest_path": options.artifact_manifest,
        "generation_id": format!("{}:{}", options.surface_id.as_deref().unwrap_or("surface"), started_at),
        "closed_at": if state == "closed" { Value::String(now) } else { Value::Null }
    });
    atomic_json(
        &options
            .diagnostics_dir
            .join(format!("instance-{proxy_pid}.json")),
        &record,
    )
}

fn list_instances(args: &[String]) -> Result<(), String> {
    let root = args
        .iter()
        .position(|value| value == "--diagnostics-dir")
        .and_then(|index| args.get(index + 1))
        .map(PathBuf::from)
        .unwrap_or_else(default_diagnostics_dir);
    let mut instances = Vec::new();
    if let Ok(entries) = fs::read_dir(&root) {
        for entry in entries.flatten().take(10_000) {
            let name = entry.file_name().to_string_lossy().to_string();
            if name.starts_with("instance-") && name.ends_with(".json") {
                if let Ok(value) = read_json(&entry.path()) {
                    instances.push(classify_instance(value));
                }
            }
        }
    }
    let count = |state: &str| {
        instances
            .iter()
            .filter(|value| value.get("observed_state").and_then(Value::as_str) == Some(state))
            .count()
    };
    let output = json!({
        "schema": "narada.mcp_runtime_proxy.instance_list.v1",
        "status": "ok",
        "diagnostics_dir": absolute(root),
        "observed_at": now_iso(),
        "counts": { "total": instances.len(), "live": count("live"), "stale": count("stale"), "reclaimed": count("reclaimed"), "closed": count("closed") },
        "instances": instances
    });
    println!(
        "{}",
        serde_json::to_string_pretty(&output).unwrap_or_else(|_| "{}".to_string())
    );
    Ok(())
}
