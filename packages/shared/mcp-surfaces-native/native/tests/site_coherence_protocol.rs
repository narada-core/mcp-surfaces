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
fn tool(id: u64, name: &str, arguments: Value) -> Value {
    rpc(id, "tools/call", json!({"name":name,"arguments":arguments}))
}
fn run(root: &Path, worker_url: Option<&str>, requests: &[Value]) -> Vec<Value> {
    let mut command = Command::new(env!("CARGO_BIN_EXE_narada-mcp-surfaces"));
    command.args([
        "--surface-id",
        "site-coherence",
        "--site-root",
        &root.to_string_lossy(),
    ]);
    if let Some(url) = worker_url {
        command.env("CLOUDFLARE_CARRIER_URL", url);
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
            writeln!(input, "{request}").expect("write");
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
        assert!(schema.get("maxLength").is_some(), "unbounded string {path}");
    }
    if schema.get("type").and_then(Value::as_str) == Some("array") {
        assert!(schema.get("maxItems").is_some(), "unbounded array {path}");
    }
    if let Some(properties) = schema.get("properties").and_then(Value::as_object) {
        for (name, child) in properties {
            assert_bounded(child, &format!("{path}/{name}"));
        }
    }
}
fn read_request(stream: &mut TcpStream) -> String {
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .expect("timeout");
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
            if let Some(header_end) = bytes.windows(4).position(|part| part == b"\r\n\r\n") {
                let headers = String::from_utf8_lossy(&bytes[..header_end]);
                let length = headers
                    .lines()
                    .find_map(|line| {
                        line.to_ascii_lowercase()
                            .strip_prefix("content-length:")
                            .map(str::trim)
                            .and_then(|value| value.parse::<usize>().ok())
                    })
                    .unwrap_or(0);
                expected = Some(header_end + 4 + length);
            }
        }
        if expected.is_some_and(|length| bytes.len() >= length) {
            break;
        }
    }
    String::from_utf8(bytes).expect("request utf8")
}
fn mock_carrier() -> (String, thread::JoinHandle<Vec<String>>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let url = format!("http://{}", listener.local_addr().expect("address"));
    let handle = thread::spawn(move || {
        let mut requests = Vec::new();
        for index in 0..3 {
            let (mut stream, _) = listener.accept().expect("accept");
            requests.push(read_request(&mut stream));
            let (status, body) = if index < 2 {
                (
                    "200 OK",
                    r#"{"site":{"site_id":"demo"},"site_product_status":{"health":"ready","next_action":"continue","continuity_state":"ready","session_count":1},"memberships":[{}]}"#,
                )
            } else {
                ("401 Unauthorized", r#"{"code":"unauthorized"}"#)
            };
            write!(stream, "HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}", body.len()).expect("respond");
        }
        requests
    });
    (url, handle)
}
fn prepare_site(root: &Path) {
    let continuity = root.join(".narada/site-continuity");
    fs::create_dir_all(continuity.join("health")).expect("health");
    fs::create_dir_all(root.join(".narada/auth")).expect("auth");
    fs::write(continuity.join("health/cloudflare-continuity-health-last.json"), serde_json::to_vec(&json!({"status":"ready","generated_at":"2026-01-01T00:00:00Z","continuity_health":{"local_sync_status":"synced","local_inbound_status":"synced"},"scheduler_task_readback":{"scheduled_task_state":"Enabled","last_result":"0","cadence_status":"matches_plan"},"cloudflare_product_posture":{"state":"ready","site_product_overview":{"next_action":"continue"}}})).expect("json")).expect("health write");
    fs::write(continuity.join("bindings.json"), r#"{"bindings":[{}]}"#).expect("bindings");
    fs::write(
        root.join(".narada/auth/cloudflare-operator-session.json"),
        r#"{"cookie":"narada_operator_session=secret-proof-cookie; Path=/"}"#,
    )
    .expect("session");
}

#[test]
fn site_coherence_public_protocol_is_complete_bounded_and_read_only() {
    let root = std::env::temp_dir().join(format!(
        "narada-site-coherence-stdio-{}",
        uuid::Uuid::new_v4()
    ));
    prepare_site(&root);
    let health_path =
        root.join(".narada/site-continuity/health/cloudflare-continuity-health-last.json");
    let health_before = fs::read(&health_path).expect("health before");
    let catalog = run(&root, None, &[rpc(1, "tools/list", json!({}))]);
    let tools = response(&catalog, 1)
        .pointer("/result/tools")
        .and_then(Value::as_array)
        .expect("tools");
    assert_eq!(tools.len(), 3);
    for entry in tools {
        let name = entry["name"].as_str().expect("name");
        assert_eq!(entry["inputSchema"]["title"], format!("{name}.input"));
        assert_eq!(entry["inputSchema"]["additionalProperties"], false);
        assert_eq!(entry["annotations"]["readOnlyHint"], true);
        assert_bounded(&entry["inputSchema"], name);
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
        );
    }

    let (url, server) = mock_carrier();
    let calls = vec![
        tool(10, "site_coherence_guidance", json!({})),
        tool(11, "site_coherence_doctor", json!({})),
        tool(
            12,
            "site_coherence_check",
            json!({"site_id":"demo","fetch_cloudflare":false}),
        ),
        tool(
            13,
            "site_coherence_check",
            json!({"site_id":"demo","fetch_cloudflare":true}),
        ),
        tool(
            14,
            "site_coherence_check",
            json!({"site_id":"demo","fetch_cloudflare":true}),
        ),
        tool(
            15,
            "site_coherence_check",
            json!({"site_id":"demo","fetch_cloudflare":true}),
        ),
        tool(
            16,
            "site_coherence_check",
            json!({"site_id":"../escape","fetch_cloudflare":false}),
        ),
    ];
    let results = run(&root, Some(&url), &calls);
    for id in 10..=15 {
        assert!(
            response(&results, id).get("error").is_none(),
            "call {id}: {}",
            response(&results, id)
        );
    }
    assert!(response(&results, 16).get("error").is_some());
    assert_eq!(
        structured(response(&results, 12))["coherence"]["state"],
        "local_only"
    );
    assert_eq!(
        structured(response(&results, 13))["coherence"]["state"],
        "coherent"
    );
    assert_eq!(
        structured(response(&results, 14))["coherence"]["state"],
        "coherent"
    );
    assert_eq!(
        structured(response(&results, 15))["coherence"]["state"],
        "degraded"
    );
    assert_eq!(
        structured(response(&results, 15))["coherence"]["operator_action"],
        "authenticate_the_cloudflare_operator_session"
    );
    assert!(!serde_json::to_string(&results)
        .unwrap()
        .contains("secret-proof-cookie"));
    let requests = server.join().expect("server");
    assert_eq!(requests.len(), 3);
    assert!(requests
        .iter()
        .all(|request| request.contains("POST /api/carrier")));
    assert!(requests
        .iter()
        .all(|request| request.contains("narada_operator_session=secret-proof-cookie")));
    assert!(requests
        .iter()
        .all(|request| request.contains("\"operation\":\"site.read\"")));
    assert_eq!(fs::read(&health_path).expect("health after"), health_before);

    let missing = root.join("missing");
    let missing_result = run(
        &missing,
        None,
        &[tool(
            20,
            "site_coherence_check",
            json!({"site_id":"demo","fetch_cloudflare":false}),
        )],
    );
    assert_eq!(
        structured(response(&missing_result, 20))["status"],
        "missing_local"
    );
    let malformed = root.join("malformed");
    fs::create_dir_all(malformed.join(".narada/site-continuity/health")).expect("malformed dir");
    fs::write(
        malformed.join(".narada/site-continuity/health/cloudflare-continuity-health-last.json"),
        "not-json",
    )
    .expect("malformed write");
    let malformed_result = run(
        &malformed,
        None,
        &[tool(
            21,
            "site_coherence_check",
            json!({"site_id":"demo","fetch_cloudflare":false}),
        )],
    );
    assert_eq!(
        structured(response(&malformed_result, 21))["status"],
        "invalid_local"
    );
    fs::remove_dir_all(root).expect("cleanup");
}
