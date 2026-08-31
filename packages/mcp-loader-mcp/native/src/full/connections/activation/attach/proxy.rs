pub(crate) fn is_runtime_proxy_command(command: &str) -> bool {
    let base = command
        .replace('\\', "/")
        .rsplit('/')
        .next()
        .unwrap_or(command)
        .to_ascii_lowercase();
    base.contains("narada-mcp-runtime") || base.contains("mcp-runtime-proxy")
}

pub(crate) fn extract_proxy_child_command(args: &[String]) -> Option<String> {
    args.iter()
        .position(|arg| arg == "--child-command")
        .and_then(|idx| args.get(idx + 1))
        .cloned()
}

pub(crate) fn extract_proxy_child_entrypoint(args: &[String]) -> Option<String> {
    args.iter()
        .position(|arg| arg == "--entrypoint")
        .and_then(|idx| args.get(idx + 1))
        .cloned()
}

pub(crate) fn extract_proxy_child_invocation_kind(args: &[String]) -> String {
    args.iter()
        .position(|arg| arg == "--child-invocation-kind")
        .and_then(|idx| args.get(idx + 1))
        .cloned()
        .unwrap_or_else(|| "entrypoint".to_string())
}

pub(crate) fn extract_proxy_child_applet(args: &[String]) -> Option<String> {
    args.iter()
        .position(|arg| arg == "--child-applet")
        .and_then(|idx| args.get(idx + 1))
        .cloned()
}

pub(crate) fn extract_proxy_child_args(args: &[String]) -> Option<Vec<String>> {
    let mut child_args = Vec::new();
    let mut found_separator = false;
    for arg in args {
        if found_separator {
            child_args.push(arg.clone());
        } else if arg == "--" {
            found_separator = true;
        }
    }
    found_separator.then_some(child_args)
}

pub(crate) fn resolve_child_command(command: &str) -> String {
    command.to_string()
}
