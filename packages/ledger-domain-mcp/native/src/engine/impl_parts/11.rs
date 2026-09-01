impl Engine {
    fn submit_review_admit(&self, root: &Path, args: &Map<String, Value>) -> Result<Value, Value> {
        let proposals_feature = &self.domain.features.proposals;
        let submission = self.proposal_submit(root, args)?;
        let proposal_id = submission["proposal_id"].as_str().ok_or_else(|| {
            self.error(
                "proposal_submission_corrupt",
                "proposal id missing",
                submission.clone(),
            )
        })?;
        let lifecycle = self.proposal_lifecycle(root, proposal_id)?;
        if lifecycle["status"] == "admitted" {
            let review = self.read_json(
                &self
                    .proposals(root)
                    .join(format!("{}.review.json", safe_name(proposal_id))),
            )?;
            return Ok(json!({
                "schema":proposals_feature.compound_schema_id,
                "status":"already_admitted",
                "submission":submission,
                "review":review,
                "admission":lifecycle,
                "review_gate_preserved":proposals_feature.review_gate_preserved,
                "certifies_truth":proposals_feature.certifies_truth
            }));
        }
        let review = self.proposal_review(
            root,
            &Map::from_iter([("proposal_id".into(), json!(proposal_id))]),
        )?;
        if review["status"] != "policy_valid" {
            return Err(self.error(
                "proposal_not_admissible",
                "compound contribution stopped at the preserved review gate",
                json!({"submission":submission,"review":review}),
            ));
        }
        let admission_idempotency = self.derived_idempotency_key(
            &self.domain.id_derivation.derived_idempotency_keys.admission,
            &json!({"proposal_id":proposal_id,"proposal_digest":submission["proposal_digest"]}),
        );
        let admission = self.proposal_admit(
            root,
            &Map::from_iter([
                ("proposal_id".into(), json!(proposal_id)),
                ("actor".into(), json!(self.required(args, "actor")?)),
                (
                    "authority_basis".into(),
                    args.get("authority_basis").cloned().unwrap_or(Value::Null),
                ),
                (
                    "expected_ledger_head".into(),
                    submission["expected_ledger_head"].clone(),
                ),
                ("idempotency_key".into(), json!(admission_idempotency)),
            ]),
        )?;
        Ok(json!({
            "schema":proposals_feature.compound_schema_id,
            "status":"admitted",
            "submission":submission,
            "review":review,
            "admission":admission,
            "review_gate_preserved":proposals_feature.review_gate_preserved,
            "certifies_truth":proposals_feature.certifies_truth
        }))
    }

    fn normalize_operations(&self, operations: &[Value]) -> Result<Vec<Value>, Value> {
        let entity_op = self.domain.id_derivation.entity.applies_to.clone();
        let relation_op = self.domain.id_derivation.relation.applies_to.clone();
        let wiring = &self.domain.id_derivation.local_ref_wiring;
        let entity_key_field = self
            .domain
            .projection
            .fold
            .iter()
            .find(|entry| entry.operation == entity_op)
            .map(|entry| entry.key_field.clone())
            .expect("entity fold entry validated at load");
        let mut local_ids = std::collections::HashMap::new();
        let mut first_pass = Vec::with_capacity(operations.len());
        for operation in operations {
            let mut normalized = operation.clone();
            if operation.get("op").and_then(Value::as_str) == Some(entity_op.as_str()) {
                let object = normalized.as_object_mut().unwrap();
                if object
                    .get(&entity_key_field)
                    .and_then(Value::as_str)
                    .is_none()
                {
                    let kind = object.get("kind").and_then(Value::as_str).unwrap_or("");
                    let title = object.get("title").and_then(Value::as_str).unwrap_or("");
                    if !kind.is_empty() && !title.is_empty() {
                        let recipe = &self.domain.id_derivation.entity;
                        let mut digest_input = Map::new();
                        for field in &recipe.digest_input_fields {
                            digest_input.insert(
                                field.clone(),
                                object.get(field).cloned().unwrap_or(Value::Null),
                            );
                        }
                        let digest = self.digest_value(&Value::Object(digest_input))?;
                        object.insert(
                            entity_key_field.clone(),
                            json!(format!(
                                "{}:{}",
                                safe_name(kind),
                                &digest[..template_truncation(&recipe.template, 20)]
                            )),
                        );
                    }
                }
                if let (Some(local_ref), Some(entity_id)) = (
                    object.get(&wiring.declare_field).and_then(Value::as_str),
                    object.get(&entity_key_field).and_then(Value::as_str),
                ) {
                    if local_ids
                        .insert(local_ref.to_string(), entity_id.to_string())
                        .is_some()
                    {
                        return Err(self.error(
                            &wiring.duplicate_refusal_code,
                            "entity local_ref must be unique within a proposal",
                            json!({"local_ref":local_ref}),
                        ));
                    }
                }
            }
            first_pass.push(normalized);
        }
        first_pass
            .iter()
            .map(|operation| {
                let mut normalized = operation.clone();
                if operation.get("op").and_then(Value::as_str) == Some(relation_op.as_str()) {
                    let object = normalized.as_object_mut().unwrap();
                    for (ref_field, id_field) in &wiring.reference_fields {
                        if object.get(id_field).and_then(Value::as_str).is_none() {
                            if let Some(reference) = object.get(ref_field).and_then(Value::as_str) {
                                let resolved = local_ids.get(reference).ok_or_else(|| self.error(&wiring.unresolved_refusal_code, "relation reference does not identify an entity in this proposal", json!({"field":ref_field,"local_ref":reference})))?;
                                object.insert(id_field.clone(), json!(resolved));
                            }
                        }
                    }
                }
                let relation_key_field = self
                    .domain
                    .projection
                    .fold
                    .iter()
                    .find(|entry| entry.operation == relation_op)
                    .map(|entry| entry.key_field.clone())
                    .expect("relation fold entry validated at load");
                if normalized.get("op").and_then(Value::as_str) == Some(relation_op.as_str())
                    && normalized
                        .get(&relation_key_field)
                        .and_then(Value::as_str)
                        .is_none()
                {
                    let relation_type = normalized
                        .get("relation_type")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string();
                    let source_id = normalized
                        .get("source_id")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string();
                    let target_id = normalized
                        .get("target_id")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string();
                    if relation_type.is_empty() || source_id.is_empty() || target_id.is_empty() {
                        return Ok(normalized);
                    }
                    let recipe = &self.domain.id_derivation.relation;
                    let mut hash_input = Vec::new();
                    for (index, segment) in recipe.hash_input.split("\\0").enumerate() {
                        if index > 0 {
                            hash_input.push(0_u8);
                        }
                        let field = segment
                            .trim_start_matches('{')
                            .trim_end_matches('}')
                            .to_string();
                        let value = match field.as_str() {
                            "relation_type" => relation_type.clone(),
                            "source_id" => source_id.clone(),
                            "target_id" => target_id.clone(),
                            _ => normalized
                                .get(&field)
                                .and_then(Value::as_str)
                                .unwrap_or("")
                                .to_string(),
                        };
                        hash_input.extend_from_slice(value.as_bytes());
                    }
                    let digest = sha256(&hash_input);
                    normalized.as_object_mut().unwrap().insert(
                        relation_key_field,
                        json!(format!(
                            "{}{}-{}",
                            template_prefix(&recipe.template),
                            safe_name(&relation_type),
                            &digest[..template_truncation(&recipe.template, 16)]
                        )),
                    );
                }
                Ok(normalized)
            })
            .collect()
    }

    fn resolve_expected_ledger_head(
        &self,
        root: &Path,
        supplied: Option<&Value>,
    ) -> Result<Value, Value> {
        if supplied.is_none() || supplied.and_then(Value::as_str) == Some("latest") {
            return Ok(self
                .ledger_head(root)?
                .map(Value::String)
                .unwrap_or(Value::Null));
        }
        Ok(supplied.cloned().unwrap_or(Value::Null))
    }

    fn derived_idempotency_key(&self, recipe: &DerivedKeyRecipe, source: &Value) -> String {
        let mut object = Map::new();
        for field in &recipe.input_fields {
            object.insert(
                field.clone(),
                source.get(field).cloned().unwrap_or(Value::Null),
            );
        }
        let canonical = serde_json::to_vec(&Value::Object(object)).unwrap_or_default();
        format!(
            "{}{}",
            template_prefix(&recipe.template),
            &sha256(&canonical)[..template_truncation(&recipe.template, 24)]
        )
    }

    fn proposal_receipt(&self, proposal: &Value) -> Value {
        json!({
            "schema":self.domain.features.proposals.submission_receipt_schema_id,
            "status":proposal["status"],
            "proposal_id":proposal["proposal_id"],
            "proposal_digest":proposal["digest"],
            "content_fingerprint":proposal["content_fingerprint"],
            "operation_count":proposal["operations"].as_array().map_or(0, Vec::len),
            "expected_ledger_head":proposal["expected_ledger_head"],
            "created_at":proposal["created_at"]
        })
    }

}
