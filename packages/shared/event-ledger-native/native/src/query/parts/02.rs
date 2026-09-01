fn parse_clause_at(
    value: &Value,
    limits: &QueryLimits,
    predicate_depth: usize,
    clause_count: &mut usize,
) -> Result<Clause, QueryFailure> {
    *clause_count = clause_count.saturating_add(1);
    if *clause_count > limits.max_clauses {
        return Err(QueryFailure::new(
            "query_clause_limit",
            format!(
                "query clauses must contain at most {} clauses in total",
                limits.max_clauses
            ),
            json!({"count":*clause_count,"max":limits.max_clauses}),
        ));
    }
    let object = value.as_object().ok_or_else(|| {
        QueryFailure::new(
            "query_invalid_clause",
            "each where clause must be an object",
            value.clone(),
        )
    })?;
    reject_unknown_keys(
        object,
        &["triple", "compare", "reachable", "exists", "not_exists"],
        "clause",
    )?;
    if object.len() != 1 {
        return Err(QueryFailure::new(
            "query_ambiguous_clause",
            "each where clause must contain exactly one clause form",
            value.clone(),
        ));
    }
    if let Some(triple) = object.get("triple") {
        let triple = triple.as_object().ok_or_else(|| {
            QueryFailure::new(
                "query_invalid_clause",
                "triple clause must be an object",
                triple.clone(),
            )
        })?;
        reject_unknown_keys(
            triple,
            &["subject", "attribute", "object", "value"],
            "triple",
        )?;
        if triple.contains_key("object") == triple.contains_key("value") {
            return Err(QueryFailure::new(
                "query_invalid_clause",
                "triple must contain exactly one object or value field",
                triple.clone().into(),
            ));
        }
        let subject = triple.get("subject").ok_or_else(|| {
            QueryFailure::new(
                "query_invalid_clause",
                "triple subject is required",
                triple.clone().into(),
            )
        })?;
        let attribute = triple.get("attribute").ok_or_else(|| {
            QueryFailure::new(
                "query_invalid_clause",
                "triple attribute is required",
                triple.clone().into(),
            )
        })?;
        let object_value = triple
            .get("object")
            .or_else(|| triple.get("value"))
            .ok_or_else(|| {
                QueryFailure::new(
                    "query_invalid_clause",
                    "triple object is required",
                    triple.clone().into(),
                )
            })?;
        return Ok(Clause::Triple {
            subject: parse_term(subject, limits)?,
            attribute: parse_term(attribute, limits)?,
            object: parse_term(object_value, limits)?,
        });
    }
    if let Some(compare) = object.get("compare") {
        let compare = compare.as_object().ok_or_else(|| {
            QueryFailure::new(
                "query_invalid_clause",
                "compare clause must be an object",
                compare.clone(),
            )
        })?;
        reject_unknown_keys(compare, &["op", "left", "right"], "compare")?;
        let op = compare.get("op").and_then(Value::as_str).ok_or_else(|| {
            QueryFailure::new(
                "query_invalid_clause",
                "compare op is required",
                compare.clone().into(),
            )
        })?;
        if !matches!(op, "=" | "!=" | ">" | ">=" | "<" | "<=") {
            return Err(QueryFailure::new(
                "query_unsupported_operator",
                format!("unsupported comparison operator: {op}"),
                json!({"op":op}),
            ));
        }
        let left = compare.get("left").ok_or_else(|| {
            QueryFailure::new(
                "query_invalid_clause",
                "compare left is required",
                compare.clone().into(),
            )
        })?;
        let right = compare.get("right").ok_or_else(|| {
            QueryFailure::new(
                "query_invalid_clause",
                "compare right is required",
                compare.clone().into(),
            )
        })?;
        return Ok(Clause::Compare {
            op: op.to_string(),
            left: parse_term(left, limits)?,
            right: parse_term(right, limits)?,
        });
    }
    if let Some(reachable) = object.get("reachable") {
        let reachable = reachable.as_object().ok_or_else(|| {
            QueryFailure::new(
                "query_invalid_clause",
                "reachable clause must be an object",
                reachable.clone(),
            )
        })?;
        reject_unknown_keys(
            reachable,
            &["from", "to", "attribute", "max_depth"],
            "reachable",
        )?;
        let from = reachable.get("from").ok_or_else(|| {
            QueryFailure::new(
                "query_invalid_clause",
                "reachable from is required",
                reachable.clone().into(),
            )
        })?;
        let to = reachable.get("to").ok_or_else(|| {
            QueryFailure::new(
                "query_invalid_clause",
                "reachable to is required",
                reachable.clone().into(),
            )
        })?;
        let attribute = reachable
            .get("attribute")
            .and_then(Value::as_str)
            .ok_or_else(|| {
                QueryFailure::new(
                    "query_invalid_clause",
                    "reachable attribute is required",
                    reachable.clone().into(),
                )
            })?;
        let requested_depth = reachable
            .get("max_depth")
            .map(|value| {
                value.as_u64().ok_or_else(|| {
                    QueryFailure::new(
                        "query_invalid_clause",
                        "reachable max_depth must be a positive integer",
                        value.clone(),
                    )
                })
            })
            .transpose()?
            .unwrap_or(1) as usize;
        let max_depth = requested_depth.min(limits.max_reach_depth);
        if max_depth == 0 {
            return Err(QueryFailure::new(
                "query_invalid_clause",
                "reachable max_depth must be positive",
                reachable.clone().into(),
            ));
        }
        return Ok(Clause::Reachable {
            from: parse_term(from, limits)?,
            attribute: attribute.to_string(),
            to: parse_term(to, limits)?,
            max_depth,
        });
    }
    for (key, constructor) in [("exists", true), ("not_exists", false)] {
        let Some(nested) = object.get(key) else {
            continue;
        };
        let next_predicate_depth = predicate_depth.saturating_add(1);
        if next_predicate_depth > limits.max_predicate_depth {
            return Err(QueryFailure::new(
                "query_predicate_depth_limit",
                format!(
                    "nested predicates may not exceed {} levels",
                    limits.max_predicate_depth
                ),
                json!({"depth":next_predicate_depth,"max":limits.max_predicate_depth}),
            ));
        }
        let nested_object = nested.as_object().ok_or_else(|| {
            QueryFailure::new(
                "query_invalid_clause",
                format!("{key} clause must be an object"),
                nested.clone(),
            )
        })?;
        reject_unknown_keys(
            nested_object,
            &[
                "where",
                "triple",
                "compare",
                "reachable",
                "exists",
                "not_exists",
            ],
            key,
        )?;
        let nested_values = if let Some(where_values) = nested_object.get("where") {
            if nested_object.len() != 1 {
                return Err(QueryFailure::new(
                    "query_ambiguous_clause",
                    format!("{key} must contain either where or one nested clause, not both"),
                    nested.clone(),
                ));
            }
            where_values
                .as_array()
                .ok_or_else(|| {
                    QueryFailure::new(
                        "query_invalid_clause",
                        format!("{key}.where must be an array"),
                        where_values.clone(),
                    )
                })?
                .clone()
        } else {
            if nested_object.len() != 1 {
                return Err(QueryFailure::new(
                    "query_ambiguous_clause",
                    format!("{key} must contain exactly one nested clause"),
                    nested.clone(),
                ));
            }
            vec![Value::Object(nested_object.clone())]
        };
        if nested_values.is_empty() || nested_values.len() > limits.max_clauses {
            return Err(QueryFailure::new(
                "query_clause_limit",
                format!(
                    "{key}.where must contain between 1 and {} clauses",
                    limits.max_clauses
                ),
                json!({"count":nested_values.len(),"max":limits.max_clauses}),
            ));
        }
        let clauses = nested_values
            .iter()
            .map(|clause| parse_clause_at(clause, limits, next_predicate_depth, clause_count))
            .collect::<Result<Vec<_>, _>>()?;
        return Ok(if constructor {
            Clause::Exists { clauses }
        } else {
            Clause::NotExists { clauses }
        });
    }
    Err(QueryFailure::new(
        "query_unsupported_clause",
        "supported clauses are triple, compare, reachable, exists, and not_exists",
        value.clone(),
    ))
}

