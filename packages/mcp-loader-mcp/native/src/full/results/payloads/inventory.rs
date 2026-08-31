use crate::full::*;

pub(crate) fn site_tool_inventory(
    arguments: &JsonObject,
    state: &mut LoaderState,
) -> Result<Value, Diagnostic> {
    let site_root = normalize_path(&required_string(
        arguments,
        "site_root",
        "missing_site_root",
    )?);
    ensure_site_root_allowed(&site_root, &state.policy)?;
    let bundle = read_site_fabric(&site_root)?;
    let site_id = bundle.fabric.get("site_id").and_then(Value::as_str);
    let servers = bundle
        .fabric
        .get("mcpServers")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    let requested = string_array(arguments.get("surface_ids"))?;
    let mut surface_ids = requested.clone().unwrap_or_else(|| {
        let mut values: Vec<String> = servers.keys().cloned().collect();
        values.sort();
        values
    });
    let runtime_kind = value_string(arguments.get("runtime_kind"));
    let include_ok = arguments
        .get("include_ok")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let mut findings = Vec::new();
    let mut observed_tools = Map::new();
    let mut observed_read_only = Map::new();
    let mut observed_mutating = Map::new();
    let mut observed_unclassified = Map::new();
    for surface_id in &surface_ids {
        let matched = find_site_server(&servers, surface_id)?;
        let Some((_, server)) = matched else {
            findings.push(json!({"surface_id":surface_id,"status":"surface_not_declared","declared_tools":[],"observed_tools":[]}));
            continue;
        };
        let resolved_surface_id = server
            .get("surface_id")
            .and_then(Value::as_str)
            .unwrap_or(surface_id)
            .to_string();
        let binding_id = canonical_binding_id(
            site_id,
            &resolved_surface_id,
            server.get("binding_id").and_then(Value::as_str),
        );
        let raw_declared: Vec<String> = server
            .get("tools")
            .and_then(Value::as_array)
            .map(|values| {
                values
                    .iter()
                    .filter_map(|value| value.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default();
        let declared = sorted_unique(&raw_declared);
        let duplicate_declared = duplicate_strings(&raw_declared);
        let requirements = surface_requirements(Some(&server));
        if !runtime_matches(&requirements, runtime_kind.as_deref()) {
            findings.push(json!({"surface_id":surface_id,"status":"runtime_not_selected","declared_count":declared.len(),"observed_count":0,"declared_tools":declared,"observed_tools":[],"runtime_kind":runtime_kind,"runtime_requirements":requirements}));
            continue;
        }
        let mut connection_id: Option<String> = None;
        let probe = attach_surface(
            &json_object!({"site_root":site_root.clone(),"binding_id":binding_id,"surface_id":resolved_surface_id,"runtime_kind":runtime_kind.clone()}),
            state,
        );
        match probe {
            Ok(attached) => {
                let id = attached
                    .get("connection_id")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                connection_id = Some(id.clone());
                if let Some(connection) = state.connections.get(&id) {
                    let observed_definitions: Vec<&Value> = connection
                        .tools
                        .iter()
                        .filter(|tool| {
                            tool.get("name").and_then(Value::as_str)
                                != Some(RUNTIME_PROXY_STATUS_TOOL_NAME)
                        })
                        .collect();
                    let raw_observed: Vec<String> = observed_definitions
                        .iter()
                        .filter_map(|tool| {
                            tool.get("name").and_then(Value::as_str).map(String::from)
                        })
                        .filter(|name| !name.is_empty())
                        .collect();
                    let observed = sorted_unique(&raw_observed);
                    let duplicate_observed = duplicate_strings(&raw_observed);
                    let read_only: Vec<String> = sorted_unique(
                        observed_definitions
                            .iter()
                            .filter(|tool| {
                                tool.get("annotations")
                                    .and_then(|value| value.get("readOnlyHint"))
                                    .and_then(Value::as_bool)
                                    == Some(true)
                            })
                            .filter_map(|tool| {
                                tool.get("name").and_then(Value::as_str).map(String::from)
                            })
                            .collect::<Vec<_>>()
                            .as_slice(),
                    );
                    let mutating: Vec<String> = sorted_unique(
                        observed_definitions
                            .iter()
                            .filter(|tool| {
                                tool.get("annotations")
                                    .and_then(|value| value.get("readOnlyHint"))
                                    .and_then(Value::as_bool)
                                    == Some(false)
                            })
                            .filter_map(|tool| {
                                tool.get("name").and_then(Value::as_str).map(String::from)
                            })
                            .collect::<Vec<_>>()
                            .as_slice(),
                    );
                    let unclassified: Vec<String> = sorted_unique(
                        observed_definitions
                            .iter()
                            .filter(|tool| {
                                tool.get("annotations")
                                    .and_then(|value| value.get("readOnlyHint"))
                                    .and_then(Value::as_bool)
                                    .is_none()
                            })
                            .filter_map(|tool| {
                                tool.get("name").and_then(Value::as_str).map(String::from)
                            })
                            .collect::<Vec<_>>()
                            .as_slice(),
                    );
                    observed_tools.insert(surface_id.clone(), json!(observed));
                    observed_read_only.insert(surface_id.clone(), json!(read_only));
                    observed_mutating.insert(surface_id.clone(), json!(mutating));
                    observed_unclassified.insert(surface_id.clone(), json!(unclassified));
                    let missing: Vec<String> = observed
                        .iter()
                        .filter(|name| !declared.contains(name))
                        .cloned()
                        .collect();
                    let extra: Vec<String> = declared
                        .iter()
                        .filter(|name| !observed.contains(name))
                        .cloned()
                        .collect();
                    let status = if missing.is_empty()
                        && extra.is_empty()
                        && duplicate_declared.is_empty()
                        && duplicate_observed.is_empty()
                        && unclassified.is_empty()
                    {
                        "ok"
                    } else {
                        "drift"
                    };
                    if include_ok || status != "ok" {
                        findings.push(json!({"surface_id":surface_id,"status":status,"declared_count":declared.len(),"observed_count":observed.len(),"missing_from_fabric":missing,"extra_in_fabric":extra,"duplicate_declared_tools":duplicate_declared,"duplicate_observed_tools":duplicate_observed,"unclassified_observed_tools":unclassified}));
                    }
                }
            }
            Err(error) => findings.push(
                json!({"surface_id":surface_id,"status":"probe_failed","error":error.value()}),
            ),
        }
        if let Some(id) = connection_id {
            let _ = detach_connection(&json_object!({"connection_id":id}), state);
        }
    }
    surface_ids.sort();
    let violation_count = findings
        .iter()
        .filter(|finding| {
            !matches!(
                finding.get("status").and_then(Value::as_str),
                Some("ok") | Some("runtime_not_selected")
            )
        })
        .count();
    let skipped: Vec<String> = findings
        .iter()
        .filter(|finding| {
            finding.get("status").and_then(Value::as_str) == Some("runtime_not_selected")
        })
        .filter_map(|finding| {
            finding
                .get("surface_id")
                .and_then(Value::as_str)
                .map(String::from)
        })
        .collect();
    let mut status_counts = Map::new();
    for finding in &findings {
        let status = finding
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or("unknown")
            .to_string();
        let count = status_counts
            .get(&status)
            .and_then(Value::as_u64)
            .unwrap_or(0)
            + 1;
        status_counts.insert(status, json!(count));
    }
    let observed_ids: Vec<String> = observed_tools.keys().cloned().collect();
    let unobserved: Vec<String> = surface_ids
        .iter()
        .filter(|id| !observed_tools.contains_key(*id))
        .cloned()
        .collect();
    let observation = json!({
        "schema":"narada.mcp_loader.site_tool_inventory_check.v1","status":if violation_count>0 {"drift"} else if !skipped.is_empty() {"partial"} else {"ok"},
        "site_root":site_root,"observed_at":now_iso(),"requested_surface_ids":requested,"runtime_kind":runtime_kind,
        "attempted_surface_ids":surface_ids,"observed_surface_ids":observed_ids,"unobserved_surface_ids":unobserved,
        "runtime_skipped_surface_ids":skipped,"runtime_skipped_count":skipped.len(),
        "observation_coverage":if requested.is_some() || !skipped.is_empty() {"partial"} else {"complete"},
        "checked_surface_count":surface_ids.len(),"violation_count":violation_count,
        "observed_tools":observed_tools,"observed_read_only_tools":observed_read_only,
        "observed_mutating_tools":observed_mutating,"observed_unclassified_tools":observed_unclassified,
        "finding_status_counts":status_counts,"findings":findings
    });
    let (reference, digest, byte_size, retention) =
        payload_observation(&site_root, &observation, state)?;
    let mut result = observation;
    if let Some(object) = result.as_object_mut() {
        object.insert("observation_ref".to_string(), json!(reference));
        object.insert("observation_sha256".to_string(), json!(digest));
        object.insert("observation_byte_size".to_string(), json!(byte_size));
        object.insert("observation_retention".to_string(), retention);
    }
    Ok(result)
}
