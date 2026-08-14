use serde_json::{json, Value};
use sha2::{Digest, Sha256};
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

fn tool(id: u64, name: &str, arguments: Value) -> Value {
    rpc(id, "tools/call", json!({"name":name,"arguments":arguments}))
}

fn run(root: &Path, requests: &[Value]) -> Vec<Value> {
    let mut child = Command::new(env!("CARGO_BIN_EXE_narada-mcp-surfaces"))
        .args([
            "--surface-id",
            "graph-mail",
            "--site-root",
            &root.to_string_lossy(),
            "--native-authority",
        ])
        .env("GRAPH_ACCESS_TOKEN", "stdio-secret-token")
        .env("NARADA_NATIVE_GRAPH_ALLOW_INSECURE_TEST", "1")
        .env("NARADA_GRAPH_MAIL_ALLOW_INSECURE_TEST", "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn graph-mail");
    {
        let input = child.stdin.as_mut().expect("stdin");
        for request in requests {
            writeln!(input, "{request}").expect("write request");
        }
    }
    let output = child.wait_with_output().expect("wait graph-mail");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
            .chars()
            .take(2000)
            .collect::<String>()
    );
    let stdout = String::from_utf8(output.stdout).expect("utf8 stdout");
    assert!(
        !stdout.contains("stdio-secret-token"),
        "credential disclosed"
    );
    stdout
        .lines()
        .map(|line| serde_json::from_str(line).expect("json response"))
        .collect()
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

fn read_request(stream: &mut TcpStream) -> String {
    stream
        .set_read_timeout(Some(Duration::from_secs(2)))
        .expect("timeout");
    let mut bytes = Vec::new();
    let mut buffer = [0u8; 4096];
    loop {
        let count = stream.read(&mut buffer).unwrap_or(0);
        if count == 0 {
            break;
        }
        bytes.extend_from_slice(&buffer[..count]);
        let Some(header_end) = bytes
            .windows(4)
            .position(|part| part == b"\r\n\r\n")
            .map(|index| index + 4)
        else {
            continue;
        };
        let headers = String::from_utf8_lossy(&bytes[..header_end]);
        let length = headers
            .lines()
            .find_map(|line| {
                line.to_ascii_lowercase()
                    .strip_prefix("content-length:")
                    .and_then(|value| value.trim().parse::<usize>().ok())
            })
            .unwrap_or(0);
        if bytes.len() >= header_end + length {
            break;
        }
    }
    String::from_utf8_lossy(&bytes).to_string()
}

fn mock_graph() -> (String, Arc<AtomicBool>, thread::JoinHandle<Vec<String>>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind mock Graph");
    listener.set_nonblocking(true).expect("nonblocking");
    let base = format!("http://{}", listener.local_addr().expect("address"));
    let upload_url = format!("{base}/upload");
    let stop = Arc::new(AtomicBool::new(false));
    let thread_stop = Arc::clone(&stop);
    let handle = thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(20);
        let mut requests = Vec::new();
        while !thread_stop.load(Ordering::SeqCst) && Instant::now() < deadline {
            match listener.accept() {
                Ok((mut stream, _)) => {
                    let request = read_request(&mut stream);
                    let is_delete = request.split_whitespace().next() == Some("DELETE");
                    let ticket_lookup = request.contains("singleValueExtendedProperties");
                    requests.push(request);
                    let values = if ticket_lookup {
                        json!([{"id":"draft-1","isDraft":true,"@odata.etag":"etag-1","singleValueExtendedProperties":[{"id":"String {d700a6f2-79ad-4f44-9df7-3e9b622f09f8} Name NaradaTicketDraftOperation","value":"operation-1"}]}])
                    } else {
                        json!([{"id":"message-1","isDraft":false,"receivedDateTime":"2026-08-14T00:00:00Z"}])
                    };
                    let body = json!({
                        "id":"draft-1","isDraft":true,"subject":"fixture","conversationId":"thread-1",
                        "name":"fixture.txt","contentType":"text/plain","contentBytes":"SGVsbG8=",
                        "singleValueExtendedProperties":[{"id":"String {d700a6f2-79ad-4f44-9df7-3e9b622f09f8} Name NaradaTicketDraftOperation","value":"operation-1"}],
                        "uploadUrl":upload_url,
                        "value":values
                    }).to_string();
                    let status = if is_delete {
                        "204 No Content"
                    } else {
                        "200 OK"
                    };
                    let response_body = if is_delete { "" } else { &body };
                    let _ = write!(stream, "HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{response_body}", response_body.len());
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    thread::sleep(Duration::from_millis(5))
                }
                Err(error) => panic!("mock Graph accept: {error}"),
            }
        }
        requests
    });
    (base, stop, handle)
}

fn slow_graph() -> (String, thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind slow Graph");
    let base = format!("http://{}", listener.local_addr().expect("address"));
    let handle = thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("slow accept");
        let _ = read_request(&mut stream);
        thread::sleep(Duration::from_millis(350));
        let body = r#"{"value":[]}"#;
        let _ = write!(stream, "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}", body.len());
    });
    (base, handle)
}

fn setup(root: &Path, base_url: &str) {
    fs::create_dir_all(root.join(".ai/tmp/mcp-outputs/workspace")).expect("state roots");
    fs::create_dir_all(root.join("attachments")).expect("attachment root");
    fs::write(root.join("attachments/upload.txt"), b"upload fixture").expect("upload fixture");
    fs::write(
        root.join(".ai/graph-mail-mcp.json"),
        serde_json::to_vec(&json!({
            "graph_base_url":base_url,
            "allowed_mailboxes":["me"],
            "allowed_attachment_roots":[root.join("attachments")],
            "allow_folder_create":true,
            "allow_message_move":true,
            "allow_message_mark_read":true,
            "allow_send_draft":true,
            "send_approval_token":"send-ok",
            "mailbox_organization_approval_token":"organize-ok",
            "allow_device_code_auth":false
        }))
        .expect("config json"),
    )
    .expect("config");
    fs::write(root.join(".ai/tmp/mcp-outputs/workspace/graph-output.json"), serde_json::to_vec(&json!({
        "schema":"narada.mcp_output_ref.v1","ref":"mcp_output:graph-output","output_id":"graph-output",
        "tool_name":"graph_mail_query","full_output":{"value":"abcdefghij"},"truncated":false
    })).expect("output json")).expect("output");
}

#[test]
fn graph_mail_public_protocol_is_closed_complete_and_native() {
    let root =
        std::env::temp_dir().join(format!("narada-graph-mail-stdio-{}", uuid::Uuid::new_v4()));
    let (base_url, stop, server) = mock_graph();
    setup(&root, &base_url);

    let catalog = run(&root, &[rpc(1, "tools/list", json!({}))]);
    let tools = response(&catalog, 1)["result"]["tools"]
        .as_array()
        .expect("tools");
    assert_eq!(tools.len(), 34);
    for tool in tools {
        let name = tool["name"].as_str().expect("tool name");
        assert_eq!(tool["inputSchema"]["title"], format!("{name}.input"));
        assert_eq!(tool["inputSchema"]["additionalProperties"], false);
    }

    let mut invalid = Vec::new();
    for (index, listed) in tools.iter().enumerate() {
        invalid.push(tool(
            100 + index as u64,
            listed["name"].as_str().unwrap(),
            json!({"unexpected":true}),
        ));
    }
    let invalid_results = run(&root, &invalid);
    for (index, listed) in tools.iter().enumerate() {
        let result = response(&invalid_results, 100 + index as u64);
        assert!(
            result.get("error").is_some(),
            "{} accepted unknown input: {result}",
            listed["name"]
        );
    }

    let digest = format!(
        "{:x}",
        Sha256::digest(br#"{"body_text":"ticket reply","mailbox_id":"me","reply_mode":"reply","source_id":"source-1","source_message_id":"message-1"}"#)
    );
    let calls = vec![
        tool(10, "graph_mail_guidance", json!({})),
        tool(11, "graph_mail_doctor", json!({})),
        tool(12, "graph_mail_auth_device_code_start", json!({})),
        tool(
            13,
            "graph_mail_auth_device_code_poll",
            json!({"flow_id":"missing"}),
        ),
        tool(14, "graph_mail_auth_status", json!({})),
        tool(15, "graph_mail_auth_clear", json!({"confirm_clear":true})),
        tool(16, "graph_mail_query", json!({"limit":2})),
        tool(
            17,
            "graph_mail_message_show",
            json!({"message_id":"message-1"}),
        ),
        tool(18, "graph_mail_folder_list", json!({"limit":2})),
        tool(
            19,
            "graph_mail_folder_create",
            json!({"display_name":"Archive","confirm_write":true,"approval_token":"organize-ok"}),
        ),
        tool(
            20,
            "graph_mail_message_move",
            json!({"message_id":"message-1","destination_folder_id":"archive","confirm_write":true,"approval_token":"organize-ok"}),
        ),
        tool(
            21,
            "graph_mail_message_mark_read",
            json!({"message_id":"message-1","confirm_write":true,"idempotency_key":"mark-1"}),
        ),
        tool(
            22,
            "graph_mail_attachment_list",
            json!({"message_id":"message-1","limit":2}),
        ),
        tool(
            23,
            "graph_mail_attachment_get",
            json!({"message_id":"message-1","attachment_id":"attachment-1","include_content":false}),
        ),
        tool(
            24,
            "graph_mail_attachment_download_file",
            json!({"message_id":"message-1","attachment_id":"attachment-1","file_path":root.join("attachments/download.txt")}),
        ),
        tool(
            25,
            "graph_mail_attachment_add",
            json!({"draft_id":"draft-1","name":"a.txt","content_type":"text/plain","content_base64":"SGVsbG8="}),
        ),
        tool(
            26,
            "graph_mail_attachment_upload_session_create",
            json!({"draft_id":"draft-1","name":"large.txt","size":5}),
        ),
        tool(
            27,
            "graph_mail_attachment_upload_chunk",
            json!({"upload_url":format!("{base_url}/upload"),"content_base64":"SGVsbG8=","range_start":0,"range_end":4,"total_size":5}),
        ),
        tool(
            28,
            "graph_mail_attachment_upload_file",
            json!({"draft_id":"draft-1","file_path":root.join("attachments/upload.txt"),"chunk_size":327680}),
        ),
        tool(
            29,
            "graph_mail_attachment_delete",
            json!({"draft_id":"draft-1","attachment_id":"attachment-1"}),
        ),
        tool(
            30,
            "graph_mail_draft_create",
            json!({"subject":"Draft","body_text":"hello","to_recipients":["recipient@example.test"]}),
        ),
        tool(
            31,
            "graph_mail_reply_draft_create",
            json!({"message_id":"message-1","comment":"reply"}),
        ),
        tool(
            32,
            "graph_mail_reply_all_draft_create",
            json!({"message_id":"message-1","comment":"reply all"}),
        ),
        tool(
            33,
            "graph_mail_forward_draft_create",
            json!({"message_id":"message-1","to_recipients":["recipient@example.test"]}),
        ),
        tool(
            34,
            "graph_mail_reply_all_to_last_in_thread_draft_create",
            json!({"conversation_id":"thread-1","comment":"latest"}),
        ),
        tool(
            35,
            "graph_mail_ticket_draft_upsert",
            json!({"ticket_id":"ticket-1","effect_claim_id":"claim-1","draft_operation_key":"operation-1","draft_request_digest":digest.clone(),"draft_source_id":"source-1","mailbox_id":"me","source_message_id":"message-1","reply_mode":"reply","body_text":"ticket reply","idempotency_key":"ticket-upsert-1"}),
        ),
        tool(
            36,
            "graph_mail_ticket_draft_discard",
            json!({"ticket_id":"ticket-1","effect_claim_id":"claim-1","draft_operation_key":"operation-1","mailbox_id":"me","draft_id":"draft-1","idempotency_key":"ticket-discard-1","confirm_discard":true}),
        ),
        tool(
            37,
            "graph_mail_ticket_draft_disposition_scan",
            json!({"limit":5}),
        ),
        tool(
            38,
            "graph_mail_ticket_draft_disposition_list",
            json!({"consumer_id":"consumer-1","limit":5}),
        ),
        tool(
            39,
            "graph_mail_ticket_draft_disposition_ack",
            json!({"observation_id":"missing","consumer_id":"consumer-1","reconciliation_ref":"event-1","reconciliation_receipt":{"status":"admitted"}}),
        ),
        tool(
            40,
            "graph_mail_draft_update",
            json!({"draft_id":"draft-1","subject":"Updated"}),
        ),
        tool(
            41,
            "graph_mail_draft_discard",
            json!({"draft_id":"draft-1"}),
        ),
        tool(
            42,
            "graph_mail_draft_send",
            json!({"draft_id":"draft-1","confirm_send":true,"approval_token":"send-ok"}),
        ),
        tool(
            43,
            "graph_mail_output_show",
            json!({"ref":"mcp_output:graph-output","offset":0,"limit":5}),
        ),
    ];
    let results = run(&root, &calls);
    let error_ids = (10..=43)
        .filter(|id| response(&results, *id).get("error").is_some())
        .collect::<Vec<_>>();
    assert_eq!(error_ids, vec![13, 41]);
    for id in 10..=43 {
        let result = response(&results, id);
        assert!(
            result.pointer("/result/structuredContent").is_some()
                || result.pointer("/error/data/schema").is_some(),
            "tool call {id} lacked a structured result or typed refusal: {result}"
        );
    }
    assert_eq!(structured(response(&results, 12))["status"], "refused");
    assert_eq!(structured(response(&results, 43))["status"], "ok");
    assert_eq!(
        fs::read(root.join("attachments/download.txt")).expect("download"),
        b"Hello"
    );

    let replay = run(
        &root,
        &[
            tool(
                50,
                "graph_mail_ticket_draft_upsert",
                json!({"ticket_id":"ticket-1","effect_claim_id":"claim-1","draft_operation_key":"operation-1","draft_request_digest":digest,"draft_source_id":"source-1","mailbox_id":"me","source_message_id":"message-1","reply_mode":"reply","body_text":"ticket reply","idempotency_key":"ticket-upsert-1"}),
            ),
            tool(
                51,
                "graph_mail_ticket_draft_discard",
                json!({"ticket_id":"ticket-1","effect_claim_id":"claim-1","draft_operation_key":"operation-1","mailbox_id":"me","draft_id":"draft-1","idempotency_key":"ticket-discard-1","confirm_discard":true}),
            ),
        ],
    );
    assert_eq!(structured(response(&replay, 50))["outcome"], "completed");
    assert_eq!(
        structured(response(&replay, 51))["idempotency_replayed_or_recovered"],
        true
    );

    let invalid_root = root.join("invalid-config");
    fs::create_dir_all(invalid_root.join(".ai")).expect("invalid config root");
    fs::write(invalid_root.join(".ai/graph-mail-mcp.json"), serde_json::to_vec(&json!({"graph_base_url":base_url,"allowed_mailboxes":(0..33).map(|index| format!("mailbox-{index}")).collect::<Vec<_>>() })).unwrap()).unwrap();
    let invalid_config = run(&invalid_root, &[tool(52, "graph_mail_query", json!({}))]);
    assert_eq!(
        response(&invalid_config, 52)["error"]["data"]["reason"],
        "graph_allowed_mailboxes_invalid"
    );

    let timeout_root = root.join("timeout");
    let (slow_url, slow_server) = slow_graph();
    setup(&timeout_root, &slow_url);
    let config_path = timeout_root.join(".ai/graph-mail-mcp.json");
    let mut config: Value = serde_json::from_slice(&fs::read(&config_path).unwrap()).unwrap();
    config["request_timeout_ms"] = json!(100);
    fs::write(&config_path, serde_json::to_vec(&config).unwrap()).unwrap();
    let started = Instant::now();
    let timed_out = run(&timeout_root, &[tool(53, "graph_mail_query", json!({}))]);
    assert!(started.elapsed() < Duration::from_secs(2));
    assert_eq!(
        response(&timed_out, 53)["error"]["data"]["reason"],
        "graph_request_failed"
    );
    slow_server.join().expect("slow Graph join");

    stop.store(true, Ordering::SeqCst);
    let requests = server.join().expect("mock Graph join");
    assert!(!requests.is_empty());
    let wire = requests.join("\n");
    assert!(wire.contains("Authorization: Bearer stdio-secret-token"));
    fs::remove_dir_all(root).expect("cleanup");
}
