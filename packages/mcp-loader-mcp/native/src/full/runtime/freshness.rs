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

fn current_artifact_pointer_target(pointer_path: &str, executable_name: &str) -> Option<String> {
    let pointer: Value = serde_json::from_str(&read_to_string(pointer_path).ok()?).ok()?;
    let artifact = pointer.get("artifacts")?.get(executable_name)?.as_str()?;
    let parent = Path::new(pointer_path).parent()?;
    Some(normalize_path(&parent.join(artifact).to_string_lossy()))
}

fn same_path(left: &str, right: &str) -> bool {
    let left = normalize_path(left);
    let right = normalize_path(right);
    #[cfg(windows)]
    {
        left.eq_ignore_ascii_case(&right)
    }
    #[cfg(not(windows))]
    {
        left == right
    }
}

fn current_artifact_reason(runtime: &str, expected: Option<&str>) -> Option<&'static str> {
    match expected {
        None => Some("current_artifact_pointer_unavailable"),
        Some(path) if !Path::new(path).exists() => Some("current_runtime_artifact_unavailable"),
        Some(path) if !same_path(runtime, path) => Some("running_artifact_not_current"),
        Some(_) => None,
    }
}

fn loader_source_inventory(root: &str) -> io::Result<Value> {
    fn visit(
        directory: &Path,
        count: &mut usize,
        newest: &mut Option<(String, u128)>,
    ) -> io::Result<()> {
        for entry in read_dir(directory)? {
            let entry = entry?;
            let kind = entry.file_type()?;
            let path = entry.path();
            if kind.is_dir() {
                visit(&path, count, newest)?;
            } else if kind.is_file() && path.extension().and_then(|ext| ext.to_str()) == Some("rs")
            {
                let stat = entry.metadata()?;
                let modified = stat
                    .modified()?
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_millis();
                *count += 1;
                if newest
                    .as_ref()
                    .map_or(true, |(_, newest_ms)| modified > *newest_ms)
                {
                    *newest = Some((normalize_path(&path.to_string_lossy()), modified));
                }
            }
        }
        Ok(())
    }

    let mut count = 0;
    let mut newest = None;
    visit(Path::new(root), &mut count, &mut newest)?;
    let newest_source = newest.map(
        |(path, modified)| json!({"path":path,"mtime_ms":modified,"mtime":ms_to_iso(modified)}),
    );
    Ok(json!({
        "root":normalize_path(root),
        "status":if count == 0 {"empty"} else {"ok"},
        "rust_source_count":count,
        "newest_source":newest_source
    }))
}

fn observation_mtime_ms(observation: &Value) -> Option<u128> {
    observation
        .get("mtime_ms")
        .and_then(Value::as_u64)
        .map(u128::from)
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
    let executable_name = Path::new(&runtime_entrypoint)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default();
    let loader_executable_name = if cfg!(windows) {
        "narada-mcp-loader.exe"
    } else {
        "narada-mcp-loader"
    };
    let is_loader_process = executable_name == loader_executable_name;
    let mut reasons = Vec::new();
    let current_artifact_pointer_path = join_path(
        &state.workspace_root,
        "packages/mcp-loader-mcp/dist/native/current.json",
    );
    let current_artifact_pointer = observe_file(&current_artifact_pointer_path);
    let expected_runtime_entrypoint = if is_loader_process {
        current_artifact_pointer_target(&current_artifact_pointer_path, loader_executable_name)
    } else {
        None
    };
    let source_root = join_path(&state.workspace_root, "packages/mcp-loader-mcp/native/src");
    let source_inventory = match loader_source_inventory(&source_root) {
        Ok(inventory) => inventory,
        Err(_) => {
            reasons.push("loader_source_inventory_unavailable".to_string());
            json!({"root":source_root,"status":"unavailable","rust_source_count":0,"newest_source":Value::Null})
        }
    };
    if source_inventory["status"] == "empty" {
        reasons.push("loader_source_inventory_unavailable:empty".to_string());
    }
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
    if is_loader_process {
        if let Some(reason) =
            current_artifact_reason(&runtime_entrypoint, expected_runtime_entrypoint.as_deref())
        {
            reasons.push(reason.to_string());
        }
    }
    let runtime_observation = observe_file(&runtime_entrypoint);
    match (
        observation_mtime_ms(&runtime_observation),
        source_inventory["newest_source"]["mtime_ms"].as_u64(),
    ) {
        (Some(runtime_ms), Some(source_ms)) if u128::from(source_ms) > runtime_ms => {
            reasons.push("loader_source_newer_than_runtime_artifact".to_string());
        }
        (None, _) => reasons.push("runtime_artifact_mtime_unavailable".to_string()),
        (_, None) => reasons.push("loader_source_mtime_unavailable".to_string()),
        _ => {}
    }
    let runtime_mtime_ms = observation_mtime_ms(&runtime_observation);
    for config in &config_observations {
        let name = config["name"].as_str().unwrap_or("unknown");
        match (
            runtime_mtime_ms,
            observation_mtime_ms(&config["observation"]),
        ) {
            (Some(runtime_ms), Some(config_ms)) if config_ms > runtime_ms => {
                reasons.push(format!(
                    "loader_build_config_newer_than_runtime_artifact:{name}"
                ));
            }
            (_, None) => reasons.push(format!("loader_build_config_unavailable:{name}")),
            (None, _) => {}
            _ => {}
        }
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
    let tracked_file_count = source_inventory["rust_source_count"].as_u64().unwrap_or(0) as usize
        + config_files.len()
        + 2;
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
        "source_inventory":source_inventory,
        "runtime_files":runtime_files,
        "dependency_files":dependencies,
        "config_files":config_observations,
        "current_artifact_pointer":current_artifact_pointer,
        "expected_runtime_entrypoint":expected_runtime_entrypoint,
        "tracked_file_count":tracked_file_count,
        "authority":"native_rust",
        "runtime_artifact_sharing":"loader_entrypoint and loader_runtime_impl are compiled into the same native executable",
        "reasons":reasons,
        "reload_action":reload_action
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_root(label: &str) -> PathBuf {
        env::temp_dir().join(format!("{}-{}", label, new_id("loader-freshness")))
    }

    #[test]
    fn source_inventory_includes_nested_rust_modules() {
        let root = temp_root("source-inventory");
        let nested = root.join("full/connections/activation/open");
        create_dir_all(&nested).expect("create nested source directory");
        fs::write(root.join("main.rs"), "fn main() {}\n").expect("write root source");
        fs::write(nested.join("handles.rs"), "fn handles() {}\n").expect("write nested source");

        let inventory = loader_source_inventory(&root.to_string_lossy()).expect("inventory");
        assert_eq!(inventory["rust_source_count"], 2);
        assert!(inventory["newest_source"]["path"]
            .as_str()
            .expect("newest path")
            .ends_with("handles.rs"));

        remove_dir_all(root).expect("remove temporary source tree");
    }

    #[test]
    fn current_pointer_resolves_versioned_runtime_artifact() {
        let root = temp_root("artifact-pointer");
        let native = root.join("dist/native");
        let artifact = native.join("versions/build-123/narada-mcp-loader.exe");
        create_dir_all(artifact.parent().expect("artifact parent"))
            .expect("create artifact directory");
        fs::write(&artifact, "test artifact").expect("write artifact");
        let pointer = native.join("current.json");
        fs::write(
            &pointer,
            r#"{"artifacts":{"narada-mcp-loader.exe":"versions/build-123/narada-mcp-loader.exe"}}"#,
        )
        .expect("write pointer");

        let resolved =
            current_artifact_pointer_target(&pointer.to_string_lossy(), "narada-mcp-loader.exe")
                .expect("resolve current artifact");
        assert!(same_path(&resolved, &artifact.to_string_lossy()));

        remove_dir_all(root).expect("remove temporary artifact tree");
    }

    #[test]
    fn runtime_path_must_match_published_artifact() {
        let root = temp_root("artifact-match");
        let old_runtime = root.join("versions/old/narada-mcp-loader.exe");
        let current_runtime = root.join("versions/current/narada-mcp-loader.exe");
        create_dir_all(old_runtime.parent().expect("old runtime parent"))
            .expect("create old runtime directory");
        create_dir_all(current_runtime.parent().expect("current runtime parent"))
            .expect("create current runtime directory");
        fs::write(&old_runtime, "old").expect("write old runtime");
        fs::write(&current_runtime, "current").expect("write current runtime");

        assert_eq!(
            current_artifact_reason(
                &old_runtime.to_string_lossy(),
                Some(&current_runtime.to_string_lossy())
            ),
            Some("running_artifact_not_current")
        );
        assert_eq!(
            current_artifact_reason(
                &current_runtime.to_string_lossy(),
                Some(&current_runtime.to_string_lossy())
            ),
            None
        );

        remove_dir_all(root).expect("remove temporary artifact tree");
    }
}
