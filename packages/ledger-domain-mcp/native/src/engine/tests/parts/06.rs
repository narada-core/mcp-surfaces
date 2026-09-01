    #[test]
    fn source_capture_builds_a_compact_deduplicated_draft_without_admitting_it() {
        let engine = engine();
        let root = std::env::temp_dir().join(format!("epistemic-capture-test-{}", Uuid::new_v4()));
        let seed = engine.proposal_submit(
            &root,
            &Map::from_iter([
                ("actor".into(), json!("tester")),
                ("authority_basis".into(), json!({"kind":"test"})),
                ("idempotency_key".into(), json!("seed-p1")),
                ("expected_ledger_head".into(), Value::Null),
                ("operations".into(), json!([{"op":"entity.declare","entity_id":"claim:existing","kind":"claim","title":"Existing claim"}])),
            ]),
        ).expect("seed proposal");
        let seed_event = engine
            .proposal_admit(
                &root,
                &Map::from_iter([
                    ("proposal_id".into(), seed["proposal_id"].clone()),
                    ("actor".into(), json!("tester")),
                    ("authority_basis".into(), json!({"kind":"test"})),
                    ("expected_ledger_head".into(), Value::Null),
                    ("idempotency_key".into(), json!("seed-a1")),
                ]),
            )
            .expect("seed admission");
        let capture = engine.capture_sources(
            &root,
            &Map::from_iter([
                ("actor".into(), json!("tester")),
                ("authority_basis".into(), json!({"kind":"test"})),
                ("idempotency_key".into(), json!("capture-p1")),
                ("expected_ledger_head".into(), seed_event["ledger_head"].clone()),
                ("sources".into(), json!([{"source_id":"source:ledger-1","title":"Ledger one","version":"1","locator":"src/ledger/1.md"}])),
                ("operations".into(), json!([
                    {"op":"entity.declare","entity_id":"claim:existing","kind":"claim","title":"Existing claim"},
                    {"op":"relation.declare","relation_id":"rel:existing-source","relation_type":"derived_from","source_id":"claim:existing","target_id":"source:ledger-1"}
                ])),
            ]),
        ).expect("source capture");
        assert_eq!(capture["status"], "draft_submitted");
        assert_eq!(capture["source_count"], 1);
        assert_eq!(capture["operation_count"], 3);
        assert_eq!(capture["existing_identity_count"], 1);
        assert_eq!(
            capture["existing_identities"][0]["identity"],
            "claim:existing"
        );
        assert_eq!(capture["admission_requires_explicit_call"], true);
        assert_eq!(engine.ledger_files(&root).expect("ledger").len(), 1);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn claim_entities_and_compact_queries_preserve_epistemic_attribution() {
        let engine = engine();
        let root = std::env::temp_dir().join(format!("epistemic-claim-test-{}", Uuid::new_v4()));
        let proposal = engine.proposal_submit(
            &root,
            &Map::from_iter([
                ("actor".into(), json!("tester")),
                ("authority_basis".into(), json!({"kind":"test"})),
                ("idempotency_key".into(), json!("claim-p1")),
                ("expected_ledger_head".into(), Value::Null),
                ("operations".into(), json!([{"op":"entity.declare","entity_id":"claim:tree-result","kind":"claim","title":"Attributed theorem result"}])),
            ]),
        ).expect("claim proposal");
        engine
            .proposal_admit(
                &root,
                &Map::from_iter([
                    ("proposal_id".into(), proposal["proposal_id"].clone()),
                    ("actor".into(), json!("tester")),
                    ("authority_basis".into(), json!({"kind":"test"})),
                    ("expected_ledger_head".into(), Value::Null),
                    ("idempotency_key".into(), json!("claim-a1")),
                ]),
            )
            .expect("claim admission");
        let result = engine
            .query(&root, &Map::from_iter([("compact".into(), json!(true))]))
            .expect("compact query");
        assert_eq!(result["compact"], true);
        assert_eq!(result["items"][0]["entity_id"], "claim:tree-result");
        assert_eq!(result["items"][0]["title"], "Attributed theorem result");
        assert!(result["items"][0].get("payload").is_none());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn projection_refuses_a_tampered_authority_event() {
        let engine = engine();
        let root = std::env::temp_dir().join(format!("epistemic-test-{}", Uuid::new_v4()));
        let proposal = engine
            .proposal_submit(
                &root,
                &Map::from_iter([
                    ("actor".into(), json!("nima")),
                    ("authority_basis".into(), json!({"kind":"test"})),
                    ("idempotency_key".into(), json!("p1")),
                    ("expected_ledger_head".into(), Value::Null),
                    ("operations".into(), json!([{"op":"entity.declare","entity_id":"problem-1","kind":"problem","title":"What explains X?"}])),
                ]),
            )
            .unwrap();
        let id = proposal["proposal_id"].as_str().unwrap();
        engine
            .proposal_admit(
                &root,
                &Map::from_iter([
                    ("proposal_id".into(), json!(id)),
                    ("actor".into(), json!("nima")),
                    ("authority_basis".into(), json!({"kind":"test"})),
                    ("expected_ledger_head".into(), Value::Null),
                    ("idempotency_key".into(), json!("a1")),
                ]),
            )
            .unwrap();
        let path = engine.ledger_files(&root).unwrap().remove(0);
        let mut event = engine.read_json(&path).unwrap();
        event["actor"] = json!("tampered");
        fs::write(&path, serde_json::to_vec_pretty(&event).unwrap()).unwrap();
        let failure = engine.rebuild_projection(&root).unwrap_err();
        assert_eq!(failure["code"], "ledger_hash_invalid");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn pure_source_capture_needs_no_placeholder_operation() {
        let engine = engine();
        let root = std::env::temp_dir().join(format!("epistemic-source-only-{}", Uuid::new_v4()));
        let result = engine
            .capture_sources(
                &root,
                &Map::from_iter([
                    ("actor".into(), json!("tester")),
                    ("authority_basis".into(), json!({"kind":"test"})),
                    ("sources".into(), json!([{"source_id":"source:only","title":"Only source","version":"1","locator":"ledger/only.md"}])),
                ]),
            )
            .expect("source-only capture");
        assert_eq!(result["source_count"], 1);
        assert_eq!(result["operation_count"], 1);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn compound_workflow_derives_relation_and_retry_identities() {
        let engine = engine();
        let root = std::env::temp_dir().join(format!("epistemic-compound-{}", Uuid::new_v4()));
        let args = Map::from_iter([
            ("actor".into(), json!("tester")),
            ("authority_basis".into(), json!({"kind":"test"})),
            (
                "operations".into(),
                json!([
                    {"op":"entity.declare","local_ref":"claim","kind":"claim","title":"A"},
                    {"op":"entity.declare","local_ref":"source","kind":"source","title":"Source A","version":"1","locator":"ledger/a.md"},
                    {"op":"relation.declare","relation_type":"derived_from","source_ref":"claim","target_ref":"source"}
                ]),
            ),
        ]);
        let first = engine
            .submit_review_admit(&root, &args)
            .expect("compound admission");
        assert_eq!(first["review"]["status"], "policy_valid");
        assert_eq!(first["admission"]["status"], "admitted");
        let proposal = engine
            .load_proposal(&root, first["submission"]["proposal_id"].as_str().unwrap())
            .unwrap();
        assert!(proposal["operations"][0]["entity_id"]
            .as_str()
            .unwrap()
            .starts_with("claim:"));
        assert!(proposal["operations"][1]["entity_id"]
            .as_str()
            .unwrap()
            .starts_with("source:"));
        assert_eq!(
            proposal["operations"][2]["source_id"],
            proposal["operations"][0]["entity_id"]
        );
        assert_eq!(
            proposal["operations"][2]["target_id"],
            proposal["operations"][1]["entity_id"]
        );
        assert!(proposal["operations"][2]["relation_id"]
            .as_str()
            .unwrap()
            .starts_with("rel:derived_from-"));
        let retried = engine
            .submit_review_admit(&root, &args)
            .expect("idempotent compound retry");
        assert_eq!(
            retried["submission"]["proposal_id"],
            first["submission"]["proposal_id"]
        );
        assert_eq!(
            retried["admission"]["event_id"],
            first["admission"]["event_id"]
        );
        let _ = fs::remove_dir_all(root);
    }

    fn sequence_test_create(engine: &Engine, root: &Path, name: &str, start_at: u64) -> Value {
        engine
            .sequence_create(
                root,
                &Map::from_iter([
                    ("sequence_name".into(), json!(name)),
                    ("actor".into(), json!("tester")),
                    ("authority_basis".into(), json!({"kind":"test"})),
                    ("start_at".into(), json!(start_at)),
                ]),
            )
            .expect("create sequence")
    }

    fn sequence_test_claim(
        engine: &Engine,
        root: &Path,
        name: &str,
        key: &str,
    ) -> Result<Value, Value> {
        engine.sequence_claim_next(
            root,
            &Map::from_iter([
                ("sequence_name".into(), json!(name)),
                ("actor".into(), json!("tester")),
                ("authority_basis".into(), json!({"kind":"test"})),
                ("idempotency_key".into(), json!(key)),
            ]),
        )
    }

    #[test]
    fn sequences_create_claim_replay_and_page_immutable_history() {
        let engine = engine();
        let root = std::env::temp_dir().join(format!("epistemic-sequence-{}", Uuid::new_v4()));
        let created = sequence_test_create(&engine, &root, "ledger-entry", 40);
        assert_eq!(created["status"], "created");
        assert_eq!(created["next_value"], 40);
        let first =
            sequence_test_claim(&engine, &root, "ledger-entry", "entry-a").expect("first claim");
        let second =
            sequence_test_claim(&engine, &root, "ledger-entry", "entry-b").expect("second claim");
        let replay =
            sequence_test_claim(&engine, &root, "ledger-entry", "entry-a").expect("claim replay");
        assert_eq!(first["value"], 40);
        assert_eq!(second["value"], 41);
        assert_eq!(replay["value"], 40);
        assert_eq!(replay["idempotency_replay"], true);
        let status = engine
            .sequence_status(
                &root,
                &Map::from_iter([("sequence_name".into(), json!("ledger-entry"))]),
            )
            .expect("status");
        assert_eq!(status["claim_count"], 2);
        assert_eq!(status["next_value"], 42);
        let page = engine
            .sequence_claims(
                &root,
                &Map::from_iter([
                    ("sequence_name".into(), json!("ledger-entry")),
                    ("limit".into(), json!(1)),
                ]),
            )
            .expect("claims page");
        assert_eq!(page["count"], 1);
        assert_eq!(page["has_more"], true);
        let listed = engine
            .sequence_list(&root, &Map::new())
            .expect("sequence list");
        assert_eq!(listed["items"][0]["sequence_name"], "ledger-entry");
        let _ = fs::remove_dir_all(root);
    }

