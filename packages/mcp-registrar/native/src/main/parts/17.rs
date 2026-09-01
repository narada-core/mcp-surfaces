fn export_target_exists(path: &Path) -> bool {
    let text = path.to_string_lossy();
    let Some(index) = text.find(['*', '?']) else {
        return path.exists();
    };
    let prefix = &text[..index];
    let directory = if prefix.ends_with(['/', '\\']) {
        PathBuf::from(&prefix[..prefix.len() - 1])
    } else {
        PathBuf::from(prefix)
            .parent()
            .unwrap_or(Path::new(""))
            .to_path_buf()
    };
    fs::read_dir(directory)
        .ok()
        .is_some_and(|mut entries| entries.next().is_some())
}
fn flag_values<'a>(args: &[&'a str], flag: &str) -> Vec<&'a str> {
    let mut result = vec![];
    let mut index = 0;
    while index < args.len() {
        if args[index] == flag && index + 1 < args.len() {
            result.push(args[index + 1]);
            index += 2
        } else {
            index += 1
        }
    }
    result
}
fn merge_value(mut left: Value, right: Value) -> Value {
    if let (Some(target), Some(source)) = (left.as_object_mut(), right.as_object()) {
        target.extend(source.clone())
    }
    left
}

fn site_bind(contract: &Value, args: &Value) -> Result<Value, String> {
    let site_id = required_argument(args, "site_id", "registrar_requires_site_id")?;
    let surface_id = required_argument(args, "surface_id", "registrar_requires_surface_id")?;
    let catalog = site_catalog(contract);
    let sites = catalog["items"].as_array().cloned().unwrap_or_default();
    let site = lookup_site_value(&sites, &site_id)?;
    let surfaces = contract
        .pointer("/read_models/registrar_surface_list/items")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let surface = surfaces
        .iter()
        .find(|surface| surface["id"] == surface_id)
        .ok_or_else(|| format!("registrar_unknown_surface:{surface_id}"))?;
    if site
        .pointer(&format!("/surface_overrides/{surface_id}/enabled"))
        .and_then(Value::as_bool)
        == Some(false)
        && args.get("allow_disabled_sidecar").and_then(Value::as_bool) != Some(true)
    {
        return Ok(
            json!({"status":"refused","reason_code":"registrar_site_bind_refused_surface_disabled","site_id":site_id,"surface_id":surface_id,"sidecar_state":"disabled_by_site_override","reason":"This Site explicitly disables the requested surface, so registrar_site_bind will not materialize a sidecar for it.","required_next_step":"Enable the surface in the Site override or pass allow_disabled_sidecar=true only for an intentional compatibility sidecar."}),
        );
    }
    let config_dir = site_mcp_control_root(Path::new(site["root"].as_str().unwrap_or("")))
        .join(".ai")
        .join("mcp");
    let aggregate = format!("{site_id}-mcp.json");
    let aggregate_exists = config_dir.join(&aggregate).exists();
    if aggregate_exists && args.get("allow_sidecar").and_then(Value::as_bool) != Some(true) {
        return Ok(
            json!({"status":"refused","reason_code":"registrar_site_bind_refused_aggregate_fabric_exists","site_id":site_id,"surface_id":surface_id,"aggregate_file":aggregate,"reason":"This Site has an authoritative aggregate MCP fabric. registrar_site_bind would create a sidecar snippet, so it is refused unless allow_sidecar is explicitly true.","required_next_step":"Update the aggregate MCP fabric through the Site materialization path, or pass allow_sidecar=true only for an intentional compatibility sidecar."}),
        );
    }
    let projection_id = args
        .get("projection_id")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty());
    let runtime_kind = args
        .get("runtime_kind")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty());
    let projection = select_projection(surface, projection_id, runtime_kind)?;
    fs::create_dir_all(&config_dir).map_err(|error| error.to_string())?;
    let prefix = site_prefix(&site_id);
    let server_key = format!("{prefix}-{surface_id}");
    let binding_id = format!("{site_id}-{surface_id}");
    let file_name = format!("{prefix}-{surface_id}-mcp.json");
    let config = build_bind_config(
        contract,
        &site,
        surface,
        projection,
        runtime_kind,
        &server_key,
    )?;
    let config_path = config_dir.join(&file_name);
    let rendered = serde_json::to_string_pretty(&config).map_err(|error| error.to_string())? + "\n";
    let binding_changed =
        fs::read_to_string(&config_path).ok().as_deref() != Some(rendered.as_str());
    fs::write(&config_path, rendered).map_err(|error| error.to_string())?;
    let registry_result = write_site_registry(contract, &site)?;
    Ok(json!({
        "status":"bound","site_id":site_id,"surface_id":surface_id,"projection_id":projection["id"],"file":file_name,"server_key":server_key,"binding_id":binding_id,"registry":registry_result,
        "activation":{
            "status":if binding_changed {"carrier_rematerialization_required"} else {"binding_unchanged_verify_carrier_admission"},
            "reason":if binding_changed {"The Site binding changed, while already materialized carrier admission envelopes are immutable snapshots."} else {"The Site binding is unchanged. A current carrier may use it only if its admission envelope already contains this binding."},
            "site_binding_ready":true,"binding_changed":binding_changed,"carrier_rematerialization_required":binding_changed,"carrier_restart_required":binding_changed,
            "next_steps":[
                {"order":1,"action":"rematerialize_carriers","owner":"narada-mcp-materializer","instruction":"Run the authoritative all-carrier materialization or recover-generation command."},
                {"order":2,"action":"restart_carrier","owner":"carrier","instruction":"Restart the carrier after successful materialization."},
                {"order":3,"action":"open_surface","owner":"mcp-loader","instruction":"Open the binding by canonical binding_id after restart.","tool":"mcp_loader_open_surface","arguments":{"site_root":site["root"],"binding_id":binding_id,"surface_id":surface_id}}
            ]
        }
    }))
}

fn site_unbind(contract: &Value, args: &Value) -> Result<Value, String> {
    let site_id = required_argument(args, "site_id", "registrar_requires_site_id")?;
    let surface_id = required_argument(args, "surface_id", "registrar_requires_surface_id")?;
    let catalog = site_catalog(contract);
    let sites = catalog["items"].as_array().cloned().unwrap_or_default();
    let site = lookup_site_value(&sites, &site_id)?;
    let directory = site_mcp_control_root(Path::new(site["root"].as_str().unwrap_or("")))
        .join(".ai")
        .join("mcp");
    if !directory.exists() {
        return Ok(json!({"status":"not_found","site_id":site_id,"surface_id":surface_id}));
    }
    let key = format!("{}-{surface_id}", site_prefix(&site_id));
    if let Ok(entries) = fs::read_dir(&directory) {
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
            if config["mcpServers"].get(&key).is_some() {
                let file = entry.file_name().to_string_lossy().to_string();
                fs::remove_file(entry.path()).map_err(|error| error.to_string())?;
                let registry = write_site_registry(contract, &site)?;
                return Ok(
                    json!({"status":"unbound","site_id":site_id,"surface_id":surface_id,"file":file,"registry":registry}),
                );
            }
        }
    }
    Ok(json!({"status":"not_bound","site_id":site_id,"surface_id":surface_id}))
}

fn write_site_registry(contract: &Value, site: &Value) -> Result<Value, String> {
    let mut registry = build_site_surface_registry(contract, site)?;
    let path = capability_registry_path(Path::new(site["root"].as_str().unwrap_or("")));
    fs::create_dir_all(
        path.parent()
            .ok_or("registrar_site_registry_path_invalid")?,
    )
    .map_err(|error| error.to_string())?;
    let registry_changed = preserve_existing_registry_when_semantically_equal(&path, &mut registry);
    if registry_changed {
        fs::write(
            &path,
            serde_json::to_string_pretty(&registry).map_err(|error| error.to_string())? + "\n",
        )
        .map_err(|error| error.to_string())?;
    }
    let surfaces = registry["surfaces"].as_array().cloned().unwrap_or_default();
    let tools = surfaces
        .iter()
        .map(|surface| {
            surface["registered_live_tools"]
                .as_array()
                .map_or(0, Vec::len)
        })
        .sum::<usize>();
    Ok(
        json!({"status":"synced","site_id":site["site_id"],"path":path_text(&path),"surface_count":surfaces.len(),"tool_count":tools,"registry_changed":registry_changed}),
    )
}

fn preserve_existing_registry_when_semantically_equal(path: &Path, registry: &mut Value) -> bool {
    let Ok(existing_text) = fs::read_to_string(path) else {
        return true;
    };
    let Ok(existing) = serde_json::from_str::<Value>(&existing_text) else {
        return true;
    };
    let mut existing_semantic = existing.clone();
    let mut next_semantic = registry.clone();
    if let Some(value) = existing_semantic.as_object_mut() {
        value.remove("generated_at");
    }
    if let Some(value) = next_semantic.as_object_mut() {
        value.remove("generated_at");
    }
    if existing_semantic != next_semantic {
        return true;
    }
    *registry = existing;
    false
}

fn required_argument(args: &Value, name: &str, code: &str) -> Result<String, String> {
    args.get(name)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .ok_or_else(|| code.to_string())
}
fn site_prefix(site_id: &str) -> String {
    if site_id == "andrey-user" {
        "narada-site-andrey-user".into()
    } else if site_id.starts_with("narada-") {
        site_id.into()
    } else {
        format!("narada-{site_id}")
    }
}

fn select_projection<'a>(
    surface: &'a Value,
    projection_id: Option<&str>,
    runtime_kind: Option<&str>,
) -> Result<&'a Value, String> {
    let projections = surface["projections"]
        .as_array()
        .ok_or("registrar_surface_projection_required")?;
    if let Some(id) = projection_id {
        return projections
            .iter()
            .find(|projection| projection["id"] == id)
            .ok_or_else(|| {
                format!(
                    "registrar_unknown_surface_projection:{}:{id}",
                    surface["id"].as_str().unwrap_or("")
                )
            });
    }
    if let Some(kind) = runtime_kind {
        let matches = projections
            .iter()
            .filter(|projection| {
                projection["runtime_requirements"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .any(|value| value == kind)
            })
            .collect::<Vec<_>>();
        if matches.len() == 1 {
            return Ok(matches[0]);
        }
        let neutral = projections
            .iter()
            .filter(|projection| {
                projection["runtime_requirements"]
                    .as_array()
                    .is_none_or(Vec::is_empty)
            })
            .collect::<Vec<_>>();
        if neutral.len() == 1 {
            return Ok(neutral[0]);
        }
    }
    if projections.len() == 1 {
        return Ok(&projections[0]);
    }
    Err(format!(
        "registrar_surface_projection_required:{}",
        surface["id"].as_str().unwrap_or("")
    ))
}

