    #[test]
    fn native_worker_reaps_nonterminal_record_with_explicit_force() {
        let root =
            std::env::temp_dir().join(format!("narada-worker-reap-{}", uuid::Uuid::new_v4()));
        let id = "run-fixture";
        let dir = run_root(&root).join(id);
        fs::create_dir_all(&dir).expect("dir");
        fs::write(dir.join("result.json"), format!(r#"{{"run_id":"{id}","status":"running","timing":{{"started_at":"2026-01-01T00:00:00Z"}}}}"#)).expect("record");
        let result = worker_run_reap(
            json!({"run_id":id,"reason":"fixture cleanup","force":true})
                .as_object()
                .unwrap(),
            &root,
        )
        .expect("reap");
        assert_eq!(result["status"], "reaped");
        assert_eq!(read_run(&root, id).expect("read")["status"], "cancelled");
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn native_worker_reconciles_prior_process_generation_before_broker_start() {
        let root = std::env::temp_dir().join(format!(
            "narada-worker-generation-reconcile-{}",
            uuid::Uuid::new_v4()
        ));
        let id = "run-prior-generation";
        let dir = run_root(&root).join(id);
        fs::create_dir_all(&dir).expect("dir");
        fs::write(
            dir.join("result.json"),
            serde_json::to_vec(&json!({
                "run_id":id,
                "status":"running",
                "completion_state":"pending",
                "phase":"awaiting_provider",
                "resolved_invocation":{"provider_broker_generation":"prior-process-generation"},
                "timing":{"started_at":"2026-01-01T00:00:00Z","finished_at":null}
            }))
            .expect("record"),
        )
        .expect("record write");
        let reconciled = read_reconciled_run(&root, id).expect("reconcile");
        assert_eq!(reconciled["status"], "orphaned");
        assert_eq!(
            reconciled["error"],
            "worker_orphaned:broker_generation_mismatch"
        );
        assert_eq!(
            reconciled["orphaned"]["current"],
            crate::codex_app_server_broker::current_generation().unwrap()
        );
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn provider_queue_timeout_is_distinct_and_discoverable() {
        let schema = input_schema("worker_run");
        assert_eq!(schema["properties"]["constraints"]["properties"]["queue_timeout_ms"]["default"], 300_000);
        let failure = queue_timeout_failure("run-fixture", 300_000, 300_001);
        assert_eq!(failure["code"], "provider_queue_timed_out");
        assert_eq!(failure["queue_timeout_ms"], 300_000);
        let root = std::env::temp_dir();
        assert_eq!(policy(&root, &[root.clone()])["provider_transports"]["codex-subscription"]["capacity"]["lanes"], 1);
    }
