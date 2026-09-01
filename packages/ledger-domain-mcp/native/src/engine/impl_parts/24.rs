impl Engine {
    fn query_batch_locked(&self, root: &Path, args: &Map<String, Value>) -> Result<Value, Value> {
        let caps = &self.domain.caps.query_batch;
        for key in args.keys() {
            if !["queries", "limit_per_query", "expected_ledger_head"].contains(&key.as_str()) {
                return Err(self.error(
                    "invalid_batch_query",
                    "batch query accepts only queries, limit_per_query, and expected_ledger_head",
                    json!({"field":key}),
                ));
            }
        }
        let queries = args
            .get("queries")
            .and_then(Value::as_array)
            .ok_or_else(|| {
                self.error(
                    "invalid_batch_query",
                    "queries must be an array",
                    Value::Null,
                )
            })?;
        if (queries.len() as u64) < caps.min_queries || (queries.len() as u64) > caps.max_queries {
            return Err(self.error(
                "invalid_batch_query",
                &format!(
                    "queries count must be between {} and {}",
                    caps.min_queries, caps.max_queries
                ),
                json!({"count":queries.len()}),
            ));
        }
        if let Some(limit) = args.get("limit_per_query") {
            if !limit.as_u64().is_some_and(|value| value > 0) {
                return Err(self.error(
                    "invalid_batch_query",
                    "limit_per_query must be a positive integer",
                    json!({"field":"limit_per_query"}),
                ));
            }
        }
        let expected_ledger_head = args.get("expected_ledger_head").cloned();
        if let Some(expected) = &expected_ledger_head {
            if !(expected.is_null()
                || expected
                    .as_str()
                    .is_some_and(|value| !value.trim().is_empty()))
            {
                return Err(self.error(
                    "invalid_batch_query",
                    "expected_ledger_head must be a non-empty string or null",
                    json!({"field":"expected_ledger_head"}),
                ));
            }
        }
        let batch_limit = args
            .get("limit_per_query")
            .and_then(Value::as_u64)
            .unwrap_or(caps.limit_per_query_default)
            .clamp(caps.limit_per_query_min, caps.limit_per_query_max);
        let mut results = Vec::with_capacity(queries.len());
        for (index, item) in queries.iter().enumerate() {
            let item = item.as_object().ok_or_else(|| {
                self.error(
                    "invalid_batch_query",
                    "each query must be an object",
                    json!({"index":index}),
                )
            })?;
            if let Some(limit) = item.get("limit") {
                if !limit.as_u64().is_some_and(|value| value > 0) {
                    return Err(self.error(
                        "invalid_batch_query",
                        "query item limit must be a positive integer",
                        json!({"index":index,"field":"limit"}),
                    ));
                }
            }
            if let Some(cursor) = item.get("cursor") {
                if !(cursor.is_null() || cursor.is_string() || cursor.is_object()) {
                    return Err(self.error(
                        "invalid_batch_query",
                        "query item cursor must be a string, object, or null",
                        json!({"index":index,"field":"cursor"}),
                    ));
                }
            }
            if let Some(item_expected) = item.get("expected_ledger_head") {
                if !(item_expected.is_null()
                    || item_expected
                        .as_str()
                        .is_some_and(|value| !value.trim().is_empty()))
                {
                    return Err(self.error(
                        "invalid_batch_query",
                        "query item expected_ledger_head must be a non-empty string or null",
                        json!({"index":index,"field":"expected_ledger_head"}),
                    ));
                }
            }
            let mut query_args = item.clone();
            if let Some(expected) = &expected_ledger_head {
                if let Some(item_expected) = item.get("expected_ledger_head") {
                    if !item_expected.is_null() && item_expected != expected {
                        return Err(self.error(
                            "query_expected_head_conflict",
                            "batch expected_ledger_head cannot be overridden by an item",
                            json!({"index":index,"batch":expected,"item":item_expected}),
                        ));
                    }
                }
                query_args.insert("expected_ledger_head".into(), expected.clone());
            }
            let generic = query_args.contains_key("query") || query_args.contains_key("template");
            let named_fields = [
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
                "match",
                "root",
                "max_depth",
            ];
            let has_cursor = query_args
                .get("cursor")
                .map(|value| !value.is_null())
                .unwrap_or(false);
            if !generic && has_cursor {
                return Err(self.error(
                    "query_cursor_unsupported",
                    "legacy batch queries use offset pagination; cursor requires query or template",
                    json!({"index":index}),
                ));
            }
            if !generic
                && named_fields
                    .iter()
                    .any(|field| query_args.contains_key(*field))
            {
                return Err(self.error(
                    "query_template_missing",
                    "template is required when named-query filters are supplied in a batch item",
                    json!({"index":index}),
                ));
            }
            if generic {
                if query_args.contains_key("query") {
                    self.validate_raw_query_arguments(&query_args)?;
                } else {
                    self.validate_named_filter_conflicts(&query_args)?;
                }
            }
            let requested_limit = query_args
                .get("limit")
                .and_then(Value::as_u64)
                .or_else(|| {
                    query_args
                        .get("query")
                        .and_then(Value::as_object)
                        .and_then(|query| query.get("limit"))
                        .and_then(Value::as_u64)
                })
                .or_else(|| {
                    query_args
                        .get("match")
                        .and_then(Value::as_object)
                        .and_then(|matched| matched.get("limit"))
                        .and_then(Value::as_u64)
                })
                .unwrap_or(batch_limit);
            let effective_limit = requested_limit.clamp(caps.limit_per_query_min, batch_limit);
            if generic {
                if let Some(query) = query_args.get_mut("query").and_then(Value::as_object_mut) {
                    query.insert("limit".into(), json!(effective_limit));
                }
                if let Some(matched) = query_args.get_mut("match").and_then(Value::as_object_mut) {
                    if matched.contains_key("limit") {
                        matched.insert("limit".into(), json!(effective_limit));
                    }
                }
                query_args.insert("limit".into(), json!(effective_limit));
            } else {
                query_args.entry("compact").or_insert(json!(true));
                query_args.insert("limit".into(), json!(effective_limit));
                query_args.insert("offset".into(), json!(0));
            }
            let is_team_work_template = query_args.get("template").and_then(Value::as_str)
                .map(|template| self.canonical_named_template(template) == "epistemic:team-work-overview")
                .unwrap_or(false);
            let result = if is_team_work_template {
                self.team_work_overview(root, &query_args)?
            } else if generic {
                self.generic_query_locked(root, &query_args)?
            } else {
                self.query_locked(root, &query_args)?
            };
            let returned = result
                .get("returned")
                .cloned()
                .or_else(|| result.get("count").cloned())
                .unwrap_or(Value::Null);
            let query_origin = result
                .get("query_origin")
                .cloned()
                .unwrap_or_else(|| json!("legacy"));
            results.push(json!({
                "index":index,
                "mode":if generic { "datalog" } else { "legacy" },
                "query_origin":query_origin.clone(),
                "request":{
                    "mode":if item.contains_key("query") { "raw" } else if item.contains_key("template") { "named_template" } else { "legacy" },
                    "template":result.get("template").cloned().unwrap_or(Value::Null),
                    "match":item.get("match").cloned().unwrap_or(Value::Null),
                    "kind":item.get("kind").cloned().unwrap_or(Value::Null),
                    "record_kind":item.get("record_kind").cloned().unwrap_or(Value::Null),
                    "text":item.get("text").cloned().unwrap_or(Value::Null)
                },
                "result_schema":result.get("schema").cloned().unwrap_or(Value::Null),
                "ledger_head":result.get("ledger_head").cloned().unwrap_or(Value::Null),
                "text":item.get("text"),
                "kind":item.get("kind"),
                "record_kind":item.get("record_kind"),
                "returned":returned,
                "count":result.get("count").cloned().unwrap_or(returned.clone()),
                "count_semantics":result.get("count_semantics").cloned().unwrap_or_else(|| json!("returned_page")),
                "limit":result.get("limit").cloned().unwrap_or_else(|| json!(effective_limit)),
                "items":result.get("items").cloned().unwrap_or_else(|| json!([])),
                "has_more":result.get("has_more").cloned().unwrap_or_else(|| json!(false)),
                "next_cursor":result.get("next_cursor").cloned().unwrap_or(Value::Null)
            }));
        }
        let mut response = json!({
            "schema":self.schema_id("query_batch.v2"),
            "status":"ok",
            "query_count":queries.len(),
            "limit_per_query":batch_limit,
            "results":results,
            "bounded":true,
            "output_bytes":0,
            "max_output_bytes":self.domain.caps.query_execution.max_output_bytes
        });
        self.finalize_bounded_output(&mut response)?;
        Ok(response)
    }

}
