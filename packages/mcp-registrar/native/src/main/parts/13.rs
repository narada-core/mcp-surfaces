fn surface_usage(contract: &Value, args: &Value) -> Result<Value, String> {
    let surface_id = args
        .get("surface_id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| "registrar_requires_surface_id".to_string())?;
    let is_local = surface_id.ends_with(".local");
    let catalog = site_catalog(contract);
    let sites = catalog["items"].as_array().cloned().unwrap_or_default();
    let mut matching_sites = vec![];
    for site in &sites {
        let site_id = site["site_id"].as_str().unwrap_or("");
        if !is_local
            && (site["surfaces"]
                .as_array()
                .into_iter()
                .flatten()
                .any(|value| value == surface_id)
                || site_fabric_surface_ids(contract, site)
                    .iter()
                    .any(|value| value == surface_id))
        {
            matching_sites.push(json!({"site_id":site_id,"via":"shared"}));
        }
        if site_local_surface_ids(contract, site)
            .iter()
            .any(|value| value == surface_id)
        {
            matching_sites.push(json!({"site_id":site_id,"via":"local"}));
        }
    }
    let carriers = contract
        .pointer("/read_models/registrar_carrier_list/items")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut matching_carriers = vec![];
    for carrier in carriers {
        let carrier_id = carrier["carrier_id"].as_str().unwrap_or("");
        let kind = carrier["kind"].as_str().unwrap_or("");
        for binding in carrier["site_bindings"].as_array().into_iter().flatten() {
            let site_id = binding["site_id"].as_str().unwrap_or("");
            let Some(site) = sites.iter().find(|site| site["site_id"] == site_id) else {
                continue;
            };
            let shared = shared_surface_ids_for_binding(contract, binding, site);
            if !is_local && shared.iter().any(|value| value == surface_id) {
                matching_carriers.push(
                    json!({"carrier_id":carrier_id,"kind":kind,"via":"shared","site_id":site_id}),
                );
            }
            if is_local || binding["surfaces"] == "all" {
                for local in site_local_surface_ids(contract, site) {
                    if local != surface_id {
                        continue;
                    }
                    let includes = binding["surfaces"] == "all"
                        || binding["surfaces"]
                            .as_array()
                            .into_iter()
                            .flatten()
                            .any(|value| value == &local);
                    if includes {
                        matching_carriers.push(json!({"carrier_id":carrier_id,"kind":kind,"via":"local","site_id":site_id}));
                    }
                }
            }
        }
    }
    let mut deduped = vec![];
    for item in matching_carriers {
        if !deduped.iter().any(|existing: &Value| existing == &item) {
            deduped.push(item);
        }
    }
    let runtime_access = json!({
        "available": !matching_sites.is_empty(),
        "owner": "mcp-loader",
        "mode": "site-scoped",
        "carrier_binding_required": matching_sites.is_empty()
    });
    Ok(
        json!({"surface_id":surface_id,"is_local":is_local,"sites":matching_sites,"carriers":deduped,"site_count":matching_sites.len(),"carrier_count":deduped.len(),"runtime_access":runtime_access}),
    )
}

fn site_fabric_surface_ids(contract: &Value, site: &Value) -> Vec<String> {
    let root = PathBuf::from(site["root"].as_str().unwrap_or(""));
    let directory = site_mcp_control_root(&root).join(".ai").join("mcp");
    let known = contract
        .pointer("/read_models/registrar_surface_list/items")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let prefix = if site["site_id"] == "andrey-user" {
        "narada-site-andrey-user".to_string()
    } else {
        let id = site["site_id"].as_str().unwrap_or("");
        if id.starts_with("narada-") {
            id.to_string()
        } else {
            format!("narada-{id}")
        }
    };
    let mut found = vec![];
    let Ok(entries) = fs::read_dir(directory) else {
        return found;
    };
    for entry in entries.filter_map(Result::ok) {
        if entry.path().extension().and_then(|value| value.to_str()) != Some("json") {
            continue;
        }
        let Some(config) = fs::read_to_string(entry.path())
            .ok()
            .and_then(|text| serde_json::from_str::<Value>(&text).ok())
        else {
            continue;
        };
        for (key, server) in config["mcpServers"].as_object().into_iter().flatten() {
            let explicit = server["surface_id"].as_str().map(str::to_string);
            let inferred = key
                .strip_prefix(&(prefix.clone() + "-"))
                .map(str::to_string)
                .unwrap_or_else(|| key.clone());
            let id = explicit.unwrap_or(inferred);
            let canonical = known
                .iter()
                .find(|surface| surface["id"] == id)
                .and_then(|surface| surface["id"].as_str())
                .unwrap_or(&id)
                .to_string();
            if !found.contains(&canonical) {
                found.push(canonical)
            }
        }
    }
    found
}

fn site_local_surface_ids(contract: &Value, site: &Value) -> Vec<String> {
    let Some(path) = site["config_path"].as_str() else {
        return vec![];
    };
    let Ok(text) = fs::read_to_string(path) else {
        return vec![];
    };
    let Some(config) = parse_jsonc(&text) else {
        return vec![];
    };
    let entries = config
        .pointer("/structural_config/agent_execution_policy/allowed_mcp_entrypoints")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let allowlist = site["local_surface_allowlist"].as_array();
    let catalog = contract
        .pointer("/read_models/registrar_surface_list/items")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut result = vec![];
    for entry in entries {
        let Some(id) = entry["surface_id"]
            .as_str()
            .filter(|value| !value.is_empty())
        else {
            continue;
        };
        if allowlist.is_some_and(|values| !values.iter().any(|value| value == id)) {
            continue;
        }
        let canonical = id.trim_end_matches(".local").trim_end_matches("-mcp");
        if let Some(surface) = catalog.iter().find(|surface| surface["id"] == canonical) {
            let local = surface["projections"]
                .as_array()
                .into_iter()
                .flatten()
                .any(|projection| projection["injection_scope"] == "local_site");
            if !local {
                continue;
            }
        }
        if !result.iter().any(|value| value == id) {
            result.push(id.to_string())
        }
    }
    result
}

fn shared_surface_ids_for_binding(contract: &Value, binding: &Value, site: &Value) -> Vec<String> {
    let items = contract
        .pointer("/read_models/registrar_surface_list/items")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let enabled = |id: &str| {
        site.pointer(&format!("/surface_overrides/{id}/enabled"))
            .and_then(Value::as_bool)
            != Some(false)
    };
    let mut ids: Vec<String> = if binding["surfaces"] == "all" {
        items
            .iter()
            .filter_map(|surface| surface["id"].as_str())
            .filter(|id| enabled(id))
            .map(str::to_string)
            .collect()
    } else {
        binding["surfaces"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .filter(|id| !id.ends_with(".local") && enabled(id))
            .map(str::to_string)
            .collect()
    };
    if binding["loading_mode"] == "progressive" {
        for id in ["task-lifecycle", "surface-feedback"] {
            if enabled(id) && !ids.iter().any(|value| value == id) {
                ids.push(id.into())
            }
        }
    } else {
        for surface in &items {
            let Some(id) = surface["id"].as_str() else {
                continue;
            };
            if !enabled(id) {
                continue;
            }
            let automatic =
                surface["projections"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .any(|projection| {
                        projection["injection_scope"] == "local_site"
                            && projection["default_injection"] == "enabled"
                    });
            if automatic && !ids.iter().any(|value| value == id) {
                ids.push(id.into())
            }
        }
    }
    ids
}

