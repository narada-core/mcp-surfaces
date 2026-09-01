impl Engine {
    fn issue_tree_transition(
        &self,
        root: &Path,
        args: &Map<String, Value>,
    ) -> Result<Value, Value> {
        let tree_id = args
            .get("tree_id")
            .and_then(Value::as_str)
            .filter(|v| !v.is_empty())
            .ok_or_else(|| self.error("issue_tree_invalid", "tree_id is required", Value::Null))?;
        if !args.contains_key("nodes") {
            let transition = args
                .get("transition")
                .and_then(Value::as_object)
                .ok_or_else(|| {
                    self.error(
                        "issue_tree_invalid",
                        "nodes or transition is required",
                        Value::Null,
                    )
                })?;
            let selected_id = args
                .get("selected_node_id")
                .and_then(Value::as_str)
                .filter(|v| !v.is_empty())
                .ok_or_else(|| {
                    self.error(
                        "issue_tree_invalid",
                        "selected_node_id is required for an ordinary transition",
                        Value::Null,
                    )
                })?;
            let expected_version = args
                .get("expected_node_version")
                .and_then(Value::as_u64)
                .ok_or_else(|| {
                    self.error(
                        "issue_tree_invalid",
                        "expected_node_version is required",
                        Value::Null,
                    )
                })?;
            let idempotency_key = args
                .get("idempotency_key")
                .and_then(Value::as_str)
                .filter(|v| !v.is_empty());
            if idempotency_key.is_none() {
                return Err(self.error(
                    "issue_tree_idempotency_required",
                    "idempotency_key is required for an ordinary transition",
                    json!({"retry_safe":true}),
                ));
            }
            let idempotency_key = idempotency_key.unwrap();
            let transition_fingerprint = self.digest_value(&Value::Object(args.clone()))?;
            let transition_receipt_path = self.proposals(root).join(format!(
                "issue-tree-transition-{}.json",
                safe_name(idempotency_key)
            ));
            if transition_receipt_path.exists() {
                let replay = self.read_json(&transition_receipt_path)?;
                if replay["content_fingerprint"].as_str() != Some(&transition_fingerprint) {
                    return Err(self.error(
                        "issue_tree_idempotency_conflict",
                        "idempotency key already names a different transition",
                        json!({"idempotency_key":idempotency_key}),
                    ));
                }
                return Ok(replay["receipt"].clone());
            }
            let (frontier, selected) = self.issue_tree_frontier_nodes(root, tree_id)?;
            let observed_ledger_head = self.ledger_head(root)?;
            let selected = selected.ok_or_else(|| {
                self.error(
                    "issue_tree_selected_missing",
                    "the tree has no selected leaf",
                    json!({"tree_id":tree_id}),
                )
            })?;
            if selected["node_id"].as_str() != Some(selected_id) {
                return Err(self.error("issue_tree_selected_conflict", "selected_node_id is stale", json!({"expected_selected_node_id":selected_id,"actual_selected_node_id":selected["node_id"],"next":{"tool":"epistemic_graph_issue_tree_resume","arguments":{"tree_id":tree_id}}})));
            }
            let actual_version = selected["version"]
                .as_str()
                .and_then(|v| v.parse::<u64>().ok())
                .unwrap_or(0);
            if actual_version != expected_version {
                return Err(self.error("issue_tree_version_conflict", "expected_node_version is stale", json!({"expected_node_version":expected_version,"actual_node_version":actual_version,"next":{"tool":"epistemic_graph_issue_tree_resume","arguments":{"tree_id":tree_id}}})));
            }
            let disposition = transition
                .get("disposition")
                .and_then(Value::as_str)
                .ok_or_else(|| {
                    self.error(
                        "issue_tree_invalid",
                        "transition.disposition is required",
                        Value::Null,
                    )
                })?;
            if ![
                "resolved",
                "rejected",
                "exhausted",
                "deferred",
                "superseded",
                "split",
            ]
            .contains(&disposition)
            {
                return Err(self.error(
                    "issue_tree_invalid",
                    "unsupported transition disposition",
                    json!({"disposition":disposition}),
                ));
            }
            let disposed_id = format!("{selected_id}:v{}:{disposition}", expected_version + 1);
            let mut nodes = vec![json!({
                "node_id":disposed_id,
                "title":selected["title"],
                "version":expected_version + 1,
                "state":"disposed",
                "disposition":disposition,
                "score":selected["score"],
                "predecessor_id":selected_id,
                "rationale":transition.get("rationale").cloned().unwrap_or(Value::Null),
                "evidence_ids":transition.get("evidence_ids").cloned().unwrap_or_else(|| json!([]))
            })];
            let successors = transition
                .get("successors")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            let select_next = args
                .get("select_next")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let selected_successor_index = select_next
                .then(|| {
                    successors
                        .iter()
                        .enumerate()
                        .filter(|(_, successor)| {
                            successor["state"].as_str().unwrap_or("open") == "open"
                        })
                        .max_by(|(left_index, left), (right_index, right)| {
                            left["score"]
                                .as_f64()
                                .unwrap_or(0.0)
                                .partial_cmp(&right["score"].as_f64().unwrap_or(0.0))
                                .unwrap_or(std::cmp::Ordering::Equal)
                                .then_with(|| {
                                    right["node_id"].as_str().cmp(&left["node_id"].as_str())
                                })
                                .then_with(|| right_index.cmp(left_index))
                        })
                        .map(|(index, _)| index)
                })
                .flatten();
            for (index, successor) in successors.iter().enumerate() {
                let object = successor.as_object().ok_or_else(|| {
                    self.error(
                        "issue_tree_invalid",
                        "each successor must be an object",
                        json!({"successor_index":index}),
                    )
                })?;
                let node_id = object
                    .get("node_id")
                    .and_then(Value::as_str)
                    .filter(|v| !v.is_empty())
                    .ok_or_else(|| {
                        self.error(
                            "issue_tree_invalid",
                            "successor node_id is required",
                            json!({"successor_index":index}),
                        )
                    })?;
                let title = object
                    .get("title")
                    .and_then(Value::as_str)
                    .filter(|v| !v.is_empty())
                    .ok_or_else(|| {
                        self.error(
                            "issue_tree_invalid",
                            "successor title is required",
                            json!({"successor_index":index}),
                        )
                    })?;
                let requested_state = object
                    .get("state")
                    .and_then(Value::as_str)
                    .unwrap_or("open");
                nodes.push(json!({
                    "node_id":node_id,
                    "title":title,
                    "version":1,
                    "state":if selected_successor_index == Some(index) {"selected"} else {requested_state},
                    "score":object.get("score").cloned().unwrap_or(json!(0.0)),
                    "parent_id":selected_id,
                    "rationale":object.get("rationale").cloned().unwrap_or(Value::Null),
                    "blocker_ids":object.get("blocker_ids").cloned().unwrap_or_else(|| json!([])),
                    "evidence_ids":object.get("evidence_ids").cloned().unwrap_or_else(|| json!([]))
                }));
            }
            if select_next && selected_successor_index.is_none() {
                if let Some(next) = frontier.into_iter().find(|node| {
                    node["node_id"].as_str() != Some(selected_id)
                        && node["state"].as_str() == Some("open")
                }) {
                    let next_id = next["node_id"].as_str().unwrap_or_default();
                    let next_version = next["version"]
                        .as_str()
                        .and_then(|v| v.parse::<u64>().ok())
                        .unwrap_or(1);
                    nodes.push(json!({
                        "node_id":format!("{next_id}:v{}:selected", next_version + 1),
                        "title":next["title"],
                        "version":next_version + 1,
                        "state":"selected",
                        "score":next["score"],
                        "predecessor_id":next_id
                    }));
                }
            }
            let mut expanded = args.clone();
            expanded.remove("transition");
            expanded.remove("selected_node_id");
            expanded.remove("expected_node_version");
            expanded.remove("select_next");
            expanded.insert("nodes".into(), Value::Array(nodes));
            if !expanded.contains_key("expected_ledger_head") {
                expanded.insert("expected_ledger_head".into(), json!(observed_ledger_head));
            }
            let mut receipt = match self.issue_tree_transition(root, &expanded) {
                Ok(receipt) => receipt,
                Err(error)
                    if error["code"].as_str() == Some("proposal_not_admissible")
                        && error
                            .pointer("/details/review/status")
                            .and_then(Value::as_str)
                            == Some("stale") =>
                {
                    let current = self.issue_tree_resume(root, &Map::from_iter([("tree_id".into(), json!(tree_id))]))?;
                    return Err(self.error(
                        "issue_tree_version_conflict",
                        "the selected leaf or ledger head changed before admission",
                        json!({
                            "tree_id":tree_id,
                            "requested":{"node_id":selected_id,"version":expected_version},
                            "current":{"node_id":current["selected"]["node_id"],"version":current["selected"]["version"],"event_id":current["selected"]["event_id"],"ledger_head":current["ledger_head"]},
                            "idempotency_key":args.get("idempotency_key").cloned().unwrap_or(Value::Null),
                            "idempotency_key_reserved":args.get("idempotency_key").is_some(),
                            "mutation_admitted":false,
                            "next":{"tool":"epistemic_graph_issue_tree_resume","arguments":{"tree_id":tree_id}},
                            "cause":error
                        }),
                    ));
                }
                Err(error) => return Err(error),
            };
            let resumed = self
                .issue_tree_resume(root, &Map::from_iter([("tree_id".into(), json!(tree_id))]))?;
            if let Some(object) = receipt.as_object_mut() {
                object.insert("workflow".into(), json!({
                    "schema":"narada.epistemic.issue_tree_transition_workflow.v1",
                    "prior_selected":{"node_id":selected_id,"version":expected_version},
                    "resulting_selected":resumed["selected"],
                    "disposition":disposition,
                    "resume":resumed["rehydrate_with"],
                    "certifies_truth":false,
                    "reconciliation":{"tool":"epistemic_graph_issue_tree_resume","arguments":{"tree_id":tree_id}}
                }));
            }
            self.write_new_json(
                &transition_receipt_path,
                &json!({
                    "schema":"narada.epistemic.issue_tree_transition_replay.v1",
                    "idempotency_key":idempotency_key,
                    "content_fingerprint":transition_fingerprint,
                    "receipt":receipt
                }),
            )?;
            return Ok(receipt);
        }
        let nodes = args
            .get("nodes")
            .and_then(Value::as_array)
            .filter(|v| !v.is_empty())
            .ok_or_else(|| {
                self.error(
                    "issue_tree_invalid",
                    "nodes must be a non-empty array",
                    Value::Null,
                )
            })?;
        let mut operations = Vec::new();
        let mut selected_successors = Vec::<String>::new();
        let mut superseded_nodes = HashSet::<String>::new();
        for (index, value) in nodes.iter().enumerate() {
            let node = value.as_object().ok_or_else(|| {
                self.error(
                    "issue_tree_invalid",
                    "each node must be an object",
                    json!({"node_index":index}),
                )
            })?;
            let node_id = node
                .get("node_id")
                .and_then(Value::as_str)
                .filter(|v| !v.is_empty())
                .ok_or_else(|| {
                    self.error(
                        "issue_tree_invalid",
                        "node_id is required",
                        json!({"node_index":index}),
                    )
                })?;
            let title = node
                .get("title")
                .and_then(Value::as_str)
                .filter(|v| !v.is_empty())
                .ok_or_else(|| {
                    self.error(
                        "issue_tree_invalid",
                        "title is required",
                        json!({"node_index":index}),
                    )
                })?;
            let version = node
                .get("version")
                .and_then(Value::as_u64)
                .filter(|v| *v > 0)
                .ok_or_else(|| {
                    self.error(
                        "issue_tree_invalid",
                        "version must be a positive integer",
                        json!({"node_index":index}),
                    )
                })?;
            let supplied_state = node.get("state").and_then(Value::as_str).unwrap_or("open");
            if !["active", "open", "selected", "blocked", "disposed"].contains(&supplied_state) {
                return Err(self.error(
                    "issue_tree_invalid",
                    "state must be open, selected, blocked, or disposed",
                    json!({"node_index":index,"state":supplied_state}),
                ));
            }
            let state = if supplied_state == "active" {
                "open"
            } else {
                supplied_state
            };
            let disposition = node.get("disposition").and_then(Value::as_str);
            if disposition.is_some_and(|v| {
                ![
                    "resolved",
                    "rejected",
                    "exhausted",
                    "deferred",
                    "superseded",
                    "split",
                ]
                .contains(&v)
            }) {
                return Err(self.error(
                    "issue_tree_invalid",
                    "unsupported disposition",
                    json!({"node_index":index,"disposition":disposition}),
                ));
            }
            if (state == "disposed") != disposition.is_some() {
                return Err(self.error(
                    "issue_tree_invalid",
                    "disposed nodes require a disposition and non-disposed nodes forbid one",
                    json!({"node_index":index}),
                ));
            }
            let predecessor = node.get("predecessor_id").and_then(Value::as_str);
            if (version == 1 && predecessor.is_some()) || (version > 1 && predecessor.is_none()) {
                return Err(self.error(
                    "issue_tree_invalid",
                    "version 1 forbids a predecessor; later versions require one",
                    json!({"node_index":index,"version":version}),
                ));
            }
            if state == "selected" {
                selected_successors.push(node_id.to_string());
            }
            if let Some(value) = predecessor {
                superseded_nodes.insert(value.to_string());
            }
            let blockers = node
                .get("blocker_ids")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            if state == "blocked" && blockers.is_empty() {
                return Err(self.error(
                    "issue_tree_invalid",
                    "blocked nodes require at least one blocker_id",
                    json!({"node_index":index}),
                ));
            }
            let score = node.get("score").and_then(Value::as_f64).unwrap_or(0.0);
            if !score.is_finite() || !(0.0..=1.0).contains(&score) {
                return Err(self.error(
                    "issue_tree_invalid",
                    "score must be finite and between 0 and 1",
                    json!({"node_index":index}),
                ));
            }
            let mut entity = Map::new();
            entity.insert("op".into(), json!("entity.declare"));
            entity.insert("entity_id".into(), json!(node_id));
            entity.insert("kind".into(), json!("research_issue"));
            entity.insert("title".into(), json!(title));
            entity.insert("version".into(), json!(version.to_string()));
            entity.insert("tree_id".into(), json!(tree_id));
            entity.insert("state".into(), json!(state));
            entity.insert("score".into(), json!(score));
            if let Some(value) = disposition {
                entity.insert("disposition".into(), json!(value));
            }
            if let Some(value) = node.get("rationale") {
                entity.insert("rationale".into(), value.clone());
            }
            operations.push(Value::Object(entity));
            let mut add_relation = |relation_type: &str, target_id: &str| {
                operations.push(json!({"op":"relation.declare","relation_type":relation_type,"source_id":node_id,"target_id":target_id}));
            };
            if let Some(value) = node.get("parent_id").and_then(Value::as_str) {
                add_relation("issue_child_of", value);
            }
            if let Some(value) = predecessor {
                add_relation("supersedes", value);
            }
            for blocker in blockers {
                let id = blocker.as_str().filter(|v| !v.is_empty()).ok_or_else(|| {
                    self.error(
                        "issue_tree_invalid",
                        "blocker_ids must contain non-empty strings",
                        json!({"node_index":index}),
                    )
                })?;
                add_relation("blocked_by", id);
            }
            for evidence in node
                .get("evidence_ids")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default()
            {
                let id = evidence.as_str().filter(|v| !v.is_empty()).ok_or_else(|| {
                    self.error(
                        "issue_tree_invalid",
                        "evidence_ids must contain non-empty strings",
                        json!({"node_index":index}),
                    )
                })?;
                add_relation("derived_from", id);
            }
        }
        if operations.len() as u64 > self.domain.caps.operations_per_proposal.max {
            return Err(self.error(
                "issue_tree_limit_exceeded",
                "expanded transition exceeds the proposal operation cap",
                json!({"expanded_operations":operations.len()}),
            ));
        }
        if selected_successors.len() > 1 {
            return Err(self.error(
                "issue_tree_selected_conflict",
                "one atomic transition cannot create more than one selected leaf",
                json!({"selected_node_ids":selected_successors}),
            ));
        }
        let (_, current_selected) = self.issue_tree_frontier_nodes(root, tree_id)?;
        if let Some(current) = current_selected {
            let current_id = current["node_id"].as_str().unwrap_or_default();
            if !selected_successors.is_empty() && !superseded_nodes.contains(current_id) {
                return Err(self.error("issue_tree_selected_conflict", "a new selected leaf must supersede the current selected leaf", json!({"current_selected_node_id":current_id,"proposed_selected_node_id":selected_successors.first()})));
            }
        }
        let mut proposal_args = Map::new();
        for field in [
            "actor",
            "authority_basis",
            "idempotency_key",
            "expected_ledger_head",
        ] {
            if let Some(value) = args.get(field) {
                proposal_args.insert(field.into(), value.clone());
            }
        }
        proposal_args.insert("operations".into(), Value::Array(operations.clone()));
        let mut receipt = self.submit_review_admit(root, &proposal_args)?;
        if let Some(object) = receipt.as_object_mut() {
            object.insert(
                "issue_tree_transition".into(),
                json!({
                    "schema":"narada.epistemic.issue_tree_transition.v1",
                    "tree_id":tree_id,
                    "node_count":nodes.len(),
                    "expanded_operation_count":operations.len(),
                    "atomic":true,
                    "evidence_promotion":false
                }),
            );
        }
        Ok(receipt)
    }

}
