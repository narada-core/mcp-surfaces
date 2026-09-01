    #[test]
    fn operator_routing_writes_a_durable_record() {
        let root = std::env::temp_dir().join(format!("narada-operator-routing-{}", Uuid::new_v4()));
        let options = Options {
            surface_id: "operator-routing".to_string(),
            site_root: root.clone(),
            allowed_roots: vec![root.clone()],
            log_root: Some(root.join("log")),
            registry_path: None,
            native_authority: false,
            environment: Vec::new(),
        };
        let params = json!({"name":"operator_route_request","arguments":{"transcript":"route this","target_runtime":"codex","request_id":"route-test"}});
        let result = call_tool(
            "operator-routing",
            params.as_object().expect("params"),
            &options,
        )
        .expect("route");
        assert_eq!(result["structuredContent"]["request_id"], "route-test");
        let replay = call_tool(
            "operator-routing",
            params.as_object().expect("params"),
            &options,
        )
        .expect("route replay");
        assert_eq!(replay["structuredContent"]["idempotency_replay"], true);
        let conflict_params = json!({"name":"operator_route_request","arguments":{"transcript":"different","target_runtime":"codex","request_id":"route-test"}});
        let conflict = call_tool(
            "operator-routing",
            conflict_params.as_object().expect("params"),
            &options,
        )
        .expect_err("request id conflict");
        assert_eq!(conflict["code"], "operator_route_request_id_conflict");
        let log = root.join("log").join("operator-routing-log.jsonl");
        assert!(log.exists());
        let content = std::fs::read_to_string(log).expect("log");
        assert!(content.contains(r#""request_id":"route-test""#));
        let role_params = json!({"name":"operator_route_request","arguments":{"transcript":"admit resident","target_runtime":"codex","target_identity":"fixture.resident","operation_kind":"role_admission","target_site_id":"fixture","target_site_root":root.to_string_lossy(),"role":"resident","principal":"operator","request_id":"route-role"}});
        let role = call_tool(
            "operator-routing",
            role_params.as_object().expect("params"),
            &options,
        )
        .expect("role route");
        assert_eq!(
            role["structuredContent"]["routing"]["handoff"]["status"],
            "ready"
        );
        assert_eq!(
            role["structuredContent"]["routing"]["handoff"]["tool"],
            "site_admit_role"
        );
        assert_eq!(
            role["structuredContent"]["routing"]["handoff"]["mutation_authorized"],
            false
        );
        let incomplete_params = json!({"name":"operator_route_request","arguments":{"transcript":"bind runtime","target_runtime":"codex","operation_kind":"runtime_binding","target_site_root":root.to_string_lossy(),"request_id":"route-runtime"}});
        let incomplete = call_tool(
            "operator-routing",
            incomplete_params.as_object().expect("params"),
            &options,
        )
        .expect("runtime route");
        assert_eq!(
            incomplete["structuredContent"]["routing"]["handoff"]["status"],
            "needs_input"
        );
        assert_eq!(
            incomplete["structuredContent"]["routing"]["handoff"]["required_inputs"],
            json!(["target_identity", "runtime_locus", "runtime_handle"])
        );
        let route_tool = list_tools("operator-routing")
            .into_iter()
            .find(|tool| tool["name"] == "operator_route_request")
            .expect("route tool");
        assert_eq!(route_tool["inputSchema"]["additionalProperties"], false);
        assert_eq!(
            route_tool["inputSchema"]["properties"]["transcript"]["maxLength"],
            65536
        );
        std::fs::remove_dir_all(root).expect("cleanup");
    }
    #[test]
    fn modern_tools_list_requires_metadata_and_has_cache_metadata() {
        let response = handle_request(
            &json!({"jsonrpc":"2.0","id":3,"method":"tools/list","params":{"_meta":{"io.modelcontextprotocol/protocolVersion":MODERN_PROTOCOL_VERSION,"io.modelcontextprotocol/clientInfo":{"name":"test","version":"1"},"io.modelcontextprotocol/clientCapabilities":{}}}}),
            &test_options(),
        ).expect("response");
        assert_eq!(response["result"]["resultType"], "complete");
        assert_eq!(response["result"]["cacheScope"], "public");
        assert!(response["result"]["ttlMs"].as_u64().unwrap_or(0) > 0);
    }

    #[test]
    fn shared_wire_reader_refuses_oversized_lines() {
        let input = format!("{}\n", "x".repeat(MAX_MCP_REQUEST_BYTES + 1));
        assert_eq!(
            read_line_bounded(&mut std::io::Cursor::new(input), MAX_MCP_REQUEST_BYTES).unwrap_err(),
            "native_surface_request_line_too_large"
        );
    }

    #[test]
    fn calendar_catalog_is_named_closed_and_bounded() {
        let tools = list_tools("calendar");
        assert_eq!(tools.len(), 9);
        for tool in tools {
            let name = tool["name"].as_str().unwrap();
            assert_eq!(tool["inputSchema"]["title"], format!("{name}.input"));
            assert_eq!(tool["inputSchema"]["additionalProperties"], false);
            assert!(tool["inputSchema"]["maxProperties"].as_u64().is_some());
        }
        let output = list_tools("calendar")
            .into_iter()
            .find(|tool| tool["name"] == "calendar_output_show")
            .unwrap();
        assert_eq!(output["inputSchema"]["required"], json!(["ref"]));
        assert!(output["inputSchema"]["properties"]
            .get("output_ref")
            .is_none());
    }
