use crate::full::*;

pub(crate) fn runtime_metadata(
    site_root: &str,
    surface_id: &str,
) -> Result<RuntimeMetadata, Diagnostic> {
    let bundle = read_site_fabric(site_root)?;
    let matched = find_site_server(
        bundle
            .fabric
            .get("mcpServers")
            .and_then(Value::as_object)
            .unwrap_or(&Map::new()),
        surface_id,
    )?;
    let server_name = matched
        .as_ref()
        .map(|(name, _)| name.clone())
        .unwrap_or_else(|| surface_id.to_string());
    let server = matched
        .as_ref()
        .map(|(_, server)| server.clone())
        .unwrap_or_else(|| json!({}));
    let projection = server.get("surface_projection").and_then(Value::as_object);
    let projection_id = projection
        .and_then(|object| object.get("id").or_else(|| object.get("projection_id")))
        .and_then(Value::as_str)
        .or_else(|| server.get("projection_id").and_then(Value::as_str))
        .unwrap_or("default")
        .to_string();
    let execution = projection
        .and_then(|object| object.get("execution"))
        .cloned()
        .unwrap_or_else(
            || json!({"adapter":"stdio","tenancy":"session_isolated","replacement":"manual"}),
        );
    let execution = if execution.is_object() {
        execution
    } else {
        json!({"adapter":"stdio","tenancy":"session_isolated","replacement":"manual"})
    };
    let lifecycle = projection
        .and_then(|object| object.get("lifecycle"))
        .or_else(|| server.get("lifecycle"))
        .filter(|value| value.get("mode").and_then(Value::as_str).is_some())
        .cloned()
        .unwrap_or_else(|| json!({"mode":"replayable"}));
    let descriptor = projection
        .and_then(|object| {
            object
                .get("descriptor")
                .or_else(|| object.get("surface_descriptor"))
        })
        .or_else(|| {
            server
                .get("descriptor")
                .or_else(|| server.get("surface_descriptor"))
        })
        .cloned();
    let descriptor_digest = projection
        .and_then(|object| {
            object
                .get("descriptor_digest")
                .or_else(|| object.get("surface_descriptor_digest"))
        })
        .or_else(|| {
            server
                .get("descriptor_digest")
                .or_else(|| server.get("surface_descriptor_digest"))
        })
        .and_then(Value::as_str)
        .map(String::from);
    let declared_digest = projection
        .and_then(|object| {
            object
                .get("tool_contract_digest")
                .or_else(|| object.get("surface_tool_contract_digest"))
        })
        .or_else(|| {
            server
                .get("tool_contract_digest")
                .or_else(|| server.get("surface_tool_contract_digest"))
        })
        .and_then(Value::as_str)
        .map(String::from);
    Ok((
        server_name,
        projection_id,
        execution,
        lifecycle,
        descriptor,
        descriptor_digest,
        declared_digest,
        surface_requirements(Some(&server)),
    ))
}
