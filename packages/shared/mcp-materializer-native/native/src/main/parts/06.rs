fn materialize(
    input: MaterializationInput,
    require_fresh_validation: bool,
) -> Result<Value, Failure> {
    validate_input(&input)?;
    verify_artifact_build_set(&input)?;
    let generated_at = OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .map_err(|error| Failure::new("materializer_clock_failed", error.to_string()))?;
    let materialization_input_digest =
        canonical_json_sha256(&serde_json::to_value(&input).map_err(json_failure)?)
            .map_err(|error| Failure::new("materializer_input_fingerprint_failed", error))?;
    let validation_policy_identity =
        sha256(b"narada.fresh_process_validation.v1:initialize+tools/list+deterministic_readiness");
    let carrier_root = input
        .installed_carrier_index_path
        .parent()
        .ok_or_else(|| {
            Failure::new(
                "materializer_carrier_root_unresolved",
                path_text(&input.installed_carrier_index_path),
            )
        })?
        .to_path_buf();
    let bundle_commit_pointer_path = carrier_root.join("current-bundle.json");
    let bundle_carriers = input
        .carriers
        .iter()
        .map(|carrier| {
            let sidecar_path = suffix_path(&carrier.config_path, ".narada-generation.json");
            json!({
                "carrier_id": carrier.carrier_id,
                "carrier_kind": carrier.carrier_kind,
                "config_path": path_text(&carrier.config_path),
                "generation_sidecar_path": path_text(&sidecar_path),
            })
        })
        .collect::<Vec<_>>();
    let bundle_unsigned = json!({
        "schema": "narada.carrier_generation_bundle.v1",
        "consistency_domain": "selected_carrier_bundle",
        "materialization_input_digest": materialization_input_digest,
        "artifact_build_set_path": path_text(&input.artifact_build_set_path),
        "artifact_build_set_fingerprint": input.artifact_build_set_fingerprint,
        "artifact_manifest_path": path_text(&input.artifact_manifest_path),
        "artifact_manifest_fingerprint": input.artifact_manifest_fingerprint,
        "validation_policy_identity": validation_policy_identity,
        "carriers": bundle_carriers,
    });
    let bundle_fingerprint = canonical_json_sha256(&bundle_unsigned)
        .map_err(|error| Failure::new("materializer_bundle_fingerprint_failed", error))?;
    let bundle_id = bundle_fingerprint.clone();
    let bundle_path = carrier_root
        .join("bundles")
        .join(&bundle_id)
        .join("bundle.json");
    let mut bundle = bundle_unsigned;
    bundle
        .as_object_mut()
        .expect("bundle is an object")
        .insert("bundle_id".to_string(), Value::String(bundle_id.clone()));
    bundle.as_object_mut().expect("bundle is an object").insert(
        "bundle_fingerprint".to_string(),
        Value::String(bundle_fingerprint.clone()),
    );
    let migration_provenance = if input.carriers.iter().any(|carrier| {
        let sidecar = suffix_path(&carrier.config_path, ".narada-generation.json");
        read_json(&sidecar, "materializer_generation_invalid")
            .ok()
            .and_then(|value| {
                value
                    .get("schema")
                    .and_then(Value::as_str)
                    .map(str::to_string)
            })
            .is_some_and(|schema| {
                schema == LEGACY_GENERATION_SCHEMA || schema == AMBIGUOUS_GENERATION_SCHEMA
            })
    }) {
        "legacy_baseline_untrusted"
    } else {
        "native_v3"
    };
    let mut publications = Vec::new();
    let mut index_carriers = Vec::new();
    for carrier in &input.carriers {
        let plan_path = suffix_path(&carrier.config_path, ".narada-runtime-plan.json");
        let sidecar_path = suffix_path(&carrier.config_path, ".narada-generation.json");
        let desired = emit_carrier(carrier)?;
        let selectors = if matches!(carrier.carrier_kind, CarrierKind::Codex) {
            codex_managed_selectors(
                carrier.servers.iter().map(|server| &server.name),
                carrier.codex_plugin_overrides.keys(),
                carrier.trust_projects.iter(),
            )
        } else {
            vec![]
        };
        let config = if matches!(carrier.carrier_kind, CarrierKind::Codex) {
            let previous_selectors = previous_managed_selectors(&sidecar_path)?;
            let existing = fs::read(&carrier.config_path).ok();
            merge_codex_configuration(
                existing.as_deref(),
                &desired,
                &previous_selectors,
                &selectors,
            )
            .map_err(|error| {
                Failure::new("materializer_codex_merge_failed", error).with_details(json!({
                    "config_path": path_text(&carrier.config_path),
                    "mutation_status": "not_started",
                }))
            })?
        } else {
            desired
        };
        let carrier_kind = match carrier.carrier_kind {
            CarrierKind::Codex => "codex",
            CarrierKind::Kimi => "kimi",
            CarrierKind::Opencode => "opencode",
            CarrierKind::Pi => "pi",
        };
        let description = describe_config(carrier_kind, &config, &selectors)
            .map_err(|error| Failure::new("materializer_contract_describe_failed", error))?;
        let plan_unsigned = json!({
            "schema": "narada.runtime_materialization_plan.v1",
            "status": "accepted",
            "runtime_profile_kind": input.runtime_profile_kind,
            "source": {
                "authority": "narada.runtime_implementation_matrix",
                "matrix_fingerprint": input.runtime_implementation_matrix_fingerprint,
                "carrier_contract_path": path_text(&input.carrier_contract_path),
                "carrier_contract_fingerprint": input.carrier_contract_fingerprint,
                "artifact_build_set_path": path_text(&input.artifact_build_set_path),
                "artifact_build_set_fingerprint": input.artifact_build_set_fingerprint,
            },
            "carrier_id": carrier.carrier_id,
            "servers": carrier.servers.iter().map(|server| json!({
                "binding_id":server.binding_id.as_deref().unwrap_or(&server.name),
                "name":server.name,
                "source_server_key":server.source_server_key,
                "command":server.command,
                "args":server.args,
            })).collect::<Vec<_>>(),
        });
        let plan_hash = sha256(&serde_json::to_vec(&plan_unsigned).map_err(json_failure)?);
        let mut plan = plan_unsigned;
        plan.as_object_mut().expect("plan is an object").insert(
            "plan_fingerprint".to_string(),
            Value::String(plan_hash.clone()),
        );
        let mut generation = Generation {
            schema: GENERATION_SCHEMA,
            contract_version: CONTRACT_VERSION,
            carrier_id: carrier.carrier_id.clone(),
            carrier_kind: carrier.carrier_kind,
            config_path: path_text(&carrier.config_path),
            config_artifact: description.config_artifact,
            managed_projection: description.managed_projection,
            materialization_contract_entrypoint: path_text(&input.registrar_entrypoint),
            materialization_contract_fingerprint: input.registrar_fingerprint.clone(),
            artifact_manifest_path: path_text(&input.artifact_manifest_path),
            artifact_manifest_fingerprint: input.artifact_manifest_fingerprint.clone(),
            artifact_build_set_path: path_text(&input.artifact_build_set_path),
            artifact_build_set_fingerprint: input.artifact_build_set_fingerprint.clone(),
            materialization_input_digest: materialization_input_digest.clone(),
            bundle_id: bundle_id.clone(),
            bundle_path: path_text(&bundle_path),
            bundle_commit_pointer_path: path_text(&bundle_commit_pointer_path),
            bundle_fingerprint: bundle_fingerprint.clone(),
            launch_catalog_fingerprint: plan_hash.clone(),
            validation_policy_identity: validation_policy_identity.clone(),
            migration_provenance: migration_provenance.to_string(),
            runtime_profile_kind: input.runtime_profile_kind.clone(),
            runtime_materialization_plan_path: path_text(&plan_path),
            runtime_materialization_plan_fingerprint: plan_hash,
            runtime_implementation_matrix_path: path_text(
                &input.runtime_implementation_matrix_path,
            ),
            runtime_implementation_matrix_fingerprint: input
                .runtime_implementation_matrix_fingerprint
                .clone(),
            registrar_entrypoint: path_text(&input.registrar_entrypoint),
            registrar_fingerprint: input.registrar_fingerprint.clone(),
            proxy_implementation: input.proxy_implementation.clone(),
            proxy_entrypoint: path_text(&input.proxy_entrypoint),
            proxy_fingerprint: input.proxy_fingerprint.clone(),
            server_count: carrier.servers.len(),
            proxy_count: carrier.servers.len(),
            generated_at: generated_at.clone(),
            generation_fingerprint: String::new(),
        };
        let mut unsigned_generation = serde_json::to_value(&generation).map_err(json_failure)?;
        unsigned_generation
            .as_object_mut()
            .expect("generation is an object")
            .remove("generation_fingerprint");
        let generation_fingerprint = generation_fingerprint(&unsigned_generation)
            .map_err(|error| Failure::new("materializer_generation_fingerprint_failed", error))?;
        generation.generation_fingerprint = generation_fingerprint.clone();
        publications.push(Publication {
            path: carrier.config_path.clone(),
            content: config,
        });
        publications.push(Publication {
            path: plan_path,
            content: pretty_json(&plan)?,
        });
        publications.push(Publication {
            path: sidecar_path.clone(),
            content: pretty_json(&serde_json::to_value(&generation).map_err(json_failure)?)?,
        });
        match (
            &carrier.binding_admission_path,
            &carrier.binding_admission_envelope,
        ) {
            (Some(path), Some(envelope)) => publications.push(Publication {
                path: path.clone(),
                content: pretty_json(envelope)?,
            }),
            (None, None) => {}
            _ => {
                return Err(Failure::new(
                    "materializer_binding_admission_incomplete",
                    carrier.carrier_id.clone(),
                ))
            }
        }
        index_carriers.push(json!({
            "carrier_id": carrier.carrier_id,
            "carrier_kind": carrier.carrier_kind,
            "config_path": path_text(&carrier.config_path),
            "generation_sidecar_path": path_text(&sidecar_path),
            "materialization_generation_fingerprint": generation_fingerprint,
            "bundle_id": bundle_id,
        }));
    }
    publications.push(Publication {
        path: bundle_path.clone(),
        content: pretty_json(&bundle)?,
    });
    publications.push(Publication {
        path: input.installed_carrier_index_path.clone(),
        content: pretty_json(&json!({
            "schema": "narada.installed_carrier_index.v1",
            "workspace_root": path_text(&input.workspace_root),
            "carrier_contract_path": path_text(&input.carrier_contract_path),
            "carrier_contract_fingerprint": input.carrier_contract_fingerprint,
            "artifact_manifest_path": path_text(&input.artifact_manifest_path),
            "artifact_build_set_path": path_text(&input.artifact_build_set_path),
            "artifact_build_set_fingerprint": input.artifact_build_set_fingerprint,
            "bundle_id": bundle_id,
            "bundle_path": path_text(&bundle_path),
            "bundle_fingerprint": bundle_fingerprint,
            "bundle_commit_pointer_path": path_text(&bundle_commit_pointer_path),
            "carriers": index_carriers,
        }))?,
    });
    if require_fresh_validation {
        fresh_process_validate(&input)?;
    }
    let commit_pointer = json!({
        "schema": "narada.carrier_generation_bundle_pointer.v1",
        "bundle_id": bundle_id,
        "bundle_path": path_text(&bundle_path),
        "bundle_fingerprint": bundle_fingerprint,
        "committed_at": generated_at,
    });
    publications.push(Publication {
        path: bundle_commit_pointer_path.clone(),
        content: pretty_json(&commit_pointer)?,
    });
    let transaction = durable_bundle_publish(
        &publications,
        &carrier_root,
        &bundle_commit_pointer_path,
        &bundle_id,
    )?;
    Ok(json!({
        "schema": "narada.materialization_operation_result.v1",
        "status": "committed",
        "bundle_id": bundle_id,
        "bundle_path": path_text(&bundle_path),
        "bundle_commit_pointer_path": path_text(&bundle_commit_pointer_path),
        "carrier_count": input.carriers.len(),
        "installed_carrier_index_path": path_text(&input.installed_carrier_index_path),
        "transaction": transaction,
        "restart_required": true,
        "restart_scope": "selected_carrier_bundle",
    }))
}

