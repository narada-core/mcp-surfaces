use serde_json::{json, Value};
use std::fs;
use std::process::Command;
use tempfile::tempdir;

#[test]
fn materializes_every_supported_carrier_kind_without_javascript_runtime_environment() {
    let root = tempdir().unwrap();
    let paths = [
        ("codex-test", "codex", root.path().join("codex/config.toml")),
        ("kimi-test", "kimi", root.path().join("kimi/mcp.json")),
        (
            "opencode-test",
            "opencode",
            root.path().join("opencode/opencode.jsonc"),
        ),
    ];
    let input_path = root.path().join("materialization-input.json");
    let input = json!({
        "schema": "narada.carrier_materialization_input.v1",
        "workspace_root": root.path(),
        "artifact_manifest_path": root.path().join("workspace-artifact-manifest.json"),
        "artifact_manifest_fingerprint": "a".repeat(64),
        "runtime_profile_kind": "native",
        "runtime_implementation_matrix_path": root.path().join("runtime-implementation-matrix.json"),
        "runtime_implementation_matrix_fingerprint": "b".repeat(64),
        "registrar_entrypoint": root.path().join("narada-mcp-materializer.exe"),
        "registrar_fingerprint": "c".repeat(64),
        "proxy_implementation": "native",
        "proxy_entrypoint": root.path().join("narada-mcp-runtime.exe"),
        "proxy_fingerprint": "d".repeat(64),
        "installed_carrier_index_path": root.path().join("installed-carriers.json"),
        "carriers": paths.iter().map(|(id, kind, path)| json!({
            "carrier_id": id,
            "carrier_kind": kind,
            "config_path": path,
            "codex_plugin_overrides": { "github@openai-curated-remote": false },
            "trust_projects": [root.path()],
            "servers": [{
                "name": "narada-site-test-local-filesystem",
                "command": root.path().join("narada-mcp-runtime.exe").to_string_lossy(),
                "args": ["proxy", "--surface-id", "local-filesystem"],
                "env_vars": ["NARADA_AGENT_ID"],
                "enabled": true,
                "approval_mode": "approve",
                "startup_timeout_sec": 60,
                "tools": [{ "name": "fs_read", "approval_mode": "approve" }]
            }]
        })).collect::<Vec<_>>()
    });
    fs::write(&input_path, serde_json::to_vec_pretty(&input).unwrap()).unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_narada-mcp-materializer"))
        .env_clear()
        .arg("materialize-all")
        .arg("--input")
        .arg(&input_path)
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );
    let result: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["status"], "materialized_all");
    assert_eq!(result["carrier_count"], 3);
    for (_, _, path) in &paths {
        assert!(path.exists(), "missing {}", path.display());
        assert!(path
            .with_file_name(format!(
                "{}.narada-generation.json",
                path.file_name().unwrap().to_string_lossy()
            ))
            .exists());
    }
    let codex = fs::read_to_string(&paths[0].2).unwrap();
    assert!(codex.contains("[features]\napps = false"));
    assert!(codex.contains("env_vars = [\"NARADA_AGENT_ID\"]"));
    assert!(codex.contains("[mcp_servers.narada-site-test-local-filesystem.tools.fs_read]"));

    let kimi: Value = serde_json::from_slice(&fs::read(&paths[1].2).unwrap()).unwrap();
    let kimi_server = &kimi["mcpServers"]["narada-site-test-local-filesystem"];
    assert_eq!(kimi_server["transport"], "stdio");
    assert_eq!(kimi_server["env_vars"][0], "NARADA_AGENT_ID");
    assert!(kimi_server.get("enabled").is_none());

    let opencode_text = fs::read_to_string(&paths[2].2).unwrap();
    let opencode_json = opencode_text
        .lines()
        .skip_while(|line| line.trim_start().starts_with("//"))
        .collect::<Vec<_>>()
        .join("\n");
    let opencode: Value = serde_json::from_str(&opencode_json).unwrap();
    let opencode_server = &opencode["mcp"]["narada-site-test-local-filesystem"];
    assert_eq!(opencode["$schema"], "https://opencode.ai/config.json");
    assert_eq!(opencode_server["type"], "local");
    assert_eq!(opencode_server["command"][1], "proxy");
    assert_eq!(opencode_server["enabled"], true);

    let index: Value =
        serde_json::from_slice(&fs::read(root.path().join("installed-carriers.json")).unwrap())
            .unwrap();
    assert_eq!(index["carriers"].as_array().unwrap().len(), 3);
}
