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
fn run(root: &Path, url: &str, requests: &[Value]) -> Vec<Value> {
    let mut child = Command::new(env!("CARGO_BIN_EXE_narada-mcp-surfaces"))
        .args([
            "--surface-id",
            "mailbox",
            "--site-root",
            &root.to_string_lossy(),
        ])
        .env("GRAPH_ACCESS_TOKEN", "test-graph-token")
        .env("NARADA_NATIVE_GRAPH_ALLOW_INSECURE_TEST", "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
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
    let values = String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect::<Vec<_>>();
    assert!(
        !serde_json::to_string(&values)
            .unwrap()
            .contains("test-graph-token"),
        "credential disclosure"
    );
    let _ = url;
    values
}
fn response(values: &[Value], id: u64) -> &Value {
    values.iter().find(|value| value["id"] == id).unwrap()
}
fn structured(value: &Value) -> &Value {
    value.pointer("/result/structuredContent").unwrap()
}
fn bounded(schema: &Value, path: &str) {
    if schema.get("type").and_then(Value::as_str) == Some("string") && schema.get("enum").is_none()
    {
        assert!(schema.get("maxLength").is_some(), "unbounded string {path}");
    }
    if schema.get("type").and_then(Value::as_str) == Some("array") {
        assert!(schema.get("maxItems").is_some(), "unbounded array {path}");
    }
    if let Some(properties) = schema.get("properties").and_then(Value::as_object) {
        for (name, child) in properties {
            bounded(child, &format!("{path}/{name}"));
        }
    }
}
fn read_request(stream: &mut TcpStream) -> String {
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let mut bytes = Vec::new();
    let mut buffer = [0u8; 2048];
    loop {
        let count = stream.read(&mut buffer).unwrap_or(0);
        if count == 0 {
            break;
        }
        bytes.extend_from_slice(&buffer[..count]);
        if bytes.windows(4).any(|part| part == b"\r\n\r\n") {
            break;
        }
    }
    String::from_utf8_lossy(&bytes).to_string()
}
fn mock_graph() -> (String, thread::JoinHandle<Vec<String>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let response_url = url.clone();
    let handle = thread::spawn(move || {
        let mut requests = Vec::new();
        for index in 0..2 {
            let (mut stream, _) = listener.accept().unwrap();
            requests.push(read_request(&mut stream));
            if index == 0 {
                thread::sleep(Duration::from_millis(250));
            }
            let body=json!({"value":[{"id":"m-live","conversationId":"thread-live","parentFolderId":"inbox","subject":"Live fixture","from":{"emailAddress":{"address":"sender@allowed.test"}},"toRecipients":[{"emailAddress":{"address":"support@example.test"}}],"receivedDateTime":"2026-08-14T00:00:00Z","isRead":false,"body":{"contentType":"text","content":"bounded live body"},"hasAttachments":false,"@odata.etag":"v1"}],"@odata.deltaLink":format!("{response_url}/delta-finished")}).to_string();
            let _=write!(stream,"HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",body.len());
        }
        requests
    });
    (url, handle)
}
fn setup(root: &Path, url: &str) {
    fs::create_dir_all(root.join(".ai/mailboxes/support")).unwrap();
    fs::write(root.join(".ai/mailboxes/support/messages.json"),serde_json::to_vec(&json!([{"id":"m-local-1","conversationId":"thread-local","mailbox_id":"support","folder":"Inbox","subject":"First local","body":{"content":"alpha needle"},"receivedDateTime":"2026-08-13T01:00:00Z","isRead":false},{"id":"m-local-2","conversationId":"thread-local","mailbox_id":"support","folder":"Inbox","subject":"Second local","body":{"content":"beta needle"},"receivedDateTime":"2026-08-13T02:00:00Z","isRead":true}])).unwrap()).unwrap();
    fs::write(
        root.join(".ai/mailboxes/support/broken.jsonl"),
        "{\"id\":\"valid-but-no-shape\"}\nnot-json\n",
    )
    .unwrap();
    fs::create_dir_all(root.join(".ai/tmp/mcp-outputs/workspace")).unwrap();
    fs::write(root.join(".ai/tmp/mcp-outputs/workspace/output-1.json"),serde_json::to_vec(&json!({"schema":"narada.mcp_output_ref.v1","ref":"mcp_output:output-1","output_id":"output-1","tool_name":"mailbox_messages_list","full_output":{"value":"abcdefghij"},"truncated":false})).unwrap()).unwrap();
    fs::create_dir_all(root.join("config")).unwrap();
    fs::write(root.join("config/config.json"),serde_json::to_vec(&json!({"scopes":[{"scope_id":"support","root_dir":".narada/runtime/mailboxes/support","sources":[{"type":"graph"}],"graph":{"user_id":"support@example.test","base_url":url,"prefer_immutable_ids":true},"scope":{"included_container_refs":["inbox"],"included_item_kinds":["message"]},"normalize":{"attachment_policy":"metadata_only","body_policy":"text_only","include_headers":false,"tombstones_enabled":true},"runtime":{"acquire_lock_timeout_ms":1000,"cleanup_tmp_on_startup":true},"admission":{"mail":{"included_folder_refs":["inbox"],"allowed_sender_domains":["allowed.test"],"unknown_sender_behavior":"ignore"}}}]})).unwrap()).unwrap();
}

#[test]
fn mailbox_public_protocol_is_complete_bounded_paged_and_recoverable() {
    let root = std::env::temp_dir().join(format!("narada-mailbox-stdio-{}", uuid::Uuid::new_v4()));
    let (url, server) = mock_graph();
    let empty_root = root.join("empty");
    fs::create_dir_all(&empty_root).unwrap();
    let empty = run(
        &empty_root,
        &url,
        &[
            tool(2, "mailbox_doctor", json!({})),
            tool(3, "mailbox_accounts_list", json!({})),
            tool(4, "mailbox_messages_list", json!({})),
            tool(5, "mailbox_search", json!({"query":"nothing"})),
            tool(6, "mailbox_thread_show", json!({"thread_id":"missing"})),
        ],
    );
    for id in 2..=6 {
        assert!(
            response(&empty, id).get("error").is_none(),
            "empty {id}: {}",
            response(&empty, id)
        );
    }
    assert_eq!(structured(response(&empty, 4))["count"], 0);
    assert_eq!(structured(response(&empty, 6))["status"], "not_found");
    let escaped = root.join("escaped");
    fs::create_dir_all(escaped.join(".ai")).unwrap();
    fs::write(
        escaped.join(".ai/mailbox-mcp.json"),
        r#"{"roots":["../outside"]}"#,
    )
    .unwrap();
    let confined = run(&escaped, &url, &[tool(7, "mailbox_doctor", json!({}))]);
    assert_eq!(structured(response(&confined, 7))["status"], "degraded");
    assert_eq!(structured(response(&confined, 7))["message_count"], 0);
    let raw_root = root.join("raw");
    fs::create_dir_all(raw_root.join(".ai/mailboxes/support")).unwrap();
    fs::write(raw_root.join(".ai/mailboxes/support/messages.json"),serde_json::to_vec(&json!({"id":"large","subject":"large","body":{"content":"body"},"extra":"x".repeat(70000)})).unwrap()).unwrap();
    let raw = run(
        &raw_root,
        &url,
        &[tool(
            8,
            "mailbox_message_show",
            json!({"message_id":"large","include_raw":true}),
        )],
    );
    assert!(response(&raw, 8).get("error").is_some());
    setup(&root, &url);
    let catalog = run(&root, &url, &[rpc(1, "tools/list", json!({}))]);
    let tools = response(&catalog, 1)
        .pointer("/result/tools")
        .and_then(Value::as_array)
        .unwrap();
    assert_eq!(tools.len(), 19);
    for entry in tools {
        let name = entry["name"].as_str().unwrap();
        assert_eq!(entry["inputSchema"]["title"], format!("{name}.input"));
        assert_eq!(entry["inputSchema"]["additionalProperties"], false);
        bounded(&entry["inputSchema"], name);
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
    let rejected = run(&root, &url, &invalid);
    for (index, entry) in tools.iter().enumerate() {
        assert!(
            response(&rejected, 100 + index as u64)
                .get("error")
                .is_some(),
            "{}",
            entry["name"]
        );
    }
    let first = run(
        &root,
        &url,
        &[
            tool(10, "mailbox_guidance", json!({})),
            tool(11, "mailbox_doctor", json!({})),
            tool(12, "mailbox_accounts_list", json!({})),
            tool(13, "mailbox_messages_list", json!({"offset":1,"limit":1})),
            tool(
                14,
                "mailbox_message_show",
                json!({"message_id":"m-local-1"}),
            ),
            tool(15, "mailbox_search", json!({"query":"needle","limit":1})),
            tool(
                16,
                "mailbox_thread_show",
                json!({"thread_id":"thread-local","offset":1,"limit":1}),
            ),
            tool(
                17,
                "mailbox_output_show",
                json!({"ref":"mcp_output:output-1","offset":0,"limit":5}),
            ),
            tool(
                18,
                "mailbox_sync_generation",
                json!({"idempotency_key":"sync-1","scope_id":"support","timeout_ms":100}),
            ),
            tool(
                19,
                "mailbox_output_show",
                json!({"ref":"mcp_output:output-1","output_ref":"mcp_output:other"}),
            ),
        ],
    );
    for id in 10..=17 {
        assert!(
            response(&first, id).get("error").is_none(),
            "{id}: {}",
            response(&first, id)
        );
    }
    assert!(response(&first, 18).get("error").is_some());
    assert!(response(&first, 19).get("error").is_some());
    assert_eq!(structured(response(&first, 11))["invalid_count"], 1);
    assert_eq!(structured(response(&first, 13))["total_count"], 2);
    assert_eq!(
        structured(response(&first, 13))["messages"][0]["message_id"],
        "m-local-1"
    );
    assert_eq!(
        structured(response(&first, 16))["messages"][0]["message_id"],
        "m-local-2"
    );
    assert_eq!(structured(response(&first, 17))["output_truncated"], true);
    let sync = run(
        &root,
        &url,
        &[tool(
            20,
            "mailbox_sync_generation",
            json!({"idempotency_key":"sync-1","scope_id":"support","timeout_ms":10000}),
        )],
    );
    assert!(
        response(&sync, 20).get("error").is_none(),
        "{}",
        response(&sync, 20)
    );
    let generation_id = structured(response(&sync, 20))
        .pointer("/result/generation_id")
        .and_then(Value::as_str)
        .unwrap()
        .to_string();
    let replay = run(
        &root,
        &url,
        &[
            tool(
                21,
                "mailbox_sync_generation",
                json!({"idempotency_key":"sync-1","scope_id":"support","timeout_ms":10000}),
            ),
            tool(
                22,
                "mailbox_generation_show",
                json!({"generation_id":generation_id,"offset":0,"limit":1}),
            ),
            tool(
                23,
                "mailbox_reconcile_first_observations",
                json!({"idempotency_key":"reconcile-1","generation_id":generation_id,"scope_id":"support"}),
            ),
            tool(
                24,
                "mailbox_message_fact_find",
                json!({"scope_id":"support","message_id":"m-live"}),
            ),
        ],
    );
    for id in 21..=24 {
        assert!(
            response(&replay, id).get("error").is_none(),
            "{id}: {}",
            response(&replay, id)
        );
    }
    assert_eq!(
        structured(response(&replay, 21)).pointer("/result/idempotency_replayed"),
        Some(&json!(true))
    );
    let found = structured(response(&replay, 24));
    let fact_id = found
        .pointer("/fact_id")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("fact projection: {found}"))
        .to_string();
    let source_event_id = found
        .pointer("/source_event_id")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("event projection: {found}"))
        .to_string();
    let admitted = run(
        &root,
        &url,
        &[
            tool(
                30,
                "mailbox_fact_show",
                json!({"fact_id":fact_id,"scope_id":"support"}),
            ),
            tool(
                31,
                "mailbox_outbox_consumer_register",
                json!({"consumer_id":"consumer-1","scope_id":"support","topics":["mailbox.message.first_observed","mailbox.message.admitted"],"start_at":"2020-01-01T00:00:00Z"}),
            ),
            tool(
                32,
                "mailbox_outbox_consumer_show",
                json!({"consumer_id":"consumer-1"}),
            ),
            tool(
                33,
                "mailbox_message_admit",
                json!({"idempotency_key":"admit-1","fact_id":fact_id,"source_event_id":source_event_id,"scope_id":"support"}),
            ),
            tool(
                34,
                "mailbox_admission_show",
                json!({"scope_id":"support","fact_id":fact_id}),
            ),
            tool(
                35,
                "mailbox_outbox_list",
                json!({"consumer_id":"consumer-1","limit":1}),
            ),
        ],
    );
    for id in 30..=35 {
        assert!(
            response(&admitted, id).get("error").is_none(),
            "{id}: {}",
            response(&admitted, id)
        );
    }
    let event_id = structured(response(&admitted, 35))["items"][0]["event_id"]
        .as_str()
        .unwrap()
        .to_string();
    let ack = run(
        &root,
        &url,
        &[
            tool(
                40,
                "mailbox_outbox_ack",
                json!({"consumer_id":"consumer-1","event_id":event_id,"receipt":{"schema":"test.receipt.v1","outcome":"completed","effect_ref":"effect:1"}}),
            ),
            tool(
                41,
                "mailbox_outbox_ack",
                json!({"consumer_id":"consumer-1","event_id":event_id,"receipt":{"schema":"test.receipt.v1","outcome":"completed","effect_ref":"effect:1"}}),
            ),
            tool(
                42,
                "mailbox_outbox_list",
                json!({"consumer_id":"consumer-1","limit":10}),
            ),
        ],
    );
    for id in 40..=42 {
        assert!(
            response(&ack, id).get("error").is_none(),
            "{id}: {}",
            response(&ack, id)
        );
    }
    assert_eq!(structured(response(&ack, 40))["replayed"], false);
    assert_eq!(structured(response(&ack, 41))["replayed"], true);
    assert_eq!(structured(response(&ack, 42))["count"], 1);
    let requests = server.join().unwrap();
    assert_eq!(requests.len(), 2);
    assert!(requests.iter().all(|request| request
        .to_ascii_lowercase()
        .contains("authorization: bearer test-graph-token")));
    fs::remove_dir_all(root).unwrap();
}
