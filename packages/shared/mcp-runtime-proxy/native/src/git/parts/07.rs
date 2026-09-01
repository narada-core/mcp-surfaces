
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
