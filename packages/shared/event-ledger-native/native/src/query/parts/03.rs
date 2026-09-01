pub fn parse(
    value: &Value,
    default_limit: usize,
    limits: &QueryLimits,
) -> Result<QuerySpec, QueryFailure> {
    let object = value.as_object().ok_or_else(|| {
        QueryFailure::new("query_invalid", "query must be an object", value.clone())
    })?;
    for key in object.keys() {
        if !["find", "inputs", "where", "order_by", "limit", "cursor"].contains(&key.as_str()) {
            return Err(QueryFailure::new(
                "query_invalid_field",
                format!("query field is not supported: {key}"),
                json!({"field":key}),
            ));
        }
    }
    let inputs = match object.get("inputs") {
        None => Map::new(),
        Some(Value::Object(inputs)) => inputs.clone(),
        Some(value) => {
            return Err(QueryFailure::new(
                "query_invalid_inputs",
                "inputs must be an object",
                value.clone(),
            ));
        }
    };
    if inputs.len() > limits.max_clauses {
        return Err(QueryFailure::new(
            "query_input_limit",
            format!(
                "inputs must contain at most {} variables",
                limits.max_clauses
            ),
            json!({"count":inputs.len(),"max":limits.max_clauses}),
        ));
    }
    let mut normalized_inputs = HashSet::new();
    for key in inputs.keys() {
        let normalized = variable_name(key).map_err(|failure| {
            QueryFailure::new(
                "query_invalid_input",
                "input names must be non-empty query variables",
                json!({"input":key,"cause":failure.code}),
            )
        })?;
        if !normalized_inputs.insert(normalized.clone()) {
            return Err(QueryFailure::new(
                "query_duplicate_input",
                "input names must not normalize to the same query variable",
                json!({"input":key,"variable":normalized}),
            ));
        }
    }
    let find_values = object
        .get("find")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            QueryFailure::new("query_invalid_find", "find must be an array", Value::Null)
        })?;
    if find_values.is_empty() {
        return Err(QueryFailure::new(
            "query_invalid_find",
            "find must not be empty",
            Value::Null,
        ));
    }
    if find_values.len() > limits.max_clauses {
        return Err(QueryFailure::new(
            "query_find_limit",
            format!("find must contain at most {} terms", limits.max_clauses),
            json!({"count":find_values.len(),"max":limits.max_clauses}),
        ));
    }
    let mut finds = Vec::new();
    let mut pulls = Vec::new();
    for item in find_values {
        if let Some(pull) = item.get("pull") {
            if let Some(item_object) = item.as_object() {
                reject_unknown_keys(item_object, &["pull"], "find")?;
            }
            let pull = pull.as_object().ok_or_else(|| {
                QueryFailure::new("query_invalid_pull", "pull must be an object", pull.clone())
            })?;
            reject_unknown_keys(pull, &["var", "variable", "target_kind", "fields"], "pull")?;
            let variable = pull
                .get("var")
                .or_else(|| pull.get("variable"))
                .and_then(Value::as_str)
                .ok_or_else(|| {
                    QueryFailure::new(
                        "query_invalid_pull",
                        "pull variable is required",
                        pull.clone().into(),
                    )
                })?;
            let target_kind = match pull.get("target_kind") {
                None => None,
                Some(value) => {
                    let target_kind = value.as_str().ok_or_else(|| {
                        QueryFailure::new(
                            "query_invalid_pull",
                            "pull target_kind must be entity, relation, or record",
                            value.clone(),
                        )
                    })?;
                    if !matches!(target_kind, "entity" | "relation" | "record") {
                        return Err(QueryFailure::new(
                            "query_invalid_pull",
                            "pull target_kind must be entity, relation, or record",
                            value.clone(),
                        ));
                    }
                    Some(target_kind.to_string())
                }
            };
            let fields = pull
                .get("fields")
                .and_then(Value::as_array)
                .ok_or_else(|| {
                    QueryFailure::new(
                        "query_invalid_pull",
                        "pull fields must be an array",
                        pull.clone().into(),
                    )
                })?
                .iter()
                .map(|field| {
                    field.as_str().map(str::to_string).ok_or_else(|| {
                        QueryFailure::new(
                            "query_invalid_pull",
                            "pull field must be a string",
                            field.clone(),
                        )
                    })
                })
                .collect::<Result<Vec<_>, _>>()?;
            if fields.is_empty() || fields.len() > limits.max_clauses {
                return Err(QueryFailure::new(
                    "query_pull_field_limit",
                    format!(
                        "pull fields must contain between 1 and {} fields",
                        limits.max_clauses
                    ),
                    json!({"count":fields.len(),"max":limits.max_clauses}),
                ));
            }
            let variable = variable_name(variable)?;
            pulls.push(PullSpec {
                variable: variable.clone(),
                fields,
                target_kind,
            });
            finds.push(Term::Variable(variable));
        } else {
            finds.push(parse_term(item, limits)?);
        }
    }
    if finds.iter().any(|term| term.as_variable_name().is_none()) {
        return Err(QueryFailure::new(
            "query_find_requires_variable",
            "find terms must be variables or pull expressions",
            Value::Null,
        ));
    }
    let mut clause_count = 0usize;
    let clauses = object
        .get("where")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            QueryFailure::new("query_invalid_where", "where must be an array", Value::Null)
        })?
        .iter()
        .map(|clause| parse_clause_at(clause, limits, 0, &mut clause_count))
        .collect::<Result<Vec<_>, _>>()?;
    if clauses.is_empty() || clauses.len() > limits.max_clauses {
        return Err(QueryFailure::new(
            "query_clause_limit",
            format!(
                "where must contain between 1 and {} clauses",
                limits.max_clauses
            ),
            json!({"count":clauses.len(),"max":limits.max_clauses}),
        ));
    }
    let empty_order = Vec::new();
    let order_values = match object.get("order_by") {
        None => &empty_order,
        Some(Value::Array(order_values)) => order_values,
        Some(value) => {
            return Err(QueryFailure::new(
                "query_invalid_order",
                "order_by must be an array",
                value.clone(),
            ));
        }
    };
    if order_values.len() > limits.max_clauses {
        return Err(QueryFailure::new(
            "query_order_limit",
            format!("order_by must contain at most {} terms", limits.max_clauses),
            json!({"count":order_values.len(),"max":limits.max_clauses}),
        ));
    }
    let order_by = order_values
        .iter()
        .map(|item| {
            let item = item.as_object().ok_or_else(|| {
                QueryFailure::new(
                    "query_invalid_order",
                    "order_by item must be an object",
                    item.clone(),
                )
            })?;
            reject_unknown_keys(item, &["term", "direction"], "order_by")?;
            let term = item.get("term").ok_or_else(|| {
                QueryFailure::new(
                    "query_invalid_order",
                    "order_by term is required",
                    item.clone().into(),
                )
            })?;
            let direction = match item.get("direction") {
                None => "asc",
                Some(Value::String(direction)) => direction.as_str(),
                Some(value) => {
                    return Err(QueryFailure::new(
                        "query_invalid_order",
                        "order direction must be asc or desc",
                        value.clone(),
                    ));
                }
            };
            if !matches!(direction, "asc" | "desc") {
                return Err(QueryFailure::new(
                    "query_invalid_order",
                    "order direction must be asc or desc",
                    item.clone().into(),
                ));
            }
            Ok(OrderTerm {
                term: parse_term(term, limits)?,
                descending: direction == "desc",
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    let limit = match object.get("limit") {
        None => default_limit,
        Some(value) => value.as_u64().map(|value| value as usize).ok_or_else(|| {
            QueryFailure::new(
                "query_invalid_limit",
                "limit must be a positive integer",
                value.clone(),
            )
        })?,
    };
    if limit == 0 || limit > limits.max_results {
        return Err(QueryFailure::new(
            "query_limit_exceeded",
            format!("query limit must be between 1 and {}", limits.max_results),
            json!({"limit":limit,"max":limits.max_results}),
        ));
    }
    let cursor_values = match object.get("cursor") {
        None | Some(Value::Null) => Map::new(),
        Some(Value::Object(cursor)) => match cursor.get("values") {
            None => Map::new(),
            Some(Value::Object(values)) => values.clone(),
            Some(value) => {
                return Err(QueryFailure::new(
                    "query_cursor_invalid",
                    "cursor values must be an object",
                    value.clone(),
                ));
            }
        },
        Some(value) => {
            return Err(QueryFailure::new(
                "query_cursor_invalid",
                "cursor must be an object or null after transport decoding",
                value.clone(),
            ));
        }
    };
    if let Some(cursor) = object.get("cursor") {
        if let Some(cursor_object) = cursor.as_object() {
            reject_unknown_keys(
                cursor_object,
                &["schema", "head", "query", "values"],
                "cursor",
            )?;
        } else if !cursor.is_null() {
            return Err(QueryFailure::new(
                "query_cursor_invalid",
                "cursor must be an object or null after transport decoding",
                cursor.clone(),
            ));
        }
    }
    if order_by
        .iter()
        .any(|order| order.term.as_variable_name().is_none())
    {
        return Err(QueryFailure::new(
            "query_order_requires_variable",
            "order_by terms must be variables so cursors can advance",
            Value::Null,
        ));
    }
    if !cursor_values.is_empty() && order_by.is_empty() {
        return Err(QueryFailure::new(
            "query_cursor_requires_order",
            "cursor pagination requires at least one order_by term",
            Value::Null,
        ));
    }
    if !cursor_values.is_empty() {
        for order in &order_by {
            let variable = order.term.as_variable_name().unwrap();
            if !cursor_values.contains_key(variable) {
                return Err(QueryFailure::new(
                    "query_cursor_incomplete",
                    "cursor must contain a value for every order_by variable",
                    json!({"variable":variable}),
                ));
            }
        }
    }
    Ok(QuerySpec {
        inputs,
        finds,
        pulls,
        clauses,
        order_by,
        limit,
        cursor_values,
        limits: limits.clone(),
    })
}

