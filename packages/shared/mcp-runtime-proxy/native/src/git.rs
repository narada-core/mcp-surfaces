use crate::filesystem::{read_message, write_message};
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
    mpsc, Arc,
};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const PROTOCOL_VERSION: &str = "2024-11-05";
const DEFAULT_MAX_TIMEOUT_MS: u64 = 60_000;
const DEFAULT_MAX_OUTPUT_BYTES: usize = 2 * 1024 * 1024;
const MAX_OUTPUT_BYTES: usize = 20 * 1024 * 1024;
const PREVIEW_CHAR_LIMIT: usize = 1_000;

#[derive(Clone)]
struct State {
    mode: String,
    allowed_roots: Vec<PathBuf>,
    max_timeout_ms: u64,
    max_output_bytes: usize,
    output_root: PathBuf,
    env: HashMap<String, String>,
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
    let allowed_roots = roots
        .into_iter()
        .map(|root| absolute(PathBuf::from(root)))
        .collect::<Vec<_>>();
    let output_root =
        absolute(PathBuf::from(output_root.unwrap_or_else(|| {
            allowed_roots[0].to_string_lossy().to_string()
        })));
    Ok(State {
        mode,
        allowed_roots,
        max_timeout_ms,
        max_output_bytes,
        output_root,
        env: env::vars().collect(),
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
    let Some(id) = request.get("id").cloned() else {
        return None;
    };
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
    json!({"name": name, "description": description, "inputSchema": {"type": "object", "additionalProperties": true}, "annotations": {"title": name, "canonicalName": name, "readOnlyHint": read_only, "destructiveHint": false, "idempotentHint": read_only, "openWorldHint": false}, "outputSchema": {"type": "object", "additionalProperties": true}})
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
        json!({"description": "Guidance for branch, inspect, stage, commit, and push workflows.", "messages": [{"role": "user", "content": {"type": "text", "text": "Start with git_guidance, then inspect git_policy_inspect and git_status. Rust-native mode is read-only; use the JavaScript Git MCP for scoped mutation and publication."}}]}),
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
    let args = params.get("arguments").unwrap_or(&Value::Null);
    let payload = match name {
        "git_guidance" => guidance(args),
        "git_policy_inspect" => Ok(policy(state)),
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
    Ok(
        json!({"schema": "narada.mcp_surface.guidance.v0", "status": "ok", "surface_id": "git", "purpose": "Governed Git inspection and publication workflows.", "tool_inventory": {"read": ["git_policy_inspect", "git_status", "git_sync_status", "git_branch_list", "git_changed_summary", "git_repositories_summary", "git_diff", "git_log", "git_show"], "write": ["git_add", "git_commit", "git_push", "git_fetch", "git_rebase", "git_merge"]}, "native_boundary": "Rust canary covers bounded read operations; JavaScript remains authoritative for scoped mutation, recovery, and publication."}),
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
    let parsed = parse_status(&status);
    let remotes = git_remotes(state, &cwd, cancellation.clone())?;
    let upstream = parsed.get("upstream").cloned().unwrap_or(Value::Null);
    Ok(
        json!({"schema": "narada.git.status.v1", "status": "ok", "working_directory": cwd.to_string_lossy(), "repository_root": root.trim(), "branch": parsed.get("branch"), "upstream": upstream, "ahead": parsed.get("ahead").unwrap_or(&json!(0)), "behind": parsed.get("behind").unwrap_or(&json!(0)), "unborn": parsed.get("unborn").unwrap_or(&Value::Bool(false)), "status_entries": parsed.get("status_entries"), "staged": parsed.get("staged"), "unstaged": parsed.get("unstaged"), "untracked": parsed.get("untracked"), "conflicts": parsed.get("conflicts"), "clean": parsed.get("clean"), "summary": parsed.get("summary"), "format": args.get("format").and_then(Value::as_str).unwrap_or("full"), "query": {"staged_only": args.get("staged_only").and_then(Value::as_bool).unwrap_or(false), "include_untracked": args.get("include_untracked").and_then(Value::as_bool).unwrap_or(true)}, "remotes": remotes, "remote_names": Value::Array(git_remotes_names(state, &cwd, cancellation)?), "push_target": {"status": "unresolved", "remote": Value::Null, "branch": parsed.get("branch"), "reason": "upstream_not_configured"}, "push_remediation": {"kind": "set_upstream_or_push_explicit_target"}}),
    )
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
    let patch = if include_patch {
        git_text(
            state,
            &cwd,
            &["show", "--format=", "--patch", "--no-ext-diff", commit],
            cancellation,
            "git_show_failed",
        )?
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
