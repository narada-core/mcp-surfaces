impl Engine {
    fn named_query(&self, args: &Map<String, Value>) -> Result<Value, Value> {
        let template = match args.get("template") {
            None => {
                return Err(self.error(
                    "query_template_missing",
                    "template is required when query is omitted",
                    Value::Null,
                ));
            }
            Some(value) => value.as_str().ok_or_else(|| {
                self.error(
                    "query_filter_type_invalid",
                    "template must be a non-empty string",
                    json!({"field":"template"}),
                )
            })?,
        };
        let canonical_template = self.canonical_named_template(template);
        let config = self
            .domain
            .query
            .named_queries
            .get(&canonical_template)
            .and_then(Value::as_object)
            .ok_or_else(|| {
                self.error(
                    "query_template_unknown",
                    "unknown named query template",
                    json!({"template":template,"canonical_template":canonical_template}),
                )
            })?;
        let mode = config.get("mode").and_then(Value::as_str).ok_or_else(|| {
            self.error(
                "query_template_invalid",
                "named query template lacks mode",
                json!({"template":canonical_template}),
            )
        })?;
        self.validate_named_filter_conflicts(args)?;
        self.validate_named_query_fields(args, mode)?;
        self.validate_named_filter_types(args, mode)?;
        let config_string = |key: &str| {
            config.get(key).and_then(Value::as_str).ok_or_else(|| {
                self.error(
                    "query_template_invalid",
                    "named query template lacks required string configuration",
                    json!({"template":canonical_template,"field":key}),
                )
            })
        };
        let config_fields = || {
            config
                .get("fields")
                .and_then(Value::as_array)
                .ok_or_else(|| {
                    self.error(
                        "query_template_invalid",
                        "named query template lacks fields",
                        json!({"template":canonical_template}),
                    )
                })
        };
        let match_value = |key: &str| {
            args.get(key).or_else(|| {
                args.get("match")
                    .and_then(Value::as_object)
                    .and_then(|matched| matched.get(key))
            })
        };
        let limit = match_value("limit")
            .cloned()
            .unwrap_or_else(|| json!(self.domain.caps.query_limit.default));
        let cursor = args.get("cursor").cloned().unwrap_or(Value::Null);
        match mode {
            "inbox" => {
                let canonical_participant = match_value("participant")
                    .and_then(Value::as_str)
                    .filter(|value| !value.trim().is_empty());
                let legacy_participant = match_value("recipient")
                    .and_then(Value::as_str)
                    .filter(|value| !value.trim().is_empty());
                let participant = match (canonical_participant, legacy_participant) {
                    (Some(canonical), Some(legacy)) if canonical != legacy => {
                        return Err(self.error(
                            "query_participant_conflict",
                            "participant and legacy recipient must agree when both are supplied",
                            json!({"participant":canonical,"recipient":legacy}),
                        ));
                    }
                    (Some(value), _) | (_, Some(value)) => value,
                    (None, None) => {
                        return Err(self.error(
                            "query_recipient_missing",
                            "inbox requires participant (or legacy recipient)",
                            Value::Null,
                        ));
                    }
                };
                let direction = match_value("direction")
                    .and_then(Value::as_str)
                    .unwrap_or("incoming");
                let participant_attribute = config
                    .get("participant_attributes")
                    .and_then(Value::as_object)
                    .and_then(|attributes| attributes.get(direction))
                    .and_then(Value::as_str)
                    .ok_or_else(|| {
                        self.error(
                            "query_direction_invalid",
                            "direction is not supported by this inbox template",
                            json!({"template":canonical_template,"direction":direction}),
                        )
                    })?;
                let kind_key = config_string("kind_key")?;
                let kinds = match_value("kinds")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_else(|| vec![Value::String(kind_key.to_string())]);
                let kinds = self.expand_kind_values(kinds)?;
                let since_event = match_value("since_event").and_then(Value::as_u64);
                let after_sequence = match_value("after_sequence").and_then(Value::as_u64);
                if let (Some(since_event), Some(after_sequence)) = (since_event, after_sequence) {
                    if since_event != after_sequence {
                        return Err(self.error(
                            "query_sequence_filter_conflict",
                            "since_event and after_sequence must agree when both are supplied",
                            json!({"since_event":since_event,"after_sequence":after_sequence}),
                        ));
                    }
                }
                let since = after_sequence.or(since_event).unwrap_or(0);
                let body_field = config.get("body_field").and_then(Value::as_str);
                let include_body = match_value("include_body")
                    .and_then(Value::as_bool)
                    .unwrap_or(true);
                let fields = config_fields()?
                    .iter()
                    .filter_map(|field| field.as_str())
                    .filter(|field| include_body || Some(*field) != body_field)
                    .map(Value::from)
                    .collect::<Vec<_>>();
                let ledger_kind = config_string("kind_attribute")?;
                let ledger_sequence = config_string("sequence_attribute")?;
                let ledger_event_id = config_string("event_id_attribute")?;
                let viewer = match_value("viewer")
                    .and_then(Value::as_str)
                    .filter(|value| !value.trim().is_empty())
                    .unwrap_or(participant);
                let sender_alias = match_value("sender").and_then(Value::as_str);
                let from_alias = match_value("from").and_then(Value::as_str);
                if let (Some(sender), Some(from)) = (sender_alias, from_alias) {
                    if sender != from {
                        return Err(self.error(
                            "query_sender_conflict",
                            "sender and legacy from must agree when both are supplied",
                            json!({"sender":sender,"from":from}),
                        ));
                    }
                }
                let mut inputs = json!({
                    "participant":participant,
                    "viewer":viewer,
                    "after_sequence":since
                });
                let mut where_clauses = vec![
                    json!({"triple":{"subject":"?message","attribute":ledger_kind,"object":{"one_of":kinds}}}),
                    json!({"triple":{"subject":"?message","attribute":participant_attribute,"object":{"input":"participant"}}}),
                    json!({"triple":{"subject":"?message","attribute":ledger_sequence,"object":"?sequence"}}),
                    json!({"triple":{"subject":"?message","attribute":ledger_event_id,"object":"?event_id"}}),
                    json!({"compare":{"op":">","left":"?sequence","right":{"input":"after_sequence"}}}),
                ];
                let sender_value = match_value("sender").or_else(|| match_value("from"));
                if let Some(sender) = sender_value.and_then(Value::as_str) {
                    inputs["sender"] = json!(sender);
                    let sender_attribute = config
                        .get("participant_attributes")
                        .and_then(Value::as_object)
                        .and_then(|attributes| attributes.get("outgoing"))
                        .and_then(Value::as_str)
                        .ok_or_else(|| {
                            self.error(
                                "query_template_invalid",
                                "inbox template lacks an outgoing participant attribute",
                                json!({"template":canonical_template}),
                            )
                        })?;
                    where_clauses.push(json!({"triple":{"subject":"?message","attribute":sender_attribute,"object":{"input":"sender"}}}));
                }
                if let Some(recipient) = match_value("to").and_then(Value::as_str) {
                    let recipient_attribute = config
                        .get("participant_attributes")
                        .and_then(Value::as_object)
                        .and_then(|attributes| attributes.get("incoming"))
                        .and_then(Value::as_str)
                        .ok_or_else(|| {
                            self.error(
                                "query_template_invalid",
                                "inbox template lacks an incoming participant attribute",
                                json!({"template":canonical_template}),
                            )
                        })?;
                    inputs["to"] = json!(recipient);
                    where_clauses.push(json!({"triple":{"subject":"?message","attribute":recipient_attribute,"object":{"input":"to"}}}));
                }
                if let Some(intent) = match_value("intent").and_then(Value::as_str) {
                    let intent_attribute = config_string("intent_attribute")?;
                    inputs["intent"] = json!(intent);
                    where_clauses.push(json!({"triple":{"subject":"?message","attribute":intent_attribute,"object":{"input":"intent"}}}));
                }
                let read_state = match_value("read_state")
                    .and_then(Value::as_str)
                    .unwrap_or("all");
                if !matches!(read_state, "all" | "read" | "unread") {
                    return Err(self.error(
                        "query_read_state_invalid",
                        "read_state must be all, read, or unread",
                        json!({"read_state":read_state}),
                    ));
                }
                if read_state != "all" {
                    let receipt_kind = self.domain.query.read_receipt_kind.as_deref();
                    let receipt_kind_attribute = self
                        .domain
                        .query
                        .read_receipt_kind_attribute
                        .as_deref()
                        .unwrap_or(ledger_kind);
                    let receipt_message_attribute =
                        self.domain.query.read_receipt_message_attribute.as_deref();
                    let receipt_reader_attribute =
                        self.domain.query.read_receipt_reader_attribute.as_deref();
                    let (
                        Some(receipt_kind),
                        Some(receipt_message_attribute),
                        Some(receipt_reader_attribute),
                    ) = (
                        receipt_kind,
                        receipt_message_attribute,
                        receipt_reader_attribute,
                    )
                    else {
                        return Err(self.error(
                            "message_state_unavailable",
                            "this domain does not configure durable message read receipts",
                            Value::Null,
                        ));
                    };
                    let receipt_where = json!({"where":[
                        {"triple":{"subject":"?receipt","attribute":receipt_kind_attribute,"object":receipt_kind}},
                        {"triple":{"subject":"?receipt","attribute":receipt_message_attribute,"object":"?message"}},
                        {"triple":{"subject":"?receipt","attribute":receipt_reader_attribute,"object":{"input":"viewer"}}}
                    ]});
                    where_clauses.push(if read_state == "read" {
                        json!({"exists":receipt_where})
                    } else {
                        json!({"not_exists":receipt_where})
                    });
                }
                let reply_state = match_value("reply_state")
                    .and_then(Value::as_str)
                    .unwrap_or("all");
                if !matches!(reply_state, "all" | "replied" | "unreplied") {
                    return Err(self.error(
                        "query_reply_state_invalid",
                        "reply_state must be all, replied, or unreplied",
                        json!({"reply_state":reply_state}),
                    ));
                }
                if reply_state != "all" {
                    let reply_attribute = self
                        .domain
                        .query
                        .reply_state_attribute
                        .as_deref()
                        .ok_or_else(|| {
                            self.error(
                                "reply_state_unavailable",
                                "this domain does not configure reply relations",
                                Value::Null,
                            )
                        })?;
                    let reply_where = json!({"where":[
                        {"triple":{"subject":"?reply","attribute":reply_attribute,"object":"?message"}}
                    ]});
                    where_clauses.push(if reply_state == "replied" {
                        json!({"exists":reply_where})
                    } else {
                        json!({"not_exists":reply_where})
                    });
                }
                Ok(json!({
                    "find":[{"pull":{"var":"?message","fields":fields}}],
                    "inputs":inputs,
                    "where":where_clauses,
                    "order_by":[
                        {"term":"?sequence","direction":"asc"},
                        {"term":"?event_id","direction":"asc"},
                        {"term":"?message","direction":"asc"}
                    ],
                    "limit":limit,
                    "cursor":cursor
                }))
            }
            "thread" => {
                let root_id = args
                    .get("root")
                    .and_then(Value::as_str)
                    .filter(|value| !value.trim().is_empty())
                    .ok_or_else(|| {
                        self.error("query_root_missing", "thread requires root", Value::Null)
                    })?;
                let fields = config_fields()?
                    .iter()
                    .filter_map(|field| field.as_str())
                    .map(Value::from)
                    .collect::<Vec<_>>();
                let relation_attribute = config_string("relation_attribute")?;
                let ledger_sequence = config_string("sequence_attribute")?;
                let ledger_event_id = config_string("event_id_attribute")?;
                let viewer = match_value("viewer")
                    .and_then(Value::as_str)
                    .filter(|value| !value.trim().is_empty())
                    .unwrap_or("");
                let configured_depth = config.get("max_depth").and_then(Value::as_u64).unwrap_or(8);
                let max_depth = args
                    .get("max_depth")
                    .and_then(Value::as_u64)
                    .unwrap_or(configured_depth);
                Ok(json!({
                    "find":[{"pull":{"var":"?message","fields":fields}}],
                    "inputs":{"root":root_id,"viewer":viewer},
                    "where":[
                        {"reachable":{"from":{"input":"root"},"attribute":relation_attribute,"to":"?message","max_depth":max_depth}},
                        {"triple":{"subject":"?message","attribute":ledger_sequence,"object":"?sequence"}},
                        {"triple":{"subject":"?message","attribute":ledger_event_id,"object":"?event_id"}}
                    ],
                    "order_by":[
                        {"term":"?sequence","direction":"asc"},
                        {"term":"?event_id","direction":"asc"},
                        {"term":"?message","direction":"asc"}
                    ],
                    "limit":limit,
                    "cursor":cursor
                }))
            }
            _ => Err(self.error(
                "query_template_invalid",
                "named query template mode is unsupported",
                json!({"template":canonical_template,"mode":mode}),
            )),
        }
    }

}
