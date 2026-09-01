impl Engine {
    fn normalize_communication_result(&self, value: &mut Value) -> usize {
        let communication = &self.domain.query.communication;
        match value {
            Value::Object(object) => {
                let mut count = 0;
                if let Some(kind) = object.get("kind").and_then(Value::as_str) {
                    if communication
                        .legacy_read_aliases
                        .iter()
                        .any(|legacy| legacy == kind)
                    {
                        object.insert(
                            "kind".into(),
                            Value::String(communication.canonical_kind.clone()),
                        );
                        count += 1;
                    }
                }
                for child in object.values_mut() {
                    count += self.normalize_communication_result(child);
                }
                count
            }
            Value::Array(values) => values
                .iter_mut()
                .map(|child| self.normalize_communication_result(child))
                .sum(),
            _ => 0,
        }
    }

    fn expand_named_identity_terms(
        &self,
        root: &Path,
        args: &Map<String, Value>,
        query: &mut Value,
    ) -> Result<(), Value> {
        let Some(template) = args.get("template").and_then(Value::as_str) else {
            return Ok(());
        };
        let canonical_template = self.canonical_named_template(template);
        let Some(identity) = self
            .domain
            .query
            .named_queries
            .get(&canonical_template)
            .and_then(Value::as_object)
            .and_then(|config| config.get("participant_identity"))
            .and_then(Value::as_object)
        else {
            return Ok(());
        };
        let Some(kind) = identity.get("kind").and_then(Value::as_str) else {
            return Ok(());
        };
        let fields = identity
            .get("alias_fields")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .collect::<Vec<_>>();
        if fields.is_empty() {
            return Ok(());
        }
        let db = Connection::open(self.projection_path(root))
            .map_err(self.db_error("projection_open_failed"))?;
        let mut statement = db
            .prepare(&format!(
                "select entity_id,payload_json from {} where kind=?1",
                self.entity_table
            ))
            .map_err(self.db_error("projection_identity_prepare_failed"))?;
        let rows = statement
            .query_map(params![kind], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })
            .map_err(self.db_error("projection_identity_query_failed"))?;
        let identities = rows
            .map(|row| row.map_err(self.db_error("projection_identity_row_failed")))
            .collect::<Result<Vec<_>, _>>()?;

        let match_value = |key: &str| {
            args.get(key).or_else(|| {
                args.get("match")
                    .and_then(Value::as_object)
                    .and_then(|matched| matched.get(key))
            })
        };
        let participant = match_value("participant").or_else(|| match_value("recipient"));
        let sender = match_value("sender").or_else(|| match_value("from"));
        let resolved_inputs = [
            ("participant", participant),
            ("viewer", match_value("viewer").or(participant)),
            ("sender", sender),
            ("to", match_value("to")),
        ];
        let mut replacements = BTreeMap::<String, Vec<Value>>::new();
        for (input_key, raw_value) in resolved_inputs {
            let Some(raw) = raw_value.and_then(Value::as_str) else {
                continue;
            };
            let mut equivalents = vec![Value::String(raw.to_string())];
            for (entity_id, payload_json) in &identities {
                let payload: Value = serde_json::from_str(payload_json).unwrap_or(Value::Null);
                let aliases = fields
                    .iter()
                    .filter_map(|field| payload.get(*field).and_then(Value::as_str))
                    .collect::<Vec<_>>();
                if entity_id == raw || aliases.contains(&raw) {
                    for candidate in std::iter::once(entity_id.as_str()).chain(aliases) {
                        let candidate = Value::String(candidate.to_string());
                        if !equivalents.contains(&candidate) {
                            equivalents.push(candidate);
                        }
                    }
                }
            }
            if equivalents.len() > 1 {
                replacements.insert(input_key.to_string(), equivalents);
            }
        }

        fn rewrite(value: &mut Value, replacements: &BTreeMap<String, Vec<Value>>) {
            match value {
                Value::Object(object) => {
                    if object.len() == 1 {
                        if let Some(key) = object.get("input").and_then(Value::as_str) {
                            if let Some(equivalents) = replacements.get(key) {
                                *value = json!({"one_of":equivalents});
                                return;
                            }
                        }
                    }
                    for child in object.values_mut() {
                        rewrite(child, replacements);
                    }
                }
                Value::Array(values) => {
                    for child in values {
                        rewrite(child, replacements);
                    }
                }
                _ => {}
            }
        }
        rewrite(query, &replacements);
        Ok(())
    }

}
