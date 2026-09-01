    #[test]
    fn wait_inlines_terminal_handoff_fields() {
        let root = std::env::temp_dir().join(format!("narada-delegated-task-handoff-{}", uuid::Uuid::new_v4()));
        let task = json!({
            "schema":"narada.delegated_task.task.v1",
            "task_id":"task-terminal",
            "owner_site_id":root.file_name().and_then(|value| value.to_str()),
            "owner_site_root":root.to_string_lossy(),
            "status":"completed",
            "objective":"done",
            "result":{"acceptance_verdict":"passed","worker_outputs":[
                {"step_id":"review","status":"completed","output":{"summary_text":"done","structured_output":{"ok":true}}}
            ]},
            "workflow":{"steps":[{"id":"review","kind":"review"}]},
            "acceptance":{}
        });
        write_task(&root, &task).expect("task");
        let response = task_wait(
            json!({"task_id":"task-terminal","timeout_ms":0})
                .as_object()
                .unwrap(),
            &root,
        )
        .expect("wait");
        assert_eq!(response["terminal_handoff"]["task_status"], "completed");
        assert_eq!(response["terminal_handoff"]["final_structured_output"]["ok"], true);
        assert_eq!(response["terminal_handoff"]["details_tool"], "delegated_task_result");
        assert_eq!(response["task"]["role"], "identity_only");
        assert_eq!(response["task"]["details_ref"], response["terminal_handoff"]["details_ref"]);
        assert!(response["task"].get("final_structured_output").is_none());
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn terminal_worker_projection_updates_reference_and_handoff() {
        let mut task = json!({"result":{"step_states":{"inspect":{"status":"running"}},"worker_refs":[{"step_id":"inspect","run_id":"run-1","status":"running"}],"worker_outputs":[]}});
        record_worker_terminal(
            &mut task,
            "inspect",
            "run-1",
            "completed",
            &json!({"summary":"{\"repository\":\"marici\",\"branch\":\"main\"}"}),
        );
        assert_eq!(task["result"]["worker_refs"][0]["status"], "completed");
        assert_eq!(task["result"]["worker_refs"][0]["output"]["structured_output"]["branch"], "main");
        assert_eq!(task["result"]["step_states"]["inspect"]["worker_status"], "completed");
        assert_eq!(task["result"]["worker_outputs"][0]["output"]["structured_output"]["repository"], "marici");
    }

    #[test]
    fn required_structured_output_is_a_terminal_worker_contract() {
        let mut task = json!({
            "acceptance":{"required":["repository_name"]},
            "result":{"step_states":{"inspect":{"status":"running"}},"worker_refs":[{"step_id":"inspect","run_id":"run-1","status":"running"}],"worker_outputs":[]}
        });
        record_worker_terminal(
            &mut task,
            "inspect",
            "run-1",
            "completed",
            &json!({"summary":"repository_name is marici"}),
        );
        assert_eq!(task["result"]["step_states"]["inspect"]["worker_output_contract"], "failed");
        assert_eq!(task["result"]["worker_outputs"][0]["status"], "failed");
        assert_eq!(task["result"]["worker_outputs"][0]["output"]["structured_output_required"], true);
    }

    #[test]
    fn runtime_progress_without_terminal_event_cannot_pass() {
        let mut task = json!({
            "objective":"compute the two invariants",
            "result":{"step_states":{"implement":{"status":"running"}},"worker_refs":[{"step_id":"implement","run_id":"run-1","status":"running"}],"worker_outputs":[]}
        });
        record_worker_terminal(
            &mut task,
            "implement",
            "run-1",
            "completed",
            &json!({"runtime":"narada-agent-runtime-server","phase":"formatting_output","status":"completed","summary":"I am starting the computation now"}),
        );
        assert_eq!(task["result"]["worker_outputs"][0]["status"], "failed");
        assert_eq!(task["result"]["worker_outputs"][0]["output"]["worker_runtime_incomplete"], true);
        assert_eq!(objective_verdict(&task).0, "failed");
    }

    #[test]
    fn delegated_task_constraints_are_closed_and_validation_reports_resolution() {
        let schema = constraints_schema();
        assert_eq!(schema["additionalProperties"], false);
        assert_eq!(schema["properties"]["preflight_paths"]["items"]["additionalProperties"], false);
        let root = std::env::temp_dir().join(format!("narada-delegated-task-validate-{}", uuid::Uuid::new_v4()));
        let invalid = validate(
            &json!({"objective":"probe","constraints":{"unknown_field":"x"}}).as_object().unwrap(),
            &root,
        ).expect("validation response");
        assert_eq!(invalid["valid"], false);
        assert_eq!(invalid["diagnostics"][0]["code"], "unknown_constraint");
        let defaulted = validate(&json!({"objective":"probe"}).as_object().unwrap(), &root).expect("default validation");
        assert_eq!(defaulted["valid"], true);
        assert_eq!(defaulted["resolved_constraints"]["cognition"], "low");
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn validated_request_reference_prevents_drift_and_reuses_request() {
        let root = std::env::temp_dir().join(format!(
            "narada-delegated-task-validated-request-{}",
            uuid::Uuid::new_v4()
        ));
        let validation = validate(
            json!({
                "objective":"inspect repository",
                "constraints":{"authority":"read"},
                "execution":{"start":false}
            })
            .as_object()
            .unwrap(),
            &root,
        )
        .expect("validation");
        let reference = validation["validated_request_ref"]
            .as_str()
            .expect("validated request reference")
            .to_string();
        assert_eq!(validation["validation_persisted"], true);
        assert!(root.join(format!("validated-requests/{reference}.json")).is_file());
        let run = task_run(
            json!({"validated_request_ref":reference})
                .as_object()
                .unwrap(),
            &root,
        )
        .expect("run from validated request");
        assert_eq!(run["created"], true);
        let task = read_task(&root, run["task_id"].as_str().unwrap()).expect("task");
        assert_eq!(task["objective"], "inspect repository");
        assert_eq!(task["validated_request_ref"], reference);
        let drift = task_run(
            json!({"validated_request_ref":reference,"constraints":{"authority":"write"}})
                .as_object()
                .unwrap(),
            &root,
        )
        .expect_err("drift must be refused");
        assert_eq!(drift["code"], "validated_request_drift");
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn execute_identity_reuses_identical_validation_and_prioritizes_idempotency_key() {
        let root = std::env::temp_dir().join(format!(
            "narada-delegated-task-execute-idempotency-{}",
            uuid::Uuid::new_v4()
        ));
        let request = json!({
            "objective":"return ok",
            "idempotency_key":"execute-retry",
            "execution":{"start":false}
        });
        let first_validation =
            validate(request.as_object().unwrap(), &root).expect("first validation");
        let replay_validation =
            validate(request.as_object().unwrap(), &root).expect("replay validation");
        assert_eq!(
            first_validation["validated_request_ref"],
            replay_validation["validated_request_ref"]
        );
        let reference = first_validation["validated_request_ref"].clone();
        let first = task_run(
            json!({
                "validated_request_ref":reference,
                "idempotency_key":"execute-retry"
            })
            .as_object()
            .unwrap(),
            &root,
        )
        .expect("first run");
        let replay = task_run(
            json!({
                "validated_request_ref":replay_validation["validated_request_ref"],
                "idempotency_key":"execute-retry"
            })
            .as_object()
            .unwrap(),
            &root,
        )
        .expect("replay");
        assert_eq!(first["task_id"], replay["task_id"]);
        assert_eq!(first["created"], true);
        assert_eq!(replay["created"], false);

        let changed_validation = validate(
            json!({
                "objective":"different payload",
                "idempotency_key":"execute-retry",
                "execution":{"start":false}
            })
            .as_object()
            .unwrap(),
            &root,
        )
        .expect("changed validation");
        let conflict = task_run(
            json!({
                "validated_request_ref":changed_validation["validated_request_ref"],
                "idempotency_key":"execute-retry"
            })
            .as_object()
            .unwrap(),
            &root,
        )
        .expect_err("changed payload under the same key must conflict");
        assert_eq!(conflict["code"], "delegated_task_idempotency_conflict");
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn legacy_task_constraints_are_normalized_on_durable_readback() {
        let root = std::env::temp_dir().join(format!("narada-delegated-task-legacy-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(root.join("tasks/legacy_task")).expect("task root");
        fs::write(
            root.join("tasks/legacy_task/task.json"),
            serde_json::to_vec(&json!({"task_id":"legacy_task","status":"completed","objective":"legacy","constraints":{},"updated_at":"2026-01-01T00:00:00Z","result":{}})).expect("encode legacy task"),
        ).expect("legacy task");
        task_run(&json!({"task_id":"legacy_task","allow_cross_site":true}).as_object().unwrap(), &root).expect("normalize");
        let task = read_task(&root, "legacy_task").expect("readback");
        assert_eq!(task["constraints"]["cognition"], "low");
        fs::remove_dir_all(root).expect("cleanup");
    }

