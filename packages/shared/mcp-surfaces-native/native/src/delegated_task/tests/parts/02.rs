    #[test]
    fn executability_blocked_separates_contract_and_objective_verdicts() {
        let task = json!({
            "task_id":"task-assessment",
            "status":"completed",
            "objective":"read the target file",
            "workflow":{"steps":[{"id":"assessment","kind":"worker","profile":"shoshin-task-executability-v1","output_schema":{"name":"task_executability_assessment_v1","required":["findings"]}}]},
            "result":{"worker_outputs":[{"step_id":"assessment","status":"completed","output":{"summary_text":"The target could not be read.","structured_output":{"findings":[],"assessment_result":"undetermined"}}}],"step_states":{"assessment":{"kind":"worker","status":"completed","worker_output_contract":"passed"}}}
        });
        let (acceptance, checks) = acceptance_verdict(&task, Path::new("."));
        assert_eq!(output_contract_verdict(&task), "passed");
        assert_eq!(objective_verdict(&task).0, "blocked");
        assert_eq!(acceptance, "blocked");
        assert!(checks.iter().any(|check| check["kind"] == "objective_outcome" && check["verdict"] == "blocked"));
        assert_eq!(
            derived_task_summary(&task),
            Some(json!(
                "assessment_result: blocked. findings=[0 items], assessment_result=undetermined"
            ))
        );
    }

    #[test]
    fn running_executability_assessment_reports_pending_not_blocked() {
        let task = json!({
            "task_id":"task-assessment-running",
            "status":"accepted_for_execution",
            "objective":"read the target file",
            "workflow":{"steps":[{"id":"assessment","kind":"worker","profile":"shoshin-task-executability-v1","output_schema":{"name":"task_executability_assessment_v1","required":["findings"]}}]},
            "result":{"worker_outputs":[],"step_states":{"assessment":{"kind":"worker","status":"running"}}}
        });
        assert_eq!(objective_verdict(&task).0, "pending");
        assert_eq!(
            derived_task_summary(&task),
            Some(json!("assessment_result: pending. No substantive objective result was reported."))
        );
        let (acceptance, checks) = acceptance_verdict(&task, Path::new("."));
        assert_eq!(acceptance, "pending");
        assert!(checks.iter().any(|check| {
            check["kind"] == "objective_outcome" && check["verdict"] == "pending"
        }));
    }

    #[test]
    fn strict_clean_run_is_pending_then_auditable_at_terminal_state() {
        let mut task = json!({
            "status":"running",
            "objective":"inspect",
            "acceptance":{"strict_clean_run":true},
            "result":{"worker_outputs":[],"step_states":{"inspect":{"status":"running","attempts":1,"error":null}}}
        });
        let (pending, pending_checks) = acceptance_verdict(&task, Path::new("."));
        assert_eq!(pending, "pending");
        assert!(pending_checks.iter().any(|check| check["kind"] == "strict_clean_run" && check["status"] == "pending"));
        task["status"] = json!("completed");
        task["result"]["step_states"]["inspect"]["status"] = json!("completed");
        let (passed, terminal_checks) = acceptance_verdict(&task, Path::new("."));
        assert_eq!(passed, "passed");
        assert!(terminal_checks.iter().any(|check| check["kind"] == "strict_clean_run" && check["status"] == "passed"));
    }

    #[test]
    fn task_advance_recomputes_terminal_acceptance_after_marking_terminal() {
        let root = std::env::temp_dir().join(format!(
            "narada-delegated-task-terminal-verdict-{}",
            uuid::Uuid::new_v4()
        ));
        task_run(
            json!({
                "task_id":"task_terminal_verdict",
                "objective":"return status ok",
                "execution":{"start":false},
                "constraints":{"authority":"read"},
                "acceptance":{"strict_clean_run":true},
                "workflow":{"steps":[{"id":"implement","kind":"worker"}]}
            })
            .as_object()
            .unwrap(),
            &root,
        )
        .expect("create");
        let mut task = read_task(&root, "task_terminal_verdict").expect("task");
        task["result"]["step_states"]["implement"]["status"] = json!("completed");
        task["result"]["step_states"]["implement"]["attempts"] = json!(1);
        task["result"]["step_states"]["implement"]["error"] = Value::Null;
        write_task(&root, &task).expect("persist worker result");
        let terminal = task_advance(
            json!({"task_id":"task_terminal_verdict"})
                .as_object()
                .unwrap(),
            &root,
        )
        .expect("advance");
        assert_eq!(terminal["status"], "ok");
        assert_eq!(terminal["task_status"], "completed");
        assert_eq!(terminal["task"]["acceptance_verdict"], "passed");
        assert!(terminal["task"]["acceptance_checks"]
            .as_array()
            .is_some_and(|checks| checks.iter().any(|check| {
                check["kind"] == "strict_clean_run" && check["status"] == "passed"
            })));
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn terminal_handoff_exposes_task_timing() {
        let task = json!({"task_id":"timed","status":"completed","created_at":"2026-01-01T00:00:00Z","created_at_ms":1000,"started_at":"2026-01-01T00:00:01Z","started_at_ms":1200,"finished_at":"2026-01-01T00:00:02Z","finished_at_ms":2500,"duration_ms":1300,"result":{"acceptance_verdict":"passed","step_states":{},"worker_refs":[{"duration_ms":1000}]}});
        let handoff = terminal_handoff(&task, Path::new("."));
        assert_eq!(handoff["duration_ms"], 1300);
        assert_eq!(handoff["started_at"], "2026-01-01T00:00:01Z");
        assert_eq!(handoff["finished_at"], "2026-01-01T00:00:02Z");
        assert_eq!(handoff["timing"]["queue_ms"], 200);
        assert_eq!(handoff["timing"]["worker_ms"], 1000);
        assert_eq!(handoff["timing"]["orchestration_ms"], 300);
        assert_eq!(handoff["timing"]["total_ms"], 1500);
    }

    #[test]
    fn generic_completed_work_has_a_passed_objective_and_compact_result() {
        let task = json!({
            "task_id":"generic",
            "status":"completed",
            "objective":"say ok",
            "result":{
                "acceptance_verdict":"passed",
                "step_states":{},
                "worker_refs":[{"run_id":"run-1","output":{"structured_output":{"answer":"ok"}}}],
                "worker_outputs":[{"step_id":"implement","status":"completed","output":{"summary_text":"ok","structured_output":{"answer":"ok"}}}]
            }
        });
        assert_eq!(objective_verdict(&task).0, "passed");
        let compact = compact_task(&task, Path::new("."));
        assert_eq!(compact["final_structured_output"]["answer"], "ok");
        assert!(compact["worker_refs"].is_null());
        assert!(compact["worker_outputs"].is_null());
    }

    #[test]
    fn terminal_worker_without_substantive_result_cannot_pass() {
        let task = json!({
            "task_id":"progress-only",
            "status":"completed",
            "objective":"compute the two invariants",
            "result":{
                "worker_outputs":[{"step_id":"implement","status":"completed","output":{"summary_text":null}}],
                "step_states":{"implement":{"kind":"worker","status":"completed"}}
            }
        });
        assert_eq!(objective_verdict(&task).0, "failed");
        let (_, checks) = acceptance_verdict(&task, Path::new("."));
        assert!(checks.iter().any(|check| {
            check["kind"] == "objective_outcome"
                && check["signal"] == "missing_terminal_result"
                && check["status"] == "failed"
        }));
    }

    #[test]
    fn terminal_summary_replaces_persisted_pending_projection() {
        let task = json!({
            "task_id":"generic",
            "status":"completed",
            "summary":"objective_result: pending. waiting",
            "result":{"worker_outputs":[{"step_id":"implement","status":"completed","output":{"summary_text":"done"}}]}
        });
        assert_eq!(task_summary_value(&task), Some(json!("objective_result: passed. done")));
    }

    #[test]
    fn terminal_summary_prefers_complete_structured_output_over_clipped_worker_summary() {
        let task = json!({
            "task_id":"generic",
            "status":"completed",
            "result":{"worker_outputs":[{"step_id":"implement","status":"completed","output":{
                "summary_text":"topic=cross-sector falsifi",
                "structured_output":{"topic":"cross-sector falsification"}
            }}]}
        });
        assert_eq!(
            task_summary_value(&task),
            Some(json!("objective_result: passed. topic=cross-sector falsification"))
        );
    }

    #[test]
    fn summary_truncation_preserves_word_boundaries() {
        assert_eq!(truncate_summary("alpha beta gamma", 11), "alpha beta…");
        let long_summary = format!("{} falsifiability", "word ".repeat(102));
        let summary = structured_output_summary(&json!({"summary":long_summary}));
        assert!(summary.ends_with('…'));
        assert!(!summary.ends_with("falsifi…"));
        let concise = concise_value(&json!(format!("{}falsification", "word ".repeat(32))));
        assert!(concise.ends_with('…'));
        assert!(!concise.ends_with("falsifi…"));
    }

    #[test]
    fn batch_execute_is_bounded_ordered_and_failure_isolated() {
        let root = std::env::temp_dir().join(format!(
            "narada-delegated-task-batch-{}",
            uuid::Uuid::new_v4()
        ));
        let batch = task_execute_batch(
            json!({"items":[{"idempotency_key":"a"},{"idempotency_key":"b"}],"max_concurrency":2})
                .as_object()
                .unwrap(),
            &root,
            &[root.clone()],
        )
        .expect("batch response");
        assert_eq!(batch["status"], "partial_failure");
        assert_eq!(batch["requested_count"], 2);
        assert_eq!(batch["failed_count"], 2);
        assert_eq!(batch["results"][0]["index"], 0);
        assert_eq!(batch["results"][1]["index"], 1);
        assert_eq!(batch["results"][0]["error"]["code"], "delegated_task_validation_failed");
        assert_eq!(batch["results"][0]["error"]["validation"]["request_valid"], false);
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn compact_batch_result_keeps_terminal_verdicts_and_durable_readback() {
        let compact = compact_batch_result(&json!({
            "idempotency_replay":false,
            "run":{"task_id":"task-compact"},
            "terminal":{"terminal_handoff":{
                "task_id":"task-compact","task_status":"completed","summary":"objective_result: passed. ok",
                "output_contract_verdict":"passed","objective_verdict":"passed","acceptance_verdict":"passed",
                "timing":{"total_ms":42},"details_ref":"delegated-task://task-compact/result",
                "final_structured_output":{"large":"omitted"},"acceptance_checks":[{"large":"omitted"}]
            }}
        }));
        assert_eq!(compact["task_id"], "task-compact");
        assert_eq!(compact["objective_verdict"], "passed");
        assert_eq!(compact["timing"]["total_ms"], 42);
        assert_eq!(compact["details_ref"], "delegated-task://task-compact/result");
        assert!(compact.get("final_structured_output").is_none());
        assert!(compact.get("acceptance_checks").is_none());
    }

    #[test]
    fn assessment_result_object_is_canonical_and_rejects_contradictory_blocking_decisions() {
        let workflow = json!({"steps":[{"id":"assessment","kind":"worker","profile":"shoshin-task-executability-v1","output_schema":{"name":"task_executability_assessment_v1","required":["assessment_result","required_decisions"]}}]});
        let mut task = json!({
            "task_id":"task-assessment-object",
            "status":"completed",
            "objective":"assess the task",
            "workflow":workflow,
            "result":{"worker_outputs":[{"step_id":"assessment","status":"completed","output":{"summary_text":"assessment complete","structured_output":{"assessment_result":{"status":"executable","implementation_ready":true,"blockers":[]},"required_decisions":[]}}}],"step_states":{"assessment":{"kind":"worker","status":"completed","worker_output_contract":"passed"}}}
        });
        let (acceptance, checks) = acceptance_verdict(&task, Path::new("."));
        assert_eq!(output_contract_verdict(&task), "passed");
        assert_eq!(objective_verdict(&task).0, "passed");
        assert_eq!(acceptance, "passed");
        assert!(checks.iter().any(|check| {
            check["kind"] == "assessment_consistency" && check["status"] == "passed"
        }));

        task["result"]["worker_outputs"][0]["output"]["structured_output"]["required_decisions"] =
            json!([{"decision":"resolve dirty edits","blocking":true}]);
        let (acceptance, checks) = acceptance_verdict(&task, Path::new("."));
        assert_eq!(output_contract_verdict(&task), "failed");
        assert_eq!(objective_verdict(&task).0, "blocked");
        assert_eq!(acceptance, "failed");
        assert!(checks.iter().any(|check| {
            check["kind"] == "assessment_consistency"
                && check["status"] == "failed"
                && check["reasons"]
                    .as_array()
                    .is_some_and(|reasons| reasons.iter().any(|reason| {
                        reason == "executable_status_has_blocking_required_decisions"
                    }))
        }));
    }

