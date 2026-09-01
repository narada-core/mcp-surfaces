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
        let mut candidates = if let Some(id) = supplied_tree_id {
            let identified = trees.iter().find(|tree| tree["tree_id"].as_str() == Some(id)).cloned();
            if let (Some(tree), Some(supplied_objective)) = (identified.as_ref(), objective) {
                if !tree["objective"].as_str().is_some_and(|stored| stored.trim().eq_ignore_ascii_case(supplied_objective.trim())) {
                    return Err(self.error("issue_tree_objective_mismatch", "tree_id exists but its objective does not match the supplied objective hint", json!({"tree_id":id,"supplied_objective":supplied_objective,"stored_objective":tree["objective"],"mutation_performed":false})));
                }
            }
            identified.into_iter().collect::<Vec<_>>()
        } else {
            trees.iter().filter(|tree| normalized.as_ref().map(|value| tree["objective"].as_str().map(|stored| stored.trim().to_lowercase()).as_ref() == Some(value)).unwrap_or(true)).cloned().collect::<Vec<_>>()
        };
        if candidates.len() > 1 {
            return Err(self.error(
                "issue_tree_objective_ambiguous",
                "objective resolves to more than one tree",
                json!({"candidates":candidates,"mutation_performed":false}),
            ));
        }
        if candidates.is_empty() {
            if !args.get("create_if_missing").and_then(Value::as_bool).unwrap_or(false) {
                if supplied_tree_id.is_some() {
                    let objective_hint_matches = normalized.as_ref().map(|needle| trees.iter().filter(|tree| tree["objective"].as_str().is_some_and(|stored| stored.trim().eq_ignore_ascii_case(needle))).map(|tree| tree["tree_id"].clone()).collect::<Vec<_>>()).unwrap_or_default();
                    return Err(self.error("issue_tree_id_not_found", "the supplied tree_id does not exist; objective was evaluated only as a separate hint", json!({"tree_id":supplied_tree_id,"objective_hint":objective,"objective_hint_matches":objective_hint_matches,"mutation_performed":false})));
                }
                return Err(self.error("issue_tree_objective_not_found", "no issue tree objective matched exactly", json!({"objective":objective,"mutation_performed":false})));
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
        let objective_match = objective.map(|supplied| json!({
            "supplied":supplied,
            "stored":tree["objective"],
            "exact_normalized_match":tree["objective"].as_str().is_some_and(|stored| stored.trim().eq_ignore_ascii_case(supplied.trim())),
            "lookup_effect":if supplied_tree_id.is_some() {"hint_only_tree_id_was_authoritative"} else {"objective_was_lookup_key"}
        })).unwrap_or(Value::Null);
        let tree_id = tree["tree_id"].as_str().unwrap_or_default();
        let limit = args.get("max_frontier_items").and_then(Value::as_u64).unwrap_or(20).clamp(1, 100) as usize;
        let (mut alternatives, selected) = self.issue_tree_frontier_nodes(root, tree_id)?;
        let selected_id = selected.as_ref().and_then(|value| value["node_id"].as_str());
        alternatives.retain(|item| item["node_id"].as_str() != selected_id);
        let ledger_head = self.ledger_head(root)?;
        let capture_seed = serde_json::to_vec(&json!({"tree_id":tree_id,"ledger_head":ledger_head,"items":alternatives,"scope":"resume_alternatives"})).unwrap_or_default();
        let result_ref = format!("issue-tree-frontier:{}", &sha256(&capture_seed)[..24]);
        let capture = json!({
            "schema":"narada.epistemic.issue_tree_frontier_capture.v1","result_ref":result_ref,"tree_id":tree_id,
            "ledger_head":ledger_head,"captured_at_event":self.ledger_files(root)?.last().and_then(|path| self.read_json(path).ok()).and_then(|event| event.get("event_id").cloned()),
            "selected":selected,"items":alternatives,"scope":"unselected alternatives; selected work is represented once in selected"
        });
        let capture_path = self.issue_tree_capture_path(root, &result_ref);
        if let Some(parent) = capture_path.parent() { fs::create_dir_all(parent).map_err(self.io_error("issue_tree_capture_create_failed"))?; }
        fs::write(&capture_path, serde_json::to_vec_pretty(&capture).unwrap_or_default()).map_err(self.io_error("issue_tree_capture_write_failed"))?;
        let frontier = self.issue_tree_frontier_page(root, &capture, 0, limit)?;
        let inline_budget = args.get("max_inline_chars").and_then(Value::as_u64).unwrap_or(6000).clamp(1000, 20000) as usize;
        let mut resume_frontier = frontier["frontier"].clone();
        resume_frontier["scope"] = capture["scope"].clone();
        let mut response = json!({
            "schema":"narada.epistemic.issue-tree.resume.v1",
            "status":"ok",
            "tree":tree,
            "objective_match":objective_match,
            "selected":frontier["selected"],
            "frontier":resume_frontier,
            "continuation":frontier["continuation"],
            "result_ref":frontier["result_ref"],
            "ledger_head":frontier["ledger_head"],
            "rehydrate_with":{"tool":"epistemic_graph_issue_tree_resume","arguments":{"tree_id":tree["tree_id"]}},
            "certifies_truth":false,
            "noncertification":"coordination state; not evidence"
        });
        if args.get("compact").and_then(Value::as_bool).unwrap_or(false) {
            response["tree"] = json!({"tree_id":tree["tree_id"],"objective":tree["objective"],"version":tree["version"]});
            let selected = &frontier["selected"];
            response["selected"] = json!({"node_id":selected["node_id"],"version":selected["version"],"state":selected["state"],"event_id":selected["event_id"]});
            if let Some(items) = response["frontier"]["items"].as_array_mut() {
                for item in items { *item = json!({"node_id":item["node_id"],"version":item["version"],"state":item["state"],"score":item["score"],"event_id":item["event_id"]}); }
            }
            response["compact"] = json!(true);
        }
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
            response["continuation"] = json!({"tool":"epistemic_graph_issue_tree_frontier_read","arguments":{"result_ref":response["result_ref"],"offset":returned,"limit":limit}});
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
        let mut operations = self.normalize_operations(supplied_operations)?;
        for operation in &mut operations {
            let Some(object) = operation.as_object_mut() else {
                continue;
            };
            if object.get("op").and_then(Value::as_str) != Some("entity.declare")
                || object.get("kind").and_then(Value::as_str)
                    != Some("narada.epistemic:communication")
                || object.contains_key("sender_identity_state")
            {
                continue;
            }
            if let Some(sender) = object
                .get("sender")
                .and_then(Value::as_str)
                .map(str::to_string)
            {
                object.insert(
                    "sender_identity_state".into(),
                    Self::sender_identity_state(&actor, &sender),
                );
            }
        }
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
