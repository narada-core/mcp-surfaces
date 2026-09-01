#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_mailbox_scans_bounded_projection() {
        let root = std::env::temp_dir().join(format!("narada-mailbox-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(root.join(".ai/mailboxes/acct")).expect("root");
        fs::write(root.join(".ai/mailboxes/acct/messages.json"), r#"[{"id":"m1","subject":"hello","folder":"Inbox","body":{"content":"world"},"receivedDateTime":"2026-01-01T00:00:00Z","isRead":false}]"#).expect("file");
        fs::write(root.join(".ai/mailboxes/acct/settings.json"), r#"{"id":"settings","enabled":true}"#).expect("settings");
        fs::create_dir_all(root.join(".ai/mailboxes/acct/views/by-thread")).expect("views");
        fs::write(root.join(".ai/mailboxes/acct/views/by-thread/m1.json"), r#"{"id":"m1","subject":"derived view should lose","conversationId":"thread-1","text":"view"}"#).expect("view");
        let result = messages(&json!({"limit":1,"include_body":false,"since":"2025-01-01T00:00:00Z"}).as_object().unwrap(), &root).expect("messages");
        assert_eq!(result["count"], 1);
        assert!(result["messages"][0].get("body_text").is_none());
        assert_eq!(result["messages"][0]["subject"], "hello");
        let doctor = doctor(&root);
        assert_eq!(doctor["skipped_non_message_records"], 1);
        let accounts = accounts(&root).expect("accounts");
        assert_eq!(accounts["accounts"][0]["folders"][0], "Inbox");
        assert_eq!(accounts["accounts"][0]["latest_message_at"], "2026-01-01T00:00:00.000Z");
        let show = message_show(&json!({"message_id":"m1"}).as_object().unwrap(), &root).expect("show");
        assert_eq!(show["message"]["body_text"], "world");
        assert_eq!(show["message"]["subject"], "hello");
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn native_mailbox_outbox_authority_is_scoped_and_idempotent() {
        let root = std::env::temp_dir().join(format!("narada-mailbox-db-{}", uuid::Uuid::new_v4()));
        let db = open_domain_db_write(&root).expect("db");
        db.execute_batch(r##"
            INSERT INTO mailbox_sync_generations(generation_id,idempotency_key,request_fingerprint,scope_id,config_fingerprint,status,batch_record_count,created_at,updated_at,completed_at)
            VALUES ('g1','k1','request','scope','cfg','completed',1,'2026-01-01','2026-01-01','2026-01-01');
            INSERT INTO mailbox_outbox(event_id,scope_id,topic,aggregate_id,aggregate_revision,schema_version,causation_id,idempotency_key,partition_key,occurred_at,payload_json)
            VALUES ('e1','scope','topic','a1',1,1,'c','k','p','2026-01-01T00:00:00.000Z','{"value":1}');
        "##).expect("schema");
        drop(db);
        let registration = json!({
            "consumer_id":"c1",
            "scope_id":"scope",
            "topics":["topic"],
            "start_at":"2026-01-01T00:00:00Z"
        });
        let registered = outbox_consumer_register(registration.as_object().unwrap(), &root)
            .expect("register");
        assert_eq!(registered["consumer"]["start_at"], "2026-01-01T00:00:00.000Z");
        assert_eq!(registered["consumer"]["topics_json"], "[\"topic\"]");
        let replay = outbox_consumer_register(registration.as_object().unwrap(), &root)
            .expect("registration replay");
        assert_eq!(replay["consumer"]["consumer_id"], "c1");
        let conflict = outbox_consumer_register(
            json!({"consumer_id":"c1","scope_id":"scope","topics":["other"],"start_at":"2026-01-01T00:00:00Z"})
                .as_object()
                .unwrap(),
            &root,
        )
        .expect_err("registration conflict");
        assert_eq!(conflict["code"], "mailbox_outbox_consumer_conflict:c1");
        assert_eq!(generation_show(&json!({"generation_id":"g1"}).as_object().unwrap(), &root).expect("generation")["generation"]["status"], "completed");
        assert_eq!(outbox_consumer_show(&json!({"consumer_id":"c1"}).as_object().unwrap(), &root).expect("consumer")["status"], "ok");
        let page = outbox_list(&json!({"consumer_id":"c1","limit":1}).as_object().unwrap(), &root).expect("outbox");
        assert_eq!(page["count"], 1);
        assert_eq!(page["items"][0]["payload"]["value"], 1);
        let acknowledgement = json!({
            "consumer_id":"c1",
            "event_id":"e1",
            "receipt":{"schema":"fixture.receipt.v1","outcome":"completed","effect_ref":"effect:1"}
        });
        let first_ack = outbox_ack(acknowledgement.as_object().unwrap(), &root).expect("ack");
        assert_eq!(first_ack["replayed"], false);
        let replayed_ack = outbox_ack(acknowledgement.as_object().unwrap(), &root).expect("ack replay");
        assert_eq!(replayed_ack["replayed"], true);
        assert_eq!(outbox_list(&json!({"consumer_id":"c1"}).as_object().unwrap(), &root).expect("drained")["count"], 0);
        let ack_conflict = outbox_ack(
            json!({"consumer_id":"c1","event_id":"e1","receipt":{"schema":"fixture.receipt.v1","outcome":"failed","effect_ref":"effect:2"}})
                .as_object()
                .unwrap(),
            &root,
        )
        .expect_err("ack conflict");
        assert_eq!(ack_conflict["code"], "mailbox_outbox_ack_conflict:c1:e1");
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn native_mailbox_reconciliation_publishes_first_observation_once() {
        let root = std::env::temp_dir().join(format!("narada-mailbox-reconcile-{}", uuid::Uuid::new_v4()));
        let scope_root = root.join(".narada/runtime/mailboxes/support");
        fs::create_dir_all(scope_root.join(".narada")).expect("scope root");
        fs::create_dir_all(root.join("config")).expect("config root");
        fs::write(
            root.join("config/config.json"),
            serde_json::to_vec(&json!({
                "scopes":[{
                    "scope_id":"support",
                    "root_dir":".narada/runtime/mailboxes/support",
                    "sources":[{"type":"graph"}],
                    "graph":{"user_id":"support@example.test","prefer_immutable_ids":true},
                    "scope":{"included_container_refs":["inbox"],"included_item_kinds":["message"]},
                    "normalize":{"attachment_policy":"metadata_only","body_policy":"text_only","include_headers":false,"tombstones_enabled":true},
                    "runtime":{"polling_interval_ms":60000,"acquire_lock_timeout_ms":1000,"cleanup_tmp_on_startup":true,"rebuild_views_after_sync":false,"rebuild_search_after_sync":false},
                    "admission":{"mail":{"included_folder_refs":["inbox"],"allowed_sender_domains":["allowed.test"],"unknown_sender_behavior":"ignore"}}
                }]
            }))
            .expect("config json"),
        )
        .expect("config");
        let domain = open_domain_db_write(&root).expect("domain");
        domain.execute(
            "INSERT INTO mailbox_sync_generations(generation_id,idempotency_key,request_fingerprint,scope_id,config_fingerprint,status,batch_record_count,created_at,updated_at,completed_at) VALUES (?,?,?,?,?,'completed',1,?,?,?)",
            params!["g1","sync-key","request","support","config","2026-01-01","2026-01-01","2026-01-01"],
        ).expect("generation");
        domain.execute(
            "INSERT INTO mailbox_sync_generation_records(generation_id,record_id,fact_id,event_kind,message_id,mailbox_id,conversation_id,source_version,application_status) VALUES (?,?,?,?,?,?,?,?,?)",
            params!["g1","record-1","fact-1","upsert","message-1","support","conversation-1","v1","projected"],
        ).expect("record");
        drop(domain);
        let facts = Connection::open(scope_root.join(".narada/facts.db")).expect("facts");
        facts.execute_batch("CREATE TABLE facts(fact_id TEXT PRIMARY KEY,fact_type TEXT NOT NULL,source_id TEXT NOT NULL,source_record_id TEXT NOT NULL,source_version TEXT,source_cursor TEXT,provenance_json TEXT NOT NULL,payload_json TEXT NOT NULL,created_at TEXT NOT NULL,admitted_at TEXT);").expect("fact schema");
        let payload = json!({
            "record_id":"record-1",
            "ordinal":"2026-01-01T00:00:00.000Z",
            "event":{
                "mailbox_id":"support",
                "message_id":"message-1",
                "event_kind":"upsert",
                "payload":{
                    "mailbox_id":"support","message_id":"message-1","conversation_id":"conversation-1",
                    "internet_message_id":"<message-1@example.test>","subject":"Fixture subject",
                    "from":{"email":"sender@allowed.test"},"folder_refs":["inbox"],
                    "body":{"text":"secret body must not cross the admission receipt"}
                }
            }
        });
        facts.execute(
            "INSERT INTO facts(fact_id,fact_type,source_id,source_record_id,source_version,provenance_json,payload_json,created_at) VALUES (?,?,?,?,?,?,?,?)",
            params!["fact-1","mail.message.discovered","support","record-1","v1",r#"{"source_id":"support","source_record_id":"record-1","source_version":"v1","source_cursor":"cursor-1","observed_at":"2026-01-01T00:00:00.000Z"}"#,serde_json::to_string(&payload).unwrap(),"2026-01-01T00:00:00.000Z"],
        ).expect("fact");
        drop(facts);
        let args = json!({"idempotency_key":"reconcile-1","generation_id":"g1","scope_id":"support"});
        let first = reconcile_first_observations(args.as_object().unwrap(), &root).expect("reconcile");
        assert_eq!(first["result"]["observations_recorded"], 1);
        assert_eq!(first["result"]["events_published"], 1);
        assert_eq!(first["result"]["idempotency_replayed"], false);
        let replay = reconcile_first_observations(args.as_object().unwrap(), &root).expect("replay");
        assert_eq!(replay["result"]["idempotency_replayed"], true);
        assert_eq!(replay["result"]["events_published"], 1);
        let db = open_domain_db(&root).expect("open").expect("db");
        let count: i64 = db.query_row(
            "SELECT COUNT(*) FROM mailbox_outbox WHERE topic='mailbox.message.first_observed'",
            [],
            |row| row.get(0),
        ).expect("event count");
        assert_eq!(count, 1);
        drop(db);
        let source_event_id = stable_id("mbe_", &format!("first-observed\0support\0message-1"));
        let admission_args = json!({
            "idempotency_key":"admit-1",
            "fact_id":"fact-1",
            "source_event_id":source_event_id,
            "scope_id":"support"
        });
        let admitted = admit_message(admission_args.as_object().unwrap(), &root).expect("admit");
        assert_eq!(admitted["result"]["decision"], "admitted");
        assert_eq!(admitted["result"]["reason"], "admitted");
        assert_eq!(admitted["result"]["idempotency_replayed"], false);
        assert!(!serde_json::to_string(&admitted).unwrap().contains("secret body"));
        let replay = admit_message(
            json!({"idempotency_key":"admit-2","fact_id":"fact-1","source_event_id":source_event_id,"scope_id":"support"})
                .as_object()
                .unwrap(),
            &root,
        )
        .expect("canonical replay");
        assert_eq!(replay["result"]["idempotency_replayed"], true);
        let shown = admission_show(
            json!({"scope_id":"support","fact_id":"fact-1"}).as_object().unwrap(),
            &root,
        )
        .expect("admission show");
        assert_eq!(shown["status"], "ok");
        assert_eq!(shown["admission"]["admission_id"], admitted["result"]["admission_id"]);
        fs::remove_dir_all(root).expect("cleanup");
    }
}
