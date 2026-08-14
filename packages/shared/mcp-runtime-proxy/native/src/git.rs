use crate::filesystem::{parse_site_extra_allowed_roots, read_message, write_message};
use crate::protocol;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::env;
use std::fs;
use std::io::{self, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    mpsc, Arc, Mutex,
};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use time::{format_description::well_known::Rfc3339, Duration as TimeDuration, OffsetDateTime};

const PROTOCOL_VERSION: &str = "2024-11-05";
const DEFAULT_MAX_TIMEOUT_MS: u64 = 60_000;
const DEFAULT_MAX_OUTPUT_BYTES: usize = 2 * 1024 * 1024;
const MAX_OUTPUT_BYTES: usize = 20 * 1024 * 1024;
const PREVIEW_CHAR_LIMIT: usize = 1_000;
const WORK_SCOPE_TTL_MINUTES: i64 = 15;

#[derive(Clone)]
struct WorkScope {
    reference: String,
    repository_root: String,
    allowed_paths: Vec<String>,
    base_state: Value,
    created_at: String,
    expires_at: OffsetDateTime,
}

#[derive(Clone)]
struct State {
    mode: String,
    allowed_roots: Vec<PathBuf>,
    max_timeout_ms: u64,
    max_output_bytes: usize,
    output_root: PathBuf,
    env: HashMap<String, String>,
    work_scopes: Arc<Mutex<HashMap<String, WorkScope>>>,
    git_write_lock: Arc<Mutex<()>>,
}

#[derive(Debug)]
struct GitError {
    code: String,
    message: String,
    details: Value,
}

impl GitError {
    fn new(code: impl Into<String>, message: impl Into<String>, details: Value) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
            details,
        }
    }
}

#[derive(Clone)]
struct GitResult {
    exit_code: Option<i32>,
    output_text: String,
    diagnostic_text: String,
    timed_out: bool,
    cancelled: bool,
    output_truncated: bool,
    diagnostic_truncated: bool,
}

enum Event {
    Request(Value, bool),
    Response(Value, bool, String),
    InputClosed,
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
    let mut active = HashMap::<String, Arc<AtomicBool>>::new();
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
                if let Some(response) = protocol::preflight_response(&request, "git-mcp") {
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
                        let response = protocol::modernize_response(&request, response, "git-mcp");
                        let _ = response_tx.send(Event::Response(response, framed, key));
                    });
                } else if let Some(response) = handle_request(&state, &request, None)
                    .map(|response| protocol::modernize_response(&request, response, "git-mcp"))
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

fn value_key(value: &Value) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| "null".to_string())
}

fn parse_state(args: &[String]) -> Result<State, String> {
    let mut mode = "read".to_string();
    let mut roots = Vec::new();
    let mut max_timeout_ms = DEFAULT_MAX_TIMEOUT_MS;
    let mut max_output_bytes = DEFAULT_MAX_OUTPUT_BYTES;
    let mut output_root = None;
    let mut index = 0;
    while index < args.len() {
        let flag = args[index].as_str();
        let value = |index: &mut usize, name: &str| -> Result<String, String> {
            *index += 1;
            args.get(*index)
                .cloned()
                .ok_or_else(|| format!("git_{name}_required"))
        };
        match flag {
            "--mode" => mode = value(&mut index, "mode")?,
            "--allowed-root" => roots.push(value(&mut index, "allowed_root")?),
            "--max-timeout-ms" => {
                max_timeout_ms = value(&mut index, "max_timeout_ms")?
                    .parse::<u64>()
                    .map_err(|_| "git_invalid_max_timeout_ms".to_string())?
                    .clamp(1, 300_000)
            }
            "--max-output-bytes" => {
                max_output_bytes = value(&mut index, "max_output_bytes")?
                    .parse::<usize>()
                    .map_err(|_| "git_invalid_max_output_bytes".to_string())?
                    .clamp(1, MAX_OUTPUT_BYTES)
            }
            "--output-root" => output_root = Some(value(&mut index, "output_root")?),
            "--help" => return Err("git_help".to_string()),
            other => return Err(format!("git_unknown_argument:{other}")),
        }
        index += 1;
    }
    if mode != "read" && mode != "write" {
        return Err("git_mode_must_be_read_or_write".to_string());
    }
    if roots.is_empty() {
        return Err("git_mcp_requires_at_least_one_allowed_root".to_string());
    }
    let output_root = absolute(PathBuf::from(
        output_root.unwrap_or_else(|| roots[0].clone()),
    ));
    roots.extend(parse_site_extra_allowed_roots(&output_root));
    let mut allowed_roots = Vec::new();
    for root in roots {
        let path = absolute(PathBuf::from(root));
        if !allowed_roots
            .iter()
            .any(|candidate: &PathBuf| path_key(candidate) == path_key(&path))
        {
            allowed_roots.push(path);
        }
    }
    Ok(State {
        mode,
        allowed_roots,
        max_timeout_ms,
        max_output_bytes,
        output_root,
        env: env::vars().collect(),
        work_scopes: Arc::new(Mutex::new(HashMap::new())),
        git_write_lock: Arc::new(Mutex::new(())),
    })
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
        "initialize" => Ok(
            json!({"protocolVersion": request.get("params").and_then(|params| params.get("protocolVersion")).cloned().unwrap_or(json!(PROTOCOL_VERSION)), "capabilities": {"tools": {}, "resources": {}, "prompts": {}, "completions": {}, "logging": {}}, "serverInfo": {"name": "git-mcp", "version": "0.1.0"}}),
        ),
        "tools/list" => Ok(json!({"tools": list_tools()})),
        "tools/call" => call_tool(state, params, cancellation),
        "resources/list" => Ok(json!({"resources": []})),
        "resources/read" => Err(GitError::new(
            "resource_not_found",
            "resource_not_found",
            json!({}),
        )),
        "prompts/list" => Ok(
            json!({"prompts": [{"name": "git_mcp_workflow", "title": "Git MCP Workflow", "description": "Guidance for branch, inspect, stage, commit, and push workflows.", "arguments": []}]}),
        ),
        "prompts/get" => prompt_get(params),
        "completion/complete" => {
            Ok(json!({"completion": {"values": [], "total": 0, "hasMore": false}}))
        }
        "logging/setLevel" => Ok(json!({})),
        _ => Err(GitError::new(
            "unsupported_mcp_method",
            format!("unsupported_mcp_method:{method}"),
            json!({"method": method}),
        )),
    };
    Some(match result {
        Ok(value) => json!({"jsonrpc": "2.0", "id": id, "result": value}),
        Err(error) => {
            json!({"jsonrpc": "2.0", "id": id, "error": {"code": -32000, "message": error.message, "data": {"schema": "narada.git.error.v1", "code": error.code, "message": error.message, "details": error.details}}})
        }
    })
}

fn list_tools() -> Vec<Value> {
    vec![
        tool(
            "git_guidance",
            "Guidance for governed Git inspection and publication workflows.",
            true,
        ),
        tool(
            "git_policy_inspect",
            "Inspect the policy governing Git MCP operations.",
            true,
        ),
        tool(
            "git_begin_work_scope",
            "Issue a short-lived explicit work-scope reference for declared paths and current base state without mutating Git.",
            true,
        ),
        tool(
            "git_workflow_record",
            "Record a bounded multi-repository workflow handoff in the local Git audit ledger.",
            false,
        ),
        tool(
            "git_add",
            "Stage explicit repository paths under the governed write mode and optional work scope.",
            false,
        ),
        tool(
            "git_unstage",
            "Remove explicit repository paths from the index under the governed write mode.",
            false,
        ),
        tool(
            "git_commit",
            "Create a commit from already staged changes under a required work scope.",
            false,
        ),
        tool(
            "git_push",
            "Push the current branch or an explicit remote and branch without force.",
            false,
        ),
        tool(
            "git_status",
            "Inspect branch, upstream, remotes, and bounded dirty-state summaries.",
            true,
        ),
        tool(
            "git_sync_status",
            "Inspect whether a rebase or merge is in progress.",
            true,
        ),
        tool(
            "git_branch_list",
            "List local and/or remote branches with object ids and upstream metadata.",
            true,
        ),
        tool(
            "git_output_show",
            "Read a materialized Git MCP output ref with offset/limit paging.",
            true,
        ),
        tool(
            "git_changed_summary",
            "Return a compact dirty-state summary without file diffs.",
            true,
        ),
        tool(
            "git_repositories_summary",
            "Summarize multiple repositories for handoff and publication checks.",
            true,
        ),
        tool(
            "git_diff",
            "Show a paged Git diff for working tree, staged changes, or one commit.",
            true,
        ),
        tool(
            "git_log",
            "List recent commits, optionally limited to one path.",
            true,
        ),
        tool(
            "git_show",
            "Show one commit with metadata and optional patch.",
            true,
        ),
    ]
}

fn tool(name: &str, description: &str, read_only: bool) -> Value {
    json!({"name": name, "description": description, "inputSchema": tool_input_schema(name), "annotations": {"title": name, "canonicalName": name, "readOnlyHint": read_only, "destructiveHint": false, "idempotentHint": read_only, "openWorldHint": false}, "outputSchema": {"type": "object", "additionalProperties": true}})
}

fn tool_input_schema(name: &str) -> Value {
    let working_directory = json!({"type": "string", "description": "Repository working directory under an allowed root; omit to use the first allowed root."});
    let path = json!({"type": "string", "description": "Repository-relative explicit path; absolute paths and parent traversal are refused."});
    let paths = json!({"type": "array", "items": path, "minItems": 1});
    let work_scope = json!({"type": "string", "description": "Live work-scope reference returned by git_begin_work_scope."});
    let schema = match name {
        "git_guidance" => {
            json!({"properties": {"workflow": {"type": "string"}, "tool": {"type": "string"}}})
        }
        "git_policy_inspect" => json!({"properties": {}}),
        "git_begin_work_scope" => {
            json!({"properties": {"working_directory": working_directory, "allowed_paths": paths, "base_state": {"type": "object", "additionalProperties": false, "properties": {"head": {"type": ["string", "null"]}, "index_digest": {"type": ["string", "null"]}}}}, "required": ["allowed_paths"]})
        }
        "git_workflow_record" => {
            json!({"properties": {"workflow_id": {"type": "string"}, "scope_label": {"type": "string"}, "summary": {"type":"object","additionalProperties":true,"maxProperties":64}, "repositories": {"type": "array", "items": {"type": "object", "additionalProperties":false,"properties":{"working_directory":working_directory,"label":{"type":"string"},"staged_paths":{"type":"array","items":path},"committed_sha":{"type":["string","null"]},"pushed":{"type":"boolean"},"push_status":{"type":"string","enum":["pushed","not_attempted","failed","not_pushable"]},"push_reason":{"type":["string","null"]},"unrelated_dirty_paths_left":{"type":"array","items":path}},"required":["working_directory"]}, "minItems": 1}}, "required": ["scope_label","repositories"]})
        }
        "git_add" | "git_unstage" => {
            json!({"properties": {"working_directory": working_directory, "paths": paths, "work_scope_ref": work_scope}, "required": ["paths"]})
        }
        "git_commit" => {
            json!({"properties": {"working_directory": working_directory, "message": {"type": "string", "minLength": 1}, "body": {"type": "string"}, "work_scope_ref": work_scope, "expected_staged_paths": paths}, "required": ["message", "work_scope_ref"]})
        }
        "git_push" => {
            json!({"properties": {"working_directory": working_directory, "remote": {"type": "string"}, "branch": {"type": "string"}, "expected_commit": {"type": "string", "description": "Expected SHA or git_commit:<sha>."}, "work_scope_ref": work_scope}, "required": ["work_scope_ref"]})
        }
        "git_status" => {
            json!({"properties": {"working_directory": working_directory, "work_scope_ref": work_scope, "pathspecs": paths, "staged_only": {"type": "boolean"}, "include_untracked": {"type": "boolean"}, "format": {"type": "string", "enum": ["full", "paths", "summary"]}}})
        }
        "git_sync_status" => json!({"properties": {"working_directory": working_directory}}),
        "git_branch_list" => {
            json!({"properties": {"working_directory": working_directory, "scope": {"type": "string", "enum": ["local", "remote", "all"]}}})
        }
        "git_output_show" => {
            json!({"properties": {"ref": {"type": "string"}, "output_ref": {"type": "string"}, "offset": {"type": "integer", "minimum": 0}, "limit": {"type": "integer", "minimum": 1, "maximum": 20000}}, "anyOf": [{"required": ["ref"]}, {"required": ["output_ref"]}]})
        }
        "git_changed_summary" => {
            json!({"properties": {"working_directory": working_directory, "pathspecs": paths, "relevance_filters": paths}})
        }
        "git_repositories_summary" => {
            json!({"properties": {"repositories": {"type": "array", "items": {"type": "object", "additionalProperties": false, "properties": {"working_directory": working_directory, "label": {"type": "string"}}, "required": ["working_directory"]}, "minItems": 1}}, "required": ["repositories"]})
        }
        "git_diff" => {
            json!({"properties": {"working_directory": working_directory, "scope": {"type": "string", "enum": ["working", "staged", "commit"]}, "commit": {"type": "string"}, "pathspec": path, "pathspecs": paths, "include_untracked": {"type": "boolean"}, "offset": {"type": "integer", "minimum": 0}, "limit": {"type": "integer", "minimum": 1, "maximum": 20000}}})
        }
        "git_log" => {
            json!({"properties": {"working_directory": working_directory, "limit": {"type": "integer", "minimum": 1, "maximum": 100}, "pathspec": path}})
        }
        "git_show" => {
            json!({"properties": {"working_directory": working_directory, "commit": {"type": "string"}, "pathspec": path, "include_patch": {"type": "boolean"}}, "required": ["commit"]})
        }
        _ => json!({"properties": {}}),
    };
    let mut object = schema;
    object["type"] = json!("object");
    object["additionalProperties"] = json!(false);
    object["title"] = json!(format!("{name}.input"));
    object["maxProperties"] = json!(64);
    bound_schema(&mut object, Some(name));
    object
}

fn bound_schema(schema: &mut Value, field: Option<&str>) {
    let Some(object) = schema.as_object_mut() else {
        return;
    };
    match object.get("type").and_then(Value::as_str) {
        Some("string") if !object.contains_key("maxLength") && !object.contains_key("enum") => {
            let maximum = if field.unwrap_or_default().contains("path")
                || field == Some("working_directory")
            {
                4096
            } else {
                8192
            };
            object.insert("maxLength".into(), json!(maximum));
        }
        Some("array") if !object.contains_key("maxItems") => {
            object.insert("maxItems".into(), json!(256));
        }
        Some("object") if !object.contains_key("maxProperties") => {
            object.insert("maxProperties".into(), json!(256));
        }
        _ => {}
    }
    if object
        .get("type")
        .and_then(Value::as_array)
        .is_some_and(|types| types.iter().any(|kind| kind == "string"))
        && !object.contains_key("maxLength")
    {
        object.insert("maxLength".into(), json!(8192));
    }
    if let Some(properties) = object.get_mut("properties").and_then(Value::as_object_mut) {
        for (name, child) in properties {
            bound_schema(child, Some(name));
        }
    }
    if let Some(items) = object.get_mut("items") {
        bound_schema(items, field);
    }
}

fn validate_tool_arguments(schema: &Value, value: &Value, path: &str) -> Result<(), GitError> {
    let invalid = |reason: String| {
        GitError::new(
            "git_invalid_arguments",
            format!("git_invalid_arguments:{path}:{reason}"),
            json!({"path":path,"reason":reason}),
        )
    };
    if schema.get("type") == Some(&json!("object")) && !value.is_object() {
        return Err(invalid("expected_object".into()));
    }
    if schema.get("type") == Some(&json!("array")) && !value.is_array() {
        return Err(invalid("expected_array".into()));
    }
    if schema.get("type") == Some(&json!("string")) && !value.is_string() {
        return Err(invalid("expected_string".into()));
    }
    if schema.get("type") == Some(&json!("integer"))
        && value.as_i64().is_none()
        && value.as_u64().is_none()
    {
        return Err(invalid("expected_integer".into()));
    }
    if let Some(text) = value.as_str() {
        if schema
            .get("maxLength")
            .and_then(Value::as_u64)
            .is_some_and(|max| text.len() > max as usize)
        {
            return Err(invalid("maxLength".into()));
        }
        if schema
            .get("enum")
            .and_then(Value::as_array)
            .is_some_and(|values| !values.iter().any(|candidate| candidate == value))
        {
            return Err(invalid("enum".into()));
        }
    }
    if let Some(array) = value.as_array() {
        if schema
            .get("minItems")
            .and_then(Value::as_u64)
            .is_some_and(|min| array.len() < min as usize)
        {
            return Err(invalid("minItems".into()));
        }
        if schema
            .get("maxItems")
            .and_then(Value::as_u64)
            .is_some_and(|max| array.len() > max as usize)
        {
            return Err(invalid("maxItems".into()));
        }
        if let Some(items) = schema.get("items") {
            for (index, item) in array.iter().enumerate() {
                validate_tool_arguments(items, item, &format!("{path}[{index}]"))?;
            }
        }
    }
    if let Some(number) = value.as_i64() {
        if schema
            .get("minimum")
            .and_then(Value::as_i64)
            .is_some_and(|min| number < min)
        {
            return Err(invalid("minimum".into()));
        }
        if schema
            .get("maximum")
            .and_then(Value::as_i64)
            .is_some_and(|max| number > max)
        {
            return Err(invalid("maximum".into()));
        }
    }
    if let Some(object) = value.as_object() {
        let properties = schema.get("properties").and_then(Value::as_object);
        if schema.get("additionalProperties") == Some(&json!(false)) {
            for key in object.keys() {
                if !properties.is_some_and(|known| known.contains_key(key)) {
                    return Err(invalid(format!("unknown_field:{key}")));
                }
            }
        }
        for required in schema
            .get("required")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
        {
            if !object.contains_key(required) {
                return Err(invalid(format!("required:{required}")));
            }
        }
        if let Some(properties) = properties {
            for (key, child) in object {
                if let Some(child_schema) = properties.get(key) {
                    validate_tool_arguments(child_schema, child, &format!("{path}.{key}"))?;
                }
            }
        }
    }
    if let Some(alternatives) = schema.get("anyOf").and_then(Value::as_array) {
        let matched = alternatives.iter().any(|alternative| {
            alternative
                .get("required")
                .and_then(Value::as_array)
                .is_some_and(|required| {
                    required
                        .iter()
                        .filter_map(Value::as_str)
                        .all(|field| value.get(field).is_some())
                })
        });
        if !matched {
            return Err(invalid("anyOf".into()));
        }
    }
    Ok(())
}

fn prompt_get(params: &Value) -> Result<Value, GitError> {
    let name = params
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if name != "git_mcp_workflow" {
        return Err(GitError::new(
            "unknown_prompt",
            format!("unknown_prompt:{name}"),
            json!({"name": name}),
        ));
    }
    Ok(
        json!({"description": "Guidance for branch, inspect, stage, commit, and push workflows.", "messages": [{"role": "user", "content": {"type": "text", "text": "Start with git_guidance, git_policy_inspect, and git_status. Use git_begin_work_scope before scoped mutations; native git_commit and git_push are authoritative and refuse stale or out-of-scope state."}}]}),
    )
}

fn call_tool(
    state: &State,
    params: &Value,
    cancellation: Option<Arc<AtomicBool>>,
) -> Result<Value, GitError> {
    let name = params
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let args_value = params
        .get("arguments")
        .cloned()
        .unwrap_or_else(|| json!({}));
    let args = &args_value;
    if let Some(params) = params.as_object() {
        for key in params.keys() {
            if !matches!(key.as_str(), "name" | "arguments" | "_meta") {
                return Err(GitError::new(
                    "git_invalid_call",
                    format!("git_invalid_call:unknown_field:{key}"),
                    json!({"field":key}),
                ));
            }
        }
    }
    let definition = list_tools()
        .into_iter()
        .find(|tool| tool["name"] == name)
        .ok_or_else(|| {
            GitError::new(
                "git_mcp_unknown_tool",
                format!("git_mcp_unknown_tool:{name}"),
                json!({"tool_name":name}),
            )
        })?;
    validate_tool_arguments(&definition["inputSchema"], args, "$args")?;
    let payload = match name {
        "git_guidance" => guidance(args),
        "git_policy_inspect" => Ok(policy(state)),
        "git_begin_work_scope" => git_begin_work_scope(state, args, cancellation),
        "git_workflow_record" => git_workflow_record(state, args, cancellation),
        "git_add" => git_add(state, args, cancellation),
        "git_unstage" => git_unstage(state, args, cancellation),
        "git_commit" => git_commit(state, args, cancellation),
        "git_push" => git_push(state, args, cancellation),
        "git_status" => git_status(state, args, cancellation),
        "git_sync_status" => git_sync_status(state, args, cancellation),
        "git_branch_list" => git_branch_list(state, args, cancellation),
        "git_changed_summary" => git_changed_summary(state, args, cancellation),
        "git_repositories_summary" => git_repositories_summary(state, args, cancellation),
        "git_diff" => git_diff(state, args, cancellation),
        "git_log" => git_log(state, args, cancellation),
        "git_show" => git_show(state, args, cancellation),
        "git_output_show" => output_show(state, args),
        _ => Err(GitError::new(
            "git_mcp_unknown_tool",
            format!("git_mcp_unknown_tool:{name}"),
            json!({"tool_name": name}),
        )),
    }?;
    tool_result(state, payload, name)
}

fn guidance(_args: &Value) -> Result<Value, GitError> {
    let tools = list_tools();
    let read = tools
        .iter()
        .filter(|tool| {
            tool.pointer("/annotations/readOnlyHint")
                .and_then(Value::as_bool)
                == Some(true)
        })
        .filter_map(|tool| tool.get("name").cloned())
        .collect::<Vec<_>>();
    let write = tools
        .iter()
        .filter(|tool| {
            tool.pointer("/annotations/readOnlyHint")
                .and_then(Value::as_bool)
                == Some(false)
        })
        .filter_map(|tool| tool.get("name").cloned())
        .collect::<Vec<_>>();
    Ok(
        json!({"schema": "narada.mcp_surface.guidance.v0", "status": "ok", "surface_id": "git", "purpose": "Governed Git inspection and publication workflows.", "tool_inventory": {"read": read, "write": write}, "native_boundary": "The Rust-native surface is authoritative for every tool in this live inventory, including scoped commit and non-force push.", "workflow": ["git_status", "git_changed_summary or git_diff", "git_begin_work_scope", "git_add", "git_commit", "git_push"]}),
    )
}

fn policy(state: &State) -> Value {
    json!({"schema": "narada.git.policy.v1", "mode": state.mode, "allowed_roots": state.allowed_roots.iter().map(|path| path.to_string_lossy().to_string()).collect::<Vec<_>>(), "max_timeout_ms": state.max_timeout_ms, "max_output_bytes": state.max_output_bytes, "mutation_audit": "mutations", "push_policy": "current_upstream_or_explicit_remote_branch", "branch_policy": "merged_only_no_force", "relative_path_resolution": {"omitted_working_directory": "Use the first allowed root.", "absolute_working_directory": "Use the supplied absolute path when it is under an allowed root.", "relative_working_directory": "Resolve an explicitly supplied relative working_directory against the MCP process current directory, then enforce allowed-root containment.", "pathspecs": "Resolve Git pathspec arguments relative to the selected repository working directory; absolute pathspecs and parent traversal are refused."}})
}

fn git_status(
    state: &State,
    args: &Value,
    cancellation: Option<Arc<AtomicBool>>,
) -> Result<Value, GitError> {
    let cwd = resolve_cwd(state, args)?;
    let root = git_text(
        state,
        &cwd,
        &["rev-parse", "--show-toplevel"],
        cancellation.clone(),
        "git_status_failed",
    )?;
    let status = git_text(
        state,
        &cwd,
        &[
            "status",
            "--porcelain=v1",
            "-z",
            "-b",
            "--untracked-files=all",
        ],
        cancellation.clone(),
        "git_status_failed",
    )?;
    let mut parsed = parse_status(&status);
    let query = apply_status_query(state, &mut parsed, args, root.trim())?;
    let remotes = git_remotes(state, &cwd, cancellation.clone())?;
    let upstream = parsed.get("upstream").cloned().unwrap_or(Value::Null);
    let push_target = upstream.as_str().and_then(|value| value.split_once('/')).map(|(remote, branch)| {
        json!({"status": "resolved", "remote": remote, "branch": branch, "source": "upstream"})
    }).unwrap_or_else(|| json!({"status": "unresolved", "remote": Value::Null, "branch": parsed.get("branch"), "reason": "upstream_not_configured"}));
    let push_remediation = if push_target.get("status").and_then(Value::as_str) == Some("resolved")
    {
        Value::Null
    } else {
        json!({"kind": "set_upstream_or_push_explicit_target"})
    };
    Ok(
        json!({"schema": "narada.git.status.v1", "status": "ok", "working_directory": cwd.to_string_lossy(), "repository_root": root.trim(), "branch": parsed.get("branch"), "upstream": upstream, "ahead": parsed.get("ahead").unwrap_or(&json!(0)), "behind": parsed.get("behind").unwrap_or(&json!(0)), "unborn": parsed.get("unborn").unwrap_or(&Value::Bool(false)), "status_entries": parsed.get("status_entries"), "staged": parsed.get("staged"), "unstaged": parsed.get("unstaged"), "untracked": parsed.get("untracked"), "conflicts": parsed.get("conflicts"), "paths": parsed.get("paths"), "clean": parsed.get("clean"), "summary": parsed.get("summary"), "format": query.get("format").and_then(Value::as_str).unwrap_or("full"), "query": query, "remotes": remotes, "remote_names": Value::Array(git_remotes_names(state, &cwd, cancellation)?), "push_target": push_target, "push_remediation": push_remediation}),
    )
}

fn git_begin_work_scope(
    state: &State,
    args: &Value,
    cancellation: Option<Arc<AtomicBool>>,
) -> Result<Value, GitError> {
    let cwd = resolve_cwd(state, args)?;
    let requested = args
        .get("allowed_paths")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            GitError::new(
                "git_begin_work_scope_requires_allowed_paths",
                "git_begin_work_scope_requires_allowed_paths",
                json!({}),
            )
        })?;
    if requested.is_empty() {
        return Err(GitError::new(
            "git_begin_work_scope_requires_allowed_paths",
            "git_begin_work_scope_requires_allowed_paths",
            json!({}),
        ));
    }
    let mut allowed_paths = requested
        .iter()
        .map(|value| {
            let path = value.as_str().ok_or_else(|| {
                GitError::new(
                    "git_begin_work_scope_requires_explicit_paths",
                    "git_begin_work_scope_requires_explicit_paths",
                    json!({}),
                )
            })?;
            validate_path(path)?;
            if path == "." || path.contains(['*', '?', '[']) {
                return Err(GitError::new(
                    "git_begin_work_scope_requires_explicit_paths",
                    "git_begin_work_scope_requires_explicit_paths",
                    json!({"path": path}),
                ));
            }
            Ok(path.replace('\\', "/"))
        })
        .collect::<Result<Vec<_>, GitError>>()?;
    allowed_paths.sort();
    allowed_paths.dedup();
    let repository_root = git_text(
        state,
        &cwd,
        &["rev-parse", "--show-toplevel"],
        cancellation.clone(),
        "git_begin_work_scope_failed",
    )?
    .trim()
    .to_string();
    let head = git_text(
        state,
        &cwd,
        &["rev-parse", "HEAD"],
        cancellation.clone(),
        "git_begin_work_scope_failed",
    )
    .ok()
    .map(|value| value.trim().to_string());
    let index_digest = git_text(
        state,
        &cwd,
        &["write-tree"],
        cancellation,
        "git_begin_work_scope_failed",
    )
    .ok()
    .map(|value| value.trim().to_string());
    let base_state = json!({"head": head, "index_digest": index_digest});
    if let Some(supplied) = args.get("base_state").and_then(Value::as_object) {
        for field in ["head", "index_digest"] {
            if let Some(value) = supplied.get(field) {
                if value != base_state.get(field).unwrap_or(&Value::Null) {
                    return Err(GitError::new(
                        "git_work_scope_base_state_mismatch",
                        "git_work_scope_base_state_mismatch",
                        json!({"field": field, "supplied": value, "actual": base_state.get(field), "mutation_started": false, "atomic": true}),
                    ));
                }
            }
        }
    }
    let created = OffsetDateTime::now_utc();
    let expires = created + TimeDuration::minutes(WORK_SCOPE_TTL_MINUTES);
    let reference = unique_id("gws");
    let scope = WorkScope {
        reference: reference.clone(),
        repository_root: repository_root.clone(),
        allowed_paths: allowed_paths.clone(),
        base_state: base_state.clone(),
        created_at: created.format(&Rfc3339).unwrap_or_default(),
        expires_at: expires,
    };
    state
        .work_scopes
        .lock()
        .map_err(|_| {
            GitError::new(
                "git_work_scope_store_unavailable",
                "git_work_scope_store_unavailable",
                json!({}),
            )
        })?
        .insert(reference.clone(), scope.clone());
    Ok(
        json!({"schema": "narada.git.work_scope.v1", "status": "ok", "working_directory": cwd.to_string_lossy(), "repository_root": repository_root, "work_scope_ref": reference, "allowed_paths": allowed_paths, "base_state": base_state, "created_at": scope.created_at, "expires_at": expires.format(&Rfc3339).unwrap_or_default(), "mutation_started": false, "summary": format!("work scope issued for {} path{}", scope.allowed_paths.len(), if scope.allowed_paths.len() == 1 { "" } else { "s" })}),
    )
}

fn apply_status_query(
    state: &State,
    parsed: &mut Value,
    args: &Value,
    repository_root: &str,
) -> Result<Value, GitError> {
    let scope = if let Some(reference) = args.get("work_scope_ref").and_then(Value::as_str) {
        Some(resolve_work_scope(state, reference, repository_root)?)
    } else {
        None
    };
    let filters = pathspecs(args)?;
    let allowed_paths = scope.as_ref().map(|value| value.allowed_paths.clone());
    let staged_only = args
        .get("staged_only")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let include_untracked = args
        .get("include_untracked")
        .and_then(Value::as_bool)
        .unwrap_or(true);
    let format = args.get("format").and_then(Value::as_str).unwrap_or("full");
    if !matches!(format, "full" | "paths" | "summary") {
        return Err(GitError::new(
            "git_invalid_status_format",
            "git_invalid_status_format",
            json!({"format": format}),
        ));
    }
    let entries = parsed
        .get("status_entries")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let selected = entries
        .into_iter()
        .filter(|entry| {
            let path = entry
                .get("path")
                .or_else(|| entry.get("display_path"))
                .and_then(Value::as_str)
                .unwrap_or_default();
            let in_scope = allowed_paths
                .as_ref()
                .is_none_or(|paths| paths.iter().any(|allowed| path_matches(path, allowed)));
            let in_pathspec =
                filters.is_empty() || filters.iter().any(|pattern| path_matches(path, pattern));
            let staged = !staged_only
                || entry
                    .get("staged")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
            let untracked = include_untracked
                || !entry
                    .get("untracked")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
            in_scope && in_pathspec && staged && untracked
        })
        .collect::<Vec<_>>();
    let staged = selected
        .iter()
        .filter(|entry| {
            entry
                .get("staged")
                .and_then(Value::as_bool)
                .unwrap_or(false)
        })
        .filter_map(|entry| entry.get("display_path").cloned())
        .collect::<Vec<_>>();
    let unstaged = selected
        .iter()
        .filter(|entry| {
            entry
                .get("unstaged")
                .and_then(Value::as_bool)
                .unwrap_or(false)
        })
        .filter_map(|entry| entry.get("display_path").cloned())
        .collect::<Vec<_>>();
    let untracked = selected
        .iter()
        .filter(|entry| {
            entry
                .get("untracked")
                .and_then(Value::as_bool)
                .unwrap_or(false)
        })
        .filter_map(|entry| entry.get("display_path").cloned())
        .collect::<Vec<_>>();
    let conflicts = selected
        .iter()
        .filter(|entry| {
            entry
                .get("conflict")
                .and_then(Value::as_bool)
                .unwrap_or(false)
        })
        .filter_map(|entry| entry.get("display_path").cloned())
        .collect::<Vec<_>>();
    let clean =
        staged.is_empty() && unstaged.is_empty() && untracked.is_empty() && conflicts.is_empty();
    parsed["status_entries"] = if format == "full" {
        json!(selected)
    } else {
        json!([])
    };
    parsed["staged"] = if format == "full" {
        json!(staged)
    } else {
        json!([])
    };
    parsed["unstaged"] = if format == "full" {
        json!(unstaged)
    } else {
        json!([])
    };
    parsed["untracked"] = if format == "full" {
        json!(untracked)
    } else {
        json!([])
    };
    parsed["conflicts"] = if format == "full" {
        json!(conflicts)
    } else {
        json!([])
    };
    parsed["clean"] = json!(clean);
    parsed["summary"] = json!({"staged_count": staged.len(), "unstaged_count": unstaged.len(), "untracked_count": untracked.len(), "conflict_count": conflicts.len(), "matching_path_count": selected.len(), "clean": clean});
    if format != "full" {
        parsed["paths"] = Value::Array(
            selected
                .iter()
                .filter_map(|entry| entry.get("display_path").cloned())
                .collect(),
        );
    }
    Ok(
        json!({"work_scope_ref": scope.as_ref().map(|value| value.reference.clone()), "pathspecs": filters, "staged_only": staged_only, "include_untracked": include_untracked, "format": format}),
    )
}

fn resolve_work_scope(
    state: &State,
    reference: &str,
    repository_root: &str,
) -> Result<WorkScope, GitError> {
    let mut scopes = state.work_scopes.lock().map_err(|_| {
        GitError::new(
            "git_work_scope_store_unavailable",
            "git_work_scope_store_unavailable",
            json!({}),
        )
    })?;
    let Some(scope) = scopes.get(reference).cloned() else {
        return Err(GitError::new(
            "git_work_scope_ref_not_found",
            "git_work_scope_ref_not_found",
            json!({"work_scope_ref": reference, "mutation_started": false, "atomic": true}),
        ));
    };
    if scope.expires_at <= OffsetDateTime::now_utc() {
        scopes.remove(reference);
        return Err(GitError::new(
            "git_work_scope_ref_expired",
            "git_work_scope_ref_expired",
            json!({"work_scope_ref": reference, "mutation_started": false, "atomic": true}),
        ));
    }
    if scope.repository_root != repository_root {
        return Err(GitError::new(
            "git_work_scope_repository_mismatch",
            "git_work_scope_repository_mismatch",
            json!({"work_scope_ref": reference, "repository_root": repository_root, "expected_repository_root": scope.repository_root, "mutation_started": false, "atomic": true}),
        ));
    }
    Ok(scope)
}

fn git_workflow_record(
    state: &State,
    args: &Value,
    cancellation: Option<Arc<AtomicBool>>,
) -> Result<Value, GitError> {
    if state.mode != "write" {
        return Err(GitError::new(
            "git_write_mode_required",
            "git_write_mode_required",
            json!({"tool_name": "git_workflow_record", "mode": state.mode, "mutation_started": false, "atomic": true}),
        ));
    }
    let scope_label = args
        .get("scope_label")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            GitError::new(
                "git_workflow_record_requires_scope_label",
                "git_workflow_record_requires_scope_label",
                json!({}),
            )
        })?;
    let repositories = args
        .get("repositories")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            GitError::new(
                "git_workflow_record_requires_repositories",
                "git_workflow_record_requires_repositories",
                json!({}),
            )
        })?;
    if repositories.is_empty() {
        return Err(GitError::new(
            "git_workflow_record_requires_repositories",
            "git_workflow_record_requires_repositories",
            json!({}),
        ));
    }
    let mut records = Vec::new();
    for repository in repositories {
        let working_directory = repository
            .get("working_directory")
            .and_then(Value::as_str)
            .ok_or_else(|| {
                GitError::new(
                    "git_workflow_record_requires_working_directory",
                    "git_workflow_record_requires_working_directory",
                    json!({}),
                )
            })?;
        let status = git_status(
            state,
            &json!({"working_directory": working_directory}),
            cancellation.clone(),
        )?;
        let push_status = repository
            .get("push_status")
            .and_then(Value::as_str)
            .unwrap_or("not_attempted");
        if !matches!(
            push_status,
            "pushed" | "not_attempted" | "failed" | "not_pushable"
        ) {
            return Err(GitError::new(
                "git_workflow_record_invalid_push_status",
                "git_workflow_record_invalid_push_status",
                json!({"push_status": push_status}),
            ));
        }
        records.push(json!({
            "working_directory": status.get("working_directory"),
            "repository_root": status.get("repository_root"),
            "branch": status.get("branch"),
            "upstream": status.get("upstream"),
            "staged_paths": repository.get("staged_paths").cloned().unwrap_or_else(|| json!([])),
            "committed_sha": repository.get("committed_sha").cloned().unwrap_or(Value::Null),
            "pushed": repository.get("pushed").and_then(Value::as_bool).unwrap_or(false),
            "push_status": push_status,
            "push_reason": repository.get("push_reason").cloned().unwrap_or(Value::Null),
            "unrelated_dirty_paths_left": repository.get("unrelated_dirty_paths_left").cloned().unwrap_or_else(|| json!([])),
            "post_status": status,
        }));
    }
    let recorded_at = now_rfc3339();
    let record = json!({
        "schema": "narada.git.workflow_record.v1",
        "status": "recorded",
        "workflow_id": args.get("workflow_id").and_then(Value::as_str).filter(|value| !value.trim().is_empty()).map(str::to_string).unwrap_or_else(|| unique_id("gitwf")),
        "scope_label": scope_label,
        "recorded_at": recorded_at,
        "summary": args.get("summary").cloned().unwrap_or(Value::Null),
        "repositories": records,
    });
    let ledger_path = state
        .output_root
        .join(".ai")
        .join("state")
        .join("git-mcp-audit")
        .join("git-workflows.jsonl");
    if let Some(parent) = ledger_path.parent() {
        fs::create_dir_all(parent).map_err(|error| {
            GitError::new(
                "git_workflow_record_persist_failed",
                error.to_string(),
                json!({"path": parent}),
            )
        })?;
    }
    let line = format!(
        "{}\n",
        serde_json::to_string(&record).unwrap_or_else(|_| "{}".to_string())
    );
    let mut file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&ledger_path)
        .map_err(|error| {
            GitError::new(
                "git_workflow_record_persist_failed",
                error.to_string(),
                json!({"path": ledger_path}),
            )
        })?;
    file.write_all(line.as_bytes()).map_err(|error| {
        GitError::new(
            "git_workflow_record_persist_failed",
            error.to_string(),
            json!({"path": ledger_path}),
        )
    })?;
    Ok(
        json!({"schema": "narada.git.workflow_record.v1", "status": "recorded", "workflow_id": record.get("workflow_id"), "scope_label": scope_label, "recorded_at": recorded_at, "summary": record.get("summary"), "repositories": record.get("repositories"), "ledger_path": ledger_path.to_string_lossy()}),
    )
}

fn now_rfc3339() -> String {
    OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .unwrap_or_else(|_| String::new())
}

fn git_add(
    state: &State,
    args: &Value,
    cancellation: Option<Arc<AtomicBool>>,
) -> Result<Value, GitError> {
    git_index_change(state, args, cancellation, true)
}

fn git_unstage(
    state: &State,
    args: &Value,
    cancellation: Option<Arc<AtomicBool>>,
) -> Result<Value, GitError> {
    git_index_change(state, args, cancellation, false)
}

fn git_commit(
    state: &State,
    args: &Value,
    cancellation: Option<Arc<AtomicBool>>,
) -> Result<Value, GitError> {
    if state.mode != "write" {
        return Err(GitError::new(
            "git_write_mode_required",
            "git_write_mode_required",
            json!({"tool_name": "git_commit", "mode": state.mode, "mutation_started": false, "atomic": true}),
        ));
    }
    let _write_guard = state.git_write_lock.lock().map_err(|_| {
        GitError::new(
            "git_write_lock_unavailable",
            "git_write_lock_unavailable",
            json!({"mutation_started": false, "atomic": true}),
        )
    })?;
    let cwd = resolve_cwd(state, args)?;
    let cwd_text = cwd.to_string_lossy().to_string();
    let message = args
        .get("message")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            GitError::new(
                "git_commit_requires_message",
                "git_commit_requires_message",
                json!({"mutation_started": false, "atomic": true}),
            )
        })?;
    let scope_ref = args
        .get("work_scope_ref")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| {
            GitError::new(
                "git_commit_requires_work_scope_ref",
                "git_commit_requires_work_scope_ref",
                json!({"mutation_started": false, "atomic": true}),
            )
        })?;
    let repository_root = git_text(
        state,
        &cwd,
        &["rev-parse", "--show-toplevel"],
        cancellation.clone(),
        "git_commit_failed",
    )?
    .trim()
    .to_string();
    let scope = resolve_work_scope(state, scope_ref, &repository_root)?;
    let head = git_text(
        state,
        &cwd,
        &["rev-parse", "HEAD"],
        cancellation.clone(),
        "git_commit_failed",
    )?
    .trim()
    .to_string();
    if scope.base_state.get("head").and_then(Value::as_str) != Some(head.as_str()) {
        return Err(GitError::new(
            "git_work_scope_head_drift",
            "git_work_scope_head_drift",
            json!({"work_scope_ref": scope_ref, "expected_head": scope.base_state.get("head"), "actual_head": head, "mutation_started": false, "atomic": true}),
        ));
    }
    let status_text = git_text(
        state,
        &cwd,
        &[
            "status",
            "--porcelain=v1",
            "-z",
            "-b",
            "--untracked-files=all",
        ],
        cancellation.clone(),
        "git_commit_failed",
    )?;
    let status = parse_status(&status_text);
    let staged_entries = status
        .get("status_entries")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|entry| entry.get("staged").and_then(Value::as_bool) == Some(true))
        .cloned()
        .collect::<Vec<_>>();
    if staged_entries.is_empty() {
        return Err(GitError::new(
            "git_commit_requires_staged_changes",
            "git_commit_requires_staged_changes",
            json!({"mutation_started": false, "atomic": true}),
        ));
    }
    let mut staged_paths = staged_entries
        .iter()
        .filter_map(|entry| entry.get("path").and_then(Value::as_str))
        .map(|path| path.replace('\\', "/"))
        .collect::<Vec<_>>();
    staged_paths.sort();
    staged_paths.dedup();
    let out_of_scope = staged_paths
        .iter()
        .filter(|path| {
            !scope
                .allowed_paths
                .iter()
                .any(|allowed| path_matches(path, allowed))
        })
        .cloned()
        .collect::<Vec<_>>();
    if !out_of_scope.is_empty() {
        return Err(GitError::new(
            "git_commit_paths_outside_work_scope",
            "git_commit_paths_outside_work_scope",
            json!({"work_scope_ref": scope_ref, "out_of_scope_staged_paths": out_of_scope, "mutation_started": false, "atomic": true}),
        ));
    }
    if let Some(values) = args.get("expected_staged_paths").and_then(Value::as_array) {
        let mut expected = values
            .iter()
            .map(|value| {
                value
                    .as_str()
                    .map(|path| path.replace('\\', "/"))
                    .ok_or_else(|| {
                        GitError::new(
                            "git_commit_expected_staged_paths_invalid",
                            "git_commit_expected_staged_paths_invalid",
                            json!({"mutation_started": false, "atomic": true}),
                        )
                    })
            })
            .collect::<Result<Vec<_>, _>>()?;
        expected.sort();
        expected.dedup();
        if expected != staged_paths {
            return Err(GitError::new(
                "git_commit_staged_scope_mismatch",
                "git_commit_staged_scope_mismatch",
                json!({"expected_staged_paths": expected, "actual_staged_paths": staged_paths, "mutation_started": false, "atomic": true}),
            ));
        }
    }
    let body = args
        .get("body")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let mut commit_args = vec!["commit", "-m", message];
    if let Some(body) = body {
        commit_args.extend(["-m", body]);
    }
    let result = run_git(state, &cwd, &commit_args, cancellation.clone());
    if result.exit_code != Some(0) || result.timed_out || result.cancelled {
        return Err(GitError::new(
            "git_commit_failed",
            "git_commit_failed",
            json!({"exit_code": result.exit_code, "timed_out": result.timed_out, "cancelled": result.cancelled, "diagnostic_text": result.diagnostic_text, "output_preview": result.output_text.chars().take(PREVIEW_CHAR_LIMIT).collect::<String>()}),
        ));
    }
    let commit = git_text(
        state,
        &cwd,
        &["rev-parse", "HEAD"],
        cancellation.clone(),
        "git_commit_failed",
    )?
    .trim()
    .to_string();
    let post_status = git_status(state, &json!({"working_directory": cwd_text}), cancellation)?;
    let output = format!("{}{}", result.output_text, result.diagnostic_text);
    Ok(
        json!({"schema": "narada.git.commit.v1", "status": "ok", "working_directory": cwd_text, "commit": commit, "commit_ref": format!("git_commit:{commit}"), "committed_entries": staged_entries, "committed_files": staged_paths, "committed_file_count": staged_paths.len(), "work_scope_ref": scope_ref, "summary": result.output_text.lines().find(|line| !line.trim().is_empty()).unwrap_or("commit created"), "output": output, "post_status": post_status}),
    )
}

fn git_push(
    state: &State,
    args: &Value,
    cancellation: Option<Arc<AtomicBool>>,
) -> Result<Value, GitError> {
    if state.mode != "write" {
        return Err(GitError::new(
            "git_write_mode_required",
            "git_write_mode_required",
            json!({"tool_name": "git_push", "mode": state.mode, "mutation_started": false, "atomic": true}),
        ));
    }
    let _write_guard = state.git_write_lock.lock().map_err(|_| {
        GitError::new(
            "git_write_lock_unavailable",
            "git_write_lock_unavailable",
            json!({"mutation_started": false, "atomic": true}),
        )
    })?;
    let cwd = resolve_cwd(state, args)?;
    let cwd_text = cwd.to_string_lossy().to_string();
    let scope_ref = args
        .get("work_scope_ref")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| {
            GitError::new(
                "git_push_requires_work_scope_ref",
                "git_push_requires_work_scope_ref",
                json!({"mutation_started": false, "atomic": true}),
            )
        })?;
    let repository_root = git_text(
        state,
        &cwd,
        &["rev-parse", "--show-toplevel"],
        cancellation.clone(),
        "git_push_failed",
    )?
    .trim()
    .to_string();
    let _scope = resolve_work_scope(state, scope_ref, &repository_root)?;
    let head = git_text(
        state,
        &cwd,
        &["rev-parse", "HEAD"],
        cancellation.clone(),
        "git_push_failed",
    )?
    .trim()
    .to_string();
    if let Some(expected) = args.get("expected_commit").and_then(Value::as_str) {
        let expected = expected.strip_prefix("git_commit:").unwrap_or(expected);
        if expected != head {
            return Err(GitError::new(
                "git_push_head_mismatch",
                "git_push_head_mismatch",
                json!({"expected_commit": expected, "actual_head": head, "mutation_started": false, "atomic": true}),
            ));
        }
    }
    let remote = args.get("remote").and_then(Value::as_str);
    let branch = args.get("branch").and_then(Value::as_str);
    if remote.is_some() != branch.is_some() {
        return Err(GitError::new(
            "git_push_remote_and_branch_required_together",
            "git_push_remote_and_branch_required_together",
            json!({"mutation_started": false, "atomic": true}),
        ));
    }
    let mut push_args = vec!["push"];
    if let (Some(remote), Some(branch)) = (remote, branch) {
        validate_commit(remote)?;
        validate_commit(branch)?;
        push_args.extend([remote, branch]);
    } else {
        git_text(
            state,
            &cwd,
            &[
                "rev-parse",
                "--abbrev-ref",
                "--symbolic-full-name",
                "@{upstream}",
            ],
            cancellation.clone(),
            "git_push_target_unresolved",
        )?;
    }
    let result = run_git(state, &cwd, &push_args, cancellation.clone());
    if result.exit_code != Some(0) || result.timed_out || result.cancelled {
        return Err(GitError::new(
            "git_push_failed",
            "git_push_failed",
            json!({"exit_code": result.exit_code, "timed_out": result.timed_out, "cancelled": result.cancelled, "diagnostic_text": result.diagnostic_text, "output_preview": result.output_text.chars().take(PREVIEW_CHAR_LIMIT).collect::<String>()}),
        ));
    }
    let post_head = git_text(
        state,
        &cwd,
        &["rev-parse", "HEAD"],
        cancellation.clone(),
        "git_push_failed",
    )?
    .trim()
    .to_string();
    if post_head != head {
        return Err(GitError::new(
            "git_push_post_state_head_mismatch",
            "git_push_post_state_head_mismatch",
            json!({"expected_commit": head, "actual_head": post_head, "mutation_started": true, "atomic": false}),
        ));
    }
    let post_status = git_status(state, &json!({"working_directory": cwd_text}), cancellation)?;
    let output = format!("{}{}", result.output_text, result.diagnostic_text);
    Ok(
        json!({"schema": "narada.git.push.v1", "status": "ok", "working_directory": cwd_text, "remote": remote, "branch": branch, "commit": head, "commit_ref": format!("git_commit:{head}"), "work_scope_ref": scope_ref, "summary": result.output_text.lines().chain(result.diagnostic_text.lines()).find(|line| !line.trim().is_empty()).unwrap_or("push completed"), "output": output, "post_status": post_status}),
    )
}

fn git_index_change(
    state: &State,
    args: &Value,
    cancellation: Option<Arc<AtomicBool>>,
    stage: bool,
) -> Result<Value, GitError> {
    if state.mode != "write" {
        return Err(GitError::new(
            "git_write_mode_required",
            "git_write_mode_required",
            json!({"tool_name": if stage { "git_add" } else { "git_unstage" }, "mode": state.mode, "mutation_started": false, "atomic": true}),
        ));
    }
    let _write_guard = state.git_write_lock.lock().map_err(|_| {
        GitError::new(
            "git_write_lock_unavailable",
            "git_write_lock_unavailable",
            json!({"mutation_started": false, "atomic": true}),
        )
    })?;
    let cwd = resolve_cwd(state, args)?;
    let values = args.get("paths").and_then(Value::as_array).ok_or_else(|| {
        GitError::new(
            "git_index_change_requires_paths",
            "git_index_change_requires_paths",
            json!({}),
        )
    })?;
    if values.is_empty() {
        return Err(GitError::new(
            "git_index_change_requires_paths",
            "git_index_change_requires_paths",
            json!({}),
        ));
    }
    let paths = values
        .iter()
        .map(|value| {
            let path = value.as_str().ok_or_else(|| {
                GitError::new("git_invalid_pathspec", "git_invalid_pathspec", json!({}))
            })?;
            validate_path(path)?;
            if path == "." || path.contains(['*', '?', '[']) {
                return Err(GitError::new(
                    "git_index_change_requires_explicit_paths",
                    "git_index_change_requires_explicit_paths",
                    json!({"pathspec": path}),
                ));
            }
            Ok(path.replace('\\', "/"))
        })
        .collect::<Result<Vec<_>, GitError>>()?;
    let repository_root = git_text(
        state,
        &cwd,
        &["rev-parse", "--show-toplevel"],
        cancellation.clone(),
        "git_index_change_failed",
    )?
    .trim()
    .to_string();
    let scope = if let Some(reference) = args.get("work_scope_ref").and_then(Value::as_str) {
        let scope = resolve_work_scope(state, reference, &repository_root)?;
        let base = read_git_base_state(state, &cwd, cancellation.clone());
        for field in ["head", "index_digest"] {
            if scope.base_state.get(field) != base.get(field) {
                return Err(GitError::new(
                    "git_work_scope_base_state_mismatch",
                    "git_work_scope_base_state_mismatch",
                    json!({"field": field, "supplied": scope.base_state.get(field), "actual": base.get(field), "mutation_started": false, "atomic": true}),
                ));
            }
        }
        if paths.iter().any(|path| {
            !scope
                .allowed_paths
                .iter()
                .any(|allowed| path_matches(path, allowed))
        }) {
            return Err(GitError::new(
                "git_index_change_path_outside_work_scope",
                "git_index_change_path_outside_work_scope",
                json!({"work_scope_ref": scope.reference, "allowed_paths": scope.allowed_paths, "paths": paths, "mutation_started": false, "atomic": true}),
            ));
        }
        Some(scope)
    } else {
        None
    };
    let path_refs = paths.iter().map(String::as_str).collect::<Vec<_>>();
    let mut command = if stage {
        vec!["add", "--"]
    } else {
        vec!["reset", "--"]
    };
    command.extend(path_refs);
    let result = run_git(state, &cwd, &command, cancellation);
    if result.exit_code != Some(0) || result.timed_out || result.cancelled {
        return Err(GitError::new(
            if stage {
                "git_add_failed"
            } else {
                "git_unstage_failed"
            },
            if stage {
                "git_add_failed"
            } else {
                "git_unstage_failed"
            },
            json!({"exit_code": result.exit_code, "timed_out": result.timed_out, "cancelled": result.cancelled, "diagnostic_text": result.diagnostic_text, "output_preview": result.output_text.chars().take(PREVIEW_CHAR_LIMIT).collect::<String>()}),
        ));
    }
    let post_status = git_status(
        state,
        &json!({"working_directory": cwd.to_string_lossy()}),
        None,
    )?;
    Ok(
        json!({"schema": if stage { "narada.git.add.v1" } else { "narada.git.unstage.v1" }, "status": "ok", "operation": if stage { "add" } else { "unstage" }, "working_directory": cwd.to_string_lossy(), "repository_root": repository_root, "paths": paths, "work_scope_ref": scope.as_ref().map(|value| value.reference.clone()), "output": result.output_text, "post_status": post_status, "summary": if stage { "staged explicit paths" } else { "unstaged explicit paths" }}),
    )
}

fn read_git_base_state(state: &State, cwd: &Path, cancellation: Option<Arc<AtomicBool>>) -> Value {
    let head = git_text(
        state,
        cwd,
        &["rev-parse", "HEAD"],
        cancellation.clone(),
        "git_base_state_failed",
    )
    .ok()
    .map(|value| value.trim().to_string());
    let index_digest = git_text(
        state,
        cwd,
        &["write-tree"],
        cancellation,
        "git_base_state_failed",
    )
    .ok()
    .map(|value| value.trim().to_string());
    json!({"head": head, "index_digest": index_digest})
}

fn git_sync_status(
    state: &State,
    args: &Value,
    cancellation: Option<Arc<AtomicBool>>,
) -> Result<Value, GitError> {
    let cwd = resolve_cwd(state, args)?;
    let status = git_status(state, args, cancellation)?;
    let git_dir = git_text(
        state,
        &cwd,
        &["rev-parse", "--git-dir"],
        None,
        "git_sync_status_failed",
    )?;
    let git_dir_path = {
        let candidate = PathBuf::from(git_dir.trim());
        if candidate.is_absolute() {
            candidate
        } else {
            cwd.join(candidate)
        }
    };
    let operation = if git_dir_path.join("rebase-merge").exists()
        || git_dir_path.join("rebase-apply").exists()
    {
        Some("rebase")
    } else if git_dir_path.join("MERGE_HEAD").exists() {
        Some("merge")
    } else {
        None
    };
    let conflicts = status
        .get("conflicts")
        .cloned()
        .unwrap_or_else(|| json!([]));
    Ok(
        json!({"schema": "narada.git.sync_status.v1", "status": "ok", "working_directory": cwd.to_string_lossy(), "operation": operation, "in_progress": operation.is_some(), "conflict_paths": conflicts, "conflict_count": conflicts.as_array().map(Vec::len).unwrap_or(0), "clean": status.get("clean"), "branch": status.get("branch"), "upstream": status.get("upstream"), "recovery": if operation == Some("rebase") { json!(["git_rebase_continue", "git_rebase_abort", "git_sync_status"]) } else if operation == Some("merge") { json!(["git_merge_continue", "git_merge_abort", "git_sync_status"]) } else { json!([])}, "post_status": status}),
    )
}

fn git_branch_list(
    state: &State,
    args: &Value,
    cancellation: Option<Arc<AtomicBool>>,
) -> Result<Value, GitError> {
    let cwd = resolve_cwd(state, args)?;
    let scope = args.get("scope").and_then(Value::as_str).unwrap_or("all");
    let limit = args
        .get("limit")
        .and_then(Value::as_u64)
        .unwrap_or(100)
        .clamp(1, 500) as usize;
    let current = git_text(
        state,
        &cwd,
        &["branch", "--show-current"],
        cancellation.clone(),
        "git_branch_list_failed",
    )?
    .trim()
    .to_string();
    let refs = match scope {
        "local" => vec!["refs/heads"],
        "remote" => vec!["refs/remotes"],
        _ => vec!["refs/heads", "refs/remotes"],
    };
    let mut command = vec!["for-each-ref", "--sort=refname", "--format=%(refname)\t%(refname:short)\t%(objectname)\t%(HEAD)\t%(upstream:short)\t%(upstream:trackshort)"];
    command.extend(refs);
    let output = git_text(
        state,
        &cwd,
        &command,
        cancellation,
        "git_branch_list_failed",
    )?;
    let branches = output.lines().filter(|line| !line.is_empty()).take(limit).map(|line| {
        let mut fields = line.split('\t');
        let ref_name = fields.next().unwrap_or_default();
        let name = fields.next().unwrap_or_default();
        json!({"name": name, "type": if ref_name.starts_with("refs/remotes/") { "remote" } else { "local" }, "object_id": fields.next().filter(|value| !value.is_empty()), "current": fields.next() == Some("*"), "upstream": fields.next().filter(|value| !value.is_empty()), "tracking": fields.next().filter(|value| !value.is_empty())})
    }).collect::<Vec<_>>();
    Ok(
        json!({"schema": "narada.git.branch_list.v1", "status": "ok", "working_directory": cwd.to_string_lossy(), "scope": scope, "limit": limit, "current_branch": if current.is_empty() { Value::Null } else { json!(current) }, "returned": branches.len(), "branches": branches}),
    )
}

fn git_changed_summary(
    state: &State,
    args: &Value,
    cancellation: Option<Arc<AtomicBool>>,
) -> Result<Value, GitError> {
    let status = git_status(state, args, cancellation)?;
    let tracked = status
        .get("status_entries")
        .and_then(Value::as_array)
        .map(|entries| {
            entries
                .iter()
                .filter(|entry| entry.get("untracked") != Some(&Value::Bool(true)))
                .filter_map(|entry| entry.get("display_path").cloned())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let untracked = status
        .get("untracked")
        .cloned()
        .unwrap_or_else(|| json!([]));
    let filters = args
        .get("pathspecs")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let single = args.get("pathspec").cloned();
    let mut relevance = filters;
    if let Some(value) = single {
        relevance.insert(0, value);
    }
    let untracked_paths = untracked.as_array().cloned().unwrap_or_default();
    let paths = tracked
        .iter()
        .chain(untracked_paths.iter())
        .cloned()
        .collect::<Vec<_>>();
    let relevant = paths
        .iter()
        .filter(|path| {
            relevance.iter().any(|filter| {
                path_matches(
                    path.as_str().unwrap_or_default(),
                    filter.as_str().unwrap_or_default(),
                )
            })
        })
        .cloned()
        .collect::<Vec<_>>();
    Ok(
        json!({"schema": "narada.git.changed_summary.v1", "status": "ok", "working_directory": status.get("working_directory"), "repository_root": status.get("repository_root"), "branch": status.get("branch"), "clean": status.get("clean"), "path_scope_applied": !relevance.is_empty(), "path_scope_filters": relevance, "whole_repository_tracked_changed_count": tracked.len(), "whole_repository_untracked_count": untracked.as_array().map(Vec::len).unwrap_or(0), "tracked_changed_count": tracked.len(), "staged_count": status.get("staged").and_then(Value::as_array).map(Vec::len).unwrap_or(0), "unstaged_count": status.get("unstaged").and_then(Value::as_array).map(Vec::len).unwrap_or(0), "conflict_count": status.get("conflicts").and_then(Value::as_array).map(Vec::len).unwrap_or(0), "untracked_count": untracked.as_array().map(Vec::len).unwrap_or(0), "tracked_changed_paths": tracked, "staged_paths": status.get("staged"), "unstaged_paths": status.get("unstaged"), "conflict_paths": status.get("conflicts"), "untracked_groups": group_untracked(&untracked), "relevance_filters": relevance, "relevant_changed_count": relevant.len(), "relevant_changed_paths": relevant, "full_diffs_omitted": true, "diff_tool": "git_diff"}),
    )
}

fn git_repositories_summary(
    state: &State,
    args: &Value,
    cancellation: Option<Arc<AtomicBool>>,
) -> Result<Value, GitError> {
    let directories = args
        .get("working_directories")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            GitError::new(
                "git_repositories_summary_requires_working_directories",
                "git_repositories_summary_requires_working_directories",
                json!({}),
            )
        })?;
    let mut repositories = Vec::new();
    for directory in directories.iter().filter_map(Value::as_str) {
        let status = git_status(
            state,
            &json!({"working_directory": directory}),
            cancellation.clone(),
        )?;
        let cwd = status
            .get("working_directory")
            .and_then(Value::as_str)
            .unwrap_or(directory);
        let latest = git_text(
            state,
            Path::new(cwd),
            &["log", "-1", "--pretty=format:%H%x1f%h%x1f%s"],
            cancellation.clone(),
            "git_repositories_summary_failed",
        )
        .unwrap_or_default();
        let fields = latest.split('\x1f').collect::<Vec<_>>();
        repositories.push(json!({"working_directory": cwd, "repository_root": status.get("repository_root"), "branch": status.get("branch"), "upstream": status.get("upstream"), "ahead": status.get("ahead"), "behind": status.get("behind"), "clean": status.get("clean"), "staged": status.get("staged"), "unstaged": status.get("unstaged"), "untracked": status.get("untracked"), "conflicts": status.get("conflicts"), "remotes": status.get("remotes"), "push_target": status.get("push_target"), "push_remediation": status.get("push_remediation"), "expected_paths": [], "unexpected_dirty_paths": [], "latest_commit": if fields.len() >= 3 { json!({"hash": fields[0], "short_hash": fields[1], "subject": fields[2]}) } else { Value::Null }}));
    }
    Ok(
        json!({"schema": "narada.git.repositories_summary.v1", "status": "ok", "scope_label": args.get("scope_label"), "repository_count": repositories.len(), "repositories": repositories}),
    )
}

fn git_diff(
    state: &State,
    args: &Value,
    cancellation: Option<Arc<AtomicBool>>,
) -> Result<Value, GitError> {
    let cwd = resolve_cwd(state, args)?;
    let scope = args
        .get("scope")
        .and_then(Value::as_str)
        .unwrap_or("working");
    let pathspecs = pathspecs(args)?;
    let offset = args.get("offset").and_then(Value::as_u64).unwrap_or(0) as usize;
    let limit = args
        .get("limit")
        .or_else(|| args.get("diff_limit"))
        .and_then(Value::as_u64)
        .unwrap_or(4_000)
        .clamp(1, 50_000) as usize;
    let mut command = match scope {
        "staged" => vec!["diff", "--cached", "--no-ext-diff"],
        "commit" => vec![
            "show",
            "--format=",
            "--patch",
            "--no-ext-diff",
            args.get("commit").and_then(Value::as_str).ok_or_else(|| {
                GitError::new(
                    "git_commitish_required",
                    "git_commitish_required",
                    json!({}),
                )
            })?,
        ],
        _ => vec!["diff", "--no-ext-diff"],
    };
    if !pathspecs.is_empty() {
        command.push("--");
        command.extend(pathspecs.iter().map(String::as_str));
    }
    let full = git_text(state, &cwd, &command, cancellation, "git_diff_failed")?;
    let (diff, next) = page_text(&full, offset, limit);
    Ok(
        json!({"schema": "narada.git.diff.v1", "status": "ok", "working_directory": cwd.to_string_lossy(), "scope": scope, "pathspec": if pathspecs.len() == 1 { json!(pathspecs[0]) } else { Value::Null }, "pathspecs": pathspecs, "offset": offset, "limit": limit, "next_offset": next.map(|value| json!(value)).unwrap_or(Value::Null), "include_untracked": false, "untracked_diff_included": false, "diff": diff, "diff_preview": full.chars().take(PREVIEW_CHAR_LIMIT).collect::<String>(), "diff_omitted": false, "diff_truncated": next.is_some(), "diff_char_length": full.chars().count()}),
    )
}

fn git_log(
    state: &State,
    args: &Value,
    cancellation: Option<Arc<AtomicBool>>,
) -> Result<Value, GitError> {
    let cwd = resolve_cwd(state, args)?;
    let limit = args
        .get("limit")
        .and_then(Value::as_u64)
        .unwrap_or(20)
        .clamp(1, 100);
    let pathspec = args.get("pathspec").and_then(Value::as_str);
    let limit_arg = format!("-{limit}");
    let mut command = vec![
        "log",
        limit_arg.as_str(),
        "--pretty=format:%H%x1f%h%x1f%an%x1f%ae%x1f%aI%x1f%s",
    ];
    if let Some(path) = pathspec {
        validate_path(path)?;
        command.extend(["--", path]);
    }
    let output = git_text(state, &cwd, &command, cancellation, "git_log_failed")?;
    let commits = output.lines().filter(|line| !line.is_empty()).map(|line| {
        let fields = line.split('\x1f').collect::<Vec<_>>();
        json!({"hash": fields.first().copied().unwrap_or_default(), "short_hash": fields.get(1).copied().unwrap_or_default(), "author_name": fields.get(2).copied().unwrap_or_default(), "author_email": fields.get(3).copied().unwrap_or_default(), "author_date": fields.get(4).copied().unwrap_or_default(), "subject": fields.get(5).copied().unwrap_or_default()})
    }).collect::<Vec<_>>();
    Ok(
        json!({"schema": "narada.git.log.v1", "status": "ok", "working_directory": cwd.to_string_lossy(), "limit": limit, "pathspec": pathspec, "returned": commits.len(), "commits": commits}),
    )
}

fn git_show(
    state: &State,
    args: &Value,
    cancellation: Option<Arc<AtomicBool>>,
) -> Result<Value, GitError> {
    let cwd = resolve_cwd(state, args)?;
    let commit = args.get("commit").and_then(Value::as_str).ok_or_else(|| {
        GitError::new(
            "git_commitish_required",
            "git_commitish_required",
            json!({}),
        )
    })?;
    validate_commit(commit)?;
    let metadata = git_text(
        state,
        &cwd,
        &[
            "show",
            "--no-patch",
            "--format=%H%x1f%h%x1f%an%x1f%ae%x1f%aI%x1f%s%x1f%b",
            commit,
        ],
        cancellation.clone(),
        "git_show_failed",
    )?;
    let fields = metadata.split('\x1f').collect::<Vec<_>>();
    let include_patch = args
        .get("include_patch")
        .and_then(Value::as_bool)
        .unwrap_or(true);
    let pathspec = args.get("pathspec").and_then(Value::as_str);
    if let Some(path) = pathspec {
        validate_path(path)?;
    }
    let patch = if include_patch {
        let mut command = vec!["show", "--format=", "--patch", "--no-ext-diff", commit];
        if let Some(path) = pathspec {
            command.extend(["--", path]);
        }
        git_text(state, &cwd, &command, cancellation, "git_show_failed")?
    } else {
        String::new()
    };
    Ok(
        json!({"schema": "narada.git.show.v1", "status": "ok", "working_directory": cwd.to_string_lossy(), "commit": commit, "hash": fields.first().copied().unwrap_or_default(), "short_hash": fields.get(1).copied().unwrap_or_default(), "author_name": fields.get(2).copied().unwrap_or_default(), "author_email": fields.get(3).copied().unwrap_or_default(), "author_date": fields.get(4).copied().unwrap_or_default(), "subject": fields.get(5).copied().unwrap_or_default(), "body": fields.get(6).copied().unwrap_or_default().trim_end(), "include_patch": include_patch, "pathspec": args.get("pathspec"), "patch": patch, "patch_preview": patch.chars().take(PREVIEW_CHAR_LIMIT).collect::<String>(), "patch_omitted": false, "patch_truncated": false, "patch_char_length": patch.chars().count()}),
    )
}

fn resolve_cwd(state: &State, args: &Value) -> Result<PathBuf, GitError> {
    let requested = args.get("working_directory").and_then(Value::as_str);
    let path = requested
        .map(|value| {
            let candidate = PathBuf::from(value);
            if candidate.is_absolute() {
                candidate
            } else {
                absolute(candidate)
            }
        })
        .unwrap_or_else(|| state.allowed_roots[0].clone());
    if !inside_any_root(&path, &state.allowed_roots) {
        return Err(GitError::new(
            "git_working_directory_outside_allowed_roots",
            "git_working_directory_outside_allowed_roots",
            json!({"working_directory": path.to_string_lossy(), "allowed_roots": state.allowed_roots.iter().map(|root| root.to_string_lossy().to_string()).collect::<Vec<_>>()}),
        ));
    }
    if !path.is_dir() {
        return Err(GitError::new(
            "git_working_directory_not_found",
            "git_working_directory_not_found",
            json!({"working_directory": path.to_string_lossy()}),
        ));
    }
    Ok(path)
}

fn pathspecs(args: &Value) -> Result<Vec<String>, GitError> {
    let mut values = Vec::new();
    if let Some(value) = args.get("pathspec").and_then(Value::as_str) {
        values.push(value.to_string());
    }
    if let Some(array) = args.get("pathspecs").and_then(Value::as_array) {
        values.extend(array.iter().filter_map(Value::as_str).map(str::to_string));
    }
    for value in &values {
        validate_path(value)?;
    }
    Ok(values)
}

fn validate_path(value: &str) -> Result<(), GitError> {
    if value.trim().is_empty()
        || value.starts_with('-')
        || Path::new(value).is_absolute()
        || value.split(['/', '\\']).any(|part| part == "..")
        || value.starts_with(":(")
    {
        return Err(GitError::new(
            "git_invalid_pathspec",
            "git_invalid_pathspec",
            json!({"pathspec": value}),
        ));
    }
    Ok(())
}

fn validate_commit(value: &str) -> Result<(), GitError> {
    if value.is_empty()
        || value.starts_with('-')
        || !value
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || "._/@{}~^:-".contains(character))
    {
        return Err(GitError::new(
            "git_invalid_commitish",
            "git_invalid_commitish",
            json!({"commit": value}),
        ));
    }
    Ok(())
}

fn inside_any_root(path: &Path, roots: &[PathBuf]) -> bool {
    let candidate = path_key(path);
    roots.iter().any(|root| {
        let key = path_key(root);
        candidate == key || candidate.starts_with(&(key + "/"))
    })
}

fn path_key(path: &Path) -> String {
    path.to_string_lossy()
        .replace('\\', "/")
        .trim_end_matches('/')
        .to_ascii_lowercase()
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

fn git_text(
    state: &State,
    cwd: &Path,
    args: &[&str],
    cancellation: Option<Arc<AtomicBool>>,
    failure_code: &str,
) -> Result<String, GitError> {
    let result = run_git(state, cwd, args, cancellation);
    if result.exit_code == Some(0) && !result.timed_out && !result.cancelled {
        return Ok(result.output_text);
    }
    Err(GitError::new(
        failure_code,
        failure_code,
        json!({"exit_code": result.exit_code, "timed_out": result.timed_out, "cancelled": result.cancelled, "diagnostic_text": result.diagnostic_text, "output_preview": result.output_text.chars().take(PREVIEW_CHAR_LIMIT).collect::<String>(), "output_truncated": result.output_truncated, "diagnostic_truncated": result.diagnostic_truncated}),
    ))
}

fn run_git(
    state: &State,
    cwd: &Path,
    args: &[&str],
    cancellation: Option<Arc<AtomicBool>>,
) -> GitResult {
    let child_result = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .envs(&state.env)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn();
    let Ok(mut child) = child_result else {
        return GitResult {
            exit_code: None,
            output_text: String::new(),
            diagnostic_text: "git_spawn_failed".to_string(),
            timed_out: false,
            cancelled: false,
            output_truncated: false,
            diagnostic_truncated: false,
        };
    };
    let max_output_bytes = state.max_output_bytes;
    let stdout_handle = child
        .stdout
        .take()
        .map(|stream| thread::spawn(move || read_bounded(stream, max_output_bytes)));
    let stderr_handle = child
        .stderr
        .take()
        .map(|stream| thread::spawn(move || read_bounded(stream, max_output_bytes)));
    let deadline = Instant::now() + Duration::from_millis(state.max_timeout_ms);
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
    let stdout = stdout_handle
        .and_then(|handle| handle.join().ok())
        .unwrap_or((Vec::new(), false));
    let stderr = stderr_handle
        .and_then(|handle| handle.join().ok())
        .unwrap_or((Vec::new(), false));
    GitResult {
        exit_code: status.and_then(|value| value.code()),
        output_text: String::from_utf8_lossy(&stdout.0).to_string(),
        diagnostic_text: String::from_utf8_lossy(&stderr.0).to_string(),
        timed_out,
        cancelled,
        output_truncated: stdout.1,
        diagnostic_truncated: stderr.1,
    }
}

fn read_bounded<R: Read>(mut reader: R, max: usize) -> (Vec<u8>, bool) {
    let mut output = Vec::with_capacity(max.min(8192));
    let mut buffer = [0_u8; 8192];
    let mut truncated = false;
    loop {
        match reader.read(&mut buffer) {
            Ok(0) => break,
            Ok(count) => {
                let keep = max.saturating_sub(output.len()).min(count);
                output.extend_from_slice(&buffer[..keep]);
                if keep < count {
                    truncated = true;
                }
            }
            Err(_) => break,
        }
    }
    (output, truncated)
}

fn kill_child(child: &mut Child) {
    #[cfg(windows)]
    {
        let pid = child.id();
        let _ = Command::new("taskkill")
            .args(["/PID", &pid.to_string(), "/T", "/F"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
    #[cfg(not(windows))]
    {
        let _ = child.kill();
    }
}

fn git_remotes(
    state: &State,
    cwd: &Path,
    cancellation: Option<Arc<AtomicBool>>,
) -> Result<Vec<Value>, GitError> {
    let output = git_text(
        state,
        cwd,
        &["remote", "-v"],
        cancellation,
        "git_status_failed",
    )?;
    let mut remotes = Vec::new();
    for line in output.lines() {
        let fields = line.split_whitespace().collect::<Vec<_>>();
        if fields.len() < 3 {
            continue;
        }
        let name = fields[0];
        let url = fields[1];
        let kind = fields[2].trim_matches(['(', ')']);
        if let Some(existing) = remotes
            .iter_mut()
            .find(|value: &&mut Value| value.get("name").and_then(Value::as_str) == Some(name))
        {
            if kind == "push" {
                existing["push_url"] = json!(url);
            }
        } else {
            remotes.push(json!({"name": name, "fetch_url": if kind == "fetch" { json!(url) } else { Value::Null }, "push_url": if kind == "push" { json!(url) } else { Value::Null }}));
        }
    }
    Ok(remotes)
}

fn git_remotes_names(
    state: &State,
    cwd: &Path,
    cancellation: Option<Arc<AtomicBool>>,
) -> Result<Vec<Value>, GitError> {
    Ok(git_remotes(state, cwd, cancellation)?
        .iter()
        .filter_map(|value| value.get("name").cloned())
        .collect())
}

fn parse_status(output: &str) -> Value {
    let mut entries = output
        .split('\0')
        .filter(|value| !value.is_empty())
        .collect::<Vec<_>>();
    let branch_line = if entries
        .first()
        .is_some_and(|value| value.starts_with("## "))
    {
        entries.remove(0).trim_start_matches("## ").to_string()
    } else {
        String::new()
    };
    let (branch, upstream, ahead, behind, unborn) = parse_branch(&branch_line);
    let mut status_entries = Vec::new();
    let mut staged = Vec::new();
    let mut unstaged = Vec::new();
    let mut untracked = Vec::new();
    let mut conflicts = Vec::new();
    let mut index = 0;
    while index < entries.len() {
        let entry = entries[index];
        let x = entry.chars().next().unwrap_or(' ');
        let y = entry.chars().nth(1).unwrap_or(' ');
        let path = entry.get(3..).unwrap_or_default().to_string();
        let original = if x == 'R' || x == 'C' {
            index += 1;
            entries.get(index).map(|value| (*value).to_string())
        } else {
            None
        };
        let display = original
            .as_ref()
            .map(|value| format!("{value} <- {path}"))
            .unwrap_or_else(|| path.clone());
        let is_untracked = x == '?' && y == '?';
        let is_conflict = x == 'U' || y == 'U' || (x == 'A' && y == 'A') || (x == 'D' && y == 'D');
        let is_staged = x != ' ' && x != '?';
        let is_unstaged = y != ' ' && y != '?';
        status_entries.push(json!({"x": x.to_string(), "y": y.to_string(), "path": path, "original_path": original, "display_path": display, "staged": is_staged, "unstaged": is_unstaged, "untracked": is_untracked, "conflict": is_conflict}));
        if is_untracked {
            untracked.push(json!(display));
        }
        if is_conflict {
            conflicts.push(json!(display));
        }
        if is_staged && !is_untracked {
            staged.push(json!(display));
        }
        if is_unstaged && !is_untracked {
            unstaged.push(json!(display));
        }
        index += 1;
    }
    let clean =
        staged.is_empty() && unstaged.is_empty() && untracked.is_empty() && conflicts.is_empty();
    json!({"branch": branch, "upstream": upstream, "ahead": ahead, "behind": behind, "unborn": unborn, "status_entries": status_entries, "staged": staged, "unstaged": unstaged, "untracked": untracked, "conflicts": conflicts, "clean": clean, "summary": {"staged_count": staged.len(), "unstaged_count": unstaged.len(), "untracked_count": untracked.len(), "conflict_count": conflicts.len(), "matching_path_count": status_entries.len(), "clean": clean}})
}

fn parse_branch(line: &str) -> (Value, Value, u64, u64, bool) {
    if let Some(branch) = line.strip_prefix("No commits yet on ") {
        return (json!(branch), Value::Null, 0, 0, true);
    }
    let (base, flags) = line
        .split_once(" [")
        .map(|(base, tail)| (base, tail.trim_end_matches(']')))
        .unwrap_or((line, ""));
    let (branch, upstream) = base
        .split_once("...")
        .map(|(left, right)| (left, Some(right)))
        .unwrap_or((base, None));
    let ahead = flags
        .split(',')
        .find_map(|value| value.trim().strip_prefix("ahead "))
        .and_then(|value| value.parse().ok())
        .unwrap_or(0);
    let behind = flags
        .split(',')
        .find_map(|value| value.trim().strip_prefix("behind "))
        .and_then(|value| value.parse().ok())
        .unwrap_or(0);
    (
        if branch.is_empty() {
            Value::Null
        } else {
            json!(branch)
        },
        upstream.map(|value| json!(value)).unwrap_or(Value::Null),
        ahead,
        behind,
        false,
    )
}

fn group_untracked(untracked: &Value) -> Value {
    let mut groups = HashMap::<String, Vec<String>>::new();
    for value in untracked
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
    {
        let top = value.split(['/', '\\']).next().unwrap_or(value).to_string();
        groups.entry(top).or_default().push(value.to_string());
    }
    Value::Array(groups.into_iter().map(|(top_level, paths)| {
        let count = paths.len();
        let sample_paths = paths.into_iter().take(20).collect::<Vec<_>>();
        json!({"top_level": top_level, "count": count, "sample_paths": sample_paths, "sample_truncated": count > 20})
    }).collect())
}

fn path_matches(path: &str, pattern: &str) -> bool {
    let path = path.replace('\\', "/");
    let pattern = pattern.replace('\\', "/");
    if !pattern.contains(['*', '?', '[']) {
        return path == pattern
            || path.starts_with(&(pattern.trim_end_matches('/').to_string() + "/"));
    }
    // Keep the read canary dependency-free. A broad `*` is useful for callers;
    // more specific glob syntax is intentionally rejected as an exact match.
    pattern == "*" || path == pattern
}

fn page_text(text: &str, offset: usize, limit: usize) -> (String, Option<usize>) {
    let chars = text.chars().collect::<Vec<_>>();
    let start = offset.min(chars.len());
    let end = (start + limit).min(chars.len());
    (
        chars[start..end].iter().collect(),
        if end < chars.len() { Some(end) } else { None },
    )
}

fn tool_result(state: &State, payload: Value, tool_name: &str) -> Result<Value, GitError> {
    let text = serde_json::to_string_pretty(&payload).unwrap_or_else(|_| "{}".to_string());
    if text.chars().count() <= 6_000 {
        return Ok(
            json!({"content": [{"type": "text", "text": text, "annotations": {"audience": ["assistant"]}}], "structuredContent": payload}),
        );
    }
    let id = unique_id("o");
    let reference = format!("mcp_output:{id}");
    let path = state
        .output_root
        .join(".ai")
        .join("tmp")
        .join("mcp-outputs")
        .join("workspace")
        .join(format!("{id}.json"));
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|error| {
            GitError::new("git_output_persist_failed", error.to_string(), json!({}))
        })?;
    }
    let record = json!({"schema": "narada.mcp_output_ref.v1", "ref": reference, "output_id": id, "tool_name": tool_name, "created_at": "", "full_output_char_length": text.chars().count(), "truncated": true, "sha256": "", "full_output": payload});
    fs::write(
        &path,
        serde_json::to_string(&record).unwrap_or_else(|_| "{}".to_string()),
    )
    .map_err(|error| GitError::new("git_output_persist_failed", error.to_string(), json!({})))?;
    let preview = text.chars().take(4_000).collect::<String>();
    let envelope = json!({"schema": "narada.producer_output_page.v1", "status": payload.get("status").and_then(Value::as_str).unwrap_or("ok"), "truncated": true, "output_ref": reference, "ref": reference, "result_materialized": true, "tool_name": tool_name, "offset": 0, "limit": 4_000, "next_offset": if text.chars().count() > 4_000 { json!(4_000) } else { Value::Null }, "output_text": preview, "output_truncated": text.chars().count() > 4_000, "reader_tool": "git_output_show", "full_output_char_length": text.chars().count()});
    Ok(
        json!({"content": [{"type": "text", "text": serde_json::to_string(&envelope).unwrap_or_else(|_| "{}".to_string()), "annotations": {"audience": ["assistant"]}}], "structuredContent": envelope}),
    )
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

fn output_show(state: &State, args: &Value) -> Result<Value, GitError> {
    let reference = args
        .get("ref")
        .and_then(Value::as_str)
        .or_else(|| args.get("output_ref").and_then(Value::as_str))
        .unwrap_or_default();
    let Some(id) = reference.strip_prefix("mcp_output:") else {
        return Err(GitError::new(
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
        return Err(GitError::new(
            "output_ref_invalid",
            "output_ref_invalid",
            json!({"ref": reference}),
        ));
    }
    let path = state
        .output_root
        .join(".ai")
        .join("tmp")
        .join("mcp-outputs")
        .join("workspace")
        .join(format!("{id}.json"));
    let record: Value = serde_json::from_slice(&fs::read(&path).map_err(|_| {
        GitError::new(
            "output_ref_not_found",
            "output_ref_not_found",
            json!({"ref": reference}),
        )
    })?)
    .map_err(|error| {
        GitError::new(
            "output_ref_invalid_json",
            error.to_string(),
            json!({"ref": reference}),
        )
    })?;
    let payload = record.get("full_output").cloned().unwrap_or(Value::Null);
    let text = serde_json::to_string_pretty(&payload).unwrap_or_else(|_| "null".to_string());
    let offset = args.get("offset").and_then(Value::as_u64).unwrap_or(0) as usize;
    let limit = args
        .get("limit")
        .and_then(Value::as_u64)
        .unwrap_or(10_000)
        .clamp(1, 20_000) as usize;
    let page = page_text(&text, offset, limit);
    Ok(
        json!({"schema": "narada.mcp_output_page.v1", "status": "ok", "ref": reference, "tool_name": record.get("tool_name"), "full_output_char_length": text.chars().count(), "offset": offset.min(text.chars().count()), "limit": limit, "output_limit": limit, "output_truncated": page.1.is_some(), "next_offset": page.1.map(|value| json!(value)).unwrap_or(Value::Null), "output_text": page.0}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn live_catalog_has_precise_closed_schemas() {
        let tools = list_tools();
        assert_eq!(tools.len(), 17);
        for tool in &tools {
            assert_eq!(tool["inputSchema"]["type"], "object");
            assert_eq!(tool["inputSchema"]["additionalProperties"], false);
        }
        let commit = tools
            .iter()
            .find(|tool| tool["name"] == "git_commit")
            .unwrap();
        assert!(commit["inputSchema"]["required"]
            .as_array()
            .unwrap()
            .contains(&json!("work_scope_ref")));
        let show = tools
            .iter()
            .find(|tool| tool["name"] == "git_show")
            .unwrap();
        assert_eq!(show["inputSchema"]["required"], json!(["commit"]));
    }

    fn assert_bounded(schema: &Value) {
        match schema.get("type").and_then(Value::as_str) {
            Some("object") => {
                assert!(schema
                    .get("maxProperties")
                    .and_then(Value::as_u64)
                    .is_some());
                if let Some(properties) = schema.get("properties").and_then(Value::as_object) {
                    for child in properties.values() {
                        assert_bounded(child);
                    }
                }
            }
            Some("array") => {
                assert!(schema.get("maxItems").and_then(Value::as_u64).is_some());
                if let Some(items) = schema.get("items") {
                    assert_bounded(items);
                }
            }
            Some("string") if schema.get("enum").is_none() => {
                assert!(schema.get("maxLength").and_then(Value::as_u64).is_some());
            }
            _ => {}
        }
    }

    #[test]
    fn every_tool_schema_is_named_bounded_and_rejects_unknown_input() {
        for tool in list_tools() {
            let name = tool["name"].as_str().unwrap();
            let schema = &tool["inputSchema"];
            assert_eq!(schema["title"], format!("{name}.input"));
            assert_bounded(schema);
            let failure =
                validate_tool_arguments(schema, &json!({"unexpected":true}), "$args").unwrap_err();
            assert_eq!(failure.code, "git_invalid_arguments");
            assert_eq!(failure.details["reason"], "unknown_field:unexpected");
        }
    }

    #[test]
    fn guidance_inventory_is_derived_from_live_catalog() {
        let value = guidance(&Value::Null).unwrap();
        let writes = value["tool_inventory"]["write"].as_array().unwrap();
        assert!(writes.contains(&json!("git_commit")));
        assert!(writes.contains(&json!("git_push")));
        assert!(!writes.contains(&json!("git_fetch")));
        assert!(value["native_boundary"]
            .as_str()
            .unwrap()
            .contains("authoritative"));
    }

    #[test]
    fn status_parser_preserves_upstream_for_push_resolution() {
        let status = parse_status("## main...origin/main\0");
        assert_eq!(status["upstream"], "origin/main");
    }
}
