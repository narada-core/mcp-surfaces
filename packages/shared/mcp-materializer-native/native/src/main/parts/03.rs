fn acknowledge_restart(
    installed_index_path: &Path,
    carrier_id: &str,
    expected_evidence_ref: &str,
) -> Result<Value, Failure> {
    validate_identifier(carrier_id, "carrier_id")?;
    if expected_evidence_ref.trim().is_empty() {
        return Err(Failure::new(
            "materializer_expected_evidence_ref_required",
            "Expected evidence reference must not be empty.",
        ));
    }
    let index = read_json(installed_index_path, "materializer_installed_index_invalid")?;
    if index.get("schema").and_then(Value::as_str) != Some("narada.installed_carrier_index.v1") {
        return Err(Failure::new(
            "materializer_installed_index_schema_unsupported",
            path_text(installed_index_path),
        ));
    }
    let workspace_root = PathBuf::from(json_field_string(&index, "workspace_root")?);
    let pressure_path = workspace_root.join(".ai/runtime/carrier-restart-pressure.json");
    let mut pressure = if pressure_path.exists() {
        read_json(&pressure_path, "materializer_restart_pressure_invalid")?
    } else {
        json!({
            "schema": "narada.carrier_restart_pressure.v1",
            "carriers": {},
        })
    };
    if pressure.get("schema").and_then(Value::as_str) != Some("narada.carrier_restart_pressure.v1")
    {
        return Err(Failure::new(
            "materializer_restart_pressure_schema_unsupported",
            path_text(&pressure_path),
        ));
    }
    let carriers = pressure
        .get_mut("carriers")
        .and_then(Value::as_object_mut)
        .ok_or_else(|| {
            Failure::new(
                "materializer_restart_pressure_carriers_required",
                path_text(&pressure_path),
            )
        })?;
    let current = carriers.get(carrier_id).cloned();
    let current_ref = current
        .as_ref()
        .and_then(|value| value.get("evidence_ref"))
        .and_then(Value::as_str);
    if current.is_some() && current_ref != Some(expected_evidence_ref) {
        return Ok(json!({
            "schema": "narada.carrier_restart_acknowledgement.v1",
            "status": "stale_ack_refused",
            "carrier_id": carrier_id,
            "expected_pressure_ref": expected_evidence_ref,
            "current_pressure": current,
            "remaining_carrier_ids": carriers.keys().cloned().collect::<Vec<_>>(),
        }));
    }
    let acknowledged = carriers.remove(carrier_id);
    let updated_at = OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .map_err(|error| Failure::new("materializer_clock_failed", error.to_string()))?;
    pressure
        .as_object_mut()
        .expect("pressure is an object")
        .insert("updated_at".to_string(), Value::String(updated_at));
    atomic_write(&pressure_path, &pretty_json(&pressure)?).map_err(|error| {
        Failure::new(
            "materializer_restart_pressure_publish_failed",
            error.to_string(),
        )
        .with_details(json!({"path": path_text(&pressure_path)}))
    })?;
    let remaining = pressure
        .get("carriers")
        .and_then(Value::as_object)
        .map(|value| value.keys().cloned().collect::<Vec<_>>())
        .unwrap_or_default();
    Ok(json!({
        "schema": "narada.carrier_restart_acknowledgement.v1",
        "status": if acknowledged.is_some() { "acknowledged" } else { "already_current" },
        "carrier_id": carrier_id,
        "acknowledged_pressure": acknowledged,
        "remaining_carrier_ids": remaining,
        "restart_pressure_path": path_text(&pressure_path),
    }))
}

fn verify_all(index_path: &Path) -> Result<Value, Failure> {
    let index = read_json(index_path, "materializer_installed_index_invalid")?;
    if index.get("schema").and_then(Value::as_str) != Some("narada.installed_carrier_index.v1") {
        return Err(Failure::new(
            "materializer_installed_index_schema_unsupported",
            path_text(index_path),
        ));
    }
    let carriers = index
        .get("carriers")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            Failure::new(
                "materializer_installed_index_carriers_required",
                path_text(index_path),
            )
        })?;
    if carriers.is_empty() {
        return Err(Failure::new(
            "materializer_installed_index_empty",
            path_text(index_path),
        ));
    }
    let mut verified = Vec::new();
    for carrier in carriers {
        let carrier_id = json_field_string(carrier, "carrier_id")?;
        let sidecar_path = PathBuf::from(json_field_string(carrier, "generation_sidecar_path")?);
        let generation = read_json(&sidecar_path, "materializer_generation_invalid")?;
        verify_generation(
            &generation,
            &sidecar_path,
            carrier
                .get("materialization_generation_fingerprint")
                .and_then(Value::as_str),
        )?;
        verified.push(carrier_id.to_string());
    }
    Ok(json!({
        "schema": "narada.mcp_materializer.verification.v1",
        "status": "current",
        "installed_carrier_index_path": path_text(index_path),
        "verified_carrier_ids": verified,
        "verified_carrier_count": verified.len(),
    }))
}

