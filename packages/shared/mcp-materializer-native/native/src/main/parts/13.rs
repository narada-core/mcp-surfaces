#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn fixture(root: &Path) -> MaterializationInput {
        fs::write(root.join("narada-mcp-materializer.exe"), b"materializer").unwrap();
        fs::write(root.join("narada-mcp-runtime.exe"), b"runtime").unwrap();
        fs::write(root.join("child.exe"), b"child").unwrap();
        fs::write(root.join("matrix.json"), b"matrix").unwrap();
        fs::write(root.join("carrier-contract.json"), b"contract").unwrap();
        let manifest = json!({
            "schema":"narada.workspace_artifact_manifest.v1",
            "workspace_root":path_text(root),
            "manifest_fingerprint":"a".repeat(64),
            "packages":[],
            "artifacts":[]
        });
        let manifest_bytes = pretty_json(&manifest).unwrap();
        fs::write(root.join("manifest.json"), &manifest_bytes).unwrap();
        let artifacts = [
            ("narada-mcp-materializer.exe", b"materializer".as_slice()),
            ("narada-mcp-runtime.exe", b"runtime".as_slice()),
            ("child.exe", b"child".as_slice()),
        ]
        .into_iter()
        .map(|(name, bytes)| {
            json!({
                "path":path_text(&root.join(name)),
                "size":bytes.len(),
                "sha256":format!("sha256:{}",sha256(bytes)),
            })
        })
        .collect::<Vec<_>>();
        let build_set_unsigned = json!({
            "schema":"narada.artifact_build_set.v1",
            "assurance":"declared_isolated_closure",
            "workspace_root":path_text(root),
            "workspace_manifest_path":path_text(&root.join("manifest.json")),
            "workspace_manifest_fingerprint":"a".repeat(64),
            "workspace_manifest_bytes_digest":format!("sha256:{}",sha256(&manifest_bytes)),
            "source_closure_digest":format!("sha256:{}", "0".repeat(64)),
            "toolchain":{},
            "ambient_input_classes":[],
            "required_references":[],
            "artifacts":artifacts,
        });
        let build_set_digest = format!(
            "sha256:{}",
            canonical_json_sha256(&build_set_unsigned).unwrap()
        );
        let mut build_set = build_set_unsigned;
        build_set["build_set_digest"] = json!(build_set_digest);
        build_set["generated_at"] = json!("2026-08-12T00:00:00Z");
        fs::write(
            root.join("artifact-build-set.json"),
            pretty_json(&build_set).unwrap(),
        )
        .unwrap();
        MaterializationInput {
            schema: INPUT_SCHEMA.into(),
            workspace_root: root.into(),
            carrier_contract_path: root.join("carrier-contract.json"),
            carrier_contract_fingerprint: "e".repeat(64),
            artifact_manifest_path: root.join("manifest.json"),
            artifact_manifest_fingerprint: Some("a".repeat(64)),
            artifact_build_set_path: root.join("artifact-build-set.json"),
            artifact_build_set_fingerprint: build_set_digest,
            runtime_profile_kind: "native".into(),
            runtime_implementation_matrix_path: root.join("matrix.json"),
            runtime_implementation_matrix_fingerprint: "b".repeat(64),
            registrar_entrypoint: root.join("narada-mcp-materializer.exe"),
            registrar_fingerprint: Some("c".repeat(64)),
            proxy_implementation: "native".into(),
            proxy_entrypoint: root.join("narada-mcp-runtime.exe"),
            proxy_fingerprint: Some("d".repeat(64)),
            installed_carrier_index_path: root.join("installed-carriers.json"),
            carriers: vec![CarrierInput {
                carrier_id: "codex-test".into(),
                carrier_kind: CarrierKind::Codex,
                config_path: root.join("config.toml"),
                codex_plugin_overrides: BTreeMap::new(),
                trust_projects: vec![],
                binding_admission_path: None,
                binding_admission_envelope: None,
                servers: vec![ServerInput {
                    binding_id: Some("fixture-binding".into()),
                    source_server_key: Some("narada-site-fixture".into()),
                    name: "narada-test".into(),
                    command: path_text(&root.join("narada-mcp-runtime.exe")),
                    args: vec![
                        "proxy".into(),
                        "--surface-id".into(),
                        "fixture".into(),
                        "--child-command".into(),
                        path_text(&root.join("child.exe")),
                        "--artifact-manifest".into(),
                        path_text(&root.join("manifest.json")),
                        "--runtime-contract-version".into(),
                        CONTRACT_VERSION.to_string(),
                        "--entrypoint".into(),
                        path_text(&root.join("child.exe")),
                        "--carrier-id".into(),
                        "codex-test".into(),
                        "--carrier-kind".into(),
                        "codex".into(),
                        "--registrar-command".into(),
                        path_text(&root.join("narada-mcp-materializer.exe")),
                        "--registrar-entrypoint".into(),
                        path_text(&root.join("narada-mcp-materializer.exe")),
                        "--materialization-sidecar".into(),
                        path_text(&root.join("config.toml.narada-generation.json")),
                        "--".into(),
                    ],
                    env_vars: vec![],
                    enabled: true,
                    approval_mode: Some("approve".into()),
                    startup_timeout_sec: Some(60),
                    tools: vec![ToolInput {
                        name: "test_show".into(),
                        approval_mode: "approve".into(),
                    }],
                }],
            }],
        }
    }

    #[test]
    fn accepts_modern_kimi_route_to_modern_only_registrar() {
        let root = tempdir().unwrap();
        let mut input = fixture(root.path());
        input.carriers[0].carrier_id = "kimi-test".into();
        input.carriers[0].carrier_kind = CarrierKind::Kimi;
        input.carriers[0].servers[0].name = "mcp-registrar".into();

        let carrier = &input.carriers[0];
        validate_protocol_route(carrier, &carrier.servers[0])
            .expect("modern Kimi route must reach the modern-only Registrar");
    }

    #[test]
    fn emits_pi_extension_with_only_bootstrap_servers() {
        let root = tempdir().unwrap();
        let mut input = fixture(root.path());
        let carrier = &mut input.carriers[0];
        carrier.carrier_kind = CarrierKind::Pi;
        carrier.servers[0].name = "mcp-loader".into();
        carrier.servers[0].startup_timeout_sec = Some(7);
        let mut lazy = carrier.servers[0].clone();
        lazy.name = "narada-marici-git".into();
        carrier.servers.push(lazy);
        let source = String::from_utf8(emit_carrier(carrier).unwrap()).unwrap();
        assert!(source.contains("export default function naradaMcpCarrier"));
        assert!(source.contains("\"name\":\"mcp-loader\""));
        assert!(source.contains("\"startupTimeoutMs\":7000"));
        assert!(!source.contains("narada-marici-git"));
        assert!(source.contains("tools/list"));
        assert!(source.contains("pi.registerTool"));
        assert!(source.contains("Array.isArray(result?.content) && result.content.length > 0"));
        assert!(source.contains("result?.structuredContent !== undefined"));
        assert!(source.contains("MAX_BOOTSTRAP_SCHEMA_CHARS"));
        assert!(source.contains("pi.registerCommand(\"marici-identity\""));
        assert!(source.contains("Mechanically admitted Narada identity"));
        assert!(source.contains("NARADA_SESSION_IDENTITY_ENTRY"));
        assert!(source.contains("use /narada-identity set marici.Name"));
        assert!(!source.contains("__NARADA_PI_MCP_SERVERS__"));
    }

    #[test]
    fn pi_projection_is_whole_document_managed() {
        let root = tempdir().unwrap();
        let mut input = fixture(root.path());
        input.carriers[0].carrier_kind = CarrierKind::Pi;
        let emitted = emit_carrier(&input.carriers[0]).unwrap();
        let description = describe_config("pi", &emitted, &[]).unwrap();
        assert_eq!(description.managed_projection.scope, "whole_document");
        assert_eq!(description.managed_projection.sha256, sha256(&emitted));
    }

    #[test]
    fn materializes_config_sidecars_and_index() {
        let root = tempdir().unwrap();
        let input = fixture(root.path());
        let result = materialize(input, false).unwrap();
        assert_eq!(result["status"], "committed");
        assert!(root.path().join("config.toml").exists());
        assert!(root
            .path()
            .join("config.toml.narada-generation.json")
            .exists());
        assert!(root
            .path()
            .join("config.toml.narada-runtime-plan.json")
            .exists());
        let index: Value =
            serde_json::from_slice(&fs::read(root.path().join("installed-carriers.json")).unwrap())
                .unwrap();
        assert_eq!(index["carriers"][0]["carrier_id"], "codex-test");
    }

    #[test]
    fn recovery_rolls_back_when_the_commit_pointer_was_not_published() {
        let root = tempdir().unwrap();
        let carrier_root = root.path().join("carrier-state");
        let transaction_root = carrier_root.join("transactions").join("crashed");
        let candidates = transaction_root.join("candidates");
        let preimages = transaction_root.join("preimages");
        fs::create_dir_all(&candidates).unwrap();
        fs::create_dir_all(&preimages).unwrap();
        let target = root.path().join("config.toml");
        let pointer = carrier_root.join("current-bundle.json");
        fs::write(&target, b"candidate").unwrap();
        fs::write(candidates.join("0.bin"), b"candidate").unwrap();
        fs::write(preimages.join("0.bin"), b"preimage").unwrap();
        fs::write(candidates.join("1.bin"), b"pointer").unwrap();
        let journal = json!({
            "schema":"narada.carrier_generation_transaction.v1",
            "transaction_id":"crashed",
            "bundle_id":"bundle",
            "state":"promoting",
            "commit_pointer_path":path_text(&pointer),
            "items":[
                {
                    "order":0,
                    "path":path_text(&target),
                    "candidate_path":path_text(&candidates.join("0.bin")),
                    "candidate_sha256":sha256(b"candidate"),
                    "preimage_path":path_text(&preimages.join("0.bin")),
                    "preimage_sha256":sha256(b"preimage"),
                    "state":"published"
                },
                {
                    "order":1,
                    "path":path_text(&pointer),
                    "candidate_path":path_text(&candidates.join("1.bin")),
                    "candidate_sha256":sha256(b"pointer"),
                    "preimage_path":Value::Null,
                    "preimage_sha256":Value::Null,
                    "state":"prepared"
                }
            ]
        });
        fs::write(
            transaction_root.join("journal.json"),
            pretty_json(&journal).unwrap(),
        )
        .unwrap();
        let recovered = recover_pending_transactions(&carrier_root).unwrap();
        assert_eq!(recovered["recovered"][0]["resolution"], "aborted");
        assert_eq!(fs::read(&target).unwrap(), b"preimage");
        assert!(!pointer.exists());
    }

    #[test]
    fn recovery_rolls_forward_when_the_commit_pointer_was_published() {
        let root = tempdir().unwrap();
        let carrier_root = root.path().join("carrier-state");
        let transaction_root = carrier_root.join("transactions").join("crashed");
        let candidates = transaction_root.join("candidates");
        let preimages = transaction_root.join("preimages");
        fs::create_dir_all(&candidates).unwrap();
        fs::create_dir_all(&preimages).unwrap();
        let target = root.path().join("config.toml");
        let pointer = carrier_root.join("current-bundle.json");
        fs::write(&target, b"preimage").unwrap();
        fs::write(candidates.join("0.bin"), b"candidate").unwrap();
        fs::write(preimages.join("0.bin"), b"preimage").unwrap();
        fs::write(candidates.join("1.bin"), b"pointer").unwrap();
        fs::create_dir_all(pointer.parent().unwrap()).unwrap();
        fs::write(&pointer, b"pointer").unwrap();
        let journal = json!({
            "schema":"narada.carrier_generation_transaction.v1",
            "transaction_id":"crashed",
            "bundle_id":"bundle",
            "state":"promoting",
            "commit_pointer_path":path_text(&pointer),
            "items":[
                {
                    "order":0,
                    "path":path_text(&target),
                    "candidate_path":path_text(&candidates.join("0.bin")),
                    "candidate_sha256":sha256(b"candidate"),
                    "preimage_path":path_text(&preimages.join("0.bin")),
                    "preimage_sha256":sha256(b"preimage"),
                    "state":"prepared"
                },
                {
                    "order":1,
                    "path":path_text(&pointer),
                    "candidate_path":path_text(&candidates.join("1.bin")),
                    "candidate_sha256":sha256(b"pointer"),
                    "preimage_path":Value::Null,
                    "preimage_sha256":Value::Null,
                    "state":"published"
                }
            ]
        });
        fs::write(
            transaction_root.join("journal.json"),
            pretty_json(&journal).unwrap(),
        )
        .unwrap();
        let recovered = recover_pending_transactions(&carrier_root).unwrap();
        assert_eq!(recovered["recovered"][0]["resolution"], "committed");
        assert_eq!(fs::read(&target).unwrap(), b"candidate");
        assert_eq!(fs::read(&pointer).unwrap(), b"pointer");
    }

    #[test]
    fn identical_materialization_inputs_reuse_the_semantic_bundle_identity() {
        let root = tempdir().unwrap();
        let first = materialize(fixture(root.path()), false).unwrap();
        let second = materialize(fixture(root.path()), false).unwrap();
        assert_eq!(first["bundle_id"], second["bundle_id"]);
    }

    #[test]
    fn publication_lock_refuses_a_concurrent_writer() {
        let root = tempdir().unwrap();
        let first = acquire_publication_lock(root.path()).unwrap();
        let second = acquire_publication_lock(root.path()).unwrap_err();
        assert_eq!(second.code, "materializer_publication_locked");
        drop(first);
        acquire_publication_lock(root.path()).unwrap();
    }

    #[test]
    fn invalid_input_writes_nothing() {
        let root = tempdir().unwrap();
        let mut input = fixture(root.path());
        input.carriers[0].servers[0].name = "bad name".into();
        assert_eq!(
            materialize(input, false).unwrap_err().code,
            "materializer_identifier_invalid"
        );
        assert!(!root.path().join("config.toml").exists());
        assert!(!root.path().join("installed-carriers.json").exists());
        assert!(!root
            .path()
            .join(".narada")
            .join("carrier-transactions")
            .exists());
    }

    #[test]
    fn publication_failure_restores_every_previous_file() {
        let root = tempdir().unwrap();
        let mut input = fixture(root.path());
        fs::write(
            root.path().join("config.toml"),
            b"model = \"operator-owned-before\"\n",
        )
        .unwrap();
        fs::write(root.path().join("blocked-parent"), b"not-a-directory\n").unwrap();
        input.carriers.push(CarrierInput {
            carrier_id: "kimi-test".into(),
            carrier_kind: CarrierKind::Kimi,
            config_path: root.path().join("blocked-parent").join("mcp.json"),
            codex_plugin_overrides: BTreeMap::new(),
            trust_projects: vec![],
            binding_admission_path: None,
            binding_admission_envelope: None,
            servers: vec![],
        });

        let failure = materialize(input, false).unwrap_err();

        assert_eq!(failure.code, "materializer_transaction_failed");
        assert_eq!(
            fs::read(root.path().join("config.toml")).unwrap(),
            b"model = \"operator-owned-before\"\n"
        );
        assert!(!root
            .path()
            .join("config.toml.narada-generation.json")
            .exists());
        assert!(!root
            .path()
            .join("config.toml.narada-runtime-plan.json")
            .exists());
        assert!(!root.path().join("installed-carriers.json").exists());
    }

    #[test]
    fn malformed_existing_codex_toml_fails_before_writes() {
        let root = tempdir().unwrap();
        let input = fixture(root.path());
        fs::write(root.path().join("config.toml"), b"[broken\n").unwrap();

        let failure = materialize(input, false).unwrap_err();

        assert_eq!(failure.code, "materializer_codex_merge_failed");
        assert_eq!(
            fs::read(root.path().join("config.toml")).unwrap(),
            b"[broken\n"
        );
        assert!(!root
            .path()
            .join("config.toml.narada-generation.json")
            .exists());
    }
}
