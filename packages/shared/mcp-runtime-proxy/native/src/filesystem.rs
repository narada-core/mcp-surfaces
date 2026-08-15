use crate::protocol;
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::env;
use std::fs;
use std::fs::OpenOptions;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;

const PROTOCOL_VERSION: &str = "2024-11-05";
const READ_TIMEOUT_MS: u64 = 5_000;
const WRITE_TIMEOUT_MS: u64 = 10_000;
const SEARCH_TIMEOUT_MS: u64 = 60_000;
const MAX_READ_LINES: i64 = 1_000;
const MAX_READ_LINE_BYTES: usize = 1024 * 1024;
const MAX_READ_WINDOW_BYTES: usize = 4 * 1024 * 1024;
const MAX_SEARCH_CAPTURE_ENTRIES: usize = 10_000;
const MAX_SEARCH_CAPTURE_BYTES: usize = 32 * 1024 * 1024;
const MAX_SEARCH_LINE_BYTES: usize = 256 * 1024;
const MAX_TEXT_MUTATION_BYTES: u64 = 8 * 1024 * 1024;
const DEFAULT_GLOB_IGNORES: &[&str] = &[
    "**/.git/**",
    "**/node_modules/**",
    "**/dist/**",
    "**/build/**",
    "**/coverage/**",
    "**/.next/**",
    "**/.turbo/**",
    "**/.cache/**",
    "**/.venv/**",
    "**/__pycache__/**",
    "**/target/**",
];
const DEFAULT_GREP_IGNORES: &[&str] = &[
    "**/.git/**",
    "**/node_modules/**",
    "**/dist/**",
    "**/build/**",
    "**/coverage/**",
    "**/.next/**",
    "**/.turbo/**",
    "**/.cache/**",
    "**/.venv/**",
    "**/__pycache__/**",
    "**/target/**",
    "**/.ai/runtime/**",
    "**/.ai/tmp/**",
    "**/.ai/output/**",
    "**/.narada/runtime/**",
    "**/.narada/tmp/**",
    "**/.tmp-tests/**",
];
const GENERATED_MARKERS: &[&str] = &[
    "/.ai/runtime/",
    "/.ai/tmp/",
    "/.ai/output/",
    "/.narada/runtime/",
    "/.narada/tmp/",
    "/.narada/local-filesystem-mcp/patch-outcomes/",
    "/.tmp-tests/",
];
const TRANSIENT_EXECUTABLE_EXTENSIONS: &[&str] = &[
    ".cmd", ".bat", ".ps1", ".psm1", ".js", ".mjs", ".cjs", ".ts",
];

#[derive(Clone)]
pub(crate) struct State {
    mode: String,
    allowed_roots: Vec<PathBuf>,
    root_entries: Vec<Value>,
    output_root: PathBuf,
    audit_log_dir: Option<PathBuf>,
    cache: HashMap<String, (String, Vec<String>, bool)>,
    snapshots: HashMap<String, (Vec<String>, bool)>,
    snapshot_order: Vec<String>,
}

pub(crate) fn site_allowed_roots_config_path(output_root: &Path) -> PathBuf {
    let control_root = if output_root
        .file_name()
        .map(|name| name.to_string_lossy().eq_ignore_ascii_case(".narada"))
        .unwrap_or(false)
    {
        output_root.to_path_buf()
    } else {
        output_root.join(".narada")
    };
    control_root.join("allowed-roots.json")
}

fn parse_site_root_config(output_root: &Path, keys: &[&str]) -> Vec<String> {
    let path = site_allowed_roots_config_path(output_root);
    let Ok(text) = fs::read_to_string(path) else {
        return Vec::new();
    };
    let Ok(value) = serde_json::from_str::<Value>(&text) else {
        return Vec::new();
    };
    keys.iter()
        .flat_map(|key| {
            value
                .get(*key)
                .and_then(Value::as_array)
                .into_iter()
                .flat_map(|items| items.iter().filter_map(Value::as_str).map(str::to_string))
        })
        .collect()
}

pub(crate) fn parse_site_allowed_roots(output_root: &Path) -> Vec<String> {
    parse_site_root_config(output_root, &["extra_allowed_roots", "temp_allowed_roots"])
}

pub(crate) fn parse_site_extra_allowed_roots(output_root: &Path) -> Vec<String> {
    parse_site_root_config(output_root, &["extra_allowed_roots"])
}

#[derive(Debug)]
struct FsError {
    code: String,
    message: String,
    details: Value,
}

impl FsError {
    fn new(code: impl Into<String>, message: impl Into<String>, details: Value) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
            details,
        }
    }
}

pub fn run(args: &[String]) -> Result<(), String> {
    let mut state = parse_state(args)?;
    let server_name = format!("local-filesystem-{}-native", state.mode);
    let stdin = io::stdin();
    let mut reader = BufReader::new(stdin.lock());
    let stdout = io::stdout();
    let mut writer = stdout.lock();
    loop {
        let Some((request, framed)) = read_message(&mut reader).map_err(|e| e.to_string())? else {
            break;
        };
        if request.get("id").is_none() {
            continue;
        }
        if let Some(response) = protocol::preflight_response(&request, &server_name) {
            write_message(&mut writer, &response, framed).map_err(|e| e.to_string())?;
            writer.flush().map_err(|e| e.to_string())?;
            continue;
        }
        if let Some(response) = handle_request(&mut state, &request) {
            let response = protocol::modernize_response(&request, response, &server_name);
            write_message(&mut writer, &response, framed).map_err(|e| e.to_string())?;
            writer.flush().map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}

fn parse_state(args: &[String]) -> Result<State, String> {
    let mut mode = "read".to_string();
    let mut roots = Vec::<String>::new();
    let mut anchored = Vec::<String>::new();
    let mut roots_config: Option<String> = None;
    let mut output_root: Option<String> = None;
    let mut audit_log_dir: Option<String> = None;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--mode" => {
                index += 1;
                mode = args.get(index).cloned().ok_or("filesystem_mode_required")?;
            }
            "--allowed-root" => {
                index += 1;
                roots.push(
                    args.get(index)
                        .cloned()
                        .ok_or("filesystem_allowed_root_required")?,
                );
            }
            "--anchored-allowed-root" => {
                index += 1;
                anchored.push(
                    args.get(index)
                        .cloned()
                        .ok_or("filesystem_anchored_root_required")?,
                );
            }
            "--roots-config" => {
                index += 1;
                roots_config = Some(
                    args.get(index)
                        .cloned()
                        .ok_or("filesystem_roots_config_required")?,
                );
            }
            "--output-root" => {
                index += 1;
                output_root = Some(
                    args.get(index)
                        .cloned()
                        .ok_or("filesystem_output_root_required")?,
                );
            }
            "--audit-log-dir" => {
                index += 1;
                audit_log_dir = Some(
                    args.get(index)
                        .cloned()
                        .ok_or("filesystem_audit_log_dir_required")?,
                );
            }
            "--roots-from-trust-config" | "--roots-from-codex-config" => {
                index += 1;
                let path = args
                    .get(index)
                    .cloned()
                    .ok_or("filesystem_trust_config_required")?;
                roots.extend(parse_trust_config(Path::new(&path)));
            }
            "--help" => return Err("filesystem_help".to_string()),
            other => return Err(format!("filesystem_unknown_argument:{other}")),
        }
        index += 1;
    }
    let output_root = absolute(
        output_root
            .map(PathBuf::from)
            .unwrap_or_else(|| env::current_dir().unwrap_or_else(|_| PathBuf::from("."))),
    );
    let mut root_specs = roots
        .into_iter()
        .map(|root| {
            (
                root,
                json!({"source": "explicit_flag", "flag": "--allowed-root"}),
            )
        })
        .collect::<Vec<_>>();
    if let Some(path) = roots_config {
        let config_path = PathBuf::from(path);
        let config_path_text = config_path.to_string_lossy().to_string();
        root_specs.extend(parse_roots_config(&config_path).into_iter().map(|root| {
            (
                root,
                json!({"source": "roots_config", "config_path": config_path_text.clone()}),
            )
        }));
    }
    let site_config_path = site_allowed_roots_config_path(&output_root);
    root_specs.extend(parse_site_allowed_roots(&output_root).into_iter().map(|root| {
        (
            root,
            json!({"source": "site_allowed_roots_config", "config_path": site_config_path.to_string_lossy()}),
        )
    }));
    for spec in anchored {
        root_specs.push((
            resolve_anchor(&spec)?,
            json!({"source": "anchored_allowed_root", "flag": "--anchored-allowed-root", "spec": spec}),
        ));
    }
    if mode != "read" && mode != "write" {
        return Err("filesystem_mode_must_be_read_or_write".to_string());
    }
    let mut entries = Vec::new();
    let mut allowed_roots = Vec::new();
    for (root, provenance) in root_specs {
        let path = absolute(PathBuf::from(root));
        let key = normalize_path(&path);
        if allowed_roots
            .iter()
            .any(|value: &PathBuf| normalize_path(value) == key)
        {
            continue;
        }
        entries.push(json!({"root": path.to_string_lossy(), "provenance": provenance}));
        allowed_roots.push(path);
    }
    if allowed_roots.is_empty() {
        return Err("filesystem_mcp_requires_at_least_one_allowed_root".to_string());
    }
    Ok(State {
        mode,
        allowed_roots,
        root_entries: entries,
        output_root,
        audit_log_dir: audit_log_dir.map(|value| absolute(PathBuf::from(value))),
        cache: HashMap::new(),
        snapshots: HashMap::new(),
        snapshot_order: Vec::new(),
    })
}

pub(crate) fn parse_state_for_rhai(args: &[String]) -> Result<State, String> {
    parse_state(args)
}

pub(crate) fn mode_for_rhai(state: &State) -> &str {
    &state.mode
}

pub(crate) fn initialize_for_rhai(request: &Value, mode: &str) -> Value {
    initialize(request, mode)
}

pub(crate) fn tools_list_for_rhai(mode: &str) -> Value {
    json!({"tools": list_tools(mode)})
}

pub(crate) fn tool_call_for_rhai(state: &mut State, params: &Value) -> Value {
    match call_tool(state, params) {
        Ok(result) => json!({"ok": true, "result": result}),
        Err(error) => json!({
            "ok": false,
            "error": {
                "code": -32000,
                "message": error.message,
                "data": diagnostic(&error)
            }
        }),
    }
}

fn parse_roots_config(path: &Path) -> Vec<String> {
    let Ok(text) = fs::read_to_string(path) else {
        return Vec::new();
    };
    let Ok(value) = serde_json::from_str::<Value>(&text) else {
        return Vec::new();
    };
    value
        .get("allowed_roots")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

fn parse_trust_config(path: &Path) -> Vec<String> {
    let Ok(text) = fs::read_to_string(path) else {
        return Vec::new();
    };
    let mut current: Option<String> = None;
    let mut roots = Vec::new();
    for raw in text.lines() {
        let line = raw.trim();
        if let Some(value) = line
            .strip_prefix("[projects.'")
            .and_then(|value| value.strip_suffix("']"))
        {
            current = Some(value.to_string());
        } else if line.starts_with('[') {
            current = None;
        } else if line.eq_ignore_ascii_case("trust_level = \"trusted\"") {
            if let Some(value) = current.clone() {
                roots.push(value);
            }
        }
    }
    roots
}

fn user_home_anchor() -> Option<PathBuf> {
    user_home_anchor_from(|key| env::var_os(key))
}

fn user_home_anchor_from<F>(get: F) -> Option<PathBuf>
where
    F: Fn(&str) -> Option<std::ffi::OsString>,
{
    for key in ["USERPROFILE", "HOME"] {
        if let Some(value) = get(key) {
            if !value.to_string_lossy().trim().is_empty() {
                return Some(PathBuf::from(value));
            }
        }
    }

    #[cfg(windows)]
    {
        if let (Some(drive), Some(path)) = (get("HOMEDRIVE"), get("HOMEPATH")) {
            if !drive.to_string_lossy().trim().is_empty()
                && !path.to_string_lossy().trim().is_empty()
            {
                return Some(PathBuf::from(format!(
                    "{}{}",
                    drive.to_string_lossy(),
                    path.to_string_lossy()
                )));
            }
        }

        for key in ["APPDATA", "LOCALAPPDATA"] {
            if let Some(value) = get(key) {
                if let Some(parent) = Path::new(&value).parent().and_then(Path::parent) {
                    return Some(parent.to_path_buf());
                }
            }
        }
    }

    None
}

fn resolve_anchor(spec: &str) -> Result<String, String> {
    let Some((anchor, relative)) = spec.split_once(':') else {
        return Err(format!("anchored_allowed_root_requires_anchor:{spec}"));
    };
    if relative.is_empty() || Path::new(relative).is_absolute() {
        return Err(format!(
            "anchored_allowed_root_path_must_be_relative:{spec}"
        ));
    }
    let base = match anchor {
        "user_home" => {
            user_home_anchor().ok_or_else(|| "user_home_anchor_unavailable".to_string())?
        }
        _ => return Err(format!("anchored_allowed_root_unknown_anchor:{anchor}")),
    };
    Ok(base.join(relative).to_string_lossy().to_string())
}

fn handle_request(state: &mut State, request: &Value) -> Option<Value> {
    let method = request
        .get("method")
        .and_then(Value::as_str)
        .unwrap_or_default();
    request.get("id")?;
    let id = request.get("id").cloned().unwrap_or(Value::Null);
    let result = match method {
        "initialize" => Ok(initialize(request, &state.mode)),
        "tools/list" => Ok(json!({"tools": list_tools(&state.mode)})),
        "tools/call" => call_tool(state, request.get("params").unwrap_or(&Value::Null)),
        "resources/list" => Ok(json!({"resources": []})),
        "resources/read" => Err(FsError::new(
            "resource_not_found",
            "resource_not_found",
            json!({}),
        )),
        "prompts/list" => Ok(
            json!({"prompts": [{"name": "local_filesystem_tool_usage", "title": "Local Filesystem Tool Usage", "description": format!("Guidance for using local-filesystem-{} tools safely.", state.mode), "arguments": []}]}),
        ),
        "prompts/get" => prompt_get(state, request.get("params").unwrap_or(&Value::Null)),
        "completion/complete" => Ok(completion(
            state,
            request.get("params").unwrap_or(&Value::Null),
        )),
        "logging/setLevel" => Ok(json!({})),
        _ => Err(FsError::new(
            "unsupported_mcp_method",
            format!("unsupported_mcp_method: {method}"),
            json!({"method": method}),
        )),
    };
    Some(match result {
        Ok(value) => json!({"jsonrpc": "2.0", "id": id, "result": value}),
        Err(error) => {
            json!({"jsonrpc": "2.0", "id": id, "error": {"code": -32000, "message": error.message, "data": diagnostic(&error)}})
        }
    })
}

fn initialize(_request: &Value, mode: &str) -> Value {
    json!({
        "protocolVersion": PROTOCOL_VERSION,
        "capabilities": {"tools": {}, "resources": {}, "prompts": {}, "completions": {}, "logging": {}},
        "serverInfo": {"name": format!("local-filesystem-{mode}-native"), "version": "0.1.0"}
    })
}

fn prompt_get(state: &State, params: &Value) -> Result<Value, FsError> {
    let name = params
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if name != "local_filesystem_tool_usage" {
        return Err(FsError::new(
            "unknown_prompt",
            format!("unknown_prompt: {name}"),
            json!({"name": name}),
        ));
    }
    Ok(
        json!({"description": format!("Guidance for using local-filesystem-{} tools safely.", state.mode), "messages": [{"role": "user", "content": {"type": "text", "text": format!("Use local-filesystem-{} tools only within allowed roots. Prefer read/search tools before mutation and preserve structuredContent as authoritative.", state.mode)}}]}),
    )
}

fn completion(state: &State, params: &Value) -> Value {
    let prefix = params
        .pointer("/argument/value")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_ascii_lowercase();
    let values: Vec<String> = state
        .allowed_roots
        .iter()
        .map(|path| normalize_path(path))
        .filter(|value| value.to_ascii_lowercase().starts_with(&prefix))
        .take(100)
        .collect();
    json!({"completion":{"total":values.len(),"hasMore":false,"values":values}})
}

fn call_tool(state: &mut State, params: &Value) -> Result<Value, FsError> {
    let name = params.get("name").and_then(Value::as_str).ok_or_else(|| {
        FsError::new(
            "tools_call_requires_name",
            "tools_call_requires_name",
            json!({}),
        )
    })?;
    let raw_args = params.get("arguments").unwrap_or(&Value::Null);
    validate_tool_arguments(&state.mode, name, raw_args)?;
    let (resolved_args, payload_source) = if name == "fs_write_file" {
        resolve_write_payload(state, raw_args)?
    } else {
        (raw_args.clone(), None)
    };
    let args = &resolved_args;
    validate_tool_arguments(&state.mode, name, args)?;
    if is_write_tool(name) && state.mode != "write" {
        return Err(FsError::new(
            format!("tool_not_available_in_{}_mode", state.mode),
            format!("tool_not_available_in_{}_mode: {name}", state.mode),
            json!({"tool_name": name, "mode": state.mode}),
        ));
    }
    let mut value = match name {
        "fs_guidance" => guidance(state, args),
        "fs_read_file" => read_file(state, args, false),
        "fs_read_file_range" => read_file(state, args, true),
        "fs_stat" => stat_tool(state, args),
        "fs_glob_search" => search_tool(state, args, false),
        "fs_grep_search" => search_tool(state, args, true),
        "fs_repository_inventory" => repository_inventory(state, args),
        "fs_file_metrics" => file_metrics(state, args),
        "fs_doctor" => Ok(doctor(state)),
        "fs_patch_outcome_show" => patch_outcome(state, args),
        "fs_write_file" => write_file(state, args),
        "fs_str_replace_file" => str_replace_file(state, args),
        "fs_replace_range" => replace_range(state, args),
        "fs_apply_patch" => apply_patch_tool(state, args),
        "fs_move_path" => move_path(state, args, false),
        "fs_create_directory" => create_directory(state, args),
        "fs_rename_directory" => move_path(state, args, true),
        "fs_delete_directory" => delete_directory(state, args),
        _ => Err(FsError::new(
            format!("tool_not_available_in_{}_mode", state.mode),
            format!("tool_not_available_in_{}_mode: {name}", state.mode),
            json!({"tool_name": name, "mode": state.mode}),
        )),
    }?;
    if is_write_tool(name) {
        state.cache.clear();
        state.snapshots.clear();
        state.snapshot_order.clear();
    }
    if let (Some(source), Some(object)) = (payload_source, value.as_object_mut()) {
        object.insert("payload_source".into(), source);
    }
    Ok(tool_result(value))
}

fn resolve_write_payload(state: &State, args: &Value) -> Result<(Value, Option<Value>), FsError> {
    let object = args.as_object().ok_or_else(|| {
        FsError::new(
            "tool_arguments_must_be_object",
            "tool_arguments_must_be_object",
            json!({}),
        )
    })?;
    let path = object
        .get("payload_path")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty());
    let reference = object
        .get("payload_ref")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty());
    if path.is_some() && reference.is_some() {
        return Err(FsError::new(
            "payload_transport_must_choose_one_of_payload_path_or_payload_ref",
            "payload_transport_must_choose_one_of_payload_path_or_payload_ref",
            json!({}),
        ));
    }
    let Some((candidate, source_kind)) = (if let Some(value) = path {
        Some((state.output_root.join(value), "file"))
    } else if let Some(value) = reference {
        let Some(rest) = value.strip_prefix("mcp_payload:") else {
            return Err(FsError::new(
                "payload_ref_invalid",
                "payload_ref_invalid",
                json!({"payload_ref":value}),
            ));
        };
        let Some((id, revision)) = rest.split_once("@v") else {
            return Err(FsError::new(
                "payload_ref_invalid",
                "payload_ref_invalid",
                json!({"payload_ref":value}),
            ));
        };
        if id.len() < 3
            || id.len() > 64
            || !id
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
            || revision
                .parse::<u64>()
                .ok()
                .filter(|value| *value > 0)
                .is_none()
        {
            return Err(FsError::new(
                "payload_ref_invalid",
                "payload_ref_invalid",
                json!({"payload_ref":value}),
            ));
        }
        Some((
            state
                .output_root
                .join(".ai/tmp/mcp-payloads/workspace")
                .join(id)
                .join(format!("v{revision}.json")),
            "ref",
        ))
    } else {
        None
    }) else {
        return Ok((args.clone(), None));
    };
    let allowed = canonicalize_with_missing(&state.output_root.join(".ai/tmp/mcp-payloads"));
    let resolved = canonicalize_with_missing(&candidate);
    if !within(&allowed, &resolved) {
        return Err(FsError::new(
            "payload_path_outside_allowed_staging",
            "payload_path_outside_allowed_staging",
            json!({"path":candidate}),
        ));
    }
    let metadata = fs::metadata(&resolved).map_err(|_| {
        FsError::new(
            "payload_path_not_found",
            "payload_path_not_found",
            json!({"path":candidate}),
        )
    })?;
    if !metadata.is_file() {
        return Err(FsError::new(
            "payload_path_not_file",
            "payload_path_not_file",
            json!({"path":candidate}),
        ));
    }
    if metadata.len() > 5 * 1024 * 1024 {
        return Err(FsError::new(
            "payload_path_too_large",
            "payload_path_too_large",
            json!({"size":metadata.len(),"max_bytes":5*1024*1024}),
        ));
    }
    let bytes = fs::read(&resolved).map_err(|e| {
        FsError::new(
            "payload_path_read_failed",
            format!("payload_path_read_failed: {e}"),
            json!({"path":candidate}),
        )
    })?;
    let record: Value = serde_json::from_slice(&bytes).map_err(|e| {
        FsError::new(
            "payload_path_invalid_json",
            format!("payload_path_invalid_json: {e}"),
            json!({"path":candidate}),
        )
    })?;
    let payload = if source_kind == "ref" {
        record.get("payload").cloned().ok_or_else(|| {
            FsError::new(
                "payload_ref_payload_missing",
                "payload_ref_payload_missing",
                json!({"path":candidate}),
            )
        })?
    } else {
        record
    };
    if !payload.is_object() {
        return Err(FsError::new(
            "payload_path_json_must_be_object",
            "payload_path_json_must_be_object",
            json!({"path":candidate}),
        ));
    }
    let source = json!({"kind":source_kind,"path":relative_path(&state.output_root,&resolved),"byte_size":metadata.len(),"max_bytes":5*1024*1024,"sha256":sha256_bytes(&bytes),"transient_not_authority":true});
    Ok((payload, Some(source)))
}

fn is_write_tool(name: &str) -> bool {
    matches!(
        name,
        "fs_write_file"
            | "fs_str_replace_file"
            | "fs_replace_range"
            | "fs_apply_patch"
            | "fs_move_path"
            | "fs_create_directory"
            | "fs_rename_directory"
            | "fs_delete_directory"
    )
}

fn tool_result(value: Value) -> Value {
    let text = if value.get("content").and_then(Value::as_str).is_some() {
        serde_json::to_string_pretty(&value)
    } else {
        serde_json::to_string(&json!({
            "schema": value.get("schema"),
            "status": value.get("status"),
            "path": value.get("path"),
            "count": value.get("count"),
            "returned": value.get("returned")
        }))
    }
    .unwrap_or_else(|_| "{}".to_string());
    json!({"content": [{"type": "text", "text": text, "annotations": {"audience": ["assistant"]}}], "structuredContent": value})
}

fn diagnostic(error: &FsError) -> Value {
    json!({"schema": "local.filesystem.error.v1", "code": error.code, "message": error.message, "details": add_diagnostic_details(error.details.clone())})
}

fn add_diagnostic_details(value: Value) -> Value {
    let mut details = value.as_object().cloned().unwrap_or_default();
    details.insert(
        "diagnostic_owner".to_string(),
        json!("local-filesystem-mcp"),
    );
    details.insert(
        "diagnostic_rule".to_string(),
        json!("surface_policy_or_tool_validation"),
    );
    details.insert("false_positive_route".to_string(), json!("Submit surface feedback with surface_id=local-filesystem, the refusal code, requested_path, and why the path classification is wrong. Do not include secret content."));
    Value::Object(details)
}

fn guidance(state: &State, args: &Value) -> Result<Value, FsError> {
    let apply_patch_available = list_tools(&state.mode)
        .iter()
        .any(|tool| tool.get("name").and_then(Value::as_str) == Some("fs_apply_patch"));
    Ok(json!({
        "schema": "narada.mcp_surface.guidance.v0",
        "status": "ok",
        "surface_id": "local-filesystem",
        "guidance_tool": "fs_guidance",
        "purpose": "Governed filesystem inspection and mutation under allowed roots.",
        "requested": {"workflow": args.get("workflow"), "tool": args.get("tool")},
        "path_resolution": {
            "base": "The first allowed root returned by fs_doctor.allowed_roots.",
            "relative_paths": "Resolve relative filesystem paths against the first allowed root.",
            "absolute_paths": "Prefer absolute paths when multiple roots are allowed.",
            "git_boundary": "Use git-mcp for authoritative tracked and ignored state."
        },
        "patch_recovery": {
            "apply_patch_available": apply_patch_available,
            "sequence": if apply_patch_available {
                json!(["Choose a stable operation_id.", "Call fs_apply_patch once.", "After timeout call fs_patch_outcome_show.", "Retry only when retry_safe is true."])
            } else {
                json!(["Use fs_patch_outcome_show only to inspect an operation_id produced by another compatible filesystem surface."])
            },
            "statuses": {"failed_before_mutation": "Parsing, validation, or planning failed and no mutation started."},
            "read_mode": "fs_patch_outcome_show is available in read mode."
        },
        "repository_inventory": {
            "sequence": ["Call fs_repository_inventory with an explicit directory, pattern, limit, and cache policy.", "Use candidate_source_paths and generated_artifact_paths.", "Set include_generated only for an explicit investigation.", "Call git_changed_summary for authoritative tracked and ignored state."],
            "default_behavior": "Known generated runtime/artifact patterns are excluded unless include_generated is true."
        },
        "file_metrics": {
            "sequence": ["Call fs_file_metrics with an explicit directory, pattern, limit, and cache policy.", "Use the files table for path, line_count, byte_count, and file_type.", "Use offset and next_offset to page larger trees."],
            "semantics": {"line_count": "Exact within the configured byte and scan budgets.", "byte_count": "Filesystem byte size from stat metadata.", "scope": "The response declares the allowed root and selected directory."}
        },
        "range_reads": {
            "page_size_lines": MAX_READ_LINES,
            "behavior": "A logical range larger than one page succeeds with a bounded first page.",
            "sequence": ["Call fs_read_file_range with the complete logical start_line and end_line.", "When has_more is true, call the same tool with continuation.arguments.", "Do not switch to a native filesystem or shell reader to bypass pagination."]
        },
        "first_use": ["Call fs_doctor before discovery.", "Use bounded reads and searches.", "Preserve structuredContent as authoritative evidence."]
    }))
}

fn doctor(state: &State) -> Value {
    let read_tools = vec![
        "fs_guidance",
        "fs_read_file",
        "fs_read_file_range",
        "fs_stat",
        "fs_glob_search",
        "fs_grep_search",
        "fs_repository_inventory",
        "fs_file_metrics",
        "fs_doctor",
        "fs_patch_outcome_show",
    ];
    let write_tools: Vec<&str> = vec![
        "fs_write_file",
        "fs_str_replace_file",
        "fs_replace_range",
        "fs_apply_patch",
        "fs_move_path",
        "fs_create_directory",
        "fs_rename_directory",
        "fs_delete_directory",
    ];
    let available_tools: Vec<String> = list_tools(&state.mode)
        .iter()
        .filter_map(|tool| tool.get("name").and_then(Value::as_str).map(str::to_string))
        .collect();
    let can_write = state.mode == "write";
    json!({
        "schema": "local.filesystem.doctor.v1",
        "status": "ok",
        "mode": state.mode,
        "allowed_roots": state.allowed_roots.iter().map(|path| path.to_string_lossy().to_string()).collect::<Vec<_>>(),
        "allowed_root_entries": state.root_entries,
        "relative_path_resolution": {
            "base": state.allowed_roots.first().map(|path| path.to_string_lossy().to_string()),
            "rule": "first_allowed_root",
            "relative_paths": "Resolve relative filesystem paths against base; the process current directory is not used.",
            "absolute_paths": "Resolve absolute paths as given, then enforce containment under an allowed root.",
            "recommendation": "Pass an absolute path when multiple roots are active or when the target root matters."
        },
        "output_root": state.output_root.to_string_lossy(),
        "audit_log_dir": state.audit_log_dir.as_ref().map(|path| path.to_string_lossy().to_string()),
        "client_roots": {"supported": false, "roots": [], "lastUpdatedAt": Value::Null},
        "effective_permissions": {"can_read": true, "can_write": can_write, "can_mutate_paths": can_write, "can_delete_directories": can_write,"can_write_patch_recovery_records":true},
        "available_tools": available_tools,
        "read_tools": read_tools,
        "recovery_tools":["fs_patch_outcome_show"],
        "write_tools": write_tools,
        "default_glob_ignore_patterns": DEFAULT_GLOB_IGNORES,
        "default_grep_ignore_patterns": DEFAULT_GREP_IGNORES
    })
}

fn read_file(state: &State, args: &Value, range: bool) -> Result<Value, FsError> {
    let (path, root) = resolve_allowed(
        state,
        args.get("path").and_then(Value::as_str),
        if range {
            "fs_read_file_range"
        } else {
            "fs_read_file"
        },
    )?;
    let (offset, requested_limit, limit, requested_end_line) = if range {
        let start = integer(args, "start_line").ok_or_else(|| {
            FsError::new(
                "start_line_must_be_positive_integer",
                "start_line_must_be_positive_integer",
                json!({}),
            )
        })?;
        let end = integer(args, "end_line").ok_or_else(|| {
            FsError::new(
                "end_line_must_be_greater_than_or_equal_start_line",
                "end_line_must_be_greater_than_or_equal_start_line",
                json!({}),
            )
        })?;
        if start < 1 {
            return Err(FsError::new(
                "start_line_must_be_positive_integer",
                "start_line_must_be_positive_integer",
                json!({"start_line": start}),
            ));
        }
        if end < start {
            return Err(FsError::new(
                "end_line_must_be_greater_than_or_equal_start_line",
                "end_line_must_be_greater_than_or_equal_start_line",
                json!({"start_line": start, "end_line": end}),
            ));
        }
        let requested = end - start + 1;
        (start, requested, requested.min(MAX_READ_LINES), Some(end))
    } else {
        let requested = integer(args, "limit").unwrap_or(400).max(1);
        if requested > MAX_READ_LINES {
            return Err(FsError::new(
                "fs_read_file_limit_exceeds_max",
                "fs_read_file_limit_exceeds_max",
                json!({"offset": integer(args, "offset").unwrap_or(1).max(1), "requested_limit": requested, "max_limit": MAX_READ_LINES, "pagination_required": true, "mutation_started": false}),
            ));
        }
        (
            integer(args, "offset").unwrap_or(1).max(1),
            requested,
            requested,
            None,
        )
    };
    let timeout = integer(args, "timeout_ms")
        .unwrap_or(READ_TIMEOUT_MS as i64)
        .clamp(1, 60_000) as u64;
    let window = stream_text_window(
        &path,
        &root,
        offset as usize,
        limit as usize,
        timeout,
        if range {
            "fs_read_file_range"
        } else {
            "fs_read_file"
        },
    )?;
    let content = window.selected.join("\n");
    let next_offset = if let Some(requested_end) = requested_end_line {
        window.next_offset.filter(|next| *next <= requested_end)
    } else {
        window.next_offset
    };
    let continuation = if range {
        next_offset
            .map(|next| {
                json!({
                    "tool": "fs_read_file_range",
                    "arguments": {
                        "path": path,
                        "start_line": next,
                        "end_line": requested_end_line,
                        "timeout_ms": timeout
                    }
                })
            })
            .unwrap_or(Value::Null)
    } else {
        Value::Null
    };
    let served_end_line = if window.selected.is_empty() {
        Value::Null
    } else {
        json!(offset + window.selected.len() as i64 - 1)
    };
    let (total_lines, total_lines_exact, line_window_complete) = if window.complete {
        (json!(window.total_lines), true, true)
    } else {
        (Value::Null, false, false)
    };
    Ok(json!({
        "schema": "local.filesystem.read.v1",
        "path": path,
        "root": root,
        "relative_path": relative_path(&root, &path),
        "total_lines": total_lines,
        "total_lines_exact": total_lines_exact,
        "total_lines_status": if total_lines_exact { "exact" } else { "unknown_after_window" },
        "line_window_complete": line_window_complete,
        "offset": offset,
        "limit": limit,
        "requested_limit": requested_limit,
        "requested_start_line": if range { json!(offset) } else { Value::Null },
        "requested_end_line": requested_end_line,
        "served_end_line": served_end_line,
        "returned_lines": window.selected.len(),
        "next_offset": next_offset,
        "next_start_line": if range { next_offset.map_or(Value::Null, Value::from) } else { Value::Null },
        "continuation": continuation,
        "content": content,
        "content_sha256": window.sha256,
        "content_hash_scope": "full_file",
        "hash_source": "live_file_bytes",
        "cache_used": false,
        "content_window_sha256": sha256_bytes(content.as_bytes()),
        "max_limit": MAX_READ_LINES,
        "limit_adjusted": limit != requested_limit,
        "pagination_required": next_offset.is_some(),
        "has_more": next_offset.is_some(),
        "requested_range_complete": if range { next_offset.is_none() } else { true },
        "timeout_ms": timeout
    }))
}

struct TextWindow {
    selected: Vec<String>,
    next_offset: Option<i64>,
    total_lines: usize,
    complete: bool,
    sha256: String,
}

fn stream_text_window(
    path: &Path,
    root: &Path,
    offset: usize,
    limit: usize,
    timeout_ms: u64,
    operation: &str,
) -> Result<TextWindow, FsError> {
    let mut file = fs::File::open(path).map_err(|error| {
        FsError::new(
            format!("{operation}_failed"),
            format!("{operation}_failed: {error}"),
            path_details(path, root),
        )
    })?;
    let started = std::time::Instant::now();
    let mut digest = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    let mut pending = Vec::new();
    let mut selected = Vec::new();
    let mut retained = 0_usize;
    let mut line_number = 0_usize;
    let mut bounded = false;
    let mut next_offset = None;
    loop {
        if started.elapsed().as_millis() as u64 > timeout_ms {
            return Err(FsError::new(
                format!("{operation}_timed_out"),
                format!("{operation}_timed_out"),
                json!({"timeout_ms":timeout_ms,"path":path,"root":root,"offset":offset,"limit":limit}),
            ));
        }
        let count = file.read(&mut buffer).map_err(|error| {
            FsError::new(
                format!("{operation}_failed"),
                format!("{operation}_failed: {error}"),
                path_details(path, root),
            )
        })?;
        if count == 0 {
            break;
        }
        let chunk = &buffer[..count];
        if chunk.contains(&0) {
            return Err(FsError::new(
                "binary_file_not_supported",
                format!("binary_file_not_supported: {}", path.display()),
                path_details(path, root),
            ));
        }
        digest.update(chunk);
        if bounded {
            continue;
        }
        for byte in chunk {
            if selected.len() >= limit
                && line_number >= offset.saturating_add(limit).saturating_sub(1)
            {
                next_offset = Some((line_number + 1) as i64);
                bounded = true;
                break;
            }
            if *byte == b'\n' {
                line_number += 1;
                let line = if pending.last() == Some(&b'\r') {
                    &pending[..pending.len() - 1]
                } else {
                    pending.as_slice()
                };
                if line_number >= offset && selected.len() < limit {
                    retained = retained.saturating_add(line.len());
                    if retained > MAX_READ_WINDOW_BYTES {
                        return Err(FsError::new(
                            "fs_read_window_too_large",
                            "fs_read_window_too_large",
                            json!({"path":path,"max_window_bytes":MAX_READ_WINDOW_BYTES,"line":line_number}),
                        ));
                    }
                    selected.push(String::from_utf8(line.to_vec()).map_err(|_| {
                        FsError::new(
                            "text_file_not_utf8",
                            "text_file_not_utf8",
                            path_details(path, root),
                        )
                    })?);
                } else if line_number >= offset.saturating_add(limit) {
                    next_offset = Some(line_number as i64);
                    bounded = true;
                }
                pending.clear();
                if bounded {
                    break;
                }
            } else {
                pending.push(*byte);
                if pending.len() > MAX_READ_LINE_BYTES {
                    return Err(FsError::new(
                        "fs_read_line_too_large",
                        "fs_read_line_too_large",
                        json!({"path":path,"max_line_bytes":MAX_READ_LINE_BYTES,"line":line_number+1}),
                    ));
                }
            }
        }
    }
    if !bounded && !pending.is_empty() {
        line_number += 1;
        let line = if pending.last() == Some(&b'\r') {
            &pending[..pending.len() - 1]
        } else {
            pending.as_slice()
        };
        if line_number >= offset && selected.len() < limit {
            retained = retained.saturating_add(line.len());
            if retained > MAX_READ_WINDOW_BYTES {
                return Err(FsError::new(
                    "fs_read_window_too_large",
                    "fs_read_window_too_large",
                    json!({"path":path,"max_window_bytes":MAX_READ_WINDOW_BYTES,"line":line_number}),
                ));
            }
            selected.push(String::from_utf8(line.to_vec()).map_err(|_| {
                FsError::new(
                    "text_file_not_utf8",
                    "text_file_not_utf8",
                    path_details(path, root),
                )
            })?);
        } else if line_number >= offset.saturating_add(limit) {
            next_offset = Some(line_number as i64);
            bounded = true;
        }
    }
    Ok(TextWindow {
        selected,
        next_offset,
        total_lines: line_number,
        complete: !bounded,
        sha256: hex::encode(digest.finalize()),
    })
}

fn write_file(state: &State, args: &Value) -> Result<Value, FsError> {
    let (path, root) = resolve_allowed(
        state,
        args.get("path").and_then(Value::as_str),
        "fs_write_file",
    )?;
    assert_mutation_target_allowed(&path, &root, "fs_write_file")?;
    let content = args
        .get("content")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let overwrite = args
        .get("overwrite")
        .and_then(Value::as_bool)
        .unwrap_or(true);
    let create_only = args
        .get("create_only")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let create_parent_directories = args
        .get("create_parent_directories")
        .and_then(Value::as_bool)
        .unwrap_or(true);
    let timeout_ms = integer(args, "timeout_ms")
        .unwrap_or(WRITE_TIMEOUT_MS as i64)
        .clamp(1, 300_000) as u64;
    let started = std::time::Instant::now();

    let before_sha256 = match fs::metadata(&path) {
        Ok(metadata) if metadata.is_file() => Some(sha256_file_with_timeout(
            &path,
            timeout_ms,
            "fs_write_file",
        )?),
        Ok(metadata) if metadata.is_dir() => {
            return Err(FsError::new(
                "fs_write_file_destination_is_directory",
                "fs_write_file_destination_is_directory",
                path_details(&path, &root),
            ));
        }
        Ok(_) => {
            return Err(FsError::new(
                "fs_write_file_destination_not_regular_file",
                "fs_write_file_destination_not_regular_file",
                path_details(&path, &root),
            ));
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => None,
        Err(error) => {
            return Err(FsError::new(
                "fs_write_file_read_failed",
                format!("fs_write_file_read_failed: {error}"),
                path_details(&path, &root),
            ));
        }
    };
    if create_only && before_sha256.is_some() {
        return Err(FsError::new(
            "write_file_destination_exists",
            "write_file_destination_exists",
            path_details(&path, &root),
        ));
    }
    if !overwrite && before_sha256.is_some() {
        return Err(FsError::new(
            "write_file_overwrite_refused",
            "write_file_overwrite_refused",
            path_details(&path, &root),
        ));
    }
    if let Some(expected) = args
        .get("expected_sha256")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
    {
        if before_sha256.as_deref() != Some(expected) {
            return Err(FsError::new(
                "fs_write_file_expected_sha256_mismatch",
                "fs_write_file_expected_sha256_mismatch",
                json!({"operation": "fs_write_file", "expected_sha256": expected, "actual_sha256": before_sha256, "path": path, "root": root, "relative_path": relative_path(&root, &path), "concurrency_diagnosis": {"reason": "file_content_changed_since_observation_or_guard_is_not_full_file_hash", "expected_hash_scope": "full_file", "actual_hash_scope": "full_file", "actual_hash_source": "live_file_bytes", "cache_used": false, "attribution": "external_or_unobserved_writer_unless_a_matching_filesystem_audit_record_exists"}, "remediation": "Re-read the full-file content_sha256, reconcile the concurrent change, and retry with that live hash."}),
            ));
        }
    }

    let parent = path.parent().unwrap_or(root.as_path());
    if !parent.exists() {
        if !create_parent_directories {
            return Err(FsError::new(
                "write_file_parent_not_found",
                "write_file_parent_not_found",
                json!({"path": path, "root": root, "relative_path": relative_path(&root, &path), "parent": parent}),
            ));
        }
        fs::create_dir_all(parent).map_err(|error| FsError::new("fs_write_file_parent_failed", format!("fs_write_file_parent_failed: {error}"), json!({"path": path, "root": root, "relative_path": relative_path(&root, &path), "parent": parent})))?;
    }
    if started.elapsed().as_millis() as u64 > timeout_ms {
        return Err(FsError::new(
            "fs_write_file_timed_out",
            "filesystem write timed out",
            json!({"timeout_ms": timeout_ms, "path": path, "root": root, "relative_path": relative_path(&root, &path)}),
        ));
    }
    fs::write(&path, content.as_bytes()).map_err(|error| {
        FsError::new(
            "fs_write_file_failed",
            format!("fs_write_file_failed: {error}"),
            path_details(&path, &root),
        )
    })?;
    let after_sha256 = sha256_bytes(content.as_bytes());
    append_audit(
        state,
        "fs_write_file",
        &path,
        &root,
        json!({
            "size": content.len(),
            "create_parent_directories": create_parent_directories,
            "before_sha256": before_sha256,
            "after_sha256": after_sha256,
        }),
    )?;
    Ok(json!({
        "schema": "local.filesystem.write_file.v1",
        "status": "written",
        "path": path,
        "root": root,
        "relative_path": relative_path(&root, &path),
        "size": content.len(),
        "create_parent_directories": create_parent_directories,
        "before_sha256": before_sha256,
        "after_sha256": after_sha256,
        "sha256": after_sha256,
        "content_sha256": after_sha256,
        "timeout_ms": timeout_ms,
    }))
}

fn str_replace_file(state: &State, args: &Value) -> Result<Value, FsError> {
    let (path, root) = resolve_allowed(
        state,
        args.get("path").and_then(Value::as_str),
        "fs_str_replace_file",
    )?;
    assert_mutation_target_allowed(&path, &root, "fs_str_replace_file")?;
    let old = args.get("old").and_then(Value::as_str).unwrap_or_default();
    let new = args.get("new").and_then(Value::as_str).unwrap_or_default();
    if old.is_empty() {
        return Err(FsError::new(
            "str_replace_requires_old",
            "str_replace_requires_old",
            path_details(&path, &root),
        ));
    }
    let before = read_bounded_mutation_text(&path, &root, "fs_str_replace_file")?;
    let before_sha256 = sha256_bytes(before.as_bytes());
    if let Some(expected) = args
        .get("expected_sha256")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
    {
        if expected != before_sha256 {
            return Err(FsError::new(
                "fs_str_replace_file_expected_sha256_mismatch",
                "fs_str_replace_file_expected_sha256_mismatch",
                json!({"operation": "fs_str_replace_file", "expected_sha256": expected, "actual_sha256": before_sha256, "path": path, "root": root, "concurrency_diagnosis": {"reason": "file_content_changed_since_observation_or_guard_is_not_full_file_hash", "expected_hash_scope": "full_file", "actual_hash_scope": "full_file", "actual_hash_source": "live_file_bytes", "cache_used": false, "attribution": "external_or_unobserved_writer_unless_a_matching_filesystem_audit_record_exists"}, "remediation": "Re-read the full-file content_sha256, reconcile the concurrent change, and retry with that live hash."}),
            ));
        }
    }
    let occurrences = before.match_indices(old).count();
    if occurrences == 0 {
        return Err(FsError::new(
            "str_replace_not_found",
            "str_replace_not_found",
            json!({"path": path, "root": root, "old_length": old.len(), "recommended_tool": "fs_replace_range"}),
        ));
    }
    if occurrences > 1 {
        return Err(FsError::new(
            "str_replace_ambiguous",
            format!("str_replace_ambiguous: {occurrences}"),
            json!({"path": path, "root": root, "occurrences": occurrences}),
        ));
    }
    let after = before.replacen(old, new, 1);
    fs::write(&path, after.as_bytes()).map_err(|error| {
        FsError::new(
            "fs_str_replace_file_failed",
            format!("fs_str_replace_file_failed: {error}"),
            path_details(&path, &root),
        )
    })?;
    let after_sha256 = sha256_bytes(after.as_bytes());
    append_audit(
        state,
        "fs_str_replace_file",
        &path,
        &root,
        json!({"old_length": old.len(), "new_length": new.len(), "before_sha256": before_sha256, "after_sha256": after_sha256}),
    )?;
    Ok(
        json!({"schema": "local.filesystem.str_replace_file.v1", "status": "replaced", "path": path, "root": root, "relative_path": relative_path(&root, &path), "occurrences": 1, "before_sha256": before_sha256, "after_sha256": after_sha256, "sha256": after_sha256, "content_sha256": after_sha256}),
    )
}

fn replace_range(state: &State, args: &Value) -> Result<Value, FsError> {
    let start = integer(args, "start_line").ok_or_else(|| {
        FsError::new(
            "start_line_must_be_positive_integer",
            "start_line_must_be_positive_integer",
            json!({}),
        )
    })?;
    let end = integer(args, "end_line").ok_or_else(|| {
        FsError::new(
            "end_line_must_be_greater_than_or_equal_start_line",
            "end_line_must_be_greater_than_or_equal_start_line",
            json!({}),
        )
    })?;
    if start < 1 {
        return Err(FsError::new(
            "start_line_must_be_positive_integer",
            "start_line_must_be_positive_integer",
            json!({"start_line": start}),
        ));
    }
    if end < start {
        return Err(FsError::new(
            "end_line_must_be_greater_than_or_equal_start_line",
            "end_line_must_be_greater_than_or_equal_start_line",
            json!({"start_line": start, "end_line": end}),
        ));
    }
    let (path, root) = resolve_allowed(
        state,
        args.get("path").and_then(Value::as_str),
        "fs_replace_range",
    )?;
    assert_mutation_target_allowed(&path, &root, "fs_replace_range")?;
    let replacement = args
        .get("replacement")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let before = read_bounded_mutation_text(&path, &root, "fs_replace_range")?;
    let before_sha256 = sha256_bytes(before.as_bytes());
    if let Some(expected) = args
        .get("expected_sha256")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
    {
        if expected != before_sha256 {
            return Err(FsError::new(
                "fs_replace_range_expected_sha256_mismatch",
                "fs_replace_range_expected_sha256_mismatch",
                json!({"operation": "fs_replace_range", "expected_sha256": expected, "actual_sha256": before_sha256, "path": path, "root": root, "concurrency_diagnosis": {"reason": "file_content_changed_since_observation_or_guard_is_not_full_file_hash", "expected_hash_scope": "full_file", "actual_hash_scope": "full_file", "actual_hash_source": "live_file_bytes", "cache_used": false, "attribution": "external_or_unobserved_writer_unless_a_matching_filesystem_audit_record_exists"}, "remediation": "Re-read the full-file content_sha256, reconcile the concurrent change, and retry with that live hash."}),
            ));
        }
    }
    let newline = if before.contains("\r\n") {
        "\r\n"
    } else {
        "\n"
    };
    let has_trailing_newline = before.ends_with('\n');
    let body = before
        .strip_suffix('\n')
        .unwrap_or(&before)
        .strip_suffix('\r')
        .unwrap_or_else(|| before.strip_suffix('\n').unwrap_or(&before));
    let lines = if body.is_empty() {
        Vec::new()
    } else {
        body.split('\n')
            .map(|line| line.strip_suffix('\r').unwrap_or(line))
            .collect::<Vec<_>>()
    };
    if start as usize > lines.len() + 1 {
        return Err(FsError::new(
            "start_line_out_of_range",
            format!("start_line_out_of_range: {start}"),
            json!({"path": path, "root": root, "start_line": start, "total_lines": lines.len()}),
        ));
    }
    if end as usize > lines.len() {
        return Err(FsError::new(
            "end_line_out_of_range",
            format!("end_line_out_of_range: {end}"),
            json!({"path": path, "root": root, "end_line": end, "total_lines": lines.len()}),
        ));
    }
    let replacement_lines = if replacement.is_empty() {
        Vec::new()
    } else {
        replacement.split('\n').collect::<Vec<_>>()
    };
    let mut after_lines = Vec::new();
    after_lines.extend_from_slice(&lines[..(start as usize - 1)]);
    after_lines.extend_from_slice(&replacement_lines);
    after_lines.extend_from_slice(&lines[end as usize..]);
    let mut after = after_lines.join(newline);
    if has_trailing_newline {
        after.push_str(newline);
    }
    fs::write(&path, after.as_bytes()).map_err(|error| {
        FsError::new(
            "fs_replace_range_failed",
            format!("fs_replace_range_failed: {error}"),
            path_details(&path, &root),
        )
    })?;
    let after_sha256 = sha256_bytes(after.as_bytes());
    append_audit(
        state,
        "fs_replace_range",
        &path,
        &root,
        json!({"start_line": start, "end_line": end, "before_sha256": before_sha256, "after_sha256": after_sha256}),
    )?;
    Ok(
        json!({"schema": "local.filesystem.replace_range.v1", "status": "replaced_range", "path": path, "root": root, "relative_path": relative_path(&root, &path), "start_line": start, "end_line": end, "inserted_lines": replacement_lines.len(), "before_sha256": before_sha256, "after_sha256": after_sha256, "sha256": after_sha256, "content_sha256": after_sha256}),
    )
}

fn create_directory(state: &State, args: &Value) -> Result<Value, FsError> {
    let (path, root) = resolve_allowed(
        state,
        args.get("path").and_then(Value::as_str),
        "fs_create_directory",
    )?;
    assert_mutation_target_allowed(&path, &root, "fs_create_directory")?;
    let recursive = args
        .get("recursive")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if path.exists() {
        if !path.is_dir() {
            return Err(FsError::new(
                "create_directory_destination_not_directory",
                "create_directory_destination_not_directory",
                path_details(&path, &root),
            ));
        }
        append_audit(
            state,
            "fs_create_directory",
            &path,
            &root,
            json!({"recursive": recursive, "created": false}),
        )?;
        return Ok(
            json!({"schema": "local.filesystem.create_directory.v1", "status": "exists", "path": path, "root": root, "relative_path": relative_path(&root, &path), "recursive": recursive, "created": false}),
        );
    }
    let parent = path.parent().unwrap_or(root.as_path());
    if !recursive && !parent.exists() {
        return Err(FsError::new(
            "create_directory_parent_not_found",
            "create_directory_parent_not_found",
            json!({"operation": "fs_create_directory", "requested_path": path, "parent": path_details(parent, &root)}),
        ));
    }
    if recursive {
        fs::create_dir_all(&path)
    } else {
        fs::create_dir(&path)
    }
    .map_err(|error| {
        FsError::new(
            "create_directory_failed",
            format!("create_directory_failed: {error}"),
            path_details(&path, &root),
        )
    })?;
    append_audit(
        state,
        "fs_create_directory",
        &path,
        &root,
        json!({"recursive": recursive}),
    )?;
    Ok(
        json!({"schema": "local.filesystem.create_directory.v1", "status": "created", "path": path, "root": root, "relative_path": relative_path(&root, &path), "recursive": recursive, "created": true}),
    )
}

fn delete_directory(state: &State, args: &Value) -> Result<Value, FsError> {
    let (path, root) = resolve_allowed(
        state,
        args.get("path").and_then(Value::as_str),
        "fs_delete_directory",
    )?;
    assert_mutation_target_allowed(&path, &root, "fs_delete_directory")?;
    assert_not_authority_root(&path, &root, "fs_delete_directory")?;
    let recursive = args
        .get("recursive")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if !path.exists() {
        return Err(FsError::new(
            "delete_directory_not_found",
            "delete_directory_not_found",
            path_details(&path, &root),
        ));
    }
    if !path.is_dir() {
        return Err(FsError::new(
            "delete_directory_target_not_directory",
            "delete_directory_target_not_directory",
            path_details(&path, &root),
        ));
    }
    metadata_guard(
        args,
        Some("expected"),
        "expected",
        &path,
        &root,
        "fs_delete_directory",
    )?;
    let entry_count = fs::read_dir(&path)
        .map(|entries| entries.count())
        .unwrap_or(0);
    if entry_count > 0 && !recursive {
        return Err(FsError::new(
            "delete_directory_not_empty",
            "delete_directory_not_empty",
            json!({"path": path, "root": root, "entry_count": entry_count}),
        ));
    }
    let result = if recursive {
        fs::remove_dir_all(&path)
    } else {
        fs::remove_dir(&path)
    };
    result.map_err(|error| {
        FsError::new(
            "delete_directory_failed",
            format!("delete_directory_failed: {error}"),
            path_details(&path, &root),
        )
    })?;
    append_audit(
        state,
        "fs_delete_directory",
        &path,
        &root,
        json!({"recursive": recursive, "entry_count": entry_count}),
    )?;
    Ok(
        json!({"schema": "local.filesystem.delete_directory.v1", "status": "deleted", "path": path, "root": root, "relative_path": relative_path(&root, &path), "recursive": recursive}),
    )
}

fn move_path(state: &State, args: &Value, directory_only: bool) -> Result<Value, FsError> {
    let operation = if directory_only {
        "fs_rename_directory"
    } else {
        "fs_move_path"
    };
    let (from, from_root) =
        resolve_allowed(state, args.get("from").and_then(Value::as_str), operation)?;
    let (to, to_root) = resolve_allowed(state, args.get("to").and_then(Value::as_str), operation)?;
    assert_mutation_target_allowed(&to, &to_root, operation)?;
    assert_not_authority_root(&from, &from_root, operation)?;
    assert_not_authority_root(&to, &to_root, operation)?;
    if same_path(&from, &to) {
        return Err(FsError::new(
            "move_source_and_destination_same",
            "move_source_and_destination_same",
            json!({"operation": operation, "from": path_details(&from, &from_root), "to": path_details(&to, &to_root)}),
        ));
    }
    if !from.exists() {
        return Err(FsError::new(
            "move_source_not_found",
            "move_source_not_found",
            json!({"operation": operation, "from": path_details(&from, &from_root)}),
        ));
    }
    let from_is_dir = from.is_dir();
    if directory_only && !from_is_dir {
        return Err(FsError::new(
            "rename_directory_source_not_directory",
            "rename_directory_source_not_directory",
            path_details(&from, &from_root),
        ));
    }
    metadata_guard(
        args,
        Some("expected_from"),
        "expected_from",
        &from,
        &from_root,
        operation,
    )?;
    if from_is_dir && within(&from, &to) {
        return Err(FsError::new(
            "move_destination_inside_source",
            "move_destination_inside_source",
            json!({"operation": operation, "from": path_details(&from, &from_root), "to": path_details(&to, &to_root)}),
        ));
    }
    let overwrite = args
        .get("overwrite")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let destination_exists = to.exists();
    let backup = if destination_exists {
        if !overwrite {
            return Err(FsError::new(
                "move_destination_exists",
                "move_destination_exists",
                json!({"operation": operation, "to": path_details(&to, &to_root)}),
            ));
        }
        if from_is_dir != to.is_dir() {
            return Err(FsError::new(
                "move_destination_type_mismatch",
                "move_destination_type_mismatch",
                json!({"operation": operation, "to": path_details(&to, &to_root)}),
            ));
        }
        metadata_guard(
            args,
            Some("expected_to"),
            "expected_to",
            &to,
            &to_root,
            operation,
        )?;
        let candidate = backup_sibling(&to);
        fs::rename(&to, &candidate).map_err(|error| {
            FsError::new(
                "move_destination_backup_failed",
                format!("move_destination_backup_failed: {error}"),
                json!({"operation": operation, "to": to, "backup": candidate}),
            )
        })?;
        Some(candidate)
    } else {
        None
    };
    if let Some(parent) = to.parent() {
        fs::create_dir_all(parent).map_err(|error| {
            FsError::new(
                "move_destination_parent_failed",
                format!("move_destination_parent_failed: {error}"),
                json!({"operation": operation, "parent": parent}),
            )
        })?;
    }
    if let Err(error) = fs::rename(&from, &to) {
        if let Some(backup_path) = backup.as_ref() {
            let _ = fs::rename(backup_path, &to);
        }
        return Err(FsError::new(
            "move_path_failed",
            format!("move_path_failed: {error}"),
            json!({"operation": operation, "from": from, "to": to}),
        ));
    }
    if let Some(backup_path) = backup.as_ref() {
        let _ = if from_is_dir {
            fs::remove_dir_all(backup_path)
        } else {
            fs::remove_file(backup_path)
        };
    }
    append_audit(
        state,
        operation,
        &to,
        &to_root,
        json!({"from": from, "from_root": from_root, "to": to, "to_root": to_root, "overwrite": overwrite}),
    )?;
    Ok(
        json!({"schema": if directory_only { "local.filesystem.rename_directory.v1" } else { "local.filesystem.move_path.v1" }, "status": "moved", "from": path_details(&from, &from_root), "to": path_details(&to, &to_root), "overwrite": overwrite}),
    )
}

fn metadata_guard(
    args: &Value,
    object_key: Option<&str>,
    prefix: &str,
    path: &Path,
    root: &Path,
    operation: &str,
) -> Result<(), FsError> {
    let object = object_key
        .and_then(|key| args.get(key))
        .and_then(Value::as_object);
    let value = |name: &str| {
        object
            .and_then(|entry| entry.get(name))
            .or_else(|| args.get(format!("{prefix}_{name}")))
    };
    let expected_size = value("size").and_then(Value::as_u64);
    let expected_mtime = value("mtime").and_then(Value::as_str);
    let expected_sha = value("sha256").and_then(Value::as_str);
    let expected_tree = value("tree_sha256").and_then(Value::as_str);
    let expected_entries = value("entry_count").and_then(Value::as_u64);
    if expected_mtime.is_none()
        && expected_size.is_none()
        && expected_sha.is_none()
        && expected_tree.is_none()
        && expected_entries.is_none()
    {
        return Ok(());
    }
    let metadata = fs::metadata(path).map_err(|error| {
        FsError::new(
            format!("{operation}_expected_metadata_mismatch"),
            format!("{operation}_expected_metadata_mismatch: {error}"),
            path_details(path, root),
        )
    })?;
    let actual_size = metadata.len();
    let actual_mtime = mtime_iso(&metadata);
    let (actual_tree, actual_entries) = if metadata.is_dir() {
        let (entries, _tree_entries, tree, _truncated) = directory_fingerprint(path, path);
        (Some(tree), Some(entries as u64))
    } else {
        (None, None)
    };
    let details = json!({"operation": operation, "path": path, "root": root, "expected_mtime":expected_mtime,"actual_mtime":actual_mtime,"expected_size": expected_size, "actual_size": actual_size, "expected_sha256": expected_sha, "expected_tree_sha256": expected_tree, "actual_tree_sha256": actual_tree, "expected_entry_count": expected_entries, "actual_entry_count": actual_entries});
    if expected_mtime.is_some_and(|expected| expected != actual_mtime)
        || expected_size.is_some_and(|expected| expected != actual_size)
        || expected_entries.is_some_and(|expected| Some(expected) != actual_entries)
        || expected_tree.is_some_and(|expected| Some(expected) != actual_tree.as_deref())
    {
        return Err(FsError::new(
            format!("{operation}_expected_metadata_mismatch"),
            format!("{operation}_expected_metadata_mismatch: {}", path.display()),
            details,
        ));
    }
    if let Some(expected) = expected_sha {
        if !metadata.is_file() {
            return Err(FsError::new(
                format!("{operation}_expected_sha256_not_supported"),
                format!(
                    "{operation}_expected_sha256_not_supported: {}",
                    path.display()
                ),
                details,
            ));
        }
        let actual = sha256_file_with_timeout(path, 60_000, operation)?;
        if actual != expected {
            return Err(FsError::new(
                format!("{operation}_expected_metadata_mismatch"),
                format!("{operation}_expected_metadata_mismatch: {}", path.display()),
                json!({"expected_sha256": expected, "actual_sha256": actual, "path": path, "root": root}),
            ));
        }
    }
    Ok(())
}

fn backup_sibling(path: &Path) -> PathBuf {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let name = path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("target");
    let stamp = OffsetDateTime::now_utc().unix_timestamp_nanos();
    let mut candidate = parent.join(format!(".{name}.overwrite-backup-{stamp}"));
    let mut index = 0_u32;
    while candidate.exists() {
        index += 1;
        candidate = parent.join(format!(".{name}.overwrite-backup-{stamp}-{index}"));
    }
    candidate
}

fn same_path(left: &Path, right: &Path) -> bool {
    normalize_path(left) == normalize_path(right)
}

fn assert_mutation_target_allowed(
    path: &Path,
    root: &Path,
    operation: &str,
) -> Result<(), FsError> {
    let normalized = normalize_path(path);
    let in_transient_directory = normalized.contains("/.ai/tmp/")
        || normalized.contains("/.ai/temp/")
        || normalized.starts_with(".ai/tmp/")
        || normalized.starts_with(".ai/temp/");
    let extension = path
        .extension()
        .and_then(|value| value.to_str())
        .map(|value| format!(".{}", value.to_ascii_lowercase()));
    if in_transient_directory
        && extension
            .as_deref()
            .is_some_and(|value| TRANSIENT_EXECUTABLE_EXTENSIONS.contains(&value))
    {
        return Err(FsError::new(
            "transient_executable_write_disallowed",
            "transient_executable_write_disallowed",
            json!({
                "operation": operation,
                "path": path,
                "root": root,
                "relative_path": relative_path(root, path),
                "refusal_reason": format!("transient_executable_write_disallowed:{}", path.display()),
                "remediation": "Do not create or edit executable wrappers/scripts under .ai/tmp or .ai/temp. Use structured_command_start or the owning MCP surface directly and preserve its execution_ref as evidence.",
            }),
        ));
    }
    Ok(())
}

fn assert_not_authority_root(path: &Path, root: &Path, operation: &str) -> Result<(), FsError> {
    if same_path(path, root) {
        return Err(FsError::new(
            "filesystem_authority_root_mutation_refused",
            "filesystem_authority_root_mutation_refused",
            json!({"operation":operation,"path":path,"root":root,"remediation":"Choose a descendant path; an allowed authority root cannot itself be moved, overwritten, or deleted."}),
        ));
    }
    Ok(())
}

fn append_audit(
    state: &State,
    operation: &str,
    path: &Path,
    root: &Path,
    detail: Value,
) -> Result<(), FsError> {
    let Some(directory) = state.audit_log_dir.as_ref() else {
        return Ok(());
    };
    fs::create_dir_all(directory).map_err(|error| {
        FsError::new(
            "fs_write_file_audit_failed",
            format!("fs_write_file_audit_failed: {error}"),
            json!({"directory": directory}),
        )
    })?;
    let audit_path = directory.join("filesystem-mcp-audit.jsonl");
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&audit_path)
        .map_err(|error| {
            FsError::new(
                "fs_write_file_audit_failed",
                format!("fs_write_file_audit_failed: {error}"),
                json!({"path": audit_path}),
            )
        })?;
    let record = json!({
        "schema": "local.filesystem.audit.v1",
        "at": OffsetDateTime::now_utc().format(&Rfc3339).unwrap_or_else(|_| "unknown".to_string()),
        "operation": operation,
        "path": path,
        "root": root,
        "relative_path": relative_path(root, path),
        "detail": detail,
    });
    writeln!(
        file,
        "{}",
        serde_json::to_string(&record).unwrap_or_else(|_| "{}".to_string())
    )
    .map_err(|error| {
        FsError::new(
            "fs_write_file_audit_failed",
            format!("fs_write_file_audit_failed: {error}"),
            json!({"path": audit_path}),
        )
    })?;
    Ok(())
}

fn stat_tool(state: &State, args: &Value) -> Result<Value, FsError> {
    let (path, root) = resolve_allowed(state, args.get("path").and_then(Value::as_str), "fs_stat")?;
    let metadata = fs::metadata(&path).map_err(|error| {
        FsError::new(
            "fs_stat_failed",
            format!("fs_stat_failed: {error}"),
            path_details(&path, &root),
        )
    })?;
    let kind = if metadata.is_dir() {
        "directory"
    } else if metadata.is_file() {
        "file"
    } else {
        "other"
    };
    let mut value = Map::new();
    value.insert("schema".into(), json!("local.filesystem.stat.v1"));
    value.insert("path".into(), json!(path));
    value.insert("root".into(), json!(root));
    value.insert("relative_path".into(), json!(relative_path(&root, &path)));
    value.insert("type".into(), json!(kind));
    value.insert("size".into(), json!(metadata.len()));
    value.insert("mtime".into(), json!(mtime_iso(&metadata)));
    if metadata.is_file() {
        let timeout = args
            .get("timeout_ms")
            .and_then(Value::as_u64)
            .unwrap_or(60_000);
        value.insert(
            "sha256".into(),
            json!(sha256_file_with_timeout(&path, timeout, "fs_stat")?),
        );
    }
    if metadata.is_dir() {
        let (entry_count, tree_entry_count, tree_sha256, truncated) =
            directory_fingerprint(&path, &path);
        value.insert("entry_count".into(), json!(entry_count));
        value.insert("tree_entry_count".into(), json!(tree_entry_count));
        value.insert("tree_truncated".into(), json!(truncated));
        value.insert("tree_sha256".into(), json!(tree_sha256));
    }
    Ok(Value::Object(value))
}

fn sha256_file_with_timeout(
    path: &Path,
    timeout_ms: u64,
    operation: &str,
) -> Result<String, FsError> {
    let mut file = fs::File::open(path).map_err(|error| {
        FsError::new(
            format!("{operation}_read_failed"),
            format!("{operation}_read_failed: {error}"),
            json!({"path":path}),
        )
    })?;
    let started = std::time::Instant::now();
    let mut digest = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        if started.elapsed().as_millis() as u64 > timeout_ms {
            return Err(FsError::new(
                format!("{operation}_timed_out"),
                format!("{operation}_timed_out"),
                json!({"path":path,"timeout_ms":timeout_ms}),
            ));
        }
        let count = file.read(&mut buffer).map_err(|error| {
            FsError::new(
                format!("{operation}_read_failed"),
                format!("{operation}_read_failed: {error}"),
                json!({"path":path}),
            )
        })?;
        if count == 0 {
            break;
        }
        digest.update(&buffer[..count]);
    }
    Ok(hex::encode(digest.finalize()))
}

fn read_bounded_mutation_text(
    path: &Path,
    root: &Path,
    operation: &str,
) -> Result<String, FsError> {
    let metadata = fs::metadata(path).map_err(|error| {
        FsError::new(
            format!("{operation}_read_failed"),
            format!("{operation}_read_failed: {error}"),
            path_details(path, root),
        )
    })?;
    if metadata.len() > MAX_TEXT_MUTATION_BYTES {
        return Err(FsError::new(
            format!("{operation}_file_too_large"),
            format!("{operation}_file_too_large"),
            json!({"path":path,"size":metadata.len(),"max_bytes":MAX_TEXT_MUTATION_BYTES}),
        ));
    }
    fs::read_to_string(path).map_err(|error| {
        FsError::new(
            format!("{operation}_read_failed"),
            format!("{operation}_read_failed: {error}"),
            path_details(path, root),
        )
    })
}

fn search_tool(state: &mut State, args: &Value, grep: bool) -> Result<Value, FsError> {
    let operation = if grep {
        "fs_grep_search"
    } else {
        "fs_glob_search"
    };
    let scope_arg = if grep {
        args.get("path").and_then(Value::as_str).unwrap_or(".")
    } else {
        args.get("directory").and_then(Value::as_str).unwrap_or(".")
    };
    let (scope, _root) = resolve_allowed(state, Some(scope_arg), operation)?;
    let pattern = args.get("pattern").and_then(Value::as_str).ok_or_else(|| {
        FsError::new(
            if grep {
                "grep_requires_pattern"
            } else {
                "glob_requires_pattern"
            },
            if grep {
                "grep_requires_pattern"
            } else {
                "glob_requires_pattern"
            },
            json!({}),
        )
    })?;
    let offset = integer(args, "offset").unwrap_or(0).max(0) as usize;
    let limit = integer(args, "limit")
        .unwrap_or(if grep { 80 } else { 100 })
        .clamp(1, 500) as usize;
    let cache_policy = args
        .get("cache_policy")
        .and_then(Value::as_str)
        .unwrap_or("auto");
    if !["auto", "snapshot", "refresh", "bypass"].contains(&cache_policy) {
        return Err(FsError::new(
            "search_cache_policy_unsupported",
            format!("search_cache_policy_unsupported: {cache_policy}"),
            json!({"cache_policy": cache_policy}),
        ));
    }
    let output_mode = args
        .get("output_mode")
        .and_then(Value::as_str)
        .unwrap_or("files_with_matches");
    if grep && !["files_with_matches", "count_matches", "content"].contains(&output_mode) {
        return Err(FsError::new(
            "grep_output_mode_unsupported",
            format!("grep_output_mode_unsupported: {output_mode}"),
            json!({"output_mode": output_mode}),
        ));
    }
    let snapshot_id = args.get("snapshot_id").and_then(Value::as_str);
    let cache_key = sha256_bytes(
        format!(
            "{}|{}|{}|{}|{}",
            grep,
            scope.to_string_lossy(),
            pattern,
            output_mode,
            args.get("ignore").cloned().unwrap_or(Value::Null)
        )
        .as_bytes(),
    );
    let mut cache_hit = false;
    let mut snapshot_reused = false;
    let mut cached_snapshot: Option<String> = None;
    let (all_matches, snapshot_complete) = if let Some(snapshot) = snapshot_id {
        let captured = state.snapshots.get(snapshot).cloned().ok_or_else(|| {
            FsError::new(
                format!("{operation}_snapshot_not_found"),
                format!("{operation}_snapshot_not_found: {snapshot}"),
                json!({"snapshot_id": snapshot}),
            )
        })?;
        cache_hit = true;
        snapshot_reused = true;
        captured
    } else if cache_policy != "bypass" && cache_policy != "refresh" {
        if let Some((id, matches, complete)) = state.cache.get(&cache_key).cloned() {
            cache_hit = true;
            cached_snapshot = Some(id);
            (matches, complete)
        } else {
            run_search_command(&scope, pattern, args, grep, output_mode, operation)?
        }
    } else {
        run_search_command(&scope, pattern, args, grep, output_mode, operation)?
    };
    let snapshot = if let Some(snapshot) = snapshot_id {
        Some(snapshot.to_string())
    } else if let Some(snapshot) = cached_snapshot {
        Some(snapshot)
    } else if cache_policy != "bypass" {
        let digest = sha256_bytes(
            format!(
                "{cache_key}\n{}\n{snapshot_complete}",
                all_matches.join("\n")
            )
            .as_bytes(),
        );
        let id = format!("s_{}", &digest[..24]);
        state.cache.insert(
            cache_key,
            (id.clone(), all_matches.clone(), snapshot_complete),
        );
        state
            .snapshots
            .insert(id.clone(), (all_matches.clone(), snapshot_complete));
        Some(id)
    } else {
        snapshot_id.map(str::to_string)
    };
    if let Some(id) = snapshot.as_deref() {
        touch_snapshot(state, id);
    }
    if !snapshot_complete && offset >= all_matches.len() {
        return Err(FsError::new(
            format!("{operation}_capture_boundary_reached"),
            format!(
                "{operation}_capture_boundary_reached: the bounded search capture is exhausted"
            ),
            json!({
                "offset": offset,
                "captured_entries": all_matches.len(),
                "snapshot_id": snapshot,
                "remediation": "Narrow the search path or pattern, then start a refreshed search."
            }),
        ));
    }
    let page: Vec<String> = all_matches
        .iter()
        .skip(offset)
        .take(limit)
        .cloned()
        .collect();
    let has_more = offset + page.len() < all_matches.len() || !snapshot_complete;
    let mut value = Map::new();
    value.insert(
        "schema".into(),
        json!(if grep {
            "local.filesystem.grep.v1"
        } else {
            "local.filesystem.glob.v1"
        }),
    );
    value.insert("status".into(), json!("ok"));
    if grep {
        value.insert("output_mode".into(), json!(output_mode));
    }
    value.insert("offset".into(), json!(offset));
    value.insert("limit".into(), json!(limit));
    value.insert("count".into(), json!(all_matches.len()));
    value.insert("count_exact".into(), json!(snapshot_complete));
    value.insert("scanned".into(), json!(all_matches.len()));
    value.insert("scanned_unit".into(), json!("matched_entries"));
    value.insert("returned".into(), json!(page.len()));
    value.insert("order".into(), json!("ripgrep_traversal"));
    value.insert("cache_hit".into(), json!(cache_hit));
    value.insert("snapshot_reused".into(), json!(snapshot_reused));
    value.insert("cache_policy".into(), json!(cache_policy));
    value.insert(
        "snapshot_id".into(),
        snapshot.clone().map(Value::String).unwrap_or(Value::Null),
    );
    value.insert(
        "requested_snapshot_id".into(),
        snapshot_id.map(|value| json!(value)).unwrap_or(Value::Null),
    );
    value.insert("snapshot_complete".into(), json!(snapshot_complete));
    value.insert(
        "cache_memory_bytes".into(),
        json!(all_matches.iter().map(|value| value.len()).sum::<usize>()),
    );
    value.insert("page_match_bytes".into(), Value::Null);
    value.insert("page_match_bytes_limit".into(), json!(512 * 1024));
    value.insert("page_matches_truncated".into(), json!(0));
    value.insert(
        "timeout_ms".into(),
        args.get("timeout_ms")
            .cloned()
            .unwrap_or(json!(SEARCH_TIMEOUT_MS)),
    );
    value.insert("freshness".into(), freshness(&scope));
    value.insert("has_more".into(), json!(has_more));
    value.insert(
        "next_offset".into(),
        if has_more {
            json!(offset + page.len())
        } else {
            Value::Null
        },
    );
    if grep {
        value.insert("matches_format".into(), json!("human"));
        value.insert(
            "matches".into(),
            json!(page
                .iter()
                .map(|line| render_grep(line, output_mode))
                .collect::<Vec<_>>()),
        );
        value.insert("match_objects_authoritative".into(), json!(true));
        value.insert(
            "match_objects".into(),
            Value::Array(
                page.iter()
                    .map(|line| grep_match_object(line, output_mode))
                    .collect(),
            ),
        );
    } else {
        value.insert("matches_format".into(), json!("path"));
        value.insert("matches".into(), json!(page));
    }
    if page.is_empty() && all_matches.is_empty() {
        value.insert("no_match_diagnostics".into(), json!({
            "status": "no_matches_observed",
            "cache_hit": cache_hit,
            "cache_policy": cache_policy,
            "snapshot_complete": true,
            "freshness": value.get("freshness").cloned().unwrap_or(Value::Null),
            "stale_cache_evidence": false,
            "remediation": "No matches were returned for the current path freshness fingerprint."
        }));
    }
    Ok(Value::Object(value))
}

fn touch_snapshot(state: &mut State, id: &str) {
    state.snapshot_order.retain(|entry| entry != id);
    state.snapshot_order.push(id.to_string());
    while state.snapshot_order.len() > 4 {
        let evicted = state.snapshot_order.remove(0);
        state.snapshots.remove(&evicted);
        state
            .cache
            .retain(|_, (snapshot, _, _)| snapshot != &evicted);
    }
}

fn run_search_command(
    scope: &Path,
    pattern: &str,
    args: &Value,
    grep: bool,
    output_mode: &str,
    operation: &str,
) -> Result<(Vec<String>, bool), FsError> {
    let mut rg_args = Vec::new();
    if grep {
        rg_args.extend(
            ["--field-match-separator", "\u{1f}", "--with-filename"]
                .iter()
                .map(|value| value.to_string()),
        );
        rg_args.push(
            match output_mode {
                "content" => "-n",
                "count_matches" => "-c",
                _ => "-l",
            }
            .to_string(),
        );
    } else {
        rg_args.extend(
            ["--files", "--hidden", "--no-ignore"]
                .iter()
                .map(|value| value.to_string()),
        );
        rg_args.push("-g".to_string());
        rg_args.push(pattern.to_string());
    }
    let ignores = if grep {
        DEFAULT_GREP_IGNORES
    } else {
        DEFAULT_GLOB_IGNORES
    };
    for ignore in ignores {
        rg_args.push("-g".to_string());
        rg_args.push(format!("!{ignore}"));
    }
    if let Some(extra) = args.get("ignore").and_then(Value::as_array) {
        for ignore in extra.iter().filter_map(Value::as_str) {
            rg_args.push("-g".to_string());
            rg_args.push(format!("!{ignore}"));
        }
    }
    if grep {
        rg_args.extend(
            ["--", pattern, &scope.to_string_lossy()]
                .iter()
                .map(|value| value.to_string()),
        );
    } else {
        rg_args.push(scope.to_string_lossy().to_string());
    }
    let matches = run_rg(
        &rg_args,
        args.get("timeout_ms")
            .and_then(Value::as_u64)
            .unwrap_or(SEARCH_TIMEOUT_MS),
        operation,
    )?;
    Ok((
        matches
            .0
            .into_iter()
            .map(|value| normalize_search_result(&value, grep))
            .collect(),
        matches.1,
    ))
}

fn normalize_search_result(value: &str, grep: bool) -> String {
    if !grep {
        return value.replace('\\', "/");
    }
    if let Some((path, remainder)) = value.split_once('\u{1f}') {
        return format!("{}\u{1f}{}", path.replace('\\', "/"), remainder);
    }
    value.replace('\\', "/")
}

fn repository_inventory(state: &mut State, args: &Value) -> Result<Value, FsError> {
    if args.get("directory").and_then(Value::as_str).is_some()
        && args.get("root").and_then(Value::as_str).is_some()
    {
        return Err(FsError::new(
            "repository_inventory_scope_ambiguous",
            "repository_inventory_scope_ambiguous",
            json!({"remediation": "Pass either directory or root, not both."}),
        ));
    }
    let include_generated = args
        .get("include_generated")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let mut cloned = args.clone();
    let object = cloned.as_object_mut().ok_or_else(|| {
        FsError::new(
            "arguments_must_be_object",
            "arguments_must_be_object",
            json!({}),
        )
    })?;
    if object.get("directory").and_then(Value::as_str).is_none() {
        if let Some(root) = object.remove("root") {
            object.insert("directory".into(), root);
        }
    }
    object.entry("pattern").or_insert(json!("**/*"));
    if !include_generated {
        let ignores = object.entry("ignore").or_insert(json!([]));
        if let Some(array) = ignores.as_array_mut() {
            array.extend(
                [
                    "**/.ai/runtime/**",
                    "**/.ai/tmp/**",
                    "**/.ai/output/**",
                    "**/.narada/runtime/**",
                    "**/.narada/tmp/**",
                    "**/.narada/local-filesystem-mcp/patch-outcomes/**",
                    "**/.tmp-tests/**",
                ]
                .iter()
                .map(|value| json!(value)),
            );
        }
    }
    let value = search_tool(state, &cloned, false)?;
    let matches = value.get("matches").cloned().unwrap_or(json!([]));
    let mut classifications = Vec::new();
    let mut candidates = Vec::new();
    let mut generated = Vec::new();
    if let Some(items) = matches.as_array() {
        for item in items.iter().filter_map(Value::as_str) {
            let classification = classify(item);
            classifications.push(json!({"path": item, "classification": classification}));
            if classification == "generated_artifact" {
                generated.push(item);
            } else {
                candidates.push(item);
            }
        }
    }
    let mut result = value.as_object().cloned().unwrap_or_default();
    result.insert(
        "schema".into(),
        json!("local.filesystem.repository_inventory.v1"),
    );
    result.insert(
        "directory".into(),
        json!(args
            .get("directory")
            .and_then(Value::as_str)
            .or_else(|| args.get("root").and_then(Value::as_str))
            .unwrap_or(".")),
    );
    result.insert(
        "pattern".into(),
        json!(args
            .get("pattern")
            .and_then(Value::as_str)
            .unwrap_or("**/*")),
    );
    result.insert("include_generated".into(), json!(include_generated));
    result.insert("classifications".into(), Value::Array(classifications));
    result.insert("candidate_source_paths".into(), json!(candidates));
    result.insert("candidate_source_count".into(), json!(candidates.len()));
    result.insert("generated_artifact_paths".into(), json!(generated));
    result.insert("generated_artifact_count".into(), json!(generated.len()));
    result.insert(
        "generated_artifacts_excluded_by_default".into(),
        json!(!include_generated),
    );
    result.insert("git_tracking_boundary".into(), json!({"tracked_paths": null, "ignored_paths": null, "authority": "git-mcp", "next_tool": "git_changed_summary", "note": "This filesystem inventory identifies bounded candidate and generated paths; Git-tracked and Git-ignored state is authoritative in git-mcp."}));
    Ok(Value::Object(result))
}

fn file_metrics(state: &mut State, args: &Value) -> Result<Value, FsError> {
    if args.get("directory").and_then(Value::as_str).is_some()
        && args.get("root").and_then(Value::as_str).is_some()
    {
        return Err(FsError::new(
            "file_metrics_directory_ambiguous",
            "file_metrics_directory_ambiguous",
            json!({"remediation": "Pass either directory or root, not both."}),
        ));
    }
    let directory_arg = args
        .get("directory")
        .and_then(Value::as_str)
        .or_else(|| args.get("root").and_then(Value::as_str))
        .unwrap_or(".");
    let (directory, root) = resolve_allowed(state, Some(directory_arg), "fs_file_metrics")?;
    let mut glob_args = args.clone();
    let object = glob_args.as_object_mut().ok_or_else(|| {
        FsError::new(
            "arguments_must_be_object",
            "arguments_must_be_object",
            json!({}),
        )
    })?;
    object.insert("directory".into(), json!(directory.to_string_lossy()));
    object.entry("pattern").or_insert(json!("**/*"));
    let mut all_matches = Vec::new();
    let mut page_offset = 0_i64;
    if let Some(object) = glob_args.as_object_mut() {
        object.insert("offset".into(), json!(page_offset));
        object.insert("limit".into(), json!(500));
    }
    let mut all = search_tool(state, &glob_args, false)?;
    loop {
        all_matches.extend(
            all.get("matches")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default(),
        );
        if all.get("has_more").and_then(Value::as_bool) != Some(true) {
            break;
        }
        page_offset = all
            .get("next_offset")
            .and_then(Value::as_i64)
            .unwrap_or(page_offset + 500);
        if all_matches.len() > 10_000 {
            break;
        }
        if let Some(object) = glob_args.as_object_mut() {
            object.insert("offset".into(), json!(page_offset));
            object.insert("limit".into(), json!(500));
        }
        all = search_tool(state, &glob_args, false)?;
    }
    let matches = all_matches;
    let metrics_snapshot_id = args
        .get("snapshot_id")
        .and_then(Value::as_str)
        .map(str::to_string)
        .or_else(|| {
            all.get("snapshot_id")
                .and_then(Value::as_str)
                .map(str::to_string)
        });
    let offset = integer(args, "offset").unwrap_or(0).max(0) as usize;
    let limit = integer(args, "limit").unwrap_or(100).clamp(1, 100) as usize;
    let max_file = integer(args, "max_bytes_per_file")
        .unwrap_or(8 * 1024 * 1024)
        .max(1) as u64;
    let max_total = integer(args, "max_total_scan_bytes")
        .unwrap_or(256 * 1024 * 1024)
        .max(1) as u64;
    let mut reserved = 0_u64;
    let mut files = Vec::new();
    for item in matches.iter().skip(offset).take(limit) {
        let Some(raw) = item.as_str() else { continue };
        let path = PathBuf::from(raw);
        let metadata = match fs::metadata(&path) {
            Ok(value) if value.is_file() => value,
            _ => continue,
        };
        let (line_count, status) = if metadata.len() > max_file {
            (Value::Null, "too_large")
        } else if reserved + metadata.len() > max_total {
            (Value::Null, "scan_budget_exceeded")
        } else {
            reserved += metadata.len();
            match count_lines(&path) {
                Ok((_count, binary)) if binary => (Value::Null, "binary"),
                Ok((count, _)) => (json!(count), "exact"),
                Err(_) => (Value::Null, "unavailable"),
            }
        };
        let relative = relative_path(&directory, &path);
        let root_relative = relative_path(&root, &path);
        files.push(json!({
            "path": path,
            "relative_path": relative,
            "root_relative_path": root_relative,
            "line_count": line_count,
            "line_count_status": status,
            "byte_count": metadata.len(),
            "file_type": if status == "binary" { "binary" } else { path.extension().and_then(|value| value.to_str()).unwrap_or("no_extension") },
            "scope_classification": classify(&root_relative),
            "mtime": mtime_iso(&metadata)
        }));
    }
    let has_more = offset + files.len() < matches.len();
    let mut result = Map::new();
    result.insert("schema".into(), json!("local.filesystem.file_metrics.v1"));
    result.insert("status".into(), json!("ok"));
    result.insert("directory".into(), json!(directory));
    result.insert(
        "pattern".into(),
        json!(args
            .get("pattern")
            .and_then(Value::as_str)
            .unwrap_or("**/*")),
    );
    result.insert("offset".into(), json!(offset));
    result.insert("limit".into(), json!(limit));
    result.insert("count".into(), json!(matches.len()));
    result.insert("count_exact".into(), json!(true));
    result.insert("returned".into(), json!(files.len()));
    result.insert("has_more".into(), json!(has_more));
    result.insert(
        "next_offset".into(),
        if has_more {
            json!(offset + files.len())
        } else {
            Value::Null
        },
    );
    result.insert("order".into(), json!("ripgrep_traversal"));
    result.insert("cache_hit".into(), json!(false));
    result.insert(
        "cache_policy".into(),
        json!(args
            .get("cache_policy")
            .and_then(Value::as_str)
            .unwrap_or("auto")),
    );
    result.insert(
        "snapshot_id".into(),
        metrics_snapshot_id
            .clone()
            .map(Value::String)
            .unwrap_or(Value::Null),
    );
    result.insert(
        "requested_snapshot_id".into(),
        args.get("snapshot_id").cloned().unwrap_or(Value::Null),
    );
    result.insert(
        "snapshot_complete".into(),
        json!(metrics_snapshot_id.is_some()),
    );
    result.insert(
        "timeout_ms".into(),
        args.get("timeout_ms").cloned().unwrap_or(json!(10_000)),
    );
    result.insert("scan_budget_bytes".into(), json!(max_total));
    result.insert("scan_bytes_reserved".into(), json!(reserved));
    result.insert("scope".into(), json!({"directory": directory, "allowed_root": root, "allowed_roots": state.allowed_roots, "include_pattern": args.get("pattern").and_then(Value::as_str).unwrap_or("**/*"), "ignore_patterns": DEFAULT_GLOB_IGNORES, "ignored_paths": [], "ignored_path_count": 0, "ignored_paths_complete": true, "ignored_paths_truncated": false, "out_of_scope_paths": [], "out_of_scope_path_count": 0, "out_of_scope_paths_complete": !has_more, "boundary": {"allowed_root": root, "directory": directory, "realpath_enforced": true}, "contents_returned": false}));
    result.insert("totals".into(), aggregate_metrics(&files));
    result.insert("totals_scope".into(), json!("returned_page"));
    result.insert("files".into(), Value::Array(files));
    Ok(Value::Object(result))
}

#[derive(Clone, Debug)]
struct ParsedPatchFile {
    old_path: Option<String>,
    new_path: Option<String>,
    move_to: Option<String>,
    delete: bool,
    hunks: Vec<ParsedPatchHunk>,
}

#[derive(Clone, Debug)]
struct ParsedPatchHunk {
    old_start: Option<usize>,
    lines: Vec<(char, String)>,
}

struct PlannedPatch {
    parsed: ParsedPatchFile,
    source: PathBuf,
    target: PathBuf,
    root: PathBuf,
    before: Option<Vec<u8>>,
    target_before: Option<Vec<u8>>,
    after: Option<Vec<u8>>,
}

fn apply_patch_tool(state: &State, args: &Value) -> Result<Value, FsError> {
    let patch = args
        .get("patch")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if patch.trim().is_empty() {
        return Err(FsError::new(
            "patch_required",
            "Patch text is required.",
            json!({}),
        ));
    }
    let operation_id = args
        .get("operation_id")
        .and_then(Value::as_str)
        .map(str::to_string)
        .unwrap_or_else(|| {
            format!(
                "patch-{}-{}",
                std::process::id(),
                OffsetDateTime::now_utc().unix_timestamp_nanos()
            )
        });
    if !valid_operation_id(&operation_id) {
        return Err(FsError::new(
            "patch_operation_id_invalid",
            "patch_operation_id_invalid",
            json!({"operation_id":operation_id}),
        ));
    }
    let patch_sha256 = sha256_bytes(patch.as_bytes());
    let mut recovery_count = 0_u64;
    if let Some(previous) = read_patch_outcome(state, &operation_id)? {
        if previous.get("patch_sha256").and_then(Value::as_str) != Some(&patch_sha256) {
            return Err(FsError::new(
                "patch_operation_id_conflict",
                "patch_operation_id_conflict",
                json!({"operation_id":operation_id,"existing_patch_sha256":previous.get("patch_sha256"),"requested_patch_sha256":patch_sha256}),
            ));
        }
        if previous.get("status").and_then(Value::as_str) == Some("interrupted_before_mutation")
            && previous.get("retry_safe").and_then(Value::as_bool) == Some(true)
        {
            recovery_count = previous
                .get("recovery_count")
                .and_then(Value::as_u64)
                .unwrap_or(0)
                + 1;
        } else {
            let mut replay = previous;
            if let Some(object) = replay.as_object_mut() {
                object.insert("operation_replayed".into(), json!(true));
            }
            return Ok(replay);
        }
    }
    let timeout_ms = integer(args, "timeout_ms")
        .unwrap_or(WRITE_TIMEOUT_MS as i64)
        .clamp(1, 300_000) as u64;
    let started = std::time::Instant::now();
    write_patch_outcome(
        state,
        &operation_id,
        &json!({
            "schema":"local.filesystem.apply_patch.outcome.v1","status":"accepted","operation_id":operation_id,
            "patch_sha256":patch_sha256,"mutation_started":false,"owner_pid":std::process::id(),"timeout_ms":timeout_ms,
            "accepted_at":now_rfc3339(),"recovery_count":recovery_count
        }),
    )?;
    let parsed = match parse_patch(patch) {
        Ok(files) if !files.is_empty() => files,
        Ok(_) => {
            return patch_failure(
                state,
                &operation_id,
                &patch_sha256,
                FsError::new(
                    "patch_contains_no_files",
                    "patch_contains_no_files",
                    json!({"expected_format":"unified_diff_or_codex_apply_patch"}),
                ),
            )
        }
        Err(error) => return patch_failure(state, &operation_id, &patch_sha256, error),
    };
    macro_rules! plan_or_fail {
        ($expression:expr) => {
            match $expression {
                Ok(value) => value,
                Err(error) => return patch_failure(state, &operation_id, &patch_sha256, error),
            }
        };
    }
    let expected = plan_or_fail!(expected_patch_hashes(args));
    let mut matched = std::collections::HashSet::new();
    let mut plans = Vec::new();
    for file in parsed {
        if started.elapsed().as_millis() as u64 > timeout_ms {
            return patch_failure(
                state,
                &operation_id,
                &patch_sha256,
                FsError::new(
                    "fs_apply_patch_timed_out",
                    "fs_apply_patch_timed_out",
                    json!({"phase":"planning","timeout_ms":timeout_ms}),
                ),
            );
        }
        let source_name = file.old_path.as_deref().or(file.new_path.as_deref());
        let source_name = plan_or_fail!(source_name.ok_or_else(|| FsError::new(
            "patch_path_required",
            "patch_path_required",
            json!({})
        )));
        let target_name = plan_or_fail!(file
            .move_to
            .as_deref()
            .or(file.new_path.as_deref())
            .or(file.old_path.as_deref())
            .ok_or_else(|| FsError::new(
                "patch_target_path_required",
                "patch_target_path_required",
                json!({})
            )));
        let (source, source_root) =
            plan_or_fail!(resolve_allowed(state, Some(source_name), "fs_apply_patch"));
        let (target, target_root) =
            plan_or_fail!(resolve_allowed(state, Some(target_name), "fs_apply_patch"));
        if source_root != target_root {
            return patch_failure(
                state,
                &operation_id,
                &patch_sha256,
                FsError::new(
                    "patch_cross_root_move_refused",
                    "patch_cross_root_move_refused",
                    json!({"source":source,"target":target}),
                ),
            );
        }
        if file.delete || !same_path(&source, &target) {
            plan_or_fail!(assert_not_authority_root(
                &source,
                &source_root,
                "fs_apply_patch"
            ));
        }
        plan_or_fail!(assert_not_authority_root(
            &target,
            &target_root,
            "fs_apply_patch"
        ));
        if !file.delete {
            plan_or_fail!(assert_mutation_target_allowed(
                &target,
                &target_root,
                "fs_apply_patch"
            ));
        }
        if source.exists()
            && fs::metadata(&source).is_ok_and(|metadata| metadata.len() > MAX_TEXT_MUTATION_BYTES)
        {
            return patch_failure(
                state,
                &operation_id,
                &patch_sha256,
                FsError::new(
                    "fs_apply_patch_source_too_large",
                    "fs_apply_patch_source_too_large",
                    json!({"path":source,"max_bytes":MAX_TEXT_MUTATION_BYTES}),
                ),
            );
        }
        if !same_path(&source, &target)
            && target.exists()
            && fs::metadata(&target).is_ok_and(|metadata| metadata.len() > MAX_TEXT_MUTATION_BYTES)
        {
            return patch_failure(
                state,
                &operation_id,
                &patch_sha256,
                FsError::new(
                    "fs_apply_patch_target_too_large",
                    "fs_apply_patch_target_too_large",
                    json!({"path":target,"max_bytes":MAX_TEXT_MUTATION_BYTES}),
                ),
            );
        }
        let before = if source.exists() {
            Some(plan_or_fail!(fs::read(&source).map_err(|error| {
                FsError::new(
                    "patch_source_read_failed",
                    format!("patch_source_read_failed: {error}"),
                    path_details(&source, &source_root),
                )
            })))
        } else {
            None
        };
        let target_before = if same_path(&source, &target) {
            before.clone()
        } else if target.exists() {
            Some(plan_or_fail!(fs::read(&target).map_err(|error| {
                FsError::new(
                    "patch_target_read_failed",
                    format!("patch_target_read_failed: {error}"),
                    path_details(&target, &target_root),
                )
            })))
        } else {
            None
        };
        if file.old_path.is_some() && before.is_none() {
            return patch_failure(
                state,
                &operation_id,
                &patch_sha256,
                FsError::new(
                    "patch_source_not_found",
                    "patch_source_not_found",
                    path_details(&source, &source_root),
                ),
            );
        }
        plan_or_fail!(match_expected_patch_hash(
            &expected,
            &mut matched,
            &file,
            &source,
            &target,
            before.as_deref(),
        ));
        let after = if file.delete {
            plan_or_fail!(apply_patch_content(
                before.as_deref().unwrap_or_default(),
                &file.hunks,
                true
            )
            .map(|_| None))
        } else {
            Some(plan_or_fail!(apply_patch_content(
                before.as_deref().unwrap_or_default(),
                &file.hunks,
                false,
            )))
        };
        plans.push(PlannedPatch {
            parsed: file,
            source,
            target,
            root: target_root,
            before,
            target_before,
            after,
        });
    }
    let unmatched: Vec<_> = expected
        .keys()
        .filter(|key| !matched.contains(*key))
        .cloned()
        .collect();
    if !unmatched.is_empty() {
        return patch_failure(
            state,
            &operation_id,
            &patch_sha256,
            FsError::new(
                "fs_apply_patch_expected_sha256_unmatched",
                "fs_apply_patch_expected_sha256_unmatched",
                json!({"unmatched_expected_sha256_keys":unmatched}),
            ),
        );
    }
    let changes: Vec<Value> = plans.iter().map(patch_change).collect();
    if args
        .get("dry_run")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        let outcome = json!({"schema":"local.filesystem.apply_patch.outcome.v1","status":"checked","operation_id":operation_id,"patch_sha256":patch_sha256,"mutation_started":false,"dry_run":true,"timeout_ms":timeout_ms,"recovery_count":recovery_count,"changed_files":changes,"finished_at":now_rfc3339()});
        write_patch_outcome(state, &operation_id, &outcome)?;
        return Ok(outcome);
    }
    let recovery_plan = json!({
        "before_state":plans.iter().flat_map(|plan| patch_states(plan, false)).collect::<Vec<_>>(),
        "after_state":plans.iter().flat_map(|plan| patch_states(plan, true)).collect::<Vec<_>>(),
        "changed_files":changes
    });
    write_patch_outcome(
        state,
        &operation_id,
        &json!({"schema":"local.filesystem.apply_patch.outcome.v1","status":"applying","operation_id":operation_id,"patch_sha256":patch_sha256,"mutation_started":true,"owner_pid":std::process::id(),"timeout_ms":timeout_ms,"recovery_count":recovery_count,"started_at":now_rfc3339(),"recovery_plan":recovery_plan}),
    )?;
    let result = apply_planned_patch(state, &plans, started, timeout_ms);
    match result {
        Ok(()) => {
            let outcome = json!({"schema":"local.filesystem.apply_patch.outcome.v1","status":"patched","operation_id":operation_id,"patch_sha256":patch_sha256,"mutation_started":true,"rollback_performed":false,"recovery_count":recovery_count,"changed_files":changes,"finished_at":now_rfc3339(),"outcome_reader":{"tool":"fs_patch_outcome_show","operation_id":operation_id}});
            write_patch_outcome(state, &operation_id, &outcome)?;
            Ok(outcome)
        }
        Err(error) => {
            rollback_planned_patch(&plans);
            let outcome = json!({"schema":"local.filesystem.apply_patch.outcome.v1","status":"failed_rolled_back","operation_id":operation_id,"patch_sha256":patch_sha256,"mutation_started":true,"rollback_performed":true,"rollback_succeeded":true,"error":diagnostic(&error),"finished_at":now_rfc3339()});
            write_patch_outcome(state, &operation_id, &outcome)?;
            Err(error)
        }
    }
}

fn parse_patch(text: &str) -> Result<Vec<ParsedPatchFile>, FsError> {
    if text
        .lines()
        .any(|line| line.trim_end() == "*** Begin Patch")
    {
        parse_codex_patch(text)
    } else {
        parse_unified_patch(text)
    }
}

fn parse_codex_patch(text: &str) -> Result<Vec<ParsedPatchFile>, FsError> {
    let lines: Vec<&str> = text.lines().collect();
    let mut files = Vec::new();
    let mut index = lines
        .iter()
        .position(|line| line.trim_end() == "*** Begin Patch")
        .ok_or_else(|| patch_parse_error("patch_begin_marker_missing", 1))?
        + 1;
    while index < lines.len() {
        let line = lines[index];
        if line == "*** End Patch" {
            return Ok(files);
        }
        let (old_path, new_path, delete) =
            if let Some(path) = line.strip_prefix("*** Update File: ") {
                (
                    Some(clean_patch_path(path)?),
                    Some(clean_patch_path(path)?),
                    false,
                )
            } else if let Some(path) = line.strip_prefix("*** Add File: ") {
                (None, Some(clean_patch_path(path)?), false)
            } else if let Some(path) = line.strip_prefix("*** Delete File: ") {
                (Some(clean_patch_path(path)?), None, true)
            } else {
                return Err(patch_parse_error("patch_file_header_invalid", index + 1));
            };
        index += 1;
        let mut move_to = None;
        if index < lines.len() {
            if let Some(path) = lines[index].strip_prefix("*** Move to: ") {
                move_to = Some(clean_patch_path(path)?);
                index += 1;
            }
        }
        let mut hunks = Vec::new();
        let mut current = ParsedPatchHunk {
            old_start: None,
            lines: Vec::new(),
        };
        while index < lines.len() && !lines[index].starts_with("*** ") {
            let item = lines[index];
            if item.starts_with("@@") {
                if !current.lines.is_empty() {
                    hunks.push(current);
                }
                current = ParsedPatchHunk {
                    old_start: parse_hunk_start(item),
                    lines: Vec::new(),
                };
            } else {
                let (kind, body) = match item.as_bytes().first().copied() {
                    Some(b'+') => ('+', &item[1..]),
                    Some(b'-') => ('-', &item[1..]),
                    Some(b' ') => (' ', &item[1..]),
                    _ => (' ', item),
                };
                current.lines.push((kind, body.to_string()));
            }
            index += 1;
        }
        if !current.lines.is_empty() {
            hunks.push(current);
        }
        files.push(ParsedPatchFile {
            old_path,
            new_path,
            move_to,
            delete,
            hunks,
        });
    }
    Err(patch_parse_error("patch_end_marker_missing", lines.len()))
}

fn parse_unified_patch(text: &str) -> Result<Vec<ParsedPatchFile>, FsError> {
    let lines: Vec<&str> = text.lines().collect();
    let mut files = Vec::new();
    let mut index = 0;
    while index < lines.len() {
        if !lines[index].starts_with("--- ") {
            index += 1;
            continue;
        }
        let old_raw = lines[index][4..].split('\t').next().unwrap_or_default();
        index += 1;
        if index >= lines.len() || !lines[index].starts_with("+++ ") {
            return Err(patch_parse_error(
                "patch_new_file_header_missing",
                index + 1,
            ));
        }
        let new_raw = lines[index][4..].split('\t').next().unwrap_or_default();
        index += 1;
        let old_path = if old_raw == "/dev/null" {
            None
        } else {
            Some(clean_patch_path(old_raw)?)
        };
        let new_path = if new_raw == "/dev/null" {
            None
        } else {
            Some(clean_patch_path(new_raw)?)
        };
        let delete = new_path.is_none();
        let mut hunks = Vec::new();
        while index < lines.len() && !lines[index].starts_with("--- ") {
            if !lines[index].starts_with("@@") {
                index += 1;
                continue;
            }
            let mut hunk = ParsedPatchHunk {
                old_start: parse_hunk_start(lines[index]),
                lines: Vec::new(),
            };
            index += 1;
            while index < lines.len()
                && !lines[index].starts_with("@@")
                && !lines[index].starts_with("--- ")
            {
                let item = lines[index];
                if item == "\\ No newline at end of file" {
                    index += 1;
                    continue;
                }
                let Some(prefix) = item.as_bytes().first().copied() else {
                    return Err(patch_parse_error("patch_hunk_line_invalid", index + 1));
                };
                if !matches!(prefix, b' ' | b'+' | b'-') {
                    return Err(patch_parse_error("patch_hunk_line_invalid", index + 1));
                }
                hunk.lines.push((prefix as char, item[1..].to_string()));
                index += 1;
            }
            hunks.push(hunk);
        }
        files.push(ParsedPatchFile {
            old_path,
            new_path,
            move_to: None,
            delete,
            hunks,
        });
    }
    Ok(files)
}

fn clean_patch_path(value: &str) -> Result<String, FsError> {
    let mut path = value.trim().replace('\\', "/");
    if path.starts_with("a/") || path.starts_with("b/") {
        path = path[2..].to_string();
    }
    if path.is_empty() || path == "/dev/null" || path.split('/').any(|part| part == "..") {
        return Err(FsError::new(
            "patch_path_invalid",
            "patch_path_invalid",
            json!({"path":value}),
        ));
    }
    Ok(path)
}

fn parse_hunk_start(header: &str) -> Option<usize> {
    let value = header
        .split_whitespace()
        .find(|part| part.starts_with('-'))?
        .trim_start_matches('-');
    value.split(',').next()?.parse().ok()
}

fn apply_patch_content(
    before: &[u8],
    hunks: &[ParsedPatchHunk],
    deleting: bool,
) -> Result<Vec<u8>, FsError> {
    let text = std::str::from_utf8(before)
        .map_err(|_| FsError::new("patch_source_not_utf8", "patch_source_not_utf8", json!({})))?;
    let newline = if text.contains("\r\n") { "\r\n" } else { "\n" };
    let trailing = text.ends_with('\n');
    let mut lines: Vec<String> = if text.is_empty() {
        Vec::new()
    } else {
        text.trim_end_matches(['\r', '\n'])
            .split(['\n'])
            .map(|line| line.trim_end_matches('\r').to_string())
            .collect()
    };
    let mut delta: isize = 0;
    let mut cursor = 0usize;
    for hunk in hunks {
        let old: Vec<&str> = hunk
            .lines
            .iter()
            .filter(|(kind, _)| *kind != '+')
            .map(|(_, line)| line.as_str())
            .collect();
        let replacement: Vec<String> = hunk
            .lines
            .iter()
            .filter(|(kind, _)| *kind != '-')
            .map(|(_, line)| line.clone())
            .collect();
        let position = if let Some(start) = hunk.old_start {
            (start.saturating_sub(1) as isize + delta).max(0) as usize
        } else {
            find_patch_context(&lines, &old, cursor).ok_or_else(|| {
                FsError::new(
                    "patch_context_not_found",
                    "patch_context_not_found",
                    json!({"context":old}),
                )
            })?
        };
        if position + old.len() > lines.len()
            || lines[position..position + old.len()]
                .iter()
                .map(String::as_str)
                .ne(old.iter().copied())
        {
            return Err(FsError::new(
                "patch_context_mismatch",
                "patch_context_mismatch",
                json!({"line":position+1,"expected":old}),
            ));
        }
        lines.splice(position..position + old.len(), replacement.clone());
        delta += replacement.len() as isize - old.len() as isize;
        cursor = position + replacement.len();
    }
    if deleting {
        return Ok(Vec::new());
    }
    let mut output = lines.join(newline);
    if trailing || (!hunks.is_empty() && before.is_empty()) {
        output.push_str(newline);
    }
    Ok(output.into_bytes())
}

fn find_patch_context(lines: &[String], context: &[&str], start: usize) -> Option<usize> {
    if context.is_empty() {
        return Some(start.min(lines.len()));
    }
    (start..=lines.len().saturating_sub(context.len())).find(|index| {
        lines[*index..*index + context.len()]
            .iter()
            .map(String::as_str)
            .eq(context.iter().copied())
    })
}

fn expected_patch_hashes(args: &Value) -> Result<HashMap<String, String>, FsError> {
    let Some(value) = args.get("expected_sha256") else {
        return Ok(HashMap::new());
    };
    let object = value.as_object().ok_or_else(|| {
        FsError::new(
            "expected_sha256_must_be_object",
            "expected_sha256_must_be_object",
            json!({}),
        )
    })?;
    let mut result = HashMap::new();
    for (key, value) in object {
        let hash = value.as_str().unwrap_or_default();
        if !valid_sha256(hash) {
            return Err(FsError::new(
                "expected_sha256_value_invalid",
                "expected_sha256_value_invalid",
                json!({"key":key}),
            ));
        }
        result.insert(key.replace('\\', "/"), hash.to_ascii_lowercase());
    }
    Ok(result)
}

fn match_expected_patch_hash(
    expected: &HashMap<String, String>,
    matched: &mut std::collections::HashSet<String>,
    file: &ParsedPatchFile,
    source: &Path,
    target: &Path,
    before: Option<&[u8]>,
) -> Result<(), FsError> {
    for key in [
        file.old_path.as_deref(),
        file.new_path.as_deref(),
        Some(normalize_path(source).as_str()),
        Some(normalize_path(target).as_str()),
    ]
    .into_iter()
    .flatten()
    {
        if let Some(want) = expected.get(key) {
            let actual = before.map(sha256_bytes);
            if actual.as_deref() != Some(want) {
                return Err(FsError::new(
                    "fs_apply_patch_expected_sha256_mismatch",
                    "fs_apply_patch_expected_sha256_mismatch",
                    json!({"key":key,"expected_sha256":want,"actual_sha256":actual}),
                ));
            }
            matched.insert(key.to_string());
        }
    }
    Ok(())
}

fn patch_change(plan: &PlannedPatch) -> Value {
    json!({"path":plan.target,"root":plan.root,"relative_path":relative_path(&plan.root,&plan.target),"operation":if plan.parsed.delete{"delete"}else if plan.parsed.old_path.is_none(){"add"}else if !same_path(&plan.source,&plan.target){"move"}else{"update"},"hunks":plan.parsed.hunks.len(),"deleted":plan.parsed.delete,"before_sha256":plan.before.as_deref().map(sha256_bytes),"after_sha256":plan.after.as_deref().map(sha256_bytes)})
}

fn patch_states(plan: &PlannedPatch, after: bool) -> Vec<Value> {
    let mut values = Vec::new();
    let content = if after {
        plan.after.as_deref()
    } else {
        plan.target_before.as_deref()
    };
    values.push(
        json!({"path":plan.target,"exists":content.is_some(),"sha256":content.map(sha256_bytes)}),
    );
    if !same_path(&plan.source, &plan.target) {
        let source_content = if after { None } else { plan.before.as_deref() };
        values.push(json!({"path":plan.source,"exists":source_content.is_some(),"sha256":source_content.map(sha256_bytes)}));
    }
    values
}

fn apply_planned_patch(
    state: &State,
    plans: &[PlannedPatch],
    started: std::time::Instant,
    timeout_ms: u64,
) -> Result<(), FsError> {
    for plan in plans {
        if started.elapsed().as_millis() as u64 > timeout_ms {
            return Err(FsError::new(
                "fs_apply_patch_timed_out",
                "fs_apply_patch_timed_out",
                json!({"phase":"mutation","timeout_ms":timeout_ms}),
            ));
        }
        if let Some(after) = &plan.after {
            if let Some(parent) = plan.target.parent() {
                fs::create_dir_all(parent).map_err(|e| {
                    FsError::new(
                        "patch_parent_create_failed",
                        format!("patch_parent_create_failed: {e}"),
                        json!({"path":parent}),
                    )
                })?;
            }
            fs::write(&plan.target, after).map_err(|e| {
                FsError::new(
                    "patch_write_failed",
                    format!("patch_write_failed: {e}"),
                    path_details(&plan.target, &plan.root),
                )
            })?;
            if !same_path(&plan.source, &plan.target) && plan.source.exists() {
                fs::remove_file(&plan.source).map_err(|e| {
                    FsError::new(
                        "patch_move_source_remove_failed",
                        format!("patch_move_source_remove_failed: {e}"),
                        path_details(&plan.source, &plan.root),
                    )
                })?;
            }
        } else {
            fs::remove_file(&plan.source).map_err(|e| {
                FsError::new(
                    "patch_delete_failed",
                    format!("patch_delete_failed: {e}"),
                    path_details(&plan.source, &plan.root),
                )
            })?;
        }
        append_audit(
            state,
            "fs_apply_patch",
            &plan.target,
            &plan.root,
            json!({"before_sha256":plan.before.as_deref().map(sha256_bytes),"after_sha256":plan.after.as_deref().map(sha256_bytes),"hunks":plan.parsed.hunks.len()}),
        )?;
    }
    Ok(())
}

fn rollback_planned_patch(plans: &[PlannedPatch]) {
    for plan in plans.iter().rev() {
        if let Some(before) = &plan.before {
            if let Some(parent) = plan.source.parent() {
                let _ = fs::create_dir_all(parent);
            }
            let _ = fs::write(&plan.source, before);
        } else if plan.source.exists() {
            let _ = fs::remove_file(&plan.source);
        }
        if !same_path(&plan.source, &plan.target) {
            if let Some(before) = &plan.target_before {
                if let Some(parent) = plan.target.parent() {
                    let _ = fs::create_dir_all(parent);
                }
                let _ = fs::write(&plan.target, before);
            } else if plan.target.exists() {
                let _ = fs::remove_file(&plan.target);
            }
        }
    }
}

fn patch_outcome_path(state: &State, operation: &str) -> PathBuf {
    state
        .output_root
        .join(".narada/local-filesystem-mcp/patch-outcomes")
        .join(format!("{operation}.json"))
}
fn read_patch_outcome(state: &State, operation: &str) -> Result<Option<Value>, FsError> {
    let path = patch_outcome_path(state, operation);
    match fs::read(&path) {
        Ok(bytes) => {
            if bytes.len() > 2 * 1024 * 1024 {
                return Err(FsError::new(
                    "fs_patch_outcome_too_large",
                    "fs_patch_outcome_too_large",
                    json!({"path":path}),
                ));
            }
            serde_json::from_slice(&bytes).map(Some).map_err(|e| {
                FsError::new(
                    "fs_patch_outcome_invalid",
                    format!("fs_patch_outcome_invalid: {e}"),
                    json!({"path":path}),
                )
            })
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(FsError::new(
            "fs_patch_outcome_read_failed",
            format!("fs_patch_outcome_read_failed: {e}"),
            json!({"path":path}),
        )),
    }
}
fn write_patch_outcome(state: &State, operation: &str, value: &Value) -> Result<(), FsError> {
    let path = patch_outcome_path(state, operation);
    let parent = path.parent().ok_or_else(|| {
        FsError::new(
            "fs_patch_outcome_path_invalid",
            "fs_patch_outcome_path_invalid",
            json!({"path":path}),
        )
    })?;
    fs::create_dir_all(parent).map_err(|e| {
        FsError::new(
            "fs_patch_outcome_write_failed",
            format!("fs_patch_outcome_write_failed: {e}"),
            json!({"path":path}),
        )
    })?;
    let temp = path.with_extension(format!("json.tmp-{}", std::process::id()));
    let bytes = serde_json::to_vec_pretty(value)
        .map_err(|e| FsError::new("fs_patch_outcome_encode_failed", e.to_string(), json!({})))?;
    fs::write(&temp, bytes)
        .and_then(|_| {
            if path.exists() {
                fs::remove_file(&path)?;
            }
            fs::rename(&temp, &path)
        })
        .map_err(|e| {
            FsError::new(
                "fs_patch_outcome_write_failed",
                format!("fs_patch_outcome_write_failed: {e}"),
                json!({"path":path}),
            )
        })
}
fn patch_failure<T>(
    state: &State,
    operation: &str,
    patch_sha: &str,
    error: FsError,
) -> Result<T, FsError> {
    let _ = write_patch_outcome(
        state,
        operation,
        &json!({"schema":"local.filesystem.apply_patch.outcome.v1","status":"failed_before_mutation","operation_id":operation,"patch_sha256":patch_sha,"mutation_started":false,"retry_safe":true,"error":diagnostic(&error),"finished_at":now_rfc3339()}),
    );
    Err(error)
}
fn patch_parse_error(code: &str, line: usize) -> FsError {
    FsError::new(code, code, json!({"line":line}))
}
fn now_rfc3339() -> String {
    OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .unwrap_or_else(|_| "unknown".into())
}

fn patch_outcome(state: &State, args: &Value) -> Result<Value, FsError> {
    let operation = args
        .get("operation_id")
        .and_then(Value::as_str)
        .ok_or_else(|| FsError::new("operation_id_required", "operation_id_required", json!({})))?;
    if !valid_operation_id(operation) {
        return Err(FsError::new(
            "patch_operation_id_invalid",
            "patch_operation_id_invalid",
            json!({"operation_id":operation}),
        ));
    }
    let value = read_patch_outcome(state, operation)?.ok_or_else(|| {
        FsError::new(
            "fs_patch_outcome_not_found",
            format!("fs_patch_outcome_not_found: {operation}"),
            json!({"operation_id":operation,"path":patch_outcome_path(state,operation)}),
        )
    })?;
    reconcile_patch_outcome(state, operation, value)
}

fn reconcile_patch_outcome(
    state: &State,
    operation: &str,
    mut value: Value,
) -> Result<Value, FsError> {
    let status = value
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if !matches!(status, "accepted" | "applying") {
        return Ok(value);
    }
    let pid = value.get("owner_pid").and_then(Value::as_u64).unwrap_or(0) as u32;
    if process_is_alive(pid) {
        if let Some(object) = value.as_object_mut() {
            object.insert("recovery".into(),json!({"status":"owner_active","terminal":false,"retry_safe":false,"remediation":"Wait for the owning MCP surface to finish, then call fs_patch_outcome_show again."}));
        }
        return Ok(value);
    }
    let (terminal, retry_safe, reason) = if status == "accepted" {
        (
            "interrupted_before_mutation",
            true,
            "owner_exited_before_mutation_started",
        )
    } else {
        let plan = value.get("recovery_plan").cloned().unwrap_or(Value::Null);
        let after = patch_state_set_matches(state, plan.get("after_state"));
        let before = patch_state_set_matches(state, plan.get("before_state"));
        if after {
            (
                "patched_recovered",
                false,
                "filesystem_matches_planned_after_state",
            )
        } else if before {
            (
                "interrupted_before_mutation",
                true,
                "filesystem_matches_captured_before_state",
            )
        } else {
            (
                "interrupted_partial",
                false,
                "filesystem_matches_neither_captured_state",
            )
        }
    };
    if let Some(object) = value.as_object_mut() {
        object.insert("status".into(), json!(terminal));
        object.insert("finished_at".into(), json!(now_rfc3339()));
        object.insert("recovered_at".into(), json!(now_rfc3339()));
        object.insert("retry_safe".into(), json!(retry_safe));
        object.insert("recovery".into(),json!({"status":terminal,"terminal":true,"retry_safe":retry_safe,"reason":reason,"remediation":if retry_safe{"Retry fs_apply_patch with the same operation_id and identical patch."}else if terminal=="patched_recovered"{"Treat the operation as complete; do not retry it."}else{"Inspect affected files before using a new operation_id."}}));
    }
    write_patch_outcome(state, operation, &value)?;
    Ok(value)
}

fn patch_state_set_matches(state: &State, value: Option<&Value>) -> bool {
    let Some(entries) = value.and_then(Value::as_array) else {
        return false;
    };
    if entries.is_empty() {
        return false;
    }
    entries.iter().all(|entry| {
        let Some(path) = entry.get("path").and_then(Value::as_str) else {
            return false;
        };
        let Ok((resolved, _)) = resolve_allowed(state, Some(path), "fs_patch_outcome_show") else {
            return false;
        };
        let expected_exists = entry
            .get("exists")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        if resolved.exists() != expected_exists {
            return false;
        }
        if !expected_exists {
            return true;
        }
        let Some(expected) = entry.get("sha256").and_then(Value::as_str) else {
            return false;
        };
        fs::read(&resolved)
            .ok()
            .is_some_and(|bytes| sha256_bytes(&bytes) == expected)
    })
}

fn process_is_alive(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }
    #[cfg(windows)]
    {
        Command::new("tasklist")
            .args(["/FI", &format!("PID eq {pid}"), "/FO", "CSV", "/NH"])
            .output()
            .ok()
            .is_some_and(|output| {
                String::from_utf8_lossy(&output.stdout).contains(&format!("\"{pid}\""))
            })
    }
    #[cfg(not(windows))]
    {
        Path::new(&format!("/proc/{pid}")).exists()
    }
}

fn list_tools(mode: &str) -> Vec<Value> {
    let mut names = vec![
        (
            "fs_guidance",
            "Show model-facing operating guidance for local-filesystem MCP workflows.",
        ),
        (
            "fs_read_file",
            "Read a text file under an allowed root with line offset and limit.",
        ),
        (
            "fs_read_file_range",
            "Read a logical text-file line range under an allowed root. Lines are 1-based and inclusive; ranges over 1,000 lines return a bounded page with continuation.arguments for the same MCP tool.",
        ),
        (
            "fs_stat",
            "Return file or directory metadata under an allowed root.",
        ),
        (
            "fs_glob_search",
            "List files under an allowed root using ripgrep file globbing.",
        ),
        (
            "fs_grep_search",
            "Search file contents under an allowed root using ripgrep.",
        ),
        (
            "fs_repository_inventory",
            "Return a bounded candidate-source inventory under an allowed root.",
        ),
        (
            "fs_file_metrics",
            "Return bounded metadata-only file metrics under an allowed root.",
        ),
        ("fs_doctor", "Inspect local-filesystem MCP policy posture."),
        (
            "fs_patch_outcome_show",
            "Read and durably reconcile the outcome for an fs_apply_patch operation_id.",
        ),
    ];
    if mode == "write" {
        names.push(("fs_write_file", "Write a text file under an allowed root and append an audit record. Refuses executable scripts under .ai/tmp or .ai/temp."));
        names.push((
            "fs_str_replace_file",
            "Replace exactly one string occurrence in a text file under an allowed root.",
        ));
        names.push((
            "fs_replace_range",
            "Replace an inclusive line range in a text file under an allowed root.",
        ));
        names.push((
            "fs_apply_patch",
            "Apply a unified diff or Codex-style patch atomically under allowed roots, with durable replay and recovery by operation_id.",
        ));
        names.push((
            "fs_move_path",
            "Move a file or directory under allowed roots.",
        ));
        names.push((
            "fs_create_directory",
            "Create a directory under an allowed root.",
        ));
        names.push((
            "fs_rename_directory",
            "Rename a directory under allowed roots.",
        ));
        names.push((
            "fs_delete_directory",
            "Delete a directory under an allowed root with explicit recursive consent.",
        ));
    }
    names.iter().map(|(name, description)| {
        let mut properties = Map::new();
        match *name {
            "fs_guidance" => { properties.insert("workflow".into(), json!({"type":"string"})); properties.insert("tool".into(), json!({"type":"string"})); }
            "fs_read_file" => { properties.insert("path".into(), json!({"type":"string"})); properties.insert("offset".into(), json!({"type":"integer","minimum":1,"maximum":10_000_000,"default":1,"description":"One-based first line to return."})); properties.insert("limit".into(), json!({"type":"integer","minimum":1,"maximum":1_000,"default":400,"description":"Maximum lines returned; paginate requests over 1,000 lines."})); properties.insert("timeout_ms".into(), json!({"type":"integer","minimum":1,"maximum":60_000,"default":READ_TIMEOUT_MS})); }
            "fs_read_file_range" => { properties.insert("path".into(), json!({"type":"string"})); properties.insert("start_line".into(), json!({"type":"integer","minimum":1,"maximum":10_000_000,"description":"Inclusive logical start line."})); properties.insert("end_line".into(), json!({"type":"integer","minimum":1,"maximum":10_000_000,"description":"Inclusive logical end line. Requests spanning over 1,000 lines return a bounded page; follow continuation.arguments until has_more is false."})); properties.insert("timeout_ms".into(), json!({"type":"integer","minimum":1,"maximum":60_000,"default":READ_TIMEOUT_MS})); }
            "fs_stat" => { properties.insert("path".into(), json!({"type":"string"})); properties.insert("timeout_ms".into(),json!({"type":"integer","minimum":1,"maximum":300_000,"default":60_000})); }
            "fs_glob_search" => { properties.insert("pattern".into(), json!({"type":"string"})); properties.insert("directory".into(), json!({"type":"string","default":"."})); properties.insert("ignore".into(), json!({"type":"array","items":{"type":"string"}})); properties.insert("offset".into(), json!({"type":"integer","minimum":0,"maximum":10_000_000,"default":0})); properties.insert("limit".into(), json!({"type":"integer","minimum":1,"maximum":500,"default":100})); properties.insert("timeout_ms".into(), json!({"type":"integer","minimum":1,"maximum":300_000,"default":SEARCH_TIMEOUT_MS})); properties.insert("cache_policy".into(), json!({"type":"string","enum":["auto","snapshot","refresh","bypass"],"default":"auto"})); properties.insert("snapshot_id".into(), json!({"type":"string"})); }
            "fs_grep_search" => { properties.insert("pattern".into(), json!({"type":"string"})); properties.insert("path".into(), json!({"type":"string","default":"."})); properties.insert("output_mode".into(), json!({"type":"string","enum":["files_with_matches","count_matches","content"],"default":"files_with_matches"})); properties.insert("ignore".into(), json!({"type":"array","items":{"type":"string"}})); properties.insert("offset".into(), json!({"type":"integer","minimum":0,"maximum":10_000_000,"default":0})); properties.insert("limit".into(), json!({"type":"integer","minimum":1,"maximum":500,"default":80})); properties.insert("timeout_ms".into(), json!({"type":"integer","minimum":1,"maximum":300_000,"default":SEARCH_TIMEOUT_MS})); properties.insert("cache_policy".into(), json!({"type":"string","enum":["auto","snapshot","refresh","bypass"],"default":"auto"})); properties.insert("snapshot_id".into(), json!({"type":"string"})); }
            "fs_repository_inventory" => { properties.insert("pattern".into(), json!({"type":"string","default":"**/*"})); properties.insert("directory".into(), json!({"type":"string","description":"Canonical inventory scope; mutually exclusive with root."})); properties.insert("root".into(), json!({"type":"string","description":"Compatibility alias for directory; mutually exclusive with directory."})); properties.insert("ignore".into(), json!({"type":"array","items":{"type":"string"}})); properties.insert("include_generated".into(), json!({"type":"boolean","default":false})); properties.insert("offset".into(), json!({"type":"integer","minimum":0,"maximum":10_000_000,"default":0})); properties.insert("limit".into(), json!({"type":"integer","minimum":1,"maximum":500,"default":100})); properties.insert("timeout_ms".into(), json!({"type":"integer","minimum":1,"maximum":300_000,"default":SEARCH_TIMEOUT_MS})); properties.insert("cache_policy".into(), json!({"type":"string","enum":["auto","snapshot","refresh","bypass"],"default":"auto"})); properties.insert("snapshot_id".into(), json!({"type":"string"})); }
            "fs_file_metrics" => { properties.insert("pattern".into(), json!({"type":"string","default":"**/*"})); properties.insert("directory".into(), json!({"type":"string","default":"."})); properties.insert("root".into(), json!({"type":"string"})); properties.insert("ignore".into(), json!({"type":"array","items":{"type":"string"}})); properties.insert("exclude".into(), json!({"type":"array","items":{"type":"string"}})); properties.insert("offset".into(), json!({"type":"integer","minimum":0,"maximum":10_000_000,"default":0})); properties.insert("limit".into(), json!({"type":"integer","minimum":1,"maximum":100,"default":100})); properties.insert("max_bytes_per_file".into(), json!({"type":"integer","minimum":1,"maximum":1_073_741_824,"default":8_388_608})); properties.insert("max_total_scan_bytes".into(), json!({"type":"integer","minimum":1,"maximum":1_073_741_824,"default":268_435_456})); properties.insert("timeout_ms".into(), json!({"type":"integer","minimum":1,"maximum":300_000,"default":SEARCH_TIMEOUT_MS})); properties.insert("cache_policy".into(), json!({"type":"string","enum":["auto","snapshot","refresh","bypass"],"default":"auto"})); properties.insert("snapshot_id".into(), json!({"type":"string"})); }
            "fs_patch_outcome_show" => { properties.insert("operation_id".into(), json!({"type":"string"})); }
            "fs_write_file" => {
                properties.insert("payload_ref".into(), json!({"type":"string","maxLength":96}));
                properties.insert("payload_path".into(), json!({"type":"string","maxLength":32_768}));
                properties.insert("path".into(), json!({"type":"string"}));
                properties.insert("content".into(), json!({"type":"string"}));
                properties.insert("overwrite".into(), json!({"type":"boolean","default":true}));
                properties.insert("create_only".into(), json!({"type":"boolean","default":false}));
                properties.insert("create_parent_directories".into(), json!({"type":"boolean","default":true}));
                properties.insert("timeout_ms".into(), json!({"type":"integer","default":WRITE_TIMEOUT_MS}));
                properties.insert("expected_sha256".into(), json!({"type":"string"}));
            }
            "fs_str_replace_file" => {
                properties.insert("path".into(), json!({"type":"string"}));
                properties.insert("old".into(), json!({"type":"string"}));
                properties.insert("new".into(), json!({"type":"string"}));
                properties.insert("expected_sha256".into(), json!({"type":"string"}));
            }
            "fs_replace_range" => {
                properties.insert("path".into(), json!({"type":"string"}));
                properties.insert("start_line".into(), json!({"type":"integer"}));
                properties.insert("end_line".into(), json!({"type":"integer"}));
                properties.insert("replacement".into(), json!({"type":"string"}));
                properties.insert("expected_sha256".into(), json!({"type":"string"}));
            }
            "fs_apply_patch" => {
                properties.insert("patch".into(), json!({"type":"string","maxLength":8_388_608}));
                properties.insert("operation_id".into(), json!({"type":"string","pattern":"^[A-Za-z0-9._-]{1,160}$","maxLength":160}));
                properties.insert("dry_run".into(), json!({"type":"boolean","default":false}));
                properties.insert("timeout_ms".into(), json!({"type":"integer","minimum":1,"maximum":300_000,"default":WRITE_TIMEOUT_MS}));
                properties.insert("expected_sha256".into(), json!({"type":"object","maxProperties":256,"additionalProperties":{"type":"string","pattern":"^[0-9a-fA-F]{64}$","maxLength":64}}));
            }
            "fs_move_path" => {
                properties.insert("from".into(), json!({"type":"string"}));
                properties.insert("to".into(), json!({"type":"string"}));
                properties.insert("overwrite".into(), json!({"type":"boolean","default":false}));
                properties.insert("expected_from_size".into(), json!({"type":"integer"}));
                properties.insert("expected_from_mtime".into(), json!({"type":"string"}));
                properties.insert("expected_from_sha256".into(), json!({"type":"string"}));
                properties.insert("expected_from_tree_sha256".into(), json!({"type":"string"}));
                properties.insert("expected_from_entry_count".into(), json!({"type":"integer"}));
                properties.insert("expected_to_size".into(), json!({"type":"integer"}));
                properties.insert("expected_to_mtime".into(), json!({"type":"string"}));
                properties.insert("expected_to_sha256".into(), json!({"type":"string"}));
                properties.insert("expected_to_tree_sha256".into(), json!({"type":"string"}));
                properties.insert("expected_to_entry_count".into(), json!({"type":"integer"}));
                properties.insert("expected_from".into(), expected_metadata_schema());
                properties.insert("expected_to".into(), expected_metadata_schema());
            }
            "fs_create_directory" => {
                properties.insert("path".into(), json!({"type":"string"}));
                properties.insert("recursive".into(), json!({"type":"boolean","default":false}));
            }
            "fs_rename_directory" => {
                properties.insert("from".into(), json!({"type":"string"}));
                properties.insert("to".into(), json!({"type":"string"}));
                properties.insert("expected_from_mtime".into(), json!({"type":"string"}));
                properties.insert("expected_from_size".into(), json!({"type":"integer"}));
                properties.insert("expected_from_tree_sha256".into(), json!({"type":"string"}));
                properties.insert("expected_from_entry_count".into(), json!({"type":"integer"}));
                properties.insert("expected_to_mtime".into(), json!({"type":"string"}));
                properties.insert("expected_to_size".into(), json!({"type":"integer"}));
                properties.insert("expected_to_tree_sha256".into(), json!({"type":"string"}));
                properties.insert("expected_to_entry_count".into(), json!({"type":"integer"}));
                properties.insert("expected_from".into(), expected_metadata_schema());
                properties.insert("expected_to".into(), expected_metadata_schema());
            }
            "fs_delete_directory" => {
                properties.insert("path".into(), json!({"type":"string"}));
                properties.insert("recursive".into(), json!({"type":"boolean","default":false}));
                properties.insert("expected_size".into(), json!({"type":"integer"}));
                properties.insert("expected_mtime".into(), json!({"type":"string"}));
                properties.insert("expected_tree_sha256".into(), json!({"type":"string"}));
                properties.insert("expected_entry_count".into(), json!({"type":"integer"}));
                properties.insert("expected".into(), expected_metadata_schema());
            }
            _ => {}
        }
        let required: Vec<&str> = match *name {
            "fs_read_file" => vec!["path"],
            "fs_read_file_range" => vec!["path", "start_line", "end_line"],
            "fs_stat" => vec!["path"],
            "fs_grep_search" => vec!["pattern"],
            "fs_glob_search" => vec!["pattern"],
            "fs_patch_outcome_show" => vec!["operation_id"],
            "fs_str_replace_file" => vec!["path", "old", "new"],
            "fs_replace_range" => vec!["path", "start_line", "end_line", "replacement"],
            "fs_apply_patch" => vec!["patch"],
            "fs_move_path" => vec!["from", "to"],
            "fs_create_directory" => vec!["path"],
            "fs_rename_directory" => vec!["from", "to"],
            "fs_delete_directory" => vec!["path"],
            _ => Vec::new()
        };
        let write_tool = tool_has_write_effect(name);
        let destructive = matches!(*name,"fs_str_replace_file"|"fs_replace_range"|"fs_apply_patch"|"fs_move_path"|"fs_rename_directory"|"fs_delete_directory");
        let idempotent = !matches!(*name,"fs_str_replace_file"|"fs_replace_range"|"fs_move_path"|"fs_rename_directory"|"fs_delete_directory");
        bound_tool_properties(&mut properties);
        json!({"name": name, "canonical_name": name, "description": description, "inputSchema": {"title":format!("{name} arguments"),"type":"object","properties": properties,"required": required,"additionalProperties":false}, "annotations": {"title":name,"readOnlyHint":!write_tool,"destructiveHint":destructive,"idempotentHint":idempotent,"openWorldHint":false}, "outputSchema":{"title":format!("{name} result"),"type":"object","maxProperties":256,"additionalProperties":true}})
    }).collect()
}

fn expected_metadata_schema() -> Value {
    json!({"type":"object","maxProperties":5,"additionalProperties":false,"properties":{
        "mtime":{"type":"string","maxLength":128},"size":{"type":"integer","minimum":0,"maximum":9_007_199_254_740_991_i64},
        "sha256":{"type":"string","pattern":"^[0-9a-fA-F]{64}$","maxLength":64},"tree_sha256":{"type":"string","pattern":"^[0-9a-fA-F]{64}$","maxLength":64},
        "entry_count":{"type":"integer","minimum":0,"maximum":5_000}
    }})
}

fn tool_has_write_effect(name: &str) -> bool {
    is_write_tool(name) || name == "fs_patch_outcome_show"
}

fn bound_tool_properties(properties: &mut Map<String, Value>) {
    for (name, schema) in properties.iter_mut() {
        let Some(object) = schema.as_object_mut() else {
            continue;
        };
        match object.get("type").and_then(Value::as_str) {
            Some("string") if !object.contains_key("maxLength") => {
                let limit = if matches!(name.as_str(), "content" | "replacement" | "old" | "new") {
                    8_388_608
                } else {
                    32_768
                };
                object.insert("maxLength".into(), json!(limit));
            }
            Some("array") => {
                object.entry("maxItems").or_insert_with(|| json!(256));
                if let Some(items) = object.get_mut("items").and_then(Value::as_object_mut) {
                    if items.get("type").and_then(Value::as_str) == Some("string") {
                        items.entry("maxLength").or_insert_with(|| json!(32_768));
                    }
                }
            }
            Some("integer") => {
                object.entry("minimum").or_insert_with(|| json!(0));
                let maximum = if name == "limit" {
                    1_000_i64
                } else if name == "timeout_ms" {
                    300_000
                } else if name.contains("bytes") {
                    1_073_741_824
                } else if name.contains("entry_count") {
                    5_000_000
                } else if matches!(name.as_str(), "offset" | "start_line" | "end_line") {
                    10_000_000
                } else {
                    9_007_199_254_740_991
                };
                object.entry("maximum").or_insert_with(|| json!(maximum));
            }
            Some("object") => {
                object.entry("maxProperties").or_insert_with(|| json!(256));
            }
            _ => {}
        }
    }
}

fn validate_tool_arguments(mode: &str, name: &str, args: &Value) -> Result<(), FsError> {
    let tool = list_tools(mode)
        .into_iter()
        .find(|tool| tool.get("name").and_then(Value::as_str) == Some(name))
        .ok_or_else(|| {
            FsError::new(
                format!("tool_not_available_in_{mode}_mode"),
                format!("tool_not_available_in_{mode}_mode: {name}"),
                json!({"tool_name":name,"mode":mode}),
            )
        })?;
    let schema = tool
        .get("inputSchema")
        .and_then(Value::as_object)
        .expect("tool schema");
    let object = args.as_object().ok_or_else(|| {
        FsError::new(
            "tool_arguments_must_be_object",
            "tool_arguments_must_be_object",
            json!({"tool_name":name,"actual_type":json_type(args)}),
        )
    })?;
    let properties = schema
        .get("properties")
        .and_then(Value::as_object)
        .expect("properties");
    let unknown: Vec<&String> = object
        .keys()
        .filter(|key| !properties.contains_key(*key))
        .collect();
    if !unknown.is_empty() {
        return Err(FsError::new(
            "tool_argument_unknown",
            "tool_argument_unknown",
            json!({"tool_name":name,"fields":unknown}),
        ));
    }
    for required in schema
        .get("required")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
    {
        if !object.contains_key(required) {
            return Err(FsError::new(
                "tool_argument_required",
                "tool_argument_required",
                json!({"tool_name":name,"field":required}),
            ));
        }
    }
    for (field, value) in object {
        validate_schema_value(name, field, value, &properties[field])?;
    }
    Ok(())
}

fn validate_schema_value(
    tool_name: &str,
    field: &str,
    value: &Value,
    schema: &Value,
) -> Result<(), FsError> {
    let expected = schema.get("type").and_then(Value::as_str).unwrap_or("any");
    let valid = match expected {
        "string" => value.is_string(),
        "integer" => value.as_i64().is_some() || value.as_u64().is_some(),
        "boolean" => value.is_boolean(),
        "array" => value.is_array(),
        "object" => value.is_object(),
        _ => true,
    };
    if !valid {
        return Err(FsError::new(
            "tool_argument_type_invalid",
            "tool_argument_type_invalid",
            json!({"tool_name":tool_name,"field":field,"expected":expected,"actual":json_type(value)}),
        ));
    }
    if let Some(text) = value.as_str() {
        if let Some(max) = schema.get("maxLength").and_then(Value::as_u64) {
            if text.chars().count() as u64 > max {
                return Err(FsError::new(
                    "tool_argument_too_long",
                    "tool_argument_too_long",
                    json!({"tool_name":tool_name,"field":field,"maximum":max}),
                ));
            }
        }
        if let Some(values) = schema.get("enum").and_then(Value::as_array) {
            if !values
                .iter()
                .any(|candidate| candidate.as_str() == Some(text))
            {
                return Err(FsError::new(
                    "tool_argument_enum_invalid",
                    "tool_argument_enum_invalid",
                    json!({"tool_name":tool_name,"field":field,"allowed":values}),
                ));
            }
        }
        if field == "operation_id" && !valid_operation_id(text) {
            return Err(FsError::new(
                "patch_operation_id_invalid",
                "patch_operation_id_invalid",
                json!({"operation_id":text}),
            ));
        }
        if matches!(
            field,
            "expected_sha256" | "expected_from_sha256" | "expected_to_sha256"
        ) && !text.is_empty()
            && !valid_sha256(text)
        {
            return Err(FsError::new(
                "tool_argument_sha256_invalid",
                "tool_argument_sha256_invalid",
                json!({"tool_name":tool_name,"field":field}),
            ));
        }
    }
    if let Some(items) = value.as_array() {
        let max = schema
            .get("maxItems")
            .and_then(Value::as_u64)
            .unwrap_or(256);
        if items.len() as u64 > max {
            return Err(FsError::new(
                "tool_argument_array_too_large",
                "tool_argument_array_too_large",
                json!({"tool_name":tool_name,"field":field,"maximum":max}),
            ));
        }
        if let Some(item_schema) = schema.get("items") {
            for item in items {
                validate_schema_value(tool_name, field, item, item_schema)?;
            }
        }
    }
    if let Some(number) = value.as_i64() {
        if schema
            .get("minimum")
            .and_then(Value::as_i64)
            .is_some_and(|min| number < min)
            || schema
                .get("maximum")
                .and_then(Value::as_i64)
                .is_some_and(|max| number > max)
        {
            return Err(FsError::new(
                "tool_argument_integer_out_of_range",
                "tool_argument_integer_out_of_range",
                json!({"tool_name":tool_name,"field":field,"value":number,"minimum":schema.get("minimum"),"maximum":schema.get("maximum")}),
            ));
        }
    }
    if let Some(object) = value.as_object() {
        let max = schema
            .get("maxProperties")
            .and_then(Value::as_u64)
            .unwrap_or(256);
        if object.len() as u64 > max {
            return Err(FsError::new(
                "tool_argument_object_too_large",
                "tool_argument_object_too_large",
                json!({"tool_name":tool_name,"field":field,"maximum":max}),
            ));
        }
        let declared = schema.get("properties").and_then(Value::as_object);
        if schema.get("additionalProperties") == Some(&Value::Bool(false)) {
            let unknown: Vec<_> = object
                .keys()
                .filter(|key| declared.is_none_or(|properties| !properties.contains_key(*key)))
                .collect();
            if !unknown.is_empty() {
                return Err(FsError::new(
                    "tool_argument_nested_unknown",
                    "tool_argument_nested_unknown",
                    json!({"tool_name":tool_name,"field":field,"fields":unknown}),
                ));
            }
        }
        for (key, item) in object {
            if key.chars().count() > 32_768 {
                return Err(FsError::new(
                    "tool_argument_key_too_long",
                    "tool_argument_key_too_long",
                    json!({"tool_name":tool_name,"field":field}),
                ));
            }
            if let Some(child) = declared
                .and_then(|properties| properties.get(key))
                .or_else(|| {
                    schema
                        .get("additionalProperties")
                        .filter(|value| value.is_object())
                })
            {
                validate_schema_value(tool_name, key, item, child)?;
            }
        }
    }
    Ok(())
}

fn json_type(value: &Value) -> &'static str {
    if value.is_null() {
        "null"
    } else if value.is_object() {
        "object"
    } else if value.is_array() {
        "array"
    } else if value.is_string() {
        "string"
    } else if value.is_boolean() {
        "boolean"
    } else {
        "number"
    }
}

fn valid_operation_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 160
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"._-".contains(&byte))
}

fn valid_sha256(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn resolve_allowed(
    state: &State,
    input: Option<&str>,
    operation: &str,
) -> Result<(PathBuf, PathBuf), FsError> {
    let input = input
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| {
            FsError::new(
                "path_required",
                "path_required",
                json!({"operation": operation}),
            )
        })?;
    if input.contains('%') {
        return Err(FsError::new(
            "path_environment_expansion_not_supported",
            format!("path_environment_expansion_not_supported: {input}"),
            json!({"operation": operation, "requested_path": input, "remediation": "Expand environment variables before calling the filesystem surface, or pass an absolute path."}),
        ));
    }
    let base = state.allowed_roots.first().cloned().ok_or_else(|| {
        FsError::new(
            "filesystem_mcp_requires_at_least_one_allowed_root",
            "filesystem_mcp_requires_at_least_one_allowed_root",
            json!({}),
        )
    })?;
    let candidate = if Path::new(input).is_absolute() {
        absolute(PathBuf::from(input))
    } else {
        absolute(base.join(input))
    };
    let root = state
        .allowed_roots
        .iter()
        .find(|root| within(root, &candidate))
        .cloned();
    let Some(root) = root else {
        return Err(FsError::new(
            "path_outside_allowed_roots",
            format!("path_outside_allowed_roots: {input}"),
            json!({"operation": operation, "requested_path": input, "active_resolution_base": base, "resolution_rule": "first_allowed_root_for_relative_paths", "allowed_roots": state.allowed_roots}),
        ));
    };
    let check_path = canonicalize_with_missing(&candidate);
    let check_root = canonicalize_with_missing(&root);
    if !within(&check_root, &check_path) {
        return Err(FsError::new(
            "path_outside_allowed_roots",
            format!("path_outside_allowed_roots: {input}"),
            path_details(&candidate, &root),
        ));
    }
    Ok((candidate, root))
}

fn canonicalize_with_missing(path: &Path) -> PathBuf {
    let mut missing = Vec::new();
    let mut current = path.to_path_buf();
    while !current.exists() {
        let Some(name) = current.file_name().map(|value| value.to_os_string()) else {
            break;
        };
        missing.push(name);
        if !current.pop() {
            break;
        }
    }
    let mut canonical = fs::canonicalize(&current).unwrap_or(current);
    for component in missing.iter().rev() {
        canonical.push(component);
    }
    canonical
}

fn path_details(path: &Path, root: &Path) -> Value {
    json!({"path": path, "root": root, "relative_path": relative_path(root, path)})
}

fn relative_path(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .map(|value| value.to_string_lossy().replace('\\', "/"))
        .unwrap_or_else(|_| path.to_string_lossy().replace('\\', "/"))
}

fn within(root: &Path, path: &Path) -> bool {
    let root = normalize_path(root);
    let path = normalize_path(path);
    path == root || path.starts_with(&(root + "/"))
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

fn normalize_path(path: &Path) -> String {
    path.to_string_lossy()
        .replace('\\', "/")
        .trim_end_matches('/')
        .to_ascii_lowercase()
}

fn integer(value: &Value, key: &str) -> Option<i64> {
    value.get(key).and_then(Value::as_i64)
}

fn sha256_bytes(bytes: &[u8]) -> String {
    let mut digest = Sha256::new();
    digest.update(bytes);
    hex::encode(digest.finalize())
}

fn mtime_iso(metadata: &fs::Metadata) -> String {
    let duration = metadata
        .modified()
        .ok()
        .and_then(|value| value.duration_since(std::time::UNIX_EPOCH).ok());
    if let Some(value) = duration {
        if let Ok(date) = OffsetDateTime::from_unix_timestamp(value.as_secs() as i64) {
            return date
                .format(&Rfc3339)
                .unwrap_or_else(|_| "1970-01-01T00:00:00Z".to_string());
        }
    }
    "1970-01-01T00:00:00Z".to_string()
}

fn freshness(path: &Path) -> Value {
    match fs::metadata(path) {
        Ok(metadata) => {
            let kind = if metadata.is_dir() {
                "directory"
            } else if metadata.is_file() {
                "file"
            } else {
                "other"
            };
            let modified_ms = metadata
                .modified()
                .ok()
                .and_then(|value| value.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|value| value.as_millis())
                .unwrap_or(0);
            let mut value = Map::new();
            value.insert("path".into(), json!(path));
            value.insert("type".into(), json!(kind));
            value.insert("size".into(), json!(metadata.len()));
            value.insert("mtime".into(), json!(mtime_iso(&metadata)));
            value.insert("mtime_ms".into(), json!(modified_ms));
            if metadata.is_file() {
                if let Ok(hash) = sha256_file_with_timeout(path, 60_000, "filesystem_freshness") {
                    value.insert("sha256".into(), json!(hash));
                }
            } else if metadata.is_dir() {
                let (entry_count, tree_entry_count, tree_sha256, truncated) =
                    directory_fingerprint(path, path);
                value.insert("entry_count".into(), json!(entry_count));
                value.insert("tree_entry_count".into(), json!(tree_entry_count));
                value.insert("tree_truncated".into(), json!(truncated));
                value.insert("tree_sha256".into(), json!(tree_sha256));
            }
            Value::Object(value)
        }
        Err(_) => json!({"path": path, "type": "missing"}),
    }
}

fn directory_fingerprint(path: &Path, root: &Path) -> (usize, usize, String, bool) {
    let mut entries = Vec::new();
    let mut truncated = false;
    walk_directory(path, root, &mut entries, &mut truncated);
    let direct_count = fs::read_dir(path).map(|iter| iter.count()).unwrap_or(0);
    (
        direct_count,
        entries.len(),
        sha256_bytes(entries.join("\n").as_bytes()),
        truncated,
    )
}

fn walk_directory(path: &Path, root: &Path, entries: &mut Vec<String>, truncated: &mut bool) {
    if entries.len() >= 5000 {
        *truncated = true;
        return;
    }
    let Ok(iter) = fs::read_dir(path) else { return };
    let mut children: Vec<_> = iter.filter_map(Result::ok).collect();
    children.sort_by_key(|entry| entry.file_name());
    for entry in children {
        if entries.len() >= 5000 {
            *truncated = true;
            return;
        }
        let child = entry.path();
        let Ok(metadata) = fs::metadata(&child) else {
            continue;
        };
        let kind = if metadata.is_dir() {
            "directory"
        } else if metadata.is_file() {
            "file"
        } else {
            "other"
        };
        entries.push(format!(
            "{}\t{}\t{}\t{}",
            relative_path(root, &child),
            kind,
            metadata.len(),
            metadata
                .modified()
                .ok()
                .and_then(|value| value.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|value| value.as_millis())
                .unwrap_or(0)
        ));
        if metadata.is_dir() {
            walk_directory(&child, root, entries, truncated);
        }
    }
}

fn run_rg(args: &[String], timeout: u64, operation: &str) -> Result<(Vec<String>, bool), FsError> {
    let started = std::time::Instant::now();
    let mut child = Command::new("rg")
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| {
            FsError::new(
                format!("{operation}_failed"),
                format!("{operation}_failed: {error}"),
                json!({"operation": operation}),
            )
        })?;
    let stdout = child.stdout.take().expect("rg stdout");
    let stderr = child.stderr.take().expect("rg stderr");
    let (sender, receiver) = mpsc::sync_channel::<Result<Option<String>, String>>(64);
    thread::spawn(move || {
        let mut reader = BufReader::new(stdout);
        loop {
            let mut bytes = Vec::new();
            match reader.read_until(b'\n', &mut bytes) {
                Ok(0) => {
                    let _ = sender.send(Ok(None));
                    break;
                }
                Ok(_) => {
                    if bytes.len() > MAX_SEARCH_LINE_BYTES {
                        let _ = sender.send(Err("search_result_line_too_large".into()));
                        break;
                    }
                    while matches!(bytes.last(), Some(b'\n' | b'\r')) {
                        bytes.pop();
                    }
                    match String::from_utf8(bytes) {
                        Ok(line) => {
                            if sender.send(Ok(Some(line))).is_err() {
                                break;
                            }
                        }
                        Err(_) => {
                            let _ = sender.send(Err("search_result_not_utf8".into()));
                            break;
                        }
                    }
                }
                Err(error) => {
                    let _ = sender.send(Err(format!("search_stdout_read_failed: {error}")));
                    break;
                }
            }
        }
    });
    let stderr_reader = thread::spawn(move || {
        let mut reader = stderr.take((64 * 1024 + 1) as u64);
        let mut bytes = Vec::new();
        let _ = reader.read_to_end(&mut bytes);
        bytes
    });
    let mut matches = Vec::new();
    let mut bytes = 0_usize;
    let mut complete = false;
    let mut capture_limited = false;
    loop {
        let elapsed = started.elapsed().as_millis() as u64;
        if elapsed > timeout {
            let _ = child.kill();
            let _ = child.wait();
            return Err(FsError::new(
                format!("{operation}_timed_out"),
                format!("{operation}_timed_out"),
                json!({"operation":operation,"timeout_ms":timeout}),
            ));
        }
        match receiver.recv_timeout(Duration::from_millis(10)) {
            Ok(Ok(Some(line))) => {
                if line.trim().is_empty() {
                    continue;
                }
                bytes = bytes.saturating_add(line.len());
                if matches.len() >= MAX_SEARCH_CAPTURE_ENTRIES || bytes > MAX_SEARCH_CAPTURE_BYTES {
                    capture_limited = true;
                    let _ = child.kill();
                    break;
                }
                matches.push(line);
            }
            Ok(Ok(None)) => {
                complete = true;
                break;
            }
            Ok(Err(code)) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(FsError::new(
                    code.clone(),
                    code,
                    json!({"operation":operation}),
                ));
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                if child.try_wait().ok().flatten().is_some() {
                    complete = true;
                    break;
                }
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                complete = true;
                break;
            }
        }
    }
    let status = child.wait().map_err(|error| {
        FsError::new(
            format!("{operation}_failed"),
            format!("{operation}_failed: {error}"),
            json!({"operation":operation}),
        )
    })?;
    let stderr = stderr_reader.join().unwrap_or_default();
    if !capture_limited && status.code().unwrap_or(2) > 1 {
        return Err(FsError::new(
            format!("{operation}_failed"),
            format!(
                "{operation}_failed: {}",
                String::from_utf8_lossy(&stderr).trim()
            ),
            json!({"operation": operation, "status": status.code(),"stderr_truncated":stderr.len()>64*1024}),
        ));
    }
    Ok((matches, complete && !capture_limited))
}

fn render_grep(line: &str, mode: &str) -> String {
    let fields: Vec<&str> = line.split('\u{1f}').collect();
    match mode {
        "count_matches" => {
            if fields.len() >= 2 {
                format!("{}: {}", fields[0], fields[1])
            } else {
                line.to_string()
            }
        }
        "content" => {
            if fields.len() >= 3 {
                format!("{}:{}:{}", fields[0], fields[1], fields[2..].join("\u{1f}"))
            } else {
                line.to_string()
            }
        }
        _ => fields.first().copied().unwrap_or(line).to_string(),
    }
}

fn grep_match_object(line: &str, mode: &str) -> Value {
    let fields: Vec<&str> = line.split('\u{1f}').collect();
    match mode {
        "count_matches" => {
            json!({"path": fields.first().copied().unwrap_or(line), "count": fields.get(1).and_then(|value| value.parse::<u64>().ok()), "raw": line})
        }
        "content" => {
            json!({"path": fields.first().copied().unwrap_or(line), "line": fields.get(1).and_then(|value| value.parse::<u64>().ok()), "text": fields.get(2).copied().unwrap_or(""), "raw": line})
        }
        _ => json!({"path": line, "raw": line}),
    }
}

fn classify(path: &str) -> &'static str {
    let normalized =
        format!("/{}/", path.replace('\\', "/").trim_matches('/')).to_ascii_lowercase();
    if GENERATED_MARKERS
        .iter()
        .any(|marker| normalized.contains(marker))
    {
        "generated_artifact"
    } else {
        "candidate_source"
    }
}

fn count_lines(path: &Path) -> io::Result<(usize, bool)> {
    let mut file = fs::File::open(path)?;
    let mut buffer = [0_u8; 64 * 1024];
    let mut lines = 0_usize;
    let mut any = false;
    let mut last_newline = false;
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        let chunk = &buffer[..count];
        if chunk.contains(&0) {
            return Ok((0, true));
        }
        any = true;
        lines += chunk.iter().filter(|byte| **byte == b'\n').count();
        last_newline = chunk.last() == Some(&b'\n');
    }
    if any && !last_newline {
        lines += 1;
    }
    Ok((lines, false))
}

fn aggregate_metrics(files: &[Value]) -> Value {
    let mut bytes = 0_u64;
    let mut lines = 0_u64;
    let mut exact = true;
    let mut binary = 0_u64;
    let mut too_large = 0_u64;
    let mut unavailable = 0_u64;
    let mut budget = 0_u64;
    for file in files {
        bytes += file.get("byte_count").and_then(Value::as_u64).unwrap_or(0);
        if let Some(value) = file.get("line_count").and_then(Value::as_u64) {
            lines += value;
        } else {
            exact = false;
            match file
                .get("line_count_status")
                .and_then(Value::as_str)
                .unwrap_or_default()
            {
                "binary" => binary += 1,
                "too_large" => too_large += 1,
                "unavailable" => unavailable += 1,
                "scan_budget_exceeded" => budget += 1,
                _ => {}
            }
        }
    }
    json!({"file_count": files.len(), "byte_count": bytes, "line_count": if exact {json!(lines)} else {Value::Null}, "line_count_status": if exact {"exact"} else {"partial"}, "binary_file_count": binary, "too_large_file_count": too_large, "unavailable_file_count": unavailable, "scan_budget_exceeded_file_count": budget})
}

pub(crate) fn read_message<R: BufRead>(reader: &mut R) -> io::Result<Option<(Value, bool)>> {
    let mut first = String::new();
    loop {
        if reader.read_line(&mut first)? == 0 {
            return Ok(None);
        }
        if !first.trim().is_empty() {
            break;
        }
        first.clear();
    }
    if first.to_ascii_lowercase().starts_with("content-length:") {
        let length = first
            .split(':')
            .nth(1)
            .and_then(|value| value.trim().parse::<usize>().ok())
            .unwrap_or(0);
        let mut header = String::new();
        loop {
            header.clear();
            reader.read_line(&mut header)?;
            if header.trim().is_empty() {
                break;
            }
        }
        let mut body = vec![0_u8; length];
        reader.read_exact(&mut body)?;
        return serde_json::from_slice(&body)
            .map(|value| Some((value, true)))
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error));
    }
    serde_json::from_str(first.trim())
        .map(|value| Some((value, false)))
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
}

pub(crate) fn write_message<W: Write>(
    writer: &mut W,
    value: &Value,
    framed: bool,
) -> io::Result<()> {
    let body = serde_json::to_string(value)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    if framed {
        write!(writer, "Content-Length: {}\r\n\r\n{}", body.len(), body)
    } else {
        writeln!(writer, "{body}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsString;

    fn test_state(root: &Path, mode: &str) -> State {
        State {
            mode: mode.to_string(),
            allowed_roots: vec![root.to_path_buf()],
            root_entries: Vec::new(),
            output_root: root.to_path_buf(),
            audit_log_dir: None,
            cache: HashMap::new(),
            snapshots: HashMap::new(),
            snapshot_order: Vec::new(),
        }
    }

    fn test_root(name: &str) -> PathBuf {
        let root = env::temp_dir().join(format!("narada-fs-{name}-{}", std::process::id()));
        if root.exists() {
            fs::remove_dir_all(&root).expect("remove stale exact test root");
        }
        fs::create_dir_all(&root).expect("create test root");
        root
    }

    fn fake_env(entries: &[(&str, &str)]) -> HashMap<String, OsString> {
        entries
            .iter()
            .map(|(key, value)| ((*key).to_string(), OsString::from(*value)))
            .collect()
    }

    #[test]
    fn user_home_anchor_prefers_non_empty_userprofile() {
        let values = fake_env(&[
            ("USERPROFILE", r"C:\Users\andrey"),
            ("HOME", r"C:\Users\fallback"),
        ]);
        assert_eq!(
            user_home_anchor_from(|key| values.get(key).cloned()),
            Some(PathBuf::from(r"C:\Users\andrey"))
        );
    }

    #[test]
    fn user_home_anchor_uses_home_when_userprofile_is_missing() {
        let values = fake_env(&[("HOME", r"C:\Users\andrey")]);
        assert_eq!(
            user_home_anchor_from(|key| values.get(key).cloned()),
            Some(PathBuf::from(r"C:\Users\andrey"))
        );
    }

    #[test]
    fn logical_range_reads_page_without_refusal_and_publish_same_tool_continuation() {
        let root = test_root("logical-range-pagination");
        let path = root.join("large.txt");
        let content = (1..=2_505)
            .map(|line| format!("line-{line}"))
            .collect::<Vec<_>>()
            .join("\n");
        fs::write(&path, content).unwrap();
        let state = test_state(&root, "read");

        let first = read_file(
            &state,
            &json!({"path":path,"start_line":1,"end_line":3_000}),
            true,
        )
        .expect("a large logical range must return its first bounded page");
        assert_eq!(first["returned_lines"], 1_000);
        assert_eq!(first["requested_limit"], 3_000);
        assert_eq!(first["limit"], 1_000);
        assert_eq!(first["limit_adjusted"], true);
        assert_eq!(first["has_more"], true);
        assert_eq!(first["next_start_line"], 1_001);
        assert_eq!(first["continuation"]["tool"], "fs_read_file_range");
        assert_eq!(first["continuation"]["arguments"]["start_line"], 1_001);
        assert_eq!(first["continuation"]["arguments"]["end_line"], 3_000);

        let second = read_file(&state, &first["continuation"]["arguments"], true)
            .expect("continuation arguments must be directly reusable");
        assert_eq!(second["returned_lines"], 1_000);
        assert_eq!(second["next_start_line"], 2_001);

        let third = read_file(&state, &second["continuation"]["arguments"], true)
            .expect("the final page must stop cleanly at end of file");
        assert_eq!(third["returned_lines"], 505);
        assert_eq!(third["has_more"], false);
        assert_eq!(third["requested_range_complete"], true);
        assert!(third["continuation"].is_null());

        let range_tool = list_tools("read")
            .into_iter()
            .find(|tool| tool["name"] == "fs_read_file_range")
            .expect("range tool must be published");
        assert!(range_tool["description"]
            .as_str()
            .unwrap()
            .contains("continuation.arguments"));
        fs::remove_dir_all(&root).unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn user_home_anchor_uses_home_drive_and_path_fallback() {
        let values = fake_env(&[("HOMEDRIVE", "C:"), ("HOMEPATH", r"\Users\andrey")]);
        assert_eq!(
            user_home_anchor_from(|key| values.get(key).cloned()),
            Some(PathBuf::from(r"C:\Users\andrey"))
        );
    }

    #[cfg(windows)]
    #[test]
    fn user_home_anchor_uses_appdata_parent_fallback() {
        let values = fake_env(&[("APPDATA", r"C:\Users\andrey\AppData\Roaming")]);
        assert_eq!(
            user_home_anchor_from(|key| values.get(key).cloned()),
            Some(PathBuf::from(r"C:\Users\andrey"))
        );
    }

    #[test]
    fn repository_inventory_honors_root_alias_and_normalizes_paths() {
        let root = test_root("inventory-root-alias");
        let scope = root.join("scope");
        fs::create_dir_all(&scope).unwrap();
        fs::write(scope.join("only.txt"), "needle\n").unwrap();
        fs::write(root.join("outside.txt"), "outside\n").unwrap();
        let mut state = test_state(&root, "read");

        let result = repository_inventory(
            &mut state,
            &json!({"root": scope, "pattern": "**/*", "limit": 10, "cache_policy": "refresh"}),
        )
        .unwrap();
        let paths = result["candidate_source_paths"].as_array().unwrap();
        assert_eq!(paths.len(), 1);
        assert!(paths[0].as_str().unwrap().ends_with("/scope/only.txt"));
        assert!(!paths[0].as_str().unwrap().contains('\\'));
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn explicit_snapshot_is_reported_as_reused_cache() {
        let root = test_root("snapshot-reuse");
        fs::write(root.join("one.txt"), "one\n").unwrap();
        let mut state = test_state(&root, "read");
        let first = search_tool(
            &mut state,
            &json!({"directory": root, "pattern": "**/*", "limit": 1, "cache_policy": "refresh"}),
            false,
        )
        .unwrap();
        assert_eq!(first["cache_hit"], false);
        let second = search_tool(
            &mut state,
            &json!({"directory": root, "pattern": "**/*", "limit": 1, "snapshot_id": first["snapshot_id"]}),
            false,
        )
        .unwrap();
        assert_eq!(second["cache_hit"], true);
        assert_eq!(second["snapshot_reused"], true);
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn environment_variable_paths_are_refused_before_resolution() {
        let root = test_root("environment-path");
        let state = test_state(&root, "read");
        let error = resolve_allowed(&state, Some("%USERPROFILE%/src"), "fs_stat").unwrap_err();
        assert_eq!(error.code, "path_environment_expansion_not_supported");
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn guidance_recommends_the_native_patch_recovery_sequence() {
        let root = test_root("guidance");
        let state = test_state(&root, "write");
        let result = guidance(&state, &json!({"workflow": "safe_edit"})).unwrap();
        assert_eq!(result["patch_recovery"]["apply_patch_available"], true);
        assert!(result["patch_recovery"]["sequence"]
            .to_string()
            .contains("Call fs_apply_patch once"));
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn mutation_results_expose_canonical_content_hash() {
        let root = test_root("mutation-hash");
        let state = test_state(&root, "write");
        let path = root.join("value.txt");
        let written = write_file(&state, &json!({"path": path, "content": "before\n"})).unwrap();
        assert_eq!(written["sha256"], written["after_sha256"]);
        assert_eq!(written["content_sha256"], written["after_sha256"]);
        let replaced = str_replace_file(
            &state,
            &json!({"path": path, "old": "before", "new": "after", "expected_sha256": written["sha256"]}),
        )
        .unwrap();
        assert_eq!(replaced["sha256"], replaced["after_sha256"]);
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn tool_text_is_a_compact_projection_of_structured_content() {
        let result = tool_result(json!({"schema": "example.v1", "status": "ok", "large": [1,2,3]}));
        let text = result["content"][0]["text"].as_str().unwrap();
        assert!(text.len() < 100);
        assert!(!text.contains("large"));
        assert_eq!(result["structuredContent"]["large"][2], 3);
    }

    #[test]
    fn every_filesystem_schema_is_named_closed_and_bounded() {
        for mode in ["read", "write"] {
            for tool in list_tools(mode) {
                let schema = &tool["inputSchema"];
                assert!(
                    schema["title"]
                        .as_str()
                        .is_some_and(|value| !value.is_empty()),
                    "{}",
                    tool["name"]
                );
                assert_eq!(schema["additionalProperties"], false, "{}", tool["name"]);
                for (field, property) in schema["properties"].as_object().unwrap() {
                    match property["type"].as_str().unwrap_or_default() {
                        "string" => assert!(
                            property["maxLength"].is_number(),
                            "{}:{field}",
                            tool["name"]
                        ),
                        "array" => {
                            assert!(property["maxItems"].is_number(), "{}:{field}", tool["name"])
                        }
                        "integer" => {
                            assert!(property["minimum"].is_number(), "{}:{field}", tool["name"]);
                            assert!(property["maximum"].is_number(), "{}:{field}", tool["name"]);
                        }
                        "object" => assert!(
                            property["maxProperties"].is_number(),
                            "{}:{field}",
                            tool["name"]
                        ),
                        _ => {}
                    }
                }
                assert!(tool["outputSchema"]["title"].is_string());
            }
        }
    }

    #[test]
    fn native_apply_patch_supports_codex_unified_replay_and_conflict() {
        let root = test_root("native-patch");
        let state = test_state(&root, "write");
        fs::write(root.join("value.txt"), "one\ntwo\n").unwrap();
        let codex = "*** Begin Patch\n*** Update File: value.txt\n@@\n-one\n+ONE\n two\n*** Add File: added.txt\n+added\n*** End Patch";
        let first =
            apply_patch_tool(&state, &json!({"patch":codex,"operation_id":"patch-one"})).unwrap();
        assert_eq!(first["status"], "patched");
        assert_eq!(
            fs::read_to_string(root.join("value.txt")).unwrap(),
            "ONE\ntwo\n"
        );
        assert_eq!(
            fs::read_to_string(root.join("added.txt")).unwrap(),
            "added\n"
        );
        let replay =
            apply_patch_tool(&state, &json!({"patch":codex,"operation_id":"patch-one"})).unwrap();
        assert_eq!(replay["operation_replayed"], true);
        assert_eq!(apply_patch_tool(&state, &json!({"patch":"*** Begin Patch\n*** Delete File: added.txt\n*** End Patch","operation_id":"patch-one"})).unwrap_err().code, "patch_operation_id_conflict");
        let unified = "--- a/value.txt\n+++ b/value.txt\n@@ -1,2 +1,2 @@\n ONE\n-two\n+TWO";
        let checked = apply_patch_tool(
            &state,
            &json!({"patch":unified,"operation_id":"patch-two","dry_run":true}),
        )
        .unwrap();
        assert_eq!(checked["status"], "checked");
        assert_eq!(
            fs::read_to_string(root.join("value.txt")).unwrap(),
            "ONE\ntwo\n"
        );
        assert_eq!(
            patch_outcome(&state, &json!({"operation_id":"patch-two"})).unwrap()["status"],
            "checked"
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn native_filesystem_rejects_unknown_and_unbounded_arguments() {
        assert_eq!(
            validate_tool_arguments(
                "write",
                "fs_write_file",
                &json!({"path":"x","content":"y","surprise":true})
            )
            .unwrap_err()
            .code,
            "tool_argument_unknown"
        );
        assert_eq!(
            validate_tool_arguments("read", "fs_read_file", &json!({"path":"x","limit":300_001}))
                .unwrap_err()
                .code,
            "tool_argument_integer_out_of_range"
        );
        assert_eq!(
            validate_tool_arguments(
                "write",
                "fs_apply_patch",
                &json!({"patch":"x","operation_id":"bad/id"})
            )
            .unwrap_err()
            .code,
            "patch_operation_id_invalid"
        );
    }

    #[test]
    fn native_apply_patch_moves_deletes_guards_and_records_recovery() {
        let root = test_root("native-patch-boundaries");
        let state = test_state(&root, "write");
        fs::write(root.join("move.txt"), "move me\n").unwrap();
        let moved = apply_patch_tool(&state, &json!({
            "patch":"*** Begin Patch\n*** Update File: move.txt\n*** Move to: moved.txt\n@@\n-move me\n+moved\n*** End Patch",
            "operation_id":"move-patch","expected_sha256":{"move.txt":sha256_bytes(b"move me\n")}
        })).unwrap();
        assert_eq!(moved["status"], "patched");
        assert!(!root.join("move.txt").exists());
        assert_eq!(
            fs::read_to_string(root.join("moved.txt")).unwrap(),
            "moved\n"
        );
        let deleted = apply_patch_tool(&state, &json!({
            "patch":"*** Begin Patch\n*** Delete File: moved.txt\n*** End Patch","operation_id":"delete-patch"
        })).unwrap();
        assert_eq!(deleted["changed_files"][0]["operation"], "delete");
        assert!(!root.join("moved.txt").exists());

        fs::write(root.join("guard.txt"), "guard\n").unwrap();
        let error = apply_patch_tool(&state, &json!({
            "patch":"*** Begin Patch\n*** Update File: guard.txt\n@@\n-guard\n+changed\n*** End Patch",
            "operation_id":"guard-patch","expected_sha256":{"guard.txt":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"}
        })).unwrap_err();
        assert_eq!(error.code, "fs_apply_patch_expected_sha256_mismatch");
        assert_eq!(
            fs::read_to_string(root.join("guard.txt")).unwrap(),
            "guard\n"
        );
        let recovered = patch_outcome(&state, &json!({"operation_id":"guard-patch"})).unwrap();
        assert_eq!(recovered["status"], "failed_before_mutation");
        assert_eq!(recovered["retry_safe"], true);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn native_patch_reconciles_interrupted_applying_state() {
        let root = test_root("native-patch-recovery");
        let state = test_state(&root, "write");
        let path = root.join("recovered.txt");
        fs::write(&path, "after\n").unwrap();
        write_patch_outcome(&state,"recover-applying",&json!({
            "schema":"local.filesystem.apply_patch.outcome.v1","status":"applying","operation_id":"recover-applying",
            "patch_sha256":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","owner_pid":4294967294_u32,
            "recovery_plan":{"before_state":[{"path":path,"exists":true,"sha256":sha256_bytes(b"before\n")}],"after_state":[{"path":path,"exists":true,"sha256":sha256_bytes(b"after\n")}],"changed_files":[]}
        })).unwrap();
        let outcome = patch_outcome(&state, &json!({"operation_id":"recover-applying"})).unwrap();
        assert_eq!(outcome["status"], "patched_recovered");
        assert_eq!(outcome["retry_safe"], false);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn native_reads_and_search_snapshots_have_hard_memory_bounds() {
        let root = test_root("bounded-memory");
        let mut state = test_state(&root, "read");
        let path = root.join("huge-line.txt");
        fs::write(&path, vec![b'x'; MAX_READ_LINE_BYTES + 1]).unwrap();
        assert_eq!(
            read_file(&state, &json!({"path":path,"limit":1}), false)
                .unwrap_err()
                .code,
            "fs_read_line_too_large"
        );
        for index in 0..5 {
            let id = format!("snapshot-{index}");
            state.snapshots.insert(id.clone(), (vec![id.clone()], true));
            touch_snapshot(&mut state, &id);
        }
        assert_eq!(state.snapshots.len(), 4);
        assert!(!state.snapshots.contains_key("snapshot-0"));
        state
            .snapshots
            .insert("truncated".into(), (vec!["one".into()], false));
        let boundary = search_tool(
            &mut state,
            &json!({
                "directory": root,
                "pattern": "*",
                "snapshot_id": "truncated",
                "offset": 1
            }),
            false,
        )
        .unwrap_err();
        assert_eq!(boundary.code, "fs_glob_search_capture_boundary_reached");
        let outcome = list_tools("read")
            .into_iter()
            .find(|tool| tool["name"] == "fs_patch_outcome_show")
            .unwrap();
        assert_eq!(outcome["annotations"]["readOnlyHint"], false);
        fs::remove_dir_all(root).unwrap();
    }
}
