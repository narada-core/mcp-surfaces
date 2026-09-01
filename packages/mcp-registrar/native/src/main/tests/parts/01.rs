
    use super::*;

    fn embedded_contract() -> Value {
        decode_contract().unwrap()
    }

    #[test]
    fn embedded_native_contract_is_valid() {
        validate_contract(&embedded_contract()).unwrap();
    }

    #[test]
    fn semantic_registry_equality_preserves_the_existing_generation() {
        let suffix = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = env::temp_dir().join(format!(
            "narada-registry-semantic-equality-{}-{suffix}.json",
            std::process::id()
        ));
        let existing = json!({"generated_at":"2026-01-01T00:00:00Z","surfaces":[{"id":"one"}]});
        fs::write(&path, serde_json::to_vec(&existing).unwrap()).unwrap();
        let mut equivalent = json!({"generated_at":"2026-02-01T00:00:00Z","surfaces":[{"id":"one"}]});
        assert!(!preserve_existing_registry_when_semantically_equal(&path, &mut equivalent));
        assert_eq!(equivalent, existing);
        let mut changed = json!({"generated_at":"2026-02-01T00:00:00Z","surfaces":[{"id":"two"}]});
        assert!(preserve_existing_registry_when_semantically_equal(&path, &mut changed));
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn structured_command_python_admission_is_complete_and_idempotent() {
        let mut contract = embedded_contract();
        admit_structured_command_python(&mut contract);
        admit_structured_command_python(&mut contract);

        let surface = contract
            .pointer("/read_models/registrar_surface_list/items")
            .and_then(Value::as_array)
            .and_then(|items| items.iter().find(|item| item["id"] == "structured-command"))
            .expect("structured-command surface");
        let count_python_admissions = |args: &Value| {
            args.as_array()
                .expect("argument array")
                .windows(2)
                .filter(|pair| pair[0] == "--allow-command" && pair[1] == "python")
                .count()
        };

        assert_eq!(count_python_admissions(&surface["args"]), 1);
        assert_eq!(
            count_python_admissions(&surface["projections"][0]["args"]),
            1
        );
        assert_eq!(
            count_python_admissions(&surface["descriptor"]["projections"][0]["transport"]["args"]),
            1
        );
        assert_eq!(
            surface["descriptor_digest"],
            sha256_text(&canonical_json(&surface["descriptor"]))
        );
    }

    #[test]
    fn runtime_matrix_path_resolves_from_nested_worktree() {
        let root = env::temp_dir().join(format!(
            "narada-registrar-runtime-matrix-{}",
            std::process::id()
        ));
        let source_root = root.join("src");
        let worktree_root = source_root.join("mcp-surfaces/.worktrees/worker");
        let matrix = source_root.join(
            "narada/packages/operator-surface-runtime-contract/contracts/runtime-implementation-matrix.json",
        );
        fs::create_dir_all(&worktree_root).expect("worktree");
        fs::create_dir_all(matrix.parent().expect("matrix parent")).expect("narada source");
        fs::write(&matrix, b"{}").expect("matrix file");

        assert_eq!(
            runtime_implementation_matrix_path(&worktree_root).expect("matrix path"),
            matrix
        );

        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn embedded_site_local_descriptor_normalizes_into_catalog_shape() {
        let server = json!({
            "surface_projection": {
                "projection_id": "default",
                "exposed_tools": ["local_read", "local_write"],
                "surface_descriptor": {
                    "surface_id": "local-domain",
                    "tools": [
                        {"name": "local_read", "effect": {"class": "read"}},
                        {"name": "local_write", "effect": {"class": "local_write"}}
                    ]
                }
            }
        });
        let catalog =
            embedded_site_local_catalog(&server, "local-domain").expect("site-local catalog");
        assert_eq!(catalog["id"], "local-domain");
        assert_eq!(catalog["projections"][0]["id"], "default");
        assert_eq!(catalog["tools"], json!(["local_read", "local_write"]));
        assert_eq!(catalog["descriptor"]["surface_id"], "local-domain");
    }

    #[test]
    fn embedded_site_local_descriptor_cannot_claim_another_surface() {
        let server = json!({
            "surface_projection": {
                "projection_id": "default",
                "surface_descriptor": {"surface_id": "other-domain"}
            }
        });
        assert!(embedded_site_local_catalog(&server, "local-domain").is_none());
    }

    #[test]
    fn embedded_git_surface_is_injected_into_site_bound_sessions() {
        let contract = embedded_contract();
        let git = contract
            .pointer("/read_models/registrar_surface_list/items")
            .and_then(Value::as_array)
            .and_then(|items| items.iter().find(|item| item["id"] == "git"))
            .expect("git surface");
        let projection = git["projections"]
            .as_array()
            .and_then(|items| items.iter().find(|item| item["id"] == "default"))
            .expect("git runtime projection");
        let descriptor_projection = git["descriptor"]["projections"]
            .as_array()
            .and_then(|items| items.iter().find(|item| item["id"] == "default"))
            .expect("git descriptor projection");
        assert_eq!(projection["default_injection"], "enabled");
        assert_eq!(descriptor_projection["default_injection"], "enabled");
    }

    #[test]
    fn duplicate_native_contract_records_are_rejected() {
        let mut contract = embedded_contract();
        let duplicate = contract["tools"][0].clone();
        contract["tools"].as_array_mut().unwrap().push(duplicate);
        assert!(validate_contract(&contract)
            .unwrap_err()
            .starts_with("tools_name_duplicate:"));
    }

    #[test]
    fn unsupported_native_contract_schema_is_rejected() {
        let mut contract = embedded_contract();
        contract["schema"] = json!("legacy");
        assert_eq!(
            validate_contract(&contract).unwrap_err(),
            "unsupported_schema"
        );
    }

    #[test]
    fn native_epistemic_catalog_matches_live_surface_tools() {
        let mut contract = embedded_contract();
        extend_epistemic_catalog(&mut contract);
        let surface = contract
            .pointer("/read_models/registrar_surface_list/items")
            .and_then(Value::as_array)
            .and_then(|items| items.iter().find(|item| item["id"] == "epistemic-graph"))
            .expect("epistemic catalog");
        let names = surface["tools"].as_array().expect("tool names");
        assert_eq!(names.len(), 21);
        for required in [
            "epistemic_graph_snapshot",
            "epistemic_graph_sequence_create",
            "epistemic_graph_sequence_status",
            "epistemic_graph_sequence_list",
            "epistemic_graph_sequence_claim_next",
            "epistemic_graph_sequence_claims",
            "epistemic_graph_query_batch",
            "epistemic_graph_source_inspect",
            "epistemic_graph_capture_sources",
            "epistemic_graph_submit_review_admit",
            "epistemic_graph_proposal_read",
            "epistemic_graph_proposal_resubmit",
        ] {
            assert!(names.iter().any(|name| name == required), "{required}");
        }
    }

    #[test]
    fn native_registry_rebinding_covers_every_distribution_artifact_class() {
        for (surface_id, projection_id, executable) in [
            ("local-filesystem", "default", "narada-mcp-runtime.exe"),
            ("mcp-loader", "default", "narada-mcp-loader.exe"),
            ("agent-context", "default", "narada-agent-context-mcp.exe"),
            ("task-lifecycle", "stdio", "narada-task-lifecycle-mcp.exe"),
            ("mcp-registrar", "default", "narada-mcp-registrar.exe"),
            ("surface-feedback", "default", "narada-mcp-surfaces.exe"),
            ("epistemic-graph", "default", "narada-ledger-domain.exe"),
        ] {
            let (_, artifact) = native_surface_artifact(surface_id, projection_id)
                .unwrap_or_else(|| panic!("missing native mapping for {surface_id}"));
            assert_eq!(artifact, executable, "{surface_id}");
        }
    }

    #[test]
    fn native_contract_repairs_guidance_schema_and_validation_entrypoint() {
        let mut contract = embedded_contract();
        let declared = contract["runtime_bindings"]["registrar_entrypoint"]
            .as_str()
            .unwrap()
            .to_string();
        let current = "C:/native/narada-mcp-registrar.exe";
        repair_native_contract(&mut contract, &declared, current);
        assert!(!contract["guidance"]
            .to_string()
            .contains("pnpm materialize:carrier"));
        assert!(contract["guidance"]
            .to_string()
            .contains("cargo native-release"));
        let list_tool = contract["tools"]
            .as_array()
            .unwrap()
            .iter()
            .find(|tool| tool["name"] == "registrar_surface_list")
            .unwrap();
        assert_eq!(
            list_tool["inputSchema"]["properties"]["compact"]["default"],
            true
        );
        for plan in contract["read_models"]["registrar_carrier_validation_plans"]
            .as_object()
            .unwrap()
            .values()
        {
            for server in plan["servers"].as_array().into_iter().flatten() {
                if server["surface_id"] == "mcp-registrar" {
                    assert_eq!(server["entrypoint"], current);
                }
            }
        }
    }

    #[test]
    fn surface_inventory_is_compact_and_paginated_by_default() {
        let contract = embedded_contract();
        let first = surface_list(&contract, &json!({"limit":1}));
        let second = surface_list(&contract, &json!({"limit":1,"offset":1}));
        assert_eq!(first["compact"], true);
        assert_eq!(first["returned"], 1);
        assert_eq!(first["has_more"], true);
        assert_eq!(first["next_offset"], 1);
        assert!(first["items"][0].get("descriptor").is_none());
        assert_ne!(first["items"][0]["id"], second["items"][0]["id"]);
        let full = surface_list(&contract, &json!({"limit":1,"compact":false}));
        assert!(full["items"][0].get("descriptor").is_some());
    }

