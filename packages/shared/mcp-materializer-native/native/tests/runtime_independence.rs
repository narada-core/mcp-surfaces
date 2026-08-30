use narada_mcp_materialization_contract::canonical_json_sha256;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use tempfile::tempdir;

fn write_fixture_build_set(
    manifest_path: &Path,
    manifest_fingerprint: &str,
    build_set_path: &Path,
    references: &[PathBuf],
) -> String {
    let manifest_bytes = fs::read(manifest_path).unwrap();
    let artifacts = references
        .iter()
        .map(|path| {
            let bytes = fs::read(path).unwrap();
            json!({
                "path": path,
                "sha256": format!("sha256:{}", hex::encode(Sha256::digest(&bytes))),
                "size": bytes.len(),
            })
        })
        .collect::<Vec<_>>();
    let mut build_set = json!({
        "schema": "narada.artifact_build_set.v1",
        "assurance": "declared_isolated_closure",
        "workspace_manifest_path": manifest_path,
        "workspace_manifest_fingerprint": manifest_fingerprint,
        "workspace_manifest_bytes_digest": format!(
            "sha256:{}",
            hex::encode(Sha256::digest(&manifest_bytes))
        ),
        "artifacts": artifacts,
        "toolchain": {
            "schema": "narada.artifact_toolchain_evidence.v2",
            "node": "fixture",
            "pnpm": "fixture",
            "rustc": "fixture",
            "cargo": "fixture"
        }
    });
    let digest = format!("sha256:{}", canonical_json_sha256(&build_set).unwrap());
    build_set["build_set_digest"] = json!(digest);
    fs::write(
        build_set_path,
        serde_json::to_vec_pretty(&build_set).unwrap(),
    )
    .unwrap();
    digest
}

#[test]
fn publishes_itself_to_a_content_addressed_immutable_location_without_javascript() {
    let root = tempdir().unwrap();
    let artifact_root = root.path().join("dist/native");
    let output = Command::new(env!("CARGO_BIN_EXE_narada-mcp-materializer"))
        .env_clear()
        .arg("publish")
        .arg("--artifact-root")
        .arg(&artifact_root)
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );
    let result: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["status"], "published");
    let executable = result["executable"].as_str().unwrap();
    assert!(std::path::Path::new(executable).exists());
    assert!(!executable.contains("target"));
    let pointer: Value =
        serde_json::from_slice(&fs::read(artifact_root.join("current.json")).unwrap()).unwrap();
    assert_eq!(
        pointer["schema"],
        "narada.mcp_materializer.native_artifact_pointer.v1"
    );
    assert_eq!(pointer["build_fingerprint"], result["build_fingerprint"]);
}

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
        (
            "pi-test",
            "pi",
            root.path().join("pi/agent/extensions/narada-mcp/index.ts"),
        ),
    ];
    let input_path = root.path().join("materialization-input.json");
    let contract_path = root.path().join("carrier-contract.json");
    let contract_bytes = b"{\"schema\":\"narada.native_carrier_contract.v2\"}\n";
    fs::write(&contract_path, contract_bytes).unwrap();
    let contract_fingerprint = hex::encode(Sha256::digest(contract_bytes));
    let manifest_path = root.path().join("workspace-artifact-manifest.json");
    let manifest_fingerprint = "a".repeat(64);
    fs::write(
        &manifest_path,
        serde_json::to_vec_pretty(&json!({
            "schema":"narada.workspace_artifact_manifest.v1",
            "manifest_fingerprint":manifest_fingerprint
        }))
        .unwrap(),
    )
    .unwrap();
    let registrar = root.path().join("narada-mcp-materializer.exe");
    let proxy = root.path().join("narada-mcp-runtime.exe");
    let filesystem = root.path().join("filesystem.exe");
    for path in [&registrar, &filesystem] {
        fs::write(path, format!("fixture:{}", path.display())).unwrap();
    }
    fs::copy(env!("CARGO_BIN_EXE_narada-mcp-materializer"), &proxy).unwrap();
    let build_set_path = root.path().join("artifact-build-set.json");
    let build_set_fingerprint = write_fixture_build_set(
        &manifest_path,
        &manifest_fingerprint,
        &build_set_path,
        &[registrar.clone(), proxy.clone(), filesystem.clone()],
    );
    let input = json!({
        "schema": "narada.carrier_materialization_input.v1",
        "workspace_root": root.path(),
        "carrier_contract_path": contract_path,
        "carrier_contract_fingerprint": contract_fingerprint,
        "artifact_manifest_path": manifest_path,
        "artifact_manifest_fingerprint": manifest_fingerprint,
        "artifact_build_set_path": build_set_path,
        "artifact_build_set_fingerprint": build_set_fingerprint,
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
                "args": [
                    "proxy", "--surface-id", "local-filesystem",
                    "--child-command", root.path().join("filesystem.exe"),
                    "--artifact-manifest", root.path().join("workspace-artifact-manifest.json"),
                    "--runtime-contract-version", "8",
                    "--entrypoint", root.path().join("filesystem.exe"),
                    "--carrier-id", id,
                    "--carrier-kind", kind,
                    "--registrar-command", root.path().join("narada-mcp-materializer.exe"),
                    "--registrar-entrypoint", root.path().join("narada-mcp-materializer.exe"),
                    "--materialization-sidecar", format!("{}.narada-generation.json", path.to_string_lossy()),
                    "--"
                ],
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
    assert_eq!(result["status"], "committed");
    assert_eq!(result["carrier_count"], 4);
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
    assert!(codex.contains("[mcp_servers.narada-site-test-local-filesystem]"));
    assert!(codex.contains("default_tools_approval_mode = \"approve\""));
    assert!(!codex
        .lines()
        .any(|line| line == "approval_mode = \"approve\""));
    assert!(!codex.contains(".tools.fs_read]"));

    let kimi: Value = serde_json::from_slice(&fs::read(&paths[1].2).unwrap()).unwrap();
    let kimi_server = &kimi["mcpServers"]["narada-site-test-local-filesystem"];
    assert_eq!(kimi_server["transport"], "stdio");
    assert_eq!(kimi_server["protocolVersion"], "2026-07-28");
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

    let pi = fs::read_to_string(&paths[3].2).unwrap();
    assert!(pi.contains("export default function naradaMcpCarrier"));
    assert!(!pi.contains("\"name\":\"narada-site-test-local-filesystem\""));
    assert!(pi.contains("Site capabilities are lazy through mcp-loader"));
    assert!(pi.contains("pi.registerTool"));

    let index: Value =
        serde_json::from_slice(&fs::read(root.path().join("installed-carriers.json")).unwrap())
            .unwrap();
    assert_eq!(index["carriers"].as_array().unwrap().len(), 4);
}

#[test]
fn derives_all_carriers_from_declared_site_capabilities_without_javascript() {
    let root = tempdir().unwrap();
    let workspace = root.path().join("workspace");
    let home = root.path().join("home");
    let registry_path = home.join("Narada/.narada/capabilities/mcp-surfaces.json");
    let matrix_path = root.path().join("runtime-implementation-matrix.json");
    let index_path = home.join(".narada/carriers/installed-carriers.json");
    let contract_path = home.join("Narada/.narada/capabilities/carrier-materialization.json");
    let proxy = root.path().join("narada-mcp-runtime.exe");
    fs::create_dir_all(workspace.join(".ai/runtime")).unwrap();
    fs::create_dir_all(registry_path.parent().unwrap()).unwrap();
    assert!(!workspace.join("node_modules").exists());
    assert!(!workspace.join("node_modules").exists());
    fs::copy(env!("CARGO_BIN_EXE_narada-mcp-materializer"), &proxy).unwrap();
    fs::write(&matrix_path, b"{\"schema\":\"fixture\"}\n").unwrap();
    fs::write(
        workspace.join(".ai/runtime/workspace-artifact-manifest.json"),
        serde_json::to_vec_pretty(&json!({
            "schema": "narada.workspace_artifact_manifest.v1",
            "manifest_fingerprint": "e".repeat(64)
        }))
        .unwrap(),
    )
    .unwrap();
    let surface_ids = [
        "agent-context",
        "local-filesystem",
        "mcp-loader",
        "task-lifecycle",
        "surface-feedback",
    ];
    let mut artifact_references = vec![
        proxy.clone(),
        PathBuf::from(env!("CARGO_BIN_EXE_narada-mcp-materializer")),
    ];
    for surface_id in surface_ids {
        let child = root.path().join(format!("{surface_id}.exe"));
        fs::write(&child, format!("fixture:{surface_id}")).unwrap();
        artifact_references.push(child);
    }
    let manifest_path = workspace.join(".ai/runtime/workspace-artifact-manifest.json");
    write_fixture_build_set(
        &manifest_path,
        &"e".repeat(64),
        &workspace.join(".ai/runtime/artifact-build-set.json"),
        &artifact_references,
    );
    let registry = json!({
        "schema": "narada.site.capabilities.mcp_surfaces.v1",
        "site_id": "andrey-user",
        "surfaces": surface_ids.iter().map(|surface_id| json!({
            "catalog_surface_id": surface_id,
            "injection_scope": "user_site",
            "server_name": format!("narada-site-andrey-user-{surface_id}"),
            "registered_live_tools": [format!("{}_guidance", surface_id.replace('-', "_"))],
            "runtime_binding": {
                "proxy_implementation": "native",
                "transport": {
                    "type": "stdio",
                    "command": proxy,
                    "args": [
                        "proxy", "--surface-id", surface_id,
                        "--child-command", root.path().join(format!("{surface_id}.exe")),
                        "--artifact-manifest", workspace.join(".ai/runtime/workspace-artifact-manifest.json"),
                        "--runtime-contract-version", "8",
                        "--entrypoint", root.path().join(format!("{surface_id}.exe")),
                        "--"
                    ]
                }
            },
            "surface_projection": {
                "projection_id": "default",
                "surface_descriptor": {
                    "metadata": { "codex_startup_timeout_sec": 60 },
                    "projections": [{
                        "id": "default",
                        "transport": { "env": ["NARADA_AGENT_ID"] }
                    }]
                }
            }
        })).collect::<Vec<_>>()
    });
    fs::write(
        &registry_path,
        serde_json::to_vec_pretty(&registry).unwrap(),
    )
    .unwrap();
    let second_registry_path = home.join("second-site/.narada/capabilities/mcp-surfaces.json");
    fs::create_dir_all(second_registry_path.parent().unwrap()).unwrap();
    let second_registry = json!({
        "schema": "narada.site.capabilities.mcp_surfaces.v1",
        "site_id": "second-site",
        "surfaces": [{
            "catalog_surface_id": "local-filesystem",
            "injection_scope": "local_site",
            "server_name": "narada-site-second-site-local-filesystem",
            "registered_live_tools": ["fs_read"],
            "runtime_binding": {
                "proxy_implementation": "native",
                "transport": {
                    "type": "stdio",
                    "command": proxy,
                    "args": [
                        "proxy", "--surface-id", "local-filesystem",
                        "--child-command", root.path().join("local-filesystem.exe"),
                        "--artifact-manifest", workspace.join(".ai/runtime/workspace-artifact-manifest.json"),
                        "--runtime-contract-version", "8",
                        "--entrypoint", root.path().join("local-filesystem.exe"),
                        "--"
                    ]
                }
            },
            "surface_projection": {
                "projection_id": "default",
                "surface_descriptor": {
                    "metadata": { "codex_startup_timeout_sec": 60 },
                    "projections": [{"id":"default","transport":{"env":["NARADA_AGENT_ID"]}}]
                }
            }
        }]
    });
    fs::write(
        &second_registry_path,
        serde_json::to_vec_pretty(&second_registry).unwrap(),
    )
    .unwrap();
    let mut contract = json!({
        "schema": "narada.native_carrier_contract.v2",
        "sites": [
            {
                "site_id": "andrey-user",
                "registry_path": registry_path,
                "admit_local_bindings": true,
                "surface_ids": surface_ids
            },
            {
                "site_id": "second-site",
                "registry_path": second_registry_path,
                "surface_ids": ["local-filesystem"]
            }
        ],
        "carriers": [
            {"carrier_id":"opencode-test","carrier_kind":"opencode","config_relative_path":".config/opencode/opencode.jsonc"},
            {"carrier_id":"kimi-test","carrier_kind":"kimi","config_relative_path":".kimi/mcp.json"},
            {"carrier_id":"codex-test","carrier_kind":"codex","config_relative_path":".codex/config.toml"}
        ]
    });
    fs::write(
        &contract_path,
        serde_json::to_vec_pretty(&contract).unwrap(),
    )
    .unwrap();

    let collision_output = Command::new(env!("CARGO_BIN_EXE_narada-mcp-materializer"))
        .env_clear()
        .arg("materialize-site")
        .arg("--contract")
        .arg(&contract_path)
        .arg("--workspace-root")
        .arg(&workspace)
        .arg("--home")
        .arg(&home)
        .arg("--matrix")
        .arg(&matrix_path)
        .arg("--installed-index")
        .arg(&index_path)
        .output()
        .unwrap();

    assert!(!collision_output.status.success());
    let collision_stderr = String::from_utf8_lossy(&collision_output.stderr);
    assert!(
        collision_stderr.contains("materializer_surface_binding_conflict"),
        "unexpected collision failure: {collision_stderr}"
    );

    contract["sites"][1]["surface_ids"] = json!([]);
    contract["sites"][1]["admit_local_bindings"] = json!(true);
    fs::write(
        &contract_path,
        serde_json::to_vec_pretty(&contract).unwrap(),
    )
    .unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_narada-mcp-materializer"))
        .env_clear()
        .arg("materialize-site")
        .arg("--contract")
        .arg(&contract_path)
        .arg("--workspace-root")
        .arg(&workspace)
        .arg("--home")
        .arg(&home)
        .arg("--matrix")
        .arg(&matrix_path)
        .arg("--installed-index")
        .arg(&index_path)
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );
    let result: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["carrier_count"], 3);
    let codex = fs::read_to_string(home.join(".codex/config.toml")).unwrap();
    assert_eq!(codex.matches("[mcp_servers.").count(), 5);
    assert!(!codex.contains(".tools."));
    for surface_id in surface_ids {
        assert!(codex.contains(&format!("[mcp_servers.{surface_id}]")));
    }
    assert!(!codex.contains("narada-site-andrey-user-"));
    assert!(codex.matches("--allowed-site-root").count() >= 2);
    let second_site_root = second_registry_path
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .parent()
        .unwrap();
    let encoded_second_site_root = second_site_root.to_string_lossy().replace('\\', "/");
    assert!(
        codex.contains(&encoded_second_site_root),
        "missing admitted Site root in loader args: {codex}"
    );

    let admission: Value = serde_json::from_slice(
        &fs::read(home.join(".codex/config.toml.narada-binding-admission.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(
        admission["authority_context"]["schema"],
        "narada.carrier_authority_context.v1"
    );
    assert_eq!(admission["authority_context"]["identity"]["status"], "anonymous");
    assert_eq!(
        admission["authority_context"]["binding_activation"],
        "capability_governed"
    );
    assert!(admission["bindings"]
        .as_array()
        .unwrap()
        .iter()
        .any(|binding| {
            binding["binding_id"] == "second-site-local-filesystem"
                && binding["authority_locus"]["site_id"] == "second-site"
        }));

    let installed: Value = serde_json::from_slice(&fs::read(&index_path).unwrap()).unwrap();
    assert_eq!(installed["carriers"].as_array().unwrap().len(), 3);

    let verification = Command::new(env!("CARGO_BIN_EXE_narada-mcp-materializer"))
        .env_clear()
        .arg("verify-all")
        .arg("--installed-index")
        .arg(&index_path)
        .output()
        .unwrap();
    assert!(
        verification.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&verification.stderr)
    );
    let verification_result: Value = serde_json::from_slice(&verification.stdout).unwrap();
    assert_eq!(verification_result["status"], "current");
    assert_eq!(verification_result["verified_carrier_count"], 3);

    fs::write(
        home.join(".codex/config.toml"),
        b"model = \"operator-preserved\"\n[mcp_servers.corrupted]\ncommand = \"bad\"\n",
    )
    .unwrap();
    let recovery = Command::new(env!("CARGO_BIN_EXE_narada-mcp-materializer"))
        .env_clear()
        .arg("recover-generation")
        .arg("--generation")
        .arg(home.join(".codex/config.toml.narada-generation.json"))
        .output()
        .unwrap();
    assert_eq!(recovery.status.code(), Some(1));
    let recovery_failure: Value = serde_json::from_slice(&recovery.stderr).unwrap();
    assert_eq!(
        recovery_failure["code"],
        "materializer_fresh_process_validation_failed"
    );
    let unchanged_codex = fs::read_to_string(home.join(".codex/config.toml")).unwrap();
    assert!(unchanged_codex.contains("model = \"operator-preserved\""));
    assert!(unchanged_codex.contains("[mcp_servers.corrupted]"));

    assert_eq!(result["restart_required"], true);
}
