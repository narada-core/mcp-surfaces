fn preflight_materialization(
    options: &Options,
    sidecar: &Path,
    manifest_fingerprint: Option<&str>,
) -> Result<(), Refusal> {
    let generation = read_json(sidecar).map_err(|error| {
        refusal(
            "materialization_generation_missing",
            "The materialization generation sidecar is missing or unreadable.",
            json!({ "error": error }),
        )
    })?;
    if matches!(
        generation.get("schema").and_then(Value::as_str),
        Some(LEGACY_GENERATION_SCHEMA) | Some(AMBIGUOUS_GENERATION_SCHEMA)
    ) {
        return Err(refusal(
            "materialization_generation_obsolete",
            "The materialization generation predates committed bundle admission.",
            json!({"remediation":"Regenerate this carrier with the current materializer; legacy generations remain readable only as untrusted recovery input."}),
        ));
    }
    if generation.get("schema").and_then(Value::as_str) != Some(GENERATION_SCHEMA) {
        return Err(refusal(
            "materialization_generation_stale",
            "The materialization generation sidecar has an unsupported schema.",
            json!({}),
        ));
    }
    let expected = generation
        .get("generation_fingerprint")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            refusal(
                "materialization_generation_stale",
                "The materialization generation sidecar is structurally incomplete.",
                json!({}),
            )
        })?;
    if generation_fingerprint(&generation).as_deref() != Ok(expected) {
        return Err(refusal(
            "materialization_generation_stale",
            "The materialization generation fingerprint does not match its contents.",
            generation_context(&generation),
        ));
    }
    if generation.get("contract_version").and_then(Value::as_u64) != Some(CONTRACT_VERSION.into()) {
        return Err(refusal(
            "materialization_generation_stale",
            "The materialization contract version is obsolete.",
            generation_context(&generation),
        ));
    }
    preflight_generation_bundle(&generation, sidecar)?;
    let trusted_contract_entrypoint = options.registrar_entrypoint.as_ref().ok_or_else(|| {
        refusal(
            "materialization_generation_stale",
            "The trusted materialization contract authority is absent from the carrier launch.",
            generation_context(&generation),
        )
    })?;
    let generation_contract_entrypoint = generation
        .get("materialization_contract_entrypoint")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            refusal(
                "materialization_generation_stale",
                "The materialization generation sidecar has no contract authority.",
                generation_context(&generation),
            )
        })?;
    if !same_path(
        generation_contract_entrypoint,
        &trusted_contract_entrypoint.to_string_lossy(),
    ) {
        return Err(refusal(
            "materialization_generation_stale",
            "The materialization generation references a different contract authority than the carrier launch.",
            generation_context(&generation),
        ));
    }
    if sha256_file(trusted_contract_entrypoint).as_deref()
        != generation
            .get("materialization_contract_fingerprint")
            .and_then(Value::as_str)
    {
        return Err(refusal(
            "materialization_generation_stale",
            "The materialization contract authority changed after generation.",
            generation_context(&generation),
        ));
    }
    let manifest_path = options
        .artifact_manifest
        .as_ref()
        .map(|path| normalized_path(path))
        .unwrap_or_default();
    if generation
        .get("artifact_manifest_path")
        .and_then(Value::as_str)
        .map(normalize_text_path)
        .as_deref()
        != Some(&manifest_path)
        || generation
            .get("artifact_manifest_fingerprint")
            .and_then(Value::as_str)
            != manifest_fingerprint
    {
        return Err(refusal(
            "materialization_generation_stale",
            "The materialization generation references a different workspace artifact manifest.",
            generation_context(&generation),
        ));
    }
    let registrar = generation
        .get("registrar_entrypoint")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            refusal(
                "materialization_generation_stale",
                "The materialization generation sidecar is structurally incomplete.",
                generation_context(&generation),
            )
        })?;
    if sha256_file(Path::new(registrar)).as_deref()
        != generation
            .get("registrar_fingerprint")
            .and_then(Value::as_str)
    {
        return Err(refusal(
            "materialization_generation_stale",
            "The registrar build changed after configuration generation.",
            generation_context(&generation),
        ));
    }
    let proxy_entrypoint = generation
        .get("proxy_entrypoint")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            refusal(
                "materialization_generation_stale",
                "The materialization generation sidecar is structurally incomplete.",
                generation_context(&generation),
            )
        })?;
    let current_proxy_entrypoint = std::env::current_exe().map_err(|error| {
        refusal(
            "materialization_generation_stale",
            "The current runtime proxy identity cannot be resolved.",
            json!({"generation":generation_context(&generation),"error":error.to_string()}),
        )
    })?;
    if !same_path(
        proxy_entrypoint,
        &current_proxy_entrypoint.to_string_lossy(),
    ) {
        return Err(refusal(
            "materialization_generation_stale",
            "The materialization generation references a different runtime proxy than the carrier launch.",
            generation_context(&generation),
        ));
    }
    if sha256_file(Path::new(proxy_entrypoint)).as_deref()
        != generation.get("proxy_fingerprint").and_then(Value::as_str)
    {
        return Err(refusal(
            "materialization_generation_stale",
            "The selected runtime proxy changed after configuration generation.",
            generation_context(&generation),
        ));
    }
    let config_path = generation
        .get("config_path")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            refusal(
                "materialization_generation_stale",
                "The materialization generation sidecar is structurally incomplete.",
                generation_context(&generation),
            )
        })?;
    let expected_config = sidecar
        .to_string_lossy()
        .strip_suffix(".narada-generation.json")
        .map(str::to_string)
        .unwrap_or_default();
    if !same_path(config_path, &expected_config) {
        return Err(refusal(
            "materialization_generation_stale",
            "The materialization generation sidecar is not paired with its carrier configuration.",
            generation_context(&generation),
        ));
    }
    let plan_path = generation
        .get("runtime_materialization_plan_path")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            refusal(
                "materialization_generation_stale",
                "The materialization generation sidecar is structurally incomplete.",
                generation_context(&generation),
            )
        })?;
    let expected_plan = format!("{expected_config}.narada-runtime-plan.json");
    if !same_path(plan_path, &expected_plan) {
        return Err(refusal(
            "materialization_generation_stale",
            "The materialization generation sidecar is not paired with its runtime materialization plan.",
            generation_context(&generation),
        ));
    }
    let plan = read_json(Path::new(plan_path)).map_err(|error| {
        refusal(
            "materialization_generation_stale",
            "The runtime materialization plan is missing or unreadable.",
            json!({ "error": error, "runtime_materialization_plan_path": plan_path }),
        )
    })?;
    let expected_plan_fingerprint = generation
        .get("runtime_materialization_plan_fingerprint")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            refusal(
                "materialization_generation_stale",
                "The materialization generation sidecar is structurally incomplete.",
                generation_context(&generation),
            )
        })?;
    if plan.get("schema").and_then(Value::as_str) != Some("narada.runtime_materialization_plan.v1")
        || plan.get("status").and_then(Value::as_str) != Some("accepted")
        || plan.get("runtime_profile_kind").and_then(Value::as_str)
            != generation
                .get("runtime_profile_kind")
                .and_then(Value::as_str)
        || plan.get("plan_fingerprint").and_then(Value::as_str) != Some(expected_plan_fingerprint)
        || generation
            .get("launch_catalog_fingerprint")
            .and_then(Value::as_str)
            != Some(expected_plan_fingerprint)
        || runtime_plan_fingerprint(&plan).as_deref() != Some(expected_plan_fingerprint)
    {
        return Err(refusal(
            "materialization_generation_stale",
            "The runtime materialization plan changed after generation.",
            generation_context(&generation),
        ));
    }
    let expected_matrix_fingerprint = generation
        .get("runtime_implementation_matrix_fingerprint")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            refusal(
                "materialization_generation_stale",
                "The materialization generation sidecar is structurally incomplete.",
                generation_context(&generation),
            )
        })?;
    let matrix_path = generation
        .get("runtime_implementation_matrix_path")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            refusal(
                "materialization_generation_stale",
                "The materialization generation sidecar is structurally incomplete.",
                generation_context(&generation),
            )
        })?;
    let plan_matrix_fingerprint = plan
        .get("source")
        .and_then(|source| source.get("matrix_fingerprint"))
        .and_then(Value::as_str);
    if plan_matrix_fingerprint != Some(expected_matrix_fingerprint)
        || sha256_file(Path::new(matrix_path)).as_deref() != Some(expected_matrix_fingerprint)
    {
        return Err(refusal(
            "materialization_generation_stale",
            "The runtime implementation matrix changed after generation.",
            generation_context(&generation),
        ));
    }
    let config = fs::read_to_string(config_path).map_err(|_| {
        refusal(
            "materialization_generation_stale",
            "The materialized configuration changed after generation.",
            generation_context(&generation),
        )
    })?;
    let kind = generation
        .get("carrier_kind")
        .and_then(Value::as_str)
        .unwrap_or("");
    let selectors = generation
        .pointer("/managed_projection/selectors")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            refusal(
                "materialization_generation_stale",
                "The materialization generation sidecar has no managed selector set.",
                generation_context(&generation),
            )
        })?
        .iter()
        .map(|value| {
            value.as_str().map(str::to_string).ok_or_else(|| {
                refusal(
                    "materialization_generation_stale",
                    "The materialization generation sidecar has an invalid managed selector.",
                    generation_context(&generation),
                )
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    let current = describe_config(kind, config.as_bytes(), &selectors).map_err(|error| {
        refusal(
            "materialization_managed_projection_stale",
            "The Narada-managed configuration projection cannot be read.",
            json!({"generation":generation_context(&generation),"error":error}),
        )
    })?;
    if current.managed_projection.sha256
        != generation
            .pointer("/managed_projection/sha256")
            .and_then(Value::as_str)
            .unwrap_or("")
    {
        return Err(refusal(
            "materialization_managed_projection_stale",
            "The Narada-managed configuration projection changed after generation.",
            generation_context(&generation),
        ));
    }
    let expected_bytes = generation
        .pointer("/config_artifact/bytes_sha256")
        .and_then(Value::as_str)
        .unwrap_or("");
    if current.config_artifact.bytes_sha256 != expected_bytes {
        eprintln!(
            "{}",
            json!({
                "schema":"narada.mcp_runtime_proxy.observation.v1",
                "code":"materialization_artifact_bytes_drift",
                "config_path":config_path,
                "expected_bytes_sha256":expected_bytes,
                "actual_bytes_sha256":current.config_artifact.bytes_sha256,
                "managed_projection_unchanged":true
            })
        );
    }
    Ok(())
}
