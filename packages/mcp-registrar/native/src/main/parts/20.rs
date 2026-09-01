fn refresh_site_sidecar_bindings(contract: &Value, site: &Value) -> Result<Value, String> {
    let site_id = site["site_id"]
        .as_str()
        .filter(|value| !value.is_empty())
        .ok_or("registrar_site_id_missing")?;
    let config_dir = site_mcp_control_root(Path::new(site["root"].as_str().unwrap_or("")))
        .join(".ai")
        .join("mcp");
    if !config_dir.exists() {
        return Ok(json!({"inspected":0,"refreshed":0,"changed":0}));
    }
    let aggregate_name = format!("{site_id}-mcp.json");
    let mut paths = fs::read_dir(&config_dir)
        .map_err(|error| error.to_string())?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            let file_name = path
                .file_name()
                .and_then(|value| value.to_str())
                .unwrap_or("");
            path.extension().and_then(|value| value.to_str()) == Some("json")
                && file_name.starts_with("narada-")
                && file_name != aggregate_name
        })
        .collect::<Vec<_>>();
    paths.sort();
    if paths.len() > 256 {
        return Err(format!(
            "registrar_site_binding_refresh_limit_exceeded:{site_id}:{}",
            paths.len()
        ));
    }
    let mut seen = BTreeSet::new();
    let mut inspected = 0usize;
    let mut refreshed = 0usize;
    let mut changed = 0usize;
    for path in paths {
        let config: Value = serde_json::from_slice(
            &fs::read(&path)
                .map_err(|error| format!("registrar_site_binding_read_failed:{error}"))?,
        )
        .map_err(|error| {
            format!(
                "registrar_site_binding_parse_failed:{}:{error}",
                path_text(&path)
            )
        })?;
        let Some(servers) = config.get("mcpServers").and_then(Value::as_object) else {
            continue;
        };
        if servers.len() > 64 {
            return Err(format!(
                "registrar_site_binding_server_limit_exceeded:{}:{}",
                path_text(&path),
                servers.len()
            ));
        }
        for server in servers.values() {
            inspected += 1;
            let Some(surface_id) = server
                .get("surface_id")
                .and_then(Value::as_str)
                .filter(|value| !value.is_empty())
            else {
                continue;
            };
            let projection_id = server
                .get("projection_id")
                .and_then(Value::as_str)
                .filter(|value| !value.is_empty())
                .unwrap_or("default");
            if !seen.insert((surface_id.to_string(), projection_id.to_string())) {
                continue;
            }
            let result = site_bind(
                contract,
                &json!({
                    "site_id":site_id,
                    "surface_id":surface_id,
                    "projection_id":projection_id,
                    "allow_sidecar":true
                }),
            )
            .map_err(|error| {
                format!(
                    "registrar_site_binding_refresh_surface_failed:{site_id}:{surface_id}:{projection_id}:{error}"
                )
            })?;
            if result["status"] != "bound" {
                return Err(format!(
                    "registrar_site_binding_refresh_refused:{site_id}:{surface_id}:{}",
                    result
                ));
            }
            refreshed += 1;
            if result
                .pointer("/activation/binding_changed")
                .and_then(Value::as_bool)
                == Some(true)
            {
                changed += 1;
            }
        }
    }
    Ok(json!({"inspected":inspected,"refreshed":refreshed,"changed":changed}))
}

fn site_surface_registry_sync(contract: &Value, args: &Value) -> Result<Value, String> {
    let requested = args
        .get("site_id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| "registrar_requires_site_id".to_string())?;
    let catalog = site_catalog(contract);
    let sites = catalog["items"].as_array().cloned().unwrap_or_default();
    let site = lookup_site_value(&sites, requested)?;
    let path = capability_registry_path(Path::new(site["root"].as_str().unwrap_or("")));
    if args.get("dry_run").and_then(Value::as_bool) == Some(true) {
        let registry = build_site_surface_registry(contract, &site)?;
        let surfaces = registry["surfaces"].as_array().cloned().unwrap_or_default();
        let tool_count = surfaces
            .iter()
            .map(|surface| {
                surface["registered_live_tools"]
                    .as_array()
                    .map_or(0, Vec::len)
            })
            .sum::<usize>();
        let mut result = json!({"schema":"narada.registrar.site_surface_registry_sync.v1","status":"dry_run","site_id":requested,"path":path_text(&path),"surface_count":surfaces.len(),"tool_count":tool_count,"registry_included":false,"bounded":true});
        if args.get("include_registry").and_then(Value::as_bool) == Some(true) {
            result["registry"] = registry;
            result["registry_included"] = json!(true);
        }
        return Ok(result);
    }
    let binding_refresh = refresh_site_sidecar_bindings(contract, &site).map_err(|error| {
        format!("registrar_site_binding_refresh_failed:{requested}:{error}")
    })?;
    let mut registry = build_site_surface_registry(contract, &site)
        .map_err(|error| format!("registrar_site_registry_build_failed:{requested}:{error}"))?;
    let registry_changed = preserve_existing_registry_when_semantically_equal(&path, &mut registry);
    let parent = path
        .parent()
        .ok_or("registrar_site_registry_path_invalid")?;
    fs::create_dir_all(parent).map_err(|error| {
        format!(
            "registrar_site_registry_parent_create_failed:{}:{error}",
            path_text(parent)
        )
    })?;
    if registry_changed {
        let temporary = path.with_extension(format!("json.tmp-{}", std::process::id()));
        fs::write(
            &temporary,
            serde_json::to_string_pretty(&registry).map_err(|error| error.to_string())? + "\n",
        )
        .map_err(|error| {
            format!(
                "registrar_site_registry_write_failed:{}:{error}",
                path_text(&temporary)
            )
        })?;
        fs::rename(&temporary, &path).map_err(|error| {
            format!(
                "registrar_site_registry_rename_failed:{}:{}:{error}",
                path_text(&temporary),
                path_text(&path)
            )
        })?;
    }
    let surfaces = registry["surfaces"].as_array().cloned().unwrap_or_default();
    let tool_count = surfaces
        .iter()
        .map(|surface| {
            surface["registered_live_tools"]
                .as_array()
                .map_or(0, Vec::len)
        })
        .sum::<usize>();
    Ok(
        json!({"schema":"narada.registrar.site_surface_registry_sync.v1","status":"synced","site_id":site["site_id"],"path":path_text(&path),"surface_count":surfaces.len(),"tool_count":tool_count,"registry_changed":registry_changed,"binding_refresh":binding_refresh,"bounded":true}),
    )
}

fn build_site_surface_registry(contract: &Value, site: &Value) -> Result<Value, String> {
    let root = PathBuf::from(site["root"].as_str().unwrap_or(""));
    let directory = site_mcp_control_root(&root).join(".ai").join("mcp");
    let mut surfaces = vec![];
    if let Ok(entries) = fs::read_dir(&directory) {
        for entry in entries.filter_map(Result::ok) {
            let path = entry.path();
            if path.extension().and_then(|value| value.to_str()) != Some("json") {
                continue;
            }
            let Some(file) = path.file_name().and_then(|value| value.to_str()) else {
                continue;
            };
            let Some(config) = fs::read_to_string(&path)
                .ok()
                .and_then(|text| serde_json::from_str::<Value>(&text).ok())
            else {
                continue;
            };
            for (server_name, server) in config["mcpServers"].as_object().into_iter().flatten() {
                surfaces.push(registry_surface(contract, site, server_name, server, file)?);
            }
        }
    }
    surfaces.sort_by(|left: &Value, right: &Value| {
        left["server_name"]
            .as_str()
            .unwrap_or("")
            .cmp(right["server_name"].as_str().unwrap_or(""))
    });
    let generated_at = time::OffsetDateTime::now_utc()
        .format(&time::format_description::well_known::Rfc3339)
        .map_err(|error| error.to_string())?;
    Ok(
        json!({"schema":"narada.site.capabilities.mcp_surfaces.v1","artifact_role":"site_capability_surface_registry_not_mcp_client_config","site_id":site["site_id"],"generated_by":"mcp-registrar","generated_at":generated_at,"generation_policy":{"source":".ai/mcp + registrar surface catalog","mode":"enabled_surface_tool_authority","note":"Every tool exposed by an enabled MCP surface is declared for action admission. The MCP surface remains responsible for command policy and mutation enforcement."},"surfaces":surfaces}),
    )
}

fn embedded_site_local_catalog(server: &Value, surface_id: &str) -> Option<Value> {
    let projection = server.get("surface_projection")?;
    let descriptor = projection.get("surface_descriptor")?;
    if descriptor.get("surface_id").and_then(Value::as_str) != Some(surface_id) {
        return None;
    }
    let mut local_projection = projection.clone();
    if let Some(object) = local_projection.as_object_mut() {
        object
            .entry("id".to_string())
            .or_insert_with(|| json!(projection["projection_id"].as_str().unwrap_or("default")));
    }
    Some(json!({
        "id": surface_id,
        "tools": projection.get("exposed_tools").cloned().unwrap_or_else(|| json!([])),
        "projections": [local_projection],
        "descriptor": descriptor
    }))
}

