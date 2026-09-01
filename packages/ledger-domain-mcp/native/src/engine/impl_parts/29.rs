impl Engine {
    fn validate_operations(&self, ops: &[Value], require_evidence: bool) -> Result<(), Value> {
        for op in ops {
            let obj = op.as_object().ok_or_else(|| {
                self.error(
                    "invalid_operation",
                    "operation must be an object",
                    Value::Null,
                )
            })?;
            let kind = self.required(obj, "op")?;
            let Some(required_fields) = self.domain.operations.required_fields.get(kind.as_str())
            else {
                return Err(self.error(
                    "invalid_operation",
                    "unsupported operation",
                    json!({"op":kind}),
                ));
            };
            for field in required_fields.clone() {
                if field == "op" {
                    continue;
                }
                let value = self.required(obj, &field)?;
                if field == "kind" && kind == self.entity_op() {
                    let communication = &self.domain.query.communication;
                    if communication
                        .legacy_read_aliases
                        .iter()
                        .any(|legacy| legacy == &value)
                    {
                        return Err(self.error(
                            &communication.legacy_write_refusal_code,
                            "legacy communication kinds are read aliases and cannot authorize writes",
                            json!({"supplied_kind":value,"canonical_replacement":communication.canonical_kind,"contract_version":communication.contract_version,"remediation":"resubmit the declaration with canonical_replacement"}),
                        ));
                    }
                    let rule = &self.domain.entities.extension_rule;
                    if !self.domain.entities.core_kinds.contains(&value)
                        && !value.contains(&rule.must_contain)
                    {
                        return Err(self.error(
                            &rule.refusal_code,
                            "extension entity kinds must be namespaced",
                            json!({"kind":value,"core_entity_kinds":self.domain.entities.core_kinds,"extension_pattern":rule.pattern,"examples":rule.examples}),
                        ));
                    }
                }
                if field == "relation_type" && kind == self.relation_op() {
                    let rule = &self.domain.relations.extension_rule;
                    if !self.domain.relations.core.contains(&value)
                        && !value.contains(&rule.must_contain)
                    {
                        return Err(self.error(
                            &rule.refusal_code,
                            "extension relations must be namespaced",
                            json!({
                                "relation_type":value,
                                "core_relations":self.domain.relations.core,
                                "extension_pattern":rule.pattern,
                                "examples":rule.examples
                            }),
                        ));
                    }
                }
            }
            if kind == self.entity_op() {
                let communication = &self.domain.query.communication;
                let entity_kind = obj.get("kind").and_then(Value::as_str).unwrap_or_default();
                if communication
                    .legacy_read_aliases
                    .iter()
                    .any(|legacy| legacy == entity_kind)
                {
                    return Err(self.error(
                        &communication.legacy_write_refusal_code,
                        "legacy communication kinds are read aliases and cannot authorize writes",
                        json!({"supplied_kind":entity_kind,"canonical_replacement":communication.canonical_kind,"contract_version":communication.contract_version,"remediation":"resubmit the declaration with canonical_replacement"}),
                    ));
                }
                if entity_kind == communication.canonical_kind {
                    for field in &communication.required_fields {
                        self.required(obj, field)?;
                    }
                    if !communication.content_any_of.iter().any(|field| {
                        obj.get(field)
                            .and_then(Value::as_str)
                            .map(str::trim)
                            .map(|value| !value.is_empty())
                            .unwrap_or(false)
                    }) {
                        return Err(self.error(
                            "communication_content_required",
                            "canonical communication requires at least one declared content field",
                            json!({"canonical_kind":communication.canonical_kind,"content_any_of":communication.content_any_of}),
                        ));
                    }
                }
                for conditional in &self.domain.entities.required_fields.conditional {
                    if obj.get("kind").and_then(Value::as_str)
                        == Some(conditional.when_kind.as_str())
                    {
                        for field in &conditional.requires {
                            self.required(obj, field)?;
                        }
                    }
                }
            }
            if kind == self.domain.query.communication.canonicalization_operation {
                let communication = &self.domain.query.communication;
                let legacy_kind = obj
                    .get("legacy_kind")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let canonical_kind = obj
                    .get("canonical_kind")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                if !communication
                    .legacy_read_aliases
                    .iter()
                    .any(|legacy| legacy == legacy_kind)
                    || canonical_kind != communication.canonical_kind
                {
                    return Err(self.error(
                        "communication_canonicalization_contract_mismatch",
                        "canonicalization must use a declared legacy alias and the descriptor canonical kind",
                        json!({"legacy_kind":legacy_kind,"canonical_kind":canonical_kind,"canonical_replacement":communication.canonical_kind,"legacy_read_aliases":communication.legacy_read_aliases}),
                    ));
                }
                let evidence = obj
                    .get("equivalence_evidence")
                    .and_then(Value::as_object)
                    .ok_or_else(|| {
                        self.error(
                    "communication_canonicalization_evidence_required",
                    "canonicalization requires payload digest and originating event evidence",
                    json!({"entity_id":obj.get("entity_id")}),
                )
                    })?;
                for field in ["payload_sha256", "originating_event_id"] {
                    self.required(evidence, field)?;
                }
            }
            if require_evidence
                && self
                    .domain
                    .operations
                    .evidence_required_at_review
                    .contains(&kind)
                && obj
                    .get("evidence")
                    .and_then(Value::as_array)
                    .map(|value| value.is_empty())
                    .unwrap_or(true)
            {
                return Err(self.error(
                    "evidence_required",
                    "assessment and outcome records require evidence",
                    json!({"op":kind}),
                ));
            }
        }
        Ok(())
    }

    fn with_authority_lock<T>(
        &self,
        root: &Path,
        key: &str,
        action: impl FnOnce() -> Result<T, Value>,
    ) -> Result<T, Value> {
        lock::with_authority_lock(
            self.error,
            &self.runtime(root).join("locks"),
            key,
            lock::AuthorityLockPolicy::default(),
            action,
        )
    }

    fn validated_sequence_name(&self, args: &Map<String, Value>) -> Result<String, Value> {
        let name = self.required(args, "sequence_name")?;
        if name.trim() != name
            || name.chars().count() as u64 > self.domain.caps.sequence_name_chars.max
            || name.chars().any(char::is_control)
        {
            return Err(self.error(
                "sequence_name_invalid",
                "sequence_name must be 1-120 non-control characters without surrounding whitespace",
                json!({"sequence_name":name}),
            ));
        }
        Ok(name)
    }

    fn required_object(&self, args: &Map<String, Value>, key: &str) -> Result<Value, Value> {
        ledger_args::required_object(
            self.error,
            args,
            key,
            self.domain.caps.authority_basis_bytes,
            "authority_basis",
        )
    }

    fn optional_u64(
        &self,
        args: &Map<String, Value>,
        key: &str,
        default: u64,
    ) -> Result<u64, Value> {
        ledger_args::optional_u64(self.error, args, key, default)
    }

    fn page_limit(&self, args: &Map<String, Value>) -> Result<usize, Value> {
        ledger_args::page_limit(self.error, args)
    }

    fn page_offset(&self, args: &Map<String, Value>) -> Result<usize, Value> {
        ledger_args::page_offset(self.error, args)
    }

    fn sequence_directory(&self, root: &Path, name: &str) -> PathBuf {
        self.sequences(root).join(sha256(name.as_bytes()))
    }

    fn sequence_claims_directory(&self, root: &Path, name: &str) -> PathBuf {
        self.sequence_directory(root, name).join("claims")
    }

    fn load_sequence_manifest(&self, root: &Path, name: &str) -> Result<Value, Value> {
        let path = self.sequence_directory(root, name).join("sequence.json");
        if !path.exists() {
            return Err(self.error(
                "sequence_not_found",
                "sequence does not exist",
                json!({"sequence_name":name}),
            ));
        }
        let manifest = self.read_json(&path)?;
        self.verify_sequence_manifest(&manifest, name)?;
        Ok(manifest)
    }

    fn verify_sequence_manifest(&self, manifest: &Value, expected_name: &str) -> Result<(), Value> {
        let sequences = &self.domain.features.sequences;
        let expected_id = self.generated_sequence_id(expected_name);
        if manifest.get("schema") != Some(&json!(sequences.manifest_schema_id))
            || manifest.get("sequence_name").and_then(Value::as_str) != Some(expected_name)
            || manifest.get("sequence_id").and_then(Value::as_str) != Some(expected_id.as_str())
            || manifest
                .get("start_at")
                .and_then(Value::as_u64)
                .is_none_or(|value| value < sequences.start_at_min)
            || manifest.get("step").and_then(Value::as_u64) != Some(sequences.step)
        {
            return Err(self.error(
                "sequence_manifest_invalid",
                "sequence manifest has invalid identity or configuration",
                json!({"sequence_name":expected_name}),
            ));
        }
        let hash_field = sequences.manifest_hash_field.clone();
        let Some(recomputed) = chain::recompute_hash(self.error, manifest, &hash_field)? else {
            return Err(self.error(
                "sequence_manifest_invalid",
                "sequence manifest lacks creation_hash",
                json!({"sequence_name":expected_name}),
            ));
        };
        if recomputed.stored != recomputed.computed {
            return Err(self.error(
                "sequence_manifest_hash_invalid",
                "sequence manifest hash does not match",
                json!({"sequence_name":expected_name,"expected_hash":recomputed.computed,"actual_hash":recomputed.stored}),
            ));
        }
        Ok(())
    }

}
