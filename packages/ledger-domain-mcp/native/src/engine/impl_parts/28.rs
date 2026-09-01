impl Engine {
    fn validate_references(&self, root: &Path, operations: &[Value]) -> Result<(), Value> {
        let mut known = std::collections::HashSet::new();
        if self.projection_path(root).exists() {
            let db = Connection::open(self.projection_path(root))
                .map_err(self.db_error("projection_open_failed"))?;
            let entity_pk = self.table(&self.entity_table).primary_key.clone();
            let mut statement = db
                .prepare(&format!("select {} from {}", entity_pk, self.entity_table))
                .map_err(self.db_error("projection_reference_prepare_failed"))?;
            let rows = statement
                .query_map([], |row| row.get::<_, String>(0))
                .map_err(self.db_error("projection_reference_query_failed"))?;
            for row in rows {
                known.insert(row.map_err(self.db_error("projection_reference_row_failed"))?);
            }
        }
        let entity_key_field = self
            .domain
            .projection
            .fold
            .iter()
            .find(|entry| entry.table == self.entity_table)
            .map(|entry| entry.key_field.clone())
            .unwrap_or_else(|| "entity_id".to_string());
        for operation in operations {
            if operation["op"] == self.entity_op() {
                known.insert(
                    operation
                        .get(&entity_key_field)
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string(),
                );
            }
        }
        let require_known = |field: &str, operation: &Value| -> Result<(), Value> {
            let id = operation
                .get(field)
                .and_then(Value::as_str)
                .unwrap_or_default();
            if known.contains(id) {
                Ok(())
            } else {
                Err(self.error(
                    "dangling_reference",
                    "operation references an unknown entity",
                    json!({"field":field,"entity_id":id,"operation":operation}),
                ))
            }
        };
        let evidence_required_fields = self
            .domain
            .operations
            .evidence_entry
            .get("required")
            .and_then(Value::as_array)
            .and_then(|fields| {
                fields
                    .iter()
                    .map(|field| field.as_str().map(str::to_string))
                    .collect::<Option<Vec<_>>>()
            })
            .unwrap_or_default();
        for operation in operations {
            let op_kind = operation["op"].as_str().unwrap_or_default();
            if op_kind == self.domain.query.communication.canonicalization_operation {
                let entity_id = operation
                    .get("entity_id")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let legacy_kind = operation
                    .get("legacy_kind")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let evidence = operation
                    .get("equivalence_evidence")
                    .and_then(Value::as_object)
                    .ok_or_else(|| {
                        self.error(
                            "communication_canonicalization_evidence_required",
                            "canonicalization evidence is missing",
                            json!({"entity_id":entity_id}),
                        )
                    })?;
                let expected_digest = evidence
                    .get("payload_sha256")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let expected_event = evidence
                    .get("originating_event_id")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let db = Connection::open(self.projection_path(root))
                    .map_err(self.db_error("projection_open_failed"))?;
                let current = db
                    .query_row(
                        &format!(
                            "select kind,payload_json,event_id from {} where entity_id=?1",
                            self.entity_table
                        ),
                        params![entity_id],
                        |row| {
                            Ok((
                                row.get::<_, String>(0)?,
                                row.get::<_, String>(1)?,
                                row.get::<_, String>(2)?,
                            ))
                        },
                    )
                    .optional()
                    .map_err(self.db_error("communication_canonicalization_lookup_failed"))?;
                let Some((current_kind, payload_json, originating_event_id)) = current else {
                    return Err(self.error(
                        "dangling_reference",
                        "canonicalization references an unknown entity",
                        json!({"entity_id":entity_id}),
                    ));
                };
                let actual_digest = sha256(payload_json.as_bytes());
                if current_kind != legacy_kind
                    || actual_digest != expected_digest
                    || originating_event_id != expected_event
                {
                    return Err(self.error(
                        &self.domain.query.communication.collision_refusal_code,
                        "canonicalization evidence does not prove identity and payload provenance equivalence",
                        json!({"entity_id":entity_id,"expected":{"kind":legacy_kind,"payload_sha256":expected_digest,"originating_event_id":expected_event},"actual":{"kind":current_kind,"payload_sha256":actual_digest,"originating_event_id":originating_event_id}}),
                    ));
                }
            }
            for binding in &self.domain.operations.reference_bindings {
                if binding.operation == "*" {
                    for field in &binding.fields {
                        let Some((array_field, sub_field)) = field.split_once("[].") else {
                            continue;
                        };
                        for entry in operation
                            .get(array_field)
                            .and_then(Value::as_array)
                            .into_iter()
                            .flatten()
                        {
                            require_known(sub_field, entry)?;
                            for required_field in &evidence_required_fields {
                                if required_field == sub_field {
                                    continue;
                                }
                                if entry
                                    .get(required_field)
                                    .and_then(Value::as_str)
                                    .filter(|value| !value.trim().is_empty())
                                    .is_none()
                                {
                                    return Err(self.error(
                                        "evidence_location_incomplete",
                                        "evidence requires locator and paraphrase",
                                        json!({"field":required_field,"evidence":entry}),
                                    ));
                                }
                            }
                        }
                    }
                } else if binding.operation == op_kind {
                    for field in &binding.fields {
                        require_known(field, operation)?;
                    }
                }
            }
        }
        Ok(())
    }

}
