fn site_list(contract: &Value, args: &Value) -> Value {
    let catalog = site_catalog(contract);
    let items = catalog["items"].as_array().cloned().unwrap_or_default();
    let mut result = paginated_catalog("narada.registrar.site_list.v1", items, args, |site| {
        json!({
            "site_id":site["site_id"],
            "root":site["root"],
            "surface_count":site["surface_count"],
            "surfaces_status":site["surfaces_status"]
        })
    });
    result["catalog_source"] = catalog["catalog_source"].clone();
    result["registry_path"] = catalog["registry_path"].clone();
    result["compatibility_fallback_used"] = catalog["compatibility_fallback_used"].clone();
    result
}

fn fallback_site_list(fallback: &Value, path: &Path, error_message: &str) -> Value {
    json!({
        "items": fallback["items"],
        "count": fallback["count"],
        "catalog_source": "legacy_compatibility_catalog",
        "registry_path": path_text(path),
        "compatibility_fallback_used": true,
        "catalog_error": error_message
    })
}

#[allow(clippy::let_and_return)]
fn read_site_registry(path: &Path, fallback: &Value) -> Result<Vec<Value>, String> {
    let connection = rusqlite::Connection::open_with_flags(
        path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|error| error.to_string())?;
    let has_lifecycle = {
        let mut statement = connection
            .prepare("PRAGMA table_info(site_registry)")
            .map_err(|error| error.to_string())?;
        let columns = statement
            .query_map([], |row| row.get::<_, String>(1))
            .map_err(|error| error.to_string())?;
        let found = columns
            .filter_map(Result::ok)
            .any(|name| name == "lifecycle_status");
        found
    };
    let sql = if has_lifecycle {
        "SELECT site_id, site_root, lifecycle_status FROM site_registry ORDER BY created_at ASC, site_id ASC"
    } else {
        "SELECT site_id, site_root, NULL FROM site_registry ORDER BY created_at ASC, site_id ASC"
    };
    let mut statement = connection.prepare(sql).map_err(|error| error.to_string())?;
    let rows = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, Option<String>>(0)?,
                row.get::<_, Option<String>>(1)?,
                row.get::<_, Option<String>>(2)?,
            ))
        })
        .map_err(|error| error.to_string())?;
    let known = fallback["items"].as_array().cloned().unwrap_or_default();
    let mut items = vec![];
    for row in rows {
        let (site_id, root, lifecycle) = row.map_err(|error| error.to_string())?;
        let site_id = site_id.unwrap_or_default().trim().to_string();
        let root = root.unwrap_or_default().trim().to_string();
        let lifecycle = lifecycle
            .unwrap_or_else(|| "active".into())
            .trim()
            .to_ascii_lowercase();
        if site_id.is_empty() || root.is_empty() || lifecycle != "active" {
            continue;
        }
        let root = canonical_root(PathBuf::from(root));
        let template = known.iter().find(|site| {
            site["root"].as_str().is_some_and(|known_root| {
                comparable_root(Path::new(known_root)) == comparable_root(&root)
            })
        });
        let config_path = site_config_path(&root);
        let fallback_overrides = template
            .and_then(|site| site.get("surface_overrides"))
            .cloned()
            .unwrap_or_else(|| json!({}));
        let overrides = read_surface_overrides(&config_path, fallback_overrides)?;
        items.push(json!({
            "site_id": site_id,
            "root": path_text(&root),
            "config_path": path_text(&config_path),
            "surfaces": template.and_then(|site| site.get("surfaces")).cloned().unwrap_or_else(||json!([])),
            "surface_overrides": overrides
        }));
        if let Some(allowlist) = template.and_then(|site| site.get("local_surface_allowlist")) {
            if !allowlist.is_null() {
                items
                    .last_mut()
                    .unwrap()
                    .as_object_mut()
                    .unwrap()
                    .insert("local_surface_allowlist".into(), allowlist.clone());
            }
        }
    }
    Ok(items)
}

fn read_surface_overrides(config_path: &Path, fallback: Value) -> Result<Value, String> {
    if !config_path.exists() {
        return Ok(fallback);
    }
    let text = fs::read_to_string(config_path).map_err(|error| error.to_string())?;
    let parsed: Value =
        serde_json::from_str(text.trim_start_matches('\u{feff}')).map_err(|error| {
            format!(
                "registrar_site_config_parse_failed:{}:{error}",
                path_text(config_path)
            )
        })?;
    let mut overrides = fallback.as_object().cloned().unwrap_or_default();
    if let Some(entries) = parsed.get("surface_overrides").and_then(Value::as_object) {
        for (surface_id, value) in entries {
            let enabled = value
                .get("enabled")
                .and_then(Value::as_bool)
                .ok_or_else(|| format!("registrar_site_surface_override_invalid:{surface_id}"))?;
            let mut item = json!({"enabled": enabled});
            if let Some(implementation) =
                value.get("surface_implementation").and_then(Value::as_str)
            {
                if implementation != "js" && implementation != "native" {
                    return Err(format!(
                        "registrar_site_surface_override_invalid:{surface_id}"
                    ));
                }
                item.as_object_mut()
                    .unwrap()
                    .insert("surface_implementation".into(), json!(implementation));
            }
            overrides.insert(surface_id.clone(), item);
        }
    }
    Ok(Value::Object(overrides))
}

fn user_site_root() -> PathBuf {
    env::var_os("NARADA_USER_SITE_ROOT")
        .or_else(|| {
            env::var_os("USERPROFILE")
                .map(|home| PathBuf::from(home).join("Narada").into_os_string())
        })
        .or_else(|| {
            env::var_os("HOME").map(|home| PathBuf::from(home).join("Narada").into_os_string())
        })
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(".narada/user-site"))
}

fn canonical_root(path: PathBuf) -> PathBuf {
    let absolute = if path.is_absolute() {
        path
    } else {
        env::current_dir().unwrap_or_default().join(path)
    };
    if absolute
        .file_name()
        .is_some_and(|name| name.to_string_lossy().eq_ignore_ascii_case(".narada"))
    {
        absolute.parent().unwrap_or(&absolute).to_path_buf()
    } else {
        absolute
    }
}

fn site_config_path(root: &Path) -> PathBuf {
    let nested = root.join(".narada").join("config.json");
    if nested.exists() {
        nested
    } else {
        root.join("config.json")
    }
}

fn comparable_root(path: &Path) -> String {
    path_text(&canonical_root(path.to_path_buf()))
        .trim_end_matches('/')
        .to_ascii_lowercase()
}

fn path_text(path: &Path) -> String {
    path.to_string_lossy().replace('/', "\\")
}
fn site_surfaces(contract: &Value, args: &Value) -> Result<Value, String> {
    let requested = args
        .get("site_id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| "registrar_requires_site_id".to_string())?;
    if requested == "narada-andrey" || requested == "narada-user-site" {
        return Err("registrar_legacy_site_id_rejected:site_id".into());
    }
    let catalog = site_catalog(contract);
    let candidates = catalog["items"].as_array().cloned().unwrap_or_default();
    let mut site = None;
    for candidate in candidates {
        let root = candidate["root"].as_str().unwrap_or("");
        let fallback_id = candidate["site_id"].as_str().unwrap_or("");
        let canonical_id = canonical_site_id(Path::new(root), fallback_id);
        if fallback_id == requested
            || canonical_id == requested
            || format!("narada-{canonical_id}") == requested
        {
            site = Some((candidate, canonical_id));
            break;
        }
    }
    let Some((site, site_id)) = site else {
        return Err(format!("registrar_unknown_site:{requested}"));
    };
    let root = PathBuf::from(site["root"].as_str().unwrap_or(""));
    let found = scan_site_surfaces(contract, &root, &site_id)?;
    let count = found.len();
    Ok(
        json!({"schema":"narada.registrar.site_surfaces.v1","status":"ok","site_id":site_id,"surfaces":found,"count":count,"bounded":true}),
    )
}

fn scan_site_surfaces(contract: &Value, root: &Path, site_id: &str) -> Result<Vec<String>, String> {
    let control_root = site_mcp_control_root(root);
    let config_dir = control_root.join(".ai").join("mcp");
    if !config_dir.exists() {
        return Ok(Vec::new());
    }
    let surface_ids = contract["read_models"]["registrar_surface_list"]["items"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|surface| surface["id"].as_str())
        .collect::<Vec<_>>();
    let prefix = if site_id == "andrey-user" {
        "narada-site-andrey-user".to_string()
    } else if site_id.starts_with("narada-") {
        site_id.to_string()
    } else {
        format!("narada-{site_id}")
    };
    let mut found: Vec<String> = vec![];
    let entries = fs::read_dir(&config_dir).map_err(|error| error.to_string())?;
    for entry in entries.filter_map(Result::ok) {
        if entry.path().extension().and_then(|value| value.to_str()) != Some("json") {
            continue;
        }
        let Ok(text) = fs::read_to_string(entry.path()) else {
            continue;
        };
        let Ok(config) = serde_json::from_str::<Value>(&text) else {
            continue;
        };
        let servers = config.get("mcpServers").and_then(Value::as_object);
        for surface_id in &surface_ids {
            let key = format!("{prefix}-{surface_id}");
            if servers.is_some_and(|value| value.contains_key(&key))
                && !found.iter().any(|value| value == surface_id)
            {
                found.push((*surface_id).to_string());
            }
        }
    }
    Ok(found)
}

