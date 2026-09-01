use narada_local_filesystem_mcp::EXPECTED_TOOLS;
use serde_json::{json, Value};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

fn temp_root() -> PathBuf {
    let suffix = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let path =
        std::env::temp_dir().join(format!("narada-local-filesystem-rust-equivalence-{suffix}"));
    fs::create_dir_all(&path).unwrap();
    path
}

fn exchange(root: &Path, mode: &str, requests: &[Value]) -> Vec<Value> {
    let executable = env!("CARGO_BIN_EXE_narada-local-filesystem-mcp");
    let mut child = Command::new(executable)
        .args([
            "--mode",
            mode,
            "--allowed-root",
            root.to_str().unwrap(),
            "--output-root",
            root.to_str().unwrap(),
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    for request in requests {
        writeln!(
            child.stdin.as_mut().unwrap(),
            "{}",
            serde_json::to_string(request).unwrap()
        )
        .unwrap();
    }
    drop(child.stdin.take());
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

fn call(root: &Path, mode: &str, id: u64, name: &str, arguments: Value) -> Value {
    exchange(root, mode, &[json!({"jsonrpc":"2.0","id":id,"method":"tools/call","params":{"name":name,"arguments":arguments}})]).pop().unwrap()
}

fn structured(response: &Value) -> &Value {
    response
        .pointer("/result/structuredContent")
        .or_else(|| response.pointer("/error/data"))
        .unwrap()
}

#[test]
fn rust_filesystem_preserves_public_protocol_and_safety_contract() {
    let root = temp_root();
    fs::write(root.join("sample.txt"), "alpha\nbeta\ngamma\ndelta\n").unwrap();

    let init = exchange(
        &root,
        "read",
        &[
            json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05"}}),
        ],
    );
    assert_eq!(
        init[0]["result"]["serverInfo"]["name"],
        "local-filesystem-read"
    );

    let catalog = exchange(
        &root,
        "write",
        &[json!({"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}})],
    );
    let names: Vec<&str> = catalog[0]["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|tool| tool["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, EXPECTED_TOOLS);
    for tool in catalog[0]["result"]["tools"].as_array().unwrap() {
        assert_eq!(
            tool["inputSchema"]["additionalProperties"], false,
            "{}",
            tool["name"]
        );
    }

    let read = call(
        &root,
        "read",
        3,
        "fs_read_file_range",
        json!({"path":"sample.txt","start_line":2,"end_line":3}),
    );
    let delivered: Value =
        serde_json::from_str(read["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(delivered["content"], "beta\ngamma");

    let refused = call(
        &root,
        "read",
        4,
        "fs_write_file",
        json!({"path":"no.txt","content":"no"}),
    );
    assert_eq!(
        refused["error"]["data"]["code"],
        "tool_not_available_in_read_mode"
    );

    let match_all = call(
        &root,
        "read",
        5,
        "fs_grep_search",
        json!({"pattern":"^","path":"sample.txt","output_mode":"content"}),
    );
    assert_eq!(
        match_all["error"]["data"]["code"],
        "grep_match_all_single_file_refused"
    );
    assert_eq!(
        match_all["error"]["data"]["details"]["replacement"]["tool"],
        "fs_read_file_range"
    );

    let bounded = call(
        &root,
        "read",
        6,
        "fs_grep_search",
        json!({"pattern":"^","path":"sample.txt","output_mode":"content","allow_match_all":true,"max_matches":2,"max_output_chars":256}),
    );
    assert_eq!(structured(&bounded)["returned"], 2);
    assert_eq!(structured(&bounded)["max_matches"], 2);
    assert_eq!(structured(&bounded)["max_output_chars"], 256);
    assert!(
        structured(&bounded).get("matches").is_none(),
        "grep must not duplicate human and structured representations"
    );
    assert_eq!(structured(&bounded)["match_objects_authoritative"], true);
    assert!(structured(&bounded)["has_more"].as_bool().unwrap());

    let count = call(
        &root,
        "read",
        7,
        "fs_grep_search",
        json!({"pattern":"^","path":"sample.txt","output_mode":"count_matches","allow_match_all":true,"max_matches":1,"max_output_chars":256}),
    );
    assert_eq!(structured(&count)["output_mode"], "count_matches");
    assert!(structured(&count)["count"].is_number());

    fs::remove_dir_all(root).unwrap();
}
