#[allow(clippy::drop_non_drop)]
fn check_registry_conformance(
    contract: &Value,
    site: &Value,
    registry: &Value,
    observed: &Value,
    observed_read_only: &Value,
    observed_mutating: &Value,
    include_ok: bool,
) -> Result<Value, String> {
    let site_id = site["site_id"].as_str().unwrap_or("");
    let root = PathBuf::from(site["root"].as_str().unwrap_or(""));
    let registry_path = capability_registry_path(&root);
    let expected = build_site_surface_registry(contract, site)?;
    let expected_surfaces = expected["surfaces"].as_array().cloned().unwrap_or_default();
    let actual_surfaces = registry["surfaces"].as_array().cloned().unwrap_or_default();
    let mut actual_by_server = std::collections::HashMap::<String, Value>::new();
    let mut violations = vec![];
    let mut add_global = |code: &str, detail: Value| {
        violations.push(merge_value(json!({"layer":"materialized_registry","code":code,"surface_id":null,"server_name":null}), detail));
    };
    if registry["schema"] != "narada.site.capabilities.mcp_surfaces.v1" {
        add_global(
            "registry_schema_mismatch",
            json!({"expected":"narada.site.capabilities.mcp_surfaces.v1","actual":registry.get("schema")}),
        );
    }
    if registry["site_id"] != site_id {
        add_global(
            "registry_site_id_mismatch",
            json!({"expected":site_id,"actual":registry.get("site_id")}),
        );
    }
    if registry["generated_by"] != "mcp-registrar" {
        add_global(
            "registry_generator_mismatch",
            json!({"expected":"mcp-registrar","actual":registry.get("generated_by")}),
        );
    }
    if registry.pointer("/generation_policy/mode") != Some(&json!("enabled_surface_tool_authority"))
    {
        add_global(
            "registry_generation_policy_mismatch",
            json!({"expected":"enabled_surface_tool_authority","actual":registry.pointer("/generation_policy/mode")}),
        );
    }
    if registry.pointer("/generation_policy/source")
        != Some(&json!(".ai/mcp + registrar surface catalog"))
    {
        add_global(
            "registry_generation_source_mismatch",
            json!({"expected":".ai/mcp + registrar surface catalog","actual":registry.pointer("/generation_policy/source")}),
        );
    }
    if registry.pointer("/generation_policy/note") != expected.pointer("/generation_policy/note") {
        add_global("registry_generation_note_mismatch", json!({}));
    }
    if registry["generated_at"]
        .as_str()
        .and_then(|value| {
            time::OffsetDateTime::parse(value, &time::format_description::well_known::Rfc3339).ok()
        })
        .is_none()
    {
        add_global(
            "registry_generated_at_invalid",
            json!({"actual":registry.get("generated_at")}),
        );
    }
    if !registry["surfaces"].is_array() {
        add_global(
            "registry_surfaces_invalid",
            json!({"actual_type":json_type_name(registry.get("surfaces"))}),
        );
    }
    for surface in &actual_surfaces {
        let name = surface["server_name"].as_str().unwrap_or("");
        if name.is_empty() {
            add_global(
                "registry_surface_server_name_missing",
                json!({"surface_id":surface.get("surface_id")}),
            );
            continue;
        }
        if actual_by_server
            .insert(name.into(), surface.clone())
            .is_some()
        {
            add_global(
                "registry_surface_server_name_duplicate",
                json!({"server_name":name}),
            );
        }
    }
    let expected_names = expected_surfaces
        .iter()
        .filter_map(|surface| surface["server_name"].as_str())
        .collect::<std::collections::HashSet<_>>();
    for name in actual_by_server.keys() {
        if !expected_names.contains(name.as_str()) {
            add_global(
                "registry_surface_not_in_fabric",
                json!({"server_name":name}),
            );
        }
    }
    drop(add_global);
    let observed_keys = observed
        .as_object()
        .map(|value| value.keys().cloned().collect::<Vec<_>>())
        .unwrap_or_default();
    let mut unobserved = vec![];
    let mut surface_results = vec![];
    for expected_surface in &expected_surfaces {
        let server_name = expected_surface["server_name"].as_str().unwrap_or("");
        let actual = actual_by_server.get(server_name);
        let surface_id = actual
            .map(|v| v["surface_id"].clone())
            .unwrap_or_else(|| json!(format!("{server_name}.local")));
        let catalog_id = expected_surface["catalog_surface_id"].clone();
        let mut local = vec![];
        let mut add = |layer: &str, code: &str, detail: Value| {
            let violation = merge_value(
                json!({"layer":layer,"code":code,"surface_id":surface_id,"server_name":server_name,"catalog_surface_id":catalog_id}),
                detail,
            );
            local.push(violation.clone());
            violations.push(violation);
        };
        if actual.is_none() {
            add(
                "materialized_registry",
                "registry_surface_missing",
                json!({}),
            );
        }
        let keys = [
            server_name,
            &format!("{server_name}.local"),
            expected_surface["surface_id"].as_str().unwrap_or(""),
            expected_surface["catalog_surface_id"]
                .as_str()
                .unwrap_or(""),
        ];
        let live = observed_array(observed, &keys);
        let live_ro = observed_array(observed_read_only, &keys);
        let live_mut = observed_array(observed_mutating, &keys);
        let requested = observed_keys.is_empty()
            || keys
                .iter()
                .any(|key| observed_keys.iter().any(|value| value == key));
        if !requested {
            unobserved.push(server_name.to_string());
        } else {
            if live.is_none() {
                add("live_surface", "live_tool_observation_missing", json!({}));
            }
            if live_ro.is_none() {
                add(
                    "live_surface",
                    "live_read_only_observation_missing",
                    json!({}),
                );
            }
            if live_mut.is_none() {
                add(
                    "live_surface",
                    "live_mutating_observation_missing",
                    json!({}),
                );
            }
        }
        let expected_tools = strings(&expected_surface["registered_live_tools"]);
        if let Some(values) = live.as_ref() {
            let duplicates = duplicate_strings(values);
            if !duplicates.is_empty() {
                add(
                    "live_surface",
                    "live_tools_duplicate",
                    json!({"duplicate_tools":duplicates}),
                );
            }
        }
        if let Some(values) = live_ro.as_ref() {
            let duplicates = duplicate_strings(values);
            if !duplicates.is_empty() {
                add(
                    "live_surface",
                    "live_read_only_tools_duplicate",
                    json!({"duplicate_tools":duplicates}),
                );
            }
        }
        if let Some(values) = live_mut.as_ref() {
            let duplicates = duplicate_strings(values);
            if !duplicates.is_empty() {
                add(
                    "live_surface",
                    "live_mutating_tools_duplicate",
                    json!({"duplicate_tools":duplicates}),
                );
            }
        }
        if let (Some(all), Some(read_only), Some(mutating)) = (&live, &live_ro, &live_mut) {
            let mut semantic_union = read_only.clone();
            semantic_union.extend(mutating.clone());
            compare_sets(
                &mut add,
                "live_surface",
                "live_tool_semantics_partition_incomplete",
                all,
                &semantic_union,
            );
            let overlaps = read_only
                .iter()
                .filter(|tool| mutating.contains(tool))
                .cloned()
                .collect::<Vec<_>>();
            if !overlaps.is_empty() {
                let mut overlaps = overlaps;
                overlaps.sort();
                overlaps.dedup();
                add(
                    "live_surface",
                    "live_tool_semantics_partition_overlap",
                    json!({"overlapping_tools":overlaps}),
                );
            }
        }
        if let Some(values) = live.as_ref() {
            compare_sets(
                &mut add,
                "site_fabric",
                "fabric_tools_differ_from_live",
                values,
                &expected_tools,
            );
            compare_sets(
                &mut add,
                "registrar_catalog",
                "catalog_tools_differ_from_live",
                values,
                &expected_tools,
            );
        }
        if let Some(actual) = actual {
            let raw_registered = strings(&actual["registered_live_tools"]);
            let registered = unique_strings(&raw_registered);
            let read_only = strings(
                &actual
                    .pointer("/tool_contract/read_only_tools")
                    .cloned()
                    .unwrap_or_else(|| json!([])),
            );
            let mutating = strings(
                &actual
                    .pointer("/tool_contract/mutating_tools")
                    .cloned()
                    .unwrap_or_else(|| json!([])),
            );
            let refused = strings(
                &actual
                    .pointer("/tool_contract/refused_tools")
                    .cloned()
                    .unwrap_or_else(|| json!([])),
            );
            let duplicate_contract_tools = json!({"registered_live_tools":duplicate_strings(&raw_registered),"read_only_tools":duplicate_strings(&read_only),"mutating_tools":duplicate_strings(&mutating),"refused_tools":duplicate_strings(&refused)});
            if duplicate_contract_tools
                .as_object()
                .unwrap()
                .values()
                .any(|value| value.as_array().is_some_and(|items| !items.is_empty()))
            {
                add(
                    "tool_contract",
                    "tool_contract_contains_duplicates",
                    duplicate_contract_tools,
                );
            }
            if let Some(values) = live.as_ref() {
                compare_sets(
                    &mut add,
                    "materialized_registry",
                    "registered_tools_differ_from_live",
                    values,
                    &registered,
                );
            }
            compare_sets(
                &mut add,
                "materialized_registry",
                "registered_tools_differ_from_fabric",
                &expected_tools,
                &registered,
            );
            compare_sets(
                &mut add,
                "materialized_registry",
                "registered_tools_differ_from_catalog",
                &expected_tools,
                &registered,
            );
            let mut contract_union = read_only.clone();
            contract_union.extend(mutating.clone());
            contract_union.extend(refused.clone());
            compare_sets(
                &mut add,
                "tool_contract",
                "tool_contract_partition_incomplete",
                &registered,
                &contract_union,
            );
            let overlaps = read_only
                .iter()
                .filter(|tool| mutating.contains(tool) || refused.contains(tool))
                .chain(mutating.iter().filter(|tool| refused.contains(tool)))
                .cloned()
                .collect::<Vec<_>>();
            if !overlaps.is_empty() {
                let overlaps = unique_strings(&overlaps);
                add(
                    "tool_contract",
                    "tool_contract_partition_overlap",
                    json!({"overlapping_tools":overlaps}),
                );
            }
            if !refused.is_empty() {
                add(
                    "tool_contract",
                    "tool_contract_contains_external_refusals",
                    json!({"refused_tools":refused}),
                );
            }
            if let Some(values) = live_ro.as_ref() {
                compare_sets(
                    &mut add,
                    "tool_contract",
                    "read_only_classification_differ_from_live",
                    values,
                    &read_only,
                );
            }
            if let Some(values) = live_mut.as_ref() {
                compare_sets(
                    &mut add,
                    "tool_contract",
                    "mutating_classification_differ_from_live",
                    values,
                    &mutating,
                );
            }
            for field in [
                "surface_id",
                "display_name",
                "server_name",
                "authority_boundary",
                "client_config",
                "catalog_surface_id",
                "descriptor_provenance",
                "surface_descriptor",
            ] {
                if canonical_json(&actual[field]) != canonical_json(&expected_surface[field]) {
                    add(
                        "materialized_registry",
                        "registry_surface_projection_drift",
                        json!({"field":field}),
                    );
                }
            }
        }
        if include_ok || !local.is_empty() {
            surface_results.push(json!({"surface_id":surface_id,"server_name":server_name,"catalog_surface_id":catalog_id,"status":if local.is_empty(){"ok"}else{"drift"},"violation_count":local.len(),"violations":local}));
        }
    }
    let output_reader = output_reader_closure_for_registry(
        contract,
        registry,
        Some(site_id),
        Some(&root),
        Some(&registry_path),
    );
    for raw in output_reader["violations"].as_array().into_iter().flatten() {
        violations.push(merge_value(
            json!({"layer":"output_reader_closure","code":"output_reader_closure_violation"}),
            raw.clone(),
        ));
    }
    unobserved.sort();
    let mut observed_sorted = observed_keys;
    observed_sorted.sort();
    Ok(
        json!({"schema":"narada.registrar.site_registry_conformance_check.v1","status":if !violations.is_empty(){"drift"}else if !unobserved.is_empty(){"incomplete"}else{"ok"},"site_id":site_id,"site_root":site["root"],"registry_path":path_text(&registry_path),"checked_surface_count":expected_surfaces.len(),"observed_surface_count":observed.as_object().map_or(0,|v|v.len()),"observation_coverage":{"status":if observed_sorted.is_empty(){"missing"}else if !unobserved.is_empty(){"partial"}else{"complete"},"observed_server_names":observed_sorted,"unobserved_server_names":unobserved},"violation_count":violations.len(),"violations":violations,"surfaces":surface_results,"output_reader_closure":output_reader}),
    )
}

