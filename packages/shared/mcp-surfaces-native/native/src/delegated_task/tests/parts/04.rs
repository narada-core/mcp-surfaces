    #[test]
    fn mutating_tool_contracts_are_closed_named_and_callable() {
        let tools = list_tools();
        let validate = tools
            .iter()
            .find(|tool| tool["name"] == "delegated_task_validate")
            .expect("validate tool");
        assert_eq!(validate["annotations"]["readOnlyHint"], false);
        assert_eq!(validate["annotations"]["destructiveHint"], false);
        assert_eq!(validate["annotations"]["stateChangingHint"], true);
        let wait = tools
            .iter()
            .find(|tool| tool["name"] == "delegated_task_wait")
            .expect("wait tool");
        assert_eq!(wait["annotations"]["destructiveHint"], false);
        assert_eq!(wait["annotations"]["stateChangingHint"], true);
        let execute = tools
            .iter()
            .find(|tool| tool["name"] == "delegated_task_execute")
            .expect("execute tool");
        assert_eq!(execute["annotations"]["readOnlyHint"], false);
        assert_eq!(execute["annotations"]["destructiveHint"], false);
        assert_eq!(execute["annotations"]["stateChangingHint"], true);
        assert_eq!(
            execute["inputSchema"]["required"],
            json!(["objective", "idempotency_key"])
        );
        let cancel = tools
            .iter()
            .find(|tool| tool["name"] == "delegated_task_cancel")
            .expect("cancel tool");
        assert_eq!(cancel["annotations"]["destructiveHint"], true);
        assert_eq!(cancel["annotations"]["stateChangingHint"], true);
        let run = tools
            .iter()
            .find(|tool| tool["name"] == "delegated_task_run")
            .expect("run tool");
        assert!(run["inputSchema"]["anyOf"]
            .as_array()
            .is_some_and(|items| items.iter().any(|item| item["required"] == json!(["validated_request_ref"]))));
        for name in MUTATING {
            let tool = tools.iter().find(|tool| tool["name"] == *name).expect("tool");
            assert_eq!(tool["inputSchema"]["additionalProperties"], false, "{name}");
            assert!(tool["inputSchema"]["properties"].as_object().is_some_and(|value| !value.is_empty()), "{name}");
        }
        let run = tools.iter().find(|tool| tool["name"] == "delegated_task_run").unwrap();
        for field in ["objective","intent","workflow","execution","execution_binding","idempotency_key"] { assert!(run["inputSchema"]["properties"].get(field).is_some(), "{field}"); }
        let wait = tools.iter().find(|tool| tool["name"] == "delegated_task_wait").unwrap();
        assert_eq!(wait["annotations"]["readOnlyHint"], false);
        assert!(wait["inputSchema"]["properties"].get("timeout_ms").is_some());
        assert_eq!(
            wait["inputSchema"]["properties"]["timeout_ms"]["maximum"],
            MAX_TRANSPORT_SAFE_WAIT_MS
        );
        assert_eq!(
            execute["inputSchema"]["properties"]["timeout_ms"]["maximum"],
            MAX_TRANSPORT_SAFE_WAIT_MS
        );
        assert!(wait["inputSchema"]["properties"].get("allow_cross_site").is_some());
    }

    #[test]
    fn native_delegated_task_reads_durable_json_without_execution() {
        let root =
            std::env::temp_dir().join(format!("narada-delegated-task-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(root.join("tasks/task_a")).expect("root");
        fs::write(root.join("tasks/task_a/task.json"), r#"{"task_id":"task_a","status":"completed","objective":"demo","updated_at":"2026-01-01T00:00:00Z","result":{"acceptance_verdict":"accepted"}}"#).expect("task");
        let listed = tasks_list(
            &json!({"limit":1,"view":"all","site_scope":"all_sites"})
                .as_object()
                .unwrap(),
            &root,
        )
        .expect("list");
        assert_eq!(listed["count"], 1);
        assert_eq!(
            task_status(&json!({"task_id":"task_a"}).as_object().unwrap(), &root).expect("status")
                ["task_status"],
            "completed"
        );
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn native_delegated_task_refuses_oversized_records() {
        let root = std::env::temp_dir().join(format!(
            "narada-delegated-task-large-{}",
            uuid::Uuid::new_v4()
        ));
        fs::create_dir_all(root.join("tasks/task_a")).expect("root");
        fs::write(
            root.join("tasks/task_a/task.json"),
            vec![b'x'; MAX_FILE_BYTES as usize + 1],
        )
        .expect("task");
        let error = task_status(&json!({"task_id":"task_a"}).as_object().unwrap(), &root)
            .expect_err("oversized record must refuse");
        assert_eq!(error["code"], "delegated_task_record_too_large");
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn native_delegated_task_owns_durable_lifecycle_mutations() {
        let root = std::env::temp_dir().join(format!(
            "narada-delegated-task-mutate-{}",
            uuid::Uuid::new_v4()
        ));
        let created = task_run(
            json!({"task_id":"task_native","objective":"demo","execution":{"start":false}})
                .as_object()
                .unwrap(),
            &root,
        )
        .expect("run");
        assert_eq!(created["task_status"], "accepted_for_execution");
        let cancelled = task_cancel(
            json!({"task_id":"task_native","reason":"fixture"})
                .as_object()
                .unwrap(),
            &root,
            false,
        )
        .expect("cancel");
        assert_eq!(cancelled["task_status"], "cancelled");
        let acknowledged = task_acknowledge(
            json!({"task_id":"task_native","acknowledged_by":"test"})
                .as_object()
                .unwrap(),
            &root,
        )
        .expect("acknowledge");
        assert_eq!(acknowledged["status"], "acknowledged");
        assert_eq!(
            task_events(json!({"task_id":"task_native"}).as_object().unwrap(), &root)
                .expect("events")["count"],
            3
        );

        task_run(
            json!({"task_id":"task_takeover","objective":"demo","execution":{"start":false}})
                .as_object()
                .unwrap(),
            &root,
        )
        .expect("second run");
        let takeover = task_cancel(
            json!({"task_id":"task_takeover","parent_task_id":"parent"})
                .as_object()
                .unwrap(),
            &root,
            true,
        )
        .expect("takeover");
        assert_eq!(takeover["status"], "parent_takeover_recorded");
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn native_delegated_task_preserves_explicit_dag_state() {
        let root = std::env::temp_dir().join(format!(
            "narada-delegated-task-dag-{}",
            uuid::Uuid::new_v4()
        ));
        task_run(json!({"task_id":"task_dag","objective":"demo","execution":{"start":false},"workflow":{"steps":[{"id":"research","kind":"research"},{"id":"synthesize","kind":"worker","depends_on":["research"]}]}}).as_object().unwrap(), &root).expect("run");
        let task = read_task(&root, "task_dag").expect("task");
        assert_eq!(task["workflow"]["steps"].as_array().map(Vec::len), Some(2));
        assert_eq!(
            task["result"]["step_states"]["research"]["status"],
            "pending"
        );
        assert_eq!(
            task["result"]["step_states"]["synthesize"]["status"],
            "pending"
        );
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn native_delegated_task_rejects_invalid_dags_before_write() {
        let root = std::env::temp_dir().join(format!(
            "narada-delegated-task-invalid-dag-{}",
            uuid::Uuid::new_v4()
        ));
        let invalid = json!({"task_id":"task_cycle","objective":"demo","execution":{"start":false},"workflow":{"steps":[{"id":"a","kind":"worker","depends_on":["b"]},{"id":"b","kind":"worker","depends_on":["a"]}]}});
        let error = task_run(invalid.as_object().unwrap(), &root).expect_err("cycle must refuse");
        assert_eq!(error["code"], "delegated_task_validation_failed");
        assert!(!root.join("tasks/task_cycle/task.json").exists());
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn native_delegated_task_schedules_only_dependency_ready_steps() {
        let mut task = json!({"workflow":{"steps":[{"id":"a","kind":"worker"},{"id":"b","kind":"worker","depends_on":["a"]}]},"result":{"step_states":{"a":{"status":"pending"},"b":{"status":"pending"}}}});
        assert_eq!(ready_step_ids(&task), vec!["a"]);
        task["result"]["step_states"]["a"]["status"] = json!("completed");
        assert_eq!(ready_step_ids(&task), vec!["b"]);
    }

    #[test]
    fn native_delegated_task_evaluates_conditions_and_acceptance() {
        let root = std::env::temp_dir().join(format!(
            "narada-delegated-task-semantics-{}",
            uuid::Uuid::new_v4()
        ));
        fs::create_dir_all(&root).expect("root");
        fs::write(root.join("proof.txt"), "accepted evidence").expect("proof");
        let task = json!({"status":"completed","objective":"demo","owner_site_id":"site-test","owner_site_root":root.to_string_lossy(),"result":{"acceptance_verdict":"passed","residual_risks":[],"verification":[{"command":"cargo test","status":"passed"}],"tools":["filesystem_search"],"step_states":{"review":{"kind":"review","status":"completed"}}},"acceptance":{"required_files":[{"path":"proof.txt","contains":"evidence"}],"required_tests":["cargo test"],"focused_tests":[{"command":"cargo test","status":"passed"}],"required_tools":["filesystem_search"],"forbidden_patterns":["forbidden-secret"],"verification_budget":{"max_attempts":2,"max_commands":2},"review_quorum":{"min_passed":1,"max_failed":0},"residual_risk_policy":"none_allowed"}});
        assert!(condition_passes(
            Some("all(step:review:completed,no_residual_risks)"),
            &task
        ));
        let (verdict, checks) = acceptance_verdict(&task, &root);
        assert_eq!(verdict, "passed");
        assert!(checks.len() >= 14);
        assert!(checks.iter().any(|check| check["kind"] == "output_contract"));
        assert!(checks.iter().any(|check| check["kind"] == "objective_outcome"));
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn acceptance_required_alias_reports_returned_fields() {
        let root = std::env::temp_dir().join(format!(
            "narada-delegated-task-required-fields-{}",
            uuid::Uuid::new_v4()
        ));
        let task = json!({
            "objective":"demo",
            "owner_site_id":"site-test",
            "owner_site_root":root.to_string_lossy(),
            "constraints":{"authority":"read"},
            "acceptance":{"required":["repository_name","current_branch","verification"]},
            "result":{"changed_files":[],"worker_outputs":[{"output":{"structured_output":{"repository_name":"marici","current_branch":"main","verification":"confirmed"}}}]}
        });
        let (_, checks) = acceptance_verdict(&task, &root);
        let fields = checks
            .iter()
            .find(|check| check["kind"] == "requested_fields")
            .expect("requested fields check");
        assert_eq!(
            fields["requested"],
            json!(["repository_name", "current_branch", "verification"])
        );
        assert_eq!(fields["status"], "passed");
    }

    #[test]
    fn acceptance_readback_refreshes_stale_requested_fields_check() {
        let root = std::env::temp_dir().join(format!(
            "narada-delegated-task-stale-acceptance-{}",
            uuid::Uuid::new_v4()
        ));
        let task = json!({
            "objective":"demo",
            "owner_site_id":"site-test",
            "owner_site_root":root.to_string_lossy(),
            "constraints":{"authority":"read"},
            "acceptance":{"required":["repository_name","current_branch","verification"]},
            "result":{
                "acceptance_checks":[
                    {"kind":"objective_present","status":"passed"},
                    {"kind":"requested_fields","requested":[],"returned":["repository_name","current_branch","verification"],"missing":[],"status":"not_applicable"}
                ],
                "worker_outputs":[{"output":{"structured_output":{"repository_name":"marici","current_branch":"main","verification":"confirmed"}}}]
            }
        });
        let result = task["result"].as_object().expect("result object");
        let checks = acceptance_checks_or_derive(&task, &root, Some(result));
        let fields = checks
            .as_array()
            .and_then(|checks| checks.iter().find(|check| check["kind"] == "requested_fields"))
            .expect("requested fields check");
        assert_eq!(
            fields["requested"],
            json!(["repository_name", "current_branch", "verification"])
        );
        assert_eq!(fields["status"], "passed");
    }

