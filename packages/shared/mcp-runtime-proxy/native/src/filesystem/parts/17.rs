
fn directory_fingerprint(path: &Path, root: &Path) -> (usize, usize, String, bool) {
    let mut entries = Vec::new();
    let mut truncated = false;
    walk_directory(path, root, &mut entries, &mut truncated);
    let direct_count = fs::read_dir(path).map(|iter| iter.count()).unwrap_or(0);
    (
        direct_count,
        entries.len(),
        sha256_bytes(entries.join("\n").as_bytes()),
        truncated,
    )
}

fn walk_directory(path: &Path, root: &Path, entries: &mut Vec<String>, truncated: &mut bool) {
    if entries.len() >= 5000 {
        *truncated = true;
        return;
    }
    let Ok(iter) = fs::read_dir(path) else { return };
    let mut children: Vec<_> = iter.filter_map(Result::ok).collect();
    children.sort_by_key(|entry| entry.file_name());
    for entry in children {
        if entries.len() >= 5000 {
            *truncated = true;
            return;
        }
        let child = entry.path();
        let Ok(metadata) = fs::metadata(&child) else {
            continue;
        };
        let kind = if metadata.is_dir() {
            "directory"
        } else if metadata.is_file() {
            "file"
        } else {
            "other"
        };
        entries.push(format!(
            "{}\t{}\t{}\t{}",
            relative_path(root, &child),
            kind,
            metadata.len(),
            metadata
                .modified()
                .ok()
                .and_then(|value| value.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|value| value.as_millis())
                .unwrap_or(0)
        ));
        if metadata.is_dir() {
            walk_directory(&child, root, entries, truncated);
        }
    }
}

fn run_rg(args: &[String], timeout: u64, operation: &str) -> Result<(Vec<String>, bool), FsError> {
    let started = std::time::Instant::now();
    let mut child = Command::new("rg")
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| {
            FsError::new(
                format!("{operation}_failed"),
                format!("{operation}_failed: {error}"),
                json!({"operation": operation}),
            )
        })?;
    let stdout = child.stdout.take().expect("rg stdout");
    let stderr = child.stderr.take().expect("rg stderr");
    let (sender, receiver) = mpsc::sync_channel::<Result<Option<String>, String>>(64);
    thread::spawn(move || {
        let mut reader = BufReader::new(stdout);
        loop {
            let mut bytes = Vec::new();
            match reader.read_until(b'\n', &mut bytes) {
                Ok(0) => {
                    let _ = sender.send(Ok(None));
                    break;
                }
                Ok(_) => {
                    if bytes.len() > MAX_SEARCH_LINE_BYTES {
                        let _ = sender.send(Err("search_result_line_too_large".into()));
                        break;
                    }
                    while matches!(bytes.last(), Some(b'\n' | b'\r')) {
                        bytes.pop();
                    }
                    match String::from_utf8(bytes) {
                        Ok(line) => {
                            if sender.send(Ok(Some(line))).is_err() {
                                break;
                            }
                        }
                        Err(_) => {
                            let _ = sender.send(Err("search_result_not_utf8".into()));
                            break;
                        }
                    }
                }
                Err(error) => {
                    let _ = sender.send(Err(format!("search_stdout_read_failed: {error}")));
                    break;
                }
            }
        }
    });
    let stderr_reader = thread::spawn(move || {
        let mut reader = stderr.take((64 * 1024 + 1) as u64);
        let mut bytes = Vec::new();
        let _ = reader.read_to_end(&mut bytes);
        bytes
    });
    let mut matches = Vec::new();
    let mut bytes = 0_usize;
    let mut complete = false;
    let mut capture_limited = false;
    loop {
        let elapsed = started.elapsed().as_millis() as u64;
        if elapsed > timeout {
            let _ = child.kill();
            let _ = child.wait();
            return Err(FsError::new(
                format!("{operation}_timed_out"),
                format!("{operation}_timed_out"),
                json!({"operation":operation,"timeout_ms":timeout}),
            ));
        }
        match receiver.recv_timeout(Duration::from_millis(10)) {
            Ok(Ok(Some(line))) => {
                if line.trim().is_empty() {
                    continue;
                }
                bytes = bytes.saturating_add(line.len());
                if matches.len() >= MAX_SEARCH_CAPTURE_ENTRIES || bytes > MAX_SEARCH_CAPTURE_BYTES {
                    capture_limited = true;
                    let _ = child.kill();
                    break;
                }
                matches.push(line);
            }
            Ok(Ok(None)) => {
                complete = true;
                break;
            }
            Ok(Err(code)) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(FsError::new(
                    code.clone(),
                    code,
                    json!({"operation":operation}),
                ));
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                if child.try_wait().ok().flatten().is_some() {
                    complete = true;
                    break;
                }
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                complete = true;
                break;
            }
        }
    }
    let status = child.wait().map_err(|error| {
        FsError::new(
            format!("{operation}_failed"),
            format!("{operation}_failed: {error}"),
            json!({"operation":operation}),
        )
    })?;
    let stderr = stderr_reader.join().unwrap_or_default();
    if !capture_limited && status.code().unwrap_or(2) > 1 {
        return Err(FsError::new(
            format!("{operation}_failed"),
            format!(
                "{operation}_failed: {}",
                String::from_utf8_lossy(&stderr).trim()
            ),
            json!({"operation": operation, "status": status.code(),"stderr_truncated":stderr.len()>64*1024}),
        ));
    }
    Ok((matches, complete && !capture_limited))
}

fn render_grep(line: &str, mode: &str) -> String {
    let fields: Vec<&str> = line.split('\u{1f}').collect();
    match mode {
        "count_matches" => {
            if fields.len() >= 2 {
                format!("{}: {}", fields[0], fields[1])
            } else {
                line.to_string()
            }
        }
        "content" => {
            if fields.len() >= 3 {
                format!("{}:{}:{}", fields[0], fields[1], fields[2..].join("\u{1f}"))
            } else {
                line.to_string()
            }
        }
        _ => fields.first().copied().unwrap_or(line).to_string(),
    }
}
