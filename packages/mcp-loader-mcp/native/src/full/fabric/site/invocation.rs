use crate::full::*;

pub(crate) fn shared_surface_registry(
    _surface_id: &str,
    _surface_root: &str,
) -> Option<(String, Vec<String>)> {
    // Site fabric is the sole launch authority. Keeping a second compiled-in
    // registry made stale TypeScript paths silently executable after migration.
    None
}

pub(crate) fn extract_runtime_entrypoint(command: &str, args: &[String]) -> Option<String> {
    if is_runtime_proxy_command(command) && args.first().map(String::as_str) == Some("proxy") {
        return extract_proxy_child_entrypoint(args);
    }
    let normalized = command.trim().replace('\\', "/");
    let base = normalized
        .rsplit('/')
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase();
    if [
        "node", "node.exe", "node.cmd", "bun", "bun.exe", "deno", "deno.exe",
    ]
    .contains(&base.as_str())
    {
        return args
            .iter()
            .find(|arg| arg.ends_with(".mjs") || arg.ends_with(".js") || arg.ends_with(".cjs"))
            .cloned();
    }
    let stripped = command
        .trim()
        .strip_prefix("node --import tsx ")
        .or_else(|| command.trim().strip_prefix("node "));
    if let Some(value) = stripped {
        if !value.trim().is_empty() && value.trim() != "node" {
            return Some(value.trim().to_string());
        }
    }
    args.iter()
        .find(|arg| arg.ends_with(".mjs") || arg.ends_with(".js") || arg.ends_with(".cjs"))
        .cloned()
}

pub(crate) fn remove_entrypoint_arg(args: &[String], entrypoint: &str) -> Vec<String> {
    let normalized = normalize_path(entrypoint);
    let mut removed = false;
    args.iter()
        .filter_map(|arg| {
            if !removed && normalize_path(arg) == normalized {
                removed = true;
                None
            } else {
                Some(arg.clone())
            }
        })
        .collect()
}

pub(crate) fn surface_requirements(server: Option<&Value>) -> Vec<String> {
    let Some(server) = server else {
        return Vec::new();
    };
    let projection = server.get("surface_projection").and_then(Value::as_object);
    let values = projection
        .and_then(|object| object.get("runtime_requirements"))
        .or_else(|| server.get("runtime_requirements"));
    values
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.as_str().and_then(optional_str))
                .collect()
        })
        .unwrap_or_default()
}

pub(crate) fn runtime_matches(requirements: &[String], runtime_kind: Option<&str>) -> bool {
    requirements.is_empty()
        || runtime_kind
            .is_some_and(|kind| requirements.iter().any(|requirement| requirement == kind))
}
