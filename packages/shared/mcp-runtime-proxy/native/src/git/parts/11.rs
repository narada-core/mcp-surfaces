
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
