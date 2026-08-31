use crate::full::*;

pub(crate) fn observe_file(path: &str) -> Value {
    match metadata(path) {
        Ok(stat) => {
            let modified = stat
                .modified()
                .ok()
                .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
                .map(|duration| duration.as_millis());
            json!({"path":path,"exists":true,"mtime_ms":modified,"mtime":modified.map(ms_to_iso)})
        }
        Err(_) => json!({"path":path,"exists":false,"mtime_ms":Value::Null,"mtime":Value::Null}),
    }
}

pub(crate) fn ms_to_iso(milliseconds: u128) -> String {
    OffsetDateTime::from_unix_timestamp_nanos((milliseconds.saturating_mul(1_000_000)) as i128)
        .ok()
        .and_then(|date| date.format(&Rfc3339).ok())
        .unwrap_or_else(|| "1970-01-01T00:00:00Z".to_string())
}

pub(crate) fn runtime_freshness(state: &LoaderState) -> Value {
    let mut reload_action = supervisor_restart_action();
    reload_action["guidance"] = json!("Restart the mcp-loader process through its carrier or runtime supervisor to load rebuilt loader code. mcp_loader_surface_restart replaces only an attached child and does not reload the mcp-loader process.");
    let loader_source = join_path(
        &state.workspace_root,
        "packages/mcp-loader-mcp/native/src/main.rs",
    );

    let runtime_entrypoint = env::current_exe()
        .ok()
        .map(|path| normalize_path(&path.to_string_lossy()))
        .unwrap_or_else(|| "narada-mcp-loader".to_string());
    let pairs = vec![
        (
            "loader_entrypoint",
            loader_source.clone(),
            runtime_entrypoint.clone(),
        ),
        (
            "loader_runtime_impl",
            join_path(
                &state.workspace_root,
                "packages/mcp-loader-mcp/native/src/full.rs",
            ),
            runtime_entrypoint.clone(),
        ),
    ];
    let config_files = vec![
        (
            "workspace_cargo_lockfile",
            join_path(&state.workspace_root, "Cargo.lock"),
        ),
        (
            "loader_cargo_manifest",
            join_path(
                &state.workspace_root,
                "packages/mcp-loader-mcp/native/Cargo.toml",
            ),
        ),
    ];
    // The native Rust sources and Cargo manifests are the loader authority.
    // The TypeScript implementation is retained only as a non-authoritative
    // compatibility artifact and is deliberately absent from freshness data.
    let mut reasons = Vec::new();
    let mut file_pairs = Vec::new();
    for (name, source, runtime) in &pairs {
        let source_obs = observe_file(source);
        let runtime_obs = observe_file(runtime);
        let runtime_exists = runtime_obs
            .get("exists")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        if *name == "loader_entrypoint" && !runtime_exists {
            reasons.push("runtime_file_unavailable:loader_entrypoint".to_string());
        }
        file_pairs.push(json!({"name":name,"source":source_obs,"runtime":runtime_obs}));
    }
    let mut config_observations = Vec::new();
    for (name, path) in &config_files {
        let observation = observe_file(path);
        config_observations.push(json!({"name":name,"observation":observation}));
    }
    let status = if reasons.iter().any(|reason| reason.contains("unavailable")) {
        "unknown"
    } else if reasons.is_empty() {
        "current"
    } else {
        "stale"
    };
    let entrypoint = file_pairs
        .iter()
        .find(|pair| pair.get("name").and_then(Value::as_str) == Some("loader_entrypoint"))
        .cloned()
        .unwrap_or_else(|| json!({"source":null,"runtime":null}));
    let source_files: Vec<Value> = file_pairs
        .iter()
        .map(|pair| {
            let mut value = json!({"name":pair["name"]});
            value["observation"] = pair["source"].clone();
            value
        })
        .collect();
    let runtime_files: Vec<Value> = file_pairs
        .iter()
        .map(|pair| {
            let mut value = json!({"name":pair["name"]});
            value["observation"] = pair["runtime"].clone();
            value
        })
        .collect();
    let dependencies: Vec<Value> = file_pairs
        .iter()
        .filter(|pair| pair.get("name").and_then(Value::as_str) != Some("loader_entrypoint"))
        .map(|pair| json!({"name":pair["name"],"source":pair["source"],"runtime":pair["runtime"]}))
        .collect();
    json!({
        "schema":"narada.mcp_loader.runtime_freshness.v1",
        "status":status,
        "reload_required":if status=="stale" {Value::Bool(true)} else if status=="current" {Value::Bool(false)} else {Value::Null},
        "process_started_at":ms_to_iso(state.started_ms),
        "process_started_at_ms":state.started_ms,
        "freshness_scope":"native_loader_artifact",
        "runtime_entrypoint":entrypoint.get("runtime").cloned().unwrap_or(Value::Null),
        "source_entrypoint":entrypoint.get("source").cloned().unwrap_or(Value::Null),
        "source_files":source_files,
        "runtime_files":runtime_files,
        "dependency_files":dependencies,
        "config_files":config_observations,
        "tracked_file_count":file_pairs.len()*2+config_files.len(),
        "authority":"native_rust",
        "runtime_artifact_sharing":"loader_entrypoint and loader_runtime_impl are compiled into the same native executable",
        "reasons":reasons,
        "reload_action":reload_action
    })
}
