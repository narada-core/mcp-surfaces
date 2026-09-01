fn verify_artifact_build_set(input: &MaterializationInput) -> Result<(), Failure> {
    let mut build_set = read_json(
        &input.artifact_build_set_path,
        "materializer_artifact_build_set_invalid",
    )?;
    if build_set.get("schema").and_then(Value::as_str) != Some("narada.artifact_build_set.v1")
        || build_set.get("assurance").and_then(Value::as_str) != Some("declared_isolated_closure")
    {
        return Err(Failure::new(
            "materializer_artifact_build_set_schema_unsupported",
            path_text(&input.artifact_build_set_path),
        ));
    }
    let expected_digest = json_field_string(&build_set, "build_set_digest")?.to_string();
    if expected_digest != input.artifact_build_set_fingerprint {
        return Err(Failure::new(
            "materializer_artifact_build_set_identity_mismatch",
            path_text(&input.artifact_build_set_path),
        ));
    }
    let unsigned = build_set
        .as_object_mut()
        .ok_or_else(|| Failure::new("materializer_artifact_build_set_invalid", "not_object"))?;
    unsigned.remove("build_set_digest");
    unsigned.remove("generated_at");
    let actual_digest = format!(
        "sha256:{}",
        canonical_json_sha256(&build_set)
            .map_err(|error| Failure::new("materializer_artifact_build_set_invalid", error))?
    );
    if actual_digest != expected_digest {
        return Err(Failure::new(
            "materializer_artifact_build_set_fingerprint_mismatch",
            path_text(&input.artifact_build_set_path),
        ));
    }
    let manifest_path = json_field_string(&build_set, "workspace_manifest_path")?;
    if !path_eq(Path::new(manifest_path), &input.artifact_manifest_path)
        || build_set
            .get("workspace_manifest_fingerprint")
            .and_then(Value::as_str)
            != input.artifact_manifest_fingerprint.as_deref()
    {
        return Err(Failure::new(
            "materializer_artifact_build_set_manifest_mismatch",
            manifest_path,
        ));
    }
    let manifest_bytes = fs::read(&input.artifact_manifest_path).map_err(|error| {
        Failure::new(
            "materializer_artifact_manifest_read_failed",
            error.to_string(),
        )
    })?;
    if build_set
        .get("workspace_manifest_bytes_digest")
        .and_then(Value::as_str)
        != Some(format!("sha256:{}", sha256(&manifest_bytes)).as_str())
    {
        return Err(Failure::new(
            "materializer_artifact_build_set_manifest_bytes_mismatch",
            path_text(&input.artifact_manifest_path),
        ));
    }
    let artifacts = build_set
        .get("artifacts")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            Failure::new(
                "materializer_artifact_build_set_artifacts_required",
                path_text(&input.artifact_build_set_path),
            )
        })?;
    let mut declared = BTreeMap::<String, String>::new();
    for artifact in artifacts {
        let path = PathBuf::from(json_field_string(artifact, "path")?);
        let expected = json_field_string(artifact, "sha256")?;
        let size = artifact
            .get("size")
            .and_then(Value::as_u64)
            .ok_or_else(|| {
                Failure::new(
                    "materializer_artifact_build_set_size_required",
                    path_text(&path),
                )
            })?;
        let bytes = fs::read(&path).map_err(|error| {
            Failure::new(
                "materializer_artifact_build_set_artifact_missing",
                error.to_string(),
            )
            .with_details(json!({"path":path_text(&path)}))
        })?;
        if bytes.len() as u64 != size || format!("sha256:{}", sha256(&bytes)) != expected {
            return Err(Failure::new(
                "materializer_artifact_build_set_artifact_stale",
                path_text(&path),
            ));
        }
        declared.insert(path_text(&path).to_lowercase(), expected.to_string());
    }
    let mut references = BTreeSet::<PathBuf>::new();
    references.insert(input.registrar_entrypoint.clone());
    references.insert(input.proxy_entrypoint.clone());
    for carrier in &input.carriers {
        for server in &carrier.servers {
            let command = PathBuf::from(&server.command);
            if command.is_absolute() {
                references.insert(command);
            }
            for index in 0..server.args.len().saturating_sub(1) {
                if matches!(
                    server.args[index].as_str(),
                    "--child-command"
                        | "--entrypoint"
                        | "--registrar-command"
                        | "--registrar-entrypoint"
                ) {
                    let reference = PathBuf::from(&server.args[index + 1]);
                    if reference.is_absolute() {
                        references.insert(reference);
                    }
                }
            }
        }
    }
    let missing = references
        .iter()
        .filter(|path| !declared.contains_key(&path_text(path).to_lowercase()))
        .map(|path| path_text(path))
        .collect::<Vec<_>>();
    if !missing.is_empty() {
        return Err(Failure::new(
            "materializer_artifact_build_set_reference_missing",
            "A launch reference is absent from the sealed build set.",
        )
        .with_details(json!({"missing_references":missing})));
    }
    Ok(())
}

fn fresh_process_validate(input: &MaterializationInput) -> Result<(), Failure> {
    let mut validated = BTreeSet::<String>::new();
    for carrier in &input.carriers {
        for server in &carrier.servers {
            let descriptor = serde_json::to_vec(&json!({
                "command": server.command,
                "args": server.args,
            }))
            .map_err(json_failure)?;
            let descriptor_digest = sha256(&descriptor);
            if !validated.insert(descriptor_digest.clone()) {
                continue;
            }
            validate_launch_descriptor(server).map_err(|failure| {
                failure.with_details(json!({
                    "carrier_id": carrier.carrier_id,
                    "server_name": server.name,
                    "descriptor_digest": descriptor_digest,
                    "scope": "fresh_process_validation",
                }))
            })?;
        }
    }
    Ok(())
}

