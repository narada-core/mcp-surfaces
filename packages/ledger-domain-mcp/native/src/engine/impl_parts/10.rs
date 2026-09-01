impl Engine {
    fn issue_tree_resume(&self, root: &Path, args: &Map<String, Value>) -> Result<Value, Value> {
        self.rebuild_projection(root)?;
        let supplied_tree_id = args
            .get("tree_id")
            .and_then(Value::as_str)
            .filter(|v| !v.is_empty());
        let objective = args
            .get("objective")
            .and_then(Value::as_str)
            .filter(|v| !v.trim().is_empty());
        if supplied_tree_id.is_none() && objective.is_none() {
            return Err(self.error(
                "issue_tree_resume_invalid",
                "tree_id or objective is required",
                Value::Null,
            ));
        }
        let db = Connection::open(self.projection_path(root))
            .map_err(self.db_error("projection_open_failed"))?;
        let mut trees = Vec::new();
        let mut statement = db
            .prepare(&format!(
                "select entity_id,payload_json,event_id from {} where kind='research_issue_tree'",
                self.entity_table
            ))
            .map_err(self.db_error("issue_tree_resume_prepare_failed"))?;
        for row in statement.query_map([], |row| {
            let payload = serde_json::from_str::<Value>(&row.get::<_, String>(1)?).unwrap_or(Value::Null);
            Ok(json!({"tree_id":row.get::<_,String>(0)?,"objective":payload["objective"],"version":payload["version"],"event_id":row.get::<_,String>(2)?}))
        }).map_err(self.db_error("issue_tree_resume_failed"))? {
            trees.push(row.map_err(self.db_error("issue_tree_resume_row_failed"))?);
        }
        let normalized = objective.map(|value| value.trim().to_lowercase());
        let mut candidates = trees
            .into_iter()
            .filter(|tree| {
                supplied_tree_id
                    .map(|id| tree["tree_id"].as_str() == Some(id))
                    .unwrap_or(true)
                    && normalized
                        .as_ref()
                        .map(|value| {
                            tree["objective"].as_str().map(str::to_lowercase).as_ref()
                                == Some(value)
                        })
                        .unwrap_or(true)
            })
            .collect::<Vec<_>>();
        if candidates.len() > 1 {
            return Err(self.error(
                "issue_tree_objective_ambiguous",
                "objective resolves to more than one tree",
                json!({"candidates":candidates,"mutation_performed":false}),
            ));
        }
        if candidates.is_empty() {
            if !args
                .get("create_if_missing")
                .and_then(Value::as_bool)
                .unwrap_or(false)
            {
                return Err(self.error("issue_tree_not_found", "no matching issue tree exists", json!({"tree_id":supplied_tree_id,"objective":objective,"mutation_performed":false})));
            }
            let objective = objective.ok_or_else(|| {
                self.error(
                    "issue_tree_creation_requires_objective",
                    "objective is required when creating a tree",
                    Value::Null,
                )
            })?;
            let actor = args.get("actor").cloned().ok_or_else(|| {
                self.error(
                    "issue_tree_creation_authority_required",
                    "actor is required for creation",
                    Value::Null,
                )
            })?;
            let authority = args.get("authority_basis").cloned().ok_or_else(|| {
                self.error(
                    "issue_tree_creation_authority_required",
                    "authority_basis is required for creation",
                    Value::Null,
                )
            })?;
            let tree_id = supplied_tree_id.map(str::to_string).unwrap_or_else(|| {
                format!(
                    "issue-tree:{}",
                    &sha256(objective.trim().to_lowercase().as_bytes())[..24]
                )
            });
            let root_id = format!("{tree_id}:root:v1");
            let mut create_args = Map::from_iter([
                ("actor".into(), actor),
                ("authority_basis".into(), authority),
                (
                    "operations".into(),
                    json!([
                        {"op":"entity.declare","entity_id":tree_id,"kind":"research_issue_tree","title":objective,"objective":objective,"version":"1"},
                        {"op":"entity.declare","entity_id":root_id,"kind":"research_issue","title":objective,"tree_id":tree_id,"version":"1","state":"selected","score":1.0}
                    ]),
                ),
            ]);
            if let Some(value) = args.get("idempotency_key") {
                create_args.insert("idempotency_key".into(), value.clone());
            }
            self.submit_review_admit(root, &create_args)?;
            candidates.push(json!({"tree_id":tree_id,"objective":objective,"version":"1"}));
        }
        let tree = candidates.remove(0);
        let mut frontier_args = Map::from_iter([("tree_id".into(), tree["tree_id"].clone())]);
        if let Some(value) = args.get("max_frontier_items") {
            frontier_args.insert("limit".into(), value.clone());
        }
        let frontier = self.issue_tree_frontier(root, &frontier_args)?;
        let inline_budget = args
            .get("max_inline_chars")
            .and_then(Value::as_u64)
            .unwrap_or(6000)
            .clamp(1000, 20000) as usize;
        let selected_id = frontier["selected"]["node_id"].as_str().map(str::to_string);
        let mut resume_frontier = frontier["frontier"].clone();
        let selected_was_in_page = resume_frontier["items"].as_array_mut().map(|items| {
            let before = items.len();
            items.retain(|item| item["node_id"].as_str() != selected_id.as_deref());
            before != items.len()
        }).unwrap_or(false);
        if selected_was_in_page {
            let returned = resume_frontier["items"].as_array().map(Vec::len).unwrap_or(0);
            resume_frontier["returned"] = json!(returned);
            if let Some(total) = resume_frontier["total"].as_u64() {
                resume_frontier["total"] = json!(total.saturating_sub(1));
            }
        }
        resume_frontier["scope"] = json!("unselected alternatives; selected work is represented once in selected");
        let mut response = json!({
            "schema":"narada.epistemic.issue-tree.resume.v1",
            "status":"ok",
            "tree":tree,
            "selected":frontier["selected"],
            "frontier":resume_frontier,
            "continuation":frontier["continuation"],
            "result_ref":frontier["result_ref"],
            "ledger_head":frontier["ledger_head"],
            "rehydrate_with":{"tool":"epistemic_graph_issue_tree_resume","arguments":{"tree_id":tree["tree_id"]}},
            "certifies_truth":false,
            "noncertification":"coordination state; not evidence"
        });
        while serde_json::to_string(&response)
            .map(|value| value.len())
            .unwrap_or(0)
            > inline_budget
        {
            let Some(items) = response["frontier"]["items"].as_array_mut() else {
                break;
            };
            if items.is_empty() {
                break;
            }
            items.pop();
            let returned = items.len();
            response["frontier"]["returned"] = json!(returned);
            response["frontier"]["complete"] = json!(false);
            let capture_offset = returned + usize::from(selected_was_in_page);
            response["continuation"] = json!({"tool":"epistemic_graph_issue_tree_frontier_read","arguments":{"result_ref":response["result_ref"],"offset":capture_offset,"limit":args.get("max_frontier_items").and_then(Value::as_u64).unwrap_or(20)}});
        }
        response["inline_budget_chars"] = json!(inline_budget);
        response["inline_chars"] = json!(serde_json::to_string(&response)
            .map(|value| value.len())
            .unwrap_or(0));
        Ok(response)
    }

    fn proposal_submit(&self, root: &Path, args: &Map<String, Value>) -> Result<Value, Value> {
        self.prepare(root)?;
        let actor = self.required(args, "actor")?;
        let supplied_operations = args
            .get("operations")
            .and_then(Value::as_array)
            .ok_or_else(|| {
                self.error(
                    "invalid_proposal",
                    "operations must be an array",
                    Value::Null,
                )
            })?;
        let count = &self.domain.caps.operations_per_proposal;
        if supplied_operations.len() < count.min as usize
            || supplied_operations.len() > count.max as usize
        {
            return Err(self.error(
                "invalid_proposal",
                &format!(
                    "operations count must be between {} and {}",
                    count.min, count.max
                ),
                json!({"count":supplied_operations.len()}),
            ));
        }
        let operations = self.normalize_operations(supplied_operations)?;
        self.validate_operations(&operations, false)?;
        let expected = self.resolve_expected_ledger_head(root, args.get("expected_ledger_head"))?;
        let semantic_content = json!({"actor":actor,"authority_basis":args.get("authority_basis"),"operations":operations});
        let content_fingerprint = self.digest_value(&semantic_content)?;
        let idempotency_key = args
            .get("idempotency_key")
            .and_then(Value::as_str)
            .filter(|value| !value.trim().is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| {
                self.derived_idempotency_key(
                    &self.domain.id_derivation.derived_idempotency_keys.proposal,
                    &semantic_content,
                )
            });
        let proposal_id = format!(
            "{}{}",
            template_prefix(&self.domain.id_derivation.generated_ids.proposal_id),
            Uuid::new_v4()
        );
        let created_at = now();
        let proposals_feature = &self.domain.features.proposals;
        let payload = json!({
            "schema":proposals_feature.proposal_schema_id, "proposal_id":proposal_id,
            "status":"submitted", "actor":actor, "authority_basis":args.get("authority_basis"),
            "idempotency_key":idempotency_key, "expected_ledger_head":expected,
            "created_at":created_at, "content_fingerprint":content_fingerprint, "operations":operations
        });
        let digest = self.digest_value(&payload)?;
        let mut stored = payload;
        stored
            .as_object_mut()
            .unwrap()
            .insert("digest".into(), json!(digest));
        let idem_path = self
            .proposals(root)
            .join(format!("idem-{}.txt", safe_name(&idempotency_key)));
        if idem_path.exists() {
            let existing = fs::read_to_string(&idem_path)
                .map_err(self.io_error("proposal_idempotency_read_failed"))?;
            let stored = self.read_json(
                &self
                    .proposals(root)
                    .join(format!("{}.json", existing.trim())),
            )?;
            if stored
                .get("content_fingerprint")
                .and_then(Value::as_str)
                .is_some()
                && stored.get("content_fingerprint") != Some(&json!(content_fingerprint))
            {
                return Err(self.error(
                    "proposal_idempotency_conflict",
                    "idempotency key already names different proposal content",
                    json!({"idempotency_key":idempotency_key,"existing_proposal_id":stored["proposal_id"]}),
                ));
            }
            return Ok(self.proposal_receipt(&stored));
        }
        self.write_new_json(
            &self.proposals(root).join(format!("{proposal_id}.json")),
            &stored,
        )?;
        self.write_new(&idem_path, proposal_id.as_bytes())?;
        Ok(self.proposal_receipt(&stored))
    }

}
