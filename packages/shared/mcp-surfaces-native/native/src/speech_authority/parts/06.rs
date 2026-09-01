fn admitted_existing_audio(path: &Path, root: &Path) -> Result<PathBuf, Value> {
    let candidate = if path.is_absolute() {
        path.to_path_buf()
    } else {
        root.join(path)
    };
    let canonical = candidate.canonicalize().map_err(|cause| {
        error(
            "speech_input_path_not_admitted",
            &cause.to_string(),
            json!({"path":candidate}),
        )
    })?;
    let metadata = fs::metadata(&canonical).map_err(|cause| {
        error(
            "speech_input_path_not_admitted",
            &cause.to_string(),
            json!({"path":canonical}),
        )
    })?;
    if !metadata.is_file() || metadata.len() == 0 || metadata.len() > MAX_AUDIO_BYTES as u64 {
        return Err(error(
            "speech_input_audio_invalid",
            "input audio must be a non-empty file no larger than 25 MiB",
            json!({"path":canonical,"size":metadata.len()}),
        ));
    }
    let mut roots = vec![env::temp_dir(), root.to_path_buf()];
    if let Ok(value) = env::var("NARADA_SPEECH_INPUT_ROOT") {
        if !value.trim().is_empty() {
            roots.push(PathBuf::from(value));
        }
    }
    let admitted = roots
        .iter()
        .filter_map(|value| value.canonicalize().ok())
        .any(|value| canonical.starts_with(value));
    if !admitted {
        return Err(error(
            "speech_input_path_not_admitted",
            "input audio is outside admitted roots",
            json!({"path":canonical}),
        ));
    }
    Ok(canonical)
}

fn provider_key(selection: &Selection) -> Result<String, Value> {
    selection.credential_env_names.iter().find_map(|name|env::var(name).ok().filter(|value|!value.trim().is_empty())).or_else(||env::var("OPENAI_API_KEY").ok().filter(|value|!value.trim().is_empty())).ok_or_else(||error("speech_provider_no_key",&selection.provider,json!({"remediation":"Configure a provider-registry credential environment variable."})))
}

fn validate_provider_url(value: &str) -> Result<(), Value> {
    let production =
        value == "https://api.openai.com" || value.starts_with("https://api.openai.com/");
    let test = bool_env("NARADA_SPEECH_ALLOW_INSECURE_TEST", false)
        && value.starts_with("http://127.0.0.1:");
    if production || test {
        Ok(())
    } else {
        Err(error(
            "speech_provider_base_url_not_allowed",
            "provider base URL is not admitted",
            json!({"origin":value}),
        ))
    }
}

fn provider_http_error(code: &str, status: u16, response: ureq::Response) -> Value {
    let detail = read_response(response, 8 * 1024)
        .ok()
        .map(|bytes| truncate(&bytes, 1000))
        .unwrap_or_default();
    error(
        code,
        &format!("provider returned HTTP {status}"),
        json!({"status":status,"detail":detail}),
    )
}

fn read_response(response: ureq::Response, maximum: u64) -> Result<Vec<u8>, Value> {
    let mut bytes = Vec::new();
    response
        .into_reader()
        .take(maximum + 1)
        .read_to_end(&mut bytes)
        .map_err(|cause| {
            error(
                "speech_provider_response_read_failed",
                &cause.to_string(),
                json!({}),
            )
        })?;
    if bytes.len() as u64 > maximum {
        return Err(error(
            "speech_provider_response_too_large",
            "provider response exceeded bound",
            json!({"maximum":maximum}),
        ));
    }
    Ok(bytes)
}

fn transcript_from_monitor(value: &Value) -> Option<Value> {
    let text = value
        .get("transcript")
        .and_then(|value| {
            value
                .as_str()
                .or_else(|| value.get("text").and_then(Value::as_str))
        })
        .or_else(|| value.get("recognized_text").and_then(Value::as_str))
        .or_else(|| value.get("text").and_then(Value::as_str))
        .filter(|value| !value.trim().is_empty())?;
    Some(
        json!({"present":true,"text":text,"raw":value.get("transcript").cloned().unwrap_or_else(||json!(text))}),
    )
}

fn compact_monitor(value: &Value) -> Value {
    let keys = [
        "schema",
        "status",
        "speech_detected",
        "selected_segment_duration_ms",
        "retained_audio_path",
        "device",
        "calibrated",
    ];
    let mut output = Map::new();
    if let Some(object) = value.as_object() {
        for key in keys {
            if let Some(value) = object.get(key) {
                output.insert(key.to_string(), value.clone());
            }
        }
    }
    Value::Object(output)
}

fn assert_remote_egress(selection: &Selection) -> Result<(), Value> {
    if selection.adapter == "openai-transcription"
        && !bool_env("NARADA_SPEECH_ALLOW_REMOTE_AUDIO_EGRESS", false)
    {
        Err(error(
            "speech_remote_audio_egress_not_admitted",
            "remote microphone audio egress is not admitted",
            json!({"provider":selection.provider,"remediation":"Set NARADA_SPEECH_ALLOW_REMOTE_AUDIO_EGRESS only after explicit operator admission."}),
        ))
    } else {
        Ok(())
    }
}

fn listen_adapter(root: &Path) -> PathBuf {
    env::var("NARADA_SPEECH_LISTEN_ADAPTER_PATH").ok().filter(|value|!value.trim().is_empty()).map(PathBuf::from).unwrap_or_else(||{let local=root.join("tools/operator-surface-carriers/Start-VoiceIntentLocalMonitor.ps1");if local.exists(){local}else{env::var("USERPROFILE").map(PathBuf::from).unwrap_or_default().join("src/narada/packages/operator-surface-carriers/src/Start-VoiceIntentLocalMonitor.ps1")}})
}

fn reap_sessions() {
    if let Ok(mut values) = sessions().lock() {
        let now = Instant::now();
        let stale = values
            .iter_mut()
            .filter_map(|(id, session)| match session.child.try_wait() {
                Ok(Some(_)) => Some((id.clone(), false)),
                Err(_) => Some((id.clone(), false)),
                Ok(None) if now >= session.deadline => Some((id.clone(), true)),
                _ => None,
            })
            .collect::<Vec<_>>();
        for (id, kill) in stale {
            if let Some(mut session) = values.remove(&id) {
                if kill {
                    let _ = session.child.kill();
                }
                let _ = session.child.wait();
            }
        }
    }
}

fn shutdown() {
    if let Ok(mut values) = sessions().lock() {
        for (_, mut session) in values.drain() {
            let _ = session.child.kill();
            let _ = session.child.wait();
        }
    }
}

fn powershell() -> Command {
    Command::new(
        env::var("NARADA_SPEECH_POWERSHELL")
            .ok()
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| "powershell.exe".to_string()),
    )
}
fn hide(command: &mut Command) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x0800_0000);
    }
}
fn bool_env(name: &str, default: bool) -> bool {
    env::var(name)
        .ok()
        .map(|value| {
            matches!(
                value.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on" | "allow" | "admitted"
            )
        })
        .unwrap_or(default)
}
fn provider_timeout() -> Result<Duration, Value> {
    let milliseconds = env::var("NARADA_SPEECH_PROVIDER_TIMEOUT_MS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(60_000);
    if !(100..=120_000).contains(&milliseconds) {
        return Err(error(
            "speech_provider_timeout_invalid",
            "NARADA_SPEECH_PROVIDER_TIMEOUT_MS must be between 100 and 120000",
            json!({"minimum":100,"maximum":120000}),
        ));
    }
    Ok(Duration::from_millis(milliseconds))
}
fn integer(
    args: &Map<String, Value>,
    key: &str,
    default: u64,
    minimum: u64,
    maximum: u64,
) -> Result<u64, Value> {
    let value = args.get(key).and_then(Value::as_u64).unwrap_or(default);
    if !(minimum..=maximum).contains(&value) {
        Err(error(
            "speech_integer_out_of_range",
            key,
            json!({"field":key,"minimum":minimum,"maximum":maximum}),
        ))
    } else {
        Ok(value)
    }
}
fn pointer_component(value: &str) -> String {
    value.replace('~', "~0").replace('/', "~1")
}
fn now() -> String {
    OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .unwrap_or_else(|_| "1970-01-01T00:00:00Z".to_string())
}
fn truncate(bytes: &[u8], maximum: usize) -> String {
    String::from_utf8_lossy(&bytes[..bytes.len().min(maximum)]).to_string()
}
fn error(code: &str, message: &str, details: Value) -> Value {
    json!({"schema":"narada.speech.error.v1","code":code,"message":message.chars().take(2000).collect::<String>(),"details":details})
}
