impl Engine {
    fn validate_named_filter_types(
        &self,
        args: &Map<String, Value>,
        mode: &str,
    ) -> Result<(), Value> {
        let validate_value = |field: &str, value: &Value| {
            let valid = match field {
                "participant" | "recipient" | "sender" | "from" | "to" | "direction" | "viewer"
                | "intent" | "read_state" | "reply_state" | "root" | "template" => {
                    value.as_str().is_some_and(|text| !text.trim().is_empty())
                }
                "expected_ledger_head" => {
                    value.is_null() || value.as_str().is_some_and(|text| !text.trim().is_empty())
                }
                "kinds" => value.as_array().is_some_and(|values| {
                    !values.is_empty()
                        && values.len() <= self.domain.query.max_one_of_values.unwrap_or(64).max(1)
                        && values
                            .iter()
                            .all(|item| item.as_str().is_some_and(|text| !text.trim().is_empty()))
                }),
                "since_event" | "after_sequence" | "max_depth" | "max_datoms" | "max_results"
                | "timeout_ms" => value.as_u64().is_some(),
                "include_body" | "latest" => value.is_boolean(),
                "limit" => value.as_u64().is_some_and(|limit| limit > 0),
                "cursor" => value.is_null() || value.is_string() || value.is_object(),
                _ => true,
            };
            if valid {
                Ok(())
            } else {
                Err(self.error(
                    "query_filter_type_invalid",
                    "named query filter has an invalid type or value",
                    json!({"template":mode,"field":field}),
                ))
            }
        };

        for (field, value) in args {
            if field == "match" {
                if !value.is_object() {
                    return Err(self.error(
                        "query_filter_type_invalid",
                        "named query match must be an object",
                        json!({"template":mode,"field":field}),
                    ));
                }
            } else {
                validate_value(field, value)?;
            }
        }
        if let Some(matched) = args.get("match").and_then(Value::as_object) {
            for (field, value) in matched {
                validate_value(field, value)?;
            }
        }
        Ok(())
    }

    fn validate_named_query_fields(
        &self,
        args: &Map<String, Value>,
        mode: &str,
    ) -> Result<(), Value> {
        let allowed = match mode {
            "inbox" => [
                "template",
                "recipient",
                "participant",
                "sender",
                "from",
                "to",
                "kinds",
                "since_event",
                "after_sequence",
                "include_body",
                "direction",
                "viewer",
                "intent",
                "read_state",
                "reply_state",
                "latest",
                "match",
                "limit",
                "cursor",
                "expected_ledger_head",
                "max_datoms",
                "max_results",
                "timeout_ms",
                "budget_escalation",
            ]
            .as_slice(),
            "thread" => [
                "template",
                "root",
                "max_depth",
                "viewer",
                "limit",
                "cursor",
                "match",
                "expected_ledger_head",
            ]
            .as_slice(),
            _ => [].as_slice(),
        };
        for key in args.keys() {
            if !allowed.contains(&key.as_str()) {
                return Err(self.error(
                    "query_filter_unsupported",
                    "the named query does not accept this argument",
                    json!({"template":mode,"field":key}),
                ));
            }
        }
        if let Some(matched) = args.get("match").and_then(Value::as_object) {
            let allowed_match = match mode {
                "inbox" => [
                    "recipient",
                    "participant",
                    "sender",
                    "from",
                    "to",
                    "kinds",
                    "since_event",
                    "after_sequence",
                    "include_body",
                    "direction",
                    "viewer",
                    "intent",
                    "read_state",
                    "reply_state",
                    "limit",
                ]
                .as_slice(),
                "thread" => ["viewer", "limit"].as_slice(),
                _ => [].as_slice(),
            };
            for key in matched.keys() {
                if !allowed_match.contains(&key.as_str()) {
                    return Err(self.error(
                        "query_filter_unsupported",
                        "the named query match object contains an unsupported field",
                        json!({"template":mode,"field":key}),
                    ));
                }
            }
        }
        Ok(())
    }

    fn validate_raw_query_arguments(&self, args: &Map<String, Value>) -> Result<(), Value> {
        let allowed = [
            "query",
            "limit",
            "cursor",
            "expected_ledger_head",
            "max_datoms",
            "max_results",
            "timeout_ms",
            "budget_escalation",
        ];
        for key in args.keys() {
            if !allowed.contains(&key.as_str()) {
                return Err(self.error(
                    "query_mode_mixed",
                    "raw Datalog query accepts query, pagination, and bounded execution controls only",
                    json!({"field":key}),
                ));
            }
        }
        if let Some(limit) = args.get("limit") {
            if !limit.as_u64().is_some_and(|value| value > 0) {
                return Err(self.error(
                    "query_control_type_invalid",
                    "raw query limit must be a positive integer",
                    json!({"field":"limit"}),
                ));
            }
        }
        for field in ["max_datoms", "max_results", "timeout_ms"] {
            if let Some(value) = args.get(field) {
                if !value.as_u64().is_some_and(|value| value > 0) {
                    return Err(self.error(
                        "query_control_type_invalid",
                        "query budget controls must be positive integers",
                        json!({"field":field}),
                    ));
                }
            }
        }
        if args.contains_key("budget_escalation") {
            return Err(self.error(
                "query_budget_escalation_unavailable",
                "this surface has no descriptor-admitted privileged query budget",
                json!({"required":"descriptor-owned maintenance authority with audit evidence"}),
            ));
        }
        if let Some(cursor) = args.get("cursor") {
            if !(cursor.is_null() || cursor.is_string() || cursor.is_object()) {
                return Err(self.error(
                    "query_control_type_invalid",
                    "raw query cursor must be a string, object, or null",
                    json!({"field":"cursor"}),
                ));
            }
        }
        if let Some(expected) = args.get("expected_ledger_head") {
            if !(expected.is_null()
                || expected
                    .as_str()
                    .is_some_and(|value| !value.trim().is_empty()))
            {
                return Err(self.error(
                    "query_control_type_invalid",
                    "expected_ledger_head must be a non-empty string or null",
                    json!({"field":"expected_ledger_head"}),
                ));
            }
        }
        let Some(query) = args.get("query").and_then(Value::as_object) else {
            return Ok(());
        };
        for key in ["limit", "cursor"] {
            if let (Some(nested), Some(top_level)) = (query.get(key), args.get(key)) {
                if nested != top_level {
                    return Err(self.error(
                        "query_override_conflict",
                        "top-level query controls must agree with the same control nested in query",
                        json!({"field":key,"nested":nested,"top_level":top_level}),
                    ));
                }
            }
        }
        Ok(())
    }

    fn canonical_named_template(&self, template: &str) -> String {
        if self.domain.query.named_queries.contains_key(template) {
            return template.to_string();
        }
        let namespaced = format!("{}:{template}", self.domain.identity.schema_namespace);
        if self.domain.query.named_queries.contains_key(&namespaced) {
            return namespaced;
        }
        let suffix = format!(":{template}");
        self.domain
            .query
            .named_queries
            .keys()
            .find(|candidate| candidate.ends_with(&suffix))
            .cloned()
            .unwrap_or_else(|| template.to_string())
    }

    pub fn list_tools(&self) -> Vec<Value> {
        self.domain
            .tools
            .iter()
            .filter(|tool| {
                tool.feature
                    .as_deref()
                    .map(|feature| self.domain.features.enabled(feature))
                    .unwrap_or(true)
            })
            .map(|tool| {
                json!({
                    "name": tool.name,
                    "description": tool.description,
                    "inputSchema": tool.input_schema,
                    "annotations": tool.annotations,
                })
            })
            .collect()
    }

}
