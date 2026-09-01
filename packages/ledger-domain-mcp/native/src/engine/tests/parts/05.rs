    #[test]
    fn admitted_assessments_are_queryable_in_neighborhood_status_and_export() {
        let engine = engine();
        let root = std::env::temp_dir().join(format!("epistemic-record-test-{}", Uuid::new_v4()));
        let operations = json!([
            {"op":"entity.declare","entity_id":"source:record-test","kind":"source","title":"Record test source","version":"1","locator":"ledger/test.md"},
            {"op":"entity.declare","entity_id":"test:record-test","kind":"test","title":"Record test"},
            {"op":"assessment.record","assessment_id":"assessment:record-test","subject_id":"test:record-test","judgment":"conditional","actor":"tester","reason":"Some gates remain open.","evidence":[{"source_id":"source:record-test","locator":"Current status","paraphrase":"The source reports a conditional result."}]}
        ]);
        let proposal = engine
            .proposal_submit(
                &root,
                &Map::from_iter([
                    ("actor".into(), json!("tester")),
                    ("authority_basis".into(), json!({"kind":"test"})),
                    ("idempotency_key".into(), json!("record-p1")),
                    ("expected_ledger_head".into(), Value::Null),
                    ("operations".into(), operations),
                ]),
            )
            .expect("proposal");
        engine
            .proposal_admit(
                &root,
                &Map::from_iter([
                    ("proposal_id".into(), proposal["proposal_id"].clone()),
                    ("actor".into(), json!("tester")),
                    ("authority_basis".into(), json!({"kind":"test"})),
                    ("expected_ledger_head".into(), Value::Null),
                    ("idempotency_key".into(), json!("record-a1")),
                ]),
            )
            .expect("admit");
        let records = engine
            .query(
                &root,
                &Map::from_iter([("record_kind".into(), json!("assessment.record"))]),
            )
            .expect("record query");
        assert_eq!(records["returned"], 1);
        assert_eq!(engine.status(&root).expect("status")["record_count"], 1);
        assert_eq!(
            engine
                .neighborhood(
                    &root,
                    &Map::from_iter([("entity_id".into(), json!("test:record-test"))])
                )
                .expect("neighborhood")["records"]
                .as_array()
                .map(Vec::len),
            Some(1)
        );
        assert_eq!(
            engine.export(&root, &Map::new()).expect("export")["records"]
                .as_array()
                .map(Vec::len),
            Some(1)
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn engine_written_ledger_verifies_through_the_shared_crate() {
        let engine = engine();
        let root = std::env::temp_dir().join(format!("epistemic-shared-verify-{}", Uuid::new_v4()));
        let proposal = engine
            .proposal_submit(
                &root,
                &Map::from_iter([
                    ("actor".into(), json!("tester")),
                    ("authority_basis".into(), json!({"kind":"test"})),
                    ("idempotency_key".into(), json!("shared-p1")),
                    ("expected_ledger_head".into(), Value::Null),
                    ("operations".into(), json!([
                        {"op":"entity.declare","entity_id":"claim:shared","kind":"claim","title":"Shared verify claim"}
                    ])),
                ]),
            )
            .expect("proposal");
        engine
            .proposal_admit(
                &root,
                &Map::from_iter([
                    ("proposal_id".into(), proposal["proposal_id"].clone()),
                    ("actor".into(), json!("tester")),
                    ("authority_basis".into(), json!({"kind":"test"})),
                    ("expected_ledger_head".into(), Value::Null),
                    ("idempotency_key".into(), json!("shared-a1")),
                ]),
            )
            .expect("admit");
        narada_mcp_event_ledger::ledger::verify(
            narada_mcp_event_ledger::ErrorSchema("narada.epistemic.error.v1"),
            &narada_mcp_event_ledger::ledger::LedgerLayout::new(
                root.join(".narada/epistemic/ledger"),
                "ev",
            ),
            "event_hash",
        )
        .expect("shared crate verifies the engine-written ledger");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn graph_snapshot_pages_nodes_and_edges_under_one_ledger_head() {
        let engine = engine();
        let root = std::env::temp_dir().join(format!("epistemic-snapshot-test-{}", Uuid::new_v4()));
        let proposal = engine
            .proposal_submit(
                &root,
                &Map::from_iter([
                    ("actor".into(), json!("tester")),
                    ("authority_basis".into(), json!({"kind":"test"})),
                    ("idempotency_key".into(), json!("snapshot-p1")),
                    ("expected_ledger_head".into(), Value::Null),
                    ("operations".into(), json!([
                        {"op":"entity.declare","entity_id":"problem:snapshot","kind":"problem","title":"Snapshot problem"},
                        {"op":"entity.declare","entity_id":"claim:snapshot","kind":"claim","title":"Snapshot claim"},
                        {"op":"relation.declare","relation_id":"relation:snapshot","relation_type":"addresses","source_id":"claim:snapshot","target_id":"problem:snapshot"}
                    ])),
                ]),
            )
            .expect("proposal");
        engine
            .proposal_admit(
                &root,
                &Map::from_iter([
                    ("proposal_id".into(), proposal["proposal_id"].clone()),
                    ("actor".into(), json!("tester")),
                    ("authority_basis".into(), json!({"kind":"test"})),
                    ("expected_ledger_head".into(), Value::Null),
                    ("idempotency_key".into(), json!("snapshot-a1")),
                ]),
            )
            .expect("admit");

        let first = engine
            .snapshot(&root, &Map::from_iter([("limit".into(), json!(1))]))
            .expect("first page");
        assert_eq!(first["entity_count"], 2);
        assert_eq!(first["relation_count"], 1);
        assert_eq!(first["entities"].as_array().map(Vec::len), Some(1));
        assert_eq!(first["relations"].as_array().map(Vec::len), Some(1));
        assert_eq!(first["next_entity_offset"], 1);
        assert!(first["next_relation_offset"].is_null());

        let second = engine
            .snapshot(
                &root,
                &Map::from_iter([
                    ("limit".into(), json!(1)),
                    ("entity_offset".into(), json!(1)),
                    ("relation_offset".into(), json!(1)),
                    ("expected_ledger_head".into(), first["ledger_head"].clone()),
                ]),
            )
            .expect("second page");
        assert_eq!(second["entities"].as_array().map(Vec::len), Some(1));
        assert!(second["next_entity_offset"].is_null());
        assert!(second["relations"].as_array().is_some_and(Vec::is_empty));

        let mismatch = engine
            .snapshot(
                &root,
                &Map::from_iter([("expected_ledger_head".into(), json!("sha256:stale"))]),
            )
            .expect_err("stale snapshot");
        assert_eq!(mismatch["code"], "ledger_head_mismatch");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn proposal_submission_is_compact_and_explicit_reads_are_bounded() {
        let engine = engine();
        let root =
            std::env::temp_dir().join(format!("epistemic-proposal-read-test-{}", Uuid::new_v4()));
        let operations = (0..engine.max_operations())
            .map(|index| json!({"op":"entity.declare","entity_id":format!("claim:{index}"),"kind":"claim","title":format!("Claim {index}")}))
            .collect::<Vec<_>>();
        let receipt = engine
            .proposal_submit(
                &root,
                &Map::from_iter([
                    ("actor".into(), json!("tester")),
                    ("authority_basis".into(), json!({"kind":"test"})),
                    ("idempotency_key".into(), json!("compact-p1")),
                    ("expected_ledger_head".into(), Value::Null),
                    ("operations".into(), json!(operations)),
                ]),
            )
            .expect("proposal");
        assert_eq!(receipt["operation_count"], engine.max_operations());
        assert!(receipt.get("operations").is_none());
        assert!(
            serde_json::to_vec(&receipt)
                .expect("serialize receipt")
                .len()
                < 1024
        );

        let first = engine
            .proposal_read(
                &root,
                &Map::from_iter([
                    ("proposal_id".into(), receipt["proposal_id"].clone()),
                    ("limit".into(), json!(7)),
                ]),
            )
            .expect("first page");
        assert_eq!(first["returned"], 7);
        assert_eq!(first["offset"], 0);
        assert_eq!(first["next_offset"], 7);
        assert_eq!(first["bounded"], true);

        let final_page = engine
            .proposal_read(
                &root,
                &Map::from_iter([
                    ("proposal_id".into(), receipt["proposal_id"].clone()),
                    ("offset".into(), json!(195)),
                    ("limit".into(), json!(100)),
                ]),
            )
            .expect("final page");
        assert_eq!(final_page["returned"], 5);
        assert_eq!(final_page["has_more"], false);
        assert!(final_page["next_offset"].is_null());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn proposal_admission_rebuilds_projection_and_preserves_truth_boundary() {
        let engine = engine();
        let root = std::env::temp_dir().join(format!("epistemic-test-{}", Uuid::new_v4()));
        let proposal=engine.proposal_submit(&root,&Map::from_iter([("actor".into(),json!("nima")),("authority_basis".into(),json!({"kind":"operator_request"})),("operations".into(),json!([{"op":"entity.declare","entity_id":"problem-1","kind":"problem","title":"What explains X?"}]))])).unwrap();
        assert_eq!(
            proposal["schema"],
            "narada.epistemic.proposal_submission.v1"
        );
        assert_eq!(proposal["operation_count"], 1);
        assert!(proposal.get("operations").is_none());
        let id = proposal["proposal_id"].as_str().unwrap();
        let event = engine
            .proposal_admit(
                &root,
                &Map::from_iter([
                    ("proposal_id".into(), json!(id)),
                    ("actor".into(), json!("nima")),
                    ("authority_basis".into(), json!({"kind":"operator_request"})),
                ]),
            )
            .unwrap();
        assert_eq!(event["schema"], "narada.epistemic.proposal_admission.v1");
        assert_eq!(event["status"], "admitted");
        assert_eq!(event["operation_count"], 1);
        assert!(event.get("operations").is_none());
        assert_eq!(event["ledger_head"].as_str().map(str::len), Some(64));
        assert_eq!(event["certifies_truth"], false);
        let retry = engine
            .proposal_admit(
                &root,
                &Map::from_iter([
                    ("proposal_id".into(), json!(id)),
                    ("actor".into(), json!("nima")),
                    ("authority_basis".into(), json!({"kind":"operator_request"})),
                ]),
            )
            .expect("deterministic admission retry");
        assert_eq!(retry["event_id"], event["event_id"]);
        let admitted = engine
            .proposal_read(&root, &Map::from_iter([("proposal_id".into(), json!(id))]))
            .expect("admitted proposal readback");
        assert_eq!(admitted["status"], "admitted");
        assert_eq!(admitted["lifecycle"]["event_id"], event["event_id"]);
        assert_eq!(admitted["lifecycle"]["ledger_head"], event["ledger_head"]);
        let result = engine.query(&root, &Map::new()).unwrap();
        assert_eq!(result["returned"], 1);
        let _ = fs::remove_dir_all(root);
    }

