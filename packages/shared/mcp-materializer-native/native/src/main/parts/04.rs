fn verify_generation(
    generation: &Value,
    sidecar_path: &Path,
    indexed_fingerprint: Option<&str>,
) -> Result<(), Failure> {
    if generation.get("schema").and_then(Value::as_str) != Some(GENERATION_SCHEMA) {
        return Err(Failure::new(
            "materializer_generation_schema_unsupported",
            path_text(sidecar_path),
        ));
    }
    let expected = json_field_string(generation, "generation_fingerprint")?;
    if indexed_fingerprint != Some(expected) {
        return Err(Failure::new(
            "materializer_index_generation_mismatch",
            path_text(sidecar_path),
        ));
    }
    if generation_fingerprint(generation)
        .map_err(|error| Failure::new("materializer_generation_fingerprint_failed", error))?
        != expected
    {
        return Err(Failure::new(
            "materializer_generation_fingerprint_mismatch",
            path_text(sidecar_path),
        ));
    }
    if generation.get("contract_version").and_then(Value::as_u64) != Some(CONTRACT_VERSION.into()) {
        return Err(Failure::new(
            "materializer_generation_contract_obsolete",
            path_text(sidecar_path),
        ));
    }
    verify_generation_bundle(generation, sidecar_path)?;
    verify_file_fingerprint(generation, "registrar_entrypoint", "registrar_fingerprint")?;
    verify_file_fingerprint(
        generation,
        "materialization_contract_entrypoint",
        "materialization_contract_fingerprint",
    )?;
    verify_file_fingerprint(generation, "proxy_entrypoint", "proxy_fingerprint")?;
    verify_file_fingerprint(
        generation,
        "runtime_implementation_matrix_path",
        "runtime_implementation_matrix_fingerprint",
    )?;
    let manifest_path = PathBuf::from(json_field_string(generation, "artifact_manifest_path")?);
    let manifest = read_json(&manifest_path, "materializer_artifact_manifest_invalid")?;
    if manifest.get("manifest_fingerprint").and_then(Value::as_str)
        != generation
            .get("artifact_manifest_fingerprint")
            .and_then(Value::as_str)
    {
        return Err(Failure::new(
            "materializer_artifact_manifest_fingerprint_mismatch",
            path_text(&manifest_path),
        ));
    }
    let config_path = PathBuf::from(json_field_string(generation, "config_path")?);
    let expected_config_path = sidecar_path
        .to_string_lossy()
        .strip_suffix(".narada-generation.json")
        .map(PathBuf::from)
        .ok_or_else(|| {
            Failure::new(
                "materializer_generation_sidecar_pair_invalid",
                path_text(sidecar_path),
            )
        })?;
    if !path_eq(&config_path, &expected_config_path) {
        return Err(Failure::new(
            "materializer_generation_config_pair_mismatch",
            path_text(sidecar_path),
        ));
    }
    let kind = json_field_string(generation, "carrier_kind")?;
    let config = fs::read(&config_path)
        .map_err(|error| Failure::new("materializer_config_read_failed", error.to_string()))?;
    let selectors = generation
        .pointer("/managed_projection/selectors")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            Failure::new(
                "materializer_managed_selectors_missing",
                path_text(sidecar_path),
            )
        })?
        .iter()
        .map(|value| {
            value.as_str().map(str::to_string).ok_or_else(|| {
                Failure::new(
                    "materializer_managed_selector_invalid",
                    path_text(sidecar_path),
                )
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    let current = describe_config(kind, &config, &selectors)
        .map_err(|error| Failure::new("materializer_contract_describe_failed", error))?;
    if current.managed_projection.sha256
        != generation
            .pointer("/managed_projection/sha256")
            .and_then(Value::as_str)
            .unwrap_or("")
    {
        return Err(Failure::new(
            "materializer_managed_projection_fingerprint_mismatch",
            path_text(&config_path),
        ));
    }
    let plan_path = PathBuf::from(json_field_string(
        generation,
        "runtime_materialization_plan_path",
    )?);
    let plan = read_json(&plan_path, "materializer_runtime_plan_invalid")?;
    let expected_plan = json_field_string(generation, "runtime_materialization_plan_fingerprint")?;
    let mut unsigned_plan = plan.clone();
    let embedded_plan = unsigned_plan
        .as_object_mut()
        .and_then(|object| object.remove("plan_fingerprint"))
        .and_then(|value| value.as_str().map(str::to_string));
    if embedded_plan.as_deref() != Some(expected_plan)
        || generation
            .get("launch_catalog_fingerprint")
            .and_then(Value::as_str)
            != Some(expected_plan)
        || sha256(&serde_json::to_vec(&unsigned_plan).map_err(json_failure)?) != expected_plan
    {
        return Err(Failure::new(
            "materializer_runtime_plan_fingerprint_mismatch",
            path_text(&plan_path),
        ));
    }
    let source = plan.get("source").ok_or_else(|| {
        Failure::new(
            "materializer_runtime_plan_source_missing",
            path_text(&plan_path),
        )
    })?;
    let contract_path = PathBuf::from(json_field_string(source, "carrier_contract_path")?);
    let contract = fs::read(&contract_path).map_err(|error| {
        Failure::new(
            "materializer_carrier_contract_read_failed",
            error.to_string(),
        )
    })?;
    if source
        .get("carrier_contract_fingerprint")
        .and_then(Value::as_str)
        != Some(sha256(&contract).as_str())
    {
        return Err(Failure::new(
            "materializer_carrier_contract_fingerprint_mismatch",
            path_text(&contract_path),
        ));
    }
    Ok(())
}

fn verify_generation_bundle(generation: &Value, sidecar_path: &Path) -> Result<(), Failure> {
    let bundle_id = json_field_string(generation, "bundle_id")?;
    let expected_bundle_fingerprint = json_field_string(generation, "bundle_fingerprint")?;
    let bundle_path = PathBuf::from(json_field_string(generation, "bundle_path")?);
    let mut bundle = read_json(&bundle_path, "materializer_bundle_invalid")?;
    if bundle.get("schema").and_then(Value::as_str) != Some("narada.carrier_generation_bundle.v1")
        || bundle.get("bundle_id").and_then(Value::as_str) != Some(bundle_id)
        || bundle.get("bundle_fingerprint").and_then(Value::as_str)
            != Some(expected_bundle_fingerprint)
    {
        return Err(Failure::new(
            "materializer_bundle_identity_mismatch",
            path_text(&bundle_path),
        ));
    }
    let object = bundle
        .as_object_mut()
        .ok_or_else(|| Failure::new("materializer_bundle_invalid", path_text(&bundle_path)))?;
    object.remove("bundle_id");
    object.remove("bundle_fingerprint");
    let actual = canonical_json_sha256(&bundle)
        .map_err(|error| Failure::new("materializer_bundle_fingerprint_failed", error))?;
    if actual != expected_bundle_fingerprint {
        return Err(Failure::new(
            "materializer_bundle_fingerprint_mismatch",
            path_text(&bundle_path),
        ));
    }
    let carrier_id = json_field_string(generation, "carrier_id")?;
    let member = bundle
        .get("carriers")
        .and_then(Value::as_array)
        .and_then(|carriers| {
            carriers.iter().find(|carrier| {
                carrier.get("carrier_id").and_then(Value::as_str) == Some(carrier_id)
            })
        })
        .ok_or_else(|| {
            Failure::new(
                "materializer_bundle_carrier_missing",
                format!("{bundle_id}:{carrier_id}"),
            )
        })?;
    if member
        .get("generation_sidecar_path")
        .and_then(Value::as_str)
        .is_none_or(|path| !path_eq(Path::new(path), sidecar_path))
    {
        return Err(Failure::new(
            "materializer_bundle_sidecar_mismatch",
            path_text(sidecar_path),
        ));
    }
    let pointer_path = PathBuf::from(json_field_string(generation, "bundle_commit_pointer_path")?);
    let pointer = read_json(&pointer_path, "materializer_bundle_pointer_invalid")?;
    if pointer.get("schema").and_then(Value::as_str)
        != Some("narada.carrier_generation_bundle_pointer.v1")
        || pointer.get("bundle_id").and_then(Value::as_str) != Some(bundle_id)
        || pointer.get("bundle_fingerprint").and_then(Value::as_str)
            != Some(expected_bundle_fingerprint)
        || pointer
            .get("bundle_path")
            .and_then(Value::as_str)
            .is_none_or(|path| !path_eq(Path::new(path), &bundle_path))
    {
        return Err(Failure::new(
            "materializer_bundle_not_committed",
            path_text(&pointer_path),
        ));
    }
    let build_set_path = PathBuf::from(json_field_string(generation, "artifact_build_set_path")?);
    let build_set = read_json(&build_set_path, "materializer_artifact_build_set_invalid")?;
    if build_set.get("build_set_digest").and_then(Value::as_str)
        != generation
            .get("artifact_build_set_fingerprint")
            .and_then(Value::as_str)
    {
        return Err(Failure::new(
            "materializer_artifact_build_set_identity_mismatch",
            path_text(&build_set_path),
        ));
    }
    Ok(())
}

fn verify_file_fingerprint(
    generation: &Value,
    path_field: &'static str,
    fingerprint_field: &'static str,
) -> Result<(), Failure> {
    let path = PathBuf::from(json_field_string(generation, path_field)?);
    let bytes = fs::read(&path).map_err(|error| {
        Failure::new("materializer_authority_file_read_failed", error.to_string())
    })?;
    if generation.get(fingerprint_field).and_then(Value::as_str) != Some(sha256(&bytes).as_str()) {
        return Err(Failure::new(
            "materializer_authority_file_fingerprint_mismatch",
            path_text(&path),
        ));
    }
    Ok(())
}

fn read_json(path: &Path, code: &'static str) -> Result<Value, Failure> {
    let bytes = fs::read(path).map_err(|error| Failure::new(code, error.to_string()))?;
    serde_json::from_slice(&bytes).map_err(|error| Failure::new(code, error.to_string()))
}

fn json_field_string<'a>(value: &'a Value, field: &'static str) -> Result<&'a str, Failure> {
    value
        .get(field)
        .and_then(Value::as_str)
        .ok_or_else(|| Failure::new("materializer_json_field_required", field))
}

