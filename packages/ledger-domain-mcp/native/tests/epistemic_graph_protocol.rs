use serde_json::{json, Value};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Barrier};
use std::thread;
use uuid::Uuid;

fn domain_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../shared/ledger-domain-epistemic/domain.json")
}

fn rpc(id: u64, method: &str, params: Value) -> Value {
    json!({"jsonrpc":"2.0","id":id,"method":method,"params":params})
}

fn tool(id: u64, name: &str, arguments: Value) -> Value {
    rpc(id, "tools/call", json!({"name":name,"arguments":arguments}))
}

fn run(root: &Path, requests: &[Value]) -> Vec<Value> {
    let mut child = Command::new(env!("CARGO_BIN_EXE_narada-ledger-domain"))
        .args([
            "--domain",
            &domain_path().to_string_lossy(),
            "--site-root",
            &root.to_string_lossy(),
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn ledger domain surface");
    {
        let input = child.stdin.as_mut().expect("stdin");
        for request in requests {
            writeln!(input, "{request}").expect("write request");
        }
    }
    let output = child.wait_with_output().expect("wait for ledger domain surface");
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
        .map(|line| serde_json::from_str(line).expect("JSON-RPC response"))
        .collect()
}

fn response(values: &[Value], id: u64) -> &Value {
    values
        .iter()
        .find(|value| value["id"] == id)
        .expect("response id")
}

fn structured(value: &Value) -> &Value {
    value
        .pointer("/result/structuredContent")
        .expect("structured result")
}

#[test]
fn live_sequence_create_claim_replay_and_readback() {
    let root = std::env::temp_dir().join(format!("epistemic-sequence-protocol-{}", Uuid::new_v4()));
    let authority = json!({"kind":"test","summary":"Neutral live protocol fixture."});
    let calls = run(
        &root,
        &[
            rpc(1, "tools/list", json!({})),
            tool(
                2,
                "epistemic_graph_sequence_create",
                json!({"sequence_name":"fixture","actor":"protocol-test","authority_basis":authority.clone(),"start_at":7}),
            ),
            tool(
                3,
                "epistemic_graph_sequence_claim_next",
                json!({"sequence_name":"fixture","actor":"protocol-test","authority_basis":authority.clone(),"idempotency_key":"fixture-first"}),
            ),
            tool(
                4,
                "epistemic_graph_sequence_claim_next",
                json!({"sequence_name":"fixture","actor":"protocol-test","authority_basis":authority,"idempotency_key":"fixture-first"}),
            ),
            tool(
                5,
                "epistemic_graph_sequence_status",
                json!({"sequence_name":"fixture"}),
            ),
            tool(
                6,
                "epistemic_graph_sequence_claims",
                json!({"sequence_name":"fixture","limit":10}),
            ),
        ],
    );
    let tools = response(&calls, 1)["result"]["tools"]
        .as_array()
        .expect("tools");
    for name in [
        "epistemic_graph_sequence_create",
        "epistemic_graph_sequence_status",
        "epistemic_graph_sequence_list",
        "epistemic_graph_sequence_claim_next",
        "epistemic_graph_sequence_claims",
    ] {
        assert!(tools.iter().any(|tool| tool["name"] == name), "{name}");
    }
    assert_eq!(structured(response(&calls, 2))["status"], "created");
    assert_eq!(structured(response(&calls, 3))["value"], 7);
    assert_eq!(structured(response(&calls, 4))["value"], 7);
    assert_eq!(structured(response(&calls, 4))["idempotency_replay"], true);
    assert_eq!(structured(response(&calls, 5))["next_value"], 8);
    assert_eq!(structured(response(&calls, 6))["count"], 1);
    let _ = fs::remove_dir_all(root);
}

#[test]
fn parallel_processes_claim_unique_contiguous_numbers() {
    let root =
        std::env::temp_dir().join(format!("epistemic-sequence-processes-{}", Uuid::new_v4()));
    let authority = json!({"kind":"test","summary":"Parallel process fixture."});
    let created = run(
        &root,
        &[tool(
            1,
            "epistemic_graph_sequence_create",
            json!({"sequence_name":"parallel","actor":"protocol-test","authority_basis":authority}),
        )],
    );
    assert_eq!(structured(response(&created, 1))["status"], "created");
    let root = Arc::new(root);
    let barrier = Arc::new(Barrier::new(8));
    let handles = (0..8)
        .map(|index| {
            let root = root.clone();
            let barrier = barrier.clone();
            thread::spawn(move || {
                barrier.wait();
                let calls = run(
                    &root,
                    &[tool(
                        1,
                        "epistemic_graph_sequence_claim_next",
                        json!({"sequence_name":"parallel","actor":"protocol-test","authority_basis":{"kind":"test"},"idempotency_key":format!("process-{index}")}),
                    )],
                );
                structured(response(&calls, 1))["value"].as_u64().unwrap()
            })
        })
        .collect::<Vec<_>>();
    let mut values = handles
        .into_iter()
        .map(|handle| handle.join().unwrap())
        .collect::<Vec<_>>();
    values.sort_unstable();
    assert_eq!(values, (1..=8).collect::<Vec<_>>());
    let status = run(
        &root,
        &[tool(
            1,
            "epistemic_graph_sequence_status",
            json!({"sequence_name":"parallel"}),
        )],
    );
    assert_eq!(structured(response(&status, 1))["claim_count"], 8);
    assert_eq!(
        structured(response(&status, 1))["integrity_status"],
        "valid"
    );
    let _ = fs::remove_dir_all(root.as_path());
}
