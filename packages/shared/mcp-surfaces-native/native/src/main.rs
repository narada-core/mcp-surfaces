use serde_json::{json, Map, Value};
use std::env;
use std::fs::{create_dir_all, OpenOptions};
use std::io::{self, BufRead, BufReader, Read, Write};
use std::path::PathBuf;
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;
use uuid::Uuid;

mod launcher;
mod calendar;
mod authority;
mod graph_authority;
mod graph_mail_authority;
mod delegated_task;
mod worker_delegation;
mod local_admin;
mod mailbox;
mod host_contracts;
mod runtime_introspection;
mod simple_surfaces;
mod site_coherence;
mod site_inbox;
mod site_loop;
mod surface_feedback;
mod scheduler;
mod scheduler_activation;
mod sop;

const LEGACY_PROTOCOL_VERSION: &str = "2024-11-05";
const MODERN_PROTOCOL_VERSION: &str = "2026-07-28";

#[derive(Clone, Debug)]
struct Options {
    surface_id: String,
    site_root: PathBuf,
    log_root: Option<PathBuf>,
    registry_path: Option<PathBuf>,
    native_authority: bool,
    environment: Vec<(String, String)>,
}

fn main() {
    if let Err(error) = run() {
        let _ = writeln!(io::stderr(), "{error}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), String> {
    let options = parse_options(env::args().skip(1).collect())?;
    for (key, value) in &options.environment {
        env::set_var(key, value);
    }
    if options.native_authority && options.surface_id == "calendar" {
        env::set_var("NARADA_NATIVE_GRAPH_AUTHORITY", "1");
    }
    if options.native_authority && options.surface_id == "graph-mail" {
        env::set_var("NARADA_NATIVE_GRAPH_MAIL_AUTHORITY", "1");
    }
    let stdin = io::stdin();
    let mut reader = BufReader::new(stdin.lock());
    let mut stdout = io::stdout().lock();
    loop {
        let mut first = String::new();
        let read = reader
            .read_line(&mut first)
            .map_err(|error| format!("native_surface_stdin_read_failed:{error}"))?;
        if read == 0 {
            break;
        }
        if first.trim().is_empty() {
            continue;
        }
        let (body, framed) = if first.to_ascii_lowercase().starts_with("content-length:") {
            let mut header = first;
            while !header.contains("\r\n\r\n") && !header.contains("\n\n") {
                let mut line = String::new();
                let read = reader
                    .read_line(&mut line)
                    .map_err(|error| format!("native_surface_header_read_failed:{error}"))?;
                if read == 0 {
                    return Err("native_surface_incomplete_content_length_header".to_string());
                }
                header.push_str(&line);
            }
            let length = header
                .lines()
                .find_map(|line| {
                    line.to_ascii_lowercase()
                        .strip_prefix("content-length:")
                        .map(|value| value.trim().to_string())
                })
                .ok_or("native_surface_content_length_missing")?
                .parse::<usize>()
                .map_err(|_| "native_surface_content_length_invalid".to_string())?;
            let mut body = vec![0_u8; length];
            reader
                .read_exact(&mut body)
                .map_err(|error| format!("native_surface_content_length_read_failed:{error}"))?;
            (body, true)
        } else {
            (first.into_bytes(), false)
        };
        let request: Value = serde_json::from_slice(&body)
            .map_err(|error| format!("native_surface_invalid_json:{error}"))?;
        if let Some(response) = handle_request(&request, &options) {
            let encoded = serde_json::to_string(&response)
                .map_err(|error| format!("native_surface_response_encode_failed:{error}"))?;
            if framed {
                write!(
                    stdout,
                    "Content-Length: {}\r\n\r\n{encoded}",
                    encoded.as_bytes().len()
                )
                .map_err(|error| format!("native_surface_stdout_write_failed:{error}"))?;
            } else {
                writeln!(stdout, "{encoded}")
                    .map_err(|error| format!("native_surface_stdout_write_failed:{error}"))?;
            }
            stdout
                .flush()
                .map_err(|error| format!("native_surface_stdout_flush_failed:{error}"))?;
        }
    }
    Ok(())
}
fn parse_options(args: Vec<String>) -> Result<Options, String> {
    let mut surface_id = None;
    let mut site_root = None;
    let mut log_root = None;
    let mut registry_path = None;
    let mut native_authority = false;
    let mut environment = Vec::new();
    let mut index = 0;
    while index < args.len() {
        let key = args[index].as_str();
        if key == "--native-authority" {
            native_authority = true;
            index += 1;
            continue;
        }
        let value = args
            .get(index + 1)
            .ok_or_else(|| format!("native_surface_argument_value_required:{key}"))?;
        match key {
            "--surface-id" => surface_id = Some(value.clone()),
            "--site-root" => site_root = Some(PathBuf::from(value)),
            "--narada-root" => site_root = Some(PathBuf::from(value)),
            "--feedback-root" | "--output-root" | "--repo-root" | "--sop-root" => {
                site_root = Some(PathBuf::from(value));
            }
            "--user-site-root" => {
                site_root = Some(PathBuf::from(value));
                environment.push(("NARADA_USER_SITE_ROOT".to_string(), value.clone()));
            }
            "--task-root" => {
                if site_root.is_none() {
                    site_root = Some(PathBuf::from(value));
                }
                environment.push(("NARADA_DELEGATED_TASK_ROOT".to_string(), value.clone()));
            }
            "--allowed-root" => {
                if site_root.is_none() {
                    site_root = Some(PathBuf::from(value));
                }
            }
            "--log-root" => log_root = Some(PathBuf::from(value)),
            "--registry-path" => registry_path = Some(PathBuf::from(value)),
            "--projection-id" => {
                let _ = value;
            }
            "--canonical-feedback-root" => environment.push((
                "NARADA_SURFACE_FEEDBACK_ROOT".to_string(),
                value.clone(),
            )),
            "--task-lifecycle-root" => environment.push((
                "NARADA_TASK_LIFECYCLE_ROOT".to_string(),
                value.clone(),
            )),
            "--site-id" => {
                environment.push(("NARADA_SITE_ID".to_string(), value.clone()))
            }
            "--owned-surface-id" => {
                if let Some((_, owned)) = environment
                    .iter_mut()
                    .find(|(candidate, _)| candidate == "NARADA_OWNED_SURFACE_IDS")
                {
                    if !owned.is_empty() {
                        owned.push(',');
                    }
                    owned.push_str(value);
                } else {
                    environment.push(("NARADA_OWNED_SURFACE_IDS".to_string(), value.clone()));
                }
            }
            "--feedback-discovery-root" => {
                if let Some((_, roots)) = environment
                    .iter_mut()
                    .find(|(candidate, _)| candidate == "NARADA_FEEDBACK_DISCOVERY_ROOTS")
                {
                    if !roots.is_empty() {
                        roots.push(';');
                    }
                    roots.push_str(value);
                } else {
                    environment.push(("NARADA_FEEDBACK_DISCOVERY_ROOTS".to_string(), value.clone()));
                }
            }
            "--projection" => environment.push((
                "NARADA_NARS_SESSION_PROJECTION".to_string(),
                value.clone(),
            )),
            "--source-kind" => environment.push((
                "NARADA_NARS_SESSION_SOURCE_KIND".to_string(),
                value.clone(),
            )),
            "--operator-id" => {
                environment.push(("NARADA_OPERATOR_ID".to_string(), value.clone()))
            }
            "--run-root" => environment.push((
                "NARADA_WORKER_RUN_ROOT".to_string(),
                value.clone(),
            )),
            "--sops-dir" => {
                environment.push(("NARADA_SOPS_DIR".to_string(), value.clone()))
            }
            "--provider-registry-path" => environment.push((
                "NARADA_SPEECH_PROVIDER_REGISTRY_PATH".to_string(),
                value.clone(),
            )),
            "--server-name" => {
                environment.push(("NARADA_MCP_SERVER_NAME".to_string(), value.clone()))
            }
            _ => return Err(format!("native_surface_unknown_argument:{key}")),
        }
        index += 2;
    }
    let surface_id = surface_id.ok_or("native_surface_missing_surface_id")?;
    let site_root =
        site_root.unwrap_or_else(|| env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
    Ok(Options {
        surface_id,
        site_root,
        log_root,
        registry_path,
        native_authority,
        environment,
    })
}

fn handle_request(request: &Value, options: &Options) -> Option<Value> {
    let object = request.as_object()?;
    let method = object
        .get("method")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if method.starts_with("notifications/") {
        return None;
    }
    let id = object.get("id").cloned().unwrap_or(Value::Null);
    let params = object
        .get("params")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    let modern = is_modern_request(&params);
    let result = if modern {
        validate_modern_request(&params).and_then(|_| match method {
            "server/discover" => Ok(server_discover_result(options)),
            "tools/list" => Ok(modern_result(
                json!({
                    "tools": list_tools(&options.surface_id),
                    "ttlMs": 300_000,
                    "cacheScope": "public"
                }),
                options,
            )),
            "tools/call" => call_tool(&options.surface_id, &params, options)
                .map(|value| modern_result(value, options)),
            method
                if options.surface_id == "site-inbox"
                    && matches!(method, "prompts/list" | "prompts/get" | "completion/complete" | "logging/setLevel") =>
            {
                site_inbox::auxiliary(method, &params).map(|value| modern_result(value, options))
            }
            method
                if options.surface_id == "calendar"
                    && matches!(method, "prompts/list" | "prompts/get" | "completion/complete" | "logging/setLevel") =>
            {
                calendar::auxiliary(method, &params).map(|value| modern_result(value, options))
            }
            method
                if options.surface_id == "site-loop"
                    && matches!(method, "prompts/list" | "prompts/get" | "completion/complete" | "logging/setLevel") =>
            {
                site_loop::auxiliary(method, &params).map(|value| modern_result(value, options))
            }
            method
                if options.surface_id == "surface-feedback"
                    && matches!(method, "prompts/list" | "prompts/get" | "completion/complete" | "logging/setLevel") =>
            {
                surface_feedback::auxiliary(method, &params).map(|value| modern_result(value, options))
            }
            method
                if options.surface_id == "sop"
                    && matches!(method, "prompts/list" | "prompts/get" | "completion/complete" | "logging/setLevel") =>
            {
                sop::auxiliary(method, &params).map(|value| modern_result(value, options))
            }
            method
                if options.surface_id == "delegated-task"
                    && matches!(method, "prompts/list" | "prompts/get" | "completion/complete" | "logging/setLevel") =>
            {
                delegated_task::auxiliary(method, &params).map(|value| modern_result(value, options))
            }
            method
                if options.surface_id == "worker-delegation"
                    && matches!(method, "prompts/list" | "prompts/get" | "completion/complete" | "logging/setLevel") =>
            {
                worker_delegation::auxiliary(method, &params).map(|value| modern_result(value, options))
            }
            method
                if matches!(options.surface_id.as_str(), "artifacts" | "nars-session" | "quota-meter")
                    && matches!(method, "prompts/list" | "prompts/get" | "completion/complete" | "logging/setLevel") =>
            {
                local_admin::auxiliary(&options.surface_id, method, &params).map(|value| modern_result(value, options))
            }
            method
                if options.surface_id == "mailbox"
                    && matches!(method, "prompts/list" | "prompts/get" | "completion/complete" | "logging/setLevel") =>
            {
                mailbox::auxiliary(method, &params).map(|value| modern_result(value, options))
            }
            method
                if options.surface_id == "scheduler"
                    && matches!(method, "prompts/list" | "prompts/get" | "completion/complete" | "logging/setLevel") =>
            {
                scheduler::auxiliary(method, &params).map(|value| modern_result(value, options))
            }
            method
                if matches!(options.surface_id.as_str(), "browser-control" | "operator-console-overlay" | "cloudflare-carrier" | "speech" | "graph-mail")
                    && matches!(method, "prompts/list" | "prompts/get" | "completion/complete" | "logging/setLevel") =>
            {
                host_contracts::auxiliary(&options.surface_id, method, &params).map(|value| modern_result(value, options))
            }
            "initialize" => Err(diagnostic(
                "initialize_removed",
                "The 2026-07-28 protocol has no initialize handshake.",
                json!({ "protocolVersion": MODERN_PROTOCOL_VERSION }),
            )),
            _ => Err(diagnostic(
                "unsupported_mcp_method",
                &format!("unsupported_mcp_method:{method}"),
                json!({ "method": method }),
            )),
        })
    } else {
        match method {
            method
                if options.surface_id == "site-inbox"
                    && matches!(method, "prompts/list" | "prompts/get" | "completion/complete" | "logging/setLevel") =>
            {
                site_inbox::auxiliary(method, &params)
            }
            method
                if options.surface_id == "calendar"
                    && matches!(method, "prompts/list" | "prompts/get" | "completion/complete" | "logging/setLevel") =>
            {
                calendar::auxiliary(method, &params)
            }
            method
                if options.surface_id == "site-loop"
                    && matches!(method, "prompts/list" | "prompts/get" | "completion/complete" | "logging/setLevel") =>
            {
                site_loop::auxiliary(method, &params)
            }
            method
                if options.surface_id == "surface-feedback"
                    && matches!(method, "prompts/list" | "prompts/get" | "completion/complete" | "logging/setLevel") =>
            {
                surface_feedback::auxiliary(method, &params)
            }
            method
                if options.surface_id == "sop"
                    && matches!(method, "prompts/list" | "prompts/get" | "completion/complete" | "logging/setLevel") =>
            {
                sop::auxiliary(method, &params)
            }
            method
                if options.surface_id == "delegated-task"
                    && matches!(method, "prompts/list" | "prompts/get" | "completion/complete" | "logging/setLevel") =>
            {
                delegated_task::auxiliary(method, &params)
            }
            method
                if options.surface_id == "worker-delegation"
                    && matches!(method, "prompts/list" | "prompts/get" | "completion/complete" | "logging/setLevel") =>
            {
                worker_delegation::auxiliary(method, &params)
            }
            method
                if matches!(options.surface_id.as_str(), "artifacts" | "nars-session" | "quota-meter")
                    && matches!(method, "prompts/list" | "prompts/get" | "completion/complete" | "logging/setLevel") =>
            {
                local_admin::auxiliary(&options.surface_id, method, &params)
            }
            method
                if options.surface_id == "mailbox"
                    && matches!(method, "prompts/list" | "prompts/get" | "completion/complete" | "logging/setLevel") =>
            {
                mailbox::auxiliary(method, &params)
            }
            method
                if options.surface_id == "scheduler"
                    && matches!(method, "prompts/list" | "prompts/get" | "completion/complete" | "logging/setLevel") =>
            {
                scheduler::auxiliary(method, &params)
            }
            method
                if matches!(options.surface_id.as_str(), "browser-control" | "operator-console-overlay" | "cloudflare-carrier" | "speech" | "graph-mail")
                    && matches!(method, "prompts/list" | "prompts/get" | "completion/complete" | "logging/setLevel") =>
            {
                host_contracts::auxiliary(&options.surface_id, method, &params)
            }
            "initialize" => Ok(initialize_result(options)),
            "tools/list" => Ok(json!({ "tools": list_tools(&options.surface_id) })),
            "tools/call" => call_tool(&options.surface_id, &params, options),
            "server/discover" => Err(diagnostic(
                "modern_metadata_required",
                "server/discover requires 2026-07-28 request metadata.",
                json!({ "protocolVersion": MODERN_PROTOCOL_VERSION }),
            )),
            _ => Err(diagnostic(
                "unsupported_mcp_method",
                &format!("unsupported_mcp_method:{method}"),
                json!({ "method": method }),
            )),
        }
    };
    Some(match result {
        Ok(result) => json!({ "jsonrpc": "2.0", "id": id, "result": result }),
        Err(error) => {
            json!({ "jsonrpc": "2.0", "id": id, "error": { "code": -32000, "message": error["message"], "data": error } })
        }
    })
}

fn is_modern_request(params: &Map<String, Value>) -> bool {
    params
        .get("_meta")
        .and_then(Value::as_object)
        .and_then(|meta| meta.get("io.modelcontextprotocol/protocolVersion"))
        .and_then(Value::as_str)
        == Some(MODERN_PROTOCOL_VERSION)
}

fn validate_modern_request(params: &Map<String, Value>) -> Result<(), Value> {
    let meta = params
        .get("_meta")
        .and_then(Value::as_object)
        .ok_or_else(|| {
            diagnostic(
                "modern_metadata_required",
                "Modern MCP requests require _meta.",
                Value::Null,
            )
        })?;
    if meta
        .get("io.modelcontextprotocol/clientInfo")
        .and_then(Value::as_object)
        .is_none()
    {
        return Err(diagnostic(
            "modern_metadata_required",
            "Modern MCP requests require clientInfo metadata.",
            json!({ "key": "io.modelcontextprotocol/clientInfo" }),
        ));
    }
    if meta
        .get("io.modelcontextprotocol/clientCapabilities")
        .and_then(Value::as_object)
        .is_none()
    {
        return Err(diagnostic(
            "modern_metadata_required",
            "Modern MCP requests require clientCapabilities metadata.",
            json!({ "key": "io.modelcontextprotocol/clientCapabilities" }),
        ));
    }
    Ok(())
}

fn server_name(options: &Options) -> String {
    match options.surface_id.as_str() {
        "site-inbox" => "narada-site-inbox-mcp".to_string(),
        "calendar" => "narada-calendar-mcp".to_string(),
        "site-loop" => "narada-site-loop-mcp".to_string(),
        "surface-feedback" => "surface-feedback-mcp".to_string(),
        "sop" => "sop-mcp".to_string(),
        "delegated-task" => "delegated-task-mcp".to_string(),
        "worker-delegation" => "worker-delegation-mcp".to_string(),
        "artifacts" => "artifacts-mcp".to_string(),
        "nars-session" => "nars-session-mcp".to_string(),
        "quota-meter" => "quota-meter-mcp".to_string(),
        "mailbox" => "mailbox-mcp".to_string(),
        "browser-control" => "browser-control-mcp".to_string(),
        "operator-console-overlay" => "operator-console-overlay-mcp".to_string(),
        "cloudflare-carrier" => "cloudflare-carrier-mcp".to_string(),
        "speech" => "speech-mcp".to_string(),
        "scheduler" => "scheduler-mcp".to_string(),
        "graph-mail" => "graph-mail-mcp".to_string(),
        "site-lifecycle" => "site-lifecycle-mcp".to_string(),
        "site-registry" => "site-registry-mcp".to_string(),
        "project-state" => "project-state-mcp".to_string(),
        "runtime-introspection" => "runtime-introspection-mcp".to_string(),
        "site-coherence" => "site-coherence-mcp".to_string(),
        "launcher" => "launcher-mcp".to_string(),
        _ => format!("{}-mcp", options.surface_id),
    }
}

fn capabilities(surface_id: &str) -> Value {
    if matches!(surface_id, "site-inbox" | "calendar" | "site-loop" | "surface-feedback" | "sop" | "delegated-task" | "worker-delegation" | "artifacts" | "nars-session" | "quota-meter" | "mailbox" | "browser-control" | "operator-console-overlay" | "cloudflare-carrier" | "speech" | "scheduler" | "graph-mail") {
        json!({"tools":{},"prompts":{},"completions":{},"logging":{}})
    } else {
        json!({"tools":{}})
    }
}

fn initialize_result(options: &Options) -> Value {
    json!({
        "protocolVersion": LEGACY_PROTOCOL_VERSION,
        "capabilities": capabilities(&options.surface_id),
        "serverInfo": { "name": server_name(options), "version": "0.1.0" }
    })
}

fn server_discover_result(options: &Options) -> Value {
    modern_result(
        json!({
            "supportedVersions": [MODERN_PROTOCOL_VERSION, LEGACY_PROTOCOL_VERSION],
            "capabilities": capabilities(&options.surface_id),
            "ttlMs": 3_600_000,
            "cacheScope": "public"
        }),
        options,
    )
}

fn modern_result(value: Value, options: &Options) -> Value {
    let mut result = value.as_object().cloned().unwrap_or_default();
    result.insert("resultType".to_string(), json!("complete"));
    let mut meta = result
        .remove("_meta")
        .and_then(|value| value.as_object().cloned())
        .unwrap_or_default();
    meta.insert(
        "io.modelcontextprotocol/serverInfo".to_string(),
        json!({ "name": server_name(options), "version": "0.1.0" }),
    );
    result.insert("_meta".to_string(), Value::Object(meta));
    Value::Object(result)
}
fn list_tools(surface_id: &str) -> Vec<Value> {
    match surface_id {
        "site-inbox" => site_inbox::list_tools(),
        "calendar" => calendar::list_tools(),
        "site-loop" => site_loop::list_tools(),
        "surface-feedback" => surface_feedback::list_tools(),
        "sop" => sop::list_tools(),
        "delegated-task" => delegated_task::list_tools(),
        "worker-delegation" => worker_delegation::list_tools(),
        "artifacts" | "nars-session" | "quota-meter" => local_admin::list_tools(surface_id),
        "mailbox" => mailbox::list_tools(),
        "scheduler" => scheduler::list_tools(),
        "browser-control" | "operator-console-overlay" | "cloudflare-carrier" | "speech" | "graph-mail" => host_contracts::list_tools(surface_id),
        "catalog-observation" => vec![
            guidance_tool("catalog-observation"),
            tool("catalog_observation_observe", "Observe a provider model catalog through the Narada-owned observation port.", json!({
                "type": "object",
                "properties": {
                    "provider_id": { "type": "string", "description": "Canonical inference-provider resource id." },
                    "observed_at": { "type": "string", "description": "Explicit observation instant in ISO format." },
                    "access_mode": { "type": "string", "enum": ["public", "credentialed", "operator_attested"], "default": "public" }
                },
                "required": ["provider_id", "observed_at"],
                "additionalProperties": false
            }), true),
        ],
        "operator-routing" => vec![
            guidance_tool("operator-routing"),
            tool("operator_route_doctor", "Report operator routing posture, fallback policy, and the suggested spoken acknowledgement shape.", json!({ "type": "object", "properties": {}, "additionalProperties": false }), true),
            tool("operator_route_request", "Compile a transcript into a routing decision and a site-inbox-compatible fallback envelope.", json!({
                "type": "object",
                "properties": {
                    "transcript": { "type": "string", "description": "Transcript text to route." },
                    "target_runtime": { "type": "string", "description": "Target runtime or runtime family to receive the command." },
                    "target_identity": { "type": "string", "default": Value::Null, "description": "Optional target agent identity." },
                    "intent_kind": { "type": "string", "default": Value::Null, "description": "Optional intent classification." },
                    "speaker_agent_id": { "type": "string", "default": Value::Null, "description": "Optional speaker identity to preserve in the route record." },
                    "allow_inbox_fallback": { "type": "boolean", "default": true, "description": "Allow a site-inbox fallback envelope when direct delivery is unavailable." },
                    "request_id": { "type": "string", "default": Value::Null, "description": "Optional stable request identifier." }
                },
                "required": ["transcript", "target_runtime"],
                "additionalProperties": false
            }), false),
        ],
        "site-lifecycle" | "site-registry" | "project-state" => simple_surfaces::list_tools(surface_id),
        "runtime-introspection" => runtime_introspection::list_tools(),
        "site-coherence" => site_coherence::list_tools(),
        "launcher" => launcher::list_tools(),
        _ => Vec::new(),
    }
}

fn guidance_tool(surface_id: &str) -> Value {
    let tool_name = surface_id.replace("-", "_") + "_guidance";
    tool(
        &tool_name,
        &format!("Show model-facing operating guidance for {surface_id} MCP workflows."),
        json!({
            "type": "object",
            "properties": {
                "workflow": { "type": "string", "description": "Optional workflow name or area to focus guidance on." },
                "tool": { "type": "string", "description": "Optional tool name for tool-specific guidance." }
            },
            "additionalProperties": false
        }),
        true,
    )
}

fn tool(name: &str, description: &str, input_schema: Value, read_only: bool) -> Value {
    json!({
        "name": name,
        "description": description,
        "inputSchema": input_schema,
        "annotations": { "title": name, "readOnlyHint": read_only, "destructiveHint": !read_only, "idempotentHint": true, "openWorldHint": false },
        "outputSchema": { "type": "object", "additionalProperties": true }
    })
}

fn call_tool(
    surface_id: &str,
    params: &Map<String, Value>,
    options: &Options,
) -> Result<Value, Value> {
    let name = params.get("name").and_then(Value::as_str).ok_or_else(|| {
        diagnostic(
            "invalid_request",
            "tools/call requires a tool name.",
            Value::Null,
        )
    })?;
    let args = params
        .get("arguments")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    let result = match (surface_id, name) {
        ("catalog-observation", "catalog_observation_guidance") => catalog_guidance(&args),
        ("catalog-observation", "catalog_observation_observe") => catalog_observation(&args),
        ("operator-routing", "operator_routing_guidance") => operator_guidance(&args),
        ("operator-routing", "operator_route_doctor") => operator_route_doctor(options),
        ("operator-routing", "operator_route_request") => operator_route_request(&args, options),
        ("site-inbox", name) => site_inbox::call_tool(name, &args, &options.site_root),
        ("calendar", name) => calendar::call_tool(name, &args, &options.site_root),
        ("site-loop", name) => site_loop::call_tool(name, &args, &options.site_root),
        ("surface-feedback", name) => surface_feedback::call_tool(name, &args, &options.site_root),
        ("sop", name) => sop::call_tool(name, &args, &options.site_root),
        ("delegated-task", name) => delegated_task::call_tool(name, &args, &options.site_root),
        ("worker-delegation", name) => worker_delegation::call_tool(name, &args, &options.site_root),
        ("artifacts", name) | ("nars-session", name) | ("quota-meter", name) => local_admin::call_tool(surface_id, name, &args, &options.site_root),
        ("mailbox", name) => mailbox::call_tool(name, &args, &options.site_root),
        ("graph-mail", name) if graph_mail_authority::enabled() && graph_mail_authority::supports(name) => graph_mail_authority::call_tool(name, &args, &options.site_root),
        ("scheduler", name) => scheduler::call_tool(name, &args, &options.site_root),
        ("browser-control", name) | ("operator-console-overlay", name) | ("cloudflare-carrier", name) | ("speech", name) | ("graph-mail", name) => host_contracts::call_tool(surface_id, name, &args, &options.site_root),
        ("site-lifecycle", name) | ("site-registry", name) | ("project-state", name) => {
            simple_surfaces::call_tool(surface_id, name, &args, &options.site_root)
        }
        ("runtime-introspection", name) => {
            runtime_introspection::call_tool(name, &args, &options.site_root)
        }
        ("site-coherence", name) => site_coherence::call_tool(name, &args, &options.site_root),
        ("launcher", name) => launcher::call_tool(
            name,
            &args,
            &options.site_root,
            options.registry_path.as_deref(),
        ),
        (_, unknown) => {
            return Err(diagnostic(
                "unknown_tool",
                &format!("unknown_tool:{unknown}"),
                json!({ "tool_name": unknown }),
            ))
        }
    }?;
    let is_error = result.get("status").and_then(Value::as_str) == Some("unavailable");
    let mut response = json!({ "content": [{ "type": "text", "text": serde_json::to_string_pretty(&result).unwrap_or_else(|_| result.to_string()) }], "structuredContent": result });
    if is_error {
        response["isError"] = json!(true);
    }
    Ok(response)
}

fn catalog_guidance(_args: &Map<String, Value>) -> Result<Value, Value> {
    Ok(json!({
        "schema": "narada.catalog-observation.guidance.v1",
        "authority": "Narada management owns catalog observation and credential resolution.",
        "boundary": "This MCP surface is read-only and forwards typed observation requests only.",
        "credentials": "Credential values never cross this MCP boundary and never appear in observations.",
        "unavailable": "Without an injected Narada observation port, the surface returns an unavailable observation."
    }))
}

fn catalog_observation(args: &Map<String, Value>) -> Result<Value, Value> {
    let provider_id = required_string(args, "provider_id")?;
    let observed_at = required_string(args, "observed_at")?;
    let access_mode = args
        .get("access_mode")
        .and_then(Value::as_str)
        .unwrap_or("public");
    if !matches!(access_mode, "public" | "credentialed" | "operator_attested") {
        return Err(diagnostic(
            "invalid_request",
            "access_mode must be public, credentialed, or operator_attested.",
            Value::Null,
        ));
    }
    if OffsetDateTime::parse(&observed_at, &Rfc3339).is_err() {
        return Err(diagnostic(
            "invalid_request",
            "observed_at must be an explicit ISO instant.",
            Value::Null,
        ));
    }
    Ok(json!({
        "schema": "narada.invokable-intelligence.catalog-observation.v1",
        "id": format!("catalog-observation:unavailable-{provider_id}"),
        "observed_at": observed_at,
        "inference_provider": { "kind": "inference-provider", "id": provider_id },
        "access_mode": "unavailable",
        "authority": { "kind": "unavailable", "authority_ref": "narada-observation-port:not-injected" },
        "source": { "kind": "unavailable", "reference": "narada-observation-port:not-injected" },
        "status": "unavailable",
        "models": [],
        "diagnostics": [{ "code": "provider-authority-unavailable", "message": "No Narada catalog observation port was injected into this surface process.", "retryable": false }],
        "digest": format!("sha256:{}", "0".repeat(64))
    }))
}

fn operator_guidance(args: &Map<String, Value>) -> Result<Value, Value> {
    let workflow = args
        .get("workflow")
        .and_then(Value::as_str)
        .filter(|v| !v.trim().is_empty());
    let tool = args
        .get("tool")
        .and_then(Value::as_str)
        .filter(|v| !v.trim().is_empty());
    Ok(json!({
        "schema": "narada.mcp_surface.guidance.v0",
        "status": "ok",
        "surface_id": "operator-routing",
        "guidance_tool": "operator_routing_guidance",
        "purpose": "User Site operator transcript-to-target routing and inbox fallback packaging.",
        "requested": { "workflow": workflow, "tool": tool },
        "first_use": ["Call this guidance command when the surface is unfamiliar, when a refusal/error is unclear, or before composing a multi-step workflow.", "Inspect policy/doctor/status tools before mutation or open-world operations.", "Use bounded list/search/query tools for discovery, then show/read/detail tools before acting on a specific object.", "Preserve structuredContent as authoritative evidence; text content is for assistant readability."],
        "boundaries": ["Guidance is read-only model-facing operating advice.", "Guidance does not weaken policy, authorize mutation, or replace tool schemas.", "The owning MCP surface remains authoritative for state and enforcement."]
    }))
}

fn operator_route_doctor(options: &Options) -> Result<Value, Value> {
    Ok(json!({
        "schema": "narada.operator_routing.doctor.v1",
        "status": "ok",
        "server_name": "operator-routing-mcp",
        "site_root": options.site_root.to_string_lossy(),
        "direct_delivery_supported": false,
        "fallback_channel": "site-inbox",
        "suggested_speech": { "provider": "openai_api", "model": "tts-1", "voice": "nova", "text": "Request recorded. Direct delivery to that runtime is not available from this surface. I can route it through the admitted inbox path." }
    }))
}

fn operator_route_request(args: &Map<String, Value>, options: &Options) -> Result<Value, Value> {
    let transcript = required_string(args, "transcript")?;
    let target_runtime = required_string(args, "target_runtime")?;
    let target_identity = optional_string(args, "target_identity");
    let intent_kind = optional_string(args, "intent_kind");
    let speaker_agent_id = optional_string(args, "speaker_agent_id");
    let allow_inbox_fallback = args
        .get("allow_inbox_fallback")
        .and_then(Value::as_bool)
        .unwrap_or(true);
    let request_id = optional_string(args, "request_id").unwrap_or_else(|| {
        format!(
            "route_{}_{}",
            compact_timestamp(),
            &Uuid::new_v4().to_string()[..8]
        )
    });
    let recorded_at = now_iso();
    let spoken_text = if allow_inbox_fallback {
        "Request recorded. Direct delivery to that runtime is not available from this surface. I can route it through the admitted inbox path."
    } else {
        "Request recorded. Direct delivery to that runtime is not available from this surface, and no fallback path was enabled."
    };
    let route_kind = if allow_inbox_fallback {
        "inbox_fallback_draft"
    } else {
        "unroutable"
    };
    let inbox_envelope = if allow_inbox_fallback {
        Some(json!({
            "kind": "command_request",
            "title": target_identity.as_ref().map(|id| format!("Route request for {id}")).unwrap_or_else(|| format!("Route request for {target_runtime}")),
            "summary": transcript.chars().take(240).collect::<String>(),
            "principal": speaker_agent_id,
            "target_role": Value::Null,
            "severity": 35,
            "authority_level": "operator_confirmed",
            "payload": { "request_id": request_id, "recorded_at": recorded_at, "transcript": transcript, "target_runtime": target_runtime, "target_identity": target_identity, "intent_kind": intent_kind, "speaker_agent_id": speaker_agent_id, "spoken_acknowledgement": spoken_text, "suggested_delivery_channel": "site-inbox" }
        }))
    } else {
        None
    };
    let route_record = json!({
        "schema": "narada.operator_routing.route_request.v1",
        "status": if allow_inbox_fallback { "drafted_for_site_inbox" } else { "unroutable" },
        "request_id": request_id,
        "recorded_at": recorded_at,
        "direct_delivery_supported": false,
        "direct_delivery_attempted": false,
        "direct_delivery_reason": "no_runtime_ingress_available",
        "target_runtime": target_runtime,
        "target_identity": target_identity,
        "intent_kind": intent_kind,
        "speaker_agent_id": speaker_agent_id,
        "transcript": transcript,
        "routing": { "target_runtime": target_runtime, "target_identity": target_identity, "route_kind": route_kind, "fallback_channel": if allow_inbox_fallback { json!("site-inbox") } else { Value::Null }, "next_step": if allow_inbox_fallback { "submit_to_site_inbox" } else { "none" } },
        "spoken_acknowledgement": { "provider": "openai_api", "model": "tts-1", "voice": "nova", "text": spoken_text },
        "inbox_envelope": inbox_envelope
    });
    let log_path = append_route_record(&route_record, options)?;
    let mut result = route_record.as_object().cloned().unwrap_or_default();
    result.insert(
        "log_path".to_string(),
        Value::String(log_path.to_string_lossy().to_string()),
    );
    Ok(Value::Object(result))
}

fn append_route_record(record: &Value, options: &Options) -> Result<PathBuf, Value> {
    let root = options.log_root.clone().unwrap_or_else(|| {
        options
            .site_root
            .join(".narada")
            .join("runtime")
            .join("operator-routing")
    });
    create_dir_all(&root).map_err(|error| {
        diagnostic(
            "operator_route_log_create_failed",
            &error.to_string(),
            Value::Null,
        )
    })?;
    let path = root.join("operator-routing-log.jsonl");
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .map_err(|error| {
            diagnostic(
                "operator_route_log_open_failed",
                &error.to_string(),
                Value::Null,
            )
        })?;
    let line = serde_json::to_string(record).map_err(|error| {
        diagnostic(
            "operator_route_log_encode_failed",
            &error.to_string(),
            Value::Null,
        )
    })?;
    writeln!(file, "{line}").map_err(|error| {
        diagnostic(
            "operator_route_log_write_failed",
            &error.to_string(),
            Value::Null,
        )
    })?;
    Ok(path)
}

fn required_string(args: &Map<String, Value>, key: &str) -> Result<String, Value> {
    optional_string(args, key).ok_or_else(|| {
        diagnostic(
            "required_argument_missing",
            &format!("required_argument_missing:{key}"),
            json!({ "key": key }),
        )
    })
}

fn optional_string(args: &Map<String, Value>, key: &str) -> Option<String> {
    args.get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
}

fn diagnostic(code: &str, message: &str, details: Value) -> Value {
    let mut object = Map::new();
    object.insert("code".to_string(), Value::String(code.to_string()));
    object.insert("message".to_string(), Value::String(message.to_string()));
    if !details.is_null() {
        object.insert("details".to_string(), details);
    }
    Value::Object(object)
}

fn now_iso() -> String {
    OffsetDateTime::now_utc()
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_else(|_| "1970-01-01T00:00:00Z".to_string())
}

fn compact_timestamp() -> String {
    now_iso()
        .replace(['-', ':', '.'], "")
        .chars()
        .take(15)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_options() -> Options {
        Options {
            surface_id: "catalog-observation".to_string(),
            site_root: PathBuf::from("."),
            log_root: None,
            registry_path: None,
            native_authority: false,
            environment: Vec::new(),
        }
    }

    fn parsed_options(args: &[&str]) -> Options {
        parse_options(args.iter().map(|value| (*value).to_string()).collect())
            .expect("registrar arguments should parse")
    }

    fn environment_value<'a>(options: &'a Options, key: &str) -> Option<&'a str> {
        options
            .environment
            .iter()
            .rev()
            .find(|(candidate, _)| candidate == key)
            .map(|(_, value)| value.as_str())
    }

    #[test]
    fn registrar_native_surface_argument_profiles_are_launchable() {
        let delegated = parsed_options(&[
            "--surface-id", "delegated-task", "--site-root", "site", "--task-root",
            "task", "--allowed-root", "site",
        ]);
        assert_eq!(delegated.site_root, PathBuf::from("site"));
        assert_eq!(environment_value(&delegated, "NARADA_DELEGATED_TASK_ROOT"), Some("task"));

        let nars = parsed_options(&[
            "--surface-id", "nars-session", "--projection", "user-site-operator",
            "--user-site-root", "user-site", "--source-kind", "operator",
            "--operator-id", "andrey",
        ]);
        assert_eq!(nars.site_root, PathBuf::from("user-site"));
        assert_eq!(environment_value(&nars, "NARADA_NARS_SESSION_PROJECTION"), Some("user-site-operator"));
        assert_eq!(environment_value(&nars, "NARADA_USER_SITE_ROOT"), Some("user-site"));

        let scheduler = parsed_options(&[
            "--surface-id", "scheduler", "--allowed-root", "site",
        ]);
        assert_eq!(scheduler.site_root, PathBuf::from("site"));

        let coherence = parsed_options(&[
            "--surface-id", "site-coherence", "--repo-root", "repo",
        ]);
        assert_eq!(coherence.site_root, PathBuf::from("repo"));

        let sop = parsed_options(&[
            "--surface-id", "sop", "--sop-root", "site", "--server-name", "site-sop",
            "--sops-dir", "site/.narada/sops",
        ]);
        assert_eq!(sop.site_root, PathBuf::from("site"));
        assert_eq!(environment_value(&sop, "NARADA_SOPS_DIR"), Some("site/.narada/sops"));

        let speech = parsed_options(&[
            "--surface-id", "speech", "--provider-registry-path", "providers.json",
        ]);
        assert_eq!(environment_value(&speech, "NARADA_SPEECH_PROVIDER_REGISTRY_PATH"), Some("providers.json"));

        let feedback = parsed_options(&[
            "--surface-id", "surface-feedback", "--feedback-root", "feedback",
            "--canonical-feedback-root", "canonical", "--task-lifecycle-root", "site",
            "--site-id", "andrey-user", "--owned-surface-id", "calendar",
            "--owned-surface-id", "site-loop",
        ]);
        assert_eq!(feedback.site_root, PathBuf::from("feedback"));
        assert_eq!(environment_value(&feedback, "NARADA_SURFACE_FEEDBACK_ROOT"), Some("canonical"));
        assert_eq!(environment_value(&feedback, "NARADA_TASK_LIFECYCLE_ROOT"), Some("site"));
        assert_eq!(environment_value(&feedback, "NARADA_SITE_ID"), Some("andrey-user"));
        assert_eq!(environment_value(&feedback, "NARADA_OWNED_SURFACE_IDS"), Some("calendar,site-loop"));

        let worker = parsed_options(&[
            "--surface-id", "worker-delegation", "--site-root", "site", "--allowed-root",
            "site", "--run-root", "site/.narada/runtime/worker-delegation",
        ]);
        assert_eq!(worker.site_root, PathBuf::from("site"));
        assert_eq!(environment_value(&worker, "NARADA_WORKER_RUN_ROOT"), Some("site/.narada/runtime/worker-delegation"));
    }

    #[test]
    fn unrecognized_native_surface_arguments_still_refuse() {
        let error = parse_options(vec![
            "--surface-id".to_string(),
            "scheduler".to_string(),
            "--not-a-registrar-argument".to_string(),
            "value".to_string(),
        ])
        .expect_err("unknown arguments must refuse");
        assert_eq!(error, "native_surface_unknown_argument:--not-a-registrar-argument");
    }

    #[test]
    fn legacy_initialize_remains_available() {
        let response = handle_request(
            &json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05"}}),
            &test_options(),
        ).expect("response");
        assert_eq!(
            response["result"]["protocolVersion"],
            LEGACY_PROTOCOL_VERSION
        );
        assert!(response["result"]["resultType"].is_null());
    }

    #[test]
    fn modern_discover_is_self_describing_and_cacheable() {
        let response = handle_request(
            &json!({"jsonrpc":"2.0","id":2,"method":"server/discover","params":{"_meta":{"io.modelcontextprotocol/protocolVersion":MODERN_PROTOCOL_VERSION,"io.modelcontextprotocol/clientInfo":{"name":"test","version":"1"},"io.modelcontextprotocol/clientCapabilities":{}}}}),
            &test_options(),
        ).expect("response");
        assert_eq!(response["result"]["resultType"], "complete");
        assert_eq!(response["result"]["cacheScope"], "public");
        assert_eq!(
            response["result"]["_meta"]["io.modelcontextprotocol/serverInfo"]["name"],
            "catalog-observation-mcp"
        );
    }

    #[test]
    fn shared_surface_tool_names_match_wire_contracts() {
        let catalog_tools = list_tools("catalog-observation");
        let catalog_names = catalog_tools
            .iter()
            .filter_map(|tool| tool.get("name").and_then(Value::as_str))
            .collect::<Vec<_>>();
        assert_eq!(
            catalog_names,
            vec![
                "catalog_observation_guidance",
                "catalog_observation_observe"
            ]
        );
        let routing_tools = list_tools("operator-routing");
        let routing_names = routing_tools
            .iter()
            .filter_map(|tool| tool.get("name").and_then(Value::as_str))
            .collect::<Vec<_>>();
        assert_eq!(
            routing_names,
            vec![
                "operator_routing_guidance",
                "operator_route_doctor",
                "operator_route_request"
            ]
        );
        let launcher_tools = list_tools("launcher");
        let launcher_names = launcher_tools
            .iter()
            .filter_map(|tool| tool.get("name").and_then(Value::as_str))
            .collect::<Vec<_>>();
        assert_eq!(
            launcher_names,
            vec![
                "launcher_guidance",
                "launcher_doctor",
                "launcher_options_list",
                "launcher_registry_list",
                "launcher_plan",
                "launcher_option_matrix",
                "launcher_coherence_check"
            ]
        );
    }

    #[test]
    fn named_native_surface_catalogs_are_present() {
        let surfaces = [
            "site-inbox", "mailbox", "graph-mail", "calendar", "site-loop",
            "site-lifecycle", "site-registry", "worker-delegation", "delegated-task",
            "sop", "scheduler", "surface-feedback", "speech", "artifacts",
            "nars-session", "quota-meter", "operator-console-overlay", "browser-control",
            "cloudflare-carrier", "site-coherence", "catalog-observation", "runtime-introspection",
            "project-state", "launcher", "operator-routing",
        ];
        for surface in surfaces {
            let tools = list_tools(surface);
            assert!(!tools.is_empty(), "missing native catalog for {surface}");
            assert!(tools.iter().all(|tool| tool.get("name").and_then(Value::as_str).is_some()), "unnamed native tool for {surface}");
        }
    }

    #[test]
    fn catalog_observation_requires_an_explicit_iso_instant() {
        let response = handle_request(
            &json!({"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"catalog_observation_observe","arguments":{"provider_id":"inference-provider:test","observed_at":"not-an-instant"}}}),
            &test_options(),
        ).expect("response");
        assert_eq!(response["error"]["data"]["code"], "invalid_request");
    }

    #[test]
    fn operator_routing_writes_a_durable_record() {
        let root = std::env::temp_dir().join(format!("narada-operator-routing-{}", Uuid::new_v4()));
        let options = Options {
            surface_id: "operator-routing".to_string(),
            site_root: root.clone(),
            log_root: Some(root.join("log")),
            registry_path: None,
            native_authority: false,
            environment: Vec::new(),
        };
        let params = json!({"name":"operator_route_request","arguments":{"transcript":"route this","target_runtime":"codex","request_id":"route-test"}});
        let result = call_tool(
            "operator-routing",
            params.as_object().expect("params"),
            &options,
        )
        .expect("route");
        assert_eq!(result["structuredContent"]["request_id"], "route-test");
        let log = root.join("log").join("operator-routing-log.jsonl");
        assert!(log.exists());
        let content = std::fs::read_to_string(log).expect("log");
        assert!(content.contains(r#""request_id":"route-test""#));
        std::fs::remove_dir_all(root).expect("cleanup");
    }
    #[test]
    fn modern_tools_list_requires_metadata_and_has_cache_metadata() {
        let response = handle_request(
            &json!({"jsonrpc":"2.0","id":3,"method":"tools/list","params":{"_meta":{"io.modelcontextprotocol/protocolVersion":MODERN_PROTOCOL_VERSION,"io.modelcontextprotocol/clientInfo":{"name":"test","version":"1"},"io.modelcontextprotocol/clientCapabilities":{}}}}),
            &test_options(),
        ).expect("response");
        assert_eq!(response["result"]["resultType"], "complete");
        assert_eq!(response["result"]["cacheScope"], "public");
        assert!(response["result"]["ttlMs"].as_u64().unwrap_or(0) > 0);
    }

}
