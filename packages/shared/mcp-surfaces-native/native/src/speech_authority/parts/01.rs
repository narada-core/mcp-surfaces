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

