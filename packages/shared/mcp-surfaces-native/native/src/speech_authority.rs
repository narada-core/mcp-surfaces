use serde_json::{json, Map, Value};
use std::collections::HashMap;
use std::env;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::{Mutex, OnceLock};
use std::thread;
use std::time::{Duration, Instant};
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;
use uuid::Uuid;

const MAX_REGISTRY_BYTES: u64 = 2 * 1024 * 1024;
const MAX_TEXT_CHARS: usize = 1_000;
const MAX_LISTEN_SECONDS: u64 = 300;
const MAX_AUDIO_BYTES: usize = 25 * 1024 * 1024;
const MAX_PROCESS_OUTPUT_BYTES: u64 = 256 * 1024;
const MAX_PROVIDER_RESPONSE_BYTES: u64 = 2 * 1024 * 1024;

struct Selection {
    provider: String,
    model: String,
    adapter: String,
    voice: Option<String>,
    voices: Vec<String>,
    base_url: String,
    credential_env_names: Vec<String>,
    source: &'static str,
}

struct ListenSession {
    child: Child,
    provider: String,
    duration_seconds: u64,
    started_at: String,
    deadline: Instant,
}

struct AudibleLock {
    path: PathBuf,
}
impl AudibleLock {
    fn acquire() -> Result<Self, Value> {
        let path = env::var("NARADA_SPEECH_AUDIBLE_OUTPUT_LOCK_DIR")
            .ok()
            .filter(|value| !value.trim().is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(|| env::temp_dir().join("speech-mcp-audible-output.lock"));
        let stale_ms = env::var("NARADA_SPEECH_AUDIBLE_OUTPUT_LOCK_STALE_MS")
            .ok()
            .and_then(|value| value.parse::<u64>().ok())
            .unwrap_or(120_000)
            .clamp(1_000, 86_400_000);
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            match fs::create_dir(&path) {
                Ok(()) => {
                    let owner = path.join("owner.json");
                    if let Err(cause) = fs::write(
                        &owner,
                        serde_json::to_vec(&json!({"pid":std::process::id(),"acquired_at":now()}))
                            .unwrap_or_default(),
                    ) {
                        let _ = fs::remove_dir(&path);
                        return Err(error(
                            "speech_audible_lock_write_failed",
                            &cause.to_string(),
                            json!({"path":owner}),
                        ));
                    }
                    return Ok(Self { path });
                }
                Err(cause) if cause.kind() == std::io::ErrorKind::AlreadyExists => {
                    let stale = fs::metadata(path.join("owner.json"))
                        .ok()
                        .and_then(|metadata| metadata.modified().ok())
                        .and_then(|modified| modified.elapsed().ok())
                        .is_some_and(|age| age >= Duration::from_millis(stale_ms));
                    if stale {
                        let _ = fs::remove_file(path.join("owner.json"));
                        let _ = fs::remove_dir(&path);
                        continue;
                    }
                    if Instant::now() >= deadline {
                        return Err(error(
                            "speech_audible_output_lock_timeout",
                            "timed out waiting for the host audible-output lock",
                            json!({"path":path,"timeout_ms":30000}),
                        ));
                    }
                    thread::sleep(Duration::from_millis(25));
                }
                Err(cause) => {
                    return Err(error(
                        "speech_audible_output_lock_failed",
                        &cause.to_string(),
                        json!({"path":path}),
                    ))
                }
            }
        }
    }
}
impl Drop for AudibleLock {
    fn drop(&mut self) {
        let _ = fs::remove_file(self.path.join("owner.json"));
        let _ = fs::remove_dir(&self.path);
    }
}

pub struct ShutdownGuard;
impl Drop for ShutdownGuard {
    fn drop(&mut self) {
        shutdown();
    }
}
pub fn shutdown_guard() -> ShutdownGuard {
    ShutdownGuard
}

static SESSIONS: OnceLock<Mutex<HashMap<String, ListenSession>>> = OnceLock::new();

fn sessions() -> &'static Mutex<HashMap<String, ListenSession>> {
    SESSIONS.get_or_init(|| Mutex::new(HashMap::new()))
}

pub fn list_tools() -> Vec<Value> {
    let selection = |voice: bool| {
        let mut properties = Map::new();
        properties.insert("provider".to_string(), bounded_string(256));
        properties.insert("model".to_string(), bounded_string(256));
        if voice {
            properties.insert("voice".to_string(), bounded_string(256));
        }
        json!({"type":"object","properties":properties,"additionalProperties":false})
    };
    vec![
        tool("speech_guidance", "Show the native speech workflow, policy boundaries, and recovery guidance.", json!({"type":"object","properties":{"workflow":bounded_string(256),"tool":bounded_string(256)},"additionalProperties":false}), true, true),
        tool("speech_speak", "Speak bounded text through a registry-resolved native SAPI or OpenAI TTS provider.", json!({"type":"object","properties":{"text":{"type":"string","minLength":1,"maxLength":1000},"selection":selection(true),"rate":{"type":"integer","minimum":-10,"maximum":10,"default":0},"speed":{"type":"number","minimum":0.25,"maximum":4.0,"default":1.0},"speaker_agent_id":bounded_string(256),"announce_speaker":{"type":"boolean"},"output_path":{"type":"string","minLength":1,"maxLength":4096},"retain_audio":{"type":"boolean","default":false}},"required":["text"],"additionalProperties":false}), false, false),
        tool("speech_voices", "List installed or registry-declared voices for the resolved TTS provider.", json!({"type":"object","properties":{"selection":selection(false)},"additionalProperties":false}), true, true),
        tool("speech_listen_status", "Inspect the native capture adapter and active bounded listen sessions.", json!({"type":"object","properties":{},"additionalProperties":false}), true, true),
        tool("speech_capture_transcribe", "Capture bounded microphone audio and transcribe it with the resolved local or admitted remote provider.", json!({"type":"object","properties":{"duration_seconds":{"type":"integer","minimum":1,"maximum":300,"default":30},"selection":selection(false),"device":bounded_string(1024),"input_wav":{"type":"string","minLength":1,"maxLength":4096},"self_test_synthetic":{"type":"boolean"},"calibrate":{"type":"boolean"},"retain_audio":{"type":"boolean","default":false}},"additionalProperties":false}), false, false),
        tool("speech_prompt_capture_response", "Speak a prompt, capture one bounded response, and return responded or no_response.", json!({"type":"object","properties":{"text":{"type":"string","minLength":1,"maxLength":1000},"tts_selection":selection(true),"transcription_selection":selection(false),"rate":{"type":"integer","minimum":-10,"maximum":10,"default":0},"speed":{"type":"number","minimum":0.25,"maximum":4.0,"default":1.0},"duration_seconds":{"type":"integer","minimum":1,"maximum":300,"default":30},"device":bounded_string(1024),"retain_audio":{"type":"boolean","default":false},"no_response_min_speech_ms":{"type":"integer","minimum":0,"maximum":300000,"default":300},"speaker_agent_id":bounded_string(256),"announce_speaker":{"type":"boolean"}},"required":["text"],"additionalProperties":false}), false, false),
        tool("speech_listen_start", "Start one bounded native capture-adapter session.", json!({"type":"object","properties":{"duration_seconds":{"type":"integer","minimum":1,"maximum":300,"default":30},"selection":selection(false),"calibrate":{"type":"boolean"},"session_id":{"type":"string","minLength":1,"maxLength":256,"pattern":"^[A-Za-z0-9._:-]+$"}},"additionalProperties":false}), false, false),
        tool("speech_listen_stop", "Stop one active listen session, or all sessions when session_id is omitted.", json!({"type":"object","properties":{"session_id":{"type":"string","minLength":1,"maxLength":256,"pattern":"^[A-Za-z0-9._:-]+$"}},"additionalProperties":false}), false, true),
    ]
}

pub fn call_tool(name: &str, args: &Map<String, Value>, root: &Path) -> Result<Value, Value> {
    match name {
        "speech_guidance" => Ok(guidance(args)),
        "speech_speak" => speak(args, root),
        "speech_voices" => voices(args, root),
        "speech_listen_status" => listen_status(args, root),
        "speech_capture_transcribe" => capture_transcribe(args, root),
        "speech_prompt_capture_response" => prompt_capture(args, root),
        "speech_listen_start" => listen_start(args, root),
        "speech_listen_stop" => listen_stop(args),
        _ => Err(error(
            "unknown_tool",
            &format!("unknown_tool:{name}"),
            json!({"tool_name":name}),
        )),
    }
}

pub fn auxiliary(method: &str, params: &Map<String, Value>) -> Result<Value, Value> {
    match method {
        "prompts/list" => Ok(
            json!({"prompts":[{"name":"speech_workflow","title":"Native Speech Workflow","description":"Orient to native speech provider, capture, privacy, and cancellation boundaries.","arguments":[]}]}),
        ),
        "prompts/get" if params.get("name").and_then(Value::as_str) == Some("speech_workflow") => {
            Ok(
                json!({"description":"Native speech workflow","messages":[{"role":"user","content":{"type":"text","text":"Call speech_listen_status before capture. Resolve voices before choosing one. Keep remote microphone egress disabled unless explicitly admitted, and stop bounded listen sessions when finished."}}]}),
            )
        }
        "prompts/get" => Err(error(
            "unknown_prompt",
            "unknown speech prompt",
            json!({"name":params.get("name").cloned().unwrap_or(Value::Null)}),
        )),
        "completion/complete" => {
            let values = if params
                .get("argument")
                .and_then(Value::as_object)
                .and_then(|value| value.get("name"))
                .and_then(Value::as_str)
                == Some("name")
            {
                list_tools()
                    .into_iter()
                    .filter_map(|tool| tool.get("name").cloned())
                    .take(100)
                    .collect::<Vec<_>>()
            } else {
                Vec::new()
            };
            Ok(json!({"completion":{"total":values.len(),"values":values,"hasMore":false}}))
        }
        "logging/setLevel" => Ok(json!({})),
        _ => Err(error(
            "unsupported_mcp_method",
            &format!("unsupported_mcp_method:{method}"),
            json!({}),
        )),
    }
}

fn tool(
    name: &str,
    description: &str,
    input_schema: Value,
    read_only: bool,
    idempotent: bool,
) -> Value {
    json!({"name":name,"description":description,"inputSchema":input_schema,"annotations":{"title":name,"readOnlyHint":read_only,"destructiveHint":false,"idempotentHint":idempotent,"openWorldHint":!read_only},"outputSchema":{"type":"object","additionalProperties":true}})
}

fn bounded_string(maximum: u64) -> Value {
    json!({"type":"string","maxLength":maximum})
}

fn guidance(args: &Map<String, Value>) -> Value {
    json!({
        "schema":"narada.speech.guidance.v1","status":"ok","surface_id":"speech",
        "requested":{"workflow":args.get("workflow").cloned().unwrap_or(Value::Null),"tool":args.get("tool").cloned().unwrap_or(Value::Null)},
        "first_use":["Call speech_listen_status before microphone workflows.","Use speech_voices before selecting a non-default voice.","Remote microphone audio is refused unless explicitly admitted by policy."],
        "workflows":{"speak":["speech_voices","speech_speak"],"capture":["speech_listen_status","speech_capture_transcribe"],"prompt_response":["speech_listen_status","speech_prompt_capture_response"],"continuous_listen":["speech_listen_start","speech_listen_status","speech_listen_stop"]},
        "boundaries":["Provider credentials remain server-bound and are never returned.","Output files must remain under the Site, temporary directory, or configured speech output root.","Listen sessions are process-local and bounded; after a process restart status truthfully reports none active."],
        "recovery":["A missing provider registry requires --provider-registry-path or NARADA_SPEECH_PROVIDER_REGISTRY_PATH.","A missing capture adapter requires NARADA_SPEECH_LISTEN_ADAPTER_PATH.","Provider timeout and process timeout diagnostics are safe to retry after correcting authority state."]
    })
}

fn registry(root: &Path) -> Result<Value, Value> {
    let path = env::var("NARADA_SPEECH_PROVIDER_REGISTRY_PATH")
        .ok()
        .or_else(|| env::var("NARADA_PROVIDER_REGISTRY_PATH").ok())
        .filter(|value| !value.trim().is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            let candidate = root.join(".narada/provider-registry.json");
            candidate.exists().then_some(candidate)
        })
        .ok_or_else(|| error("speech_provider_registry_path_required", "speech provider registry path is required", json!({"remediation":"Pass --provider-registry-path or set NARADA_SPEECH_PROVIDER_REGISTRY_PATH."})))?;
    let metadata = fs::metadata(&path).map_err(|cause| {
        error(
            "speech_provider_registry_read_failed",
            &cause.to_string(),
            json!({"path":path}),
        )
    })?;
    if metadata.len() > MAX_REGISTRY_BYTES {
        return Err(error(
            "speech_provider_registry_too_large",
            "provider registry exceeds 2 MiB",
            json!({"size":metadata.len()}),
        ));
    }
    let value: Value = serde_json::from_slice(&fs::read(&path).map_err(|cause| {
        error(
            "speech_provider_registry_read_failed",
            &cause.to_string(),
            json!({"path":path}),
        )
    })?)
    .map_err(|cause| {
        error(
            "speech_provider_registry_invalid",
            &cause.to_string(),
            json!({"path":path}),
        )
    })?;
    if value.get("providers").and_then(Value::as_object).is_none() {
        return Err(error(
            "speech_provider_registry_invalid",
            "providers object is required",
            json!({"path":path}),
        ));
    }
    Ok(value)
}

fn resolve_selection(
    args: &Map<String, Value>,
    key: &str,
    capability: &str,
    root: &Path,
) -> Result<Selection, Value> {
    let registry = registry(root)?;
    let explicit = args.get(key).and_then(Value::as_object);
    let defaults = registry
        .get("defaults")
        .and_then(Value::as_object)
        .and_then(|defaults| defaults.get(capability))
        .and_then(Value::as_object);
    let provider = explicit
        .and_then(|value| value.get("provider"))
        .and_then(Value::as_str)
        .or_else(|| {
            defaults
                .and_then(|value| value.get("provider"))
                .and_then(Value::as_str)
        })
        .ok_or_else(|| {
            error(
                "speech_provider_default_missing",
                "provider selection is required",
                json!({"capability":capability}),
            )
        })?;
    let provider_record = registry
        .pointer(&format!("/providers/{}", pointer_component(provider)))
        .and_then(Value::as_object)
        .ok_or_else(|| {
            error(
                "speech_provider_unknown",
                provider,
                json!({"capability":capability}),
            )
        })?;
    let model = explicit
        .and_then(|value| value.get("model"))
        .and_then(Value::as_str)
        .or_else(|| {
            defaults
                .and_then(|value| value.get("model"))
                .and_then(Value::as_str)
        })
        .or_else(|| {
            provider_record
                .get("capabilities")
                .and_then(Value::as_object)
                .and_then(|value| value.get(capability))
                .and_then(Value::as_object)
                .and_then(|value| value.get("default_model"))
                .and_then(Value::as_str)
        })
        .ok_or_else(|| {
            error(
                "speech_model_default_missing",
                provider,
                json!({"capability":capability}),
            )
        })?;
    let model_record = provider_record
        .get("models")
        .and_then(Value::as_object)
        .and_then(|models| models.get(model))
        .and_then(Value::as_object)
        .ok_or_else(|| {
            error(
                "speech_model_unknown",
                model,
                json!({"provider":provider,"capability":capability}),
            )
        })?;
    if model_record
        .get("status")
        .and_then(Value::as_str)
        .is_some_and(|status| status != "active")
    {
        return Err(error(
            "speech_model_inactive",
            model,
            json!({"provider":provider,"capability":capability}),
        ));
    }
    let capability_record = model_record
        .get("capabilities")
        .and_then(Value::as_object)
        .and_then(|value| value.get(capability))
        .and_then(Value::as_object)
        .ok_or_else(|| {
            error(
                "speech_capability_not_supported",
                capability,
                json!({"provider":provider,"model":model}),
            )
        })?;
    let adapter = capability_record
        .get("adapter")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            error(
                "speech_adapter_missing",
                capability,
                json!({"provider":provider,"model":model}),
            )
        })?;
    let voices: Vec<String> = capability_record
        .get("voices")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| {
                    item.as_str()
                        .or_else(|| item.get("id").and_then(Value::as_str))
                })
                .take(100)
                .map(ToOwned::to_owned)
                .collect()
        })
        .unwrap_or_default();
    let voice = explicit
        .and_then(|value| value.get("voice"))
        .and_then(Value::as_str)
        .or_else(|| {
            capability_record
                .get("default_voice")
                .and_then(Value::as_str)
        })
        .map(ToOwned::to_owned);
    if let Some(voice) = voice.as_deref() {
        if !voices.is_empty() && !voices.iter().any(|candidate| candidate == voice) {
            return Err(error(
                "speech_voice_unknown",
                voice,
                json!({"provider":provider,"model":model}),
            ));
        }
    }
    let base_url = provider_record
        .get("base_url")
        .and_then(Value::as_str)
        .unwrap_or("https://api.openai.com")
        .trim_end_matches('/')
        .to_string();
    let credential_env_names = provider_record
        .get("credential_requirement")
        .and_then(Value::as_object)
        .and_then(|value| value.get("env_names"))
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .take(16)
                .map(ToOwned::to_owned)
                .collect()
        })
        .unwrap_or_default();
    Ok(Selection {
        provider: provider.to_string(),
        model: model.to_string(),
        adapter: adapter.to_string(),
        voice,
        voices,
        base_url,
        credential_env_names,
        source: if explicit.is_some() {
            "explicit"
        } else {
            "registry_default"
        },
    })
}

fn selection_public(selection: &Selection, capability: &str) -> Value {
    json!({"provider":selection.provider,"model":selection.model,"capability":capability,"adapter":selection.adapter,"voice":selection.voice,"status":"active"})
}

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
