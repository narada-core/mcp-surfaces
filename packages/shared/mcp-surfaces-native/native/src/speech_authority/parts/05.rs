fn openai_transcribe(selection: &Selection, path: &Path) -> Result<Value, Value> {
    validate_provider_url(&selection.base_url)?;
    let bytes = fs::read(path).map_err(|cause| {
        error(
            "speech_capture_audio_read_failed",
            &cause.to_string(),
            json!({"path":path}),
        )
    })?;
    if bytes.is_empty() {
        return Err(error(
            "speech_capture_audio_empty",
            "captured audio is empty",
            json!({"path":path}),
        ));
    }
    if bytes.len() > MAX_AUDIO_BYTES {
        return Err(error(
            "speech_capture_audio_too_large",
            "captured audio exceeds 25 MiB",
            json!({"size":bytes.len()}),
        ));
    }
    let boundary = format!("narada-{}", Uuid::new_v4());
    let mut body = Vec::with_capacity(bytes.len() + 1024);
    body.extend_from_slice(format!("--{boundary}\r\nContent-Disposition: form-data; name=\"model\"\r\n\r\n{}\r\n--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"audio.wav\"\r\nContent-Type: audio/wav\r\n\r\n",selection.model).as_bytes());
    body.extend_from_slice(&bytes);
    body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
    let key = provider_key(selection)?;
    let response = ureq::post(&format!("{}/v1/audio/transcriptions", selection.base_url))
        .set("Authorization", &format!("Bearer {key}"))
        .set(
            "Content-Type",
            &format!("multipart/form-data; boundary={boundary}"),
        )
        .timeout(provider_timeout()?)
        .send_bytes(&body);
    let response = match response {
        Ok(value) => value,
        Err(ureq::Error::Status(code, value)) => {
            return Err(provider_http_error(
                "speech_openai_transcription_api_error",
                code,
                value,
            ))
        }
        Err(cause) => {
            return Err(error(
                "speech_openai_transcription_request_failed",
                &cause.to_string(),
                json!({}),
            ))
        }
    };
    let value: Value =
        serde_json::from_slice(&read_response(response, MAX_PROVIDER_RESPONSE_BYTES)?).map_err(
            |cause| {
                error(
                    "speech_openai_transcription_invalid_json",
                    &cause.to_string(),
                    json!({}),
                )
            },
        )?;
    let text = value
        .get("text")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| {
            error(
                "speech_openai_transcription_empty",
                "provider returned no transcript",
                json!({}),
            )
        })?;
    Ok(json!({"present":true,"text":text,"raw":value}))
}

fn play_wave(path: &Path) -> Result<(), Value> {
    let mut command = powershell();
    command.args(["-NoProfile","-NonInteractive","-Command",r#"$p=[System.Media.SoundPlayer]::new($env:NARADA_SPEECH_WAVE); try {$p.PlaySync()} finally {$p.Dispose()}"#]).env("NARADA_SPEECH_WAVE",path);
    let result = run_bounded(command, Duration::from_secs(60))?;
    if !result.status.success() {
        return Err(error(
            "speech_playback_failed",
            "WAV playback failed",
            json!({"exit_code":result.status.code(),"stderr":truncate(&result.stderr,2000)}),
        ));
    }
    Ok(())
}

fn play_listen_cue(phase: &str) {
    if !bool_env("NARADA_SPEECH_LISTEN_AUDIO_CUES", true) {
        return;
    }
    let Ok(_lock) = AudibleLock::acquire() else {
        return;
    };
    let sound = if phase == "start" { "Asterisk" } else { "Beep" };
    let mut command = powershell();
    command.args(["-NoProfile","-NonInteractive","-Command",r#"$sound=[string]$env:NARADA_SPEECH_CUE;if($sound -eq 'Asterisk'){[System.Media.SystemSounds]::Asterisk.Play()}else{[System.Media.SystemSounds]::Beep.Play()};Start-Sleep -Milliseconds 220"#]).env("NARADA_SPEECH_CUE",sound);
    let _ = run_bounded(command, Duration::from_secs(5));
}

struct BoundedOutput {
    status: ExitStatus,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}
fn run_bounded(mut command: Command, timeout: Duration) -> Result<BoundedOutput, Value> {
    hide(&mut command);
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command
        .spawn()
        .map_err(|cause| error("speech_process_start_failed", &cause.to_string(), json!({})))?;
    let stdout = child.stdout.take().expect("piped stdout");
    let stderr = child.stderr.take().expect("piped stderr");
    let out = thread::spawn(move || {
        let mut bytes = Vec::new();
        let _ = stdout
            .take(MAX_PROCESS_OUTPUT_BYTES + 1)
            .read_to_end(&mut bytes);
        bytes
    });
    let err = thread::spawn(move || {
        let mut bytes = Vec::new();
        let _ = stderr
            .take(MAX_PROCESS_OUTPUT_BYTES + 1)
            .read_to_end(&mut bytes);
        bytes
    });
    let deadline = Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(20)),
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(error(
                    "speech_process_timeout",
                    "speech authority process timed out",
                    json!({"timeout_ms":timeout.as_millis()}),
                ));
            }
            Err(cause) => {
                return Err(error(
                    "speech_process_wait_failed",
                    &cause.to_string(),
                    json!({}),
                ))
            }
        }
    };
    let stdout = out.join().unwrap_or_default();
    let stderr = err.join().unwrap_or_default();
    if stdout.len() as u64 > MAX_PROCESS_OUTPUT_BYTES
        || stderr.len() as u64 > MAX_PROCESS_OUTPUT_BYTES
    {
        return Err(error(
            "speech_process_output_too_large",
            "speech process output exceeded 256 KiB",
            json!({}),
        ));
    }
    Ok(BoundedOutput {
        status,
        stdout,
        stderr,
    })
}

fn output_path(
    args: &Map<String, Value>,
    root: &Path,
    retain: bool,
) -> Result<Option<PathBuf>, Value> {
    if !retain {
        return Ok(None);
    }
    let candidate = args
        .get("output_path")
        .and_then(Value::as_str)
        .map(PathBuf::from)
        .unwrap_or_else(|| env::temp_dir().join(format!("narada-speech-{}.wav", Uuid::new_v4())));
    let candidate = if candidate.is_absolute() {
        candidate
    } else {
        root.join(candidate)
    };
    let parent = candidate.parent().ok_or_else(|| {
        error(
            "speech_output_path_not_admitted",
            "output path has no parent",
            json!({"path":candidate}),
        )
    })?;
    fs::create_dir_all(parent).map_err(|cause| {
        error(
            "speech_output_create_failed",
            &cause.to_string(),
            json!({"path":candidate}),
        )
    })?;
    let canonical_parent = parent.canonicalize().map_err(|cause| {
        error(
            "speech_output_path_not_admitted",
            &cause.to_string(),
            json!({"path":candidate}),
        )
    })?;
    let mut roots = vec![env::temp_dir(), root.to_path_buf()];
    if let Ok(value) = env::var("NARADA_SPEECH_OUTPUT_ROOT") {
        if !value.trim().is_empty() {
            roots.push(PathBuf::from(value));
        }
    }
    let admitted = roots
        .iter()
        .filter_map(|value| value.canonicalize().ok())
        .any(|value| canonical_parent.starts_with(value));
    if !admitted {
        return Err(error(
            "speech_output_path_not_admitted",
            "output path is outside admitted roots",
            json!({"path":candidate}),
        ));
    }
    Ok(Some(candidate))
}

