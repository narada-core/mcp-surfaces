
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

fn canonical_path_text(path: &Path) -> String {
    absolute(path.to_path_buf())
        .to_string_lossy()
        .replace('\\', "/")
        .trim_end_matches('/')
        .to_string()
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
