#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn native_sop_mutations_publish_named_closed_schemas() {
        let tools = list_tools();
        for name in MUTATING {
            let tool = tools.iter().find(|tool| tool["name"] == *name).unwrap_or_else(|| panic!("missing tool {name}"));
            assert_eq!(tool["inputSchema"]["additionalProperties"], false, "{name} must reject misspelled arguments");
            assert!(tool["inputSchema"]["properties"].as_object().is_some_and(|properties| !properties.is_empty()), "{name} must advertise named arguments");
        }
        let compound = tools.iter().find(|tool| tool["name"] == "sop_handoff_claim_and_advance").expect("compound handoff tool");
        assert_eq!(compound["inputSchema"]["required"], json!(["consumer_id", "completion_key", "outcome", "principal"]));
    }

    #[test]
    fn native_sop_template_read_is_bounded() {
        let root = std::env::temp_dir().join(format!("narada-sop-{}", uuid::Uuid::new_v4())); let dir = root.join("sops"); fs::create_dir_all(&dir).expect("dir"); fs::write(dir.join("demo.sop.yaml"), "schema: narada.sop.v1\nid: demo\n").expect("yaml");
        assert_eq!(candidate_list(&json!({"limit":1}).as_object().unwrap(), &root).expect("list")["count"], 1);
        assert!(candidate_show(&json!({"sop_id":"demo"}).as_object().unwrap(), &root).expect("show")["raw_yaml"].as_str().unwrap().contains("demo"));
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn native_sop_registry_reads_templates_without_execution() {
        let root = std::env::temp_dir().join(format!("narada-sop-db-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(root.join(".sop")).expect("root");
        let connection = Connection::open(db_path(&root)).expect("db");
        connection.execute_batch("CREATE TABLE sop_templates (sop_id TEXT, version INTEGER, title TEXT, status TEXT, description TEXT, steps_json TEXT, trigger_kind TEXT, input_schema_json TEXT, output_mapping_json TEXT, output_ref_mapping_json TEXT, output_schema_json TEXT, acceptance_criteria_json TEXT, evidence_requirements_json TEXT, created_at TEXT, updated_at TEXT); INSERT INTO sop_templates VALUES ('demo',1,'Demo','active','A demo','[{\"id\":\"step-1\"}]','manual',NULL,NULL,NULL,NULL,'[]','[]','2026-01-01','2026-01-01');").expect("schema");
        drop(connection);
        assert_eq!(template_list(&json!({"limit":1}).as_object().unwrap(), &root).expect("list")["count"], 1);
        assert_eq!(template_show(&json!({"sop_id":"demo"}).as_object().unwrap(), &root).expect("show")["steps"][0]["id"], "step-1");
        assert_eq!(template_search(&json!({"query":"Demo"}).as_object().unwrap(), &root).expect("search")["count"], 1);
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn native_sop_action_reads_are_bounded_and_read_only() {
        let root = std::env::temp_dir().join(format!("narada-sop-actions-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(root.join(".sop")).expect("root");
        let connection = Connection::open(db_path(&root)).expect("db");
        connection.execute_batch("CREATE TABLE sop_actions (action_id TEXT, run_id TEXT, step_id TEXT, occurrence_key TEXT, surface_id TEXT, tool_name TEXT, arguments_json TEXT, request_fingerprint TEXT, status TEXT, completion_key TEXT, completion_fingerprint TEXT, operation_ref TEXT, result_json TEXT, result_ref_json TEXT, error_message TEXT, created_at TEXT, updated_at TEXT, completed_at TEXT); INSERT INTO sop_actions VALUES ('action-1','run-1','step-1','occ-1','surface','tool','{}','fingerprint','pending',NULL,NULL,NULL,'{}',NULL,NULL,'2026-01-01','2026-01-01',NULL);").expect("schema");
        drop(connection);
        let list = action_list(&json!({"limit":1}).as_object().unwrap(), &root).expect("list");
        assert_eq!(list["count"], 1);
        assert_eq!(list["items"][0]["action_id"], "action-1");
        let show = action_show(&json!({"action_id":"action-1"}).as_object().unwrap(), &root).expect("show");
        assert_eq!(show["schema"], "narada.sop.action.v1");
        assert_eq!(show["arguments"], json!({}));
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn native_sop_run_list_rediscovers_terminal_runs_by_default() {
        let root = std::env::temp_dir().join(format!("narada-sop-runs-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(root.join(".sop")).expect("root");
        let connection = Connection::open(db_path(&root)).expect("db");
        connection.execute_batch("CREATE TABLE sop_runs (run_id TEXT, sop_id TEXT, sop_version INTEGER, sop_title TEXT, status TEXT, occurrence_key TEXT, parent_run_id TEXT, parent_step_id TEXT, created_at TEXT, updated_at TEXT, completed_at TEXT); INSERT INTO sop_runs VALUES ('run-1','demo',1,'Demo','running','occ-1',NULL,NULL,'2026-01-01','2026-01-01',NULL); INSERT INTO sop_runs VALUES ('run-2','demo',1,'Demo','completed','occ-2',NULL,NULL,'2026-01-02','2026-01-02','2026-01-02');").expect("schema");
        drop(connection);
        let list = run_list(&json!({"limit":10}).as_object().unwrap(), &root).expect("list");
        assert_eq!(list["count"], 2);
        assert_eq!(list["items"][0]["run_id"], "run-2");
        let active = run_list(&json!({"limit":10,"include_terminal":false}).as_object().unwrap(), &root).expect("active list");
        assert_eq!(active["count"], 1);
        assert_eq!(active["items"][0]["run_id"], "run-1");
        let invalid = run_list(&json!({"status":"unknown"}).as_object().unwrap(), &root).expect_err("status validation");
        assert_eq!(invalid["code"], "sop_run_status_unsupported");
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn native_sop_run_status_rehydrates_bounded_public_projection() {
        let root = std::env::temp_dir().join(format!("narada-sop-run-status-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(root.join(".sop")).expect("root");
        let connection = Connection::open(db_path(&root)).expect("db");
        connection.execute_batch("CREATE TABLE sop_runs (run_id TEXT, sop_id TEXT, sop_version INTEGER, sop_title TEXT, status TEXT, occurrence_key TEXT, request_fingerprint TEXT, definition_fingerprint TEXT, definition_json TEXT, input_json TEXT, input_ref_json TEXT, output_json TEXT, output_ref_json TEXT, step_states_json TEXT, trigger_source_kind TEXT, trigger_source_ref TEXT, triggered_by TEXT, parent_run_id TEXT, parent_step_id TEXT, created_at TEXT, updated_at TEXT, completed_at TEXT);").expect("schema");
        let steps = r#"[{"step_id":"step-1","executor":"operator","blocking":true,"title":"Approve","status":"running","depends_on":[],"instructions":"approve","when":null,"input":{},"input_ref":null,"result_schema":null,"action":null,"sop_id":null,"sop_version":null,"wait_policy":null,"pinned_child_definition_fingerprint":null,"child_run_id":null,"action_id":null,"started_at":"2026-01-01","completed_at":null,"result":{"instructions":"approve now"},"result_ref":null,"completion_key":null,"completion_fingerprint":null,"error_message":null}]"#;
        connection.execute("INSERT INTO sop_runs (run_id,sop_id,sop_version,sop_title,status,occurrence_key,request_fingerprint,definition_fingerprint,definition_json,input_json,input_ref_json,output_json,output_ref_json,step_states_json,trigger_source_kind,trigger_source_ref,triggered_by,parent_run_id,parent_step_id,created_at,updated_at,completed_at) VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)", params!["run-1", "demo", 1, "Demo", "awaiting_confirmation", "occ-1", "request-fp", "definition-fp", r#"{"steps":[]}"#, r#"{"input":1}"#, Option::<String>::None, r#"{}"#, Option::<String>::None, steps, "manual", "", "operator", Option::<String>::None, Option::<String>::None, "2026-01-01", "2026-01-01", Option::<String>::None]).expect("run");
        drop(connection);
        let status = run_status(&json!({"run_id":"run-1"}).as_object().unwrap(), &root).expect("status");
        assert_eq!(status["schema"], "narada.sop.run.v2");
        assert_eq!(status["step_states"][0]["status"], "running");
        assert_eq!(status["next_awaits_confirmation"], true);
        assert_eq!(status["next_step"]["instructions"], "approve now");
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn native_sop_handoff_reads_redact_lease_tokens() {
        let root = std::env::temp_dir().join(format!("narada-sop-handoffs-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(root.join(".sop")).expect("root");
        let connection = Connection::open(db_path(&root)).expect("db");
        connection.execute_batch("CREATE TABLE sop_handoffs (handoff_id TEXT, run_id TEXT, step_id TEXT, occurrence_key TEXT, sop_id TEXT, sop_version INTEGER, executor TEXT, title TEXT, instructions TEXT, input_json TEXT, input_ref_json TEXT, result_schema_json TEXT, request_fingerprint TEXT, status TEXT, lease_owner TEXT, lease_token TEXT, lease_expires_at TEXT, attempt_count INTEGER, last_error TEXT, completion_key TEXT, completion_fingerprint TEXT, principal TEXT, result_json TEXT, result_ref_json TEXT, error_message TEXT, created_at TEXT, updated_at TEXT, completed_at TEXT); INSERT INTO sop_handoffs VALUES ('handoff-1','run-1','step-1','occ-1','demo',1,'operator','Approve','approve now','{}',NULL,NULL,'request-fp','leased','consumer','secret-token','2026-01-01T01:00:00Z',1,NULL,NULL,NULL,NULL,'{}',NULL,NULL,'2026-01-01','2026-01-01',NULL);").expect("schema");
        drop(connection);
        let list = handoff_list(&json!({"status":"leased"}).as_object().unwrap(), &root).expect("list");
        assert_eq!(list["count"], 1);
        assert!(list["items"][0].get("lease_token").is_none());
        let show = handoff_show(&json!({"handoff_id":"handoff-1"}).as_object().unwrap(), &root).expect("show");
        assert_eq!(show["schema"], "narada.sop.handoff.v1");
        assert!(show.get("lease_token").is_none());
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn native_sop_run_events_page_durable_records() {
        let root = std::env::temp_dir().join(format!("narada-sop-events-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(root.join(".sop")).expect("root");
        let connection = Connection::open(db_path(&root)).expect("db");
        connection.execute_batch("CREATE TABLE sop_events (event_id TEXT, run_id TEXT, step_id TEXT, event_kind TEXT, details_json TEXT, recorded_at TEXT); INSERT INTO sop_events VALUES ('event-1','run-1','step-1','step_started','{\"detail\":1}','2026-01-01'); INSERT INTO sop_events VALUES ('event-2','run-1','','run_completed','{}','2026-01-02');").expect("schema");
        drop(connection);
        let page = run_events(&json!({"run_id":"run-1","limit":1}).as_object().unwrap(), &root).expect("events");
        assert_eq!(page["count"], 1);
        assert_eq!(page["items"][0]["event_id"], "event-2");
        assert_eq!(page["items"][0]["details"], json!({}));
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn native_sop_coverage_classifies_stale_templates() {
        let root = std::env::temp_dir().join(format!("narada-sop-coverage-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(root.join(".sop")).expect("root");
        let connection = Connection::open(db_path(&root)).expect("db");
        connection.execute_batch("CREATE TABLE sop_templates (sop_id TEXT, version INTEGER, title TEXT, status TEXT, updated_at TEXT); CREATE TABLE sop_runs (run_id TEXT, sop_id TEXT, sop_version INTEGER, sop_title TEXT, status TEXT, occurrence_key TEXT, parent_run_id TEXT, parent_step_id TEXT, created_at TEXT, updated_at TEXT, completed_at TEXT); INSERT INTO sop_templates VALUES ('demo',1,'Demo','active','2026-01-01T00:00:00Z'); INSERT INTO sop_runs VALUES ('run-1','demo',1,'Demo','running','occ-1',NULL,NULL,'2026-01-01T00:00:00Z','2026-01-01T00:00:00Z',NULL);").expect("schema");
        drop(connection);
        let coverage = run_coverage_since(&json!({"since":"2026-02-01T00:00:00Z"}).as_object().unwrap(), &root).expect("coverage");
        assert_eq!(coverage["count"], 1);
        assert_eq!(coverage["items"][0]["classification"], "stale");
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn native_sop_outbox_list_respects_consumer_start_and_receipts() {
        let root = std::env::temp_dir().join(format!("narada-sop-outbox-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(root.join(".sop")).expect("root");
        let connection = Connection::open(db_path(&root)).expect("db");
        connection.execute_batch("CREATE TABLE sop_outbox (event_id TEXT, topic TEXT, partition_key TEXT, run_id TEXT, sop_id TEXT, sop_version INTEGER, occurrence_key TEXT, outcome TEXT, payload_json TEXT, created_at TEXT, available_at TEXT, compacted_at TEXT); CREATE TABLE sop_outbox_consumer_requirements (topic TEXT, consumer_id TEXT, start_at TEXT, registered_at TEXT); CREATE TABLE sop_outbox_receipts (event_id TEXT, consumer_id TEXT, processed_at TEXT, receipt_json TEXT); INSERT INTO sop_outbox_consumer_requirements VALUES ('sop.run.terminal.v1','consumer-1','2026-01-01T00:00:00Z','2026-01-01T00:00:00Z'); INSERT INTO sop_outbox VALUES ('event-1','sop.run.terminal.v1','run-1','run-1','demo',1,'occ-1','completed','{\"status\":\"completed\"}','2026-01-02T00:00:00Z','2026-01-02T00:00:00Z',NULL); INSERT INTO sop_outbox VALUES ('event-2','sop.run.terminal.v1','run-2','run-2','demo',1,'occ-2','completed','{}','2026-01-03T00:00:00Z','2026-01-03T00:00:00Z',NULL); INSERT INTO sop_outbox_receipts VALUES ('event-2','consumer-1','2026-01-04T00:00:00Z','{}');").expect("schema");
        drop(connection);
        let page = outbox_list(&json!({"consumer_id":"consumer-1"}).as_object().unwrap(), &root).expect("outbox");
        assert_eq!(page["count"], 1);
        assert_eq!(page["items"][0]["event_id"], "event-1");
        assert_eq!(page["items"][0]["payload"]["status"], "completed");
        fs::remove_dir_all(root).expect("cleanup");
    }
}
