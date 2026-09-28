use crate::full::*;

pub(crate) fn open_surface(
    arguments: &JsonObject,
    state: &mut LoaderState,
) -> Result<Value, Diagnostic> {
    let attached = attach_surface(arguments, state)?;
    let connection_id = attached
        .get("connection_id")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            Diagnostic::new(
                "surface_attach_missing_connection_id",
                "surface_attach_missing_connection_id",
            )
        })?
        .to_string();
    let connection = state
        .connections
        .get(&connection_id)
        .ok_or_else(|| Diagnostic::new("connection_not_found", &connection_id))?;
    let handle = format!("{}{}", SURFACE_HANDLE_PREFIX, new_id("h").replace('-', ""));
    let created_ms = now_ms();
    let record = SurfaceHandle {
        handle: handle.clone(),
        logical_connection_id: connection.logical_connection_id.clone(),
        binding_id: connection.binding_id.clone(),
        site_root: connection.site_root.clone(),
        surface_id: connection.surface_id.clone(),
        runtime_kind: connection.runtime_kind.clone(),
        created_at: ms_to_iso(created_ms),
    };
    let created_at = record.created_at.clone();
    state.handles.insert(handle.clone(), record);
    let mut result = json!({
        "schema":"narada.mcp_loader.surface_handle_opened.v1","status":"opened","surface_handle":handle,
        "handle_scope":"loader_process","handle_survives_child_restart":true,"handle_survives_loader_restart":false,
        "logical_connection_id":connection.logical_connection_id,"connection_id":connection.connection_id,
        "binding_id":connection.binding_id,
        "ownership":connection_ownership(connection),"generation_id":connection.generation_id,"site_root":connection.site_root,
        "surface_id":connection.surface_id,"runtime_kind":connection.runtime_kind,"runtime_requirements":connection.runtime_requirements,
        "tool_count":connection.tools.len(),"created_at":created_at,
        "call":{"tool_name":"mcp_loader_call_surface_tool","arguments":{"surface_handle":handle,"tool_name":"<child_tool>","arguments":{}}}
    });
    if arguments
        .get("include_runtime_metadata")
        .and_then(Value::as_bool)
        == Some(true)
    {
        result["runtime_lifecycle"] =
            runtime_lifecycle(Some(&connection.connection_id), Some(&connection.lifecycle));
        result["runtime_freshness"] = runtime_freshness(state);
    }
    Ok(result)
}

pub(crate) fn resume_or_open_surface(
    arguments: &JsonObject,
    state: &mut LoaderState,
) -> Result<Value, Diagnostic> {
    let binding_id = required_string(arguments, "binding_id", "missing_binding_id")?;
    let requested_site_root = normalize_path(&required_string(
        arguments,
        "site_root",
        "missing_site_root",
    )?);
    // Resume is not an independently admitted mutation. Resolve aliases from
    // the materialized admission envelope for identity matching, then let
    // open_surface/attach enforce the admitted attach operation only if no
    // live child can be reused.
    let admitted_entry = state
        .binding_admission
        .as_ref()
        .and_then(|envelope| admitted_binding_entry(envelope, &binding_id));
    let resolved_binding_id = admitted_entry
        .as_ref()
        .and_then(|entry| entry.get("binding_id"))
        .and_then(Value::as_str)
        .unwrap_or(binding_id.as_str())
        .to_string();
    let resolved_surface_id = admitted_entry
        .as_ref()
        .and_then(|entry| entry.get("surface_id"))
        .and_then(Value::as_str)
        .map(ToString::to_string);
    if let Some(record) = state.handles.values().find(|handle| {
        state.connections.values().any(|connection| {
            connection.logical_connection_id == handle.logical_connection_id
                && handle.binding_id == connection.binding_id
                && normalize_path(&handle.site_root) == requested_site_root
                && normalize_path(&connection.site_root) == requested_site_root
                && handle.surface_id == connection.surface_id
                && connection_matches_requested_binding(
                    connection.binding_id.as_deref(),
                    &connection.site_root,
                    &connection.surface_id,
                    &resolved_binding_id,
                    resolved_surface_id.as_deref(),
                    &requested_site_root,
                )
                && connection_live(connection)
        })
    }) {
        return Ok(json!({
            "schema":"narada.mcp_loader.surface_handle_resumed.v1",
            "status":"resumed",
            "surface_handle":record.handle,
            "binding_id":binding_id,
            "canonical_binding_id":resolved_binding_id,
            "binding_id_canonicalized":binding_id != resolved_binding_id,
            "site_root":record.site_root,
            "surface_id":record.surface_id,
            "handle_scope":"loader_process"
        }));
    }
    // Handles are a convenience projection, not connection identity.  A prior
    // attach or a concurrent binding inspection may already have established
    // the one live child for this admitted binding without creating a handle.
    if let Some(connection) = state.connections.values().find(|connection| {
        connection_matches_requested_binding(
            connection.binding_id.as_deref(),
            &connection.site_root,
            &connection.surface_id,
            &resolved_binding_id,
            resolved_surface_id.as_deref(),
            &requested_site_root,
        ) && connection_live(connection)
    }) {
        let handle = format!("{}{}", SURFACE_HANDLE_PREFIX, new_id("h").replace('-', ""));
        let record = SurfaceHandle {
            handle: handle.clone(),
            logical_connection_id: connection.logical_connection_id.clone(),
            binding_id: connection.binding_id.clone(),
            site_root: connection.site_root.clone(),
            surface_id: connection.surface_id.clone(),
            runtime_kind: connection.runtime_kind.clone(),
            created_at: ms_to_iso(now_ms()),
        };
        let connection_id = connection.connection_id.clone();
        let site_root = connection.site_root.clone();
        let surface_id = connection.surface_id.clone();
        state.handles.insert(handle.clone(), record);
        return Ok(json!({
            "schema":"narada.mcp_loader.surface_handle_resumed.v1",
            "status":"adopted_existing_connection",
            "surface_handle":handle,
            "connection_id":connection_id,
            "binding_id":binding_id,
            "canonical_binding_id":resolved_binding_id,
            "binding_id_canonicalized":binding_id != resolved_binding_id,
            "site_root":site_root,
            "surface_id":surface_id,
            "handle_scope":"loader_process"
        }));
    }
    let mut result = open_surface(arguments, state)?;
    result["status"] = json!("reopened");
    result["resume_attempted"] = json!(true);
    result["canonical_binding_id"] = json!(resolved_binding_id);
    result["binding_id_canonicalized"] = json!(binding_id != resolved_binding_id);
    Ok(result)
}

fn connection_matches_requested_binding(
    connection_binding_id: Option<&str>,
    connection_site_root: &str,
    connection_surface_id: &str,
    resolved_binding_id: &str,
    resolved_surface_id: Option<&str>,
    requested_site_root: &str,
) -> bool {
    if normalize_path(connection_site_root) != normalize_path(requested_site_root) {
        return false;
    }

    let surface_matches = resolved_surface_id
        .map(|surface| connection_surface_id == surface)
        .unwrap_or(true);
    let binding_matches = match connection_binding_id {
        Some(binding_id) => binding_id == resolved_binding_id,
        None => resolved_surface_id == Some(connection_surface_id),
    };

    surface_matches && binding_matches
}

#[cfg(test)]
mod tests {
    use super::connection_matches_requested_binding;

    #[test]
    fn resume_does_not_reuse_a_shared_surface_from_another_site() {
        assert!(!connection_matches_requested_binding(
            Some("andrey-user-site-inbox"),
            "C:/Users/andrey/Narada",
            "site-inbox",
            "sonar-site-inbox",
            Some("site-inbox"),
            "C:/Users/andrey/src/narada.sonar",
        ));
    }

    #[test]
    fn resume_reuses_only_the_exact_binding_at_the_requested_site_root() {
        assert!(connection_matches_requested_binding(
            Some("sonar-site-inbox"),
            "C:/Users/andrey/src/narada.sonar",
            "site-inbox",
            "sonar-site-inbox",
            Some("site-inbox"),
            "C:/Users/andrey/src/narada.sonar",
        ));
        assert!(!connection_matches_requested_binding(
            Some("sonar-site-inbox"),
            "C:/Users/andrey/Narada",
            "site-inbox",
            "sonar-site-inbox",
            Some("site-inbox"),
            "C:/Users/andrey/src/narada.sonar",
        ));
    }

    #[test]
    fn legacy_bindingless_connection_fallback_is_site_scoped() {
        assert!(connection_matches_requested_binding(
            None,
            "C:/Users/andrey/src/narada.sonar",
            "site-inbox",
            "sonar-site-inbox",
            Some("site-inbox"),
            "C:/Users/andrey/src/narada.sonar",
        ));
        assert!(!connection_matches_requested_binding(
            None,
            "C:/Users/andrey/Narada",
            "site-inbox",
            "sonar-site-inbox",
            Some("site-inbox"),
            "C:/Users/andrey/src/narada.sonar",
        ));
    }
}
