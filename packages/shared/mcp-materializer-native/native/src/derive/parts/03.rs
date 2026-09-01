fn stable_digest(value: &Value) -> String {
    canonical_json_sha256(value).expect("JSON values must canonicalize")
}

fn registry_site_root(registry_path: &Path) -> Result<PathBuf, Failure> {
    let capabilities = registry_path.parent().ok_or_else(|| {
        Failure::new(
            "materializer_registry_site_root_unresolved",
            path_text(registry_path),
        )
    })?;
    let narada = capabilities.parent().ok_or_else(|| {
        Failure::new(
            "materializer_registry_site_root_unresolved",
            path_text(registry_path),
        )
    })?;
    let site_root = narada.parent().ok_or_else(|| {
        Failure::new(
            "materializer_registry_site_root_unresolved",
            path_text(registry_path),
        )
    })?;
    if capabilities.file_name().and_then(|value| value.to_str()) != Some("capabilities")
        || narada.file_name().and_then(|value| value.to_str()) != Some(".narada")
    {
        return Err(Failure::new(
            "materializer_registry_site_root_unresolved",
            path_text(registry_path),
        ));
    }
    Ok(site_root.to_path_buf())
}

fn binding_admission_entry_digest_v1(entry: &Value) -> String {
    let mut unsigned = entry.clone();
    let object = unsigned
        .as_object_mut()
        .expect("binding admission entry must be an object");
    object.remove("binding_digest");
    let identity = object
        .remove("binding_identity")
        .expect("binding admission entry must carry binding_identity");
    object.insert("launch_identity".to_string(), identity);
    stable_digest(&unsigned)
}
fn ambient_binding_entry(
    site_id: &str,
    surface: &Value,
    admit_local: bool,
) -> Result<Option<Value>, Failure> {
    let injection_scope = surface
        .get("injection_scope")
        .and_then(Value::as_str)
        .or_else(|| {
            surface
                .get("narada_scope")
                .and_then(|value| value.get("injection_scope"))
                .and_then(Value::as_str)
        })
        .or_else(|| {
            surface
                .get("surface_projection")
                .and_then(|value| value.get("injection_scope"))
                .and_then(Value::as_str)
        })
        .filter(|value| !value.trim().is_empty())
        .map(str::to_string)
        .ok_or_else(|| Failure::new("materializer_registry_field_required", "injection_scope"))?;
    if injection_scope == "local_site" && !admit_local {
        return Ok(None);
    }
    if injection_scope != "host"
        && injection_scope != "user_site"
        && injection_scope != "local_site"
    {
        return Err(Failure::new(
            "materializer_injection_scope_unsupported",
            injection_scope,
        ));
    }
    let surface_id = required_string(surface, "catalog_surface_id")?;
    let binding_id = surface
        .get("binding_id")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .unwrap_or_else(|| format!("{site_id}-{surface_id}"));
    let projection = surface
        .get("surface_projection")
        .cloned()
        .unwrap_or(Value::Null);
    let projection_id = projection
        .get("projection_id")
        .and_then(Value::as_str)
        .unwrap_or("default")
        .to_string();
    let authority_locus = surface
        .get("authority_locus")
        .or_else(|| {
            surface
                .get("narada_scope")
                .and_then(|value| value.get("authority_locus"))
        })
        .cloned()
        .unwrap_or_else(|| match injection_scope.as_str() {
            "host" => json!({"kind": "host"}),
            "user_site" => json!({"kind": "user_site", "site_id": site_id}),
            "local_site" => json!({"kind": "local_site", "site_id": site_id}),
            _ => Value::Null,
        });
    if authority_locus.is_null() {
        return Err(Failure::new(
            "materializer_authority_locus_required",
            surface_id,
        ));
    }
    let runtime_binding = surface
        .get("runtime_binding")
        .ok_or_else(|| Failure::new("materializer_runtime_binding_required", surface_id.clone()))?;
    let transport = runtime_binding
        .get("transport")
        .ok_or_else(|| Failure::new("materializer_transport_required", surface_id.clone()))?;
    if transport.get("type").and_then(Value::as_str) != Some("stdio") {
        return Err(Failure::new(
            "materializer_transport_unsupported",
            surface_id,
        ));
    }
    let command = required_string(transport, "command")?;
    let args = string_array(transport, "args")?;
    let descriptor = projection.get("surface_descriptor").unwrap_or(&Value::Null);
    let env_vars = descriptor
        .get("projections")
        .and_then(Value::as_array)
        .and_then(|items| {
            items
                .iter()
                .find(|item| item.get("id").and_then(Value::as_str) == Some(projection_id.as_str()))
        })
        .and_then(|item| item.get("transport"))
        .map(|transport| string_array(transport, "env"))
        .transpose()?
        .unwrap_or_default();
    let target_site_root = authority_locus
        .get("site_root")
        .cloned()
        .unwrap_or(Value::Null);
    let binding_identity = json!({
        "schema": "narada.mcp.binding_identity.v1",
        "binding_id": binding_id.clone(),
        "surface_id": surface_id.clone(),
        "projection_id": projection_id.clone(),
        "injection_scope": injection_scope.clone(),
        "authority_locus": authority_locus,
        "transport": "stdio",
        "command": command,
        "args": args,
        "env": {},
        "env_vars": env_vars,
        "target_site_root": target_site_root,
        "surface_projection": projection,
    });
    let mut entry = json!({
        "binding_id": binding_id,
        "surface_id": surface_id,
        "projection_id": projection_id,
        "authority_locus": binding_identity["authority_locus"].clone(),
        "injection_scope": injection_scope,
        "operations": ["discover", "attach", "restart"],
        "binding_identity": binding_identity,
        "binding_digest": "",
    });
    let digest = binding_admission_entry_digest_v1(&entry);
    entry["binding_digest"] = Value::String(digest);
    Ok(Some(entry))
}

pub(crate) fn options_from_generation(path: &Path) -> Result<DeriveOptions, Failure> {
    require_absolute(path, "generation")?;
    let bytes = read_required(path, "materializer_generation_read_failed")?;
    let generation: Value = serde_json::from_slice(&bytes)
        .map_err(|error| Failure::new("materializer_generation_invalid", error.to_string()))?;
    if !matches!(
        generation.get("schema").and_then(Value::as_str),
        Some("narada.mcp_materialization_generation.v1")
            | Some("narada.mcp_materialization_generation.v2")
            | Some("narada.mcp_materialization_generation.v3")
    ) {
        return Err(Failure::new(
            "materializer_generation_schema_unsupported",
            path_text(path),
        ));
    }
    let config_path = PathBuf::from(required_string(&generation, "config_path")?);
    let carrier_kind = required_string(&generation, "carrier_kind")?;
    let artifact_manifest = PathBuf::from(required_string(&generation, "artifact_manifest_path")?);
    let matrix = PathBuf::from(required_string(
        &generation,
        "runtime_implementation_matrix_path",
    )?);
    let workspace_root = artifact_manifest
        .parent()
        .and_then(Path::parent)
        .and_then(Path::parent)
        .map(Path::to_path_buf)
        .ok_or_else(|| {
            Failure::new(
                "materializer_workspace_root_unresolved",
                path_text(&artifact_manifest),
            )
        })?;
    let plan_path = PathBuf::from(required_string(
        &generation,
        "runtime_materialization_plan_path",
    )?);
    let plan_bytes = read_required(&plan_path, "materializer_runtime_plan_read_failed")?;
    let plan: Value = serde_json::from_slice(&plan_bytes)
        .map_err(|error| Failure::new("materializer_runtime_plan_invalid", error.to_string()))?;
    let source = plan.get("source").ok_or_else(|| {
        Failure::new(
            "materializer_runtime_plan_source_missing",
            path_text(&plan_path),
        )
    })?;
    let contract_path = PathBuf::from(required_string(source, "carrier_contract_path")?);
    require_absolute(&contract_path, "contract")?;
    let contract_bytes =
        read_required(&contract_path, "materializer_carrier_contract_read_failed")?;
    let expected_contract_fingerprint = required_string(source, "carrier_contract_fingerprint")?;
    if sha256(&contract_bytes) != expected_contract_fingerprint {
        return Err(Failure::new(
            "materializer_carrier_contract_fingerprint_mismatch",
            path_text(&contract_path),
        ));
    }
    let contract: Contract = serde_json::from_slice(&contract_bytes).map_err(|error| {
        Failure::new("materializer_carrier_contract_invalid", error.to_string())
    })?;
    if contract.schema != "narada.native_carrier_contract.v2" {
        return Err(Failure::new(
            "materializer_carrier_contract_schema_unsupported",
            contract.schema,
        ));
    }
    let declared = contract
        .carriers
        .iter()
        .find(|carrier| match carrier.carrier_kind {
            CarrierKind::Codex => carrier_kind == "codex",
            CarrierKind::Kimi => carrier_kind == "kimi",
            CarrierKind::Opencode => carrier_kind == "opencode",
            CarrierKind::Pi => carrier_kind == "pi",
        })
        .ok_or_else(|| Failure::new("materializer_carrier_kind_unsupported", carrier_kind))?;
    let relative = PathBuf::from(&declared.config_relative_path);
    if !config_path.ends_with(&relative) {
        return Err(Failure::new(
            "materializer_config_path_contract_mismatch",
            path_text(&config_path),
        ));
    }
    let mut home = config_path.clone();
    for _ in relative.components() {
        home.pop();
    }
    Ok(DeriveOptions {
        contract: contract_path,
        workspace_root,
        matrix,
        installed_index: home.join(".narada/carriers/installed-carriers.json"),
        home,
    })
}

