
fn resolve_command_for_spawn(
    command: &str,
    args: &[String],
    environment: &std::collections::HashMap<String, String>,
) -> (PathBuf, Vec<String>) {
    if !cfg!(windows) || Path::new(command).extension().is_some() {
        return (PathBuf::from(command), args.to_vec());
    }
    let Some(path) = environment
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case("PATH"))
        .map(|(_, value)| value)
    else {
        return (PathBuf::from(command), args.to_vec());
    };
    if let Some(resolved) = resolve_corepack_pnpm(command, path, args) {
        return resolved;
    }
    for directory in env::split_paths(path) {
        for extension in [".exe", ".com", ".ps1", ".cmd", ".bat", ""] {
            let candidate = directory.join(format!("{command}{extension}"));
            if !candidate.is_file() {
                continue;
            }
            if extension == ".ps1" {
                let mut wrapped = vec![
                    "-NoLogo".to_string(),
                    "-NoProfile".to_string(),
                    "-NonInteractive".to_string(),
                    "-ExecutionPolicy".to_string(),
                    "Bypass".to_string(),
                    "-File".to_string(),
                    candidate.to_string_lossy().to_string(),
                ];
                wrapped.extend_from_slice(args);
                return (resolve_noninteractive_powershell(environment), wrapped);
            }
            if extension == ".cmd" || extension == ".bat" {
                let script = candidate.with_extension("ps1");
                if script.is_file() {
                    let mut wrapped = vec![
                        "-NoLogo".to_string(),
                        "-NoProfile".to_string(),
                        "-NonInteractive".to_string(),
                        "-ExecutionPolicy".to_string(),
                        "Bypass".to_string(),
                        "-File".to_string(),
                        script.to_string_lossy().to_string(),
                    ];
                    wrapped.extend_from_slice(args);
                    return (resolve_noninteractive_powershell(environment), wrapped);
                }
            }
            return (candidate, args.to_vec());
        }
    }
    (PathBuf::from(command), args.to_vec())
}

fn resolve_corepack_pnpm(
    command: &str,
    path: &str,
    args: &[String],
) -> Option<(PathBuf, Vec<String>)> {
    if !command.eq_ignore_ascii_case("pnpm") {
        return None;
    }
    for directory in env::split_paths(path) {
        let node = directory.join("node.exe");
        let entrypoint = directory.join("node_modules/corepack/dist/pnpm.js");
        if node.is_file() && entrypoint.is_file() {
            let mut direct_args = vec![entrypoint.to_string_lossy().to_string()];
            direct_args.extend_from_slice(args);
            return Some((node, direct_args));
        }
    }
    None
}

fn resolve_noninteractive_powershell(
    environment: &std::collections::HashMap<String, String>,
) -> PathBuf {
    let native_pwsh = environment
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case("PATH"))
        .and_then(|(_, value)| {
            env::split_paths(value)
                .map(|directory| directory.join("pwsh.exe"))
                .find(|candidate| {
                    candidate.is_file()
                        && !candidate
                            .to_string_lossy()
                            .to_ascii_lowercase()
                            .contains("\\windowsapps\\")
                })
        });
    if let Some(executable) = native_pwsh {
        return executable;
    }
    let system_root = environment
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case("SystemRoot"))
        .map(|(_, value)| PathBuf::from(value))
        .unwrap_or_else(|| PathBuf::from(r"C:\Windows"));
    system_root.join(r"System32\WindowsPowerShell\v1.0\powershell.exe")
}
fn read_bounded<R: Read>(mut reader: R, max: usize) -> (Vec<u8>, bool) {
    let mut output = Vec::with_capacity(max.min(8192));
    let mut buffer = [0_u8; 8192];
    let mut truncated = false;
    loop {
        match reader.read(&mut buffer) {
            Ok(0) => break,
            Ok(count) => {
                if output.len() < max {
                    let keep = (max - output.len()).min(count);
                    output.extend_from_slice(&buffer[..keep]);
                    if keep < count {
                        truncated = true;
                    }
                } else {
                    truncated = true;
                }
            }
            Err(_) => break,
        }
    }
    (output, truncated)
}

fn apply_headless_process_posture(command: &mut Command) {
    for variable in TERMINAL_INTEGRATION_ENVIRONMENT {
        command.env_remove(variable);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(CREATE_NO_WINDOW);
    }
}

fn kill_child(child: &mut Child) {
    #[cfg(windows)]
    {
        let pid = child.id();
        let mut command = Command::new("taskkill");
        command
            .args(["/PID", &pid.to_string(), "/T", "/F"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        apply_headless_process_posture(&mut command);
        let _ = command.status();
    }
    #[cfg(not(windows))]
    {
        let _ = child.kill();
    }
}

fn audit(state: &State, payload: &Value) {
    let Some(directory) = &state.audit_log_dir else {
        return;
    };
    if fs::create_dir_all(directory).is_err() {
        return;
    }
    let path = directory.join("structured-command.jsonl");
    let Ok(mut file) = OpenOptions::new().create(true).append(true).open(path) else {
        return;
    };
    let _ = writeln!(
        file,
        "{}",
        serde_json::to_string(payload).unwrap_or_else(|_| "{}".to_string())
    );
}

fn inside_any_root(path: &Path, roots: &[PathBuf]) -> bool {
    let candidate_key = path_key(path);
    roots.iter().any(|root| {
        let root_key = path_key(root);
        candidate_key == root_key || candidate_key.starts_with(&(root_key + "/"))
    })
}

fn path_key(path: &Path) -> String {
    let value = path.to_string_lossy().replace('\\', "/");
    value.trim_end_matches('/').to_ascii_lowercase()
}

fn resolve_path(value: &str, base: &Path) -> PathBuf {
    let path = PathBuf::from(value);
    if path.is_absolute() {
        absolute(path)
    } else {
        absolute(base.join(path))
    }
}
