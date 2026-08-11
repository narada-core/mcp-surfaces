use serde_json::{json, Value};
use std::env;
use std::fs;
use std::io::{self, BufRead, Read, Write};
use std::path::{Path, PathBuf};

const CONTRACT: &[u8] = include_bytes!("../tool-catalog.json.gz");

fn main() {
    if let Err(error) = run() {
        eprintln!("{error}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), String> {
    let stdin = io::stdin();
    let mut input = stdin.lock();
    let stdout = io::stdout();
    let mut output = stdout.lock();
    loop {
        let Some(request) = read_message(&mut input)? else {
            break;
        };
        if request
            .get("method")
            .and_then(Value::as_str)
            .is_some_and(|v| v.starts_with("notifications/"))
        {
            continue;
        }
        let response = dispatch(&request);
        let body = serde_json::to_vec(&response).map_err(|e| e.to_string())?;
        write!(output, "Content-Length: {}\r\n\r\n", body.len()).map_err(|e| e.to_string())?;
        output.write_all(&body).map_err(|e| e.to_string())?;
        output.flush().map_err(|e| e.to_string())?;
    }
    Ok(())
}

fn dispatch(request: &Value) -> Value {
    let id = request.get("id").cloned().unwrap_or(Value::Null);
    let contract: Value = match flate2::read::GzDecoder::new(CONTRACT)
        .bytes()
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())
        .and_then(|bytes| serde_json::from_slice(&bytes).map_err(|e| e.to_string()))
    {
        Ok(v) => v,
        Err(e) => return error(id, format!("mcp_registrar_native_contract_invalid:{e}")),
    };
    match request.get("method").and_then(Value::as_str).unwrap_or("") {
        "initialize" => {
            json!({"jsonrpc":"2.0","id":id,"result":{"protocolVersion":request.pointer("/params/protocolVersion").cloned().unwrap_or_else(||json!("2024-11-05")),"capabilities":{"tools":{}},"serverInfo":{"name":"mcp-registrar","version":"0.1.0"}}})
        }
        "tools/list" => json!({"jsonrpc":"2.0","id":id,"result":{"tools":contract["tools"]}}),
        "tools/call" => {
            let name = request
                .pointer("/params/name")
                .and_then(Value::as_str)
                .unwrap_or("");
            if matches!(
                name,
                "registrar_guidance"
                    | "registrar_surface_list"
                    | "registrar_carrier_list"
                    | "registrar_site_list"
            ) {
                let mut guidance = if name == "registrar_guidance" {
                    contract["guidance"].clone()
                } else if name == "registrar_site_list" {
                    site_list(&contract)
                } else {
                    contract["read_models"][name].clone()
                };
                let args = request
                    .pointer("/params/arguments")
                    .cloned()
                    .unwrap_or_else(|| json!({}));
                if name == "registrar_guidance" {
                    guidance.as_object_mut().unwrap().insert("requested".into(),json!({"workflow":args.get("workflow").and_then(Value::as_str).filter(|v|!v.trim().is_empty()).map(str::trim),"tool":args.get("tool").and_then(Value::as_str).filter(|v|!v.trim().is_empty()).map(str::trim)}));
                }
                json!({"jsonrpc":"2.0","id":id,"result":{"content":[{"type":"text","text":serde_json::to_string_pretty(&guidance).unwrap()}],"structuredContent":guidance}})
            } else if name == "registrar_surface_tool_inventory_check" {
                let args = request
                    .pointer("/params/arguments")
                    .cloned()
                    .unwrap_or_else(|| json!({}));
                let result = surface_tool_inventory(&contract, &args);
                json!({"jsonrpc":"2.0","id":id,"result":{"content":[{"type":"text","text":serde_json::to_string_pretty(&result).unwrap()}],"structuredContent":result}})
            } else if name == "registrar_site_surfaces" {
                let args = request
                    .pointer("/params/arguments")
                    .cloned()
                    .unwrap_or_else(|| json!({}));
                match site_surfaces(&contract, &args) {
                    Ok(result) => {
                        json!({"jsonrpc":"2.0","id":id,"result":{"content":[{"type":"text","text":serde_json::to_string_pretty(&result).unwrap()}],"structuredContent":result}})
                    }
                    Err(message) => error(id, message),
                }
            } else if name == "registrar_site_output_reader_closure_check" {
                let args = request
                    .pointer("/params/arguments")
                    .cloned()
                    .unwrap_or_else(|| json!({}));
                match site_output_reader_closure_check(&contract, &args) {
                    Ok(result) => {
                        json!({"jsonrpc":"2.0","id":id,"result":{"content":[{"type":"text","text":serde_json::to_string_pretty(&result).unwrap()}],"structuredContent":result}})
                    }
                    Err(message) => error(id, message),
                }
            } else if name == "registrar_surface_usage" {
                let args = request
                    .pointer("/params/arguments")
                    .cloned()
                    .unwrap_or_else(|| json!({}));
                match surface_usage(&contract, &args) {
                    Ok(result) => {
                        json!({"jsonrpc":"2.0","id":id,"result":{"content":[{"type":"text","text":serde_json::to_string_pretty(&result).unwrap()}],"structuredContent":result}})
                    }
                    Err(message) => error(id, message),
                }
            } else if name == "registrar_site_surface_registry_sync" {
                let args = request
                    .pointer("/params/arguments")
                    .cloned()
                    .unwrap_or_else(|| json!({}));
                match site_surface_registry_sync(&contract, &args) {
                    Ok(result) => {
                        json!({"jsonrpc":"2.0","id":id,"result":{"content":[{"type":"text","text":serde_json::to_string_pretty(&result).unwrap()}],"structuredContent":result}})
                    }
                    Err(message) => error(id, message),
                }
            } else if name == "registrar_site_bind" || name == "registrar_site_unbind" {
                let args = request
                    .pointer("/params/arguments")
                    .cloned()
                    .unwrap_or_else(|| json!({}));
                let result = if name == "registrar_site_bind" {
                    site_bind(&contract, &args)
                } else {
                    site_unbind(&contract, &args)
                };
                match result {
                    Ok(value) => {
                        json!({"jsonrpc":"2.0","id":id,"result":{"content":[{"type":"text","text":serde_json::to_string_pretty(&value).unwrap()}],"structuredContent":value}})
                    }
                    Err(message) => error(id, message),
                }
            } else if name == "registrar_site_mcp_fabric_validate" {
                let args = request
                    .pointer("/params/arguments")
                    .cloned()
                    .unwrap_or_else(|| json!({}));
                match site_mcp_fabric_validate(&contract, &args) {
                    Ok(value) => {
                        json!({"jsonrpc":"2.0","id":id,"result":{"content":[{"type":"text","text":serde_json::to_string_pretty(&value).unwrap()}],"structuredContent":value}})
                    }
                    Err(message) => error(id, message),
                }
            } else if name == "registrar_carrier_validate" {
                let args = request
                    .pointer("/params/arguments")
                    .cloned()
                    .unwrap_or_else(|| json!({}));
                match carrier_validate(&contract, &args) {
                    Ok(value) => {
                        json!({"jsonrpc":"2.0","id":id,"result":{"content":[{"type":"text","text":serde_json::to_string_pretty(&value).unwrap()}],"structuredContent":value}})
                    }
                    Err(message) => error(id, message),
                }
            } else {
                error(
                    id,
                    format!("mcp_registrar_native_tool_not_implemented:{name}"),
                )
            }
        }
        method => error(id, format!("unsupported_mcp_method:{method}")),
    }
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
        json!({"status":if errors>0{"invalid"}else if warnings>0{"valid_with_warnings"}else{"valid"},"carrier_id":carrier_id,"server_count":servers.len(),"errors":errors,"warnings":warnings,"findings":findings}),
    )
}
fn site_list(contract: &Value) -> Value {
    let fallback = &contract["read_models"]["registrar_site_list_fallback"];
    let registry_path = env::var_os("NARADA_SITE_REGISTRY_DB")
        .map(PathBuf::from)
        .unwrap_or_else(|| user_site_root().join("registry.db"));
    if !registry_path.exists() {
        return fallback_site_list(fallback, &registry_path, "registry_file_missing");
    }
    match read_site_registry(&registry_path, fallback) {
        Ok(items) => json!({
            "items": items,
            "count": items.len(),
            "catalog_source": "user_site_site_registry",
            "registry_path": path_text(&registry_path),
            "compatibility_fallback_used": false
        }),
        Err(message) => fallback_site_list(fallback, &registry_path, &message),
    }
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
    let catalog = site_list(contract);
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
    let control_root = site_mcp_control_root(&root);
    let config_dir = control_root.join(".ai").join("mcp");
    if !config_dir.exists() {
        return Ok(json!({"site_id":site_id,"surfaces":[],"count":0}));
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
        site_id.clone()
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
    Ok(json!({"site_id":site_id,"surfaces":found,"count":found.len()}))
}

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
    let catalog = site_list(contract);
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
fn surface_usage(contract: &Value, args: &Value) -> Result<Value, String> {
    let surface_id = args
        .get("surface_id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| "registrar_requires_surface_id".to_string())?;
    let is_local = surface_id.ends_with(".local");
    let catalog = site_list(contract);
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
    Ok(
        json!({"surface_id":surface_id,"is_local":is_local,"sites":matching_sites,"carriers":deduped,"site_count":matching_sites.len(),"carrier_count":deduped.len()}),
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
                            && projection["default_injection"] == "all_site_bound_sessions"
                    });
            if automatic && !ids.iter().any(|value| value == id) {
                ids.push(id.into())
            }
        }
    }
    ids
}

fn parse_jsonc(text: &str) -> Option<Value> {
    let mut output = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    let mut quoted = false;
    let mut escaped = false;
    while let Some(ch) = chars.next() {
        if quoted {
            output.push(ch);
            if escaped {
                escaped = false
            } else if ch == '\\' {
                escaped = true
            } else if ch == '"' {
                quoted = false
            };
            continue;
        }
        if ch == '"' {
            quoted = true;
            output.push(ch);
            continue;
        }
        if ch == '/' && chars.peek() == Some(&'/') {
            chars.next();
            for next in chars.by_ref() {
                if next == '\n' {
                    output.push('\n');
                    break;
                }
            }
            continue;
        }
        if ch == '/' && chars.peek() == Some(&'*') {
            chars.next();
            let mut previous = '\0';
            for next in chars.by_ref() {
                if previous == '*' && next == '/' {
                    break;
                }
                previous = next
            }
            continue;
        }
        output.push(ch)
    }
    serde_json::from_str(&output).ok()
}
fn site_mcp_fabric_validate(contract: &Value, args: &Value) -> Result<Value, String> {
    let requested = required_argument(args, "site_id", "registrar_requires_site_id")?;
    let include_ok = args.get("include_ok").and_then(Value::as_bool) == Some(true);
    let catalog = site_list(contract);
    let sites = catalog["items"].as_array().cloned().unwrap_or_default();
    let site = lookup_site_value(&sites, &requested)?;
    let site_id = site["site_id"].as_str().unwrap_or(&requested);
    let root = PathBuf::from(site["root"].as_str().unwrap_or(""));
    let directory = site_mcp_control_root(&root).join(".ai").join("mcp");
    let surface_catalog = contract
        .pointer("/read_models/registrar_surface_list/items")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let servers = discover_fabric_servers(&directory, "site_fabric");
    let carrier_servers =
        discover_fabric_servers(&directory.join("carriers"), "carrier_projection");
    let mut findings = vec![];
    let mut add = |severity: &str, code: &str, message: String, detail: Value| {
        let mut finding = json!({"severity":severity,"code":code,"message":message});
        if let Some(values) = detail.as_object() {
            finding.as_object_mut().unwrap().extend(values.clone())
        }
        findings.push(finding)
    };
    if servers.is_empty() {
        add(
            "warning",
            "registrar_site_fabric_empty",
            format!(
                "No MCP servers found in {}",
                path_text(&root.join(".ai").join("mcp"))
            ),
            json!({"site_id":site_id}),
        );
    }
    let mut seen_keys = std::collections::HashSet::new();
    let mut seen_surfaces = std::collections::HashMap::<String, (String, String)>::new();
    let mut present = std::collections::HashSet::new();
    for server in &servers {
        let key = server["server_key"].as_str().unwrap_or("");
        let surface_id = server["surface_id"].as_str().unwrap_or(key);
        let file = server["source_file"].as_str().unwrap_or("");
        present.insert(surface_id.to_string());
        let detail = merge_value(
            json!({"site_id":site_id,"server_key":key,"source_file":file,"surface_id":surface_id}),
            server_scope_detail(&surface_catalog, server, surface_id, site_id, &root),
        );
        let known = surface_catalog
            .iter()
            .find(|surface| surface["id"] == surface_id);
        if known.is_none() && server["surface_descriptor_path"].is_null() {
            add(
                "error",
                "registrar_site_local_descriptor_missing",
                format!("Site-local surface '{surface_id}' has no governed descriptor"),
                merge_value(
                    detail.clone(),
                    json!({"remediation":"Declare a Site-relative surface_descriptor_path on the Site-local MCP server entry."}),
                ),
            );
        }
        if !seen_keys.insert(key.to_string()) {
            add(
                "error",
                "registrar_site_fabric_duplicate_server_key",
                format!("Duplicate server key '{key}' in site fabric"),
                detail.clone(),
            );
        } else if include_ok {
            add(
                "info",
                "registrar_site_fabric_server_key_ok",
                format!("Server key '{key}' found"),
                detail.clone(),
            );
        }
        if known.is_some() {
            if let Some((old_key, old_file)) = seen_surfaces.get(surface_id) {
                add(
                    "error",
                    "registrar_site_fabric_duplicate_canonical_surface",
                    format!("Multiple Site fabric entries claim canonical surface '{surface_id}'"),
                    merge_value(
                        detail.clone(),
                        json!({"canonical_surface_id":surface_id,"conflicting_server_key":old_key,"conflicting_source_file":old_file,"remediation":format!("Remove the superseded projection from {} and rematerialize from authoritative Site registration.",path_text(&site_mcp_control_root(&root).join(".ai").join("mcp")))}),
                    ),
                );
            } else {
                seen_surfaces.insert(surface_id.to_string(), (key.to_string(), file.to_string()));
            }
        }
        let child_args = server["args"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .collect::<Vec<_>>();
        let entrypoint = server["entrypoint"].as_str().unwrap_or("");
        let unresolved = std::iter::once(entrypoint)
            .chain(child_args.iter().copied())
            .filter(|value| value.contains('{') && value.contains('}'))
            .collect::<Vec<_>>();
        if !unresolved.is_empty() {
            add(
                "error",
                "registrar_site_fabric_unresolved_template",
                format!("Surface {key} contains unresolved materialization tokens"),
                merge_value(
                    detail.clone(),
                    json!({"unresolved_values":unresolved,"remediation":"Regenerate the Site fabric from registrar materialization; do not defer placeholder expansion to the loader."}),
                ),
            );
        }
        let resolved = canonical_root(PathBuf::from(entrypoint));
        if !resolved.exists() {
            add(
                "error",
                "registrar_site_fabric_missing_entrypoint",
                format!(
                    "Entrypoint for '{key}' does not exist: {}",
                    path_text(&resolved)
                ),
                merge_value(detail.clone(), json!({"entrypoint":path_text(&resolved)})),
            );
        } else if include_ok {
            add(
                "info",
                "registrar_site_fabric_entrypoint_exists",
                format!("Entrypoint for '{key}' exists: {}", path_text(&resolved)),
                merge_value(detail.clone(), json!({"entrypoint":path_text(&resolved)})),
            );
        }
        add_runtime_preflight(
            &mut add,
            include_ok,
            merge_value(detail.clone(), json!({"entrypoint":path_text(&resolved)})),
            known,
            server["uses_runtime_proxy"].as_bool() == Some(true),
        );
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
                add("error","registrar_site_fabric_missing_allowed_root",format!("Surface '{surface_id}' requires at least one --allowed-root but '{key}' has none"),detail.clone());
            } else if include_ok {
                add(
                    "info",
                    "registrar_site_fabric_allowed_root_ok",
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
                    "registrar_site_fabric_missing_output_root",
                    format!("Filesystem surface '{key}' is missing --output-root"),
                    detail.clone(),
                );
            } else if include_ok {
                add(
                    "info",
                    "registrar_site_fabric_output_root_ok",
                    format!("Filesystem surface '{key}' has --output-root"),
                    detail.clone(),
                );
            }
        }
        if [
            "agent-context",
            "task-lifecycle",
            "site-inbox",
            "site-loop",
            "mailbox",
            "graph-mail",
            "delegated-task",
        ]
        .contains(&surface_id)
        {
            if !child_args.contains(&"--site-root") {
                add(
                    "error",
                    "registrar_site_fabric_missing_site_root",
                    format!("Surface '{surface_id}' on '{key}' is missing --site-root"),
                    detail.clone(),
                );
            } else if include_ok {
                add(
                    "info",
                    "registrar_site_fabric_site_root_ok",
                    format!("Surface '{surface_id}' on '{key}' has --site-root"),
                    detail.clone(),
                );
            }
        }
    }
    for server in &carrier_servers {
        let surface_id = server["surface_id"].as_str().unwrap_or("");
        let key = server["server_key"].as_str().unwrap_or("");
        let detail = json!({"site_id":site_id,"server_key":key,"surface_id":surface_id,"source_file":server["source_file"],"projection_kind":"carrier_projection"});
        let Some(authority) = surface_catalog
            .iter()
            .find(|surface| surface["id"] == surface_id)
        else {
            add(
                "error",
                "registrar_carrier_projection_unknown_surface",
                format!("Carrier projection '{key}' has no authoritative surface definition"),
                detail,
            );
            continue;
        };
        let actual = server["entrypoint"]
            .as_str()
            .unwrap_or("")
            .replace('\\', "/");
        let expected = authority["entrypoint"]
            .as_str()
            .unwrap_or("")
            .replace(
                "{mcp_surfaces_root}",
                &workspace_repo_root()
                    .map(|root| path_text(&root.join("packages")))
                    .unwrap_or_default(),
            )
            .replace('\\', "/");
        if actual != expected {
            add("error","registrar_carrier_projection_entrypoint_drift",format!("Carrier projection '{key}' does not use the authoritative '{surface_id}' entrypoint"),merge_value(detail.clone(),json!({"entrypoint":actual,"expected_entrypoint":expected,"authoritative_package":authority["package"]})));
        } else if include_ok {
            add(
                "info",
                "registrar_carrier_projection_entrypoint_ok",
                format!(
                    "Carrier projection '{key}' uses the authoritative '{surface_id}' entrypoint"
                ),
                detail.clone(),
            );
        }
        let values = server["args"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .collect::<Vec<_>>();
        if [
            "agent-context",
            "task-lifecycle",
            "site-inbox",
            "site-loop",
            "mailbox",
            "graph-mail",
            "delegated-task",
        ]
        .contains(&surface_id)
            && !values.contains(&"--site-root")
        {
            add(
                "error",
                "registrar_carrier_projection_missing_site_root",
                format!("Carrier projection '{key}' is missing required --site-root"),
                detail,
            );
        }
    }
    for surface in &surface_catalog {
        let Some(id) = surface["id"].as_str() else {
            continue;
        };
        if site
            .pointer(&format!("/surface_overrides/{id}/enabled"))
            .and_then(Value::as_bool)
            == Some(false)
        {
            continue;
        }
        let required = surface["projections"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|projection| {
                projection["injection_scope"] == "local_site"
                    && projection["default_injection"] == "all_site_bound_sessions"
            });
        let Some(projection) = required else { continue };
        if present.contains(id) || (id == "task-lifecycle" && present.contains("work-lifecycle")) {
            continue;
        }
        add("error","registrar_site_fabric_missing_default_surface",format!("Default local Site surface '{id}' is missing from runtime-authoritative Site MCP fabric"),json!({"site_id":site_id,"surface_id":id,"projection_id":projection["id"],"default_injection":projection["default_injection"],"injection_scope":projection["injection_scope"],"expected_server_key":format!("{}-{id}",site_prefix(site_id)),"required_repair_locus":{"kind":"local_site","site_root":site["root"]},"remediation":format!("Materialize '{id}' with projection '{}' into {} before launching Site-bound sessions.",projection["id"].as_str().unwrap_or(""),path_text(&root.join(".ai").join("mcp")))}));
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
        json!({"status":if errors>0{"invalid"}else if warnings>0{"valid_with_warnings"}else{"valid"},"site_id":site_id,"server_count":servers.len(),"carrier_projection_count":carrier_servers.len(),"errors":errors,"warnings":warnings,"findings":findings}),
    )
}

fn discover_fabric_servers(directory: &Path, projection_kind: &str) -> Vec<Value> {
    let mut result = vec![];
    let Ok(entries) = fs::read_dir(directory) else {
        return result;
    };
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
        for (key, server) in config["mcpServers"].as_object().into_iter().flatten() {
            let raw_command = server["command"].as_str().unwrap_or("node");
            let raw_args = server["args"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect::<Vec<_>>();
            let launch = unwrap_launch(raw_command, &raw_args);
            let args = if launch.proxied {
                let separator = raw_args.iter().position(|value| value == "--");
                separator
                    .map(|index| raw_args[index + 1..].to_vec())
                    .unwrap_or_default()
            } else {
                raw_args.iter().skip(1).cloned().collect()
            };
            let surface_id = server["surface_id"].as_str().unwrap_or(key);
            result.push(json!({"server_key":key,"surface_id":surface_id,"entrypoint":launch.entrypoint,"args":args,"uses_runtime_proxy":launch.proxied,"surface_descriptor_path":server.get("surface_descriptor_path"),"narada_scope":server.get("narada_scope"),"surface_projection":server.get("surface_projection"),"source_file":if projection_kind=="carrier_projection"{format!("carriers/{file}")}else{file.to_string()},"projection_kind":projection_kind}));
        }
    }
    result
}
fn server_scope_detail(
    catalog: &[Value],
    server: &Value,
    surface_id: &str,
    site_id: &str,
    root: &Path,
) -> Value {
    let projection_id = server
        .pointer("/surface_projection/projection_id")
        .and_then(Value::as_str)
        .unwrap_or("default");
    let projection = catalog
        .iter()
        .find(|surface| surface["id"] == surface_id)
        .and_then(|surface| surface["projections"].as_array())
        .and_then(|items| {
            items
                .iter()
                .find(|projection| projection["id"] == projection_id)
        });
    let computed = projection.map(|value| scope_metadata(value, root)).unwrap_or_else(|| {
        json!({"injection_scope":"local_site","authority_locus":{"kind":"local_site","site_root":path_text(root)},"mutation_locus":{"kind":"local_site","site_root":path_text(root)},"restart_owner":"local_site"})
    });
    let raw = server["narada_scope"].as_object();
    let injection = raw
        .and_then(|value| value.get("injection_scope"))
        .cloned()
        .unwrap_or_else(|| computed["injection_scope"].clone());
    let authority = raw
        .and_then(|value| value.get("authority_locus"))
        .cloned()
        .unwrap_or_else(|| computed["authority_locus"].clone());
    let mutation = raw
        .and_then(|value| value.get("mutation_locus"))
        .cloned()
        .unwrap_or_else(|| computed["mutation_locus"].clone());
    let restart = raw
        .and_then(|value| value.get("restart_owner"))
        .cloned()
        .unwrap_or_else(|| computed["restart_owner"].clone());
    let bound = raw
        .and_then(|value| value.get("bound_into_site"))
        .cloned()
        .unwrap_or_else(|| json!(site_id));
    let source = if raw.is_some() {
        json!("site_config_narada_scope")
    } else {
        json!("registrar_surface_catalog")
    };
    let narada = json!({"injection_scope":injection,"authority_locus":authority,"mutation_locus":mutation,"restart_owner":restart,"bound_into_site":bound,"scope_source":source});
    json!({"injection_scope":injection,"authority_locus":authority,"mutation_locus":mutation,"restart_owner":restart,"bound_into_site":bound,"scope_source":source,"narada_scope":narada,"diagnostic_class":if injection=="host"{"host_injected_surface_missing_or_misconfigured_in_session"}else if injection=="user_site"{"user_site_injected_surface_missing_or_misconfigured_in_session"}else{"local_site_surface_missing_or_misconfigured"},"required_repair_locus":mutation})
}

fn scope_finding_detail(scope: Value) -> Value {
    let injection = scope["injection_scope"].as_str().unwrap_or("local_site");
    let diagnostic_class = if injection == "host" {
        "host_injected_surface_missing_or_misconfigured_in_session"
    } else if injection == "user_site" {
        "user_site_injected_surface_missing_or_misconfigured_in_session"
    } else {
        "local_site_surface_missing_or_misconfigured"
    };
    let mut detail = scope.clone();
    if let Some(object) = detail.as_object_mut() {
        object.insert("narada_scope".into(), scope.clone());
        object.insert("diagnostic_class".into(), json!(diagnostic_class));
        object.insert(
            "required_repair_locus".into(),
            scope["mutation_locus"].clone(),
        );
    }
    detail
}
fn add_runtime_preflight(
    add: &mut impl FnMut(&str, &str, String, Value),
    include_ok: bool,
    detail: Value,
    surface: Option<&Value>,
    proxied: bool,
) {
    let Some(workspace) = workspace_repo_root() else {
        return;
    };
    if proxied {
        let manifest = workspace
            .join(".ai")
            .join("runtime")
            .join("workspace-artifact-manifest.json");
        let manifest_text = manifest.to_string_lossy().replace('\\', "/");
        if manifest.exists() {
            if include_ok {
                add(
                    "info",
                    "registrar_workspace_artifact_manifest_exists",
                    format!("Workspace artifact manifest exists: {manifest_text}"),
                    merge_value(
                        detail.clone(),
                        json!({"artifact_manifest_path":manifest_text}),
                    ),
                );
            }
        } else {
            add(
                "error",
                "registrar_workspace_artifact_manifest_missing",
                format!("Workspace artifact manifest does not exist: {manifest_text}"),
                merge_value(
                    detail.clone(),
                    json!({"artifact_manifest_path":manifest_text,"remediation":"Run pnpm build from mcp-surfaces before launching carrier MCPs."}),
                ),
            );
        }
        let proxy = native_proxy_entrypoint().unwrap_or_default();
        if Path::new(&proxy).exists() {
            if include_ok {
                add(
                    "info",
                    "registrar_runtime_proxy_exists",
                    format!("Runtime proxy exists: {proxy}"),
                    merge_value(
                        detail.clone(),
                        json!({"runtime_proxy_entrypoint":proxy,"runtime_proxy_implementation":"native"}),
                    ),
                );
            }
        } else {
            add(
                "error",
                "registrar_runtime_proxy_missing",
                format!("Runtime proxy does not exist: {proxy}"),
                merge_value(
                    detail.clone(),
                    json!({"runtime_proxy_entrypoint":proxy,"runtime_proxy_implementation":"native","remediation":"Run pnpm --filter @narada-core/mcp-runtime-proxy build before launching carrier MCPs."}),
                ),
            );
        }
    }
    let Some(surface) = surface else { return };
    for check in runtime_dependency_checks(&workspace, surface) {
        let dependency = check["dependency"].as_str().unwrap_or("").to_string();
        let export = check["export_path"].as_str().unwrap_or("").to_string();
        let mut finding_detail = check.clone();
        finding_detail.as_object_mut().unwrap().remove("exists");
        if check["exists"].as_bool() == Some(true) {
            if include_ok {
                add(
                    "info",
                    "registrar_runtime_dependency_exists",
                    format!("Runtime dependency export for '{dependency}' exists: {export}"),
                    merge_value(detail.clone(), finding_detail),
                );
            }
        } else {
            add(
                "error",
                "registrar_runtime_dependency_missing",
                format!("Runtime dependency export for '{dependency}' does not exist: {export}"),
                merge_value(
                    detail.clone(),
                    merge_value(
                        finding_detail,
                        json!({"remediation":format!("Run pnpm --filter {dependency} build before launching carrier MCPs.")}),
                    ),
                ),
            );
        }
    }
}
fn runtime_dependency_checks(workspace: &Path, surface: &Value) -> Vec<Value> {
    let package = surface["package"].as_str().unwrap_or("");
    let package_root = workspace.join("packages").join(package);
    let Some(manifest) = fs::read_to_string(package_root.join("package.json"))
        .ok()
        .and_then(|text| serde_json::from_str::<Value>(&text).ok())
    else {
        return vec![];
    };
    let mut result = vec![];
    for dependency in manifest["dependencies"]
        .as_object()
        .into_iter()
        .flatten()
        .map(|(name, _)| name)
        .filter(|name| name.starts_with("@narada-core/mcp-"))
    {
        let name = dependency.trim_start_matches("@narada-core/");
        let shared = workspace.join("packages").join("shared").join(name);
        let dependency_root = if shared.join("package.json").exists() {
            shared
        } else {
            workspace.join("packages").join(name)
        };
        let package_path = dependency_root.join("package.json");
        let Some(package_json) = fs::read_to_string(&package_path)
            .ok()
            .and_then(|text| serde_json::from_str::<Value>(&text).ok())
        else {
            result.push(json!({"dependency":dependency,"package_root":dependency_root.to_string_lossy().replace('\\',"/"),"export_path":package_path.to_string_lossy().replace('\\',"/"),"exists":false}));
            continue;
        };
        for target in export_targets(&package_json) {
            let export = dependency_root.join(target.trim_start_matches("./"));
            let export_text = export.to_string_lossy().replace('\\', "/");
            result.push(json!({"dependency":dependency,"package_root":dependency_root.to_string_lossy().replace('\\',"/"),"export_path":export_text,"exists":export_target_exists(&export)}));
        }
    }
    result
}
fn export_targets(package: &Value) -> Vec<String> {
    let mut result = vec![];
    match &package["exports"] {
        Value::String(value) => result.push(value.clone()),
        Value::Object(values) => {
            for value in values.values() {
                if let Some(target) = value.as_str().or_else(|| value["default"].as_str()) {
                    if !result.iter().any(|item| item == target) {
                        result.push(target.into())
                    }
                }
            }
        }
        _ => {}
    }
    result
}
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
    let catalog = site_list(contract);
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
    let file_name = format!("{prefix}-{surface_id}-mcp.json");
    let config = build_bind_config(
        contract,
        &site,
        surface,
        projection,
        runtime_kind,
        &server_key,
    )?;
    fs::write(
        config_dir.join(&file_name),
        serde_json::to_string_pretty(&config).map_err(|error| error.to_string())? + "\n",
    )
    .map_err(|error| error.to_string())?;
    let registry_result = write_site_registry(contract, &site)?;
    Ok(
        json!({"status":"bound","site_id":site_id,"surface_id":surface_id,"projection_id":projection["id"],"file":file_name,"server_key":server_key,"registry":registry_result}),
    )
}

fn site_unbind(contract: &Value, args: &Value) -> Result<Value, String> {
    let site_id = required_argument(args, "site_id", "registrar_requires_site_id")?;
    let surface_id = required_argument(args, "surface_id", "registrar_requires_surface_id")?;
    let catalog = site_list(contract);
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
    let registry = build_site_surface_registry(contract, site)?;
    let path = capability_registry_path(Path::new(site["root"].as_str().unwrap_or("")));
    fs::create_dir_all(
        path.parent()
            .ok_or("registrar_site_registry_path_invalid")?,
    )
    .map_err(|error| error.to_string())?;
    fs::write(
        &path,
        serde_json::to_string_pretty(&registry).map_err(|error| error.to_string())? + "\n",
    )
    .map_err(|error| error.to_string())?;
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
        json!({"status":"synced","site_id":site["site_id"],"path":path_text(&path),"surface_count":surfaces.len(),"tool_count":tools}),
    )
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
                    .map_or(true, Vec::is_empty)
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

fn build_bind_config(
    _contract: &Value,
    site: &Value,
    surface: &Value,
    projection: &Value,
    runtime_kind: Option<&str>,
    server_key: &str,
) -> Result<Value, String> {
    let site_id = site["site_id"].as_str().unwrap_or("");
    let surface_id = surface["id"].as_str().unwrap_or("");
    let root = canonical_root(PathBuf::from(site["root"].as_str().unwrap_or("")));
    let workspace = site_workspace_root(site);
    let source_args = projection
        .get("args")
        .and_then(Value::as_array)
        .or_else(|| surface["args"].as_array())
        .cloned()
        .unwrap_or_default();
    let mut child_args = source_args
        .iter()
        .filter_map(Value::as_str)
        .map(|value| interpolate(value, site_id, &root, &workspace))
        .collect::<Vec<_>>();
    if projection["id"] == "user-site-operator" {
        child_args.extend(
            [
                "--projection",
                "user-site-operator",
                "--user-site-root",
                &path_text(&user_site_root()),
                "--source-kind",
                "operator",
                "--operator-id",
                &default_operator_id(),
            ]
            .map(str::to_string),
        );
    }
    let entrypoint_template = projection["entrypoint"]
        .as_str()
        .or_else(|| surface["entrypoint"].as_str())
        .unwrap_or("");
    let child_entrypoint = canonical_root(PathBuf::from(interpolate(
        entrypoint_template,
        site_id,
        &root,
        &workspace,
    )));
    let implementation = site
        .pointer(&format!(
            "/surface_overrides/{surface_id}/surface_implementation"
        ))
        .and_then(Value::as_str);
    let launch = site_launch(
        surface_id,
        projection,
        implementation,
        &path_text(&child_entrypoint),
        &child_args,
    )?;
    let exposed = projection["exposed_tools"]
        .as_array()
        .cloned()
        .unwrap_or_else(|| surface["tools"].as_array().cloned().unwrap_or_default());
    let scope = scope_metadata(projection, &root);
    let mut envs = surface["env_vars"].as_array().cloned().unwrap_or_default();
    envs.extend(
        projection["env_vars"]
            .as_array()
            .cloned()
            .unwrap_or_default(),
    );
    let envs = unique(envs.iter().filter_map(Value::as_str));
    let projection_metadata = projection_metadata(surface, projection, runtime_kind);
    Ok(
        json!({"schema":"narada.mcp.client_config.v0","site_id":site_id,"description":format!("{} MCP surface bound by registrar.",surface["package"].as_str().unwrap_or("")),"mcpServers":{server_key:{"transport":"stdio","command":launch.0,"args":launch.1,"tools":exposed,"env_vars":envs,"surface_id":surface_id,"projection_id":projection["id"],"surface_projection":projection_metadata,"authority_posture":if scope["injection_scope"]=="local_site"{"site_local_mcp_surface"}else{"injected_mcp_surface"},"injection_scope":scope["injection_scope"],"authority_locus":scope["authority_locus"],"mutation_locus":scope["mutation_locus"],"restart_owner":scope["restart_owner"],"bound_into_site":site_id,"narada_scope":{"injection_scope":scope["injection_scope"],"authority_locus":scope["authority_locus"],"mutation_locus":scope["mutation_locus"],"restart_owner":scope["restart_owner"],"bound_into_site":site_id,"scope_source":"registrar_surface_catalog"}}}}),
    )
}

fn site_workspace_root(site: &Value) -> PathBuf {
    let config = site["config_path"]
        .as_str()
        .and_then(|path| fs::read_to_string(path).ok())
        .and_then(|text| serde_json::from_str::<Value>(&text).ok());
    let configured = config.as_ref().and_then(|value| {
        value["workspace_root"].as_str().or_else(|| {
            value
                .pointer("/site/workspace_root")
                .and_then(Value::as_str)
        })
    });
    canonical_root(PathBuf::from(
        configured.unwrap_or_else(|| site["root"].as_str().unwrap_or("")),
    ))
}
fn interpolate(value: &str, site_id: &str, root: &Path, workspace: &Path) -> String {
    let control = if root
        .file_name()
        .is_some_and(|name| name.to_string_lossy().eq_ignore_ascii_case(".narada"))
    {
        root.to_path_buf()
    } else {
        root.join(".narada")
    };
    value
        .replace(
            "{mcp_surfaces_root}",
            &workspace_repo_root()
                .map(|root| path_text(&root.join("packages")))
                .unwrap_or_default(),
        )
        .replace("{site_root}", &path_text(root))
        .replace("{site_control_root}", &path_text(&control))
        .replace("{site_runtime_root}", &path_text(&control.join("runtime")))
        .replace("{workspace_root}", &path_text(workspace))
        .replace("{site_id}", site_id)
}
fn default_operator_id() -> String {
    user_site_root()
        .parent()
        .and_then(Path::file_name)
        .and_then(|value| value.to_str())
        .unwrap_or("operator")
        .to_ascii_lowercase()
}
fn workspace_repo_root() -> Option<PathBuf> {
    let executable = env::current_exe().ok()?;
    executable
        .ancestors()
        .find(|root| root.join("packages").join("mcp-registrar").exists())
        .map(Path::to_path_buf)
}

fn scope_metadata(projection: &Value, root: &Path) -> Value {
    let injection = projection["injection_scope"]
        .as_str()
        .unwrap_or("local_site");
    let locus = if injection == "host" {
        json!({"kind":"host"})
    } else if injection == "user_site" {
        json!({"kind":"user_site","site_root":path_text(&user_site_root())})
    } else {
        json!({"kind":"local_site","site_root":path_text(root)})
    };
    json!({"injection_scope":injection,"authority_locus":locus,"mutation_locus":locus,"restart_owner":projection["restart_owner"].as_str().unwrap_or(injection)})
}
fn projection_metadata(surface: &Value, projection: &Value, runtime_kind: Option<&str>) -> Value {
    let tools = projection["exposed_tools"]
        .as_array()
        .cloned()
        .unwrap_or_else(|| surface["tools"].as_array().cloned().unwrap_or_default());
    let descriptor = &surface["descriptor"];
    let mut value = json!({"surface_id":surface["id"],"projection_id":projection["id"],"injection_scope":projection["injection_scope"],"runtime_requirements":projection.get("runtime_requirements").cloned().unwrap_or_else(||json!([])),"exposed_tools":tools,"execution":projection["execution"],"descriptor_digest":surface["descriptor_digest"],"tool_contract_digest":surface["tool_contract_digest"],"surface_descriptor":descriptor});
    for key in ["default_injection"] {
        if let Some(item) = projection.get(key) {
            value
                .as_object_mut()
                .unwrap()
                .insert(key.into(), item.clone());
        }
    }
    if let Some(kind) = runtime_kind {
        value
            .as_object_mut()
            .unwrap()
            .insert("runtime_kind".into(), json!(kind));
    }
    if let Some(lifecycle) = descriptor["projections"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|candidate| candidate["id"] == projection["id"])
        .and_then(|candidate| candidate.get("lifecycle"))
    {
        value
            .as_object_mut()
            .unwrap()
            .insert("lifecycle".into(), lifecycle.clone());
    }
    value
}

fn site_launch(
    surface_id: &str,
    projection: &Value,
    implementation: Option<&str>,
    entrypoint: &str,
    args: &[String],
) -> Result<(String, Vec<String>), String> {
    let component = component_kind(surface_id);
    let engine = runtime_engine(&component, implementation)?;
    let proxy = native_proxy_entrypoint().ok_or("registrar_native_runtime_proxy_missing")?;
    let mut effective_command = if engine == "rust" {
        projection["command"].as_str().unwrap_or("node").to_string()
    } else {
        runtime_executable(&engine)?
    };
    let mut effective_entrypoint = entrypoint.to_string();
    let mut effective_args = args.to_vec();
    let mut invocation = None;
    let mut applet = None;
    let shared = [
        "catalog-observation",
        "operator-routing",
        "site-inbox",
        "site-lifecycle",
        "site-registry",
        "project-state",
        "runtime-introspection",
        "site-coherence",
        "launcher",
        "mailbox",
        "graph-mail",
        "calendar",
        "site-loop",
        "worker-delegation",
        "delegated-task",
        "sop",
        "scheduler",
        "surface-feedback",
        "speech",
        "artifacts",
        "nars-session",
        "quota-meter",
        "operator-console-overlay",
        "browser-control",
        "cloudflare-carrier",
    ]
    .contains(&surface_id);
    if engine == "rust" {
        if ["local-filesystem", "structured-command", "git"].contains(&surface_id) {
            effective_command = proxy.clone();
            effective_entrypoint = proxy.clone();
            invocation = Some("native_applet");
            applet = Some(if surface_id == "local-filesystem" {
                "filesystem"
            } else {
                surface_id
            });
        } else if surface_id == "mcp-loader" {
            let path = native_artifact_entrypoint("mcp-loader-mcp", "narada-mcp-loader.exe")
                .ok_or("registrar_native_mcp_loader_missing")?;
            effective_command = path.clone();
            effective_entrypoint = path;
            invocation = Some("native_entrypoint");
        } else if surface_id == "task-lifecycle" || surface_id == "work-lifecycle" {
            let artifact = if surface_id == "task-lifecycle" {
                "narada-task-lifecycle-mcp.exe"
            } else {
                "narada-work-lifecycle-mcp.exe"
            };
            let path = native_artifact_entrypoint("shared/mcp-lifecycle-native", artifact)
                .ok_or("registrar_native_lifecycle_missing")?;
            effective_command = path.clone();
            effective_entrypoint = path;
            invocation = Some("native_entrypoint");
        } else if surface_id == "agent-context" && projection["id"] == "default" {
            let path =
                native_artifact_entrypoint("agent-context-mcp", "narada-agent-context-mcp.exe")
                    .ok_or("registrar_native_agent_context_missing")?;
            effective_command = path.clone();
            effective_entrypoint = path;
            invocation = Some("native_entrypoint");
        } else if shared {
            let path =
                native_artifact_entrypoint("shared/mcp-surfaces-native", "narada-mcp-surfaces.exe")
                    .ok_or("registrar_native_shared_surface_missing")?;
            effective_command = path.clone();
            effective_entrypoint = path;
            effective_args = native_shared_args(surface_id, args);
            invocation = Some("native_entrypoint");
        } else if surface_id == "mcp-registrar" {
            let path = native_artifact_entrypoint("mcp-registrar", "narada-mcp-registrar.exe")
                .ok_or("registrar_native_registrar_missing")?;
            effective_command = path.clone();
            effective_entrypoint = path;
            invocation = Some("native_entrypoint");
        }
    }
    let mut proxy_args = vec![
        "proxy".into(),
        "--surface-id".into(),
        surface_id.into(),
        "--child-command".into(),
        effective_command,
        "--artifact-manifest".into(),
        workspace_repo_root()
            .map(|root| {
                root.join(".ai/runtime/workspace-artifact-manifest.json")
                    .to_string_lossy()
                    .replace('\\', "/")
            })
            .unwrap_or_default(),
        "--runtime-contract-version".into(),
        "6".into(),
        "--entrypoint".into(),
        effective_entrypoint,
    ];
    if let Some(kind) = invocation {
        proxy_args.extend(["--child-invocation-kind", kind].map(str::to_string));
        if kind == "native_applet" {
            proxy_args
                .extend(["--child-applet", applet.unwrap_or("filesystem")].map(str::to_string));
        }
    }
    proxy_args.push("--".into());
    proxy_args.extend(effective_args);
    Ok((proxy, proxy_args))
}

fn native_shared_args(surface_id: &str, args: &[String]) -> Vec<String> {
    let mut result = vec!["--surface-id".into(), surface_id.into()];
    if surface_id == "calendar" || surface_id == "graph-mail" {
        result.push("--native-authority".into())
    }
    let roots = [
        "--site-root",
        "--narada-root",
        "--feedback-root",
        "--output-root",
        "--user-site-root",
        "--repo-root",
        "--sop-root",
        "--task-root",
        "--allowed-root",
    ];
    let forwarded = [
        "--log-root",
        "--registry-path",
        "--projection-id",
        "--canonical-feedback-root",
        "--task-lifecycle-root",
        "--feedback-discovery-root",
        "--site-id",
        "--owned-surface-id",
        "--projection",
        "--source-kind",
        "--operator-id",
        "--run-root",
        "--sops-dir",
        "--provider-registry-path",
        "--server-name",
    ];
    let mut index = 0;
    while index < args.len() {
        let key = &args[index];
        if (roots.contains(&key.as_str()) || forwarded.contains(&key.as_str()))
            && index + 1 < args.len()
            && !args[index + 1].starts_with("--")
        {
            result.push(key.clone());
            result.push(args[index + 1].clone());
            index += 2
        } else {
            index += 1
        }
    }
    result
}
fn component_kind(surface: &str) -> String {
    match surface {
        "mcp-loader" => "mcp-loader-mcp",
        "local-filesystem" => "filesystem-mcp",
        "structured-command" => "structured-command-mcp",
        "git" => "git-mcp",
        "agent-context" => "agent-context-mcp",
        "mcp-registrar" => "mcp-registrar",
        "task-lifecycle" => "task-lifecycle-mcp",
        "work-lifecycle" => "work-lifecycle-mcp",
        value => return format!("{value}-mcp"),
    }
    .into()
}
fn runtime_engine(component: &str, implementation: Option<&str>) -> Result<String, String> {
    let workspace = workspace_repo_root().ok_or("registrar_workspace_root_unavailable")?;
    let path=workspace.parent().unwrap_or(&workspace).join("narada/packages/operator-surface-runtime-contract/contracts/runtime-implementation-matrix.json");
    let matrix: Value =
        serde_json::from_str(&fs::read_to_string(path).map_err(|error| error.to_string())?)
            .map_err(|error| error.to_string())?;
    let component = if implementation == Some("js") {
        "mcp-javascript-surface"
    } else {
        component
    };
    let row = matrix["rows"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|row| row["component_kind"] == component)
        .ok_or_else(|| format!("registrar_runtime_implementation_unavailable:{component}"))?;
    let engine = if implementation == Some("native") {
        "rust"
    } else {
        row.pointer("/profile_runtime_engine_kinds/native")
            .and_then(Value::as_str)
            .unwrap_or("bun")
    };
    if row
        .pointer(&format!("/implementations/{engine}/status"))
        .and_then(Value::as_str)
        != Some("admitted")
    {
        return Err(format!(
            "registrar_runtime_implementation_unavailable:{component}"
        ));
    }
    Ok(engine.into())
}
fn runtime_executable(engine: &str) -> Result<String, String> {
    let override_name = if engine == "bun" {
        "NARADA_BUN_EXECUTABLE"
    } else {
        "NARADA_NODE_EXECUTABLE"
    };
    let candidates = env::var_os(override_name)
        .map(PathBuf::from)
        .into_iter()
        .chain(env::var_os("PATH").into_iter().flat_map(|path| {
            env::split_paths(&path)
                .map(|dir| dir.join(format!("{engine}.exe")))
                .collect::<Vec<_>>()
        }))
        .chain(if engine == "bun" {
            Some(
                user_site_root()
                    .parent()
                    .unwrap_or(Path::new(""))
                    .join(".bun/bin/bun.exe"),
            )
        } else {
            None
        });
    for path in candidates {
        if path.exists() {
            return Ok(path.to_string_lossy().replace('\\', "/"));
        }
    }
    Err(format!("registrar_runtime_executable_unresolved:{engine}"))
}

fn site_surface_registry_sync(contract: &Value, args: &Value) -> Result<Value, String> {
    let requested = args
        .get("site_id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| "registrar_requires_site_id".to_string())?;
    let catalog = site_list(contract);
    let sites = catalog["items"].as_array().cloned().unwrap_or_default();
    let site = lookup_site_value(&sites, requested)?;
    let registry = build_site_surface_registry(contract, &site)?;
    let path = capability_registry_path(Path::new(site["root"].as_str().unwrap_or("")));
    if args.get("dry_run").and_then(Value::as_bool) == Some(true) {
        return Ok(
            json!({"status":"dry_run","site_id":requested,"path":path_text(&path),"registry":registry}),
        );
    }
    fs::create_dir_all(
        path.parent()
            .ok_or("registrar_site_registry_path_invalid")?,
    )
    .map_err(|error| error.to_string())?;
    let temporary = path.with_extension(format!("json.tmp-{}", std::process::id()));
    fs::write(
        &temporary,
        serde_json::to_string_pretty(&registry).map_err(|error| error.to_string())? + "\n",
    )
    .map_err(|error| error.to_string())?;
    fs::rename(&temporary, &path).map_err(|error| error.to_string())?;
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
        json!({"status":"synced","site_id":site["site_id"],"path":path_text(&path),"surface_count":surfaces.len(),"tool_count":tool_count}),
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

fn registry_surface(
    contract: &Value,
    site: &Value,
    server_name: &str,
    server: &Value,
    file: &str,
) -> Result<Value, String> {
    let site_id = site["site_id"].as_str().unwrap_or("");
    let prefix = if site_id == "andrey-user" {
        "narada-site-andrey-user".to_string()
    } else if site_id.starts_with("narada-") {
        site_id.to_string()
    } else {
        format!("narada-{site_id}")
    };
    let inferred = server_name
        .strip_prefix(&(prefix + "-"))
        .unwrap_or(server_name);
    let surface_id = server["surface_id"].as_str().unwrap_or(inferred);
    let items = contract
        .pointer("/read_models/registrar_surface_list/items")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let catalog = items
        .iter()
        .find(|surface| surface["id"] == surface_id)
        .ok_or_else(|| format!("registrar_site_local_descriptor_missing:{surface_id}"))?;
    let projection_id = server["projection_id"]
        .as_str()
        .or_else(|| {
            server
                .pointer("/surface_projection/projection_id")
                .and_then(Value::as_str)
        })
        .unwrap_or("default");
    let projection = catalog["projections"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|projection| projection["id"] == projection_id)
        .ok_or_else(|| {
            format!("registrar_unknown_surface_projection:{surface_id}:{projection_id}")
        })?;
    let tool_source = projection
        .get("exposed_tools")
        .filter(|value| value.is_array())
        .unwrap_or(&catalog["tools"]);
    let registered = unique(
        tool_source
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str),
    );
    let descriptor = &catalog["descriptor"];
    let mut read_only = vec![];
    let mut refused = vec![];
    for tool in descriptor["tools"].as_array().into_iter().flatten() {
        let Some(name) = tool["name"].as_str() else {
            continue;
        };
        if !registered.iter().any(|value| value == name) {
            continue;
        }
        if tool.pointer("/effect/class").and_then(Value::as_str) == Some("read")
            || tool
                .pointer("/annotations/readOnlyHint")
                .and_then(Value::as_bool)
                == Some(true)
        {
            read_only.push(name.to_string());
        }
        if tool
            .pointer("/annotations/legacy_policy")
            .and_then(Value::as_str)
            == Some("refused")
        {
            refused.push(name.to_string());
        }
    }
    let mut classified = read_only.clone();
    classified.extend(refused.clone());
    let mutating = registered
        .iter()
        .filter(|name| !classified.contains(name))
        .cloned()
        .collect::<Vec<_>>();
    let raw_command = server["command"].as_str().unwrap_or("node").to_string();
    let mut raw_args = server["args"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(str::to_string)
        .collect::<Vec<_>>();
    let mut launch = unwrap_launch(&raw_command, &raw_args);
    if surface_id == "mcp-loader" && launch.invocation.as_deref() == Some("native_entrypoint") {
        if let Some(canonical) =
            native_artifact_entrypoint("mcp-loader-mcp", "narada-mcp-loader.exe")
        {
            for flag in ["--child-command", "--entrypoint"] {
                if let Some(index) = raw_args.iter().position(|value| value == flag) {
                    if let Some(value) = raw_args.get_mut(index + 1) {
                        *value = canonical.clone();
                    }
                }
            }
            launch.entrypoint = canonical.clone();
            launch.child_command = canonical;
        }
    }
    let runtime_kind = if matches!(
        launch.invocation.as_deref(),
        Some("native_applet" | "native_entrypoint")
    ) {
        "rust-stdio"
    } else if executable_name(&launch.child_command) == "bun" {
        "bun-stdio"
    } else {
        "node-stdio"
    };
    let mut surface_projection = json!({"surface_id":surface_id,"projection_id":projection_id,"injection_scope":projection["injection_scope"],"runtime_requirements":projection.get("runtime_requirements").cloned().unwrap_or_else(||json!([])),"exposed_tools":registered,"execution":projection["execution"],"descriptor_digest":catalog["descriptor_digest"],"tool_contract_digest":catalog["tool_contract_digest"],"surface_descriptor":descriptor});
    if let Some(value) = projection.get("default_injection") {
        surface_projection
            .as_object_mut()
            .unwrap()
            .insert("default_injection".into(), value.clone());
    }
    if let Some(value) = server.pointer("/surface_projection/runtime_kind") {
        surface_projection
            .as_object_mut()
            .unwrap()
            .insert("runtime_kind".into(), value.clone());
    }
    if let Some(value) = descriptor["projections"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|candidate| candidate["id"] == projection_id)
        .and_then(|candidate| candidate.get("lifecycle"))
    {
        surface_projection
            .as_object_mut()
            .unwrap()
            .insert("lifecycle".into(), value.clone());
    }
    let transport_command = if launch.proxied {
        native_proxy_entrypoint().unwrap_or(raw_command.clone())
    } else {
        raw_command.clone()
    };
    let transport_args = if !launch.proxied && raw_args.is_empty() {
        vec![String::new()]
    } else {
        raw_args
    };
    Ok(
        json!({"surface_id":format!("{server_name}.local"),"surface_projection":surface_projection,"surface_type":catalog["kind"],"display_name":server_name,"server_name":server_name,"runtime_binding":{"runtime_kind":runtime_kind,"proxy_implementation":if launch.proxied{json!("native")}else{Value::Null},"entrypoint":launch.entrypoint,"owner_site_id":site_id,"transport":{"type":"stdio","command":transport_command,"args":transport_args}},"authority_boundary":{"posture":"registrar_generated_runtime_surface_registry","grants_tool_authority":true,"granted_tool_authority_kind":"declared_enabled_mcp_surface_tools","source":"site_mcp_fabric_and_registrar_catalog"},"client_config":{"generated_path":format!(".ai/mcp/{file}"),"generated_file":file},"tool_contract":{"exposed_tools":registered,"semantic_operations":[],"deprecated_aliases":{},"read_only_tools":read_only,"mutating_tools":mutating,"refused_tools":refused},"registered_live_tools":registered,"catalog_surface_id":descriptor["surface_id"],"evidence":{"source":"site_mcp_fabric","path":format!(".ai/mcp/{file}"),"projection_kind":"site_fabric"}}),
    )
}

struct Launch {
    entrypoint: String,
    child_command: String,
    proxied: bool,
    invocation: Option<String>,
}
fn unwrap_launch(command: &str, args: &[String]) -> Launch {
    if args.first().map(String::as_str) == Some("proxy") {
        let value = |flag: &str| {
            args.iter()
                .position(|item| item == flag)
                .and_then(|index| args.get(index + 1))
                .cloned()
                .unwrap_or_default()
        };
        return Launch {
            entrypoint: value("--entrypoint"),
            child_command: value("--child-command"),
            proxied: true,
            invocation: Some(value("--child-invocation-kind")).filter(|value| !value.is_empty()),
        };
    }
    Launch {
        entrypoint: args.first().cloned().unwrap_or_default(),
        child_command: command.to_string(),
        proxied: false,
        invocation: None,
    }
}
fn executable_name(command: &str) -> String {
    Path::new(command)
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or(command)
        .to_ascii_lowercase()
}
fn native_proxy_entrypoint() -> Option<String> {
    native_artifact_entrypoint("shared/mcp-runtime-proxy", "narada-mcp-runtime.exe")
}
fn native_artifact_entrypoint(package: &str, artifact: &str) -> Option<String> {
    let executable = env::current_exe().ok()?;
    let workspace = executable.ancestors().find(|root| {
        root.join("packages")
            .join("shared")
            .join("mcp-runtime-proxy")
            .exists()
    })?;
    let native_root = package
        .split('/')
        .fold(workspace.join("packages"), |root, part| root.join(part))
        .join("dist")
        .join("native");
    let pointer: Value =
        serde_json::from_str(&fs::read_to_string(native_root.join("current.json")).ok()?).ok()?;
    let relative = pointer.get("artifacts")?.get(artifact)?.as_str()?;
    Some(
        native_root
            .join(relative)
            .to_string_lossy()
            .replace('\\', "/"),
    )
}

fn surface_tool_inventory(contract: &Value, args: &Value) -> Value {
    let observed = args.get("observed_tools").and_then(Value::as_object);
    let include_ok = args.get("include_ok") == Some(&Value::Bool(true));
    let items = contract
        .pointer("/read_models/registrar_surface_list/items")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut findings = vec![];
    let mut checked = 0;
    for surface in &items {
        let id = surface["id"].as_str().unwrap_or("");
        let Some(input) = observed.and_then(|value| value.get(id)) else {
            continue;
        };
        checked += 1;
        let registered = unique(
            surface["tools"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str),
        );
        let actual = unique(
            input
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str),
        );
        let missing = actual
            .iter()
            .filter(|value| !registered.contains(value))
            .cloned()
            .collect::<Vec<_>>();
        let extra = registered
            .iter()
            .filter(|value| !actual.contains(value))
            .cloned()
            .collect::<Vec<_>>();
        let status = if missing.is_empty() && extra.is_empty() {
            "ok"
        } else {
            "drift"
        };
        if status != "ok" || include_ok {
            findings.push(json!({"surface_id":id,"package":surface["package"],"status":status,"registered_count":registered.len(),"observed_count":actual.len(),"missing_from_registrar":missing,"extra_in_registrar":extra}));
        }
    }
    let without = items
        .iter()
        .filter_map(|value| value["id"].as_str())
        .filter(|id| observed.is_none_or(|value| !value.contains_key(*id)))
        .collect::<Vec<_>>();
    json!({"schema":"narada.registrar.surface_tool_inventory_check.v1","status":if findings.iter().any(|value|value["status"]=="drift"){"drift"}else{"ok"},"checked_count":checked,"surfaces_without_observations":without,"findings":findings})
}
fn unique<'a>(values: impl Iterator<Item = &'a str>) -> Vec<String> {
    let mut result = vec![];
    for value in values {
        if !result.iter().any(|existing| existing == value) {
            result.push(value.to_string());
        }
    }
    result
}
fn error(id: Value, message: String) -> Value {
    json!({"jsonrpc":"2.0","id":id,"error":{"code":-32000,"message":message}})
}

fn read_message<R: BufRead>(input: &mut R) -> Result<Option<Value>, String> {
    let mut first = String::new();
    if input.read_line(&mut first).map_err(|e| e.to_string())? == 0 {
        return Ok(None);
    }
    if first.to_ascii_lowercase().starts_with("content-length:") {
        let length = first
            .split_once(':')
            .and_then(|(_, v)| v.trim().parse::<usize>().ok())
            .ok_or("invalid_content_length")?;
        loop {
            let mut line = String::new();
            input.read_line(&mut line).map_err(|e| e.to_string())?;
            if line == "\r\n" || line == "\n" {
                break;
            }
            if line.is_empty() {
                return Err("unexpected_eof_in_headers".into());
            }
        }
        let mut body = vec![0; length];
        input.read_exact(&mut body).map_err(|e| e.to_string())?;
        serde_json::from_slice(&body)
            .map(Some)
            .map_err(|e| e.to_string())
    } else {
        serde_json::from_str(first.trim())
            .map(Some)
            .map_err(|e| e.to_string())
    }
}
