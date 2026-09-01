
    use super::*;

    fn test_options() -> Options {
        Options {
            surface_id: "catalog-observation".to_string(),
            site_root: PathBuf::from("."),
            allowed_roots: vec![PathBuf::from(".")],
            log_root: None,
            registry_path: None,
            native_authority: false,
            environment: Vec::new(),
        }
    }

    fn parsed_options(args: &[&str]) -> Options {
        parse_options(args.iter().map(|value| (*value).to_string()).collect())
            .expect("registrar arguments should parse")
    }

    fn environment_value<'a>(options: &'a Options, key: &str) -> Option<&'a str> {
        options
            .environment
            .iter()
            .rev()
            .find(|(candidate, _)| candidate == key)
            .map(|(_, value)| value.as_str())
    }

    #[test]
    fn registrar_native_surface_argument_profiles_are_launchable() {
        let delegated = parsed_options(&[
            "--surface-id",
            "delegated-task",
            "--site-root",
            "site",
            "--task-root",
            "task",
            "--allowed-root",
            "site",
        ]);
        assert_eq!(delegated.site_root, PathBuf::from("site"));
        assert_eq!(delegated.allowed_roots, vec![PathBuf::from("site")]);
        assert_eq!(
            environment_value(&delegated, "NARADA_DELEGATED_TASK_ROOT"),
            Some("task")
        );

        let nars = parsed_options(&[
            "--surface-id",
            "nars-session",
            "--projection",
            "user-site-operator",
            "--user-site-root",
            "user-site",
            "--source-kind",
            "operator",
            "--operator-id",
            "andrey",
        ]);
        assert_eq!(nars.site_root, PathBuf::from("user-site"));
        assert_eq!(
            environment_value(&nars, "NARADA_NARS_SESSION_PROJECTION"),
            Some("user-site-operator")
        );
        assert_eq!(
            environment_value(&nars, "NARADA_USER_SITE_ROOT"),
            Some("user-site")
        );

        let scheduler = parsed_options(&["--surface-id", "scheduler", "--allowed-root", "site"]);
        assert_eq!(scheduler.site_root, PathBuf::from("site"));

        let worker = parsed_options(&[
            "--surface-id",
            "worker-delegation",
            "--site-root",
            "site",
            "--allowed-root",
            "site",
            "--allowed-root",
            "src",
        ]);
        assert_eq!(
            worker.allowed_roots,
            vec![PathBuf::from("site"), PathBuf::from("src")]
        );

        let worker_without_explicit_site = parsed_options(&[
            "--surface-id",
            "worker-delegation",
            "--site-root",
            "site",
            "--allowed-root",
            "src",
        ]);
        assert_eq!(
            worker_without_explicit_site.allowed_roots,
            vec![PathBuf::from("src")]
        );

        let coherence = parsed_options(&["--surface-id", "site-coherence", "--repo-root", "repo"]);
        assert_eq!(coherence.site_root, PathBuf::from("repo"));

        let sop = parsed_options(&[
            "--surface-id",
            "sop",
            "--sop-root",
            "site",
            "--server-name",
            "site-sop",
            "--sops-dir",
            "site/.narada/sops",
        ]);
        assert_eq!(sop.site_root, PathBuf::from("site"));
        assert_eq!(
            environment_value(&sop, "NARADA_SOPS_DIR"),
            Some("site/.narada/sops")
        );

        let speech = parsed_options(&[
            "--surface-id",
            "speech",
            "--provider-registry-path",
            "providers.json",
        ]);
        assert_eq!(
            environment_value(&speech, "NARADA_SPEECH_PROVIDER_REGISTRY_PATH"),
            Some("providers.json")
        );

        let feedback = parsed_options(&[
            "--surface-id",
            "surface-feedback",
            "--feedback-root",
            "feedback",
            "--canonical-feedback-root",
            "canonical",
            "--task-lifecycle-root",
            "site",
            "--site-id",
            "andrey-user",
            "--owned-surface-id",
            "calendar",
        ]);
        assert_eq!(feedback.site_root, PathBuf::from("feedback"));
        assert_eq!(
            environment_value(&feedback, "NARADA_SURFACE_FEEDBACK_ROOT"),
            Some("canonical")
        );
        assert_eq!(
            environment_value(&feedback, "NARADA_TASK_LIFECYCLE_ROOT"),
            Some("site")
        );
        assert_eq!(
            environment_value(&feedback, "NARADA_SITE_ID"),
            Some("andrey-user")
        );
        assert_eq!(
            environment_value(&feedback, "NARADA_OWNED_SURFACE_IDS"),
            Some("calendar")
        );

        let worker = parsed_options(&[
            "--surface-id",
            "worker-delegation",
            "--site-root",
            "site",
            "--allowed-root",
            "site",
            "--run-root",
            "site/.narada/runtime/worker-delegation",
        ]);
        assert_eq!(worker.site_root, PathBuf::from("site"));
        assert_eq!(
            environment_value(&worker, "NARADA_WORKER_RUN_ROOT"),
            Some("site/.narada/runtime/worker-delegation")
        );
    }

    #[test]
    fn unrecognized_native_surface_arguments_still_refuse() {
        let error = parse_options(vec![
            "--surface-id".to_string(),
            "scheduler".to_string(),
            "--not-a-registrar-argument".to_string(),
            "value".to_string(),
        ])
        .expect_err("unknown arguments must refuse");
        assert_eq!(
            error,
            "native_surface_unknown_argument:--not-a-registrar-argument"
        );
    }

    #[test]
    fn legacy_initialize_remains_available() {
        let response = handle_request(
            &json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05"}}),
            &test_options(),
        ).expect("response");
        assert_eq!(
            response["result"]["protocolVersion"],
            LEGACY_PROTOCOL_VERSION
        );
        assert!(response["result"]["resultType"].is_null());
    }

    #[test]
    fn modern_discover_is_self_describing_and_cacheable() {
        let response = handle_request(
            &json!({"jsonrpc":"2.0","id":2,"method":"server/discover","params":{"_meta":{"io.modelcontextprotocol/protocolVersion":MODERN_PROTOCOL_VERSION,"io.modelcontextprotocol/clientInfo":{"name":"test","version":"1"},"io.modelcontextprotocol/clientCapabilities":{}}}}),
            &test_options(),
        ).expect("response");
        assert_eq!(response["result"]["resultType"], "complete");
        assert_eq!(response["result"]["cacheScope"], "public");
        assert_eq!(
            response["result"]["_meta"]["io.modelcontextprotocol/serverInfo"]["name"],
            "catalog-observation-mcp"
        );
    }

    #[test]
    fn shared_surface_tool_names_match_wire_contracts() {
        let catalog_tools = list_tools("catalog-observation");
        let catalog_names = catalog_tools
            .iter()
            .filter_map(|tool| tool.get("name").and_then(Value::as_str))
            .collect::<Vec<_>>();
        assert_eq!(
            catalog_names,
            vec![
                "catalog_observation_guidance",
                "catalog_observation_observe"
            ]
        );
        let routing_tools = list_tools("operator-routing");
        let routing_names = routing_tools
            .iter()
            .filter_map(|tool| tool.get("name").and_then(Value::as_str))
            .collect::<Vec<_>>();
        assert_eq!(
            routing_names,
            vec![
                "operator_routing_guidance",
                "operator_route_doctor",
                "operator_route_request"
            ]
        );
        let launcher_tools = list_tools("launcher");
        let launcher_names = launcher_tools
            .iter()
            .filter_map(|tool| tool.get("name").and_then(Value::as_str))
            .collect::<Vec<_>>();
        assert_eq!(
            launcher_names,
            vec![
                "launcher_guidance",
                "launcher_doctor",
                "launcher_options_list",
                "launcher_registry_list",
                "launcher_plan",
                "launcher_option_matrix",
                "launcher_coherence_check"
            ]
        );
    }

