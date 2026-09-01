
    use super::*;

    fn datom(subject: &str, attribute: &str, value: Value, sequence: u64, event_id: &str) -> Datom {
        Datom {
            origin_id: subject.to_string(),
            subject: subject.to_string(),
            attribute: attribute.to_string(),
            value,
            event_sequence: sequence,
            event_id: event_id.to_string(),
        }
    }

    fn limits() -> QueryLimits {
        QueryLimits {
            max_clauses: 16,
            max_results: 100,
            max_reach_depth: 8,
            max_one_of_values: 16,
            max_predicate_depth: 8,
            max_datoms_scanned: 10_000,
            max_traversal_edges: 10_000,
        }
    }

    #[test]
    fn executable_clauses_are_planned_by_bindings() {
        let query = json!({
            "find":["?message"],
            "where":[
                {"compare":{"op":">","left":"?sequence","right":1}},
                {"triple":{"subject":"?message","attribute":"kind","object":"claim"}},
                {"triple":{"subject":"?message","attribute":"sequence","object":"?sequence"}}
            ],
            "order_by":[{"term":"?sequence"}],
            "limit":10
        });
        let datoms = vec![
            datom("m1", "kind", json!("claim"), 1, "e1"),
            datom("m1", "sequence", json!(1), 1, "e1"),
            datom("m2", "kind", json!("claim"), 2, "e2"),
            datom("m2", "sequence", json!(2), 2, "e2"),
        ];
        let result = execute(
            &parse(&query, 20, &limits()).expect("query parses"),
            &datoms,
        )
        .expect("planner should move compare after its binding triple");
        assert_eq!(result.bindings.len(), 1);
        assert_eq!(result.bindings[0]["?message"], "m2");
    }

    #[test]
    fn datom_scan_and_traversal_budgets_refuse_expensive_queries() {
        let scan_query = json!({
            "find":["?message"],
            "where":[{"triple":{"subject":"?message","attribute":"kind","object":"claim"}}],
            "limit":10
        });
        let mut scan_limits = limits();
        scan_limits.max_datoms_scanned = 1;
        let scan_failure = execute(
            &parse(&scan_query, 20, &scan_limits).expect("scan query parses"),
            &[
                datom("m1", "kind", json!("claim"), 1, "e1"),
                datom("m2", "kind", json!("claim"), 2, "e2"),
            ],
        )
        .expect_err("scan budget must refuse");
        assert_eq!(scan_failure.code, "query_datom_scan_limit");

        let reach_query = json!({
            "find":["?message"],
            "inputs":{"root":"m0"},
            "where":[{"reachable":{"from":{"input":"root"},"attribute":"replied_by","to":"?message","max_depth":2}}],
            "limit":10
        });
        let mut reach_limits = limits();
        reach_limits.max_traversal_edges = 1;
        let reach_failure = execute(
            &parse(&reach_query, 20, &reach_limits).expect("reach query parses"),
            &[
                datom("m0", "replied_by", json!("m1"), 1, "e1"),
                datom("m1", "replied_by", json!("m2"), 2, "e2"),
            ],
        )
        .expect_err("traversal budget must refuse");
        assert_eq!(reach_failure.code, "query_traversal_limit");
    }

    #[test]
    fn bound_subject_joins_use_the_subject_index_within_scan_budget() {
        let query = json!({
            "find":["?message","?sequence"],
            "where":[
                {"triple":{"subject":"?message","attribute":"recipient","object":"marici.Nima"}},
                {"triple":{"subject":"?message","attribute":"kind","object":"communication"}},
                {"triple":{"subject":"?message","attribute":"sequence","object":"?sequence"}}
            ],
            "order_by":[{"term":"?sequence"}],
            "limit":100
        });
        let mut datoms = Vec::new();
        for sequence in 1..=100 {
            let subject = format!("message-{sequence}");
            let event = format!("event-{sequence}");
            datoms.push(datom(
                &subject,
                "recipient",
                json!("marici.Nima"),
                sequence,
                &event,
            ));
            datoms.push(datom(
                &subject,
                "kind",
                json!("communication"),
                sequence,
                &event,
            ));
            datoms.push(datom(
                &subject,
                "sequence",
                json!(sequence),
                sequence,
                &event,
            ));
        }
        let mut indexed_limits = limits();
        indexed_limits.max_datoms_scanned = 700;
        let result = execute(
            &parse(&query, 100, &indexed_limits).expect("indexed join query parses"),
            &datoms,
        )
        .expect("bound subject joins must stay within the indexed scan budget");
        assert_eq!(result.bindings.len(), 100);
    }

    #[test]
    fn query_shape_limits_cover_inputs_and_order_terms() {
        let mut shape_limits = limits();
        shape_limits.max_clauses = 1;
        let base = |extra: Value| {
            let mut query = json!({
                "find":["?message"],
                "where":[{"triple":{"subject":"?message","attribute":"kind","object":"claim"}}]
            });
            if let (Some(target), Some(extra)) = (query.as_object_mut(), extra.as_object()) {
                for (key, value) in extra {
                    target.insert(key.clone(), value.clone());
                }
            }
            query
        };
        let input_failure = parse(
            &base(json!({"inputs":{"?one":1,"?two":2}})),
            20,
            &shape_limits,
        )
        .expect_err("input count must be bounded");
        assert_eq!(input_failure.code, "query_input_limit");
        let order_failure = parse(
            &base(json!({
                "order_by":[{"term":"?message"},{"term":"?message"}]
            })),
            20,
            &shape_limits,
        )
        .expect_err("order term count must be bounded");
        assert_eq!(order_failure.code, "query_order_limit");
    }

    #[test]
    fn query_shape_limits_cover_terms_predicates_and_normalized_inputs() {
        let mut term_limits = limits();
        term_limits.max_one_of_values = 2;
        let one_of_query = json!({
            "find":["?message"],
            "where":[{"triple":{"subject":"?message","attribute":"kind","object":{"one_of":["claim","test","source"]}}}]
        });
        let one_of_failure =
            parse(&one_of_query, 20, &term_limits).expect_err("one_of values must be bounded");
        assert_eq!(one_of_failure.code, "query_term_limit");

        let mut predicate_limits = limits();
        predicate_limits.max_predicate_depth = 2;
        let mut nested = json!({
            "triple":{"subject":"?message","attribute":"kind","object":"claim"}
        });
        for _ in 0..3 {
            nested = json!({"exists":{"where":[nested]}});
        }
        let predicate_failure = parse(
            &json!({"find":["?message"],"where":[nested]}),
            20,
            &predicate_limits,
        )
        .expect_err("nested predicate depth must be bounded");
        assert_eq!(predicate_failure.code, "query_predicate_depth_limit");

        let mut clause_limits = limits();
        clause_limits.max_clauses = 3;
        let nested_clause = json!({
            "exists":{"where":[
                {"triple":{"subject":"?message","attribute":"kind","object":"claim"}},
                {"exists":{"where":[
                    {"triple":{"subject":"?message","attribute":"status","object":"open"}}
                ]}}
            ]}
        });
        let clause_failure = parse(
            &json!({"find":["?message"],"where":[nested_clause]}),
            20,
            &clause_limits,
        )
        .expect_err("nested predicate clauses must share the clause budget");
        assert_eq!(clause_failure.code, "query_clause_limit");

        let duplicate_input_failure = parse(
            &json!({
                "find":["?message"],
                "inputs":{"message":"one","?message":"two"},
                "where":[{"triple":{"subject":"?message","attribute":"kind","object":"claim"}}]
            }),
            20,
            &limits(),
        )
        .expect_err("normalized input names must be unique");
        assert_eq!(duplicate_input_failure.code, "query_duplicate_input");

        let typed_pull = parse(
            &json!({
                "find":[{"pull":{"var":"?relation","target_kind":"relation","fields":["*"]}}],
                "where":[{"triple":{"subject":"?relation","attribute":"relation/id","object":"?relation"}}]
            }),
            20,
            &limits(),
        )
        .expect("typed pull parses");
        assert_eq!(typed_pull.pulls[0].target_kind.as_deref(), Some("relation"));
    }

