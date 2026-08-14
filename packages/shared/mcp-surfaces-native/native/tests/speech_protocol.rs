use serde_json::{json, Value};
use std::fs;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::thread;
use std::time::{Duration, Instant};

fn rpc(id: u64, method: &str, params: Value) -> Value {
    json!({"jsonrpc":"2.0","id":id,"method":method,"params":params})
}
fn tool(id: u64, name: &str, args: Value) -> Value {
    rpc(id, "tools/call", json!({"name":name,"arguments":args}))
}
fn response(values: &[Value], id: u64) -> &Value {
    values
        .iter()
        .find(|value| value["id"] == id)
        .unwrap_or_else(|| panic!("missing {id}"))
}
fn structured(value: &Value) -> &Value {
    value
        .pointer("/result/structuredContent")
        .unwrap_or_else(|| panic!("missing structured result: {value}"))
}

fn run(
    root: &Path,
    registry: &Path,
    adapter: &Path,
    requests: &[Value],
    extra: &[(&str, &str)],
) -> Vec<Value> {
    let mut command = Command::new(env!("CARGO_BIN_EXE_narada-mcp-surfaces"));
    command
        .args([
            "--surface-id",
            "speech",
            "--site-root",
            &root.to_string_lossy(),
            "--provider-registry-path",
            &registry.to_string_lossy(),
        ])
        .env("NARADA_SPEECH_LISTEN_ADAPTER_PATH", adapter)
        .env("NARADA_SPEECH_ALLOW_REMOTE_AUDIO_EGRESS", "1")
        .env("NARADA_SPEECH_ALLOW_INSECURE_TEST", "1")
        .env("NARADA_SPEECH_DISABLE_PLAYBACK_TEST", "1")
        .env("NARADA_SPEECH_LISTEN_AUDIO_CUES", "0")
        .env("NARADA_AGENT_ID", "speech.test")
        .env(
            "NARADA_SPEECH_AUDIBLE_OUTPUT_LOCK_DIR",
            root.join(".speech-audible.lock"),
        )
        .env("NARADA_SPEECH_AUDIBLE_OUTPUT_LOCK_STALE_MS", "1000")
        .env("OPENAI_API_KEY", "speech-secret-key")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (key, value) in extra {
        command.env(key, value);
    }
    let mut child = command.spawn().expect("spawn speech");
    {
        let input = child.stdin.as_mut().unwrap();
        for request in requests {
            writeln!(input, "{request}").unwrap();
        }
    }
    let output = child.wait_with_output().expect("wait speech");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
            .chars()
            .take(2000)
            .collect::<String>()
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(
        !stdout.contains("speech-secret-key"),
        "credential disclosed"
    );
    stdout
        .lines()
        .map(|line| serde_json::from_str(line).expect("response json"))
        .collect()
}

fn read_request(stream: &mut TcpStream) -> String {
    stream
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    let mut bytes = Vec::new();
    let mut buffer = [0u8; 4096];
    loop {
        let count = stream.read(&mut buffer).unwrap_or(0);
        if count == 0 {
            break;
        }
        bytes.extend_from_slice(&buffer[..count]);
        let Some(end) = bytes
            .windows(4)
            .position(|part| part == b"\r\n\r\n")
            .map(|index| index + 4)
        else {
            continue;
        };
        let headers = String::from_utf8_lossy(&bytes[..end]);
        let length = headers
            .lines()
            .find_map(|line| {
                line.to_ascii_lowercase()
                    .strip_prefix("content-length:")
                    .and_then(|value| value.trim().parse::<usize>().ok())
            })
            .unwrap_or(0);
        if bytes.len() >= end + length {
            break;
        }
    }
    String::from_utf8_lossy(&bytes).to_string()
}

fn mock_provider() -> (String, Arc<AtomicBool>, thread::JoinHandle<Vec<String>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let stop = Arc::new(AtomicBool::new(false));
    let worker_stop = Arc::clone(&stop);
    let handle = thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(20);
        let mut requests = Vec::new();
        while !worker_stop.load(Ordering::SeqCst) && Instant::now() < deadline {
            match listener.accept() {
                Ok((mut stream, _)) => {
                    let request = read_request(&mut stream);
                    let transcription = request.contains("/v1/audio/transcriptions");
                    requests.push(request);
                    let (content_type, body) = if transcription {
                        (
                            "application/json",
                            br#"{"text":"operator response"}"#.to_vec(),
                        )
                    } else {
                        ("audio/wav", b"RIFFfixture-wave".to_vec())
                    };
                    let _=write!(stream,"HTTP/1.1 200 OK\r\ncontent-type: {content_type}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",body.len());
                    let _ = stream.write_all(&body);
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    thread::sleep(Duration::from_millis(5))
                }
                Err(error) => panic!("provider accept: {error}"),
            }
        }
        requests
    });
    (base, stop, handle)
}

fn slow_provider() -> (String, thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let handle = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let _ = read_request(&mut stream);
        thread::sleep(Duration::from_millis(350));
        let body = b"RIFFlate";
        let _ = write!(
            stream,
            "HTTP/1.1 200 OK\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
            body.len()
        );
        let _ = stream.write_all(body);
    });
    (base, handle)
}

fn setup(root: &Path, base: &str) -> (std::path::PathBuf, std::path::PathBuf) {
    fs::create_dir_all(root.join("audio")).unwrap();
    let wav = root.join("audio/input.wav");
    fs::write(&wav, b"RIFFinput-wave").unwrap();
    let adapter = root.join("capture.ps1");
    let escaped = wav.to_string_lossy().replace('\'', "''");
    fs::write(&adapter,format!(r#"param([int]$DurationSeconds,[string]$RecognitionAdapter,[switch]$Calibrate,[switch]$DisableDebugAudioCues,[switch]$RetainAudio,[switch]$DispatchDryRun,[switch]$PassThru,[string]$InputWav,[string]$Device,[switch]$SelfTestSynthetic)
if($PassThru){{@{{schema='narada.voice.local_monitor.v1';status='ok';speech_detected=$true;selected_segment_duration_ms=700;retained_audio_path='{escaped}'}}|ConvertTo-Json -Compress;exit 0}}
Start-Sleep -Seconds 30
"#)).unwrap();
    let registry = root.join("providers.json");
    fs::write(&registry,serde_json::to_vec(&json!({"schema":"narada.provider_registry.v2","version":2,"defaults":{"tts":{"provider":"fixture","model":"tts"},"transcription":{"provider":"fixture","model":"stt"}},"providers":{"fixture":{"id":"fixture","base_url":base,"credential_requirement":{"kind":"api_key_secret","env_names":["OPENAI_API_KEY"]},"models":{"tts":{"id":"tts","status":"active","capabilities":{"tts":{"adapter":"openai-tts","voices":[{"id":"voice-1"}],"default_voice":"voice-1"}}},"stt":{"id":"stt","status":"active","capabilities":{"transcription":{"adapter":"openai-transcription"}}}},"capabilities":{"tts":{"default_model":"tts"},"transcription":{"default_model":"stt"}}},"sapi":{"id":"sapi","credential_requirement":{"kind":"none"},"models":{"default":{"id":"default","status":"active","capabilities":{"tts":{"adapter":"sapi"},"transcription":{"adapter":"sapi"}}}},"capabilities":{"tts":{"default_model":"default"},"transcription":{"default_model":"default"}}}}})).unwrap()).unwrap();
    (registry, adapter)
}

#[test]
fn speech_public_protocol_is_native_complete_bounded_and_cancellable() {
    let root = std::env::temp_dir().join(format!("narada-speech-stdio-{}", uuid::Uuid::new_v4()));
    let (base, stop, server) = mock_provider();
    let (registry, adapter) = setup(&root, &base);
    let catalog = run(
        &root,
        &registry,
        &adapter,
        &[
            rpc(1, "tools/list", json!({})),
            rpc(2, "prompts/list", json!({})),
            rpc(3, "prompts/get", json!({"name":"speech_workflow"})),
            rpc(
                4,
                "completion/complete",
                json!({"argument":{"name":"name","value":"speech_"}}),
            ),
        ],
        &[],
    );
    let tools = response(&catalog, 1)["result"]["tools"].as_array().unwrap();
    assert_eq!(tools.len(), 8);
    assert_eq!(
        response(&catalog, 2)["result"]["prompts"][0]["name"],
        "speech_workflow"
    );
    assert!(response(&catalog, 3)["result"]["messages"].is_array());
    assert_eq!(response(&catalog, 4)["result"]["completion"]["total"], 8);
    for listed in tools {
        let name = listed["name"].as_str().unwrap();
        assert_eq!(listed["inputSchema"]["title"], format!("{name}.input"));
        assert_eq!(listed["inputSchema"]["additionalProperties"], false);
    }
    let invalid = tools
        .iter()
        .enumerate()
        .map(|(index, listed)| {
            tool(
                100 + index as u64,
                listed["name"].as_str().unwrap(),
                json!({"unexpected":true}),
            )
        })
        .collect::<Vec<_>>();
    let invalid_results = run(&root, &registry, &adapter, &invalid, &[]);
    for index in 0..tools.len() {
        assert!(response(&invalid_results, 100 + index as u64)
            .get("error")
            .is_some());
    }
    let calls = vec![
        tool(10, "speech_guidance", json!({})),
        tool(11, "speech_voices", json!({})),
        tool(12, "speech_listen_status", json!({})),
        tool(
            13,
            "speech_speak",
            json!({"text":"hello","retain_audio":true,"output_path":root.join("audio/output.wav")}),
        ),
        tool(
            14,
            "speech_capture_transcribe",
            json!({"duration_seconds":1,"input_wav":root.join("audio/input.wav"),"retain_audio":true}),
        ),
        tool(
            15,
            "speech_prompt_capture_response",
            json!({"text":"respond","duration_seconds":1,"no_response_min_speech_ms":300}),
        ),
        tool(
            16,
            "speech_listen_start",
            json!({"duration_seconds":30,"session_id":"session-1"}),
        ),
        tool(17, "speech_listen_status", json!({})),
        tool(18, "speech_listen_stop", json!({"session_id":"session-1"})),
        tool(
            19,
            "speech_voices",
            json!({"selection":{"provider":"sapi","model":"default"}}),
        ),
        tool(
            20,
            "speech_speak",
            json!({"text":"local fixture","selection":{"provider":"sapi","model":"default"},"retain_audio":true,"output_path":root.join("audio/local.wav")}),
        ),
    ];
    fs::create_dir(root.join(".speech-audible.lock")).unwrap();
    fs::write(root.join(".speech-audible.lock/owner.json"), b"{\"pid\":0}").unwrap();
    thread::sleep(Duration::from_millis(1100));
    let results = run(&root, &registry, &adapter, &calls, &[]);
    for id in 10..=18 {
        assert!(
            response(&results, id).get("error").is_none(),
            "{id}: {}",
            response(&results, id)
        );
    }
    assert_eq!(structured(response(&results, 11))["count"], 1);
    assert_eq!(structured(response(&results, 15))["status"], "responded");
    assert_eq!(
        structured(response(&results, 13))["speaker_announcement"]["prefix_text"],
        "speech.test here:"
    );
    assert_eq!(
        structured(response(&results, 13))["audible_output"]["serialized"],
        true
    );
    assert_eq!(structured(response(&results, 17))["active_count"], 1);
    assert_eq!(
        structured(response(&results, 18))["stopped_session_ids"],
        json!(["session-1"])
    );
    assert!(root.join("audio/output.wav").exists());
    for id in 19..=20 {
        assert!(
            response(&results, id).get("error").is_none(),
            "{id}: {}",
            response(&results, id)
        );
    }
    assert!(root.join("audio/local.wav").exists());
    let blocked = run(
        &root,
        &registry,
        &adapter,
        &[tool(
            30,
            "speech_capture_transcribe",
            json!({"duration_seconds":1}),
        )],
        &[("NARADA_SPEECH_ALLOW_REMOTE_AUDIO_EGRESS", "0")],
    );
    assert_eq!(
        response(&blocked, 30)["error"]["data"]["code"],
        "speech_remote_audio_egress_not_admitted"
    );
    let escaped_input = std::path::PathBuf::from(std::env::var("WINDIR").unwrap()).join("win.ini");
    let confined = run(
        &root,
        &registry,
        &adapter,
        &[tool(
            34,
            "speech_capture_transcribe",
            json!({"duration_seconds":1,"input_wav":escaped_input}),
        )],
        &[],
    );
    assert_eq!(
        response(&confined, 34)["error"]["data"]["code"],
        "speech_input_path_not_admitted"
    );
    let missing = root.join("missing.ps1");
    let unavailable = run(
        &root,
        &registry,
        &missing,
        &[
            tool(31, "speech_listen_status", json!({})),
            tool(32, "speech_listen_start", json!({"duration_seconds":1})),
        ],
        &[],
    );
    assert_eq!(structured(response(&unavailable, 31))["status"], "blocked");
    assert_eq!(
        response(&unavailable, 32)["error"]["data"]["code"],
        "speech_listen_adapter_missing"
    );
    stop.store(true, Ordering::SeqCst);
    let requests = server.join().unwrap();
    assert_eq!(requests.len(), 4);
    let wire = requests.join("\n");
    assert!(wire.contains("Authorization: Bearer speech-secret-key"));
    let timeout_root = root.join("timeout");
    let (slow, slow_server) = slow_provider();
    let (slow_registry, slow_adapter) = setup(&timeout_root, &slow);
    let started = Instant::now();
    let timed = run(
        &timeout_root,
        &slow_registry,
        &slow_adapter,
        &[tool(33, "speech_speak", json!({"text":"timeout"}))],
        &[("NARADA_SPEECH_PROVIDER_TIMEOUT_MS", "100")],
    );
    assert!(started.elapsed() < Duration::from_secs(2));
    assert_eq!(
        response(&timed, 33)["error"]["data"]["code"],
        "speech_openai_request_failed"
    );
    slow_server.join().unwrap();
    fs::remove_dir_all(root).unwrap();
}
