impl Engine {

    fn identity_state_for_event(actor: &str, operation: &str) -> Value {
        json!({
            "schema":"narada.agent.identity_state.v1",
            "claimed_identity":{"identity":actor,"status":"claimed","source":"ledger_actor","asserted_at":now(),"evidence_refs":[],"authority_granted":false},
            "authentication":{"status":"missing","authenticated_identity":null,"evidence_refs":[]},
            "authority":{"status":"not_evaluated","operation":operation,"granted":false,"evidence_refs":[]}
        })
    }

    pub fn new(domain: Descriptor) -> Result<Engine, String> {
        let tables = parse_ddl_tables(&domain.projection.ddl)?;
        let entity_op = &domain.id_derivation.entity.applies_to;
        let relation_op = &domain.id_derivation.relation.applies_to;
        let fold_table = |operation: &str| {
            domain
                .projection
                .fold
                .iter()
                .find(|entry| entry.operation == operation)
                .map(|entry| entry.table.clone())
                .ok_or_else(|| format!("domain_invalid:projection_fold_missing:{operation}"))
        };
        let entity_table = fold_table(entity_op)?;
        let relation_table = fold_table(relation_op)?;
        let records_table = domain
            .projection
            .fold
            .iter()
            .find(|entry| entry.table != entity_table && entry.table != relation_table)
            .map(|entry| entry.table.clone())
            .ok_or_else(|| "domain_invalid:projection_fold_missing_records".to_string())?;
        let datoms_table = tables
            .iter()
            .find(|spec| spec.name == "datoms")
            .map(|spec| spec.name.clone());
        let projection_meta_table = tables
            .iter()
            .find(|spec| spec.name == "projection_meta")
            .map(|spec| spec.name.clone());
        for table in [&entity_table, &relation_table, &records_table] {
            if !tables.iter().any(|spec| &spec.name == table) {
                return Err(format!(
                    "domain_invalid:projection_fold_unknown_table:{table}"
                ));
            }
        }
        let error_schema: &'static str =
            Box::leak(domain.identity.error_schema_id.clone().into_boxed_str());
        let event_hash_field: &'static str =
            Box::leak(domain.storage.event_hash_field.clone().into_boxed_str());
        Ok(Engine {
            domain,
            error: ErrorSchema(error_schema),
            event_hash_field,
            tables,
            entity_table,
            relation_table,
            records_table,
            datoms_table,
            projection_meta_table,
        })
    }

    fn table(&self, name: &str) -> &TableSpec {
        self.tables
            .iter()
            .find(|spec| spec.name == name)
            .expect("fold tables are validated at load")
    }

    fn entity_op(&self) -> &str {
        &self.domain.id_derivation.entity.applies_to
    }

    fn relation_op(&self) -> &str {
        &self.domain.id_derivation.relation.applies_to
    }

    #[cfg(test)]
    fn max_operations(&self) -> usize {
        self.domain.caps.operations_per_proposal.max as usize
    }

    /// Schema id derived from the domain namespace: `<namespace>.<name>`.
    fn schema_id(&self, name: &str) -> String {
        format!("{}.{}", self.domain.identity.schema_namespace, name)
    }

    fn finalize_bounded_output(&self, response: &mut Value) -> Result<u64, Value> {
        let max_output_bytes = self.domain.caps.query_execution.max_output_bytes;
        let mut output_bytes = 0u64;
        for _ in 0..4 {
            response["output_bytes"] = json!(output_bytes);
            output_bytes = serde_json::to_vec(response)
                .map_err(|_| {
                    self.error(
                        "query_output_limit",
                        "query response could not be serialized",
                        Value::Null,
                    )
                })?
                .len() as u64;
        }
        response["output_bytes"] = json!(output_bytes);
        let actual_output_bytes = serde_json::to_vec(response)
            .map_err(|_| {
                self.error(
                    "query_output_limit",
                    "query response could not be serialized",
                    Value::Null,
                )
            })?
            .len() as u64;
        if actual_output_bytes > max_output_bytes {
            return Err(self.error(
                "query_output_limit",
                "query response exceeded the descriptor output-byte budget",
                json!({"output_bytes":actual_output_bytes,"max_output_bytes":max_output_bytes}),
            ));
        }
        if actual_output_bytes != output_bytes {
            response["output_bytes"] = json!(actual_output_bytes);
        }
        Ok(actual_output_bytes)
    }

    /// Tool name derived from the tool prefix: `<prefix>_<verb>`.
    fn tool_name(&self, verb: &str) -> String {
        format!("{}_{}", self.domain.identity.tool_prefix, verb)
    }

    fn expand_kind_values(&self, kinds: Vec<Value>) -> Result<Vec<Value>, Value> {
        let mut expanded = Vec::new();
        for kind in kinds {
            if !expanded.iter().any(|candidate| candidate == &kind) {
                expanded.push(kind);
            }
        }
        for (canonical, aliases) in &self.domain.query.kind_aliases {
            let matched = expanded.iter().any(|kind| {
                kind.as_str() == Some(canonical.as_str())
                    || aliases
                        .iter()
                        .any(|alias| kind.as_str() == Some(alias.as_str()))
            });
            if !matched {
                continue;
            }
            let canonical = Value::String(canonical.clone());
            if !expanded.iter().any(|kind| kind == &canonical) {
                expanded.push(canonical);
            }
            for alias in aliases {
                let alias = Value::String(alias.clone());
                if !expanded.iter().any(|kind| kind == &alias) {
                    expanded.push(alias);
                }
            }
        }
        let max_values = self.domain.query.max_one_of_values.unwrap_or(64).max(1);
        if expanded.len() > max_values {
            return Err(self.error(
                "query_kind_limit",
                "named kind aliases expand beyond the descriptor one_of budget",
                json!({"count":expanded.len(),"max":max_values}),
            ));
        }
        Ok(expanded)
    }

    fn expand_legacy_kind_value(&self, kind: &str) -> Result<Vec<Value>, Value> {
        let mut expanded = vec![Value::String(kind.to_string())];
        for (canonical, aliases) in &self.domain.query.kind_aliases {
            if canonical == kind || aliases.iter().any(|alias| alias == kind) {
                for candidate in std::iter::once(canonical).chain(aliases.iter()) {
                    let candidate = Value::String(candidate.clone());
                    if !expanded.iter().any(|value| value == &candidate) {
                        expanded.push(candidate);
                    }
                }
            }
        }
        let max_values = self.domain.query.max_one_of_values.unwrap_or(64).max(1);
        if expanded.len() > max_values {
            return Err(self.error(
                "query_kind_limit",
                "legacy kind aliases expand beyond the descriptor one_of budget",
                json!({"count":expanded.len(),"max":max_values}),
            ));
        }
        Ok(expanded)
    }

    fn configured_message_kinds(&self) -> HashSet<String> {
        let mut kinds = HashSet::new();
        for config in self.domain.query.named_queries.values() {
            if config.get("mode").and_then(Value::as_str) != Some("inbox") {
                continue;
            }
            let Some(canonical) = config.get("kind_key").and_then(Value::as_str) else {
                continue;
            };
            kinds.insert(canonical.to_string());
            if let Some(aliases) = self.domain.query.kind_aliases.get(canonical) {
                kinds.extend(aliases.iter().cloned());
            }
        }
        kinds
    }

    fn is_configured_message_kind(&self, kind: &str) -> bool {
        self.configured_message_kinds().contains(kind)
    }

    fn sql_quote(value: &str) -> String {
        format!("'{}'", value.replace('\'', "''"))
    }

    fn visible_entity_predicate(&self) -> String {
        self.domain
            .query
            .read_receipt_kind
            .as_deref()
            .map(|kind| format!("kind <> {}", Self::sql_quote(kind)))
            .unwrap_or_else(|| "1=1".to_string())
    }

    fn validate_named_filter_conflicts(&self, args: &Map<String, Value>) -> Result<(), Value> {
        let Some(matched) = args.get("match").and_then(Value::as_object) else {
            return Ok(());
        };
        for key in [
            "participant",
            "recipient",
            "sender",
            "from",
            "to",
            "direction",
            "viewer",
            "kinds",
            "since_event",
            "after_sequence",
            "intent",
            "read_state",
            "reply_state",
            "include_body",
            "limit",
        ] {
            if let (Some(flat), Some(nested)) = (args.get(key), matched.get(key)) {
                if flat != nested {
                    return Err(self.error(
                        "query_filter_conflict",
                        "flat and match query filters must agree when both are supplied",
                        json!({"field":key,"flat":flat,"match":nested}),
                    ));
                }
            }
        }
        Ok(())
    }

}
