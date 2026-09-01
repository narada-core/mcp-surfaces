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
                    json!({"artifact_manifest_path":manifest_text,"remediation":"Run cargo native-package from mcp-surfaces before launching carrier MCPs."}),
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
                    json!({"runtime_proxy_entrypoint":proxy,"runtime_proxy_implementation":"native","remediation":"Run cargo native-package from mcp-surfaces before launching carrier MCPs."}),
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
                        json!({"remediation":format!("Run cargo native-package from mcp-surfaces before launching carrier MCPs; missing native dependency: {dependency}.")}),
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
