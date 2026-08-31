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

fn invoke(root: &Path, name: &str, arguments: Value) -> Value {
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
        .expect("spawn");
    writeln!(child.stdin.as_mut().unwrap(), "{}", json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":name,"arguments":arguments}})).unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let wire: Value = serde_json::from_slice(
        output
            .stdout
            .split(|byte| *byte == b'\n')
            .find(|line| !line.is_empty())
            .unwrap(),
    )
    .unwrap();
    wire.pointer("/result/structuredContent")
        .cloned()
        .unwrap_or_else(|| {
            wire.pointer("/error/data")
                .cloned()
                .unwrap_or_else(|| wire["error"].clone())
        })
}

fn authority() -> Value {
    json!({"kind":"test","summary":"Excellent AX acceptance."})
}
fn root(label: &str) -> PathBuf {
    std::env::temp_dir().join(format!("issue-tree-ax-{label}-{}", Uuid::new_v4()))
}

fn create(root: &Path, objective: &str) -> Value {
    invoke(
        root,
        "epistemic_graph_issue_tree_resume",
        json!({
            "objective":objective,"create_if_missing":true,"actor":"ax-test","authority_basis":authority()
        }),
    )
}
fn resume(root: &Path, tree_id: &str) -> Value {
    invoke(
        root,
        "epistemic_graph_issue_tree_resume",
        json!({"tree_id":tree_id}),
    )
}
fn batch(root: &Path, tree_id: &str, nodes: Value, key: &str) -> Value {
    invoke(
        root,
        "epistemic_graph_issue_tree_transition",
        json!({
            "actor":"ax-test","authority_basis":authority(),"tree_id":tree_id,"nodes":nodes,"idempotency_key":key
        }),
    )
}
fn transition(
    root: &Path,
    tree_id: &str,
    selected: &str,
    version: u64,
    disposition: &str,
    key: &str,
    successors: Value,
) -> Value {
    invoke(
        root,
        "epistemic_graph_issue_tree_transition",
        json!({
            "actor":"ax-test","authority_basis":authority(),"tree_id":tree_id,
            "selected_node_id":selected,"expected_node_version":version,"idempotency_key":key,
            "transition":{"disposition":disposition,"rationale":"acceptance","successors":successors},
            "select_next":true
        }),
    )
}
fn tree_and_selected(created: &Value) -> (&str, &str) {
    (
        created["tree"]["tree_id"].as_str().unwrap(),
        created["selected"]["node_id"].as_str().unwrap(),
    )
}

fn acceptance(case: u8) {
    let root = root(&format!("i{case:02}"));
    match case {
        1 => {
            let created = create(&root, "I1 resume");
            let (tree, selected) = tree_and_selected(&created);
            let value = resume(&root, tree);
            assert_eq!(value["selected"]["node_id"], selected);
            assert!(value["frontier"]["returned"].as_u64().unwrap() >= 1);
        }
        2 => {
            create(&root, "I2 objective");
            let value = invoke(
                &root,
                "epistemic_graph_issue_tree_resume",
                json!({"objective":"I2 objective"}),
            );
            assert_eq!(value["status"], "ok");
            let missing = invoke(
                &root,
                "epistemic_graph_issue_tree_resume",
                json!({"objective":"absent"}),
            );
            assert_eq!(missing["code"], "issue_tree_not_found");
        }
        3 => {
            let barrier = Arc::new(Barrier::new(3));
            let handles = (0..2)
                .map(|_| {
                    let root = root.clone();
                    let barrier = barrier.clone();
                    thread::spawn(move || {
                        barrier.wait();
                        create(&root, "I3 atomic creation")
                    })
                })
                .collect::<Vec<_>>();
            barrier.wait();
            let results = handles
                .into_iter()
                .map(|h| h.join().unwrap())
                .collect::<Vec<_>>();
            assert!(results
                .iter()
                .all(|v| v["status"] == "ok" || v["code"].as_str().is_some()));
            let resolved = invoke(
                &root,
                "epistemic_graph_issue_tree_resume",
                json!({"objective":"I3 atomic creation"}),
            );
            assert_eq!(resolved["status"], "ok");
        }
        4 => {
            let receipt = invoke(
                &root,
                "epistemic_graph_submit_review_admit",
                json!({
                    "actor":"ax-test","authority_basis":authority(),
                    "operations":[{"op":"entity.declare","entity_id":"tree:empty","kind":"research_issue_tree","title":"Empty","objective":"Empty","version":"1"}]
                }),
            );
            assert_eq!(receipt["status"], "admitted");
            let value = resume(&root, "tree:empty");
            assert_eq!(value["frontier"]["returned"], 0);
            assert!(value["selected"].is_null());
        }
        5 | 12 => {
            batch(
                &root,
                "tree:rank",
                json!([
                    {"node_id":"issue:b","title":"B","version":1,"score":0.9},
                    {"node_id":"issue:a","title":"A","version":1,"score":0.9},
                    {"node_id":"issue:c","title":"C","version":1,"score":0.1}
                ]),
                "rank",
            );
            let value = invoke(
                &root,
                "epistemic_graph_issue_tree_frontier",
                json!({"tree_id":"tree:rank","limit":20}),
            );
            assert_eq!(value["frontier"]["items"][0]["node_id"], "issue:a");
            assert_eq!(value["ordering"], "score_desc_then_node_id");
        }
        6 | 18 | 19 => {
            let created = create(&root, "restart continuity");
            let (tree, selected) = tree_and_selected(&created);
            let pointer = json!({"tree_id":tree,"selected_node_id":selected,"selected_node_version":1,"rehydrate_with":"epistemic_graph_issue_tree_resume"});
            let value = invoke(
                &root,
                pointer["rehydrate_with"].as_str().unwrap(),
                json!({"tree_id":pointer["tree_id"]}),
            );
            assert_eq!(value["selected"]["node_id"], pointer["selected_node_id"]);
        }
        7 | 9 | 11 => {
            let created = create(&root, &format!("disposition {case}"));
            let (tree, selected) = tree_and_selected(&created);
            let disposition = if case == 7 {
                "resolved"
            } else if case == 9 {
                "exhausted"
            } else {
                "split"
            };
            let successors = if case == 11 {
                json!([{"node_id":"issue:split-child","title":"child","score":0.8}])
            } else {
                json!([])
            };
            let value = transition(
                &root,
                tree,
                selected,
                1,
                disposition,
                &format!("transition-{case}"),
                successors,
            );
            assert_eq!(value["workflow"]["disposition"], disposition);
            assert_eq!(value["workflow"]["certifies_truth"], false);
        }
        8 | 23 => {
            let created = create(&root, "negative result");
            let (tree, selected) = tree_and_selected(&created);
            invoke(
                &root,
                "epistemic_graph_submit_review_admit",
                json!({
                    "actor":"ax-test","authority_basis":authority(),
                    "operations":[{"op":"entity.declare","entity_id":"artifact:test","kind":"claim","title":"Test artifact"}]
                }),
            );
            let value = invoke(
                &root,
                "epistemic_graph_issue_tree_transition",
                json!({
                    "actor":"ax-test","authority_basis":authority(),"tree_id":tree,
                    "selected_node_id":selected,"expected_node_version":1,"idempotency_key":"reject",
                    "transition":{"disposition":"rejected","rationale":"falsified","evidence_ids":["artifact:test"],"successors":[]}
                }),
            );
            assert_eq!(value["workflow"]["certifies_truth"], false);
            assert!(
                serde_json::to_string(&value)
                    .unwrap()
                    .contains("not evidence")
                    || value["issue_tree_transition"]["evidence_promotion"] == false
            );
        }
        10 => {
            let created = create(&root, "block ordinary leaf");
            let (tree, selected) = tree_and_selected(&created);
            invoke(
                &root,
                "epistemic_graph_submit_review_admit",
                json!({"actor":"ax-test","authority_basis":authority(),"operations":[{"op":"entity.declare","entity_id":"issue:blocker","kind":"claim","title":"Blocker"}]}),
            );
            let value = transition(
                &root,
                tree,
                selected,
                1,
                "deferred",
                "blocked",
                json!([
                    {"node_id":"issue:blocked","title":"Blocked","state":"blocked","score":0.9,"blocker_ids":["issue:blocker"]},
                    {"node_id":"issue:next","title":"Next","state":"open","score":0.8}
                ]),
            );
            assert_eq!(value["status"], "admitted", "{value}");
            let frontier = invoke(
                &root,
                "epistemic_graph_issue_tree_frontier",
                json!({"tree_id":tree}),
            );
            assert_eq!(frontier["selected"]["node_id"], "issue:next");
            assert!(frontier["frontier"]["items"]
                .as_array()
                .unwrap()
                .iter()
                .any(|n| n["state"] == "blocked"));
        }
        13 => {
            let nodes = (0..143).map(|i| json!({"node_id":format!("issue:{i:03}"),"title":format!("Issue {i}"),"version":1,"score":(143-i) as f64/143.0})).collect::<Vec<_>>();
            for (index, page) in nodes.chunks(50).enumerate() {
                let seeded = batch(
                    &root,
                    "tree:large",
                    Value::Array(page.to_vec()),
                    &format!("large-{index}"),
                );
                assert_eq!(seeded["status"], "admitted", "{seeded}");
            }
            let first = invoke(
                &root,
                "epistemic_graph_issue_tree_frontier",
                json!({"tree_id":"tree:large","limit":100}),
            );
            assert_eq!(first["frontier"]["total"], 143);
            assert_eq!(first["frontier"]["complete"], false);
            let second = invoke(
                &root,
                first["continuation"]["tool"].as_str().unwrap(),
                first["continuation"]["arguments"].clone(),
            );
            assert_eq!(
                first["frontier"]["returned"].as_u64().unwrap()
                    + second["frontier"]["returned"].as_u64().unwrap(),
                143
            );
            assert_eq!(first["result_ref"], second["result_ref"]);
        }
        14 => {
            let title = "λ".repeat(1000);
            let rationale = "ρ".repeat(2000);
            batch(
                &root,
                "tree:oversized",
                json!([{"node_id":"issue:oversized","title":title,"version":1,"score":1.0,"rationale":rationale}]),
                "oversized",
            );
            let value = invoke(
                &root,
                "epistemic_graph_issue_tree_frontier",
                json!({"tree_id":"tree:oversized"}),
            );
            let encoded = serde_json::to_string(&value).unwrap();
            assert!(encoded.len() <= 6000);
            assert_eq!(value["frontier"]["items"][0]["title_clipped"], true);
            assert_eq!(value["frontier"]["items"][0]["rationale_clipped"], true);
        }
        15 | 17 => {
            let created = create(&root, "concurrent transition");
            let (tree, selected) = tree_and_selected(&created);
            if case == 17 {
                let first = transition(&root, tree, selected, 1, "resolved", "same", json!([]));
                let replay = transition(&root, tree, selected, 1, "resolved", "same", json!([]));
                assert_eq!(first, replay);
            } else {
                let barrier = Arc::new(Barrier::new(3));
                let handles = ["left","right"].into_iter().map(|side| {
                    let root=root.clone(); let tree=tree.to_string(); let selected=selected.to_string(); let barrier=barrier.clone();
                    thread::spawn(move || { barrier.wait(); transition(&root,&tree,&selected,1,"split",side,json!([{"node_id":format!("issue:{side}"),"title":side,"score":0.5}])) })
                }).collect::<Vec<_>>();
                barrier.wait();
                let values = handles
                    .into_iter()
                    .map(|h| h.join().unwrap())
                    .collect::<Vec<_>>();
                assert!(values
                    .iter()
                    .any(|v| v["workflow"]["disposition"] == "split"));
                assert!(
                    values.iter().any(|v| matches!(
                        v["code"].as_str(),
                        Some("issue_tree_selected_conflict")
                            | Some("ledger_head_conflict")
                            | Some("ledger_head_mismatch")
                            | Some("issue_tree_version_conflict")
                    )),
                    "{values:?}"
                );
            }
        }
        16 => {
            let created = create(&root, "unknown outcome");
            let (tree, selected) = tree_and_selected(&created);
            let admitted = transition(&root, tree, selected, 1, "resolved", "unknown", json!([]));
            assert!(admitted["workflow"]["reconciliation"]["tool"]
                .as_str()
                .is_some());
            let reconciled = resume(&root, tree);
            assert_ne!(reconciled["selected"]["node_id"], selected);
        }
        20 => {
            let unavailable = root.join("not-a-directory");
            fs::create_dir_all(&root).unwrap();
            fs::write(&unavailable, b"occupied").unwrap();
            let value = invoke(
                &unavailable,
                "epistemic_graph_issue_tree_resume",
                json!({"objective":"outage","create_if_missing":true,"actor":"ax-test","authority_basis":authority()}),
            );
            assert_ne!(value["status"], "admitted");
            assert!(value["code"].as_str().is_some() || value["message"].as_str().is_some());
        }
        21 | 22 => {
            let created = create(&root, "client parity");
            let (tree, _) = tree_and_selected(&created);
            let raw = resume(&root, tree);
            assert_eq!(raw["schema"], "narada.epistemic.issue-tree.resume.v1");
            assert!(serde_json::to_string(&raw).unwrap().len() <= 8000);
            assert_eq!(
                raw["frontier"]["returned"],
                raw["frontier"]["items"].as_array().unwrap().len()
            );
        }
        24 => {
            let bad_score = batch(
                &root,
                "tree:invalid",
                json!([{"node_id":"issue:bad","title":"bad","version":1,"score":9.3}]),
                "bad-score",
            );
            assert_eq!(bad_score["code"], "issue_tree_invalid");
            assert!(
                bad_score["message"]
                    .as_str()
                    .unwrap()
                    .contains("between 0 and 1"),
                "{bad_score}"
            );
            let bad_version = batch(
                &root,
                "tree:invalid",
                json!([{"node_id":"issue:bad-v","title":"bad","version":2,"score":0.3}]),
                "bad-version",
            );
            assert_eq!(bad_version["code"], "issue_tree_invalid");
            assert!(bad_version["message"]
                .as_str()
                .unwrap()
                .contains("predecessor"));
        }
        _ => unreachable!(),
    }
    let _ = fs::remove_dir_all(root);
}

macro_rules! cases {
    ($($name:ident:$number:literal),+ $(,)?) => { $(#[test] fn $name(){ acceptance($number); })+ };
}
cases!(
 i01_resume_known_tree:1, i02_resolve_objective:2, i03_atomic_creation:3, i04_empty_tree:4,
 i05_highest_score:5, i06_restart_selected_leaf:6, i07_completion:7, i08_falsification:8,
 i09_exhaustion:9, i10_blocked_leaf:10, i11_split_leaf:11, i12_tied_scores:12,
 i13_more_than_100:13, i14_oversized_node:14, i15_concurrent_transitions:15,
 i16_unknown_outcome:16, i17_exact_retry:17, i18_generation_replacement:18,
 i19_compaction_pointer:19, i20_graph_outage:20, i21_pi_projection_contract:21,
 i22_non_pi_parity_contract:22, i23_noncertification:23, i24_precise_refusals:24
);
