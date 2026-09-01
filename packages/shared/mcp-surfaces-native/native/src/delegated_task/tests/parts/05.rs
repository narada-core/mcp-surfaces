    #[test]
    fn native_delegated_task_enforces_owner_site_on_mutation() {
        let root = std::env::temp_dir().join(format!("site-current-{}", uuid::Uuid::new_v4()));
        let task =
            json!({"task_id":"task_owned","owner_site_id":"site-other","visibility_scope":"site"});
        let denied = assert_mutation_scope(&task, &Map::new(), &root)
            .expect_err("cross-site mutation denied");
        assert_eq!(denied["code"], "delegated_task_cross_site_mutation_denied");
        let allowed = assert_mutation_scope(
            &task,
            json!({"allow_cross_site":true,"expected_owner_site_id":"site-other"})
                .as_object()
                .unwrap(),
            &root,
        )
        .expect("override");
        assert_eq!(allowed["owner_site_id"], "site-other");
    }

    #[test]
    fn native_delegated_task_bounds_concurrency_and_waits_on_terminal_state() {
        let root = std::env::temp_dir().join(format!(
            "narada-delegated-task-wait-{}",
            uuid::Uuid::new_v4()
        ));
        let task = json!({"schema":"narada.delegated_task.task.v1","task_id":"task_terminal","owner_site_id":root.file_name().and_then(|v|v.to_str()),"visibility_scope":"site","status":"completed","objective":"done","constraints":{"max_concurrency":99},"result":{}});
        write_task(&root, &task).expect("task");
        assert_eq!(max_concurrency(&task), 32);
        let waited = task_wait(
            json!({"task_id":"task_terminal","timeout_ms":0})
                .as_object()
                .unwrap(),
            &root,
        )
        .expect("wait");
        assert_eq!(waited["status"], "finished");
        assert_eq!(waited["canonical_terminal_handoff"], true);
        assert_eq!(waited["result_readback_redundant"], true);
        let result = task_result(
            json!({"task_id":"task_terminal"}).as_object().unwrap(),
            &root,
        )
        .expect("result");
        assert_eq!(result["canonical_terminal_handoff"], true);
        assert_eq!(result["readback_role"], "secondary_durable_readback");
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn native_delegated_task_keeps_local_steps_out_of_worker_schedule() {
        let task = json!({"workflow":{"steps":[{"id":"gate","kind":"gate"},{"id":"join","kind":"join"},{"id":"note","kind":"note"}]},"result":{"step_states":{"gate":{"status":"pending"},"join":{"status":"pending"},"note":{"status":"pending"}}}});
        assert_eq!(ready_step_ids(&task), vec!["gate", "join", "note"]);
        for step in task["workflow"]["steps"].as_array().unwrap() {
            assert!(matches!(
                step["kind"].as_str(),
                Some("gate" | "join" | "note")
            ));
        }
    }

    #[test]
    fn native_delegated_task_stale_lock_has_one_reclaim_winner() {
        let root = std::env::temp_dir().join(format!(
            "narada-delegated-task-reclaim-{}",
            uuid::Uuid::new_v4()
        ));
        let lock = root.join("mutation.lockdir");
        fs::create_dir_all(&lock).expect("stale lock");
        fs::write(lock.join("owner.json"), "{}").expect("owner");
        let barrier = Arc::new(std::sync::Barrier::new(3));
        let contenders = (0..2)
            .map(|_| {
                let lock = lock.clone();
                let barrier = Arc::clone(&barrier);
                std::thread::spawn(move || {
                    barrier.wait();
                    reclaim_stale_lock(&lock)
                })
            })
            .collect::<Vec<_>>();
        barrier.wait();
        let winners = contenders
            .into_iter()
            .map(|contender| contender.join().expect("contender"))
            .filter(|won| *won)
            .count();
        assert_eq!(winners, 1);
        assert!(!lock.exists());
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn native_delegated_task_rejects_conflicting_idempotent_replay() {
        let root = std::env::temp_dir().join(format!(
            "narada-delegated-task-idempotency-{}",
            uuid::Uuid::new_v4()
        ));
        let first =
            json!({"objective":"first","execution":{"start":false},"idempotency_key":"stable"});
        task_run(first.as_object().unwrap(), &root).expect("first");
        let conflict =
            json!({"objective":"different","execution":{"start":false},"idempotency_key":"stable"});
        let error = task_run(conflict.as_object().unwrap(), &root).expect_err("conflict");
        assert_eq!(error["code"], "delegated_task_idempotency_conflict");
        let task_id = stable_task_id(first.as_object().unwrap());
        task_cancel(
            json!({"task_id":task_id}).as_object().unwrap(),
            &root,
            false,
        )
        .expect("terminal replay fixture");
        let replay = task_run(
            json!({"idempotency_key":"stable","execution":{"start":false}})
                .as_object()
                .unwrap(),
            &root,
        )
        .expect("identity replay");
        assert_eq!(replay["status"], "existing");
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn native_delegated_task_list_honors_lifecycle_views() {
        let root = std::env::temp_dir().join(format!(
            "narada-delegated-task-list-{}",
            uuid::Uuid::new_v4()
        ));
        task_run(
            json!({"task_id":"active","objective":"active","execution":{"start":false}})
                .as_object()
                .unwrap(),
            &root,
        )
        .expect("active");
        task_run(
            json!({"task_id":"terminal","objective":"terminal","execution":{"start":false}})
                .as_object()
                .unwrap(),
            &root,
        )
        .expect("terminal");
        task_cancel(
            json!({"task_id":"terminal"}).as_object().unwrap(),
            &root,
            false,
        )
        .expect("cancel");
        let active = tasks_list(json!({"view":"active_queue"}).as_object().unwrap(), &root)
            .expect("active list");
        let history =
            tasks_list(json!({"view":"history"}).as_object().unwrap(), &root).expect("history");
        assert_eq!(active["count"], 1);
        assert_eq!(history["count"], 1);
        fs::remove_dir_all(root).expect("cleanup");
    }

    fn complete_fixture_task(root: &Path, id: &str, output: Option<Value>) {
        let mut task = read_task(root, id).expect("fixture task");
        task["status"] = json!("completed");
        task["result"]["step_states"]["primary"]["status"] = json!("completed");
        task["result"]["step_states"]["primary"]["finished_at"] = json!(now());
        if let Some(output) = output {
            task["result"]["worker_outputs"] = json!([{"step_id":"primary","run_id":"fixture","status":"completed","output":{"structured_output":output,"summary_text":"fixture","truncated":false}}]);
        }
        write_task(root, &task).expect("complete fixture task");
    }

    #[test]
    fn cross_task_dependency_is_persisted_waits_and_imports_bounded_output() {
        let root = std::env::temp_dir().join(format!("narada-delegated-task-cross-dag-{}", uuid::Uuid::new_v4()));
        task_run(json!({"task_id":"task_a","objective":"extract typed list","constraints":{"authority":"read"},"execution":{"start":false}}).as_object().unwrap(), &root).expect("A");
        let b_args = json!({"task_id":"task_b","objective":"reduce typed list","constraints":{"authority":"read"},"depends_on_task_ids":["task_a"],"import_task_outputs":["task_a"],"workflow":{"steps":[{"id":"record","kind":"note"}]}});
        let b = task_run(b_args.as_object().unwrap(), &root).expect("B");
        assert_eq!(b["task_status"], "accepted_for_execution");
        let waiting = read_task(&root, "task_b").expect("persisted B");
        assert_eq!(waiting["depends_on_task_ids"], json!(["task_a"]));
        assert_eq!(waiting["import_task_outputs"], json!(["task_a"]));
        assert_eq!(waiting["external_dependencies"]["status"], "waiting");
        assert_eq!(waiting["result"]["step_states"]["record"]["status"], "pending");
        complete_fixture_task(&root, "task_a", Some(json!({"items":[3,1,2]})));
        let resolved = advance_task_closure(&root, "task_b", &[root.clone()], &mut std::collections::BTreeSet::new()).expect("automatic dependency closure");
        assert_eq!(resolved["status"], "completed");
        assert_eq!(resolved["external_dependencies"]["status"], "resolved");
        assert_eq!(resolved["result"]["imported_task_outputs"][0]["task_id"], "task_a");
        assert_eq!(resolved["result"]["imported_task_outputs"][0]["structured_output"]["items"], json!([3,1,2]));
        assert!(resolved["result"]["prior_step_outputs_ref"].as_str().is_some());
        let replay = task_run(b_args.as_object().unwrap(), &root).expect("idempotent replay");
        assert_eq!(replay["created"], false);
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn malformed_or_failed_predecessor_blocks_descendant_without_worker_start() {
        let root = std::env::temp_dir().join(format!("narada-delegated-task-cross-block-{}", uuid::Uuid::new_v4()));
        task_run(json!({"task_id":"task_a","objective":"extract","constraints":{"authority":"read"},"execution":{"start":false}}).as_object().unwrap(), &root).expect("A");
        task_run(json!({"task_id":"task_b","objective":"consume","constraints":{"authority":"read"},"depends_on_task_ids":["task_a"],"import_task_outputs":["task_a"],"execution":{"start":false}}).as_object().unwrap(), &root).expect("B");
        complete_fixture_task(&root, "task_a", None);
        let blocked = advance_task_closure(&root, "task_b", &[root.clone()], &mut std::collections::BTreeSet::new()).expect("blocked result");
        assert_eq!(blocked["status"], "failed");
        assert_eq!(blocked["external_dependencies"]["reason"], "predecessor_structured_output_missing");
        assert!(blocked["result"]["worker_refs"].as_array().is_some_and(Vec::is_empty));
        let events = task_events(json!({"task_id":"task_b","limit":20}).as_object().unwrap(), &root).expect("events");
        assert!(events["events"].as_array().is_some_and(|items| items.iter().any(|event| event["event_kind"] == "task_dependency_blocked")));
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn ordinary_status_reconciles_terminal_predecessor_failure() {
        let root = std::env::temp_dir().join(format!("narada-delegated-task-status-reconcile-{}", uuid::Uuid::new_v4()));
        task_run(json!({"task_id":"task_a","objective":"extract","constraints":{"authority":"read"},"execution":{"start":false}}).as_object().unwrap(), &root).expect("A");
        task_run(json!({"task_id":"task_b","objective":"consume","constraints":{"authority":"read"},"depends_on_task_ids":["task_a"],"execution":{"start":false}}).as_object().unwrap(), &root).expect("B");
        let mut predecessor = read_task(&root, "task_a").expect("predecessor");
        predecessor["status"] = json!("failed");
        write_task(&root, &predecessor).expect("fail predecessor");
        let status = task_status(json!({"task_id":"task_b"}).as_object().unwrap(), &root).expect("status reconciliation");
        assert_eq!(status["task_status"], "failed");
        assert_eq!(read_task(&root, "task_b").expect("persisted descendant")["external_dependencies"]["status"], "blocked");
        let events = task_events(json!({"task_id":"task_b","limit":20}).as_object().unwrap(), &root).expect("events");
        assert!(events["events"].as_array().is_some_and(|items| items.iter().any(|event| event["event_kind"] == "task_dependency_blocked")));
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn ordinary_status_reconciles_independent_worker_completion() {
        let root = std::env::temp_dir().join(format!(
            "narada-delegated-task-status-worker-reconcile-{}",
            uuid::Uuid::new_v4()
        ));
        task_run(
            json!({"task_id":"task_a","objective":"extract","constraints":{"authority":"read"},"execution":{"start":false}})
                .as_object()
                .unwrap(),
            &root,
        )
        .expect("task");
        let run_id = "run-status-reconcile";
        let mut task = read_task(&root, "task_a").expect("task record");
        task["status"] = json!("running");
        task["result"]["step_states"]["primary"]["status"] = json!("running");
        task["result"]["step_states"]["primary"]["attempts"] = json!(1);
        task["result"]["step_states"]["primary"]["current_run_id"] = json!(run_id);
        task["result"]["step_states"]["primary"]["run_ids"] = json!([run_id]);
        write_task(&root, &task).expect("running task");
        let run_dir = root
            .join(".narada/runtime/worker-delegation")
            .join(run_id);
        fs::create_dir_all(&run_dir).expect("worker run directory");
        fs::write(
            run_dir.join("result.json"),
            serde_json::to_vec(&json!({
                "run_id":run_id,
                "status":"completed",
                "completion_state":"complete",
                "phase":"completed",
                "summary":"fixture complete",
                "result":"fixture complete",
                "timing":{"started_at":"2026-01-01T00:00:00Z","finished_at":"2026-01-01T00:00:01Z","duration_ms":1000}
            }))
            .expect("worker record"),
        )
        .expect("worker record write");
        let status = task_status(
            json!({"task_id":"task_a"}).as_object().unwrap(),
            &root,
        )
        .expect("status reconciliation");
        assert_eq!(status["task_status"], "completed");
        assert_eq!(
            read_task(&root, "task_a").expect("persisted task")["result"]["step_states"]
                ["primary"]["status"],
            "completed"
        );
        fs::remove_dir_all(root).expect("cleanup");
    }

