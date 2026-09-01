fn rebind_native_registrar(contract: &mut Value) -> Result<(), String> {
    let declared = contract
        .pointer("/runtime_bindings/registrar_entrypoint")
        .and_then(Value::as_str)
        .ok_or("native_registrar_binding_missing")?
        .to_string();
    let current = native_artifact_entrypoint(
        "mcp-registrar",
        if cfg!(windows) {
            "narada-mcp-registrar.exe"
        } else {
            "narada-mcp-registrar"
        },
    )
    .ok_or("native_registrar_artifact_unavailable")?;
    repair_native_contract(contract, &declared, &current);
    Ok(())
}

fn repair_native_contract(contract: &mut Value, declared: &str, current: &str) {
    if declared != current {
        replace_value_string(contract, declared, current);
        replace_value_string(
            contract,
            &declared.replace('/', "\\"),
            &current.replace('/', "\\"),
        );
    }
    replace_value_string(
        &mut contract["guidance"],
        "pnpm materialize:carrier",
        "cargo native-release",
    );
    if let Some(tools) = contract.get_mut("tools").and_then(Value::as_array_mut) {
        if let Some(tool) = tools
            .iter_mut()
            .find(|tool| tool["name"] == "registrar_surface_list")
        {
            tool["inputSchema"] = json!({"type":"object","properties":{"compact":{"type":"boolean","default":true,"description":"Return identity and summary fields; set false for full descriptors."},"limit":{"type":"integer","minimum":1,"maximum":200,"default":50},"offset":{"type":"integer","minimum":0,"maximum":10000,"default":0}},"additionalProperties":false});
        }
        for name in ["registrar_site_list", "registrar_carrier_list"] {
            if let Some(tool) = tools.iter_mut().find(|tool| tool["name"] == name) {
                tool["inputSchema"] = json!({"type":"object","properties":{"compact":{"type":"boolean","default":true,"description":"Return identity and summary fields; set false for full records."},"limit":{"type":"integer","minimum":1,"maximum":100,"default":20},"offset":{"type":"integer","minimum":0,"maximum":10000,"default":0}},"additionalProperties":false});
            }
        }
        if let Some(tool) = tools
            .iter_mut()
            .find(|tool| tool["name"] == "registrar_site_surface_registry_sync")
        {
            tool["inputSchema"]["properties"]["include_registry"] = json!({"type":"boolean","default":false,"description":"Include the complete generated registry in a dry-run response; the default returns only its bounded summary."});
        }
    }
    if let Some(plans) = contract
        .pointer_mut("/read_models/registrar_carrier_validation_plans")
        .and_then(Value::as_object_mut)
    {
        for plan in plans.values_mut() {
            for server in plan
                .get_mut("servers")
                .and_then(Value::as_array_mut)
                .into_iter()
                .flatten()
            {
                if server["surface_id"] == "mcp-registrar" {
                    server["entrypoint"] = json!(current);
                }
            }
        }
    }
}

fn surface_list(contract: &Value, args: &Value) -> Value {
    let items = contract
        .pointer("/read_models/registrar_surface_list/items")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let total = items.len();
    let offset = args
        .get("offset")
        .and_then(Value::as_u64)
        .unwrap_or(0)
        .min(10_000) as usize;
    let limit = args
        .get("limit")
        .and_then(Value::as_u64)
        .unwrap_or(50)
        .clamp(1, 200) as usize;
    let compact = args.get("compact").and_then(Value::as_bool).unwrap_or(true);
    let page = items
        .into_iter()
        .skip(offset)
        .take(limit + 1)
        .collect::<Vec<_>>();
    let has_more = page.len() > limit;
    let projected = page.into_iter().take(limit).map(|surface| {
        if !compact { return surface; }
        json!({
            "id":surface["id"],"package":surface["package"],"kind":surface["kind"],
            "injection_scope":surface["injection_scope"],"restart_owner":surface["restart_owner"],
            "descriptor_source":surface["descriptor_source"],
            "tool_count":surface["tools"].as_array().map_or(0, Vec::len),
            "projection_count":surface["projections"].as_array().map_or(0, Vec::len)
        })
    }).collect::<Vec<_>>();
    json!({"schema":"narada.registrar.surface_list.v1","status":"ok","items":projected,"returned":projected.len(),"total":total,"offset":offset,"limit":limit,"has_more":has_more,"next_offset":if has_more{json!(offset + limit)}else{Value::Null},"compact":compact})
}
fn carrier_list(contract: &Value, args: &Value) -> Value {
    let items = contract
        .pointer("/read_models/registrar_carrier_list/items")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    paginated_catalog("narada.registrar.carrier_list.v1", items, args, |carrier| {
        json!({
            "carrier_id":carrier["carrier_id"],
            "kind":carrier["kind"],
            "config_path":carrier["config_path"],
            "site_binding_count":carrier["site_bindings"].as_array().map_or(0, Vec::len),
            "surface_count":carrier["surfaces"].as_array().map_or(0, Vec::len)
        })
    })
}

fn paginated_catalog(
    schema: &str,
    items: Vec<Value>,
    args: &Value,
    compact_projection: impl Fn(&Value) -> Value,
) -> Value {
    let total = items.len();
    let offset = args
        .get("offset")
        .and_then(Value::as_u64)
        .unwrap_or(0)
        .min(10_000) as usize;
    let limit = args
        .get("limit")
        .and_then(Value::as_u64)
        .unwrap_or(20)
        .clamp(1, 100) as usize;
    let compact = args.get("compact").and_then(Value::as_bool).unwrap_or(true);
    let page = items
        .into_iter()
        .skip(offset)
        .take(limit + 1)
        .collect::<Vec<_>>();
    let has_more = page.len() > limit;
    let projected = page
        .into_iter()
        .take(limit)
        .map(|item| {
            if compact {
                compact_projection(&item)
            } else {
                item
            }
        })
        .collect::<Vec<_>>();
    json!({"schema":schema,"status":"ok","items":projected,"returned":projected.len(),"total":total,"offset":offset,"limit":limit,"has_more":has_more,"next_offset":if has_more{json!(offset + limit)}else{Value::Null},"compact":compact})
}
fn registrar_sync(contract: &Value, args: &Value) -> Result<Value, String> {
    let target = required_argument(args, "target", "registrar_requires_target")?;
    let carriers = contract
        .pointer("/read_models/registrar_carrier_list/items")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    if target == "all_surfaces_to_carriers" {
        let carrier_id = required_argument(
            args,
            "carrier_id",
            "registrar_requires_carrier_id_for_target",
        )?;
        let carrier = carrier_record(contract, &carrier_id)?;
        if carrier["site_bindings"]
            .as_array()
            .into_iter()
            .flatten()
            .any(|binding| binding["loading_mode"] == "progressive")
        {
            return Err(format!(
                "registrar_progressive_bulk_bind_refused:{carrier_id}"
            ));
        }
    }
    if target == "all_surfaces_to_all_carriers"
        && carriers.iter().any(|carrier| {
            carrier["site_bindings"]
                .as_array()
                .into_iter()
                .flatten()
                .any(|binding| binding["loading_mode"] == "progressive")
        })
    {
        return Err("registrar_progressive_bulk_bind_refused:all_carriers".into());
    }
    if target == "all_surfaces_to_carriers" || target == "all_surfaces_to_all_carriers" {
        return Err(format!("registrar_native_sync_unreachable:{target}"));
    }
    let surface_id = required_argument(args, "surface_id", "registrar_requires_surface_id")?;
    ensure_surface(contract, &surface_id)?;
    let mut results = vec![];
    if target == "all_sites" || target == "all" {
        for site in site_catalog(contract)["items"]
            .as_array()
            .into_iter()
            .flatten()
        {
            let site_id = site["site_id"].as_str().unwrap_or("");
            let call = json!({"site_id":site_id,"surface_id":surface_id,"projection_id":args.get("projection_id"),"runtime_kind":args.get("runtime_kind"),"allow_sidecar":args["allow_sidecar"]==true});
            match site_bind(contract, &call) {
                Ok(value) => results.push(value),
                Err(message) => {
                    results.push(json!({"site_id":site_id,"surface_id":surface_id,"error":message}))
                }
            }
        }
    }
    if target == "all_carriers" || target == "all" {
        for carrier in &carriers {
            let carrier_id = carrier["carrier_id"].as_str().unwrap_or("");
            match carrier_bind(
                contract,
                &json!({"carrier_id":carrier_id,"surface_id":surface_id,"projection_id":args.get("projection_id")}),
            ) {
                Ok(value) => results.push(value),
                Err(failure) => results
                    .push(json!({"carrier_id":carrier_id,"surface_id":surface_id,"error":failure.message})),
            }
        }
    }
    Ok(json!({"surface_id":surface_id,"target":target,"count":results.len(),"results":results}))
}

