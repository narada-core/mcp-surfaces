fn speak(args: &Map<String, Value>, root: &Path) -> Result<Value, Value> {
    let text = args
        .get("text")
        .and_then(Value::as_str)
        .ok_or_else(|| error("speech_requires_text", "text is required", json!({})))?;
    if text.chars().count() > MAX_TEXT_CHARS {
        return Err(error(
            "speech_text_too_large",
            "text exceeds 1000 characters",
            json!({"maximum":MAX_TEXT_CHARS}),
        ));
    }
    let selection = resolve_selection(args, "selection", "tts", root)?;
    let announce = args
        .get("announce_speaker")
        .and_then(Value::as_bool)
        .unwrap_or_else(|| bool_env("NARADA_SPEECH_ANNOUNCE_SPEAKER", true));
    let speaker_from_env = env::var("NARADA_AGENT_ID")
        .ok()
        .or_else(|| env::var("NARADA_AGENT_NAME").ok());
    let speaker = args
        .get("speaker_agent_id")
        .and_then(Value::as_str)
        .or(speaker_from_env.as_deref())
        .map(ToOwned::to_owned);
    let prefix = if announce {
        speaker.as_deref().map(|value| format!("{value} here:"))
    } else {
        None
    };
    let spoken = prefix
        .as_deref()
        .map(|value| format!("{value} {text}"))
        .unwrap_or_else(|| text.to_string());
    let retain = args
        .get("retain_audio")
        .and_then(Value::as_bool)
        .unwrap_or(false)
        || args.contains_key("output_path");
    let output = output_path(args, root, retain)?;
    let audible_lock = AudibleLock::acquire()?;
    let lock_path = audible_lock.path.clone();
    match selection.adapter.as_str() {
        "sapi" => sapi_speak(&spoken, args, selection.voice.as_deref(), output.as_deref())?,
        "openai-tts" => openai_speak(&selection, &spoken, args, output.as_deref(), retain)?,
        _ => {
            return Err(error(
                "speech_provider_not_implemented",
                &selection.provider,
                json!({"adapter":selection.adapter}),
            ))
        }
    }
    drop(audible_lock);
    Ok(
        json!({"schema":"narada.speech.speak.v1","status":"spoken","provider":selection.provider,"adapter":selection.adapter,"model":selection.model,"voice":selection.voice,"resolved_selection":selection_public(&selection,"tts"),"selection_source":selection.source,"text_length":text.chars().count(),"spoken_text_length":spoken.chars().count(),"speaker_announcement":{"announced":prefix.is_some(),"agent_id":speaker,"prefix_text":prefix},"audio_path":output,"retained":retain,"audible_output":{"serialized":true,"lock_scope":"host","lock_dir":lock_path}}),
    )
}

fn voices(args: &Map<String, Value>, root: &Path) -> Result<Value, Value> {
    let selection = resolve_selection(args, "selection", "tts", root)?;
    let items = match selection.adapter.as_str() {
        "sapi" => sapi_voices()?,
        "openai-tts" => selection.voices.iter().map(|id| json!({"id":id})).collect(),
        _ => {
            return Err(error(
                "speech_provider_not_implemented",
                &selection.provider,
                json!({"adapter":selection.adapter}),
            ))
        }
    };
    Ok(
        json!({"schema":"narada.speech.voices.v1","status":"ok","provider":selection.provider,"model":selection.model,"resolved_selection":selection_public(&selection,"tts"),"voices":items,"count":items.len()}),
    )
}

fn listen_status(_args: &Map<String, Value>, root: &Path) -> Result<Value, Value> {
    reap_sessions();
    let adapter = listen_adapter(root);
    let values = sessions().lock().map_err(|_| {
        error(
            "speech_session_state_poisoned",
            "listen state unavailable",
            json!({}),
        )
    })?;
    let active = values.iter().take(100).map(|(id, session)| json!({"session_id":id,"provider":session.provider,"duration_seconds":session.duration_seconds,"started_at":session.started_at})).collect::<Vec<_>>();
    Ok(
        json!({"schema":"narada.speech.listen_status.v1","status":if adapter.exists(){"ready"}else{"blocked"},"adapter":{"ready":adapter.exists(),"path":adapter,"required":true,"remediation":if adapter.exists(){Value::Null}else{json!("Set NARADA_SPEECH_LISTEN_ADAPTER_PATH to the native capture adapter.")}},"policy":{"remote_audio_egress":if bool_env("NARADA_SPEECH_ALLOW_REMOTE_AUDIO_EGRESS",false){"admitted"}else{"forbidden_without_explicit_policy"},"max_duration_seconds":MAX_LISTEN_SECONDS,"audio_cues":bool_env("NARADA_SPEECH_LISTEN_AUDIO_CUES",true),"announce_speaker_default":bool_env("NARADA_SPEECH_ANNOUNCE_SPEAKER",true)},"active_sessions":active,"active_count":active.len()}),
    )
}

fn listen_start(args: &Map<String, Value>, root: &Path) -> Result<Value, Value> {
    reap_sessions();
    let selection = resolve_selection(args, "selection", "transcription", root)?;
    assert_remote_egress(&selection)?;
    let adapter = listen_adapter(root);
    if !adapter.exists() {
        return Err(error(
            "speech_listen_adapter_missing",
            "capture adapter is missing",
            json!({"path":adapter,"remediation":"Set NARADA_SPEECH_LISTEN_ADAPTER_PATH."}),
        ));
    }
    let duration = integer(args, "duration_seconds", 30, 1, MAX_LISTEN_SECONDS)?;
    let session_id = args
        .get("session_id")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| format!("listen-{}", Uuid::new_v4()));
    let mut values = sessions().lock().map_err(|_| {
        error(
            "speech_session_state_poisoned",
            "listen state unavailable",
            json!({}),
        )
    })?;
    if values.contains_key(&session_id) {
        return Err(error(
            "speech_listen_session_exists",
            &session_id,
            json!({"session_id":session_id}),
        ));
    }
    if values.len() >= 16 {
        return Err(error(
            "speech_listen_session_limit",
            "at most 16 sessions may be active",
            json!({"maximum":16}),
        ));
    }
    play_listen_cue("start");
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
        .args(["-DurationSeconds", &duration.to_string()]);
    if selection.adapter == "openai-transcription" {
        command.args(["-RecognitionAdapter", "openai-transcriptions"]);
    }
    if args.get("calibrate").and_then(Value::as_bool) == Some(true) {
        command.arg("-Calibrate");
    }
    command
        .arg("-DisableDebugAudioCues")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    hide(&mut command);
    let child = command.spawn().map_err(|cause| {
        error(
            "speech_listen_start_failed",
            &cause.to_string(),
            json!({"adapter":adapter}),
        )
    })?;
    let started_at = now();
    values.insert(
        session_id.clone(),
        ListenSession {
            child,
            provider: selection.provider.clone(),
            duration_seconds: duration,
            started_at: started_at.clone(),
            deadline: Instant::now() + Duration::from_secs(duration + 1),
        },
    );
    Ok(
        json!({"schema":"narada.speech.listen_start.v1","status":"started","session_id":session_id,"provider":selection.provider,"resolved_selection":selection_public(&selection,"transcription"),"selection_source":selection.source,"duration_seconds":duration,"calibrate":args.get("calibrate").and_then(Value::as_bool).unwrap_or(false),"bounded":true,"stop_tool":"speech_listen_stop","started_at":started_at}),
    )
}

fn listen_stop(args: &Map<String, Value>) -> Result<Value, Value> {
    reap_sessions();
    let requested = args
        .get("session_id")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned);
    let mut values = sessions().lock().map_err(|_| {
        error(
            "speech_session_state_poisoned",
            "listen state unavailable",
            json!({}),
        )
    })?;
    let ids = requested
        .clone()
        .map(|value| vec![value])
        .unwrap_or_else(|| values.keys().cloned().collect());
    let mut stopped = Vec::new();
    for id in ids {
        if let Some(mut session) = values.remove(&id) {
            let _ = session.child.kill();
            let _ = session.child.wait();
            stopped.push(id);
        }
    }
    if !stopped.is_empty() {
        play_listen_cue("end");
    }
    Ok(
        json!({"schema":"narada.speech.listen_stop.v1","status":"stopped","requested_session_id":requested,"stopped_session_ids":stopped,"active_count":values.len()}),
    )
}

fn capture_transcribe(args: &Map<String, Value>, root: &Path) -> Result<Value, Value> {
    let selection = resolve_selection(args, "selection", "transcription", root)?;
    assert_remote_egress(&selection)?;
    let duration = integer(args, "duration_seconds", 30, 1, MAX_LISTEN_SECONDS)?;
    let monitor = run_capture_adapter(args, root, &selection, duration)?;
    if monitor.get("schema").and_then(Value::as_str)
        == Some("narada.voice.local_audio_calibration.v0")
    {
        return Ok(
            json!({"schema":"narada.speech.capture_calibration.v1","status":"calibrated","provider":"local_audio","resolved_selection":selection_public(&selection,"transcription"),"duration_seconds":duration,"calibration":monitor,"privacy":{"remote_audio_egress":"not_used","raw_audio_retained":false}}),
        );
    }
    let transcript = if selection.adapter == "sapi" {
        transcript_from_monitor(&monitor).ok_or_else(|| {
            error(
                "speech_local_transcription_unavailable",
                "capture adapter returned no local transcript",
                json!({"monitor":compact_monitor(&monitor)}),
            )
        })?
    } else if selection.adapter == "openai-transcription" {
        let path = monitor
            .get("retained_audio_path")
            .and_then(Value::as_str)
            .ok_or_else(|| {
                error(
                    "speech_capture_no_audio",
                    "capture adapter returned no retained audio",
                    json!({"monitor":compact_monitor(&monitor)}),
                )
            })?;
        let admitted = admitted_existing_audio(Path::new(path), root)?;
        let result = openai_transcribe(&selection, &admitted);
        if args.get("retain_audio").and_then(Value::as_bool) != Some(true)
            && args.get("input_wav").and_then(Value::as_str) != Some(path)
        {
            let _ = fs::remove_file(path);
        }
        result?
    } else {
        return Err(error(
            "speech_provider_not_implemented",
            &selection.provider,
            json!({"adapter":selection.adapter}),
        ));
    };
    Ok(
        json!({"schema":"narada.speech.capture_transcribe.v1","status":"transcribed","provider":selection.provider,"adapter":selection.adapter,"model":selection.model,"resolved_selection":selection_public(&selection,"transcription"),"selection_source":selection.source,"duration_seconds":duration,"transcript":transcript,"monitor":compact_monitor(&monitor),"privacy":{"remote_audio_egress":if selection.adapter=="openai-transcription"{"admitted"}else{"not_used"},"raw_audio_retained":args.get("retain_audio").and_then(Value::as_bool).unwrap_or(false)}}),
    )
}

