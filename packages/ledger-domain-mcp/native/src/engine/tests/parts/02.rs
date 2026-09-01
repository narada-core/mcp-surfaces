    #[test]
    fn communication_entities_require_bounded_provenance_fields() {
        let engine = engine();
        let complete = json!({
            "op":"entity.declare",
            "entity_id":"communication:caroline-to-benincasa-1",
            "kind":"narada.epistemic:communication",
            "title":"Flavor result handoff",
            "sender":"marici.Caroline",
            "recipient":"marici.Benincasa",
            "body":"The loop phase is chart-level.",
            "intent":"result",
            "sent_at":"2026-08-19T19:00:00Z"
        });
        engine
            .validate_operations(&[complete], false)
            .expect("complete communication must validate");

        let incomplete = json!({
            "op":"entity.declare",
            "entity_id":"communication:incomplete",
            "kind":"narada.epistemic:communication",
            "title":"Incomplete message",
            "recipient":"marici.Benincasa",
            "intent":"result",
            "sent_at":"2026-08-19T19:00:00Z"
        });
        let failure = engine
            .validate_operations(&[incomplete], false)
            .expect_err("communication without sender must refuse");
        assert_eq!(failure["code"], "required_argument_missing");
        assert_eq!(failure["details"]["field"], "sender");

        let legacy = json!({"op":"entity.declare","entity_id":"communication:legacy","kind":"communication","title":"Legacy","sender":"a","recipient":"b","intent":"result","sent_at":"2026-08-19T19:00:00Z"});
        let failure = engine
            .validate_operations(&[legacy], false)
            .expect_err("legacy write must refuse");
        assert_eq!(failure["code"], "legacy_communication_kind_write_refused");
        assert_eq!(
            failure["details"]["canonical_replacement"],
            "narada.epistemic:communication"
        );

        let guidance = engine.guidance();
        assert_eq!(
            guidance.pointer("/communication_model/entity_kind"),
            Some(&json!("narada.epistemic:communication"))
        );
        assert_eq!(
            guidance.pointer("/communication_model/rule"),
            Some(&json!("Communication records provenance and argumentative causality, but does not become epistemic evidence unless a separate reviewed promotes_to_evidence relation is admitted."))
        );
        assert_eq!(
            guidance.pointer("/communication_model/sender_identity"),
            Some(&json!("The engine adds sender_identity_state to each new communication. It is self_claimed when sender equals proposal actor and asserted_by_actor otherwise; authentication is missing and authority_granted is false. Inbox and pull queries expose this state. Neither sent_by nor admission authenticates the sender."))
        );
    }

    #[test]
    fn self_claimed_actor_is_structured_and_grants_no_authority() {
        let state = Engine::identity_state_for_event(
            "marici.Aspect",
            "ledger.proposal_submit",
        );
        assert_eq!(
            state.pointer("/claimed_identity/identity"),
            Some(&json!("marici.Aspect"))
        );
        assert_eq!(
            state.pointer("/claimed_identity/status"),
            Some(&json!("claimed"))
        );
        assert_eq!(
            state.pointer("/claimed_identity/authority_granted"),
            Some(&json!(false))
        );
        assert_eq!(
            state.pointer("/authentication/status"),
            Some(&json!("missing"))
        );
        assert_eq!(
            state.pointer("/authority/granted"),
            Some(&json!(false))
        );
        let self_claim = Engine::sender_identity_state("marici.Aspect", "marici.Aspect");
        assert_eq!(
            self_claim.pointer("/status"),
            Some(&json!("self_claimed"))
        );
        assert_eq!(
            self_claim.pointer("/authority_granted"),
            Some(&json!(false))
        );
        let third_party = Engine::sender_identity_state("carrier", "marici.Aspect");
        assert_eq!(
            third_party.pointer("/status"),
            Some(&json!("asserted_by_actor"))
        );
    }

    #[test]
    fn payload_backed_submit_review_admit_preserves_canonical_validation() {
        let engine = engine();
        let root =
            std::env::temp_dir().join(format!("epistemic-payload-submit-{}", Uuid::new_v4()));
        let reference = "mcp_payload:epistemic-submit-test@v1";
        let payload = json!({
            "actor":"payload-test",
            "authority_basis":{"kind":"test","summary":"Immutable payload transport fixture."},
            "operations":[{
                "op":"entity.declare",
                "entity_id":"claim:payload-backed",
                "kind":"claim",
                "title":"Payload-backed canonical admission"
            }]
        });
        let canonical = serde_json::to_vec(&canonical_json(&payload)).expect("canonical payload");
        let path = root.join(".ai/tmp/mcp-payloads/workspace/epistemic-submit-test/v1.json");
        fs::create_dir_all(path.parent().expect("payload parent")).expect("payload directory");
        fs::write(
            &path,
            serde_json::to_vec_pretty(&json!({
                "schema":"narada.mcp_payload.revision.v1",
                "ref":reference,
                "payload_id":"epistemic-submit-test",
                "revision":1,
                "payload":payload,
                "byte_size":canonical.len(),
                "sha256":sha256(&canonical)
            }))
            .expect("payload record"),
        )
        .expect("write payload");

        let resolved = engine
            .resolve_payload_arguments(
                &root,
                &Map::from_iter([("payload_ref".into(), json!(reference))]),
            )
            .expect("resolve immutable payload");
        let admitted = engine
            .submit_review_admit(&root, &resolved)
            .expect("payload-backed canonical admission");
        assert_eq!(admitted["status"], "admitted");

        let legacy_payload = json!({
            "actor":"payload-test",
            "authority_basis":{"kind":"test","summary":"Immutable legacy payload refusal fixture."},
            "operations":[{
                "op":"entity.declare","local_ref":"message","kind":"marici:communication",
                "sender":"payload-test","recipient":"payload-reviewer","title":"Legacy payload",
                "intent":"result","sent_at":"2026-08-24T16:00:00Z"
            }]
        });
        let legacy_canonical =
            serde_json::to_vec(&canonical_json(&legacy_payload)).expect("canonical legacy payload");
        fs::write(
            &path,
            serde_json::to_vec_pretty(&json!({
                "schema":"narada.mcp_payload.revision.v1","ref":reference,
                "payload_id":"epistemic-submit-test","revision":1,"payload":legacy_payload,
                "byte_size":legacy_canonical.len(),"sha256":sha256(&legacy_canonical)
            }))
            .expect("legacy payload record"),
        )
        .expect("write legacy payload");
        let failure = engine
            .call_tool(
                "epistemic_graph_submit_review_admit",
                &Map::from_iter([("payload_ref".into(), json!(reference))]),
                &root,
            )
            .expect_err("legacy kind in immutable payload must refuse with recovery");
        assert_eq!(failure["code"], "legacy_communication_kind_write_refused");
        assert_eq!(
            failure["details"]["input_transport"],
            "immutable_payload_ref"
        );
        assert_eq!(failure["details"]["payload_revision_mutable"], false);
        assert_eq!(failure["details"]["graph_mutation_committed"], false);
        assert_eq!(
            failure["details"]["recovery"]["suggested_payload_ref"],
            "mcp_payload:epistemic-submit-test@v2"
        );
        assert_eq!(
            failure["details"]["recovery"]["replace"]["entity.kind"]["to"],
            "narada.epistemic:communication"
        );
        assert_eq!(
            failure["details"]["recovery"]["payload_revision_tools"]["derive"],
            "mcp_payload_derive"
        );
        assert_eq!(
            failure["details"]["recovery"]["then_retry_with"]["tool"],
            "epistemic_graph_submit_review_admit"
        );

        let mut record: Value =
            serde_json::from_slice(&fs::read(&path).expect("read payload")).expect("payload JSON");
        record["sha256"] =
            json!("ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff");
        fs::write(
            &path,
            serde_json::to_vec_pretty(&record).expect("tampered payload"),
        )
        .expect("write tampered payload");
        let failure = engine
            .resolve_payload_arguments(
                &root,
                &Map::from_iter([("payload_ref".into(), json!(reference))]),
            )
            .expect_err("tampered payload must refuse");
        assert_eq!(failure["code"], "payload_ref_sha256_mismatch");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn inbox_unifies_canonical_participant_ids_with_declared_names() {
        let engine = engine();
        let root = std::env::temp_dir().join(format!(
            "epistemic-participant-identity-test-{}",
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
            |ctx| {
                json!({
                    "schema":engine.domain.storage.event_schema_id,
                    "sequence":ctx.sequence,
                    "event_id":ctx.event_id,
                    "previous_hash":ctx.previous_hash,
                    "operations":[
                        {
                            "op":"entity.declare",
                            "entity_id":"team_member:kitaev",
                            "kind":"team_member",
                            "title":"marici.Kitaev",
                            "canonical_name":"marici.Kitaev"
                        },
                        {
                            "op":"entity.declare",
                            "entity_id":"communication:canonical-recipient",
                            "kind":"narada.epistemic:communication",
                            "title":"Canonical recipient message",
                            "sender":"team_member:aspect",
                            "recipient":"team_member:kitaev",
                            "body":"identity regression",
                            "intent":"request",
                            "sent_at":"2026-08-29T00:00:00Z"
                        }
                    ],
                    "actor":"historical-fixture"
                })
            },
        )
        .expect("append identity fixture");

        for participant in ["marici.Kitaev", "team_member:kitaev"] {
            let result = engine
                .generic_query(
                    &root,
                    &Map::from_iter([
                        ("template".into(), json!("inbox")),
                        ("recipient".into(), json!(participant)),
                        ("read_state".into(), json!("all")),
                        ("limit".into(), json!(10)),
                    ]),
                )
                .expect("identity-normalized inbox query");
            assert_eq!(result["count"], 1, "participant {participant}");
            assert_eq!(
                result["items"][0]["entity_id"],
                "communication:canonical-recipient"
            );
        }
        let receipt = engine
            .message_mark_read(
                &root,
                &Map::from_iter([
                    (
                        "message_id".into(),
                        json!("communication:canonical-recipient"),
                    ),
                    ("reader".into(), json!("marici.Kitaev")),
                    ("actor".into(), json!("protocol-test")),
                    (
                        "authority_basis".into(),
                        json!({"kind":"operator_request","summary":"identity alias regression"}),
                    ),
                ]),
            )
            .expect("canonical identity marks entity-addressed message read");
        assert_eq!(receipt["reader"], "marici.Kitaev");
        let _ = fs::remove_dir_all(root);
    }

