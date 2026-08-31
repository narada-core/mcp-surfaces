use crate::full::*;

pub(crate) fn parse_options(args: Vec<String>) -> Result<Options, Diagnostic> {
    let mut options = Options::default();
    let mut allowed_roots = Vec::new();
    let mut allowed_prefixes = Vec::new();
    let mut allowed_surfaces = Vec::new();
    let mut allowed_env = Vec::new();
    let mut index = 0;
    while index < args.len() {
        let arg = &args[index];
        let mut next = || -> Result<String, Diagnostic> {
            index += 1;
            args.get(index).cloned().ok_or_else(|| {
                Diagnostic::new("argument_required", format!("argument_required:{}", arg))
            })
        };
        match arg.as_str() {
            "--allowed-site-root" => allowed_roots.push(next()?),
            "--allowed-entrypoint-prefix" => allowed_prefixes.push(next()?),
            "--allowed-surface-id" => allowed_surfaces.push(next()?),
            "--allowed-env-var" => allowed_env.push(next()?),
            "--max-connections" => {
                options.max_connections = bounded_usize(&next()?, "--max-connections", 1, 64)?
            }
            "--max-request-bytes" => {
                options.max_request_bytes =
                    bounded_usize(&next()?, "--max-request-bytes", 4096, 16 * 1024 * 1024)?
            }
            "--max-response-bytes" => {
                options.max_response_bytes =
                    bounded_usize(&next()?, "--max-response-bytes", 4096, 64 * 1024 * 1024)?
            }
            "--attach-timeout-ms" => {
                options.attach_timeout_ms =
                    bounded_u64(&next()?, "--attach-timeout-ms", 1000, 300000)?
            }
            "--tool-call-timeout-ms" => {
                options.tool_call_timeout_ms =
                    bounded_u64(&next()?, "--tool-call-timeout-ms", 1000, 900000)?
            }
            "--tool-timeout-grace-ms" => {
                options.tool_call_grace_ms = bounded_u64(
                    &next()?,
                    "--tool-timeout-grace-ms",
                    0,
                    MAX_TOOL_TIMEOUT_GRACE_MS,
                )?
            }
            "--child-command" => options.child_command = Some(next()?),
            "--child-entrypoint" => options.child_entrypoint = Some(next()?),
            "--child-arg" => options.child_args.push(next()?),
            "--binding-admission-path" => options.binding_admission_path = Some(next()?),
            "--binding-admission-digest" => options.binding_admission_digest = Some(next()?),
            "--standalone-ambient-attachment" => options.standalone_ambient_attachment = true,
            "--" => {
                options
                    .child_args
                    .extend(args.iter().skip(index + 1).cloned());
                break;
            }
            _ => {
                return Err(Diagnostic::new(
                    "unknown_argument",
                    format!("unknown_argument:{}", arg),
                ))
            }
        }
        index += 1;
    }
    if !allowed_roots.is_empty() {
        options.allowed_site_roots = Some(allowed_roots);
    }
    if !allowed_prefixes.is_empty() {
        options.allowed_entrypoint_prefixes = Some(allowed_prefixes);
    }
    if !allowed_surfaces.is_empty() {
        options.allowed_surface_ids = Some(allowed_surfaces);
    }
    if !allowed_env.is_empty() {
        options.allowed_env_vars = Some(allowed_env);
    }
    Ok(options)
}

pub(crate) fn bounded_usize(
    value: &str,
    flag: &str,
    min: usize,
    max: usize,
) -> Result<usize, Diagnostic> {
    let parsed = value
        .parse::<usize>()
        .map_err(|_| Diagnostic::new("invalid_argument", format!("invalid_argument:{}", flag)))?;
    Ok(parsed.clamp(min, max))
}

pub(crate) fn bounded_u64(value: &str, flag: &str, min: u64, max: u64) -> Result<u64, Diagnostic> {
    let parsed = value
        .parse::<u64>()
        .map_err(|_| Diagnostic::new("invalid_argument", format!("invalid_argument:{}", flag)))?;
    Ok(parsed.clamp(min, max))
}
