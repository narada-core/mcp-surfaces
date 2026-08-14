use serde_json::{json, Value};
use std::fs;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex,
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
        .find(|v| v["id"] == id)
        .unwrap_or_else(|| panic!("missing {id}"))
}
fn structured(value: &Value) -> &Value {
    value
        .pointer("/result/structuredContent")
        .unwrap_or_else(|| panic!("missing structured: {value}"))
}
fn run(root: &Path, base: &str, requests: &[Value], extra: &[(&str, &str)]) -> Vec<Value> {
    let mut command = Command::new(env!("CARGO_BIN_EXE_narada-mcp-surfaces"));
    command
        .args([
            "--surface-id",
            "cloudflare-carrier",
            "--site-root",
            &root.to_string_lossy(),
            "--native-authority",
        ])
        .env("NARADA_ROOT", root)
        .env("CLOUDFLARE_CARRIER_URL", base)
        .env("NARADA_CLOUDFLARE_ALLOW_INSECURE_TEST", "1")
        .env("NARADA_CLOUDFLARE_REQUEST_TIMEOUT_MS", "1000")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (k, v) in extra {
        command.env(k, v);
    }
    let mut child = command.spawn().unwrap();
    {
        let input = child.stdin.as_mut().unwrap();
        for request in requests {
            writeln!(input, "{request}").unwrap();
        }
    }
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
            .chars()
            .take(2000)
            .collect::<String>()
    );
    let text = String::from_utf8(output.stdout).unwrap();
    assert!(!text.contains("operator-cookie-secret"));
    text.lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}
fn read_request(stream: &mut TcpStream) -> String {
    stream.set_read_timeout(Some(Duration::from_secs(2))).ok();
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
            .position(|v| v == b"\r\n\r\n")
            .map(|v| v + 4)
        else {
            continue;
        };
        let headers = String::from_utf8_lossy(&bytes[..end]);
        let length = headers
            .lines()
            .find_map(|line| {
                line.to_ascii_lowercase()
                    .strip_prefix("content-length:")
                    .and_then(|v| v.trim().parse::<usize>().ok())
            })
            .unwrap_or(0);
        if bytes.len() >= end + length {
            break;
        }
    }
    String::from_utf8_lossy(&bytes).to_string()
}
fn mock() -> (
    String,
    Arc<AtomicBool>,
    Arc<Mutex<Vec<String>>>,
    thread::JoinHandle<()>,
) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let stop = Arc::new(AtomicBool::new(false));
    let requests = Arc::new(Mutex::new(Vec::new()));
    let worker_stop = Arc::clone(&stop);
    let worker_requests = Arc::clone(&requests);
    let handle = thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(30);
        while !worker_stop.load(Ordering::SeqCst) && Instant::now() < deadline {
            match listener.accept() {
                Ok((mut stream, _)) => {
                    stream.set_nonblocking(false).unwrap();
                    let request = read_request(&mut stream);
                    worker_requests.lock().unwrap().push(request.clone());
                    let first = request.lines().next().unwrap_or("");
                    let body_text = request.split("\r\n\r\n").nth(1).unwrap_or("");
                    let input: Value =
                        serde_json::from_str(body_text).unwrap_or_else(|_| json!({}));
                    let site = input.pointer("/params/site_id").and_then(Value::as_str);
                    if site == Some("slow") {
                        thread::sleep(Duration::from_millis(300));
                    }
                    let (status, body) = if first.contains("/health") {
                        (200, json!({"status":"healthy"}))
                    } else if first.contains("/events?") {
                        (
                            200,
                            json!({"cursor":{"last_sequence":9},"events":[{"projected_at":"2026-08-14T00:00:00Z"}]}),
                        )
                    } else if site == Some("unauthorized") {
                        (401, json!({"code":"unauthorized","token":"must-redact"}))
                    } else if site == Some("oversize") {
                        (200, json!({"payload":"x".repeat(2 * 1024 * 1024 + 1)}))
                    } else {
                        match input.get("operation").and_then(Value::as_str) {
                            Some("site.list") => (
                                200,
                                json!({"site_product_overview":{"site_count":2,"next_action":"observe","health_counts":{"healthy":2}}}),
                            ),
                            Some("site.read") => (
                                200,
                                json!({"site":{"site_id":site},"site_product_status":{"health":"healthy","next_action":"none","continuity_state":"active"}}),
                            ),
                            Some("operation.list") => (
                                200,
                                json!({"operations":[{"operation_id":"op-1","status":"needs_continuation"},{"operation_id":"op-2","status":"complete"}]}),
                            ),
                            Some("operation.read") => (
                                200,
                                json!({"operation":{"operation_id":"op-1","status":"complete"},"operation_lifecycle_status":{"phase":"done","health":"healthy","next_action":"none"}}),
                            ),
                            _ => (404, json!({"code":"not_found"})),
                        }
                    };
                    let encoded = body.to_string();
                    let reason = if status == 200 {
                        "OK"
                    } else if status == 401 {
                        "Unauthorized"
                    } else {
                        "Not Found"
                    };
                    let _=write!(stream,"HTTP/1.1 {status} {reason}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{encoded}",encoded.len());
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    thread::sleep(Duration::from_millis(5))
                }
                Err(error) => panic!("accept: {error}"),
            }
        }
    });
    (base, stop, requests, handle)
}
fn setup(root: &Path, base: &str) {
    fs::create_dir_all(root.join(".narada/auth")).unwrap();
    fs::write(root.join(".narada/auth/cloudflare-operator-session.json"),r#"{"cookie":"narada_operator_session=operator-cookie-secret","captured_at":"2026-08-14T00:00:00Z","principal":"operator"}"#).unwrap();
    fs::create_dir_all(root.join(".narada/site-continuity/health")).unwrap();
    fs::write(root.join(".narada/site-continuity/health/cloudflare-continuity-health-last.json"),r#"{"status":"healthy","generated_at":"2026-08-14T00:00:00Z","continuity_health":{"local_sync_status":"healthy","local_sync_artifact_count":2,"local_inbound_status":"healthy","local_inbound_artifact_count":1,"reconciliation_execution_status":"complete"},"scheduler_task_readback":{"scheduled_task_state":"Ready"},"cloudflare_product_posture":{"state":"ready","site_product_overview":{"site_count":2}},"cloudflare_product_binding_alignment":{"state":"aligned","local_site_count":2}}"#).unwrap();
    let p = root.join(".narada/crew/nars-projections/projection-1");
    fs::create_dir_all(&p).unwrap();
    fs::write(p.join("intent.json"),serde_json::to_vec(&json!({"site_id":"site-1","source_ref":{"kind":"cloudflare_carrier","operation_id":"op-1"},"projection_api_base_url":base})).unwrap()).unwrap();
    fs::write(p.join("remote-access.json"),serde_json::to_vec(&json!({"site_id":"site-1","browser_access_tokens":[{"kind":"browser","status":"active","token_fingerprint":"fingerprint-1"}],"lifecycle_state":"active"})).unwrap()).unwrap();
}

#[test]
fn cloudflare_public_protocol_is_native_complete_bounded_and_read_only() {
    let root =
        std::env::temp_dir().join(format!("narada-cloudflare-stdio-{}", uuid::Uuid::new_v4()));
    let (base, stop, requests, server) = mock();
    setup(&root, &base);
    let session_before =
        fs::read(root.join(".narada/auth/cloudflare-operator-session.json")).unwrap();
    let health_before = fs::read(
        root.join(".narada/site-continuity/health/cloudflare-continuity-health-last.json"),
    )
    .unwrap();
    let catalog = run(
        &root,
        &base,
        &[
            rpc(1, "tools/list", json!({})),
            rpc(2, "prompts/list", json!({})),
            rpc(
                3,
                "prompts/get",
                json!({"name":"cloudflare_carrier_workflow"}),
            ),
            rpc(
                4,
                "completion/complete",
                json!({"argument":{"name":"name","value":"cloudflare_"}}),
            ),
        ],
        &[],
    );
    let tools = response(&catalog, 1)["result"]["tools"].as_array().unwrap();
    assert_eq!(tools.len(), 6);
    assert_eq!(response(&catalog, 4)["result"]["completion"]["total"], 6);
    for listed in tools {
        let name = listed["name"].as_str().unwrap();
        assert_eq!(listed["inputSchema"]["title"], format!("{name}.input"));
        assert_eq!(listed["inputSchema"]["additionalProperties"], false);
        assert!(listed["inputSchema"]["properties"]
            .get("session_file")
            .is_none());
    }
    let invalid = tools
        .iter()
        .enumerate()
        .map(|(i, v)| {
            tool(
                100 + i as u64,
                v["name"].as_str().unwrap(),
                json!({"unexpected":true}),
            )
        })
        .collect::<Vec<_>>();
    let invalid_results = run(&root, &base, &invalid, &[]);
    for i in 0..6 {
        assert!(response(&invalid_results, 100 + i).get("error").is_some());
    }
    let calls = run(
        &root,
        &base,
        &[
            tool(10, "cloudflare_carrier_guidance", json!({})),
            tool(11, "cloudflare_session_status", json!({})),
            tool(12, "cloudflare_health", json!({})),
            tool(13, "cloudflare_doctor", json!({})),
            tool(14, "cloudflare_product_read", json!({})),
            tool(
                15,
                "cloudflare_product_read",
                json!({"operation":"site.read","site_id":"site-1","format":"summary"}),
            ),
            tool(
                16,
                "cloudflare_product_read",
                json!({"operation":"operation.list","site_id":"site-1","limit":7,"continuation":true,"format":"summary"}),
            ),
            tool(
                17,
                "cloudflare_product_read",
                json!({"operation":"operation.read","site_id":"site-1","operation_id":"op-1"}),
            ),
            tool(
                18,
                "cloudflare_carrier_health",
                json!({"projection_id":"projection-1"}),
            ),
            tool(
                19,
                "cloudflare_product_read",
                json!({"operation":"site.read"}),
            ),
            tool(
                20,
                "cloudflare_product_read",
                json!({"operation":"operation.read","site_id":"site-1"}),
            ),
            tool(
                21,
                "cloudflare_product_read",
                json!({"operation":"site.read","site_id":"unauthorized"}),
            ),
            tool(
                22,
                "cloudflare_carrier_health",
                json!({"projection_id":"missing"}),
            ),
            tool(
                23,
                "cloudflare_carrier_health",
                json!({"projection_id":"../escape"}),
            ),
            tool(24, "cloudflare_product_read", json!({})),
            tool(
                25,
                "cloudflare_product_read",
                json!({"operation":"site.read","site_id":"oversize"}),
            ),
            tool(26, "cloudflare_product_read", json!({"format":"text"})),
        ],
        &[],
    );
    assert_eq!(structured(response(&calls, 11))["has_cookie"], true);
    assert_eq!(
        structured(response(&calls, 12)).pointer("/local/sync_status"),
        Some(&json!("healthy"))
    );
    assert_eq!(structured(response(&calls, 13))["native_authority"], true);
    assert_eq!(
        structured(response(&calls, 14)).pointer("/response/site_product_overview/site_count"),
        Some(&json!(2))
    );
    assert_eq!(
        structured(response(&calls, 15)).pointer("/summary/health"),
        Some(&json!("healthy"))
    );
    assert_eq!(
        structured(response(&calls, 16)).pointer("/summary/needs_continuation_count"),
        Some(&json!(1))
    );
    assert_eq!(
        structured(response(&calls, 17)).pointer("/response/operation/status"),
        Some(&json!("complete"))
    );
    assert_eq!(structured(response(&calls, 18))["status"], "healthy");
    assert_eq!(
        structured(response(&calls, 18)).pointer("/projection/last_event_sequence"),
        Some(&json!(9))
    );
    assert_eq!(structured(response(&calls, 22))["status"], "missing");
    for (id, code) in [
        (19, "site_id_required"),
        (20, "operation_id_required"),
        (21, "cloudflare_product_read_failed"),
        (23, "input_schema_validation_failed"),
        (25, "cloudflare_response_too_large"),
    ] {
        let actual = response(&calls, id)
            .pointer("/error/data/code")
            .and_then(Value::as_str)
            .unwrap_or("");
        assert_eq!(actual, code, "{}", response(&calls, id));
    }
    assert_eq!(
        structured(response(&calls, 14)),
        structured(response(&calls, 24))
    );
    assert_eq!(
        structured(response(&calls, 26)).pointer("/response/site_product_overview/site_count"),
        Some(&json!(2))
    );
    let timed = run(
        &root,
        &base,
        &[tool(
            40,
            "cloudflare_product_read",
            json!({"operation":"site.read","site_id":"slow"}),
        )],
        &[("NARADA_CLOUDFLARE_REQUEST_TIMEOUT_MS", "100")],
    );
    assert_eq!(
        response(&timed, 40).pointer("/error/data/code"),
        Some(&json!("cloudflare_transport_failed"))
    );
    let observed = requests.lock().unwrap().join("\n");
    assert!(observed.contains("cookie: narada_operator_session=operator-cookie-secret"));
    assert!(observed.contains("x-narada-browser-token-fingerprint: fingerprint-1"));
    assert!(observed.contains("\"limit\":7"));
    assert_eq!(
        fs::read(root.join(".narada/auth/cloudflare-operator-session.json")).unwrap(),
        session_before
    );
    assert_eq!(
        fs::read(
            root.join(".narada/site-continuity/health/cloudflare-continuity-health-last.json")
        )
        .unwrap(),
        health_before
    );
    let missing = std::env::temp_dir().join(format!(
        "narada-cloudflare-missing-{}",
        uuid::Uuid::new_v4()
    ));
    fs::create_dir_all(&missing).unwrap();
    let empty = run(
        &missing,
        &base,
        &[
            tool(30, "cloudflare_session_status", json!({})),
            tool(31, "cloudflare_health", json!({})),
            tool(32, "cloudflare_doctor", json!({})),
        ],
        &[],
    );
    assert_eq!(structured(response(&empty, 30))["status"], "missing");
    assert_eq!(structured(response(&empty, 31))["status"], "missing");
    assert_eq!(
        structured(response(&empty, 32))["projection_registry_status"],
        "missing"
    );
    stop.store(true, Ordering::SeqCst);
    server.join().unwrap();
    let _ = fs::remove_dir_all(root);
    let _ = fs::remove_dir_all(missing);
}
