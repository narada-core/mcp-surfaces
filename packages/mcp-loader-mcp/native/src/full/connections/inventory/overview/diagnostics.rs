use crate::full::*;

pub(crate) fn site_fabric_diagnostics(
    arguments: &JsonObject,
    state: &LoaderState,
) -> Result<Value, Diagnostic> {
    let site_root = normalize_path(&required_string(
        arguments,
        "site_root",
        "missing_site_root",
    )?);
    ensure_site_root_allowed(&site_root, &state.policy)?;
    let bundle = read_site_fabric(&site_root)?;
    let servers = bundle
        .fabric
        .get("mcpServers")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    let mut diagnostics = Vec::new();
    for (surface_id, server) in &servers {
        let config_path = bundle
            .source_by_surface
            .get(surface_id)
            .cloned()
            .or_else(|| bundle.paths.first().cloned())
            .unwrap_or_default();
        let command = server
            .get("command")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let raw_args: Vec<String> = server
            .get("args")
            .and_then(Value::as_array)
            .map(|values| {
                values
                    .iter()
                    .filter_map(|value| value.as_str().map(ToOwned::to_owned))
                    .collect()
            })
            .unwrap_or_default();
        let declared =
            extract_runtime_entrypoint(&command, &raw_args).map(|value| normalize_path(&value));
        let expected = shared_surface_registry(surface_id, &state.surface_root)
            .map(|(entrypoint, _)| normalize_path(&entrypoint));
        let exists = declared
            .as_ref()
            .is_some_and(|value| Path::new(value).exists());
        let (classification, remediation) = classify_fabric_entrypoint(
            &site_root,
            declared.as_deref(),
            expected.as_deref(),
            exists,
        );
        diagnostics.push(json!({
            "surface_id":surface_id,"source":"site_fabric","config_path":config_path,"command":command,"args":raw_args,
            "declared_entrypoint":declared,"shared_registry_entrypoint":expected,"entrypoint_exists":exists,
            "classification":classification,
            "durability":{"local_repair_durable":"unknown","reason":"mcp-loader reads site fabric but does not own the generator or VCS ignore rules for this config."},
            "provenance":{"config_source":config_path,"shared_registry_source":if expected.is_some() {json!("@narada-core/mcp-loader-mcp embedded registry")} else {Value::Null},"generator":server.get("generated_by").cloned().unwrap_or(Value::Null),"generated_at":server.get("generated_at").cloned().unwrap_or(Value::Null),"tracking_state":"unknown","tracking_state_reason":"VCS tracking and ignore state are outside mcp-loader authority."},
            "remediation":remediation
        }));
    }
    let mut fallbacks = Vec::new();
    for known in [
        "operator-console-overlay",
        "local-filesystem",
        "structured-command",
        "git",
        "site-inbox",
        "mailbox",
        "graph-mail",
        "calendar",
        "task-lifecycle",
        "agent-context",
        "catalog-observation",
        "runtime-introspection",
        "worker-delegation",
        "delegated-task",
        "sop",
        "scheduler",
        "mcp-registrar",
        "surface-feedback",
        "speech",
        "cloudflare-carrier",
        "site-coherence",
        "site-lifecycle",
        "artifacts",
        "epistemic-graph",
        "ledger-domain",
        "nars-session",
        "quota-meter",
    ] {
        if !servers.contains_key(known) {
            if let Some((entrypoint, _)) = shared_surface_registry(known, &state.surface_root) {
                fallbacks.push(json!({"surface_id":known,"source":"shared_registry_fallback","shared_registry_entrypoint":normalize_path(&entrypoint),"classification":"registry_fallback_available","provenance":{"shared_registry_source":"@narada-core/mcp-loader-mcp embedded registry"}}));
            }
        }
    }
    Ok(
        json!({"schema":"narada.mcp_loader.site_fabric_diagnostics.v1","site_root":site_root,"config_path":if bundle.paths.len()==1 {bundle.paths.first().cloned().map(Value::String).unwrap_or(Value::Null)} else {Value::Null},"config_paths":bundle.paths,"config_exists":true,"diagnostics":diagnostics,"shared_registry_fallbacks":fallbacks}),
    )
}
