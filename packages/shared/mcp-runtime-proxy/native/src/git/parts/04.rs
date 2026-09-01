
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
        "git_end_work_scope" => git_end_work_scope(state, args, cancellation),
        "git_workflow_record" => git_workflow_record(state, args, cancellation),
        "git_add" => git_add(state, args, cancellation),
        "git_unstage" => git_unstage(state, args, cancellation),
        "git_commit" => git_commit(state, args, cancellation),
        "git_push" => git_push(state, args, cancellation),
        "git_status" => git_status(state, args, cancellation),
        "git_sync_status" => git_sync_status(state, args, cancellation),
        "git_branch_list" => git_branch_list(state, args, cancellation),
        "git_worktree_list" => git_worktree_list(state, args, cancellation),
        "git_worktree_add" => git_worktree_add(state, args, cancellation),
        "git_worktree_remove" => git_worktree_remove(state, args, cancellation),
        "git_worktree_prune" => git_worktree_prune(state, args, cancellation),
        "git_branch_delete" => git_branch_delete(state, args, cancellation),
        "git_branch_delete_remote" => git_branch_delete_remote(state, args, cancellation),
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
    if state.mode != "write" {
        return Err(GitError::new(
            "git_write_mode_required",
            "git_write_mode_required",
            json!({"tool_name": "git_begin_work_scope", "mode": state.mode, "mutation_started": false, "atomic": true}),
        ));
    }
    let cwd = resolve_cwd(state, args)?;
    let owner_id = args
        .get("owner_id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            GitError::new(
                "git_begin_work_scope_requires_owner_id",
                "git_begin_work_scope_requires_owner_id",
                json!({}),
            )
        })?;
    let authority = match args
        .get("scope_kind")
        .and_then(Value::as_str)
        .unwrap_or("paths")
    {
        "paths" => "paths",
        "repository_topology" => "repository_topology",
        value => {
            return Err(GitError::new(
                "git_invalid_scope_kind",
                "git_invalid_scope_kind",
                json!({"scope_kind": value}),
            ))
        }
    };
    let requested = args.get("allowed_paths").and_then(Value::as_array);
    if authority == "paths" && requested.is_none_or(|paths| paths.is_empty()) {
        return Err(GitError::new(
            "git_begin_work_scope_requires_allowed_paths",
            "git_begin_work_scope_requires_allowed_paths",
            json!({}),
        ));
    }
    if authority == "repository_topology"
        && requested.is_some_and(|paths| !paths.is_empty())
    {
        return Err(GitError::new(
            "git_repository_scope_does_not_accept_paths",
            "git_repository_scope_does_not_accept_paths",
            json!({"mutation_started": false, "atomic": true}),
        ));
    }
    let mut allowed_paths = requested
        .into_iter()
        .flatten()
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
        cancellation.clone(),
        "git_begin_work_scope_failed",
    )
    .ok()
    .map(|value| value.trim().to_string());
    let worktree_digest = git_text(
        state,
        &cwd,
        &["status", "--porcelain=v1", "-z", "--untracked-files=all"],
        cancellation,
        "git_begin_work_scope_failed",
    )
    .ok()
    .map(|value| hex::encode(Sha256::digest(value.as_bytes())));
    let base_state = json!({"head": head, "index_digest": index_digest, "worktree_digest": worktree_digest});
    if let Some(supplied) = args.get("base_state").and_then(Value::as_object) {
        for field in ["head", "index_digest", "worktree_digest"] {
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
        owner_id: owner_id.to_string(),
        authority: authority.to_string(),
        allowed_paths: allowed_paths.clone(),
        base_state: base_state.clone(),
        created_at: created.format(&Rfc3339).unwrap_or_default(),
        expires_at: expires,
    };
    with_work_scope_lock(state, |store| {
    let mut scopes = load_work_scopes_unlocked(store)?;
    for expired in scopes.values().filter(|scope| scope.expires_at <= OffsetDateTime::now_utc()).map(|scope| scope.reference.clone()).collect::<Vec<_>>() {
        remove_work_scope_unlocked(store, &expired)?;
        scopes.remove(&expired);
    }
    for existing in scopes.values() {
        let overlap = if authority == "repository_topology"
            || existing.authority == "repository_topology"
        {
            vec!["<repository-topology>".to_string()]
        } else {
            allowed_paths
                .iter()
                .filter(|path| {
                    existing
                        .allowed_paths
                        .iter()
                        .any(|held| path_matches(path, held) || path_matches(held, path))
                })
                .cloned()
                .collect::<Vec<_>>()
        };
        if !overlap.is_empty() {
            return Err(GitError::new(
                "git_work_scope_path_already_owned",
                "git_work_scope_path_already_owned",
                json!({"requested_owner_id": owner_id, "current_owner_id": existing.owner_id, "current_work_scope_ref": existing.reference, "paths": overlap, "expires_at": existing.expires_at.format(&Rfc3339).unwrap_or_default(), "mutation_started": false, "atomic": true}),
            ));
        }
    }
    persist_work_scope_unlocked(store, &scope)?;
    Ok(
        json!({"schema": "narada.git.work_scope.v1", "status": "ok", "working_directory": cwd.to_string_lossy(), "repository_root": repository_root, "work_scope_ref": reference, "owner_id": owner_id, "authority": authority, "allowed_paths": allowed_paths, "base_state": base_state, "created_at": scope.created_at, "expires_at": expires.format(&Rfc3339).unwrap_or_default(), "mutation_started": true, "summary": if authority == "repository_topology" { "repository topology scope leased".to_string() } else { format!("work scope issued for {} path{}", scope.allowed_paths.len(), if scope.allowed_paths.len() == 1 { "" } else { "s" }) }}),
    )
    })
}
