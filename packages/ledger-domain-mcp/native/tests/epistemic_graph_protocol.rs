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

fn modernize(request: Value) -> Value {
    let mut request = request.as_object().cloned().expect("request object");
    let mut params = request
        .get("params")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    params.insert(
        "_meta".into(),
        json!({
            "io.modelcontextprotocol/protocolVersion":"2026-07-28",
            "io.modelcontextprotocol/clientInfo":{"name":"ledger-domain-e2e","version":"1"},
            "io.modelcontextprotocol/clientCapabilities":{}
        }),
    );
    request.insert("params".into(), Value::Object(params));
    Value::Object(request)
}

fn run_framed(root: &Path, requests: &[Value]) -> Vec<Value> {
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
        .expect("spawn framed ledger domain surface");
    {
        let input = child.stdin.as_mut().expect("stdin");
        for request in requests {
            let body = serde_json::to_vec(request).expect("encode request");
            write!(input, "Content-Length: {}\r\n\r\n", body.len()).expect("write frame header");
            input.write_all(&body).expect("write frame body");
        }
    }
    let output = child
        .wait_with_output()
        .expect("wait for framed ledger domain surface");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
            .chars()
            .take(2000)
            .collect::<String>()
    );

    let separator = b"\r\n\r\n";
    let mut remaining = output.stdout.as_slice();
    let mut values = Vec::new();
    while !remaining.is_empty() {
        let header_end = remaining
            .windows(separator.len())
            .position(|window| window == separator)
            .expect("framed response header separator");
        let header = std::str::from_utf8(&remaining[..header_end]).expect("framed response header");
        let length = header
            .lines()
            .find_map(|line| {
                let (name, value) = line.split_once(':')?;
                name.eq_ignore_ascii_case("Content-Length")
                    .then(|| value.trim().parse::<usize>().ok())
                    .flatten()
            })
            .expect("framed response content length");
        let body_start = header_end + separator.len();
        assert!(remaining.len() >= body_start + length, "truncated framed response");
        values.push(
            serde_json::from_slice(&remaining[body_start..body_start + length])
                .expect("framed JSON-RPC response"),
        );
        remaining = &remaining[body_start + length..];
    }
    values
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
            tool(
                7,
                "epistemic_graph_sequence_list",
                json!({"limit":10}),
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
    assert_eq!(structured(response(&calls, 7))["count"], 1);
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

#[test]
fn live_query_inbox_thread_cursor_and_error_envelopes() {
    let root = std::env::temp_dir().join(format!("epistemic-query-protocol-{}", Uuid::new_v4()));
    let authority = json!({"kind":"test","summary":"Live query protocol fixture."});
    let submit = |id: u64, key: &str, operation: Value| {
        tool(
            id,
            "epistemic_graph_submit_review_admit",
            json!({
                "actor":"protocol-test",
                "authority_basis":authority.clone(),
                "idempotency_key":key,
                "operations":[operation]
            }),
        )
    };
    let message = |entity_id: &str, kind: &str, sender: &str, recipient: &str, body: &str, intent: &str| {
        json!({
            "op":"entity.declare",
            "entity_id":entity_id,
            "kind":kind,
            "title":entity_id,
            "sender":sender,
            "recipient":recipient,
            "body":body,
            "intent":intent,
            "sent_at":"2026-08-20T00:00:00Z"
        })
    };
    let calls = run(
        &root,
        &[
            rpc(1, "initialize", json!({"protocolVersion":"2024-11-05"})),
            submit(
                2,
                "query-root",
                message(
                    "communication:root",
                    "narada.epistemic:communication",
                    "marici.Caroline",
                    "marici.Grothendieck",
                    "root body",
                    "result",
                ),
            ),
            submit(
                3,
                "query-reply",
                message(
                    "communication:reply",
                    "narada.epistemic:communication",
                    "marici.Benincasa",
                    "marici.Grothendieck",
                    "reply body",
                    "reply",
                ),
            ),
            submit(
                4,
                "query-outgoing",
                message(
                    "communication:outgoing",
                    "narada.epistemic:communication",
                    "marici.Grothendieck",
                    "marici.Caroline",
                    "outgoing body",
                    "result",
                ),
            ),
            submit(
                5,
                "query-legacy",
                message(
                    "communication:legacy",
                    "narada.epistemic:communication",
                    "marici.Nima",
                    "marici.Grothendieck",
                    "legacy body",
                    "reply",
                ),
            ),
            submit(
                6,
                "query-relation",
                json!({
                    "op":"relation.declare",
                    "relation_id":"relation:reply",
                    "relation_type":"replies_to",
                    "source_id":"communication:reply",
                    "target_id":"communication:root"
                }),
            ),
            tool(
                7,
                "epistemic_graph_query",
                json!({
                    "template":"epistemic:inbox",
                    "recipient":"marici.Grothendieck",
                    "include_body":false,
                    "limit":2
                }),
            ),
            tool(
                8,
                "epistemic_graph_query",
                json!({
                    "template":"epistemic:thread",
                    "root":"communication:root",
                    "max_depth":1,
                    "limit":10
                }),
            ),
            tool(
                9,
                "epistemic_graph_query",
                json!({
                    "query":{
                        "find":["?message","?sequence"],
                        "inputs":{"recipient":"marici.Grothendieck"},
                        "where":[
                            {"triple":{"subject":"?message","attribute":"narada.ledger:entity/kind","object":{"one_of":["narada.epistemic:communication"]}}},
                            {"triple":{"subject":"?message","attribute":"narada.epistemic:recipient","object":{"input":"recipient"}}},
                            {"triple":{"subject":"?message","attribute":"narada.ledger:event/sequence","object":"?sequence"}},
                            {"compare":{"op":">=","left":"?sequence","right":{"value":1}}}
                        ],
                        "order_by":[{"term":"?sequence"}],
                        "limit":10
                    }
                }),
            ),
            tool(
                10,
                "epistemic_graph_query",
                json!({"template":"epistemic:inbox"}),
            ),
            tool(
                11,
                "epistemic_graph_query",
                json!({
                    "template":"inbox",
                    "recipient":"marici.Grothendieck",
                    "since_event":2,
                    "max_datoms":500,
                    "max_results":20,
                    "timeout_ms":5000,
                    "limit":10
                }),
            ),
            tool(
                12,
                "epistemic_graph_query",
                json!({
                    "template":"inbox",
                    "recipient":"marici.Grothendieck",
                    "direction":"outgoing",
                    "limit":10
                }),
            ),
            tool(
                13,
                "epistemic_graph_query_batch",
                json!({
                    "limit_per_query":1,
                    "queries":[
                        {"text":"root body"},
                        {"query":{
                            "find":[{"pull":{"var":"?relation","fields":["relation_id","relation_type","source_id","target_id"]}}],
                            "where":[{"triple":{"subject":"?relation","attribute":"narada.ledger:relation/id","object":"?relation"}}],
                            "order_by":[{"term":"?relation"}],
                            "limit":1
                        }}
                    ]
                }),
            ),
        ],
    );

    assert!(response(&calls, 2)["error"].is_null());
    let inbox = structured(response(&calls, 7));
    assert_eq!(inbox["query_mode"], "datalog");
    assert_eq!(inbox["template"], "epistemic:inbox");
    assert_eq!(inbox["count"], 2);
    assert_eq!(inbox["has_more"], true);
    let inbox_items = inbox["items"].as_array().expect("inbox items");
    assert!(inbox_items.iter().all(|item| item.get("body").is_none()));
    assert_eq!(
        inbox_items
            .iter()
            .find(|item| item["entity_id"] == "communication:root")
            .expect("root inbox item")["reply_state"]["has_replies"],
        true
    );
    let cursor = inbox["next_cursor"].clone();
    assert!(cursor.as_str().is_some());

    let thread = structured(response(&calls, 8));
    assert_eq!(thread["count"], 1);
    assert_eq!(thread["items"][0]["entity_id"], "communication:reply");
    assert_eq!(thread["items"][0]["payload"]["body"], "reply body");
    assert_eq!(thread["items"][0]["reply_state"]["has_replies"], false);

    let datalog = structured(response(&calls, 9));
    assert_eq!(datalog["query_mode"], "datalog");
    assert_eq!(datalog["count"], 3);
    assert!(datalog["items"].as_array().unwrap().iter().all(|item| {
        item.get("message").and_then(Value::as_str).is_some()
            && item.get("sequence").and_then(Value::as_u64).is_some()
    }));
    assert_eq!(
        response(&calls, 10)["error"]["data"]["code"],
        "query_recipient_missing"
    );
    assert_eq!(response(&calls, 10)["error"]["code"], -32000);
    assert_eq!(
        response(&calls, 10)["error"]["message"],
        "inbox requires participant (or legacy recipient)"
    );
    assert!(
        response(&calls, 11).pointer("/result/structuredContent").is_some(),
        "budgeted suffix query failed: {:?}",
        response(&calls, 11)
    );
    let legacy = structured(response(&calls, 11));
    assert_eq!(legacy["count"], 1);
    assert_eq!(legacy["query_cost"]["max_datoms"], 500);
    assert_eq!(legacy["query_cost"]["max_results"], 20);
    assert_eq!(legacy["query_cost"]["timeout_ms"], 5000);
    assert!(legacy["query_cost"]["datoms_loaded"].as_u64().unwrap() <= 500);
    assert!(legacy["query_cost"]["hard_caps"]["max_datoms"].as_u64().unwrap() >= 500);
    assert!(legacy["max_output_bytes"].as_u64().is_some());
    assert_eq!(
        legacy["output_bytes"],
        serde_json::to_vec(legacy).expect("legacy response serializes").len() as u64
    );
    assert_eq!(
        legacy["items"][0]["entity_id"],
        "communication:legacy"
    );
    assert_eq!(structured(response(&calls, 12))["count"], 1);
    assert_eq!(
        structured(response(&calls, 12))["items"][0]["entity_id"],
        "communication:outgoing"
    );
    let batch = structured(response(&calls, 13));
    assert_eq!(batch["schema"], "narada.epistemic.query_batch.v2");
    assert_eq!(batch["results"].as_array().map(Vec::len), Some(2));
    assert!(batch["results"][0].get("result").is_none());
    assert_eq!(batch["results"][1]["items"][0]["relation_type"], "replies_to");
    assert_eq!(batch["results"][0]["request"]["mode"], "legacy");
    assert_eq!(batch["results"][1]["request"]["mode"], "raw");
    let budget_controls = run(
        &root,
        &[
            tool(1, "epistemic_graph_query", json!({
                "template":"inbox",
                "recipient":"marici.Grothendieck",
                "max_datoms":u64::MAX,
                "max_results":u64::MAX,
                "timeout_ms":u64::MAX,
                "limit":1
            })),
            tool(2, "epistemic_graph_query", json!({
                "template":"inbox",
                "recipient":"marici.Grothendieck",
                "max_datoms":0
            })),
            tool(3, "epistemic_graph_query", json!({
                "template":"inbox",
                "recipient":"marici.Grothendieck",
                "budget_escalation":{"role":"maintenance","evidence":"caller-authored"}
            })),
        ],
    );
    assert!(
        response(&budget_controls, 1)
            .pointer("/result/structuredContent")
            .is_some(),
        "capped query failed: {:?}",
        response(&budget_controls, 1)
    );
    let capped = structured(response(&budget_controls, 1));
    assert_eq!(
        capped["query_cost"]["max_datoms"],
        capped["query_cost"]["hard_caps"]["max_datoms"]
    );
    assert_eq!(
        capped["query_cost"]["max_results"],
        capped["query_cost"]["hard_caps"]["max_results"]
    );
    assert_eq!(
        capped["query_cost"]["timeout_ms"],
        capped["query_cost"]["hard_caps"]["timeout_ms"]
    );
    assert_eq!(
        response(&budget_controls, 2)["error"]["data"]["code"],
        "input_schema_validation_failed"
    );
    assert_eq!(
        response(&budget_controls, 3)["error"]["data"]["code"],
        "query_budget_escalation_unavailable"
    );


    let page_two = run(
        &root,
        &[tool(
            1,
            "epistemic_graph_query",
            json!({
                "template":"inbox",
                "recipient":"marici.Grothendieck",
                "limit":2,
                "cursor":cursor.clone()
            }),
        )],
    );
    assert!(
        response(&page_two, 1).pointer("/result/structuredContent").is_some(),
        "page two query failed: {page_two:?}"
    );
    let page_two_result = structured(response(&page_two, 1));
    assert_eq!(page_two_result["count"], 1);
    assert_eq!(page_two_result["has_more"], false);
    assert_eq!(page_two_result["items"][0]["entity_id"], "communication:legacy");

    let scope_mismatch = run(
        &root,
        &[tool(
            1,
            "epistemic_graph_query",
            json!({
                "template":"inbox",
                "recipient":"marici.Grothendieck",
                "reply_state":"replied",
                "limit":2,
                "cursor":cursor.clone()
            }),
        )],
    );
    assert_eq!(
        response(&scope_mismatch, 1)["error"]["data"]["code"],
        "query_cursor_scope_mismatch"
    );

    let state = run(
        &root,
        &[
            tool(
                1,
                "epistemic_graph_message_mark_read",
                json!({
                    "message_id":"communication:root",
                    "reader":"marici.Grothendieck",
                    "actor":"protocol-test",
                    "authority_basis":authority.clone()
                }),
            ),
            tool(
                2,
                "epistemic_graph_message_mark_read",
                json!({
                    "message_id":"communication:root",
                    "reader":"marici.Grothendieck",
                    "actor":"protocol-test",
                    "authority_basis":authority.clone()
                }),
            ),
            tool(
                3,
                "epistemic_graph_query",
                json!({
                    "template":"inbox",
                    "recipient":"marici.Grothendieck",
                    "read_state":"read",
                    "include_body":true,
                    "limit":10
                }),
            ),
            tool(
                4,
                "epistemic_graph_query",
                json!({
                    "template":"inbox",
                    "recipient":"marici.Grothendieck",
                    "match":{"sender":"marici.Nima","read_state":"unread","reply_state":"unreplied"},
                    "limit":10
                }),
            ),
            tool(
                5,
                "epistemic_graph_query",
                json!({
                    "template":"inbox",
                    "recipient":"marici.Grothendieck",
                    "reply_state":"replied",
                    "limit":10
                }),
            ),
        ],
    );
    assert!(response(&state, 1)["error"].is_null());
    assert_eq!(structured(response(&state, 2))["admission"]["status"], "already_admitted");
    let read_items = structured(response(&state, 3))["items"].as_array().unwrap();
    assert_eq!(read_items.len(), 1);
    assert_eq!(read_items[0]["entity_id"], "communication:root");
    assert_eq!(read_items[0]["message_state"]["status"], "read");
    assert_eq!(read_items[0]["message_state"]["unread"], false);
    assert_eq!(structured(response(&state, 4))["count"], 1);
    assert_eq!(structured(response(&state, 4))["items"][0]["entity_id"], "communication:legacy");
    assert_eq!(structured(response(&state, 5))["count"], 1);

    let appended = run(
        &root,
        &[submit(
            1,
            "query-appended",
            message(
                "communication:appended",
                "narada.epistemic:communication",
                "marici.Benincasa",
                "marici.Grothendieck",
                "appended body",
                "result",
            ),
        )],
    );
    assert!(response(&appended, 1)["error"].is_null());
    let stale = run(
        &root,
        &[tool(
            1,
            "epistemic_graph_query",
            json!({
                "template":"inbox",
                "recipient":"marici.Grothendieck",
                "limit":2,
                "cursor":cursor
            }),
        )],
    );
    assert_eq!(
        response(&stale, 1)["error"]["data"]["code"],
        "query_cursor_stale"
    );
    let _ = fs::remove_dir_all(root);
}

#[test]
fn live_inbox_suffix_filters_old_recipient_history_before_budgeting() {
    let root = std::env::temp_dir().join(format!("epistemic-query-suffix-{}", Uuid::new_v4()));
    let old_messages = (0..100)
        .map(|index| {
            json!({
                "op":"entity.declare",
                "entity_id":format!("communication:old-{index}"),
                "kind":"narada.epistemic:communication",
                "title":format!("old-{index}"),
                "sender":"marici.Caroline",
                "recipient":"marici.Nima",
                "body":"old body",
                "intent":"result",
                "sent_at":"2026-08-20T00:00:00Z"
            })
        })
        .collect::<Vec<_>>();
    let calls = run(
        &root,
        &[
            tool(
                1,
                "epistemic_graph_submit_review_admit",
                json!({
                    "actor":"protocol-test",
                    "authority_basis":{"kind":"test","summary":"Old recipient history."},
                    "idempotency_key":"suffix-old-history",
                    "operations":old_messages
                }),
            ),
            tool(
                2,
                "epistemic_graph_submit_review_admit",
                json!({
                    "actor":"protocol-test",
                    "authority_basis":{"kind":"test","summary":"New recipient message."},
                    "idempotency_key":"suffix-new-message",
                    "operations":[{
                        "op":"entity.declare",
                        "entity_id":"communication:new",
                        "kind":"narada.epistemic:communication",
                        "title":"new",
                        "sender":"marici.Caroline",
                        "recipient":"marici.Nima",
                        "body":"new body",
                        "intent":"result",
                        "sent_at":"2026-08-21T00:00:00Z"
                    }]
                }),
            ),
            tool(
                3,
                "epistemic_graph_query",
                json!({
                    "template":"inbox",
                    "participant":"marici.Nima",
                    "viewer":"marici.Nima",
                    "after_sequence":1,
                    "read_state":"unread",
                    "include_body":true,
                    "max_datoms":500,
                    "max_results":10,
                    "timeout_ms":5000,
                    "limit":10
                }),
            ),
            tool(
                4,
                "epistemic_graph_query",
                json!({
                    "template":"inbox",
                    "match":{
                        "participant":"marici.Nima",
                        "viewer":"marici.Nima",
                        "after_sequence":1,
                        "read_state":"unread",
                        "include_body":true
                    },
                    "max_datoms":500,
                    "max_results":10,
                    "timeout_ms":5000,
                    "limit":10
                }),
            ),
        ],
    );
    assert!(
        response(&calls, 3).pointer("/result/structuredContent").is_some(),
        "indexed suffix query failed: {:?}",
        response(&calls, 3)
    );
    let result = structured(response(&calls, 3));
    assert_eq!(result["count"], 1);
    assert_eq!(result["items"][0]["entity_id"], "communication:new");
    assert_eq!(result["query_cost"]["planner_mode"], "indexed_subject_suffix");
    assert!(result["query_cost"]["datoms_loaded"].as_u64().unwrap() <= 500);
    let nested = structured(response(&calls, 4));
    assert_eq!(nested["count"], 1);
    assert_eq!(nested["items"][0]["entity_id"], "communication:new");
    assert_eq!(nested["query_cost"]["planner_mode"], "indexed_subject_suffix");
    assert!(nested["query_cost"]["datoms_loaded"].as_u64().unwrap() <= 500);
    let _ = fs::remove_dir_all(root.as_path());
}

#[test]
fn live_inbox_without_sequence_bound_keeps_decoration_subject_local() {
    let root = std::env::temp_dir().join(format!("epistemic-query-all-history-{}", Uuid::new_v4()));
    let mut messages = (0..100)
        .map(|index| {
            json!({
                "op":"entity.declare",
                "entity_id":format!("communication:unrelated-{index}"),
                "kind":"narada.epistemic:communication",
                "title":format!("unrelated-{index}"),
                "sender":"marici.Caroline",
                "recipient":"marici.SomeoneElse",
                "body":"unrelated body",
                "intent":"result",
                "sent_at":"2026-08-20T00:00:00Z"
            })
        })
        .collect::<Vec<_>>();
    messages.push(json!({
        "op":"entity.declare",
        "entity_id":"communication:target",
        "kind":"narada.epistemic:communication",
        "title":"target",
        "sender":"marici.Benincasa",
        "recipient":"marici.Nima",
        "body":"target body",
        "intent":"result",
        "sent_at":"2026-08-21T00:00:00Z"
    }));
    let calls = run(
        &root,
        &[
            tool(
                1,
                "epistemic_graph_submit_review_admit",
                json!({
                    "actor":"protocol-test",
                    "authority_basis":{"kind":"test","summary":"All-history recipient query fixture."},
                    "idempotency_key":"all-history-recipient-query",
                    "operations":messages
                }),
            ),
            tool(
                2,
                "epistemic_graph_query",
                json!({
                    "template":"inbox",
                    "participant":"marici.Nima",
                    "viewer":"marici.Nima",
                    "read_state":"all",
                    "reply_state":"all",
                    "include_body":true,
                    "max_datoms":100,
                    "max_results":10,
                    "timeout_ms":5000,
                    "limit":10
                }),
            ),
        ],
    );
    assert!(
        response(&calls, 2).pointer("/result/structuredContent").is_some(),
        "all-history indexed inbox query failed: {:?}",
        response(&calls, 2)
    );
    let result = structured(response(&calls, 2));
    assert_eq!(result["count"], 1);
    assert_eq!(result["items"][0]["entity_id"], "communication:target");
    assert_eq!(result["query_cost"]["planner_mode"], "indexed_subject_suffix");
    assert_eq!(result["query_cost"]["subject_local_attribute"], "narada.ledger:event/sequence");
    assert!(result["query_cost"]["datoms_loaded"].as_u64().unwrap() <= 100);
    let _ = fs::remove_dir_all(root.as_path());
}

#[test]
fn live_query_modes_aliases_and_message_receipt_boundaries() {
    let root = std::env::temp_dir().join(format!("epistemic-query-boundaries-{}", Uuid::new_v4()));
    let authority = json!({"kind":"test","summary":"Query boundary fixture."});
    let submit = |id: u64, key: &str, operations: Value| {
        tool(
            id,
            "epistemic_graph_submit_review_admit",
            json!({
                "actor":"protocol-test",
                "authority_basis":authority.clone(),
                "idempotency_key":key,
                "operations":operations
            }),
        )
    };
    let message = |entity_id: &str, kind: &str, sender: &str, recipient: &str| {
        json!({
            "op":"entity.declare",
            "entity_id":entity_id,
            "kind":kind,
            "title":entity_id,
            "sender":sender,
            "recipient":recipient,
            "body":format!("body for {entity_id}"),
            "intent":"result",
            "sent_at":"2026-08-20T00:00:00Z"
        })
    };
    let mut owned_message = message(
        "communication:one",
        "narada.epistemic:communication",
        "marici.Nima",
        "marici.Grothendieck",
    );
    owned_message["message_state"] = json!("domain-owned");
    owned_message["reply_state"] = json!("domain-owned");
    let seed = run(
        &root,
        &[
            submit(
                1,
                "query-boundary-claim",
                json!([{"op":"entity.declare","entity_id":"claim:one","kind":"claim","title":"Boundary claim"}]),
            ),
            submit(2, "query-boundary-one", json!([owned_message])),
            submit(
                3,
                "query-boundary-two",
                json!([message(
                    "communication:two",
                    "narada.epistemic:communication",
                    "marici.Benincasa",
                    "marici.Grothendieck",
                )]),
            ),
            submit(
                4,
                "query-boundary-outgoing",
                json!([message(
                    "communication:outgoing",
                    "narada.epistemic:communication",
                    "marici.Grothendieck",
                    "marici.Caroline",
                )]),
            ),
        ],
    );
    for id in 1..=4 {
        assert!(response(&seed, id)["error"].is_null(), "seed request {id}: {seed:?}");
    }

    let errors = run(
        &root,
        &[
            tool(1, "epistemic_graph_query", json!({"participant":"marici.Grothendieck"})),
            tool(
                2,
                "epistemic_graph_query",
                json!({"query":{"find":["?message"],"where":[{"triple":{"subject":"?message","attribute":"narada.ledger:entity/kind","object":"communication"}}]},"template":"inbox"}),
            ),
            tool(
                3,
                "epistemic_graph_query",
                json!({"query":{"find":["?message"],"where":[{"triple":{"subject":"?message","attribute":"narada.ledger:entity/kind","object":"communication"}}]},"kind":"communication"}),
            ),
            tool(
                4,
                "epistemic_graph_query",
                json!({"query":{"find":["?message"],"where":[{"triple":{"subject":"?message","attribute":"narada.ledger:entity/kind","object":"communication"}}],"limit":1},"limit":2}),
            ),
            tool(
                5,
                "epistemic_graph_query",
                json!({"template":"inbox","participant":"marici.Grothendieck","sender":"marici.Nima","from":"marici.Benincasa"}),
            ),
            tool(
                6,
                "epistemic_graph_query",
                json!({"kind":"claim","cursor":"v1.deadbeef"}),
            ),
        ],
    );
    assert_eq!(response(&errors, 1)["error"]["data"]["code"], "query_template_missing");
    assert_eq!(response(&errors, 2)["error"]["data"]["code"], "query_mode_ambiguous");
    assert_eq!(response(&errors, 3)["error"]["data"]["code"], "query_mode_mixed");
    assert_eq!(response(&errors, 4)["error"]["data"]["code"], "query_override_conflict");
    assert_eq!(response(&errors, 5)["error"]["data"]["code"], "query_sender_conflict");
    assert_eq!(response(&errors, 6)["error"]["data"]["code"], "query_cursor_unsupported");

    let named = run(
        &root,
        &[
            tool(
                1,
                "epistemic_graph_query",
                json!({"template":"inbox","participant":"marici.Grothendieck","kinds":["marici:communication"],"limit":10}),
            ),
            tool(
                2,
                "epistemic_graph_query",
                json!({"template":"inbox","participant":"marici.Grothendieck","direction":"outgoing","to":"marici.Caroline","kinds":["marici:communication"],"limit":10}),
            ),
            tool(3, "epistemic_graph_query", json!({"kind":"communication","limit":10})),
            tool(4, "epistemic_graph_query", json!({"kind":"narada.epistemic:communication","limit":10})),
            tool(
                5,
                "epistemic_graph_query",
                json!({"query":{"find":["?message"],"where":[{"triple":{"subject":"?message","attribute":"narada.ledger:entity/kind","object":"communication"}}],"limit":10}}),
            ),
        ],
    );
    assert_eq!(structured(response(&named, 1))["query_origin"], "named_template");
    assert_eq!(structured(response(&named, 2))["query_origin"], "named_template");
    assert_eq!(structured(response(&named, 1))["count"], 2);
    assert_eq!(structured(response(&named, 2))["count"], 1);
    assert_eq!(structured(response(&named, 2))["items"][0]["entity_id"], "communication:outgoing");
    assert_eq!(structured(response(&named, 3))["returned"], 3);
    assert_eq!(structured(response(&named, 4))["returned"], 3);
    assert_eq!(structured(response(&named, 5))["query_origin"], "raw");

    let head = structured(response(&named, 3))["ledger_head"].clone();
    let pinned = run(
        &root,
        &[
            tool(1, "epistemic_graph_query", json!({"kind":"claim","expected_ledger_head":head})),
            tool(2, "epistemic_graph_query", json!({"kind":"claim","expected_ledger_head":"wrong-head"})),
        ],
    );
    assert!(response(&pinned, 1)["error"].is_null());
    assert_eq!(response(&pinned, 2)["error"]["data"]["code"], "ledger_head_mismatch");

    let reads = run(
        &root,
        &[
            tool(
                1,
                "epistemic_graph_message_mark_read",
                json!({"message_id":"communication:one","reader":"marici.Grothendieck","actor":"protocol-test","authority_basis":authority.clone(),"read_at":"2026-08-20T01:00:00Z","idempotency_key":"query-boundary-read"}),
            ),
            tool(
                2,
                "epistemic_graph_message_mark_read",
                json!({"message_id":"communication:one","reader":"marici.Grothendieck","actor":"protocol-test","authority_basis":authority.clone(),"read_at":"2026-08-20T02:00:00Z","idempotency_key":"query-boundary-read-retry"}),
            ),
            tool(
                3,
                "epistemic_graph_message_mark_read",
                json!({"message_id":"claim:one","reader":"marici.Grothendieck","actor":"protocol-test","authority_basis":authority.clone()}),
            ),
            tool(
                4,
                "epistemic_graph_message_mark_read",
                json!({"message_id":"communication:one","reader":"marici.Other","actor":"protocol-test","authority_basis":authority.clone()}),
            ),
            tool(5, "epistemic_graph_query", json!({"limit":20})),
            tool(6, "epistemic_graph_query", json!({"kind":"narada.epistemic:message_read","limit":20})),
            tool(7, "epistemic_graph_status", json!({})),
            tool(8, "epistemic_graph_snapshot", json!({"limit":20})),
        ],
    );
    assert!(response(&reads, 1)["error"].is_null());
    assert_eq!(structured(response(&reads, 2))["replayed"], true);
    assert_eq!(structured(response(&reads, 2))["read_at"], "2026-08-20T01:00:00Z");
    assert_eq!(response(&reads, 3)["error"]["data"]["code"], "message_kind_invalid");
    assert_eq!(response(&reads, 4)["error"]["data"]["code"], "message_reader_not_participant");
    let visible_items = structured(response(&reads, 5))["items"].as_array().unwrap();
    assert!(visible_items.iter().all(|item| item["kind"] != "narada.epistemic:message_read"));
    assert_eq!(structured(response(&reads, 6))["returned"], 1);
    assert_eq!(structured(response(&reads, 7))["entity_count"], 4);
    assert_eq!(structured(response(&reads, 7))["stored_entity_count"], 5);
    assert_eq!(structured(response(&reads, 7))["internal_entity_count"], 1);
    assert_eq!(structured(response(&reads, 8))["entity_count"], 4);

    let pulls = run(
        &root,
        &[tool(
            1,
            "epistemic_graph_query",
            json!({
                "query":{
                    "find":[{"pull":{"var":"?message","fields":["entity_id","message_state","reply_state","payload"]}}],
                    "inputs":{"viewer":"marici.Grothendieck"},
                    "where":[
                        {"triple":{"subject":"?message","attribute":"narada.ledger:entity/kind","object":{"one_of":["narada.epistemic:communication"]}}},
                        {"triple":{"subject":"?message","attribute":"narada.epistemic:recipient","object":"marici.Grothendieck"}}
                    ],
                    "order_by":[{"term":"?message"}],
                    "limit":10
                }
            }),
        )],
    );
    let single = structured(response(&pulls, 1));
    let owned = single["items"].as_array().unwrap().iter().find(|item| item["entity_id"] == "communication:one").expect("owned message pull");
    assert_eq!(owned["message_state"], "domain-owned");
    assert_eq!(owned["reply_state"], "domain-owned");
    assert_eq!(owned["_narada_query"]["message_state"]["status"], "read");

    let multi = run(
        &root,
        &[tool(
            1,
            "epistemic_graph_query",
            json!({
                "query":{
                    "find":["?message","?sequence",{"pull":{"var":"?message","fields":["entity_id","payload"]}}],
                    "inputs":{"viewer":"marici.Grothendieck"},
                    "where":[
                        {"triple":{"subject":"?message","attribute":"narada.ledger:entity/kind","object":{"one_of":["narada.epistemic:communication"]}}},
                        {"triple":{"subject":"?message","attribute":"narada.epistemic:recipient","object":"marici.Grothendieck"}},
                        {"triple":{"subject":"?message","attribute":"narada.ledger:event/sequence","object":"?sequence"}}
                    ],
                    "order_by":[{"term":"?sequence"}],
                    "limit":10
                }
            }),
        )],
    );
    let multi_items = structured(response(&multi, 1))["items"].as_array().unwrap();
    assert!(multi_items.iter().all(|item| item["message"]["_narada_query"]["message_state"].is_object()));
    let _ = fs::remove_dir_all(root);
}

#[test]
fn live_projection_rebuild_is_serialized_across_processes() {
    let root = std::env::temp_dir().join(format!("epistemic-projection-processes-{}", Uuid::new_v4()));
    let authority = json!({"kind":"test","summary":"Concurrent projection fixture."});
    let seed = run(
        &root,
        &[tool(
            1,
            "epistemic_graph_submit_review_admit",
            json!({
                "actor":"protocol-test",
                "authority_basis":authority.clone(),
                "idempotency_key":"projection-seed",
                "operations":[{"op":"entity.declare","entity_id":"claim:seed","kind":"claim","title":"Projection seed"}]
            }),
        )],
    );
    assert!(response(&seed, 1)["error"].is_null());
    let projection = root.join(".narada/.ai/epistemic-graph/projection.sqlite");
    fs::remove_file(&projection).expect("remove projection to force concurrent rebuild");

    let root = Arc::new(root);
    let barrier = Arc::new(Barrier::new(6));
    let handles = (0..6)
        .map(|index| {
            let root = root.clone();
            let barrier = barrier.clone();
            let authority = authority.clone();
            thread::spawn(move || {
                barrier.wait();
                let calls = if index == 0 {
                    run(
                        &root,
                        &[tool(
                            1,
                            "epistemic_graph_submit_review_admit",
                            json!({
                                "actor":"protocol-test",
                                "authority_basis":authority,
                                "idempotency_key":"projection-writer",
                                "operations":[{"op":"entity.declare","entity_id":"claim:writer","kind":"claim","title":"Concurrent writer"}]
                            }),
                        )],
                    )
                } else {
                    run(
                        &root,
                        &[tool(
                            1,
                            "epistemic_graph_query",
                            json!({"kind":"claim","limit":10}),
                        )],
                    )
                };
                assert!(response(&calls, 1)["error"].is_null(), "{calls:?}");
            })
        })
        .collect::<Vec<_>>();
    for handle in handles {
        handle.join().expect("projection process");
    }

    let status = run(&root, &[tool(1, "epistemic_graph_status", json!({}))]);
    assert!(response(&status, 1)["error"].is_null());
    assert_eq!(structured(response(&status, 1))["entity_count"], 2);
    assert!(projection.exists());
    let runtime = root.join(".narada/.ai/epistemic-graph");
    for entry in fs::read_dir(&runtime).expect("read projection runtime") {
        let name = entry.expect("projection runtime entry").file_name();
        assert!(!name.to_string_lossy().contains(".next-"), "left scratch projection: {name:?}");
    }
    let _ = fs::remove_dir_all(root.as_ref());
}

#[test]
fn live_read_refuses_tampered_earlier_event_when_terminal_head_is_unchanged() {
    let root = std::env::temp_dir().join(format!("epistemic-tamper-protocol-{}", Uuid::new_v4()));
    let authority = json!({"kind":"test","summary":"Tamper protocol fixture."});
    for (id, key, entity_id) in [
        (1_u64, "tamper-first", "claim:first"),
        (2_u64, "tamper-second", "claim:second"),
    ] {
        let calls = run(
            &root,
            &[tool(
                id,
                "epistemic_graph_submit_review_admit",
                json!({
                    "actor":"protocol-test",
                    "authority_basis":authority.clone(),
                    "idempotency_key":key,
                    "operations":[{"op":"entity.declare","entity_id":entity_id,"kind":"claim","title":entity_id}]
                }),
            )],
        );
        assert!(response(&calls, id)["error"].is_null());
    }
    let ledger = root.join(".narada/epistemic/ledger");
    let mut events = fs::read_dir(&ledger)
        .expect("ledger")
        .map(|entry| entry.expect("ledger entry").path())
        .collect::<Vec<_>>();
    events.sort();
    assert!(events.len() >= 2);
    let first = &events[0];
    let mut event: Value = serde_json::from_slice(&fs::read(first).expect("read first event"))
        .expect("first event JSON");
    event["actor"] = json!("tampered");
    fs::write(first, serde_json::to_vec_pretty(&event).expect("encode tampered event"))
        .expect("write tampered event");

    let status = run(&root, &[tool(1, "epistemic_graph_status", json!({}))]);
    assert_eq!(
        response(&status, 1)["error"]["data"]["code"],
        "ledger_hash_invalid"
    );
    let _ = fs::remove_dir_all(root);
}

#[test]
fn live_modern_content_length_transport_exposes_status() {
    let root = std::env::temp_dir().join(format!("epistemic-modern-protocol-{}", Uuid::new_v4()));
    let calls = run_framed(
        &root,
        &[
            modernize(rpc(1, "server/discover", json!({}))),
            modernize(rpc(2, "tools/list", json!({}))),
            modernize(tool(3, "epistemic_graph_status", json!({}))),
        ],
    );
    assert_eq!(response(&calls, 1)["result"]["resultType"], "complete");
    assert!(response(&calls, 1)["result"]["supportedVersions"]
        .as_array()
        .unwrap()
        .iter()
        .any(|version| version == "2026-07-28"));
    assert_eq!(response(&calls, 2)["result"]["resultType"], "complete");
    assert!(response(&calls, 2)["result"]["tools"].as_array().unwrap().len() >= 21);
    assert_eq!(response(&calls, 3)["result"]["structuredContent"]["status"], "ok");
    let _ = fs::remove_dir_all(root);
}

#[test]
fn live_tools_call_rejects_non_object_arguments() {
    let root = std::env::temp_dir().join(format!("epistemic-invalid-arguments-{}", Uuid::new_v4()));
    let calls = run(
        &root,
        &[
            rpc(1, "tools/call", json!({"name":"epistemic_graph_status","arguments":[]})),
            rpc(2, "tools/call", json!({"name":"epistemic_graph_status","arguments":"wrong"})),
        ],
    );
    for id in [1, 2] {
        assert_eq!(response(&calls, id)["error"]["data"]["code"], "invalid_request");
    }
    let _ = fs::remove_dir_all(root);
}
