use rusqlite::params;
use serde_json::{json, Value};
use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

struct Server {
    child: Child,
    input: ChildStdin,
    output: BufReader<ChildStdout>,
    next_id: u64,
}

impl Server {
    fn start(root: &Path, projection: &str) -> Self {
        let entry_dir = root.join(".ai/runtime/orientation-entry/carrier-fixture");
        fs::create_dir_all(&entry_dir).unwrap();
        let entry_file = entry_dir.join("entry.json");
        if !entry_file.exists() {
            fs::write(&entry_file, "{}\n").unwrap();
        }
        let mut child = Command::new(env!("CARGO_BIN_EXE_narada-agent-context-mcp"))
            .args([
                "--site-root",
                root.to_str().unwrap(),
                "--site-id",
                "fixture",
                "--tool-projection",
                projection,
            ])
            .env(
                "NARADA_AGENT_CONTEXT_DB",
                root.join(".ai/state/context.sqlite"),
            )
            .env("NARADA_ORIENTATION_ENTRY_FILE", entry_file)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let input = child.stdin.take().unwrap();
        let output = BufReader::new(child.stdout.take().unwrap());
        Self {
            child,
            input,
            output,
            next_id: 1,
        }
    }

    fn request(&mut self, method: &str, params: Value) -> Value {
        let id = self.next_id;
        self.next_id += 1;
        let request = json!({"jsonrpc":"2.0","id":id,"method":method,"params":params});
        let bytes = serde_json::to_vec(&request).unwrap();
        write!(self.input, "Content-Length: {}\r\n\r\n", bytes.len()).unwrap();
        self.input.write_all(&bytes).unwrap();
        self.input.flush().unwrap();
        let mut content_length = None;
        loop {
            let mut line = String::new();
            self.output.read_line(&mut line).unwrap();
            assert!(!line.is_empty(), "server closed before response");
            if line == "\r\n" || line == "\n" {
                break;
            }
            if let Some(value) = line.strip_prefix("Content-Length:") {
                content_length = Some(value.trim().parse::<usize>().unwrap());
            }
        }
        let mut body = vec![0; content_length.unwrap()];
        self.output.read_exact(&mut body).unwrap();
        let response: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(response["id"], id);
        response
    }

    fn tool(&mut self, name: &str, arguments: Value) -> Value {
        self.request("tools/call", json!({"name":name,"arguments":arguments}))
    }

    fn notify(&mut self, method: &str, params: Value) {
        let request = json!({"jsonrpc":"2.0","method":method,"params":params});
        let bytes = serde_json::to_vec(&request).unwrap();
        write!(self.input, "Content-Length: {}\r\n\r\n", bytes.len()).unwrap();
        self.input.write_all(&bytes).unwrap();
        self.input.flush().unwrap();
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn fixture(name: &str) -> PathBuf {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = std::env::temp_dir().join(format!(
        "narada-agent-context-{name}-{}-{stamp}",
        std::process::id()
    ));
    fs::create_dir_all(&root).unwrap();
    root
}

fn structured(response: &Value) -> &Value {
    &response["result"]["structuredContent"]
}

fn full_structured(root: &Path, response: &Value) -> Value {
    let value = structured(response);
    let Some(reference) = value.get("output_ref").and_then(Value::as_str) else {
        return value.clone();
    };
    let output_id = reference.strip_prefix("mcp_output:").unwrap();
    let record: Value = serde_json::from_slice(
        &fs::read(root.join(format!(".ai/tmp/mcp-outputs/workspace/{output_id}.json"))).unwrap(),
    )
    .unwrap();
    record["full_output"].clone()
}

#[test]
fn public_protocol_is_native_bounded_persistent_and_recoverable() {
    let root = fixture("protocol");
    fs::write(
        root.join("AGENTS.md"),
        "# Fixture law\nPreserve exact authority.\n",
    )
    .unwrap();
    let output_dir = root.join(".ai/tmp/mcp-outputs/workspace");
    fs::create_dir_all(&output_dir).unwrap();
    fs::write(
        output_dir.join("o_fixture.json"),
        serde_json::to_vec(&json!({
            "schema":"narada.mcp_output_ref.v1","ref":"mcp_output:o_fixture",
            "full_output":{"status":"ok","value":"persisted"}
        }))
        .unwrap(),
    )
    .unwrap();
    for index in 0..101 {
        let id = format!("o_page{index:03}");
        fs::write(
            output_dir.join(format!("{id}.json")),
            serde_json::to_vec(&json!({
                "schema":"narada.mcp_output_ref.v1","ref":format!("mcp_output:{id}"),
                "full_output":{"status":"ok"}
            }))
            .unwrap(),
        )
        .unwrap();
    }

    let mut server = Server::start(&root, "admin");
    let initialized = server.request(
        "initialize",
        json!({"protocolVersion":"2099-01-01","clientInfo":{"name":"test","version":"1"},"capabilities":{}}),
    );
    assert_eq!(initialized["result"]["protocolVersion"], "2024-11-05");
    let meta = json!({"_meta":{
        "io.modelcontextprotocol/protocolVersion":"2026-07-28",
        "io.modelcontextprotocol/clientInfo":{"name":"test","version":"1"},
        "io.modelcontextprotocol/clientCapabilities":{}
    }});
    let discovered = server.request("server/discover", meta.clone());
    assert_eq!(discovered["result"]["resultType"], "complete");
    assert_eq!(discovered["result"]["supportedVersions"][0], "2026-07-28");
    assert!(server.request("server/discover", json!({}))["error"].is_object());
    assert!(server.request(
        "tools/list",
        json!({"_meta":{"io.modelcontextprotocol/protocolVersion":"2026-07-28"}})
    )["error"]
        .is_object());
    server.notify(
        "notifications/cancelled",
        json!({"requestId":999,"reason":"test"}),
    );

    let listed = server.request("tools/list", meta);
    let tools = listed["result"]["tools"].as_array().unwrap();
    assert_eq!(tools.len(), 14);
    for tool in tools {
        let name = tool["name"].as_str().unwrap();
        assert_eq!(tool["inputSchema"]["title"], format!("{name}.input"));
        assert_eq!(tool["inputSchema"]["additionalProperties"], false);
        assert!(tool["inputSchema"]["maxProperties"].is_number());
        let invalid = server.tool(name, json!({"unexpected":true}));
        assert!(invalid["error"]["message"]
            .as_str()
            .unwrap()
            .contains("unknown_field"));
    }

    let resources = server.request("resources/list", json!({}));
    assert_eq!(
        resources["result"]["resources"].as_array().unwrap().len(),
        100
    );
    assert_eq!(resources["result"]["has_more"], true);
    let next_cursor = resources["result"]["nextCursor"].clone();
    assert_eq!(
        server.request("resources/list", json!({"cursor":next_cursor}))["result"]["resources"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    let uri = resources["result"]["resources"]
        .as_array()
        .unwrap()
        .iter()
        .find(|resource| resource["name"] == "mcp_output:o_fixture")
        .unwrap()["uri"]
        .clone();
    assert_eq!(
        server.request("resources/read", json!({"uri":uri}))["result"]["contents"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        server.request("prompts/list", json!({}))["result"]["prompts"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert!(
        server.request("prompts/get", json!({"name":"agent_context_startup"}))["result"]
            ["messages"]
            .is_array()
    );
    assert!(server.request(
        "completion/complete",
        json!({"argument":{"name":"name","value":"agent_"}})
    )["result"]["completion"]["values"]
        .is_array());
    assert!(server.request("logging/setLevel", json!({"level":"info"}))["result"].is_object());

    assert_eq!(
        structured(&server.tool("agent_context_doctor", json!({})))["status"],
        "ok"
    );
    assert_eq!(
        structured(&server.tool("agent_context_guidance", json!({})))["status"],
        "ok"
    );
    assert_eq!(
        structured(&server.tool("agent_context_whoami", json!({})))["status"],
        "blocked"
    );
    assert_eq!(
        structured(&server.tool(
            "agent_context_start_session",
            json!({"identity":"fixture.builder","dry_run":true})
        ))["status"],
        "dry_run"
    );
    let orientation = server.tool("agent_orientation_read", json!({}));
    assert_eq!(structured(&orientation)["status"], "anonymous");
    assert_eq!(structured(&orientation)["ordinary_work_gate"], "open");
    assert_eq!(structured(&orientation)["retry_safe"], true);
    assert!(
        server.tool("agent_orientation_acknowledge", json!({}))["error"]["message"]
            .as_str()
            .unwrap()
            .contains("admission_receipt_required")
    );
    assert!(
        server.tool("agent_context_startup_sequence", json!({}))["error"]["message"]
            .as_str()
            .unwrap()
            .contains("admission_receipt_required")
    );
    assert_eq!(
        structured(&server.tool("agent_context_hydrate_current", json!({})))["status"],
        "blocked"
    );
    assert_eq!(
        structured(&server.tool("agent_context_list_sessions", json!({"offset":0,"limit":1})))
            ["returned"],
        0
    );
    assert_eq!(
        structured(&server.tool(
            "mcp_output_show",
            json!({"ref":"mcp_output:o_fixture","limit":8})
        ))["status"],
        "ok"
    );

    let admission = json!({
        "schema":"narada.carrier_session.admission_receipt.v0",
        "receipt_id":"receipt:fixture:1","decision":"admitted","state":"starting",
        "coordinate":{"authority_scope":"test","site_ref":"site:fixture","carrier_session_id":"carrier-fixture","authority_epoch":1},
        "agent_identity":{"source_authority_ref":"agent-identity:fixture","artifact_ref":"agent:fixture.builder@1","revision":"1","local_agent_id":"fixture.builder","canonical_agent_id":"fixture.builder"},
        "carrier_kind":"codex","admission_policy":{"source_authority_ref":"site-law:fixture","artifact_ref":"carrier-policy:fixture","revision":"1"},
        "issued_at":"2026-08-14T00:00:00.000Z","valid_until":null,
        "authority_readback_ref":"carrier-session-authority:fixture","evidence_refs":[],"reason_codes":[]
    });
    let started_response = server.tool(
        "agent_context_start_session",
        json!({
            "identity":"fixture.builder","runtime":"codex",
            "admission_receipt":admission,"generated_at":"2026-08-14T00:00:01.000Z"
        }),
    );
    let started = full_structured(&root, &started_response);
    assert_eq!(started["status"], "materialized");
    let brief = &started["orientation_brief"];
    let manifest = &started["orientation_manifest"];
    let delivery = json!({
        "schema":"narada.carrier_session.orientation_delivery_receipt.v1",
        "receipt_id":"delivery:fixture:1","status":"delivered",
        "admission_receipt_ref":admission["receipt_id"],
        "manifest_id":manifest["manifest_id"],"manifest_digest":manifest["manifest_digest"],
        "brief_id":brief["brief_id"],"brief_digest":brief["brief_digest"],
        "coordinate":admission["coordinate"],"delivered_at":"2026-08-14T00:00:02.000Z"
    });
    let connection = rusqlite::Connection::open(root.join(".ai/state/context.sqlite")).unwrap();
    connection.execute(
        "INSERT INTO orientation_delivery_receipts (receipt_id,manifest_id,brief_id,carrier_session_id,authority_epoch,receipt_json,delivered_at) VALUES (?1,?2,?3,?4,?5,?6,?7)",
        params![
            delivery["receipt_id"].as_str(), manifest["manifest_id"].as_str(),
            brief["brief_id"].as_str(), "carrier-fixture", 1,
            serde_json::to_string(&delivery).unwrap(), "2026-08-14T00:00:02.000Z"
        ],
    ).unwrap();
    drop(connection);
    let evidence = json!({
        "manifest_id":manifest["manifest_id"],
        "admission_receipt":admission,
        "delivery_receipt":delivery
    });
    assert_eq!(
        structured(&server.tool(
            "agent_context_whoami",
            json!({"admission_receipt":admission})
        ))["status"],
        "ok"
    );
    let orientation = server.tool("agent_orientation_read", evidence.clone());
    assert_eq!(structured(&orientation)["status"], "orientation_required");
    assert_eq!(
        structured(&server.tool("agent_context_startup_sequence", evidence.clone()))["status"],
        "orientation_required"
    );
    let mut read_evidence = evidence.clone();
    read_evidence["step_id"] = json!("read:site-law");
    read_evidence["offset"] = json!(0);
    let required_read = server.tool("agent_orientation_read", read_evidence);
    let required_read = full_structured(&root, &required_read);
    assert!(
        required_read["result_evidence"]["returned_lines"]
            .as_u64()
            .is_some(),
        "required read missing evidence; status={} schema={}",
        required_read["status"],
        required_read["schema"]
    );
    let acknowledgement = server.tool("agent_orientation_acknowledge", evidence.clone());
    assert!(
        acknowledgement.get("error").is_none(),
        "acknowledgement failed: {}",
        acknowledgement["error"]["message"]
    );
    assert_eq!(
        full_structured(&root, &acknowledgement)["status"],
        "acknowledged"
    );
    fs::remove_file(
        root.join(".ai/runtime/orientation-entry/carrier-fixture/acknowledgement.json"),
    )
    .unwrap();
    assert_eq!(
        full_structured(
            &root,
            &server.tool("agent_orientation_acknowledge", evidence)
        )["status"],
        "already_acknowledged"
    );
    assert!(root
        .join(".ai/runtime/orientation-entry/carrier-fixture/acknowledgement.json")
        .exists());
    let repeated_start = server.tool(
        "agent_context_start_session",
        json!({
            "identity":"fixture.builder","runtime":"codex",
            "admission_receipt":admission,"generated_at":"2026-08-14T00:00:01.000Z"
        }),
    );
    assert_eq!(
        full_structured(&root, &repeated_start)["status"],
        "materialized"
    );
    let first_session_page =
        server.tool("agent_context_list_sessions", json!({"offset":0,"limit":1}));
    assert_eq!(structured(&first_session_page)["has_more"], true);
    assert_eq!(structured(&first_session_page)["next_offset"], 1);
    assert_eq!(
        structured(&server.tool("agent_context_list_sessions", json!({"offset":1,"limit":1})))
            ["returned"],
        1
    );

    let checkpoint = server.tool(
        "agent_context_checkpoint",
        json!({
            "agent_id":"fixture.builder",
            "continuation":{
                "schema":"narada.continuation.v1","objective":"Prove native continuity",
                "current_state":"checkpointed","next_action":"resume","resume_mode":"fresh_session"
            }
        }),
    );
    let checkpoint_id = structured(&checkpoint)["checkpoint_id"]
        .as_str()
        .unwrap()
        .to_string();
    assert_eq!(
        structured(&server.tool(
            "agent_context_rehydrate",
            json!({"agent_id":"fixture.builder","checkpoint_id":checkpoint_id})
        ))["status"],
        "ok"
    );
    let exported = server.tool(
        "agent_context_continuation_export",
        json!({"agent_id":"fixture.builder"}),
    );
    let exported = full_structured(&root, &exported);
    assert_eq!(exported["status"], "exported");
    assert_eq!(
        structured(&server.tool(
            "agent_context_continuation_read",
            json!({"agent_id":"fixture.builder"})
        ))["status"],
        "ok"
    );
    fs::write(
        root.join(exported["continuation_ref"]["path"].as_str().unwrap()),
        "tampered\n",
    )
    .unwrap();
    assert_eq!(
        structured(&server.tool(
            "agent_context_continuation_read",
            json!({"agent_id":"fixture.builder"})
        ))["status"],
        "stale"
    );
    drop(server);

    let mut resumed = Server::start(&root, "admin");
    let persisted = resumed.tool(
        "agent_context_rehydrate",
        json!({"agent_id":"fixture.builder"}),
    );
    assert_eq!(structured(&persisted)["status"], "ok");
    assert_eq!(structured(&persisted)["checkpoint_id"], checkpoint_id);
    drop(resumed);

    let mut occupant = Server::start(&root, "occupant");
    let listed = occupant.request("tools/list", json!({}));
    assert_eq!(listed["result"]["tools"].as_array().unwrap().len(), 2);
    assert_eq!(
        structured(&occupant.tool("agent_orientation_read", json!({})))["status"],
        "anonymous"
    );
    assert_eq!(
        structured(&occupant.tool("mcp_output_show", json!({"ref":"mcp_output:o_fixture"})))
            ["status"],
        "ok"
    );
    fs::remove_dir_all(root).unwrap();
}
