use narada_structured_command_mcp::EXPECTED_TOOLS;
use serde_json::{json, Value};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

fn temp_root() -> PathBuf {
    let suffix = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let path = std::env::temp_dir().join(format!(
        "narada-structured-command-rust-equivalence-{suffix}"
    ));
    fs::create_dir_all(&path).unwrap();
    path
}

fn exchange(root: &Path, requests: &[Value]) -> Vec<Value> {
    let executable = env!("CARGO_BIN_EXE_narada-structured-command-mcp");
    let mut child = Command::new(executable)
        .args([
            "--allowed-root",
            root.to_str().unwrap(),
            "--site-root",
            root.to_str().unwrap(),
            "--storage-root",
            root.to_str().unwrap(),
            "--allow-command",
            "node",
            "--max-timeout-ms",
            "5000",
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

fn call(root: &Path, id: u64, name: &str, arguments: Value) -> Value {
    exchange(root, &[json!({"jsonrpc":"2.0","id":id,"method":"tools/call","params":{"name":name,"arguments":arguments}})]).pop().unwrap()
}

fn structured(response: &Value) -> &Value {
    response
        .pointer("/result/structuredContent")
        .or_else(|| response.pointer("/error/data"))
        .unwrap()
}

#[test]
fn rust_structured_command_preserves_public_protocol_policy_and_persistence() {
    let root = temp_root();
    let init = exchange(
        &root,
        &[
            json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05"}}),
        ],
    );
    assert_eq!(
        init[0]["result"]["serverInfo"]["name"],
        "structured-command-mcp"
    );

    let catalog = exchange(
        &root,
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

    let policy = call(
        &root,
        3,
        "structured_command_execution_policy_inspect",
        json!({}),
    );
    assert_eq!(
        structured(&policy)["schema"],
        "narada.structured_command.execution_policy.v0"
    );
    assert!(structured(&policy)["allowed_commands"]
        .as_array()
        .unwrap()
        .iter()
        .any(|value| value == "node"));

    let refused = call(
        &root,
        4,
        "structured_command_execute",
        json!({"command":"sh","args":["-c","echo forbidden"],"working_directory":"."}),
    );
    assert_eq!(structured(&refused)["status"], "refused");
    assert_eq!(structured(&refused)["executed"], false);

    let executed = call(
        &root,
        5,
        "structured_command_execute",
        json!({"command":"node","args":["-e","process.stdout.write('abcdef')"],"working_directory":".","stdout_limit":3}),
    );
    assert_eq!(structured(&executed)["status"], "ok");
    assert_eq!(structured(&executed)["stdout"], "abc");
    assert_eq!(structured(&executed)["stdout_next_offset"], 3);
    let execution_ref = structured(&executed)["execution_ref"]
        .as_str()
        .unwrap()
        .to_owned();

    let paged = call(
        &root,
        6,
        "structured_command_execute",
        json!({"execution_ref":execution_ref,"stdout_offset":3,"stdout_limit":3}),
    );
    assert_eq!(structured(&paged)["page_source"], "persisted_execution");
    assert_eq!(structured(&paged)["stdout"], "def");
    assert_eq!(structured(&paged)["stdout_next_offset"], Value::Null);

    let input = call(
        &root,
        7,
        "structured_command_input_create",
        json!({"input_id":"equivalence_input","command":"node","args":["-e","console.log('input-ok')"],"working_directory":"."}),
    );
    assert_eq!(structured(&input)["status"], "created");
    let input_ref = structured(&input)["input_ref"].as_str().unwrap();
    let via_input = call(
        &root,
        8,
        "structured_command_execute",
        json!({"input_ref":input_ref}),
    );
    assert_eq!(structured(&via_input)["status"], "ok");
    assert!(structured(&via_input)["stdout"]
        .as_str()
        .unwrap()
        .contains("input-ok"));

    let timeout = call(
        &root,
        9,
        "structured_command_execute",
        json!({"command":"node","args":["-e","setTimeout(()=>{},1000)"],"working_directory":".","timeout_ms":10}),
    );
    assert_eq!(structured(&timeout)["status"], "timed_out");
    assert_eq!(structured(&timeout)["timed_out"], true);

    let started = call(
        &root,
        10,
        "structured_command_start",
        json!({"command":"node","args":["-e","setTimeout(()=>console.log('background-ok'),100)"],"working_directory":".","durable_process_lifetime_ms":3000}),
    );
    assert_eq!(structured(&started)["status"], "running");
    let background_ref = structured(&started)["execution_ref"]
        .as_str()
        .unwrap()
        .to_owned();
    let mut terminal = None;
    for _ in 0..30 {
        thread::sleep(Duration::from_millis(50));
        let shown = call(
            &root,
            11,
            "structured_command_execution_show",
            json!({"execution_ref":background_ref,"stdout_limit":200}),
        );
        if structured(&shown)["pending"] == false {
            terminal = Some(shown);
            break;
        }
    }
    let terminal = terminal.expect("background command should reach a terminal persisted state");
    assert_eq!(structured(&terminal)["status"], "ok");
    assert!(structured(&terminal)["stdout"]
        .as_str()
        .unwrap()
        .contains("background-ok"));

    fs::remove_dir_all(root).unwrap();
}
