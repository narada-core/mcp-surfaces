struct MutationFailure {
    code: String,
    message: String,
    details: Value,
}
fn mutation_failure(code: &str, message: String, details: Value) -> MutationFailure {
    MutationFailure {
        code: code.into(),
        message,
        details,
    }
}
fn carrier_mutation_error(id: Value, failure: MutationFailure) -> Value {
    let child_data = json!({"schema":"narada.registrar.error.v1","code":failure.code,"message":failure.message,"details":failure.details});
    let child_error = json!({"code":-32000,"message":failure.message,"data":child_data});
    let entrypoint = env::current_exe()
        .map(|path| path_text(&path).replace('\\', "/"))
        .unwrap_or_else(|_| "narada-mcp-registrar".into());
    json!({"jsonrpc":"2.0","id":id,"error":{"code":-32000,"message":failure.message,"data":{"schema":"narada.registrar.error.v1","code":"registrar_fresh_materialization_failed","message":failure.message,"details":{"entrypoint":entrypoint,"stderr_tail":"","exit_code":0,"signal":null,"child_error":child_error}}}})
}
fn sync_error(id: Value, message: String) -> Value {
    if let Some(suffix) = message.strip_prefix("registrar_progressive_bulk_bind_refused:") {
        let remediation = if suffix == "all_carriers" {
            "Progressive carriers expose only their explicit bootstrap allowlists; use mcp-loader for runtime attachment or switch the bindings to static loading."
        } else {
            "Progressive carriers expose only their explicit bootstrap allowlist; use mcp-loader for runtime attachment or switch the binding to static loading."
        };
        let mut details = json!({"remediation":remediation});
        if suffix != "all_carriers" {
            details
                .as_object_mut()
                .unwrap()
                .insert("carrier_id".into(), json!(suffix));
        }
        return json!({"jsonrpc":"2.0","id":id,"error":{"code":-32000,"message":message,"data":{"schema":"narada.registrar.error.v1","code":"registrar_progressive_bulk_bind_refused","message":message,"details":details}}});
    }
    error(id, message)
}
fn carrier_bind(contract: &Value, args: &Value) -> Result<Value, MutationFailure> {
    let carrier_id = required_argument(args, "carrier_id", "registrar_requires_carrier_id")
        .map_err(|message| mutation_failure("registrar_requires_carrier_id", message, json!({})))?;
    let surface_id = required_argument(args, "surface_id", "registrar_requires_surface_id")
        .map_err(|message| mutation_failure("registrar_requires_surface_id", message, json!({})))?;
    let carrier = carrier_record(contract, &carrier_id)
        .map_err(|message| mutation_failure("registrar_unknown_carrier", message, json!({})))?;
    let surface = ensure_surface(contract, &surface_id)
        .map_err(|message| mutation_failure("registrar_unknown_surface", message, json!({})))?;
    if let Some(projection_id) = args
        .get("projection_id")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
    {
        if !surface["projections"].as_array().is_some_and(|items| {
            items
                .iter()
                .any(|projection| projection["id"] == projection_id)
        }) {
            return Err(mutation_failure(
                "registrar_unknown_surface_projection",
                format!("registrar_unknown_surface_projection:{surface_id}:{projection_id}"),
                json!({"surface_id":surface_id,"projection_id":projection_id}),
            ));
        }
    }
    let site_id = args
        .get("site_id")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .unwrap_or("andrey-user");
    let sites = site_catalog(contract)["items"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let site = lookup_site_value(&sites,site_id).map_err(|message|mutation_failure("registrar_unknown_site",message,json!({"known":sites.iter().filter_map(|site|site["site_id"].as_str()).collect::<Vec<_>>() })))?;
    let site_declares_surface = site["surfaces"]
        .as_array()
        .into_iter()
        .flatten()
        .any(|value| value == &surface_id)
        || site_fabric_surface_ids(contract, &site)
            .iter()
            .any(|value| value == &surface_id)
        || site_local_surface_ids(contract, &site)
            .iter()
            .any(|value| value == &surface_id);
    let binding = carrier["site_bindings"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|binding| binding["site_id"] == site_id);
    if binding.is_none() {
        let next_route = if site_declares_surface {
            "mcp-loader"
        } else {
            "registrar_site_bind"
        };
        let remediation = if site_declares_surface {
            "The surface is declared by the Site but this carrier has no Site binding for it. Use mcp-loader with the Site root for runtime attachment; do not add a carrier binding for a site-scoped surface."
        } else {
            "The requested Site does not declare this surface. Bind the surface to the Site first, then use mcp-loader for a site-scoped runtime attachment or add it to the native carrier contract for static materialization."
        };
        return Err(mutation_failure(
            "registrar_carrier_site_binding_missing",
            format!("registrar_carrier_site_binding_missing:{carrier_id}:{site_id}:{surface_id}"),
            json!({
                "carrier_id":carrier_id,
                "site_id":site_id,
                "surface_id":surface_id,
                "carrier_site_binding":"absent",
                "site_surface_declared":site_declares_surface,
                "site_root":site["root"],
                "next_route":next_route,
                "remediation":remediation
            }),
        ));
    }
    let keys = carrier_surface_keys(contract, &carrier_id, &surface_id);
    if binding.is_some_and(|value| value["loading_mode"] == "progressive") && keys.is_empty() {
        return Err(mutation_failure(
            "registrar_progressive_surface_bind_refused",
            format!("registrar_progressive_surface_bind_refused:{carrier_id}:{surface_id}"),
            json!({"carrier_id":carrier_id,"site_id":site_id,"surface_id":surface_id,"loading_mode":"progressive","remediation":"Use mcp-loader to attach this surface at runtime, or explicitly add it to the progressive bootstrap allowlist before materializing the carrier."}),
        ));
    }
    if !keys.is_empty() {
        return Err(mutation_failure(
            "registrar_carrier_config_owned_by_native_materializer",
            format!(
                "registrar_carrier_config_owned_by_native_materializer:{carrier_id}:{surface_id}"
            ),
            json!({"carrier_id":carrier_id,"surface_id":surface_id,"server_keys":keys,"remediation":"Edit the external native carrier contract or the owning Site registry, then run cargo native-materialize."}),
        ));
    }
    Err(mutation_failure(
        "registrar_carrier_bind_requires_native_materializer",
        format!("registrar_carrier_bind_requires_native_materializer:{carrier_id}:{site_id}:{surface_id}"),
        json!({
            "carrier_id":carrier_id,
            "site_id":site_id,
            "surface_id":surface_id,
            "route":"native-materializer",
            "remediation":"Change the owning native carrier contract or Site registry, then run cargo native-materialize; this registrar does not emit single-carrier configuration."
        }),
    ))
}
