impl Engine {
    fn capture_sources(&self, root: &Path, args: &Map<String, Value>) -> Result<Value, Value> {
        self.prepare(root)?;
        let caps = &self.domain.caps.capture_sources;
        let sources = args
            .get("sources")
            .and_then(Value::as_array)
            .ok_or_else(|| {
                self.error("invalid_capture", "sources must be an array", Value::Null)
            })?;
        let supplied = args
            .get("operations")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        if (sources.len() as u64) < caps.sources_min {
            return Err(self.error(
                "invalid_capture",
                "at least one source is required",
                Value::Null,
            ));
        }
        let mut operations = Vec::with_capacity(sources.len() + supplied.len());
        for source in sources {
            let source = source.as_object().ok_or_else(|| {
                self.error(
                    "invalid_capture",
                    "each source must be an object",
                    Value::Null,
                )
            })?;
            operations.push(json!({
                "op":self.entity_op(),
                "entity_id":self.required(source, "source_id")?,
                "kind":"source",
                "title":self.required(source, "title")?,
                "version":self.required(source, "version")?,
                "locator":self.required(source, "locator")?
            }));
        }
        for operation in &supplied {
            if operation.get("kind").and_then(Value::as_str) == Some("source") {
                return Err(self.error(
                    "invalid_capture",
                    "declare sources through the sources field, not operations",
                    Value::Null,
                ));
            }
            operations.push(operation.clone());
        }
        if operations.len() as u64 > caps.combined_max {
            return Err(self.error(
                "invalid_capture",
                &format!("combined source and operation count exceeds {}", caps.combined_max),
                json!({"source_count":sources.len(),"operation_count":supplied.len(),"combined_count":operations.len()}),
            ));
        }
        let existing_identities = self.with_stable_projection(root, || {
            self.existing_operation_identities(root, &operations)
        })?;
        let mut proposal_args = args.clone();
        proposal_args.remove("sources");
        proposal_args.insert("operations".into(), json!(operations));
        let receipt = self.proposal_submit(root, &proposal_args)?;
        Ok(json!({
            "schema":self.domain.features.proposals.source_capture_schema_id,
            "status":"draft_submitted",
            "proposal_id":receipt["proposal_id"],
            "proposal_digest":receipt["proposal_digest"],
            "expected_ledger_head":receipt["expected_ledger_head"],
            "source_count":sources.len(),
            "operation_count":receipt["operation_count"],
            "existing_identity_count":existing_identities.len(),
            "existing_identities":existing_identities,
            "next":{"review":{"tool":self.tool_name("proposal_review"),"proposal_id":receipt["proposal_id"]}},
            "admission_requires_explicit_call":self.domain.features.proposals.capture_sources.admission_requires_explicit_call,
            "certifies_truth":self.domain.features.proposals.certifies_truth,
            "bounded":true
        }))
    }

    fn existing_operation_identities(
        &self,
        root: &Path,
        operations: &[Value],
    ) -> Result<Vec<Value>, Value> {
        let db = Connection::open(self.projection_path(root))
            .map_err(self.db_error("projection_open_failed"))?;
        let mut existing = Vec::new();
        for operation in operations {
            let Some(op_kind) = operation.get("op").and_then(Value::as_str) else {
                continue;
            };
            let Some(fold) = self
                .domain
                .projection
                .fold
                .iter()
                .find(|entry| entry.operation == op_kind)
            else {
                continue;
            };
            let Some(identity) = operation.get(&fold.key_field).and_then(Value::as_str) else {
                continue;
            };
            let table = self.table(&fold.table);
            let sql = format!(
                "select 1 from {} where {}=?1 limit 1",
                table.name, table.primary_key
            );
            let found = db
                .query_row(&sql, params![identity], |_| Ok(()))
                .optional()
                .map_err(self.db_error("projection_duplicate_check_failed"))?
                .is_some();
            if found {
                existing.push(json!({"op":operation["op"],"identity":identity}));
            }
        }
        Ok(existing)
    }

    fn proposal_read(&self, root: &Path, args: &Map<String, Value>) -> Result<Value, Value> {
        let id = self.required(args, "proposal_id")?;
        let proposal = self.load_proposal(root, &id)?;
        let operations = proposal["operations"].as_array().ok_or_else(|| {
            self.error(
                "proposal_corrupt",
                "proposal operations missing",
                json!({"proposal_id":id}),
            )
        })?;
        let offset = args.get("offset").and_then(Value::as_u64).unwrap_or(0) as usize;
        let caps = &self.domain.caps.proposal_read_limit;
        let limit = args
            .get("limit")
            .and_then(Value::as_u64)
            .unwrap_or(caps.default)
            .min(caps.max) as usize;
        let items = operations
            .iter()
            .skip(offset)
            .take(limit)
            .cloned()
            .collect::<Vec<_>>();
        let next_offset = (offset + items.len() < operations.len()).then_some(offset + items.len());
        let lifecycle = self.proposal_lifecycle(root, &id)?;
        Ok(json!({
            "schema":self.domain.features.proposals.read_schema_id,
            "status":lifecycle["status"],
            "lifecycle":lifecycle,
            "proposal_id":proposal["proposal_id"],
            "proposal_digest":proposal["digest"],
            "actor":proposal["actor"],
            "authority_basis":proposal["authority_basis"],
            "idempotency_key":proposal["idempotency_key"],
            "expected_ledger_head":proposal["expected_ledger_head"],
            "created_at":proposal["created_at"],
            "operation_count":operations.len(),
            "offset":offset,
            "limit":limit,
            "returned":items.len(),
            "operations":items,
            "has_more":next_offset.is_some(),
            "next_offset":next_offset,
            "bounded":true
        }))
    }

    fn operation_identity(&self, operation: &Value) -> Option<String> {
        let op_kind = operation.get("op").and_then(Value::as_str)?;
        let identity = self
            .domain
            .id_derivation
            .operation_identity_prefixes
            .get(op_kind)?;
        operation
            .get(&identity.id_field)
            .and_then(Value::as_str)
            .map(|value| format!("{}:{value}", identity.prefix))
    }

}
