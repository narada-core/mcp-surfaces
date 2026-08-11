use crate::protocol;
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::env;
use std::fs;
use std::fs::OpenOptions;
use std::io::{self, BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;

const PROTOCOL_VERSION: &str = "2024-11-05";
const READ_TIMEOUT_MS: u64 = 5_000;
const WRITE_TIMEOUT_MS: u64 = 10_000;
const SEARCH_TIMEOUT_MS: u64 = 60_000;
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
    cache: HashMap<String, (String, Vec<String>)>,
    snapshots: HashMap<String, Vec<String>>,
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
    if let Some(path) = roots_config {
        roots.extend(parse_roots_config(Path::new(&path)));
    }
    for spec in anchored {
        roots.push(resolve_anchor(&spec)?);
    }
    if mode != "read" && mode != "write" {
        return Err("filesystem_mode_must_be_read_or_write".to_string());
    }
    let mut entries = Vec::new();
    let mut allowed_roots = Vec::new();
    for root in roots {
        let path = absolute(PathBuf::from(root));
        let key = normalize_path(&path);
        if allowed_roots
            .iter()
            .any(|value: &PathBuf| normalize_path(value) == key)
        {
            continue;
        }
        entries.push(json!({"root": path.to_string_lossy(), "provenance": {"source": "explicit_flag", "flag": "--allowed-root"}}));
        allowed_roots.push(path);
    }
    if allowed_roots.is_empty() {
        return Err("filesystem_mcp_requires_at_least_one_allowed_root".to_string());
    }
    Ok(State {
        mode,
        allowed_roots,
        root_entries: entries,
        output_root: absolute(
            output_root
                .map(PathBuf::from)
                .unwrap_or_else(|| env::current_dir().unwrap_or_else(|_| PathBuf::from("."))),
        ),
        audit_log_dir: audit_log_dir.map(|value| absolute(PathBuf::from(value))),
        cache: HashMap::new(),
        snapshots: HashMap::new(),
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
    if request.get("id").is_none() {
        return None;
    }
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
        "completion/complete" => {
            Ok(json!({"completion": {"values": [], "total": 0, "hasMore": false}}))
        }
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

fn initialize(request: &Value, mode: &str) -> Value {
    json!({
        "protocolVersion": request.get("params").and_then(|value| value.get("protocolVersion")).cloned().unwrap_or(json!(PROTOCOL_VERSION)),
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

fn call_tool(state: &mut State, params: &Value) -> Result<Value, FsError> {
    let name = params.get("name").and_then(Value::as_str).ok_or_else(|| {
        FsError::new(
            "tools_call_requires_name",
            "tools_call_requires_name",
            json!({}),
        )
    })?;
    let args = params.get("arguments").unwrap_or(&Value::Null);
    if is_write_tool(name) && state.mode != "write" {
        return Err(FsError::new(
            format!("tool_not_available_in_{}_mode", state.mode),
            format!("tool_not_available_in_{}_mode: {name}", state.mode),
            json!({"tool_name": name, "mode": state.mode}),
        ));
    }
    let value = match name {
        "fs_guidance" => guidance(args),
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
    Ok(tool_result(value))
}

fn is_write_tool(name: &str) -> bool {
    matches!(
        name,
        "fs_write_file"
            | "fs_str_replace_file"
            | "fs_replace_range"
            | "fs_move_path"
            | "fs_create_directory"
            | "fs_rename_directory"
            | "fs_delete_directory"
    )
}

fn tool_result(value: Value) -> Value {
    let text = serde_json::to_string_pretty(&value).unwrap_or_else(|_| "{}".to_string());
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

fn guidance(args: &Value) -> Result<Value, FsError> {
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
            "sequence": ["Choose a stable operation_id.", "Call fs_apply_patch once.", "After timeout call fs_patch_outcome_show.", "Retry only when retry_safe is true."],
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
        "effective_permissions": {"can_read": true, "can_write": can_write, "can_mutate_paths": can_write, "can_delete_directories": false},
        "available_tools": available_tools,
        "read_tools": read_tools,
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
    let (offset, limit) = if range {
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
        if requested > 1000 {
            return Err(FsError::new(
                "fs_read_file_range_limit_exceeds_max",
                "fs_read_file_range_limit_exceeds_max",
                json!({"start_line": start, "end_line": end, "requested_limit": requested, "max_limit": 1000, "pagination_required": true, "mutation_started": false}),
            ));
        }
        (start, requested)
    } else {
        let requested = integer(args, "limit").unwrap_or(400).max(1);
        if requested > 1000 {
            return Err(FsError::new(
                "fs_read_file_limit_exceeds_max",
                "fs_read_file_limit_exceeds_max",
                json!({"offset": integer(args, "offset").unwrap_or(1).max(1), "requested_limit": requested, "max_limit": 1000, "pagination_required": true, "mutation_started": false}),
            ));
        }
        (integer(args, "offset").unwrap_or(1).max(1), requested)
    };
    let timeout = integer(args, "timeout_ms")
        .unwrap_or(READ_TIMEOUT_MS as i64)
        .max(1)
        .min(60_000) as u64;
    let started = std::time::Instant::now();
    let bytes = fs::read(&path).map_err(|error| {
        FsError::new(
            "fs_read_file_failed",
            format!("fs_read_file_failed: {error}"),
            path_details(&path, &root),
        )
    })?;
    if bytes.contains(&0) {
        return Err(FsError::new(
            "binary_file_not_supported",
            format!("binary_file_not_supported: {}", path.display()),
            path_details(&path, &root),
        ));
    }
    if started.elapsed().as_millis() as u64 > timeout {
        return Err(FsError::new(
            if range {
                "fs_read_file_range_timed_out"
            } else {
                "fs_read_file_timed_out"
            },
            "filesystem read timed out",
            json!({"timeout_ms": timeout, "offset": offset, "limit": limit, "path": path, "root": root}),
        ));
    }
    let text = String::from_utf8_lossy(&bytes).replace("\r\n", "\n");
    let mut lines: Vec<&str> = text.split('\n').collect();
    if lines.last() == Some(&"") {
        lines.pop();
    }
    let start_index = offset.saturating_sub(1) as usize;
    let end_index = (start_index + limit as usize).min(lines.len());
    let selected = if start_index < lines.len() {
        &lines[start_index..end_index]
    } else {
        &[]
    };
    let content = selected.join("\n");
    let complete = end_index >= lines.len();
    let next_offset = if complete {
        None
    } else {
        Some((end_index + 1) as i64)
    };
    let (total_lines, total_lines_exact, line_window_complete) = if complete {
        (json!(lines.len()), true, true)
    } else {
        (Value::Null, false, false)
    };
    let content_hash = sha256_bytes(&bytes);
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
        "returned_lines": selected.len(),
        "next_offset": next_offset,
        "content": content,
        "content_sha256": content_hash,
        "content_hash_scope": "full_file",
        "hash_source": "live_file_bytes",
        "cache_used": false,
        "content_window_sha256": sha256_bytes(content.as_bytes()),
        "max_limit": 1000,
        "limit_adjusted": false,
        "pagination_required": next_offset.is_some(),
        "timeout_ms": timeout
    }))
}

fn write_file(state: &State, args: &Value) -> Result<Value, FsError> {
    if args.get("payload_ref").is_some() || args.get("payload_path").is_some() {
        return Err(FsError::new(
            "payload_transport_not_supported_in_native_write",
            "payload_transport_not_supported_in_native_write",
            json!({"supported": ["content"]}),
        ));
    }
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
        .max(1)
        .min(300_000) as u64;
    let started = std::time::Instant::now();

    let before_bytes = match fs::metadata(&path) {
        Ok(metadata) if metadata.is_file() => Some(fs::read(&path).map_err(|error| {
            FsError::new(
                "fs_write_file_read_failed",
                format!("fs_write_file_read_failed: {error}"),
                path_details(&path, &root),
            )
        })?),
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
    let before_sha256 = before_bytes.as_ref().map(|bytes| sha256_bytes(bytes));
    if create_only && before_bytes.is_some() {
        return Err(FsError::new(
            "write_file_destination_exists",
            "write_file_destination_exists",
            path_details(&path, &root),
        ));
    }
    if !overwrite && before_bytes.is_some() {
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
    let before = fs::read_to_string(&path).map_err(|error| {
        FsError::new(
            "fs_str_replace_file_read_failed",
            format!("fs_str_replace_file_read_failed: {error}"),
            path_details(&path, &root),
        )
    })?;
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
        json!({"schema": "local.filesystem.str_replace_file.v1", "status": "replaced", "path": path, "root": root, "relative_path": relative_path(&root, &path), "occurrences": 1, "before_sha256": before_sha256, "after_sha256": after_sha256}),
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
    let before = fs::read_to_string(&path).map_err(|error| {
        FsError::new(
            "fs_replace_range_read_failed",
            format!("fs_replace_range_read_failed: {error}"),
            path_details(&path, &root),
        )
    })?;
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
        json!({"schema": "local.filesystem.replace_range.v1", "status": "replaced_range", "path": path, "root": root, "relative_path": relative_path(&root, &path), "start_line": start, "end_line": end, "inserted_lines": replacement_lines.len(), "before_sha256": before_sha256, "after_sha256": after_sha256}),
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
    let expected_sha = value("sha256").and_then(Value::as_str);
    let expected_tree = value("tree_sha256").and_then(Value::as_str);
    let expected_entries = value("entry_count").and_then(Value::as_u64);
    if expected_size.is_none()
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
    let (actual_tree, actual_entries) = if metadata.is_dir() {
        let (entries, _tree_entries, tree, _truncated) = directory_fingerprint(path, path);
        (Some(tree), Some(entries as u64))
    } else {
        (None, None)
    };
    let details = json!({"operation": operation, "path": path, "root": root, "expected_size": expected_size, "actual_size": actual_size, "expected_sha256": expected_sha, "expected_tree_sha256": expected_tree, "actual_tree_sha256": actual_tree, "expected_entry_count": expected_entries, "actual_entry_count": actual_entries});
    if expected_size.is_some_and(|expected| expected != actual_size)
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
        let actual = sha256_bytes(&fs::read(path).map_err(|error| {
            FsError::new(
                format!("{operation}_expected_metadata_mismatch"),
                error.to_string(),
                path_details(path, root),
            )
        })?);
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
        if let Ok(bytes) = fs::read(&path) {
            value.insert("sha256".into(), json!(sha256_bytes(&bytes)));
        }
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
        .max(1)
        .min(500) as usize;
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
    let mut cached_snapshot: Option<String> = None;
    let all_matches = if let Some(snapshot) = snapshot_id {
        state.snapshots.get(snapshot).cloned().ok_or_else(|| {
            FsError::new(
                format!("{operation}_snapshot_not_found"),
                format!("{operation}_snapshot_not_found: {snapshot}"),
                json!({"snapshot_id": snapshot}),
            )
        })?
    } else if cache_policy != "bypass" && cache_policy != "refresh" {
        if let Some((id, matches)) = state.cache.get(&cache_key).cloned() {
            cache_hit = true;
            cached_snapshot = Some(id);
            matches
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
        let digest = sha256_bytes(all_matches.join("\n").as_bytes());
        let id = format!("s_{}", &digest[..24]);
        state
            .cache
            .insert(cache_key, (id.clone(), all_matches.clone()));
        state.snapshots.insert(id.clone(), all_matches.clone());
        Some(id)
    } else {
        snapshot_id.map(str::to_string)
    };
    if cache_policy == "auto" && snapshot_id.is_none() && !cache_hit {
        cache_hit = true;
    }
    let page: Vec<String> = all_matches
        .iter()
        .skip(offset)
        .take(limit)
        .cloned()
        .collect();
    let has_more = offset + page.len() < all_matches.len();
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
    value.insert("count_exact".into(), json!(true));
    value.insert("scanned".into(), json!(all_matches.len()));
    value.insert("scanned_unit".into(), json!("matched_entries"));
    value.insert("returned".into(), json!(page.len()));
    value.insert("order".into(), json!("ripgrep_traversal"));
    value.insert("cache_hit".into(), json!(cache_hit));
    value.insert("cache_policy".into(), json!(cache_policy));
    value.insert(
        "snapshot_id".into(),
        snapshot.clone().map(Value::String).unwrap_or(Value::Null),
    );
    value.insert(
        "requested_snapshot_id".into(),
        snapshot_id.map(|value| json!(value)).unwrap_or(Value::Null),
    );
    value.insert("snapshot_complete".into(), json!(true));
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

fn run_search_command(
    scope: &Path,
    pattern: &str,
    args: &Value,
    grep: bool,
    output_mode: &str,
    operation: &str,
) -> Result<Vec<String>, FsError> {
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
    run_rg(
        &rg_args,
        args.get("timeout_ms")
            .and_then(Value::as_u64)
            .unwrap_or(SEARCH_TIMEOUT_MS),
        operation,
    )
}

fn repository_inventory(state: &mut State, args: &Value) -> Result<Value, FsError> {
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
        json!(args.get("directory").and_then(Value::as_str).unwrap_or(".")),
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
    let limit = integer(args, "limit").unwrap_or(100).max(1).min(100) as usize;
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

fn patch_outcome(state: &State, args: &Value) -> Result<Value, FsError> {
    let operation = args
        .get("operation_id")
        .and_then(Value::as_str)
        .ok_or_else(|| FsError::new("operation_id_required", "operation_id_required", json!({})))?;
    let path = state
        .output_root
        .join(".narada")
        .join("local-filesystem-mcp")
        .join("patch-outcomes")
        .join(format!("{operation}.json"));
    let text = fs::read_to_string(&path).map_err(|_| {
        FsError::new(
            "fs_patch_outcome_not_found",
            format!("fs_patch_outcome_not_found: {operation}"),
            json!({"operation_id": operation, "path": path}),
        )
    })?;
    serde_json::from_str(&text).map_err(|_| {
        FsError::new(
            "fs_patch_outcome_invalid",
            "fs_patch_outcome_invalid",
            json!({"operation_id": operation, "path": path}),
        )
    })
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
            "Read a text file line range under an allowed root. Lines are 1-based and inclusive.",
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
            "fs_read_file" => { properties.insert("path".into(), json!({"type":"string"})); properties.insert("offset".into(), json!({"type":"integer","default":1})); properties.insert("limit".into(), json!({"type":"integer","default":400})); properties.insert("timeout_ms".into(), json!({"type":"integer"})); }
            "fs_read_file_range" => { properties.insert("path".into(), json!({"type":"string"})); properties.insert("start_line".into(), json!({"type":"integer"})); properties.insert("end_line".into(), json!({"type":"integer"})); properties.insert("timeout_ms".into(), json!({"type":"integer"})); }
            "fs_stat" => { properties.insert("path".into(), json!({"type":"string"})); }
            "fs_glob_search" => { properties.insert("pattern".into(), json!({"type":"string"})); properties.insert("directory".into(), json!({"type":"string","default":"."})); properties.insert("ignore".into(), json!({"type":"array","items":{"type":"string"}})); properties.insert("offset".into(), json!({"type":"integer","default":0})); properties.insert("limit".into(), json!({"type":"integer","default":100})); properties.insert("timeout_ms".into(), json!({"type":"integer"})); properties.insert("cache_policy".into(), json!({"type":"string","enum":["auto","snapshot","refresh","bypass"],"default":"auto"})); properties.insert("snapshot_id".into(), json!({"type":"string"})); }
            "fs_grep_search" => { properties.insert("pattern".into(), json!({"type":"string"})); properties.insert("path".into(), json!({"type":"string","default":"."})); properties.insert("output_mode".into(), json!({"type":"string","enum":["files_with_matches","count_matches","content"],"default":"files_with_matches"})); properties.insert("ignore".into(), json!({"type":"array","items":{"type":"string"}})); properties.insert("offset".into(), json!({"type":"integer","default":0})); properties.insert("limit".into(), json!({"type":"integer","default":80})); properties.insert("timeout_ms".into(), json!({"type":"integer"})); properties.insert("cache_policy".into(), json!({"type":"string","enum":["auto","snapshot","refresh","bypass"],"default":"auto"})); properties.insert("snapshot_id".into(), json!({"type":"string"})); }
            "fs_repository_inventory" => { properties.insert("pattern".into(), json!({"type":"string","default":"**/*"})); properties.insert("directory".into(), json!({"type":"string","default":"."})); properties.insert("ignore".into(), json!({"type":"array","items":{"type":"string"}})); properties.insert("include_generated".into(), json!({"type":"boolean","default":false})); properties.insert("offset".into(), json!({"type":"integer","default":0})); properties.insert("limit".into(), json!({"type":"integer","default":100})); properties.insert("timeout_ms".into(), json!({"type":"integer"})); properties.insert("cache_policy".into(), json!({"type":"string","enum":["auto","snapshot","refresh","bypass"],"default":"auto"})); properties.insert("snapshot_id".into(), json!({"type":"string"})); }
            "fs_file_metrics" => { properties.insert("pattern".into(), json!({"type":"string","default":"**/*"})); properties.insert("directory".into(), json!({"type":"string","default":"."})); properties.insert("root".into(), json!({"type":"string"})); properties.insert("ignore".into(), json!({"type":"array","items":{"type":"string"}})); properties.insert("exclude".into(), json!({"type":"array","items":{"type":"string"}})); properties.insert("offset".into(), json!({"type":"integer","default":0})); properties.insert("limit".into(), json!({"type":"integer","default":100})); properties.insert("max_bytes_per_file".into(), json!({"type":"integer"})); properties.insert("max_total_scan_bytes".into(), json!({"type":"integer"})); properties.insert("timeout_ms".into(), json!({"type":"integer"})); properties.insert("cache_policy".into(), json!({"type":"string","enum":["auto","snapshot","refresh","bypass"],"default":"auto"})); properties.insert("snapshot_id".into(), json!({"type":"string"})); }
            "fs_patch_outcome_show" => { properties.insert("operation_id".into(), json!({"type":"string"})); }
            "fs_write_file" => {
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
            "fs_move_path" => {
                properties.insert("from".into(), json!({"type":"string"}));
                properties.insert("to".into(), json!({"type":"string"}));
                properties.insert("overwrite".into(), json!({"type":"boolean","default":false}));
                properties.insert("expected_from_size".into(), json!({"type":"integer"}));
                properties.insert("expected_from_sha256".into(), json!({"type":"string"}));
                properties.insert("expected_to_size".into(), json!({"type":"integer"}));
                properties.insert("expected_to_sha256".into(), json!({"type":"string"}));
            }
            "fs_create_directory" => {
                properties.insert("path".into(), json!({"type":"string"}));
                properties.insert("recursive".into(), json!({"type":"boolean","default":false}));
            }
            "fs_rename_directory" => {
                properties.insert("from".into(), json!({"type":"string"}));
                properties.insert("to".into(), json!({"type":"string"}));
            }
            "fs_delete_directory" => {
                properties.insert("path".into(), json!({"type":"string"}));
                properties.insert("recursive".into(), json!({"type":"boolean","default":false}));
                properties.insert("expected_size".into(), json!({"type":"integer"}));
                properties.insert("expected_tree_sha256".into(), json!({"type":"string"}));
                properties.insert("expected_entry_count".into(), json!({"type":"integer"}));
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
            "fs_move_path" => vec!["from", "to"],
            "fs_create_directory" => vec!["path"],
            "fs_rename_directory" => vec!["from", "to"],
            "fs_delete_directory" => vec!["path"],
            _ => Vec::new()
        };
        let write_tool = is_write_tool(name);
        json!({"name": name, "canonical_name": name, "description": description, "inputSchema": {"type":"object","properties": properties,"required": required,"additionalProperties":false}, "annotations": {"title":name,"readOnlyHint":!write_tool,"destructiveHint":write_tool,"idempotentHint":true,"openWorldHint":false}, "outputSchema":{"type":"object","additionalProperties":true}})
    }).collect()
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
                if let Ok(bytes) = fs::read(path) {
                    value.insert("sha256".into(), json!(sha256_bytes(&bytes)));
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

fn run_rg(args: &[String], timeout: u64, operation: &str) -> Result<Vec<String>, FsError> {
    let started = std::time::Instant::now();
    let output = Command::new("rg").args(args).output().map_err(|error| {
        FsError::new(
            format!("{operation}_failed"),
            format!("{operation}_failed: {error}"),
            json!({"operation": operation}),
        )
    })?;
    if started.elapsed().as_millis() as u64 > timeout {
        return Err(FsError::new(
            format!("{operation}_timed_out"),
            format!("{operation}_timed_out"),
            json!({"operation": operation, "timeout_ms": timeout}),
        ));
    }
    if output.status.code().unwrap_or(2) > 1 {
        return Err(FsError::new(
            format!("{operation}_failed"),
            format!(
                "{operation}_failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            ),
            json!({"operation": operation, "status": output.status.code()}),
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| line.trim_end_matches('\r').to_string())
        .collect())
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
    let bytes = fs::read(path)?;
    if bytes.contains(&0) {
        return Ok((0, true));
    }
    let text = String::from_utf8_lossy(&bytes).replace("\r\n", "\n");
    let mut lines: Vec<&str> = text.split('\n').collect();
    if lines.last() == Some(&"") {
        lines.pop();
    }
    Ok((lines.len(), false))
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
        write!(
            writer,
            "Content-Length: {}\r\n\r\n{}",
            body.as_bytes().len(),
            body
        )
    } else {
        writeln!(writer, "{body}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsString;

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
}
