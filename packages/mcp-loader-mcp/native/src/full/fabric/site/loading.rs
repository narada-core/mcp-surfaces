use crate::full::*;

pub(crate) fn read_site_fabric(site_root: &str) -> Result<FabricBundle, Diagnostic> {
    let paths = resolve_site_fabric_paths(site_root)?;
    let mut servers = Map::new();
    let mut source_by_surface = HashMap::new();
    let mut site_id: Option<String> = None;
    for path in &paths {
        let text = read_to_string(path).map_err(|error| {
            Diagnostic::new(
                "site_fabric_parse_error",
                format!("site_fabric_parse_error:{}:{}", path, error),
            )
        })?;
        let fragment: Value = serde_json::from_str(&text).map_err(|error| {
            Diagnostic::new(
                "site_fabric_parse_error",
                format!("site_fabric_parse_error:{}:{}", path, error),
            )
        })?;
        let fragment_obj = fragment.as_object().cloned().ok_or_else(|| {
            Diagnostic::new(
                "site_fabric_parse_error",
                format!("site_fabric_parse_error:{}:object_required", path),
            )
        })?;
        if let Some(fragment_site_id) = value_string(fragment_obj.get("site_id")) {
            if fragment_site_id == "narada-andrey" || fragment_site_id == "narada-user-site" {
                return Err(Diagnostic::new(
                    "site_fabric_legacy_site_id_rejected",
                    format!(
                        "site_fabric_legacy_site_id_rejected:{}:{}",
                        fragment_site_id, path
                    ),
                ));
            }
            if let Some(existing) = &site_id {
                if existing != &fragment_site_id {
                    return Err(Diagnostic::new(
                        "site_fabric_site_id_mismatch",
                        format!(
                            "site_fabric_site_id_mismatch:{}:{}:{}",
                            existing, fragment_site_id, path
                        ),
                    ));
                }
            }
            site_id = Some(fragment_site_id);
        }
        let fragment_servers = fragment_obj
            .get("mcpServers")
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();
        for (surface_id, server) in fragment_servers {
            if let Some(previous) = source_by_surface.get(&surface_id) {
                return Err(Diagnostic::new(
                    "site_fabric_duplicate_surface",
                    format!(
                        "site_fabric_duplicate_surface:{}:{}:{}",
                        surface_id, previous, path
                    ),
                ));
            }
            servers.insert(surface_id.clone(), server);
            source_by_surface.insert(surface_id, path.clone());
        }
    }
    let schema = if paths.len() == 1 {
        "narada.mcp_loader.site_fabric.v1"
    } else {
        "narada.mcp_loader.fragmented_site_fabric.v1"
    };
    let mut fabric = Map::new();
    fabric.insert("schema".to_string(), json!(schema));
    fabric.insert(
        "site_id".to_string(),
        site_id.map(Value::String).unwrap_or(Value::Null),
    );
    fabric.insert("mcpServers".to_string(), Value::Object(servers));
    Ok(FabricBundle {
        fabric,
        paths,
        source_by_surface,
    })
}

pub(crate) fn resolve_site_fabric_paths(site_root: &str) -> Result<Vec<String>, Diagnostic> {
    let mcp_dir = join_path(site_root, ".ai/mcp");
    let canonical = join_path(&mcp_dir, "config.json");
    let canonical_exists = Path::new(&canonical).exists();
    let canonical_has_servers = if canonical_exists {
        site_fabric_has_declared_servers(&canonical)
    } else {
        None
    };
    if canonical_exists && canonical_has_servers != Some(false) {
        return Ok(vec![canonical]);
    }
    let site_base = Path::new(site_root)
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("site")
        .replace('.', "-");
    let aggregate = join_path(&mcp_dir, &format!("{}-mcp.json", site_base));
    if Path::new(&aggregate).exists() {
        return Ok(vec![aggregate]);
    }
    if !Path::new(&mcp_dir).exists() {
        return Err(Diagnostic::new(
            "site_fabric_not_found",
            format!("site_fabric_not_found:{}", canonical),
        ));
    }
    let mut candidates = Vec::new();
    if let Ok(entries) = read_dir(&mcp_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            let name = path
                .file_name()
                .and_then(|value| value.to_str())
                .unwrap_or_default();
            if !name.ends_with("-mcp.json") {
                continue;
            }
            let Some(path_string) = path.to_str().map(normalize_path) else {
                continue;
            };
            let Ok(text) = read_to_string(&path_string) else {
                continue;
            };
            let Ok(value) = serde_json::from_str::<Value>(&text) else {
                continue;
            };
            if value.get("mcpServers").and_then(Value::as_object).is_some() {
                candidates.push(path_string);
            }
        }
    }
    candidates.sort();
    if !candidates.is_empty() {
        return Ok(candidates);
    }
    if canonical_exists {
        return Ok(vec![canonical]);
    }
    Err(Diagnostic::new(
        "site_fabric_not_found",
        format!("site_fabric_not_found:{}", canonical),
    ))
}

pub(crate) fn site_fabric_has_declared_servers(path: &str) -> Option<bool> {
    let text = read_to_string(path).ok()?;
    let value = serde_json::from_str::<Value>(&text).ok()?;
    Some(
        value
            .get("mcpServers")
            .and_then(Value::as_object)
            .map(|servers| !servers.is_empty())
            .unwrap_or(false),
    )
}

pub(crate) fn find_site_server(
    servers: &JsonObject,
    requested: &str,
) -> Result<Option<(String, Value)>, Diagnostic> {
    if let Some(server) = servers.get(requested) {
        return Ok(Some((requested.to_string(), server.clone())));
    }
    let mut matches = Vec::new();
    for (server_name, server) in servers {
        if server.get("surface_id").and_then(Value::as_str) == Some(requested) {
            matches.push((server_name.clone(), server.clone()));
        }
    }
    if matches.len() > 1 {
        let names = matches
            .iter()
            .map(|(name, _)| name.clone())
            .collect::<Vec<_>>();
        return Err(Diagnostic::new(
            "surface_id_ambiguous",
            format!("surface_id_ambiguous:{}", requested),
        )
        .with_details(json!({"surface_id":requested,"server_names":names})));
    }
    Ok(matches.into_iter().next())
}
