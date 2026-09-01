// Bounded, read-only Datalog-like query primitives for event-ledger projections.
//
// The evaluator deliberately operates on normalized datoms rather than domain
// payload JSON. Domains provide the vocabulary and emit datoms; this module
// knows only joins, comparisons, bounded reachability, ordering, and paging.

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
                format!(
                    "one_of must contain at most {} values",
                    limits.max_one_of_values
                ),
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

