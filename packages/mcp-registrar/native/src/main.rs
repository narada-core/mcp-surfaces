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
