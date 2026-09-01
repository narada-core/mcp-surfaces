fn carrier_servers(value: &Value) -> serde_json::Map<String, Value> {
    value
        .get("mcpServers")
        .or_else(|| value.get("mcp"))
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default()
}

fn canonical_json(value: &Value) -> String {
    match value {
        Value::Array(values) => format!(
            "[{}]",
            values
                .iter()
                .map(canonical_json)
                .collect::<Vec<_>>()
                .join(",")
        ),
        Value::Object(values) => {
            let mut keys = values.keys().collect::<Vec<_>>();
            keys.sort();
            format!(
                "{{{}}}",
                keys.into_iter()
                    .map(|key| format!(
                        "{}:{}",
                        serde_json::to_string(key).unwrap(),
                        canonical_json(&values[key])
                    ))
                    .collect::<Vec<_>>()
                    .join(",")
            )
        }
        _ => serde_json::to_string(value).unwrap(),
    }
}

fn parse_carrier_config(kind: &str, content: &str) -> Option<Value> {
    match kind {
        "opencode" | "kimi" => parse_jsonc(content),
        "codex" => Some(parse_codex_toml(content)),
        _ => None,
    }
}

fn parse_codex_toml(content: &str) -> Value {
    let mut servers = serde_json::Map::new();
    let mut current: Option<String> = None;
    for raw in content.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if line.starts_with("[mcp_servers.") && line.ends_with(']') {
            let key = &line[13..line.len() - 1];
            if key.contains(".tools.") {
                current = None;
            } else {
                current = Some(key.to_string());
                servers.insert(key.to_string(), json!({}));
            }
            continue;
        }
        let Some(key) = current.as_ref() else {
            continue;
        };
        let Some((field, raw_value)) = line.split_once('=') else {
            continue;
        };
        let field = field.trim();
        let raw_value = raw_value.trim();
        let value =
            serde_json::from_str(raw_value).unwrap_or_else(|_| json!(raw_value.trim_matches('"')));
        servers
            .get_mut(key)
            .and_then(Value::as_object_mut)
            .unwrap()
            .insert(field.to_string(), value);
    }
    json!({"mcpServers":servers})
}

fn carrier_validate(contract: &Value, args: &Value) -> Result<Value, String> {
    let carrier_id = required_argument(args, "carrier_id", "registrar_requires_carrier_id")?;
    let include_ok = args.get("include_ok").and_then(Value::as_bool) == Some(true);
    let carriers = contract
        .pointer("/read_models/registrar_carrier_list/items")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    if !carriers
        .iter()
        .any(|candidate| candidate["carrier_id"] == carrier_id)
    {
        return Err(format!("registrar_unknown_carrier:{carrier_id}"));
    }
    let surface_catalog = contract
        .pointer("/read_models/registrar_surface_list/items")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let servers = contract
        .pointer(&format!(
            "/read_models/registrar_carrier_validation_plans/{carrier_id}/servers"
        ))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut findings = vec![];
    let mut add = |severity: &str, code: &str, message: String, detail: Value| {
        let mut finding = json!({"severity":severity,"code":code,"message":message});
        if let Some(values) = detail.as_object() {
            finding.as_object_mut().unwrap().extend(values.clone())
        }
        findings.push(finding)
    };
    let mut seen = std::collections::HashMap::<String, String>::new();
    for server in &servers {
        let key = server["server_key"].as_str().unwrap_or("");
        let surface_id = server["surface_id"].as_str().unwrap_or(key);
        let detail = merge_value(
            json!({"server_key":key,"surface_id":surface_id}),
            scope_finding_detail(server["narada_scope"].clone()),
        );
        if let Some(previous) = seen.insert(key.to_string(), surface_id.to_string()) {
            add(
                "error",
                "registrar_duplicate_server_key",
                format!("Server key '{key}' is produced by both '{previous}' and '{surface_id}'"),
                detail.clone(),
            );
        } else if include_ok {
            add(
                "info",
                "registrar_server_key_ok",
                format!("Server key '{key}' resolved for surface '{surface_id}'"),
                detail.clone(),
            );
        }
    }
    for server in &servers {
        let key = server["server_key"].as_str().unwrap_or("");
        let surface_id = server["surface_id"].as_str().unwrap_or(key);
        let detail = merge_value(
            json!({"server_key":key,"surface_id":surface_id}),
            scope_finding_detail(server["narada_scope"].clone()),
        );
        let entrypoint = canonical_root(PathBuf::from(server["entrypoint"].as_str().unwrap_or("")));
        if !entrypoint.exists() {
            add(
                "error",
                "registrar_missing_entrypoint",
                format!(
                    "Entrypoint for '{key}' does not exist: {}",
                    path_text(&entrypoint)
                ),
                merge_value(detail.clone(), json!({"entrypoint":path_text(&entrypoint)})),
            );
        } else if include_ok {
            add(
                "info",
                "registrar_entrypoint_exists",
                format!("Entrypoint for '{key}' exists: {}", path_text(&entrypoint)),
                merge_value(detail.clone(), json!({"entrypoint":path_text(&entrypoint)})),
            );
        }
        let known = surface_catalog
            .iter()
            .find(|surface| surface["id"] == surface_id);
        add_runtime_preflight(
            &mut add,
            include_ok,
            merge_value(detail.clone(), json!({"entrypoint":path_text(&entrypoint)})),
            known,
            server["uses_runtime_proxy"].as_bool() == Some(true),
        );
        let child_args = server["args"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .collect::<Vec<_>>();
        if [
            "local-filesystem",
            "git",
            "structured-command",
            "delegated-task",
            "worker-delegation",
        ]
        .contains(&surface_id)
        {
            let roots = flag_values(&child_args, "--allowed-root");
            if roots.is_empty() {
                add("error", "registrar_missing_allowed_root", format!("Surface '{surface_id}' requires at least one --allowed-root but '{key}' has none"), detail.clone());
            } else if include_ok {
                add(
                    "info",
                    "registrar_allowed_root_ok",
                    format!(
                        "Surface '{surface_id}' on '{key}' has {} allowed root(s)",
                        roots.len()
                    ),
                    merge_value(detail.clone(), json!({"allowed_roots":roots})),
                );
            }
        }
        if surface_id == "local-filesystem" || surface_id == "local-filesystem-mcp.local" {
            if !child_args.contains(&"--output-root") {
                add(
                    "warning",
                    "registrar_missing_output_root",
                    format!("Filesystem surface '{key}' is missing --output-root"),
                    detail.clone(),
                );
            } else if include_ok {
                add(
                    "info",
                    "registrar_output_root_ok",
                    format!("Filesystem surface '{key}' has --output-root"),
                    detail.clone(),
                );
            }
        }
    }
    let errors = findings
        .iter()
        .filter(|finding| finding["severity"] == "error")
        .count();
    let warnings = findings
        .iter()
        .filter(|finding| finding["severity"] == "warning")
        .count();
    Ok(
        json!({"schema":"narada.registrar.carrier_validation.v1","status":if errors>0{"invalid"}else if warnings>0{"valid_with_warnings"}else{"valid"},"carrier_id":carrier_id,"server_count":servers.len(),"errors":errors,"warnings":warnings,"findings":findings,"bounded":true}),
    )
}
fn site_catalog(contract: &Value) -> Value {
    let fallback = &contract["read_models"]["registrar_site_list_fallback"];
    let registry_path = env::var_os("NARADA_SITE_REGISTRY_DB")
        .map(PathBuf::from)
        .unwrap_or_else(|| user_site_root().join("registry.db"));
    if !registry_path.exists() {
        return fallback_site_list(fallback, &registry_path, "registry_file_missing");
    }
    match read_site_registry(&registry_path, fallback) {
        Ok(mut items) => {
            for site in &mut items {
                let root = PathBuf::from(site["root"].as_str().unwrap_or(""));
                let fallback = site["site_id"].as_str().unwrap_or("");
                let site_id = canonical_site_id(&root, fallback);
                match scan_site_surfaces(contract, &root, &site_id) {
                    Ok(surfaces) => {
                        let surface_count = surfaces.len();
                        site["surfaces"] = json!(surfaces);
                        site["surface_count"] = json!(surface_count);
                        site["surfaces_status"] = json!("current");
                    }
                    Err(message) => {
                        site["surfaces"] = json!([]);
                        site["surface_count"] = json!(0);
                        site["surfaces_status"] = json!("unavailable");
                        site["surfaces_error"] = json!(message);
                    }
                }
            }
            json!({
                "items": items,
                "count": items.len(),
                "catalog_source": "user_site_site_registry",
                "registry_path": path_text(&registry_path),
                "compatibility_fallback_used": false
            })
        }
        Err(message) => fallback_site_list(fallback, &registry_path, &message),
    }
}

