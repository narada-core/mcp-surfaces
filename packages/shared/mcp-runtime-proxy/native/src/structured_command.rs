use crate::filesystem::{parse_site_extra_allowed_roots, read_message, write_message};
use crate::protocol;
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use std::env;
use std::fs::{self, OpenOptions};
use std::io::{self, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    mpsc, Arc,
};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;

const PROTOCOL_VERSION: &str = "2024-11-05";
const DEFAULT_MAX_TIMEOUT_MS: u64 = 900_000;
const MAX_SYNCHRONOUS_TIMEOUT_MS: u64 = 240_000;
const DEFAULT_MAX_OUTPUT_BYTES: usize = 1024 * 1024;
const MAX_OUTPUT_BYTES: usize = 20 * 1024 * 1024;
const DEFAULT_ALLOWED_COMMANDS: &[&str] = &["railway", "wrangler"];
const DEFAULT_ALLOWED_PREFIXES: &[&[&str]] = &[
    &["pnpm", "test"],
    &["pnpm", "build"],
    &["pnpm", "typecheck"],
    &["pnpm", "--filter"],
    &["pwsh", "-file"],
    &["pwsh", "-noprofile", "-file"],
    &["pwsh", "-noprofile", "-executionpolicy", "bypass", "-file"],
];
const DEFAULT_BLOCKED_COMMANDS: &[&str] = &[
    "cmd",
    "cmd.exe",
    "powershell",
    "powershell.exe",
    "wsl",
    "wsl.exe",
    "wt",
    "wt.exe",
    "windowsterminal",
    "windowsterminal.exe",
    "openconsole",
    "openconsole.exe",
];
const TERMINAL_INTEGRATION_ENVIRONMENT: &[&str] = &[
    "WT_SESSION",
    "WT_PROFILE_ID",
    "TERM_PROGRAM",
    "TERM_PROGRAM_VERSION",
];
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;
const TRANSIENT_EXTENSIONS: &[&str] = &[".ps1", ".psm1", ".js", ".mjs", ".cjs", ".ts"];

#[derive(Clone)]
struct State {
    allowed_roots: Vec<PathBuf>,
    allowed_commands: Vec<String>,
    allowed_prefixes: Vec<Vec<String>>,
    blocked_commands: Vec<String>,
    max_timeout_ms: u64,
    max_output_bytes: usize,
    audit_log_dir: Option<PathBuf>,
    site_root: PathBuf,
    storage_root: PathBuf,
    env: std::collections::HashMap<String, String>,
}

#[derive(Debug)]
struct StructuredError {
    code: String,
    message: String,
    details: Value,
}

impl StructuredError {
    fn new(code: impl Into<String>, message: impl Into<String>, details: Value) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
            details,
        }
    }
}

pub fn run(args: &[String]) -> Result<(), String> {
    let state = parse_state(args)?;
    let (events_tx, events_rx) = mpsc::channel::<Event>();
    let reader_tx = events_tx.clone();
    thread::spawn(move || {
        let stdin = io::stdin();
        let mut reader = BufReader::new(stdin.lock());
        loop {
            match read_message(&mut reader) {
                Ok(Some((request, framed))) => {
                    if reader_tx.send(Event::Request(request, framed)).is_err() {
                        return;
                    }
                }
                Ok(None) | Err(_) => {
                    let _ = reader_tx.send(Event::InputClosed);
                    return;
                }
            }
        }
    });
    let stdout = io::stdout();
    let mut writer = stdout.lock();
    let mut active = std::collections::HashMap::<String, Arc<AtomicBool>>::new();
    let mut input_closed = false;
    while let Ok(event) = events_rx.recv() {
        match event {
            Event::Request(request, framed) => {
                let method = request
                    .get("method")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                if method == "notifications/cancelled" {
                    let request_id = request
                        .get("params")
                        .and_then(|params| params.get("requestId"))
                        .map(value_key)
                        .unwrap_or_default();
                    if let Some(token) = active.get(&request_id) {
                        token.store(true, Ordering::Release);
                    }
                    continue;
                }
                if request.get("id").is_none() {
                    continue;
                }
                if let Some(response) =
                    protocol::preflight_response(&request, "structured-command-native")
                {
                    write_message(&mut writer, &response, framed)
                        .map_err(|error| error.to_string())?;
                    writer.flush().map_err(|error| error.to_string())?;
                    continue;
                }
                if method == "tools/call" {
                    let id = request.get("id").cloned().unwrap_or(Value::Null);
                    let key = value_key(&id);
                    let token = Arc::new(AtomicBool::new(false));
                    active.insert(key.clone(), token.clone());
                    let state_clone = state.clone();
                    let response_tx = events_tx.clone();
                    thread::spawn(move || {
                        let response = handle_request(&state_clone, &request, Some(token)).unwrap_or_else(|| json!({"jsonrpc": "2.0", "id": request.get("id").cloned().unwrap_or(Value::Null), "result": {}}));
                        let response = protocol::modernize_response(
                            &request,
                            response,
                            "structured-command-native",
                        );
                        let _ = response_tx.send(Event::Response(response, framed, key));
                    });
                } else if let Some(response) =
                    handle_request(&state, &request, None).map(|response| {
                        protocol::modernize_response(
                            &request,
                            response,
                            "structured-command-native",
                        )
                    })
                {
                    write_message(&mut writer, &response, framed)
                        .map_err(|error| error.to_string())?;
                    writer.flush().map_err(|error| error.to_string())?;
                }
            }
            Event::Response(response, framed, key) => {
                active.remove(&key);
                write_message(&mut writer, &response, framed).map_err(|error| error.to_string())?;
                writer.flush().map_err(|error| error.to_string())?;
            }
            Event::InputClosed => {
                input_closed = true;
                if active.is_empty() {
                    break;
                }
            }
        }
        if input_closed && active.is_empty() {
            break;
        }
    }
    Ok(())
}

pub fn run_background(args: &[String]) -> Result<(), String> {
    let request_path = argument_value(args, "--request")
        .ok_or_else(|| "structured_command_background_request_required".to_string())?;
    let expected_sha256 = argument_value(args, "--sha256")
        .ok_or_else(|| "structured_command_background_sha256_required".to_string())?;
    let bytes = fs::read(&request_path)
        .map_err(|error| format!("structured_command_background_request_read_failed:{error}"))?;
    if hex::encode(Sha256::digest(&bytes)) != expected_sha256 {
        return Err("structured_command_background_request_integrity_mismatch".to_string());
    }
    let request: Value = serde_json::from_slice(&bytes)
        .map_err(|error| format!("structured_command_background_request_invalid:{error}"))?;
    if request.get("schema").and_then(Value::as_str)
        != Some("narada.structured_command.background_request.v1")
    {
        return Err("structured_command_background_request_schema_invalid".to_string());
    }
    let command = request
        .get("command")
        .and_then(Value::as_str)
        .ok_or_else(|| "structured_command_background_command_required".to_string())?;
    let command_args = value_strings(request.get("args"));
    let cwd = PathBuf::from(
        request
            .get("working_directory")
            .and_then(Value::as_str)
            .ok_or_else(|| "structured_command_background_cwd_required".to_string())?,
    );
    let timeout_ms = request
        .get("timeout_ms")
        .and_then(Value::as_u64)
        .ok_or_else(|| "structured_command_background_timeout_required".to_string())?;
    let max_output_bytes = request
        .get("max_output_bytes")
        .and_then(Value::as_u64)
        .unwrap_or(DEFAULT_MAX_OUTPUT_BYTES as u64)
        .clamp(1, MAX_OUTPUT_BYTES as u64) as usize;
    let storage_root = PathBuf::from(
        request
            .get("storage_root")
            .and_then(Value::as_str)
            .ok_or_else(|| "structured_command_background_storage_root_required".to_string())?,
    );
    let execution_ref = request
        .get("execution_ref")
        .and_then(Value::as_str)
        .ok_or_else(|| "structured_command_background_execution_ref_required".to_string())?;
    let state = State {
        allowed_roots: vec![cwd.clone()],
        allowed_commands: vec![],
        allowed_prefixes: vec![],
        blocked_commands: vec![],
        max_timeout_ms: timeout_ms,
        max_output_bytes,
        audit_log_dir: request
            .get("audit_log_dir")
            .and_then(Value::as_str)
            .map(PathBuf::from),
        site_root: cwd.clone(),
        storage_root,
        env: execution_environment(),
    };
    let result = run_process(
        command,
        &command_args,
        &cwd,
        timeout_ms,
        max_output_bytes,
        None,
        &state.env,
    );
    let payload = execution_payload(
        command,
        &command_args,
        &cwd,
        request
            .get("started_at")
            .and_then(Value::as_str)
            .unwrap_or_default(),
        timeout_ms,
        request
            .get("execution_posture")
            .cloned()
            .unwrap_or_else(|| json!({})),
        result,
        "background",
        false,
        request.get("input_ref").cloned().unwrap_or(Value::Null),
    );
    audit(&state, &payload);
    update_execution_record(&state, execution_ref, &payload).map_err(|error| error.message)?;
    let _ = fs::remove_file(request_path);
    Ok(())
}

fn argument_value(args: &[String], name: &str) -> Option<String> {
    args.windows(2)
        .find(|pair| pair[0] == name)
        .map(|pair| pair[1].clone())
}

enum Event {
    Request(Value, bool),
    Response(Value, bool, String),
    InputClosed,
}

fn value_key(value: &Value) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| "null".to_string())
}

fn parse_state(args: &[String]) -> Result<State, String> {
    let mut roots = Vec::new();
    let mut allowed_commands = DEFAULT_ALLOWED_COMMANDS
        .iter()
        .map(|value| (*value).to_string())
        .collect::<Vec<_>>();
    let mut allowed_prefixes = DEFAULT_ALLOWED_PREFIXES
        .iter()
        .map(|prefix| prefix.iter().map(|part| (*part).to_string()).collect())
        .collect::<Vec<Vec<String>>>();
    let mut blocked_commands = DEFAULT_BLOCKED_COMMANDS
        .iter()
        .map(|value| (*value).to_string())
        .collect::<Vec<_>>();
    let mut max_timeout_ms = DEFAULT_MAX_TIMEOUT_MS;
    let mut max_output_bytes = DEFAULT_MAX_OUTPUT_BYTES;
    let mut audit_log_dir = None;
    let mut site_root = None;
    let mut storage_root = None;
    let mut index = 0;
    while index < args.len() {
        let flag = args[index].as_str();
        let needs_value = |index: &mut usize, name: &str| -> Result<String, String> {
            *index += 1;
            args.get(*index)
                .cloned()
                .ok_or_else(|| format!("structured_command_{name}_required"))
        };
        match flag {
            "--allowed-root" => roots.push(needs_value(&mut index, "allowed_root")?),
            "--allow-command" => allowed_commands
                .push(needs_value(&mut index, "allow_command")?.to_ascii_lowercase()),
            "--allow-prefix" => {
                let value = needs_value(&mut index, "allow_prefix")?;
                let prefix = value
                    .split_whitespace()
                    .map(|part| part.to_ascii_lowercase())
                    .collect::<Vec<_>>();
                if prefix.is_empty() {
                    return Err("structured_command_allow_prefix_must_not_be_empty".to_string());
                }
                allowed_prefixes.push(prefix);
            }
            "--blocked-command" => blocked_commands
                .push(needs_value(&mut index, "blocked_command")?.to_ascii_lowercase()),
            "--max-timeout-ms" => {
                max_timeout_ms = parse_bounded_u64(
                    &needs_value(&mut index, "max_timeout_ms")?,
                    1,
                    3_600_000,
                    DEFAULT_MAX_TIMEOUT_MS,
                    "max_timeout_ms",
                )?
            }
            "--max-output-bytes" => {
                max_output_bytes = parse_bounded_usize(
                    &needs_value(&mut index, "max_output_bytes")?,
                    1,
                    MAX_OUTPUT_BYTES,
                    DEFAULT_MAX_OUTPUT_BYTES,
                    "max_output_bytes",
                )?
            }
            "--audit-log-dir" => audit_log_dir = Some(needs_value(&mut index, "audit_log_dir")?),
            "--site-root" => site_root = Some(needs_value(&mut index, "site_root")?),
            "--storage-root" => storage_root = Some(needs_value(&mut index, "storage_root")?),
            "--help" => return Err("structured_command_help".to_string()),
            other => return Err(format!("structured_command_unknown_argument:{other}")),
        }
        index += 1;
    }
    if roots.is_empty() {
        return Err("structured_command_mcp_requires_at_least_one_allowed_root".to_string());
    }
    let mut allowed_roots = roots
        .into_iter()
        .map(|root| absolute(PathBuf::from(root)))
        .collect::<Vec<_>>();
    let site_root =
        absolute(PathBuf::from(site_root.unwrap_or_else(|| {
            allowed_roots[0].to_string_lossy().to_string()
        })));
    for root in parse_site_extra_allowed_roots(&site_root) {
        let root = absolute(PathBuf::from(root));
        if !allowed_roots.iter().any(|candidate| candidate == &root) {
            allowed_roots.push(root);
        }
    }
    let storage_root =
        absolute(PathBuf::from(storage_root.unwrap_or_else(|| {
            allowed_roots[0].to_string_lossy().to_string()
        })));
    Ok(State {
        allowed_roots,
        allowed_commands: dedupe(allowed_commands),
        allowed_prefixes,
        blocked_commands: dedupe(blocked_commands),
        max_timeout_ms,
        max_output_bytes,
        audit_log_dir: audit_log_dir.map(|path| absolute(PathBuf::from(path))),
        site_root,
        storage_root,
        env: execution_environment(),
    })
}

fn execution_environment() -> std::collections::HashMap<String, String> {
    let mut environment = env::vars().collect::<std::collections::HashMap<_, _>>();
    #[cfg(windows)]
    augment_windows_msvc_environment(&mut environment);
    environment
}

#[cfg(windows)]
fn augment_windows_msvc_environment(environment: &mut std::collections::HashMap<String, String>) {
    let program_files_x86 = environment_value(environment, "ProgramFiles(x86)")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(r"C:\Program Files (x86)"));
    let visual_studio_root = program_files_x86
        .join("Microsoft Visual Studio")
        .join("2022")
        .join("BuildTools");
    let msvc_versions = visual_studio_root.join("VC").join("Tools").join("MSVC");
    let Some(msvc_root) = latest_child_directory(&msvc_versions, |candidate| {
        candidate
            .join("bin")
            .join("Hostx64")
            .join("x64")
            .join("link.exe")
            .is_file()
            && candidate.join("lib").join("x64").is_dir()
            && candidate.join("include").is_dir()
    }) else {
        return;
    };

    let sdk_root = program_files_x86.join("Windows Kits").join("10");
    let sdk_include_root = sdk_root.join("Include");
    let Some(sdk_version_root) = latest_child_directory(&sdk_include_root, |candidate| {
        let version = candidate.file_name().unwrap_or_default();
        let lib = sdk_root.join("Lib").join(version);
        candidate.join("ucrt").is_dir()
            && candidate.join("shared").is_dir()
            && candidate.join("um").is_dir()
            && lib.join("ucrt").join("x64").is_dir()
            && lib.join("um").join("x64").join("kernel32.lib").is_file()
    }) else {
        return;
    };
    let sdk_version = sdk_version_root.file_name().unwrap_or_default();
    let sdk_lib_root = sdk_root.join("Lib").join(sdk_version);
    let sdk_bin = sdk_root.join("bin").join(sdk_version).join("x64");

    prepend_environment_paths(
        environment,
        "PATH",
        &[msvc_root.join("bin").join("Hostx64").join("x64"), sdk_bin],
    );
    prepend_environment_paths(
        environment,
        "LIB",
        &[
            msvc_root.join("lib").join("x64"),
            sdk_lib_root.join("ucrt").join("x64"),
            sdk_lib_root.join("um").join("x64"),
        ],
    );
    prepend_environment_paths(
        environment,
        "INCLUDE",
        &[
            msvc_root.join("include"),
            sdk_version_root.join("ucrt"),
            sdk_version_root.join("shared"),
            sdk_version_root.join("um"),
            sdk_version_root.join("winrt"),
            sdk_version_root.join("cppwinrt"),
        ],
    );
    set_environment_value(
        environment,
        "VCINSTALLDIR",
        visual_studio_root.join("VC").to_string_lossy().to_string(),
    );
    set_environment_value(
        environment,
        "VCToolsInstallDir",
        msvc_root.to_string_lossy().to_string(),
    );
    set_environment_value(
        environment,
        "WindowsSdkDir",
        sdk_root.to_string_lossy().to_string(),
    );
    set_environment_value(
        environment,
        "WindowsSDKVersion",
        format!("{}\\", sdk_version.to_string_lossy()),
    );
}

#[cfg(windows)]
fn latest_child_directory(root: &Path, admitted: impl Fn(&Path) -> bool) -> Option<PathBuf> {
    let mut candidates = fs::read_dir(root)
        .ok()?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.is_dir() && admitted(path))
        .collect::<Vec<_>>();
    candidates.sort_by(|left, right| {
        left.file_name()
            .unwrap_or_default()
            .cmp(right.file_name().unwrap_or_default())
    });
    candidates.pop()
}

#[cfg(windows)]
fn environment_value<'a>(
    environment: &'a std::collections::HashMap<String, String>,
    name: &str,
) -> Option<&'a str> {
    environment
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case(name))
        .map(|(_, value)| value.as_str())
}

#[cfg(windows)]
fn set_environment_value(
    environment: &mut std::collections::HashMap<String, String>,
    name: &str,
    value: String,
) {
    if let Some(key) = environment
        .keys()
        .find(|key| key.eq_ignore_ascii_case(name))
        .cloned()
    {
        environment.insert(key, value);
    } else {
        environment.insert(name.to_string(), value);
    }
}

#[cfg(windows)]
fn prepend_environment_paths(
    environment: &mut std::collections::HashMap<String, String>,
    name: &str,
    required: &[PathBuf],
) {
    let mut paths = required
        .iter()
        .filter(|path| path.is_dir())
        .cloned()
        .collect::<Vec<_>>();
    if let Some(existing) = environment_value(environment, name) {
        paths.extend(env::split_paths(existing));
    }
    let mut deduplicated = Vec::<PathBuf>::new();
    for path in paths {
        if !deduplicated.iter().any(|candidate| {
            candidate
                .to_string_lossy()
                .eq_ignore_ascii_case(&path.to_string_lossy())
        }) {
            deduplicated.push(path);
        }
    }
    if let Ok(value) = env::join_paths(deduplicated) {
        set_environment_value(environment, name, value.to_string_lossy().to_string());
    }
}

fn parse_bounded_u64(
    value: &str,
    min: u64,
    max: u64,
    fallback: u64,
    name: &str,
) -> Result<u64, String> {
    value
        .parse::<u64>()
        .map(|parsed| parsed.clamp(min, max))
        .map_err(|_| format!("structured_command_invalid_{name}:{value}"))
        .or(Ok(fallback))
}

fn parse_bounded_usize(
    value: &str,
    min: usize,
    max: usize,
    fallback: usize,
    name: &str,
) -> Result<usize, String> {
    value
        .parse::<usize>()
        .map(|parsed| parsed.clamp(min, max))
        .map_err(|_| format!("structured_command_invalid_{name}:{value}"))
        .or(Ok(fallback))
}

fn dedupe(values: Vec<String>) -> Vec<String> {
    let mut result = Vec::new();
    for value in values {
        if !result.iter().any(|existing| existing == &value) {
            result.push(value);
        }
    }
    result
}

fn handle_request(
    state: &State,
    request: &Value,
    cancellation: Option<Arc<AtomicBool>>,
) -> Option<Value> {
    let method = request
        .get("method")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let id = request.get("id").cloned()?;
    let params = request.get("params").unwrap_or(&Value::Null);
    let result = match method {
        "initialize" => Ok(initialize(request)),
        "tools/list" => Ok(json!({"tools": list_tools()})),
        "tools/call" => call_tool(state, params, cancellation),
        "resources/list" => Ok(json!({"resources": []})),
        "resources/read" => Err(StructuredError::new(
            "resource_not_found",
            "resource_not_found",
            json!({}),
        )),
        "prompts/list" => Ok(
            json!({"prompts": [{"name": "structured_command_safe_execution", "title": "Structured Command Safe Execution", "description": "Guidance for argv-only command execution.", "arguments": []}]}),
        ),
        "prompts/get" => prompt_get(params),
        "completion/complete" => {
            Ok(json!({"completion": {"values": [], "total": 0, "hasMore": false}}))
        }
        "logging/setLevel" => Ok(json!({})),
        _ => Err(StructuredError::new(
            "unsupported_mcp_method",
            format!("unsupported_mcp_method:{method}"),
            json!({"method": method}),
        )),
    };
    Some(match result {
        Ok(value) => json!({"jsonrpc": "2.0", "id": id, "result": value}),
        Err(error) => {
            json!({"jsonrpc": "2.0", "id": id, "error": {"code": -32000, "message": error.message, "data": error_diagnostic(&error)}})
        }
    })
}

fn initialize(request: &Value) -> Value {
    json!({
        "protocolVersion": request.get("params").and_then(|params| params.get("protocolVersion")).cloned().unwrap_or(json!(PROTOCOL_VERSION)),
        "capabilities": {"tools": {}, "resources": {}, "prompts": {}, "completions": {}, "logging": {}},
        "serverInfo": {"name": "structured-command-native", "version": "0.1.0"}
    })
}

fn list_tools() -> Vec<Value> {
    vec![
        tool("structured_command_guidance", "Guidance for argv-only structured command execution.", json!({"type": "object", "properties": {"workflow": {"type": "string"}, "tool": {"type": "string"}}, "additionalProperties": false}), true),
        tool("structured_command_execution_policy_inspect", "Inspect the policy governing structured command execution.", json!({"type": "object", "additionalProperties": false}), true),
        tool("structured_command_output_show", "Read a materialized structured-command output ref with offset/limit paging.", json!({"type": "object", "properties": {"ref": {"type": "string"}, "output_ref": {"type": "string"}, "offset": {"type": "integer", "minimum": 0}, "limit": {"type": "integer", "minimum": 1, "maximum": 20000}}, "oneOf": [{"required":["ref"]},{"required":["output_ref"]}], "additionalProperties": false}), true),
        tool("structured_command_execute", "Execute a structured argv command under allowed-root and command policy, or read an existing execution_ref. Supply exactly one of command, input_ref, or execution_ref.", execution_schema(true), false),
        tool("structured_command_start", "Start a durable detached native command and return an execution_ref immediately. Supply command or input_ref.", execution_schema(false), false),
        tool("structured_command_execution_show", "Read one durable structured command execution by execution_ref.", json!({"type": "object", "properties": {"execution_ref": {"type": "string", "minLength": 1}, "stdout_offset": {"type": "integer", "minimum": 0}, "stderr_offset": {"type": "integer", "minimum": 0}, "stdout_limit": {"type": "integer", "minimum": 1, "maximum": 20000}, "stderr_limit": {"type": "integer", "minimum": 1, "maximum": 20000}}, "required": ["execution_ref"], "additionalProperties": false}), true),
        tool("structured_command_powershell_parse_check", "Parse-check an allowed-root PowerShell script without admitting arbitrary execution.", json!({"type": "object", "properties": {"path": {"type": "string"}, "working_directory": {"type": "string"}, "timeout_ms": {"type": "integer"}}, "required": ["path"], "additionalProperties": false}), true),
        tool("structured_command_input_create", "Create a scoped structured command input ref.", json!({"type": "object", "properties": {"input_id": {"type": "string"}, "command": {"type": "string", "minLength": 1}, "args": {"type": "array", "items": {"type": "string"}}, "working_directory": {"type": "string"}, "timeout_ms": {"type": "integer", "minimum": 1}, "wait_for_completion": {"type": "boolean"}, "test_scope": {"type": "string"}, "expected_cost": {"type": "string"}}, "required": ["command"], "additionalProperties": false}), false),
        tool("structured_command_elevated_window_execute", "On Windows, launch a policy-approved command in a visible elevated UAC window. Execution requires confirm_elevation=true.", json!({"type": "object", "properties": {"command": {"type": "string", "minLength": 1}, "args": {"type": "array", "items": {"type": "string"}}, "working_directory": {"type": "string", "minLength": 1}, "confirm_elevation": {"type": "boolean"}, "wait": {"type": "boolean"}, "dry_run": {"type": "boolean"}, "timeout_ms":{"type":"integer","minimum":1}}, "required": ["command", "working_directory"], "additionalProperties": false}), false),
    ]
}

fn execution_schema(allow_execution_ref: bool) -> Value {
    let mut properties = json!({"input_ref": {"type": "string", "minLength": 1}, "command": {"type": "string", "minLength": 1}, "args": {"type": "array", "items": {"type": "string"}}, "working_directory": {"type": "string"}, "timeout_ms": {"type": "integer", "minimum": 1}, "wait_for_completion": {"type": "boolean"}, "test_scope": {"type": "string"}, "expected_cost": {"type": "string"}, "stdout_offset": {"type": "integer", "minimum": 0}, "stderr_offset": {"type": "integer", "minimum": 0}, "stdout_limit": {"type": "integer", "minimum": 1, "maximum": 20000}, "stderr_limit": {"type": "integer", "minimum": 1, "maximum": 20000}}).as_object().unwrap().clone();
    if allow_execution_ref {
        properties.insert(
            "execution_ref".to_string(),
            json!({"type": "string", "minLength": 1}),
        );
    }
    let mut alternatives = vec![
        json!({"required": ["command"]}),
        json!({"required": ["input_ref"]}),
    ];
    if allow_execution_ref {
        alternatives.push(json!({"required": ["execution_ref"]}));
    }
    json!({"type": "object", "properties": properties, "oneOf": alternatives, "additionalProperties": false})
}

fn tool(name: &str, description: &str, schema: Value, read_only: bool) -> Value {
    json!({"name": name, "description": description, "inputSchema": schema, "annotations": {"title": name, "canonicalName": name, "readOnlyHint": read_only, "destructiveHint": !read_only, "idempotentHint": read_only, "openWorldHint": false}, "outputSchema": {"type": "object", "additionalProperties": true}})
}

fn prompt_get(params: &Value) -> Result<Value, StructuredError> {
    let name = params
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if name != "structured_command_safe_execution" {
        return Err(StructuredError::new(
            "unknown_prompt",
            format!("unknown_prompt:{name}"),
            json!({"name": name}),
        ));
    }
    Ok(
        json!({"description": "Guidance for argv-only command execution.", "messages": [{"role": "user", "content": {"type": "text", "text": "Use structured_command_execute with explicit argv arrays only. Inspect policy before relying on command availability."}}]}),
    )
}

fn call_tool(
    state: &State,
    params: &Value,
    cancellation: Option<Arc<AtomicBool>>,
) -> Result<Value, StructuredError> {
    let name = params.get("name").and_then(Value::as_str).ok_or_else(|| {
        StructuredError::new(
            "tools_call_requires_name",
            "tools_call_requires_name",
            json!({}),
        )
    })?;
    let args = params.get("arguments").unwrap_or(&Value::Null);
    let payload = match name {
        "structured_command_guidance" => guidance(args),
        "structured_command_execution_policy_inspect" => Ok(policy_payload(state)),
        "structured_command_execute" => execute(state, args, cancellation, false),
        "structured_command_execution_show" => show_execution(state, args),
        "structured_command_input_create" => create_input_record(state, args),
        "structured_command_output_show" => output_show(state, args),
        "structured_command_powershell_parse_check" => {
            powershell_parse_check(state, args, cancellation)
        }
        "structured_command_start" => execute(state, args, cancellation, true),
        "structured_command_elevated_window_execute" => elevated_execute(state, args, cancellation),
        _ => Err(StructuredError::new(
            "structured_command_unknown_tool",
            format!("structured_command_unknown_tool:{name}"),
            json!({"tool_name": name}),
        )),
    }?;
    tool_result(state, payload, name)
}

fn guidance(args: &Value) -> Result<Value, StructuredError> {
    Ok(
        json!({"schema": "narada.mcp_surface.guidance.v0", "status": "ok", "surface_id": "structured-command", "guidance_tool": "structured_command_guidance", "purpose": "Bounded argv-only process execution under explicit command and root policy.", "requested": {"workflow": args.get("workflow"), "tool": args.get("tool")}, "safety": ["Inspect policy before execution.", "Pass command arguments as an array; no shell interpolation is performed.", "Retain structuredContent as the authoritative execution record.", "Use structured_command_start for long work; it returns a durable execution_ref immediately. Poll structured_command_execution_show for terminal status."]}),
    )
}

fn powershell_parse_check(
    state: &State,
    args: &Value,
    cancellation: Option<Arc<AtomicBool>>,
) -> Result<Value, StructuredError> {
    let object = args.as_object().cloned().unwrap_or_default();
    let path_value = object
        .get("path")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let path = resolve_path(path_value, &state.allowed_roots[0]);
    if !path
        .to_string_lossy()
        .to_ascii_lowercase()
        .ends_with(".ps1")
    {
        return Err(StructuredError::new(
            "structured_command_powershell_parse_check_requires_ps1",
            "structured_command_powershell_parse_check_requires_ps1",
            json!({"path": path_value}),
        ));
    }
    if !inside_any_root(&path, &state.allowed_roots) {
        return Err(StructuredError::new(
            "structured_command_powershell_parse_check_path_outside_allowed_roots",
            "structured_command_powershell_parse_check_path_outside_allowed_roots",
            json!({"path": path.to_string_lossy()}),
        ));
    }
    if !path.is_file() {
        return Err(StructuredError::new(
            "structured_command_powershell_parse_check_file_not_found",
            "structured_command_powershell_parse_check_file_not_found",
            json!({"path": path.to_string_lossy()}),
        ));
    }
    let cwd = object
        .get("working_directory")
        .and_then(Value::as_str)
        .map(|value| resolve_path(value, &state.allowed_roots[0]))
        .unwrap_or_else(|| {
            path.parent()
                .unwrap_or(&state.allowed_roots[0])
                .to_path_buf()
        });
    if !inside_any_root(&cwd, &state.allowed_roots) {
        return Err(StructuredError::new(
            "structured_command_powershell_parse_check_cwd_outside_allowed_roots",
            "structured_command_powershell_parse_check_cwd_outside_allowed_roots",
            json!({"working_directory": cwd.to_string_lossy()}),
        ));
    }
    let timeout = object
        .get("timeout_ms")
        .and_then(Value::as_u64)
        .unwrap_or(30_000)
        .clamp(1, state.max_timeout_ms);
    let script = format!("$ErrorActionPreference = 'Stop'; $tokens = $null; $errors = $null; [System.Management.Automation.Language.Parser]::ParseFile('{}', [ref]$tokens, [ref]$errors) > $null; if ($errors.Count -gt 0) {{ $errors | ForEach-Object {{ Write-Error ($_.ToString()) }}; exit 1 }}; Write-Output 'parse_ok'", ps_single_quote(&path.to_string_lossy()));
    let result = run_process(
        "pwsh",
        &["-NoProfile".to_string(), "-Command".to_string(), script],
        &cwd,
        timeout,
        state.max_output_bytes,
        cancellation,
        &state.env,
    );
    Ok(
        json!({"schema": "narada.structured_command.powershell_parse_check.v0", "status": if result.cancelled { "cancelled" } else if result.timed_out { "timed_out" } else if result.exit_code == Some(0) { "ok" } else { "failed" }, "path": path.to_string_lossy(), "working_directory": cwd.to_string_lossy(), "timeout_ms": timeout, "exit_code": result.exit_code, "stdout": result.stdout, "stderr": result.stderr, "stdout_truncated": result.stdout_truncated, "stderr_truncated": result.stderr_truncated, "timed_out": result.timed_out, "cancelled": result.cancelled, "arbitrary_command_execution_admitted": false, "parser_api": "System.Management.Automation.Language.Parser.ParseFile"}),
    )
}

fn elevated_execute(
    state: &State,
    args: &Value,
    cancellation: Option<Arc<AtomicBool>>,
) -> Result<Value, StructuredError> {
    let object = args.as_object().cloned().unwrap_or_default();
    let command = object
        .get("command")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let command_args = value_strings(object.get("args"));
    let cwd = resolve_path(
        object
            .get("working_directory")
            .and_then(Value::as_str)
            .unwrap_or_default(),
        &state.allowed_roots[0],
    );
    let decision = decide(state, command, &command_args, &cwd);
    if decision.get("status").and_then(Value::as_str) != Some("allowed") {
        return Ok(
            json!({"schema":"narada.structured_command.elevated_window_result.v0","status":"refused","executed":false,"command":command,"args":command_args,"working_directory":cwd.to_string_lossy(),"decision":decision,"refusal_reasons":decision.get("reasons").cloned().unwrap_or_else(||json!([]))}),
        );
    }
    let wait = object.get("wait").and_then(Value::as_bool).unwrap_or(false);
    let script = format!("$ErrorActionPreference = 'Stop'; $p = Start-Process -FilePath {} -ArgumentList {} -WorkingDirectory {} -Verb RunAs -WindowStyle Normal -PassThru; {}", ps_single_quote(command), ps_array_literal(&command_args), ps_single_quote(&cwd.to_string_lossy()), if wait { "if ($p) { $p.WaitForExit(); exit $p.ExitCode }" } else { "if ($p) { Write-Output (\"started_pid=\" + $p.Id) }" });
    let broker = json!({"command": "powershell.exe", "args": ["-NoProfile", "-ExecutionPolicy", "Bypass", "-Command", script], "script": script});
    let dry_run = object
        .get("dry_run")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if dry_run {
        return Ok(
            json!({"schema": "narada.structured_command.elevated_window_result.v0", "status": "planned", "executed": false, "command": command, "args": command_args, "working_directory": cwd.to_string_lossy(), "wait": wait, "decision":decision,"broker": broker}),
        );
    }
    if object.get("confirm_elevation").and_then(Value::as_bool) != Some(true) {
        return Ok(
            json!({"schema":"narada.structured_command.elevated_window_result.v0","status":"refused","executed":false,"command":command,"args":command_args,"working_directory":cwd.to_string_lossy(),"wait":wait,"decision":decision,"refusal_reasons":["confirm_elevation_required"],"remediation_hints":["Retry with confirm_elevation=true only when a visible Windows UAC prompt is intended."],"broker":broker}),
        );
    }
    let timeout_ms = object
        .get("timeout_ms")
        .and_then(Value::as_u64)
        .unwrap_or(if wait { state.max_timeout_ms } else { 60_000 })
        .clamp(1, state.max_timeout_ms);
    let result = run_process(
        "powershell.exe",
        &[
            "-NoProfile".into(),
            "-NonInteractive".into(),
            "-ExecutionPolicy".into(),
            "Bypass".into(),
            "-Command".into(),
            script,
        ],
        &cwd,
        timeout_ms,
        state.max_output_bytes,
        cancellation,
        &state.env,
    );
    Ok(
        json!({"schema":"narada.structured_command.elevated_window_result.v0","status":if result.cancelled{"cancelled"}else if result.timed_out{"timed_out"}else if result.exit_code==Some(0){"started"}else{"failed"},"executed":true,"command":command,"args":command_args,"working_directory":cwd.to_string_lossy(),"wait":wait,"decision":decision,"timeout_ms":timeout_ms,"exit_code":result.exit_code,"stdout":result.stdout,"stderr":result.stderr,"stdout_truncated":result.stdout_truncated,"stderr_truncated":result.stderr_truncated,"timed_out":result.timed_out,"cancelled":result.cancelled,"broker":broker}),
    )
}

fn ps_single_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}

fn ps_array_literal(args: &[String]) -> String {
    if args.is_empty() {
        "@()".to_string()
    } else {
        format!(
            "@({})",
            args.iter()
                .map(|value| ps_single_quote(value))
                .collect::<Vec<_>>()
                .join(", ")
        )
    }
}

fn policy_payload(state: &State) -> Value {
    json!({
        "schema": "narada.structured_command.execution_policy.v0",
        "allowed_roots": state.allowed_roots.iter().map(|path| path.to_string_lossy().to_string()).collect::<Vec<_>>(),
        "allowed_commands": sorted_strings(&state.allowed_commands),
        "default_allowed_commands": DEFAULT_ALLOWED_COMMANDS,
        "allowed_prefixes": state.allowed_prefixes.iter().map(|prefix| prefix.join(" ")).collect::<Vec<_>>(),
        "default_allowed_prefixes": DEFAULT_ALLOWED_PREFIXES.iter().map(|prefix| prefix.join(" ")).collect::<Vec<_>>(),
        "blocked_commands": sorted_strings(&state.blocked_commands),
        "max_timeout_ms": state.max_timeout_ms,
        "max_output_bytes": state.max_output_bytes,
        "shell_interpolation": false,
    })
}

fn sorted_strings(values: &[String]) -> Vec<String> {
    let mut result = values.to_vec();
    result.sort();
    result
}

fn parse_ref(value: &str, kind: &str) -> Result<String, StructuredError> {
    let prefix = format!("structured_command_{kind}:");
    let Some(id) = value.strip_prefix(&prefix) else {
        return Err(StructuredError::new(
            format!("structured_command_invalid_{kind}_ref"),
            format!("structured_command_invalid_{kind}_ref"),
            json!({"ref": value, "expected_kind": kind}),
        ));
    };
    if id.len() < 8
        || !id.chars().all(|character| {
            character.is_ascii_alphanumeric() || character == '_' || character == '-'
        })
    {
        return Err(StructuredError::new(
            format!("structured_command_invalid_{kind}_ref"),
            format!("structured_command_invalid_{kind}_ref"),
            json!({"ref": value, "expected_kind": kind}),
        ));
    }
    Ok(id.to_string())
}

fn value_strings(value: Option<&Value>) -> Vec<String> {
    value
        .and_then(Value::as_array)
        .map(|values| {
            values
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

fn execution_path(state: &State, id: &str) -> PathBuf {
    state
        .storage_root
        .join("executions")
        .join(format!("{id}.json"))
}

fn input_path(state: &State, id: &str) -> PathBuf {
    state.storage_root.join("inputs").join(format!("{id}.json"))
}

fn write_json_record(path: &Path, value: &Value) -> Result<(), StructuredError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|error| {
            StructuredError::new(
                "structured_command_persistence_failed",
                error.to_string(),
                json!({"path": parent.to_string_lossy()}),
            )
        })?;
    }
    let serialized = format!(
        "{}\n",
        serde_json::to_string_pretty(value).map_err(|error| StructuredError::new(
            "structured_command_persistence_failed",
            error.to_string(),
            json!({"path": path.to_string_lossy()})
        ))?
    );
    fs::write(path, serialized).map_err(|error| {
        StructuredError::new(
            "structured_command_persistence_failed",
            error.to_string(),
            json!({"path": path.to_string_lossy()}),
        )
    })
}

fn read_json_record(path: &Path) -> Result<Value, StructuredError> {
    let bytes = fs::read(path).map_err(|_| {
        StructuredError::new(
            "structured_command_ref_not_found",
            "structured_command_ref_not_found",
            json!({"path": path.to_string_lossy()}),
        )
    })?;
    serde_json::from_slice(&bytes).map_err(|error| {
        StructuredError::new(
            "structured_command_ref_invalid_json",
            error.to_string(),
            json!({"path": path.to_string_lossy()}),
        )
    })
}

fn create_execution_record(state: &State, payload: &Value) -> Result<String, StructuredError> {
    let id = unique_id("e");
    let reference = format!("structured_command_execution:{id}");
    let record = json!({"schema": "narada.structured_command.execution.v0", "ref": reference, "created_at": now_rfc3339(), "sha256": sha256_json(payload), "result": payload});
    write_json_record(&execution_path(state, &id), &record)?;
    Ok(reference)
}

fn update_execution_record(
    state: &State,
    reference: &str,
    payload: &Value,
) -> Result<(), StructuredError> {
    let id = parse_ref(reference, "execution")?;
    let path = execution_path(state, &id);
    let existing = read_json_record(&path)?;
    let created_at = existing
        .get("created_at")
        .cloned()
        .unwrap_or_else(|| json!(now_rfc3339()));
    let record = json!({"schema": "narada.structured_command.execution.v0", "ref": reference, "created_at": created_at, "updated_at": now_rfc3339(), "sha256": sha256_json(payload), "result": payload});
    write_json_record(&path, &record)
}

fn read_execution_record(state: &State, reference: &str) -> Result<Value, StructuredError> {
    let id = parse_ref(reference, "execution")?;
    let record = read_json_record(&execution_path(state, &id))?;
    record.get("result").cloned().ok_or_else(|| {
        StructuredError::new(
            "structured_command_ref_invalid_json",
            "structured_command_execution_result_missing",
            json!({"ref": reference}),
        )
    })
}

fn create_input_record(state: &State, args: &Value) -> Result<Value, StructuredError> {
    let object = args.as_object().cloned().unwrap_or_default();
    let command = object
        .get("command")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| {
            StructuredError::new(
                "structured_command_command_required",
                "structured_command_command_required",
                json!({}),
            )
        })?;
    let id = object
        .get("input_id")
        .and_then(Value::as_str)
        .map(str::to_string)
        .unwrap_or_else(|| unique_id("i"));
    if id.len() < 8
        || !id.chars().all(|character| {
            character.is_ascii_alphanumeric() || character == '_' || character == '-'
        })
    {
        return Err(StructuredError::new(
            "structured_command_invalid_ref_id",
            "structured_command_invalid_ref_id",
            json!({"input_id": id}),
        ));
    }
    let input = json!({
        "command": command,
        "args": object.get("args").and_then(Value::as_array).map(|values| values.iter().map(|value| value.as_str().unwrap_or_default()).collect::<Vec<_>>()).unwrap_or_default(),
        "working_directory": object.get("working_directory"),
        "timeout_ms": object.get("timeout_ms"),
        "wait_for_completion": object.get("wait_for_completion"),
        "test_scope": object.get("test_scope").and_then(Value::as_str).unwrap_or("unknown"),
        "expected_cost": object.get("expected_cost").and_then(Value::as_str).unwrap_or("unknown"),
    });
    let reference = format!("structured_command_input:{id}");
    let record = json!({"schema": "narada.structured_command.input.v0", "ref": reference, "created_at": now_rfc3339(), "sha256": sha256_json(&input), "input": input});
    write_json_record(&input_path(state, &id), &record)?;
    Ok(
        json!({"schema": "narada.structured_command.input_create_result.v0", "status": "created", "input_ref": reference, "sha256": sha256_json(record.get("input").unwrap_or(&Value::Null))}),
    )
}

fn read_input_record(state: &State, reference: &str) -> Result<Value, StructuredError> {
    let id = parse_ref(reference, "input")?;
    let record = read_json_record(&input_path(state, &id))?;
    record.get("input").cloned().ok_or_else(|| {
        StructuredError::new(
            "structured_command_ref_invalid_json",
            "structured_command_input_missing",
            json!({"ref": reference}),
        )
    })
}

fn unique_id(prefix: &str) -> String {
    static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    format!(
        "{prefix}_{}_{}_{}",
        std::process::id(),
        nanos,
        COUNTER.fetch_add(1, Ordering::Relaxed)
    )
}

fn sha256_json(value: &Value) -> String {
    let bytes = serde_json::to_vec(value).unwrap_or_else(|_| b"null".to_vec());
    hex::encode(Sha256::digest(bytes))
}

fn execute(
    state: &State,
    args: &Value,
    cancellation: Option<Arc<AtomicBool>>,
    force_background: bool,
) -> Result<Value, StructuredError> {
    let args_object = args.as_object().cloned().unwrap_or_default();
    let selectors = ["command", "input_ref", "execution_ref"]
        .iter()
        .filter(|name| {
            args_object
                .get(**name)
                .and_then(Value::as_str)
                .is_some_and(|value| !value.is_empty())
        })
        .copied()
        .collect::<Vec<_>>();
    let valid = selectors.len() == 1 && (!force_background || selectors[0] != "execution_ref");
    if !valid {
        return Err(StructuredError::new(
            "structured_command_execution_selector_invalid",
            "structured_command_execution_selector_invalid",
            json!({"provided":selectors,"required":"Supply exactly one of command or input_ref; structured_command_execute also accepts execution_ref."}),
        ));
    }
    if args_object
        .get("execution_ref")
        .and_then(Value::as_str)
        .is_some()
    {
        return show_execution(state, args);
    }
    let effective_args =
        if let Some(reference) = args_object.get("input_ref").and_then(Value::as_str) {
            read_input_record(state, reference)?
                .as_object()
                .cloned()
                .unwrap_or_default()
        } else {
            args_object.clone()
        };
    let command = normalize_command(
        effective_args
            .get("command")
            .and_then(Value::as_str)
            .unwrap_or_default(),
    );
    let command_args = effective_args
        .get("args")
        .and_then(Value::as_array)
        .map(|values| {
            values
                .iter()
                .map(|value| value.as_str().unwrap_or_default().to_string())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let working_directory = effective_args
        .get("working_directory")
        .and_then(Value::as_str)
        .map(|value| resolve_path(value, &state.allowed_roots[0]))
        .unwrap_or_else(|| state.allowed_roots[0].clone());
    let timeout_ms = effective_args
        .get("timeout_ms")
        .and_then(Value::as_u64)
        .unwrap_or(60_000)
        .clamp(1, state.max_timeout_ms);
    let test_scope = effective_args
        .get("test_scope")
        .and_then(Value::as_str)
        .unwrap_or_else(|| infer_test_scope(&command, &command_args));
    let expected_cost = effective_args
        .get("expected_cost")
        .and_then(Value::as_str)
        .unwrap_or_else(|| infer_expected_cost(test_scope));
    let posture = json!({"schema": "narada.structured_command.execution_posture.v0", "test_scope": test_scope, "expected_cost": expected_cost, "source": if args_object.get("test_scope").is_some() || args_object.get("expected_cost").is_some() { "caller_declared" } else { "derived" }});
    let decision = decide(state, &command, &command_args, &working_directory);
    if decision.get("status").and_then(Value::as_str) != Some("allowed") {
        let reasons = decision
            .get("reasons")
            .cloned()
            .unwrap_or_else(|| json!([]));
        return Ok(
            json!({"schema": "narada.structured_command.execution_result.v0", "status": "refused", "decision": decision, "refusal_reasons": reasons, "remediation_hints": decision.get("remediation_hints").cloned().unwrap_or_else(|| json!([])), "mcp_fallbacks": [], "command": command, "args": command_args, "working_directory": working_directory.to_string_lossy(), "execution_posture": posture, "test_scope": test_scope, "expected_cost": expected_cost, "executed": false}),
        );
    }
    let background = force_background
        || args_object
            .get("wait_for_completion")
            .and_then(Value::as_bool)
            == Some(false);
    if background {
        return start_background(
            state,
            &command,
            &command_args,
            &working_directory,
            timeout_ms,
            posture,
            args_object.get("input_ref").cloned().unwrap_or(Value::Null),
        );
    }
    if timeout_ms > MAX_SYNCHRONOUS_TIMEOUT_MS {
        return Ok(
            json!({"schema": "narada.structured_command.execution_result.v0", "status": "refused", "executed": false, "decision": decision, "refusal_reasons": ["synchronous_timeout_exceeds_reliable_bound"], "remediation_hints": [format!("Use structured_command_start for commands requiring more than {MAX_SYNCHRONOUS_TIMEOUT_MS}ms, then poll structured_command_execution_show.")], "mcp_fallbacks": [], "command": command, "args": command_args, "working_directory": working_directory.to_string_lossy(), "timeout_ms": timeout_ms, "max_synchronous_timeout_ms": MAX_SYNCHRONOUS_TIMEOUT_MS}),
        );
    }
    let started_at = now_rfc3339();
    let result = run_process(
        &command,
        &command_args,
        &working_directory,
        timeout_ms,
        state.max_output_bytes,
        cancellation,
        &state.env,
    );
    let payload = execution_payload(
        &command,
        &command_args,
        &working_directory,
        &started_at,
        timeout_ms,
        posture,
        result,
        "synchronous",
        true,
        args_object.get("input_ref").cloned().unwrap_or(Value::Null),
    );
    audit(state, &payload);
    let reference = create_execution_record(state, &payload)?;
    Ok(page_execution(&payload, &args_object, Some(reference)))
}

fn show_execution(state: &State, args: &Value) -> Result<Value, StructuredError> {
    let object = args.as_object().cloned().unwrap_or_default();
    let reference = object
        .get("execution_ref")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            StructuredError::new(
                "structured_command_execution_ref_required",
                "structured_command_execution_ref_required",
                json!({}),
            )
        })?;
    let payload = read_execution_record(state, reference)?;
    Ok(page_execution(
        &payload,
        &object,
        Some(reference.to_string()),
    ))
}

fn start_background(
    state: &State,
    command: &str,
    args: &[String],
    cwd: &Path,
    timeout_ms: u64,
    posture: Value,
    input_ref: Value,
) -> Result<Value, StructuredError> {
    let started_at = now_rfc3339();
    let pending = json!({
        "schema":"narada.structured_command.execution_result.v0","status":"running","executed":true,
        "command":command,"args":args,"working_directory":cwd.to_string_lossy(),"started_at":started_at,
        "finished_at":Value::Null,"timeout_ms":timeout_ms,"execution_posture":posture,
        "test_scope":posture.get("test_scope").and_then(Value::as_str).unwrap_or("unknown"),
        "expected_cost":posture.get("expected_cost").and_then(Value::as_str).unwrap_or("unknown"),
        "execution_mode":"background","wait_for_completion":false,"pending":true,"exit_code":Value::Null,
        "stdout":"","stderr":"","stdout_truncated":false,"stderr_truncated":false,
        "timed_out":false,"cancelled":false,"input_ref":input_ref,
    });
    let execution_ref = create_execution_record(state, &pending)?;
    let execution_id = parse_ref(&execution_ref, "execution")?;
    let request_path = state
        .storage_root
        .join("background")
        .join(format!("{execution_id}.json"));
    let request = json!({
        "schema":"narada.structured_command.background_request.v1","execution_ref":execution_ref,
        "command":command,"args":args,"working_directory":cwd.to_string_lossy(),"timeout_ms":timeout_ms,
        "max_output_bytes":state.max_output_bytes,"storage_root":state.storage_root.to_string_lossy(),
        "audit_log_dir":state.audit_log_dir.as_ref().map(|path| path.to_string_lossy().to_string()),
        "started_at":started_at,"execution_posture":posture,"input_ref":input_ref,
    });
    write_json_record(&request_path, &request)?;
    let bytes = fs::read(&request_path).map_err(|error| {
        StructuredError::new(
            "structured_command_background_request_read_failed",
            error.to_string(),
            json!({"path":request_path.to_string_lossy()}),
        )
    })?;
    let digest = hex::encode(Sha256::digest(&bytes));
    let executable = env::current_exe().map_err(|error| {
        StructuredError::new(
            "structured_command_background_executable_unavailable",
            error.to_string(),
            json!({}),
        )
    })?;
    let mut runner = Command::new(executable);
    runner
        .args([
            "structured-command-background",
            "--request",
            &request_path.to_string_lossy(),
            "--sha256",
            &digest,
        ])
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    apply_headless_process_posture(&mut runner);
    match runner.spawn() {
        Ok(child) => Ok(page_execution(&pending, &Map::new(), Some(execution_ref))
            .as_object()
            .cloned()
            .map(|mut value| {
                value.insert("runner_pid".to_string(), json!(child.id()));
                Value::Object(value)
            })
            .unwrap_or(pending)),
        Err(error) => {
            let failed = json!({"schema":"narada.structured_command.execution_result.v0","status":"failed","executed":false,"pending":false,"command":command,"args":args,"working_directory":cwd.to_string_lossy(),"started_at":started_at,"finished_at":now_rfc3339(),"timeout_ms":timeout_ms,"execution_mode":"background","wait_for_completion":false,"error":"background_runner_spawn_failed","stderr":error.to_string(),"stdout":"","exit_code":Value::Null,"timed_out":false,"cancelled":false});
            update_execution_record(state, &execution_ref, &failed)?;
            let _ = fs::remove_file(request_path);
            Err(StructuredError::new(
                "structured_command_background_spawn_failed",
                error.to_string(),
                json!({"execution_ref":execution_ref}),
            ))
        }
    }
}

fn infer_test_scope(command: &str, args: &[String]) -> &'static str {
    if command.eq_ignore_ascii_case("pnpm")
        && args.iter().any(|value| value.eq_ignore_ascii_case("test"))
    {
        if args.iter().any(|value| value == "--filter") {
            "focused"
        } else {
            "broad"
        }
    } else if command.eq_ignore_ascii_case("npm")
        && args.iter().any(|value| value.eq_ignore_ascii_case("test"))
    {
        "broad"
    } else {
        "unknown"
    }
}

fn infer_expected_cost(test_scope: &str) -> &'static str {
    match test_scope {
        "focused" => "low",
        "broad" | "known_slow" => "high",
        _ => "unknown",
    }
}

#[allow(clippy::too_many_arguments)]
fn execution_payload(
    command: &str,
    args: &[String],
    cwd: &Path,
    started_at: &str,
    timeout_ms: u64,
    posture: Value,
    result: ProcessResult,
    mode: &str,
    wait_for_completion: bool,
    input_ref: Value,
) -> Value {
    json!({
        "schema": "narada.structured_command.execution_result.v0",
        "status": if result.cancelled { "cancelled" } else if result.timed_out { "timed_out" } else if result.exit_code == Some(0) { "ok" } else { "failed" },
        "executed": true,
        "command": command,
        "args": args,
        "working_directory": cwd.to_string_lossy(),
        "started_at": started_at,
        "finished_at": now_rfc3339(),
        "timeout_ms": timeout_ms,
        "execution_posture": posture,
        "test_scope": posture.get("test_scope").and_then(Value::as_str).unwrap_or("unknown"),
        "expected_cost": posture.get("expected_cost").and_then(Value::as_str).unwrap_or("unknown"),
        "execution_mode": mode,
        "wait_for_completion": wait_for_completion,
        "pending": false,
        "exit_code": result.exit_code,
        "stdout": result.stdout,
        "stderr": result.stderr,
        "stdout_truncated": result.stdout_truncated,
        "stderr_truncated": result.stderr_truncated,
        "timed_out": result.timed_out,
        "cancelled": result.cancelled,
        "input_ref": input_ref,
    })
}

fn page_execution(payload: &Value, args: &Map<String, Value>, reference: Option<String>) -> Value {
    let persisted = args.contains_key("execution_ref");
    if payload.get("executed").and_then(Value::as_bool) == Some(false) {
        let mut result = payload.clone();
        if let Some(object) = result.as_object_mut() {
            object.insert(
                "execution_ref".to_string(),
                reference.clone().map(Value::String).unwrap_or(Value::Null),
            );
        }
        return result;
    }
    let stdout = payload
        .get("stdout")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let stderr = payload
        .get("stderr")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let stdout_offset = args
        .get("stdout_offset")
        .and_then(Value::as_u64)
        .unwrap_or(0) as usize;
    let stderr_offset = args
        .get("stderr_offset")
        .and_then(Value::as_u64)
        .unwrap_or(0) as usize;
    let stdout_limit = args
        .get("stdout_limit")
        .and_then(Value::as_u64)
        .unwrap_or(1000)
        .clamp(1, 20_000) as usize;
    let stderr_limit = args
        .get("stderr_limit")
        .and_then(Value::as_u64)
        .unwrap_or(1000)
        .clamp(1, 20_000) as usize;
    let stdout_page = text_page(stdout, stdout_offset, stdout_limit);
    let stderr_page = text_page(stderr, stderr_offset, stderr_limit);
    let mut result = payload.clone();
    if let Some(object) = result.as_object_mut() {
        object.insert(
            "execution_ref".to_string(),
            reference.clone().map(Value::String).unwrap_or(Value::Null),
        );
        object.insert("stdout".to_string(), Value::String(stdout_page.0));
        object.insert("stderr".to_string(), Value::String(stderr_page.0));
        object.insert("stdout_offset".to_string(), json!(stdout_offset));
        object.insert("stderr_offset".to_string(), json!(stderr_offset));
        object.insert("stdout_limit".to_string(), json!(stdout_limit));
        object.insert("stderr_limit".to_string(), json!(stderr_limit));
        object.insert(
            "stdout_next_offset".to_string(),
            stdout_page
                .1
                .map(|value| json!(value))
                .unwrap_or(Value::Null),
        );
        object.insert(
            "stderr_next_offset".to_string(),
            stderr_page
                .1
                .map(|value| json!(value))
                .unwrap_or(Value::Null),
        );
        object.insert(
            "stdout_output_truncated".to_string(),
            json!(stdout_page.1.is_some()),
        );
        object.insert(
            "stderr_output_truncated".to_string(),
            json!(stderr_page.1.is_some()),
        );
        object.insert(
            "stdout_char_length".to_string(),
            json!(stdout.chars().count()),
        );
        object.insert(
            "stderr_char_length".to_string(),
            json!(stderr.chars().count()),
        );
        object.insert(
            "page_source".to_string(),
            json!(if persisted {
                "persisted_execution"
            } else {
                "new_execution"
            }),
        );
    }
    result
}

fn text_page(text: &str, offset: usize, limit: usize) -> (String, Option<usize>) {
    let chars = text.chars().collect::<Vec<_>>();
    let start = offset.min(chars.len());
    let end = (start + limit).min(chars.len());
    let chunk = chars[start..end].iter().collect::<String>();
    let next = if end < chars.len() { Some(end) } else { None };
    (chunk, next)
}

fn decide(state: &State, command: &str, args: &[String], cwd: &Path) -> Value {
    let mut reasons = Vec::<String>::new();
    if command.is_empty() {
        reasons.push("command_required".to_string());
    }
    let command_lower = command.to_ascii_lowercase();
    if state
        .blocked_commands
        .iter()
        .any(|value| value == &command_lower)
    {
        reasons.push(format!("blocked_command:{command}"));
    }
    if wraps_cargo_with_pnpm(command, args) {
        reasons.push("package_manager_wrapper_for_native_tool:pnpm cargo".to_string());
    }
    if !inside_any_root(cwd, &state.allowed_roots) {
        reasons.push(format!(
            "working_directory_outside_allowed_roots:{}",
            cwd.to_string_lossy()
        ));
    }
    for value in std::iter::once(command).chain(args.iter().map(String::as_str)) {
        let normalized = value.replace('\\', "/");
        let extension = Path::new(&normalized)
            .extension()
            .and_then(|value| value.to_str())
            .map(|value| format!(".{value}").to_ascii_lowercase());
        if matches!(extension.as_deref(), Some(".cmd") | Some(".bat")) {
            let candidate = resolve_path(value, cwd);
            if !inside_any_root(&candidate, &state.allowed_roots)
                || !candidate.is_file()
                || transient_path(&normalized)
            {
                reasons.push(format!("wrapper_execution_disallowed:{value}"));
            }
        }
        if transient_path(&normalized)
            && extension
                .as_deref()
                .is_some_and(|extension| TRANSIENT_EXTENSIONS.contains(&extension))
        {
            reasons.push(format!("transient_wrapper_path_disallowed:{value}"));
        }
    }
    if !is_command_allowed(
        command,
        args,
        &state.allowed_commands,
        &state.allowed_prefixes,
    ) {
        reasons.push(format!(
            "command_not_allowed:{}",
            std::iter::once(command)
                .chain(args.iter().map(String::as_str))
                .collect::<Vec<_>>()
                .join(" ")
        ));
    }
    let status = if reasons.is_empty() {
        "allowed"
    } else {
        "refused"
    };
    let remediation_hints = reasons
        .iter()
        .map(|reason| {
            if reason.starts_with("blocked_command:") {
                "Use an explicit argv-based allowed command; shell interpreters remain disallowed."
            } else if reason.starts_with("working_directory_outside_allowed_roots:") {
                "Run from an allowed root or request a policy update."
            } else if reason.starts_with("command_not_allowed:") {
                "Inspect policy and use an allowlisted command or prefix."
            } else if reason.starts_with("package_manager_wrapper_for_native_tool:") {
                "Invoke cargo directly; pnpm is not part of the native Rust toolchain."
            } else {
                "Use the owning MCP surface or a canonical repository entrypoint."
            }
        })
        .map(String::from)
        .collect::<Vec<_>>();
    json!({"schema": "narada.structured_command.execution_decision.v0", "status": status, "reasons": reasons, "remediation_hints": remediation_hints, "mcp_fallbacks": [], "command": command, "args": args, "working_directory": cwd.to_string_lossy(), "shell_interpolation": false})
}

fn wraps_cargo_with_pnpm(command: &str, args: &[String]) -> bool {
    command.eq_ignore_ascii_case("pnpm")
        && args
            .first()
            .is_some_and(|value| value.eq_ignore_ascii_case("exec"))
        && args.get(1).is_some_and(|value| {
            value.eq_ignore_ascii_case("cargo") || value.eq_ignore_ascii_case("cargo.exe")
        })
}

fn is_command_allowed(
    command: &str,
    args: &[String],
    allowed_commands: &[String],
    allowed_prefixes: &[Vec<String>],
) -> bool {
    let command_lower = command.to_ascii_lowercase();
    if allowed_commands.iter().any(|value| value == &command_lower) {
        return true;
    }
    let argv = std::iter::once(command)
        .chain(args.iter().map(String::as_str))
        .map(|value| value.to_ascii_lowercase())
        .collect::<Vec<_>>();
    allowed_prefixes.iter().any(|prefix| {
        prefix.iter().enumerate().all(|(index, expected)| {
            let Some(actual) = argv.get(index) else {
                return false;
            };
            if index == 0 {
                actual == expected || (expected == "pwsh" && actual == "pwsh.exe")
            } else {
                actual == expected
            }
        }) && !(prefix.len() >= 2
            && prefix[0] == "pnpm"
            && prefix[1] == "--filter"
            && !matches!(
                argv.get(3).map(String::as_str),
                Some("test" | "build" | "typecheck")
            ))
    })
}

fn transient_path(value: &str) -> bool {
    let normalized = value.replace('\\', "/").to_ascii_lowercase();
    normalized.contains("/.ai/tmp/")
        || normalized.contains("/.ai/temp/")
        || normalized.starts_with(".ai/tmp/")
        || normalized.starts_with(".ai/temp/")
}

fn normalize_command(value: &str) -> String {
    let trimmed = value.trim();
    if trimmed.is_empty()
        || trimmed
            .chars()
            .any(|character| matches!(character, '\r' | '\n' | ';' | '&' | '|' | '<' | '>'))
    {
        String::new()
    } else {
        trimmed.to_string()
    }
}

fn tool_result(state: &State, payload: Value, tool_name: &str) -> Result<Value, StructuredError> {
    let text = serde_json::to_string_pretty(&payload).unwrap_or_else(|_| "{}".to_string());
    if text.chars().count() <= 4_000 {
        return Ok(
            json!({"content": [{"type": "text", "text": text, "annotations": {"audience": ["assistant"]}}], "structuredContent": payload}),
        );
    }
    let (reference, full_length) = materialize_output(state, tool_name, &payload, &text)?;
    let preview = text.chars().take(3_200).collect::<String>();
    let envelope = json!({"schema": "narada.producer_output_page.v1", "status": payload.get("status").and_then(Value::as_str).unwrap_or("ok"), "truncated": true, "output_ref": reference, "ref": reference, "result_materialized": true, "tool_name": tool_name, "offset": 0, "limit": 3_200, "next_offset": if full_length > 3_200 { json!(3_200) } else { Value::Null }, "transport_offset": 0, "transport_limit": 3_200, "transport_next_offset": if full_length > 3_200 { json!(3_200) } else { Value::Null }, "output_text": preview, "output_truncated": full_length > 3_200, "reader_tool": "structured_command_output_show", "site_root": state.site_root.to_string_lossy(), "read_command": format!("structured_command_output_show({{ ref: \\\"{reference}\\\" }})"), "remediation": format!("Use structured_command_output_show with ref={reference} to read bounded pages."), "inline_limit": 3_200, "full_output_char_length": full_length});
    let content = serde_json::to_string(&envelope).unwrap_or_else(|_| "{}".to_string());
    Ok(
        json!({"content": [{"type": "text", "text": content, "annotations": {"audience": ["assistant"]}}], "structuredContent": envelope}),
    )
}

fn materialize_output(
    state: &State,
    tool_name: &str,
    payload: &Value,
    text: &str,
) -> Result<(String, usize), StructuredError> {
    let id = unique_id("o");
    let reference = format!("mcp_output:{id}");
    let path = state
        .site_root
        .join(".ai")
        .join("tmp")
        .join("mcp-outputs")
        .join("workspace")
        .join(format!("{id}.json"));
    let record = json!({"schema": "narada.mcp_output_ref.v1", "ref": reference, "output_id": id, "tool_name": tool_name, "created_at": now_rfc3339(), "created_by": Value::Null, "content_type": "application/json", "inline_char_limit": 3_200, "full_output_char_length": text.chars().count(), "truncated": true, "sha256": sha256_json(payload), "max_bytes": 20 * 1024 * 1024, "full_output": payload});
    let serialized = format!(
        "{}\n",
        serde_json::to_string(&record).unwrap_or_else(|_| "{}".to_string())
    );
    if serialized.len() > 20 * 1024 * 1024 {
        return Err(StructuredError::new(
            "mcp_output_too_large",
            "mcp_output_too_large",
            json!({"ref": reference}),
        ));
    }
    write_json_record(&path, &record)?;
    Ok((reference, text.chars().count()))
}

fn output_show(state: &State, args: &Value) -> Result<Value, StructuredError> {
    let object = args.as_object().cloned().unwrap_or_default();
    let reference = object
        .get("ref")
        .and_then(Value::as_str)
        .or_else(|| object.get("output_ref").and_then(Value::as_str))
        .unwrap_or_default();
    let Some(id) = reference.strip_prefix("mcp_output:") else {
        return Err(StructuredError::new(
            "output_ref_invalid",
            "output_ref_invalid",
            json!({"ref": reference}),
        ));
    };
    if id.len() < 8
        || !id.chars().all(|character| {
            character.is_ascii_alphanumeric() || character == '_' || character == '-'
        })
    {
        return Err(StructuredError::new(
            "output_ref_invalid",
            "output_ref_invalid",
            json!({"ref": reference}),
        ));
    }
    let record = read_json_record(
        &state
            .site_root
            .join(".ai")
            .join("tmp")
            .join("mcp-outputs")
            .join("workspace")
            .join(format!("{id}.json")),
    )?;
    if record.get("schema").and_then(Value::as_str) != Some("narada.mcp_output_ref.v1") {
        return Err(StructuredError::new(
            "output_ref_schema_unsupported",
            "output_ref_schema_unsupported",
            json!({"ref": reference}),
        ));
    }
    let payload = record.get("full_output").cloned().unwrap_or(Value::Null);
    let text = serde_json::to_string_pretty(&payload).unwrap_or_else(|_| "null".to_string());
    let offset = object.get("offset").and_then(Value::as_u64).unwrap_or(0) as usize;
    let limit = object
        .get("limit")
        .and_then(Value::as_u64)
        .unwrap_or(20_000)
        .clamp(1, 20_000) as usize;
    let page = text_page(&text, offset, limit);
    Ok(
        json!({"schema": "narada.mcp_output_page.v1", "status": "ok", "ref": reference, "tool_name": record.get("tool_name"), "full_output_char_length": text.chars().count(), "byte_size": Value::Null, "original_truncated": true, "path": format!(".ai/tmp/mcp-outputs/workspace/{id}.json"), "offset": offset.min(text.chars().count()), "limit": limit, "output_limit": limit, "output_truncated": page.1.is_some(), "next_offset": page.1.map(|value| json!(value)).unwrap_or(Value::Null), "output_text": page.0}),
    )
}

fn error_diagnostic(error: &StructuredError) -> Value {
    let mut details = error.details.as_object().cloned().unwrap_or_default();
    details.insert(
        "diagnostic_owner".to_string(),
        json!("structured-command-mcp"),
    );
    details.insert(
        "diagnostic_rule".to_string(),
        json!("surface_policy_or_tool_validation"),
    );
    json!({"schema": "narada.structured_command.error.v0", "code": error.code, "message": error.message, "details": details})
}

struct ProcessResult {
    exit_code: Option<i32>,
    timed_out: bool,
    cancelled: bool,
    stdout: String,
    stderr: String,
    stdout_truncated: bool,
    stderr_truncated: bool,
}

fn run_process(
    command: &str,
    args: &[String],
    cwd: &Path,
    timeout_ms: u64,
    max_output_bytes: usize,
    cancellation: Option<Arc<AtomicBool>>,
    environment: &std::collections::HashMap<String, String>,
) -> ProcessResult {
    let (spawn_command, spawn_args) = resolve_command_for_spawn(command, args, environment);
    let mut process = Command::new(spawn_command);
    process
        .args(spawn_args)
        .current_dir(cwd)
        .envs(environment)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    apply_headless_process_posture(&mut process);
    let child_result = process.spawn();
    let Ok(mut child) = child_result else {
        return ProcessResult {
            exit_code: None,
            timed_out: false,
            cancelled: false,
            stdout: String::new(),
            stderr: child_result
                .err()
                .map(|error| error.to_string())
                .unwrap_or_else(|| "process_spawn_failed".to_string()),
            stdout_truncated: false,
            stderr_truncated: false,
        };
    };
    let stdout_handle = child
        .stdout
        .take()
        .map(|stream| thread::spawn(move || read_bounded(stream, max_output_bytes)));
    let stderr_handle = child
        .stderr
        .take()
        .map(|stream| thread::spawn(move || read_bounded(stream, max_output_bytes)));
    let deadline = Instant::now() + Duration::from_millis(timeout_ms);
    let mut timed_out = false;
    let mut cancelled = false;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None)
                if cancellation
                    .as_ref()
                    .is_some_and(|token| token.load(Ordering::Acquire)) =>
            {
                cancelled = true;
                kill_child(&mut child);
                break child.wait().ok();
            }
            Ok(None) if Instant::now() >= deadline => {
                timed_out = true;
                kill_child(&mut child);
                break child.wait().ok();
            }
            Ok(None) => thread::sleep(Duration::from_millis(5)),
            Err(_) => break child.wait().ok(),
        }
    };
    let stdout_result = stdout_handle
        .and_then(|handle| handle.join().ok())
        .unwrap_or((Vec::new(), false));
    let stderr_result = stderr_handle
        .and_then(|handle| handle.join().ok())
        .unwrap_or((Vec::new(), false));
    ProcessResult {
        exit_code: status.and_then(|status| status.code()),
        timed_out,
        cancelled,
        stdout: String::from_utf8_lossy(&stdout_result.0).to_string(),
        stderr: String::from_utf8_lossy(&stderr_result.0).to_string(),
        stdout_truncated: stdout_result.1,
        stderr_truncated: stderr_result.1,
    }
}

fn resolve_command_for_spawn(
    command: &str,
    args: &[String],
    environment: &std::collections::HashMap<String, String>,
) -> (PathBuf, Vec<String>) {
    if !cfg!(windows) || Path::new(command).extension().is_some() {
        return (PathBuf::from(command), args.to_vec());
    }
    let Some(path) = environment
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case("PATH"))
        .map(|(_, value)| value)
    else {
        return (PathBuf::from(command), args.to_vec());
    };
    if let Some(resolved) = resolve_corepack_pnpm(command, path, args) {
        return resolved;
    }
    for directory in env::split_paths(path) {
        for extension in [".exe", ".com", ".ps1", ".cmd", ".bat", ""] {
            let candidate = directory.join(format!("{command}{extension}"));
            if !candidate.is_file() {
                continue;
            }
            if extension == ".ps1" {
                let mut wrapped = vec![
                    "-NoLogo".to_string(),
                    "-NoProfile".to_string(),
                    "-NonInteractive".to_string(),
                    "-ExecutionPolicy".to_string(),
                    "Bypass".to_string(),
                    "-File".to_string(),
                    candidate.to_string_lossy().to_string(),
                ];
                wrapped.extend_from_slice(args);
                return (resolve_noninteractive_powershell(environment), wrapped);
            }
            if extension == ".cmd" || extension == ".bat" {
                let script = candidate.with_extension("ps1");
                if script.is_file() {
                    let mut wrapped = vec![
                        "-NoLogo".to_string(),
                        "-NoProfile".to_string(),
                        "-NonInteractive".to_string(),
                        "-ExecutionPolicy".to_string(),
                        "Bypass".to_string(),
                        "-File".to_string(),
                        script.to_string_lossy().to_string(),
                    ];
                    wrapped.extend_from_slice(args);
                    return (resolve_noninteractive_powershell(environment), wrapped);
                }
            }
            return (candidate, args.to_vec());
        }
    }
    (PathBuf::from(command), args.to_vec())
}

fn resolve_corepack_pnpm(
    command: &str,
    path: &str,
    args: &[String],
) -> Option<(PathBuf, Vec<String>)> {
    if !command.eq_ignore_ascii_case("pnpm") {
        return None;
    }
    for directory in env::split_paths(path) {
        let node = directory.join("node.exe");
        let entrypoint = directory.join("node_modules/corepack/dist/pnpm.js");
        if node.is_file() && entrypoint.is_file() {
            let mut direct_args = vec![entrypoint.to_string_lossy().to_string()];
            direct_args.extend_from_slice(args);
            return Some((node, direct_args));
        }
    }
    None
}

fn resolve_noninteractive_powershell(
    environment: &std::collections::HashMap<String, String>,
) -> PathBuf {
    let native_pwsh = environment
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case("PATH"))
        .and_then(|(_, value)| {
            env::split_paths(value)
                .map(|directory| directory.join("pwsh.exe"))
                .find(|candidate| {
                    candidate.is_file()
                        && !candidate
                            .to_string_lossy()
                            .to_ascii_lowercase()
                            .contains("\\windowsapps\\")
                })
        });
    if let Some(executable) = native_pwsh {
        return executable;
    }
    let system_root = environment
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case("SystemRoot"))
        .map(|(_, value)| PathBuf::from(value))
        .unwrap_or_else(|| PathBuf::from(r"C:\Windows"));
    system_root.join(r"System32\WindowsPowerShell\v1.0\powershell.exe")
}
fn read_bounded<R: Read>(mut reader: R, max: usize) -> (Vec<u8>, bool) {
    let mut output = Vec::with_capacity(max.min(8192));
    let mut buffer = [0_u8; 8192];
    let mut truncated = false;
    loop {
        match reader.read(&mut buffer) {
            Ok(0) => break,
            Ok(count) => {
                if output.len() < max {
                    let keep = (max - output.len()).min(count);
                    output.extend_from_slice(&buffer[..keep]);
                    if keep < count {
                        truncated = true;
                    }
                } else {
                    truncated = true;
                }
            }
            Err(_) => break,
        }
    }
    (output, truncated)
}

fn apply_headless_process_posture(command: &mut Command) {
    for variable in TERMINAL_INTEGRATION_ENVIRONMENT {
        command.env_remove(variable);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(CREATE_NO_WINDOW);
    }
}

fn kill_child(child: &mut Child) {
    #[cfg(windows)]
    {
        let pid = child.id();
        let mut command = Command::new("taskkill");
        command
            .args(["/PID", &pid.to_string(), "/T", "/F"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        apply_headless_process_posture(&mut command);
        let _ = command.status();
    }
    #[cfg(not(windows))]
    {
        let _ = child.kill();
    }
}

fn audit(state: &State, payload: &Value) {
    let Some(directory) = &state.audit_log_dir else {
        return;
    };
    if fs::create_dir_all(directory).is_err() {
        return;
    }
    let path = directory.join("structured-command.jsonl");
    let Ok(mut file) = OpenOptions::new().create(true).append(true).open(path) else {
        return;
    };
    let _ = writeln!(
        file,
        "{}",
        serde_json::to_string(payload).unwrap_or_else(|_| "{}".to_string())
    );
}

fn inside_any_root(path: &Path, roots: &[PathBuf]) -> bool {
    let candidate_key = path_key(path);
    roots.iter().any(|root| {
        let root_key = path_key(root);
        candidate_key == root_key || candidate_key.starts_with(&(root_key + "/"))
    })
}

fn path_key(path: &Path) -> String {
    let value = path.to_string_lossy().replace('\\', "/");
    value.trim_end_matches('/').to_ascii_lowercase()
}

fn resolve_path(value: &str, base: &Path) -> PathBuf {
    let path = PathBuf::from(value);
    if path.is_absolute() {
        absolute(path)
    } else {
        absolute(base.join(path))
    }
}

fn absolute(path: PathBuf) -> PathBuf {
    if path.is_absolute() {
        path
    } else {
        env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join(path)
    }
}

fn now_rfc3339() -> String {
    OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .unwrap_or_else(|_| "1970-01-01T00:00:00Z".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn site_extra_allowed_roots_extend_structured_command_authority() {
        let site_root = env::temp_dir().join(format!(
            "narada-structured-command-site-roots-{}",
            unique_id("test")
        ));
        let worktree_root = site_root.with_extension("worktrees");
        fs::create_dir_all(site_root.join(".narada")).expect("control root");
        fs::create_dir_all(&worktree_root).expect("worktree root");
        fs::write(
            site_root.join(".narada/allowed-roots.json"),
            serde_json::to_vec(&json!({
                "schema": "narada.site.allowed_roots.v1",
                "extra_allowed_roots": [worktree_root.to_string_lossy()]
            }))
            .expect("config"),
        )
        .expect("write config");

        let state = parse_state(&[
            "--allowed-root".into(),
            site_root.to_string_lossy().to_string(),
            "--site-root".into(),
            site_root.to_string_lossy().to_string(),
        ])
        .expect("state");
        assert!(state
            .allowed_roots
            .iter()
            .any(|root| root == &worktree_root));

        fs::remove_dir_all(site_root).expect("cleanup site");
        fs::remove_dir_all(worktree_root).expect("cleanup worktrees");
    }

    #[cfg(windows)]
    #[test]
    fn windows_execution_environment_hydrates_complete_x64_msvc_toolchain() {
        let root = env::temp_dir().join(format!(
            "narada-structured-command-msvc-{}",
            unique_id("test")
        ));
        let visual_studio = root
            .join("Microsoft Visual Studio")
            .join("2022")
            .join("BuildTools");
        let msvc = visual_studio
            .join("VC")
            .join("Tools")
            .join("MSVC")
            .join("14.44.35207");
        let msvc_bin = msvc.join("bin").join("Hostx64").join("x64");
        fs::create_dir_all(&msvc_bin).expect("msvc bin");
        fs::create_dir_all(msvc.join("lib").join("x64")).expect("msvc lib");
        fs::create_dir_all(msvc.join("include")).expect("msvc include");
        fs::write(msvc_bin.join("link.exe"), b"fixture").expect("link fixture");

        let sdk = root.join("Windows Kits").join("10");
        let version = "10.0.26100.0";
        for include in ["ucrt", "shared", "um", "winrt", "cppwinrt"] {
            fs::create_dir_all(sdk.join("Include").join(version).join(include))
                .expect("sdk include");
        }
        fs::create_dir_all(sdk.join("Lib").join(version).join("ucrt").join("x64"))
            .expect("sdk ucrt lib");
        let sdk_um = sdk.join("Lib").join(version).join("um").join("x64");
        fs::create_dir_all(&sdk_um).expect("sdk um lib");
        fs::write(sdk_um.join("kernel32.lib"), b"fixture").expect("kernel fixture");
        fs::create_dir_all(sdk.join("bin").join(version).join("x64")).expect("sdk bin");

        let mut environment = std::collections::HashMap::from([
            (
                "ProgramFiles(x86)".to_string(),
                root.to_string_lossy().to_string(),
            ),
            ("Path".to_string(), r"C:\existing".to_string()),
        ]);
        augment_windows_msvc_environment(&mut environment);

        let path = environment_value(&environment, "PATH").expect("path");
        assert_eq!(
            env::split_paths(path).next().as_deref(),
            Some(msvc_bin.as_path())
        );
        let lib = environment_value(&environment, "LIB").expect("lib");
        assert!(env::split_paths(lib).any(|path| path == sdk_um));
        assert_eq!(
            environment_value(&environment, "VCToolsInstallDir"),
            Some(msvc.to_string_lossy().as_ref())
        );
        assert_eq!(
            environment_value(&environment, "WindowsSDKVersion"),
            Some("10.0.26100.0\\")
        );

        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn command_tools_publish_closed_alternative_aware_schemas() {
        let tools = list_tools();
        for name in [
            "structured_command_execute",
            "structured_command_start",
            "structured_command_input_create",
        ] {
            let tool = tools
                .iter()
                .find(|tool| tool["name"] == name)
                .expect("tool");
            assert_eq!(tool["inputSchema"]["additionalProperties"], false);
        }
        let execute = tools
            .iter()
            .find(|tool| tool["name"] == "structured_command_execute")
            .expect("execute");
        assert_eq!(
            execute["inputSchema"]["oneOf"].as_array().map(Vec::len),
            Some(3)
        );
        let start = tools
            .iter()
            .find(|tool| tool["name"] == "structured_command_start")
            .expect("start");
        assert_eq!(
            start["inputSchema"]["oneOf"].as_array().map(Vec::len),
            Some(2)
        );
    }

    #[test]
    fn execution_selector_refuses_ambiguous_calls() {
        let root = env::temp_dir();
        let state = State {
            allowed_roots: vec![root.clone()],
            allowed_commands: vec![],
            allowed_prefixes: vec![],
            blocked_commands: vec![],
            max_timeout_ms: 10_000,
            max_output_bytes: 4096,
            audit_log_dir: None,
            site_root: root.clone(),
            storage_root: root,
            env: env::vars().collect(),
        };
        let error = execute(
            &state,
            &json!({"command":"cargo","input_ref":"structured_command_input:fixture01"}),
            None,
            false,
        )
        .expect_err("ambiguous selector");
        assert_eq!(error.code, "structured_command_execution_selector_invalid");
        let error = execute(
            &state,
            &json!({"execution_ref":"structured_command_execution:fixture01"}),
            None,
            true,
        )
        .expect_err("start cannot read");
        assert_eq!(error.code, "structured_command_execution_selector_invalid");
    }

    #[test]
    fn background_runner_completes_durable_execution_record() {
        let root = env::temp_dir().join(format!(
            "narada-structured-command-background-{}",
            unique_id("test")
        ));
        fs::create_dir_all(&root).expect("root");
        let state = State {
            allowed_roots: vec![root.clone()],
            allowed_commands: vec![],
            allowed_prefixes: vec![],
            blocked_commands: vec![],
            max_timeout_ms: 10_000,
            max_output_bytes: 4096,
            audit_log_dir: None,
            site_root: root.clone(),
            storage_root: root.clone(),
            env: env::vars().collect(),
        };
        let pending = json!({"schema":"narada.structured_command.execution_result.v0","status":"running","executed":true,"pending":true,"stdout":"","stderr":""});
        let execution_ref = create_execution_record(&state, &pending).expect("execution");
        let request_path = root.join("background-request.json");
        let request = json!({
            "schema":"narada.structured_command.background_request.v1","execution_ref":execution_ref,
            "command":"cargo","args":["--version"],"working_directory":root.to_string_lossy(),
            "timeout_ms":10_000,"max_output_bytes":4096,"storage_root":root.to_string_lossy(),
            "audit_log_dir":Value::Null,"started_at":now_rfc3339(),
            "execution_posture":{"test_scope":"focused","expected_cost":"low"},"input_ref":Value::Null,
        });
        write_json_record(&request_path, &request).expect("request");
        let digest = hex::encode(Sha256::digest(fs::read(&request_path).expect("bytes")));
        run_background(&[
            "--request".into(),
            request_path.to_string_lossy().to_string(),
            "--sha256".into(),
            digest,
        ])
        .expect("background runner");
        let completed = read_execution_record(&state, &execution_ref).expect("completed");
        assert_eq!(completed["status"], "ok");
        assert_eq!(completed["pending"], false);
        assert!(completed["stdout"]
            .as_str()
            .is_some_and(|stdout| stdout.starts_with("cargo ")));
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn pnpm_corepack_shim_resolves_without_shell_or_terminal() {
        let root = env::temp_dir().join(format!(
            "narada-structured-command-resolver-{}",
            std::process::id()
        ));
        let entrypoint = root.join("node_modules/corepack/dist/pnpm.js");
        fs::create_dir_all(entrypoint.parent().unwrap()).unwrap();
        fs::write(root.join("node.exe"), b"fixture").unwrap();
        fs::write(&entrypoint, b"fixture").unwrap();
        let path = env::join_paths([&root])
            .unwrap()
            .to_string_lossy()
            .to_string();
        let requested = vec!["exec".to_string(), "cargo".to_string()];

        let (executable, arguments) =
            resolve_corepack_pnpm("pnpm", &path, &requested).expect("direct Corepack launch");

        assert_eq!(executable, root.join("node.exe"));
        assert_eq!(arguments[0], entrypoint.to_string_lossy());
        assert_eq!(&arguments[1..], requested);
        assert!(!executable
            .to_string_lossy()
            .to_ascii_lowercase()
            .contains("wt.exe"));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn pnpm_must_not_wrap_cargo() {
        assert!(wraps_cargo_with_pnpm(
            "pnpm",
            &["exec".to_string(), "cargo".to_string(), "check".to_string()]
        ));
        assert!(!wraps_cargo_with_pnpm("cargo", &["check".to_string()]));
    }
}
