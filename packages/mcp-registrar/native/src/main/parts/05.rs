fn carrier_unbind(contract: &Value, args: &Value) -> Result<Value, MutationFailure> {
    let carrier_id = required_argument(args, "carrier_id", "registrar_requires_carrier_id")
        .map_err(|message| mutation_failure("registrar_requires_carrier_id", message, json!({})))?;
    let surface_id = required_argument(args, "surface_id", "registrar_requires_surface_id")
        .map_err(|message| mutation_failure("registrar_requires_surface_id", message, json!({})))?;
    let carrier = carrier_record(contract, &carrier_id)
        .map_err(|message| mutation_failure("registrar_unknown_carrier", message, json!({})))?;
    let keys = carrier_surface_keys(contract, &carrier_id, &surface_id);
    if !keys.is_empty() {
        return Err(mutation_failure(
            "registrar_carrier_unbind_refused_aggregate_surface",
            format!("registrar_carrier_unbind_refused_aggregate_surface:{surface_id}"),
            json!({"carrier_id":carrier_id,"surface_id":surface_id,"server_keys":keys,"remediation":"This surface is produced by the external native carrier contract. Remove it from that contract or the owning Site registry, then run cargo native-materialize."}),
        ));
    }
    let kind = carrier["kind"].as_str().unwrap_or("");
    if kind == "opencode" {
        return Err(mutation_failure(
            "registrar_single_surface_unbind_unsupported_for_opencode_aggregate",
            "registrar_single_surface_unbind_unsupported_for_opencode_aggregate".into(),
            json!({}),
        ));
    }
    let declared_path = carrier["config_path"].as_str().unwrap_or("");
    let path_value = effective_carrier_config_path(kind, declared_path);
    let path = path_value.as_str();
    let content = fs::read_to_string(path).map_err(|_| {
        mutation_failure(
            "registrar_config_not_found",
            format!("registrar_config_not_found:{path}"),
            json!({}),
        )
    })?;
    let bound = if kind == "kimi" {
        parse_jsonc(&content)
            .and_then(|value| value["mcpServers"].as_object().cloned())
            .is_some_and(|servers| {
                servers.contains_key(&format!("narada-site-andrey-user-{surface_id}"))
            })
    } else {
        content.contains(&format!("[mcp_servers.{surface_id}]"))
    };
    if !bound {
        return Ok(json!({"status":"not_bound","carrier_id":carrier_id,"surface_id":surface_id}));
    }
    let (next_content, server_key) = if kind == "kimi" {
        let mut parsed = parse_jsonc(&content).unwrap();
        let key = format!("narada-site-andrey-user-{surface_id}");
        parsed["mcpServers"].as_object_mut().unwrap().remove(&key);
        (
            String::from_utf8(pretty_json(&parsed).map_err(|error| {
                mutation_failure(
                    "registrar_json_emit_failed",
                    error,
                    json!({"config_path":path}),
                )
            })?)
            .map_err(|error| {
                mutation_failure(
                    "registrar_json_emit_failed",
                    error.to_string(),
                    json!({"config_path":path}),
                )
            })?,
            key,
        )
    } else {
        let section = format!("[mcp_servers.{surface_id}]");
        let index = content.find(&section).unwrap();
        let next = content[index + section.len()..]
            .find("\n[")
            .map(|offset| index + section.len() + offset);
        (
            if let Some(next) = next {
                format!("{}{}", &content[..index], &content[next..])
            } else {
                content[..index].trim_end().to_string()
            },
            surface_id.clone(),
        )
    };
    let template=contract.pointer(&format!("/read_models/registrar_carrier_projection_plans/{carrier_id}/recovery_unbind/{surface_id}")).ok_or_else(||mutation_failure("registrar_native_carrier_unbind_template_missing",format!("registrar_native_carrier_unbind_template_missing:{carrier_id}:{surface_id}"),json!({})))?;
    let mut runtime_plan = template["runtime_materialization_plan"].clone();
    let validation = template["materialization_validation"].clone();
    let mut generation = template["generation_unsigned"].clone();
    let current_sidecar_path = format!("{path}.narada-generation.json");
    let current_generation: Value =
        serde_json::from_slice(&fs::read(&current_sidecar_path).map_err(|error| {
            mutation_failure(
                "registrar_generation_sidecar_read_failed",
                error.to_string(),
                json!({"path":current_sidecar_path}),
            )
        })?)
        .map_err(|error| {
            mutation_failure(
                "registrar_generation_sidecar_invalid",
                error.to_string(),
                json!({"path":current_sidecar_path}),
            )
        })?;
    let artifact_manifest_fingerprint = current_generation
        .get("artifact_manifest_fingerprint")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            mutation_failure(
                "registrar_artifact_manifest_fingerprint_missing",
                "registrar_artifact_manifest_fingerprint_missing".into(),
                json!({"path":current_sidecar_path}),
            )
        })?;
    generation.as_object_mut().unwrap().insert(
        "artifact_manifest_fingerprint".into(),
        json!(artifact_manifest_fingerprint),
    );
    if path != declared_path {
        replace_value_string(&mut runtime_plan, declared_path, path);
        replace_value_string(&mut generation, declared_path, path);
    }
    let object = runtime_plan.as_object_mut().unwrap();
    object.remove("plan_fingerprint");
    let fingerprint = sha256_text(&serde_json::to_string(object).unwrap());
    object.insert("plan_fingerprint".into(), json!(fingerprint.clone()));
    generation.as_object_mut().unwrap().insert(
        "runtime_materialization_plan_fingerprint".into(),
        json!(fingerprint),
    );
    let selectors = generation
        .pointer("/managed_projection/selectors")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            mutation_failure(
                "registrar_managed_selectors_missing",
                "registrar_managed_selectors_missing".into(),
                json!({"config_path":path}),
            )
        })?
        .iter()
        .map(|value| {
            value.as_str().map(str::to_string).ok_or_else(|| {
                mutation_failure(
                    "registrar_managed_selector_invalid",
                    "registrar_managed_selector_invalid".into(),
                    json!({"config_path":path}),
                )
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    let description =
        describe_config(kind, next_content.as_bytes(), &selectors).map_err(|error| {
            mutation_failure(
                "registrar_materialization_contract_failed",
                format!("registrar_materialization_contract_failed:{error}"),
                json!({"config_path":path}),
            )
        })?;
    generation.as_object_mut().unwrap().insert(
        "config_artifact".into(),
        serde_json::to_value(description.config_artifact).map_err(|error| {
            mutation_failure(
                "registrar_materialization_contract_failed",
                error.to_string(),
                json!({"config_path":path}),
            )
        })?,
    );
    generation.as_object_mut().unwrap().insert(
        "managed_projection".into(),
        serde_json::to_value(description.managed_projection).map_err(|error| {
            mutation_failure(
                "registrar_materialization_contract_failed",
                error.to_string(),
                json!({"config_path":path}),
            )
        })?,
    );
    let generated_at = time::OffsetDateTime::now_utc()
        .format(&time::macros::format_description!(
            "[year]-[month]-[day]T[hour]:[minute]:[second].[subsecond digits:3]Z"
        ))
        .map_err(|error| {
            mutation_failure("registrar_clock_failed", error.to_string(), json!({}))
        })?;
    generation
        .as_object_mut()
        .unwrap()
        .insert("generated_at".into(), json!(generated_at));
    let fingerprint = generation_fingerprint(&generation).map_err(|error| {
        mutation_failure(
            "registrar_generation_fingerprint_failed",
            format!("registrar_generation_fingerprint_failed:{error}"),
            json!({"config_path":path}),
        )
    })?;
    generation
        .as_object_mut()
        .unwrap()
        .insert("generation_fingerprint".into(), json!(fingerprint));
    fs::write(path, &next_content).map_err(|error| {
        mutation_failure(
            "registrar_config_write_failed",
            error.to_string(),
            json!({"config_path":path}),
        )
    })?;
    let plan_path = format!("{path}.narada-runtime-plan.json");
    let sidecar_path = format!("{path}.narada-generation.json");
    write_pretty_json(&plan_path, &runtime_plan).map_err(|message| {
        mutation_failure(
            "registrar_runtime_plan_write_failed",
            message,
            json!({"path":plan_path}),
        )
    })?;
    write_pretty_json(&sidecar_path, &generation).map_err(|message| {
        mutation_failure(
            "registrar_generation_write_failed",
            message,
            json!({"path":sidecar_path}),
        )
    })?;
    Ok(
        json!({"status":"unbound","carrier_id":carrier_id,"surface_id":surface_id,"server_key":server_key,"runtime_contract_version":CONTRACT_VERSION,"materialization_validation":validation,"materialization_generation":generation,"generation_sidecar_path":sidecar_path,"runtime_materialization_plan":runtime_plan,"runtime_materialization_plan_path":plan_path,"recovery_escape_hatch":true}),
    )
}
fn write_pretty_json(path: &str, value: &Value) -> Result<(), String> {
    let target = PathBuf::from(path);
    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent).map_err(|error| error.to_string())?
    }
    let temporary = PathBuf::from(format!("{path}.tmp-{}", std::process::id()));
    fs::write(
        &temporary,
        serde_json::to_string_pretty(value).map_err(|error| error.to_string())? + "\n",
    )
    .map_err(|error| error.to_string())?;
    fs::rename(temporary, target).map_err(|error| error.to_string())
}
fn effective_carrier_config_path(kind: &str, fallback: &str) -> String {
    let name = match kind {
        "opencode" => "NARADA_OPENCODE_CONFIG_PATH",
        "kimi" => "NARADA_KIMI_CONFIG_PATH",
        "codex" => "NARADA_CODEX_CONFIG_PATH",
        _ => "",
    };
    env::var(name)
        .ok()
        .filter(|value| !value.trim().is_empty())
        .map(|value| path_text(&canonical_root(PathBuf::from(value))))
        .unwrap_or_else(|| fallback.into())
}
fn replace_value_string(value: &mut Value, old: &str, new: &str) {
    match value {
        Value::String(text) => *text = text.replace(old, new),
        Value::Array(items) => {
            for item in items {
                replace_value_string(item, old, new)
            }
        }
        Value::Object(items) => {
            for item in items.values_mut() {
                replace_value_string(item, old, new)
            }
        }
        _ => {}
    }
}
