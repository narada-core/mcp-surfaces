
    use super::*;

    #[test]
    fn live_catalog_has_precise_closed_schemas() {
        let tools = list_tools();
        assert_eq!(tools.len(), 24);
        for tool in &tools {
            assert_eq!(tool["inputSchema"]["type"], "object");
            assert_eq!(tool["inputSchema"]["additionalProperties"], false);
        }
        let commit = tools
            .iter()
            .find(|tool| tool["name"] == "git_commit")
            .unwrap();
        assert!(commit["inputSchema"]["required"]
            .as_array()
            .unwrap()
            .contains(&json!("work_scope_ref")));
        let show = tools
            .iter()
            .find(|tool| tool["name"] == "git_show")
            .unwrap();
        assert_eq!(show["inputSchema"]["required"], json!(["commit"]));
        let output_show = tools
            .iter()
            .find(|tool| tool["name"] == "git_output_show")
            .unwrap();
        assert!(output_show["inputSchema"].get("anyOf").is_none());
    }

    fn assert_bounded(schema: &Value) {
        match schema.get("type").and_then(Value::as_str) {
            Some("object") => {
                assert!(schema
                    .get("maxProperties")
                    .and_then(Value::as_u64)
                    .is_some());
                if let Some(properties) = schema.get("properties").and_then(Value::as_object) {
                    for child in properties.values() {
                        assert_bounded(child);
                    }
                }
            }
            Some("array") => {
                assert!(schema.get("maxItems").and_then(Value::as_u64).is_some());
                if let Some(items) = schema.get("items") {
                    assert_bounded(items);
                }
            }
            Some("string") if schema.get("enum").is_none() => {
                assert!(schema.get("maxLength").and_then(Value::as_u64).is_some());
            }
            _ => {}
        }
    }

    #[test]
    fn every_tool_schema_is_named_bounded_and_rejects_unknown_input() {
        for tool in list_tools() {
            let name = tool["name"].as_str().unwrap();
            let schema = &tool["inputSchema"];
            assert_eq!(schema["title"], format!("{name}.input"));
            assert_bounded(schema);
            let failure =
                validate_tool_arguments(schema, &json!({"unexpected":true}), "$args").unwrap_err();
            assert_eq!(failure.code, "git_invalid_arguments");
            assert_eq!(failure.details["reason"], "unknown_field:unexpected");
        }
    }

    #[test]
    fn guidance_inventory_is_derived_from_live_catalog() {
        let value = guidance(&Value::Null).unwrap();
        let writes = value["tool_inventory"]["write"].as_array().unwrap();
        assert!(writes.contains(&json!("git_commit")));
        assert!(writes.contains(&json!("git_push")));
        assert!(!writes.contains(&json!("git_fetch")));
        assert!(value["native_boundary"]
            .as_str()
            .unwrap()
            .contains("authoritative"));
    }

    #[test]
    fn diff_schema_publishes_runtime_bounds_and_untracked_contract() {
        let tool = list_tools().into_iter().find(|tool| tool["name"] == "git_diff").unwrap();
        assert_eq!(tool["inputSchema"]["properties"]["limit"]["maximum"], 50_000);
        assert_eq!(tool["inputSchema"]["properties"]["limit"]["default"], 4_000);
        assert!(tool["inputSchema"]["properties"]["include_untracked"]["description"].as_str().unwrap().contains("untracked"));
    }

    #[test]
    fn status_parser_preserves_upstream_for_push_resolution() {
        let status = parse_status("## main...origin/main\0");
        assert_eq!(status["upstream"], "origin/main");
    }

    #[test]
    fn work_scope_survives_git_surface_process_replacement() {
        let root = env::temp_dir().join(unique_id("narada-git-work-scope-test"));
        let store = root.join("scopes");
        let state = State {
            mode: "write".to_string(),
            allowed_roots: vec![root.clone()],
            max_timeout_ms: DEFAULT_MAX_TIMEOUT_MS,
            max_output_bytes: DEFAULT_MAX_OUTPUT_BYTES,
            output_root: root.clone(),
            env: HashMap::new(),
            work_scope_store: store.clone(),
            git_write_lock: Arc::new(Mutex::new(())),
        };
        let scope = WorkScope {
            reference: "gws_durable_test".to_string(),
            repository_root: root.to_string_lossy().to_string(),
            owner_id: "test-owner".to_string(),
            authority: "paths".to_string(),
            allowed_paths: vec!["README.md".to_string()],
            base_state: json!({"head": "abc"}),
            created_at: OffsetDateTime::now_utc().format(&Rfc3339).unwrap(),
            expires_at: OffsetDateTime::now_utc() + TimeDuration::minutes(1),
        };
        with_work_scope_lock(&state, |path| persist_work_scope_unlocked(path, &scope)).unwrap();

        let replacement_state = State {
            git_write_lock: Arc::new(Mutex::new(())),
            ..state.clone()
        };
        let recovered = resolve_work_scope(
            &replacement_state,
            "gws_durable_test",
            &root.to_string_lossy(),
        )
        .unwrap();
        assert_eq!(recovered.owner_id, "test-owner");
        assert_eq!(recovered.allowed_paths, vec!["README.md"]);
        fs::remove_dir_all(root).unwrap();
    }
