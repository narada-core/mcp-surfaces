impl Engine {
    fn emit_datoms(
        &self,
        tx: &Transaction<'_>,
        operation: &Value,
        event: &Value,
        event_id: &str,
        operation_kind: &str,
    ) -> Result<(), Value> {
        let Some(table) = &self.datoms_table else {
            return Ok(());
        };
        let (origin_id, subject) = if operation_kind == self.entity_op() {
            let id = operation
                .get("entity_id")
                .and_then(Value::as_str)
                .ok_or_else(|| {
                    self.error(
                        "projection_datom_invalid",
                        "entity operation lacks entity_id",
                        operation.clone(),
                    )
                })?;
            (id.to_string(), id.to_string())
        } else if operation_kind == self.relation_op() {
            let origin = operation
                .get("relation_id")
                .and_then(Value::as_str)
                .ok_or_else(|| {
                    self.error(
                        "projection_datom_invalid",
                        "relation operation lacks relation_id",
                        operation.clone(),
                    )
                })?;
            let subject = operation
                .get("source_id")
                .and_then(Value::as_str)
                .ok_or_else(|| {
                    self.error(
                        "projection_datom_invalid",
                        "relation operation lacks source_id",
                        operation.clone(),
                    )
                })?;
            (origin.to_string(), subject.to_string())
        } else {
            let fold = self
                .domain
                .projection
                .fold
                .iter()
                .find(|entry| entry.operation == operation_kind)
                .ok_or_else(|| {
                    self.error(
                        "projection_datom_invalid",
                        "operation has no fold descriptor",
                        json!({"operation":operation_kind}),
                    )
                })?;
            let id = operation
                .get(&fold.key_field)
                .and_then(Value::as_str)
                .ok_or_else(|| {
                    self.error(
                        "projection_datom_invalid",
                        "record operation lacks its identity field",
                        json!({"operation":operation_kind,"field":fold.key_field}),
                    )
                })?;
            (id.to_string(), id.to_string())
        };
        let identity_field = if operation_kind == self.entity_op() {
            "entity_id".to_string()
        } else if operation_kind == self.relation_op() {
            "relation_id".to_string()
        } else {
            self.domain
                .projection
                .fold
                .iter()
                .find(|entry| entry.operation == operation_kind)
                .map(|entry| entry.key_field.clone())
                .unwrap_or_default()
        };
        tx.execute(
            &format!("delete from {table} where origin_id=?1"),
            params![origin_id],
        )
        .map_err(self.db_error("projection_datom_delete_failed"))?;

        let sequence = event["sequence"].as_u64().unwrap_or_default();
        let metadata_subject = if operation_kind == self.relation_op() {
            origin_id.clone()
        } else {
            subject.clone()
        };
        let identity_attribute = if operation_kind == self.entity_op() {
            "narada.ledger:entity/id"
        } else if operation_kind == self.relation_op() {
            "narada.ledger:relation/id"
        } else {
            "narada.ledger:record/id"
        };
        self.write_datom(
            tx,
            table,
            &origin_id,
            &metadata_subject,
            identity_attribute,
            &Value::String(origin_id.clone()),
            sequence,
            event_id,
        )?;
        self.write_datom(
            tx,
            table,
            &origin_id,
            &metadata_subject,
            "narada.ledger:event/id",
            &Value::String(event_id.to_string()),
            sequence,
            event_id,
        )?;
        self.write_datom(
            tx,
            table,
            &origin_id,
            &metadata_subject,
            "narada.ledger:event/sequence",
            &json!(sequence),
            sequence,
            event_id,
        )?;

        if operation_kind == self.entity_op() {
            if let Some(kind) = operation.get("kind") {
                self.write_datom(
                    tx,
                    table,
                    &origin_id,
                    &metadata_subject,
                    "narada.ledger:entity/kind",
                    kind,
                    sequence,
                    event_id,
                )?;
            }
        } else if operation_kind == self.relation_op() {
            if let Some(relation_type) = operation.get("relation_type").and_then(Value::as_str) {
                self.write_datom(
                    tx,
                    table,
                    &origin_id,
                    &metadata_subject,
                    "narada.ledger:relation/type",
                    &Value::String(relation_type.to_string()),
                    sequence,
                    event_id,
                )?;
                if let Some(target) = operation.get("target_id") {
                    let attribute =
                        format!("{}:{relation_type}", self.domain.identity.schema_namespace);
                    self.write_datom(
                        tx, table, &origin_id, &subject, &attribute, target, sequence, event_id,
                    )?;
                    if let Some(inverse_type) =
                        self.domain.query.relation_inverses.get(relation_type)
                    {
                        let inverse =
                            format!("{}:{inverse_type}", self.domain.identity.schema_namespace);
                        self.write_datom(
                            tx,
                            table,
                            &origin_id,
                            target.as_str().unwrap_or_default(),
                            &inverse,
                            &Value::String(subject.clone()),
                            sequence,
                            event_id,
                        )?;
                    }
                }
            }
        } else if let Some(record_kind) = self
            .domain
            .projection
            .fold
            .iter()
            .find(|entry| entry.operation == operation_kind)
            .and_then(|entry| entry.columns.get("record_kind"))
            .and_then(Value::as_str)
        {
            self.write_datom(
                tx,
                table,
                &origin_id,
                &metadata_subject,
                "narada.ledger:record/kind",
                &Value::String(record_kind.to_string()),
                sequence,
                event_id,
            )?;
        }

        if let Some(object) = operation.as_object() {
            for (field, value) in object {
                if field == "op"
                    || field == &identity_field
                    || field == "relation_type"
                    || field == "kind"
                {
                    continue;
                }
                let attribute = format!("{}:{field}", self.domain.identity.schema_namespace);
                self.write_datom(
                    tx,
                    table,
                    &origin_id,
                    &metadata_subject,
                    &attribute,
                    value,
                    sequence,
                    event_id,
                )?;
            }
        }
        Ok(())
    }

    fn write_datom(
        &self,
        tx: &Transaction<'_>,
        table: &str,
        origin_id: &str,
        subject: &str,
        attribute: &str,
        value: &Value,
        sequence: u64,
        event_id: &str,
    ) -> Result<(), Value> {
        let value_json = value.to_string();
        let value_kind = match value {
            Value::Null => "null",
            Value::Bool(_) => "boolean",
            Value::Number(_) => "number",
            Value::String(_) => "string",
            Value::Array(_) | Value::Object(_) => "json",
        };
        let datom_id =
            sha256(format!("{origin_id}\0{subject}\0{attribute}\0{value_json}").as_bytes());
        tx.execute(
            &format!("insert or replace into {table}(datom_id,origin_id,subject,attribute,value_json,value_kind,event_sequence,event_id) values(?1,?2,?3,?4,?5,?6,?7,?8)"),
            params![datom_id, origin_id, subject, attribute, value_json, value_kind, sequence as i64, event_id],
        )
        .map_err(self.db_error("projection_datom_write_failed"))?;
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn verify_ledger(&self, root: &Path) -> Result<(), Value> {
        event_ledger::verify(self.error, &self.ledger_layout(root), self.event_hash_field)
    }

}
