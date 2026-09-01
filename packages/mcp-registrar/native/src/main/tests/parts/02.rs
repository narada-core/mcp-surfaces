    #[test]
    fn carrier_inventory_is_compact_self_describing_and_paginated() {
        let contract = embedded_contract();
        let first = carrier_list(&contract, &json!({"limit":1}));
        let second = carrier_list(&contract, &json!({"limit":1,"offset":1}));
        assert_eq!(first["schema"], "narada.registrar.carrier_list.v1");
        assert_eq!(first["status"], "ok");
        assert_eq!(first["compact"], true);
        assert_eq!(first["returned"], 1);
        assert_eq!(first["has_more"], true);
        assert!(first["items"][0].get("site_bindings").is_none());
        assert_ne!(
            first["items"][0]["carrier_id"],
            second["items"][0]["carrier_id"]
        );
        let full = carrier_list(&contract, &json!({"limit":1,"compact":false}));
        assert!(full["items"][0].get("site_bindings").is_some());
    }

    #[test]
    fn every_public_schema_is_named_closed_bounded_and_enforced() {
        let mut contract = embedded_contract();
        extend_epistemic_catalog(&mut contract);
        let declared = contract["runtime_bindings"]["registrar_entrypoint"]
            .as_str()
            .unwrap()
            .to_string();
        repair_native_contract(
            &mut contract,
            &declared,
            "C:/native/narada-mcp-registrar.exe",
        );
        normalize_tool_schemas(&mut contract);
        for tool in contract["tools"].as_array().unwrap() {
            let name = tool["name"].as_str().unwrap();
            let schema = &tool["inputSchema"];
            assert_eq!(schema["title"], format!("{name}.input"));
            assert_eq!(schema["additionalProperties"], false);
            assert!(schema["maxProperties"].as_u64().is_some());
            let failure = validate_tool_call(
                &contract,
                &json!({"name":name,"arguments":{"unexpected":true}}),
            )
            .unwrap_err();
            assert!(
                failure.contains("unknown_field:unexpected"),
                "{name}: {failure}"
            );
        }
    }

    #[test]
    fn native_descriptor_schemas_match_live_worker_and_epistemic_contracts() {
        let mut contract = embedded_contract();
        extend_epistemic_catalog(&mut contract);
        align_native_surface_descriptor_schemas(&mut contract);
        let items = contract.pointer("/read_models/registrar_surface_list/items").and_then(Value::as_array).unwrap();
        let schema = |surface: &str, name: &str| {
            items.iter().find(|item| item["id"] == surface).unwrap()
                .pointer("/descriptor/tools").and_then(Value::as_array).unwrap()
                .iter().find(|tool| tool["name"] == name).unwrap()["input_schema"].clone()
        };
        assert!(schema("worker-delegation", "worker_run").pointer("/properties/constraints/properties/site_root").is_none());
        assert_eq!(
            schema("worker-delegation", "worker_run")
                .pointer("/properties/constraints/properties/wait_for_completion/default")
                .and_then(Value::as_bool),
            Some(false)
        );
        assert_eq!(
            schema("worker-delegation", "worker_run")
                .pointer("/properties/constraints/properties/wait_timeout_ms/maximum")
                .and_then(Value::as_u64),
            Some(300_000)
        );
        let run_constraints = schema("worker-delegation", "worker_run")
            .pointer("/properties/constraints")
            .cloned()
            .unwrap();
        let batch_item = schema("worker-delegation", "worker_run_batch")
            .pointer("/properties/requests/items")
            .cloned()
            .unwrap();
        assert_eq!(batch_item["properties"]["constraints"], run_constraints);
        assert_eq!(batch_item["properties"]["intent"], schema("worker-delegation", "worker_run")["properties"]["intent"]);
        assert!(batch_item.pointer("/properties/constraints/properties/preflight_paths").is_some());
        for legacy_field in ["site_root", "provider", "resumable", "exit_interview", "verification_budget", "test_budget", "required_mcp_tools", "overrides"] {
            assert!(
                batch_item
                    .pointer(&format!("/properties/constraints/properties/{legacy_field}"))
                    .is_none(),
                "legacy catalog-only field remained advertised: {legacy_field}"
            );
        }
        assert_eq!(schema("worker-delegation", "worker_runs_list")["properties"]["compact"]["default"], true);
        assert_eq!(schema("worker-delegation", "worker_runs_list")["properties"]["site_scope"]["default"], "current_site");
        assert_eq!(schema("worker-delegation", "worker_run_wait_batch")["properties"]["timeout_ms"]["maximum"], 180_000);
        assert_eq!(schema("worker-delegation", "worker_run_wait_batch")["properties"]["poll_ms"]["default"], 5_000);
        assert_eq!(schema("worker-delegation", "worker_config_resolve")["additionalProperties"], false);
        assert_eq!(schema("epistemic-graph", "epistemic_graph_guidance")["properties"]["workflow"]["type"], "string");
        let feedback = items.iter().find(|item| item["id"] == "surface-feedback").unwrap();
        let site_reporter = feedback["projections"].as_array().unwrap().iter().find(|projection| projection["id"] == "site-reporter").unwrap();
        assert_eq!(site_reporter["injection_scope"], "local_site");
        assert!(site_reporter["args"].as_array().unwrap().iter().any(|argument| argument == "{user_site_control_root}/feedback"));
        assert!(feedback["descriptor"]["projections"].as_array().unwrap().iter().any(|projection| projection["id"] == "site-reporter"));
        assert_eq!(schema("surface-feedback", "surface_feedback_submit")["required"], json!(["surface_id","kind","summary"]));
        assert!(schema("surface-feedback", "surface_feedback_submit")["properties"]["idempotency_key"].is_object());
    }

    #[test]
    fn protocol_versions_are_honest_and_modern_requests_are_self_describing() {
        for version in ["2024-11-05", "2025-03-26", "2099-01-01"] {
            let initialized = dispatch(
                &json!({"id":1,"method":"initialize","params":{"protocolVersion":version}}),
            );
            assert_eq!(initialized["result"]["protocolVersion"], LEGACY_PROTOCOL_VERSION);
            assert_eq!(initialized["result"]["serverInfo"]["name"], "mcp-registrar");
        }
        let incomplete = dispatch(
            &json!({"id":3,"method":"server/discover","params":{"_meta":{"io.modelcontextprotocol/protocolVersion":MODERN_PROTOCOL_VERSION}}}),
        );
        assert_eq!(incomplete["error"]["message"], "modern_metadata_required");
        let modern = dispatch(
            &json!({"id":4,"method":"server/discover","params":{"_meta":{"io.modelcontextprotocol/protocolVersion":MODERN_PROTOCOL_VERSION,"io.modelcontextprotocol/clientInfo":{"name":"test","version":"1"},"io.modelcontextprotocol/clientCapabilities":{}}}}),
        );
        assert_eq!(
            modern["result"]["supportedVersions"][0],
            MODERN_PROTOCOL_VERSION
        );
        assert_eq!(
            modern["result"]["supportedVersions"][1],
            LEGACY_PROTOCOL_VERSION
        );
        assert_eq!(modern["result"]["resultType"], "complete");
    }

    #[test]
    fn wire_reader_refuses_oversized_messages_before_allocation() {
        let framed = format!("Content-Length: {}\r\n\r\n", MAX_MESSAGE_BYTES + 1);
        assert_eq!(
            read_message(&mut std::io::Cursor::new(framed)).unwrap_err(),
            "mcp_body_exceeds_byte_limit"
        );
        let jsonl = format!("{}\n", "x".repeat(MAX_MESSAGE_BYTES + 1));
        assert_eq!(
            read_message(&mut std::io::Cursor::new(jsonl)).unwrap_err(),
            "mcp_line_exceeds_byte_limit"
        );
    }

    #[test]
    fn repaired_contract_makes_expanding_reads_bounded_by_default() {
        let mut contract = embedded_contract();
        let declared = contract
            .pointer("/read_models/registrar_surface_list/items/0/entrypoint")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        repair_native_contract(&mut contract, &declared, &declared);
        let find = |name: &str| {
            contract["tools"]
                .as_array()
                .unwrap()
                .iter()
                .find(|tool| tool["name"] == name)
                .unwrap()
        };
        for name in ["registrar_site_list", "registrar_carrier_list"] {
            assert_eq!(
                find(name)["inputSchema"]["properties"]["compact"]["default"],
                true
            );
            assert_eq!(
                find(name)["inputSchema"]["properties"]["limit"]["default"],
                20
            );
        }
        assert_eq!(
            find("registrar_site_surface_registry_sync")["inputSchema"]["properties"]
                ["include_registry"]["default"],
            false
        );
    }

    #[test]
    fn site_scoped_surface_reports_loader_route_when_carrier_binding_is_absent() {
        let contract = embedded_contract();
        let failure = carrier_bind(
            &contract,
            &json!({
                "carrier_id":"codex-andrey",
                "site_id":"marici",
                "surface_id":"scheduler"
            }),
        )
        .expect_err("site-scoped scheduler must not be emitted as a carrier mutation");
        assert_eq!(failure.code, "registrar_carrier_site_binding_missing");
        assert_eq!(failure.details["site_surface_declared"], true);
        assert_eq!(failure.details["next_route"], "mcp-loader");
        assert!(failure.details["site_root"]
            .as_str()
            .unwrap_or_default()
            .replace('\\', "/")
            .ends_with("/marici"));
    }

    #[test]
    fn surface_usage_exposes_site_runtime_access_without_carrier_projection() {
        let contract = embedded_contract();
        let usage = surface_usage(&contract, &json!({"surface_id":"scheduler"})).unwrap();
        assert_eq!(usage["runtime_access"]["owner"], "mcp-loader");
        assert_eq!(usage["runtime_access"]["mode"], "site-scoped");
        assert_eq!(usage["runtime_access"]["available"], true);
    }

    #[test]
    fn delegation_bindings_consume_durable_extra_allowed_roots() {
        let suffix = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = env::temp_dir().join(format!(
            "narada-registrar-extra-roots-{}-{suffix}",
            std::process::id()
        ));
        fs::create_dir_all(root.join(".narada")).unwrap();
        let src = PathBuf::from("C:/Users/andrey/src");
        let wt = PathBuf::from("C:/Users/andrey/wt");
        fs::write(
            root.join(".narada/allowed-roots.json"),
            serde_json::to_vec(&json!({
                "extra_allowed_roots": [
                    src.to_string_lossy(),
                    wt.to_string_lossy(),
                    src.to_string_lossy()
                ],
                "temp_allowed_roots": ["C:/Users/andrey/tmp"]
            }))
            .unwrap(),
        )
        .unwrap();

        for surface_id in ["worker-delegation", "delegated-task"] {
            let mut args = vec!["--allowed-root".to_string(), path_text(&root)];
            append_durable_delegation_allowed_roots(surface_id, &root, &mut args).unwrap();
            let roots = args
                .windows(2)
                .filter(|pair| pair[0] == "--allowed-root")
                .map(|pair| comparable_root(Path::new(&pair[1])))
                .collect::<BTreeSet<_>>();
            assert!(!roots.contains(&comparable_root(&root)));
            assert!(roots.contains(&comparable_root(&src)));
            assert!(roots.contains(&comparable_root(&wt)));
            assert_eq!(roots.len(), 2);
            assert!(!roots.contains(&comparable_root(Path::new("C:/Users/andrey/tmp"))));
        }

        fs::remove_dir_all(root).unwrap();
    }

