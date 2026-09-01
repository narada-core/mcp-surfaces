
fn contract_method_admitted(field: &str, method: &str) -> bool {
    contract_string_array(&format!("/request_admission/{field}")).contains(&method)
}

fn contract_tool_admitted(options: &Options, request: &Value) -> bool {
    let Some(tool_name) = request.pointer("/params/name").and_then(Value::as_str) else {
        return false;
    };
    if contract_string_array("/request_admission/proxy_tool_calls").contains(&tool_name) {
        return true;
    }
    contract_value("/request_admission/allowed_tool_calls")
        .as_array()
        .expect("orientation_entry_enforcement_contract_allowed_tool_calls_invalid")
        .iter()
        .any(|candidate| {
            candidate.get("surface_id").and_then(Value::as_str) == options.surface_id.as_deref()
                && candidate
                    .get("tool_names")
                    .and_then(Value::as_array)
                    .is_some_and(|names| {
                        names
                            .iter()
                            .filter_map(Value::as_str)
                            .any(|name| name == tool_name)
                    })
        })
}

fn orientation_request_refusal(options: &Options, request: &Value) -> Option<Value> {
    let state = orientation_entry_state();
    if state.get("ordinary_work_gate").and_then(Value::as_str) == Some("open") {
        return None;
    }
    let method = request.get("method").and_then(Value::as_str).unwrap_or("");
    let is_request = request.get("id").is_some();
    let admitted = if is_request {
        contract_method_admitted("allowed_request_methods", method)
            || (method == "tools/call" && contract_tool_admitted(options, request))
    } else {
        contract_method_admitted("allowed_notification_methods", method)
    };
    if admitted {
        return None;
    }
    Some(state)
}

#[derive(Clone)]
struct WireMessage {
    value: Value,
    framed: bool,
}

enum Event {
    Carrier(WireMessage),
    CarrierClosed,
    Child(WireMessage),
    ChildOutputClosed,
    ChildStderr(Vec<u8>),
}

struct Pending {
    method: String,
    framed: bool,
    deadline: Instant,
    effective_timeout_ms: u64,
    requested_transport_timeout_ms: Option<u64>,
}

#[derive(Clone)]
struct FileSnapshot {
    path: PathBuf,
    exists: bool,
    modified_ms: Option<u128>,
    size: Option<u64>,
    sha256: Option<String>,
}

#[derive(Clone)]
struct FreshnessTracker {
    started_at: String,
    proxy_runtime: FileSnapshot,
    child_runtime: FileSnapshot,
}

struct NativeStartupTrace {
    started_at: String,
    started_clock: Instant,
    path: PathBuf,
    events: Vec<Value>,
    completed: bool,
}

fn main() {
    let args = env::args().skip(1).collect::<Vec<_>>();
    let result = match args.first().map(String::as_str) {
        Some("proxy") => run_proxy(&args[1..]),
        Some("filesystem") => filesystem::run(&args[1..]),
        Some("git") => git::run(&args[1..]),
        Some("structured-command") => structured_command::run(&args[1..]),
        Some("structured-command-background") => structured_command::run_background(&args[1..]),
        Some(other) => Err(format!("narada_mcp_runtime_unknown_applet:{other}")),
        None => Err("narada_mcp_runtime_applet_required".to_string()),
    };
    if let Err(error) = result {
        eprintln!("{error}");
        std::process::exit(1);
    }
}

fn parse_options(args: &[String]) -> Result<Options, String> {
    if args.iter().any(|arg| arg == "--list-runtime-instances") {
        return Err("list_runtime_instances_dispatched_separately".to_string());
    }
    let split = args
        .iter()
        .position(|arg| arg == "--")
        .unwrap_or(args.len());
    let mut values = HashMap::<String, String>::new();
    let mut index = 0;
    while index < split {
        let key = &args[index];
        if !key.starts_with("--") {
            return Err(format!("mcp_runtime_proxy_unknown_argument:{key}"));
        }
        let value = args
            .get(index + 1)
            .ok_or_else(|| format!("mcp_runtime_proxy_argument_value_required:{key}"))?;
        values.insert(key.clone(), value.clone());
        index += 2;
    }
    let contract = values
        .get("--runtime-contract-version")
        .map(|value| positive(value, "runtime_contract_version"))
        .transpose()?;
    let child_command = values
        .get("--child-command")
        .cloned()
        .ok_or("mcp_runtime_proxy_missing_child_command")?;
    let child_invocation_kind = values
        .get("--child-invocation-kind")
        .cloned()
        .unwrap_or_else(|| "entrypoint".to_string());
    if child_invocation_kind != "entrypoint"
        && child_invocation_kind != "native_applet"
        && child_invocation_kind != "native_entrypoint"
    {
        return Err("mcp_runtime_proxy_invalid_child_invocation_kind".to_string());
    }
    let child_applet = values.get("--child-applet").cloned();
    if child_invocation_kind == "native_applet" && child_applet.is_none() {
        return Err("mcp_runtime_proxy_missing_child_applet".to_string());
    }
    let child_prefix_args = values
        .get("--child-prefix-args")
        .map(|value| {
            serde_json::from_str::<Vec<String>>(value)
                .map_err(|_| "mcp_runtime_proxy_invalid_child_prefix_args".to_string())
        })
        .transpose()?
        .unwrap_or_default();
    let entrypoint = values
        .get("--entrypoint")
        .map(PathBuf::from)
        .ok_or("mcp_runtime_proxy_missing_entrypoint")?;
    let registrar_entrypoint = values.get("--registrar-entrypoint").map(PathBuf::from);
    let registrar_command = values.get("--registrar-command").cloned();
    if contract.unwrap_or(0) >= 3 && registrar_entrypoint.is_some() && registrar_command.is_none() {
        return Err("mcp_runtime_proxy_missing_registrar_command".to_string());
    }
    let diagnostics_dir = values
        .get("--diagnostics-dir")
        .map(PathBuf::from)
        .unwrap_or_else(default_diagnostics_dir);
    Ok(Options {
        child_command,
        entrypoint: absolute(entrypoint),
        child_invocation_kind,
        child_applet,
        child_prefix_args,
        child_args: if split < args.len() {
            args[split + 1..].to_vec()
        } else {
            Vec::new()
        },
        carrier_id: values.get("--carrier-id").cloned(),
        carrier_kind: values.get("--carrier-kind").cloned(),
        registrar_command,
        registrar_entrypoint: registrar_entrypoint.map(absolute),
        artifact_manifest: values
            .get("--artifact-manifest")
            .map(|value| absolute(PathBuf::from(value))),
        materialization_sidecar: values
            .get("--materialization-sidecar")
            .map(|value| absolute(PathBuf::from(value))),
        surface_id: values.get("--surface-id").cloned(),
        request_timeout_ms: values
            .get("--request-timeout-ms")
            .map(|value| positive(value, "request_timeout_ms"))
            .transpose()?
            .unwrap_or(DEFAULT_REQUEST_TIMEOUT_MS),
        tool_timeout_grace_ms: values
            .get("--tool-timeout-grace-ms")
            .map(|value| positive(value, "tool_timeout_grace_ms"))
            .transpose()?
            .unwrap_or(DEFAULT_TOOL_TIMEOUT_GRACE_MS),
        diagnostics_dir: absolute(diagnostics_dir),
        liveness_check_ms: values
            .get("--liveness-check-ms")
            .map(|value| positive(value, "liveness_check_ms"))
            .transpose()?
            .unwrap_or(DEFAULT_LIVENESS_CHECK_MS),
        orphan_grace_ms: values
            .get("--orphan-grace-ms")
            .map(|value| positive(value, "orphan_grace_ms"))
            .transpose()?
            .unwrap_or(DEFAULT_ORPHAN_GRACE_MS),
        runtime_contract_version: contract,
    })
}
