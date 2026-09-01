
    use super::*;

    #[test]
    fn worker_run_schema_declares_low_cognition_default() {
        assert_eq!(
            input_schema("worker_run")["properties"]["constraints"]["properties"]["cognition"]
                ["default"],
            "low"
        );
        assert_eq!(
            cognition_defaults(Path::new("."))["default_cognition"],
            "low"
        );
        assert_eq!(guidance(&Map::new())["cognition"]["default"], "low");
        assert_eq!(
            input_schema("worker_run")["properties"]["constraints"]["properties"]
                ["wait_for_completion"]["default"],
            false
        );
        assert_eq!(
            input_schema("worker_run")["properties"]["constraints"]["properties"]
                ["wait_timeout_ms"]["default"],
            30_000
        );
    }

    #[test]
    fn policy_declares_secret_store_reference_projection() {
        let value = policy(Path::new("."), &[PathBuf::from(".")]);
        assert_eq!(value["secret_projection"], "secret_store_reference_only");
        assert_eq!(value["provider_transports"]["codex-subscription"]["transport"], "codex_app_server_broker");
        assert_eq!(value["provider_transports"]["codex-subscription"]["thread_policy"], "fresh_ephemeral_per_turn");
        assert_eq!(value["provider_transports"]["codex-subscription"]["fallback"], "none");
        assert!(guidance(&Map::new())["boundaries"][1]
            .as_str()
            .is_some_and(|text| text.contains("SecretStore-referenced")));
    }

    #[test]
    fn config_resolve_reports_site_cognition_mapping_without_launching() {
        let root =
            std::env::temp_dir().join(format!("narada-worker-config-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(root.join(".narada")).expect("site root");
        fs::write(
            defaults_path(&root),
            serde_json::to_vec(&json!({"effective_cognition_defaults":{"low":{"provider":"codex-subscription","model":"gpt-5.6-luna","reasoning_effort":"max"},"medium":{"provider":"codex-subscription","model":"gpt-5.6-sol","reasoning_effort":"low"},"high":{"provider":"codex-subscription","model":"gpt-5.6-sol","reasoning_effort":"max"}}})).expect("encode defaults"),
        ).expect("defaults");
        let resolved = config_resolve(
            &json!({"constraints":{"cognition":"medium"}})
                .as_object()
                .unwrap(),
            &root,
            std::slice::from_ref(&root),
        )
        .expect("resolve");
        assert_eq!(resolved["resolved"]["cognition"], "medium");
        assert_eq!(resolved["resolved"]["provider_mode"], "codex-subscription");
        assert_eq!(resolved["resolved"]["model"], "gpt-5.6-sol");
        assert_eq!(resolved["resolved"]["reasoning_effort"], "low");
        assert_eq!(resolved["resolved"]["launch"], false);
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn compact_run_preserves_effective_invocation_provenance() {
        let compact = compact_run(
            &json!({"run_id":"run-test","status":"completed","resolved_invocation":{"cognition":"low","provider_model":"gpt-5.6-luna"}}),
        );
        assert_eq!(compact["resolved_invocation"]["cognition"], "low");
        assert_eq!(
            compact["resolved_invocation"]["provider_model"],
            "gpt-5.6-luna"
        );
    }

    #[test]
    fn minimal_readback_omits_capability_and_exposes_batch_wait_controls() {
        let minimal = minimal_run(&json!({
            "run_id":"run-test",
            "task_label":"inspect ledger",
            "status":"completed",
            "completion_state":"complete",
            "capability_snapshot":{"large":true},
            "resolved_invocation":{"provider_model":"model"},
            "timing":{"started_at":"start","finished_at":"finish","duration_ms":12},
            "summary":"done",
            "error":null
        }));
        assert_eq!(minimal["task_label"], "inspect ledger");
        assert_eq!(minimal["error"], Value::Null);
        assert!(minimal.get("capability_snapshot").is_none());
        assert!(minimal.get("resolved_invocation").is_none());
        let batch = input_schema("worker_run_wait_batch");
        assert_eq!(batch["properties"]["compact"]["default"], true);
        assert_eq!(batch["properties"]["timeout_ms"]["maximum"], 180_000);
        assert_eq!(batch["properties"]["poll_ms"]["default"], 5_000);
        let list = input_schema("worker_runs_list");
        assert_eq!(list["properties"]["site_scope"]["default"], "current_site");
    }

    #[test]
    fn compact_run_surfaces_failure_and_elapsed_time() {
        let compact = compact_run(&json!({
            "run_id":"run-timeout",
            "status":"failed",
            "error":"worker_runtime_timed_out:max_run_ms=120000:elapsed_ms=120004",
            "failure":{"code":"worker_runtime_timed_out","elapsed_ms":120004},
            "timing":{"duration_ms":120004}
        }));
        assert_eq!(compact["error"], "worker_runtime_timed_out:max_run_ms=120000:elapsed_ms=120004");
        assert_eq!(compact["duration_ms"], 120004);
        assert_eq!(compact["failure"]["code"], "worker_runtime_timed_out");
    }

    #[test]
    fn timeout_failure_contains_remediation_and_elapsed_time() {
        let failure = timeout_failure("run-timeout", 120_000, 120_004);
        assert_eq!(failure["code"], "worker_runtime_timed_out");
        assert_eq!(failure["elapsed_ms"], 120_004);
        assert!(failure["remediation"].as_str().is_some_and(|text| !text.is_empty()));
    }

    #[test]
    fn worker_output_repairs_common_utf8_display_corruption() {
        assert_eq!(repair_mojibake("x Â· y â€“ z"), "x · y – z");
    }

    #[test]
    fn worker_preflight_rejects_missing_read_path() {
        let root = std::env::temp_dir().join(format!("narada-worker-preflight-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&root).expect("root");
        let constraints = json!({"preflight_paths":[{"path":"missing.txt","access":"read"}]});
        let error = preflight_paths(constraints.as_object(), &root, std::slice::from_ref(&root))
            .expect_err("missing path must fail");
        assert_eq!(error["code"], "worker_preflight_path_missing");
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn worker_preflight_accepts_existing_read_path() {
        let root = std::env::temp_dir().join(format!("narada-worker-preflight-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&root).expect("root");
        fs::write(root.join("present.txt"), "ok").expect("file");
        let constraints = json!({"preflight_paths":[{"path":"present.txt","access":"read"}]});
        let result = preflight_paths(constraints.as_object(), &root, std::slice::from_ref(&root))
            .expect("existing path");
        assert_eq!(result["status"], "passed");
        assert_eq!(result["items"][0]["status"], "passed");
        assert_eq!(result["items"][0]["native_read"]["status"], "passed");
        assert_eq!(result["items"][0]["native_read"]["content"], "ok");
        let prompt = native_read_evidence_prompt(&result);
        assert!(prompt.contains("CONTENT (native, bounded; authoritative; do not call shell or command tools for this path)"));
        assert!(prompt.contains("ok"));
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn worker_prompt_places_terminal_contract_after_native_evidence() {
        let preflight = json!({"items":[{"path":"fixture.txt","native_read":{"status":"passed","content":"fixture evidence"}}]});
        let contract = "MANDATORY TERMINAL OUTPUT CONTRACT: return exactly one JSON object";
        let prompt = worker_prompt(contract, &preflight);
        let evidence_offset = prompt.find("fixture evidence").expect("evidence");
        let contract_offset = prompt.find(contract).expect("contract");
        assert!(contract_offset > evidence_offset);
        assert!(prompt.ends_with(contract));
    }

    #[test]
    fn every_public_tool_has_a_closed_bounded_input_contract() {
        for tool in list_tools() {
            let name = tool["name"].as_str().expect("tool name");
            let schema = &tool["inputSchema"];
            assert_eq!(
                schema["additionalProperties"], false,
                "{name} must be closed"
            );
            if ![
                "worker_policy_inspect",
                "worker_cognition_defaults_inspect",
                "worker_operator_affordances",
            ]
            .contains(&name)
            {
                assert_ne!(
                    schema,
                    &json!({"type":"object","additionalProperties":false}),
                    "{name} unexpectedly has no declared arguments"
                );
            }
        }
        for name in [
            "worker_policy_inspect",
            "worker_cognition_defaults_inspect",
            "worker_operator_affordances",
        ] {
            assert_eq!(
                input_schema(name),
                json!({"type":"object","additionalProperties":false})
            );
        }
    }

    #[test]
    fn containment_is_path_component_aware() {
        assert!(path_components_equal_or_child(
            Path::new("C:/Users/Andrey/Narada/project"),
            Path::new("C:/Users/Andrey/Narada")
        ));
        assert!(!path_components_equal_or_child(
            Path::new("C:/Users/Andrey/Narada-other"),
            Path::new("C:/Users/Andrey/Narada")
        ));
        assert!(path_components_equal_or_child(
            Path::new("C:/Users/Andrey/src/mcp-surfaces"),
            Path::new("C:/Users/Andrey/src")
        ));
        assert!(path_components_equal_or_child(
            Path::new("C:/Users/Andrey/wt/mcp-surfaces"),
            Path::new("C:/Users/Andrey/wt")
        ));
        assert!(!path_components_equal_or_child(
            Path::new("C:/Users/Andrey/src-other/mcp-surfaces"),
            Path::new("C:/Users/Andrey/src")
        ));
    }

    #[test]
    fn wait_and_windows_toolchain_contracts_are_explicit() {
        assert_eq!(
            input_schema("worker_run_wait")["properties"]["timeout_ms"]["maximum"],
            MAX_INLINE_WAIT_MS
        );
        assert_eq!(
            input_schema("worker_run")["properties"]["constraints"]["properties"]["wait_timeout_ms"]
                ["maximum"],
            MAX_INLINE_WAIT_MS
        );
        assert_eq!(
            guidance(&Map::new())["windows_rust_toolchain"]["status"],
            "caller_environment_required"
        );
        assert!(guidance(&Map::new())["first_use"]
            .as_array()
            .expect("guidance steps")
            .iter()
            .any(|step| step
                .as_str()
                .is_some_and(|text| text.starts_with("READ-ONLY COMMAND CONTRACT"))));
        assert_eq!(
            policy(Path::new("."), &[])["windows_msvc_environment"]["inherited"],
            true
        );
    }

    #[test]
    fn bounded_wait_returns_terminal_run_without_polling_forever() {
        let root =
            std::env::temp_dir().join(format!("narada-worker-wait-{}", uuid::Uuid::new_v4()));
        let run_dir = run_root(&root).join("run-test");
        fs::create_dir_all(&run_dir).expect("run directory");
        fs::write(
            run_dir.join("result.json"),
            serde_json::to_vec(&json!({
                "schema":"narada.worker.run.v1",
                "run_id":"run-test",
                "status":"completed",
                "completion_state":"complete"
            }))
            .expect("run record"),
        )
        .expect("write run");
        let (run, wait) = wait_for_run(&root, "run-test", 30_000).expect("bounded wait");
        assert_eq!(run["status"], "completed");
        assert_eq!(wait["status"], "finished");
        assert_eq!(wait["timeout_ms"], 30_000);
        assert_eq!(wait["native_execution"], "bounded_state_poll");
        fs::remove_dir_all(root).expect("cleanup");
    }

