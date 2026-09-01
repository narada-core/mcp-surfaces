fn action_policy_reasons(
    command: &str,
    arguments: &str,
    working_dir: Option<&Path>,
    root: &Path,
) -> Vec<String> {
    let mut reasons = Vec::new();
    let executable = command
        .trim_matches(['\'', '"'])
        .replace('\\', "/")
        .rsplit('/')
        .next()
        .unwrap_or("")
        .to_ascii_lowercase();
    if matches!(executable.as_str(), "cmd" | "cmd.exe") {
        reasons.push(format!("scheduler_shell_action_disallowed:{command}"));
    }
    for token in std::iter::once(command.to_string()).chain(action_tokens(arguments)) {
        let normalized = token.replace('\\', "/");
        let clean = normalized.trim_matches(['\'', '"']);
        let extension = Path::new(clean)
            .extension()
            .and_then(|value| value.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        if !matches!(extension.as_str(), "cmd" | "bat") {
            continue;
        }
        let candidate = absolute(if Path::new(clean).is_absolute() {
            PathBuf::from(clean)
        } else {
            working_dir.unwrap_or(root).join(clean)
        });
        let canonical =
            !transient_path(&normalized) && is_within(&candidate, root) && candidate.is_file();
        if !canonical {
            reasons.push(format!("scheduler_transient_wrapper_refused:{token}"));
        }
    }
    for value in [command, arguments] {
        let normalized = value.replace('\\', "/").to_ascii_lowercase();
        if transient_path(&normalized)
            && [".ps1", ".psm1", ".js", ".mjs", ".cjs", ".ts"]
                .iter()
                .any(|suffix| normalized.contains(suffix))
        {
            reasons.push(format!("scheduler_transient_script_path_refused:{value}"));
        }
    }
    if let Some(directory) = working_dir {
        if !is_within(
            &absolute(directory.to_path_buf()),
            &absolute(root.to_path_buf()),
        ) {
            reasons.push(format!(
                "scheduler_working_dir_outside_allowed_root:{}",
                directory.to_string_lossy()
            ));
        }
    }
    reasons.sort();
    reasons.dedup();
    reasons
}

fn transient_path(value: &str) -> bool {
    let normalized = value.replace('\\', "/").to_ascii_lowercase();
    normalized.contains("/.ai/tmp/")
        || normalized.contains("/.ai/temp/")
        || normalized.ends_with("/.ai/tmp")
        || normalized.ends_with("/.ai/temp")
}
fn action_tokens(value: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut quote = None;
    for character in value.chars() {
        match (quote, character) {
            (Some(expected), value) if value == expected => quote = None,
            (Some(_), value) => current.push(value),
            (None, '\'' | '"') => quote = Some(character),
            (None, value) if value.is_whitespace() => {
                if !current.is_empty() {
                    tokens.push(std::mem::take(&mut current));
                }
            }
            (None, value) => current.push(value),
        }
    }
    if !current.is_empty() {
        tokens.push(current);
    }
    tokens
}
fn is_within(candidate: &Path, root: &Path) -> bool {
    let candidate = absolute(candidate.to_path_buf());
    let root = absolute(root.to_path_buf());
    candidate == root || candidate.starts_with(root)
}
fn absolute(path: PathBuf) -> PathBuf {
    if path.is_absolute() {
        normalize_path(path)
    } else {
        normalize_path(env::current_dir().unwrap_or_default().join(path))
    }
}
fn normalize_path(path: PathBuf) -> PathBuf {
    let mut result = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                result.pop();
            }
            other => result.push(other.as_os_str()),
        }
    }
    result
}

fn split_task_path(value: &str) -> Result<(String, String), Value> {
    let mut normalized = value.trim().replace('/', "\\");
    if !normalized.starts_with('\\') {
        normalized.insert(0, '\\');
    }
    let Some(index) = normalized.rfind('\\') else {
        return Err(error(
            "scheduler_requires_task_name",
            "scheduler_requires_task_name",
            Value::Null,
        ));
    };
    let name = normalized[index + 1..].to_string();
    if name.is_empty() {
        return Err(error(
            "scheduler_requires_task_name",
            "scheduler_requires_task_name",
            Value::Null,
        ));
    }
    Ok((name, normalized[..=index].to_string()))
}
fn same_windows_path(left: &str, right: &str) -> bool {
    left.trim()
        .trim_matches('"')
        .replace('/', "\\")
        .eq_ignore_ascii_case(&right.trim().trim_matches('"').replace('/', "\\"))
}

fn parse_csv(csv: &str) -> Vec<Map<String, Value>> {
    let mut lines = csv.trim().lines();
    let Some(header) = lines.next() else {
        return Vec::new();
    };
    let headers = parse_csv_line(header);
    let mut rows = Vec::new();
    for line in lines {
        let values = parse_csv_line(line);
        if values.len() != headers.len() {
            continue;
        }
        let mut row = Map::new();
        for (key, value) in headers.iter().zip(values) {
            row.insert(key.clone(), json!(value));
        }
        rows.push(row);
    }
    rows
}
fn parse_csv_line(line: &str) -> Vec<String> {
    let mut values = Vec::new();
    let mut current = String::new();
    let mut chars = line.chars().peekable();
    let mut quoted = false;
    while let Some(character) = chars.next() {
        if quoted {
            if character == '"' {
                if chars.peek() == Some(&'"') {
                    current.push('"');
                    chars.next();
                } else {
                    quoted = false;
                }
            } else {
                current.push(character);
            }
        } else if character == '"' {
            quoted = true;
        } else if character == ',' {
            values.push(current.trim().to_string());
            current.clear();
        } else {
            current.push(character);
        }
    }
    values.push(current.trim().to_string());
    values
}

fn compact_rows(rows: &[Map<String, Value>]) -> Vec<Value> {
    let mut order = Vec::<String>::new();
    let mut grouped = BTreeTaskMap::new();
    for row in rows {
        let name = row.get("TaskName").and_then(Value::as_str).unwrap_or("");
        if name.is_empty() || name == "TaskName" {
            continue;
        }
        let trigger = json!({"schedule":field(row,"Schedule Type"),"start_time":field(row,"Start Time"),"start_date":field(row,"Start Date"),"end_date":field(row,"End Date"),"days":field(row,"Days"),"months":field(row,"Months"),"repeat_every":field(row,"Repeat: Every"),"repeat_until_time":field(row,"Repeat: Until: Time"),"repeat_until_duration":field(row,"Repeat: Until: Duration"),"repeat_stop_if_still_running":field(row,"Repeat: Stop If Still Running"),"next_run":field(row,"Next Run Time")});
        if let Some(existing) = grouped.get_mut(name) {
            existing
                .get_mut("triggers")
                .and_then(Value::as_array_mut)
                .unwrap()
                .push(trigger);
            let count = existing["triggers"].as_array().map(Vec::len).unwrap_or(0);
            existing["trigger_count"] = json!(count);
        } else {
            order.push(name.to_string());
            grouped.insert(name.to_string(),json!({"task_name":name,"status":field(row,"Status"),"schedule":field(row,"Schedule Type"),"next_run":field(row,"Next Run Time"),"last_run":field(row,"Last Run Time"),"last_result":field(row,"Last Result"),"command":field(row,"Task To Run"),"trigger_count":1,"triggers":[trigger]}));
        }
    }
    order
        .into_iter()
        .filter_map(|key| grouped.remove(&key))
        .collect()
}
type BTreeTaskMap = std::collections::BTreeMap<String, Value>;
fn field(row: &Map<String, Value>, name: &str) -> Value {
    row.get(name).cloned().unwrap_or(Value::Null)
}

fn command_failure(
    code: &str,
    operation: &str,
    result: &CommandResult,
    task_name: &str,
    command: &str,
) -> Value {
    let combined = format!("{}\n{}", result.stdout, result.stderr).to_ascii_lowercase();
    let classification = if result.timed_out {
        "scheduler_command_timed_out"
    } else if combined.contains("access is denied") {
        "requires_elevation"
    } else if combined.contains("folder") || combined.contains("cannot find") {
        "invalid_task_path_or_missing_task"
    } else if combined.contains("invalid") || result.exit_code == 2147500037_i64 as i32 {
        "invalid_arguments_or_unsupported_scheduler_option"
    } else {
        "scheduler_command_failed"
    };
    error(
        code,
        &format!("{code}:{}", result.exit_code),
        json!({"operation":operation,"exit_code":result.exit_code,"classification":classification,"requires_elevation":classification=="requires_elevation","timed_out":result.timed_out,"timeout_ms":COMMAND_TIMEOUT.as_millis(),"task_name":task_name,"stdout":result.stdout,"stderr":result.stderr,"command":command,"remediation":if classification=="requires_elevation"{"Run the equivalent scheduler command from an elevated operator terminal."}else{"Inspect bounded stdout/stderr and retry with a concrete task path."}}),
    )
}

fn required(args: &Map<String, Value>, field: &str, code: &str) -> Result<String, Value> {
    args.get(field)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
        .ok_or_else(|| error(code, code, Value::Null))
}
fn optional(args: &Map<String, Value>, field: &str) -> Option<String> {
    args.get(field)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
}
