impl Engine {
    fn guidance(&self) -> Value {
        let mut object = Map::new();
        for key in &self.domain.guidance.emission_order {
            let value = match key.as_str() {
                "schema" => json!(self.domain.guidance.schema_id),
                "entity_kinds" => json!(self.domain.entities.core_kinds),
                "core_relations" => json!(self.domain.relations.core),
                "operation_kinds" => json!(self.domain.operations.kinds),
                "extension_relation_rule" | "extension_entity_kind_rule" => self
                    .domain
                    .guidance
                    .engine_derived_fields
                    .get(key)
                    .and_then(|entry| entry.get("text"))
                    .cloned()
                    .unwrap_or(Value::Null),
                _ => self
                    .domain
                    .guidance
                    .fields
                    .get(key)
                    .cloned()
                    .unwrap_or(Value::Null),
            };
            object.insert(key.clone(), value);
        }
        Value::Object(object)
    }

    fn guidance_with_request(&self, args: &Map<String, Value>) -> Value {
        let mut value = self.guidance();
        value["requested"] = json!({"workflow":args.get("workflow").cloned().unwrap_or(Value::Null),"tool":args.get("tool").cloned().unwrap_or(Value::Null)});
        value
    }

    fn with_inbox_sequence_remediation(
        &self,
        error: Value,
        has_explicit_sequence_bound: bool,
        has_subject_seed: bool,
    ) -> Value {
        if has_explicit_sequence_bound
            || !has_subject_seed
            || error.get("code").and_then(Value::as_str) != Some("query_datom_scan_limit")
        {
            return error;
        }
        let mut details = error.get("details").cloned().unwrap_or_else(|| json!({}));
        if !details.is_object() {
            details = json!({"cause":details});
        }
        details["remediation"] = json!(
            "Retry the inbox query with after_sequence set to the last inbox sequence already rehydrated."
        );
        details["retry_arguments"] = json!({
            "after_sequence":"<last_rehydrated_inbox_sequence>"
        });
        details["planner_mode"] = json!("indexed_subject_suffix");
        self.error(
            "query_datom_scan_limit",
            "recipient inbox history exceeds the bounded hydration budget; resume from the last rehydrated sequence",
            details,
        )
    }

    fn error(&self, code: &str, message: &str, details: Value) -> Value {
        self.error.error(code, message, details)
    }

    fn io_error(&self, code: &'static str) -> impl FnOnce(std::io::Error) -> Value {
        self.error.io_error(code)
    }

    fn db_error(&self, code: &'static str) -> impl FnOnce(rusqlite::Error) -> Value {
        self.error.db_error(code)
    }
}
