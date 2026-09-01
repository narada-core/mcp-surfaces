fn apply_reachable(
    binding: &Map<String, Value>,
    clause: &Clause,
    index: &DatomIndex<'_>,
    budget: &mut ExecutionBudget<'_>,
) -> Result<Vec<Map<String, Value>>, QueryFailure> {
    let Clause::Reachable {
        from,
        attribute,
        to,
        max_depth,
    } = clause
    else {
        return Ok(Vec::new());
    };
    let start_value = resolve(from, binding).ok_or_else(|| {
        QueryFailure::new(
            "query_reachable_unbound",
            "reachable from must be bound before the reachable clause is evaluated",
            json!({"term":"from"}),
        )
    })?;
    let start = start_value.as_str().map(str::to_string).ok_or_else(|| {
        QueryFailure::new(
            "query_reachable_type",
            "reachable from must resolve to a string id",
            json!({"term":"from","value":start_value}),
        )
    })?;
    let mut adjacency: HashMap<String, Vec<String>> = HashMap::new();
    for datom in index.attribute_candidates(attribute) {
        budget.scan_datom()?;
        if datom.attribute != *attribute {
            continue;
        }
        if let Some(target) = datom.value.as_str() {
            budget.traverse_edge()?;
            adjacency
                .entry(datom.subject.clone())
                .or_default()
                .push(target.to_string());
        }
    }
    let mut queue = VecDeque::from(vec![(start.clone(), 0usize)]);
    let mut visited = HashSet::new();
    visited.insert(start);
    let mut results = Vec::new();
    while let Some((current, depth)) = queue.pop_front() {
        if depth >= *max_depth {
            continue;
        }
        for next in adjacency.get(&current).into_iter().flatten() {
            if !visited.insert(next.clone()) {
                continue;
            }
            if let Some(result) = unify(to, &Value::String(next.clone()), binding) {
                if results.len() >= budget.work_limit {
                    return Err(query_work_failure(budget.work_limit));
                }
                results.push(result);
            }
            queue.push_back((next.clone(), depth + 1));
        }
    }
    Ok(results)
}

fn apply_nested(
    binding: &Map<String, Value>,
    clauses: &[Clause],
    index: &DatomIndex<'_>,
    budget: &mut ExecutionBudget<'_>,
) -> Result<Vec<Map<String, Value>>, QueryFailure> {
    apply_planned(binding.clone(), clauses, index, budget)
}

fn apply_clause(
    binding: &Map<String, Value>,
    clause: &Clause,
    index: &DatomIndex<'_>,
    budget: &mut ExecutionBudget<'_>,
) -> Result<Vec<Map<String, Value>>, QueryFailure> {
    match clause {
        Clause::Triple { .. } => apply_triple(binding, clause, index, budget),
        Clause::Reachable { .. } => apply_reachable(binding, clause, index, budget),
        Clause::Exists { clauses } => {
            if !apply_nested(binding, clauses, index, budget)?.is_empty() {
                Ok(vec![binding.clone()])
            } else {
                Ok(Vec::new())
            }
        }
        Clause::NotExists { clauses } => {
            if apply_nested(binding, clauses, index, budget)?.is_empty() {
                Ok(vec![binding.clone()])
            } else {
                Ok(Vec::new())
            }
        }
        Clause::Compare { op, left, right } => {
            let left = resolve(left, binding).ok_or_else(|| {
                QueryFailure::new(
                    "query_compare_unbound",
                    "compare left must be bound before the compare clause is evaluated",
                    json!({"term":"left"}),
                )
            })?;
            let right = resolve(right, binding).ok_or_else(|| {
                QueryFailure::new(
                    "query_compare_unbound",
                    "compare right must be bound before the compare clause is evaluated",
                    json!({"term":"right"}),
                )
            })?;
            Ok(compare(op, &left, &right)
                .then_some(binding.clone())
                .into_iter()
                .collect())
        }
    }
}

fn clause_ready(binding: &Map<String, Value>, clause: &Clause, allow_nested: bool) -> bool {
    match clause {
        Clause::Triple { .. } => true,
        Clause::Reachable { from, .. } => resolve(from, binding).is_some(),
        Clause::Compare { left, right, .. } => {
            resolve(left, binding).is_some() && resolve(right, binding).is_some()
        }
        // A nested predicate is deliberately delayed until all other
        // top-level clauses have run. This preserves correlated
        // `exists`/`not_exists` semantics while still allowing the ordinary
        // positive/compare/reach clauses to be written in any order.
        Clause::Exists { .. } | Clause::NotExists { .. } => allow_nested,
    }
}

fn apply_planned(
    initial: Map<String, Value>,
    clauses: &[Clause],
    datom_index: &DatomIndex<'_>,
    budget: &mut ExecutionBudget<'_>,
) -> Result<Vec<Map<String, Value>>, QueryFailure> {
    let mut bindings = vec![initial];
    let mut remaining = (0..clauses.len()).collect::<Vec<_>>();
    while !remaining.is_empty() {
        if bindings.is_empty() {
            break;
        }
        let allow_nested = remaining.iter().all(|index| {
            matches!(
                clauses[*index],
                Clause::Exists { .. } | Clause::NotExists { .. }
            )
        });
        let position = remaining.iter().position(|index| {
            bindings
                .iter()
                .all(|binding| clause_ready(binding, &clauses[*index], allow_nested))
        });
        let position = match position {
            Some(position) => position,
            None => {
                // Preserve the precise refusal emitted by the blocked
                // clause (for example query_compare_unbound) while reporting
                // it only after every executable clause has been attempted.
                let index = remaining
                    .iter()
                    .copied()
                    .find(|index| {
                        !matches!(
                            clauses[*index],
                            Clause::Exists { .. } | Clause::NotExists { .. }
                        )
                    })
                    .unwrap_or(remaining[0]);
                return apply_clause(&bindings[0], &clauses[index], datom_index, budget);
            }
        };
        let index = remaining.remove(position);
        let clause = &clauses[index];
        let mut next = Vec::new();
        for binding in &bindings {
            next.extend(apply_clause(binding, clause, datom_index, budget)?);
            if next.len() > budget.work_limit {
                return Err(query_work_failure(budget.work_limit));
            }
        }
        bindings = next;
    }
    Ok(bindings)
}

fn term_key(term: &Term, binding: &Map<String, Value>) -> Option<Value> {
    resolve(term, binding)
}

fn after_cursor(
    binding: &Map<String, Value>,
    order_by: &[OrderTerm],
    cursor: &Map<String, Value>,
) -> Result<bool, QueryFailure> {
    if cursor.is_empty() {
        return Ok(true);
    }
    for order in order_by {
        let Some(key) = order.term.as_variable_name() else {
            return Err(QueryFailure::new(
                "query_cursor_invalid",
                "cursor ordering terms must be variables",
                Value::Null,
            ));
        };
        let (Some(current), Some(previous)) = (binding.get(key), cursor.get(key)) else {
            return Err(QueryFailure::new(
                "query_cursor_unbound",
                "cursor ordering variable is not bound in the result",
                json!({"variable":key}),
            ));
        };
        let Some(ordering) = compare_values(current, previous) else {
            return Err(QueryFailure::new(
                "query_cursor_type_mismatch",
                "cursor value is not comparable with the ordered result value",
                json!({"variable":key}),
            ));
        };
        if ordering == Ordering::Equal {
            continue;
        }
        return Ok(if order.descending {
            ordering == Ordering::Less
        } else {
            ordering == Ordering::Greater
        });
    }
    Ok(false)
}

impl Term {
    pub fn as_variable_name(&self) -> Option<&str> {
        match self {
            Term::Variable(name) => Some(name.as_str()),
            _ => None,
        }
    }
}

