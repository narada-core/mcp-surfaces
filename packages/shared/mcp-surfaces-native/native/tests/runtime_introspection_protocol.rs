use rusqlite::Connection;
use serde_json::{json, Value};
use std::fs;
use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

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
            "runtime-introspection",
            "--site-root",
            &root.to_string_lossy(),
        ])
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
    values
        .iter()
        .find(|value| value["id"] == id)
        .expect("response id")
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
    if let Some(items) = schema.get("items") {
        assert_bounded(items, &format!("{path}/*"));
    }
}
fn prepare_store(root: &Path) {
    let store = root.join(".narada/runtime/mcp-runtime-observer");
    fs::create_dir_all(&store).expect("store");
    let db = Connection::open(store.join("observations.db")).expect("db");
    db.execute_batch(
        "CREATE TABLE owners(owner_id TEXT PRIMARY KEY,site_id TEXT,authority_ref TEXT,owner_kind TEXT,pid INTEGER,process_started_at TEXT,process_creation_ticks INTEGER,parent_owner_id TEXT,surface_id TEXT,instance_id TEXT,generation_id TEXT,carrier_session_id TEXT,executable_name TEXT,observed_at TEXT,active INTEGER);
         CREATE TABLE process_samples(sample_id TEXT,sampled_at_ms INTEGER,owner_id TEXT,pid INTEGER,parent_pid INTEGER,creation_ticks INTEGER,working_set_bytes INTEGER,private_bytes INTEGER,commit_bytes INTEGER,virtual_bytes INTEGER,handle_count INTEGER,thread_count INTEGER,cpu_time_ms INTEGER,executable_name TEXT,sample_status TEXT);
         CREATE TABLE worker_samples(sample_id TEXT,sampled_at_ms INTEGER,owner_id TEXT,instance_id TEXT,generation_id TEXT,heap_total_bytes INTEGER,heap_used_bytes INTEGER,external_bytes INTEGER,array_buffers_bytes INTEGER,heap_limit_bytes INTEGER,invocation_count INTEGER,inflight INTEGER,active_resource_counts_json TEXT,sample_status TEXT);
         CREATE TABLE incidents(incident_id TEXT,owner_id TEXT,opened_at_ms INTEGER,updated_at_ms INTEGER,status TEXT,detector TEXT,attribution TEXT,confidence REAL,baseline_bytes INTEGER,observed_bytes INTEGER,slope_bytes_per_minute REAL,review_note TEXT);
         CREATE TABLE evidence(evidence_id TEXT,incident_id TEXT,created_at_ms INTEGER,evidence_type TEXT,payload_json TEXT);
         CREATE TABLE artifacts(artifact_id TEXT,incident_id TEXT,created_at_ms INTEGER,path TEXT,kind TEXT,bytes INTEGER);
         CREATE TABLE observer_cycles(started_at_ms INTEGER,duration_ms REAL,sampled_processes INTEGER);
         INSERT INTO owners VALUES('process-1','site','authority','process',10,'2026-01-01',1,NULL,'runtime-introspection',NULL,NULL,'carrier','native.exe','2026-01-01',1);
         INSERT INTO owners VALUES('worker-1','site','authority','worker',NULL,'2026-01-01',2,'process-1','runtime-introspection','instance','generation','carrier','native.exe','2026-01-01',1);
         INSERT INTO process_samples VALUES('p1',1000,'process-1',10,1,1,800,1000,1100,1200,5,2,10,'native.exe','ok');
         INSERT INTO process_samples VALUES('p2',2000,'process-1',10,1,1,900,1200,1300,1400,6,3,20,'native.exe','ok');
         INSERT INTO worker_samples VALUES('w1',1500,'worker-1','instance','generation',700,600,200,100,1000,1,0,'{}','ok');
         INSERT INTO incidents VALUES('incident-1','worker-1',1000,2000,'open','growth','partial',0.7,500,1200,10.0,NULL);
         INSERT INTO evidence VALUES('evidence-1','incident-1',2000,'sample','{\"private_bytes\":1200}');
         INSERT INTO artifacts VALUES('artifact-1','incident-1',2000,'bounded.json','report',42);",
    )
    .expect("schema");
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_millis() as i64;
    db.execute("INSERT INTO observer_cycles VALUES(?1,12.0,2)", [now_ms])
        .expect("observer cycle");
}

#[test]
fn runtime_introspection_public_protocol_is_complete_bounded_and_read_only() {
    let root = std::env::temp_dir().join(format!(
        "narada-runtime-introspection-stdio-{}",
        uuid::Uuid::new_v4()
    ));
    prepare_store(&root);
    let database_path = root.join(".narada/runtime/mcp-runtime-observer/observations.db");
    let database_before = fs::read(&database_path).expect("database before");

    let catalog = run(&root, &[rpc(1, "tools/list", json!({}))]);
    let tools = response(&catalog, 1)
        .pointer("/result/tools")
        .and_then(Value::as_array)
        .expect("tools");
    assert_eq!(tools.len(), 14);
    for entry in tools {
        let name = entry["name"].as_str().expect("name");
        let schema = &entry["inputSchema"];
        assert_eq!(schema["title"], format!("{name}.input"));
        assert_eq!(schema["additionalProperties"], false);
        assert_eq!(entry["annotations"]["readOnlyHint"], true);
        assert_bounded(schema, name);
    }

    let invalid = tools
        .iter()
        .enumerate()
        .map(|(index, entry)| {
            tool(
                1000 + index as u64,
                entry["name"].as_str().unwrap(),
                json!({"__unexpected_contract_probe__":true}),
            )
        })
        .collect::<Vec<_>>();
    let invalid_results = run(&root, &invalid);
    for (index, entry) in tools.iter().enumerate() {
        assert!(
            response(&invalid_results, 1000 + index as u64)
                .get("error")
                .is_some(),
            "{} accepted unknown input",
            entry["name"]
        );
    }

    let events = json!([{"event_id":"event-1","timestamp":"2026-01-01T00:00:00Z","kind":"tool_call","status":"ok","surface_id":"git","tool_name":"git_status","duration_ms":4},{"event_id":"event-2","kind":"error","status":"refused","surface_id":"structured-command","tool_name":"structured_command_execute","duration_ms":8,"message":"refused"}]);
    let calls = vec![
        tool(10, "runtime_introspection_guidance", json!({})),
        tool(11, "runtime_introspection_formats", json!({})),
        tool(
            12,
            "runtime_introspection_top_events",
            json!({"events":events,"limit":1}),
        ),
        tool(
            13,
            "runtime_introspection_analyze_trace",
            json!({"events":events}),
        ),
        tool(
            14,
            "runtime_introspection_analyze",
            json!({"events":events}),
        ),
        tool(
            15,
            "runtime_introspection_top",
            json!({"events":events,"dimension":"surface","limit":2}),
        ),
        tool(
            16,
            "runtime_introspection_show",
            json!({"events":events,"view":"errors","limit":2}),
        ),
        tool(
            17,
            "runtime_introspection_show_event",
            json!({"events":events,"event_id":"event-2"}),
        ),
        tool(18, "runtime_introspection_memory_status", json!({})),
        tool(
            19,
            "runtime_introspection_memory_owners",
            json!({"limit":1}),
        ),
        tool(
            20,
            "runtime_introspection_memory_timeline",
            json!({"owner_id":"process-1","limit":1}),
        ),
        tool(
            21,
            "runtime_introspection_memory_attribution",
            json!({"owner_id":"worker-1"}),
        ),
        tool(
            22,
            "runtime_introspection_memory_incidents",
            json!({"status":"open","limit":1}),
        ),
        tool(
            23,
            "runtime_introspection_memory_incident_show",
            json!({"incident_id":"incident-1"}),
        ),
        tool(24, "runtime_introspection_analyze", json!({"events":[]})),
        tool(
            25,
            "runtime_introspection_analyze",
            json!({"format":"codex-jsonl","jsonl":"not-json"}),
        ),
        tool(
            26,
            "runtime_introspection_show_event",
            json!({"events":events}),
        ),
    ];
    let results = run(&root, &calls);
    for id in 10..=24 {
        assert!(response(&results, id).get("error").is_none(), "call {id}");
    }
    assert!(response(&results, 25).get("error").is_some());
    assert!(response(&results, 26).get("error").is_some());
    assert_eq!(
        structured(response(&results, 13))["summary"]["event_count"],
        2
    );
    assert_eq!(
        structured(response(&results, 16))["data"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        structured(response(&results, 19))["items"][0]["private_bytes"],
        1200
    );
    assert_eq!(structured(response(&results, 18))["status"], "stale");
    assert_eq!(structured(response(&results, 18))["observer"]["cycles"], 1);
    assert_eq!(structured(response(&results, 20))["next_before_ms"], 2000);
    assert_eq!(
        structured(response(&results, 23))["evidence"][0]["payload"]["private_bytes"],
        1200
    );
    assert_eq!(
        structured(response(&results, 24))["summary"]["event_count"],
        0
    );

    let retry = run(
        &root,
        &[tool(
            30,
            "runtime_introspection_memory_incident_show",
            json!({"incident_id":"incident-1"}),
        )],
    );
    assert_eq!(
        structured(response(&retry, 30))["incident"]["incident_id"],
        "incident-1"
    );
    assert_eq!(
        fs::read(database_path).expect("database after"),
        database_before
    );
    let unavailable = run(
        &root.join("missing-site"),
        &[tool(31, "runtime_introspection_memory_status", json!({}))],
    );
    assert_eq!(
        response(&unavailable, 31)
            .pointer("/error/data/code")
            .and_then(Value::as_str),
        Some("runtime_introspection_memory_store_unavailable")
    );
    fs::remove_dir_all(root).expect("cleanup");
}
