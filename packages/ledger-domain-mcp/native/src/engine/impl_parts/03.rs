impl Engine {
    pub fn call_tool(
        &self,
        name: &str,
        args: &Map<String, Value>,
        site_root: &Path,
    ) -> Result<Value, Value> {
        let prefix = format!("{}_", self.domain.identity.tool_prefix);
        let unknown =
            || Err(self.error("unknown_tool", &format!("unknown_tool:{name}"), Value::Null));
        let Some(verb) = name.strip_prefix(&prefix) else {
            return unknown();
        };
        let advertised = self.domain.tools.iter().any(|tool| {
            tool.name == name
                && tool
                    .feature
                    .as_deref()
                    .map(|feature| self.domain.features.enabled(feature))
                    .unwrap_or(true)
        });
        if !advertised {
            return unknown();
        }
        // Feature-owned verbs dispatch only when the feature is enabled.
        let feature = match verb {
            "source_inspect" => Some("source_inspect"),
            "snapshot" => Some("snapshot"),
            "export" => Some("export"),
            "sequence_create"
            | "sequence_status"
            | "sequence_list"
            | "sequence_claim_next"
            | "sequence_claims" => Some("sequences"),
            "proposal_submit"
            | "operations_batch"
            | "issue_tree_transition"
            | "submit_review_admit"
            | "capture_sources"
            | "proposal_read"
            | "proposal_resubmit"
            | "proposal_review"
            | "proposal_admit"
            | "proposal_reject" => Some("proposals"),
            _ => None,
        };
        if let Some(feature) = feature {
            if !self.domain.features.enabled(feature) {
                return unknown();
            }
        }
        match verb {
            "guidance" => Ok(self.guidance_with_request(args)),
            "status" => self.status(site_root),
            "communication_migration_preflight" => {
                self.communication_migration_preflight(site_root, args)
            }
            "communication_migrate" => self.communication_migrate(site_root, args),
            "query" => {
                let has_raw_query = args.contains_key("query");
                let has_template = args.contains_key("template");
                let named_fields = [
                    "recipient",
                    "participant",
                    "sender",
                    "from",
                    "to",
                    "kinds",
                    "since_event",
                    "after_sequence",
                    "include_body",
                    "direction",
                    "viewer",
                    "intent",
                    "read_state",
                    "reply_state",
                    "match",
                    "root",
                    "max_depth",
                ];
                let legacy_fields = ["kind", "record_kind", "text", "compact", "offset"];
                let has_named_fields = named_fields.iter().any(|field| args.contains_key(*field));
                let has_legacy_fields = legacy_fields.iter().any(|field| args.contains_key(*field));
                let has_cursor = args
                    .get("cursor")
                    .map(|value| !value.is_null())
                    .unwrap_or(false);
                let is_team_work_template = args.get("template").and_then(Value::as_str)
                    .map(|template| self.canonical_named_template(template) == "epistemic:team-work-overview")
                    .unwrap_or(false);
                if is_team_work_template && !has_raw_query {
                    self.team_work_overview(site_root, args)
                } else if has_raw_query && has_template {
                    Err(self.error(
                        "query_mode_ambiguous",
                        "query and template are mutually exclusive",
                        Value::Null,
                    ))
                } else if !has_raw_query && !has_template && has_cursor {
                    Err(self.error(
                        "query_cursor_unsupported",
                        "legacy queries use offset pagination; cursor requires query or template",
                        Value::Null,
                    ))
                } else if has_raw_query && (has_named_fields || has_legacy_fields) {
                    Err(self.error(
                        "query_mode_mixed",
                        "raw Datalog query cannot be combined with named-query filters",
                        json!({"fields":named_fields.iter().chain(legacy_fields.iter()).filter(|field| args.contains_key(**field)).collect::<Vec<_>>() }),
                    ))
                } else if has_raw_query || has_template {
                    if has_raw_query {
                        self.validate_raw_query_arguments(args)?;
                    }
                    self.generic_query(site_root, args)
                } else if has_named_fields {
                    Err(self.error(
                        "query_template_missing",
                        "template is required when named-query filters are supplied",
                        Value::Null,
                    ))
                } else {
                    self.query(site_root, args)
                }
            }
            "message_mark_read" => self.message_mark_read(site_root, args),
            "communication_inbox_poll" => {
                let mut query_args = args.clone();
                query_args.insert("template".into(), json!("epistemic:inbox"));
                if !query_args.contains_key("latest") { query_args.insert("latest".into(), json!(true)); }
                let mut result = self.query(site_root, &query_args)?;
                let checkpoint = result["ledger_head"].clone();
                let last_sequence = result["last_sequence"].clone();
                if let Some(object) = result.as_object_mut() {
                    object.insert("poll_contract".into(), json!({
                        "phase":args.get("phase").cloned().unwrap_or_else(|| json!("unspecified")),
                        "checkpoint_ledger_head":checkpoint,
                        "distinct_turn_boundary_checks_required":true,
                        "next_poll":{"tool":"epistemic_graph_communication_inbox_poll","arguments":{"participant":args.get("participant").or_else(|| args.get("recipient")).cloned().unwrap_or(Value::Null),"after_sequence":last_sequence,"phase":"closing"}}
                    }));
                }
                Ok(result)
            }
            "query_batch" => self.query_batch(site_root, args),
            "team_work_overview" => self.team_work_overview(site_root, args),
            "source_inspect" => self.source_inspect(site_root, args),
            "neighborhood" => self.neighborhood(site_root, args),
            "snapshot" => self.snapshot(site_root, args),
            "sequence_create" => self.sequence_create(site_root, args),
            "sequence_status" => self.sequence_status(site_root, args),
            "sequence_list" => self.sequence_list(site_root, args),
            "sequence_claim_next" => self.sequence_claim_next(site_root, args),
            "sequence_claims" => self.sequence_claims(site_root, args),
            "proposal_submit" => {
                let payload_ref = args.get("payload_ref").and_then(Value::as_str);
                let resolved = self.resolve_payload_arguments(site_root, args)?;
                self.proposal_submit(site_root, &resolved)
                    .map_err(|error| self.enrich_payload_ref_refusal(error, payload_ref, name))
            }
            "operations_batch" => self.operations_batch(site_root, args),
            "issue_tree_transition" => self.issue_tree_transition(site_root, args),
            "issue_tree_resume" => self.issue_tree_resume(site_root, args),
            "issue_tree_frontier" => self.issue_tree_frontier(site_root, args),
            "issue_tree_frontier_read" => self.issue_tree_frontier_read(site_root, args),
            "submit_review_admit" => {
                let payload_ref = args.get("payload_ref").and_then(Value::as_str);
                let resolved = self.resolve_payload_arguments(site_root, args)?;
                self.submit_review_admit(site_root, &resolved)
                    .map_err(|error| self.enrich_payload_ref_refusal(error, payload_ref, name))
            }
            "capture_sources" => self.capture_sources(site_root, args),
            "proposal_read" => self.proposal_read(site_root, args),
            "proposal_resubmit" => self.proposal_resubmit(site_root, args),
            "proposal_review" => self.proposal_review(site_root, args),
            "proposal_admit" => self.proposal_admit(site_root, args),
            "proposal_reject" => self.proposal_reject(site_root, args),
            "export" => self.export(site_root, args),
            _ => unknown(),
        }
    }

}
