fn prompt_capture(args: &Map<String, Value>, root: &Path) -> Result<Value, Value> {
    let mut speak_args = Map::new();
    for key in [
        "text",
        "rate",
        "speed",
        "speaker_agent_id",
        "announce_speaker",
    ] {
        if let Some(value) = args.get(key) {
            speak_args.insert(key.to_string(), value.clone());
        }
    }
    if let Some(value) = args.get("tts_selection") {
        speak_args.insert("selection".to_string(), value.clone());
    }
    let prompt = speak(&speak_args, root)?;
    let mut capture_args = Map::new();
    for key in ["duration_seconds", "device", "retain_audio"] {
        if let Some(value) = args.get(key) {
            capture_args.insert(key.to_string(), value.clone());
        }
    }
    if let Some(value) = args.get("transcription_selection") {
        capture_args.insert("selection".to_string(), value.clone());
    }
    let capture = match capture_transcribe(&capture_args, root) {
        Ok(value) => value,
        Err(value)
            if matches!(
                value.get("code").and_then(Value::as_str),
                Some(
                    "speech_capture_no_audio"
                        | "speech_local_transcription_unavailable"
                        | "speech_openai_transcription_empty"
                )
            ) =>
        {
            return Ok(
                json!({"schema":"narada.speech.prompt_capture_response.v1","status":"no_response","reason":value.get("code").cloned().unwrap_or(Value::Null),"prompt":prompt,"capture_error":value,"response":{"present":false,"text":Value::Null}}),
            )
        }
        Err(value) => return Err(value),
    };
    let transcript = capture.get("transcript").cloned().unwrap_or(Value::Null);
    let text = transcript
        .get("text")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty());
    let minimum = args
        .get("no_response_min_speech_ms")
        .and_then(Value::as_u64)
        .unwrap_or(300);
    let observed = capture
        .pointer("/monitor/selected_segment_duration_ms")
        .and_then(Value::as_u64)
        .unwrap_or(if text.is_some() { minimum } else { 0 });
    if text.is_none() || observed < minimum {
        return Ok(
            json!({"schema":"narada.speech.prompt_capture_response.v1","status":"no_response","reason":if text.is_none(){"empty_transcript"}else{"speech_segment_too_short"},"prompt":prompt,"capture":capture,"response":{"present":false,"text":Value::Null},"no_response_min_speech_ms":minimum}),
        );
    }
    Ok(
        json!({"schema":"narada.speech.prompt_capture_response.v1","status":"responded","prompt":prompt,"capture":capture,"response":{"present":true,"text":text},"transcript":transcript,"no_response_min_speech_ms":minimum}),
    )
}

fn run_capture_adapter(
    args: &Map<String, Value>,
    root: &Path,
    selection: &Selection,
    duration: u64,
) -> Result<Value, Value> {
    let adapter = listen_adapter(root);
    if !adapter.exists() {
        return Err(error(
            "speech_listen_adapter_missing",
            "capture adapter is missing",
            json!({"path":adapter,"remediation":"Set NARADA_SPEECH_LISTEN_ADAPTER_PATH."}),
        ));
    }
    let mut command = powershell();
    command
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-ExecutionPolicy",
            "Bypass",
            "-File",
        ])
        .arg(&adapter)
        .args([
            "-DurationSeconds",
            &duration.to_string(),
            "-RetainAudio",
            "-DispatchDryRun",
            "-PassThru",
        ]);
    if let Some(path) = args.get("input_wav").and_then(Value::as_str) {
        command
            .arg("-InputWav")
            .arg(admitted_existing_audio(Path::new(path), root)?);
    }
    if let Some(device) = args.get("device").and_then(Value::as_str) {
        command.args(["-Device", device]);
    }
    if args.get("self_test_synthetic").and_then(Value::as_bool) == Some(true) {
        command.arg("-SelfTestSynthetic");
    }
    if args.get("calibrate").and_then(Value::as_bool) == Some(true) {
        command.arg("-Calibrate");
    }
    if args.get("retain_audio").and_then(Value::as_bool) != Some(true) {
        command.arg("-DisableDebugAudioCues");
    }
    if selection.adapter == "openai-transcription" {
        command.env("NARADA_SPEECH_CAPTURE_RETAIN_REQUIRED", "1");
    }
    play_listen_cue("start");
    let output = run_bounded(command, Duration::from_secs(duration + 90));
    play_listen_cue("end");
    let output = output?;
    if !output.status.success() {
        return Err(error(
            "speech_capture_failed",
            "capture adapter exited unsuccessfully",
            json!({"exit_code":output.status.code(),"stderr":truncate(&output.stderr,1000)}),
        ));
    }
    serde_json::from_slice(&output.stdout).map_err(|cause| {
        error(
            "speech_capture_output_invalid_json",
            &cause.to_string(),
            json!({"stdout":truncate(&output.stdout,1000),"stderr":truncate(&output.stderr,1000)}),
        )
    })
}

fn sapi_speak(
    text: &str,
    args: &Map<String, Value>,
    voice: Option<&str>,
    output: Option<&Path>,
) -> Result<(), Value> {
    let mut command = powershell();
    command.args(["-NoProfile","-NonInteractive","-Command",r#"Add-Type -AssemblyName System.Speech; $s=[System.Speech.Synthesis.SpeechSynthesizer]::new(); try { $s.Rate=[int]$env:NARADA_SPEECH_RATE; if($env:NARADA_SPEECH_VOICE){$s.SelectVoice($env:NARADA_SPEECH_VOICE)}; if($env:NARADA_SPEECH_OUTPUT){$s.SetOutputToWaveFile($env:NARADA_SPEECH_OUTPUT)}; $s.Speak($env:NARADA_SPEECH_TEXT) } finally { $s.Dispose() }"#]);
    command.env("NARADA_SPEECH_TEXT", text).env(
        "NARADA_SPEECH_RATE",
        args.get("rate")
            .and_then(Value::as_i64)
            .unwrap_or(0)
            .to_string(),
    );
    if let Some(voice) = voice {
        command.env("NARADA_SPEECH_VOICE", voice);
    }
    if let Some(path) = output {
        command.env("NARADA_SPEECH_OUTPUT", path);
    }
    let result = run_bounded(command, Duration::from_secs(60))?;
    if !result.status.success() {
        return Err(error(
            "speech_sapi_failed",
            "SAPI synthesis failed",
            json!({"exit_code":result.status.code(),"stderr":truncate(&result.stderr,2000)}),
        ));
    }
    Ok(())
}

fn sapi_voices() -> Result<Vec<Value>, Value> {
    let mut command = powershell();
    command.args(["-NoProfile","-NonInteractive","-Command",r#"Add-Type -AssemblyName System.Speech; $s=[System.Speech.Synthesis.SpeechSynthesizer]::new(); try { @($s.GetInstalledVoices()|ForEach-Object { @{id=$_.VoiceInfo.Name;name=$_.VoiceInfo.Name;culture=$_.VoiceInfo.Culture.Name;gender=$_.VoiceInfo.Gender.ToString()} })|ConvertTo-Json -Compress } finally {$s.Dispose()}"#]);
    let result = run_bounded(command, Duration::from_secs(15))?;
    if !result.status.success() {
        return Err(error(
            "speech_sapi_failed",
            "SAPI voice listing failed",
            json!({"exit_code":result.status.code(),"stderr":truncate(&result.stderr,2000)}),
        ));
    }
    let value: Value = serde_json::from_slice(&result.stdout)
        .map_err(|cause| error("speech_sapi_output_invalid", &cause.to_string(), json!({})))?;
    Ok(match value {
        Value::Array(values) => values.into_iter().take(100).collect(),
        Value::Object(_) => vec![value],
        _ => Vec::new(),
    })
}

fn openai_speak(
    selection: &Selection,
    text: &str,
    args: &Map<String, Value>,
    output: Option<&Path>,
    retain: bool,
) -> Result<(), Value> {
    validate_provider_url(&selection.base_url)?;
    let key = provider_key(selection)?;
    let voice = selection.voice.as_deref().ok_or_else(|| {
        error(
            "speech_provider_voice_required",
            &selection.provider,
            json!({"model":selection.model}),
        )
    })?;
    let url = format!("{}/v1/audio/speech", selection.base_url);
    let body = json!({"model":selection.model,"voice":voice,"input":text,"response_format":"wav","speed":args.get("speed").and_then(Value::as_f64).unwrap_or(1.0)});
    let encoded = serde_json::to_vec(&body).map_err(|cause| {
        error(
            "speech_openai_request_encode_failed",
            &cause.to_string(),
            json!({}),
        )
    })?;
    let response = ureq::post(&url)
        .set("Authorization", &format!("Bearer {key}"))
        .set("Content-Type", "application/json")
        .timeout(provider_timeout()?)
        .send_bytes(&encoded);
    let response = match response {
        Ok(value) => value,
        Err(ureq::Error::Status(code, value)) => {
            return Err(provider_http_error("speech_openai_api_error", code, value))
        }
        Err(cause) => {
            return Err(error(
                "speech_openai_request_failed",
                &cause.to_string(),
                json!({}),
            ))
        }
    };
    let bytes = read_response(response, MAX_AUDIO_BYTES as u64)?;
    if !bytes.starts_with(b"RIFF") {
        return Err(error(
            "speech_openai_audio_invalid",
            "TTS provider did not return a WAV document",
            json!({"size":bytes.len()}),
        ));
    }
    let generated = output
        .map(PathBuf::from)
        .unwrap_or_else(|| env::temp_dir().join(format!("narada-speech-{}.wav", Uuid::new_v4())));
    if let Some(parent) = generated.parent() {
        fs::create_dir_all(parent).map_err(|cause| {
            error(
                "speech_output_create_failed",
                &cause.to_string(),
                json!({"path":generated}),
            )
        })?;
    }
    fs::write(&generated, &bytes).map_err(|cause| {
        error(
            "speech_output_write_failed",
            &cause.to_string(),
            json!({"path":generated}),
        )
    })?;
    if !bool_env("NARADA_SPEECH_DISABLE_PLAYBACK_TEST", false) {
        play_wave(&generated)?;
    }
    if !retain && output.is_none() {
        let _ = fs::remove_file(generated);
    }
    Ok(())
}

