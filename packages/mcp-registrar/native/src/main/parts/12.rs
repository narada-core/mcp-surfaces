fn canonical_site_id(root: &Path, fallback: &str) -> String {
    for path in [
        root.join(".narada").join("site.json"),
        root.join("site.json"),
    ] {
        if let Ok(text) = fs::read_to_string(path) {
            if let Ok(value) = serde_json::from_str::<Value>(&text) {
                if let Some(site_id) = value
                    .get("site_id")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                {
                    return site_id.to_string();
                }
            }
        }
    }
    fallback.to_string()
}

fn site_mcp_control_root(root: &Path) -> PathBuf {
    if root
        .file_name()
        .is_some_and(|name| name.to_string_lossy().eq_ignore_ascii_case(".narada"))
        || root.join(".ai").join("mcp").exists()
    {
        return root.to_path_buf();
    }
    let nested = root.join(".narada");
    if nested.join(".ai").join("mcp").exists() {
        nested
    } else {
        root.to_path_buf()
    }
}
fn site_output_reader_closure_check(contract: &Value, args: &Value) -> Result<Value, String> {
    let include_ok = args.get("include_ok").and_then(Value::as_bool) == Some(true);
    let mut requested: Vec<(Option<String>, PathBuf)> = vec![];
    let mut add = |site_id: Option<String>, root: PathBuf| {
        let registry = capability_registry_path(&root);
        if !requested.iter().any(|(_, existing)| {
            comparable_root(&capability_registry_path(existing)) == comparable_root(&registry)
        }) {
            requested.push((site_id, canonical_root(root)));
        }
    };
    let catalog = site_catalog(contract);
    let sites = catalog["items"].as_array().cloned().unwrap_or_default();
    for site_id in argument_strings(args, "site_id", "site_ids") {
        let site = lookup_site_value(&sites, &site_id)?;
        add(
            Some(site["site_id"].as_str().unwrap_or(&site_id).to_string()),
            PathBuf::from(site["root"].as_str().unwrap_or("")),
        );
    }
    for root in argument_strings(args, "site_root", "site_roots") {
        let path = canonical_root(PathBuf::from(&root));
        let known = sites
            .iter()
            .find(|site| {
                let candidate = PathBuf::from(site["root"].as_str().unwrap_or(""));
                comparable_root(&path) == comparable_root(&candidate)
                    || comparable_root(&path) == comparable_root(&candidate.join(".narada"))
            })
            .and_then(|site| site["site_id"].as_str())
            .map(str::to_string);
        add(known, path);
    }
    if requested.is_empty() {
        return Err("registrar_requires_site_for_output_reader_closure_check".into());
    }
    let mut site_results = vec![];
    let mut violations = vec![];
    let mut missing_count = 0;
    let mut drift_count = 0;
    let mut checked_surface_count = 0;
    for (site_id, root) in &requested {
        let registry_path = capability_registry_path(root);
        if !registry_path.exists() {
            missing_count += 1;
            site_results.push(json!({"status":"missing","site_id":site_id,"site_root":path_text(root),"registry_path":path_text(&registry_path),"violation":"missing_registry"}));
            continue;
        }
        let registry = fs::read_to_string(&registry_path)
            .ok()
            .and_then(|text| serde_json::from_str::<Value>(&text).ok());
        let Some(registry) = registry else {
            drift_count += 1;
            let invalid = json!({"status":"drift","site_id":site_id,"site_root":path_text(root),"registry_path":path_text(&registry_path),"violation":"invalid_registry_json"});
            site_results.push(invalid.clone());
            violations.push(invalid);
            continue;
        };
        let check = output_reader_closure_for_registry(
            contract,
            &registry,
            site_id.as_deref(),
            Some(root),
            Some(&registry_path),
        );
        checked_surface_count += check["checked_surface_count"].as_u64().unwrap_or(0);
        if check["status"] == "drift" {
            drift_count += 1;
        }
        if let Some(items) = check["violations"].as_array() {
            violations.extend(items.iter().cloned());
        }
        if check["status"] != "ok" || include_ok {
            site_results.push(check);
        }
    }
    Ok(json!({
        "schema":"narada.registrar.site_output_reader_closure_check.v1",
        "status":if drift_count>0{"drift"}else if missing_count>0{"missing"}else{"ok"},
        "checked_site_count":requested.len(),
        "checked_surface_count":checked_surface_count,
        "missing_count":missing_count,
        "drift_count":drift_count,
        "violation_count":violations.len(),
        "violations":violations,
        "sites":site_results
    }))
}

fn output_reader_closure_for_registry(
    contract: &Value,
    registry: &Value,
    site_id: Option<&str>,
    site_root: Option<&Path>,
    registry_path: Option<&Path>,
) -> Value {
    let raw_surfaces = registry.get("surfaces");
    let mut violations = vec![];
    let mut producer_rule_count = 0;
    let context = |surface: Option<&Value>, producer: Option<&str>, reader: Option<&str>| {
        json!({
            "site_id":site_id,
            "site_root":site_root.map(path_text),
            "registry_path":registry_path.map(path_text),
            "surface_id":surface.and_then(|v|v["surface_id"].as_str()),
            "server_name":surface.and_then(|v|v["server_name"].as_str()),
            "catalog_surface_id":surface.and_then(|v|v["catalog_surface_id"].as_str()),
            "producer_tool":producer,
            "required_reader_tool":reader
        })
    };
    if !raw_surfaces.is_some_and(Value::is_array) {
        let mut violation = context(None, None, None);
        violation
            .as_object_mut()
            .unwrap()
            .insert("violation".into(), json!("invalid_registry_surfaces"));
        violations.push(violation);
    } else if let Some(surfaces) = raw_surfaces.and_then(Value::as_array) {
        for surface in surfaces {
            let registered = unique(
                surface["registered_live_tools"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_str),
            );
            let read_only = unique(
                surface
                    .pointer("/tool_contract/read_only_tools")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_str),
            );
            let closure = output_reader_closure(contract, surface, &registered);
            producer_rule_count += closure.len();
            for (producer, reader_value) in closure {
                let Some(reader) = reader_value.as_str() else {
                    continue;
                };
                if !registered.iter().any(|value| value == &producer) {
                    continue;
                }
                let base = context(Some(surface), Some(&producer), Some(reader));
                if !registered.iter().any(|value| value == reader) {
                    let mut violation = base.clone();
                    violation
                        .as_object_mut()
                        .unwrap()
                        .insert("violation".into(), json!("missing_registered_live_tool"));
                    violations.push(violation);
                }
                if !read_only.iter().any(|value| value == reader) {
                    let mut violation = base;
                    violation
                        .as_object_mut()
                        .unwrap()
                        .insert("violation".into(), json!("missing_read_only_admission"));
                    violations.push(violation);
                }
            }
        }
    }
    json!({"schema":"narada.registrar.output_reader_closure_check.v1","status":if violations.is_empty(){"ok"}else{"drift"},"site_id":site_id,"site_root":site_root.map(path_text),"registry_path":registry_path.map(path_text),"checked_surface_count":raw_surfaces.and_then(Value::as_array).map_or(0,Vec::len),"producer_rule_count":producer_rule_count,"violation_count":violations.len(),"violations":violations})
}

fn output_reader_closure(
    contract: &Value,
    surface: &Value,
    registered: &[String],
) -> serde_json::Map<String, Value> {
    let catalog_id = surface["catalog_surface_id"].as_str().unwrap_or("");
    let items = contract
        .pointer("/read_models/registrar_surface_list/items")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    if let Some(closure) = items
        .iter()
        .find(|item| item["id"] == catalog_id)
        .and_then(|item| item["output_reader_closure"].as_object())
    {
        return closure.clone();
    }
    items
        .iter()
        .filter_map(|item| item["output_reader_closure"].as_object())
        .find(|closure| closure.keys().any(|producer| registered.contains(producer)))
        .cloned()
        .unwrap_or_default()
}

fn argument_strings(args: &Value, singular: &str, plural: &str) -> Vec<String> {
    let values = args
        .get(singular)
        .and_then(Value::as_str)
        .into_iter()
        .chain(
            args.get(plural)
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(Value::as_str),
        );
    unique(
        values
            .filter(|value| !value.trim().is_empty())
            .map(str::trim),
    )
}

fn lookup_site_value(sites: &[Value], requested: &str) -> Result<Value, String> {
    if requested == "narada-andrey" || requested == "narada-user-site" {
        return Err("registrar_legacy_site_id_rejected:site_id".into());
    }
    for site in sites {
        let fallback = site["site_id"].as_str().unwrap_or("");
        let canonical = canonical_site_id(Path::new(site["root"].as_str().unwrap_or("")), fallback);
        if requested == fallback
            || requested == canonical
            || requested == format!("narada-{canonical}")
        {
            let mut found = site.clone();
            found
                .as_object_mut()
                .unwrap()
                .insert("site_id".into(), json!(canonical));
            return Ok(found);
        }
    }
    Err(format!("registrar_unknown_site:{requested}"))
}

fn capability_registry_path(root: &Path) -> PathBuf {
    let root = canonical_root(root.to_path_buf());
    root.join(".narada")
        .join("capabilities")
        .join("mcp-surfaces.json")
}
