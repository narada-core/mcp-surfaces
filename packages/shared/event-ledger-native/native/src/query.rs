//! Bounded, read-only Datalog-like query primitives for event-ledger projections.
//!
//! The evaluator deliberately operates on normalized datoms rather than domain
//! payload JSON. Domains provide the vocabulary and emit datoms; this module
//! knows only joins, comparisons, bounded reachability, ordering, and paging.

use serde_json::{json, Map, Value};
use std::cmp::Ordering;
use std::collections::{HashMap, HashSet, VecDeque};

#[derive(Clone, Debug)]
pub struct Datom {
    pub origin_id: String,
    pub subject: String,
    pub attribute: String,
    pub value: Value,
    pub event_sequence: u64,
    pub event_id: String,
}

#[derive(Clone, Debug)]
pub struct QueryLimits {
    pub max_clauses: usize,
    pub max_results: usize,
    pub max_reach_depth: usize,
    pub max_one_of_values: usize,
    pub max_predicate_depth: usize,
    pub max_datoms_scanned: usize,
    pub max_traversal_edges: usize,
}

#[derive(Clone, Debug)]
pub struct PullSpec {
    pub variable: String,
    pub fields: Vec<String>,
    pub target_kind: Option<String>,
}

#[derive(Clone, Debug)]
pub struct QueryResult {
    pub bindings: Vec<Map<String, Value>>,
    pub pulls: Vec<PullSpec>,
    pub order_by: Vec<OrderTerm>,
    pub has_more: bool,
}

#[derive(Clone, Debug)]
pub struct OrderTerm {
    pub term: Term,
    pub descending: bool,
}

#[derive(Clone, Debug)]
pub enum Term {
    Variable(String),
    Value(Value),
    OneOf(Vec<Value>),
}

#[derive(Clone, Debug)]
pub enum Clause {
    Triple {
        subject: Term,
        attribute: Term,
        object: Term,
    },
    Compare {
        op: String,
        left: Term,
        right: Term,
    },
    Reachable {
        from: Term,
        attribute: String,
        to: Term,
        max_depth: usize,
    },
    Exists {
        clauses: Vec<Clause>,
    },
    NotExists {
        clauses: Vec<Clause>,
    },
}

#[derive(Clone, Debug)]
pub struct QuerySpec {
    pub inputs: Map<String, Value>,
    pub finds: Vec<Term>,
    pub pulls: Vec<PullSpec>,
    pub clauses: Vec<Clause>,
    pub order_by: Vec<OrderTerm>,
    pub limit: usize,
    pub cursor_values: Map<String, Value>,
    pub limits: QueryLimits,
}

#[derive(Clone, Debug)]
pub struct QueryFailure {
    pub code: &'static str,
    pub message: String,
    pub details: Value,
}

impl QueryFailure {
    fn new(code: &'static str, message: impl Into<String>, details: Value) -> Self {
        Self {
            code,
            message: message.into(),
            details,
        }
    }
}

fn variable_name(value: &str) -> Result<String, QueryFailure> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(QueryFailure::new(
            "query_invalid_variable",
            "query variable must not be empty",
            Value::Null,
        ));
    }
    if trimmed.starts_with('?') {
        Ok(trimmed.to_string())
    } else {
        Ok(format!("?{trimmed}"))
    }
}

fn reject_unknown_keys(
    object: &Map<String, Value>,
    allowed: &[&str],
    context: &str,
) -> Result<(), QueryFailure> {
    if let Some(key) = object.keys().find(|key| !allowed.contains(&key.as_str())) {
        return Err(QueryFailure::new(
            "query_invalid_field",
            format!("{context} field is not supported: {key}"),
            json!({"context":context,"field":key}),
        ));
    }
    Ok(())
}

fn parse_term(value: &Value, limits: &QueryLimits) -> Result<Term, QueryFailure> {
    if let Some(text) = value.as_str() {
        if text.starts_with('?') {
            return Ok(Term::Variable(variable_name(text)?));
        }
        return Ok(Term::Value(Value::String(text.to_string())));
    }
    let Some(object) = value.as_object() else {
        return Ok(Term::Value(value.clone()));
    };
    reject_unknown_keys(object, &["var", "input", "value", "one_of"], "term")?;
    let form_count = ["var", "input", "value", "one_of"]
        .iter()
        .filter(|key| object.contains_key(**key))
        .count();
    if form_count > 1 {
        return Err(QueryFailure::new(
            "query_ambiguous_term",
            "term must contain exactly one variable, input, value, or one_of form",
            value.clone(),
        ));
    }
    if let Some(variable) = object.get("var").and_then(Value::as_str) {
        return Ok(Term::Variable(variable_name(variable)?));
    }
    if let Some(input) = object.get("input").and_then(Value::as_str) {
        return Ok(Term::Variable(variable_name(input)?));
    }
    if let Some(value) = object.get("value") {
        return Ok(Term::Value(value.clone()));
    }
    if let Some(values) = object.get("one_of").and_then(Value::as_array) {
        if values.is_empty() {
            return Err(QueryFailure::new(
                "query_invalid_term",
                "one_of must contain at least one value",
                value.clone(),
            ));
        }
        if values.len() > limits.max_one_of_values {
            return Err(QueryFailure::new(
                "query_term_limit",
                format!("one_of must contain at most {} values", limits.max_one_of_values),
                json!({"count":values.len(),"max":limits.max_one_of_values}),
            ));
        }
        return Ok(Term::OneOf(values.clone()));
    }
    Err(QueryFailure::new(
        "query_invalid_term",
        "term must be a literal, variable, input, or one_of object",
        value.clone(),
    ))
}

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
            format!("query clauses must contain at most {} clauses in total", limits.max_clauses),
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
        reject_unknown_keys(triple, &["subject", "attribute", "object", "value"], "triple")?;
        if triple.contains_key("object") == triple.contains_key("value") {
            return Err(QueryFailure::new(
                "query_invalid_clause",
                "triple must contain exactly one object or value field",
                triple.clone().into(),
            ));
        }
        let subject = triple.get("subject").ok_or_else(|| {
            QueryFailure::new("query_invalid_clause", "triple subject is required", triple.clone().into())
        })?;
        let attribute = triple.get("attribute").ok_or_else(|| {
            QueryFailure::new("query_invalid_clause", "triple attribute is required", triple.clone().into())
        })?;
        let object_value = triple.get("object").or_else(|| triple.get("value")).ok_or_else(|| {
            QueryFailure::new("query_invalid_clause", "triple object is required", triple.clone().into())
        })?;
        return Ok(Clause::Triple {
            subject: parse_term(subject, limits)?,
            attribute: parse_term(attribute, limits)?,
            object: parse_term(object_value, limits)?,
        });
    }
    if let Some(compare) = object.get("compare") {
        let compare = compare.as_object().ok_or_else(|| {
            QueryFailure::new("query_invalid_clause", "compare clause must be an object", compare.clone())
        })?;
        reject_unknown_keys(compare, &["op", "left", "right"], "compare")?;
        let op = compare.get("op").and_then(Value::as_str).ok_or_else(|| {
            QueryFailure::new("query_invalid_clause", "compare op is required", compare.clone().into())
        })?;
        if !matches!(op, "=" | "!=" | ">" | ">=" | "<" | "<=") {
            return Err(QueryFailure::new(
                "query_unsupported_operator",
                format!("unsupported comparison operator: {op}"),
                json!({"op":op}),
            ));
        }
        let left = compare.get("left").ok_or_else(|| {
            QueryFailure::new("query_invalid_clause", "compare left is required", compare.clone().into())
        })?;
        let right = compare.get("right").ok_or_else(|| {
            QueryFailure::new("query_invalid_clause", "compare right is required", compare.clone().into())
        })?;
        return Ok(Clause::Compare {
            op: op.to_string(),
            left: parse_term(left, limits)?,
            right: parse_term(right, limits)?,
        });
    }
    if let Some(reachable) = object.get("reachable") {
        let reachable = reachable.as_object().ok_or_else(|| {
            QueryFailure::new("query_invalid_clause", "reachable clause must be an object", reachable.clone())
        })?;
        reject_unknown_keys(reachable, &["from", "to", "attribute", "max_depth"], "reachable")?;
        let from = reachable.get("from").ok_or_else(|| {
            QueryFailure::new("query_invalid_clause", "reachable from is required", reachable.clone().into())
        })?;
        let to = reachable.get("to").ok_or_else(|| {
            QueryFailure::new("query_invalid_clause", "reachable to is required", reachable.clone().into())
        })?;
        let attribute = reachable.get("attribute").and_then(Value::as_str).ok_or_else(|| {
            QueryFailure::new("query_invalid_clause", "reachable attribute is required", reachable.clone().into())
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
    for (key, constructor) in [
        ("exists", true),
        ("not_exists", false),
    ] {
        let Some(nested) = object.get(key) else {
            continue;
        };
        let next_predicate_depth = predicate_depth.saturating_add(1);
        if next_predicate_depth > limits.max_predicate_depth {
            return Err(QueryFailure::new(
                "query_predicate_depth_limit",
                format!("nested predicates may not exceed {} levels", limits.max_predicate_depth),
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
            &["where", "triple", "compare", "reachable", "exists", "not_exists"],
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
            where_values.as_array().ok_or_else(|| {
                QueryFailure::new(
                    "query_invalid_clause",
                    format!("{key}.where must be an array"),
                    where_values.clone(),
                )
            })?.clone()
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
                format!("{key}.where must contain between 1 and {} clauses", limits.max_clauses),
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

pub fn parse(value: &Value, default_limit: usize, limits: &QueryLimits) -> Result<QuerySpec, QueryFailure> {
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
            format!("inputs must contain at most {} variables", limits.max_clauses),
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
        .ok_or_else(|| QueryFailure::new("query_invalid_find", "find must be an array", Value::Null))?;
    if find_values.is_empty() {
        return Err(QueryFailure::new("query_invalid_find", "find must not be empty", Value::Null));
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
            let variable = pull.get("var").or_else(|| pull.get("variable")).and_then(Value::as_str).ok_or_else(|| {
                QueryFailure::new("query_invalid_pull", "pull variable is required", pull.clone().into())
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
                .ok_or_else(|| QueryFailure::new("query_invalid_pull", "pull fields must be an array", pull.clone().into()))?
                .iter()
                .map(|field| field.as_str().map(str::to_string).ok_or_else(|| QueryFailure::new("query_invalid_pull", "pull field must be a string", field.clone())))
                .collect::<Result<Vec<_>, _>>()?;
            if fields.is_empty() || fields.len() > limits.max_clauses {
                return Err(QueryFailure::new(
                    "query_pull_field_limit",
                    format!("pull fields must contain between 1 and {} fields", limits.max_clauses),
                    json!({"count":fields.len(),"max":limits.max_clauses}),
                ));
            }
            let variable = variable_name(variable)?;
            pulls.push(PullSpec { variable: variable.clone(), fields, target_kind });
            finds.push(Term::Variable(variable));
        } else {
            finds.push(parse_term(item, limits)?);
        }
    }
    if finds
        .iter()
        .any(|term| term.as_variable_name().is_none())
    {
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
        .ok_or_else(|| QueryFailure::new("query_invalid_where", "where must be an array", Value::Null))?
        .iter()
        .map(|clause| parse_clause_at(clause, limits, 0, &mut clause_count))
        .collect::<Result<Vec<_>, _>>()?;
    if clauses.is_empty() || clauses.len() > limits.max_clauses {
        return Err(QueryFailure::new(
            "query_clause_limit",
            format!("where must contain between 1 and {} clauses", limits.max_clauses),
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
            let item = item.as_object().ok_or_else(|| QueryFailure::new("query_invalid_order", "order_by item must be an object", item.clone()))?;
            reject_unknown_keys(item, &["term", "direction"], "order_by")?;
            let term = item.get("term").ok_or_else(|| QueryFailure::new("query_invalid_order", "order_by term is required", item.clone().into()))?;
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
                return Err(QueryFailure::new("query_invalid_order", "order direction must be asc or desc", item.clone().into()));
            }
            Ok(OrderTerm { term: parse_term(term, limits)?, descending: direction == "desc" })
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
            reject_unknown_keys(cursor_object, &["schema", "head", "query", "values"], "cursor")?;
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
    Ok(QuerySpec { inputs, finds, pulls, clauses, order_by, limit, cursor_values, limits: limits.clone() })
}

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
        Term::OneOf(values) => values.iter().any(|expected| expected == value).then_some(result),
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
    let Some(ordering) = compare_values(left, right) else { return op == "=" && left == right };
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
    by_attribute: HashMap<&'a str, Vec<&'a Datom>>,
}

impl<'a> DatomIndex<'a> {
    fn new(datoms: &'a [Datom]) -> Self {
        let mut by_subject: HashMap<&str, Vec<&Datom>> = HashMap::new();
        let mut by_attribute: HashMap<&str, Vec<&Datom>> = HashMap::new();
        for datom in datoms {
            by_subject.entry(datom.subject.as_str()).or_default().push(datom);
            by_attribute.entry(datom.attribute.as_str()).or_default().push(datom);
        }
        Self { all: datoms, by_subject, by_attribute }
    }

    fn triple_candidates(&self, subject: &Term, attribute: &Term, binding: &Map<String, Value>) -> Vec<&'a Datom> {
        if let Some(value) = resolve(subject, binding) {
            if let Some(subject) = value.as_str() {
                return self.by_subject.get(subject).cloned().unwrap_or_default();
            }
        }
        if let Some(value) = resolve(attribute, binding) {
            if let Some(attribute) = value.as_str() {
                return self.by_attribute.get(attribute).cloned().unwrap_or_default();
            }
        }
        self.all.iter().collect()
    }

    fn attribute_candidates(&self, attribute: &str) -> Vec<&'a Datom> {
        self.by_attribute.get(attribute).cloned().unwrap_or_default()
    }
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
    let Clause::Triple { subject, attribute, object } = clause else { return Ok(Vec::new()) };
    let mut results = Vec::new();
    for datom in index.triple_candidates(subject, attribute, binding) {
        budget.scan_datom()?;
        let Some(after_subject) = unify(subject, &Value::String(datom.subject.clone()), binding) else {
            continue;
        };
        let Some(after_attribute) = unify(attribute, &Value::String(datom.attribute.clone()), &after_subject) else {
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

fn apply_reachable(
    binding: &Map<String, Value>,
    clause: &Clause,
    index: &DatomIndex<'_>,
    budget: &mut ExecutionBudget<'_>,
) -> Result<Vec<Map<String, Value>>, QueryFailure> {
    let Clause::Reachable { from, attribute, to, max_depth } = clause else { return Ok(Vec::new()) };
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
            adjacency.entry(datom.subject.clone()).or_default().push(target.to_string());
        }
    }
    let mut queue = VecDeque::from(vec![(start.clone(), 0usize)]);
    let mut visited = HashSet::new();
    visited.insert(start);
    let mut results = Vec::new();
    while let Some((current, depth)) = queue.pop_front() {
        if depth >= *max_depth { continue; }
        for next in adjacency.get(&current).into_iter().flatten() {
            if !visited.insert(next.clone()) { continue; }
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
            Ok(compare(op, &left, &right).then_some(binding.clone()).into_iter().collect())
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
            matches!(clauses[*index], Clause::Exists { .. } | Clause::NotExists { .. })
        });
        let position = remaining
            .iter()
            .position(|index| bindings.iter().all(|binding| clause_ready(binding, &clauses[*index], allow_nested)));
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
                        !matches!(clauses[*index], Clause::Exists { .. } | Clause::NotExists { .. })
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

fn term_key(term: &Term, binding: &Map<String, Value>) -> Option<Value> { resolve(term, binding) }

fn after_cursor(
    binding: &Map<String, Value>,
    order_by: &[OrderTerm],
    cursor: &Map<String, Value>,
) -> Result<bool, QueryFailure> {
    if cursor.is_empty() { return Ok(true); }
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
        if ordering == Ordering::Equal { continue; }
        return Ok(if order.descending { ordering == Ordering::Less } else { ordering == Ordering::Greater });
    }
    Ok(false)
}

impl Term {
    pub fn as_variable_name(&self) -> Option<&str> {
        match self { Term::Variable(name) => Some(name.as_str()), _ => None }
    }
}

pub fn execute(spec: &QuerySpec, datoms: &[Datom]) -> Result<QueryResult, QueryFailure> {
    let initial = spec.inputs.iter().fold(Map::new(), |mut binding, (key, value)| {
        binding.insert(variable_name(key).unwrap_or_else(|_| format!("?{key}")), value.clone());
        binding
    });
    let work_limit = spec.limit.saturating_mul(100).max(1000);
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
            let (Some(left), Some(right)) = (term_key(&order.term, left), term_key(&order.term, right)) else { continue };
            let ordering = compare_values(&left, &right).unwrap_or(Ordering::Equal);
            if ordering != Ordering::Equal { return if order.descending { ordering.reverse() } else { ordering }; }
        }
        Ordering::Equal
    });
    let has_more = bindings.len() > spec.limit;
    if has_more && !spec.order_by.is_empty() {
        let duplicate_order_key = bindings.windows(2).any(|window| {
            spec.order_by.iter().all(|order| {
                match (term_key(&order.term, &window[0]), term_key(&order.term, &window[1])) {
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
    Ok(QueryResult { bindings, pulls: spec.pulls.clone(), order_by: spec.order_by.clone(), has_more })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn datom(subject: &str, attribute: &str, value: Value, sequence: u64, event_id: &str) -> Datom {
        Datom {
            origin_id: subject.to_string(),
            subject: subject.to_string(),
            attribute: attribute.to_string(),
            value,
            event_sequence: sequence,
            event_id: event_id.to_string(),
        }
    }

    fn limits() -> QueryLimits {
        QueryLimits {
            max_clauses: 16,
            max_results: 100,
            max_reach_depth: 8,
            max_one_of_values: 16,
            max_predicate_depth: 8,
            max_datoms_scanned: 10_000,
            max_traversal_edges: 10_000,
        }
    }

    #[test]
    fn executable_clauses_are_planned_by_bindings() {
        let query = json!({
            "find":["?message"],
            "where":[
                {"compare":{"op":">","left":"?sequence","right":1}},
                {"triple":{"subject":"?message","attribute":"kind","object":"claim"}},
                {"triple":{"subject":"?message","attribute":"sequence","object":"?sequence"}}
            ],
            "order_by":[{"term":"?sequence"}],
            "limit":10
        });
        let datoms = vec![
            datom("m1", "kind", json!("claim"), 1, "e1"),
            datom("m1", "sequence", json!(1), 1, "e1"),
            datom("m2", "kind", json!("claim"), 2, "e2"),
            datom("m2", "sequence", json!(2), 2, "e2"),
        ];
        let result = execute(&parse(&query, 20, &limits()).expect("query parses"), &datoms)
            .expect("planner should move compare after its binding triple");
        assert_eq!(result.bindings.len(), 1);
        assert_eq!(result.bindings[0]["?message"], "m2");
    }

    #[test]
    fn datom_scan_and_traversal_budgets_refuse_expensive_queries() {
        let scan_query = json!({
            "find":["?message"],
            "where":[{"triple":{"subject":"?message","attribute":"kind","object":"claim"}}],
            "limit":10
        });
        let mut scan_limits = limits();
        scan_limits.max_datoms_scanned = 1;
        let scan_failure = execute(
            &parse(&scan_query, 20, &scan_limits).expect("scan query parses"),
            &[
                datom("m1", "kind", json!("claim"), 1, "e1"),
                datom("m2", "kind", json!("claim"), 2, "e2"),
            ],
        )
        .expect_err("scan budget must refuse");
        assert_eq!(scan_failure.code, "query_datom_scan_limit");

        let reach_query = json!({
            "find":["?message"],
            "inputs":{"root":"m0"},
            "where":[{"reachable":{"from":{"input":"root"},"attribute":"replied_by","to":"?message","max_depth":2}}],
            "limit":10
        });
        let mut reach_limits = limits();
        reach_limits.max_traversal_edges = 1;
        let reach_failure = execute(
            &parse(&reach_query, 20, &reach_limits).expect("reach query parses"),
            &[
                datom("m0", "replied_by", json!("m1"), 1, "e1"),
                datom("m1", "replied_by", json!("m2"), 2, "e2"),
            ],
        )
        .expect_err("traversal budget must refuse");
        assert_eq!(reach_failure.code, "query_traversal_limit");
    }

    #[test]
    fn bound_subject_joins_use_the_subject_index_within_scan_budget() {
        let query = json!({
            "find":["?message","?sequence"],
            "where":[
                {"triple":{"subject":"?message","attribute":"recipient","object":"marici.Nima"}},
                {"triple":{"subject":"?message","attribute":"kind","object":"communication"}},
                {"triple":{"subject":"?message","attribute":"sequence","object":"?sequence"}}
            ],
            "order_by":[{"term":"?sequence"}],
            "limit":100
        });
        let mut datoms = Vec::new();
        for sequence in 1..=100 {
            let subject = format!("message-{sequence}");
            let event = format!("event-{sequence}");
            datoms.push(datom(&subject, "recipient", json!("marici.Nima"), sequence, &event));
            datoms.push(datom(&subject, "kind", json!("communication"), sequence, &event));
            datoms.push(datom(&subject, "sequence", json!(sequence), sequence, &event));
        }
        let mut indexed_limits = limits();
        indexed_limits.max_datoms_scanned = 700;
        let result = execute(
            &parse(&query, 100, &indexed_limits).expect("indexed join query parses"),
            &datoms,
        ).expect("bound subject joins must stay within the indexed scan budget");
        assert_eq!(result.bindings.len(), 100);
    }

    #[test]
    fn query_shape_limits_cover_inputs_and_order_terms() {
        let mut shape_limits = limits();
        shape_limits.max_clauses = 1;
        let base = |extra: Value| {
            let mut query = json!({
                "find":["?message"],
                "where":[{"triple":{"subject":"?message","attribute":"kind","object":"claim"}}]
            });
            if let (Some(target), Some(extra)) = (query.as_object_mut(), extra.as_object()) {
                for (key, value) in extra {
                    target.insert(key.clone(), value.clone());
                }
            }
            query
        };
        let input_failure = parse(
            &base(json!({"inputs":{"?one":1,"?two":2}})),
            20,
            &shape_limits,
        )
        .expect_err("input count must be bounded");
        assert_eq!(input_failure.code, "query_input_limit");
        let order_failure = parse(
            &base(json!({
                "order_by":[{"term":"?message"},{"term":"?message"}]
            })),
            20,
            &shape_limits,
        )
        .expect_err("order term count must be bounded");
        assert_eq!(order_failure.code, "query_order_limit");
    }

    #[test]
    fn query_shape_limits_cover_terms_predicates_and_normalized_inputs() {
        let mut term_limits = limits();
        term_limits.max_one_of_values = 2;
        let one_of_query = json!({
            "find":["?message"],
            "where":[{"triple":{"subject":"?message","attribute":"kind","object":{"one_of":["claim","test","source"]}}}]
        });
        let one_of_failure = parse(&one_of_query, 20, &term_limits)
            .expect_err("one_of values must be bounded");
        assert_eq!(one_of_failure.code, "query_term_limit");

        let mut predicate_limits = limits();
        predicate_limits.max_predicate_depth = 2;
        let mut nested = json!({
            "triple":{"subject":"?message","attribute":"kind","object":"claim"}
        });
        for _ in 0..3 {
            nested = json!({"exists":{"where":[nested]}});
        }
        let predicate_failure = parse(
            &json!({"find":["?message"],"where":[nested]}),
            20,
            &predicate_limits,
        )
        .expect_err("nested predicate depth must be bounded");
        assert_eq!(predicate_failure.code, "query_predicate_depth_limit");

        let mut clause_limits = limits();
        clause_limits.max_clauses = 3;
        let nested_clause = json!({
            "exists":{"where":[
                {"triple":{"subject":"?message","attribute":"kind","object":"claim"}},
                {"exists":{"where":[
                    {"triple":{"subject":"?message","attribute":"status","object":"open"}}
                ]}}
            ]}
        });
        let clause_failure = parse(
            &json!({"find":["?message"],"where":[nested_clause]}),
            20,
            &clause_limits,
        )
        .expect_err("nested predicate clauses must share the clause budget");
        assert_eq!(clause_failure.code, "query_clause_limit");

        let duplicate_input_failure = parse(
            &json!({
                "find":["?message"],
                "inputs":{"message":"one","?message":"two"},
                "where":[{"triple":{"subject":"?message","attribute":"kind","object":"claim"}}]
            }),
            20,
            &limits(),
        )
        .expect_err("normalized input names must be unique");
        assert_eq!(duplicate_input_failure.code, "query_duplicate_input");

        let typed_pull = parse(
            &json!({
                "find":[{"pull":{"var":"?relation","target_kind":"relation","fields":["*"]}}],
                "where":[{"triple":{"subject":"?relation","attribute":"relation/id","object":"?relation"}}]
            }),
            20,
            &limits(),
        )
        .expect("typed pull parses");
        assert_eq!(typed_pull.pulls[0].target_kind.as_deref(), Some("relation"));
    }

    #[test]
    fn joins_and_keyset_pagination_are_deterministic() {
        let query = json!({
            "find":["?message"],
            "inputs":{"recipient":"marici.Grothendieck"},
            "where":[
                {"triple":{"subject":"?message","attribute":"kind","object":{"value":"communication"}}},
                {"triple":{"subject":"?message","attribute":"recipient","object":{"input":"recipient"}}},
                {"triple":{"subject":"?message","attribute":"sequence","object":"?sequence"}},
                {"triple":{"subject":"?message","attribute":"event_id","object":"?event_id"}}
            ],
            "order_by":[{"term":"?sequence"},{"term":"?event_id"}],
            "limit":1
        });
        let datoms = vec![
            datom("m2", "kind", json!("communication"), 2, "e2"),
            datom("m2", "recipient", json!("marici.Grothendieck"), 2, "e2"),
            datom("m2", "sequence", json!(2), 2, "e2"),
            datom("m2", "event_id", json!("e2"), 2, "e2"),
            datom("m1", "kind", json!("communication"), 1, "e1"),
            datom("m1", "recipient", json!("marici.Grothendieck"), 1, "e1"),
            datom("m1", "sequence", json!(1), 1, "e1"),
            datom("m1", "event_id", json!("e1"), 1, "e1"),
        ];
        let spec = parse(&query, 20, &limits()).expect("query parses");
        let first = execute(&spec, &datoms).expect("query executes");
        assert_eq!(first.bindings.len(), 1);
        assert_eq!(first.bindings[0]["?message"], "m1");
        assert!(first.has_more);

        let second_query = json!({
            "find":["?message"],
            "inputs":{"recipient":"marici.Grothendieck"},
            "where":query["where"],
            "order_by":query["order_by"],
            "limit":1,
            "cursor":{"values":{"?sequence":1,"?event_id":"e1"}}
        });
        let second = execute(&parse(&second_query, 20, &limits()).expect("cursor parses"), &datoms)
            .expect("cursor executes");
        assert_eq!(second.bindings.len(), 1);
        assert_eq!(second.bindings[0]["?message"], "m2");
        assert!(!second.has_more);
    }

    #[test]
    fn reachability_is_bounded_and_joins_the_target_variable() {
        let query = json!({
            "find":["?message"],
            "inputs":{"root":"m0"},
            "where":[
                {"reachable":{"from":{"input":"root"},"attribute":"replied_by","to":"?message","max_depth":2}}
            ],
            "order_by":[{"term":"?message"}],
            "limit":10
        });
        let datoms = vec![
            datom("m0", "replied_by", json!("m1"), 1, "e1"),
            datom("m1", "replied_by", json!("m2"), 2, "e2"),
            datom("m2", "replied_by", json!("m3"), 3, "e3"),
        ];
        let result = execute(&parse(&query, 20, &limits()).expect("query parses"), &datoms)
            .expect("query executes");
        let ids = result
            .bindings
            .iter()
            .map(|binding| binding["?message"].as_str().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(ids, vec!["m1", "m2"]);
    }

    #[test]
    fn unbound_reachability_and_compare_are_refused() {
        let reachable = json!({
            "find":["?message"],
            "where":[{"reachable":{"from":"?root","attribute":"replied_by","to":"?message"}}],
            "order_by":[{"term":"?message"}],
            "limit":10
        });
        let failure = execute(
            &parse(&reachable, 20, &limits()).expect("reachable query parses"),
            &[],
        )
        .expect_err("unbound reachable source must refuse");
        assert_eq!(failure.code, "query_reachable_unbound");

        let compare_query = json!({
            "find":["?message"],
            "where":[{"compare":{"op":"=","left":"?missing","right":1}}],
            "limit":10
        });
        let failure = execute(
            &parse(&compare_query, 20, &limits()).expect("compare query parses"),
            &[],
        )
        .expect_err("unbound compare term must refuse");
        assert_eq!(failure.code, "query_compare_unbound");
    }

    #[test]
    fn multiple_nested_predicates_filter_the_entire_binding_set() {
        let query = json!({
            "find":["?message"],
            "where":[
                {"triple":{"subject":"?message","attribute":"kind","object":"communication"}},
                {"not_exists":{"where":[
                    {"triple":{"subject":"?receipt","attribute":"kind","object":"message_read"}},
                    {"triple":{"subject":"?receipt","attribute":"message_id","object":"?message"}}
                ]}},
                {"not_exists":{"where":[
                    {"triple":{"subject":"?reply","attribute":"replies_to","object":"?message"}}
                ]}}
            ],
            "order_by":[{"term":"?message"}],
            "limit":10
        });
        let datoms = vec![
            datom("m1", "kind", json!("communication"), 1, "e1"),
            datom("m2", "kind", json!("communication"), 2, "e2"),
            datom("m3", "kind", json!("communication"), 3, "e3"),
            datom("r1", "kind", json!("message_read"), 4, "e4"),
            datom("r1", "message_id", json!("m1"), 4, "e4"),
            datom("reply", "replies_to", json!("m2"), 5, "e5"),
        ];
        let result = execute(&parse(&query, 20, &limits()).expect("query parses"), &datoms)
            .expect("nested predicates execute");
        assert_eq!(result.bindings.len(), 1);
        assert_eq!(result.bindings[0]["?message"], "m3");
    }

    #[test]
    fn nested_predicates_are_planned_by_bindings() {
        let query = json!({
            "find":["?message"],
            "where":[
                {"triple":{"subject":"?message","attribute":"kind","object":"communication"}},
                {"exists":{"where":[
                    {"compare":{"op":"=","left":"?reply_kind","right":"reply"}},
                    {"triple":{"subject":"?reply","attribute":"kind","object":"?reply_kind"}},
                    {"triple":{"subject":"?reply","attribute":"target","object":"?message"}}
                ]}}
            ],
            "order_by":[{"term":"?message"}],
            "limit":10
        });
        let datoms = vec![
            datom("m1", "kind", json!("communication"), 1, "e1"),
            datom("m2", "kind", json!("communication"), 2, "e2"),
            datom("r1", "kind", json!("reply"), 3, "e3"),
            datom("r1", "target", json!("m1"), 3, "e3"),
            datom("r2", "kind", json!("note"), 4, "e4"),
            datom("r2", "target", json!("m2"), 4, "e4"),
        ];
        let result = execute(&parse(&query, 20, &limits()).expect("query parses"), &datoms)
            .expect("nested planner should move compare after its binding triple");
        assert_eq!(result.bindings.len(), 1);
        assert_eq!(result.bindings[0]["?message"], "m1");
    }

    #[test]
    fn cursor_without_order_is_refused() {
        let query = json!({
            "find":["?message"],
            "where":[{"triple":{"subject":"?message","attribute":"kind","object":"claim"}}],
            "cursor":{"values":{"?message":"m1"}}
        });
        let failure = parse(&query, 20, &limits()).expect_err("unordered cursor must refuse");
        assert_eq!(failure.code, "query_cursor_requires_order");
    }

    #[test]
    fn cursor_type_mismatch_is_refused() {
        let query = json!({
            "find":["?message"],
            "where":[{"triple":{"subject":"?message","attribute":"kind","object":"claim"}}],
            "order_by":[{"term":"?message"}],
            "limit":1,
            "cursor":{"values":{"?message":1}}
        });
        let spec = parse(&query, 20, &limits()).expect("cursor parses");
        let failure = execute(
            &spec,
            &[datom("m1", "kind", json!("claim"), 1, "e1")],
        )
        .expect_err("incomparable cursor must refuse");
        assert_eq!(failure.code, "query_cursor_type_mismatch");
    }

    #[test]
    fn paginated_order_must_be_unique() {
        let query = json!({
            "find":["?message"],
            "where":[{"triple":{"subject":"?message","attribute":"kind","object":"?kind"}}],
            "order_by":[{"term":"?kind"}],
            "limit":1
        });
        let spec = parse(&query, 20, &limits()).expect("query parses");
        let failure = execute(
            &spec,
            &[
                datom("m1", "kind", json!("claim"), 1, "e1"),
                datom("m2", "kind", json!("claim"), 2, "e2"),
            ],
        )
        .expect_err("non-unique ordering must refuse pagination");
        assert_eq!(failure.code, "query_order_not_unique");
    }

    #[test]
    fn exists_and_not_exists_filter_without_leaking_inner_bindings() {
        let query = json!({
            "find":["?message"],
            "inputs":{"reader":"marici.Grothendieck"},
            "where":[
                {"triple":{"subject":"?message","attribute":"kind","object":"communication"}},
                {"not_exists":{"where":[
                    {"triple":{"subject":"?receipt","attribute":"kind","object":"message_read"}},
                    {"triple":{"subject":"?receipt","attribute":"message_id","object":"?message"}},
                    {"triple":{"subject":"?receipt","attribute":"reader","object":{"input":"reader"}}}
                ]}}
            ],
            "order_by":[{"term":"?message"}],
            "limit":10
        });
        let datoms = vec![
            datom("m1", "kind", json!("communication"), 1, "e1"),
            datom("m2", "kind", json!("communication"), 2, "e2"),
            datom("r1", "kind", json!("message_read"), 3, "e3"),
            datom("r1", "message_id", json!("m1"), 3, "e3"),
            datom("r1", "reader", json!("marici.Grothendieck"), 3, "e3"),
        ];
        let spec = parse(&query, 20, &limits()).expect("query parses");
        let result = execute(&spec, &datoms).expect("query executes");
        assert_eq!(result.bindings.len(), 1);
        assert_eq!(result.bindings[0]["?message"], "m2");
        assert!(!result.bindings[0].contains_key("?receipt"));
    }

    #[test]
    fn unknown_query_fields_are_refused() {
        let query = json!({
            "find":["?message"],
            "where":[{"triple":{"subject":"?message","attribute":"kind","object":"claim"}}],
            "wat":true
        });
        let failure = parse(&query, 20, &limits()).expect_err("unknown query fields must refuse");
        assert_eq!(failure.code, "query_invalid_field");
    }

    #[test]
    fn nested_query_fields_and_ambiguous_forms_are_refused() {
        let nested_unknown = json!({
            "find":["?message"],
            "where":[{"triple":{
                "subject":"?message",
                "attribute":"kind",
                "object":"claim",
                "typo":true
            }}]
        });
        let failure = parse(&nested_unknown, 20, &limits())
            .expect_err("unknown nested clause fields must refuse");
        assert_eq!(failure.code, "query_invalid_field");

        let ambiguous_triple = json!({
            "find":["?message"],
            "where":[{"triple":{
                "subject":"?message",
                "attribute":"kind",
                "object":"claim",
                "value":"claim"
            }}]
        });
        let failure = parse(&ambiguous_triple, 20, &limits())
            .expect_err("object/value aliases must not be ambiguous");
        assert_eq!(failure.code, "query_invalid_clause");

        let invalid_types = [
            (
                json!({
                    "find":["?message"],
                    "inputs":true,
                    "where":[{"triple":{"subject":"?message","attribute":"kind","object":"claim"}}]
                }),
                "query_invalid_inputs",
            ),
            (
                json!({
                    "find":["?message"],
                    "limit":"one",
                    "where":[{"triple":{"subject":"?message","attribute":"kind","object":"claim"}}]
                }),
                "query_invalid_limit",
            ),
            (
                json!({
                    "find":["?message"],
                    "order_by":{},
                    "where":[{"triple":{"subject":"?message","attribute":"kind","object":"claim"}}]
                }),
                "query_invalid_order",
            ),
        ];
        for (query, expected_code) in invalid_types {
            let failure = parse(&query, 20, &limits()).expect_err("invalid query types must refuse");
            assert_eq!(failure.code, expected_code);
        }
    }

    #[test]
    fn nested_predicates_share_the_intermediate_work_bound() {
        let query = json!({
            "find":["?message"],
            "where":[
                {"triple":{"subject":"?message","attribute":"kind","object":"communication"}},
                {"exists":{"where":[
                    {"triple":{"subject":"?receipt","attribute":"kind","object":"message_read"}}
                ]}}
            ],
            "limit":1
        });
        let mut datoms = vec![datom("m1", "kind", json!("communication"), 1, "e1")];
        for index in 0..1001 {
            datoms.push(datom(
                &format!("receipt-{index}"),
                "kind",
                json!("message_read"),
                2,
                "e2",
            ));
        }
        let spec = parse(&query, 20, &limits()).expect("query parses");
        let failure = execute(&spec, &datoms).expect_err("nested intermediate work must refuse");
        assert_eq!(failure.code, "query_work_limit");
    }
}
