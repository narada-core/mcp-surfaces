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
fn run(root: &Path, state: &Path, script: &Path, url: &str, requests: &[Value]) -> Vec<Value> {
    let mut child = Command::new(env!("CARGO_BIN_EXE_narada-mcp-surfaces"))
        .args([
            "--surface-id",
            "quota-meter",
            "--site-root",
            &root.to_string_lossy(),
        ])
        .env("QUOTA_METER_STATE_ROOT", state)
        .env("QUOTA_METER_OVERLAY_SCRIPT", script)
        .env("QUOTA_METER_POWERSHELL", "pwsh")
        .env("USERPROFILE", root)
        .env("HOME", root)
        .env("KIMI_CODE_HOME", root.join("kimi-home"))
        .env("KIMI_CODE_CREDENTIALS", root.join("missing-kimi-credentials.json"))
        .env("KIMI_CODE_API_KEY", "test-token")
        .env("KIMI_USAGE_URL", url)
        .env("QUOTA_METER_CODEX_COMMAND", "definitely-not-a-command")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn");
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
    String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
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
    let mut request = Vec::new();
    let mut buffer = [0u8; 2048];
    while request.len() < 16 * 1024 {
        let count = stream.read(&mut buffer).unwrap_or(0);
        if count == 0 {
            break;
        }
        request.extend_from_slice(&buffer[..count]);
        if request.windows(4).any(|window| window == b"\r\n\r\n") {
            break;
        }
    }
    String::from_utf8_lossy(&request).to_string()
}
fn mock_kimi(count: usize) -> (String, thread::JoinHandle<Vec<String>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let handle = thread::spawn(move || {
        let mut requests = Vec::new();
        for index in 0..count {
            let (mut stream, _) = listener.accept().unwrap();
            requests.push(read_request(&mut stream));
            if index == count - 1 && count > 1 {
                thread::sleep(Duration::from_millis(250));
            }
            let body=json!({"subType":"test","usage":{"used":25,"remaining":75,"limit":100,"resetTime":"2026-08-15T00:00:00Z"}}).to_string();
            let _=write!(stream,"HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",body.len());
        }
        requests
    });
    (url, handle)
}

#[test]
fn quota_meter_public_protocol_is_native_complete_bounded_and_recoverable() {
    let root = std::env::temp_dir().join(format!("narada-quota-stdio-{}", uuid::Uuid::new_v4()));
    let state = root.join("state");
    fs::create_dir_all(&state).unwrap();
    let script = root.join("overlay-test.ps1");
    fs::write(&script,r#"param([string]$Action,[string]$NativePath,[string]$ProviderSelection,[int]$RefreshSeconds,[string]$PidPath,[string]$PositionPath,[string]$RefreshPath,[string]$StatusPath,[string]$LoginStatePath)
if($Action -eq 'stop'){if(Test-Path -LiteralPath $PidPath){$target=[int](Get-Content -LiteralPath $PidPath -Raw);Stop-Process -Id $target -Force -ErrorAction SilentlyContinue;Remove-Item -LiteralPath $PidPath -Force -ErrorAction SilentlyContinue};exit 0}
[IO.File]::WriteAllText($PidPath,[string]$PID);$status=@{schemaVersion=1;updatedAt=(Get-Date).ToUniversalTime().ToString('o');visible=$true}|ConvertTo-Json -Compress;[IO.File]::WriteAllText($StatusPath,$status);Start-Sleep -Seconds 20
"#).unwrap();
    let (url, server) = mock_kimi(2);
    let catalog = run(
        &root,
        &state,
        &script,
        &url,
        &[rpc(1, "tools/list", json!({}))],
    );
    let tools = response(&catalog, 1)
        .pointer("/result/tools")
        .and_then(Value::as_array)
        .unwrap();
    assert_eq!(tools.len(), 5);
    for entry in tools {
        let name = entry["name"].as_str().unwrap();
        assert_eq!(entry["inputSchema"]["title"], format!("{name}.input"));
        assert_eq!(entry["inputSchema"]["additionalProperties"], false);
        bounded(&entry["inputSchema"], name);
    }
    let invalid = tools
        .iter()
        .enumerate()
        .map(|(i, entry)| {
            tool(
                100 + i as u64,
                entry["name"].as_str().unwrap(),
                json!({"unexpected":true}),
            )
        })
        .collect::<Vec<_>>();
    let invalid_results = run(&root, &state, &script, &url, &invalid);
    for (i, entry) in tools.iter().enumerate() {
        assert!(
            response(&invalid_results, 100 + i as u64)
                .get("error")
                .is_some(),
            "{}",
            entry["name"]
        );
    }
    let calls = run(
        &root,
        &state,
        &script,
        &url,
        &[
            tool(10, "quota_meter_guidance", json!({})),
            tool(11, "quota_meter_glide_status", json!({"providers":"kimi"})),
            tool(
                12,
                "quota_meter_glide_status",
                json!({"providers":"codex","timeout_ms":100}),
            ),
            tool(13, "quota_meter_overlay_status", json!({})),
            tool(
                14,
                "quota_meter_overlay_start",
                json!({"providers":"kimi","refresh_seconds":5}),
            ),
            tool(15, "quota_meter_overlay_status", json!({})),
            tool(16, "quota_meter_overlay_stop", json!({})),
            tool(17, "quota_meter_overlay_stop", json!({})),
            tool(
                18,
                "quota_meter_glide_status",
                json!({"providers":"kimi","timeout_ms":100}),
            ),
        ],
    );
    for id in 10..=18 {
        assert!(
            response(&calls, id).get("error").is_none(),
            "{id}: {}",
            response(&calls, id)
        );
    }
    assert_eq!(structured(response(&calls, 11))["status"], "ok");
    assert_eq!(
        structured(response(&calls, 11))["providers"][0]["windows"][0]["glidePath"]["formula"],
        "usedPercent / elapsedTimePercent"
    );
    assert_eq!(structured(response(&calls, 12))["status"], "partial");
    assert_eq!(structured(response(&calls, 13))["status"], "stopped");
    assert_eq!(structured(response(&calls, 14))["status"], "started");
    assert_eq!(structured(response(&calls, 15))["status"], "running");
    assert_eq!(structured(response(&calls, 16))["status"], "stopped");
    assert_eq!(
        structured(response(&calls, 17))["status"],
        "already_stopped"
    );
    assert_eq!(structured(response(&calls, 18))["status"], "partial");
    assert!(!serde_json::to_string(&calls)
        .unwrap()
        .contains("test-token"));
    let requests = server.join().unwrap();
    assert_eq!(requests.len(), 2);
    assert!(requests.iter().all(|request| request
        .to_ascii_lowercase()
        .contains("authorization: bearer test-token")));
    fs::remove_dir_all(root).unwrap();
}
