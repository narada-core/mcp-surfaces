    #[test]
    fn sequence_claim_idempotency_is_recovered_from_canonical_history() {
        let engine = engine();
        let root =
            std::env::temp_dir().join(format!("epistemic-sequence-recovery-{}", Uuid::new_v4()));
        sequence_test_create(&engine, &root, "research-item", 1);
        let first =
            sequence_test_claim(&engine, &root, "research-item", "research-a").expect("claim");
        fs::remove_file(
            engine
                .sequence_directory(&root, "research-item")
                .join("idempotency")
                .join(format!("{}.json", sha256(b"research-a"))),
        )
        .expect("remove disposable index");
        let replay = sequence_test_claim(&engine, &root, "research-item", "research-a")
            .expect("recover replay");
        assert_eq!(replay["claim_id"], first["claim_id"]);
        assert_eq!(replay["idempotency_replay"], true);
        assert!(engine
            .sequence_directory(&root, "research-item")
            .join("idempotency")
            .join(format!("{}.json", sha256(b"research-a")))
            .exists());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn concurrent_sequence_claims_are_unique_and_contiguous() {
        let engine = std::sync::Arc::new(engine());
        let root =
            std::env::temp_dir().join(format!("epistemic-sequence-concurrent-{}", Uuid::new_v4()));
        sequence_test_create(&engine, &root, "parallel", 1);
        let root = std::sync::Arc::new(root);
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(12));
        let handles = (0..12)
            .map(|index| {
                let engine = engine.clone();
                let root = root.clone();
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    barrier.wait();
                    sequence_test_claim(&engine, &root, "parallel", &format!("parallel-{index}"))
                        .expect("parallel claim")["value"]
                        .as_u64()
                        .unwrap()
                })
            })
            .collect::<Vec<_>>();
        let mut values = handles
            .into_iter()
            .map(|handle| handle.join().unwrap())
            .collect::<Vec<_>>();
        values.sort_unstable();
        assert_eq!(values, (1..=12).collect::<Vec<_>>());
        assert_eq!(
            engine
                .sequence_status(
                    &root,
                    &Map::from_iter([("sequence_name".into(), json!("parallel"))])
                )
                .unwrap()["integrity_status"],
            "valid"
        );
        let _ = fs::remove_dir_all(root.as_path());
    }

    #[test]
    fn sequence_refuses_reconfiguration_conflicting_replay_and_tampering() {
        let engine = engine();
        let root =
            std::env::temp_dir().join(format!("epistemic-sequence-invalid-{}", Uuid::new_v4()));
        sequence_test_create(&engine, &root, "audit", 5);
        let conflict = engine
            .sequence_create(
                &root,
                &Map::from_iter([
                    ("sequence_name".into(), json!("audit")),
                    ("actor".into(), json!("tester")),
                    ("authority_basis".into(), json!({"kind":"test"})),
                    ("start_at".into(), json!(6)),
                ]),
            )
            .expect_err("configuration conflict");
        assert_eq!(conflict["code"], "sequence_configuration_conflict");
        sequence_test_claim(&engine, &root, "audit", "same-key").expect("claim");
        let replay_conflict = engine
            .sequence_claim_next(
                &root,
                &Map::from_iter([
                    ("sequence_name".into(), json!("audit")),
                    ("actor".into(), json!("other")),
                    ("authority_basis".into(), json!({"kind":"test"})),
                    ("idempotency_key".into(), json!("same-key")),
                ]),
            )
            .expect_err("replay conflict");
        assert_eq!(
            replay_conflict["code"],
            "sequence_claim_idempotency_conflict"
        );
        let claim_path = engine
            .sequence_claims_directory(&root, "audit")
            .join("claim-00000000000000000005.json");
        let mut claim = engine.read_json(&claim_path).unwrap();
        claim["actor"] = json!("tampered");
        fs::write(&claim_path, serde_json::to_vec_pretty(&claim).unwrap()).unwrap();
        let corrupt = engine
            .sequence_status(
                &root,
                &Map::from_iter([("sequence_name".into(), json!("audit"))]),
            )
            .expect_err("tampered claim");
        assert_eq!(corrupt["code"], "sequence_claim_chain_invalid");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn sequence_refuses_invalid_names_and_reports_exhaustion() {
        let engine = engine();
        let root =
            std::env::temp_dir().join(format!("epistemic-sequence-exhausted-{}", Uuid::new_v4()));
        let invalid = engine
            .sequence_create(
                &root,
                &Map::from_iter([
                    ("sequence_name".into(), json!(" bad ")),
                    ("actor".into(), json!("tester")),
                    ("authority_basis".into(), json!({"kind":"test"})),
                ]),
            )
            .expect_err("invalid name");
        assert_eq!(invalid["code"], "sequence_name_invalid");
        sequence_test_create(&engine, &root, "finite", u64::MAX);
        let final_claim =
            sequence_test_claim(&engine, &root, "finite", "last").expect("last claim");
        assert_eq!(final_claim["value"], u64::MAX);
        assert_eq!(final_claim["exhausted"], true);
        let exhausted = sequence_test_claim(&engine, &root, "finite", "past-end")
            .expect_err("sequence exhausted");
        assert_eq!(exhausted["code"], "sequence_exhausted");
        let status = engine
            .sequence_status(
                &root,
                &Map::from_iter([("sequence_name".into(), json!("finite"))]),
            )
            .expect("exhausted status");
        assert_eq!(status["next_value"], Value::Null);
        assert_eq!(status["exhausted"], true);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn ledger_admission_lock_serializes_writers_and_recovers_idempotency_index() {
        let engine = std::sync::Arc::new(engine());
        let root = std::env::temp_dir().join(format!("epistemic-ledger-lock-{}", Uuid::new_v4()));
        let proposals = (0..2)
            .map(|index| engine.proposal_submit(&root, &Map::from_iter([("actor".into(), json!("tester")), ("authority_basis".into(), json!({"kind":"test"})), ("idempotency_key".into(), json!(format!("proposal-{index}"))), ("expected_ledger_head".into(), Value::Null), ("operations".into(), json!([{"op":"entity.declare","entity_id":format!("claim:lock-{index}"),"kind":"claim","title":format!("Lock {index}")}]))])).expect("proposal"))
            .collect::<Vec<_>>();
        let root = std::sync::Arc::new(root);
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
        let handles = proposals
            .into_iter()
            .enumerate()
            .map(|(index, proposal)| {
                let engine = engine.clone();
                let root = root.clone();
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    barrier.wait();
                    engine.proposal_admit(
                        &root,
                        &Map::from_iter([
                            ("proposal_id".into(), proposal["proposal_id"].clone()),
                            ("actor".into(), json!("tester")),
                            ("authority_basis".into(), json!({"kind":"test"})),
                            ("expected_ledger_head".into(), Value::Null),
                            (
                                "idempotency_key".into(),
                                json!(format!("admission-{index}")),
                            ),
                        ]),
                    )
                })
            })
            .collect::<Vec<_>>();
        let results = handles
            .into_iter()
            .map(|handle| handle.join().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
        assert_eq!(
            results
                .iter()
                .filter(|result| {
                    result.as_ref().is_err_and(|failure| {
                        failure["code"] == "ledger_head_conflict"
                            || failure["code"] == "proposal_not_admissible"
                    })
                })
                .count(),
            1
        );
        engine.verify_ledger(&root).expect("serialized ledger");
        assert_eq!(engine.ledger_files(&root).unwrap().len(), 1);
        let admitted = results.into_iter().find_map(Result::ok).unwrap();
        let event = engine
            .read_json(
                &engine
                    .ledger(&root)
                    .join(format!("{}.json", admitted["event_id"].as_str().unwrap())),
            )
            .unwrap();
        let key = event["idempotency_key"].as_str().unwrap();
        fs::remove_file(
            engine
                .ledger(&root)
                .join(format!("idem-{}.txt", safe_name(key))),
        )
        .expect("remove disposable ledger index");
        let replay = engine
            .proposal_admit(
                &root,
                &Map::from_iter([
                    ("proposal_id".into(), event["proposal_id"].clone()),
                    ("actor".into(), json!("tester")),
                    ("authority_basis".into(), json!({"kind":"test"})),
                    ("idempotency_key".into(), json!(key)),
                ]),
            )
            .expect("recover ledger replay");
        assert_eq!(replay["event_id"], admitted["event_id"]);
        assert_eq!(engine.ledger_files(&root).unwrap().len(), 1);
        let _ = fs::remove_dir_all(root.as_path());
    }

    fn fixture_root() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/epistemic-ledger")
    }

    fn copy_directory(source: &Path, target: &Path) {
        fs::create_dir_all(target).expect("create copy target");
        for entry in fs::read_dir(source).expect("read copy source") {
            let entry = entry.expect("copy entry");
            let destination = target.join(entry.file_name());
            if entry.file_type().expect("copy entry type").is_dir() {
                copy_directory(&entry.path(), &destination);
            } else {
                fs::copy(entry.path(), &destination).expect("copy file");
            }
        }
    }

