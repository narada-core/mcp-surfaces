use serde_json::{json, Value};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

fn root() -> PathBuf {
    let suffix = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let path = std::env::temp_dir().join(format!("narada-filesystem-protocol-{suffix}"));
    fs::create_dir_all(&path).expect("root");
    path
}

fn exchange(root: &Path, mode: &str, requests: &[Value]) -> Vec<Value> {
    let executable = env!("CARGO_BIN_EXE_narada-mcp-runtime");
    let mut child = Command::new(executable)
        .args([
            "filesystem",
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
        .expect("spawn native filesystem");
    {
        let input = child.stdin.as_mut().expect("stdin");
        for request in requests {
            writeln!(input, "{}", serde_json::to_string(request).unwrap()).unwrap();
        }
    }
    drop(child.stdin.take());
    let output = child.wait_with_output().expect("wait");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).expect("JSON-RPC line"))
        .collect()
}

fn call(root: &Path, mode: &str, id: u64, name: &str, arguments: Value) -> Value {
    exchange(
        root,
        mode,
        &[json!({"jsonrpc":"2.0","id":id,"method":"tools/call","params":{"name":name,"arguments":arguments}})],
    )
    .pop()
    .unwrap()
}

fn structured(response: &Value) -> &Value {
    response
        .pointer("/result/structuredContent")
        .expect("structured result")
}

fn delivered_read(response: &Value) -> Value {
    let text = response
        .pointer("/result/content/0/text")
        .and_then(Value::as_str)
        .expect("read text content");
    serde_json::from_str(text).expect("read text content JSON")
}

#[test]
fn filesystem_public_protocol_is_complete_bounded_paged_recoverable_and_native() {
    let root = root();
    fs::write(root.join("seed.txt"), "alpha\nbeta\ngamma\n").unwrap();
    fs::write(root.join("second.txt"), "second\n").unwrap();

    let protocol = exchange(
        &root,
        "write",
        &[
            json!({"jsonrpc":"2.0","id":90,"method":"initialize","params":{"protocolVersion":"2099-01-01"}}),
            json!({"jsonrpc":"2.0","id":91,"method":"server/discover","params":{"_meta":{"io.modelcontextprotocol/protocolVersion":"2026-07-28","io.modelcontextprotocol/clientInfo":{"name":"fixture","version":"1"},"io.modelcontextprotocol/clientCapabilities":{}}}}),
            json!({"jsonrpc":"2.0","id":92,"method":"prompts/list","params":{}}),
            json!({"jsonrpc":"2.0","id":93,"method":"prompts/get","params":{"name":"local_filesystem_tool_usage"}}),
            json!({"jsonrpc":"2.0","id":94,"method":"completion/complete","params":{"argument":{"name":"path","value":""}}}),
            json!({"jsonrpc":"2.0","id":95,"method":"resources/list","params":{}}),
        ],
    );
    assert_eq!(protocol[0]["result"]["protocolVersion"], "2024-11-05");
    assert_eq!(protocol[1]["result"]["supportedVersions"][0], "2026-07-28");
    assert_eq!(
        protocol[2]["result"]["prompts"].as_array().unwrap().len(),
        1
    );
    assert!(protocol[3]
        .pointer("/result/messages/0/content/text")
        .is_some());
    assert_eq!(protocol[4]["result"]["completion"]["total"], 1);
    assert_eq!(protocol[5]["result"]["resources"], json!([]));

    let catalog = exchange(
        &root,
        "write",
        &[json!({"jsonrpc":"2.0","id":1,"method":"tools/list","params":{}})],
    );
    let tools = catalog[0]
        .pointer("/result/tools")
        .unwrap()
        .as_array()
        .unwrap();
    let names: Vec<&str> = tools
        .iter()
        .filter_map(|tool| tool["name"].as_str())
        .collect();
    assert_eq!(names.len(), 18);
    assert!(names.contains(&"fs_apply_patch"));
    for tool in tools {
        assert!(tool["inputSchema"]["title"].is_string(), "{}", tool["name"]);
        assert_eq!(
            tool["inputSchema"]["additionalProperties"], false,
            "{}",
            tool["name"]
        );
        let invalid = call(
            &root,
            "write",
            2,
            tool["name"].as_str().unwrap(),
            json!({"unexpected":true}),
        );
        assert_eq!(
            invalid.pointer("/error/data/code").and_then(Value::as_str),
            Some("tool_argument_unknown"),
            "{}",
            tool["name"]
        );
    }

    assert_eq!(
        structured(&call(&root, "write", 3, "fs_guidance", json!({})))["status"],
        "ok"
    );
    assert_eq!(
        structured(&call(&root, "write", 4, "fs_doctor", json!({})))["mode"],
        "write"
    );
    assert_eq!(
        delivered_read(&call(
            &root,
            "write",
            5,
            "fs_read_file",
            json!({"path":"seed.txt","offset":1,"limit":1})
        ))["content"],
        "alpha"
    );
    assert_eq!(
        delivered_read(&call(
            &root,
            "write",
            6,
            "fs_read_file_range",
            json!({"path":"seed.txt","start_line":2,"end_line":2})
        ))["content"],
        "beta"
    );
    assert_eq!(
        structured(&call(
            &root,
            "write",
            7,
            "fs_stat",
            json!({"path":"seed.txt"})
        ))["type"],
        "file"
    );

    let glob = call(
        &root,
        "write",
        8,
        "fs_glob_search",
        json!({"pattern":"*.txt","directory":".","limit":1,"cache_policy":"refresh"}),
    );
    assert_eq!(structured(&glob)["returned"], 1);
    assert_eq!(structured(&glob)["has_more"], true);
    let empty = call(
        &root,
        "write",
        80,
        "fs_grep_search",
        json!({"pattern":"definitely-absent","path":".","limit":2,"cache_policy":"refresh"}),
    );
    assert_eq!(structured(&empty)["count"], 0);
    let grep = call(
        &root,
        "write",
        9,
        "fs_grep_search",
        json!({"pattern":"beta","path":".","output_mode":"content","limit":1,"cache_policy":"refresh"}),
    );
    assert_eq!(structured(&grep)["returned"], 1);
    assert_eq!(
        structured(&call(
            &root,
            "write",
            10,
            "fs_repository_inventory",
            json!({"directory":".","pattern":"**/*","limit":1,"cache_policy":"refresh"})
        ))["returned"],
        1
    );
    assert_eq!(
        structured(&call(
            &root,
            "write",
            11,
            "fs_file_metrics",
            json!({"directory":".","pattern":"**/*","limit":1,"cache_policy":"refresh"})
        ))["returned"],
        1
    );

    let written = call(
        &root,
        "write",
        12,
        "fs_write_file",
        json!({"path":"work.txt","content":"one\ntwo\n"}),
    );
    assert_eq!(structured(&written)["status"], "written");
    let payload_path = root.join(".ai/tmp/mcp-payloads/workspace/write.json");
    fs::create_dir_all(payload_path.parent().unwrap()).unwrap();
    fs::write(
        &payload_path,
        r#"{"path":"payload.txt","content":"payload body\n"}"#,
    )
    .unwrap();
    let payload_write = call(
        &root,
        "write",
        81,
        "fs_write_file",
        json!({"payload_path":".ai/tmp/mcp-payloads/workspace/write.json"}),
    );
    assert_eq!(structured(&payload_write)["status"], "written");
    assert_eq!(structured(&payload_write)["payload_source"]["kind"], "file");
    let payload_ref_path = root.join(".ai/tmp/mcp-payloads/workspace/fixture/v1.json");
    fs::create_dir_all(payload_ref_path.parent().unwrap()).unwrap();
    fs::write(
        &payload_ref_path,
        r#"{"payload":{"path":"payload-ref.txt","content":"ref body\n"}}"#,
    )
    .unwrap();
    let payload_ref_write = call(
        &root,
        "write",
        82,
        "fs_write_file",
        json!({"payload_ref":"mcp_payload:fixture@v1"}),
    );
    assert_eq!(
        structured(&payload_ref_write)["payload_source"]["kind"],
        "ref"
    );
    assert_eq!(
        structured(&call(
            &root,
            "write",
            13,
            "fs_str_replace_file",
            json!({"path":"work.txt","old":"one","new":"ONE"})
        ))["status"],
        "replaced"
    );
    assert_eq!(
        structured(&call(
            &root,
            "write",
            14,
            "fs_replace_range",
            json!({"path":"work.txt","start_line":2,"end_line":2,"replacement":"TWO"})
        ))["status"],
        "replaced_range"
    );
    let patch = "*** Begin Patch\n*** Update File: work.txt\n@@\n-ONE\n+first\n TWO\n*** Add File: added.txt\n+added\n*** End Patch";
    assert_eq!(
        structured(&call(
            &root,
            "write",
            15,
            "fs_apply_patch",
            json!({"patch":patch,"operation_id":"stdio-patch"})
        ))["status"],
        "patched"
    );
    assert_eq!(
        structured(&call(
            &root,
            "write",
            16,
            "fs_apply_patch",
            json!({"patch":patch,"operation_id":"stdio-patch"})
        ))["operation_replayed"],
        true
    );
    assert_eq!(
        structured(&call(
            &root,
            "write",
            17,
            "fs_patch_outcome_show",
            json!({"operation_id":"stdio-patch"})
        ))["status"],
        "patched"
    );
    assert_eq!(
        structured(&call(
            &root,
            "write",
            18,
            "fs_move_path",
            json!({"from":"added.txt","to":"moved.txt"})
        ))["status"],
        "moved"
    );
    assert_eq!(
        structured(&call(
            &root,
            "write",
            19,
            "fs_create_directory",
            json!({"path":"empty"})
        ))["status"],
        "created"
    );
    let empty_stat = call(&root, "write", 83, "fs_stat", json!({"path":"empty"}));
    let empty_meta = structured(&empty_stat);
    assert_eq!(
        structured(&call(
            &root,
            "write",
            20,
            "fs_rename_directory",
            json!({"from":"empty","to":"renamed","expected_from":{"mtime":empty_meta["mtime"],"size":empty_meta["size"],"tree_sha256":empty_meta["tree_sha256"],"entry_count":empty_meta["entry_count"]}})
        ))["status"],
        "moved"
    );
    assert_eq!(
        structured(&call(
            &root,
            "write",
            21,
            "fs_delete_directory",
            json!({"path":"renamed"})
        ))["status"],
        "deleted"
    );
    let root_delete = call(
        &root,
        "write",
        84,
        "fs_delete_directory",
        json!({"path":root,"recursive":true}),
    );
    assert_eq!(
        root_delete
            .pointer("/error/data/code")
            .and_then(Value::as_str),
        Some("filesystem_authority_root_mutation_refused")
    );
    assert!(root.exists());

    let fresh = call(
        &root,
        "read",
        22,
        "fs_read_file",
        json!({"path":"work.txt","offset":1,"limit":10}),
    );
    assert!(structured(&fresh).get("content").is_none());
    assert_eq!(
        structured(&fresh)["content_delivery"]["duplicated_in_structured_content"],
        false
    );
    let read_text = fresh
        .pointer("/result/content/0/text")
        .and_then(Value::as_str)
        .expect("read delivery text");
    assert_eq!(read_text.matches("first\\nTWO").count(), 1);
    assert_eq!(
        call(
            &root,
            "read",
            23,
            "fs_write_file",
            json!({"path":"no.txt","content":"no"})
        )
        .pointer("/error/data/code")
        .and_then(Value::as_str),
        Some("tool_not_available_in_read_mode")
    );

    let accepted = root.join(".narada/local-filesystem-mcp/patch-outcomes/interrupted.json");
    fs::create_dir_all(accepted.parent().unwrap()).unwrap();
    fs::write(&accepted, r#"{"schema":"local.filesystem.apply_patch.outcome.v1","status":"accepted","operation_id":"interrupted","patch_sha256":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","owner_pid":4294967294}"#).unwrap();
    let recovered = call(
        &root,
        "read",
        24,
        "fs_patch_outcome_show",
        json!({"operation_id":"interrupted"}),
    );
    assert_eq!(
        structured(&recovered)["status"],
        "interrupted_before_mutation"
    );
    assert_eq!(structured(&recovered)["retry_safe"], true);

    let huge = "needleless line\n".repeat(300_000);
    fs::write(root.join("hay.txt"), huge).unwrap();
    let timeout = call(
        &root,
        "read",
        25,
        "fs_grep_search",
        json!({"pattern":"absent-needle","path":".","limit":1,"timeout_ms":1,"cache_policy":"bypass"}),
    );
    let timeout_code = timeout.pointer("/error/data/code").and_then(Value::as_str);
    assert!(
        matches!(
            timeout_code,
            Some("fs_grep_search_timed_out") | Some("search_timed_out")
        ),
        "{timeout}"
    );

    fs::remove_dir_all(root).unwrap();
}
