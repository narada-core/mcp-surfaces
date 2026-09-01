
fn verify_fingerprint(value: Option<&Value>, code: &str, reason: &str) -> Result<(), Refusal> {
    let value = value.ok_or_else(|| refusal(code, reason, json!({})))?;
    let path = value
        .get("path")
        .and_then(Value::as_str)
        .ok_or_else(|| refusal(code, reason, json!({})))?;
    let expected_hash = value
        .get("sha256")
        .and_then(Value::as_str)
        .ok_or_else(|| refusal(code, reason, json!({ "path": path })))?;
    let expected_size = value
        .get("size")
        .and_then(Value::as_u64)
        .ok_or_else(|| refusal(code, reason, json!({ "path": path })))?;
    let bytes = fs::read(path).map_err(|_| refusal(code, reason, json!({ "path": path })))?;
    if bytes.len() as u64 != expected_size || sha256_bytes(&bytes) != expected_hash {
        return Err(refusal(code, reason, json!({ "path": path })));
    }
    Ok(())
}

fn preflight_generation_bundle(generation: &Value, sidecar: &Path) -> Result<(), Refusal> {
    let stale =
        |reason: &str, details: Value| refusal("materialization_generation_stale", reason, details);
    let required = |field: &'static str| {
        generation
            .get(field)
            .and_then(Value::as_str)
            .ok_or_else(|| {
                stale(
                    "The materialization generation has an incomplete bundle chain.",
                    json!({"field":field,"admission_state":"integrity_refused"}),
                )
            })
    };
    let bundle_id = required("bundle_id")?;
    let expected_fingerprint = required("bundle_fingerprint")?;
    let bundle_path = PathBuf::from(required("bundle_path")?);
    let mut bundle = read_json(&bundle_path).map_err(|error| {
        stale(
            "The committed carrier bundle is missing or unreadable.",
            json!({"error":error,"bundle_path":bundle_path,"admission_state":"integrity_refused"}),
        )
    })?;
    if bundle.get("schema").and_then(Value::as_str) != Some("narada.carrier_generation_bundle.v1")
        || bundle.get("bundle_id").and_then(Value::as_str) != Some(bundle_id)
        || bundle.get("bundle_fingerprint").and_then(Value::as_str) != Some(expected_fingerprint)
    {
        return Err(stale(
            "The carrier bundle identity does not match the generation.",
            json!({"bundle_path":bundle_path,"admission_state":"integrity_refused"}),
        ));
    }
    let object = bundle.as_object_mut().ok_or_else(|| {
        stale(
            "The carrier bundle is not an object.",
            json!({"bundle_path":bundle_path,"admission_state":"integrity_refused"}),
        )
    })?;
    object.remove("bundle_id");
    object.remove("bundle_id");
    object.remove("bundle_fingerprint");
    object.remove("generated_at");
    if canonical_json_sha256(&bundle).as_deref() != Ok(expected_fingerprint) {
        return Err(stale(
            "The carrier bundle fingerprint does not match its contents.",
            json!({"bundle_path":bundle_path,"admission_state":"integrity_refused"}),
        ));
    }
    let carrier_id = required("carrier_id")?;
    let member = bundle
        .get("carriers")
        .and_then(Value::as_array)
        .and_then(|carriers| {
            carriers.iter().find(|carrier| {
                carrier.get("carrier_id").and_then(Value::as_str) == Some(carrier_id)
            })
        })
        .ok_or_else(|| {
            stale(
                "The carrier generation is absent from its selected-carrier bundle.",
                json!({"bundle_id":bundle_id,"carrier_id":carrier_id,"admission_state":"integrity_refused"}),
            )
        })?;
    if member
        .get("generation_sidecar_path")
        .and_then(Value::as_str)
        .is_none_or(|path| !same_path(path, &sidecar.to_string_lossy()))
    {
        return Err(stale(
            "The bundle maps the carrier to a different generation sidecar.",
            json!({"bundle_id":bundle_id,"carrier_id":carrier_id,"admission_state":"integrity_refused"}),
        ));
    }
    let pointer_path = PathBuf::from(required("bundle_commit_pointer_path")?);
    let pointer = read_json(&pointer_path).map_err(|error| {
        stale(
            "The carrier bundle commit pointer is missing or unreadable.",
            json!({"error":error,"commit_pointer_path":pointer_path,"admission_state":"integrity_refused"}),
        )
    })?;
    if pointer.get("schema").and_then(Value::as_str)
        != Some("narada.carrier_generation_bundle_pointer.v1")
        || pointer.get("bundle_id").and_then(Value::as_str) != Some(bundle_id)
        || pointer.get("bundle_fingerprint").and_then(Value::as_str) != Some(expected_fingerprint)
        || pointer
            .get("bundle_path")
            .and_then(Value::as_str)
            .is_none_or(|path| !same_path(path, &bundle_path.to_string_lossy()))
    {
        return Err(stale(
            "The generation does not belong to the currently committed bundle.",
            json!({"bundle_id":bundle_id,"commit_pointer_path":pointer_path,"admission_state":"integrity_refused"}),
        ));
    }
    let build_set_path = PathBuf::from(required("artifact_build_set_path")?);
    let build_set = read_json(&build_set_path).map_err(|error| {
        stale(
            "The sealed artifact build set is missing or unreadable.",
            json!({"error":error,"artifact_build_set_path":build_set_path,"admission_state":"integrity_refused"}),
        )
    })?;
    if build_set.get("schema").and_then(Value::as_str) != Some("narada.artifact_build_set.v1")
        || build_set.get("build_set_digest").and_then(Value::as_str)
            != generation
                .get("artifact_build_set_fingerprint")
                .and_then(Value::as_str)
    {
        return Err(stale(
            "The generation references a different artifact build set.",
            json!({"artifact_build_set_path":build_set_path,"admission_state":"integrity_refused"}),
        ));
    }
    Ok(())
}

