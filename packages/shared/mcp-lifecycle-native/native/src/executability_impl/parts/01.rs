/*
 * Native task-executability implementation.
 *
 * This file is included in lib.rs so the implementation remains part of the
 * lifecycle authority and can use the server's private SQLite/query helpers.
 */

fn native_canonical_value(value: &Value) -> Value {
    match value {
        Value::Array(values) => {
            Value::Array(values.iter().map(native_canonical_value).collect())
        }
        Value::Object(object) => {
            let mut keys = object.keys().cloned().collect::<Vec<_>>();
            keys.sort();
            let mut sorted = Map::new();
            for key in keys {
                if let Some(value) = object.get(&key) {
                    sorted.insert(key, native_canonical_value(value));
                }
            }
            Value::Object(sorted)
        }
        _ => value.clone(),
    }
}

fn native_canonical_digest(value: &Value) -> String {
    let mut hasher = Sha256::new();
    hasher.update(
        serde_json::to_vec(&native_canonical_value(value))
            .unwrap_or_default(),
    );
    format!("{:x}", hasher.finalize())
}

fn native_node_platform() -> &'static str {
    if cfg!(windows) {
        "win32"
    } else if cfg!(target_os = "macos") {
        "darwin"
    } else if cfg!(target_os = "linux") {
        "linux"
    } else {
        env::consts::OS
    }
}

fn native_site_id(root: &Path) -> String {
    if let Ok(value) = env::var("NARADA_SITE_ID") {
        if !value.trim().is_empty() {
            return value;
        }
    }
    root.file_name()
        .and_then(|value| value.to_str())
        .unwrap_or_default()
        .to_string()
}

fn native_environment(root: &Path) -> Value {
    let mut environment = json!({
        "schema": "narada.task_executability_declared_environment.v1",
        "site_id": native_site_id(root),
        "substrate": native_node_platform(),
        "declared_tools": [],
        "declared_authority": []
    });
    if let Ok(variant) = env::var("NARADA_SUBSTRATE_VARIANT") {
        if !variant.trim().is_empty() {
            if let Some(object) = environment.as_object_mut() {
                object.insert("variant".to_string(), Value::String(variant));
            }
        }
    }
    environment
}

fn native_environment_digest(root: &Path) -> String {
    let environment = native_environment(root);
    native_canonical_digest(&json!({
        "kind": "declared_environment",
        "site_id": environment.get("site_id"),
        "substrate": environment.get("substrate"),
        "variant": environment.get("variant"),
        "declared_tools": environment.get("declared_tools"),
        "declared_authority": environment.get("declared_authority")
    }))
}

fn native_policy(root: &Path) -> Result<(String, Value), String> {
    let defaults = [
        ("trigger", "manual"),
        ("enforcement", "off"),
        ("evaluator_profile", "shoshin-v1"),
    ];
    let mut values = Map::new();
    let mut provenance = Vec::new();
    let loci = [
        ("target_site", root.to_path_buf()),
        (
            "user_site",
            env::var_os("NARADA_USER_SITE_ROOT")
                .map(PathBuf::from)
                .unwrap_or_default(),
        ),
        (
            "host_site",
            env::var_os("NARADA_HOST_SITE_ROOT")
                .map(PathBuf::from)
                .unwrap_or_default(),
        ),
    ];
    for (field, default_value) in defaults {
        let mut selected: Option<(String, String, String)> = None;
        for (source, locus) in &loci {
            if locus.as_os_str().is_empty() {
                continue;
            }
            let path = locus.join(".ai").join("task-executability-policy.json");
            if !path.exists() {
                continue;
            }
            let text = fs::read_to_string(&path)
                .map_err(|_| format!("task_executability_policy_invalid:{source}:read:{path:?}"))?;
            let document: Value = serde_json::from_str(&text).map_err(|_| {
                format!("task_executability_policy_invalid:{source}:not_json:{path:?}")
            })?;
            let object = document.as_object().ok_or_else(|| {
                format!("task_executability_policy_invalid:{source}:not_object:{path:?}")
            })?;
            if object.get("schema").and_then(Value::as_str)
                != Some("narada.task_executability_policy.v1")
            {
                return Err(format!(
                    "task_executability_policy_invalid:{source}:policy_schema_mismatch:{path:?}"
                ));
            }
            for forbidden in [
                "provider",
                "model",
                "provider_id",
                "model_id",
                "reasoning_effort",
                "cognition",
            ] {
                if object.contains_key(forbidden) {
                    return Err(format!(
                        "task_executability_policy_invalid:{source}:policy_field_forbidden:{forbidden}:{path:?}"
                    ));
                }
            }
            if let Some(candidate) = object.get(field) {
                let value = candidate.as_str().ok_or_else(|| {
                    format!(
                        "task_executability_policy_invalid:{source}:{field}_invalid:{path:?}"
                    )
                })?;
                if field == "trigger" && !matches!(value, "manual" | "on_create") {
                    return Err(format!(
                        "task_executability_policy_invalid:{source}:policy_trigger_invalid:{path:?}"
                    ));
                }
                if field == "enforcement" && !matches!(value, "off" | "warn" | "strict") {
                    return Err(format!(
                        "task_executability_policy_invalid:{source}:policy_enforcement_invalid:{path:?}"
                    ));
                }
                if value.trim().is_empty() {
                    return Err(format!(
                        "task_executability_policy_invalid:{source}:{field}_invalid:{path:?}"
                    ));
                }
                selected = Some((
                    value.to_string(),
                    source.to_string(),
                    path.to_string_lossy().to_string(),
                ));
                break;
            }
        }
        let (value, source, source_ref) = selected.unwrap_or_else(|| {
            (
                default_value.to_string(),
                "product_default".to_string(),
                "product-defaults".to_string(),
            )
        });
        values.insert(field.to_string(), Value::String(value.clone()));
        provenance.push(json!({
            "field": field,
            "value": value,
            "source": source,
            "source_ref": source_ref
        }));
    }
    let profile = values
        .get("evaluator_profile")
        .and_then(Value::as_str)
        .unwrap_or("shoshin-v1")
        .to_string();
    Ok((
        profile,
        json!({
            "schema": "narada.task_executability_resolved_policy.v1",
            "trigger": values.get("trigger"),
            "enforcement": values.get("enforcement"),
            "evaluator_profile": values.get("evaluator_profile"),
            "provenance": provenance
        }),
    ))
}
