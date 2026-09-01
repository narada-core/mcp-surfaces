// Generic ledger-domain engine: hosts one `narada.ledger-domain.v1`
// descriptor as a complete event-ledger MCP domain.
//
// The behavior is lifted from the epistemic-graph implementation
// (`packages/shared/mcp-surfaces-native/native/src/epistemic_graph.rs`) with
// every domain constant replaced by descriptor reads: vocabulary, operation
// validation and ID derivation, projection DDL and fold, query behavior,
// numeric caps, schema ids, storage layout, guidance text, and the five
// feature modules (proposals, sequences, source_inspect, snapshot, export).
// Digest-bearing JSON emission keeps its original field insertion order.

use crate::descriptor::{DerivedKeyRecipe, Descriptor};
use narada_mcp_event_ledger::digest::{now, safe_name, sha256};
use narada_mcp_event_ledger::ledger::LedgerLayout;
use narada_mcp_event_ledger::{
    args as ledger_args, chain, io as ledger_io, ledger as event_ledger, lock,
    projection as ledger_projection, query as ledger_query, ErrorSchema,
};
use rusqlite::{params, types::ToSql, Connection, OptionalExtension, Transaction};
use serde_json::{json, Map, Value};
use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use uuid::Uuid;

/// One `create table` statement parsed out of the projection DDL.
#[derive(Clone, Debug)]
struct TableSpec {
    name: String,
    columns: Vec<String>,
    primary_key: String,
}

#[derive(Default)]
struct QuerySeedPlan {
    attribute_values: Vec<(String, Value)>,
    subject_attributes: Vec<(String, String)>,
    attributes: Vec<String>,
    unindexable: bool,
}

impl QuerySeedPlan {
    fn push_attribute(&mut self, attribute: &str) {
        if !self.attributes.iter().any(|value| value == attribute) {
            self.attributes.push(attribute.to_string());
        }
    }

    fn push_attribute_value(&mut self, attribute: &str, value: Value) {
        if !self
            .attribute_values
            .iter()
            .any(|(candidate, existing)| candidate == attribute && existing == &value)
        {
            self.attribute_values.push((attribute.to_string(), value));
        }
    }

    fn push_subject_attribute(&mut self, subject: &str, attribute: &str) {
        let candidate = (subject.to_string(), attribute.to_string());
        if !self.subject_attributes.contains(&candidate) {
            self.subject_attributes.push(candidate);
        }
    }
}

fn query_term_values(term: &ledger_query::Term, inputs: &Map<String, Value>) -> Option<Vec<Value>> {
    match term {
        ledger_query::Term::Value(value) => Some(vec![value.clone()]),
        ledger_query::Term::OneOf(values) => Some(values.clone()),
        ledger_query::Term::Variable(variable) => {
            let key = variable.trim_start_matches('?');
            inputs
                .get(key)
                .or_else(|| inputs.get(variable))
                .cloned()
                .map(|value| vec![value])
        }
    }
}

fn query_attribute_values(
    term: &ledger_query::Term,
    inputs: &Map<String, Value>,
) -> Option<Vec<String>> {
    query_term_values(term, inputs).map(|values| {
        values
            .into_iter()
            .filter_map(|value| value.as_str().map(str::to_string))
            .collect()
    })
}

fn collect_query_seed_clauses(
    clauses: &[ledger_query::Clause],
    inputs: &Map<String, Value>,
    plan: &mut QuerySeedPlan,
    descend_nested: bool,
) {
    for clause in clauses {
        match clause {
            ledger_query::Clause::Triple {
                subject,
                attribute,
                object,
            } => {
                let Some(attributes) = query_attribute_values(attribute, inputs) else {
                    plan.unindexable = true;
                    continue;
                };
                let subject_values = query_term_values(subject, inputs).map(|values| {
                    values
                        .into_iter()
                        .filter_map(|value| value.as_str().map(str::to_string))
                        .collect::<Vec<_>>()
                });
                let object_values = query_term_values(object, inputs);
                for attribute in attributes {
                    if let Some(values) = object_values.as_ref() {
                        for value in values {
                            plan.push_attribute_value(&attribute, value.clone());
                        }
                    } else if let Some(values) = subject_values.as_ref() {
                        for subject in values {
                            plan.push_subject_attribute(subject, &attribute);
                        }
                    } else {
                        plan.push_attribute(&attribute);
                    }
                }
            }
            ledger_query::Clause::Reachable {
                from, attribute, ..
            } => {
                plan.push_attribute(attribute);
                if let Some(values) = query_term_values(from, inputs) {
                    for subject in values
                        .into_iter()
                        .filter_map(|value| value.as_str().map(str::to_string))
                    {
                        plan.push_subject_attribute(&subject, attribute);
                    }
                }
            }
            ledger_query::Clause::Exists { clauses }
            | ledger_query::Clause::NotExists { clauses } => {
                if descend_nested {
                    collect_query_seed_clauses(clauses, inputs, plan, true);
                }
            }
            ledger_query::Clause::Compare { .. } => {}
        }
    }
}

/// A loaded domain: the descriptor plus derived parse products.
pub struct Engine {
    pub domain: Descriptor,
    error: ErrorSchema,
    event_hash_field: &'static str,
    tables: Vec<TableSpec>,
    entity_table: String,
    relation_table: String,
    records_table: String,
    datoms_table: Option<String>,
    projection_meta_table: Option<String>,
}

/// Parse the `[..N]` truncation out of an id-recipe template.
fn template_truncation(template: &str, fallback: usize) -> usize {
    if let Some(start) = template.find("[..") {
        let rest = &template[start + 3..];
        if let Some(end) = rest.find(']') {
            if let Ok(value) = rest[..end].parse::<usize>() {
                return value;
            }
        }
    }
    fallback
}

/// Parse the literal prefix before the first `{` placeholder of a template.
fn template_prefix(template: &str) -> &str {
    match template.find('{') {
        Some(index) => &template[..index],
        None => template,
    }
}

/// Cursor tokens are opaque to callers while remaining self-contained and
/// portable across MCP transports. The payload is deliberately JSON so the
/// engine can keep accepting the pre-token object form during migration.
fn encode_cursor_token(value: &Value) -> String {
    let bytes = serde_json::to_vec(value).expect("cursor payload is serializable");
    let mut encoded = String::with_capacity(bytes.len() * 2 + 3);
    encoded.push_str("v1.");
    for byte in bytes {
        encoded.push_str(&format!("{byte:02x}"));
    }
    encoded
}

fn decode_cursor_token(value: &Value, expected_schema: &str) -> Result<Value, ()> {
    let Some(token) = value.as_str() else {
        return Ok(value.clone());
    };
    let Some(hex) = token.strip_prefix("v1.") else {
        return Err(());
    };
    if hex.is_empty() || hex.len() % 2 != 0 {
        return Err(());
    }
    let mut bytes = Vec::with_capacity(hex.len() / 2);
    let mut index = 0;
    while index < hex.len() {
        let byte = u8::from_str_radix(&hex[index..index + 2], 16).map_err(|_| ())?;
        bytes.push(byte);
        index += 2;
    }
    let decoded: Value = serde_json::from_slice(&bytes).map_err(|_| ())?;
    let valid = decoded
        .as_object()
        .map(|object| {
            object.get("schema").and_then(Value::as_str) == Some(expected_schema)
                && object.get("head").and_then(Value::as_str).is_some()
                && object.get("values").and_then(Value::as_object).is_some()
        })
        .unwrap_or(false);
    valid.then_some(decoded).ok_or(())
}

fn canonical_json(value: &Value) -> Value {
    match value {
        Value::Object(object) => {
            let mut keys = object.keys().cloned().collect::<Vec<_>>();
            keys.sort();
            let mut canonical = Map::new();
            for key in keys {
                if let Some(value) = object.get(&key) {
                    canonical.insert(key, canonical_json(value));
                }
            }
            Value::Object(canonical)
        }
        Value::Array(values) => Value::Array(values.iter().map(canonical_json).collect()),
        other => other.clone(),
    }
}

/// Parse the projection DDL into table specs. Column names and the primary
/// key come from each `create table` statement; non-table segments (pragma)
/// are skipped.
fn parse_ddl_tables(ddl: &str) -> Result<Vec<TableSpec>, String> {
    let mut tables = Vec::new();
    for segment in ddl.split(';') {
        let segment = segment.trim();
        let Some(rest) = segment.strip_prefix("create table ") else {
            continue;
        };
        let Some(open) = rest.find('(') else {
            return Err(format!("domain_invalid:projection_ddl:{segment}"));
        };
        let name = rest[..open].trim().to_string();
        let body = rest[open + 1..].trim_end_matches(')').trim();
        let mut columns = Vec::new();
        let mut primary_key = None;
        for column in body.split(',') {
            let column = column.trim();
            let Some(column_name) = column.split_whitespace().next() else {
                continue;
            };
            columns.push(column_name.to_string());
            if column.contains("primary key") {
                primary_key = Some(column_name.to_string());
            }
        }
        let primary_key = primary_key
            .ok_or_else(|| format!("domain_invalid:projection_ddl_no_primary_key:{name}"))?;
        tables.push(TableSpec {
            name,
            columns,
            primary_key,
        });
    }
    if tables.is_empty() {
        return Err("domain_invalid:projection_ddl_no_tables".to_string());
    }
    Ok(tables)
}

