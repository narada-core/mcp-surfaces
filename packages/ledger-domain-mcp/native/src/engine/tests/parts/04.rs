    #[test]
    fn batch_query_and_resubmission_are_bounded_and_identity_driven() {
        let engine = engine();
        let root = std::env::temp_dir().join(format!("epistemic-batch-test-{}", Uuid::new_v4()));
        let proposal = engine
            .proposal_submit(
                &root,
                &Map::from_iter([
                    ("actor".into(), json!("tester")),
                    ("authority_basis".into(), json!({"kind":"test"})),
                    ("idempotency_key".into(), json!("batch-p1")),
                    ("expected_ledger_head".into(), Value::Null),
                    ("operations".into(), json!([
                        {"op":"entity.declare","entity_id":"claim:keep","kind":"claim","title":"Keep alpha"},
                        {"op":"entity.declare","entity_id":"claim:drop","kind":"claim","title":"Drop beta"}
                    ])),
                ]),
            )
            .expect("proposal");
        let resubmitted = engine
            .proposal_resubmit(
                &root,
                &Map::from_iter([
                    ("source_proposal_id".into(), proposal["proposal_id"].clone()),
                    ("actor".into(), json!("tester")),
                    ("authority_basis".into(), json!({"kind":"test"})),
                    ("idempotency_key".into(), json!("batch-p2")),
                    ("expected_ledger_head".into(), Value::Null),
                    ("drop_operation_ids".into(), json!(["entity:claim:drop"])),
                    ("replacements".into(), json!([
                        {"op":"entity.declare","entity_id":"claim:replacement","kind":"claim","title":"Replacement beta"}
                    ])),
                ]),
            )
            .expect("resubmit");
        assert_eq!(resubmitted["operation_count"], 2);
        let page = engine
            .proposal_read(
                &root,
                &Map::from_iter([("proposal_id".into(), resubmitted["proposal_id"].clone())]),
            )
            .expect("read resubmission");
        assert_eq!(page["operations"][0]["entity_id"], "claim:keep");
        assert_eq!(page["operations"][1]["entity_id"], "claim:replacement");

        engine
            .proposal_admit(
                &root,
                &Map::from_iter([
                    ("proposal_id".into(), resubmitted["proposal_id"].clone()),
                    ("actor".into(), json!("tester")),
                    ("authority_basis".into(), json!({"kind":"test"})),
                    ("expected_ledger_head".into(), Value::Null),
                    ("idempotency_key".into(), json!("batch-a1")),
                ]),
            )
            .expect("admit");
        let result = engine
            .query_batch(
                &root,
                &Map::from_iter([
                    (
                        "queries".into(),
                        json!([{"text":"alpha"},{"text":"replacement"}]),
                    ),
                    ("limit_per_query".into(), json!(1)),
                ]),
            )
            .expect("batch query");
        assert_eq!(result["query_count"], 2);
        assert_eq!(result["results"][0]["returned"], 1);
        assert_eq!(result["results"][1]["returned"], 1);

        let hydrated = engine
            .query_batch(
                &root,
                &Map::from_iter([
                    (
                        "queries".into(),
                        json!([{
                            "query":{
                                "find":[{"pull":{"var":"?claim","fields":["entity_id","payload"]}}],
                                "where":[{"triple":{"subject":"?claim","attribute":"narada.ledger:entity/kind","object":"claim"}}],
                                "order_by":[{"term":"?claim"}],
                                "limit":1
                            }
                        }]),
                    ),
                    ("limit_per_query".into(), json!(1)),
                ]),
            )
            .expect("hydrated batch query");
        assert_eq!(hydrated["results"][0]["mode"], "datalog");
        assert_eq!(hydrated["results"][0]["query_origin"], "raw");
        assert_eq!(hydrated["results"][0]["returned"], 1);
        assert_eq!(
            hydrated["results"][0]["items"][0]["payload"]["title"],
            "Keep alpha"
        );
        assert!(hydrated["results"][0].get("result").is_none());
        assert_eq!(
            hydrated["results"][0]["result_schema"],
            "narada.epistemic.query.v2"
        );
        assert_eq!(
            hydrated["output_bytes"],
            serde_json::to_vec(&hydrated)
                .expect("batch response serializes")
                .len() as u64
        );
        let head_conflict = engine
            .query_batch(
                &root,
                &Map::from_iter([
                    ("expected_ledger_head".into(), json!("sha256:batch")),
                    (
                        "queries".into(),
                        json!([{
                            "expected_ledger_head":"sha256:item",
                            "text":"alpha"
                        }]),
                    ),
                ]),
            )
            .expect_err("batch head pin must not be overridden by an item");
        assert_eq!(head_conflict["code"], "query_expected_head_conflict");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn generic_pull_hydrates_entity_relation_and_record_bindings() {
        let engine = engine();
        let root =
            std::env::temp_dir().join(format!("epistemic-generic-pull-test-{}", Uuid::new_v4()));
        let proposal = engine
            .proposal_submit(
                &root,
                &Map::from_iter([
                    ("actor".into(), json!("tester")),
                    ("authority_basis".into(), json!({"kind":"test"})),
                    ("idempotency_key".into(), json!("generic-pull-p1")),
                    ("expected_ledger_head".into(), Value::Null),
                    ("operations".into(), json!([
                        {"op":"entity.declare","entity_id":"claim:pull","kind":"claim","title":"Pull claim"},
                        {"op":"entity.declare","entity_id":"test:pull","kind":"test","title":"Pull test"},
                        {"op":"entity.declare","entity_id":"shared:pull","kind":"claim","title":"Shared pull identity"},
                        {"op":"relation.declare","relation_id":"relation:pull","relation_type":"tests","source_id":"test:pull","target_id":"claim:pull"},
                        {"op":"relation.declare","relation_id":"shared:pull","relation_type":"tests","source_id":"test:pull","target_id":"claim:pull"},
                        {"op":"assessment.record","assessment_id":"assessment:pull","subject_id":"test:pull","judgment":"conditional","actor":"tester","reason":"Pull record","evidence":[{"source_id":"claim:pull","locator":"Current status","paraphrase":"The claim is conditional."}]}
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
                    ("idempotency_key".into(), json!("generic-pull-a1")),
                ]),
            )
            .expect("admit");

        let relation = engine
            .generic_query(
                &root,
                &Map::from_iter([(
                    "query".into(),
                    json!({
                        "find":[{"pull":{"var":"?relation","fields":["*"]}}],
                        "where":[{"triple":{"subject":"?relation","attribute":"narada.ledger:relation/id","object":"relation:pull"}}],
                        "order_by":[{"term":"?relation"}],
                        "limit":10
                    }),
                )]),
            )
            .expect("relation pull");
        assert_eq!(relation["count"], 1);
        assert_eq!(relation["items"][0]["relation_id"], "relation:pull");
        assert_eq!(relation["items"][0]["relation_type"], "tests");
        assert_eq!(relation["items"][0]["source_id"], "test:pull");
        assert!(relation["items"][0].get("*").is_none());

        let record = engine
            .generic_query(
                &root,
                &Map::from_iter([(
                    "query".into(),
                    json!({
                        "find":[{"pull":{"var":"?record","fields":["record_id","record_kind","payload"]}}],
                        "where":[{"triple":{"subject":"?record","attribute":"narada.ledger:record/id","object":"?record"}}],
                        "order_by":[{"term":"?record"}],
                        "limit":10
                    }),
                )]),
            )
            .expect("record pull");
        assert_eq!(record["count"], 1);
        assert_eq!(record["items"][0]["record_id"], "assessment:pull");
        assert_eq!(record["items"][0]["record_kind"], "assessment.record");
        assert_eq!(record["items"][0]["payload"]["judgment"], "conditional");

        let ambiguous = engine
            .generic_query(
                &root,
                &Map::from_iter([(
                    "query".into(),
                    json!({
                        "find":[{"pull":{"var":"?object","fields":["*"]}}],
                        "inputs":{"object":"shared:pull"},
                        "where":[{"triple":{"subject":{"input":"object"},"attribute":"narada.ledger:event/id","object":"?event"}}],
                        "order_by":[{"term":"?event"}],
                        "limit":10
                    }),
                )]),
            )
            .expect_err("untyped colliding pull identity must refuse");
        assert_eq!(ambiguous["code"], "query_pull_target_ambiguous");

        let typed_entity = engine
            .generic_query(
                &root,
                &Map::from_iter([(
                    "query".into(),
                    json!({
                        "find":[{"pull":{"var":"?object","target_kind":"entity","fields":["*"]}}],
                        "inputs":{"object":"shared:pull"},
                        "where":[{"triple":{"subject":{"input":"object"},"attribute":"narada.ledger:event/id","object":"?event"}}],
                        "order_by":[{"term":"?event"}],
                        "limit":10
                    }),
                )]),
            )
            .expect("typed entity pull");
        assert_eq!(typed_entity["items"][0]["entity_id"], "shared:pull");

        let typed_relation = engine
            .generic_query(
                &root,
                &Map::from_iter([(
                    "query".into(),
                    json!({
                        "find":[{"pull":{"var":"?object","target_kind":"relation","fields":["*"]}}],
                        "inputs":{"object":"shared:pull"},
                        "where":[{"triple":{"subject":{"input":"object"},"attribute":"narada.ledger:event/id","object":"?event"}}],
                        "order_by":[{"term":"?event"}],
                        "limit":10
                    }),
                )]),
            )
            .expect("typed relation pull");
        assert_eq!(typed_relation["items"][0]["relation_id"], "shared:pull");
        let _ = fs::remove_dir_all(root);
    }

