
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
        cancellation.clone(),
        "git_base_state_failed",
    )
    .ok()
    .map(|value| value.trim().to_string());
    let worktree_digest = git_text(
        state,
        cwd,
        &["status", "--porcelain=v1", "-z", "--untracked-files=all"],
        cancellation,
        "git_base_state_failed",
    )
    .ok()
    .map(|value| hex::encode(Sha256::digest(value.as_bytes())));
    json!({"head": head, "index_digest": index_digest, "worktree_digest": worktree_digest})
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

fn require_topology_mutation(
    state: &State,
    args: &Value,
    cwd: &Path,
    tool: &str,
    cancellation: Option<Arc<AtomicBool>>,
) -> Result<String, GitError> {
    if state.mode != "write" {
        return Err(GitError::new("git_write_mode_required", "git_write_mode_required", json!({"tool_name":tool,"mutation_started":false})));
    }
    let root = git_text(state, cwd, &["rev-parse","--show-toplevel"], None, "git_topology_preflight_failed")?.trim().to_string();
    let reference = args.get("work_scope_ref").and_then(Value::as_str).ok_or_else(|| GitError::new("git_work_scope_ref_required","git_work_scope_ref_required",json!({"tool_name":tool})))?;
    let scope = resolve_work_scope(state, reference, &root)?;
    if scope.authority != "repository_topology" {
        return Err(GitError::new(
            "git_repository_topology_scope_required",
            "git_repository_topology_scope_required",
            json!({"tool_name": tool, "work_scope_ref": reference, "supplied_authority": scope.authority, "mutation_started": false, "atomic": true, "remediation": "Acquire git_begin_work_scope with scope_kind=repository_topology and no allowed_paths."}),
        ));
    }
    let current = read_git_base_state(state, cwd, cancellation);
    let changed_fields = ["head", "index_digest", "worktree_digest"]
        .iter()
        .filter(|field| scope.base_state.get(**field) != current.get(**field))
        .map(|field| (*field).to_string())
        .collect::<Vec<_>>();
    if !changed_fields.is_empty() {
        return Err(GitError::new(
            "git_repository_topology_scope_base_state_drift",
            "git_repository_topology_scope_base_state_drift",
            json!({"work_scope_ref": reference, "changed_fields": changed_fields, "expected_base_state": scope.base_state, "actual_base_state": current, "mutation_started": false, "atomic": true, "cooperative_boundary": true}),
        ));
    }
    Ok(root)
}

fn requested_worktree_path(state: &State, cwd: &Path, args: &Value, require_allowed: bool) -> Result<PathBuf, GitError> {
    let raw = args.get("path").and_then(Value::as_str).unwrap_or_default();
    let path = absolute(if Path::new(raw).is_absolute() { PathBuf::from(raw) } else { cwd.join(raw) });
    if require_allowed && !inside_any_root(&path, &state.allowed_roots) {
        return Err(GitError::new("git_worktree_path_outside_allowed_roots","git_worktree_path_outside_allowed_roots",json!({"path":path.to_string_lossy(),"allowed_roots":state.allowed_roots.iter().map(|root|root.to_string_lossy().to_string()).collect::<Vec<_>>(),"mutation_started":false})));
    }
    Ok(path)
}

fn git_worktree_list(state: &State, args: &Value, cancellation: Option<Arc<AtomicBool>>) -> Result<Value, GitError> {
    let cwd = resolve_cwd(state,args)?;
    let output = git_text(state,&cwd,&["worktree","list","--porcelain","-z"],cancellation,"git_worktree_list_failed")?;
    let mut worktrees=vec![]; let mut current=serde_json::Map::new();
    for field in output.split('\0').filter(|value|!value.is_empty()) {
        if let Some(path)=field.strip_prefix("worktree ") {
            if !current.is_empty(){worktrees.push(Value::Object(std::mem::take(&mut current)));}
            current.insert("path".into(),json!(canonical_path_text(Path::new(path))));
        } else if let Some(head)=field.strip_prefix("HEAD ") { current.insert("head".into(),json!(head)); }
        else if let Some(branch)=field.strip_prefix("branch ") { current.insert("branch".into(),json!(branch.strip_prefix("refs/heads/").unwrap_or(branch))); }
        else if field=="bare" || field=="detached" || field=="locked" || field=="prunable" { current.insert(field.into(),json!(true)); }
        else if let Some(reason)=field.strip_prefix("locked ") { current.insert("locked".into(),json!(true)); current.insert("lock_reason".into(),json!(reason)); }
        else if let Some(reason)=field.strip_prefix("prunable ") { current.insert("prunable".into(),json!(true)); current.insert("prune_reason".into(),json!(reason)); }
    }
    if !current.is_empty(){worktrees.push(Value::Object(current));}
    Ok(json!({"schema":"narada.git.worktree_list.v1","status":"ok","working_directory":cwd.to_string_lossy(),"count":worktrees.len(),"worktrees":worktrees}))
}

fn git_worktree_add(state:&State,args:&Value,cancellation:Option<Arc<AtomicBool>>)->Result<Value,GitError>{
    let cwd=resolve_cwd(state,args)?; let root=require_topology_mutation(state,args,&cwd,"git_worktree_add",cancellation.clone())?;
    let path=requested_worktree_path(state,&cwd,args,true)?;
    if path.exists(){return Err(GitError::new("git_worktree_path_exists","git_worktree_path_exists",json!({"path":path.to_string_lossy(),"mutation_started":false})));}
    let branch=args.get("branch").and_then(Value::as_str); let new_branch=args.get("new_branch").and_then(Value::as_str);
    if branch.is_some()==new_branch.is_some(){return Err(GitError::new("git_worktree_requires_exactly_one_branch_mode","git_worktree_requires_exactly_one_branch_mode",json!({"mutation_started":false})));}
    let start=args.get("start_point").and_then(Value::as_str).unwrap_or("HEAD");
    let path_text=canonical_path_text(&path); let mut command=vec!["worktree","add"];
    if let Some(name)=new_branch {command.extend(["-b",name]);command.push(&path_text);command.push(start);} else {command.push(&path_text);command.push(branch.unwrap());}
    let _guard=state.git_write_lock.lock().map_err(|_|GitError::new("git_write_lock_unavailable","git_write_lock_unavailable",json!({})))?;
    git_text(state,&cwd,&command,cancellation,"git_worktree_add_failed")?;
    Ok(json!({"schema":"narada.git.worktree_mutation.v1","status":"added","repository_root":root,"path":path_text,"branch":branch,"new_branch":new_branch,"start_point":start}))
}

fn git_worktree_remove(state:&State,args:&Value,cancellation:Option<Arc<AtomicBool>>)->Result<Value,GitError>{
    let cwd=resolve_cwd(state,args)?; let root=require_topology_mutation(state,args,&cwd,"git_worktree_remove",cancellation.clone())?;
    let path=requested_worktree_path(state,&cwd,args,false)?; let path_text=canonical_path_text(&path);
    let inventory=git_text(state,&cwd,&["worktree","list","--porcelain"],cancellation.clone(),"git_worktree_remove_failed")?;
    if !inventory.lines().any(|line|line.strip_prefix("worktree ").is_some_and(|value|path_key(Path::new(value))==path_key(&path))){
        return Err(GitError::new("git_worktree_not_registered","git_worktree_not_registered",json!({"path":path_text,"mutation_started":false})));
    }
    let dirty=git_text(state,&path,&["status","--porcelain=v1","--untracked-files=all"],cancellation.clone(),"git_worktree_remove_preflight_failed")?;
    if !dirty.trim().is_empty(){return Err(GitError::new("git_worktree_not_clean","git_worktree_not_clean",json!({"path":path_text,"dirty_entries":dirty.lines().collect::<Vec<_>>(),"mutation_started":false})));}
    let _guard=state.git_write_lock.lock().map_err(|_|GitError::new("git_write_lock_unavailable","git_write_lock_unavailable",json!({})))?;
    git_text(state,&cwd,&["worktree","remove",&path_text],cancellation,"git_worktree_remove_failed")?;
    Ok(json!({"schema":"narada.git.worktree_mutation.v1","status":"removed","repository_root":root,"path":path_text}))
}
