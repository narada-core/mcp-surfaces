use crate::full::*;

pub(crate) fn ensure_site_root_allowed(site_root: &str, policy: &Policy) -> Result<(), Diagnostic> {
    let normalized = normalize_path(site_root);
    let candidate = if cfg!(windows) {
        normalized.to_lowercase()
    } else {
        normalized.clone()
    };
    if policy.allowed_site_roots.iter().any(|allowed| {
        let boundary = if cfg!(windows) {
            allowed.to_lowercase()
        } else {
            allowed.clone()
        };
        candidate == boundary || candidate.starts_with(&(boundary + "/"))
    }) {
        Ok(())
    } else {
        Err(Diagnostic::new(
            "site_root_not_allowed",
            format!("site_root_not_allowed:{}", site_root),
        ).with_details(json!({
            "blocked_operation":"site_binding_activation",
            "failed_requirement":"materialized_site_authority",
            "requested_site_root":site_root,
            "unaffected_authority":["other_materialized_sites","static_carrier_surfaces"],
            "repair_owner":"carrier_materialization",
            "agent_may_repair":false,
            "restart_required":true,
            "remediation":"Register the Site root in the User Site authority, rematerialize carriers, and restart. Do not infer authority from the current working directory."
        })))
    }
}

pub(crate) fn ensure_surface_allowed(
    surface_id: &str,
    site_root: &str,
    policy: &Policy,
    state: &LoaderState,
) -> Result<(), Diagnostic> {
    let bundle = read_site_fabric(site_root)?;
    let servers = bundle
        .fabric
        .get("mcpServers")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    let allowed = match &policy.allowed_surface_ids {
        None => {
            find_site_server(&servers, surface_id)?.is_some()
                || shared_surface_registry(surface_id, &state.surface_root).is_some()
        }
        Some(ids) => {
            find_site_server(&servers, surface_id)?.is_some_and(|(key, _)| ids.contains(&key))
                || ids.contains(&surface_id.to_string())
        }
    };
    if allowed {
        Ok(())
    } else {
        Err(Diagnostic::new(
            "surface_not_allowed",
            format!("surface_not_allowed:{}", surface_id),
        ))
    }
}

pub(crate) fn ensure_entrypoint_allowed(
    site_root: &str,
    entrypoint: &str,
    policy: &Policy,
) -> Result<(), Diagnostic> {
    let normalized = normalize_path(entrypoint);
    for prefix in &policy.allowed_entrypoint_prefixes {
        let expanded = prefix.replace("{site_root}", &normalize_path(site_root));
        if normalized == expanded || normalized.starts_with(&(expanded + "/")) {
            return Ok(());
        }
    }
    Err(Diagnostic::new(
        "entrypoint_not_allowed",
        format!("entrypoint_not_allowed:{}", entrypoint),
    ))
}

pub(crate) fn assert_binding_admission_available(state: &LoaderState) -> Result<(), Diagnostic> {
    if state.binding_admission.is_some() || state.standalone_ambient_attachment {
        Ok(())
    } else {
        Err(Diagnostic::new("mcp_binding_admission_required", "mcp_binding_admission_required")
            .with_details(json!({"child_spawned":false,"remediation":"Launch through an admitted Narada carrier session or use --standalone-ambient-attachment only for explicit development fixtures."})))
    }
}

pub(crate) fn canonical_binding_id(
    site_id: Option<&str>,
    surface_id: &str,
    declared: Option<&str>,
) -> String {
    declared
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
        .or_else(|| {
            site_id
                .filter(|value| !value.is_empty())
                .map(|site| format!("{site}-{surface_id}"))
        })
        .unwrap_or_default()
}
