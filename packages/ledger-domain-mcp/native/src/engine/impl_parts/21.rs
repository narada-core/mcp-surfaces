impl Engine {
    fn decorate_query_entity(
        &self,
        db: &Connection,
        id: &str,
        value: &mut Value,
        binding: &Map<String, Value>,
        datoms: &[ledger_query::Datom],
    ) -> Result<(), Value> {
        let kind = self.entity_kind(db, id)?;
        if !self.is_configured_message_kind(&kind) {
            return Ok(());
        }
        let Some(object) = value.as_object_mut() else {
            return Ok(());
        };
        let viewer = binding
            .get("?viewer")
            .and_then(Value::as_str)
            .filter(|value| !value.trim().is_empty());
        let message_state = self.message_read_state(id, viewer, datoms);
        let reply_state_attribute = self
            .domain
            .query
            .reply_state_attribute
            .as_deref()
            .unwrap_or("");
        let reply_count = if reply_state_attribute.is_empty() {
            0
        } else {
            datoms
                .iter()
                .filter(|datom| {
                    datom.attribute == reply_state_attribute && datom.value.as_str() == Some(id)
                })
                .count()
        };
        let is_reply = !reply_state_attribute.is_empty()
            && datoms
                .iter()
                .any(|datom| datom.attribute == reply_state_attribute && datom.subject == id);
        let reply_state = json!({
            "status":if reply_count > 0 { "replied" } else { "unreplied" },
            "has_replies":reply_count > 0,
            "reply_count":reply_count,
            "is_reply":is_reply
        });
        let query_meta = json!({
            "message_state":message_state,
            "reply_state":reply_state,
            "kind":kind,
            "viewer":viewer
        });
        // Preserve legacy top-level fields for callers, but never overwrite a
        // domain payload field. `_narada_query` is the collision-free source
        // of truth for new callers.
        if !object.contains_key("message_state") {
            object.insert("message_state".into(), message_state.clone());
        }
        if !object.contains_key("reply_state") {
            object.insert("reply_state".into(), reply_state.clone());
        }
        object.insert("_narada_query".into(), query_meta);
        Ok(())
    }

    fn render_query_binding(
        &self,
        db: &Connection,
        binding: &Map<String, Value>,
        spec: &ledger_query::QuerySpec,
        datoms: &[ledger_query::Datom],
    ) -> Result<Value, Value> {
        if spec.pulls.len() == 1 && spec.finds.len() == 1 {
            let pull = &spec.pulls[0];
            let id = binding
                .get(&pull.variable)
                .and_then(Value::as_str)
                .ok_or_else(|| {
                    self.error(
                        "query_pull_target_invalid",
                        "pull target is not a string entity, relation, or record id",
                        json!({"variable":pull.variable}),
                    )
                })?;
            let (mut value, is_entity) =
                self.pull_target(db, id, &pull.fields, pull.target_kind.as_deref())?;
            if is_entity {
                self.decorate_query_entity(db, id, &mut value, binding, datoms)?;
            }
            return Ok(value);
        }
        let mut output = Map::new();
        for find in &spec.finds {
            let Some(name) = find.as_variable_name() else {
                continue;
            };
            let value = binding.get(name).ok_or_else(|| {
                self.error(
                    "query_find_unbound",
                    "find variable is not bound in the result",
                    json!({"variable":name}),
                )
            })?;
            output.insert(name.trim_start_matches('?').to_string(), value.clone());
        }
        for pull in &spec.pulls {
            let id = binding
                .get(&pull.variable)
                .and_then(Value::as_str)
                .ok_or_else(|| {
                    self.error(
                        "query_pull_target_invalid",
                        "pull target is not a string entity, relation, or record id",
                        json!({"variable":pull.variable}),
                    )
                })?;
            let (mut value, is_entity) =
                self.pull_target(db, id, &pull.fields, pull.target_kind.as_deref())?;
            if is_entity {
                self.decorate_query_entity(db, id, &mut value, binding, datoms)?;
            }
            output.insert(pull.variable.trim_start_matches('?').to_string(), value);
        }
        Ok(Value::Object(output))
    }

    fn message_read_state(
        &self,
        message_id: &str,
        viewer: Option<&str>,
        datoms: &[ledger_query::Datom],
    ) -> Value {
        let read = match (
            viewer,
            self.domain.query.read_receipt_kind.as_deref(),
            self.domain.query.read_receipt_kind_attribute.as_deref(),
            self.domain.query.read_receipt_message_attribute.as_deref(),
            self.domain.query.read_receipt_reader_attribute.as_deref(),
        ) {
            (
                Some(viewer),
                Some(receipt_kind),
                Some(kind_attribute),
                Some(message_attribute),
                Some(reader_attribute),
            ) => {
                let receipt_ids = datoms
                    .iter()
                    .filter(|datom| {
                        datom.attribute == kind_attribute
                            && datom.value.as_str() == Some(receipt_kind)
                    })
                    .map(|datom| datom.subject.as_str())
                    .collect::<HashSet<_>>();
                Some(datoms.iter().any(|datom| {
                    receipt_ids.contains(datom.subject.as_str())
                        && datom.attribute == message_attribute
                        && datom.value.as_str() == Some(message_id)
                        && datoms.iter().any(|reader_datom| {
                            reader_datom.subject == datom.subject
                                && reader_datom.attribute == reader_attribute
                                && reader_datom.value.as_str() == Some(viewer)
                        })
                }))
            }
            _ => None,
        };
        let status = match read {
            Some(true) => "read",
            Some(false) => "unread",
            None => "unknown",
        };
        json!({
            "status":status,
            "read":read,
            "unread":read.map(|value| !value),
            "viewer":viewer
        })
    }

    fn render_pull_fields(
        &self,
        fields: &[String],
        base: &Map<String, Value>,
        payload: &Value,
    ) -> Value {
        let mut output = Map::new();
        let full = fields.iter().any(|field| field == "*");
        for field in fields {
            if field == "*" {
                continue;
            }
            let value = base
                .get(field)
                .cloned()
                .or_else(|| payload.get(field).cloned())
                .unwrap_or(Value::Null);
            output.insert(field.clone(), value);
        }
        if full {
            for (field, value) in base {
                output.insert(field.clone(), value.clone());
            }
            output.insert("payload".into(), payload.clone());
        }
        Value::Object(output)
    }

    fn parse_pull_payload(&self, target_id: &str, payload_json: &str) -> Result<Value, Value> {
        if payload_json.len() as u64 > self.domain.caps.query_execution.max_output_bytes {
            return Err(self.error(
                "query_payload_limit",
                "pull target payload exceeds the descriptor response-byte budget",
                json!({"target_id":target_id,"payload_bytes":payload_json.len(),"max_output_bytes":self.domain.caps.query_execution.max_output_bytes}),
            ));
        }
        serde_json::from_str::<Value>(payload_json).map_err(|_| {
            self.error(
                "query_pull_payload_invalid",
                "pull target payload is not valid JSON",
                json!({"target_id":target_id}),
            )
        })
    }

}
