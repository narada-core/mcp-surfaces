
    use super::*;

    #[test]
    fn delegated_task_defaults_cognition_to_low() {
        assert_eq!(policy(Path::new("."))["default_cognition"], "low");
        assert_eq!(guidance(&Map::new())["cognition"]["default"], "low");
        assert_eq!(
            list_tools()
                .iter()
                .find(|tool| tool["name"] == "delegated_task_validate")
                .expect("validate tool")["inputSchema"]["properties"]["constraints"]["properties"]["cognition"]["default"],
            "low"
        );
        assert_eq!(
            list_tools()
                .iter()
                .find(|tool| tool["name"] == "delegated_task_wait")
                .expect("wait tool")["inputSchema"]["properties"]["poll_ms"]["default"],
            5000
        );
        let root = std::env::temp_dir().join(format!(
            "narada-delegated-task-default-cognition-{}",
            uuid::Uuid::new_v4()
        ));
        task_run(
            json!({"task_id":"task_default_cognition","objective":"demo","execution":{"start":false}})
                .as_object()
                .unwrap(),
            &root,
        )
        .expect("run");
        let task = read_task(&root, "task_default_cognition").expect("task");
        assert_eq!(task["constraints"]["cognition"], "low");
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn completed_worker_output_is_structured_and_bounded() {
        let run = json!({"summary":"{\"repository\":\"marici\",\"branch\":\"main\"}"});
        let output = worker_output_from_run_with_required_fields(&run, &[]).expect("worker output");
        assert_eq!(output["structured_output"]["repository"], "marici");
        assert_eq!(output["structured_output"]["branch"], "main");
        assert_eq!(output["truncated"], false);
    }

    #[test]
    fn completed_native_worker_shape_is_terminal_without_terminal_event_field() {
        let mut task = json!({
            "acceptance":{"required":["items"]},
            "workflow":{"steps":[{"id":"extract","output_schema":{"required":["items"]}}]},
            "result":{"worker_refs":[{"step_id":"extract","run_id":"run-live","status":"running"}],"worker_outputs":[],"step_states":{"extract":{"status":"running"}}}
        });
        let run = json!({
            "run_id":"run-live",
            "status":"completed",
            "completion_state":"complete",
            "phase":"completed",
            "summary":"{\"items\":[3,1,2]}",
            "error":null
        });
        record_worker_terminal(&mut task, "extract", "run-live", "completed", &run);
        assert_eq!(task["result"]["step_states"]["extract"]["worker_status"], "completed");
        assert_eq!(task["result"]["step_states"]["extract"]["worker_output_contract"], "passed");
        assert_eq!(task["result"]["worker_outputs"][0]["output"]["structured_output"]["items"], json!([3,1,2]));
        assert!(task["result"]["worker_outputs"][0]["output"].get("worker_runtime_incomplete").is_none());
    }

    #[test]
    fn completed_worker_output_extracts_json_after_prose() {
        let run = json!({"summary":"I checked the repository. {\"repository\":\"marici\",\"branch\":\"main\"}"});
        let output = worker_output_from_run_with_required_fields(&run, &[]).expect("worker output");
        assert_eq!(output["structured_output"]["repository"], "marici");
        assert_eq!(output["structured_output"]["branch"], "main");
        assert_eq!(output["summary_text"], "repository=marici, branch=main");
        assert_eq!(output["diagnostics_text"], "I checked the repository.");
    }

    #[test]
    fn completed_worker_output_extracts_fenced_json_after_prose() {
        let run = json!({"summary":"The checks are complete.```json\n{\"repository\":\"marici\",\"branch\":\"main\"}\n```"});
        let output = worker_output_from_run_with_required_fields(&run, &[]).expect("worker output");
        assert_eq!(output["structured_output"]["repository"], "marici");
        assert_eq!(output["structured_output"]["branch"], "main");
    }

    #[test]
    fn completed_worker_output_extracts_multiline_fenced_json_after_prose() {
        let run = json!({"summary":"I’ll perform two read-only checks.```json\n{\n  \"directory_name\": \"marici\",\n  \"current_git_branch\": \"main\",\n  \"verification\": {\n    \"path\": \"C:\\\\Users\\\\andrey\\\\src\\\\marici\",\n    \"changes_made\": false\n  }\n}\n```"});
        let output = worker_output_from_run_with_required_fields(&run, &[]).expect("worker output");
        assert_eq!(output["structured_output"]["directory_name"], "marici");
        assert_eq!(output["structured_output"]["current_git_branch"], "main");
        assert_eq!(output["structured_output"]["verification"]["changes_made"], false);
    }

    #[test]
    fn completed_worker_output_normalizes_required_markdown_fields() {
        let run = json!({"summary":"- **repository_name**: marici\n- current_branch: main\n- verification: read-only check confirmed the branch."});
        let required = vec![
            "repository_name".to_string(),
            "current_branch".to_string(),
            "verification".to_string(),
        ];
        let output = worker_output_from_run_with_required_fields(&run, &required)
            .expect("worker output");
        assert_eq!(output["structured_output"]["repository_name"], "marici");
        assert_eq!(output["structured_output"]["current_branch"], "main");
        assert_eq!(output["structured_output"]["verification"], "read-only check confirmed the branch.");
        assert_eq!(output["structured_output_normalization"], "markdown_summary");
    }

    #[test]
    fn completed_worker_output_marks_missing_structured_output_explicitly() {
        let run = json!({"summary":"The repository is marici and the branch is main."});
        let required = vec!["repository_name".to_string(), "current_branch".to_string()];
        let output = worker_output_from_run_with_required_fields(&run, &required)
            .expect("worker output");
        assert_eq!(output["structured_output_required"], true);
        assert_eq!(output["structured_output_error"]["code"], "worker_structured_output_required");
    }

    #[test]
    fn structured_output_instruction_names_acceptance_fields() {
        let task = json!({"acceptance":{"required":["repository_name","current_branch"]}});
        let instruction = structured_output_instruction(&task).expect("contract");
        assert!(instruction.contains("repository_name, current_branch"));
        assert!(instruction.contains("exactly one JSON object"));
        assert!(instruction.contains("entire final answer"));
        assert!(!instruction.contains("explanation may follow"));
    }

    #[test]
    fn terminal_worker_poll_requests_full_durable_result() {
        let args = worker_status_args("run-test");
        assert_eq!(args.get("run_id"), Some(&json!("run-test")));
        assert_eq!(args.get("compact"), Some(&json!(false)));
    }

    #[test]
    fn structured_output_instruction_uses_step_schema_and_probe_contract() {
        let task = json!({"objective":"assess"});
        let step = json!({"output_schema":{"required":["dimensions","findings"]}});
        let instruction =
            structured_output_instruction_for_step(&task, Some(&step)).expect("contract");
        assert!(instruction.contains("dimensions, findings"));
        assert!(instruction.contains("READ-ONLY PROBE RULE"));
    }

    #[test]
    fn executability_instruction_requires_conditional_assessment_result() {
        let task = json!({"objective":"assess"});
        let step = json!({
            "output_schema": {
                "name": "task_executability_assessment_v1",
                "required": ["assessment_result"]
            }
        });
        let instruction =
            structured_output_instruction_for_step(&task, Some(&step)).expect("contract");
        assert!(instruction.contains("assessment_result MUST be an object"));
        assert!(instruction.contains("executable => implementation_ready=true"));
        assert!(instruction.contains("blocked => implementation_ready=false"));
    }

    #[test]
    fn executability_template_has_bounded_five_minute_worker_deadline() {
        let template = assessment_template();
        assert_eq!(template["bounds"]["max_run_ms"], 300_000);
        assert_eq!(template["steps"][0]["constraints"]["max_run_ms"], 300_000);
        assert!(template["output_schema"]["fields"]["assessment_result"]
            .as_str()
            .is_some_and(|description| description.contains("implementation_ready")));
        assert_eq!(
            template["output_schema"]["conditional_rules"]
                .as_array()
                .map(Vec::len),
            Some(3)
        );
    }

    #[test]
    fn merged_step_constraints_preserves_caller_read_preflight() {
        let task = json!({
            "constraints":{"preflight_paths":[{"path":"README.md","access":"read"}],"cwd":"C:/site"}
        });
        let step = json!({
            "constraints":{"authority":"read","preflight_paths":[],"max_run_ms":300_000}
        });
        let merged = merged_step_constraints(&task, &step);
        assert_eq!(merged["authority"], "read");
        assert_eq!(merged["max_run_ms"], 300_000);
        assert_eq!(merged["cwd"], "C:/site");
        assert_eq!(merged["preflight_paths"][0]["path"], "README.md");
    }

    #[test]
    fn lifecycle_worker_launch_is_always_asynchronous() {
        let task = json!({"constraints":{"authority":"read","wait_for_completion":true,"wait_timeout_ms":180000}});
        let step = json!({"constraints":{"max_run_ms":600000}});
        let constraints = asynchronous_worker_constraints(&task, &step);
        assert_eq!(constraints["wait_for_completion"], false);
        assert!(constraints.get("wait_timeout_ms").is_none());
        assert_eq!(constraints["max_run_ms"], 600000);
    }

    #[test]
    fn validation_reports_deferred_preflight_without_inspecting_filesystem() {
        let root = std::env::temp_dir().join(format!("narada-delegated-task-preflight-{}", uuid::Uuid::new_v4()));
        let response = validate(
            json!({"objective":"inspect","constraints":{"preflight_paths":[{"path":"does-not-exist.txt","access":"read"}]}})
                .as_object()
                .unwrap(),
            &root,
        )
        .expect("validation");
        assert_eq!(response["valid"], true);
        assert_eq!(response["request_valid"], true);
        assert_eq!(response["execution_preflight_pending"], true);
        assert_eq!(response["preflight_status"], "deferred");
        assert_eq!(response["preflight_authority"], "worker-delegation.worker_run");
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn template_catalog_defaults_to_compact_and_supports_detail_lookup() {
        let compact = template_catalog(&Map::new());
        assert_eq!(compact["mode"], "compact");
        assert!(compact["templates"][0].get("stages").is_some());
        assert!(compact["templates"][0].get("detail_available").is_some());
        assert!(compact["templates"][0].get("best_for").is_some());
        assert!(compact["templates"][0].get("avoid_when").is_some());
        let detail = template_catalog(
            json!({"template_id":"implement_review"})
                .as_object()
                .unwrap(),
        );
        assert_eq!(detail["mode"], "detail");
        assert!(detail["templates"][0].get("steps").is_some());
        assert!(detail["templates"][0]["best_for"].as_array().is_some_and(|items| !items.is_empty()));
        assert!(detail["templates"][0]["avoid_when"].as_array().is_some_and(|items| !items.is_empty()));
    }

    #[test]
    fn final_projection_uses_review_and_preserves_prior_output_reference() {
        let task = json!({
            "task_id":"task-review",
            "status":"completed",
            "workflow":{"steps":[
                {"id":"implement","kind":"worker"},
                {"id":"review","kind":"review","depends_on":["implement"]}
            ]},
            "result":{"worker_outputs":[
                {"step_id":"implement","status":"completed","output":{"summary_text":"implementation"}},
                {"step_id":"review","status":"completed","output":{"summary_text":"review","structured_output":{"verdict":"passed"}}}
            ]}
        });
        let projection = final_step_projection(&task);
        assert_eq!(projection["final_step"], "review");
        assert_eq!(projection["final_structured_output"]["verdict"], "passed");
        assert_eq!(
            derived_task_summary(&task),
            Some(json!("objective_result: passed. verdict=passed"))
        );
        assert!(projection["prior_step_outputs_ref"].as_str().is_some());
    }

