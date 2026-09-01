impl Engine {
    fn operations_batch(&self, root: &Path, args: &Map<String, Value>) -> Result<Value, Value> {
        let batches = args
            .get("batches")
            .and_then(Value::as_array)
            .ok_or_else(|| {
                self.error(
                    "operations_batch_required",
                    "batches must be a non-empty array",
                    Value::Null,
                )
            })?;
        if batches.is_empty() {
            return Err(self.error(
                "operations_batch_required",
                "batches must be a non-empty array",
                Value::Null,
            ));
        }
        let mut operations = Vec::new();
        let mut census = Vec::new();
        for (batch_index, batch) in batches.iter().enumerate() {
            let batch = batch.as_object().ok_or_else(|| {
                self.error(
                    "operations_batch_invalid",
                    "each batch must be an object",
                    json!({"batch_index":batch_index}),
                )
            })?;
            let defaults = batch
                .get("defaults")
                .and_then(Value::as_object)
                .ok_or_else(|| {
                    self.error(
                        "operations_batch_defaults_invalid",
                        "defaults must be a non-empty object",
                        json!({"batch_index":batch_index}),
                    )
                })?;
            if defaults.is_empty() {
                return Err(self.error(
                    "operations_batch_defaults_invalid",
                    "defaults must be a non-empty object",
                    json!({"batch_index":batch_index}),
                ));
            }
            let columns = batch
                .get("columns")
                .and_then(Value::as_array)
                .ok_or_else(|| {
                    self.error(
                        "operations_batch_columns_invalid",
                        "columns must be a non-empty array",
                        json!({"batch_index":batch_index}),
                    )
                })?;
            if columns.is_empty() {
                return Err(self.error(
                    "operations_batch_columns_invalid",
                    "columns must be a non-empty array",
                    json!({"batch_index":batch_index}),
                ));
            }
            let mut names = Vec::new();
            let mut unique = std::collections::HashSet::new();
            for (column_index, column) in columns.iter().enumerate() {
                let name = column
                    .as_str()
                    .filter(|value| !value.is_empty())
                    .ok_or_else(|| {
                        self.error(
                            "operations_batch_column_invalid",
                            "every column must be a non-empty string",
                            json!({"batch_index":batch_index,"column_index":column_index}),
                        )
                    })?;
                if !unique.insert(name.to_string()) {
                    return Err(self.error(
                        "operations_batch_column_duplicate",
                        "columns must be unique",
                        json!({"batch_index":batch_index,"column":name}),
                    ));
                }
                names.push(name.to_string());
            }
            let rows = batch.get("rows").and_then(Value::as_array).ok_or_else(|| {
                self.error(
                    "operations_batch_rows_invalid",
                    "rows must be a non-empty array",
                    json!({"batch_index":batch_index}),
                )
            })?;
            if rows.is_empty() {
                return Err(self.error(
                    "operations_batch_rows_invalid",
                    "rows must be a non-empty array",
                    json!({"batch_index":batch_index}),
                ));
            }
            for (row_index, row) in rows.iter().enumerate() {
                let cells = row.as_array().ok_or_else(|| {
                    self.error(
                        "operations_batch_row_invalid",
                        "each row must be an array",
                        json!({"batch_index":batch_index,"row_index":row_index}),
                    )
                })?;
                if cells.len() != names.len() {
                    return Err(self.error(
                        "operations_batch_row_width_mismatch",
                        "row width must equal the number of columns",
                        json!({"batch_index":batch_index,"row_index":row_index,"expected":names.len(),"actual":cells.len()}),
                    ));
                }
                if operations.len() as u64 >= self.domain.caps.operations_per_proposal.max {
                    return Err(self.error(
                        "operations_batch_limit_exceeded",
                        "expanded operations exceed the proposal hard cap",
                        json!({"max_operations":self.domain.caps.operations_per_proposal.max,"batch_index":batch_index,"row_index":row_index}),
                    ));
                }
                let mut operation = defaults.clone();
                for (name, value) in names.iter().zip(cells.iter()) {
                    operation.insert(name.clone(), value.clone());
                }
                operations.push(Value::Object(operation));
            }
            census.push(json!({"batch_index":batch_index,"row_count":rows.len(),"columns":names,"default_fields":defaults.keys().collect::<Vec<_>>() }));
        }
        let mut proposal_args = Map::new();
        for field in [
            "actor",
            "authority_basis",
            "idempotency_key",
            "expected_ledger_head",
        ] {
            if let Some(value) = args.get(field) {
                proposal_args.insert(field.to_string(), value.clone());
            }
        }
        proposal_args.insert("operations".to_string(), Value::Array(operations.clone()));
        let mut receipt = self.proposal_submit(root, &proposal_args)?;
        if let Some(object) = receipt.as_object_mut() {
            object.insert(
                "compact_input".to_string(),
                json!({
                    "schema":"narada.epistemic.operations_batch_expansion.v1",
                    "batch_count":batches.len(),
                    "expanded_operation_count":operations.len(),
                    "batches":census,
                    "normalization":"defaults_then_row_columns"
                }),
            );
        }
        Ok(receipt)
    }

}
