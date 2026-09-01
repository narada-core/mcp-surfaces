    #[test]
    fn named_native_surface_catalogs_are_present() {
        fn assert_bounded(schema: &Value, path: &str) {
            if schema.get("type").and_then(Value::as_str) == Some("string")
                && schema.get("enum").is_none()
            {
                assert!(
                    schema.get("maxLength").and_then(Value::as_u64).is_some(),
                    "unbounded string: {path}"
                );
            }
            if schema.get("type").and_then(Value::as_str) == Some("array") {
                assert!(
                    schema.get("maxItems").and_then(Value::as_u64).is_some(),
                    "unbounded array: {path}"
                );
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
        let surfaces = [
            "site-inbox",
            "mailbox",
            "graph-mail",
            "calendar",
            "site-lifecycle",
            "site-registry",
            "worker-delegation",
            "delegated-task",
            "sop",
            "scheduler",
            "surface-feedback",
            "speech",
            "artifacts",
            "nars-session",
            "quota-meter",
            "operator-console-overlay",
            "browser-control",
            "cloudflare-carrier",
            "site-coherence",
            "catalog-observation",
            "runtime-introspection",
            "project-state",
            "launcher",
            "operator-routing",
        ];
        for surface in surfaces {
            let tools = list_tools(surface);
            assert!(!tools.is_empty(), "missing native catalog for {surface}");
            assert!(
                tools
                    .iter()
                    .all(|tool| tool.get("name").and_then(Value::as_str).is_some()),
                "unnamed native tool for {surface}"
            );
            for tool in tools {
                let name = tool["name"].as_str().expect("tool name");
                let schema = &tool["inputSchema"];
                assert_eq!(schema["title"], format!("{name}.input"), "{surface}/{name}");
                assert_eq!(schema["additionalProperties"], false, "{surface}/{name}");
                assert_bounded(schema, &format!("{surface}/{name}"));
            }
        }
    }

    #[test]
    fn graph_mail_catalog_exposes_the_arguments_its_native_authority_requires() {
        let tools = list_tools("graph-mail");
        assert_eq!(tools.len(), 34);
        let by_name = |name: &str| {
            tools
                .iter()
                .find(|tool| tool["name"] == name)
                .unwrap_or_else(|| panic!("missing {name}"))
        };
        let query = by_name("graph_mail_query");
        assert!(query["inputSchema"]["properties"]["mailbox_id"].is_object());
        assert_eq!(query["inputSchema"]["properties"]["limit"]["maximum"], 100);
        let show = by_name("graph_mail_message_show");
        assert_eq!(show["inputSchema"]["required"], json!(["message_id"]));
        let upload = by_name("graph_mail_attachment_upload_chunk");
        assert_eq!(
            upload["inputSchema"]["required"].as_array().map(Vec::len),
            Some(5)
        );
        let discard = by_name("graph_mail_ticket_draft_discard");
        assert_eq!(
            discard["inputSchema"]["properties"]["confirm_discard"]["const"],
            true
        );
        assert_eq!(discard["annotations"]["destructiveHint"], true);
        assert!(tools.iter().all(|tool| {
            tool["description"].as_str().is_some_and(|description| {
                !description.contains("external authority remains explicit")
            })
        }));
    }

    #[test]
    fn native_input_validation_enforces_declared_const_enum_pattern_and_numeric_bounds() {
        let schema = json!({
            "type":"object",
            "properties":{
                "confirm":{"type":"boolean","const":true},
                "mode":{"type":"string","enum":["safe"]},
                "digest":{"type":"string","pattern":"^[a-f0-9]{64}$"},
                "limit":{"type":"integer","minimum":1,"maximum":5}
            },
            "required":["confirm","mode","digest","limit"],
            "additionalProperties":false
        });
        assert!(validate_input_schema(
            &schema,
            &json!({"confirm":true,"mode":"safe","digest":"a".repeat(64),"limit":5}),
            "arguments"
        )
        .is_ok());
        for invalid in [
            json!({"confirm":false,"mode":"safe","digest":"a".repeat(64),"limit":5}),
            json!({"confirm":true,"mode":"unsafe","digest":"a".repeat(64),"limit":5}),
            json!({"confirm":true,"mode":"safe","digest":"wrong","limit":5}),
            json!({"confirm":true,"mode":"safe","digest":"a".repeat(64),"limit":6}),
        ] {
            assert!(
                validate_input_schema(&schema, &invalid, "arguments").is_err(),
                "accepted {invalid}"
            );
        }
    }

    #[test]
    fn catalog_observation_requires_an_explicit_iso_instant() {
        let response = handle_request(
            &json!({"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"catalog_observation_observe","arguments":{"provider_id":"inference-provider:test","observed_at":"not-an-instant"}}}),
            &test_options(),
        ).expect("response");
        assert_eq!(
            response["error"]["data"]["code"],
            "catalog_observation_observed_at_invalid"
        );
    }

    #[test]
    fn catalog_observation_is_closed_and_truthful_without_authority() {
        let tools = list_tools("catalog-observation");
        assert!(tools
            .iter()
            .all(|tool| tool["inputSchema"]["additionalProperties"] == false));
        let guidance = handle_request(
            &json!({"jsonrpc":"2.0","id":5,"method":"tools/call","params":{"name":"catalog_observation_guidance","arguments":{}}}),
            &test_options(),
        )
        .expect("guidance");
        assert_eq!(
            guidance["result"]["structuredContent"]["capability_status"],
            "contract_only_until_observation_port_installed"
        );
        let observed = handle_request(
            &json!({"jsonrpc":"2.0","id":6,"method":"tools/call","params":{"name":"catalog_observation_observe","arguments":{"provider_id":"inference-provider:test","observed_at":"2026-08-14T00:00:00Z","access_mode":"credentialed"}}}),
            &test_options(),
        )
        .expect("observation");
        assert_eq!(observed["result"]["isError"], true);
        assert_eq!(
            observed["result"]["structuredContent"]["status"],
            "unavailable"
        );
        assert_eq!(
            observed["result"]["structuredContent"]["requested_access_mode"],
            "credentialed"
        );
        assert_eq!(observed["result"]["structuredContent"]["models"], json!([]));
        assert!(!observed.to_string().contains("credential_value"));
    }

    #[test]
    fn catalog_observation_invalid_access_mode_has_actionable_diagnostic() {
        let response = handle_request(
            &json!({"jsonrpc":"2.0","id":7,"method":"tools/call","params":{"name":"catalog_observation_observe","arguments":{"provider_id":"inference-provider:test","observed_at":"2026-08-14T00:00:00Z","access_mode":"ambient"}}}),
            &test_options(),
        )
        .expect("response");
        assert_eq!(
            response["error"]["data"]["code"],
            "input_schema_validation_failed"
        );
        assert_eq!(response["error"]["data"]["details"]["path"], "/arguments");
        assert!(response["error"]["data"]["details"]["message"]
            .as_str()
            .is_some_and(|message| message.contains("ambient") && message.contains("public")));
    }

