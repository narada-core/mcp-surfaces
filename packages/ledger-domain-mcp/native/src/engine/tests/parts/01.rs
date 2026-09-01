
    use super::*;

    fn descriptor_path() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../shared/ledger-domain-epistemic/domain.json")
    }

    fn engine() -> Engine {
        Engine::new(Descriptor::load(&descriptor_path()).expect("epistemic descriptor"))
            .expect("engine")
    }

    #[test]
    fn unbounded_inbox_scan_failure_names_the_sequence_continuation() {
        let engine = engine();
        let failure = engine.error(
            "query_datom_scan_limit",
            "query datom-scan budget was exceeded",
            json!({"max_datoms_scanned":200000}),
        );
        let refusal = engine.with_inbox_sequence_remediation(failure, false, true);
        assert_eq!(refusal["code"], "query_datom_scan_limit");
        assert_eq!(
            refusal["details"]["retry_arguments"]["after_sequence"],
            "<last_rehydrated_inbox_sequence>"
        );
        assert_eq!(refusal["details"]["planner_mode"], "indexed_subject_suffix");
        assert_eq!(refusal["details"]["max_datoms_scanned"], 200000);
    }

    #[test]
    fn bounded_or_non_inbox_scan_failure_is_not_rewritten() {
        let engine = engine();
        let failure = engine.error(
            "query_datom_scan_limit",
            "query datom-scan budget was exceeded",
            json!({"max_datoms_scanned":200000}),
        );
        assert_eq!(
            engine.with_inbox_sequence_remediation(failure.clone(), true, true),
            failure
        );
        assert_eq!(
            engine.with_inbox_sequence_remediation(failure.clone(), false, false),
            failure
        );
    }

    #[test]
    fn storage_layout_matches_the_epistemic_control_root_convention() {
        let engine = engine();
        let root = Path::new("site");
        assert_eq!(
            engine.ledger(root),
            Path::new("site").join(".narada/epistemic/ledger")
        );
        assert_eq!(
            engine.proposals(root),
            Path::new("site").join(".narada/epistemic/proposals")
        );
        assert_eq!(
            engine.sequences(root),
            Path::new("site").join(".narada/epistemic/sequences")
        );
        assert_eq!(
            engine.runtime(root),
            Path::new("site").join(".narada/.ai/epistemic-graph")
        );
        assert_eq!(
            engine.projection_path(root),
            Path::new("site").join(".narada/.ai/epistemic-graph/projection.sqlite")
        );
        let narada = Path::new("site/.narada");
        assert_eq!(engine.ledger(narada), narada.join("epistemic/ledger"));
        assert_eq!(engine.runtime(narada), narada.join(".ai/epistemic-graph"));
    }

    #[test]
    fn compact_operation_batches_expand_into_an_ordinary_immutable_proposal() {
        let engine = engine();
        let root =
            std::env::temp_dir().join(format!("epistemic-operations-batch-{}", Uuid::new_v4()));
        let args = json!({
            "actor":"operator",
            "authority_basis":{"kind":"operator_direct_instruction"},
            "batches":[{
                "defaults":{"op":"entity.declare","kind":"claim","version":"v1"},
                "columns":["title","locator"],
                "rows":[
                    ["First claim","urn:claim:first"],
                    ["Second claim","urn:claim:second"]
                ]
            }]
        });
        let receipt = engine
            .operations_batch(&root, args.as_object().expect("batch arguments"))
            .expect("compact proposal");
        assert_eq!(receipt["compact_input"]["expanded_operation_count"], 2);
        let proposal = engine
            .proposal_read(
                &root,
                json!({"proposal_id":receipt["proposal_id"],"limit":10})
                    .as_object()
                    .expect("read arguments"),
            )
            .expect("stored proposal");
        assert_eq!(proposal["operations"][0]["title"], "First claim");
        assert_eq!(proposal["operations"][1]["locator"], "urn:claim:second");

        let malformed = json!({
            "actor":"operator",
            "authority_basis":{"kind":"operator_direct_instruction"},
            "batches":[{
                "defaults":{"op":"entity.declare","kind":"claim"},
                "columns":["title","locator"],
                "rows":[["missing locator"]]
            }]
        });
        let error = engine
            .operations_batch(&root, malformed.as_object().expect("malformed arguments"))
            .expect_err("row width refusal");
        assert_eq!(error["code"], "operations_batch_row_width_mismatch");
        assert_eq!(error["details"]["batch_index"], 0);
        assert_eq!(error["details"]["row_index"], 0);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn proposal_tool_schema_describes_every_operation_shape() {
        let engine = engine();
        let schema = &engine
            .domain
            .tools
            .iter()
            .find(|tool| tool.name == "epistemic_graph_proposal_submit")
            .expect("proposal tool")
            .input_schema;
        let variants = schema
            .pointer("/properties/operations/items/oneOf")
            .and_then(Value::as_array)
            .expect("operation variants");
        assert_eq!(variants.len(), 5);
        assert_eq!(
            variants[0].pointer("/properties/op/const"),
            Some(&json!("entity.declare"))
        );
        assert_eq!(
            variants[1].pointer("/properties/op/const"),
            Some(&json!("relation.declare"))
        );
        assert_eq!(
            variants[2].pointer("/properties/evidence/items/required/2"),
            Some(&json!("paraphrase"))
        );
    }

    #[test]
    fn guidance_contains_copyable_end_to_end_workflow() {
        let engine = engine();
        let value = engine.guidance();
        assert_eq!(value["schema"], "narada.epistemic.guidance.v2");
        assert_eq!(
            value.pointer("/minimal_example/tool"),
            Some(&json!("epistemic_graph_submit_review_admit"))
        );
        assert_eq!(
            value.pointer("/minimal_example/arguments/operations/0/op"),
            Some(&json!("entity.declare"))
        );
        assert_eq!(
            value.pointer("/minimal_example/arguments/operations/2/op"),
            Some(&json!("relation.declare"))
        );
        assert_eq!(
            value.pointer("/payload_transport/accepted_by/1"),
            Some(&json!("epistemic_graph_submit_review_admit"))
        );
        assert_eq!(
            value.pointer("/immutable_payload_recovery/steps/1/action"),
            Some(&json!("create_successor_revision"))
        );
        assert_eq!(
            value.pointer("/communication_example/kind"),
            Some(&json!("narada.epistemic:communication"))
        );
        assert!(value["concurrency_rule"]
            .as_str()
            .unwrap_or_default()
            .contains("ledger_head"));
    }

    #[test]
    fn guidance_schema_accepts_declared_routing_hints() {
        let engine = engine();
        let tool = engine
            .list_tools()
            .into_iter()
            .find(|tool| tool["name"] == "epistemic_graph_guidance")
            .unwrap();
        assert_eq!(tool["inputSchema"]["additionalProperties"], false);
        assert_eq!(
            tool["inputSchema"]["properties"]["workflow"]["type"],
            "string"
        );
        let value = engine.guidance_with_request(
            json!({"workflow":"query_current_frontier"})
                .as_object()
                .unwrap(),
        );
        assert_eq!(value["requested"]["workflow"], "query_current_frontier");
    }

    #[test]
    fn disabled_feature_tools_are_hidden_and_refused() {
        let mut value: Value =
            serde_json::from_str(&fs::read_to_string(descriptor_path()).expect("descriptor text"))
                .expect("descriptor json");
        value["features"]["source_inspect"]["enabled"] = json!(false);
        let engine =
            Engine::new(Descriptor::from_value(value).expect("descriptor")).expect("engine");
        assert!(!engine
            .list_tools()
            .iter()
            .any(|tool| tool["name"] == "epistemic_graph_source_inspect"));
        let failure = engine
            .call_tool(
                "epistemic_graph_source_inspect",
                &Map::new(),
                Path::new("."),
            )
            .expect_err("disabled feature refuses");
        assert_eq!(failure["code"], "unknown_tool");
        assert_eq!(
            failure["message"],
            "unknown_tool:epistemic_graph_source_inspect"
        );
    }

    #[test]
    fn source_entity_requires_a_version_and_locator() {
        let engine = engine();
        let operation = json!({"op":"entity.declare","entity_id":"source:unlocated","kind":"source","title":"Unlocated source","version":"1"});
        let failure = engine
            .validate_operations(&[operation], false)
            .expect_err("unlocated source must refuse");
        assert_eq!(failure["code"], "required_argument_missing");
        assert_eq!(failure["details"]["field"], "locator");
    }

    #[test]
    fn extension_entity_kinds_must_be_namespaced() {
        let engine = engine();
        let extension = json!({"op":"entity.declare","entity_id":"exp:demo","kind":"cintamani:experiment","title":"Demo experiment","version":"1","payload":{"intent":"falsification"}});
        engine
            .validate_operations(&[extension], false)
            .expect("namespaced extension kind must validate");
        let bare = json!({"op":"entity.declare","entity_id":"exp:demo","kind":"experiment","title":"Demo experiment"});
        let failure = engine
            .validate_operations(&[bare], false)
            .expect_err("unnamespaced extension kind must refuse");
        assert_eq!(failure["code"], "invalid_entity_kind");
        assert_eq!(failure["details"]["kind"], "experiment");
    }

