    #[test]
    fn nested_query_fields_and_ambiguous_forms_are_refused() {
        let nested_unknown = json!({
            "find":["?message"],
            "where":[{"triple":{
                "subject":"?message",
                "attribute":"kind",
                "object":"claim",
                "typo":true
            }}]
        });
        let failure = parse(&nested_unknown, 20, &limits())
            .expect_err("unknown nested clause fields must refuse");
        assert_eq!(failure.code, "query_invalid_field");

        let ambiguous_triple = json!({
            "find":["?message"],
            "where":[{"triple":{
                "subject":"?message",
                "attribute":"kind",
                "object":"claim",
                "value":"claim"
            }}]
        });
        let failure = parse(&ambiguous_triple, 20, &limits())
            .expect_err("object/value aliases must not be ambiguous");
        assert_eq!(failure.code, "query_invalid_clause");

        let invalid_types = [
            (
                json!({
                    "find":["?message"],
                    "inputs":true,
                    "where":[{"triple":{"subject":"?message","attribute":"kind","object":"claim"}}]
                }),
                "query_invalid_inputs",
            ),
            (
                json!({
                    "find":["?message"],
                    "limit":"one",
                    "where":[{"triple":{"subject":"?message","attribute":"kind","object":"claim"}}]
                }),
                "query_invalid_limit",
            ),
            (
                json!({
                    "find":["?message"],
                    "order_by":{},
                    "where":[{"triple":{"subject":"?message","attribute":"kind","object":"claim"}}]
                }),
                "query_invalid_order",
            ),
        ];
        for (query, expected_code) in invalid_types {
            let failure =
                parse(&query, 20, &limits()).expect_err("invalid query types must refuse");
            assert_eq!(failure.code, expected_code);
        }
    }

    #[test]
    fn combinatorial_joins_share_the_server_capped_intermediate_work_bound() {
        let query = json!({
            "find":["?message", "?receipt"],
            "where":[
                {"triple":{"subject":"?message","attribute":"kind","object":"communication"}},
                {"triple":{"subject":"?receipt","attribute":"kind","object":"message_read"}}
            ],
            "limit":1
        });
        let mut datoms = Vec::new();
        for index in 0..101 {
            datoms.push(datom(
                &format!("message-{index}"),
                "kind",
                json!("communication"),
                1,
                "e1",
            ));
            datoms.push(datom(
                &format!("receipt-{index}"),
                "kind",
                json!("message_read"),
                2,
                "e2",
            ));
        }
        let spec = parse(&query, 20, &limits()).expect("query parses");
        let failure = execute(&spec, &datoms).expect_err("combinatorial work must refuse");
        assert!(matches!(
            failure.code,
            "query_work_limit" | "query_datom_scan_limit"
        ));
    }

    #[test]
    fn selective_history_may_exceed_result_limit_multiplier_without_false_refusal() {
        let query = json!({
            "find":["?message", "?sequence"],
            "where":[
                {"triple":{"subject":"?message","attribute":"kind","object":"communication"}},
                {"triple":{"subject":"?message","attribute":"recipient","object":"nima"}},
                {"triple":{"subject":"?message","attribute":"sequence","object":"?sequence"}}
            ],
            "order_by":[{"term":"?sequence","direction":"asc"},{"term":"?message","direction":"asc"}],
            "limit":5
        });
        let mut datoms = Vec::new();
        for index in 0..1500 {
            let subject = format!("message-{index:04}");
            datoms.push(datom(
                &subject,
                "kind",
                json!("communication"),
                index,
                "event",
            ));
            datoms.push(datom(&subject, "recipient", json!("nima"), index, "event"));
            datoms.push(datom(&subject, "sequence", json!(index), index, "event"));
        }
        let spec = parse(&query, 20, &limits()).expect("query parses");
        let result = execute(&spec, &datoms).expect("indexed history remains within capped work");
        assert_eq!(result.bindings.len(), 5);
        assert!(result.has_more);
    }
