impl Engine {
    fn generic_query_locked(&self, root: &Path, args: &Map<String, Value>) -> Result<Value, Value> {
        if args.contains_key("budget_escalation") {
            return Err(self.error(
                "query_budget_escalation_unavailable",
                "this surface has no descriptor-admitted privileged query budget",
                json!({"required":"descriptor-owned maintenance authority with audit evidence"}),
            ));
        }
        let Some(datoms_table) = &self.datoms_table else {
            return Err(self.error(
                "query_unavailable",
                "this domain does not expose a normalized datom projection",
                Value::Null,
            ));
        };
        let head = self.ledger_head(root)?;
        if let Some(expected) = args.get("expected_ledger_head").and_then(Value::as_str) {
            if Some(expected) != head.as_deref() {
                return Err(self.error(
                    "ledger_head_mismatch",
                    "query expected_ledger_head does not match the current ledger head",
                    json!({"expected_ledger_head":expected,"actual_ledger_head":head}),
                ));
            }
        }
        let mut query_value = if let Some(query) = args.get("query") {
            query.clone()
        } else {
            self.named_query(args)?
        };
        if !args.contains_key("query") {
            self.expand_named_identity_terms(root, args, &mut query_value)?;
        }
        let raw_cursor = args
            .get("cursor")
            .cloned()
            .or_else(|| query_value.get("cursor").cloned())
            .unwrap_or(Value::Null);
        let cursor_schema = self.schema_id("cursor.v1");
        let cursor_value = if raw_cursor.is_null() {
            Value::Null
        } else {
            decode_cursor_token(&raw_cursor, &cursor_schema).map_err(|_| {
                self.error(
                    "query_cursor_invalid",
                    "cursor must be a valid v1 opaque cursor token or legacy cursor object",
                    json!({"cursor_schema":cursor_schema}),
                )
            })?
        };
        if let Some(query_object) = query_value.as_object_mut() {
            query_object.insert("cursor".into(), cursor_value.clone());
            if !query_object.contains_key("limit") {
                if let Some(limit) = args.get("limit") {
                    query_object.insert("limit".into(), limit.clone());
                }
            }
        }
        let query_scope = {
            let mut scope = query_value.clone();
            if let Some(scope_object) = scope.as_object_mut() {
                scope_object.remove("cursor");
                scope_object.remove("limit");
                if let Some(find) = scope_object.get_mut("find").and_then(Value::as_array_mut) {
                    for term in find {
                        if let Some(pull) = term
                            .as_object_mut()
                            .and_then(|object| object.get_mut("pull"))
                            .and_then(Value::as_object_mut)
                        {
                            // Projection fields are presentation, not
                            // result identity; callers may add/remove a
                            // body pull while continuing the same page.
                            pull.remove("fields");
                        }
                    }
                }
            }
            sha256(&serde_json::to_vec(&canonical_json(&scope)).unwrap_or_default())
        };
        let cursor_ref = (!cursor_value.is_null()).then_some(&cursor_value);
        let cursor_head = cursor_ref
            .and_then(|cursor| cursor.get("head"))
            .and_then(Value::as_str);
        let cursor_has_values = cursor_ref
            .and_then(|cursor| cursor.get("values"))
            .and_then(Value::as_object)
            .map(|values| !values.is_empty())
            .unwrap_or(false);
        if cursor_has_values && cursor_head.is_none() {
            return Err(self.error(
                "query_cursor_unpinned",
                "cursor pagination requires the ledger head returned with the previous page",
                Value::Null,
            ));
        }
        if let Some(cursor_head) = cursor_head {
            if Some(cursor_head) != head.as_deref() {
                return Err(self.error(
                    "query_cursor_stale",
                    "query cursor belongs to a different ledger head",
                    json!({"cursor_head":cursor_head,"actual_ledger_head":head}),
                ));
            }
        }
        if let Some(cursor_scope) = cursor_ref
            .and_then(|cursor| cursor.get("query"))
            .and_then(Value::as_str)
        {
            if cursor_scope != query_scope {
                return Err(self.error(
                    "query_cursor_scope_mismatch",
                    "query cursor belongs to a different query shape",
                    json!({"cursor_query":cursor_scope,"actual_query":query_scope}),
                ));
            }
        }
        let hard_max_datoms = self.domain.caps.query_execution.max_datoms_scanned;
        let hard_max_results = self.domain.caps.query_limit.max;
        let hard_timeout_ms = self.domain.caps.query_execution.max_timeout_ms;
        let effective_max_datoms = args
            .get("max_datoms")
            .and_then(Value::as_u64)
            .unwrap_or(hard_max_datoms)
            .min(hard_max_datoms);
        let effective_max_results = args
            .get("max_results")
            .and_then(Value::as_u64)
            .unwrap_or(hard_max_results)
            .min(hard_max_results);
        let effective_timeout_ms = args
            .get("timeout_ms")
            .and_then(Value::as_u64)
            .unwrap_or(hard_timeout_ms)
            .min(hard_timeout_ms);
        let started = Instant::now();
        let limits = ledger_query::QueryLimits {
            max_clauses: self.domain.query.max_clauses.unwrap_or(64).max(1),
            max_results: effective_max_results as usize,
            max_reach_depth: self.domain.query.max_reach_depth.unwrap_or(8).max(1),
            max_one_of_values: self.domain.query.max_one_of_values.unwrap_or(64).max(1),
            max_predicate_depth: self.domain.query.max_predicate_depth.unwrap_or(8).max(1),
            max_datoms_scanned: effective_max_datoms as usize,
            max_traversal_edges: self.domain.caps.query_execution.max_traversal_edges as usize,
        };
        let default_limit = self.domain.caps.query_limit.default as usize;
        let spec = ledger_query::parse(&query_value, default_limit, &limits)
            .map_err(|failure| self.error(failure.code, &failure.message, failure.details))?;
        let db = Connection::open(self.projection_path(root))
            .map_err(self.db_error("projection_open_failed"))?;
        let planner_value = |key: &str| {
            args.get(key).or_else(|| {
                args.get("match")
                    .and_then(Value::as_object)
                    .and_then(|matched| matched.get(key))
            })
        };
        let has_inbox_participant = planner_value("participant")
            .or_else(|| planner_value("recipient"))
            .and_then(Value::as_str)
            .is_some_and(|value| !value.trim().is_empty());
        let explicit_sequence_lower_bound = planner_value("after_sequence")
            .or_else(|| planner_value("since_event"))
            .and_then(Value::as_u64);
        let sequence_lower_bound = explicit_sequence_lower_bound
            // Event sequences are positive.  For a recipient-scoped
            // named inbox query, an omitted lower bound therefore has
            // the same semantics as `after_sequence: 0`, while enabling
            // subject-local hydration instead of global decoration scans.
            .or_else(|| has_inbox_participant.then_some(0));
        let named_query_config = args
            .get("template")
            .and_then(Value::as_str)
            .map(|template| self.canonical_named_template(template))
            .and_then(|template| self.domain.query.named_queries.get(&template))
            .and_then(Value::as_object);
        let participant_direction = planner_value("direction")
            .and_then(Value::as_str)
            .unwrap_or("incoming");
        let subject_seed_attribute = named_query_config
            .and_then(|config| config.get("participant_attributes"))
            .and_then(Value::as_object)
            .and_then(|attributes| attributes.get(participant_direction))
            .and_then(Value::as_str)
            .filter(|_| has_inbox_participant);
        let subject_local_sequence = named_query_config
            .and_then(|config| config.get("sequence_attribute"))
            .and_then(Value::as_str)
            .zip(sequence_lower_bound)
            .filter(|_| subject_seed_attribute.is_some());
        let datoms = self
            .load_datoms_for_query(
                &db,
                datoms_table,
                &spec,
                subject_local_sequence,
                subject_seed_attribute,
            )
            .map_err(|error| {
                self.with_inbox_sequence_remediation(
                    error,
                    explicit_sequence_lower_bound.is_some(),
                    subject_seed_attribute.is_some(),
                )
            })?;
        if started.elapsed() > Duration::from_millis(effective_timeout_ms) {
            return Err(self.error("query_timeout", "query exceeded its capped time budget while loading indexed datoms", json!({"timeout_ms":effective_timeout_ms,"phase":"datom_load","datoms_loaded":datoms.len()})));
        }
        let execution = ledger_query::execute(&spec, &datoms).map_err(|failure| {
            self.with_inbox_sequence_remediation(
                self.error(failure.code, &failure.message, failure.details),
                explicit_sequence_lower_bound.is_some(),
                subject_seed_attribute.is_some(),
            )
        })?;
        if started.elapsed() > Duration::from_millis(effective_timeout_ms) {
            return Err(self.error(
                    "query_timeout",
                    "query exceeded its capped time budget while evaluating datoms",
                    json!({"timeout_ms":effective_timeout_ms,"phase":"evaluation","datoms_loaded":datoms.len()}),
                ));
        }
        if execution.has_more && spec.order_by.is_empty() {
            return Err(self.error(
                "query_pagination_requires_order",
                "a query that exceeds its limit must declare order_by for continuation",
                json!({"limit":spec.limit}),
            ));
        }
        let mut items = execution
            .bindings
            .iter()
            .map(|binding| self.render_query_binding(&db, binding, &spec, &datoms))
            .collect::<Result<Vec<_>, _>>()?;
        let normalized_legacy_count = items
            .iter_mut()
            .map(|item| self.normalize_communication_result(item))
            .filter(|count| *count > 0)
            .count();
        let next_cursor = execution.bindings.last().and_then(|binding| {
            if !execution.has_more {
                return None;
            }
            let mut values = Map::new();
            for order in &spec.order_by {
                if let Some(variable) = order.term.as_variable_name() {
                    if let Some(value) = binding.get(variable) {
                        values.insert(variable.to_string(), value.clone());
                    }
                }
            }
            Some(Value::String(encode_cursor_token(&json!({
                "schema":cursor_schema,
                "head":head,
                "query":query_scope,
                "values":values
            }))))
        });
        let response_template = args
            .get("template")
            .and_then(Value::as_str)
            .map(|template| Value::String(self.canonical_named_template(template)))
            .unwrap_or(Value::Null);
        let query_origin = if args.contains_key("query") {
            "raw"
        } else {
            "named_template"
        };
        let mut response = json!({
            "schema":self.schema_id("query.v2"),
            "query_mode":"datalog",
            "query_origin":query_origin,
            "template":response_template,
            "ledger_head":head,
            "items":items,
            "count":execution.bindings.len(),
            "returned_count":execution.bindings.len(),
            "count_semantics":"returned_page",
            "limit":spec.limit,
            "output_bytes":0,
            "max_output_bytes":self.domain.caps.query_execution.max_output_bytes,
            "has_more":execution.has_more,
            "next_cursor":next_cursor,
            "normalization":{"applied":normalized_legacy_count > 0,"normalized_count":normalized_legacy_count,"canonical_kind":self.domain.query.communication.canonical_kind,"legacy_read_policy":self.domain.query.communication.legacy_read_policy,"contract_version":self.domain.query.communication.contract_version},
            "query_cost":{"planner_mode":if subject_local_sequence.is_some() {"indexed_subject_suffix"} else {"bounded_clause_plan"},"subject_local_attribute":subject_local_sequence.map(|(attribute, _)| attribute),"datoms_loaded":datoms.len(),"max_datoms":effective_max_datoms,"max_results":effective_max_results,"timeout_ms":effective_timeout_ms,"elapsed_ms":started.elapsed().as_millis() as u64,"hard_caps":{"max_datoms":hard_max_datoms,"max_results":hard_max_results,"timeout_ms":hard_timeout_ms}}
        });
        self.finalize_bounded_output(&mut response)?;
        Ok(response)
    }

}
