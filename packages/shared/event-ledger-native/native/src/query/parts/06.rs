pub fn execute(spec: &QuerySpec, datoms: &[Datom]) -> Result<QueryResult, QueryFailure> {
    let initial = spec
        .inputs
        .iter()
        .fold(Map::new(), |mut binding, (key, value)| {
            binding.insert(
                variable_name(key).unwrap_or_else(|_| format!("?{key}")),
                value.clone(),
            );
            binding
        });
    // Intermediate bindings are independently bounded by the effective,
    // server-capped work budget. A result-page limit is not a sound work
    // ceiling: a selective indexed predicate may legitimately identify more
    // candidates before read/reply filters and stable ordering are applied.
    let work_limit = spec.limits.max_datoms_scanned.max(1000);
    let mut budget = ExecutionBudget::new(&spec.limits, work_limit);
    let index = DatomIndex::new(datoms);
    let mut bindings = apply_planned(initial, &spec.clauses, &index, &mut budget)?;
    let mut seen = HashSet::new();
    bindings.retain(|binding| seen.insert(serde_json::to_string(binding).unwrap_or_default()));
    if !spec.cursor_values.is_empty() {
        let mut after = Vec::with_capacity(bindings.len());
        for binding in bindings {
            if after_cursor(&binding, &spec.order_by, &spec.cursor_values)? {
                after.push(binding);
            }
        }
        bindings = after;
    }
    for binding in &bindings {
        for order in &spec.order_by {
            if term_key(&order.term, binding).is_none() {
                return Err(QueryFailure::new(
                    "query_order_unbound",
                    "order_by variable is not bound in every result",
                    json!({"term":order.term.as_variable_name()}),
                ));
            }
        }
    }
    bindings.sort_by(|left, right| {
        for order in &spec.order_by {
            let (Some(left), Some(right)) =
                (term_key(&order.term, left), term_key(&order.term, right))
            else {
                continue;
            };
            let ordering = compare_values(&left, &right).unwrap_or(Ordering::Equal);
            if ordering != Ordering::Equal {
                return if order.descending {
                    ordering.reverse()
                } else {
                    ordering
                };
            }
        }
        Ordering::Equal
    });
    let has_more = bindings.len() > spec.limit;
    if has_more && !spec.order_by.is_empty() {
        let duplicate_order_key = bindings.windows(2).any(|window| {
            spec.order_by.iter().all(|order| {
                match (
                    term_key(&order.term, &window[0]),
                    term_key(&order.term, &window[1]),
                ) {
                    (Some(left), Some(right)) => left == right,
                    _ => false,
                }
            })
        });
        if duplicate_order_key {
            return Err(QueryFailure::new(
                "query_order_not_unique",
                "order_by values must uniquely advance a paginated result",
                Value::Null,
            ));
        }
    }
    let bindings = bindings.into_iter().take(spec.limit).collect();
    Ok(QueryResult {
        bindings,
        pulls: spec.pulls.clone(),
        order_by: spec.order_by.clone(),
        has_more,
    })
}

