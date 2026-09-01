    #[test]
    fn query_incrementally_catches_up_multiple_missing_events() {
        let engine = engine();
        let root =
            std::env::temp_dir().join(format!("epistemic-query-catch-up-{}", Uuid::new_v4()));
        engine
            .rebuild_projection(&root)
            .expect("initial projection");

        for (entity_id, title) in [
            ("claim:increment-one", "Incremental precursor"),
            ("claim:increment-target", "Exact incremental target"),
        ] {
            event_ledger::append_event(
                engine.error,
                &engine.ledger_layout(&root),
                engine.event_hash_field,
                None,
                None,
                |ctx| {
                    json!({
                        "schema":engine.domain.storage.event_schema_id,
                        "sequence":ctx.sequence,
                        "event_id":ctx.event_id,
                        "previous_hash":ctx.previous_hash,
                        "operations":[{
                            "op":"entity.declare",
                            "entity_id":entity_id,
                            "kind":"claim",
                            "title":title
                        }],
                        "actor":"incremental-test"
                    })
                },
            )
            .expect("append canonical event without projection refresh");
        }

        let stale = engine.status(&root).expect("stale status");
        assert_eq!(stale["projection_status"], "stale");
        assert_eq!(stale["event_count"], 2);

        let result = engine
            .query(
                &root,
                &Map::from_iter([
                    ("kind".into(), json!("claim")),
                    ("text".into(), json!("Exact incremental target")),
                    ("limit".into(), json!(10)),
                ]),
            )
            .expect("incremental exact-title query");
        assert_eq!(result["returned"], 1);
        assert_eq!(result["items"][0]["entity_id"], "claim:increment-target");

        let current = engine.status(&root).expect("current status");
        assert_eq!(current["projection_status"], "current");
        assert_eq!(current["projection_current"], true);
        let runtime = engine.projection_path(&root);
        let runtime = runtime.parent().expect("projection parent");
        assert!(
            fs::read_dir(runtime)
                .expect("read projection runtime")
                .all(|entry| !entry
                    .expect("runtime entry")
                    .file_name()
                    .to_string_lossy()
                    .contains(".next-")),
            "incremental catch-up must not create a scratch projection"
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn issue_tree_transition_is_atomic_and_frontier_is_score_ordered() {
        let engine = engine();
        let root = std::env::temp_dir().join(format!("epistemic-issue-tree-{}", Uuid::new_v4()));
        let receipt = engine.issue_tree_transition(&root, &Map::from_iter([
            ("actor".into(), json!("tester")),
            ("authority_basis".into(), json!({"kind":"test"})),
            ("tree_id".into(), json!("tree:rh")),
            ("nodes".into(), json!([
                {"node_id":"issue:root","title":"Root issue","version":1,"score":0.2},
                {"node_id":"issue:front","title":"Priority issue","version":1,"score":0.9,"parent_id":"issue:root"}
            ])),
        ])).expect("atomic issue transition");
        assert_eq!(receipt["issue_tree_transition"]["atomic"], true);
        assert_eq!(
            receipt["issue_tree_transition"]["evidence_promotion"],
            false
        );
        let frontier = engine
            .issue_tree_frontier(
                &root,
                &Map::from_iter([
                    ("tree_id".into(), json!("tree:rh")),
                    ("limit".into(), json!(10)),
                ]),
            )
            .expect("issue frontier");
        assert_eq!(frontier["frontier"]["items"][0]["node_id"], "issue:front");
        assert_eq!(frontier["certifies_truth"], false);
        assert!(frontier["result_ref"]
            .as_str()
            .unwrap_or_default()
            .starts_with("issue-tree-frontier:"));
        let invalid = engine.issue_tree_transition(&root, &Map::from_iter([
            ("actor".into(), json!("tester")),
            ("authority_basis".into(), json!({"kind":"test"})),
            ("tree_id".into(), json!("tree:rh")),
            ("nodes".into(), json!([
                {"node_id":"issue:invalid","title":"Invalid blocked issue","version":1,"state":"blocked"}
            ])),
        ])).expect_err("blocked issue without blockers");
        assert_eq!(invalid["code"], "issue_tree_invalid");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn issue_tree_resume_creates_and_ordinary_transition_advances_selected_leaf() {
        let engine = engine();
        let root =
            std::env::temp_dir().join(format!("epistemic-issue-tree-resume-{}", Uuid::new_v4()));
        let resume_args = Map::from_iter([
            (
                "objective".into(),
                json!("Resolve the excellent AX frontier"),
            ),
            ("create_if_missing".into(), json!(true)),
            ("actor".into(), json!("tester")),
            ("authority_basis".into(), json!({"kind":"test"})),
            ("max_inline_chars".into(), json!(6000)),
        ]);
        let resumed = engine
            .issue_tree_resume(&root, &resume_args)
            .expect("create and resume tree");
        assert_eq!(resumed["status"], "ok");
        assert_eq!(resumed["selected"]["state"], "selected");
        assert!(resumed["inline_chars"].as_u64().unwrap_or(u64::MAX) <= 6000);
        let tree_id = resumed["tree"]["tree_id"]
            .as_str()
            .expect("tree id")
            .to_string();
        let selected_id = resumed["selected"]["node_id"]
            .as_str()
            .expect("selected id")
            .to_string();
        assert_eq!(resumed["frontier"]["scope"], "unselected alternatives; selected work is represented once in selected");
        assert!(!resumed["frontier"]["items"].as_array().unwrap().iter().any(|item| item["node_id"] == selected_id));
        let hinted = engine.issue_tree_resume(&root, &Map::from_iter([
            ("tree_id".into(), json!(tree_id.clone())),
            ("objective".into(), json!("A paraphrase that is not the stored objective")),
        ])).expect("tree id remains authoritative when objective is only a hint");
        assert_eq!(hinted["tree"]["tree_id"], tree_id);
        assert_eq!(hinted["objective_match"]["exact_normalized_match"], false);
        assert_eq!(hinted["objective_match"]["lookup_effect"], "hint_only_tree_id_was_authoritative");
        let transition = engine.issue_tree_transition(&root, &Map::from_iter([
            ("actor".into(), json!("tester")),
            ("authority_basis".into(), json!({"kind":"test"})),
            ("tree_id".into(), json!(tree_id)),
            ("selected_node_id".into(), json!(selected_id)),
            ("expected_node_version".into(), json!(1)),
            ("idempotency_key".into(), json!("advance-excellent-ax-v1")),
            ("select_next".into(), json!(true)),
            ("transition".into(), json!({
                "disposition":"split",
                "rationale":"Decompose the objective without promoting evidence.",
                "successors":[{"node_id":"issue:excellent-ax:child","title":"Verify the first child frontier","score":0.8}]
            })),
        ])).expect("ordinary transition");
        assert_eq!(transition["workflow"]["disposition"], "split");
        assert_eq!(
            transition["workflow"]["resulting_selected"]["node_id"],
            "issue:excellent-ax:child"
        );
        assert_eq!(transition["workflow"]["certifies_truth"], false);
        let replay = engine.issue_tree_transition(&root, &Map::from_iter([
            ("actor".into(), json!("tester")),
            ("authority_basis".into(), json!({"kind":"test"})),
            ("tree_id".into(), json!(tree_id)),
            ("selected_node_id".into(), json!(selected_id)),
            ("expected_node_version".into(), json!(1)),
            ("idempotency_key".into(), json!("advance-excellent-ax-v1")),
            ("select_next".into(), json!(true)),
            ("transition".into(), json!({
                "disposition":"split",
                "rationale":"Decompose the objective without promoting evidence.",
                "successors":[{"node_id":"issue:excellent-ax:child","title":"Verify the first child frontier","score":0.8}]
            })),
        ])).expect("exact retry replays receipt");
        assert_eq!(replay, transition);
        let stale = engine
            .issue_tree_transition(
                &root,
                &Map::from_iter([
                    ("actor".into(), json!("tester")),
                    ("authority_basis".into(), json!({"kind":"test"})),
                    ("tree_id".into(), json!(tree_id)),
                    ("selected_node_id".into(), json!(selected_id)),
                    ("expected_node_version".into(), json!(1)),
                    ("idempotency_key".into(), json!("stale-excellent-ax-v1")),
                    ("transition".into(), json!({"disposition":"resolved"})),
                ]),
            )
            .expect_err("stale selected leaf must fail");
        assert_eq!(stale["code"], "issue_tree_selected_conflict");
        let _ = fs::remove_dir_all(root);
    }
