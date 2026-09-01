#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> PathBuf {
        let root = std::env::temp_dir().join(format!("narada-scheduler-native-{}", Uuid::new_v4()));
        fs::create_dir_all(&root).expect("root");
        prepare(&root).expect("prepare");
        root
    }
    fn call(root: &Path, name: &str, args: Value) -> Result<Value, Value> {
        call_tool(name, args.as_object().expect("args"), root)
    }
    fn binding() -> Value {
        json!({"binding_id":"mailbox-sync-continuation","trigger_kind":"completion","source_topic":"sop.run.terminal.v1","source_sop_id":"sonar.mailbox-sync","terminal_outcomes":["synced","retryable_failure","blocked"],"target_sop_id":"sonar.mailbox-sync","target_template_version":"v1","concurrency":"singleton","delay_by_outcome_ms":{"synced":0},"retry_base_ms":0,"retry_max_ms":1000,"max_attempts":3})
    }
    fn event(id: &str, outcome: &str) -> Value {
        json!({"event_id":id,"topic":"sop.run.terminal.v1","partition_key":"sonar.mailbox-sync","aggregate_id":format!("run-{id}"),"aggregate_revision":1,"schema_version":1,"causation_id":id,"idempotency_key":id,"payload":{"sop_id":"sonar.mailbox-sync","outcome":outcome},"occurred_at":now_iso()})
    }
    #[test]
    fn prepare_and_inspect_are_explicit() {
        let root = std::env::temp_dir().join(format!("narada-scheduler-native-{}", Uuid::new_v4()));
        assert_eq!(doctor(&root)["preparation"]["status"], "missing");
        prepare(&root).expect("prepare");
        assert_eq!(doctor(&root)["preparation"]["status"], "prepared");
        let _ = fs::remove_dir_all(root);
    }
    #[test]
    fn prepare_migrates_legacy_leases_without_replaying_them_as_live() {
        let root = std::env::temp_dir().join(format!("narada-scheduler-native-{}", Uuid::new_v4()));
        fs::create_dir_all(root.join(".ai")).expect("root");
        let db = Connection::open(root.join(DB_RELATIVE)).expect("db");
        db.execute_batch(
            "pragma journal_mode=wal;
             create table scheduler_meta(singleton integer primary key, schema_version integer not null, prepared_at text not null);
             create table scheduler_bindings(binding_id text primary key, trigger_kind text, source_topic text, source_sop_id text, terminal_outcomes_json text, target_sop_id text, target_template_version text, concurrency text, delay_by_outcome_ms_json text, default_delay_ms integer, retry_base_ms integer, retry_max_ms integer, max_attempts integer, blocked_policy text, status text, revision integer, spec_digest text, created_at text, updated_at text);
             create table scheduler_source_events(event_id text primary key, topic text, partition_key text, aggregate_id text, aggregate_revision integer, schema_version integer, causation_id text, idempotency_key text, payload_json text, event_digest text, occurred_at text, admitted_at text);
             create table scheduler_activations(activation_id text primary key, binding_id text, source_event_id text, occurrence_key text, target_sop_id text, target_template_version text, partition_key text, due_at text, status text, attempt_count integer, lease_owner text, lease_expires_at text, sop_run_id text, terminal_outcome text, last_error text, created_at text, updated_at text);
             create table scheduler_activation_receipts(activation_id text, receipt_kind text, receipt_id text, receipt_json text, recorded_at text);
             insert into scheduler_meta values(1,1,'2026-01-01T00:00:00.000Z');
             insert into scheduler_activations values('a','b','e','o','s','v1','p','2026-01-01T00:00:00.000Z','leased',0,'old-owner','2026-01-01T00:01:00.000Z',null,null,null,'2026-01-01T00:00:00.000Z','2026-01-01T00:00:00.000Z');",
        ).expect("legacy schema");
        drop(db);
        prepare(&root).expect("migrate");
        let db = Connection::open(root.join(DB_RELATIVE)).expect("reopen");
        let row:(String,i64,Option<String>)=db.query_row("select status,attempt_count,lease_token from scheduler_activations where activation_id='a'",[],|row|Ok((row.get(0)?,row.get(1)?,row.get(2)?))).expect("activation");
        assert_eq!(row, ("pending".into(), 1, None));
        let _ = fs::remove_dir_all(root);
    }
    #[test]
    fn events_are_replay_safe_and_revision_guarded() {
        let root = fixture();
        let created = call(&root, "scheduler_binding_upsert", binding()).expect("binding");
        assert_eq!(created["binding"]["revision"], 1);
        let admitted_event = event("event-1", "synced");
        let first = call(&root, "scheduler_event_admit", admitted_event.clone()).expect("admit");
        assert_eq!(first["status"], "admitted");
        assert_eq!(first["activation_count"], 1);
        let replay = call(&root, "scheduler_event_admit", admitted_event).expect("replay");
        assert_eq!(replay["status"], "replayed");
        assert!(call(&root, "scheduler_event_admit", event("event-1", "blocked")).is_err());
        let mut changed = binding().as_object().unwrap().clone();
        changed.insert("default_delay_ms".into(), json!(1));
        assert!(call_tool("scheduler_binding_upsert", &changed, &root).is_err());
        let _ = fs::remove_dir_all(root);
    }
    #[test]
    fn binding_and_activation_lists_are_cursor_pageable() {
        let root = fixture();
        let mut first = binding().as_object().unwrap().clone();
        first.insert("binding_id".into(), json!("binding-a"));
        let mut second = binding().as_object().unwrap().clone();
        second.insert("binding_id".into(), json!("binding-b"));
        call_tool("scheduler_binding_upsert", &first, &root).expect("first binding");
        call_tool("scheduler_binding_upsert", &second, &root).expect("second binding");
        let first_page = call(
            &root,
            "scheduler_binding_list",
            json!({"limit":1,"offset":0}),
        )
        .expect("first page");
        let second_page = call(
            &root,
            "scheduler_binding_list",
            json!({"limit":1,"offset":1}),
        )
        .expect("second page");
        assert_eq!(first_page["returned"], 1);
        assert_eq!(first_page["has_more"], true);
        assert_eq!(first_page["next_offset"], 1);
        assert_ne!(
            first_page["bindings"][0]["binding_id"],
            second_page["bindings"][0]["binding_id"]
        );

        call(
            &root,
            "scheduler_event_admit",
            event("event-page", "synced"),
        )
        .expect("event");
        let activation_page = call(
            &root,
            "scheduler_activation_list",
            json!({"limit":1,"offset":0}),
        )
        .expect("activation page");
        assert_eq!(activation_page["status"], "ok");
        assert_eq!(activation_page["returned"], 1);
        assert_eq!(activation_page["bounded"], true);
        let _ = fs::remove_dir_all(root);
    }
    #[test]
    fn lease_receipts_hold_singleton_until_terminal() {
        let root = fixture();
        call(&root, "scheduler_binding_upsert", binding()).expect("binding");
        call(&root, "scheduler_event_admit", event("event-1", "synced")).expect("event1");
        call(&root, "scheduler_event_admit", event("event-2", "synced")).expect("event2");
        let claim = call(
            &root,
            "scheduler_activation_claim",
            json!({"consumer_id":"dispatcher","lease_ms":30000}),
        )
        .expect("claim")["activation"]
            .clone();
        let id = claim["activation_id"].as_str().unwrap();
        let token = claim["lease_token"].as_str().unwrap();
        call(&root,"scheduler_activation_admit_sop",json!({"activation_id":id,"consumer_id":"dispatcher","lease_token":token,"sop_run_id":"run-1","receipt_id":"admit-1","receipt":{}})).expect("admit sop");
        assert!(call(
            &root,
            "scheduler_activation_claim",
            json!({"consumer_id":"dispatcher"})
        )
        .expect("blocked")["activation"]
            .is_null());
        call(
            &root,
            "scheduler_activation_resolve",
            json!({"sop_run_id":"run-1","outcome":"synced","receipt_id":"terminal-1","receipt":{}}),
        )
        .expect("resolve");
        assert!(!call(
            &root,
            "scheduler_activation_claim",
            json!({"consumer_id":"dispatcher"})
        )
        .expect("next")["activation"]
            .is_null());
        let _ = fs::remove_dir_all(root);
    }
    #[test]
    fn blocked_activation_requires_explicit_unblock() {
        let root = fixture();
        call(&root, "scheduler_binding_upsert", binding()).expect("binding");
        let admitted = call(
            &root,
            "scheduler_event_admit",
            event("event-blocked", "blocked"),
        )
        .expect("event");
        let id = admitted["activations"][0]["activation_id"]
            .as_str()
            .unwrap();
        assert_eq!(admitted["activations"][0]["status"], "blocked");
        let unblocked = call(
            &root,
            "scheduler_activation_unblock",
            json!({"activation_id":id}),
        )
        .expect("unblock");
        assert_eq!(unblocked["activation"]["status"], "pending");
        let _ = fs::remove_dir_all(root);
    }
}
