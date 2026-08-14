use serde_json::{json, Value};
use std::fs;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::Path;
use std::process::{Command, Stdio};
use std::thread;
use std::time::Duration;

fn rpc(id: u64, method: &str, params: Value) -> Value {
    json!({"jsonrpc":"2.0","id":id,"method":method,"params":params})
}
fn tool(id: u64, name: &str, args: Value) -> Value {
    rpc(id, "tools/call", json!({"name":name,"arguments":args}))
}
fn run(root: &Path, endpoint: Option<&str>, requests: &[Value]) -> Vec<Value> {
    let mut command = Command::new(env!("CARGO_BIN_EXE_narada-mcp-surfaces"));
    command
        .args([
            "--surface-id",
            "artifacts",
            "--site-root",
            &root.to_string_lossy(),
        ])
        .env("NARADA_SESSION_ID", "session-1");
    if let Some(endpoint) = endpoint {
        command.env("NARADA_NARS_BASE_URL", endpoint);
    } else {
        command
            .env_remove("NARADA_NARS_BASE_URL")
            .env_remove("NARADA_AGENT_RUNTIME_SERVER_URL")
            .env_remove("NARADA_RUNTIME_SERVER_URL");
    }
    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn");
    {
        let input = child.stdin.as_mut().expect("stdin");
        for request in requests {
            writeln!(input, "{request}").expect("write")
        }
    }
    let output = child.wait_with_output().expect("wait");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
            .chars()
            .take(2000)
            .collect::<String>()
    );
    String::from_utf8(output.stdout)
        .expect("utf8")
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| serde_json::from_str(line).expect("response"))
        .collect()
}
fn response(values: &[Value], id: u64) -> &Value {
    values.iter().find(|value| value["id"] == id).expect("id")
}
fn structured(value: &Value) -> &Value {
    value
        .pointer("/result/structuredContent")
        .expect("structured")
}
fn assert_bounded(schema: &Value, path: &str) {
    if schema.get("type").and_then(Value::as_str) == Some("string") && schema.get("enum").is_none()
    {
        assert!(schema.get("maxLength").is_some(), "unbounded string {path}")
    }
    if schema.get("type").and_then(Value::as_str) == Some("array") {
        assert!(schema.get("maxItems").is_some(), "unbounded array {path}")
    }
    if let Some(properties) = schema.get("properties").and_then(Value::as_object) {
        for (name, child) in properties {
            assert_bounded(child, &format!("{path}/{name}"))
        }
    }
}
fn read_request(stream: &mut TcpStream) -> String {
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let mut bytes = Vec::new();
    let mut buffer = [0_u8; 2048];
    let mut expected = None;
    loop {
        let count = stream.read(&mut buffer).expect("read");
        if count == 0 {
            break;
        }
        bytes.extend_from_slice(&buffer[..count]);
        if expected.is_none() {
            if let Some(end) = bytes.windows(4).position(|part| part == b"\r\n\r\n") {
                let headers = String::from_utf8_lossy(&bytes[..end]);
                let length = headers
                    .lines()
                    .find_map(|line| {
                        line.to_ascii_lowercase()
                            .strip_prefix("content-length:")
                            .map(str::trim)
                            .and_then(|value| value.parse::<usize>().ok())
                    })
                    .unwrap_or(0);
                expected = Some(end + 4 + length)
            }
        }
        if expected.is_some_and(|length| bytes.len() >= length) {
            break;
        }
    }
    String::from_utf8(bytes).expect("utf8")
}
fn mock_nars() -> (String, thread::JoinHandle<Vec<String>>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let url = format!("http://{}", listener.local_addr().unwrap());
    let handle = thread::spawn(move || {
        let mut requests = Vec::new();
        for index in 0..6 {
            let (mut stream, _) = listener.accept().expect("accept");
            let request = read_request(&mut stream);
            let first = request.lines().next().unwrap_or_default().to_string();
            requests.push(request);
            let body = if first.starts_with("GET /sessions/session-1/artifacts/artifact-1 ") {
                json!({"artifact":{"artifact_id":"artifact-1","kind":"markdown","title":"Report","render_hint":"inline"}})
            } else if first.starts_with("GET /sessions/session-1/artifacts ") {
                json!({"schema":"narada.nars.artifact_index.v1","artifacts":[{"artifact_id":"artifact-1","kind":"markdown"},{"artifact_id":"artifact-2","kind":"text"},{"artifact_id":"artifact-3","kind":"json"}]})
            } else if first.contains("/artifact-1/message ") {
                json!({"artifact":{"artifact_id":"artifact-1","kind":"markdown","title":"Report"},"event":{"event":"assistant_message","request_id":"present-1"},"message_part":{"type":"artifact_ref","artifact_id":"artifact-1"},"idempotent_replay":index==5})
            } else {
                json!({"artifact":{"artifact_id":"artifact-1","kind":"markdown","title":"Report","render_hint":"inline"},"idempotent_replay":index==1})
            };
            let text = body.to_string();
            write!(stream,"HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{text}",text.len()).expect("response")
        }
        requests
    });
    (url, handle)
}

#[test]
fn artifacts_public_protocol_is_complete_bounded_scoped_and_retry_safe() {
    let root =
        std::env::temp_dir().join(format!("narada-artifacts-stdio-{}", uuid::Uuid::new_v4()));
    fs::create_dir_all(&root).expect("root");
    fs::write(root.join("report.md"), "bounded report").expect("report");
    let catalog = run(&root, None, &[rpc(1, "tools/list", json!({}))]);
    let tools = response(&catalog, 1)
        .pointer("/result/tools")
        .and_then(Value::as_array)
        .expect("tools");
    assert_eq!(tools.len(), 7);
    for entry in tools {
        let name = entry["name"].as_str().unwrap();
        assert_eq!(entry["inputSchema"]["title"], format!("{name}.input"));
        assert_eq!(entry["inputSchema"]["additionalProperties"], false);
        assert_bounded(&entry["inputSchema"], name)
    }
    let invalid = tools
        .iter()
        .enumerate()
        .map(|(index, entry)| {
            tool(
                100 + index as u64,
                entry["name"].as_str().unwrap(),
                json!({"unexpected":true}),
            )
        })
        .collect::<Vec<_>>();
    let invalid_results = run(&root, None, &invalid);
    for (index, entry) in tools.iter().enumerate() {
        assert!(
            response(&invalid_results, 100 + index as u64)
                .get("error")
                .is_some(),
            "{}",
            entry["name"]
        )
    }
    let (url, server) = mock_nars();
    let calls = vec![
        tool(10, "artifacts_guidance", json!({})),
        tool(11, "artifacts_doctor", json!({})),
        tool(
            12,
            "artifact_message_part_create",
            json!({"artifact_id":"artifact-1","kind":"markdown"}),
        ),
        tool(
            13,
            "artifact_register_file",
            json!({"path":"report.md","kind":"markdown","title":"Report","idempotency_key":"register-1"}),
        ),
        tool(
            14,
            "artifact_register_file",
            json!({"path":"report.md","kind":"markdown","title":"Report","idempotency_key":"register-1"}),
        ),
        tool(15, "artifact_list", json!({"limit":2})),
        tool(16, "artifact_read", json!({"artifact_id":"artifact-1"})),
        tool(
            17,
            "artifact_present",
            json!({"artifact_id":"artifact-1","idempotency_key":"present-1"}),
        ),
        tool(
            18,
            "artifact_present",
            json!({"artifact_id":"artifact-1","idempotency_key":"present-1"}),
        ),
        tool(19, "artifact_list", json!({"session_id":"other-session"})),
        tool(
            20,
            "artifact_register_file",
            json!({"path":"../outside.md","kind":"markdown","idempotency_key":"outside-1"}),
        ),
    ];
    let results = run(&root, Some(&url), &calls);
    for id in 10..=18 {
        assert!(
            response(&results, id).get("error").is_none(),
            "call {id}: {}",
            response(&results, id)
        )
    }
    assert!(response(&results, 19).get("error").is_some());
    assert!(response(&results, 20).get("error").is_some());
    assert_eq!(structured(response(&results, 15))["count"], 2);
    assert_eq!(structured(response(&results, 15))["total_count"], 3);
    assert_eq!(structured(response(&results, 15))["next_offset"], 2);
    assert_eq!(
        structured(response(&results, 13))["idempotent_replay"],
        false
    );
    assert_eq!(
        structured(response(&results, 14))["idempotent_replay"],
        true
    );
    assert_eq!(
        structured(response(&results, 17))["idempotent_replay"],
        false
    );
    assert_eq!(
        structured(response(&results, 18))["idempotent_replay"],
        true
    );
    let requests = server.join().expect("server");
    assert_eq!(requests.len(), 6);
    assert!(requests[0].contains("\"idempotency_key\":\"register-1\""));
    assert!(requests[1].contains("\"idempotency_key\":\"register-1\""));
    assert!(requests[4].contains("\"request_id\":\"present-1\""));
    assert!(requests[5].contains("\"request_id\":\"present-1\""));
    assert!(requests
        .iter()
        .all(|request| !request.contains("nars_base_url")));
    let index_path = root.join(".narada/crew/nars-sessions/session-1/artifacts/index.json");
    fs::create_dir_all(index_path.parent().unwrap()).expect("index directory");
    fs::write(
        &index_path,
        serde_json::to_vec(&json!({"schema":"narada.nars.artifact_index.v1","artifacts":[{"artifact_id":"local-1","kind":"text"},{"artifact_id":"local-2","kind":"json"}]})).unwrap(),
    )
    .expect("index");
    let before = fs::read(&index_path).unwrap();
    let local = run(
        &root,
        None,
        &[
            tool(30, "artifact_list", json!({"offset":1,"limit":1})),
            tool(31, "artifact_read", json!({"artifact_id":"local-2"})),
        ],
    );
    assert_eq!(
        structured(response(&local, 30))["items"][0]["artifact_id"],
        "local-2"
    );
    assert_eq!(
        structured(response(&local, 31))["artifact"]["artifact_id"],
        "local-2"
    );
    assert_eq!(fs::read(&index_path).unwrap(), before);
    fs::remove_dir_all(root).expect("cleanup");
}
