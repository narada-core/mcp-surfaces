fn site_registry_conformance_check(contract: &Value, args: &Value) -> Result<Value, String> {
    let requested = required_argument(args, "site_id", "registrar_requires_site_id")?;
    let observation_ref = required_argument(
        args,
        "observation_ref",
        "registrar_requires_observation_ref",
    )?;
    let include_ok = args.get("include_ok").and_then(Value::as_bool) == Some(true);
    let sites = site_catalog(contract)["items"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let site = lookup_site_value(&sites, &requested)?;
    let root = PathBuf::from(site["root"].as_str().unwrap_or(""));
    let registry_path = capability_registry_path(&root);
    if !registry_path.exists() {
        return Err(format!(
            "registrar_site_surface_registry_not_found:{}",
            path_text(&registry_path)
        ));
    }
    let registry: Value = serde_json::from_str(
        &fs::read_to_string(&registry_path)
            .map_err(|error| format!("registrar_site_surface_registry_parse_failed:{error}"))?,
    )
    .map_err(|error| format!("registrar_site_surface_registry_parse_failed:{error}"))?;
    let shown = read_payload_revision(&root, &observation_ref)?;
    if shown["created_by"] != "mcp-loader-mcp"
        || !shown["payload_id"]
            .as_str()
            .unwrap_or("")
            .starts_with("site-tools-")
    {
        return Err("registrar_inventory_observation_lineage_mismatch".into());
    }
    let observation = &shown["payload"];
    if observation["schema"] != "narada.mcp_loader.site_tool_inventory_check.v1" {
        return Err("registrar_inventory_observation_schema_mismatch".into());
    }
    if comparable_path(observation["site_root"].as_str().unwrap_or(""))
        != comparable_path(site["root"].as_str().unwrap_or(""))
    {
        return Err("registrar_inventory_observation_site_mismatch".into());
    }
    for field in [
        "observed_tools",
        "observed_read_only_tools",
        "observed_mutating_tools",
    ] {
        if !observation[field].is_object() {
            return Err(format!(
                "registrar_inventory_observation_field_missing:{field}"
            ));
        }
    }
    let mut result = check_registry_conformance(
        contract,
        &site,
        &registry,
        &observation["observed_tools"],
        &observation["observed_read_only_tools"],
        &observation["observed_mutating_tools"],
        include_ok,
    )?;
    let object = result.as_object_mut().unwrap();
    object.insert("observation_ref".into(), json!(observation_ref));
    object.insert("observation_sha256".into(), shown["sha256"].clone());
    object.insert("observation_created_at".into(), shown["created_at"].clone());
    object.insert("observation_status".into(), observation["status"].clone());
    object.insert(
        "observation_observed_at".into(),
        observation["observed_at"].clone(),
    );
    object.insert(
        "observation_lineage".into(),
        json!({"declared_creator":shown["created_by"],"payload_id":shown["payload_id"],"assurance":"declarative_lineage_guard_not_cryptographic_provenance","authority_effect":"none"}),
    );
    Ok(result)
}

fn read_payload_revision(root: &Path, reference: &str) -> Result<Value, String> {
    let Some(rest) = reference.strip_prefix("mcp_payload:") else {
        return Err(format!("payload_ref_invalid: {reference}"));
    };
    let Some((payload_id, revision_text)) = rest.rsplit_once("@v") else {
        return Err(format!("payload_ref_invalid: {reference}"));
    };
    let revision = revision_text
        .parse::<u64>()
        .map_err(|_| format!("payload_ref_invalid: {reference}"))?;
    let path = root
        .join(".ai/tmp/mcp-payloads/workspace")
        .join(payload_id)
        .join(format!("v{revision}.json"));
    let content =
        fs::read_to_string(&path).map_err(|_| format!("payload_ref_not_found: {reference}"))?;
    let record: Value = serde_json::from_str(&content)
        .map_err(|error| format!("payload_ref_invalid_json: {error}"))?;
    if record["schema"] != "narada.mcp_payload.revision.v1" {
        return Err(format!(
            "payload_ref_schema_unsupported: {}",
            record["schema"].as_str().unwrap_or("")
        ));
    }
    if record["ref"] != reference
        || record["payload_id"] != payload_id
        || record["revision"] != revision
    {
        return Err(format!("payload_ref_metadata_mismatch: {reference}"));
    }
    let payload_text = canonical_json(&record["payload"]);
    if record["byte_size"].as_u64() != Some(payload_text.len() as u64) {
        return Err(format!("payload_ref_byte_size_mismatch: {reference}"));
    }
    if record["sha256"] != sha256_text(&payload_text) {
        return Err(format!("payload_ref_sha256_mismatch: {reference}"));
    }
    Ok(record)
}

