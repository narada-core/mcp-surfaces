fn publish_self(artifact_root: &Path) -> Result<Value, Failure> {
    let artifact_root = if artifact_root.is_absolute() {
        artifact_root.to_path_buf()
    } else {
        env::current_dir()
            .map_err(|error| {
                Failure::new("materializer_working_directory_failed", error.to_string())
            })?
            .join(artifact_root)
    };
    let executable = env::current_exe()
        .map_err(|error| Failure::new("materializer_executable_unresolved", error.to_string()))?;
    let bytes = fs::read(&executable)
        .map_err(|error| Failure::new("materializer_executable_read_failed", error.to_string()))?;
    let fingerprint = sha256(&bytes);
    let name = if cfg!(windows) {
        "narada-mcp-materializer.exe"
    } else {
        "narada-mcp-materializer"
    };
    let destination = artifact_root.join("versions").join(&fingerprint).join(name);
    if let Some(parent) = destination.parent() {
        fs::create_dir_all(parent).map_err(|error| {
            Failure::new("materializer_artifact_directory_failed", error.to_string())
        })?;
    }
    if destination.exists() {
        let existing = fs::read(&destination).map_err(|error| {
            Failure::new("materializer_artifact_read_failed", error.to_string())
        })?;
        if existing != bytes {
            return Err(Failure::new(
                "materializer_artifact_collision",
                path_text(&destination),
            ));
        }
    } else {
        atomic_write(&destination, &bytes).map_err(|error| {
            Failure::new("materializer_artifact_publish_failed", error.to_string())
        })?;
    }
    let generated_at = OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .map_err(|error| Failure::new("materializer_clock_failed", error.to_string()))?;
    let relative = format!("versions/{fingerprint}/{name}");
    let pointer = json!({
        "schema": "narada.mcp_materializer.native_artifact_pointer.v1",
        "generated_at": generated_at,
        "build_fingerprint": fingerprint,
        "artifacts": { name: relative },
    });
    atomic_write(&artifact_root.join("current.json"), &pretty_json(&pointer)?).map_err(
        |error| {
            Failure::new(
                "materializer_artifact_pointer_publish_failed",
                error.to_string(),
            )
        },
    )?;
    Ok(json!({
        "schema": "narada.mcp_materializer.publish_result.v1",
        "status": "published",
        "executable": path_text(&destination),
        "pointer_path": path_text(&artifact_root.join("current.json")),
        "build_fingerprint": fingerprint,
    }))
}

fn previous_managed_selectors(sidecar_path: &Path) -> Result<Vec<String>, Failure> {
    if !sidecar_path.exists() {
        return Ok(vec![]);
    }
    let generation = read_json(sidecar_path, "materializer_generation_invalid")?;
    match generation.get("schema").and_then(Value::as_str) {
        Some(GENERATION_SCHEMA) | Some(LEGACY_GENERATION_SCHEMA) => generation
            .pointer("/managed_projection/selectors")
            .and_then(Value::as_array)
            .ok_or_else(|| {
                Failure::new(
                    "materializer_managed_selectors_missing",
                    path_text(sidecar_path),
                )
            })?
            .iter()
            .map(|selector| {
                selector.as_str().map(str::to_string).ok_or_else(|| {
                    Failure::new(
                        "materializer_managed_selector_invalid",
                        path_text(sidecar_path),
                    )
                })
            })
            .collect(),
        Some(AMBIGUOUS_GENERATION_SCHEMA) => Ok(vec!["/mcp_servers".to_string()]),
        Some(schema) => Err(Failure::new(
            "materializer_generation_schema_unsupported",
            schema,
        )),
        None => Err(Failure::new(
            "materializer_generation_schema_missing",
            path_text(sidecar_path),
        )),
    }
}

