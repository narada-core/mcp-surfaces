//! Generic ledger-domain engine: hosts one `narada.ledger-domain.v1`
//! descriptor as a complete event-ledger MCP domain.
//!
//! The behavior is lifted from the epistemic-graph implementation
//! (`packages/shared/mcp-surfaces-native/native/src/epistemic_graph.rs`) with
//! every domain constant replaced by descriptor reads: vocabulary, operation
//! validation and ID derivation, projection DDL and fold, query behavior,
//! numeric caps, schema ids, storage layout, guidance text, and the five
//! feature modules (proposals, sequences, source_inspect, snapshot, export).
//! Digest-bearing JSON emission keeps its original field insertion order.

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

impl Engine {
    pub fn new(domain: Descriptor) -> Result<Engine, String> {
        let tables = parse_ddl_tables(&domain.projection.ddl)?;
        let entity_op = &domain.id_derivation.entity.applies_to;
        let relation_op = &domain.id_derivation.relation.applies_to;
        let fold_table = |operation: &str| {
            domain
                .projection
                .fold
                .iter()
                .find(|entry| entry.operation == operation)
                .map(|entry| entry.table.clone())
                .ok_or_else(|| format!("domain_invalid:projection_fold_missing:{operation}"))
        };
        let entity_table = fold_table(entity_op)?;
        let relation_table = fold_table(relation_op)?;
        let records_table = domain
            .projection
            .fold
            .iter()
            .find(|entry| entry.table != entity_table && entry.table != relation_table)
            .map(|entry| entry.table.clone())
            .ok_or_else(|| "domain_invalid:projection_fold_missing_records".to_string())?;
        let datoms_table = tables
            .iter()
            .find(|spec| spec.name == "datoms")
            .map(|spec| spec.name.clone());
        let projection_meta_table = tables
            .iter()
            .find(|spec| spec.name == "projection_meta")
            .map(|spec| spec.name.clone());
        for table in [&entity_table, &relation_table, &records_table] {
            if !tables.iter().any(|spec| &spec.name == table) {
                return Err(format!(
                    "domain_invalid:projection_fold_unknown_table:{table}"
                ));
            }
        }
        let error_schema: &'static str =
            Box::leak(domain.identity.error_schema_id.clone().into_boxed_str());
        let event_hash_field: &'static str =
            Box::leak(domain.storage.event_hash_field.clone().into_boxed_str());
        Ok(Engine {
            domain,
            error: ErrorSchema(error_schema),
            event_hash_field,
            tables,
            entity_table,
            relation_table,
            records_table,
            datoms_table,
            projection_meta_table,
        })
    }

    fn table(&self, name: &str) -> &TableSpec {
        self.tables
            .iter()
            .find(|spec| spec.name == name)
            .expect("fold tables are validated at load")
    }

    fn entity_op(&self) -> &str {
        &self.domain.id_derivation.entity.applies_to
    }

    fn relation_op(&self) -> &str {
        &self.domain.id_derivation.relation.applies_to
    }

    #[cfg(test)]
    fn max_operations(&self) -> usize {
        self.domain.caps.operations_per_proposal.max as usize
    }

    /// Schema id derived from the domain namespace: `<namespace>.<name>`.
    fn schema_id(&self, name: &str) -> String {
        format!("{}.{}", self.domain.identity.schema_namespace, name)
    }

    fn finalize_bounded_output(&self, response: &mut Value) -> Result<u64, Value> {
        let max_output_bytes = self.domain.caps.query_execution.max_output_bytes;
        let mut output_bytes = 0u64;
        for _ in 0..4 {
            response["output_bytes"] = json!(output_bytes);
            output_bytes = serde_json::to_vec(response)
                .map_err(|_| {
                    self.error(
                        "query_output_limit",
                        "query response could not be serialized",
                        Value::Null,
                    )
                })?
                .len() as u64;
        }
        response["output_bytes"] = json!(output_bytes);
        let actual_output_bytes = serde_json::to_vec(response)
            .map_err(|_| {
                self.error(
                    "query_output_limit",
                    "query response could not be serialized",
                    Value::Null,
                )
            })?
            .len() as u64;
        if actual_output_bytes > max_output_bytes {
            return Err(self.error(
                "query_output_limit",
                "query response exceeded the descriptor output-byte budget",
                json!({"output_bytes":actual_output_bytes,"max_output_bytes":max_output_bytes}),
            ));
        }
        if actual_output_bytes != output_bytes {
            response["output_bytes"] = json!(actual_output_bytes);
        }
        Ok(actual_output_bytes)
    }

    /// Tool name derived from the tool prefix: `<prefix>_<verb>`.
    fn tool_name(&self, verb: &str) -> String {
        format!("{}_{}", self.domain.identity.tool_prefix, verb)
    }

    fn expand_kind_values(&self, kinds: Vec<Value>) -> Result<Vec<Value>, Value> {
        let mut expanded = Vec::new();
        for kind in kinds {
            if !expanded.iter().any(|candidate| candidate == &kind) {
                expanded.push(kind);
            }
        }
        for (canonical, aliases) in &self.domain.query.kind_aliases {
            let matched = expanded.iter().any(|kind| {
                kind.as_str() == Some(canonical.as_str())
                    || aliases
                        .iter()
                        .any(|alias| kind.as_str() == Some(alias.as_str()))
            });
            if !matched {
                continue;
            }
            let canonical = Value::String(canonical.clone());
            if !expanded.iter().any(|kind| kind == &canonical) {
                expanded.push(canonical);
            }
            for alias in aliases {
                let alias = Value::String(alias.clone());
                if !expanded.iter().any(|kind| kind == &alias) {
                    expanded.push(alias);
                }
            }
        }
        let max_values = self.domain.query.max_one_of_values.unwrap_or(64).max(1);
        if expanded.len() > max_values {
            return Err(self.error(
                "query_kind_limit",
                "named kind aliases expand beyond the descriptor one_of budget",
                json!({"count":expanded.len(),"max":max_values}),
            ));
        }
        Ok(expanded)
    }

    fn expand_legacy_kind_value(&self, kind: &str) -> Result<Vec<Value>, Value> {
        let mut expanded = vec![Value::String(kind.to_string())];
        for (canonical, aliases) in &self.domain.query.kind_aliases {
            if canonical == kind || aliases.iter().any(|alias| alias == kind) {
                for candidate in std::iter::once(canonical).chain(aliases.iter()) {
                    let candidate = Value::String(candidate.clone());
                    if !expanded.iter().any(|value| value == &candidate) {
                        expanded.push(candidate);
                    }
                }
            }
        }
        let max_values = self.domain.query.max_one_of_values.unwrap_or(64).max(1);
        if expanded.len() > max_values {
            return Err(self.error(
                "query_kind_limit",
                "legacy kind aliases expand beyond the descriptor one_of budget",
                json!({"count":expanded.len(),"max":max_values}),
            ));
        }
        Ok(expanded)
    }

    fn configured_message_kinds(&self) -> HashSet<String> {
        let mut kinds = HashSet::new();
        for config in self.domain.query.named_queries.values() {
            if config.get("mode").and_then(Value::as_str) != Some("inbox") {
                continue;
            }
            let Some(canonical) = config.get("kind_key").and_then(Value::as_str) else {
                continue;
            };
            kinds.insert(canonical.to_string());
            if let Some(aliases) = self.domain.query.kind_aliases.get(canonical) {
                kinds.extend(aliases.iter().cloned());
            }
        }
        kinds
    }

    fn is_configured_message_kind(&self, kind: &str) -> bool {
        self.configured_message_kinds().contains(kind)
    }

    fn sql_quote(value: &str) -> String {
        format!("'{}'", value.replace('\'', "''"))
    }

    fn visible_entity_predicate(&self) -> String {
        self.domain
            .query
            .read_receipt_kind
            .as_deref()
            .map(|kind| format!("kind <> {}", Self::sql_quote(kind)))
            .unwrap_or_else(|| "1=1".to_string())
    }

    fn validate_named_filter_conflicts(&self, args: &Map<String, Value>) -> Result<(), Value> {
        let Some(matched) = args.get("match").and_then(Value::as_object) else {
            return Ok(());
        };
        for key in [
            "participant",
            "recipient",
            "sender",
            "from",
            "to",
            "direction",
            "viewer",
            "kinds",
            "since_event",
            "after_sequence",
            "intent",
            "read_state",
            "reply_state",
            "include_body",
            "limit",
        ] {
            if let (Some(flat), Some(nested)) = (args.get(key), matched.get(key)) {
                if flat != nested {
                    return Err(self.error(
                        "query_filter_conflict",
                        "flat and match query filters must agree when both are supplied",
                        json!({"field":key,"flat":flat,"match":nested}),
                    ));
                }
            }
        }
        Ok(())
    }

    fn validate_named_filter_types(
        &self,
        args: &Map<String, Value>,
        mode: &str,
    ) -> Result<(), Value> {
        let validate_value = |field: &str, value: &Value| {
            let valid = match field {
                "participant" | "recipient" | "sender" | "from" | "to" | "direction" | "viewer"
                | "intent" | "read_state" | "reply_state" | "root" | "template" => {
                    value.as_str().is_some_and(|text| !text.trim().is_empty())
                }
                "expected_ledger_head" => {
                    value.is_null() || value.as_str().is_some_and(|text| !text.trim().is_empty())
                }
                "kinds" => value.as_array().is_some_and(|values| {
                    !values.is_empty()
                        && values.len() <= self.domain.query.max_one_of_values.unwrap_or(64).max(1)
                        && values
                            .iter()
                            .all(|item| item.as_str().is_some_and(|text| !text.trim().is_empty()))
                }),
                "since_event" | "after_sequence" | "max_depth" | "max_datoms" | "max_results"
                | "timeout_ms" => value.as_u64().is_some(),
                "include_body" => value.is_boolean(),
                "limit" => value.as_u64().is_some_and(|limit| limit > 0),
                "cursor" => value.is_null() || value.is_string() || value.is_object(),
                _ => true,
            };
            if valid {
                Ok(())
            } else {
                Err(self.error(
                    "query_filter_type_invalid",
                    "named query filter has an invalid type or value",
                    json!({"template":mode,"field":field}),
                ))
            }
        };

        for (field, value) in args {
            if field == "match" {
                if !value.is_object() {
                    return Err(self.error(
                        "query_filter_type_invalid",
                        "named query match must be an object",
                        json!({"template":mode,"field":field}),
                    ));
                }
            } else {
                validate_value(field, value)?;
            }
        }
        if let Some(matched) = args.get("match").and_then(Value::as_object) {
            for (field, value) in matched {
                validate_value(field, value)?;
            }
        }
        Ok(())
    }

    fn validate_named_query_fields(
        &self,
        args: &Map<String, Value>,
        mode: &str,
    ) -> Result<(), Value> {
        let allowed = match mode {
            "inbox" => [
                "template",
                "recipient",
                "participant",
                "sender",
                "from",
                "to",
                "kinds",
                "since_event",
                "after_sequence",
                "include_body",
                "direction",
                "viewer",
                "intent",
                "read_state",
                "reply_state",
                "match",
                "limit",
                "cursor",
                "expected_ledger_head",
                "max_datoms",
                "max_results",
                "timeout_ms",
                "budget_escalation",
            ]
            .as_slice(),
            "thread" => [
                "template",
                "root",
                "max_depth",
                "viewer",
                "limit",
                "cursor",
                "match",
                "expected_ledger_head",
            ]
            .as_slice(),
            _ => [].as_slice(),
        };
        for key in args.keys() {
            if !allowed.contains(&key.as_str()) {
                return Err(self.error(
                    "query_filter_unsupported",
                    "the named query does not accept this argument",
                    json!({"template":mode,"field":key}),
                ));
            }
        }
        if let Some(matched) = args.get("match").and_then(Value::as_object) {
            let allowed_match = match mode {
                "inbox" => [
                    "recipient",
                    "participant",
                    "sender",
                    "from",
                    "to",
                    "kinds",
                    "since_event",
                    "after_sequence",
                    "include_body",
                    "direction",
                    "viewer",
                    "intent",
                    "read_state",
                    "reply_state",
                    "limit",
                ]
                .as_slice(),
                "thread" => ["viewer", "limit"].as_slice(),
                _ => [].as_slice(),
            };
            for key in matched.keys() {
                if !allowed_match.contains(&key.as_str()) {
                    return Err(self.error(
                        "query_filter_unsupported",
                        "the named query match object contains an unsupported field",
                        json!({"template":mode,"field":key}),
                    ));
                }
            }
        }
        Ok(())
    }

    fn validate_raw_query_arguments(&self, args: &Map<String, Value>) -> Result<(), Value> {
        let allowed = [
            "query",
            "limit",
            "cursor",
            "expected_ledger_head",
            "max_datoms",
            "max_results",
            "timeout_ms",
            "budget_escalation",
        ];
        for key in args.keys() {
            if !allowed.contains(&key.as_str()) {
                return Err(self.error(
                    "query_mode_mixed",
                    "raw Datalog query accepts query, pagination, and bounded execution controls only",
                    json!({"field":key}),
                ));
            }
        }
        if let Some(limit) = args.get("limit") {
            if !limit.as_u64().is_some_and(|value| value > 0) {
                return Err(self.error(
                    "query_control_type_invalid",
                    "raw query limit must be a positive integer",
                    json!({"field":"limit"}),
                ));
            }
        }
        for field in ["max_datoms", "max_results", "timeout_ms"] {
            if let Some(value) = args.get(field) {
                if !value.as_u64().is_some_and(|value| value > 0) {
                    return Err(self.error(
                        "query_control_type_invalid",
                        "query budget controls must be positive integers",
                        json!({"field":field}),
                    ));
                }
            }
        }
        if args.contains_key("budget_escalation") {
            return Err(self.error(
                "query_budget_escalation_unavailable",
                "this surface has no descriptor-admitted privileged query budget",
                json!({"required":"descriptor-owned maintenance authority with audit evidence"}),
            ));
        }
        if let Some(cursor) = args.get("cursor") {
            if !(cursor.is_null() || cursor.is_string() || cursor.is_object()) {
                return Err(self.error(
                    "query_control_type_invalid",
                    "raw query cursor must be a string, object, or null",
                    json!({"field":"cursor"}),
                ));
            }
        }
        if let Some(expected) = args.get("expected_ledger_head") {
            if !(expected.is_null()
                || expected
                    .as_str()
                    .is_some_and(|value| !value.trim().is_empty()))
            {
                return Err(self.error(
                    "query_control_type_invalid",
                    "expected_ledger_head must be a non-empty string or null",
                    json!({"field":"expected_ledger_head"}),
                ));
            }
        }
        let Some(query) = args.get("query").and_then(Value::as_object) else {
            return Ok(());
        };
        for key in ["limit", "cursor"] {
            if let (Some(nested), Some(top_level)) = (query.get(key), args.get(key)) {
                if nested != top_level {
                    return Err(self.error(
                        "query_override_conflict",
                        "top-level query controls must agree with the same control nested in query",
                        json!({"field":key,"nested":nested,"top_level":top_level}),
                    ));
                }
            }
        }
        Ok(())
    }

    fn canonical_named_template(&self, template: &str) -> String {
        if self.domain.query.named_queries.contains_key(template) {
            return template.to_string();
        }
        let namespaced = format!("{}:{template}", self.domain.identity.schema_namespace);
        if self.domain.query.named_queries.contains_key(&namespaced) {
            return namespaced;
        }
        let suffix = format!(":{template}");
        self.domain
            .query
            .named_queries
            .keys()
            .find(|candidate| candidate.ends_with(&suffix))
            .cloned()
            .unwrap_or_else(|| template.to_string())
    }

    pub fn list_tools(&self) -> Vec<Value> {
        self.domain
            .tools
            .iter()
            .filter(|tool| {
                tool.feature
                    .as_deref()
                    .map(|feature| self.domain.features.enabled(feature))
                    .unwrap_or(true)
            })
            .map(|tool| {
                json!({
                    "name": tool.name,
                    "description": tool.description,
                    "inputSchema": tool.input_schema,
                    "annotations": tool.annotations,
                })
            })
            .collect()
    }

    pub fn call_tool(
        &self,
        name: &str,
        args: &Map<String, Value>,
        site_root: &Path,
    ) -> Result<Value, Value> {
        let prefix = format!("{}_", self.domain.identity.tool_prefix);
        let unknown =
            || Err(self.error("unknown_tool", &format!("unknown_tool:{name}"), Value::Null));
        let Some(verb) = name.strip_prefix(&prefix) else {
            return unknown();
        };
        let advertised = self.domain.tools.iter().any(|tool| {
            tool.name == name
                && tool
                    .feature
                    .as_deref()
                    .map(|feature| self.domain.features.enabled(feature))
                    .unwrap_or(true)
        });
        if !advertised {
            return unknown();
        }
        // Feature-owned verbs dispatch only when the feature is enabled.
        let feature = match verb {
            "source_inspect" => Some("source_inspect"),
            "snapshot" => Some("snapshot"),
            "export" => Some("export"),
            "sequence_create"
            | "sequence_status"
            | "sequence_list"
            | "sequence_claim_next"
            | "sequence_claims" => Some("sequences"),
            "proposal_submit"
            | "submit_review_admit"
            | "capture_sources"
            | "proposal_read"
            | "proposal_resubmit"
            | "proposal_review"
            | "proposal_admit"
            | "proposal_reject" => Some("proposals"),
            _ => None,
        };
        if let Some(feature) = feature {
            if !self.domain.features.enabled(feature) {
                return unknown();
            }
        }
        match verb {
            "guidance" => Ok(self.guidance_with_request(args)),
            "status" => self.status(site_root),
            "communication_migration_preflight" => {
                self.communication_migration_preflight(site_root, args)
            }
            "communication_migrate" => self.communication_migrate(site_root, args),
            "query" => {
                let has_raw_query = args.contains_key("query");
                let has_template = args.contains_key("template");
                let named_fields = [
                    "recipient",
                    "participant",
                    "sender",
                    "from",
                    "to",
                    "kinds",
                    "since_event",
                    "after_sequence",
                    "include_body",
                    "direction",
                    "viewer",
                    "intent",
                    "read_state",
                    "reply_state",
                    "match",
                    "root",
                    "max_depth",
                ];
                let legacy_fields = ["kind", "record_kind", "text", "compact", "offset"];
                let has_named_fields = named_fields.iter().any(|field| args.contains_key(*field));
                let has_legacy_fields = legacy_fields.iter().any(|field| args.contains_key(*field));
                let has_cursor = args
                    .get("cursor")
                    .map(|value| !value.is_null())
                    .unwrap_or(false);
                if has_raw_query && has_template {
                    Err(self.error(
                        "query_mode_ambiguous",
                        "query and template are mutually exclusive",
                        Value::Null,
                    ))
                } else if !has_raw_query && !has_template && has_cursor {
                    Err(self.error(
                        "query_cursor_unsupported",
                        "legacy queries use offset pagination; cursor requires query or template",
                        Value::Null,
                    ))
                } else if has_raw_query && (has_named_fields || has_legacy_fields) {
                    Err(self.error(
                        "query_mode_mixed",
                        "raw Datalog query cannot be combined with named-query filters",
                        json!({"fields":named_fields.iter().chain(legacy_fields.iter()).filter(|field| args.contains_key(**field)).collect::<Vec<_>>() }),
                    ))
                } else if has_raw_query || has_template {
                    if has_raw_query {
                        self.validate_raw_query_arguments(args)?;
                    }
                    self.generic_query(site_root, args)
                } else if has_named_fields {
                    Err(self.error(
                        "query_template_missing",
                        "template is required when named-query filters are supplied",
                        Value::Null,
                    ))
                } else {
                    self.query(site_root, args)
                }
            }
            "message_mark_read" => self.message_mark_read(site_root, args),
            "query_batch" => self.query_batch(site_root, args),
            "source_inspect" => self.source_inspect(site_root, args),
            "neighborhood" => self.neighborhood(site_root, args),
            "snapshot" => self.snapshot(site_root, args),
            "sequence_create" => self.sequence_create(site_root, args),
            "sequence_status" => self.sequence_status(site_root, args),
            "sequence_list" => self.sequence_list(site_root, args),
            "sequence_claim_next" => self.sequence_claim_next(site_root, args),
            "sequence_claims" => self.sequence_claims(site_root, args),
            "proposal_submit" => {
                let payload_ref = args.get("payload_ref").and_then(Value::as_str);
                let resolved = self.resolve_payload_arguments(site_root, args)?;
                self.proposal_submit(site_root, &resolved)
                    .map_err(|error| self.enrich_payload_ref_refusal(error, payload_ref, name))
            }
            "submit_review_admit" => {
                let payload_ref = args.get("payload_ref").and_then(Value::as_str);
                let resolved = self.resolve_payload_arguments(site_root, args)?;
                self.submit_review_admit(site_root, &resolved)
                    .map_err(|error| self.enrich_payload_ref_refusal(error, payload_ref, name))
            }
            "capture_sources" => self.capture_sources(site_root, args),
            "proposal_read" => self.proposal_read(site_root, args),
            "proposal_resubmit" => self.proposal_resubmit(site_root, args),
            "proposal_review" => self.proposal_review(site_root, args),
            "proposal_admit" => self.proposal_admit(site_root, args),
            "proposal_reject" => self.proposal_reject(site_root, args),
            "export" => self.export(site_root, args),
            _ => unknown(),
        }
    }

    fn resolve_payload_arguments(
        &self,
        root: &Path,
        args: &Map<String, Value>,
    ) -> Result<Map<String, Value>, Value> {
        let Some(reference) = args.get("payload_ref").and_then(Value::as_str) else {
            return Ok(args.clone());
        };
        if args.len() != 1 {
            return Err(self.error(
                "payload_ref_ambiguous",
                "payload_ref cannot be combined with inline proposal arguments",
                json!({"payload_ref":reference}),
            ));
        }
        let body = reference.strip_prefix("mcp_payload:").ok_or_else(|| {
            self.error(
                "payload_ref_invalid",
                "payload_ref must use mcp_payload:<id>@v<revision>",
                json!({"payload_ref":reference}),
            )
        })?;
        let (payload_id, revision_text) = body.split_once("@v").ok_or_else(|| {
            self.error(
                "payload_ref_invalid",
                "payload_ref must include an immutable revision",
                json!({"payload_ref":reference}),
            )
        })?;
        if !(3..=64).contains(&payload_id.len())
            || !payload_id
                .chars()
                .next()
                .is_some_and(|ch| ch.is_ascii_alphanumeric())
            || !payload_id
                .chars()
                .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_'))
        {
            return Err(self.error(
                "payload_ref_invalid",
                "payload_ref id is invalid",
                json!({"payload_ref":reference}),
            ));
        }
        let revision = revision_text
            .parse::<u64>()
            .ok()
            .filter(|value| *value > 0)
            .ok_or_else(|| {
                self.error(
                    "payload_ref_invalid",
                    "payload_ref revision must be a positive integer",
                    json!({"payload_ref":reference}),
                )
            })?;
        let path = root
            .join(".ai")
            .join("tmp")
            .join("mcp-payloads")
            .join("workspace")
            .join(payload_id)
            .join(format!("v{revision}.json"));
        let metadata = fs::metadata(&path).map_err(|_| {
            self.error(
                "payload_ref_not_found",
                "immutable payload revision was not found",
                json!({"payload_ref":reference}),
            )
        })?;
        const MAX_PAYLOAD_BYTES: u64 = 256 * 1024;
        if metadata.len() > MAX_PAYLOAD_BYTES {
            return Err(self.error(
                "payload_ref_too_large",
                "immutable payload revision exceeds the transport ceiling",
                json!({"payload_ref":reference,"byte_size":metadata.len(),"max_bytes":MAX_PAYLOAD_BYTES}),
            ));
        }
        let record = self.read_json(&path)?;
        if record.get("schema").and_then(Value::as_str) != Some("narada.mcp_payload.revision.v1")
            || record.get("ref").and_then(Value::as_str) != Some(reference)
            || record.get("payload_id").and_then(Value::as_str) != Some(payload_id)
            || record.get("revision").and_then(Value::as_u64) != Some(revision)
        {
            return Err(self.error(
                "payload_ref_metadata_mismatch",
                "immutable payload metadata does not match its reference",
                json!({"payload_ref":reference}),
            ));
        }
        let payload = record
            .get("payload")
            .and_then(Value::as_object)
            .ok_or_else(|| {
                self.error(
                    "payload_ref_payload_must_be_object",
                    "proposal payload must be a JSON object",
                    json!({"payload_ref":reference}),
                )
            })?;
        if payload.contains_key("payload_ref") {
            return Err(self.error(
                "payload_ref_recursive",
                "payload-backed arguments cannot contain another payload_ref",
                json!({"payload_ref":reference}),
            ));
        }
        let canonical = serde_json::to_vec(&canonical_json(&Value::Object(payload.clone())))
            .unwrap_or_default();
        if record.get("byte_size").and_then(Value::as_u64) != Some(canonical.len() as u64) {
            return Err(self.error(
                "payload_ref_byte_size_mismatch",
                "immutable payload byte size verification failed",
                json!({"payload_ref":reference}),
            ));
        }
        let actual_sha256 = sha256(&canonical);
        if record.get("sha256").and_then(Value::as_str) != Some(actual_sha256.as_str()) {
            return Err(self.error(
                "payload_ref_sha256_mismatch",
                "immutable payload digest verification failed",
                json!({"payload_ref":reference}),
            ));
        }
        Ok(payload.clone())
    }

    fn enrich_payload_ref_refusal(
        &self,
        mut error: Value,
        payload_ref: Option<&str>,
        retry_tool: &str,
    ) -> Value {
        let Some(reference) = payload_ref else {
            return error;
        };
        if error.get("code").and_then(Value::as_str)
            != Some(
                self.domain
                    .query
                    .communication
                    .legacy_write_refusal_code
                    .as_str(),
            )
        {
            return error;
        }
        let Some((payload_id, revision)) = reference
            .strip_prefix("mcp_payload:")
            .and_then(|body| body.rsplit_once("@v"))
            .and_then(|(id, revision)| revision.parse::<u64>().ok().map(|value| (id, value)))
        else {
            return error;
        };
        let canonical = self.domain.query.communication.canonical_kind.clone();
        let supplied = error
            .pointer("/details/supplied_kind")
            .cloned()
            .unwrap_or(Value::Null);
        if let Some(details) = error.get_mut("details").and_then(Value::as_object_mut) {
            details.insert("input_transport".into(), json!("immutable_payload_ref"));
            details.insert("payload_ref".into(), json!(reference));
            details.insert("payload_revision_mutable".into(), json!(false));
            details.insert("graph_mutation_committed".into(), json!(false));
            details.insert(
                "remediation".into(),
                json!("Create a successor immutable payload revision with canonical communication kinds, then retry the same submission tool. Do not edit or retry the rejected revision."),
            );
            details.insert(
                "recovery".into(),
                json!({
                    "action":"create_successor_payload_revision",
                    "source_payload_ref":reference,
                    "suggested_payload_ref":format!("mcp_payload:{payload_id}@v{}", revision + 1),
                    "preserve_source_revision":true,
                    "replace":{"entity.kind":{"from":supplied,"to":canonical}},
                    "payload_revision_tools":{
                        "read":"mcp_payload_show",
                        "derive":"mcp_payload_derive",
                        "validate":"mcp_payload_validate",
                        "surface":"task-lifecycle"
                    },
                    "then_retry_with":{"argument":"payload_ref","tool":retry_tool}
                }),
            );
        }
        error
    }

    fn communication_migration_preflight(
        &self,
        root: &Path,
        args: &Map<String, Value>,
    ) -> Result<Value, Value> {
        self.prepare(root)?;
        let requested = args.get("limit").and_then(Value::as_u64).unwrap_or(50);
        let limit = requested.clamp(1, self.domain.caps.operations_per_proposal.max) as usize;
        let head = self
            .status(root)?
            .get("ledger_head")
            .cloned()
            .unwrap_or(Value::Null);
        let communication = &self.domain.query.communication;
        let cursor_schema = self.schema_id("communication_migration_cursor.v1");
        let query_digest = sha256(
            &serde_json::to_vec(&json!({
                "canonical_kind": communication.canonical_kind,
                "legacy_read_aliases": communication.legacy_read_aliases,
                "contract_version": communication.contract_version
            }))
            .unwrap_or_default(),
        );
        let cursor = if let Some(raw_cursor) = args.get("cursor") {
            let decoded = decode_cursor_token(raw_cursor, &cursor_schema).map_err(|_| {
                self.error(
                    "communication_migration_cursor_invalid",
                    "migration cursor is malformed or belongs to another operation",
                    json!({}),
                )
            })?;
            if decoded.get("ledger_head") != Some(&head)
                || decoded.get("query_digest").and_then(Value::as_str)
                    != Some(query_digest.as_str())
            {
                return Err(self.error(
                    "communication_migration_cursor_stale",
                    "migration cursor is not bound to the current ledger head and descriptor query",
                    json!({"cursor_ledger_head":decoded.get("ledger_head"),"actual_ledger_head":head}),
                ));
            }
            decoded
                .get("entity_id")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string()
        } else {
            String::new()
        };
        let db = Connection::open(self.projection_path(root))
            .map_err(self.db_error("projection_open_failed"))?;
        let placeholders = (0..communication.legacy_read_aliases.len())
            .map(|index| format!("?{}", index + 2))
            .collect::<Vec<_>>()
            .join(",");
        let sql = format!("select entity_id,kind,payload_json,event_id,event_sequence from {} where entity_id>?1 and kind in ({placeholders}) order by entity_id limit {}", self.entity_table, limit + 1);
        let mut parameters = Vec::<String>::new();
        parameters.push(cursor);
        parameters.extend(communication.legacy_read_aliases.iter().cloned());
        let mut statement = db
            .prepare(&sql)
            .map_err(self.db_error("communication_migration_prepare_failed"))?;
        let rows = statement
            .query_map(rusqlite::params_from_iter(parameters.iter()), |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, u64>(4)?,
                ))
            })
            .map_err(self.db_error("communication_migration_query_failed"))?;
        let mut candidates = rows
            .collect::<Result<Vec<_>, _>>()
            .map_err(self.db_error("communication_migration_row_failed"))?;
        let has_more = candidates.len() > limit;
        candidates.truncate(limit);
        let mut by_kind = BTreeMap::<String, u64>::new();
        let mut by_sender = BTreeMap::<String, u64>::new();
        let mut by_recipient = BTreeMap::<String, u64>::new();
        let mut operations = Vec::new();
        let mut census = Vec::new();
        for (entity_id, kind, payload_json, event_id, event_sequence) in candidates {
            let payload: Value = serde_json::from_str(&payload_json).map_err(|error| {
                self.error(
                    "communication_migration_payload_invalid",
                    &error.to_string(),
                    json!({"entity_id":entity_id}),
                )
            })?;
            let sender = payload
                .get("sender")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            let recipient = payload
                .get("recipient")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            *by_kind.entry(kind.clone()).or_default() += 1;
            *by_sender.entry(sender.clone()).or_default() += 1;
            *by_recipient.entry(recipient.clone()).or_default() += 1;
            let thread_member: bool = db.query_row(&format!("select exists(select 1 from {} where relation_type='replies_to' and (source_id=?1 or target_id=?1))", self.relation_table), params![entity_id], |row| row.get(0)).map_err(self.db_error("communication_migration_thread_census_failed"))?;
            let payload_sha256 = sha256(payload_json.as_bytes());
            operations.push(json!({"op":communication.canonicalization_operation,"entity_id":entity_id,"legacy_kind":kind,"canonical_kind":communication.canonical_kind,"equivalence_evidence":{"payload_sha256":payload_sha256,"originating_event_id":event_id}}));
            census.push(json!({"entity_id":entity_id,"kind":kind,"sender":sender,"recipient":recipient,"thread_member":thread_member,"event_id":event_id,"event_sequence":event_sequence,"payload_sha256":payload_sha256}));
        }
        let next_cursor = if has_more {
            census.last().and_then(|item| item.get("entity_id")).and_then(Value::as_str).map(|entity_id| {
                encode_cursor_token(&json!({"schema":cursor_schema,"ledger_head":head,"query_digest":query_digest,"entity_id":entity_id}))
            })
        } else {
            None
        };
        Ok(
            json!({"schema":self.schema_id("communication_migration_preflight.v1"),"status":"ok","ledger_head":head,"query_digest":query_digest,"canonical_kind":communication.canonical_kind,"contract_version":communication.contract_version,"bounded":{"limit":limit,"returned":census.len(),"has_more":has_more,"next_cursor":next_cursor},"census":{"scope":"page","by_kind":by_kind,"by_sender":by_sender,"by_recipient":by_recipient,"messages":census},"proposed_operations":operations}),
        )
    }

    fn communication_migrate(
        &self,
        root: &Path,
        args: &Map<String, Value>,
    ) -> Result<Value, Value> {
        let actor = self.required(args, "actor")?;
        let authority_basis = self.required_object(args, "authority_basis")?;
        let preflight = self.communication_migration_preflight(root, args)?;
        let operations = preflight
            .get("proposed_operations")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        if operations.is_empty() {
            return Ok(
                json!({"schema":self.schema_id("communication_migration.v1"),"status":"complete","migrated":0,"preflight":preflight}),
            );
        }
        let mut submit = Map::new();
        submit.insert("actor".into(), Value::String(actor));
        submit.insert("authority_basis".into(), authority_basis);
        submit.insert("operations".into(), Value::Array(operations.clone()));
        submit.insert(
            "expected_ledger_head".into(),
            preflight.get("ledger_head").cloned().unwrap_or(Value::Null),
        );
        submit.insert(
            "idempotency_key".into(),
            Value::String(format!(
                "communication-migration-{}",
                &sha256(&serde_json::to_vec(&operations).unwrap_or_default())[..24]
            )),
        );
        let admission = self.submit_review_admit(root, &submit)?;
        Ok(
            json!({"schema":self.schema_id("communication_migration.v1"),"status":"migrated","migrated":operations.len(),"preflight":preflight,"admission":admission}),
        )
    }

    fn sequence_create(&self, root: &Path, args: &Map<String, Value>) -> Result<Value, Value> {
        self.prepare(root)?;
        let name = self.validated_sequence_name(args)?;
        let actor = self.required(args, "actor")?;
        let authority_basis = self.required_object(args, "authority_basis")?;
        let start_at = self.optional_u64(args, "start_at", 1)?;
        if start_at < self.domain.features.sequences.start_at_min {
            return Err(self.error(
                "sequence_start_invalid",
                "sequence start_at must be at least 1",
                json!({"start_at":start_at}),
            ));
        }
        let lock_key = format!("sequence-{}", sha256(name.as_bytes()));
        self.with_authority_lock(root, &lock_key, || {
            let directory = self.sequence_directory(root, &name);
            let manifest_path = directory.join("sequence.json");
            if manifest_path.exists() {
                let manifest = self.read_json(&manifest_path)?;
                self.verify_sequence_manifest(&manifest, &name)?;
                if manifest.get("start_at").and_then(Value::as_u64) != Some(start_at) {
                    return Err(self.error(
                        "sequence_configuration_conflict",
                        "sequence already exists with a different start_at",
                        json!({"sequence_name":name,"existing_start_at":manifest.get("start_at"),"requested_start_at":start_at}),
                    ));
                }
                return self.sequence_status_value(root, &name, "already_exists");
            }
            fs::create_dir_all(directory.join("claims"))
                .map_err(self.io_error("sequence_claim_store_create_failed"))?;
            fs::create_dir_all(directory.join("idempotency"))
                .map_err(self.io_error("sequence_idempotency_store_create_failed"))?;
            let sequences = &self.domain.features.sequences;
            let mut manifest = json!({
                "schema":sequences.manifest_schema_id,
                "sequence_id":self.generated_sequence_id(&name),
                "sequence_name":name,
                "start_at":start_at,
                "step":sequences.step,
                "created_by":actor,
                "authority_basis":authority_basis,
                "idempotency_key":args.get("idempotency_key").cloned().unwrap_or(Value::Null),
                "created_at":now()
            });
            let hash = self.digest_value(&manifest)?;
            manifest[self.domain.features.sequences.manifest_hash_field.clone()] = json!(hash);
            self.write_new_json(&manifest_path, &manifest)?;
            self.sequence_status_value(root, &name, "created")
        })
    }

    fn sequence_status(&self, root: &Path, args: &Map<String, Value>) -> Result<Value, Value> {
        let name = self.validated_sequence_name(args)?;
        let lock_key = format!("sequence-{}", sha256(name.as_bytes()));
        self.with_authority_lock(root, &lock_key, || {
            self.sequence_status_value(root, &name, "ready")
        })
    }

    fn sequence_status_value(&self, root: &Path, name: &str, status: &str) -> Result<Value, Value> {
        let manifest = self.load_sequence_manifest(root, name)?;
        let claims = self.verified_sequence_claims(root, name, &manifest)?;
        let start_at = manifest["start_at"].as_u64().unwrap();
        let last_claim = claims.last().cloned().unwrap_or(Value::Null);
        let last_value = last_claim.get("value").and_then(Value::as_u64);
        let next_value = match last_value {
            Some(value) => value.checked_add(1).map(Value::from).unwrap_or(Value::Null),
            None => Value::from(start_at),
        };
        Ok(json!({
            "schema":self.domain.features.sequences.status_schema_id,
            "status":status,
            "sequence_id":manifest["sequence_id"],
            "sequence_name":name,
            "start_at":start_at,
            "step":self.domain.features.sequences.step,
            "claim_count":claims.len(),
            "last_claimed_value":last_value,
            "next_value":next_value,
            "exhausted":next_value.is_null(),
            "latest_claim":last_claim,
            "integrity_status":"valid"
        }))
    }

    fn sequence_list(&self, root: &Path, args: &Map<String, Value>) -> Result<Value, Value> {
        let limit = self.page_limit(args)?;
        let offset = self.page_offset(args)?;
        let mut items = Vec::new();
        if self.sequences(root).exists() {
            for entry in fs::read_dir(self.sequences(root))
                .map_err(self.io_error("sequence_store_read_failed"))?
            {
                let Ok(entry) = entry else { continue };
                let manifest_path = entry.path().join("sequence.json");
                if !manifest_path.exists() {
                    continue;
                }
                let hash = entry.file_name().to_string_lossy().to_string();
                let item = self.with_authority_lock(root, &format!("sequence-{hash}"), || {
                    let manifest = self.read_json(&manifest_path)?;
                    let name = manifest
                        .get("sequence_name")
                        .and_then(Value::as_str)
                        .ok_or_else(|| {
                            self.error(
                                "sequence_manifest_invalid",
                                "sequence manifest lacks sequence_name",
                                json!({"path":manifest_path.to_string_lossy()}),
                            )
                        })?;
                    self.verify_sequence_manifest(&manifest, name)?;
                    let claims = self.verified_sequence_claims(root, name, &manifest)?;
                    Ok(json!({
                        "sequence_id":manifest["sequence_id"],
                        "sequence_name":name,
                        "start_at":manifest["start_at"],
                        "claim_count":claims.len(),
                        "last_claimed_value":claims.last().and_then(|claim| claim.get("value")).cloned().unwrap_or(Value::Null),
                        "created_at":manifest["created_at"]
                    }))
                })?;
                items.push(item);
            }
        }
        items.sort_by(|left, right| {
            left["sequence_name"]
                .as_str()
                .cmp(&right["sequence_name"].as_str())
        });
        let total = items.len();
        let page = items
            .into_iter()
            .skip(offset)
            .take(limit)
            .collect::<Vec<_>>();
        let count = page.len();
        Ok(
            json!({"schema":self.domain.features.sequences.list_schema_id,"items":page,"offset":offset,"limit":limit,"count":count,"total":total,"has_more":offset+count<total}),
        )
    }

    fn sequence_claim_next(&self, root: &Path, args: &Map<String, Value>) -> Result<Value, Value> {
        self.prepare(root)?;
        let name = self.validated_sequence_name(args)?;
        let actor = self.required(args, "actor")?;
        let authority_basis = self.required_object(args, "authority_basis")?;
        let idempotency_key = self.required(args, "idempotency_key")?;
        let request_digest = self.digest_value(
            &json!({"sequence_name":name,"actor":actor,"authority_basis":authority_basis}),
        )?;
        let lock_key = format!("sequence-{}", sha256(name.as_bytes()));
        self.with_authority_lock(root, &lock_key, || {
            let manifest = self.load_sequence_manifest(root, &name)?;
            let claims = self.verified_sequence_claims(root, &name, &manifest)?;
            if let Some(claim) = Self::find_sequence_claim_by_idempotency(&claims, &idempotency_key) {
                if claim.get("request_digest").and_then(Value::as_str) != Some(request_digest.as_str())
                {
                    return Err(self.error(
                        "sequence_claim_idempotency_conflict",
                        "idempotency key already names a different claim request",
                        json!({"sequence_name":name,"idempotency_key":idempotency_key,"claim_id":claim["claim_id"]}),
                    ));
                }
                self.recover_sequence_idempotency_index(root, &name, &idempotency_key, claim)?;
                return Ok(self.sequence_claim_receipt(claim, true));
            }
            let start_at = manifest["start_at"].as_u64().unwrap();
            let value = match claims.last().and_then(|claim| claim["value"].as_u64()) {
                Some(previous) => previous.checked_add(1).ok_or_else(|| {
                    self.error(
                        "sequence_exhausted",
                        "sequence has exhausted u64 values",
                        json!({"sequence_name":name,"last_claimed_value":previous}),
                    )
                })?,
                None => start_at,
            };
            let chain_field = &self.domain.features.sequences.claim_chain_field;
            let previous_hash = claims
                .last()
                .and_then(|claim| claim[self.domain.features.sequences.claim_hash_field.clone()].as_str())
                .map(str::to_string);
            let claim_id = self.generated_claim_id(&name, &idempotency_key);
            let mut claim = json!({
                "schema":self.domain.features.sequences.claim_schema_id,
                "sequence_id":manifest["sequence_id"],
                "sequence_name":name,
                "value":value,
                "claim_id":claim_id,
                chain_field.clone():previous_hash,
                "actor":actor,
                "authority_basis":authority_basis,
                "idempotency_key":idempotency_key,
                "request_digest":request_digest,
                "claimed_at":now()
            });
            let claim_hash = self.digest_value(&claim)?;
            claim[self.domain.features.sequences.claim_hash_field.clone()] = json!(claim_hash);
            self.write_new_json(
                &self
                    .sequence_claims_directory(root, &name)
                    .join(self.sequence_claim_file_name(value)),
                &claim,
            )?;
            self.recover_sequence_idempotency_index(root, &name, &idempotency_key, &claim)?;
            Ok(self.sequence_claim_receipt(&claim, false))
        })
    }

    fn sequence_claims(&self, root: &Path, args: &Map<String, Value>) -> Result<Value, Value> {
        let name = self.validated_sequence_name(args)?;
        let limit = self.page_limit(args)?;
        let offset = self.page_offset(args)?;
        let lock_key = format!("sequence-{}", sha256(name.as_bytes()));
        self.with_authority_lock(root, &lock_key, || {
            let manifest = self.load_sequence_manifest(root, &name)?;
            let claims = self.verified_sequence_claims(root, &name, &manifest)?;
            let total = claims.len();
            let page = claims
                .into_iter()
                .skip(offset)
                .take(limit)
                .collect::<Vec<_>>();
            let count = page.len();
            Ok(
                json!({"schema":self.domain.features.sequences.claims_schema_id,"sequence_name":name,"claims":page,"offset":offset,"limit":limit,"count":count,"total":total,"has_more":offset+count<total}),
            )
        })
    }

    fn sequence_claim_receipt(&self, claim: &Value, replay: bool) -> Value {
        let next_value = claim["value"]
            .as_u64()
            .and_then(|value| value.checked_add(1));
        json!({
            "schema":self.domain.features.sequences.claim_receipt_schema_id,
            "status":if replay{"idempotent_replay"}else{"claimed"},
            "idempotency_replay":replay,
            "sequence_id":claim["sequence_id"],
            "sequence_name":claim["sequence_name"],
            "value":claim["value"],
            "claim_id":claim["claim_id"],
            "claimed_at":claim["claimed_at"],
            "next_value":next_value,
            "exhausted":next_value.is_none()
        })
    }

    fn proposal_submit(&self, root: &Path, args: &Map<String, Value>) -> Result<Value, Value> {
        self.prepare(root)?;
        let actor = self.required(args, "actor")?;
        let supplied_operations = args
            .get("operations")
            .and_then(Value::as_array)
            .ok_or_else(|| {
                self.error(
                    "invalid_proposal",
                    "operations must be an array",
                    Value::Null,
                )
            })?;
        let count = &self.domain.caps.operations_per_proposal;
        if supplied_operations.len() < count.min as usize
            || supplied_operations.len() > count.max as usize
        {
            return Err(self.error(
                "invalid_proposal",
                &format!(
                    "operations count must be between {} and {}",
                    count.min, count.max
                ),
                json!({"count":supplied_operations.len()}),
            ));
        }
        let operations = self.normalize_operations(supplied_operations)?;
        self.validate_operations(&operations, false)?;
        let expected = self.resolve_expected_ledger_head(root, args.get("expected_ledger_head"))?;
        let semantic_content = json!({"actor":actor,"authority_basis":args.get("authority_basis"),"operations":operations});
        let content_fingerprint = self.digest_value(&semantic_content)?;
        let idempotency_key = args
            .get("idempotency_key")
            .and_then(Value::as_str)
            .filter(|value| !value.trim().is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| {
                self.derived_idempotency_key(
                    &self.domain.id_derivation.derived_idempotency_keys.proposal,
                    &semantic_content,
                )
            });
        let proposal_id = format!(
            "{}{}",
            template_prefix(&self.domain.id_derivation.generated_ids.proposal_id),
            Uuid::new_v4()
        );
        let created_at = now();
        let proposals_feature = &self.domain.features.proposals;
        let payload = json!({
            "schema":proposals_feature.proposal_schema_id, "proposal_id":proposal_id,
            "status":"submitted", "actor":actor, "authority_basis":args.get("authority_basis"),
            "idempotency_key":idempotency_key, "expected_ledger_head":expected,
            "created_at":created_at, "content_fingerprint":content_fingerprint, "operations":operations
        });
        let digest = self.digest_value(&payload)?;
        let mut stored = payload;
        stored
            .as_object_mut()
            .unwrap()
            .insert("digest".into(), json!(digest));
        let idem_path = self
            .proposals(root)
            .join(format!("idem-{}.txt", safe_name(&idempotency_key)));
        if idem_path.exists() {
            let existing = fs::read_to_string(&idem_path)
                .map_err(self.io_error("proposal_idempotency_read_failed"))?;
            let stored = self.read_json(
                &self
                    .proposals(root)
                    .join(format!("{}.json", existing.trim())),
            )?;
            if stored
                .get("content_fingerprint")
                .and_then(Value::as_str)
                .is_some()
                && stored.get("content_fingerprint") != Some(&json!(content_fingerprint))
            {
                return Err(self.error(
                    "proposal_idempotency_conflict",
                    "idempotency key already names different proposal content",
                    json!({"idempotency_key":idempotency_key,"existing_proposal_id":stored["proposal_id"]}),
                ));
            }
            return Ok(self.proposal_receipt(&stored));
        }
        self.write_new_json(
            &self.proposals(root).join(format!("{proposal_id}.json")),
            &stored,
        )?;
        self.write_new(&idem_path, proposal_id.as_bytes())?;
        Ok(self.proposal_receipt(&stored))
    }

    fn submit_review_admit(&self, root: &Path, args: &Map<String, Value>) -> Result<Value, Value> {
        let proposals_feature = &self.domain.features.proposals;
        let submission = self.proposal_submit(root, args)?;
        let proposal_id = submission["proposal_id"].as_str().ok_or_else(|| {
            self.error(
                "proposal_submission_corrupt",
                "proposal id missing",
                submission.clone(),
            )
        })?;
        let lifecycle = self.proposal_lifecycle(root, proposal_id)?;
        if lifecycle["status"] == "admitted" {
            let review = self.read_json(
                &self
                    .proposals(root)
                    .join(format!("{}.review.json", safe_name(proposal_id))),
            )?;
            return Ok(json!({
                "schema":proposals_feature.compound_schema_id,
                "status":"already_admitted",
                "submission":submission,
                "review":review,
                "admission":lifecycle,
                "review_gate_preserved":proposals_feature.review_gate_preserved,
                "certifies_truth":proposals_feature.certifies_truth
            }));
        }
        let review = self.proposal_review(
            root,
            &Map::from_iter([("proposal_id".into(), json!(proposal_id))]),
        )?;
        if review["status"] != "policy_valid" {
            return Err(self.error(
                "proposal_not_admissible",
                "compound contribution stopped at the preserved review gate",
                json!({"submission":submission,"review":review}),
            ));
        }
        let admission_idempotency = self.derived_idempotency_key(
            &self.domain.id_derivation.derived_idempotency_keys.admission,
            &json!({"proposal_id":proposal_id,"proposal_digest":submission["proposal_digest"]}),
        );
        let admission = self.proposal_admit(
            root,
            &Map::from_iter([
                ("proposal_id".into(), json!(proposal_id)),
                ("actor".into(), json!(self.required(args, "actor")?)),
                (
                    "authority_basis".into(),
                    args.get("authority_basis").cloned().unwrap_or(Value::Null),
                ),
                (
                    "expected_ledger_head".into(),
                    submission["expected_ledger_head"].clone(),
                ),
                ("idempotency_key".into(), json!(admission_idempotency)),
            ]),
        )?;
        Ok(json!({
            "schema":proposals_feature.compound_schema_id,
            "status":"admitted",
            "submission":submission,
            "review":review,
            "admission":admission,
            "review_gate_preserved":proposals_feature.review_gate_preserved,
            "certifies_truth":proposals_feature.certifies_truth
        }))
    }

    fn normalize_operations(&self, operations: &[Value]) -> Result<Vec<Value>, Value> {
        let entity_op = self.domain.id_derivation.entity.applies_to.clone();
        let relation_op = self.domain.id_derivation.relation.applies_to.clone();
        let wiring = &self.domain.id_derivation.local_ref_wiring;
        let entity_key_field = self
            .domain
            .projection
            .fold
            .iter()
            .find(|entry| entry.operation == entity_op)
            .map(|entry| entry.key_field.clone())
            .expect("entity fold entry validated at load");
        let mut local_ids = std::collections::HashMap::new();
        let mut first_pass = Vec::with_capacity(operations.len());
        for operation in operations {
            let mut normalized = operation.clone();
            if operation.get("op").and_then(Value::as_str) == Some(entity_op.as_str()) {
                let object = normalized.as_object_mut().unwrap();
                if object
                    .get(&entity_key_field)
                    .and_then(Value::as_str)
                    .is_none()
                {
                    let kind = object.get("kind").and_then(Value::as_str).unwrap_or("");
                    let title = object.get("title").and_then(Value::as_str).unwrap_or("");
                    if !kind.is_empty() && !title.is_empty() {
                        let recipe = &self.domain.id_derivation.entity;
                        let mut digest_input = Map::new();
                        for field in &recipe.digest_input_fields {
                            digest_input.insert(
                                field.clone(),
                                object.get(field).cloned().unwrap_or(Value::Null),
                            );
                        }
                        let digest = self.digest_value(&Value::Object(digest_input))?;
                        object.insert(
                            entity_key_field.clone(),
                            json!(format!(
                                "{}:{}",
                                safe_name(kind),
                                &digest[..template_truncation(&recipe.template, 20)]
                            )),
                        );
                    }
                }
                if let (Some(local_ref), Some(entity_id)) = (
                    object.get(&wiring.declare_field).and_then(Value::as_str),
                    object.get(&entity_key_field).and_then(Value::as_str),
                ) {
                    if local_ids
                        .insert(local_ref.to_string(), entity_id.to_string())
                        .is_some()
                    {
                        return Err(self.error(
                            &wiring.duplicate_refusal_code,
                            "entity local_ref must be unique within a proposal",
                            json!({"local_ref":local_ref}),
                        ));
                    }
                }
            }
            first_pass.push(normalized);
        }
        first_pass
            .iter()
            .map(|operation| {
                let mut normalized = operation.clone();
                if operation.get("op").and_then(Value::as_str) == Some(relation_op.as_str()) {
                    let object = normalized.as_object_mut().unwrap();
                    for (ref_field, id_field) in &wiring.reference_fields {
                        if object.get(id_field).and_then(Value::as_str).is_none() {
                            if let Some(reference) = object.get(ref_field).and_then(Value::as_str) {
                                let resolved = local_ids.get(reference).ok_or_else(|| self.error(&wiring.unresolved_refusal_code, "relation reference does not identify an entity in this proposal", json!({"field":ref_field,"local_ref":reference})))?;
                                object.insert(id_field.clone(), json!(resolved));
                            }
                        }
                    }
                }
                let relation_key_field = self
                    .domain
                    .projection
                    .fold
                    .iter()
                    .find(|entry| entry.operation == relation_op)
                    .map(|entry| entry.key_field.clone())
                    .expect("relation fold entry validated at load");
                if normalized.get("op").and_then(Value::as_str) == Some(relation_op.as_str())
                    && normalized
                        .get(&relation_key_field)
                        .and_then(Value::as_str)
                        .is_none()
                {
                    let relation_type = normalized
                        .get("relation_type")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string();
                    let source_id = normalized
                        .get("source_id")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string();
                    let target_id = normalized
                        .get("target_id")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string();
                    if relation_type.is_empty() || source_id.is_empty() || target_id.is_empty() {
                        return Ok(normalized);
                    }
                    let recipe = &self.domain.id_derivation.relation;
                    let mut hash_input = Vec::new();
                    for (index, segment) in recipe.hash_input.split("\\0").enumerate() {
                        if index > 0 {
                            hash_input.push(0_u8);
                        }
                        let field = segment
                            .trim_start_matches('{')
                            .trim_end_matches('}')
                            .to_string();
                        let value = match field.as_str() {
                            "relation_type" => relation_type.clone(),
                            "source_id" => source_id.clone(),
                            "target_id" => target_id.clone(),
                            _ => normalized
                                .get(&field)
                                .and_then(Value::as_str)
                                .unwrap_or("")
                                .to_string(),
                        };
                        hash_input.extend_from_slice(value.as_bytes());
                    }
                    let digest = sha256(&hash_input);
                    normalized.as_object_mut().unwrap().insert(
                        relation_key_field,
                        json!(format!(
                            "{}{}-{}",
                            template_prefix(&recipe.template),
                            safe_name(&relation_type),
                            &digest[..template_truncation(&recipe.template, 16)]
                        )),
                    );
                }
                Ok(normalized)
            })
            .collect()
    }

    fn resolve_expected_ledger_head(
        &self,
        root: &Path,
        supplied: Option<&Value>,
    ) -> Result<Value, Value> {
        if supplied.is_none() || supplied.and_then(Value::as_str) == Some("latest") {
            return Ok(self
                .ledger_head(root)?
                .map(Value::String)
                .unwrap_or(Value::Null));
        }
        Ok(supplied.cloned().unwrap_or(Value::Null))
    }

    fn derived_idempotency_key(&self, recipe: &DerivedKeyRecipe, source: &Value) -> String {
        let mut object = Map::new();
        for field in &recipe.input_fields {
            object.insert(
                field.clone(),
                source.get(field).cloned().unwrap_or(Value::Null),
            );
        }
        let canonical = serde_json::to_vec(&Value::Object(object)).unwrap_or_default();
        format!(
            "{}{}",
            template_prefix(&recipe.template),
            &sha256(&canonical)[..template_truncation(&recipe.template, 24)]
        )
    }

    fn proposal_receipt(&self, proposal: &Value) -> Value {
        json!({
            "schema":self.domain.features.proposals.submission_receipt_schema_id,
            "status":proposal["status"],
            "proposal_id":proposal["proposal_id"],
            "proposal_digest":proposal["digest"],
            "content_fingerprint":proposal["content_fingerprint"],
            "operation_count":proposal["operations"].as_array().map_or(0, Vec::len),
            "expected_ledger_head":proposal["expected_ledger_head"],
            "created_at":proposal["created_at"]
        })
    }

    fn capture_sources(&self, root: &Path, args: &Map<String, Value>) -> Result<Value, Value> {
        self.prepare(root)?;
        let caps = &self.domain.caps.capture_sources;
        let sources = args
            .get("sources")
            .and_then(Value::as_array)
            .ok_or_else(|| {
                self.error("invalid_capture", "sources must be an array", Value::Null)
            })?;
        let supplied = args
            .get("operations")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        if (sources.len() as u64) < caps.sources_min {
            return Err(self.error(
                "invalid_capture",
                "at least one source is required",
                Value::Null,
            ));
        }
        let mut operations = Vec::with_capacity(sources.len() + supplied.len());
        for source in sources {
            let source = source.as_object().ok_or_else(|| {
                self.error(
                    "invalid_capture",
                    "each source must be an object",
                    Value::Null,
                )
            })?;
            operations.push(json!({
                "op":self.entity_op(),
                "entity_id":self.required(source, "source_id")?,
                "kind":"source",
                "title":self.required(source, "title")?,
                "version":self.required(source, "version")?,
                "locator":self.required(source, "locator")?
            }));
        }
        for operation in &supplied {
            if operation.get("kind").and_then(Value::as_str) == Some("source") {
                return Err(self.error(
                    "invalid_capture",
                    "declare sources through the sources field, not operations",
                    Value::Null,
                ));
            }
            operations.push(operation.clone());
        }
        if operations.len() as u64 > caps.combined_max {
            return Err(self.error(
                "invalid_capture",
                &format!("combined source and operation count exceeds {}", caps.combined_max),
                json!({"source_count":sources.len(),"operation_count":supplied.len(),"combined_count":operations.len()}),
            ));
        }
        let existing_identities = self.with_stable_projection(root, || {
            self.existing_operation_identities(root, &operations)
        })?;
        let mut proposal_args = args.clone();
        proposal_args.remove("sources");
        proposal_args.insert("operations".into(), json!(operations));
        let receipt = self.proposal_submit(root, &proposal_args)?;
        Ok(json!({
            "schema":self.domain.features.proposals.source_capture_schema_id,
            "status":"draft_submitted",
            "proposal_id":receipt["proposal_id"],
            "proposal_digest":receipt["proposal_digest"],
            "expected_ledger_head":receipt["expected_ledger_head"],
            "source_count":sources.len(),
            "operation_count":receipt["operation_count"],
            "existing_identity_count":existing_identities.len(),
            "existing_identities":existing_identities,
            "next":{"review":{"tool":self.tool_name("proposal_review"),"proposal_id":receipt["proposal_id"]}},
            "admission_requires_explicit_call":self.domain.features.proposals.capture_sources.admission_requires_explicit_call,
            "certifies_truth":self.domain.features.proposals.certifies_truth,
            "bounded":true
        }))
    }

    fn existing_operation_identities(
        &self,
        root: &Path,
        operations: &[Value],
    ) -> Result<Vec<Value>, Value> {
        let db = Connection::open(self.projection_path(root))
            .map_err(self.db_error("projection_open_failed"))?;
        let mut existing = Vec::new();
        for operation in operations {
            let Some(op_kind) = operation.get("op").and_then(Value::as_str) else {
                continue;
            };
            let Some(fold) = self
                .domain
                .projection
                .fold
                .iter()
                .find(|entry| entry.operation == op_kind)
            else {
                continue;
            };
            let Some(identity) = operation.get(&fold.key_field).and_then(Value::as_str) else {
                continue;
            };
            let table = self.table(&fold.table);
            let sql = format!(
                "select 1 from {} where {}=?1 limit 1",
                table.name, table.primary_key
            );
            let found = db
                .query_row(&sql, params![identity], |_| Ok(()))
                .optional()
                .map_err(self.db_error("projection_duplicate_check_failed"))?
                .is_some();
            if found {
                existing.push(json!({"op":operation["op"],"identity":identity}));
            }
        }
        Ok(existing)
    }

    fn proposal_read(&self, root: &Path, args: &Map<String, Value>) -> Result<Value, Value> {
        let id = self.required(args, "proposal_id")?;
        let proposal = self.load_proposal(root, &id)?;
        let operations = proposal["operations"].as_array().ok_or_else(|| {
            self.error(
                "proposal_corrupt",
                "proposal operations missing",
                json!({"proposal_id":id}),
            )
        })?;
        let offset = args.get("offset").and_then(Value::as_u64).unwrap_or(0) as usize;
        let caps = &self.domain.caps.proposal_read_limit;
        let limit = args
            .get("limit")
            .and_then(Value::as_u64)
            .unwrap_or(caps.default)
            .min(caps.max) as usize;
        let items = operations
            .iter()
            .skip(offset)
            .take(limit)
            .cloned()
            .collect::<Vec<_>>();
        let next_offset = (offset + items.len() < operations.len()).then_some(offset + items.len());
        let lifecycle = self.proposal_lifecycle(root, &id)?;
        Ok(json!({
            "schema":self.domain.features.proposals.read_schema_id,
            "status":lifecycle["status"],
            "lifecycle":lifecycle,
            "proposal_id":proposal["proposal_id"],
            "proposal_digest":proposal["digest"],
            "actor":proposal["actor"],
            "authority_basis":proposal["authority_basis"],
            "idempotency_key":proposal["idempotency_key"],
            "expected_ledger_head":proposal["expected_ledger_head"],
            "created_at":proposal["created_at"],
            "operation_count":operations.len(),
            "offset":offset,
            "limit":limit,
            "returned":items.len(),
            "operations":items,
            "has_more":next_offset.is_some(),
            "next_offset":next_offset,
            "bounded":true
        }))
    }

    fn operation_identity(&self, operation: &Value) -> Option<String> {
        let op_kind = operation.get("op").and_then(Value::as_str)?;
        let identity = self
            .domain
            .id_derivation
            .operation_identity_prefixes
            .get(op_kind)?;
        operation
            .get(&identity.id_field)
            .and_then(Value::as_str)
            .map(|value| format!("{}:{value}", identity.prefix))
    }

    fn proposal_resubmit(&self, root: &Path, args: &Map<String, Value>) -> Result<Value, Value> {
        let source_id = self.required(args, "source_proposal_id")?;
        let source = self.load_proposal(root, &source_id)?;
        let original = source["operations"].as_array().ok_or_else(|| {
            self.error(
                "proposal_corrupt",
                "proposal operations missing",
                json!({"proposal_id":source_id}),
            )
        })?;
        let requested_drops = args
            .get("drop_operation_ids")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let drop_ids = requested_drops
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_string)
            .collect::<HashSet<_>>();
        if drop_ids.len() != requested_drops.len() {
            return Err(self.error(
                "invalid_proposal_resubmission",
                "drop_operation_ids must contain unique strings",
                Value::Null,
            ));
        }
        let known = original
            .iter()
            .filter_map(|operation| self.operation_identity(operation))
            .collect::<HashSet<_>>();
        let missing = drop_ids.difference(&known).cloned().collect::<Vec<_>>();
        if !missing.is_empty() {
            return Err(self.error(
                &self
                    .domain
                    .features
                    .proposals
                    .resubmit
                    .missing_drop_refusal_code,
                "one or more drop_operation_ids do not identify source proposal operations",
                json!({"missing":missing}),
            ));
        }
        let replacements = args
            .get("replacements")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        self.validate_operations(&replacements, false)?;
        let mut operations = original
            .iter()
            .filter(|operation| {
                self.operation_identity(operation)
                    .map(|identity| !drop_ids.contains(&identity))
                    .unwrap_or(true)
            })
            .cloned()
            .collect::<Vec<_>>();
        operations.extend(replacements);
        let resubmit_caps = &self.domain.caps.resubmit;
        if (operations.len() as u64) < resubmit_caps.resulting_min
            || operations.len() as u64 > resubmit_caps.resulting_max
        {
            return Err(self.error(
                "invalid_proposal_resubmission",
                &format!(
                    "resulting operations count must be between {} and {}",
                    resubmit_caps.resulting_min, resubmit_caps.resulting_max
                ),
                json!({"count":operations.len()}),
            ));
        }
        let mut submit_args = Map::new();
        for key in [
            "actor",
            "authority_basis",
            "idempotency_key",
            "expected_ledger_head",
        ] {
            if let Some(value) = args.get(key) {
                submit_args.insert(key.to_string(), value.clone());
            }
        }
        submit_args.insert("operations".into(), json!(operations));
        let receipt = self.proposal_submit(root, &submit_args)?;
        Ok(json!({
            "schema":self.domain.features.proposals.resubmission_schema_id,
            "status":"draft_submitted",
            "source_proposal_id":source_id,
            "proposal_id":receipt["proposal_id"],
            "proposal_digest":receipt["proposal_digest"],
            "operation_count":receipt["operation_count"],
            "dropped_operation_ids":drop_ids,
            "replacement_count":args.get("replacements").and_then(Value::as_array).map_or(0, Vec::len),
            "expected_ledger_head":receipt["expected_ledger_head"],
            "next":{"review":{"tool":self.tool_name("proposal_review"),"proposal_id":receipt["proposal_id"]}},
            "admission_requires_explicit_call":true,
            "certifies_truth":self.domain.features.proposals.certifies_truth,
            "bounded":true
        }))
    }

    fn proposal_lifecycle(&self, root: &Path, proposal_id: &str) -> Result<Value, Value> {
        for path in self.ledger_files(root)? {
            let event = self.read_json(&path)?;
            if event.get("proposal_id").and_then(Value::as_str) == Some(proposal_id) {
                return Ok(json!({
                    "status":"admitted",
                    "event_id":event["event_id"],
                    "sequence":event["sequence"],
                    "ledger_head":event[self.domain.storage.event_hash_field.clone()],
                    "admitted_at":event["occurred_at"]
                }));
            }
        }
        let rejection_path = self
            .proposals(root)
            .join(format!("{}.rejection.json", safe_name(proposal_id)));
        if rejection_path.exists() {
            let rejection = self.read_json(&rejection_path)?;
            return Ok(json!({
                "status":"rejected",
                "rejected_at":rejection["occurred_at"],
                "reason":rejection["reason"]
            }));
        }
        let review_path = self
            .proposals(root)
            .join(format!("{}.review.json", safe_name(proposal_id)));
        if review_path.exists() {
            let review = self.read_json(&review_path)?;
            return Ok(json!({"status":"reviewed","review_status":review["status"]}));
        }
        Ok(json!({"status":"submitted"}))
    }

    fn proposal_review(&self, root: &Path, args: &Map<String, Value>) -> Result<Value, Value> {
        let id = self.required(args, "proposal_id")?;
        let proposal = self.load_proposal(root, &id)?;
        let operations = proposal["operations"].as_array().ok_or_else(|| {
            self.error(
                "proposal_corrupt",
                "proposal operations missing",
                json!({"proposal_id":id}),
            )
        })?;
        self.validate_operations(operations, true)?;
        self.validate_references(root, operations)?;
        let expected = proposal.get("expected_ledger_head").and_then(Value::as_str);
        let current = self.ledger_head(root)?;
        let head_matches = expected == current.as_deref();
        let review = json!({"schema":self.domain.features.proposals.review_schema_id,"proposal_id":id,"status":if head_matches{"policy_valid"}else{"stale"},"certifies_truth":self.domain.features.proposals.certifies_truth,"checks":{"schema":true,"references":true,"evidence_locations":true,"graph_invariants":true,"ledger_head":head_matches},"expected_ledger_head":expected,"actual_ledger_head":current});
        self.write_replace_json(
            &self.proposals(root).join(format!("{id}.review.json")),
            &review,
        )?;
        Ok(review)
    }

    fn proposal_admit(&self, root: &Path, args: &Map<String, Value>) -> Result<Value, Value> {
        self.prepare(root)?;
        self.with_authority_lock(root, "ledger", || self.proposal_admit_locked(root, args))
    }

    fn proposal_admit_locked(
        &self,
        root: &Path,
        args: &Map<String, Value>,
    ) -> Result<Value, Value> {
        self.prepare(root)?;
        let id = self.required(args, "proposal_id")?;
        let actor = self.required(args, "actor")?;
        let proposal = self.load_proposal(root, &id)?;
        let idem = args
            .get("idempotency_key")
            .and_then(Value::as_str)
            .filter(|value| !value.trim().is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| {
                self.derived_idempotency_key(
                    &self.domain.id_derivation.derived_idempotency_keys.admission,
                    &json!({"proposal_id":id,"proposal_digest":proposal["digest"]}),
                )
            });
        let idem_path = self
            .ledger(root)
            .join(format!("idem-{}.txt", safe_name(&idem)));
        if idem_path.exists() {
            let event_id = fs::read_to_string(&idem_path)
                .map_err(self.io_error("ledger_idempotency_read_failed"))?;
            let event =
                self.read_json(&self.ledger(root).join(format!("{}.json", event_id.trim())))?;
            if event.get("proposal_id") != Some(&json!(id))
                || event.get("proposal_digest") != proposal.get("digest")
            {
                return Err(self.error(
                    "admission_idempotency_conflict",
                    "idempotency key already names a different proposal admission",
                    json!({"idempotency_key":idem,"existing_event_id":event_id.trim()}),
                ));
            }
            return Ok(self.admission_receipt(&event));
        }
        if let Some(event) = self.find_ledger_event_by_idempotency(root, &idem)? {
            if event.get("proposal_id") != Some(&json!(id))
                || event.get("proposal_digest") != proposal.get("digest")
            {
                return Err(self.error(
                    "admission_idempotency_conflict",
                    "idempotency key already names a different proposal admission",
                    json!({"idempotency_key":idem,"existing_event_id":event["event_id"]}),
                ));
            }
            if !idem_path.exists() {
                self.write_new(&idem_path, event["event_id"].as_str().unwrap().as_bytes())?;
            }
            return Ok(self.admission_receipt(&event));
        }
        let review =
            self.proposal_review(root, &Map::from_iter([("proposal_id".into(), json!(id))]))?;
        if review["status"] != "policy_valid" {
            return Err(self.error(
                "proposal_not_admissible",
                "proposal review is not policy_valid",
                review,
            ));
        }
        let expected_value =
            self.resolve_expected_ledger_head(root, args.get("expected_ledger_head"))?;
        let expected = expected_value.as_str();
        let current = self.ledger_head(root)?;
        if expected != current.as_deref()
            || proposal.get("expected_ledger_head").and_then(Value::as_str) != current.as_deref()
        {
            return Err(self.error(
                "ledger_head_conflict",
                "expected ledger head does not match",
                json!({"expected":expected,"proposal_expected":proposal.get("expected_ledger_head"),"actual":current}),
            ));
        }
        let event_hash_field = self.domain.storage.event_hash_field.clone();
        let outcome = event_ledger::append_event(
            self.error,
            &self.ledger_layout(root),
            &event_hash_field,
            None,
            Some(&idem),
            |ctx| json!({"schema":self.domain.storage.event_schema_id,"sequence":ctx.sequence,"event_id":ctx.event_id,"event_kind":self.domain.features.proposals.event_kind,"previous_hash":ctx.previous_hash,"proposal_id":id,"proposal_digest":proposal["digest"],"operations":proposal["operations"],"actor":actor,"authority_basis":args.get("authority_basis"),"idempotency_key":idem,"occurred_at":now(),"certifies_truth":self.domain.features.proposals.certifies_truth}),
        )?;
        self.rebuild_projection(root)?;
        Ok(self.admission_receipt(&outcome.event))
    }

    fn admission_receipt(&self, event: &Value) -> Value {
        json!({
            "schema":self.domain.features.proposals.admission_receipt_schema_id,
            "status":"admitted",
            "proposal_id":event["proposal_id"],
            "proposal_digest":event["proposal_digest"],
            "event_id":event["event_id"],
            "sequence":event["sequence"],
            "operation_count":event["operations"].as_array().map_or(0, Vec::len),
            "ledger_head":event[self.domain.storage.event_hash_field.clone()],
            "certifies_truth":self.domain.features.proposals.certifies_truth
        })
    }

    fn proposal_reject(&self, root: &Path, args: &Map<String, Value>) -> Result<Value, Value> {
        let id = self.required(args, "proposal_id")?;
        let _ = self.load_proposal(root, &id)?;
        let rejection = json!({"schema":self.domain.features.proposals.rejection_schema_id,"proposal_id":id,"status":"rejected","actor":self.required(args,"actor")?,"reason":self.required(args,"reason")?,"occurred_at":now()});
        self.write_new_json(
            &self.proposals(root).join(format!("{id}.rejection.json")),
            &rejection,
        )?;
        Ok(rejection)
    }

    fn status(&self, root: &Path) -> Result<Value, Value> {
        self.prepare(root)?;
        event_ledger::verify(self.error, &self.ledger_layout(root), self.event_hash_field)?;
        let ledger_head = self.ledger_head(root)?;
        let event_count = self.ledger_files(root)?.len();
        let projection_path = self.projection_path(root);
        let projection_exists = projection_path.exists();
        let projection_current = projection_exists
            && self.projection_is_current(root, &ledger_head, event_count as u64)?;
        let projection_status = if projection_current {
            "current"
        } else if projection_exists {
            "stale"
        } else {
            "missing"
        };
        let (stored_entities, visible_entities, relations, records) = if projection_exists {
            let db = Connection::open_with_flags(
                &projection_path,
                rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
            )
            .map_err(self.db_error("projection_open_failed"))?;
            let stored_entities: i64 = db
                .query_row(
                    &format!("select count(*) from {}", self.entity_table),
                    [],
                    |r| r.get(0),
                )
                .map_err(self.db_error("projection_count_failed"))?;
            let visible_entities: i64 = db
                .query_row(
                    &format!(
                        "select count(*) from {} where {}",
                        self.entity_table,
                        self.visible_entity_predicate()
                    ),
                    [],
                    |r| r.get(0),
                )
                .map_err(self.db_error("projection_count_failed"))?;
            let relations: i64 = db
                .query_row(
                    &format!("select count(*) from {}", self.relation_table),
                    [],
                    |r| r.get(0),
                )
                .map_err(self.db_error("projection_count_failed"))?;
            let records: i64 = db
                .query_row(
                    &format!("select count(*) from {}", self.records_table),
                    [],
                    |r| r.get(0),
                )
                .map_err(self.db_error("projection_count_failed"))?;
            (stored_entities, visible_entities, relations, records)
        } else {
            (0, 0, 0, 0)
        };
        Ok(json!({
            "schema":self.schema_id("status.v1"),"status":"ok",
            "implementation":self.domain.identity.implementation,
            "canonical_store":self.ledger(root).to_string_lossy(),
            "projection":projection_path.to_string_lossy(),
            "ledger_head":ledger_head,"event_count":event_count,
            "entity_count":visible_entities,"entity_count_semantics":"graph_visible",
            "stored_entity_count":stored_entities,
            "internal_entity_count":stored_entities - visible_entities,
            "relation_count":relations,"record_count":records,
            "projection_status":projection_status,
            "projection_current":projection_current,
            "projection_rebuildable":true,
            "status_rebuilds_projection":false,
            "truth_certification":false
        }))
    }
    /// Project one query row into the descriptor-listed field order. Row
    /// columns win, `"payload"` selects the full payload, anything else is
    /// looked up inside the payload (missing yields null, as before).
    fn project_row(
        row_values: &Map<String, Value>,
        payload: &Value,
        projection: &[String],
    ) -> Value {
        let mut out = Map::new();
        for field in projection {
            let value = if field == "payload" {
                payload.clone()
            } else if let Some(value) = row_values.get(field) {
                value.clone()
            } else {
                payload.get(field).cloned().unwrap_or(Value::Null)
            };
            out.insert(field.clone(), value);
        }
        Value::Object(out)
    }

    fn message_mark_read(&self, root: &Path, args: &Map<String, Value>) -> Result<Value, Value> {
        let read_receipt_kind = self.domain.query.read_receipt_kind.clone().ok_or_else(|| {
            self.error(
                "message_state_unavailable",
                "this domain does not configure durable message read receipts",
                Value::Null,
            )
        })?;
        let message_id = self.required(args, "message_id")?;
        let reader = self.required(args, "reader")?;
        let actor = self.required(args, "actor")?;
        let authority_basis = self.required_object(args, "authority_basis")?;
        let receipt_id = format!(
            "{}:{}",
            safe_name(&read_receipt_kind),
            &sha256(format!("{message_id}\0{reader}").as_bytes())[..24]
        );
        let (message_target, existing_receipt) = self.with_stable_projection(root, || {
            let db = Connection::open(self.projection_path(root))
                .map_err(self.db_error("projection_open_failed"))?;
            let entity_pk = self.table(&self.entity_table).primary_key.clone();
            let message_target = db
                .query_row(
                    &format!(
                        "select kind,payload_json from {} where {}=?1",
                        self.entity_table, entity_pk
                    ),
                    params![message_id],
                    |row| {
                        let kind = row.get::<_, String>(0)?;
                        let payload = row.get::<_, String>(1)?;
                        Ok((
                            kind,
                            serde_json::from_str::<Value>(&payload).unwrap_or(Value::Null),
                        ))
                    },
                )
                .optional()
                .map_err(self.db_error("message_read_target_lookup_failed"))?;
            let existing_receipt = db
                .query_row(
                    &format!(
                        "select kind,payload_json,event_id from {} where {}=?1",
                        self.entity_table, entity_pk
                    ),
                    params![receipt_id],
                    |row| {
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, String>(1)?,
                            row.get::<_, String>(2)?,
                        ))
                    },
                )
                .optional()
                .map_err(self.db_error("message_read_receipt_lookup_failed"))?;
            Ok((message_target, existing_receipt))
        })?;
        let Some((message_kind, message_payload)) = message_target else {
            return Err(self.error(
                "message_not_found",
                "message_id must identify an existing entity",
                json!({"message_id":message_id}),
            ));
        };
        if !self.is_configured_message_kind(&message_kind) {
            return Err(self.error(
                "message_kind_invalid",
                "message_id must identify a configured communication kind",
                json!({"message_id":message_id,"kind":message_kind}),
            ));
        }
        let sender = message_payload.get("sender").and_then(Value::as_str);
        let recipient = message_payload.get("recipient").and_then(Value::as_str);
        if ![sender, recipient]
            .into_iter()
            .flatten()
            .any(|participant| participant == reader)
        {
            return Err(self.error(
                "message_reader_not_participant",
                "reader must be the sender or recipient of the message",
                json!({"message_id":message_id,"reader":reader}),
            ));
        }
        if let Some((existing_kind, receipt_payload_json, event_id)) = existing_receipt {
            if existing_kind != read_receipt_kind {
                return Err(self.error(
                    "message_read_receipt_conflict",
                    "the deterministic read-receipt identity is already occupied by another entity kind",
                    json!({"receipt_id":receipt_id,"existing_kind":existing_kind,"expected_kind":read_receipt_kind}),
                ));
            }
            let receipt_payload =
                serde_json::from_str::<Value>(&receipt_payload_json).map_err(|_| {
                    self.error(
                        "message_read_receipt_corrupt",
                        "existing message read receipt payload is invalid",
                        json!({"receipt_id":receipt_id}),
                    )
                })?;
            if receipt_payload.get("message_id").and_then(Value::as_str)
                != Some(message_id.as_str())
                || receipt_payload.get("reader").and_then(Value::as_str) != Some(reader.as_str())
            {
                return Err(self.error(
                    "message_read_receipt_conflict",
                    "the existing read receipt does not match the requested message and reader",
                    json!({"receipt_id":receipt_id,"message_id":message_id,"reader":reader}),
                ));
            }
            let event = self.read_json(&self.ledger(root).join(format!("{event_id}.json")))?;
            let mut admission = self.admission_receipt(&event);
            if let Some(status) = admission.as_object_mut() {
                status.insert(
                    "status".into(),
                    Value::String("already_admitted".to_string()),
                );
            }
            return Ok(json!({
                "schema":self.schema_id("message_read.v1"),
                "status":"read",
                "replayed":true,
                "message_id":message_id,
                "reader":reader,
                "receipt_id":receipt_id,
                "read_at":receipt_payload.get("read_at"),
                "admission":admission
            }));
        }
        let read_at = args
            .get("read_at")
            .and_then(Value::as_str)
            .filter(|value| !value.trim().is_empty())
            .map(str::to_string)
            .unwrap_or_else(now);
        let idempotency_key = args
            .get("idempotency_key")
            .and_then(Value::as_str)
            .filter(|value| !value.trim().is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| {
                format!(
                    "message-read-{}",
                    &sha256(format!("{message_id}\0{reader}").as_bytes())[..48]
                )
            });
        let operation = json!({
            "op":self.entity_op(),
            "entity_id":receipt_id,
            "kind":read_receipt_kind,
            "title":format!("Read receipt for {message_id}"),
            "message_id":message_id,
            "reader":reader,
            "read_at":read_at
        });
        let admission = self.submit_review_admit(
            root,
            &Map::from_iter([
                ("actor".into(), json!(actor)),
                ("authority_basis".into(), authority_basis),
                ("idempotency_key".into(), json!(idempotency_key)),
                ("operations".into(), json!([operation])),
            ]),
        )?;
        Ok(json!({
            "schema":self.schema_id("message_read.v1"),
            "status":"read",
            "message_id":message_id,
            "reader":reader,
            "receipt_id":receipt_id,
            "read_at":read_at,
            "admission":admission
        }))
    }

    fn generic_query(&self, root: &Path, args: &Map<String, Value>) -> Result<Value, Value> {
        self.with_stable_projection(root, || self.generic_query_locked(root, args))
    }

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
        let sequence_lower_bound = planner_value("after_sequence")
            .or_else(|| planner_value("since_event"))
            .and_then(Value::as_u64)
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
        let datoms = self.load_datoms_for_query(
            &db,
            datoms_table,
            &spec,
            subject_local_sequence,
            subject_seed_attribute,
        )?;
        if started.elapsed() > Duration::from_millis(effective_timeout_ms) {
            return Err(self.error("query_timeout", "query exceeded its capped time budget while loading indexed datoms", json!({"timeout_ms":effective_timeout_ms,"phase":"datom_load","datoms_loaded":datoms.len()})));
        }
        let execution = ledger_query::execute(&spec, &datoms)
            .map_err(|failure| self.error(failure.code, &failure.message, failure.details))?;
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

    fn normalize_communication_result(&self, value: &mut Value) -> usize {
        let communication = &self.domain.query.communication;
        match value {
            Value::Object(object) => {
                let mut count = 0;
                if let Some(kind) = object.get("kind").and_then(Value::as_str) {
                    if communication
                        .legacy_read_aliases
                        .iter()
                        .any(|legacy| legacy == kind)
                    {
                        object.insert(
                            "kind".into(),
                            Value::String(communication.canonical_kind.clone()),
                        );
                        count += 1;
                    }
                }
                for child in object.values_mut() {
                    count += self.normalize_communication_result(child);
                }
                count
            }
            Value::Array(values) => values
                .iter_mut()
                .map(|child| self.normalize_communication_result(child))
                .sum(),
            _ => 0,
        }
    }

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
                    "order_by":[{"term":"?sequence","direction":"asc"},{"term":"?event_id","direction":"asc"}],
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
                    "order_by":[{"term":"?sequence","direction":"asc"},{"term":"?event_id","direction":"asc"}],
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

    fn datom_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<ledger_query::Datom> {
        let value_json: String = row.get(3)?;
        Ok(ledger_query::Datom {
            origin_id: row.get(0)?,
            subject: row.get(1)?,
            attribute: row.get(2)?,
            value: serde_json::from_str(&value_json).unwrap_or(Value::String(value_json)),
            event_sequence: row.get::<_, i64>(4)? as u64,
            event_id: row.get(5)?,
        })
    }

    fn load_datoms_sql(
        &self,
        db: &Connection,
        sql: &str,
        parameters: &[&dyn ToSql],
    ) -> Result<Vec<ledger_query::Datom>, Value> {
        let mut statement = db
            .prepare(sql)
            .map_err(self.db_error("projection_datom_prepare_failed"))?;
        let rows = statement
            .query_map(
                rusqlite::params_from_iter(parameters.iter().copied()),
                |row| Self::datom_from_row(row),
            )
            .map_err(self.db_error("projection_datom_query_failed"))?;
        rows.map(|row| row.map_err(self.db_error("projection_datom_row_failed")))
            .collect::<Result<Vec<_>, _>>()
    }

    fn append_datoms(
        &self,
        db: &Connection,
        sql: &str,
        parameters: &[&dyn ToSql],
        datoms: &mut Vec<ledger_query::Datom>,
        seen: &mut HashSet<String>,
        max_datoms: u64,
    ) -> Result<(), Value> {
        for datom in self.load_datoms_sql(db, sql, parameters)? {
            let key = format!(
                "{}\u{1}{}\u{1}{}\u{1}{}\u{1}{}\u{1}{}",
                datom.origin_id,
                datom.subject,
                datom.attribute,
                serde_json::to_string(&datom.value).unwrap_or_default(),
                datom.event_sequence,
                datom.event_id,
            );
            if seen.insert(key) {
                datoms.push(datom);
                if datoms.len() as u64 > max_datoms {
                    return Err(self.error(
                        "query_datom_scan_limit",
                        "normalized datom projection exceeds the descriptor scan budget",
                        json!({"max_datoms_scanned":max_datoms,"planner_mode":"indexed_seed_with_broad_join","cause":"an indexed clause or broad join exceeded the work budget","suggestion":"make the most selective indexed equality clause explicit and inspect broad join attributes"}),
                    ));
                }
            }
        }
        Ok(())
    }

    fn load_datoms(
        &self,
        db: &Connection,
        table: &str,
        max_datoms: u64,
    ) -> Result<Vec<ledger_query::Datom>, Value> {
        let row_limit = max_datoms.saturating_add(1) as i64;
        let sql = format!(
            "select origin_id,subject,attribute,value_json,event_sequence,event_id from {table} limit ?1"
        );
        let parameters: [&dyn ToSql; 1] = [&row_limit];
        let datoms = self.load_datoms_sql(db, &sql, &parameters)?;
        if datoms.len() as u64 > max_datoms {
            return Err(self.error(
                "query_datom_scan_limit",
                "normalized datom projection exceeds the descriptor scan budget",
                json!({"max_datoms_scanned":max_datoms,"planner_mode":"bounded_full_scan","cause":"query has no usable indexed seed","suggestion":"add an equality predicate on an indexed attribute before increasing the caller budget"}),
            ));
        }
        Ok(datoms)
    }

    fn load_datoms_for_query(
        &self,
        db: &Connection,
        table: &str,
        spec: &ledger_query::QuerySpec,
        subject_local_sequence: Option<(&str, u64)>,
        subject_seed_attribute: Option<&str>,
    ) -> Result<Vec<ledger_query::Datom>, Value> {
        let max_datoms = spec.limits.max_datoms_scanned as u64;
        let mut plan = QuerySeedPlan::default();
        // A subject-local sequence suffix already provides the authoritative
        // positive seed for inbox queries. Exists/not-exists clauses encode
        // read/reply decoration and must be evaluated only after the selected
        // message subjects are hydrated; treating them as global seeds
        // defeats the suffix budget.
        collect_query_seed_clauses(
            &spec.clauses,
            &spec.inputs,
            &mut plan,
            subject_local_sequence.is_none(),
        );

        // Pull decoration derives durable read/reply state from normalized
        // datoms too. Load those narrow attributes alongside the selected
        // subjects without making their subjects seed a second expansion.
        if !spec.pulls.is_empty() && subject_local_sequence.is_none() {
            for attribute in [
                self.domain.query.reply_state_attribute.as_deref(),
                self.domain.query.read_receipt_kind_attribute.as_deref(),
                self.domain.query.read_receipt_message_attribute.as_deref(),
                self.domain.query.read_receipt_reader_attribute.as_deref(),
            ]
            .into_iter()
            .flatten()
            {
                plan.push_attribute(attribute);
            }
        }

        // A variable attribute cannot be safely narrowed by the available
        // indexes. Preserve complete query semantics for that shape by using
        // the bounded full-scan path.
        if plan.unindexable
            || (plan.attribute_values.is_empty()
                && plan.subject_attributes.is_empty()
                && plan.attributes.is_empty())
        {
            return self.load_datoms(db, table, max_datoms);
        }

        let mut datoms = Vec::new();
        let mut seen = HashSet::new();
        let mut candidate_subjects = HashSet::new();
        let row_limit = max_datoms.saturating_add(1) as i64;

        for (attribute, value) in &plan.attribute_values {
            // Named inbox queries have a descriptor-declared participant
            // equality.  It is the authoritative subject seed; the complete
            // subject hydration below already supplies every other equality
            // fact, so scanning broad kind/intent predicates would only add
            // unrelated subjects and exhaust the work budget.
            if subject_local_sequence.is_some()
                && subject_seed_attribute.is_some_and(|seed| attribute != seed)
            {
                continue;
            }
            let value_json = serde_json::to_string(value).map_err(|error| {
                self.error(
                    "projection_datom_value_encode_failed",
                    &error.to_string(),
                    Value::Null,
                )
            })?;
            let before = datoms.len();
            if let Some((sequence_attribute, since_event)) = subject_local_sequence {
                let parameters: [&dyn ToSql; 5] = [
                    attribute,
                    &value_json,
                    &sequence_attribute,
                    &since_event,
                    &row_limit,
                ];
                self.append_datoms(
                    db,
                    &format!(
                        "select d.origin_id,d.subject,d.attribute,d.value_json,d.event_sequence,d.event_id from {table} d where d.attribute=?1 and d.value_json=?2 and exists (select 1 from {table} suffix where suffix.subject=d.subject and suffix.attribute=?3 and cast(suffix.value_json as integer)>?4) limit ?5"
                    ),
                    &parameters,
                    &mut datoms,
                    &mut seen,
                    max_datoms,
                )?;
            } else {
                let parameters: [&dyn ToSql; 3] = [attribute, &value_json, &row_limit];
                self.append_datoms(
                    db,
                    &format!(
                        "select origin_id,subject,attribute,value_json,event_sequence,event_id from {table} where attribute=?1 and value_json=?2 limit ?3"
                    ),
                    &parameters,
                    &mut datoms,
                    &mut seen,
                    max_datoms,
                )?;
            }
            for datom in datoms[before..].iter() {
                candidate_subjects.insert(datom.subject.clone());
            }
        }

        for (subject, attribute) in &plan.subject_attributes {
            let parameters: [&dyn ToSql; 3] = [subject, attribute, &row_limit];
            let before = datoms.len();
            self.append_datoms(
                db,
                &format!(
                    "select origin_id,subject,attribute,value_json,event_sequence,event_id from {table} where subject=?1 and attribute=?2 limit ?3"
                ),
                &parameters,
                &mut datoms,
                &mut seen,
                max_datoms,
            )?;
            for datom in datoms[before..].iter() {
                candidate_subjects.insert(datom.subject.clone());
            }
        }

        // Once a selective indexed predicate has identified subjects, load
        // their complete normalized records so joins, comparisons, and pull
        // decoration see the same subject-local facts as a full scan.
        for subject in &candidate_subjects {
            let parameters: [&dyn ToSql; 2] = [subject, &row_limit];
            self.append_datoms(
                db,
                &format!(
                    "select origin_id,subject,attribute,value_json,event_sequence,event_id from {table} where subject=?1 limit ?2"
                ),
                &parameters,
                &mut datoms,
                &mut seen,
                max_datoms,
            )?;
        }

        // Suffix-planned inbox queries must not turn pull decoration back
        // into a global attribute scan. Resolve reply targets and read
        // receipts by the already-selected message ids, then hydrate only
        // those decoration subjects.
        if !spec.pulls.is_empty() && subject_local_sequence.is_some() {
            let mut decoration_subjects = HashSet::new();
            for subject in &candidate_subjects {
                let value_json = serde_json::to_string(subject).map_err(|error| {
                    self.error(
                        "projection_datom_value_encode_failed",
                        &error.to_string(),
                        Value::Null,
                    )
                })?;
                for attribute in [
                    self.domain.query.reply_state_attribute.as_deref(),
                    self.domain.query.read_receipt_message_attribute.as_deref(),
                ]
                .into_iter()
                .flatten()
                {
                    let parameters: [&dyn ToSql; 3] = [&attribute, &value_json, &row_limit];
                    let before = datoms.len();
                    self.append_datoms(
                        db,
                        &format!(
                            "select origin_id,subject,attribute,value_json,event_sequence,event_id from {table} where attribute=?1 and value_json=?2 limit ?3"
                        ),
                        &parameters,
                        &mut datoms,
                        &mut seen,
                        max_datoms,
                    )?;
                    for datom in datoms[before..].iter() {
                        decoration_subjects.insert(datom.subject.clone());
                    }
                }
            }
            for subject in decoration_subjects {
                let parameters: [&dyn ToSql; 2] = [&subject, &row_limit];
                self.append_datoms(
                    db,
                    &format!(
                        "select origin_id,subject,attribute,value_json,event_sequence,event_id from {table} where subject=?1 limit ?2"
                    ),
                    &parameters,
                    &mut datoms,
                    &mut seen,
                    max_datoms,
                )?;
            }
        }

        // Broad attributes are still needed for joins, correlated predicates,
        // reachability, and durable message-state decoration. A named query
        // may declare one attribute subject-local when another indexed clause
        // has already selected the complete candidate records.
        if subject_local_sequence.is_none() {
            for attribute in &plan.attributes {
                let parameters: [&dyn ToSql; 2] = [attribute, &row_limit];
                self.append_datoms(
                    db,
                    &format!(
                        "select origin_id,subject,attribute,value_json,event_sequence,event_id from {table} where attribute=?1 limit ?2"
                    ),
                    &parameters,
                    &mut datoms,
                    &mut seen,
                    max_datoms,
                )?;
            }
        }
        Ok(datoms)
    }
    fn entity_kind(&self, db: &Connection, entity_id: &str) -> Result<String, Value> {
        db.query_row(
            &format!("select kind from {} where entity_id=?1", self.entity_table),
            params![entity_id],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .map_err(self.db_error("query_entity_kind_failed"))?
        .ok_or_else(|| {
            self.error(
                "query_entity_not_found",
                "pull target entity was not found",
                json!({"entity_id":entity_id}),
            )
        })
    }

    fn decorate_query_entity(
        &self,
        db: &Connection,
        id: &str,
        value: &mut Value,
        binding: &Map<String, Value>,
        datoms: &[ledger_query::Datom],
    ) -> Result<(), Value> {
        let kind = self.entity_kind(db, id)?;
        if !self.is_configured_message_kind(&kind) {
            return Ok(());
        }
        let Some(object) = value.as_object_mut() else {
            return Ok(());
        };
        let viewer = binding
            .get("?viewer")
            .and_then(Value::as_str)
            .filter(|value| !value.trim().is_empty());
        let message_state = self.message_read_state(id, viewer, datoms);
        let reply_state_attribute = self
            .domain
            .query
            .reply_state_attribute
            .as_deref()
            .unwrap_or("");
        let reply_count = if reply_state_attribute.is_empty() {
            0
        } else {
            datoms
                .iter()
                .filter(|datom| {
                    datom.attribute == reply_state_attribute && datom.value.as_str() == Some(id)
                })
                .count()
        };
        let is_reply = !reply_state_attribute.is_empty()
            && datoms
                .iter()
                .any(|datom| datom.attribute == reply_state_attribute && datom.subject == id);
        let reply_state = json!({
            "status":if reply_count > 0 { "replied" } else { "unreplied" },
            "has_replies":reply_count > 0,
            "reply_count":reply_count,
            "is_reply":is_reply
        });
        let query_meta = json!({
            "message_state":message_state,
            "reply_state":reply_state,
            "kind":kind,
            "viewer":viewer
        });
        // Preserve legacy top-level fields for callers, but never overwrite a
        // domain payload field. `_narada_query` is the collision-free source
        // of truth for new callers.
        if !object.contains_key("message_state") {
            object.insert("message_state".into(), message_state.clone());
        }
        if !object.contains_key("reply_state") {
            object.insert("reply_state".into(), reply_state.clone());
        }
        object.insert("_narada_query".into(), query_meta);
        Ok(())
    }

    fn render_query_binding(
        &self,
        db: &Connection,
        binding: &Map<String, Value>,
        spec: &ledger_query::QuerySpec,
        datoms: &[ledger_query::Datom],
    ) -> Result<Value, Value> {
        if spec.pulls.len() == 1 && spec.finds.len() == 1 {
            let pull = &spec.pulls[0];
            let id = binding
                .get(&pull.variable)
                .and_then(Value::as_str)
                .ok_or_else(|| {
                    self.error(
                        "query_pull_target_invalid",
                        "pull target is not a string entity, relation, or record id",
                        json!({"variable":pull.variable}),
                    )
                })?;
            let (mut value, is_entity) =
                self.pull_target(db, id, &pull.fields, pull.target_kind.as_deref())?;
            if is_entity {
                self.decorate_query_entity(db, id, &mut value, binding, datoms)?;
            }
            return Ok(value);
        }
        let mut output = Map::new();
        for find in &spec.finds {
            let Some(name) = find.as_variable_name() else {
                continue;
            };
            let value = binding.get(name).ok_or_else(|| {
                self.error(
                    "query_find_unbound",
                    "find variable is not bound in the result",
                    json!({"variable":name}),
                )
            })?;
            output.insert(name.trim_start_matches('?').to_string(), value.clone());
        }
        for pull in &spec.pulls {
            let id = binding
                .get(&pull.variable)
                .and_then(Value::as_str)
                .ok_or_else(|| {
                    self.error(
                        "query_pull_target_invalid",
                        "pull target is not a string entity, relation, or record id",
                        json!({"variable":pull.variable}),
                    )
                })?;
            let (mut value, is_entity) =
                self.pull_target(db, id, &pull.fields, pull.target_kind.as_deref())?;
            if is_entity {
                self.decorate_query_entity(db, id, &mut value, binding, datoms)?;
            }
            output.insert(pull.variable.trim_start_matches('?').to_string(), value);
        }
        Ok(Value::Object(output))
    }

    fn message_read_state(
        &self,
        message_id: &str,
        viewer: Option<&str>,
        datoms: &[ledger_query::Datom],
    ) -> Value {
        let read = match (
            viewer,
            self.domain.query.read_receipt_kind.as_deref(),
            self.domain.query.read_receipt_kind_attribute.as_deref(),
            self.domain.query.read_receipt_message_attribute.as_deref(),
            self.domain.query.read_receipt_reader_attribute.as_deref(),
        ) {
            (
                Some(viewer),
                Some(receipt_kind),
                Some(kind_attribute),
                Some(message_attribute),
                Some(reader_attribute),
            ) => {
                let receipt_ids = datoms
                    .iter()
                    .filter(|datom| {
                        datom.attribute == kind_attribute
                            && datom.value.as_str() == Some(receipt_kind)
                    })
                    .map(|datom| datom.subject.as_str())
                    .collect::<HashSet<_>>();
                Some(datoms.iter().any(|datom| {
                    receipt_ids.contains(datom.subject.as_str())
                        && datom.attribute == message_attribute
                        && datom.value.as_str() == Some(message_id)
                        && datoms.iter().any(|reader_datom| {
                            reader_datom.subject == datom.subject
                                && reader_datom.attribute == reader_attribute
                                && reader_datom.value.as_str() == Some(viewer)
                        })
                }))
            }
            _ => None,
        };
        let status = match read {
            Some(true) => "read",
            Some(false) => "unread",
            None => "unknown",
        };
        json!({
            "status":status,
            "read":read,
            "unread":read.map(|value| !value),
            "viewer":viewer
        })
    }

    fn render_pull_fields(
        &self,
        fields: &[String],
        base: &Map<String, Value>,
        payload: &Value,
    ) -> Value {
        let mut output = Map::new();
        let full = fields.iter().any(|field| field == "*");
        for field in fields {
            if field == "*" {
                continue;
            }
            let value = base
                .get(field)
                .cloned()
                .or_else(|| payload.get(field).cloned())
                .unwrap_or(Value::Null);
            output.insert(field.clone(), value);
        }
        if full {
            for (field, value) in base {
                output.insert(field.clone(), value.clone());
            }
            output.insert("payload".into(), payload.clone());
        }
        Value::Object(output)
    }

    fn parse_pull_payload(&self, target_id: &str, payload_json: &str) -> Result<Value, Value> {
        if payload_json.len() as u64 > self.domain.caps.query_execution.max_output_bytes {
            return Err(self.error(
                "query_payload_limit",
                "pull target payload exceeds the descriptor response-byte budget",
                json!({"target_id":target_id,"payload_bytes":payload_json.len(),"max_output_bytes":self.domain.caps.query_execution.max_output_bytes}),
            ));
        }
        serde_json::from_str::<Value>(payload_json).map_err(|_| {
            self.error(
                "query_pull_payload_invalid",
                "pull target payload is not valid JSON",
                json!({"target_id":target_id}),
            )
        })
    }

    fn pull_target(
        &self,
        db: &Connection,
        target_id: &str,
        fields: &[String],
        target_kind: Option<&str>,
    ) -> Result<(Value, bool), Value> {
        if let Some(target_kind) = target_kind {
            if !matches!(target_kind, "entity" | "relation" | "record") {
                return Err(self.error(
                    "query_pull_target_invalid",
                    "pull target_kind must be entity, relation, or record",
                    json!({"target_id":target_id,"target_kind":target_kind}),
                ));
            }
        }
        let mut matches: Vec<(&str, Value, bool)> = Vec::new();
        if target_kind.is_none() || target_kind == Some("entity") {
            let entity = db
                .query_row(
                    &format!("select entity_id,kind,payload_json,event_id,event_sequence from {} where entity_id=?1", self.entity_table),
                    params![target_id],
                    |row| {
                        let payload_json: String = row.get(2)?;
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, String>(1)?,
                            payload_json,
                            row.get::<_, String>(3)?,
                            row.get::<_, i64>(4)?,
                        ))
                    },
                )
                .optional()
                .map_err(self.db_error("projection_pull_query_failed"))?;
            if let Some((entity_id, kind, payload_json, event_id, event_sequence)) = entity {
                let payload = self.parse_pull_payload(target_id, &payload_json)?;
                let base = Map::from_iter([
                    ("entity_id".into(), json!(entity_id)),
                    ("kind".into(), json!(kind)),
                    ("event_id".into(), json!(event_id)),
                    ("event_sequence".into(), json!(event_sequence)),
                    ("payload".into(), payload.clone()),
                ]);
                matches.push((
                    "entity",
                    self.render_pull_fields(fields, &base, &payload),
                    true,
                ));
            }
        }
        if target_kind.is_none() || target_kind == Some("relation") {
            let relation = db
                .query_row(
                    &format!("select relation_id,relation_type,source_id,target_id,payload_json,event_id,event_sequence from {} where relation_id=?1", self.relation_table),
                    params![target_id],
                    |row| {
                        let payload_json: String = row.get(4)?;
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, String>(1)?,
                            row.get::<_, String>(2)?,
                            row.get::<_, String>(3)?,
                            payload_json,
                            row.get::<_, String>(5)?,
                            row.get::<_, i64>(6)?,
                        ))
                    },
                )
                .optional()
                .map_err(self.db_error("projection_pull_query_failed"))?;
            if let Some((
                relation_id,
                relation_type,
                source_id,
                relation_target_id,
                payload_json,
                event_id,
                event_sequence,
            )) = relation
            {
                let payload = self.parse_pull_payload(target_id, &payload_json)?;
                let base = Map::from_iter([
                    ("relation_id".into(), json!(relation_id)),
                    ("relation_type".into(), json!(relation_type)),
                    ("source_id".into(), json!(source_id)),
                    ("target_id".into(), json!(relation_target_id)),
                    ("event_id".into(), json!(event_id)),
                    ("event_sequence".into(), json!(event_sequence)),
                    ("payload".into(), payload.clone()),
                ]);
                matches.push((
                    "relation",
                    self.render_pull_fields(fields, &base, &payload),
                    false,
                ));
            }
        }
        if target_kind.is_none() || target_kind == Some("record") {
            let record = db
                .query_row(
                    &format!("select record_id,record_kind,payload_json,event_id,event_sequence from {} where record_id=?1", self.records_table),
                    params![target_id],
                    |row| {
                        let payload_json: String = row.get(2)?;
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, String>(1)?,
                            payload_json,
                            row.get::<_, String>(3)?,
                            row.get::<_, i64>(4)?,
                        ))
                    },
                )
                .optional()
                .map_err(self.db_error("projection_pull_query_failed"))?;
            if let Some((record_id, record_kind, payload_json, event_id, event_sequence)) = record {
                let payload = self.parse_pull_payload(target_id, &payload_json)?;
                let base = Map::from_iter([
                    ("record_id".into(), json!(record_id)),
                    ("record_kind".into(), json!(record_kind)),
                    ("event_id".into(), json!(event_id)),
                    ("event_sequence".into(), json!(event_sequence)),
                    ("payload".into(), payload.clone()),
                ]);
                matches.push((
                    "record",
                    self.render_pull_fields(fields, &base, &payload),
                    false,
                ));
            }
        }
        if matches.len() > 1 {
            let target_kinds = matches.iter().map(|(kind, _, _)| *kind).collect::<Vec<_>>();
            return Err(self.error(
                "query_pull_target_ambiguous",
                "pull target id exists in more than one projection; specify target_kind",
                json!({"target_id":target_id,"target_kinds":target_kinds}),
            ));
        }
        if let Some((_, value, is_entity)) = matches.pop() {
            return Ok((value, is_entity));
        }
        Err(self.error(
            "query_pull_target_not_found",
            "pull target was not found in the requested entity, relation, or record projection",
            json!({"target_id":target_id,"target_kind":target_kind,"target_kinds":["entity","relation","record"]}),
        ))
    }

    fn query(&self, root: &Path, args: &Map<String, Value>) -> Result<Value, Value> {
        self.with_stable_projection(root, || self.query_locked(root, args))
    }

    fn query_locked(&self, root: &Path, args: &Map<String, Value>) -> Result<Value, Value> {
        let ledger_head = self.ledger_head(root)?;
        if let Some(expected) = args.get("expected_ledger_head").and_then(Value::as_str) {
            if Some(expected) != ledger_head.as_deref() {
                return Err(self.error(
                    "ledger_head_mismatch",
                    "query expected_ledger_head does not match the current ledger head",
                    json!({"expected_ledger_head":expected,"actual_ledger_head":ledger_head}),
                ));
            }
        }
        let db = Connection::open(self.projection_path(root))
            .map_err(self.db_error("projection_open_failed"))?;
        let limit = args
            .get("limit")
            .and_then(Value::as_u64)
            .unwrap_or(self.domain.caps.query_limit.default)
            .clamp(
                self.domain.caps.query_limit.min,
                self.domain.caps.query_limit.max,
            );
        let offset = args.get("offset").and_then(Value::as_u64).unwrap_or(0);
        let kind = args.get("kind").and_then(Value::as_str).unwrap_or("");
        let compact = args
            .get("compact")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let text = args.get("text").and_then(Value::as_str).unwrap_or("");
        let like = format!("%{text}%");
        if let Some(record_kind) = args.get("record_kind").and_then(Value::as_str) {
            let sql = format!("select record_id,record_kind,payload_json,event_id from {} where record_kind=?1 and (?2='' or payload_json like ?3) order by record_id limit ?4 offset ?5", self.records_table);
            let mut stmt = db
                .prepare(&sql)
                .map_err(self.db_error("projection_record_query_prepare_failed"))?;
            let projection = if compact {
                &self.domain.query.record_compact_projection
            } else {
                &self.domain.query.record_full_projection
            };
            let rows = stmt
                .query_map(params![record_kind, text, like, limit, offset], |row| {
                    let payload = serde_json::from_str::<Value>(&row.get::<_, String>(2)?)
                        .unwrap_or(Value::Null);
                    let mut row_values = Map::new();
                    row_values.insert("record_id".into(), json!(row.get::<_, String>(0)?));
                    row_values.insert("record_kind".into(), json!(row.get::<_, String>(1)?));
                    row_values.insert("event_id".into(), json!(row.get::<_, String>(3)?));
                    Ok(Self::project_row(&row_values, &payload, projection))
                })
                .map_err(self.db_error("projection_record_query_failed"))?;
            let items = rows
                .collect::<Result<Vec<_>, _>>()
                .map_err(self.db_error("projection_record_query_row_failed"))?;
            let mut response = json!({"schema":self.schema_id("query.v1"),"status":"ok","result_kind":"records","record_kind":record_kind,"compact":compact,"ledger_head":ledger_head,"items":items,"offset":offset,"limit":limit,"returned":items.len(),"bounded":true,"max_output_bytes":self.domain.caps.query_execution.max_output_bytes});
            self.finalize_bounded_output(&mut response)?;
            return Ok(response);
        }
        let accepted_kind_values = if kind.is_empty() {
            Vec::new()
        } else {
            self.expand_legacy_kind_value(kind)?
        };
        let accepted_kinds = accepted_kind_values
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_string)
            .collect::<Vec<_>>();
        let kind_predicate = if accepted_kinds.is_empty() {
            self.visible_entity_predicate()
        } else {
            let literals = accepted_kinds
                .iter()
                .map(|value| Self::sql_quote(value))
                .collect::<Vec<_>>()
                .join(",");
            format!("kind in ({literals})")
        };
        let sql = format!("select entity_id,kind,payload_json,event_id from {} where {kind_predicate} and (?1='' or payload_json like ?2) order by entity_id limit ?3 offset ?4", self.entity_table);
        let mut stmt = db
            .prepare(&sql)
            .map_err(self.db_error("projection_query_prepare_failed"))?;
        let projection = if compact {
            &self.domain.query.entity_compact_projection
        } else {
            &self.domain.query.entity_full_projection
        };
        let rows = stmt
            .query_map(params![text, like, limit, offset], |row| {
                let payload =
                    serde_json::from_str::<Value>(&row.get::<_, String>(2)?).unwrap_or(Value::Null);
                let mut row_values = Map::new();
                row_values.insert("entity_id".into(), json!(row.get::<_, String>(0)?));
                row_values.insert("kind".into(), json!(row.get::<_, String>(1)?));
                row_values.insert("event_id".into(), json!(row.get::<_, String>(3)?));
                Ok(Self::project_row(&row_values, &payload, projection))
            })
            .map_err(self.db_error("projection_query_failed"))?;
        let items = rows
            .collect::<Result<Vec<_>, _>>()
            .map_err(self.db_error("projection_query_row_failed"))?;
        let mut response = json!({"schema":self.schema_id("query.v1"),"status":"ok","result_kind":"entities","kind":kind,"expanded_kinds":accepted_kinds,"compact":compact,"ledger_head":ledger_head,"items":items,"offset":offset,"limit":limit,"returned":items.len(),"bounded":true,"max_output_bytes":self.domain.caps.query_execution.max_output_bytes});
        self.finalize_bounded_output(&mut response)?;
        Ok(response)
    }

    fn snapshot(&self, root: &Path, args: &Map<String, Value>) -> Result<Value, Value> {
        self.with_stable_projection(root, || self.snapshot_locked(root, args))
    }

    fn snapshot_locked(&self, root: &Path, args: &Map<String, Value>) -> Result<Value, Value> {
        let feature = &self.domain.features.snapshot;
        let ledger_head = self.ledger_head(root)?;
        if let Some(expected) = args.get("expected_ledger_head").and_then(Value::as_str) {
            if Some(expected) != ledger_head.as_deref() {
                return Err(self.error(
                    &feature.head_mismatch_refusal_code,
                    "The graph changed after the requested snapshot began.",
                    json!({"expected_ledger_head":expected,"actual_ledger_head":ledger_head}),
                ));
            }
        }
        let caps = &self.domain.caps.snapshot_limit;
        let limit = args
            .get("limit")
            .and_then(Value::as_u64)
            .unwrap_or(caps.default)
            .clamp(caps.min, caps.max);
        let entity_offset = args
            .get("entity_offset")
            .and_then(Value::as_u64)
            .unwrap_or(0);
        let relation_offset = args
            .get("relation_offset")
            .and_then(Value::as_u64)
            .unwrap_or(0);
        let db = Connection::open(self.projection_path(root))
            .map_err(self.db_error("projection_open_failed"))?;
        let visible_entities = self.visible_entity_predicate();
        let entity_count: i64 = db
            .query_row(
                &format!(
                    "select count(*) from {} where {visible_entities}",
                    self.entity_table
                ),
                [],
                |row| row.get(0),
            )
            .map_err(self.db_error("projection_count_failed"))?;
        let relation_count: i64 = db
            .query_row(
                &format!("select count(*) from {}", self.relation_table),
                [],
                |row| row.get(0),
            )
            .map_err(self.db_error("projection_count_failed"))?;

        let mut entity_statement = db
            .prepare(&format!("select entity_id,kind,payload_json,event_id from {} where {visible_entities} order by entity_id limit ?1 offset ?2", self.entity_table))
            .map_err(self.db_error("projection_snapshot_entities_prepare_failed"))?;
        let entities = entity_statement
            .query_map(params![limit, entity_offset], |row| {
                let payload =
                    serde_json::from_str::<Value>(&row.get::<_, String>(2)?).unwrap_or(Value::Null);
                Ok(json!({
                    "entity_id":row.get::<_,String>(0)?,
                    "kind":row.get::<_,String>(1)?,
                    "title":payload.get("title"),
                    "payload":payload,
                    "event_id":row.get::<_,String>(3)?
                }))
            })
            .map_err(self.db_error("projection_snapshot_entities_failed"))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(self.db_error("projection_snapshot_entity_row_failed"))?;

        let mut relation_statement = db
            .prepare(&format!("select relation_id,relation_type,source_id,target_id,payload_json,event_id from {} order by relation_id limit ?1 offset ?2", self.relation_table))
            .map_err(self.db_error("projection_snapshot_relations_prepare_failed"))?;
        let relations = relation_statement
            .query_map(params![limit, relation_offset], |row| {
                let payload =
                    serde_json::from_str::<Value>(&row.get::<_, String>(4)?).unwrap_or(Value::Null);
                Ok(json!({
                    "relation_id":row.get::<_,String>(0)?,
                    "relation_type":row.get::<_,String>(1)?,
                    "source_id":row.get::<_,String>(2)?,
                    "target_id":row.get::<_,String>(3)?,
                    "payload":payload,
                    "event_id":row.get::<_,String>(5)?
                }))
            })
            .map_err(self.db_error("projection_snapshot_relations_failed"))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(self.db_error("projection_snapshot_relation_row_failed"))?;

        let next_entity_offset = entity_offset + entities.len() as u64;
        let next_relation_offset = relation_offset + relations.len() as u64;
        Ok(json!({
            "schema":feature.response_schema_id,
            "status":"ok",
            "ledger_head":ledger_head,
            "entity_count":entity_count,
            "relation_count":relation_count,
            "entities":entities,
            "relations":relations,
            "entity_offset":entity_offset,
            "relation_offset":relation_offset,
            "next_entity_offset":(next_entity_offset < entity_count as u64).then_some(next_entity_offset),
            "next_relation_offset":(next_relation_offset < relation_count as u64).then_some(next_relation_offset),
            "limit":limit,
            "bounded":true
        }))
    }

    fn query_batch(&self, root: &Path, args: &Map<String, Value>) -> Result<Value, Value> {
        self.with_stable_projection(root, || self.query_batch_locked(root, args))
    }

    fn query_batch_locked(&self, root: &Path, args: &Map<String, Value>) -> Result<Value, Value> {
        let caps = &self.domain.caps.query_batch;
        for key in args.keys() {
            if !["queries", "limit_per_query", "expected_ledger_head"].contains(&key.as_str()) {
                return Err(self.error(
                    "invalid_batch_query",
                    "batch query accepts only queries, limit_per_query, and expected_ledger_head",
                    json!({"field":key}),
                ));
            }
        }
        let queries = args
            .get("queries")
            .and_then(Value::as_array)
            .ok_or_else(|| {
                self.error(
                    "invalid_batch_query",
                    "queries must be an array",
                    Value::Null,
                )
            })?;
        if (queries.len() as u64) < caps.min_queries || (queries.len() as u64) > caps.max_queries {
            return Err(self.error(
                "invalid_batch_query",
                &format!(
                    "queries count must be between {} and {}",
                    caps.min_queries, caps.max_queries
                ),
                json!({"count":queries.len()}),
            ));
        }
        if let Some(limit) = args.get("limit_per_query") {
            if !limit.as_u64().is_some_and(|value| value > 0) {
                return Err(self.error(
                    "invalid_batch_query",
                    "limit_per_query must be a positive integer",
                    json!({"field":"limit_per_query"}),
                ));
            }
        }
        let expected_ledger_head = args.get("expected_ledger_head").cloned();
        if let Some(expected) = &expected_ledger_head {
            if !(expected.is_null()
                || expected
                    .as_str()
                    .is_some_and(|value| !value.trim().is_empty()))
            {
                return Err(self.error(
                    "invalid_batch_query",
                    "expected_ledger_head must be a non-empty string or null",
                    json!({"field":"expected_ledger_head"}),
                ));
            }
        }
        let batch_limit = args
            .get("limit_per_query")
            .and_then(Value::as_u64)
            .unwrap_or(caps.limit_per_query_default)
            .clamp(caps.limit_per_query_min, caps.limit_per_query_max);
        let mut results = Vec::with_capacity(queries.len());
        for (index, item) in queries.iter().enumerate() {
            let item = item.as_object().ok_or_else(|| {
                self.error(
                    "invalid_batch_query",
                    "each query must be an object",
                    json!({"index":index}),
                )
            })?;
            if let Some(limit) = item.get("limit") {
                if !limit.as_u64().is_some_and(|value| value > 0) {
                    return Err(self.error(
                        "invalid_batch_query",
                        "query item limit must be a positive integer",
                        json!({"index":index,"field":"limit"}),
                    ));
                }
            }
            if let Some(cursor) = item.get("cursor") {
                if !(cursor.is_null() || cursor.is_string() || cursor.is_object()) {
                    return Err(self.error(
                        "invalid_batch_query",
                        "query item cursor must be a string, object, or null",
                        json!({"index":index,"field":"cursor"}),
                    ));
                }
            }
            if let Some(item_expected) = item.get("expected_ledger_head") {
                if !(item_expected.is_null()
                    || item_expected
                        .as_str()
                        .is_some_and(|value| !value.trim().is_empty()))
                {
                    return Err(self.error(
                        "invalid_batch_query",
                        "query item expected_ledger_head must be a non-empty string or null",
                        json!({"index":index,"field":"expected_ledger_head"}),
                    ));
                }
            }
            let mut query_args = item.clone();
            if let Some(expected) = &expected_ledger_head {
                if let Some(item_expected) = item.get("expected_ledger_head") {
                    if !item_expected.is_null() && item_expected != expected {
                        return Err(self.error(
                            "query_expected_head_conflict",
                            "batch expected_ledger_head cannot be overridden by an item",
                            json!({"index":index,"batch":expected,"item":item_expected}),
                        ));
                    }
                }
                query_args.insert("expected_ledger_head".into(), expected.clone());
            }
            let generic = query_args.contains_key("query") || query_args.contains_key("template");
            let named_fields = [
                "recipient",
                "participant",
                "sender",
                "from",
                "to",
                "kinds",
                "since_event",
                "after_sequence",
                "include_body",
                "direction",
                "viewer",
                "intent",
                "read_state",
                "reply_state",
                "match",
                "root",
                "max_depth",
            ];
            let has_cursor = query_args
                .get("cursor")
                .map(|value| !value.is_null())
                .unwrap_or(false);
            if !generic && has_cursor {
                return Err(self.error(
                    "query_cursor_unsupported",
                    "legacy batch queries use offset pagination; cursor requires query or template",
                    json!({"index":index}),
                ));
            }
            if !generic
                && named_fields
                    .iter()
                    .any(|field| query_args.contains_key(*field))
            {
                return Err(self.error(
                    "query_template_missing",
                    "template is required when named-query filters are supplied in a batch item",
                    json!({"index":index}),
                ));
            }
            if generic {
                if query_args.contains_key("query") {
                    self.validate_raw_query_arguments(&query_args)?;
                } else {
                    self.validate_named_filter_conflicts(&query_args)?;
                }
            }
            let requested_limit = query_args
                .get("limit")
                .and_then(Value::as_u64)
                .or_else(|| {
                    query_args
                        .get("query")
                        .and_then(Value::as_object)
                        .and_then(|query| query.get("limit"))
                        .and_then(Value::as_u64)
                })
                .or_else(|| {
                    query_args
                        .get("match")
                        .and_then(Value::as_object)
                        .and_then(|matched| matched.get("limit"))
                        .and_then(Value::as_u64)
                })
                .unwrap_or(batch_limit);
            let effective_limit = requested_limit.clamp(caps.limit_per_query_min, batch_limit);
            if generic {
                if let Some(query) = query_args.get_mut("query").and_then(Value::as_object_mut) {
                    query.insert("limit".into(), json!(effective_limit));
                }
                if let Some(matched) = query_args.get_mut("match").and_then(Value::as_object_mut) {
                    if matched.contains_key("limit") {
                        matched.insert("limit".into(), json!(effective_limit));
                    }
                }
                query_args.insert("limit".into(), json!(effective_limit));
            } else {
                query_args.entry("compact").or_insert(json!(true));
                query_args.insert("limit".into(), json!(effective_limit));
                query_args.insert("offset".into(), json!(0));
            }
            let result = if generic {
                self.generic_query_locked(root, &query_args)?
            } else {
                self.query_locked(root, &query_args)?
            };
            let returned = result
                .get("returned")
                .cloned()
                .or_else(|| result.get("count").cloned())
                .unwrap_or(Value::Null);
            let query_origin = result
                .get("query_origin")
                .cloned()
                .unwrap_or_else(|| json!("legacy"));
            results.push(json!({
                "index":index,
                "mode":if generic { "datalog" } else { "legacy" },
                "query_origin":query_origin.clone(),
                "request":{
                    "mode":if item.contains_key("query") { "raw" } else if item.contains_key("template") { "named_template" } else { "legacy" },
                    "template":result.get("template").cloned().unwrap_or(Value::Null),
                    "match":item.get("match").cloned().unwrap_or(Value::Null),
                    "kind":item.get("kind").cloned().unwrap_or(Value::Null),
                    "record_kind":item.get("record_kind").cloned().unwrap_or(Value::Null),
                    "text":item.get("text").cloned().unwrap_or(Value::Null)
                },
                "result_schema":result.get("schema").cloned().unwrap_or(Value::Null),
                "ledger_head":result.get("ledger_head").cloned().unwrap_or(Value::Null),
                "text":item.get("text"),
                "kind":item.get("kind"),
                "record_kind":item.get("record_kind"),
                "returned":returned,
                "count":result.get("count").cloned().unwrap_or(returned.clone()),
                "count_semantics":result.get("count_semantics").cloned().unwrap_or_else(|| json!("returned_page")),
                "limit":result.get("limit").cloned().unwrap_or_else(|| json!(effective_limit)),
                "items":result.get("items").cloned().unwrap_or_else(|| json!([])),
                "has_more":result.get("has_more").cloned().unwrap_or_else(|| json!(false)),
                "next_cursor":result.get("next_cursor").cloned().unwrap_or(Value::Null)
            }));
        }
        let mut response = json!({
            "schema":self.schema_id("query_batch.v2"),
            "status":"ok",
            "query_count":queries.len(),
            "limit_per_query":batch_limit,
            "results":results,
            "bounded":true,
            "output_bytes":0,
            "max_output_bytes":self.domain.caps.query_execution.max_output_bytes
        });
        self.finalize_bounded_output(&mut response)?;
        Ok(response)
    }

    fn source_inspect(&self, root: &Path, args: &Map<String, Value>) -> Result<Value, Value> {
        let feature = &self.domain.features.source_inspect;
        let caps = &self.domain.caps.source_inspect;
        let paths = args.get("paths").and_then(Value::as_array).ok_or_else(|| {
            self.error(
                "invalid_source_inspection",
                "paths must be an array",
                Value::Null,
            )
        })?;
        if (paths.len() as u64) < caps.paths_min || (paths.len() as u64) > caps.paths_max {
            return Err(self.error(
                "invalid_source_inspection",
                &format!(
                    "paths count must be between {} and {}",
                    caps.paths_min, caps.paths_max
                ),
                json!({"count":paths.len()}),
            ));
        }
        let max_sections = args
            .get("max_sections_per_file")
            .and_then(Value::as_u64)
            .unwrap_or(caps.sections_default)
            .min(caps.sections_max) as usize;
        let max_chars = args
            .get("max_chars_per_section")
            .and_then(Value::as_u64)
            .unwrap_or(caps.chars_default)
            .clamp(caps.chars_min, caps.chars_max) as usize;
        let canonical_root =
            fs::canonicalize(root).map_err(self.io_error("site_root_resolve_failed"))?;
        let relevant = &feature.keywords;
        let mut files = Vec::with_capacity(paths.len());
        for value in paths {
            let locator = value.as_str().ok_or_else(|| {
                self.error(
                    "invalid_source_inspection",
                    "each path must be a string",
                    Value::Null,
                )
            })?;
            let requested = PathBuf::from(locator);
            let candidate = if requested.is_absolute() {
                requested
            } else {
                canonical_root.join(requested)
            };
            let canonical =
                fs::canonicalize(&candidate).map_err(self.io_error("source_resolve_failed"))?;
            if !canonical.starts_with(&canonical_root) {
                return Err(self.error(
                    &feature.outside_refusal_code,
                    "source path must remain inside the site root",
                    json!({"path":locator}),
                ));
            }
            let metadata =
                fs::metadata(&canonical).map_err(self.io_error("source_metadata_failed"))?;
            if metadata.len() > caps.file_bytes_max {
                return Err(self.error(
                    &feature.too_large_refusal_code,
                    "source exceeds the 1 MiB inspection limit",
                    json!({"path":locator,"size":metadata.len(),"max_size":caps.file_bytes_max}),
                ));
            }
            let content =
                fs::read_to_string(&canonical).map_err(self.io_error("source_read_failed"))?;
            let lines = content.lines().collect::<Vec<_>>();
            let headings = lines
                .iter()
                .enumerate()
                .filter_map(|(index, line)| {
                    let trimmed = line.trim_start();
                    trimmed
                        .starts_with('#')
                        .then_some((index, trimmed.trim_start_matches('#').trim()))
                })
                .collect::<Vec<_>>();
            let title = headings.first().map(|(_, heading)| *heading);
            let mut sections = Vec::new();
            for (heading_index, (start, heading)) in headings.iter().enumerate() {
                let normalized = heading.to_ascii_lowercase();
                if !relevant.iter().any(|needle| normalized.contains(needle)) {
                    continue;
                }
                let end = headings
                    .get(heading_index + 1)
                    .map(|(line, _)| *line)
                    .unwrap_or(lines.len());
                let full = lines[*start..end].join("\n");
                let excerpt = full.chars().take(max_chars).collect::<String>();
                sections.push(json!({
                    "heading":heading,
                    "start_line":start + 1,
                    "end_line":end,
                    "excerpt":excerpt,
                    "truncated":full.chars().count() > max_chars
                }));
                if sections.len() == max_sections {
                    break;
                }
            }
            files.push(json!({
                "path":locator,
                "title":title,
                "line_count":lines.len(),
                "sections":sections,
                "section_count":sections.len(),
                "sections_truncated":headings.iter().filter(|(_, heading)| {
                    let normalized = heading.to_ascii_lowercase();
                    relevant.iter().any(|needle| normalized.contains(needle))
                }).count() > sections.len()
            }));
        }
        Ok(json!({
            "schema":feature.response_schema_id,
            "status":"ok",
            "file_count":files.len(),
            "files":files,
            "bounded":true
        }))
    }

    fn neighborhood(&self, root: &Path, args: &Map<String, Value>) -> Result<Value, Value> {
        self.with_stable_projection(root, || self.neighborhood_locked(root, args))
    }

    fn neighborhood_locked(&self, root: &Path, args: &Map<String, Value>) -> Result<Value, Value> {
        let id = self.required(args, "entity_id")?;
        let limit = args
            .get("limit")
            .and_then(Value::as_u64)
            .unwrap_or(self.domain.caps.neighborhood_limit.default)
            .min(self.domain.caps.neighborhood_limit.max);
        let db = Connection::open(self.projection_path(root))
            .map_err(self.db_error("projection_open_failed"))?;
        let entity_pk = self.table(&self.entity_table).primary_key.clone();
        let entity: Option<String> = db
            .query_row(
                &format!(
                    "select payload_json from {} where {}=?1",
                    self.entity_table, entity_pk
                ),
                [&id],
                |r| r.get(0),
            )
            .optional()
            .map_err(self.db_error("projection_entity_read_failed"))?;
        let entity = entity.ok_or_else(|| {
            self.error(
                "entity_not_found",
                "entity not found",
                json!({"entity_id":id}),
            )
        })?;
        let mut stmt = db.prepare(&format!("select relation_id,relation_type,source_id,target_id,payload_json from {} where source_id=?1 or target_id=?1 order by relation_id limit ?2", self.relation_table)).map_err(self.db_error("projection_relation_prepare_failed"))?;
        let relation_fields = &self.domain.query.neighborhood_relation_fields;
        let rows = stmt
            .query_map(params![id, limit], |r| {
                let payload =
                    serde_json::from_str::<Value>(&r.get::<_, String>(4)?).unwrap_or(Value::Null);
                let mut row_values = Map::new();
                row_values.insert("relation_id".into(), json!(r.get::<_, String>(0)?));
                row_values.insert("relation_type".into(), json!(r.get::<_, String>(1)?));
                row_values.insert("source_id".into(), json!(r.get::<_, String>(2)?));
                row_values.insert("target_id".into(), json!(r.get::<_, String>(3)?));
                Ok(Self::project_row(&row_values, &payload, relation_fields))
            })
            .map_err(self.db_error("projection_relation_query_failed"))?;
        let relations = rows
            .collect::<Result<Vec<_>, _>>()
            .map_err(self.db_error("projection_relation_row_failed"))?;
        let match_clause = self
            .domain
            .query
            .neighborhood_record_match_fields
            .iter()
            .map(|field| format!("json_extract(payload_json,'$.{field}')=?1"))
            .collect::<Vec<_>>()
            .join(" or ");
        let record_sql = format!("select record_id,record_kind,payload_json,event_id from {} where {} order by record_id limit ?2", self.records_table, match_clause);
        let record_fields = &self.domain.query.neighborhood_record_fields;
        let mut record_stmt = db
            .prepare(&record_sql)
            .map_err(self.db_error("projection_neighborhood_record_prepare_failed"))?;
        let records = record_stmt
            .query_map(params![id, limit], |r| {
                let payload =
                    serde_json::from_str::<Value>(&r.get::<_, String>(2)?).unwrap_or(Value::Null);
                let mut row_values = Map::new();
                row_values.insert("record_id".into(), json!(r.get::<_, String>(0)?));
                row_values.insert("record_kind".into(), json!(r.get::<_, String>(1)?));
                row_values.insert("event_id".into(), json!(r.get::<_, String>(3)?));
                Ok(Self::project_row(&row_values, &payload, record_fields))
            })
            .map_err(self.db_error("projection_neighborhood_record_query_failed"))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(self.db_error("projection_neighborhood_record_row_failed"))?;
        Ok(
            json!({"schema":self.schema_id("neighborhood.v1"),"status":"ok","entity":serde_json::from_str::<Value>(&entity).unwrap_or(Value::Null),"relations":relations,"records":records,"limit":limit,"bounded":true}),
        )
    }

    fn export(&self, root: &Path, args: &Map<String, Value>) -> Result<Value, Value> {
        self.with_stable_projection(root, || self.export_locked(root, args))
    }

    fn export_locked(&self, root: &Path, args: &Map<String, Value>) -> Result<Value, Value> {
        let feature = &self.domain.features.export;
        let caps = &self.domain.caps.export;
        let format = args
            .get("format")
            .and_then(Value::as_str)
            .unwrap_or(&feature.default_format);
        let entities = self.query_locked(
            root,
            &Map::from_iter([("limit".into(), json!(caps.entities))]),
        )?["items"]
            .clone();
        let db = Connection::open(self.projection_path(root))
            .map_err(self.db_error("projection_open_failed"))?;
        let mut stmt = db
            .prepare(&format!(
                "select payload_json from {} order by relation_id limit {}",
                self.relation_table, caps.relations
            ))
            .map_err(self.db_error("projection_export_prepare_failed"))?;
        let relations = stmt
            .query_map([], |r| {
                Ok(serde_json::from_str::<Value>(&r.get::<_, String>(0)?).unwrap_or(Value::Null))
            })
            .map_err(self.db_error("projection_export_failed"))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(self.db_error("projection_export_row_failed"))?;
        let mut record_stmt = db
            .prepare(&format!(
                "select payload_json from {} order by record_id limit {}",
                self.records_table, caps.records
            ))
            .map_err(self.db_error("projection_export_record_prepare_failed"))?;
        let records = record_stmt
            .query_map([], |r| {
                Ok(serde_json::from_str::<Value>(&r.get::<_, String>(0)?).unwrap_or(Value::Null))
            })
            .map_err(self.db_error("projection_export_record_failed"))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(self.db_error("projection_export_record_row_failed"))?;
        let context = if format == "jsonld" {
            json!(feature.jsonld_context)
        } else {
            Value::Null
        };
        Ok(
            json!({"schema":feature.response_schema_id,"format":format,"ledger_head":self.ledger_head(root)?,"@context":context,"entities":entities,"relations":relations,"records":records,"bounded":true}),
        )
    }

    fn rebuild_projection(&self, root: &Path) -> Result<(), Value> {
        self.prepare(root)?;
        self.with_authority_lock(root, "projection", || self.rebuild_projection_locked(root))
    }

    fn with_stable_projection<T>(
        &self,
        root: &Path,
        action: impl FnOnce() -> Result<T, Value>,
    ) -> Result<T, Value> {
        self.prepare(root)?;
        // Ledger first, projection second: proposal admission already holds
        // the ledger lock while refreshing the projection, so every stable
        // read uses the same lock order and cannot observe a moving head.
        self.with_authority_lock(root, "ledger", || {
            self.with_authority_lock(root, "projection", || {
                self.rebuild_projection_locked(root)?;
                action()
            })
        })
    }

    fn projection_is_current(
        &self,
        root: &Path,
        ledger_head: &Option<String>,
        ledger_sequence: u64,
    ) -> Result<bool, Value> {
        let Some(table) = &self.projection_meta_table else {
            return Ok(false);
        };
        let path = self.projection_path(root);
        if !path.exists() {
            return Ok(false);
        }
        let db = Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .map_err(self.db_error("projection_open_failed"))?;
        let stored = db
            .query_row(
                &format!("select ledger_head,ledger_sequence from {table} where meta_id='current'"),
                [],
                |row| Ok((row.get::<_, Option<String>>(0)?, row.get::<_, i64>(1)?)),
            )
            .optional();
        let Ok(Some((stored_head, stored_sequence))) = stored else {
            // A projection built by an older descriptor is disposable. A
            // missing metadata table/row therefore means rebuild, not a
            // surfaced corruption error.
            return Ok(false);
        };
        Ok(stored_head.as_deref() == ledger_head.as_deref()
            && stored_sequence == ledger_sequence as i64)
    }

    fn rebuild_projection_locked(&self, root: &Path) -> Result<(), Value> {
        event_ledger::verify(self.error, &self.ledger_layout(root), self.event_hash_field)?;
        let ledger_files = self.ledger_files(root)?;
        let ledger_head = self.ledger_head(root)?;
        let ledger_sequence = ledger_files.len() as u64;
        if self.projection_is_current(root, &ledger_head, ledger_sequence)? {
            return Ok(());
        }
        if self.catch_up_projection_locked(root, &ledger_files)? {
            return Ok(());
        }
        let ddl = self.domain.projection.ddl.clone();
        ledger_projection::rebuild_projection(
            self.error,
            &self.ledger_layout(root),
            self.event_hash_field,
            &self.projection_path(root),
            &ddl,
            |tx, event, event_id| self.fold_projection_event(tx, event, event_id),
        )
    }

    fn catch_up_projection_locked(
        &self,
        root: &Path,
        ledger_files: &[PathBuf],
    ) -> Result<bool, Value> {
        let Some(meta_table) = &self.projection_meta_table else {
            return Ok(false);
        };
        let projection_path = self.projection_path(root);
        if !projection_path.exists() {
            return Ok(false);
        }
        let mut db =
            Connection::open(&projection_path).map_err(self.db_error("projection_open_failed"))?;
        let stored = db
            .query_row(
                &format!(
                    "select ledger_head,ledger_sequence from {meta_table} where meta_id='current'"
                ),
                [],
                |row| Ok((row.get::<_, Option<String>>(0)?, row.get::<_, i64>(1)?)),
            )
            .optional();
        let Ok(Some((stored_head, stored_sequence))) = stored else {
            return Ok(false);
        };
        if stored_sequence < 0 || stored_sequence as usize > ledger_files.len() {
            return Ok(false);
        }
        let prefix_head = if stored_sequence == 0 {
            None
        } else {
            let event = self.read_json(&ledger_files[stored_sequence as usize - 1])?;
            event
                .get(self.event_hash_field)
                .and_then(Value::as_str)
                .map(str::to_string)
        };
        if prefix_head != stored_head {
            return Ok(false);
        }

        let tx = db
            .transaction()
            .map_err(self.db_error("projection_increment_begin_failed"))?;
        for path in ledger_files.iter().skip(stored_sequence as usize) {
            let event = self.read_json(path)?;
            let event_id = event["event_id"].as_str().ok_or_else(|| {
                self.error(
                    "projection_event_invalid",
                    "ledger event lacks event_id",
                    json!({"path":path}),
                )
            })?;
            self.fold_projection_event(&tx, &event, event_id)?;
        }
        tx.commit()
            .map_err(self.db_error("projection_increment_commit_failed"))?;
        Ok(true)
    }

    fn fold_projection_event(
        &self,
        tx: &Transaction<'_>,
        event: &Value,
        event_id: &str,
    ) -> Result<(), Value> {
        for op in event["operations"].as_array().into_iter().flatten() {
            let op_kind = op["op"].as_str().unwrap_or_default();
            if op_kind == self.domain.query.communication.canonicalization_operation {
                let entity_id = op
                    .get("entity_id")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let canonical_kind = op
                    .get("canonical_kind")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let changed = tx
                    .execute(
                        &format!(
                            "update {} set kind=?1 where entity_id=?2",
                            self.entity_table
                        ),
                        params![canonical_kind, entity_id],
                    )
                    .map_err(self.db_error("projection_entity_canonicalization_failed"))?;
                if changed != 1 {
                    return Err(self.error(
                        "projection_entity_canonicalization_missing",
                        "canonicalization target is absent from the projection",
                        json!({"entity_id":entity_id}),
                    ));
                }
                if let Some(table) = &self.datoms_table {
                    tx.execute(
                        &format!("delete from {table} where origin_id=?1 and attribute='narada.ledger:entity/kind'"),
                        params![entity_id],
                    ).map_err(self.db_error("projection_datom_delete_failed"))?;
                    let sequence = event["sequence"].as_u64().unwrap_or_default();
                    self.write_datom(
                        tx,
                        table,
                        entity_id,
                        entity_id,
                        "narada.ledger:entity/kind",
                        &Value::String(canonical_kind.to_string()),
                        sequence,
                        event_id,
                    )?;
                    self.write_datom(
                        tx,
                        table,
                        entity_id,
                        entity_id,
                        "narada.ledger:entity/kind_canonicalized_from",
                        op.get("legacy_kind").unwrap_or(&Value::Null),
                        sequence,
                        event_id,
                    )?;
                    self.write_datom(
                        tx,
                        table,
                        entity_id,
                        entity_id,
                        "narada.ledger:entity/kind_canonicalization_event",
                        &Value::String(event_id.to_string()),
                        sequence,
                        event_id,
                    )?;
                }
                continue;
            }
            let Some(fold) = self
                .domain
                .projection
                .fold
                .iter()
                .find(|entry| entry.operation == op_kind)
            else {
                continue;
            };
            let table = self.table(&fold.table);
            let placeholders = (1..=table.columns.len())
                .map(|index| format!("?{index}"))
                .collect::<Vec<_>>()
                .join(",");
            let sql = format!(
                "{} into {} values({})",
                self.domain.projection.write_mode, table.name, placeholders
            );
            let mut values = Vec::with_capacity(table.columns.len());
            for column in &table.columns {
                let value = if *column == table.primary_key {
                    op.get(&fold.key_field)
                        .and_then(Value::as_str)
                        .unwrap()
                        .to_string()
                } else if column == "payload_json" {
                    op.to_string()
                } else if column == "event_id" {
                    event_id.to_string()
                } else if column == "event_sequence" {
                    event["sequence"].as_u64().unwrap_or_default().to_string()
                } else {
                    let mapping = fold
                        .columns
                        .get(column)
                        .and_then(Value::as_str)
                        .unwrap_or_default();
                    op.get(mapping)
                        .and_then(Value::as_str)
                        .unwrap_or(mapping)
                        .to_string()
                };
                values.push(value);
            }
            let code = if table.name == self.entity_table {
                "projection_entity_write_failed"
            } else if table.name == self.relation_table {
                "projection_relation_write_failed"
            } else {
                "projection_assessment_write_failed"
            };
            tx.execute(&sql, rusqlite::params_from_iter(values))
                .map_err(self.db_error(code))?;
            self.emit_datoms(tx, op, event, event_id, op_kind)?;
        }
        if let Some(table) = &self.projection_meta_table {
            tx.execute(
                &format!(
                    "insert or replace into {table}(meta_id,ledger_head,ledger_sequence,updated_event_id) values('current',?1,?2,?3)"
                ),
                params![
                    event.get(self.event_hash_field).and_then(Value::as_str),
                    event["sequence"].as_u64().unwrap_or_default() as i64,
                    event_id,
                ],
            ).map_err(self.db_error("projection_metadata_write_failed"))?;
        }
        Ok(())
    }

    fn emit_datoms(
        &self,
        tx: &Transaction<'_>,
        operation: &Value,
        event: &Value,
        event_id: &str,
        operation_kind: &str,
    ) -> Result<(), Value> {
        let Some(table) = &self.datoms_table else {
            return Ok(());
        };
        let (origin_id, subject) = if operation_kind == self.entity_op() {
            let id = operation
                .get("entity_id")
                .and_then(Value::as_str)
                .ok_or_else(|| {
                    self.error(
                        "projection_datom_invalid",
                        "entity operation lacks entity_id",
                        operation.clone(),
                    )
                })?;
            (id.to_string(), id.to_string())
        } else if operation_kind == self.relation_op() {
            let origin = operation
                .get("relation_id")
                .and_then(Value::as_str)
                .ok_or_else(|| {
                    self.error(
                        "projection_datom_invalid",
                        "relation operation lacks relation_id",
                        operation.clone(),
                    )
                })?;
            let subject = operation
                .get("source_id")
                .and_then(Value::as_str)
                .ok_or_else(|| {
                    self.error(
                        "projection_datom_invalid",
                        "relation operation lacks source_id",
                        operation.clone(),
                    )
                })?;
            (origin.to_string(), subject.to_string())
        } else {
            let fold = self
                .domain
                .projection
                .fold
                .iter()
                .find(|entry| entry.operation == operation_kind)
                .ok_or_else(|| {
                    self.error(
                        "projection_datom_invalid",
                        "operation has no fold descriptor",
                        json!({"operation":operation_kind}),
                    )
                })?;
            let id = operation
                .get(&fold.key_field)
                .and_then(Value::as_str)
                .ok_or_else(|| {
                    self.error(
                        "projection_datom_invalid",
                        "record operation lacks its identity field",
                        json!({"operation":operation_kind,"field":fold.key_field}),
                    )
                })?;
            (id.to_string(), id.to_string())
        };
        let identity_field = if operation_kind == self.entity_op() {
            "entity_id".to_string()
        } else if operation_kind == self.relation_op() {
            "relation_id".to_string()
        } else {
            self.domain
                .projection
                .fold
                .iter()
                .find(|entry| entry.operation == operation_kind)
                .map(|entry| entry.key_field.clone())
                .unwrap_or_default()
        };
        tx.execute(
            &format!("delete from {table} where origin_id=?1"),
            params![origin_id],
        )
        .map_err(self.db_error("projection_datom_delete_failed"))?;

        let sequence = event["sequence"].as_u64().unwrap_or_default();
        let metadata_subject = if operation_kind == self.relation_op() {
            origin_id.clone()
        } else {
            subject.clone()
        };
        let identity_attribute = if operation_kind == self.entity_op() {
            "narada.ledger:entity/id"
        } else if operation_kind == self.relation_op() {
            "narada.ledger:relation/id"
        } else {
            "narada.ledger:record/id"
        };
        self.write_datom(
            tx,
            table,
            &origin_id,
            &metadata_subject,
            identity_attribute,
            &Value::String(origin_id.clone()),
            sequence,
            event_id,
        )?;
        self.write_datom(
            tx,
            table,
            &origin_id,
            &metadata_subject,
            "narada.ledger:event/id",
            &Value::String(event_id.to_string()),
            sequence,
            event_id,
        )?;
        self.write_datom(
            tx,
            table,
            &origin_id,
            &metadata_subject,
            "narada.ledger:event/sequence",
            &json!(sequence),
            sequence,
            event_id,
        )?;

        if operation_kind == self.entity_op() {
            if let Some(kind) = operation.get("kind") {
                self.write_datom(
                    tx,
                    table,
                    &origin_id,
                    &metadata_subject,
                    "narada.ledger:entity/kind",
                    kind,
                    sequence,
                    event_id,
                )?;
            }
        } else if operation_kind == self.relation_op() {
            if let Some(relation_type) = operation.get("relation_type").and_then(Value::as_str) {
                self.write_datom(
                    tx,
                    table,
                    &origin_id,
                    &metadata_subject,
                    "narada.ledger:relation/type",
                    &Value::String(relation_type.to_string()),
                    sequence,
                    event_id,
                )?;
                if let Some(target) = operation.get("target_id") {
                    let attribute =
                        format!("{}:{relation_type}", self.domain.identity.schema_namespace);
                    self.write_datom(
                        tx, table, &origin_id, &subject, &attribute, target, sequence, event_id,
                    )?;
                    if let Some(inverse_type) =
                        self.domain.query.relation_inverses.get(relation_type)
                    {
                        let inverse =
                            format!("{}:{inverse_type}", self.domain.identity.schema_namespace);
                        self.write_datom(
                            tx,
                            table,
                            &origin_id,
                            target.as_str().unwrap_or_default(),
                            &inverse,
                            &Value::String(subject.clone()),
                            sequence,
                            event_id,
                        )?;
                    }
                }
            }
        } else if let Some(record_kind) = self
            .domain
            .projection
            .fold
            .iter()
            .find(|entry| entry.operation == operation_kind)
            .and_then(|entry| entry.columns.get("record_kind"))
            .and_then(Value::as_str)
        {
            self.write_datom(
                tx,
                table,
                &origin_id,
                &metadata_subject,
                "narada.ledger:record/kind",
                &Value::String(record_kind.to_string()),
                sequence,
                event_id,
            )?;
        }

        if let Some(object) = operation.as_object() {
            for (field, value) in object {
                if field == "op"
                    || field == &identity_field
                    || field == "relation_type"
                    || field == "kind"
                {
                    continue;
                }
                let attribute = format!("{}:{field}", self.domain.identity.schema_namespace);
                self.write_datom(
                    tx,
                    table,
                    &origin_id,
                    &metadata_subject,
                    &attribute,
                    value,
                    sequence,
                    event_id,
                )?;
            }
        }
        Ok(())
    }

    fn write_datom(
        &self,
        tx: &Transaction<'_>,
        table: &str,
        origin_id: &str,
        subject: &str,
        attribute: &str,
        value: &Value,
        sequence: u64,
        event_id: &str,
    ) -> Result<(), Value> {
        let value_json = value.to_string();
        let value_kind = match value {
            Value::Null => "null",
            Value::Bool(_) => "boolean",
            Value::Number(_) => "number",
            Value::String(_) => "string",
            Value::Array(_) | Value::Object(_) => "json",
        };
        let datom_id =
            sha256(format!("{origin_id}\0{subject}\0{attribute}\0{value_json}").as_bytes());
        tx.execute(
            &format!("insert or replace into {table}(datom_id,origin_id,subject,attribute,value_json,value_kind,event_sequence,event_id) values(?1,?2,?3,?4,?5,?6,?7,?8)"),
            params![datom_id, origin_id, subject, attribute, value_json, value_kind, sequence as i64, event_id],
        )
        .map_err(self.db_error("projection_datom_write_failed"))?;
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn verify_ledger(&self, root: &Path) -> Result<(), Value> {
        event_ledger::verify(self.error, &self.ledger_layout(root), self.event_hash_field)
    }

    fn validate_references(&self, root: &Path, operations: &[Value]) -> Result<(), Value> {
        let mut known = std::collections::HashSet::new();
        if self.projection_path(root).exists() {
            let db = Connection::open(self.projection_path(root))
                .map_err(self.db_error("projection_open_failed"))?;
            let entity_pk = self.table(&self.entity_table).primary_key.clone();
            let mut statement = db
                .prepare(&format!("select {} from {}", entity_pk, self.entity_table))
                .map_err(self.db_error("projection_reference_prepare_failed"))?;
            let rows = statement
                .query_map([], |row| row.get::<_, String>(0))
                .map_err(self.db_error("projection_reference_query_failed"))?;
            for row in rows {
                known.insert(row.map_err(self.db_error("projection_reference_row_failed"))?);
            }
        }
        let entity_key_field = self
            .domain
            .projection
            .fold
            .iter()
            .find(|entry| entry.table == self.entity_table)
            .map(|entry| entry.key_field.clone())
            .unwrap_or_else(|| "entity_id".to_string());
        for operation in operations {
            if operation["op"] == self.entity_op() {
                known.insert(
                    operation
                        .get(&entity_key_field)
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string(),
                );
            }
        }
        let require_known = |field: &str, operation: &Value| -> Result<(), Value> {
            let id = operation
                .get(field)
                .and_then(Value::as_str)
                .unwrap_or_default();
            if known.contains(id) {
                Ok(())
            } else {
                Err(self.error(
                    "dangling_reference",
                    "operation references an unknown entity",
                    json!({"field":field,"entity_id":id,"operation":operation}),
                ))
            }
        };
        let evidence_required_fields = self
            .domain
            .operations
            .evidence_entry
            .get("required")
            .and_then(Value::as_array)
            .and_then(|fields| {
                fields
                    .iter()
                    .map(|field| field.as_str().map(str::to_string))
                    .collect::<Option<Vec<_>>>()
            })
            .unwrap_or_default();
        for operation in operations {
            let op_kind = operation["op"].as_str().unwrap_or_default();
            if op_kind == self.domain.query.communication.canonicalization_operation {
                let entity_id = operation
                    .get("entity_id")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let legacy_kind = operation
                    .get("legacy_kind")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let evidence = operation
                    .get("equivalence_evidence")
                    .and_then(Value::as_object)
                    .ok_or_else(|| {
                        self.error(
                            "communication_canonicalization_evidence_required",
                            "canonicalization evidence is missing",
                            json!({"entity_id":entity_id}),
                        )
                    })?;
                let expected_digest = evidence
                    .get("payload_sha256")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let expected_event = evidence
                    .get("originating_event_id")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let db = Connection::open(self.projection_path(root))
                    .map_err(self.db_error("projection_open_failed"))?;
                let current = db
                    .query_row(
                        &format!(
                            "select kind,payload_json,event_id from {} where entity_id=?1",
                            self.entity_table
                        ),
                        params![entity_id],
                        |row| {
                            Ok((
                                row.get::<_, String>(0)?,
                                row.get::<_, String>(1)?,
                                row.get::<_, String>(2)?,
                            ))
                        },
                    )
                    .optional()
                    .map_err(self.db_error("communication_canonicalization_lookup_failed"))?;
                let Some((current_kind, payload_json, originating_event_id)) = current else {
                    return Err(self.error(
                        "dangling_reference",
                        "canonicalization references an unknown entity",
                        json!({"entity_id":entity_id}),
                    ));
                };
                let actual_digest = sha256(payload_json.as_bytes());
                if current_kind != legacy_kind
                    || actual_digest != expected_digest
                    || originating_event_id != expected_event
                {
                    return Err(self.error(
                        &self.domain.query.communication.collision_refusal_code,
                        "canonicalization evidence does not prove identity and payload provenance equivalence",
                        json!({"entity_id":entity_id,"expected":{"kind":legacy_kind,"payload_sha256":expected_digest,"originating_event_id":expected_event},"actual":{"kind":current_kind,"payload_sha256":actual_digest,"originating_event_id":originating_event_id}}),
                    ));
                }
            }
            for binding in &self.domain.operations.reference_bindings {
                if binding.operation == "*" {
                    for field in &binding.fields {
                        let Some((array_field, sub_field)) = field.split_once("[].") else {
                            continue;
                        };
                        for entry in operation
                            .get(array_field)
                            .and_then(Value::as_array)
                            .into_iter()
                            .flatten()
                        {
                            require_known(sub_field, entry)?;
                            for required_field in &evidence_required_fields {
                                if required_field == sub_field {
                                    continue;
                                }
                                if entry
                                    .get(required_field)
                                    .and_then(Value::as_str)
                                    .filter(|value| !value.trim().is_empty())
                                    .is_none()
                                {
                                    return Err(self.error(
                                        "evidence_location_incomplete",
                                        "evidence requires locator and paraphrase",
                                        json!({"field":required_field,"evidence":entry}),
                                    ));
                                }
                            }
                        }
                    }
                } else if binding.operation == op_kind {
                    for field in &binding.fields {
                        require_known(field, operation)?;
                    }
                }
            }
        }
        Ok(())
    }

    fn validate_operations(&self, ops: &[Value], require_evidence: bool) -> Result<(), Value> {
        for op in ops {
            let obj = op.as_object().ok_or_else(|| {
                self.error(
                    "invalid_operation",
                    "operation must be an object",
                    Value::Null,
                )
            })?;
            let kind = self.required(obj, "op")?;
            let Some(required_fields) = self.domain.operations.required_fields.get(kind.as_str())
            else {
                return Err(self.error(
                    "invalid_operation",
                    "unsupported operation",
                    json!({"op":kind}),
                ));
            };
            for field in required_fields.clone() {
                if field == "op" {
                    continue;
                }
                let value = self.required(obj, &field)?;
                if field == "kind" && kind == self.entity_op() {
                    let communication = &self.domain.query.communication;
                    if communication
                        .legacy_read_aliases
                        .iter()
                        .any(|legacy| legacy == &value)
                    {
                        return Err(self.error(
                            &communication.legacy_write_refusal_code,
                            "legacy communication kinds are read aliases and cannot authorize writes",
                            json!({"supplied_kind":value,"canonical_replacement":communication.canonical_kind,"contract_version":communication.contract_version,"remediation":"resubmit the declaration with canonical_replacement"}),
                        ));
                    }
                    let rule = &self.domain.entities.extension_rule;
                    if !self.domain.entities.core_kinds.contains(&value)
                        && !value.contains(&rule.must_contain)
                    {
                        return Err(self.error(
                            &rule.refusal_code,
                            "extension entity kinds must be namespaced",
                            json!({"kind":value,"core_entity_kinds":self.domain.entities.core_kinds,"extension_pattern":rule.pattern,"examples":rule.examples}),
                        ));
                    }
                }
                if field == "relation_type" && kind == self.relation_op() {
                    let rule = &self.domain.relations.extension_rule;
                    if !self.domain.relations.core.contains(&value)
                        && !value.contains(&rule.must_contain)
                    {
                        return Err(self.error(
                            &rule.refusal_code,
                            "extension relations must be namespaced",
                            json!({
                                "relation_type":value,
                                "core_relations":self.domain.relations.core,
                                "extension_pattern":rule.pattern,
                                "examples":rule.examples
                            }),
                        ));
                    }
                }
            }
            if kind == self.entity_op() {
                let communication = &self.domain.query.communication;
                let entity_kind = obj.get("kind").and_then(Value::as_str).unwrap_or_default();
                if communication
                    .legacy_read_aliases
                    .iter()
                    .any(|legacy| legacy == entity_kind)
                {
                    return Err(self.error(
                        &communication.legacy_write_refusal_code,
                        "legacy communication kinds are read aliases and cannot authorize writes",
                        json!({"supplied_kind":entity_kind,"canonical_replacement":communication.canonical_kind,"contract_version":communication.contract_version,"remediation":"resubmit the declaration with canonical_replacement"}),
                    ));
                }
                if entity_kind == communication.canonical_kind {
                    for field in &communication.required_fields {
                        self.required(obj, field)?;
                    }
                    if !communication.content_any_of.iter().any(|field| {
                        obj.get(field)
                            .and_then(Value::as_str)
                            .map(str::trim)
                            .map(|value| !value.is_empty())
                            .unwrap_or(false)
                    }) {
                        return Err(self.error(
                            "communication_content_required",
                            "canonical communication requires at least one declared content field",
                            json!({"canonical_kind":communication.canonical_kind,"content_any_of":communication.content_any_of}),
                        ));
                    }
                }
                for conditional in &self.domain.entities.required_fields.conditional {
                    if obj.get("kind").and_then(Value::as_str)
                        == Some(conditional.when_kind.as_str())
                    {
                        for field in &conditional.requires {
                            self.required(obj, field)?;
                        }
                    }
                }
            }
            if kind == self.domain.query.communication.canonicalization_operation {
                let communication = &self.domain.query.communication;
                let legacy_kind = obj
                    .get("legacy_kind")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let canonical_kind = obj
                    .get("canonical_kind")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                if !communication
                    .legacy_read_aliases
                    .iter()
                    .any(|legacy| legacy == legacy_kind)
                    || canonical_kind != communication.canonical_kind
                {
                    return Err(self.error(
                        "communication_canonicalization_contract_mismatch",
                        "canonicalization must use a declared legacy alias and the descriptor canonical kind",
                        json!({"legacy_kind":legacy_kind,"canonical_kind":canonical_kind,"canonical_replacement":communication.canonical_kind,"legacy_read_aliases":communication.legacy_read_aliases}),
                    ));
                }
                let evidence = obj
                    .get("equivalence_evidence")
                    .and_then(Value::as_object)
                    .ok_or_else(|| {
                        self.error(
                    "communication_canonicalization_evidence_required",
                    "canonicalization requires payload digest and originating event evidence",
                    json!({"entity_id":obj.get("entity_id")}),
                )
                    })?;
                for field in ["payload_sha256", "originating_event_id"] {
                    self.required(evidence, field)?;
                }
            }
            if require_evidence
                && self
                    .domain
                    .operations
                    .evidence_required_at_review
                    .contains(&kind)
                && obj
                    .get("evidence")
                    .and_then(Value::as_array)
                    .map(|value| value.is_empty())
                    .unwrap_or(true)
            {
                return Err(self.error(
                    "evidence_required",
                    "assessment and outcome records require evidence",
                    json!({"op":kind}),
                ));
            }
        }
        Ok(())
    }

    fn with_authority_lock<T>(
        &self,
        root: &Path,
        key: &str,
        action: impl FnOnce() -> Result<T, Value>,
    ) -> Result<T, Value> {
        lock::with_authority_lock(
            self.error,
            &self.runtime(root).join("locks"),
            key,
            lock::AuthorityLockPolicy::default(),
            action,
        )
    }

    fn validated_sequence_name(&self, args: &Map<String, Value>) -> Result<String, Value> {
        let name = self.required(args, "sequence_name")?;
        if name.trim() != name
            || name.chars().count() as u64 > self.domain.caps.sequence_name_chars.max
            || name.chars().any(char::is_control)
        {
            return Err(self.error(
                "sequence_name_invalid",
                "sequence_name must be 1-120 non-control characters without surrounding whitespace",
                json!({"sequence_name":name}),
            ));
        }
        Ok(name)
    }

    fn required_object(&self, args: &Map<String, Value>, key: &str) -> Result<Value, Value> {
        ledger_args::required_object(
            self.error,
            args,
            key,
            self.domain.caps.authority_basis_bytes,
            "authority_basis",
        )
    }

    fn optional_u64(
        &self,
        args: &Map<String, Value>,
        key: &str,
        default: u64,
    ) -> Result<u64, Value> {
        ledger_args::optional_u64(self.error, args, key, default)
    }

    fn page_limit(&self, args: &Map<String, Value>) -> Result<usize, Value> {
        ledger_args::page_limit(self.error, args)
    }

    fn page_offset(&self, args: &Map<String, Value>) -> Result<usize, Value> {
        ledger_args::page_offset(self.error, args)
    }

    fn sequence_directory(&self, root: &Path, name: &str) -> PathBuf {
        self.sequences(root).join(sha256(name.as_bytes()))
    }

    fn sequence_claims_directory(&self, root: &Path, name: &str) -> PathBuf {
        self.sequence_directory(root, name).join("claims")
    }

    fn load_sequence_manifest(&self, root: &Path, name: &str) -> Result<Value, Value> {
        let path = self.sequence_directory(root, name).join("sequence.json");
        if !path.exists() {
            return Err(self.error(
                "sequence_not_found",
                "sequence does not exist",
                json!({"sequence_name":name}),
            ));
        }
        let manifest = self.read_json(&path)?;
        self.verify_sequence_manifest(&manifest, name)?;
        Ok(manifest)
    }

    fn verify_sequence_manifest(&self, manifest: &Value, expected_name: &str) -> Result<(), Value> {
        let sequences = &self.domain.features.sequences;
        let expected_id = self.generated_sequence_id(expected_name);
        if manifest.get("schema") != Some(&json!(sequences.manifest_schema_id))
            || manifest.get("sequence_name").and_then(Value::as_str) != Some(expected_name)
            || manifest.get("sequence_id").and_then(Value::as_str) != Some(expected_id.as_str())
            || manifest
                .get("start_at")
                .and_then(Value::as_u64)
                .is_none_or(|value| value < sequences.start_at_min)
            || manifest.get("step").and_then(Value::as_u64) != Some(sequences.step)
        {
            return Err(self.error(
                "sequence_manifest_invalid",
                "sequence manifest has invalid identity or configuration",
                json!({"sequence_name":expected_name}),
            ));
        }
        let hash_field = sequences.manifest_hash_field.clone();
        let Some(recomputed) = chain::recompute_hash(self.error, manifest, &hash_field)? else {
            return Err(self.error(
                "sequence_manifest_invalid",
                "sequence manifest lacks creation_hash",
                json!({"sequence_name":expected_name}),
            ));
        };
        if recomputed.stored != recomputed.computed {
            return Err(self.error(
                "sequence_manifest_hash_invalid",
                "sequence manifest hash does not match",
                json!({"sequence_name":expected_name,"expected_hash":recomputed.computed,"actual_hash":recomputed.stored}),
            ));
        }
        Ok(())
    }

    fn verified_sequence_claims(
        &self,
        root: &Path,
        name: &str,
        manifest: &Value,
    ) -> Result<Vec<Value>, Value> {
        let sequences = &self.domain.features.sequences;
        let directory = self.sequence_claims_directory(root, name);
        if !directory.exists() {
            return Ok(Vec::new());
        }
        let mut paths = fs::read_dir(&directory)
            .map_err(self.io_error("sequence_claim_store_read_failed"))?
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| path.extension().and_then(|value| value.to_str()) == Some("json"))
            .collect::<Vec<_>>();
        paths.sort();
        let total = paths.len();
        let mut claims = Vec::with_capacity(total);
        let mut expected_value = manifest["start_at"].as_u64().unwrap();
        let mut previous_hash: Option<String> = None;
        let mut idempotency_keys = HashSet::new();
        let mut claim_ids = HashSet::new();
        for (index, path) in paths.into_iter().enumerate() {
            let claim = self.read_json(&path)?;
            let hash_field = sequences.claim_hash_field.clone();
            let Some(chain::RecomputedHash {
                stored: actual_hash,
                computed: computed_hash,
            }) = chain::recompute_hash(self.error, &claim, &hash_field)?
            else {
                return Err(self.error(
                    "sequence_claim_invalid",
                    "sequence claim lacks claim_hash",
                    json!({"path":path.to_string_lossy()}),
                ));
            };
            let idempotency_key = claim.get("idempotency_key").and_then(Value::as_str);
            let claim_id = claim.get("claim_id").and_then(Value::as_str);
            if claim.get("schema") != Some(&json!(sequences.claim_schema_id))
                || claim.get("sequence_name").and_then(Value::as_str) != Some(name)
                || claim.get("sequence_id") != manifest.get("sequence_id")
                || claim.get("value").and_then(Value::as_u64) != Some(expected_value)
                || claim
                    .get(&sequences.claim_chain_field)
                    .and_then(Value::as_str)
                    != previous_hash.as_deref()
                || claim
                    .get("request_digest")
                    .and_then(Value::as_str)
                    .is_none()
                || idempotency_key.is_none_or(str::is_empty)
                || claim_id.is_none_or(str::is_empty)
                || !idempotency_keys.insert(idempotency_key.unwrap().to_string())
                || !claim_ids.insert(claim_id.unwrap().to_string())
                || actual_hash != computed_hash
            {
                return Err(self.error(
                    "sequence_claim_chain_invalid",
                    "sequence claim chain is not contiguous and hash-valid",
                    json!({"sequence_name":name,"path":path.to_string_lossy(),"expected_value":expected_value}),
                ));
            }
            previous_hash = Some(actual_hash.to_string());
            claims.push(claim);
            if index + 1 < total {
                expected_value = expected_value.checked_add(1).ok_or_else(|| {
                    self.error(
                        "sequence_claim_chain_invalid",
                        "claim exists after u64 exhaustion",
                        json!({"sequence_name":name}),
                    )
                })?;
            }
        }
        Ok(claims)
    }

    fn find_sequence_claim_by_idempotency<'a>(claims: &'a [Value], key: &str) -> Option<&'a Value> {
        claims
            .iter()
            .find(|claim| claim.get("idempotency_key").and_then(Value::as_str) == Some(key))
    }

    fn recover_sequence_idempotency_index(
        &self,
        root: &Path,
        name: &str,
        key: &str,
        claim: &Value,
    ) -> Result<(), Value> {
        let directory = self.sequence_directory(root, name).join("idempotency");
        fs::create_dir_all(&directory)
            .map_err(self.io_error("sequence_idempotency_store_create_failed"))?;
        let path = directory.join(format!("{}.json", sha256(key.as_bytes())));
        if path.exists() {
            let existing = self.read_json(&path)?;
            if existing.get("claim_id") != claim.get("claim_id") {
                return Err(self.error(
                    "sequence_claim_idempotency_conflict",
                    "idempotency index names a different claim",
                    json!({"sequence_name":name,"idempotency_key":key,"existing_claim_id":existing.get("claim_id"),"claim_id":claim.get("claim_id")}),
                ));
            }
            return Ok(());
        }
        self.write_new_json(
            &path,
            &json!({"schema":self.domain.features.sequences.idempotency_schema_id,"idempotency_key":key,"claim_id":claim["claim_id"],"value":claim["value"]}),
        )
    }

    fn find_ledger_event_by_idempotency(
        &self,
        root: &Path,
        key: &str,
    ) -> Result<Option<Value>, Value> {
        event_ledger::find_event_by_idempotency(self.error, &self.ledger_layout(root), key)
    }

    fn prepare(&self, root: &Path) -> Result<(), Value> {
        fs::create_dir_all(self.ledger(root)).map_err(self.io_error("ledger_create_failed"))?;
        fs::create_dir_all(self.proposals(root))
            .map_err(self.io_error("proposal_store_create_failed"))?;
        fs::create_dir_all(self.runtime(root))
            .map_err(self.io_error("projection_root_create_failed"))?;
        Ok(())
    }

    /// Site control root: the site root itself when its basename is
    /// `.narada`, otherwise `<site_root>/.narada` (engine constant).
    fn control(&self, root: &Path) -> PathBuf {
        if root.file_name().and_then(|value| value.to_str()) == Some(".narada") {
            root.to_path_buf()
        } else {
            root.join(".narada")
        }
    }

    // Storage subdirs join as one '/'-separated segment so rendered paths stay
    // byte-identical to the reference implementations on every platform.
    fn ledger(&self, root: &Path) -> PathBuf {
        self.control(root).join(format!(
            "{}/{}",
            self.domain.storage.control_root_subdir, self.domain.storage.subdirs.ledger
        ))
    }

    fn proposals(&self, root: &Path) -> PathBuf {
        self.control(root).join(format!(
            "{}/{}",
            self.domain.storage.control_root_subdir, self.domain.storage.subdirs.proposals
        ))
    }

    fn sequences(&self, root: &Path) -> PathBuf {
        self.control(root).join(format!(
            "{}/{}",
            self.domain.storage.control_root_subdir, self.domain.storage.subdirs.sequences
        ))
    }

    fn runtime(&self, root: &Path) -> PathBuf {
        self.control(root).join(&self.domain.storage.runtime_subdir)
    }

    fn projection_path(&self, root: &Path) -> PathBuf {
        self.runtime(root).join("projection.sqlite")
    }

    fn ledger_layout(&self, root: &Path) -> LedgerLayout {
        LedgerLayout::new(self.ledger(root), &self.domain.storage.ledger_file_prefix)
    }

    fn ledger_files(&self, root: &Path) -> Result<Vec<PathBuf>, Value> {
        event_ledger::files(self.error, &self.ledger_layout(root))
    }

    fn ledger_head(&self, root: &Path) -> Result<Option<String>, Value> {
        event_ledger::head(
            self.error,
            &self.ledger_layout(root),
            &self.domain.storage.event_hash_field,
        )
    }

    fn load_proposal(&self, root: &Path, id: &str) -> Result<Value, Value> {
        self.read_json(&self.proposals(root).join(format!("{}.json", safe_name(id))))
    }

    fn read_json(&self, path: &Path) -> Result<Value, Value> {
        ledger_io::read_json(self.error, path)
    }

    fn write_new_json(&self, path: &Path, value: &Value) -> Result<(), Value> {
        ledger_io::write_new_json(self.error, path, value)
    }

    fn write_replace_json(&self, path: &Path, value: &Value) -> Result<(), Value> {
        ledger_io::write_replace_json(self.error, path, value)
    }

    fn write_new(&self, path: &Path, bytes: &[u8]) -> Result<(), Value> {
        ledger_io::write_new(self.error, path, bytes)
    }

    fn digest_value(&self, value: &Value) -> Result<String, Value> {
        narada_mcp_event_ledger::digest::digest_value(self.error, value)
    }

    fn required(&self, args: &Map<String, Value>, key: &str) -> Result<String, Value> {
        ledger_args::required(self.error, args, key)
    }

    fn generated_sequence_id(&self, name: &str) -> String {
        let template = &self.domain.id_derivation.generated_ids.sequence_id;
        format!(
            "{}{}",
            template_prefix(template),
            &sha256(name.as_bytes())[..template_truncation(template, 24)]
        )
    }

    fn generated_claim_id(&self, name: &str, idempotency_key: &str) -> String {
        let template = &self.domain.id_derivation.generated_ids.claim_id;
        format!(
            "{}{}",
            template_prefix(template),
            &sha256(format!("{name}\0{idempotency_key}").as_bytes())
                [..template_truncation(template, 24)]
        )
    }

    /// Render one claim file name from the descriptor's
    /// `claim_file_pattern` (for example `claims/claim-{value:020}.json`).
    /// Only the file-name portion is returned; the caller joins the claims
    /// directory.
    fn sequence_claim_file_name(&self, value: u64) -> String {
        let pattern = &self.domain.features.sequences.claim_file_pattern;
        let Some((left, right)) = pattern.split_once("{value:") else {
            return format!("claim-{value:020}.json");
        };
        let prefix = left.rsplit('/').next().unwrap_or(left);
        let Some((width_text, suffix)) = right.split_once('}') else {
            return format!("claim-{value:020}.json");
        };
        let Ok(width) = width_text.parse::<usize>() else {
            return format!("claim-{value:020}.json");
        };
        format!("{prefix}{value:0width$}{suffix}")
    }

    fn guidance(&self) -> Value {
        let mut object = Map::new();
        for key in &self.domain.guidance.emission_order {
            let value = match key.as_str() {
                "schema" => json!(self.domain.guidance.schema_id),
                "entity_kinds" => json!(self.domain.entities.core_kinds),
                "core_relations" => json!(self.domain.relations.core),
                "operation_kinds" => json!(self.domain.operations.kinds),
                "extension_relation_rule" | "extension_entity_kind_rule" => self
                    .domain
                    .guidance
                    .engine_derived_fields
                    .get(key)
                    .and_then(|entry| entry.get("text"))
                    .cloned()
                    .unwrap_or(Value::Null),
                _ => self
                    .domain
                    .guidance
                    .fields
                    .get(key)
                    .cloned()
                    .unwrap_or(Value::Null),
            };
            object.insert(key.clone(), value);
        }
        Value::Object(object)
    }

    fn guidance_with_request(&self, args: &Map<String, Value>) -> Value {
        let mut value = self.guidance();
        value["requested"] = json!({"workflow":args.get("workflow").cloned().unwrap_or(Value::Null),"tool":args.get("tool").cloned().unwrap_or(Value::Null)});
        value
    }

    fn error(&self, code: &str, message: &str, details: Value) -> Value {
        self.error.error(code, message, details)
    }

    fn io_error(&self, code: &'static str) -> impl FnOnce(std::io::Error) -> Value {
        self.error.io_error(code)
    }

    fn db_error(&self, code: &'static str) -> impl FnOnce(rusqlite::Error) -> Value {
        self.error.db_error(code)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn descriptor_path() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../shared/ledger-domain-epistemic/domain.json")
    }

    fn engine() -> Engine {
        Engine::new(Descriptor::load(&descriptor_path()).expect("epistemic descriptor"))
            .expect("engine")
    }

    #[test]
    fn storage_layout_matches_the_epistemic_control_root_convention() {
        let engine = engine();
        let root = Path::new("site");
        assert_eq!(
            engine.ledger(root),
            Path::new("site").join(".narada/epistemic/ledger")
        );
        assert_eq!(
            engine.proposals(root),
            Path::new("site").join(".narada/epistemic/proposals")
        );
        assert_eq!(
            engine.sequences(root),
            Path::new("site").join(".narada/epistemic/sequences")
        );
        assert_eq!(
            engine.runtime(root),
            Path::new("site").join(".narada/.ai/epistemic-graph")
        );
        assert_eq!(
            engine.projection_path(root),
            Path::new("site").join(".narada/.ai/epistemic-graph/projection.sqlite")
        );
        let narada = Path::new("site/.narada");
        assert_eq!(engine.ledger(narada), narada.join("epistemic/ledger"));
        assert_eq!(engine.runtime(narada), narada.join(".ai/epistemic-graph"));
    }

    #[test]
    fn proposal_tool_schema_describes_every_operation_shape() {
        let engine = engine();
        let schema = &engine
            .domain
            .tools
            .iter()
            .find(|tool| tool.name == "epistemic_graph_proposal_submit")
            .expect("proposal tool")
            .input_schema;
        let variants = schema
            .pointer("/properties/operations/items/oneOf")
            .and_then(Value::as_array)
            .expect("operation variants");
        assert_eq!(variants.len(), 5);
        assert_eq!(
            variants[0].pointer("/properties/op/const"),
            Some(&json!("entity.declare"))
        );
        assert_eq!(
            variants[1].pointer("/properties/op/const"),
            Some(&json!("relation.declare"))
        );
        assert_eq!(
            variants[2].pointer("/properties/evidence/items/required/2"),
            Some(&json!("paraphrase"))
        );
    }

    #[test]
    fn guidance_contains_copyable_end_to_end_workflow() {
        let engine = engine();
        let value = engine.guidance();
        assert_eq!(value["schema"], "narada.epistemic.guidance.v2");
        assert_eq!(
            value.pointer("/minimal_example/tool"),
            Some(&json!("epistemic_graph_submit_review_admit"))
        );
        assert_eq!(
            value.pointer("/minimal_example/arguments/operations/0/op"),
            Some(&json!("entity.declare"))
        );
        assert_eq!(
            value.pointer("/minimal_example/arguments/operations/2/op"),
            Some(&json!("relation.declare"))
        );
        assert_eq!(
            value.pointer("/payload_transport/accepted_by/1"),
            Some(&json!("epistemic_graph_submit_review_admit"))
        );
        assert_eq!(
            value.pointer("/immutable_payload_recovery/steps/1/action"),
            Some(&json!("create_successor_revision"))
        );
        assert_eq!(
            value.pointer("/communication_example/kind"),
            Some(&json!("narada.epistemic:communication"))
        );
        assert!(value["concurrency_rule"]
            .as_str()
            .unwrap_or_default()
            .contains("ledger_head"));
    }

    #[test]
    fn guidance_schema_accepts_declared_routing_hints() {
        let engine = engine();
        let tool = engine
            .list_tools()
            .into_iter()
            .find(|tool| tool["name"] == "epistemic_graph_guidance")
            .unwrap();
        assert_eq!(tool["inputSchema"]["additionalProperties"], false);
        assert_eq!(
            tool["inputSchema"]["properties"]["workflow"]["type"],
            "string"
        );
        let value = engine.guidance_with_request(
            json!({"workflow":"query_current_frontier"})
                .as_object()
                .unwrap(),
        );
        assert_eq!(value["requested"]["workflow"], "query_current_frontier");
    }

    #[test]
    fn disabled_feature_tools_are_hidden_and_refused() {
        let mut value: Value =
            serde_json::from_str(&fs::read_to_string(descriptor_path()).expect("descriptor text"))
                .expect("descriptor json");
        value["features"]["source_inspect"]["enabled"] = json!(false);
        let engine =
            Engine::new(Descriptor::from_value(value).expect("descriptor")).expect("engine");
        assert!(!engine
            .list_tools()
            .iter()
            .any(|tool| tool["name"] == "epistemic_graph_source_inspect"));
        let failure = engine
            .call_tool(
                "epistemic_graph_source_inspect",
                &Map::new(),
                Path::new("."),
            )
            .expect_err("disabled feature refuses");
        assert_eq!(failure["code"], "unknown_tool");
        assert_eq!(
            failure["message"],
            "unknown_tool:epistemic_graph_source_inspect"
        );
    }

    #[test]
    fn source_entity_requires_a_version_and_locator() {
        let engine = engine();
        let operation = json!({"op":"entity.declare","entity_id":"source:unlocated","kind":"source","title":"Unlocated source","version":"1"});
        let failure = engine
            .validate_operations(&[operation], false)
            .expect_err("unlocated source must refuse");
        assert_eq!(failure["code"], "required_argument_missing");
        assert_eq!(failure["details"]["field"], "locator");
    }

    #[test]
    fn extension_entity_kinds_must_be_namespaced() {
        let engine = engine();
        let extension = json!({"op":"entity.declare","entity_id":"exp:demo","kind":"cintamani:experiment","title":"Demo experiment","version":"1","payload":{"intent":"falsification"}});
        engine
            .validate_operations(&[extension], false)
            .expect("namespaced extension kind must validate");
        let bare = json!({"op":"entity.declare","entity_id":"exp:demo","kind":"experiment","title":"Demo experiment"});
        let failure = engine
            .validate_operations(&[bare], false)
            .expect_err("unnamespaced extension kind must refuse");
        assert_eq!(failure["code"], "invalid_entity_kind");
        assert_eq!(failure["details"]["kind"], "experiment");
    }

    #[test]
    fn communication_entities_require_bounded_provenance_fields() {
        let engine = engine();
        let complete = json!({
            "op":"entity.declare",
            "entity_id":"communication:caroline-to-benincasa-1",
            "kind":"narada.epistemic:communication",
            "title":"Flavor result handoff",
            "sender":"marici.Caroline",
            "recipient":"marici.Benincasa",
            "body":"The loop phase is chart-level.",
            "intent":"result",
            "sent_at":"2026-08-19T19:00:00Z"
        });
        engine
            .validate_operations(&[complete], false)
            .expect("complete communication must validate");

        let incomplete = json!({
            "op":"entity.declare",
            "entity_id":"communication:incomplete",
            "kind":"narada.epistemic:communication",
            "title":"Incomplete message",
            "recipient":"marici.Benincasa",
            "intent":"result",
            "sent_at":"2026-08-19T19:00:00Z"
        });
        let failure = engine
            .validate_operations(&[incomplete], false)
            .expect_err("communication without sender must refuse");
        assert_eq!(failure["code"], "required_argument_missing");
        assert_eq!(failure["details"]["field"], "sender");

        let legacy = json!({"op":"entity.declare","entity_id":"communication:legacy","kind":"communication","title":"Legacy","sender":"a","recipient":"b","intent":"result","sent_at":"2026-08-19T19:00:00Z"});
        let failure = engine
            .validate_operations(&[legacy], false)
            .expect_err("legacy write must refuse");
        assert_eq!(failure["code"], "legacy_communication_kind_write_refused");
        assert_eq!(
            failure["details"]["canonical_replacement"],
            "narada.epistemic:communication"
        );

        let guidance = engine.guidance();
        assert_eq!(
            guidance.pointer("/communication_model/entity_kind"),
            Some(&json!("narada.epistemic:communication"))
        );
        assert_eq!(
            guidance.pointer("/communication_model/rule"),
            Some(&json!("Communication records provenance and argumentative causality, but does not become epistemic evidence unless a separate reviewed promotes_to_evidence relation is admitted."))
        );
    }

    #[test]
    fn payload_backed_submit_review_admit_preserves_canonical_validation() {
        let engine = engine();
        let root =
            std::env::temp_dir().join(format!("epistemic-payload-submit-{}", Uuid::new_v4()));
        let reference = "mcp_payload:epistemic-submit-test@v1";
        let payload = json!({
            "actor":"payload-test",
            "authority_basis":{"kind":"test","summary":"Immutable payload transport fixture."},
            "operations":[{
                "op":"entity.declare",
                "entity_id":"claim:payload-backed",
                "kind":"claim",
                "title":"Payload-backed canonical admission"
            }]
        });
        let canonical = serde_json::to_vec(&canonical_json(&payload)).expect("canonical payload");
        let path = root.join(".ai/tmp/mcp-payloads/workspace/epistemic-submit-test/v1.json");
        fs::create_dir_all(path.parent().expect("payload parent")).expect("payload directory");
        fs::write(
            &path,
            serde_json::to_vec_pretty(&json!({
                "schema":"narada.mcp_payload.revision.v1",
                "ref":reference,
                "payload_id":"epistemic-submit-test",
                "revision":1,
                "payload":payload,
                "byte_size":canonical.len(),
                "sha256":sha256(&canonical)
            }))
            .expect("payload record"),
        )
        .expect("write payload");

        let resolved = engine
            .resolve_payload_arguments(
                &root,
                &Map::from_iter([("payload_ref".into(), json!(reference))]),
            )
            .expect("resolve immutable payload");
        let admitted = engine
            .submit_review_admit(&root, &resolved)
            .expect("payload-backed canonical admission");
        assert_eq!(admitted["status"], "admitted");

        let legacy_payload = json!({
            "actor":"payload-test",
            "authority_basis":{"kind":"test","summary":"Immutable legacy payload refusal fixture."},
            "operations":[{
                "op":"entity.declare","local_ref":"message","kind":"marici:communication",
                "sender":"payload-test","recipient":"payload-reviewer","title":"Legacy payload",
                "intent":"result","sent_at":"2026-08-24T16:00:00Z"
            }]
        });
        let legacy_canonical =
            serde_json::to_vec(&canonical_json(&legacy_payload)).expect("canonical legacy payload");
        fs::write(
            &path,
            serde_json::to_vec_pretty(&json!({
                "schema":"narada.mcp_payload.revision.v1","ref":reference,
                "payload_id":"epistemic-submit-test","revision":1,"payload":legacy_payload,
                "byte_size":legacy_canonical.len(),"sha256":sha256(&legacy_canonical)
            }))
            .expect("legacy payload record"),
        )
        .expect("write legacy payload");
        let failure = engine
            .call_tool(
                "epistemic_graph_submit_review_admit",
                &Map::from_iter([("payload_ref".into(), json!(reference))]),
                &root,
            )
            .expect_err("legacy kind in immutable payload must refuse with recovery");
        assert_eq!(failure["code"], "legacy_communication_kind_write_refused");
        assert_eq!(
            failure["details"]["input_transport"],
            "immutable_payload_ref"
        );
        assert_eq!(failure["details"]["payload_revision_mutable"], false);
        assert_eq!(failure["details"]["graph_mutation_committed"], false);
        assert_eq!(
            failure["details"]["recovery"]["suggested_payload_ref"],
            "mcp_payload:epistemic-submit-test@v2"
        );
        assert_eq!(
            failure["details"]["recovery"]["replace"]["entity.kind"]["to"],
            "narada.epistemic:communication"
        );
        assert_eq!(
            failure["details"]["recovery"]["payload_revision_tools"]["derive"],
            "mcp_payload_derive"
        );
        assert_eq!(
            failure["details"]["recovery"]["then_retry_with"]["tool"],
            "epistemic_graph_submit_review_admit"
        );

        let mut record: Value =
            serde_json::from_slice(&fs::read(&path).expect("read payload")).expect("payload JSON");
        record["sha256"] =
            json!("ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff");
        fs::write(
            &path,
            serde_json::to_vec_pretty(&record).expect("tampered payload"),
        )
        .expect("write tampered payload");
        let failure = engine
            .resolve_payload_arguments(
                &root,
                &Map::from_iter([("payload_ref".into(), json!(reference))]),
            )
            .expect_err("tampered payload must refuse");
        assert_eq!(failure["code"], "payload_ref_sha256_mismatch");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn canonical_communication_query_includes_namespaced_legacy_kind() {
        let engine = engine();
        let root = std::env::temp_dir().join(format!(
            "epistemic-communication-alias-test-{}",
            Uuid::new_v4()
        ));
        engine
            .rebuild_projection(&root)
            .expect("initial projection");
        event_ledger::append_event(
            engine.error,
            &engine.ledger_layout(&root),
            engine.event_hash_field,
            None,
            None,
            |ctx| json!({"schema":engine.domain.storage.event_schema_id,"sequence":ctx.sequence,"event_id":ctx.event_id,"previous_hash":ctx.previous_hash,"operations":[{"op":"entity.declare","entity_id":"communication:legacy","kind":"communication","title":"Legacy message","sender":"marici.Nima","recipient":"marici.Benincasa","body":"legacy body","intent":"reply","sent_at":"2026-08-20T00:00:00Z"}],"actor":"historical-fixture"}),
        ).expect("append historical legacy event");
        let proposal = engine
            .proposal_submit(
                &root,
                &Map::from_iter([
                    ("actor".into(), json!("tester")),
                    ("authority_basis".into(), json!({"kind":"test"})),
                    (
                        "idempotency_key".into(),
                        json!("communication-alias-proposal"),
                    ),
                    (
                        "operations".into(),
                        json!([
                            {
                                "op":"entity.declare",
                                "entity_id":"communication:canonical",
                                "kind":"narada.epistemic:communication",
                                "title":"Canonical message",
                                "sender":"marici.Nima",
                                "recipient":"marici.Benincasa",
                                "body":"canonical body",
                                "intent":"reply",
                                "sent_at":"2026-08-20T00:01:00Z"
                            }
                        ]),
                    ),
                ]),
            )
            .expect("proposal");
        engine
            .proposal_admit(
                &root,
                &Map::from_iter([
                    ("proposal_id".into(), proposal["proposal_id"].clone()),
                    ("actor".into(), json!("tester")),
                    ("authority_basis".into(), json!({"kind":"test"})),
                    (
                        "idempotency_key".into(),
                        json!("communication-alias-admission"),
                    ),
                ]),
            )
            .expect("admit");

        let canonical = engine
            .generic_query(
                &root,
                &Map::from_iter([
                    ("template".into(), json!("inbox")),
                    ("recipient".into(), json!("marici.Benincasa")),
                    ("limit".into(), json!(10)),
                ]),
            )
            .expect("canonical query");
        assert_eq!(canonical["count"], 2);
        assert!(canonical["items"]
            .as_array()
            .unwrap()
            .iter()
            .all(|item| item["kind"] == "narada.epistemic:communication"));
        assert_eq!(canonical["normalization"]["applied"], true);
        assert_eq!(canonical["normalization"]["normalized_count"], 1);

        let preflight = engine
            .communication_migration_preflight(&root, &Map::new())
            .expect("migration preflight");
        assert_eq!(preflight["census"]["by_kind"]["communication"], 1);
        assert_eq!(
            preflight["proposed_operations"].as_array().unwrap().len(),
            1
        );
        let originating_event = preflight["census"]["messages"][0]["event_id"].clone();
        let mut collision_operation = preflight["proposed_operations"][0].clone();
        collision_operation["equivalence_evidence"]["payload_sha256"] =
            json!("sha256:ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff");
        let collision_proposal = engine
            .proposal_submit(
                &root,
                &Map::from_iter([
                    ("actor".into(), json!("tester")),
                    ("authority_basis".into(), json!({"kind":"test"})),
                    ("idempotency_key".into(), json!("communication-collision")),
                    ("operations".into(), json!([collision_operation])),
                ]),
            )
            .expect("collision proposal may be staged for authoritative review");
        let collision = engine
            .proposal_admit(
                &root,
                &Map::from_iter([
                    (
                        "proposal_id".into(),
                        collision_proposal["proposal_id"].clone(),
                    ),
                    ("actor".into(), json!("tester")),
                    ("authority_basis".into(), json!({"kind":"test"})),
                    (
                        "idempotency_key".into(),
                        json!("communication-collision-admission"),
                    ),
                ]),
            )
            .expect_err("mismatched canonicalization evidence must stop at admission");
        assert_eq!(
            collision["code"],
            "communication_kind_canonicalization_collision"
        );
        let migrated = engine.communication_migrate(&root, &Map::from_iter([
            ("actor".into(), json!("operator")),
            ("authority_basis".into(), json!({"kind":"operator_direct_instruction","summary":"Canonical communication migration test."})),
        ])).expect("migration");
        assert_eq!(migrated["migrated"], 1);
        let replay = engine.communication_migrate(&root, &Map::from_iter([
            ("actor".into(), json!("operator")),
            ("authority_basis".into(), json!({"kind":"operator_direct_instruction","summary":"Canonical communication migration test."})),
        ])).expect("idempotent migration replay");
        assert_eq!(replay["migrated"], 0);
        assert_eq!(replay["status"], "complete");
        let db = Connection::open(engine.projection_path(&root)).expect("projection");
        let effective: (String, String) = db
            .query_row(
                "select kind,event_id from entities where entity_id='communication:legacy'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .expect("legacy entity");
        assert_eq!(effective.0, "narada.epistemic:communication");
        assert_eq!(json!(effective.1), originating_event);
        let after = engine
            .generic_query(
                &root,
                &Map::from_iter([
                    ("template".into(), json!("inbox")),
                    ("recipient".into(), json!("marici.Benincasa")),
                    ("limit".into(), json!(10)),
                ]),
            )
            .expect("post-migration query");
        assert_eq!(after["count"], 2);
        assert_eq!(after["normalization"]["applied"], false);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn named_query_filter_types_are_refused_instead_of_defaulted() {
        let engine = engine();
        for arguments in [
            json!({"template":"inbox","participant":"marici.Benincasa","include_body":"false"}),
            json!({"template":"inbox","participant":"marici.Benincasa","direction":false}),
            json!({"template":"inbox","participant":"marici.Benincasa","match":[]}),
            json!({"template":"inbox","participant":"marici.Benincasa","expected_ledger_head":true}),
        ] {
            let failure = engine
                .named_query(arguments.as_object().expect("query arguments"))
                .expect_err("malformed named filter must refuse");
            assert_eq!(failure["code"], "query_filter_type_invalid");
        }
    }

    #[test]
    fn named_and_legacy_kind_aliases_share_the_one_of_budget() {
        let mut engine = engine();
        engine.domain.query.max_one_of_values = Some(2);
        engine.domain.query.kind_aliases.insert(
            "communication".to_string(),
            vec![
                "marici:communication".to_string(),
                "communication.v2".to_string(),
            ],
        );

        let legacy = engine
            .expand_legacy_kind_value("communication")
            .expect_err("legacy aliases must be bounded");
        assert_eq!(legacy["code"], "query_kind_limit");

        let named = engine
            .named_query(
                json!({
                    "template":"inbox",
                    "participant":"marici.Benincasa",
                    "kinds":["communication"]
                })
                .as_object()
                .expect("named query arguments"),
            )
            .expect_err("named aliases must be bounded");
        assert_eq!(named["code"], "query_kind_limit");
    }

    #[test]
    fn source_inspection_returns_all_relevant_sections_with_line_ranges() {
        let engine = engine();
        let root = std::env::temp_dir().join(format!("epistemic-source-test-{}", Uuid::new_v4()));
        fs::create_dir_all(root.join("ledger")).expect("ledger directory");
        fs::write(
            root.join("ledger/example.md"),
            "# Example\n\n## Record\nA\n\n## Decision\nB\n\n## Subsequent Update\nC\n",
        )
        .expect("source");
        let result = engine
            .source_inspect(
                &root,
                &Map::from_iter([("paths".into(), json!(["ledger/example.md"]))]),
            )
            .expect("inspection");
        assert_eq!(result["files"][0]["title"], "Example");
        assert_eq!(result["files"][0]["section_count"], 3);
        assert_eq!(result["files"][0]["sections"][1]["start_line"], 6);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn batch_query_and_resubmission_are_bounded_and_identity_driven() {
        let engine = engine();
        let root = std::env::temp_dir().join(format!("epistemic-batch-test-{}", Uuid::new_v4()));
        let proposal = engine
            .proposal_submit(
                &root,
                &Map::from_iter([
                    ("actor".into(), json!("tester")),
                    ("authority_basis".into(), json!({"kind":"test"})),
                    ("idempotency_key".into(), json!("batch-p1")),
                    ("expected_ledger_head".into(), Value::Null),
                    ("operations".into(), json!([
                        {"op":"entity.declare","entity_id":"claim:keep","kind":"claim","title":"Keep alpha"},
                        {"op":"entity.declare","entity_id":"claim:drop","kind":"claim","title":"Drop beta"}
                    ])),
                ]),
            )
            .expect("proposal");
        let resubmitted = engine
            .proposal_resubmit(
                &root,
                &Map::from_iter([
                    ("source_proposal_id".into(), proposal["proposal_id"].clone()),
                    ("actor".into(), json!("tester")),
                    ("authority_basis".into(), json!({"kind":"test"})),
                    ("idempotency_key".into(), json!("batch-p2")),
                    ("expected_ledger_head".into(), Value::Null),
                    ("drop_operation_ids".into(), json!(["entity:claim:drop"])),
                    ("replacements".into(), json!([
                        {"op":"entity.declare","entity_id":"claim:replacement","kind":"claim","title":"Replacement beta"}
                    ])),
                ]),
            )
            .expect("resubmit");
        assert_eq!(resubmitted["operation_count"], 2);
        let page = engine
            .proposal_read(
                &root,
                &Map::from_iter([("proposal_id".into(), resubmitted["proposal_id"].clone())]),
            )
            .expect("read resubmission");
        assert_eq!(page["operations"][0]["entity_id"], "claim:keep");
        assert_eq!(page["operations"][1]["entity_id"], "claim:replacement");

        engine
            .proposal_admit(
                &root,
                &Map::from_iter([
                    ("proposal_id".into(), resubmitted["proposal_id"].clone()),
                    ("actor".into(), json!("tester")),
                    ("authority_basis".into(), json!({"kind":"test"})),
                    ("expected_ledger_head".into(), Value::Null),
                    ("idempotency_key".into(), json!("batch-a1")),
                ]),
            )
            .expect("admit");
        let result = engine
            .query_batch(
                &root,
                &Map::from_iter([
                    (
                        "queries".into(),
                        json!([{"text":"alpha"},{"text":"replacement"}]),
                    ),
                    ("limit_per_query".into(), json!(1)),
                ]),
            )
            .expect("batch query");
        assert_eq!(result["query_count"], 2);
        assert_eq!(result["results"][0]["returned"], 1);
        assert_eq!(result["results"][1]["returned"], 1);

        let hydrated = engine
            .query_batch(
                &root,
                &Map::from_iter([
                    (
                        "queries".into(),
                        json!([{
                            "query":{
                                "find":[{"pull":{"var":"?claim","fields":["entity_id","payload"]}}],
                                "where":[{"triple":{"subject":"?claim","attribute":"narada.ledger:entity/kind","object":"claim"}}],
                                "order_by":[{"term":"?claim"}],
                                "limit":1
                            }
                        }]),
                    ),
                    ("limit_per_query".into(), json!(1)),
                ]),
            )
            .expect("hydrated batch query");
        assert_eq!(hydrated["results"][0]["mode"], "datalog");
        assert_eq!(hydrated["results"][0]["query_origin"], "raw");
        assert_eq!(hydrated["results"][0]["returned"], 1);
        assert_eq!(
            hydrated["results"][0]["items"][0]["payload"]["title"],
            "Keep alpha"
        );
        assert!(hydrated["results"][0].get("result").is_none());
        assert_eq!(
            hydrated["results"][0]["result_schema"],
            "narada.epistemic.query.v2"
        );
        assert_eq!(
            hydrated["output_bytes"],
            serde_json::to_vec(&hydrated)
                .expect("batch response serializes")
                .len() as u64
        );
        let head_conflict = engine
            .query_batch(
                &root,
                &Map::from_iter([
                    ("expected_ledger_head".into(), json!("sha256:batch")),
                    (
                        "queries".into(),
                        json!([{
                            "expected_ledger_head":"sha256:item",
                            "text":"alpha"
                        }]),
                    ),
                ]),
            )
            .expect_err("batch head pin must not be overridden by an item");
        assert_eq!(head_conflict["code"], "query_expected_head_conflict");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn generic_pull_hydrates_entity_relation_and_record_bindings() {
        let engine = engine();
        let root =
            std::env::temp_dir().join(format!("epistemic-generic-pull-test-{}", Uuid::new_v4()));
        let proposal = engine
            .proposal_submit(
                &root,
                &Map::from_iter([
                    ("actor".into(), json!("tester")),
                    ("authority_basis".into(), json!({"kind":"test"})),
                    ("idempotency_key".into(), json!("generic-pull-p1")),
                    ("expected_ledger_head".into(), Value::Null),
                    ("operations".into(), json!([
                        {"op":"entity.declare","entity_id":"claim:pull","kind":"claim","title":"Pull claim"},
                        {"op":"entity.declare","entity_id":"test:pull","kind":"test","title":"Pull test"},
                        {"op":"entity.declare","entity_id":"shared:pull","kind":"claim","title":"Shared pull identity"},
                        {"op":"relation.declare","relation_id":"relation:pull","relation_type":"tests","source_id":"test:pull","target_id":"claim:pull"},
                        {"op":"relation.declare","relation_id":"shared:pull","relation_type":"tests","source_id":"test:pull","target_id":"claim:pull"},
                        {"op":"assessment.record","assessment_id":"assessment:pull","subject_id":"test:pull","judgment":"conditional","actor":"tester","reason":"Pull record","evidence":[{"source_id":"claim:pull","locator":"Current status","paraphrase":"The claim is conditional."}]}
                    ])),
                ]),
            )
            .expect("proposal");
        engine
            .proposal_admit(
                &root,
                &Map::from_iter([
                    ("proposal_id".into(), proposal["proposal_id"].clone()),
                    ("actor".into(), json!("tester")),
                    ("authority_basis".into(), json!({"kind":"test"})),
                    ("expected_ledger_head".into(), Value::Null),
                    ("idempotency_key".into(), json!("generic-pull-a1")),
                ]),
            )
            .expect("admit");

        let relation = engine
            .generic_query(
                &root,
                &Map::from_iter([(
                    "query".into(),
                    json!({
                        "find":[{"pull":{"var":"?relation","fields":["*"]}}],
                        "where":[{"triple":{"subject":"?relation","attribute":"narada.ledger:relation/id","object":"relation:pull"}}],
                        "order_by":[{"term":"?relation"}],
                        "limit":10
                    }),
                )]),
            )
            .expect("relation pull");
        assert_eq!(relation["count"], 1);
        assert_eq!(relation["items"][0]["relation_id"], "relation:pull");
        assert_eq!(relation["items"][0]["relation_type"], "tests");
        assert_eq!(relation["items"][0]["source_id"], "test:pull");
        assert!(relation["items"][0].get("*").is_none());

        let record = engine
            .generic_query(
                &root,
                &Map::from_iter([(
                    "query".into(),
                    json!({
                        "find":[{"pull":{"var":"?record","fields":["record_id","record_kind","payload"]}}],
                        "where":[{"triple":{"subject":"?record","attribute":"narada.ledger:record/id","object":"?record"}}],
                        "order_by":[{"term":"?record"}],
                        "limit":10
                    }),
                )]),
            )
            .expect("record pull");
        assert_eq!(record["count"], 1);
        assert_eq!(record["items"][0]["record_id"], "assessment:pull");
        assert_eq!(record["items"][0]["record_kind"], "assessment.record");
        assert_eq!(record["items"][0]["payload"]["judgment"], "conditional");

        let ambiguous = engine
            .generic_query(
                &root,
                &Map::from_iter([(
                    "query".into(),
                    json!({
                        "find":[{"pull":{"var":"?object","fields":["*"]}}],
                        "inputs":{"object":"shared:pull"},
                        "where":[{"triple":{"subject":{"input":"object"},"attribute":"narada.ledger:event/id","object":"?event"}}],
                        "order_by":[{"term":"?event"}],
                        "limit":10
                    }),
                )]),
            )
            .expect_err("untyped colliding pull identity must refuse");
        assert_eq!(ambiguous["code"], "query_pull_target_ambiguous");

        let typed_entity = engine
            .generic_query(
                &root,
                &Map::from_iter([(
                    "query".into(),
                    json!({
                        "find":[{"pull":{"var":"?object","target_kind":"entity","fields":["*"]}}],
                        "inputs":{"object":"shared:pull"},
                        "where":[{"triple":{"subject":{"input":"object"},"attribute":"narada.ledger:event/id","object":"?event"}}],
                        "order_by":[{"term":"?event"}],
                        "limit":10
                    }),
                )]),
            )
            .expect("typed entity pull");
        assert_eq!(typed_entity["items"][0]["entity_id"], "shared:pull");

        let typed_relation = engine
            .generic_query(
                &root,
                &Map::from_iter([(
                    "query".into(),
                    json!({
                        "find":[{"pull":{"var":"?object","target_kind":"relation","fields":["*"]}}],
                        "inputs":{"object":"shared:pull"},
                        "where":[{"triple":{"subject":{"input":"object"},"attribute":"narada.ledger:event/id","object":"?event"}}],
                        "order_by":[{"term":"?event"}],
                        "limit":10
                    }),
                )]),
            )
            .expect("typed relation pull");
        assert_eq!(typed_relation["items"][0]["relation_id"], "shared:pull");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn admitted_assessments_are_queryable_in_neighborhood_status_and_export() {
        let engine = engine();
        let root = std::env::temp_dir().join(format!("epistemic-record-test-{}", Uuid::new_v4()));
        let operations = json!([
            {"op":"entity.declare","entity_id":"source:record-test","kind":"source","title":"Record test source","version":"1","locator":"ledger/test.md"},
            {"op":"entity.declare","entity_id":"test:record-test","kind":"test","title":"Record test"},
            {"op":"assessment.record","assessment_id":"assessment:record-test","subject_id":"test:record-test","judgment":"conditional","actor":"tester","reason":"Some gates remain open.","evidence":[{"source_id":"source:record-test","locator":"Current status","paraphrase":"The source reports a conditional result."}]}
        ]);
        let proposal = engine
            .proposal_submit(
                &root,
                &Map::from_iter([
                    ("actor".into(), json!("tester")),
                    ("authority_basis".into(), json!({"kind":"test"})),
                    ("idempotency_key".into(), json!("record-p1")),
                    ("expected_ledger_head".into(), Value::Null),
                    ("operations".into(), operations),
                ]),
            )
            .expect("proposal");
        engine
            .proposal_admit(
                &root,
                &Map::from_iter([
                    ("proposal_id".into(), proposal["proposal_id"].clone()),
                    ("actor".into(), json!("tester")),
                    ("authority_basis".into(), json!({"kind":"test"})),
                    ("expected_ledger_head".into(), Value::Null),
                    ("idempotency_key".into(), json!("record-a1")),
                ]),
            )
            .expect("admit");
        let records = engine
            .query(
                &root,
                &Map::from_iter([("record_kind".into(), json!("assessment.record"))]),
            )
            .expect("record query");
        assert_eq!(records["returned"], 1);
        assert_eq!(engine.status(&root).expect("status")["record_count"], 1);
        assert_eq!(
            engine
                .neighborhood(
                    &root,
                    &Map::from_iter([("entity_id".into(), json!("test:record-test"))])
                )
                .expect("neighborhood")["records"]
                .as_array()
                .map(Vec::len),
            Some(1)
        );
        assert_eq!(
            engine.export(&root, &Map::new()).expect("export")["records"]
                .as_array()
                .map(Vec::len),
            Some(1)
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn engine_written_ledger_verifies_through_the_shared_crate() {
        let engine = engine();
        let root = std::env::temp_dir().join(format!("epistemic-shared-verify-{}", Uuid::new_v4()));
        let proposal = engine
            .proposal_submit(
                &root,
                &Map::from_iter([
                    ("actor".into(), json!("tester")),
                    ("authority_basis".into(), json!({"kind":"test"})),
                    ("idempotency_key".into(), json!("shared-p1")),
                    ("expected_ledger_head".into(), Value::Null),
                    ("operations".into(), json!([
                        {"op":"entity.declare","entity_id":"claim:shared","kind":"claim","title":"Shared verify claim"}
                    ])),
                ]),
            )
            .expect("proposal");
        engine
            .proposal_admit(
                &root,
                &Map::from_iter([
                    ("proposal_id".into(), proposal["proposal_id"].clone()),
                    ("actor".into(), json!("tester")),
                    ("authority_basis".into(), json!({"kind":"test"})),
                    ("expected_ledger_head".into(), Value::Null),
                    ("idempotency_key".into(), json!("shared-a1")),
                ]),
            )
            .expect("admit");
        narada_mcp_event_ledger::ledger::verify(
            narada_mcp_event_ledger::ErrorSchema("narada.epistemic.error.v1"),
            &narada_mcp_event_ledger::ledger::LedgerLayout::new(
                root.join(".narada/epistemic/ledger"),
                "ev",
            ),
            "event_hash",
        )
        .expect("shared crate verifies the engine-written ledger");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn graph_snapshot_pages_nodes_and_edges_under_one_ledger_head() {
        let engine = engine();
        let root = std::env::temp_dir().join(format!("epistemic-snapshot-test-{}", Uuid::new_v4()));
        let proposal = engine
            .proposal_submit(
                &root,
                &Map::from_iter([
                    ("actor".into(), json!("tester")),
                    ("authority_basis".into(), json!({"kind":"test"})),
                    ("idempotency_key".into(), json!("snapshot-p1")),
                    ("expected_ledger_head".into(), Value::Null),
                    ("operations".into(), json!([
                        {"op":"entity.declare","entity_id":"problem:snapshot","kind":"problem","title":"Snapshot problem"},
                        {"op":"entity.declare","entity_id":"claim:snapshot","kind":"claim","title":"Snapshot claim"},
                        {"op":"relation.declare","relation_id":"relation:snapshot","relation_type":"addresses","source_id":"claim:snapshot","target_id":"problem:snapshot"}
                    ])),
                ]),
            )
            .expect("proposal");
        engine
            .proposal_admit(
                &root,
                &Map::from_iter([
                    ("proposal_id".into(), proposal["proposal_id"].clone()),
                    ("actor".into(), json!("tester")),
                    ("authority_basis".into(), json!({"kind":"test"})),
                    ("expected_ledger_head".into(), Value::Null),
                    ("idempotency_key".into(), json!("snapshot-a1")),
                ]),
            )
            .expect("admit");

        let first = engine
            .snapshot(&root, &Map::from_iter([("limit".into(), json!(1))]))
            .expect("first page");
        assert_eq!(first["entity_count"], 2);
        assert_eq!(first["relation_count"], 1);
        assert_eq!(first["entities"].as_array().map(Vec::len), Some(1));
        assert_eq!(first["relations"].as_array().map(Vec::len), Some(1));
        assert_eq!(first["next_entity_offset"], 1);
        assert!(first["next_relation_offset"].is_null());

        let second = engine
            .snapshot(
                &root,
                &Map::from_iter([
                    ("limit".into(), json!(1)),
                    ("entity_offset".into(), json!(1)),
                    ("relation_offset".into(), json!(1)),
                    ("expected_ledger_head".into(), first["ledger_head"].clone()),
                ]),
            )
            .expect("second page");
        assert_eq!(second["entities"].as_array().map(Vec::len), Some(1));
        assert!(second["next_entity_offset"].is_null());
        assert!(second["relations"].as_array().is_some_and(Vec::is_empty));

        let mismatch = engine
            .snapshot(
                &root,
                &Map::from_iter([("expected_ledger_head".into(), json!("sha256:stale"))]),
            )
            .expect_err("stale snapshot");
        assert_eq!(mismatch["code"], "ledger_head_mismatch");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn proposal_submission_is_compact_and_explicit_reads_are_bounded() {
        let engine = engine();
        let root =
            std::env::temp_dir().join(format!("epistemic-proposal-read-test-{}", Uuid::new_v4()));
        let operations = (0..engine.max_operations())
            .map(|index| json!({"op":"entity.declare","entity_id":format!("claim:{index}"),"kind":"claim","title":format!("Claim {index}")}))
            .collect::<Vec<_>>();
        let receipt = engine
            .proposal_submit(
                &root,
                &Map::from_iter([
                    ("actor".into(), json!("tester")),
                    ("authority_basis".into(), json!({"kind":"test"})),
                    ("idempotency_key".into(), json!("compact-p1")),
                    ("expected_ledger_head".into(), Value::Null),
                    ("operations".into(), json!(operations)),
                ]),
            )
            .expect("proposal");
        assert_eq!(receipt["operation_count"], engine.max_operations());
        assert!(receipt.get("operations").is_none());
        assert!(
            serde_json::to_vec(&receipt)
                .expect("serialize receipt")
                .len()
                < 1024
        );

        let first = engine
            .proposal_read(
                &root,
                &Map::from_iter([
                    ("proposal_id".into(), receipt["proposal_id"].clone()),
                    ("limit".into(), json!(7)),
                ]),
            )
            .expect("first page");
        assert_eq!(first["returned"], 7);
        assert_eq!(first["offset"], 0);
        assert_eq!(first["next_offset"], 7);
        assert_eq!(first["bounded"], true);

        let final_page = engine
            .proposal_read(
                &root,
                &Map::from_iter([
                    ("proposal_id".into(), receipt["proposal_id"].clone()),
                    ("offset".into(), json!(195)),
                    ("limit".into(), json!(100)),
                ]),
            )
            .expect("final page");
        assert_eq!(final_page["returned"], 5);
        assert_eq!(final_page["has_more"], false);
        assert!(final_page["next_offset"].is_null());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn proposal_admission_rebuilds_projection_and_preserves_truth_boundary() {
        let engine = engine();
        let root = std::env::temp_dir().join(format!("epistemic-test-{}", Uuid::new_v4()));
        let proposal=engine.proposal_submit(&root,&Map::from_iter([("actor".into(),json!("nima")),("authority_basis".into(),json!({"kind":"operator_request"})),("operations".into(),json!([{"op":"entity.declare","entity_id":"problem-1","kind":"problem","title":"What explains X?"}]))])).unwrap();
        assert_eq!(
            proposal["schema"],
            "narada.epistemic.proposal_submission.v1"
        );
        assert_eq!(proposal["operation_count"], 1);
        assert!(proposal.get("operations").is_none());
        let id = proposal["proposal_id"].as_str().unwrap();
        let event = engine
            .proposal_admit(
                &root,
                &Map::from_iter([
                    ("proposal_id".into(), json!(id)),
                    ("actor".into(), json!("nima")),
                    ("authority_basis".into(), json!({"kind":"operator_request"})),
                ]),
            )
            .unwrap();
        assert_eq!(event["schema"], "narada.epistemic.proposal_admission.v1");
        assert_eq!(event["status"], "admitted");
        assert_eq!(event["operation_count"], 1);
        assert!(event.get("operations").is_none());
        assert_eq!(event["ledger_head"].as_str().map(str::len), Some(64));
        assert_eq!(event["certifies_truth"], false);
        let retry = engine
            .proposal_admit(
                &root,
                &Map::from_iter([
                    ("proposal_id".into(), json!(id)),
                    ("actor".into(), json!("nima")),
                    ("authority_basis".into(), json!({"kind":"operator_request"})),
                ]),
            )
            .expect("deterministic admission retry");
        assert_eq!(retry["event_id"], event["event_id"]);
        let admitted = engine
            .proposal_read(&root, &Map::from_iter([("proposal_id".into(), json!(id))]))
            .expect("admitted proposal readback");
        assert_eq!(admitted["status"], "admitted");
        assert_eq!(admitted["lifecycle"]["event_id"], event["event_id"]);
        assert_eq!(admitted["lifecycle"]["ledger_head"], event["ledger_head"]);
        let result = engine.query(&root, &Map::new()).unwrap();
        assert_eq!(result["returned"], 1);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn source_capture_builds_a_compact_deduplicated_draft_without_admitting_it() {
        let engine = engine();
        let root = std::env::temp_dir().join(format!("epistemic-capture-test-{}", Uuid::new_v4()));
        let seed = engine.proposal_submit(
            &root,
            &Map::from_iter([
                ("actor".into(), json!("tester")),
                ("authority_basis".into(), json!({"kind":"test"})),
                ("idempotency_key".into(), json!("seed-p1")),
                ("expected_ledger_head".into(), Value::Null),
                ("operations".into(), json!([{"op":"entity.declare","entity_id":"claim:existing","kind":"claim","title":"Existing claim"}])),
            ]),
        ).expect("seed proposal");
        let seed_event = engine
            .proposal_admit(
                &root,
                &Map::from_iter([
                    ("proposal_id".into(), seed["proposal_id"].clone()),
                    ("actor".into(), json!("tester")),
                    ("authority_basis".into(), json!({"kind":"test"})),
                    ("expected_ledger_head".into(), Value::Null),
                    ("idempotency_key".into(), json!("seed-a1")),
                ]),
            )
            .expect("seed admission");
        let capture = engine.capture_sources(
            &root,
            &Map::from_iter([
                ("actor".into(), json!("tester")),
                ("authority_basis".into(), json!({"kind":"test"})),
                ("idempotency_key".into(), json!("capture-p1")),
                ("expected_ledger_head".into(), seed_event["ledger_head"].clone()),
                ("sources".into(), json!([{"source_id":"source:ledger-1","title":"Ledger one","version":"1","locator":"src/ledger/1.md"}])),
                ("operations".into(), json!([
                    {"op":"entity.declare","entity_id":"claim:existing","kind":"claim","title":"Existing claim"},
                    {"op":"relation.declare","relation_id":"rel:existing-source","relation_type":"derived_from","source_id":"claim:existing","target_id":"source:ledger-1"}
                ])),
            ]),
        ).expect("source capture");
        assert_eq!(capture["status"], "draft_submitted");
        assert_eq!(capture["source_count"], 1);
        assert_eq!(capture["operation_count"], 3);
        assert_eq!(capture["existing_identity_count"], 1);
        assert_eq!(
            capture["existing_identities"][0]["identity"],
            "claim:existing"
        );
        assert_eq!(capture["admission_requires_explicit_call"], true);
        assert_eq!(engine.ledger_files(&root).expect("ledger").len(), 1);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn claim_entities_and_compact_queries_preserve_epistemic_attribution() {
        let engine = engine();
        let root = std::env::temp_dir().join(format!("epistemic-claim-test-{}", Uuid::new_v4()));
        let proposal = engine.proposal_submit(
            &root,
            &Map::from_iter([
                ("actor".into(), json!("tester")),
                ("authority_basis".into(), json!({"kind":"test"})),
                ("idempotency_key".into(), json!("claim-p1")),
                ("expected_ledger_head".into(), Value::Null),
                ("operations".into(), json!([{"op":"entity.declare","entity_id":"claim:tree-result","kind":"claim","title":"Attributed theorem result"}])),
            ]),
        ).expect("claim proposal");
        engine
            .proposal_admit(
                &root,
                &Map::from_iter([
                    ("proposal_id".into(), proposal["proposal_id"].clone()),
                    ("actor".into(), json!("tester")),
                    ("authority_basis".into(), json!({"kind":"test"})),
                    ("expected_ledger_head".into(), Value::Null),
                    ("idempotency_key".into(), json!("claim-a1")),
                ]),
            )
            .expect("claim admission");
        let result = engine
            .query(&root, &Map::from_iter([("compact".into(), json!(true))]))
            .expect("compact query");
        assert_eq!(result["compact"], true);
        assert_eq!(result["items"][0]["entity_id"], "claim:tree-result");
        assert_eq!(result["items"][0]["title"], "Attributed theorem result");
        assert!(result["items"][0].get("payload").is_none());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn projection_refuses_a_tampered_authority_event() {
        let engine = engine();
        let root = std::env::temp_dir().join(format!("epistemic-test-{}", Uuid::new_v4()));
        let proposal = engine
            .proposal_submit(
                &root,
                &Map::from_iter([
                    ("actor".into(), json!("nima")),
                    ("authority_basis".into(), json!({"kind":"test"})),
                    ("idempotency_key".into(), json!("p1")),
                    ("expected_ledger_head".into(), Value::Null),
                    ("operations".into(), json!([{"op":"entity.declare","entity_id":"problem-1","kind":"problem","title":"What explains X?"}])),
                ]),
            )
            .unwrap();
        let id = proposal["proposal_id"].as_str().unwrap();
        engine
            .proposal_admit(
                &root,
                &Map::from_iter([
                    ("proposal_id".into(), json!(id)),
                    ("actor".into(), json!("nima")),
                    ("authority_basis".into(), json!({"kind":"test"})),
                    ("expected_ledger_head".into(), Value::Null),
                    ("idempotency_key".into(), json!("a1")),
                ]),
            )
            .unwrap();
        let path = engine.ledger_files(&root).unwrap().remove(0);
        let mut event = engine.read_json(&path).unwrap();
        event["actor"] = json!("tampered");
        fs::write(&path, serde_json::to_vec_pretty(&event).unwrap()).unwrap();
        let failure = engine.rebuild_projection(&root).unwrap_err();
        assert_eq!(failure["code"], "ledger_hash_invalid");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn pure_source_capture_needs_no_placeholder_operation() {
        let engine = engine();
        let root = std::env::temp_dir().join(format!("epistemic-source-only-{}", Uuid::new_v4()));
        let result = engine
            .capture_sources(
                &root,
                &Map::from_iter([
                    ("actor".into(), json!("tester")),
                    ("authority_basis".into(), json!({"kind":"test"})),
                    ("sources".into(), json!([{"source_id":"source:only","title":"Only source","version":"1","locator":"ledger/only.md"}])),
                ]),
            )
            .expect("source-only capture");
        assert_eq!(result["source_count"], 1);
        assert_eq!(result["operation_count"], 1);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn compound_workflow_derives_relation_and_retry_identities() {
        let engine = engine();
        let root = std::env::temp_dir().join(format!("epistemic-compound-{}", Uuid::new_v4()));
        let args = Map::from_iter([
            ("actor".into(), json!("tester")),
            ("authority_basis".into(), json!({"kind":"test"})),
            (
                "operations".into(),
                json!([
                    {"op":"entity.declare","local_ref":"claim","kind":"claim","title":"A"},
                    {"op":"entity.declare","local_ref":"source","kind":"source","title":"Source A","version":"1","locator":"ledger/a.md"},
                    {"op":"relation.declare","relation_type":"derived_from","source_ref":"claim","target_ref":"source"}
                ]),
            ),
        ]);
        let first = engine
            .submit_review_admit(&root, &args)
            .expect("compound admission");
        assert_eq!(first["review"]["status"], "policy_valid");
        assert_eq!(first["admission"]["status"], "admitted");
        let proposal = engine
            .load_proposal(&root, first["submission"]["proposal_id"].as_str().unwrap())
            .unwrap();
        assert!(proposal["operations"][0]["entity_id"]
            .as_str()
            .unwrap()
            .starts_with("claim:"));
        assert!(proposal["operations"][1]["entity_id"]
            .as_str()
            .unwrap()
            .starts_with("source:"));
        assert_eq!(
            proposal["operations"][2]["source_id"],
            proposal["operations"][0]["entity_id"]
        );
        assert_eq!(
            proposal["operations"][2]["target_id"],
            proposal["operations"][1]["entity_id"]
        );
        assert!(proposal["operations"][2]["relation_id"]
            .as_str()
            .unwrap()
            .starts_with("rel:derived_from-"));
        let retried = engine
            .submit_review_admit(&root, &args)
            .expect("idempotent compound retry");
        assert_eq!(
            retried["submission"]["proposal_id"],
            first["submission"]["proposal_id"]
        );
        assert_eq!(
            retried["admission"]["event_id"],
            first["admission"]["event_id"]
        );
        let _ = fs::remove_dir_all(root);
    }

    fn sequence_test_create(engine: &Engine, root: &Path, name: &str, start_at: u64) -> Value {
        engine
            .sequence_create(
                root,
                &Map::from_iter([
                    ("sequence_name".into(), json!(name)),
                    ("actor".into(), json!("tester")),
                    ("authority_basis".into(), json!({"kind":"test"})),
                    ("start_at".into(), json!(start_at)),
                ]),
            )
            .expect("create sequence")
    }

    fn sequence_test_claim(
        engine: &Engine,
        root: &Path,
        name: &str,
        key: &str,
    ) -> Result<Value, Value> {
        engine.sequence_claim_next(
            root,
            &Map::from_iter([
                ("sequence_name".into(), json!(name)),
                ("actor".into(), json!("tester")),
                ("authority_basis".into(), json!({"kind":"test"})),
                ("idempotency_key".into(), json!(key)),
            ]),
        )
    }

    #[test]
    fn sequences_create_claim_replay_and_page_immutable_history() {
        let engine = engine();
        let root = std::env::temp_dir().join(format!("epistemic-sequence-{}", Uuid::new_v4()));
        let created = sequence_test_create(&engine, &root, "ledger-entry", 40);
        assert_eq!(created["status"], "created");
        assert_eq!(created["next_value"], 40);
        let first =
            sequence_test_claim(&engine, &root, "ledger-entry", "entry-a").expect("first claim");
        let second =
            sequence_test_claim(&engine, &root, "ledger-entry", "entry-b").expect("second claim");
        let replay =
            sequence_test_claim(&engine, &root, "ledger-entry", "entry-a").expect("claim replay");
        assert_eq!(first["value"], 40);
        assert_eq!(second["value"], 41);
        assert_eq!(replay["value"], 40);
        assert_eq!(replay["idempotency_replay"], true);
        let status = engine
            .sequence_status(
                &root,
                &Map::from_iter([("sequence_name".into(), json!("ledger-entry"))]),
            )
            .expect("status");
        assert_eq!(status["claim_count"], 2);
        assert_eq!(status["next_value"], 42);
        let page = engine
            .sequence_claims(
                &root,
                &Map::from_iter([
                    ("sequence_name".into(), json!("ledger-entry")),
                    ("limit".into(), json!(1)),
                ]),
            )
            .expect("claims page");
        assert_eq!(page["count"], 1);
        assert_eq!(page["has_more"], true);
        let listed = engine
            .sequence_list(&root, &Map::new())
            .expect("sequence list");
        assert_eq!(listed["items"][0]["sequence_name"], "ledger-entry");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn sequence_claim_idempotency_is_recovered_from_canonical_history() {
        let engine = engine();
        let root =
            std::env::temp_dir().join(format!("epistemic-sequence-recovery-{}", Uuid::new_v4()));
        sequence_test_create(&engine, &root, "research-item", 1);
        let first =
            sequence_test_claim(&engine, &root, "research-item", "research-a").expect("claim");
        fs::remove_file(
            engine
                .sequence_directory(&root, "research-item")
                .join("idempotency")
                .join(format!("{}.json", sha256(b"research-a"))),
        )
        .expect("remove disposable index");
        let replay = sequence_test_claim(&engine, &root, "research-item", "research-a")
            .expect("recover replay");
        assert_eq!(replay["claim_id"], first["claim_id"]);
        assert_eq!(replay["idempotency_replay"], true);
        assert!(engine
            .sequence_directory(&root, "research-item")
            .join("idempotency")
            .join(format!("{}.json", sha256(b"research-a")))
            .exists());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn concurrent_sequence_claims_are_unique_and_contiguous() {
        let engine = std::sync::Arc::new(engine());
        let root =
            std::env::temp_dir().join(format!("epistemic-sequence-concurrent-{}", Uuid::new_v4()));
        sequence_test_create(&engine, &root, "parallel", 1);
        let root = std::sync::Arc::new(root);
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(12));
        let handles = (0..12)
            .map(|index| {
                let engine = engine.clone();
                let root = root.clone();
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    barrier.wait();
                    sequence_test_claim(&engine, &root, "parallel", &format!("parallel-{index}"))
                        .expect("parallel claim")["value"]
                        .as_u64()
                        .unwrap()
                })
            })
            .collect::<Vec<_>>();
        let mut values = handles
            .into_iter()
            .map(|handle| handle.join().unwrap())
            .collect::<Vec<_>>();
        values.sort_unstable();
        assert_eq!(values, (1..=12).collect::<Vec<_>>());
        assert_eq!(
            engine
                .sequence_status(
                    &root,
                    &Map::from_iter([("sequence_name".into(), json!("parallel"))])
                )
                .unwrap()["integrity_status"],
            "valid"
        );
        let _ = fs::remove_dir_all(root.as_path());
    }

    #[test]
    fn sequence_refuses_reconfiguration_conflicting_replay_and_tampering() {
        let engine = engine();
        let root =
            std::env::temp_dir().join(format!("epistemic-sequence-invalid-{}", Uuid::new_v4()));
        sequence_test_create(&engine, &root, "audit", 5);
        let conflict = engine
            .sequence_create(
                &root,
                &Map::from_iter([
                    ("sequence_name".into(), json!("audit")),
                    ("actor".into(), json!("tester")),
                    ("authority_basis".into(), json!({"kind":"test"})),
                    ("start_at".into(), json!(6)),
                ]),
            )
            .expect_err("configuration conflict");
        assert_eq!(conflict["code"], "sequence_configuration_conflict");
        sequence_test_claim(&engine, &root, "audit", "same-key").expect("claim");
        let replay_conflict = engine
            .sequence_claim_next(
                &root,
                &Map::from_iter([
                    ("sequence_name".into(), json!("audit")),
                    ("actor".into(), json!("other")),
                    ("authority_basis".into(), json!({"kind":"test"})),
                    ("idempotency_key".into(), json!("same-key")),
                ]),
            )
            .expect_err("replay conflict");
        assert_eq!(
            replay_conflict["code"],
            "sequence_claim_idempotency_conflict"
        );
        let claim_path = engine
            .sequence_claims_directory(&root, "audit")
            .join("claim-00000000000000000005.json");
        let mut claim = engine.read_json(&claim_path).unwrap();
        claim["actor"] = json!("tampered");
        fs::write(&claim_path, serde_json::to_vec_pretty(&claim).unwrap()).unwrap();
        let corrupt = engine
            .sequence_status(
                &root,
                &Map::from_iter([("sequence_name".into(), json!("audit"))]),
            )
            .expect_err("tampered claim");
        assert_eq!(corrupt["code"], "sequence_claim_chain_invalid");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn sequence_refuses_invalid_names_and_reports_exhaustion() {
        let engine = engine();
        let root =
            std::env::temp_dir().join(format!("epistemic-sequence-exhausted-{}", Uuid::new_v4()));
        let invalid = engine
            .sequence_create(
                &root,
                &Map::from_iter([
                    ("sequence_name".into(), json!(" bad ")),
                    ("actor".into(), json!("tester")),
                    ("authority_basis".into(), json!({"kind":"test"})),
                ]),
            )
            .expect_err("invalid name");
        assert_eq!(invalid["code"], "sequence_name_invalid");
        sequence_test_create(&engine, &root, "finite", u64::MAX);
        let final_claim =
            sequence_test_claim(&engine, &root, "finite", "last").expect("last claim");
        assert_eq!(final_claim["value"], u64::MAX);
        assert_eq!(final_claim["exhausted"], true);
        let exhausted = sequence_test_claim(&engine, &root, "finite", "past-end")
            .expect_err("sequence exhausted");
        assert_eq!(exhausted["code"], "sequence_exhausted");
        let status = engine
            .sequence_status(
                &root,
                &Map::from_iter([("sequence_name".into(), json!("finite"))]),
            )
            .expect("exhausted status");
        assert_eq!(status["next_value"], Value::Null);
        assert_eq!(status["exhausted"], true);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn ledger_admission_lock_serializes_writers_and_recovers_idempotency_index() {
        let engine = std::sync::Arc::new(engine());
        let root = std::env::temp_dir().join(format!("epistemic-ledger-lock-{}", Uuid::new_v4()));
        let proposals = (0..2)
            .map(|index| engine.proposal_submit(&root, &Map::from_iter([("actor".into(), json!("tester")), ("authority_basis".into(), json!({"kind":"test"})), ("idempotency_key".into(), json!(format!("proposal-{index}"))), ("expected_ledger_head".into(), Value::Null), ("operations".into(), json!([{"op":"entity.declare","entity_id":format!("claim:lock-{index}"),"kind":"claim","title":format!("Lock {index}")}]))])).expect("proposal"))
            .collect::<Vec<_>>();
        let root = std::sync::Arc::new(root);
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
        let handles = proposals
            .into_iter()
            .enumerate()
            .map(|(index, proposal)| {
                let engine = engine.clone();
                let root = root.clone();
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    barrier.wait();
                    engine.proposal_admit(
                        &root,
                        &Map::from_iter([
                            ("proposal_id".into(), proposal["proposal_id"].clone()),
                            ("actor".into(), json!("tester")),
                            ("authority_basis".into(), json!({"kind":"test"})),
                            ("expected_ledger_head".into(), Value::Null),
                            (
                                "idempotency_key".into(),
                                json!(format!("admission-{index}")),
                            ),
                        ]),
                    )
                })
            })
            .collect::<Vec<_>>();
        let results = handles
            .into_iter()
            .map(|handle| handle.join().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
        assert_eq!(
            results
                .iter()
                .filter(|result| {
                    result.as_ref().is_err_and(|failure| {
                        failure["code"] == "ledger_head_conflict"
                            || failure["code"] == "proposal_not_admissible"
                    })
                })
                .count(),
            1
        );
        engine.verify_ledger(&root).expect("serialized ledger");
        assert_eq!(engine.ledger_files(&root).unwrap().len(), 1);
        let admitted = results.into_iter().find_map(Result::ok).unwrap();
        let event = engine
            .read_json(
                &engine
                    .ledger(&root)
                    .join(format!("{}.json", admitted["event_id"].as_str().unwrap())),
            )
            .unwrap();
        let key = event["idempotency_key"].as_str().unwrap();
        fs::remove_file(
            engine
                .ledger(&root)
                .join(format!("idem-{}.txt", safe_name(key))),
        )
        .expect("remove disposable ledger index");
        let replay = engine
            .proposal_admit(
                &root,
                &Map::from_iter([
                    ("proposal_id".into(), event["proposal_id"].clone()),
                    ("actor".into(), json!("tester")),
                    ("authority_basis".into(), json!({"kind":"test"})),
                    ("idempotency_key".into(), json!(key)),
                ]),
            )
            .expect("recover ledger replay");
        assert_eq!(replay["event_id"], admitted["event_id"]);
        assert_eq!(engine.ledger_files(&root).unwrap().len(), 1);
        let _ = fs::remove_dir_all(root.as_path());
    }

    fn fixture_root() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/epistemic-ledger")
    }

    fn copy_directory(source: &Path, target: &Path) {
        fs::create_dir_all(target).expect("create copy target");
        for entry in fs::read_dir(source).expect("read copy source") {
            let entry = entry.expect("copy entry");
            let destination = target.join(entry.file_name());
            if entry.file_type().expect("copy entry type").is_dir() {
                copy_directory(&entry.path(), &destination);
            } else {
                fs::copy(entry.path(), &destination).expect("copy file");
            }
        }
    }

    #[test]
    #[ignore = "rewrites the golden fixture on disk; run explicitly with --ignored"]
    fn regenerate_golden_fixture() {
        let engine = engine();
        let fixture = fixture_root();
        let _ = fs::remove_dir_all(&fixture);
        let root = std::env::temp_dir().join(format!("epistemic-fixture-gen-{}", Uuid::new_v4()));
        let admit = |operations: Value,
                     proposal_key: &str,
                     admission_key: &str,
                     expected_head: Value|
         -> Value {
            let proposal = engine
                .proposal_submit(
                    &root,
                    &Map::from_iter([
                        ("actor".into(), json!("fixture")),
                        (
                            "authority_basis".into(),
                            json!({"kind":"fixture","summary":"Golden event-ledger fixture."}),
                        ),
                        ("idempotency_key".into(), json!(proposal_key)),
                        ("expected_ledger_head".into(), expected_head),
                        ("operations".into(), operations),
                    ]),
                )
                .expect("fixture proposal");
            engine
                .proposal_admit(
                    &root,
                    &Map::from_iter([
                        ("proposal_id".into(), proposal["proposal_id"].clone()),
                        ("actor".into(), json!("fixture")),
                        (
                            "authority_basis".into(),
                            json!({"kind":"fixture","summary":"Golden event-ledger fixture."}),
                        ),
                        (
                            "expected_ledger_head".into(),
                            proposal["expected_ledger_head"].clone(),
                        ),
                        ("idempotency_key".into(), json!(admission_key)),
                    ]),
                )
                .expect("fixture admission")
        };
        let first = admit(
            json!([
                {"op":"entity.declare","entity_id":"problem:fixture","kind":"problem","title":"Fixture problem"},
                {"op":"entity.declare","entity_id":"source:fixture","kind":"source","title":"Fixture source","version":"1","locator":"docs/fixture.md"}
            ]),
            "fixture-p1",
            "fixture-a1",
            Value::Null,
        );
        let second = admit(
            json!([
                {"op":"entity.declare","entity_id":"claim:fixture","kind":"claim","title":"Fixture claim"},
                {"op":"relation.declare","relation_id":"rel:fixture-addresses","relation_type":"addresses","source_id":"claim:fixture","target_id":"problem:fixture"}
            ]),
            "fixture-p2",
            "fixture-a2",
            first["ledger_head"].clone(),
        );
        let third = admit(
            json!([
                {"op":"assessment.record","assessment_id":"assessment:fixture","subject_id":"claim:fixture","judgment":"supported","actor":"fixture","reason":"Fixture assessment.","evidence":[{"source_id":"source:fixture","locator":"docs/fixture.md","paraphrase":"The fixture source supports the claim."}]}
            ]),
            "fixture-p3",
            "fixture-a3",
            second["ledger_head"].clone(),
        );
        engine
            .sequence_create(
                &root,
                &Map::from_iter([
                    ("sequence_name".into(), json!("fixture-ledger-entry")),
                    ("actor".into(), json!("fixture")),
                    ("authority_basis".into(), json!({"kind":"fixture"})),
                    ("start_at".into(), json!(40)),
                ]),
            )
            .expect("fixture sequence");
        for key in ["fixture-c1", "fixture-c2"] {
            engine
                .sequence_claim_next(
                    &root,
                    &Map::from_iter([
                        ("sequence_name".into(), json!("fixture-ledger-entry")),
                        ("actor".into(), json!("fixture")),
                        ("authority_basis".into(), json!({"kind":"fixture"})),
                        ("idempotency_key".into(), json!(key)),
                    ]),
                )
                .expect("fixture claim");
        }
        let head = engine
            .ledger_head(&root)
            .expect("fixture head")
            .expect("non-empty fixture ledger");
        let mut event_ids = Vec::new();
        let mut event_hashes = Vec::new();
        for path in engine.ledger_files(&root).expect("fixture ledger files") {
            let event = engine.read_json(&path).expect("fixture event");
            event_ids.push(event["event_id"].clone());
            event_hashes.push(event["event_hash"].clone());
        }
        let manifest = engine
            .load_sequence_manifest(&root, "fixture-ledger-entry")
            .expect("manifest");
        let claims = engine
            .verified_sequence_claims(&root, "fixture-ledger-entry", &manifest)
            .expect("claims");
        let expected = json!({
            "schema":"narada.epistemic.golden-fixture.v1",
            "ledger_head":head,
            "event_ids":event_ids,
            "event_hashes":event_hashes,
            "replay":{"proposal_id":second["proposal_id"],"idempotency_key":"fixture-a2","event_id":second["event_id"]},
            "scan":{"idempotency_key":"fixture-a3","event_id":third["event_id"]},
            "sequence":{
                "name":"fixture-ledger-entry",
                "sequence_id":manifest["sequence_id"],
                "creation_hash":manifest["creation_hash"],
                "claim_ids":claims.iter().map(|claim| claim["claim_id"].clone()).collect::<Vec<_>>(),
                "claim_hashes":claims.iter().map(|claim| claim["claim_hash"].clone()).collect::<Vec<_>>(),
                "values":claims.iter().map(|claim| claim["value"].clone()).collect::<Vec<_>>()
            }
        });
        fs::create_dir_all(&fixture).expect("fixture directory");
        for (name, directory) in [
            ("ledger", engine.ledger(&root)),
            ("proposals", engine.proposals(&root)),
            ("sequences", engine.sequences(&root)),
        ] {
            copy_directory(&directory, &fixture.join(name));
        }
        fs::write(
            fixture.join("expected.json"),
            format!("{}\n", serde_json::to_string_pretty(&expected).unwrap()),
        )
        .expect("write expected fixture metadata");
        println!(
            "digest golden vector: {}",
            engine
                .digest_value(
                    &json!({"alpha":1,"beta":"x","gamma":[1,2],"nested":{"z":true,"a":null}})
                )
                .unwrap()
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn golden_fixture_verifies_identically() {
        let engine = engine();
        let fixture = fixture_root();
        let expected = engine
            .read_json(&fixture.join("expected.json"))
            .expect("fixture metadata");
        let root =
            std::env::temp_dir().join(format!("epistemic-fixture-verify-{}", Uuid::new_v4()));
        for (name, directory) in [
            ("ledger", engine.ledger(&root)),
            ("proposals", engine.proposals(&root)),
            ("sequences", engine.sequences(&root)),
        ] {
            copy_directory(&fixture.join(name), &directory);
        }
        engine
            .verify_ledger(&root)
            .expect("fixture ledger chain verifies");
        assert_eq!(
            engine.ledger_head(&root).expect("fixture head").as_deref(),
            expected["ledger_head"].as_str()
        );
        let files = engine.ledger_files(&root).expect("fixture ledger files");
        assert_eq!(files.len(), expected["event_ids"].as_array().unwrap().len());
        for (index, path) in files.iter().enumerate() {
            let event = engine.read_json(path).expect("fixture event");
            assert_eq!(event["event_id"], expected["event_ids"][index]);
            assert_eq!(event["event_hash"], expected["event_hashes"][index]);
            assert_eq!(event["sequence"], (index + 1) as u64);
        }
        let scanned = engine
            .find_ledger_event_by_idempotency(
                &root,
                expected["scan"]["idempotency_key"].as_str().unwrap(),
            )
            .expect("idempotency scan")
            .expect("fixture event recovered by scan");
        assert_eq!(scanned["event_id"], expected["scan"]["event_id"]);
        let replay = engine
            .proposal_admit(
                &root,
                &Map::from_iter([
                    (
                        "proposal_id".into(),
                        expected["replay"]["proposal_id"].clone(),
                    ),
                    ("actor".into(), json!("fixture")),
                    ("authority_basis".into(), json!({"kind":"fixture"})),
                    (
                        "idempotency_key".into(),
                        expected["replay"]["idempotency_key"].clone(),
                    ),
                ]),
            )
            .expect("fixture admission replay");
        assert_eq!(replay["event_id"], expected["replay"]["event_id"]);
        assert_eq!(replay["ledger_head"], expected["event_hashes"][1]);
        let name = expected["sequence"]["name"].as_str().unwrap();
        let manifest = engine
            .load_sequence_manifest(&root, name)
            .expect("fixture manifest verifies");
        assert_eq!(
            manifest["creation_hash"],
            expected["sequence"]["creation_hash"]
        );
        let claims = engine
            .verified_sequence_claims(&root, name, &manifest)
            .expect("fixture claim chain verifies");
        let expected_hashes = expected["sequence"]["claim_hashes"].as_array().unwrap();
        assert_eq!(claims.len(), expected_hashes.len());
        for (claim, hash) in claims.iter().zip(expected_hashes.iter()) {
            assert_eq!(&claim["claim_hash"], hash);
        }
        let _ = fs::remove_dir_all(root);
    }
    #[test]
    fn status_reports_stale_projection_without_rebuilding_it() {
        let engine = engine();
        let root = std::env::temp_dir().join(format!("epistemic-status-stale-{}", Uuid::new_v4()));
        engine
            .rebuild_projection(&root)
            .expect("initial projection");
        let table = engine
            .projection_meta_table
            .as_ref()
            .expect("projection metadata table");
        let projection = engine.projection_path(&root);
        let db = Connection::open(&projection).expect("open projection");
        db.execute(
            &format!("update {table} set ledger_sequence = 99 where meta_id = 'current'"),
            [],
        )
        .expect("make projection metadata stale");
        drop(db);

        let status = engine.status(&root).expect("bounded status");
        assert_eq!(status["status"], "ok");
        assert_eq!(status["projection_status"], "stale");
        assert_eq!(status["projection_current"], false);
        assert_eq!(status["status_rebuilds_projection"], false);

        let db = Connection::open(&projection).expect("reopen projection");
        let stored_sequence: i64 = db
            .query_row(
                &format!("select ledger_sequence from {table} where meta_id = 'current'"),
                [],
                |row| row.get(0),
            )
            .expect("read unchanged stale metadata");
        assert_eq!(stored_sequence, 99);
        drop(db);
        let runtime = projection.parent().expect("projection parent");
        assert!(
            fs::read_dir(runtime)
                .expect("read projection runtime")
                .all(|entry| !entry
                    .expect("runtime entry")
                    .file_name()
                    .to_string_lossy()
                    .contains(".next-")),
            "status must not create a scratch projection"
        );
        let _ = fs::remove_dir_all(root);
    }
    #[test]
    fn query_incrementally_catches_up_multiple_missing_events() {
        let engine = engine();
        let root =
            std::env::temp_dir().join(format!("epistemic-query-catch-up-{}", Uuid::new_v4()));
        engine
            .rebuild_projection(&root)
            .expect("initial projection");

        for (entity_id, title) in [
            ("claim:increment-one", "Incremental precursor"),
            ("claim:increment-target", "Exact incremental target"),
        ] {
            event_ledger::append_event(
                engine.error,
                &engine.ledger_layout(&root),
                engine.event_hash_field,
                None,
                None,
                |ctx| {
                    json!({
                        "schema":engine.domain.storage.event_schema_id,
                        "sequence":ctx.sequence,
                        "event_id":ctx.event_id,
                        "previous_hash":ctx.previous_hash,
                        "operations":[{
                            "op":"entity.declare",
                            "entity_id":entity_id,
                            "kind":"claim",
                            "title":title
                        }],
                        "actor":"incremental-test"
                    })
                },
            )
            .expect("append canonical event without projection refresh");
        }

        let stale = engine.status(&root).expect("stale status");
        assert_eq!(stale["projection_status"], "stale");
        assert_eq!(stale["event_count"], 2);

        let result = engine
            .query(
                &root,
                &Map::from_iter([
                    ("kind".into(), json!("claim")),
                    ("text".into(), json!("Exact incremental target")),
                    ("limit".into(), json!(10)),
                ]),
            )
            .expect("incremental exact-title query");
        assert_eq!(result["returned"], 1);
        assert_eq!(result["items"][0]["entity_id"], "claim:increment-target");

        let current = engine.status(&root).expect("current status");
        assert_eq!(current["projection_status"], "current");
        assert_eq!(current["projection_current"], true);
        let runtime = engine.projection_path(&root);
        let runtime = runtime.parent().expect("projection parent");
        assert!(
            fs::read_dir(runtime)
                .expect("read projection runtime")
                .all(|entry| !entry
                    .expect("runtime entry")
                    .file_name()
                    .to_string_lossy()
                    .contains(".next-")),
            "incremental catch-up must not create a scratch projection"
        );
        let _ = fs::remove_dir_all(root);
    }
}
