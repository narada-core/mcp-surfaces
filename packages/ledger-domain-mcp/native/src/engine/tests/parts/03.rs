    #[test]
    fn canonical_communication_query_includes_namespaced_legacy_kind() {
        let engine = engine();
        let root = std::env::temp_dir().join(format!(
            "epistemic-communication-alias-test-{}",
            Uuid::new_v4()
        ));
        engine
            .rebuild_projection(&root)
            .expect("initial projection");
        event_ledger::append_event(
            engine.error,
            &engine.ledger_layout(&root),
            engine.event_hash_field,
            None,
            None,
            |ctx| json!({"schema":engine.domain.storage.event_schema_id,"sequence":ctx.sequence,"event_id":ctx.event_id,"previous_hash":ctx.previous_hash,"operations":[{"op":"entity.declare","entity_id":"communication:legacy","kind":"communication","title":"Legacy message","sender":"marici.Nima","recipient":"marici.Benincasa","body":"legacy body","intent":"reply","sent_at":"2026-08-20T00:00:00Z"}],"actor":"historical-fixture"}),
        ).expect("append historical legacy event");
        let proposal = engine
            .proposal_submit(
                &root,
                &Map::from_iter([
                    ("actor".into(), json!("tester")),
                    ("authority_basis".into(), json!({"kind":"test"})),
                    (
                        "idempotency_key".into(),
                        json!("communication-alias-proposal"),
                    ),
                    (
                        "operations".into(),
                        json!([
                            {
                                "op":"entity.declare",
                                "entity_id":"communication:canonical",
                                "kind":"narada.epistemic:communication",
                                "title":"Canonical message",
                                "sender":"marici.Nima",
                                "recipient":"marici.Benincasa",
                                "body":"canonical body",
                                "intent":"reply",
                                "sent_at":"2026-08-20T00:01:00Z"
                            }
                        ]),
                    ),
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
                    (
                        "idempotency_key".into(),
                        json!("communication-alias-admission"),
                    ),
                ]),
            )
            .expect("admit");

        let canonical = engine
            .generic_query(
                &root,
                &Map::from_iter([
                    ("template".into(), json!("inbox")),
                    ("recipient".into(), json!("marici.Benincasa")),
                    ("limit".into(), json!(10)),
                ]),
            )
            .expect("canonical query");
        assert_eq!(canonical["count"], 2);
        assert!(canonical["items"]
            .as_array()
            .unwrap()
            .iter()
            .all(|item| item["kind"] == "narada.epistemic:communication"));
        assert_eq!(canonical["normalization"]["applied"], true);
        assert_eq!(canonical["normalization"]["normalized_count"], 1);

        let poll = engine.call_tool(
            "epistemic_graph_communication_inbox_poll",
            &Map::from_iter([
                ("participant".into(), json!("marici.Benincasa")),
                ("phase".into(), json!("opening")),
                ("limit".into(), json!(1)),
            ]),
            &root,
        ).expect("opening poll");
        assert_eq!(poll["items"].as_array().unwrap().len(), 1);
        assert_eq!(poll["items"][0]["entity_id"], "communication:canonical");
        let checkpoint = poll["poll_contract"]["checkpoint_after_sequence"].as_u64().expect("numeric checkpoint");
        assert!(checkpoint > 0, "poll={poll}");
        assert_eq!(poll["poll_contract"]["next_poll"]["arguments"]["after_sequence"], checkpoint);

        let preflight = engine
            .communication_migration_preflight(&root, &Map::new())
            .expect("migration preflight");
        assert_eq!(preflight["census"]["by_kind"]["communication"], 1);
        assert_eq!(
            preflight["proposed_operations"].as_array().unwrap().len(),
            1
        );
        let originating_event = preflight["census"]["messages"][0]["event_id"].clone();
        let mut collision_operation = preflight["proposed_operations"][0].clone();
        collision_operation["equivalence_evidence"]["payload_sha256"] =
            json!("sha256:ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff");
        let collision_proposal = engine
            .proposal_submit(
                &root,
                &Map::from_iter([
                    ("actor".into(), json!("tester")),
                    ("authority_basis".into(), json!({"kind":"test"})),
                    ("idempotency_key".into(), json!("communication-collision")),
                    ("operations".into(), json!([collision_operation])),
                ]),
            )
            .expect("collision proposal may be staged for authoritative review");
        let collision = engine
            .proposal_admit(
                &root,
                &Map::from_iter([
                    (
                        "proposal_id".into(),
                        collision_proposal["proposal_id"].clone(),
                    ),
                    ("actor".into(), json!("tester")),
                    ("authority_basis".into(), json!({"kind":"test"})),
                    (
                        "idempotency_key".into(),
                        json!("communication-collision-admission"),
                    ),
                ]),
            )
            .expect_err("mismatched canonicalization evidence must stop at admission");
        assert_eq!(
            collision["code"],
            "communication_kind_canonicalization_collision"
        );
        let migrated = engine.communication_migrate(&root, &Map::from_iter([
            ("actor".into(), json!("operator")),
            ("authority_basis".into(), json!({"kind":"operator_direct_instruction","summary":"Canonical communication migration test."})),
        ])).expect("migration");
        assert_eq!(migrated["migrated"], 1);
        let replay = engine.communication_migrate(&root, &Map::from_iter([
            ("actor".into(), json!("operator")),
            ("authority_basis".into(), json!({"kind":"operator_direct_instruction","summary":"Canonical communication migration test."})),
        ])).expect("idempotent migration replay");
        assert_eq!(replay["migrated"], 0);
        assert_eq!(replay["status"], "complete");
        let db = Connection::open(engine.projection_path(&root)).expect("projection");
        let effective: (String, String) = db
            .query_row(
                "select kind,event_id from entities where entity_id='communication:legacy'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .expect("legacy entity");
        assert_eq!(effective.0, "narada.epistemic:communication");
        assert_eq!(json!(effective.1), originating_event);
        let after = engine
            .generic_query(
                &root,
                &Map::from_iter([
                    ("template".into(), json!("inbox")),
                    ("recipient".into(), json!("marici.Benincasa")),
                    ("limit".into(), json!(10)),
                ]),
            )
            .expect("post-migration query");
        assert_eq!(after["count"], 2);
        assert_eq!(after["normalization"]["applied"], false);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn named_query_filter_types_are_refused_instead_of_defaulted() {
        let engine = engine();
        for arguments in [
            json!({"template":"inbox","participant":"marici.Benincasa","include_body":"false"}),
            json!({"template":"inbox","participant":"marici.Benincasa","direction":false}),
            json!({"template":"inbox","participant":"marici.Benincasa","match":[]}),
            json!({"template":"inbox","participant":"marici.Benincasa","expected_ledger_head":true}),
        ] {
            let failure = engine
                .named_query(arguments.as_object().expect("query arguments"))
                .expect_err("malformed named filter must refuse");
            assert_eq!(failure["code"], "query_filter_type_invalid");
        }
    }

    #[test]
    fn named_and_legacy_kind_aliases_share_the_one_of_budget() {
        let mut engine = engine();
        engine.domain.query.max_one_of_values = Some(2);
        engine.domain.query.kind_aliases.insert(
            "communication".to_string(),
            vec![
                "marici:communication".to_string(),
                "communication.v2".to_string(),
            ],
        );

        let legacy = engine
            .expand_legacy_kind_value("communication")
            .expect_err("legacy aliases must be bounded");
        assert_eq!(legacy["code"], "query_kind_limit");

        let named = engine
            .named_query(
                json!({
                    "template":"inbox",
                    "participant":"marici.Benincasa",
                    "kinds":["communication"]
                })
                .as_object()
                .expect("named query arguments"),
            )
            .expect_err("named aliases must be bounded");
        assert_eq!(named["code"], "query_kind_limit");
    }

    #[test]
    fn source_inspection_returns_all_relevant_sections_with_line_ranges() {
        let engine = engine();
        let root = std::env::temp_dir().join(format!("epistemic-source-test-{}", Uuid::new_v4()));
        fs::create_dir_all(root.join("ledger")).expect("ledger directory");
        fs::write(
            root.join("ledger/example.md"),
            "# Example\n\n## Record\nA\n\n## Decision\nB\n\n## Subsequent Update\nC\n",
        )
        .expect("source");
        let result = engine
            .source_inspect(
                &root,
                &Map::from_iter([("paths".into(), json!(["ledger/example.md"]))]),
            )
            .expect("inspection");
        assert_eq!(result["files"][0]["title"], "Example");
        assert_eq!(result["files"][0]["section_count"], 3);
        assert_eq!(result["files"][0]["sections"][1]["start_line"], 6);
        let _ = fs::remove_dir_all(root);
    }

