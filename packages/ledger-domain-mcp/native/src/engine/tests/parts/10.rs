    fn append_team_work_event(root: &Path, engine: &Engine, actor: &str, operations: Value) {
        engine.prepare(root).expect("prepare team-work fixture root");
        event_ledger::append_event(
            engine.error,
            &engine.ledger_layout(root),
            engine.event_hash_field,
            None,
            None,
            |ctx| json!({
                "schema":engine.domain.storage.event_schema_id,
                "sequence":ctx.sequence,
                "event_id":ctx.event_id,
                "previous_hash":ctx.previous_hash,
                "occurred_at":format!("2026-09-01T00:{:02}:00Z", ctx.sequence % 60),
                "operations":operations,
                "actor":actor
            }),
        ).expect("append team-work fixture event");
    }

    fn tree_and_issue(tree: &str, issue: &str, state: &str, disposition: Option<&str>) -> Value {
        let mut node = json!({
            "op":"entity.declare","entity_id":issue,"kind":"research_issue","title":format!("Work {issue}"),
            "tree_id":tree,"version":"1","state":state,"score":0.8
        });
        if let Some(disposition) = disposition { node["disposition"] = json!(disposition); }
        json!([
            {"op":"entity.declare","entity_id":tree,"kind":"research_issue_tree","title":format!("Objective {tree}"),"objective":format!("Objective {tree}"),"version":"1"},
            node
        ])
    }

    #[test]
    fn team_work_overview_attributes_active_assignments_and_never_frontier_or_anonymous_presence() {
        let engine = engine();
        let root = std::env::temp_dir().join(format!("epistemic-team-work-active-{}", Uuid::new_v4()));
        append_team_work_event(&root, &engine, "marici.Nima", tree_and_issue("tree:active", "issue:active", "selected", None));
        append_team_work_event(&root, &engine, "anonymous", tree_and_issue("tree:unowned", "issue:unowned", "open", None));
        append_team_work_event(&root, &engine, "anonymous", tree_and_issue("tree:assigned", "issue:assigned", "open", None));
        append_team_work_event(&root, &engine, "operator", json!([
            {"op":"relation.declare","relation_id":"rel:assign-benincasa","relation_type":"marici:assigned_to","source_id":"issue:assigned","target_id":"team_member:bc28f30924d7df1af02a"}
        ]));
        let result = engine.team_work_overview(&root, &Map::from_iter([("compact".into(), json!(false))])).expect("team overview");
        let items = result["items"].as_array().unwrap();
        let nima = items.iter().find(|item| item["member"] == "marici.Nima").unwrap();
        assert_eq!(nima["status"], "active");
        assert_eq!(nima["leaf"]["node_id"], "issue:active");
        assert_eq!(nima["attribution_basis"], "canonical_transition_actor");
        assert_eq!(nima["live_presence"]["claimed"], false);
        let benincasa = items.iter().find(|item| item["member"] == "marici.Benincasa").unwrap();
        assert_eq!(benincasa["attribution_basis"], "explicit_assignment_or_claim");
        assert_eq!(result["coverage"]["unattributed_active_tree_count"], 1);
        assert!(!items.iter().any(|item| item["tree_id"] == "tree:unowned"));
        assert!(!items.iter().any(|item| item["member"] == "anonymous"));
        assert_eq!(result["semantics"]["live_presence"], "not claimed without a separate typed heartbeat capability");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn team_work_overview_reports_blocked_handoffs_deferred_and_disposed() {
        let engine = engine();
        let root = std::env::temp_dir().join(format!("epistemic-team-work-states-{}", Uuid::new_v4()));
        append_team_work_event(&root, &engine, "operator", json!([
            {"op":"entity.declare","entity_id":"blocker:typed","kind":"claim","title":"Typed blocker"}
        ]));
        append_team_work_event(&root, &engine, "marici.Nima", tree_and_issue("tree:blocked", "issue:blocked", "blocked", None));
        append_team_work_event(&root, &engine, "operator", json!([
            {"op":"relation.declare","relation_id":"rel:block","relation_type":"blocked_by","source_id":"issue:blocked","target_id":"blocker:typed"},
            {"op":"relation.declare","relation_id":"rel:handoff","relation_type":"marici:handoff_to","source_id":"issue:blocked","target_id":"team_member:bc28f30924d7df1af02a"}
        ]));
        append_team_work_event(&root, &engine, "marici.Grothendieck", tree_and_issue("tree:deferred", "issue:deferred", "disposed", Some("deferred")));
        append_team_work_event(&root, &engine, "marici.Kitaev", tree_and_issue("tree:disposed", "issue:disposed", "disposed", Some("resolved")));
        let result = engine.team_work_overview(&root, &Map::from_iter([("compact".into(), json!(false))])).unwrap();
        let items = result["items"].as_array().unwrap();
        let blocked = items.iter().find(|item| item["tree_id"] == "tree:blocked").unwrap();
        assert_eq!(blocked["status"], "blocked");
        assert_eq!(blocked["blocker_count"], 1);
        assert_eq!(blocked["directed_handoff_count"], 1);
        assert_eq!(items.iter().find(|item| item["tree_id"] == "tree:deferred").unwrap()["status"], "deferred");
        assert_eq!(items.iter().find(|item| item["tree_id"] == "tree:disposed").unwrap()["status"], "none");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn team_work_overview_supports_many_to_many_attribution_and_staleness() {
        let engine = engine();
        let root = std::env::temp_dir().join(format!("epistemic-team-work-many-{}", Uuid::new_v4()));
        append_team_work_event(&root, &engine, "anonymous", tree_and_issue("tree:one", "issue:one", "open", None));
        append_team_work_event(&root, &engine, "anonymous", tree_and_issue("tree:two", "issue:two", "open", None));
        append_team_work_event(&root, &engine, "operator", json!([
            {"op":"relation.declare","relation_id":"rel:one-nima","relation_type":"marici:claimed_by","source_id":"issue:one","target_id":"team_member:aa2834674c8559a5dee0"},
            {"op":"relation.declare","relation_id":"rel:one-aspect","relation_type":"marici:handoff_accepted_by","source_id":"issue:one","target_id":"team_member:ae219c2b8562ec798ba1"},
            {"op":"relation.declare","relation_id":"rel:two-nima","relation_type":"marici:assigned_to","source_id":"issue:two","target_id":"team_member:aa2834674c8559a5dee0"}
        ]));
        for index in 0..101 {
            append_team_work_event(&root, &engine, "anonymous", json!([
                {"op":"entity.declare","entity_id":format!("claim:filler:{index}"),"kind":"claim","title":format!("Filler {index}")}
            ]));
        }
        let result = engine.team_work_overview(&root, &Map::new()).unwrap();
        let items = result["items"].as_array().unwrap();
        assert_eq!(items.iter().filter(|item| item["member"] == "marici.Nima").count(), 2);
        assert_eq!(items.iter().filter(|item| item["tree_id"] == "tree:one").count(), 2);
        assert!(items.iter().all(|item| item["status"] == "stale"));
        assert!(items.iter().all(|item| item["freshness"]["classification"] == "stale"));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn team_work_overview_paginates_at_one_head_and_rejects_drift() {
        let engine = engine();
        let root = std::env::temp_dir().join(format!("epistemic-team-work-page-{}", Uuid::new_v4()));
        append_team_work_event(&root, &engine, "marici.Nima", tree_and_issue("tree:a", "issue:a", "selected", None));
        append_team_work_event(&root, &engine, "marici.Aspect", tree_and_issue("tree:b", "issue:b", "selected", None));
        let first = engine.team_work_overview(&root, &Map::from_iter([("limit".into(), json!(1))])).unwrap();
        assert_eq!(first["returned"], 1);
        assert_eq!(first["has_more"], true);
        let cursor = first["next_cursor"].as_str().unwrap().to_string();
        let second = engine.team_work_overview(&root, &Map::from_iter([
            ("limit".into(), json!(1)), ("cursor".into(), json!(cursor.clone())),
            ("expected_ledger_head".into(), first["ledger_head"].clone())
        ])).unwrap();
        assert_eq!(second["ledger_head"], first["ledger_head"]);
        assert_ne!(second["items"][0]["member"], first["items"][0]["member"]);
        append_team_work_event(&root, &engine, "anonymous", json!([
            {"op":"entity.declare","entity_id":"claim:drift","kind":"claim","title":"Drift"}
        ]));
        let drift = engine.team_work_overview(&root, &Map::from_iter([("cursor".into(), json!(cursor))])).unwrap_err();
        assert_eq!(drift["code"], "ledger_head_mismatch");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn team_work_overview_unknown_requires_incomplete_coverage_and_query_batch_uses_same_model() {
        let engine = engine();
        let root = std::env::temp_dir().join(format!("epistemic-team-work-unknown-{}", Uuid::new_v4()));
        append_team_work_event(&root, &engine, "marici.Nima", tree_and_issue("tree:batch", "issue:batch", "selected", None));
        let unknown = engine.team_work_overview(&root, &Map::from_iter([("member_ids".into(), json!(["marici.Unknown"]))])).unwrap();
        assert_eq!(unknown["coverage"]["complete"], false);
        assert_eq!(unknown["items"][0]["status"], "unknown");
        assert_ne!(unknown["items"][0]["status"], "none");
        let batch = engine.query_batch(&root, &Map::from_iter([("queries".into(), json!([
            {"template":"epistemic:team-work-overview","member_ids":["marici.Nima"],"limit":5}
        ]))])).unwrap();
        assert_eq!(batch["results"][0]["result_schema"], "narada.epistemic.team_work_overview.v1");
        assert_eq!(batch["results"][0]["items"][0]["member"], "marici.Nima");
        let _ = fs::remove_dir_all(root);
    }
