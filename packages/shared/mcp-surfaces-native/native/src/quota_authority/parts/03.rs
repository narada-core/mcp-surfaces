pub fn overlay_start(args: &Map<String, Value>, root: &Path) -> Result<Value, Value> {
    let providers = provider_selection(args)?;
    let refresh = args
        .get("refresh_seconds")
        .and_then(Value::as_u64)
        .unwrap_or(60);
    let current = overlay_status(root);
    if current.get("running").and_then(Value::as_bool) == Some(true) {
        return Ok(
            json!({"schema":"narada.quota_meter.overlay_lifecycle.v1","status":"already_running","provider_selection":providers,"refresh_seconds":refresh,"overlay":current}),
        );
    }
    let script = overlay_script(root);
    if !script.is_file() {
        return Err(error(
            "quota_meter_overlay_script_not_found",
            json!({"path":script,"remediation":"Set QUOTA_METER_ROOT to the quota-meter checkout containing src/overlay.ps1."}),
        ));
    }
    let exe = env::current_exe().map_err(|e| {
        error(
            "quota_meter_native_executable_unavailable",
            json!({"message":e.to_string()}),
        )
    })?;
    let base = state_root(root);
    fs::create_dir_all(&base).map_err(|e| {
        error(
            "quota_meter_state_root_create_failed",
            json!({"message":e.to_string()}),
        )
    })?;
    let shell = env::var("QUOTA_METER_POWERSHELL").unwrap_or_else(|_| "pwsh".into());
    let mut command = Command::new(shell);
    command
        .args(["-NoProfile", "-ExecutionPolicy", "Bypass", "-File"])
        .arg(&script)
        .args(["-Action", "start", "-NativePath"])
        .arg(&exe)
        .args([
            "-ProviderSelection",
            &providers,
            "-RefreshSeconds",
            &refresh.to_string(),
            "-PidPath",
        ])
        .arg(base.join("overlay.pid"))
        .args(["-PositionPath"])
        .arg(base.join("overlay-position.json"))
        .args(["-RefreshPath"])
        .arg(base.join("overlay-refresh.signal"))
        .args(["-StatusPath"])
        .arg(base.join("overlay-status.json"))
        .args(["-LoginStatePath"])
        .arg(base.join("overlay-login-state.json"))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .creation_flags_hidden();
    command.spawn().map_err(|e| {
        error(
            "quota_meter_overlay_start_failed",
            json!({"message":e.to_string()}),
        )
    })?;
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        let status = overlay_status(root);
        if status.get("running").and_then(Value::as_bool) == Some(true) {
            return Ok(
                json!({"schema":"narada.quota_meter.overlay_lifecycle.v1","status":"started","provider_selection":providers,"refresh_seconds":refresh,"overlay":status}),
            );
        }
        thread::sleep(Duration::from_millis(50));
    }
    Err(error(
        "quota_meter_overlay_start_timeout",
        json!({"timeout_ms":5000}),
    ))
}

pub fn overlay_stop(root: &Path) -> Result<Value, Value> {
    let before = overlay_status(root);
    if before.get("running").and_then(Value::as_bool) != Some(true) {
        return Ok(
            json!({"schema":"narada.quota_meter.overlay_lifecycle.v1","status":"already_stopped","overlay":before}),
        );
    }
    let script = overlay_script(root);
    let base = state_root(root);
    let shell = env::var("QUOTA_METER_POWERSHELL").unwrap_or_else(|_| "pwsh".into());
    let output = Command::new(shell)
        .args(["-NoProfile", "-ExecutionPolicy", "Bypass", "-File"])
        .arg(script)
        .args(["-Action", "stop", "-NativePath"])
        .arg(env::current_exe().unwrap_or_default())
        .args(["-PidPath"])
        .arg(base.join("overlay.pid"))
        .args(["-PositionPath"])
        .arg(base.join("overlay-position.json"))
        .args(["-RefreshPath"])
        .arg(base.join("overlay-refresh.signal"))
        .args(["-StatusPath"])
        .arg(base.join("overlay-status.json"))
        .args(["-LoginStatePath"])
        .arg(base.join("overlay-login-state.json"))
        .creation_flags_hidden()
        .output()
        .map_err(|e| {
            error(
                "quota_meter_overlay_stop_failed",
                json!({"message":e.to_string()}),
            )
        })?;
    if !output.status.success() {
        return Err(error(
            "quota_meter_overlay_stop_failed",
            json!({"exit_code":output.status.code(),"stderr":String::from_utf8_lossy(&output.stderr).chars().take(2000).collect::<String>()}),
        ));
    }
    Ok(
        json!({"schema":"narada.quota_meter.overlay_lifecycle.v1","status":"stopped","overlay":overlay_status(root)}),
    )
}

fn overlay_script(root: &Path) -> PathBuf {
    if let Ok(path) = env::var("QUOTA_METER_OVERLAY_SCRIPT") {
        if !path.trim().is_empty() {
            return PathBuf::from(path);
        }
    }
    let source = env::var("NARADA_SRC_ROOT")
        .ok()
        .map(PathBuf::from)
        .or_else(|| {
            env::var("USERPROFILE")
                .ok()
                .map(|v| PathBuf::from(v).join("src"))
        })
        .unwrap_or_else(|| root.to_path_buf());
    let quota_root = env::var("QUOTA_METER_ROOT")
        .ok()
        .filter(|v| !v.trim().is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| source.join("quota-meter"));
    quota_root.join("src/overlay.ps1")
}
fn state_root(root: &Path) -> PathBuf {
    if let Ok(path) = env::var("QUOTA_METER_STATE_ROOT") {
        if !path.trim().is_empty() {
            return PathBuf::from(path);
        }
    }
    env::var("LOCALAPPDATA")
        .or_else(|_| env::var("TEMP"))
        .or_else(|_| env::var("TMP"))
        .map(PathBuf::from)
        .unwrap_or_else(|_| root.to_path_buf())
        .join("quota-meter")
}
fn bounded_file_json(path: &Path) -> Option<Value> {
    let meta = fs::metadata(path).ok()?;
    if meta.len() > 64_000 {
        return None;
    }
    serde_json::from_str(&fs::read_to_string(path).ok()?).ok()
}
fn process_alive(pid: u32) -> bool {
    #[cfg(windows)]
    {
        let output = Command::new("tasklist")
            .args(["/FI", &format!("PID eq {pid}"), "/FO", "CSV", "/NH"])
            .creation_flags_hidden()
            .output();
        return output
            .ok()
            .filter(|v| v.status.success())
            .map(|v| String::from_utf8_lossy(&v.stdout).contains(&format!("\"{pid}\"")))
            .unwrap_or(false);
    }
    #[cfg(not(windows))]
    {
        Command::new("kill")
            .args(["-0", &pid.to_string()])
            .status()
            .map(|v| v.success())
            .unwrap_or(false)
    }
}

fn provider_error(id: &str, name: &str, message: &str, fetched: &str, login: &str) -> Value {
    let auth =
        message.contains("auth") || message.contains("login") || message.contains("subscription");
    json!({"provider":id,"displayName":name,"status":if auth{"auth_required"}else{"unavailable"},"auth":{"mode":"unknown"},"windows":[],"usage":Value::Null,"metadata":{},"loginCommand":login,"error":{"code":if auth{"AUTH_REQUIRED"}else{"PROVIDER_UNAVAILABLE"},"message":message},"fetchedAt":fetched})
}
fn number(value: Option<&Value>) -> Option<f64> {
    value
        .and_then(|v| {
            v.as_f64()
                .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
        })
        .filter(|v| v.is_finite())
}
fn timestamp(value: Option<&Value>) -> Option<String> {
    let value = value?;
    if let Some(text) = value.as_str() {
        return OffsetDateTime::parse(text, &Rfc3339)
            .ok()
            .and_then(|v| v.format(&Rfc3339).ok());
    }
    let number = value.as_f64()?;
    let seconds = if number < 100_000_000_000.0 {
        number
    } else {
        number / 1000.0
    };
    OffsetDateTime::from_unix_timestamp(seconds as i64)
        .ok()
        .and_then(|v| v.format(&Rfc3339).ok())
}
fn duration_label(value: Option<f64>, fallback: &str) -> String {
    let Some(seconds) = value else {
        return fallback.into();
    };
    let seconds = seconds as u64;
    if seconds % 86400 == 0 {
        format!("{}d", seconds / 86400)
    } else if seconds % 3600 == 0 {
        format!("{}h", seconds / 3600)
    } else if seconds % 60 == 0 {
        format!("{}m", seconds / 60)
    } else {
        format!("{seconds}s")
    }
}
fn epoch_ms(value: Option<&Value>) -> Option<f64> {
    let text = value?.as_str()?;
    OffsetDateTime::parse(text, &Rfc3339)
        .ok()
        .map(|v| v.unix_timestamp_nanos() as f64 / 1_000_000.0)
}
fn epoch_now() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64()
        * 1000.0
}
fn now_iso() -> String {
    OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .unwrap_or_default()
}
fn bounded_json(value: &Value) -> String {
    serde_json::to_string(value)
        .unwrap_or_default()
        .chars()
        .take(2000)
        .collect()
}
fn error(code: &str, details: Value) -> Value {
    json!({"schema":"narada.quota_meter.error.v1","code":code,"message":code,"details":details})
}
fn render_error(value: Value) -> String {
    value
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or("quota_meter_error")
        .into()
}

trait HiddenCommand {
    fn creation_flags_hidden(&mut self) -> &mut Self;
}
