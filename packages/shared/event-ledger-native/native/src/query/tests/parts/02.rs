    #[test]
    fn joins_and_keyset_pagination_are_deterministic() {
        let query = json!({
            "find":["?message"],
            "inputs":{"recipient":"marici.Grothendieck"},
            "where":[
                {"triple":{"subject":"?message","attribute":"kind","object":{"value":"communication"}}},
                {"triple":{"subject":"?message","attribute":"recipient","object":{"input":"recipient"}}},
                {"triple":{"subject":"?message","attribute":"sequence","object":"?sequence"}},
                {"triple":{"subject":"?message","attribute":"event_id","object":"?event_id"}}
            ],
            "order_by":[{"term":"?sequence"},{"term":"?event_id"}],
            "limit":1
        });
        let datoms = vec![
            datom("m2", "kind", json!("communication"), 2, "e2"),
            datom("m2", "recipient", json!("marici.Grothendieck"), 2, "e2"),
            datom("m2", "sequence", json!(2), 2, "e2"),
            datom("m2", "event_id", json!("e2"), 2, "e2"),
            datom("m1", "kind", json!("communication"), 1, "e1"),
            datom("m1", "recipient", json!("marici.Grothendieck"), 1, "e1"),
            datom("m1", "sequence", json!(1), 1, "e1"),
            datom("m1", "event_id", json!("e1"), 1, "e1"),
        ];
        let spec = parse(&query, 20, &limits()).expect("query parses");
        let first = execute(&spec, &datoms).expect("query executes");
        assert_eq!(first.bindings.len(), 1);
        assert_eq!(first.bindings[0]["?message"], "m1");
        assert!(first.has_more);

        let second_query = json!({
            "find":["?message"],
            "inputs":{"recipient":"marici.Grothendieck"},
            "where":query["where"],
            "order_by":query["order_by"],
            "limit":1,
            "cursor":{"values":{"?sequence":1,"?event_id":"e1"}}
        });
        let second = execute(
            &parse(&second_query, 20, &limits()).expect("cursor parses"),
            &datoms,
        )
        .expect("cursor executes");
        assert_eq!(second.bindings.len(), 1);
        assert_eq!(second.bindings[0]["?message"], "m2");
        assert!(!second.has_more);
    }

    #[test]
    fn reachability_is_bounded_and_joins_the_target_variable() {
        let query = json!({
            "find":["?message"],
            "inputs":{"root":"m0"},
            "where":[
                {"reachable":{"from":{"input":"root"},"attribute":"replied_by","to":"?message","max_depth":2}}
            ],
            "order_by":[{"term":"?message"}],
            "limit":10
        });
        let datoms = vec![
            datom("m0", "replied_by", json!("m1"), 1, "e1"),
            datom("m1", "replied_by", json!("m2"), 2, "e2"),
            datom("m2", "replied_by", json!("m3"), 3, "e3"),
        ];
        let result = execute(
            &parse(&query, 20, &limits()).expect("query parses"),
            &datoms,
        )
        .expect("query executes");
        let ids = result
            .bindings
            .iter()
            .map(|binding| binding["?message"].as_str().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(ids, vec!["m1", "m2"]);
    }

    #[test]
    fn unbound_reachability_and_compare_are_refused() {
        let reachable = json!({
            "find":["?message"],
            "where":[{"reachable":{"from":"?root","attribute":"replied_by","to":"?message"}}],
            "order_by":[{"term":"?message"}],
            "limit":10
        });
        let failure = execute(
            &parse(&reachable, 20, &limits()).expect("reachable query parses"),
            &[],
        )
        .expect_err("unbound reachable source must refuse");
        assert_eq!(failure.code, "query_reachable_unbound");

        let compare_query = json!({
            "find":["?message"],
            "where":[{"compare":{"op":"=","left":"?missing","right":1}}],
            "limit":10
        });
        let failure = execute(
            &parse(&compare_query, 20, &limits()).expect("compare query parses"),
            &[],
        )
        .expect_err("unbound compare term must refuse");
        assert_eq!(failure.code, "query_compare_unbound");
    }

    #[test]
    fn multiple_nested_predicates_filter_the_entire_binding_set() {
        let query = json!({
            "find":["?message"],
            "where":[
                {"triple":{"subject":"?message","attribute":"kind","object":"communication"}},
                {"not_exists":{"where":[
                    {"triple":{"subject":"?receipt","attribute":"kind","object":"message_read"}},
                    {"triple":{"subject":"?receipt","attribute":"message_id","object":"?message"}}
                ]}},
                {"not_exists":{"where":[
                    {"triple":{"subject":"?reply","attribute":"replies_to","object":"?message"}}
                ]}}
            ],
            "order_by":[{"term":"?message"}],
            "limit":10
        });
        let datoms = vec![
            datom("m1", "kind", json!("communication"), 1, "e1"),
            datom("m2", "kind", json!("communication"), 2, "e2"),
            datom("m3", "kind", json!("communication"), 3, "e3"),
            datom("r1", "kind", json!("message_read"), 4, "e4"),
            datom("r1", "message_id", json!("m1"), 4, "e4"),
            datom("reply", "replies_to", json!("m2"), 5, "e5"),
        ];
        let result = execute(
            &parse(&query, 20, &limits()).expect("query parses"),
            &datoms,
        )
        .expect("nested predicates execute");
        assert_eq!(result.bindings.len(), 1);
        assert_eq!(result.bindings[0]["?message"], "m3");
    }

    #[test]
    fn nested_predicates_are_planned_by_bindings() {
        let query = json!({
            "find":["?message"],
            "where":[
                {"triple":{"subject":"?message","attribute":"kind","object":"communication"}},
                {"exists":{"where":[
                    {"compare":{"op":"=","left":"?reply_kind","right":"reply"}},
                    {"triple":{"subject":"?reply","attribute":"kind","object":"?reply_kind"}},
                    {"triple":{"subject":"?reply","attribute":"target","object":"?message"}}
                ]}}
            ],
            "order_by":[{"term":"?message"}],
            "limit":10
        });
        let datoms = vec![
            datom("m1", "kind", json!("communication"), 1, "e1"),
            datom("m2", "kind", json!("communication"), 2, "e2"),
            datom("r1", "kind", json!("reply"), 3, "e3"),
            datom("r1", "target", json!("m1"), 3, "e3"),
            datom("r2", "kind", json!("note"), 4, "e4"),
            datom("r2", "target", json!("m2"), 4, "e4"),
        ];
        let result = execute(
            &parse(&query, 20, &limits()).expect("query parses"),
            &datoms,
        )
        .expect("nested planner should move compare after its binding triple");
        assert_eq!(result.bindings.len(), 1);
        assert_eq!(result.bindings[0]["?message"], "m1");
    }

    #[test]
    fn cursor_without_order_is_refused() {
        let query = json!({
            "find":["?message"],
            "where":[{"triple":{"subject":"?message","attribute":"kind","object":"claim"}}],
            "cursor":{"values":{"?message":"m1"}}
        });
        let failure = parse(&query, 20, &limits()).expect_err("unordered cursor must refuse");
        assert_eq!(failure.code, "query_cursor_requires_order");
    }

    #[test]
    fn cursor_type_mismatch_is_refused() {
        let query = json!({
            "find":["?message"],
            "where":[{"triple":{"subject":"?message","attribute":"kind","object":"claim"}}],
            "order_by":[{"term":"?message"}],
            "limit":1,
            "cursor":{"values":{"?message":1}}
        });
        let spec = parse(&query, 20, &limits()).expect("cursor parses");
        let failure = execute(&spec, &[datom("m1", "kind", json!("claim"), 1, "e1")])
            .expect_err("incomparable cursor must refuse");
        assert_eq!(failure.code, "query_cursor_type_mismatch");
    }

    #[test]
    fn paginated_order_must_be_unique() {
        let query = json!({
            "find":["?message"],
            "where":[{"triple":{"subject":"?message","attribute":"kind","object":"?kind"}}],
            "order_by":[{"term":"?kind"}],
            "limit":1
        });
        let spec = parse(&query, 20, &limits()).expect("query parses");
        let failure = execute(
            &spec,
            &[
                datom("m1", "kind", json!("claim"), 1, "e1"),
                datom("m2", "kind", json!("claim"), 2, "e2"),
            ],
        )
        .expect_err("non-unique ordering must refuse pagination");
        assert_eq!(failure.code, "query_order_not_unique");
    }

    #[test]
    fn exists_and_not_exists_filter_without_leaking_inner_bindings() {
        let query = json!({
            "find":["?message"],
            "inputs":{"reader":"marici.Grothendieck"},
            "where":[
                {"triple":{"subject":"?message","attribute":"kind","object":"communication"}},
                {"not_exists":{"where":[
                    {"triple":{"subject":"?receipt","attribute":"kind","object":"message_read"}},
                    {"triple":{"subject":"?receipt","attribute":"message_id","object":"?message"}},
                    {"triple":{"subject":"?receipt","attribute":"reader","object":{"input":"reader"}}}
                ]}}
            ],
            "order_by":[{"term":"?message"}],
            "limit":10
        });
        let datoms = vec![
            datom("m1", "kind", json!("communication"), 1, "e1"),
            datom("m2", "kind", json!("communication"), 2, "e2"),
            datom("r1", "kind", json!("message_read"), 3, "e3"),
            datom("r1", "message_id", json!("m1"), 3, "e3"),
            datom("r1", "reader", json!("marici.Grothendieck"), 3, "e3"),
        ];
        let spec = parse(&query, 20, &limits()).expect("query parses");
        let result = execute(&spec, &datoms).expect("query executes");
        assert_eq!(result.bindings.len(), 1);
        assert_eq!(result.bindings[0]["?message"], "m2");
        assert!(!result.bindings[0].contains_key("?receipt"));
    }

    #[test]
    fn unknown_query_fields_are_refused() {
        let query = json!({
            "find":["?message"],
            "where":[{"triple":{"subject":"?message","attribute":"kind","object":"claim"}}],
            "wat":true
        });
        let failure = parse(&query, 20, &limits()).expect_err("unknown query fields must refuse");
        assert_eq!(failure.code, "query_invalid_field");
    }

