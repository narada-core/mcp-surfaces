fn registry(root: &Path) -> Result<Value, Value> {
    let path = env::var("NARADA_SPEECH_PROVIDER_REGISTRY_PATH")
        .ok()
        .or_else(|| env::var("NARADA_PROVIDER_REGISTRY_PATH").ok())
        .filter(|value| !value.trim().is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            let candidate = root.join(".narada/provider-registry.json");
            candidate.exists().then_some(candidate)
        })
        .ok_or_else(|| error("speech_provider_registry_path_required", "speech provider registry path is required", json!({"remediation":"Pass --provider-registry-path or set NARADA_SPEECH_PROVIDER_REGISTRY_PATH."})))?;
    let metadata = fs::metadata(&path).map_err(|cause| {
        error(
            "speech_provider_registry_read_failed",
            &cause.to_string(),
            json!({"path":path}),
        )
    })?;
    if metadata.len() > MAX_REGISTRY_BYTES {
        return Err(error(
            "speech_provider_registry_too_large",
            "provider registry exceeds 2 MiB",
            json!({"size":metadata.len()}),
        ));
    }
    let value: Value = serde_json::from_slice(&fs::read(&path).map_err(|cause| {
        error(
            "speech_provider_registry_read_failed",
            &cause.to_string(),
            json!({"path":path}),
        )
    })?)
    .map_err(|cause| {
        error(
            "speech_provider_registry_invalid",
            &cause.to_string(),
            json!({"path":path}),
        )
    })?;
    if value.get("providers").and_then(Value::as_object).is_none() {
        return Err(error(
            "speech_provider_registry_invalid",
            "providers object is required",
            json!({"path":path}),
        ));
    }
    Ok(value)
}

fn resolve_selection(
    args: &Map<String, Value>,
    key: &str,
    capability: &str,
    root: &Path,
) -> Result<Selection, Value> {
    let registry = registry(root)?;
    let explicit = args.get(key).and_then(Value::as_object);
    let defaults = registry
        .get("defaults")
        .and_then(Value::as_object)
        .and_then(|defaults| defaults.get(capability))
        .and_then(Value::as_object);
    let provider = explicit
        .and_then(|value| value.get("provider"))
        .and_then(Value::as_str)
        .or_else(|| {
            defaults
                .and_then(|value| value.get("provider"))
                .and_then(Value::as_str)
        })
        .ok_or_else(|| {
            error(
                "speech_provider_default_missing",
                "provider selection is required",
                json!({"capability":capability}),
            )
        })?;
    let provider_record = registry
        .pointer(&format!("/providers/{}", pointer_component(provider)))
        .and_then(Value::as_object)
        .ok_or_else(|| {
            error(
                "speech_provider_unknown",
                provider,
                json!({"capability":capability}),
            )
        })?;
    let model = explicit
        .and_then(|value| value.get("model"))
        .and_then(Value::as_str)
        .or_else(|| {
            defaults
                .and_then(|value| value.get("model"))
                .and_then(Value::as_str)
        })
        .or_else(|| {
            provider_record
                .get("capabilities")
                .and_then(Value::as_object)
                .and_then(|value| value.get(capability))
                .and_then(Value::as_object)
                .and_then(|value| value.get("default_model"))
                .and_then(Value::as_str)
        })
        .ok_or_else(|| {
            error(
                "speech_model_default_missing",
                provider,
                json!({"capability":capability}),
            )
        })?;
    let model_record = provider_record
        .get("models")
        .and_then(Value::as_object)
        .and_then(|models| models.get(model))
        .and_then(Value::as_object)
        .ok_or_else(|| {
            error(
                "speech_model_unknown",
                model,
                json!({"provider":provider,"capability":capability}),
            )
        })?;
    if model_record
        .get("status")
        .and_then(Value::as_str)
        .is_some_and(|status| status != "active")
    {
        return Err(error(
            "speech_model_inactive",
            model,
            json!({"provider":provider,"capability":capability}),
        ));
    }
    let capability_record = model_record
        .get("capabilities")
        .and_then(Value::as_object)
        .and_then(|value| value.get(capability))
        .and_then(Value::as_object)
        .ok_or_else(|| {
            error(
                "speech_capability_not_supported",
                capability,
                json!({"provider":provider,"model":model}),
            )
        })?;
    let adapter = capability_record
        .get("adapter")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            error(
                "speech_adapter_missing",
                capability,
                json!({"provider":provider,"model":model}),
            )
        })?;
    let voices: Vec<String> = capability_record
        .get("voices")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| {
                    item.as_str()
                        .or_else(|| item.get("id").and_then(Value::as_str))
                })
                .take(100)
                .map(ToOwned::to_owned)
                .collect()
        })
        .unwrap_or_default();
    let voice = explicit
        .and_then(|value| value.get("voice"))
        .and_then(Value::as_str)
        .or_else(|| {
            capability_record
                .get("default_voice")
                .and_then(Value::as_str)
        })
        .map(ToOwned::to_owned);
    if let Some(voice) = voice.as_deref() {
        if !voices.is_empty() && !voices.iter().any(|candidate| candidate == voice) {
            return Err(error(
                "speech_voice_unknown",
                voice,
                json!({"provider":provider,"model":model}),
            ));
        }
    }
    let base_url = provider_record
        .get("base_url")
        .and_then(Value::as_str)
        .unwrap_or("https://api.openai.com")
        .trim_end_matches('/')
        .to_string();
    let credential_env_names = provider_record
        .get("credential_requirement")
        .and_then(Value::as_object)
        .and_then(|value| value.get("env_names"))
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .take(16)
                .map(ToOwned::to_owned)
                .collect()
        })
        .unwrap_or_default();
    Ok(Selection {
        provider: provider.to_string(),
        model: model.to_string(),
        adapter: adapter.to_string(),
        voice,
        voices,
        base_url,
        credential_env_names,
        source: if explicit.is_some() {
            "explicit"
        } else {
            "registry_default"
        },
    })
}

fn selection_public(selection: &Selection, capability: &str) -> Value {
    json!({"provider":selection.provider,"model":selection.model,"capability":capability,"adapter":selection.adapter,"voice":selection.voice,"status":"active"})
}

