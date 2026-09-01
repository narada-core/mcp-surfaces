impl Engine {
    fn proposal_resubmit(&self, root: &Path, args: &Map<String, Value>) -> Result<Value, Value> {
        let source_id = self.required(args, "source_proposal_id")?;
        let source = self.load_proposal(root, &source_id)?;
        let original = source["operations"].as_array().ok_or_else(|| {
            self.error(
                "proposal_corrupt",
                "proposal operations missing",
                json!({"proposal_id":source_id}),
            )
        })?;
        let requested_drops = args
            .get("drop_operation_ids")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let drop_ids = requested_drops
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_string)
            .collect::<HashSet<_>>();
        if drop_ids.len() != requested_drops.len() {
            return Err(self.error(
                "invalid_proposal_resubmission",
                "drop_operation_ids must contain unique strings",
                Value::Null,
            ));
        }
        let known = original
            .iter()
            .filter_map(|operation| self.operation_identity(operation))
            .collect::<HashSet<_>>();
        let missing = drop_ids.difference(&known).cloned().collect::<Vec<_>>();
        if !missing.is_empty() {
            return Err(self.error(
                &self
                    .domain
                    .features
                    .proposals
                    .resubmit
                    .missing_drop_refusal_code,
                "one or more drop_operation_ids do not identify source proposal operations",
                json!({"missing":missing}),
            ));
        }
        let replacements = args
            .get("replacements")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        self.validate_operations(&replacements, false)?;
        let mut operations = original
            .iter()
            .filter(|operation| {
                self.operation_identity(operation)
                    .map(|identity| !drop_ids.contains(&identity))
                    .unwrap_or(true)
            })
            .cloned()
            .collect::<Vec<_>>();
        operations.extend(replacements);
        let resubmit_caps = &self.domain.caps.resubmit;
        if (operations.len() as u64) < resubmit_caps.resulting_min
            || operations.len() as u64 > resubmit_caps.resulting_max
        {
            return Err(self.error(
                "invalid_proposal_resubmission",
                &format!(
                    "resulting operations count must be between {} and {}",
                    resubmit_caps.resulting_min, resubmit_caps.resulting_max
                ),
                json!({"count":operations.len()}),
            ));
        }
        let mut submit_args = Map::new();
        for key in [
            "actor",
            "authority_basis",
            "idempotency_key",
            "expected_ledger_head",
        ] {
            if let Some(value) = args.get(key) {
                submit_args.insert(key.to_string(), value.clone());
            }
        }
        submit_args.insert("operations".into(), json!(operations));
        let receipt = self.proposal_submit(root, &submit_args)?;
        Ok(json!({
            "schema":self.domain.features.proposals.resubmission_schema_id,
            "status":"draft_submitted",
            "source_proposal_id":source_id,
            "proposal_id":receipt["proposal_id"],
            "proposal_digest":receipt["proposal_digest"],
            "operation_count":receipt["operation_count"],
            "dropped_operation_ids":drop_ids,
            "replacement_count":args.get("replacements").and_then(Value::as_array).map_or(0, Vec::len),
            "expected_ledger_head":receipt["expected_ledger_head"],
            "next":{"review":{"tool":self.tool_name("proposal_review"),"proposal_id":receipt["proposal_id"]}},
            "admission_requires_explicit_call":true,
            "certifies_truth":self.domain.features.proposals.certifies_truth,
            "bounded":true
        }))
    }

    fn proposal_lifecycle(&self, root: &Path, proposal_id: &str) -> Result<Value, Value> {
        for path in self.ledger_files(root)? {
            let event = self.read_json(&path)?;
            if event.get("proposal_id").and_then(Value::as_str) == Some(proposal_id) {
                return Ok(json!({
                    "status":"admitted",
                    "event_id":event["event_id"],
                    "sequence":event["sequence"],
                    "ledger_head":event[self.domain.storage.event_hash_field.clone()],
                    "admitted_at":event["occurred_at"]
                }));
            }
        }
        let rejection_path = self
            .proposals(root)
            .join(format!("{}.rejection.json", safe_name(proposal_id)));
        if rejection_path.exists() {
            let rejection = self.read_json(&rejection_path)?;
            return Ok(json!({
                "status":"rejected",
                "rejected_at":rejection["occurred_at"],
                "reason":rejection["reason"]
            }));
        }
        let review_path = self
            .proposals(root)
            .join(format!("{}.review.json", safe_name(proposal_id)));
        if review_path.exists() {
            let review = self.read_json(&review_path)?;
            return Ok(json!({"status":"reviewed","review_status":review["status"]}));
        }
        Ok(json!({"status":"submitted"}))
    }

    fn proposal_review(&self, root: &Path, args: &Map<String, Value>) -> Result<Value, Value> {
        let id = self.required(args, "proposal_id")?;
        let proposal = self.load_proposal(root, &id)?;
        let operations = proposal["operations"].as_array().ok_or_else(|| {
            self.error(
                "proposal_corrupt",
                "proposal operations missing",
                json!({"proposal_id":id}),
            )
        })?;
        self.validate_operations(operations, true)?;
        self.validate_references(root, operations)?;
        let expected = proposal.get("expected_ledger_head").and_then(Value::as_str);
        let current = self.ledger_head(root)?;
        let head_matches = expected == current.as_deref();
        let review = json!({"schema":self.domain.features.proposals.review_schema_id,"proposal_id":id,"status":if head_matches{"policy_valid"}else{"stale"},"certifies_truth":self.domain.features.proposals.certifies_truth,"checks":{"schema":true,"references":true,"evidence_locations":true,"graph_invariants":true,"ledger_head":head_matches},"expected_ledger_head":expected,"actual_ledger_head":current});
        self.write_replace_json(
            &self.proposals(root).join(format!("{id}.review.json")),
            &review,
        )?;
        Ok(review)
    }

    fn proposal_admit(&self, root: &Path, args: &Map<String, Value>) -> Result<Value, Value> {
        self.prepare(root)?;
        self.with_authority_lock(root, "ledger", || self.proposal_admit_locked(root, args))
    }

    fn proposal_admit_locked(
        &self,
        root: &Path,
        args: &Map<String, Value>,
    ) -> Result<Value, Value> {
        self.prepare(root)?;
        let id = self.required(args, "proposal_id")?;
        let actor = self.required(args, "actor")?;
        let proposal = self.load_proposal(root, &id)?;
        let idem = args
            .get("idempotency_key")
            .and_then(Value::as_str)
            .filter(|value| !value.trim().is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| {
                self.derived_idempotency_key(
                    &self.domain.id_derivation.derived_idempotency_keys.admission,
                    &json!({"proposal_id":id,"proposal_digest":proposal["digest"]}),
                )
            });
        let idem_path = self
            .ledger(root)
            .join(format!("idem-{}.txt", safe_name(&idem)));
        if idem_path.exists() {
            let event_id = fs::read_to_string(&idem_path)
                .map_err(self.io_error("ledger_idempotency_read_failed"))?;
            let event =
                self.read_json(&self.ledger(root).join(format!("{}.json", event_id.trim())))?;
            if event.get("proposal_id") != Some(&json!(id))
                || event.get("proposal_digest") != proposal.get("digest")
            {
                return Err(self.error(
                    "admission_idempotency_conflict",
                    "idempotency key already names a different proposal admission",
                    json!({"idempotency_key":idem,"existing_event_id":event_id.trim()}),
                ));
            }
            return Ok(self.admission_receipt(&event));
        }
        if let Some(event) = self.find_ledger_event_by_idempotency(root, &idem)? {
            if event.get("proposal_id") != Some(&json!(id))
                || event.get("proposal_digest") != proposal.get("digest")
            {
                return Err(self.error(
                    "admission_idempotency_conflict",
                    "idempotency key already names a different proposal admission",
                    json!({"idempotency_key":idem,"existing_event_id":event["event_id"]}),
                ));
            }
            if !idem_path.exists() {
                self.write_new(&idem_path, event["event_id"].as_str().unwrap().as_bytes())?;
            }
            return Ok(self.admission_receipt(&event));
        }
        let review =
            self.proposal_review(root, &Map::from_iter([("proposal_id".into(), json!(id))]))?;
        if review["status"] != "policy_valid" {
            return Err(self.error(
                "proposal_not_admissible",
                "proposal review is not policy_valid",
                review,
            ));
        }
        let expected_value =
            self.resolve_expected_ledger_head(root, args.get("expected_ledger_head"))?;
        let expected = expected_value.as_str();
        let current = self.ledger_head(root)?;
        if expected != current.as_deref()
            || proposal.get("expected_ledger_head").and_then(Value::as_str) != current.as_deref()
        {
            return Err(self.error(
                "ledger_head_conflict",
                "expected ledger head does not match",
                json!({"expected":expected,"proposal_expected":proposal.get("expected_ledger_head"),"actual":current}),
            ));
        }
        let event_hash_field = self.domain.storage.event_hash_field.clone();
        let outcome = event_ledger::append_event(
            self.error,
            &self.ledger_layout(root),
            &event_hash_field,
            None,
            Some(&idem),
            |ctx| json!({"schema":self.domain.storage.event_schema_id,"sequence":ctx.sequence,"event_id":ctx.event_id,"event_kind":self.domain.features.proposals.event_kind,"previous_hash":ctx.previous_hash,"proposal_id":id,"proposal_digest":proposal["digest"],"operations":proposal["operations"],"actor":actor,"identity_state":Engine::identity_state_for_event(&actor, "ledger.proposal_submit"),"authority_basis":args.get("authority_basis"),"idempotency_key":idem,"occurred_at":now(),"certifies_truth":self.domain.features.proposals.certifies_truth}),
        )?;
        self.rebuild_projection(root)?;
        Ok(self.admission_receipt(&outcome.event))
    }

    fn admission_receipt(&self, event: &Value) -> Value {
        json!({
            "schema":self.domain.features.proposals.admission_receipt_schema_id,
            "status":"admitted",
            "proposal_id":event["proposal_id"],
            "proposal_digest":event["proposal_digest"],
            "event_id":event["event_id"],
            "sequence":event["sequence"],
            "operation_count":event["operations"].as_array().map_or(0, Vec::len),
            "ledger_head":event[self.domain.storage.event_hash_field.clone()],
            "certifies_truth":self.domain.features.proposals.certifies_truth
        })
    }

    fn proposal_reject(&self, root: &Path, args: &Map<String, Value>) -> Result<Value, Value> {
        let id = self.required(args, "proposal_id")?;
        let _ = self.load_proposal(root, &id)?;
        let rejection = json!({"schema":self.domain.features.proposals.rejection_schema_id,"proposal_id":id,"status":"rejected","actor":self.required(args,"actor")?,"reason":self.required(args,"reason")?,"occurred_at":now()});
        self.write_new_json(
            &self.proposals(root).join(format!("{id}.rejection.json")),
            &rejection,
        )?;
        Ok(rejection)
    }

}
