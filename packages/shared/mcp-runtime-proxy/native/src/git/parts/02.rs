
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
    let work_scope_store = output_root.join(".ai").join("runtime").join("git-work-scopes");
    Ok(State {
        mode,
        allowed_roots,
        max_timeout_ms,
        max_output_bytes,
        output_root,
        env: env::vars().collect(),
        work_scope_store,
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
            "git_end_work_scope",
            "Release a live work-scope reference owned by the caller.",
            false,
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
        tool("git_worktree_list", "List registered worktrees with branch, HEAD, lock, and prune metadata.", true),
        tool("git_worktree_add", "Create a worktree at an explicitly allowed path.", false),
        tool("git_worktree_remove", "Remove an explicitly registered clean worktree without force.", false),
        tool("git_worktree_prune", "Prune stale worktree administrative records.", false),
        tool("git_branch_delete", "Delete one merged local branch without force.", false),
        tool("git_branch_delete_remote", "Delete one merged remote branch without force.", false),
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
