    #[test]
    fn cross_task_dependencies_reject_missing_imports_cycles_and_authority_escalation() {
        let root = std::env::temp_dir().join(format!("narada-delegated-task-cross-invalid-{}", uuid::Uuid::new_v4()));
        task_run(json!({"task_id":"task_a","objective":"A","constraints":{"authority":"read"},"execution":{"start":false}}).as_object().unwrap(), &root).expect("A");
        let missing = task_run(json!({"task_id":"task_missing","objective":"missing","constraints":{"authority":"read"},"depends_on_task_ids":["absent"]}).as_object().unwrap(), &root).expect_err("missing predecessor");
        assert_eq!(missing["code"], "delegated_task_validation_failed");
        let undeclared = task_run(json!({"task_id":"task_import","objective":"import","constraints":{"authority":"read"},"import_task_outputs":["task_a"]}).as_object().unwrap(), &root).expect_err("undeclared import");
        assert_eq!(undeclared["code"], "delegated_task_validation_failed");
        let escalation = task_run(json!({"task_id":"task_write","objective":"write","constraints":{"authority":"write"},"depends_on_task_ids":["task_a"]}).as_object().unwrap(), &root).expect_err("authority escalation");
        assert_eq!(escalation["code"], "delegated_task_validation_failed");
        let mut a = read_task(&root, "task_a").expect("A task");
        a["depends_on_task_ids"] = json!(["task_cycle"]);
        write_task(&root, &a).expect("legacy cyclic edge fixture");
        let cycle = task_run(json!({"task_id":"task_cycle","objective":"cycle","constraints":{"authority":"read"},"depends_on_task_ids":["task_a"]}).as_object().unwrap(), &root).expect_err("cycle");
        assert_eq!(cycle["code"], "delegated_task_validation_failed");
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn delegated_tasks_propagate_queue_budget_and_default_to_provider_capacity() {
        let schema = constraints_schema();
        assert_eq!(schema["properties"]["queue_timeout_ms"]["default"], 300_000);
        assert!(CONSTRAINT_FIELDS.contains(&"queue_timeout_ms"));
        assert_eq!(max_concurrency(&json!({})), 1);
        assert_eq!(asynchronous_worker_constraints(
            &json!({"constraints":{"queue_timeout_ms":600000}}),
            &json!({})
        )["queue_timeout_ms"], 600_000);
    }
