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
use tungstenite::{accept, Message};

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
        .unwrap_or_else(|| panic!("missing response {id}"))
}
fn structured(value: &Value) -> &Value {
    value
        .pointer("/result/structuredContent")
        .unwrap_or_else(|| panic!("missing structured result: {value}"))
}

fn run(root: &Path, requests: &[Value]) -> Vec<Value> {
    let mut child = Command::new(env!("CARGO_BIN_EXE_narada-mcp-surfaces"))
        .args([
            "--surface-id",
            "browser-control",
            "--site-root",
            &root.to_string_lossy(),
            "--native-authority",
        ])
        .env("NARADA_BROWSER_CONTROL_CDP_TIMEOUT_MS", "1000")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn browser surface");
    {
        let input = child.stdin.as_mut().unwrap();
        for request in requests {
            writeln!(input, "{request}").unwrap();
        }
    }
    let output = child.wait_with_output().expect("wait browser surface");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
            .chars()
            .take(2000)
            .collect::<String>()
    );
    String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).expect("response json"))
        .collect()
}

fn read_headers(stream: &mut TcpStream) -> String {
    stream.set_read_timeout(Some(Duration::from_secs(2))).ok();
    let mut bytes = Vec::new();
    let mut one = [0u8; 1];
    while bytes.len() < 32_768 {
        if stream.read_exact(&mut one).is_err() {
            break;
        }
        bytes.push(one[0]);
        if bytes.ends_with(b"\r\n\r\n") {
            break;
        }
    }
    String::from_utf8_lossy(&bytes).to_string()
}

fn mock_cdp() -> (String, Arc<AtomicBool>, thread::JoinHandle<()>) {
    let http = TcpListener::bind("127.0.0.1:0").unwrap();
    let ws = TcpListener::bind("127.0.0.1:0").unwrap();
    http.set_nonblocking(true).unwrap();
    ws.set_nonblocking(true).unwrap();
    let endpoint = format!("http://{}", http.local_addr().unwrap());
    let websocket = format!("ws://{}/devtools/page/page-1", ws.local_addr().unwrap());
    let stop = Arc::new(AtomicBool::new(false));
    let worker_stop = Arc::clone(&stop);
    let handle = thread::spawn(move || {
        let http_stop = Arc::clone(&worker_stop);
        let http_worker = thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(30);
            while !http_stop.load(Ordering::SeqCst) && Instant::now() < deadline {
                match http.accept() {
                    Ok((mut stream, _)) => {
                        stream.set_nonblocking(false).unwrap();
                        let _ = read_headers(&mut stream);
                        let body=json!([{"id":"page-1","type":"page","title":"Fixture page","url":"https://allowed.example/start?token=secret","webSocketDebuggerUrl":websocket}]).to_string();
                        let _=write!(stream,"HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",body.len());
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(5))
                    }
                    Err(error) => panic!("mock HTTP accept: {error}"),
                }
            }
        });
        let deadline = Instant::now() + Duration::from_secs(30);
        while !worker_stop.load(Ordering::SeqCst) && Instant::now() < deadline {
            match ws.accept() {
                Ok((stream, _)) => {
                    thread::spawn(move || {
                        stream.set_nonblocking(false).unwrap();
                        let mut socket = accept(stream).expect("accept websocket");
                        loop {
                            match socket.read() {
                                Ok(Message::Text(text)) => {
                                    let request: Value = serde_json::from_str(&text).unwrap();
                                    let id = request["id"].clone();
                                    let method = request["method"].as_str().unwrap_or("");
                                    let params = &request["params"];
                                    if method == "DOM.querySelector"
                                        && params["selector"] == "#cdp-timeout"
                                    {
                                        continue;
                                    }
                                    let result = match method {
                                        "Page.navigate" => json!({"frameId":"frame-1"}),
                                        "Accessibility.getFullAXTree" => {
                                            json!({"nodes":[{"nodeId":"ax-1","ignored":false,"role":{"value":"button"},"name":{"value":"Continue"},"properties":[{"name":"focused","value":{"value":false}}]}]})
                                        }
                                        "Page.captureScreenshot" => json!({"data":"aGVsbG8="}),
                                        "DOM.getDocument" => json!({"root":{"nodeId":1}}),
                                        "DOM.querySelector" => {
                                            if params["selector"] == "#missing" {
                                                json!({"nodeId":0})
                                            } else if params["selector"] == "#password" {
                                                json!({"nodeId":3})
                                            } else {
                                                json!({"nodeId":2})
                                            }
                                        }
                                        "DOM.describeNode" => {
                                            if params["nodeId"] == 3 {
                                                json!({"node":{"nodeName":"INPUT","attributes":["type","password","name","credential"]}})
                                            } else {
                                                json!({"node":{"nodeName":"INPUT","attributes":["type","text","name","query"]}})
                                            }
                                        }
                                        "DOM.getBoxModel" => {
                                            json!({"model":{"content":[0,0,10,0,10,10,0,10]}})
                                        }
                                        "DOM.getOuterHTML" => {
                                            json!({"outerHTML":"<button>Continue safely</button>"})
                                        }
                                        _ => json!({}),
                                    };
                                    socket
                                        .send(Message::Text(
                                            json!({"id":id,"result":result}).to_string().into(),
                                        ))
                                        .unwrap();
                                }
                                Ok(Message::Ping(v)) => {
                                    let _ = socket.send(Message::Pong(v));
                                }
                                Ok(Message::Close(_)) | Err(_) => break,
                                _ => {}
                            }
                        }
                    });
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    thread::sleep(Duration::from_millis(5))
                }
                Err(error) => panic!("mock WebSocket accept: {error}"),
            }
        }
        http_worker.join().unwrap();
    });
    (endpoint, stop, handle)
}

#[test]
fn browser_control_public_protocol_is_native_complete_bounded_and_recoverable() {
    let root = std::env::temp_dir().join(format!("narada-browser-stdio-{}", uuid::Uuid::new_v4()));
    fs::create_dir_all(root.join(".ai/tmp/mcp-outputs/workspace")).unwrap();
    fs::write(root.join(".ai/tmp/mcp-outputs/workspace/fixture.json"),serde_json::to_vec(&json!({"schema":"narada.mcp_output_ref.v1","ref":"mcp_output:fixture","output_id":"fixture","tool_name":"browser_control_screenshot","full_output_char_length":17,"truncated":false,"full_output":{"fixture":"paged"}})).unwrap()).unwrap();
    let (endpoint, stop, server) = mock_cdp();
    let session = json!({"profile_id":"profile-1","session_id":"page-1"});
    let attach = json!({"profile_id":"profile-1","session_id":"page-1","cdp_endpoint":endpoint,"allowed_origins":["https://allowed.example"]});
    let catalog = run(
        &root,
        &[
            rpc(1, "tools/list", json!({})),
            rpc(2, "prompts/list", json!({})),
            rpc(3, "prompts/get", json!({"name":"browser_control_workflow"})),
            rpc(
                4,
                "completion/complete",
                json!({"argument":{"name":"name","value":"browser_"}}),
            ),
        ],
    );
    let tools = response(&catalog, 1)["result"]["tools"].as_array().unwrap();
    assert_eq!(tools.len(), 13);
    assert_eq!(
        response(&catalog, 2)["result"]["prompts"][0]["name"],
        "browser_control_workflow"
    );
    assert_eq!(response(&catalog, 4)["result"]["completion"]["total"], 13);
    for listed in tools {
        let name = listed["name"].as_str().unwrap();
        assert_eq!(listed["inputSchema"]["title"], format!("{name}.input"));
        assert_eq!(listed["inputSchema"]["additionalProperties"], false);
    }
    let invalid = tools
        .iter()
        .enumerate()
        .map(|(i, t)| {
            tool(
                100 + i as u64,
                t["name"].as_str().unwrap(),
                json!({"unexpected":true}),
            )
        })
        .collect::<Vec<_>>();
    let rejected = run(&root, &invalid);
    for i in 0..tools.len() {
        assert!(response(&rejected, 100 + i as u64).get("error").is_some());
    }
    let mut status = session.clone();
    status["unexpected"] = json!(true);
    let calls = run(
        &root,
        &[
            tool(10, "browser_control_guidance", json!({})),
            tool(11, "browser_control_session_inventory", json!({})),
            tool(12, "browser_control_attach", attach.clone()),
            tool(13, "browser_control_status", session.clone()),
            tool(
                14,
                "browser_control_navigate",
                merge(
                    &session,
                    json!({"url":"https://allowed.example/next?token=secret"}),
                ),
            ),
            tool(
                15,
                "browser_control_accessibility_snapshot",
                merge(&session, json!({"max_nodes":1})),
            ),
            tool(
                16,
                "browser_control_screenshot",
                merge(&session, json!({"format":"png"})),
            ),
            tool(
                17,
                "browser_control_click",
                merge(&session, json!({"selector":"#continue"})),
            ),
            tool(
                18,
                "browser_control_fill",
                merge(&session, json!({"selector":"#query","value":"public text"})),
            ),
            tool(
                19,
                "browser_control_wait",
                merge(&session, json!({"selector":"#continue","timeout_ms":200})),
            ),
            tool(
                20,
                "browser_control_assert",
                merge(
                    &session,
                    json!({"selector":"#continue","contains_text":"Continue"}),
                ),
            ),
            tool(
                21,
                "mcp_output_show",
                json!({"ref":"mcp_output:fixture","limit":20}),
            ),
            tool(22, "browser_control_attach", attach.clone()),
            tool(
                23,
                "browser_control_navigate",
                merge(&session, json!({"url":"https://refused.example/"})),
            ),
            tool(
                24,
                "browser_control_click",
                merge(&session, json!({"selector":"#continue","intent":"submit"})),
            ),
            tool(
                25,
                "browser_control_fill",
                merge(
                    &session,
                    json!({"selector":"#password","value":"must-not-leak"}),
                ),
            ),
            tool(
                26,
                "browser_control_wait",
                merge(&session, json!({"selector":"#missing","timeout_ms":120})),
            ),
            tool(
                27,
                "browser_control_assert",
                merge(
                    &session,
                    json!({"selector":"#continue","contains_text":"absent"}),
                ),
            ),
            tool(28, "browser_control_detach", session.clone()),
            tool(29, "browser_control_status", session.clone()),
            tool(30, "browser_control_attach", attach.clone()),
            tool(
                31,
                "browser_control_wait",
                merge(
                    &session,
                    json!({"selector":"#cdp-timeout","timeout_ms":200}),
                ),
            ),
            tool(32, "browser_control_detach", session.clone()),
        ],
    );
    assert_eq!(structured(response(&calls, 11))["count"], 0);
    assert_eq!(structured(response(&calls, 12))["status"], "ok");
    assert_eq!(
        structured(response(&calls, 14)).pointer("/result/navigation_error"),
        Some(&Value::Null)
    );
    assert_eq!(structured(response(&calls, 15))["node_count"], 1);
    assert_eq!(structured(response(&calls, 16))["data_base64"], "aGVsbG8=");
    assert_eq!(
        structured(response(&calls, 17)).pointer("/result/clicked"),
        Some(&json!(true))
    );
    assert_eq!(
        structured(response(&calls, 18)).pointer("/result/value_length"),
        Some(&json!(11))
    );
    assert_eq!(
        structured(response(&calls, 19)).pointer("/result/found"),
        Some(&json!(true))
    );
    assert_eq!(structured(response(&calls, 20))["matched"], true);
    assert_eq!(
        structured(response(&calls, 21))["output_text"],
        "{\n  \"fixture\": \"page"
    );
    for (id, code) in [
        (22, "browser_session_already_attached"),
        (23, "navigation_origin_refused"),
        (24, "confirmation_required"),
        (25, "sensitive_field_refused"),
        (29, "browser_session_not_attached"),
        (31, "cdp_response_timeout_or_read_failed"),
    ] {
        let actual = response(&calls, id)["error"]["data"]["code"]
            .as_str()
            .unwrap_or("");
        assert_eq!(actual, code, "id={id}: {}", response(&calls, id));
    }
    assert_eq!(
        structured(response(&calls, 26)).pointer("/result/timed_out"),
        Some(&json!(true))
    );
    assert_eq!(structured(response(&calls, 27))["matched"], false);
    assert_eq!(
        structured(response(&calls, 28)).pointer("/result/detached"),
        Some(&json!(true))
    );
    assert_eq!(structured(response(&calls, 30))["status"], "ok");
    assert_eq!(
        structured(response(&calls, 32)).pointer("/result/detached"),
        Some(&json!(true))
    );
    let fresh = run(
        &root,
        &[tool(40, "browser_control_session_inventory", json!({}))],
    );
    assert_eq!(structured(response(&fresh, 40))["count"], 0);
    let receipts =
        fs::read_to_string(root.join(".ai/tmp/browser-control/action-receipts.jsonl")).unwrap();
    assert!(!receipts.contains("must-not-leak"));
    assert!(receipts.lines().count() >= 12);
    stop.store(true, Ordering::SeqCst);
    server.join().unwrap();
    let _ = fs::remove_dir_all(root);
}

fn merge(base: &Value, extra: Value) -> Value {
    let mut out = base.as_object().cloned().unwrap();
    out.extend(extra.as_object().cloned().unwrap());
    Value::Object(out)
}
