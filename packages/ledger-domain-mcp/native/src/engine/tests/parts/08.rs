    #[test]
    #[ignore = "rewrites the golden fixture on disk; run explicitly with --ignored"]
    fn regenerate_golden_fixture() {
        let engine = engine();
        let fixture = fixture_root();
        let _ = fs::remove_dir_all(&fixture);
        let root = std::env::temp_dir().join(format!("epistemic-fixture-gen-{}", Uuid::new_v4()));
        let admit = |operations: Value,
                     proposal_key: &str,
                     admission_key: &str,
                     expected_head: Value|
         -> Value {
            let proposal = engine
                .proposal_submit(
                    &root,
                    &Map::from_iter([
                        ("actor".into(), json!("fixture")),
                        (
                            "authority_basis".into(),
                            json!({"kind":"fixture","summary":"Golden event-ledger fixture."}),
                        ),
                        ("idempotency_key".into(), json!(proposal_key)),
                        ("expected_ledger_head".into(), expected_head),
                        ("operations".into(), operations),
                    ]),
                )
                .expect("fixture proposal");
            engine
                .proposal_admit(
                    &root,
                    &Map::from_iter([
                        ("proposal_id".into(), proposal["proposal_id"].clone()),
                        ("actor".into(), json!("fixture")),
                        (
                            "authority_basis".into(),
                            json!({"kind":"fixture","summary":"Golden event-ledger fixture."}),
                        ),
                        (
                            "expected_ledger_head".into(),
                            proposal["expected_ledger_head"].clone(),
                        ),
                        ("idempotency_key".into(), json!(admission_key)),
                    ]),
                )
                .expect("fixture admission")
        };
        let first = admit(
            json!([
                {"op":"entity.declare","entity_id":"problem:fixture","kind":"problem","title":"Fixture problem"},
                {"op":"entity.declare","entity_id":"source:fixture","kind":"source","title":"Fixture source","version":"1","locator":"docs/fixture.md"}
            ]),
            "fixture-p1",
            "fixture-a1",
            Value::Null,
        );
        let second = admit(
            json!([
                {"op":"entity.declare","entity_id":"claim:fixture","kind":"claim","title":"Fixture claim"},
                {"op":"relation.declare","relation_id":"rel:fixture-addresses","relation_type":"addresses","source_id":"claim:fixture","target_id":"problem:fixture"}
            ]),
            "fixture-p2",
            "fixture-a2",
            first["ledger_head"].clone(),
        );
        let third = admit(
            json!([
                {"op":"assessment.record","assessment_id":"assessment:fixture","subject_id":"claim:fixture","judgment":"supported","actor":"fixture","reason":"Fixture assessment.","evidence":[{"source_id":"source:fixture","locator":"docs/fixture.md","paraphrase":"The fixture source supports the claim."}]}
            ]),
            "fixture-p3",
            "fixture-a3",
            second["ledger_head"].clone(),
        );
        engine
            .sequence_create(
                &root,
                &Map::from_iter([
                    ("sequence_name".into(), json!("fixture-ledger-entry")),
                    ("actor".into(), json!("fixture")),
                    ("authority_basis".into(), json!({"kind":"fixture"})),
                    ("start_at".into(), json!(40)),
                ]),
            )
            .expect("fixture sequence");
        for key in ["fixture-c1", "fixture-c2"] {
            engine
                .sequence_claim_next(
                    &root,
                    &Map::from_iter([
                        ("sequence_name".into(), json!("fixture-ledger-entry")),
                        ("actor".into(), json!("fixture")),
                        ("authority_basis".into(), json!({"kind":"fixture"})),
                        ("idempotency_key".into(), json!(key)),
                    ]),
                )
                .expect("fixture claim");
        }
        let head = engine
            .ledger_head(&root)
            .expect("fixture head")
            .expect("non-empty fixture ledger");
        let mut event_ids = Vec::new();
        let mut event_hashes = Vec::new();
        for path in engine.ledger_files(&root).expect("fixture ledger files") {
            let event = engine.read_json(&path).expect("fixture event");
            event_ids.push(event["event_id"].clone());
            event_hashes.push(event["event_hash"].clone());
        }
        let manifest = engine
            .load_sequence_manifest(&root, "fixture-ledger-entry")
            .expect("manifest");
        let claims = engine
            .verified_sequence_claims(&root, "fixture-ledger-entry", &manifest)
            .expect("claims");
        let expected = json!({
            "schema":"narada.epistemic.golden-fixture.v1",
            "ledger_head":head,
            "event_ids":event_ids,
            "event_hashes":event_hashes,
            "replay":{"proposal_id":second["proposal_id"],"idempotency_key":"fixture-a2","event_id":second["event_id"]},
            "scan":{"idempotency_key":"fixture-a3","event_id":third["event_id"]},
            "sequence":{
                "name":"fixture-ledger-entry",
                "sequence_id":manifest["sequence_id"],
                "creation_hash":manifest["creation_hash"],
                "claim_ids":claims.iter().map(|claim| claim["claim_id"].clone()).collect::<Vec<_>>(),
                "claim_hashes":claims.iter().map(|claim| claim["claim_hash"].clone()).collect::<Vec<_>>(),
                "values":claims.iter().map(|claim| claim["value"].clone()).collect::<Vec<_>>()
            }
        });
        fs::create_dir_all(&fixture).expect("fixture directory");
        for (name, directory) in [
            ("ledger", engine.ledger(&root)),
            ("proposals", engine.proposals(&root)),
            ("sequences", engine.sequences(&root)),
        ] {
            copy_directory(&directory, &fixture.join(name));
        }
        fs::write(
            fixture.join("expected.json"),
            format!("{}\n", serde_json::to_string_pretty(&expected).unwrap()),
        )
        .expect("write expected fixture metadata");
        println!(
            "digest golden vector: {}",
            engine
                .digest_value(
                    &json!({"alpha":1,"beta":"x","gamma":[1,2],"nested":{"z":true,"a":null}})
                )
                .unwrap()
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn golden_fixture_verifies_identically() {
        let engine = engine();
        let fixture = fixture_root();
        let expected = engine
            .read_json(&fixture.join("expected.json"))
            .expect("fixture metadata");
        let root =
            std::env::temp_dir().join(format!("epistemic-fixture-verify-{}", Uuid::new_v4()));
        for (name, directory) in [
            ("ledger", engine.ledger(&root)),
            ("proposals", engine.proposals(&root)),
            ("sequences", engine.sequences(&root)),
        ] {
            copy_directory(&fixture.join(name), &directory);
        }
        engine
            .verify_ledger(&root)
            .expect("fixture ledger chain verifies");
        assert_eq!(
            engine.ledger_head(&root).expect("fixture head").as_deref(),
            expected["ledger_head"].as_str()
        );
        let files = engine.ledger_files(&root).expect("fixture ledger files");
        assert_eq!(files.len(), expected["event_ids"].as_array().unwrap().len());
        for (index, path) in files.iter().enumerate() {
            let event = engine.read_json(path).expect("fixture event");
            assert_eq!(event["event_id"], expected["event_ids"][index]);
            assert_eq!(event["event_hash"], expected["event_hashes"][index]);
            assert_eq!(event["sequence"], (index + 1) as u64);
        }
        let scanned = engine
            .find_ledger_event_by_idempotency(
                &root,
                expected["scan"]["idempotency_key"].as_str().unwrap(),
            )
            .expect("idempotency scan")
            .expect("fixture event recovered by scan");
        assert_eq!(scanned["event_id"], expected["scan"]["event_id"]);
        let replay = engine
            .proposal_admit(
                &root,
                &Map::from_iter([
                    (
                        "proposal_id".into(),
                        expected["replay"]["proposal_id"].clone(),
                    ),
                    ("actor".into(), json!("fixture")),
                    ("authority_basis".into(), json!({"kind":"fixture"})),
                    (
                        "idempotency_key".into(),
                        expected["replay"]["idempotency_key"].clone(),
                    ),
                ]),
            )
            .expect("fixture admission replay");
        assert_eq!(replay["event_id"], expected["replay"]["event_id"]);
        assert_eq!(replay["ledger_head"], expected["event_hashes"][1]);
        let name = expected["sequence"]["name"].as_str().unwrap();
        let manifest = engine
            .load_sequence_manifest(&root, name)
            .expect("fixture manifest verifies");
        assert_eq!(
            manifest["creation_hash"],
            expected["sequence"]["creation_hash"]
        );
        let claims = engine
            .verified_sequence_claims(&root, name, &manifest)
            .expect("fixture claim chain verifies");
        let expected_hashes = expected["sequence"]["claim_hashes"].as_array().unwrap();
        assert_eq!(claims.len(), expected_hashes.len());
        for (claim, hash) in claims.iter().zip(expected_hashes.iter()) {
            assert_eq!(&claim["claim_hash"], hash);
        }
        let _ = fs::remove_dir_all(root);
    }
    #[test]
    fn status_reports_stale_projection_without_rebuilding_it() {
        let engine = engine();
        let root = std::env::temp_dir().join(format!("epistemic-status-stale-{}", Uuid::new_v4()));
        engine
            .rebuild_projection(&root)
            .expect("initial projection");
        let table = engine
            .projection_meta_table
            .as_ref()
            .expect("projection metadata table");
        let projection = engine.projection_path(&root);
        let db = Connection::open(&projection).expect("open projection");
        db.execute(
            &format!("update {table} set ledger_sequence = 99 where meta_id = 'current'"),
            [],
        )
        .expect("make projection metadata stale");
        drop(db);

        let status = engine.status(&root).expect("bounded status");
        assert_eq!(status["status"], "ok");
        assert_eq!(status["projection_status"], "stale");
        assert_eq!(status["projection_current"], false);
        assert_eq!(status["status_rebuilds_projection"], false);

        let db = Connection::open(&projection).expect("reopen projection");
        let stored_sequence: i64 = db
            .query_row(
                &format!("select ledger_sequence from {table} where meta_id = 'current'"),
                [],
                |row| row.get(0),
            )
            .expect("read unchanged stale metadata");
        assert_eq!(stored_sequence, 99);
        drop(db);
        let runtime = projection.parent().expect("projection parent");
        assert!(
            fs::read_dir(runtime)
                .expect("read projection runtime")
                .all(|entry| !entry
                    .expect("runtime entry")
                    .file_name()
                    .to_string_lossy()
                    .contains(".next-")),
            "status must not create a scratch projection"
        );
        let _ = fs::remove_dir_all(root);
    }
