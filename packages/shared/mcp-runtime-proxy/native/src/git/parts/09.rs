
fn git_worktree_prune(state:&State,args:&Value,cancellation:Option<Arc<AtomicBool>>)->Result<Value,GitError>{
    let cwd=resolve_cwd(state,args)?; let root=require_topology_mutation(state,args,&cwd,"git_worktree_prune",cancellation.clone())?;
    let before=git_worktree_list(state,args,cancellation.clone())?;
    let _guard=state.git_write_lock.lock().map_err(|_|GitError::new("git_write_lock_unavailable","git_write_lock_unavailable",json!({})))?;
    git_text(state,&cwd,&["worktree","prune"],cancellation.clone(),"git_worktree_prune_failed")?;
    let after=git_worktree_list(state,args,cancellation)?;
    Ok(json!({"schema":"narada.git.worktree_prune.v1","status":"ok","repository_root":root,"before_count":before["count"],"after_count":after["count"]}))
}

fn git_branch_delete(state:&State,args:&Value,cancellation:Option<Arc<AtomicBool>>)->Result<Value,GitError>{
    let cwd=resolve_cwd(state,args)?; let root=require_topology_mutation(state,args,&cwd,"git_branch_delete",cancellation.clone())?;
    let branch=args["branch"].as_str().unwrap(); let base=args["merged_into"].as_str().unwrap();
    git_text(state,&cwd,&["merge-base","--is-ancestor",branch,base],cancellation.clone(),"git_branch_not_merged")?;
    let _guard=state.git_write_lock.lock().map_err(|_|GitError::new("git_write_lock_unavailable","git_write_lock_unavailable",json!({})))?;
    git_text(state,&cwd,&["branch","-d",branch],cancellation,"git_branch_delete_failed")?;
    Ok(json!({"schema":"narada.git.branch_delete.v1","status":"deleted","repository_root":root,"branch":branch,"merged_into":base,"force":false}))
}

fn git_branch_delete_remote(state:&State,args:&Value,cancellation:Option<Arc<AtomicBool>>)->Result<Value,GitError>{
    let cwd=resolve_cwd(state,args)?; let root=require_topology_mutation(state,args,&cwd,"git_branch_delete_remote",cancellation.clone())?;
    let remote=args["remote"].as_str().unwrap(); let branch=args["branch"].as_str().unwrap(); let base=args["merged_into"].as_str().unwrap();
    if !git_remotes_names(state,&cwd,cancellation.clone())?.iter().any(|value|value==remote){return Err(GitError::new("git_remote_not_configured","git_remote_not_configured",json!({"remote":remote})));}
    let remote_ref=format!("{remote}/{branch}");
    git_text(state,&cwd,&["merge-base","--is-ancestor",&remote_ref,base],cancellation.clone(),"git_branch_not_merged")?;
    let _guard=state.git_write_lock.lock().map_err(|_|GitError::new("git_write_lock_unavailable","git_write_lock_unavailable",json!({})))?;
    git_text(state,&cwd,&["push",remote,"--delete",branch],cancellation,"git_branch_delete_remote_failed")?;
    Ok(json!({"schema":"narada.git.branch_delete.v1","status":"deleted_remote","repository_root":root,"remote":remote,"branch":branch,"merged_into":base,"force":false}))
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
    let include_untracked = args.get("include_untracked").and_then(Value::as_bool).unwrap_or(false);
    if include_untracked && scope != "working" {
        return Err(GitError::new("git_diff_include_untracked_requires_working_scope", "git_diff_include_untracked_requires_working_scope", json!({"scope":scope})));
    }
    let mut full = git_text(state, &cwd, &command, cancellation.clone(), "git_diff_failed")?;
    let mut untracked_included = false;
    if include_untracked {
        let mut list = vec!["ls-files", "--others", "--exclude-standard"];
        if !pathspecs.is_empty() {
            list.push("--");
            list.extend(pathspecs.iter().map(String::as_str));
        }
        for path in git_text(state, &cwd, &list, cancellation.clone(), "git_diff_untracked_list_failed")?.lines().filter(|value| !value.trim().is_empty()) {
            let result = run_git(state, &cwd, &["diff", "--no-index", "--", "/dev/null", path], cancellation.clone());
            if matches!(result.exit_code, Some(0) | Some(1)) && !result.timed_out && !result.cancelled {
                full.push_str(&result.output_text);
                untracked_included = true;
            } else {
                return Err(GitError::new("git_diff_untracked_failed", "git_diff_untracked_failed", json!({"path":path,"diagnostic":result.diagnostic_text})));
            }
        }
    }
    let (diff, next) = page_text(&full, offset, limit);
    Ok(
        json!({"schema": "narada.git.diff.v1", "status": "ok", "working_directory": cwd.to_string_lossy(), "scope": scope, "pathspec": if pathspecs.len() == 1 { json!(pathspecs[0]) } else { Value::Null }, "pathspecs": pathspecs, "offset": offset, "limit": limit, "next_offset": next.map(|value| json!(value)).unwrap_or(Value::Null), "include_untracked": include_untracked, "untracked_diff_included": untracked_included, "diff": diff, "diff_preview": full.chars().take(PREVIEW_CHAR_LIMIT).collect::<String>(), "diff_omitted": false, "diff_truncated": next.is_some(), "diff_char_length": full.chars().count()}),
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
