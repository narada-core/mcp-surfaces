fn site_mcp_fabric_validate(contract: &Value, args: &Value) -> Result<Value, String> {
    let requested = required_argument(args, "site_id", "registrar_requires_site_id")?;
    let include_ok = args.get("include_ok").and_then(Value::as_bool) == Some(true);
    let catalog = site_catalog(contract);
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
        let has_embedded_descriptor =
            embedded_site_local_catalog(server, surface_id).is_some();
        if known.is_none()
            && server["surface_descriptor_path"].is_null()
            && !has_embedded_descriptor
        {
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
                    && projection["default_injection"] == "enabled"
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
        json!({"schema":"narada.registrar.site_fabric_validation.v1","status":if errors>0{"invalid"}else if warnings>0{"valid_with_warnings"}else{"valid"},"site_id":site_id,"server_count":servers.len(),"carrier_projection_count":carrier_servers.len(),"errors":errors,"warnings":warnings,"findings":findings,"bounded":true}),
    )
}

