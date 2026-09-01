
fn git_end_work_scope(
    state: &State,
    args: &Value,
    _cancellation: Option<Arc<AtomicBool>>,
) -> Result<Value, GitError> {
    if state.mode != "write" {
        return Err(GitError::new(
            "git_write_mode_required",
            "git_write_mode_required",
            json!({"tool_name": "git_end_work_scope", "mode": state.mode, "mutation_started": false, "atomic": true}),
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
                "git_end_work_scope_requires_owner_id",
                "git_end_work_scope_requires_owner_id",
                json!({}),
            )
        })?;
    let reference = args
        .get("work_scope_ref")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            GitError::new(
                "git_end_work_scope_requires_work_scope_ref",
                "git_end_work_scope_requires_work_scope_ref",
                json!({}),
            )
        })?;
    let repository_root = git_text(
        state,
        &cwd,
        &["rev-parse", "--show-toplevel"],
        None,
        "git_end_work_scope_failed",
    )?
    .trim()
    .to_string();
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
    if scope.owner_id != owner_id {
        return Err(GitError::new(
            "git_work_scope_owner_mismatch",
            "git_work_scope_owner_mismatch",
            json!({"work_scope_ref": reference, "expected_owner_id": scope.owner_id, "supplied_owner_id": owner_id, "mutation_started": false, "atomic": true}),
        ));
    }
    remove_work_scope_unlocked(store, reference)?;
    Ok(json!({"schema": "narada.git.work_scope_end.v1", "status": "released", "work_scope_ref": reference, "owner_id": owner_id, "authority": scope.authority, "released_paths": scope.allowed_paths, "mutation_started": true}))
    })
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
