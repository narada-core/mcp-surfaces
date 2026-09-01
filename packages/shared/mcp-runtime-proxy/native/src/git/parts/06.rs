
fn resolve_work_scope(
    state: &State,
    reference: &str,
    repository_root: &str,
) -> Result<WorkScope, GitError> {
    with_work_scope_lock(state, |store| {
    let scopes = load_work_scopes_unlocked(store)?;
    let Some(scope) = scopes.get(reference).cloned() else {
        return Err(GitError::new(
            "git_work_scope_ref_not_found",
            "git_work_scope_ref_not_found",
            json!({"work_scope_ref": reference, "mutation_started": false, "atomic": true}),
        ));
    };
    if scope.expires_at <= OffsetDateTime::now_utc() {
        remove_work_scope_unlocked(store, reference)?;
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
    })
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
