fn resolve(term: &Term, binding: &Map<String, Value>) -> Option<Value> {
    match term {
        Term::Variable(name) => binding.get(name).cloned(),
        Term::Value(value) => Some(value.clone()),
        Term::OneOf(_) => None,
    }
}

fn unify(term: &Term, value: &Value, binding: &Map<String, Value>) -> Option<Map<String, Value>> {
    let mut result = binding.clone();
    match term {
        Term::Variable(name) => {
            if let Some(existing) = result.get(name) {
                (existing == value).then_some(result)
            } else {
                result.insert(name.clone(), value.clone());
                Some(result)
            }
        }
        Term::Value(expected) => (expected == value).then_some(result),
        Term::OneOf(values) => values
            .iter()
            .any(|expected| expected == value)
            .then_some(result),
    }
}

fn compare_values(left: &Value, right: &Value) -> Option<Ordering> {
    match (left, right) {
        (Value::Number(left), Value::Number(right)) => left.as_f64()?.partial_cmp(&right.as_f64()?),
        (Value::String(left), Value::String(right)) => Some(left.cmp(right)),
        (Value::Bool(left), Value::Bool(right)) => Some(left.cmp(right)),
        _ => None,
    }
}

fn compare(op: &str, left: &Value, right: &Value) -> bool {
    let Some(ordering) = compare_values(left, right) else {
        return op == "=" && left == right;
    };
    match op {
        "=" => ordering == Ordering::Equal,
        "!=" => ordering != Ordering::Equal,
        ">" => ordering == Ordering::Greater,
        ">=" => ordering != Ordering::Less,
        "<" => ordering == Ordering::Less,
        "<=" => ordering != Ordering::Greater,
        _ => false,
    }
}

fn query_work_failure(limit: usize) -> QueryFailure {
    QueryFailure::new(
        "query_work_limit",
        "query intermediate result exceeded the work limit",
        json!({"work_limit":limit}),
    )
}

fn query_datom_scan_failure(limit: usize) -> QueryFailure {
    QueryFailure::new(
        "query_datom_scan_limit",
        "query datom-scan budget was exceeded",
        json!({"max_datoms_scanned":limit}),
    )
}

fn query_traversal_failure(limit: usize) -> QueryFailure {
    QueryFailure::new(
        "query_traversal_limit",
        "query traversal-edge budget was exceeded",
        json!({"max_traversal_edges":limit}),
    )
}

struct ExecutionBudget<'a> {
    limits: &'a QueryLimits,
    work_limit: usize,
    datoms_scanned: usize,
    traversal_edges: usize,
}

struct DatomIndex<'a> {
    all: &'a [Datom],
    by_subject: HashMap<&'a str, Vec<&'a Datom>>,
    by_subject_attribute: HashMap<(&'a str, &'a str), Vec<&'a Datom>>,
    by_attribute: HashMap<&'a str, Vec<&'a Datom>>,
    by_attribute_value: HashMap<(&'a str, String), Vec<&'a Datom>>,
}

impl<'a> DatomIndex<'a> {
    fn new(datoms: &'a [Datom]) -> Self {
        let mut by_subject: HashMap<&str, Vec<&Datom>> = HashMap::new();
        let mut by_subject_attribute: HashMap<(&str, &str), Vec<&Datom>> = HashMap::new();
        let mut by_attribute: HashMap<&str, Vec<&Datom>> = HashMap::new();
        let mut by_attribute_value: HashMap<(&str, String), Vec<&Datom>> = HashMap::new();
        for datom in datoms {
            by_subject
                .entry(datom.subject.as_str())
                .or_default()
                .push(datom);
            by_subject_attribute
                .entry((datom.subject.as_str(), datom.attribute.as_str()))
                .or_default()
                .push(datom);
            by_attribute
                .entry(datom.attribute.as_str())
                .or_default()
                .push(datom);
            by_attribute_value
                .entry((datom.attribute.as_str(), stable_value_key(&datom.value)))
                .or_default()
                .push(datom);
        }
        Self {
            all: datoms,
            by_subject,
            by_subject_attribute,
            by_attribute,
            by_attribute_value,
        }
    }

    fn triple_candidates(
        &self,
        subject: &Term,
        attribute: &Term,
        object: &Term,
        binding: &Map<String, Value>,
    ) -> Vec<&'a Datom> {
        if let Some(value) = resolve(subject, binding) {
            if let Some(subject) = value.as_str() {
                if let Some(attribute) = resolve(attribute, binding) {
                    if let Some(attribute) = attribute.as_str() {
                        return self
                            .by_subject_attribute
                            .get(&(subject, attribute))
                            .cloned()
                            .unwrap_or_default();
                    }
                }
                return self.by_subject.get(subject).cloned().unwrap_or_default();
            }
        }
        if let Some(value) = resolve(attribute, binding) {
            if let Some(attribute) = value.as_str() {
                if let Some(object) = resolve(object, binding) {
                    return self
                        .by_attribute_value
                        .get(&(attribute, stable_value_key(&object)))
                        .cloned()
                        .unwrap_or_default();
                }
                return self
                    .by_attribute
                    .get(attribute)
                    .cloned()
                    .unwrap_or_default();
            }
        }
        self.all.iter().collect()
    }

    fn attribute_candidates(&self, attribute: &str) -> Vec<&'a Datom> {
        self.by_attribute
            .get(attribute)
            .cloned()
            .unwrap_or_default()
    }
}

fn stable_value_key(value: &Value) -> String {
    serde_json::to_string(value).unwrap_or_default()
}

impl<'a> ExecutionBudget<'a> {
    fn new(limits: &'a QueryLimits, work_limit: usize) -> Self {
        Self {
            limits,
            work_limit,
            datoms_scanned: 0,
            traversal_edges: 0,
        }
    }

    fn scan_datom(&mut self) -> Result<(), QueryFailure> {
        self.datoms_scanned = self.datoms_scanned.saturating_add(1);
        if self.datoms_scanned > self.limits.max_datoms_scanned {
            return Err(query_datom_scan_failure(self.limits.max_datoms_scanned));
        }
        Ok(())
    }

    fn traverse_edge(&mut self) -> Result<(), QueryFailure> {
        self.traversal_edges = self.traversal_edges.saturating_add(1);
        if self.traversal_edges > self.limits.max_traversal_edges {
            return Err(query_traversal_failure(self.limits.max_traversal_edges));
        }
        Ok(())
    }
}

fn apply_triple(
    binding: &Map<String, Value>,
    clause: &Clause,
    index: &DatomIndex<'_>,
    budget: &mut ExecutionBudget<'_>,
) -> Result<Vec<Map<String, Value>>, QueryFailure> {
    let Clause::Triple {
        subject,
        attribute,
        object,
    } = clause
    else {
        return Ok(Vec::new());
    };
    let mut results = Vec::new();
    for datom in index.triple_candidates(subject, attribute, object, binding) {
        budget.scan_datom()?;
        let Some(after_subject) = unify(subject, &Value::String(datom.subject.clone()), binding)
        else {
            continue;
        };
        let Some(after_attribute) = unify(
            attribute,
            &Value::String(datom.attribute.clone()),
            &after_subject,
        ) else {
            continue;
        };
        if let Some(result) = unify(object, &datom.value, &after_attribute) {
            results.push(result);
            if results.len() > budget.work_limit {
                return Err(query_work_failure(budget.work_limit));
            }
        }
    }
    Ok(results)
}

