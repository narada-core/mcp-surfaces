//! Typed `narada.ledger-domain.v1` descriptor loading and validation.
//!
//! A descriptor is validated at startup against the compiled-in generic
//! descriptor schema (`packages/shared/ledger-domain-mcp/domain.schema.json`)
//! and then deserialized into typed structs for every engine section. Domain
//! vocabularies and tool schemas remain descriptor data. Every refusal carries
//! the `domain_invalid:<detail>` shape; startup fails hard on any violation.

use serde::Deserialize;
use serde_json::{Map, Value};
use std::collections::BTreeMap;
use std::path::Path;

const DESCRIPTOR_SCHEMA_JSON: &str =
    include_str!("../../../shared/ledger-domain-mcp/domain.schema.json");

pub const DESCRIPTOR_SCHEMA_ID: &str = "narada.ledger-domain.v1";

#[derive(Clone, Debug, Deserialize)]
pub struct Descriptor {
    pub schema: String,
    pub identity: Identity,
    pub storage: Storage,
    pub entities: Entities,
    pub relations: Relations,
    pub operations: Operations,
    pub id_derivation: IdDerivation,
    pub projection: Projection,
    pub query: QueryConfig,
    pub caps: Caps,
    pub features: Features,
    pub guidance: Guidance,
    pub tools: Vec<ToolSpec>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct Identity {
    pub domain_id: String,
    pub tool_prefix: String,
    pub schema_namespace: String,
    pub error_schema_id: String,
    pub implementation: String,
}

#[derive(Clone, Debug, Deserialize)]
pub struct Storage {
    pub control_root_subdir: String,
    pub runtime_subdir: String,
    pub ledger_file_prefix: String,
    pub event_schema_id: String,
    pub event_hash_field: String,
    pub subdirs: StorageSubdirs,
}

#[derive(Clone, Debug, Deserialize)]
pub struct StorageSubdirs {
    pub ledger: String,
    pub proposals: String,
    pub sequences: String,
}

#[derive(Clone, Debug, Deserialize)]
pub struct Entities {
    pub core_kinds: Vec<String>,
    pub required_fields: EntityRequiredFields,
    pub extension_rule: ExtensionRule,
}

#[derive(Clone, Debug, Deserialize)]
pub struct EntityRequiredFields {
    pub always: Vec<String>,
    #[serde(default)]
    pub conditional: Vec<ConditionalRequirement>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct ConditionalRequirement {
    pub when_kind: String,
    pub requires: Vec<String>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct ExtensionRule {
    pub must_contain: String,
    pub pattern: String,
    #[serde(default)]
    pub examples: Vec<String>,
    pub refusal_code: String,
}

#[derive(Clone, Debug, Deserialize)]
pub struct Relations {
    pub core: Vec<String>,
    pub extension_pattern: String,
    pub extension_rule: ExtensionRule,
}

#[derive(Clone, Debug, Deserialize)]
pub struct Operations {
    pub kinds: Vec<String>,
    pub count: Bound,
    pub additional_properties: bool,
    pub required_fields: BTreeMap<String, Vec<String>>,
    pub evidence_entry: Value,
    pub evidence_required_at_review: Vec<String>,
    pub reference_bindings: Vec<ReferenceBinding>,
    pub reference_resolution_scope: Vec<String>,
    pub schema: Value,
}

#[derive(Clone, Debug, Deserialize)]
pub struct ReferenceBinding {
    pub operation: String,
    pub fields: Vec<String>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct Bound {
    pub min: u64,
    pub max: u64,
}

#[derive(Clone, Debug, Deserialize)]
pub struct IdDerivation {
    pub digest_rule: String,
    pub safe_name_rule: String,
    pub entity: EntityIdRecipe,
    pub relation: RelationIdRecipe,
    pub local_ref_wiring: LocalRefWiring,
    pub operation_identity_prefixes: BTreeMap<String, OperationIdentityPrefix>,
    pub derived_idempotency_keys: DerivedIdempotencyKeys,
    pub generated_ids: GeneratedIds,
}

#[derive(Clone, Debug, Deserialize)]
pub struct EntityIdRecipe {
    pub applies_to: String,
    pub when: String,
    pub template: String,
    pub digest_input_fields: Vec<String>,
    #[serde(default)]
    pub digest_input_note: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct RelationIdRecipe {
    pub applies_to: String,
    pub when: String,
    pub template: String,
    pub hash_input: String,
}

#[derive(Clone, Debug, Deserialize)]
pub struct LocalRefWiring {
    pub declare_field: String,
    pub uniqueness: String,
    pub duplicate_refusal_code: String,
    pub reference_fields: BTreeMap<String, String>,
    pub unresolved_refusal_code: String,
}

#[derive(Clone, Debug, Deserialize)]
pub struct OperationIdentityPrefix {
    pub prefix: String,
    pub id_field: String,
}

#[derive(Clone, Debug, Deserialize)]
pub struct DerivedIdempotencyKeys {
    pub proposal: DerivedKeyRecipe,
    pub admission: DerivedKeyRecipe,
}

#[derive(Clone, Debug, Deserialize)]
pub struct DerivedKeyRecipe {
    pub template: String,
    pub input_fields: Vec<String>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct GeneratedIds {
    pub proposal_id: String,
    pub sequence_id: String,
    pub claim_id: String,
}

#[derive(Clone, Debug, Deserialize)]
pub struct Projection {
    pub ddl: String,
    pub fold: Vec<FoldEntry>,
    pub write_mode: String,
    #[serde(default)]
    pub payload_column: Option<String>,
    #[serde(default)]
    pub event_column: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct FoldEntry {
    pub operation: String,
    pub table: String,
    pub key_field: String,
    #[serde(default)]
    pub columns: Map<String, Value>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct QueryConfig {
    pub record_kind_enum: Vec<String>,
    #[serde(default)]
    pub kind_aliases: BTreeMap<String, Vec<String>>,
    #[serde(default)]
    pub max_clauses: Option<usize>,
    #[serde(default)]
    pub max_reach_depth: Option<usize>,
    #[serde(default)]
    pub max_one_of_values: Option<usize>,
    #[serde(default)]
    pub max_predicate_depth: Option<usize>,
    #[serde(default)]
    pub named_queries: BTreeMap<String, Value>,
    #[serde(default)]
    pub reply_state_attribute: Option<String>,
    #[serde(default)]
    pub relation_inverses: BTreeMap<String, String>,
    #[serde(default)]
    pub read_receipt_kind: Option<String>,
    #[serde(default)]
    pub read_receipt_kind_attribute: Option<String>,
    #[serde(default)]
    pub read_receipt_message_attribute: Option<String>,
    #[serde(default)]
    pub read_receipt_reader_attribute: Option<String>,
    pub entity_compact_projection: Vec<String>,
    pub entity_full_projection: Vec<String>,
    pub record_compact_projection: Vec<String>,
    pub record_full_projection: Vec<String>,
    #[serde(default)]
    pub text_filter: Option<String>,
    pub neighborhood_relation_fields: Vec<String>,
    pub neighborhood_record_match_fields: Vec<String>,
    pub neighborhood_record_fields: Vec<String>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct Caps {
    pub operations_per_proposal: Bound,
    pub query_limit: CappedLimit,
    pub proposal_read_limit: CappedLimit,
    pub query_batch: QueryBatchCaps,
    pub query_execution: QueryExecutionCaps,
    pub neighborhood_limit: CappedLimit,
    pub snapshot_limit: CappedLimit,
    pub export: ExportCaps,
    pub source_inspect: SourceInspectCaps,
    pub sequence_name_chars: Bound,
    pub sequence_page_limit: CappedLimit,
    pub authority_basis_properties: Bound,
    pub authority_basis_bytes: usize,
    pub resubmit: ResubmitCaps,
    pub capture_sources: CaptureSourcesCaps,
    pub actor_chars: u64,
    pub idempotency_key_chars: Bound,
    pub guidance_routing_hint_chars: u64,
}

#[derive(Clone, Debug, Deserialize)]
pub struct QueryExecutionCaps {
    pub max_datoms_scanned: u64,
    pub max_traversal_edges: u64,
    pub max_output_bytes: u64,
}

#[derive(Clone, Debug, Deserialize)]
pub struct CappedLimit {
    pub min: u64,
    pub max: u64,
    pub default: u64,
}

#[derive(Clone, Debug, Deserialize)]
pub struct QueryBatchCaps {
    pub min_queries: u64,
    pub max_queries: u64,
    pub limit_per_query_min: u64,
    pub limit_per_query_max: u64,
    pub limit_per_query_default: u64,
}

#[derive(Clone, Debug, Deserialize)]
pub struct ExportCaps {
    pub entities: u64,
    pub relations: u64,
    pub records: u64,
}

#[derive(Clone, Debug, Deserialize)]
pub struct SourceInspectCaps {
    pub paths_min: u64,
    pub paths_max: u64,
    pub sections_min: u64,
    pub sections_max: u64,
    pub sections_default: u64,
    pub chars_min: u64,
    pub chars_max: u64,
    pub chars_default: u64,
    pub file_bytes_max: u64,
}

#[derive(Clone, Debug, Deserialize)]
pub struct ResubmitCaps {
    pub drop_operation_ids_max: u64,
    pub replacements_max: u64,
    pub resulting_min: u64,
    pub resulting_max: u64,
}

#[derive(Clone, Debug, Deserialize)]
pub struct CaptureSourcesCaps {
    pub sources_min: u64,
    pub sources_max: u64,
    pub operations_max: u64,
    pub combined_max: u64,
}

#[derive(Clone, Debug, Deserialize)]
pub struct Features {
    pub proposals: ProposalsFeature,
    pub sequences: SequencesFeature,
    pub source_inspect: SourceInspectFeature,
    pub snapshot: SnapshotFeature,
    pub export: ExportFeature,
}

impl Features {
    pub fn enabled(&self, feature: &str) -> bool {
        match feature {
            "proposals" => self.proposals.enabled,
            "sequences" => self.sequences.enabled,
            "source_inspect" => self.source_inspect.enabled,
            "snapshot" => self.snapshot.enabled,
            "export" => self.export.enabled,
            _ => false,
        }
    }
}

#[derive(Clone, Debug, Deserialize)]
pub struct ProposalsFeature {
    pub enabled: bool,
    pub proposal_schema_id: String,
    pub submission_receipt_schema_id: String,
    pub review_schema_id: String,
    pub admission_receipt_schema_id: String,
    pub rejection_schema_id: String,
    pub resubmission_schema_id: String,
    pub source_capture_schema_id: String,
    pub compound_schema_id: String,
    pub read_schema_id: String,
    pub event_kind: String,
    pub compound: bool,
    pub review_gate_preserved: bool,
    pub certifies_truth: bool,
    pub capture_sources: CaptureSourcesFeature,
    pub resubmit: ResubmitFeature,
}

#[derive(Clone, Debug, Deserialize)]
pub struct CaptureSourcesFeature {
    pub sources_field_only: bool,
    pub source_item_fields: Vec<String>,
    pub reports_existing_identities: bool,
    pub admission_requires_explicit_call: bool,
}

#[derive(Clone, Debug, Deserialize)]
pub struct ResubmitFeature {
    pub drop_identity_format: String,
    pub drop_max: u64,
    pub replacements_max: u64,
    pub missing_drop_refusal_code: String,
}

#[derive(Clone, Debug, Deserialize)]
pub struct SequencesFeature {
    pub enabled: bool,
    pub manifest_schema_id: String,
    pub claim_schema_id: String,
    pub claim_receipt_schema_id: String,
    pub status_schema_id: String,
    pub list_schema_id: String,
    pub claims_schema_id: String,
    pub idempotency_schema_id: String,
    pub step: u64,
    pub start_at_min: u64,
    pub claim_file_pattern: String,
    pub manifest_hash_field: String,
    pub claim_hash_field: String,
    pub claim_chain_field: String,
}

#[derive(Clone, Debug, Deserialize)]
pub struct SourceInspectFeature {
    pub enabled: bool,
    pub keywords: Vec<String>,
    #[serde(default)]
    pub keyword_match: Option<String>,
    pub response_schema_id: String,
    #[serde(default)]
    pub containment: Option<String>,
    pub outside_refusal_code: String,
    pub too_large_refusal_code: String,
}

#[derive(Clone, Debug, Deserialize)]
pub struct SnapshotFeature {
    pub enabled: bool,
    pub response_schema_id: String,
    pub stability_retries: u64,
    pub unstable_refusal_code: String,
    pub head_mismatch_refusal_code: String,
}

#[derive(Clone, Debug, Deserialize)]
pub struct ExportFeature {
    pub enabled: bool,
    pub formats: Vec<String>,
    pub default_format: String,
    pub response_schema_id: String,
    pub jsonld_context: Map<String, Value>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct Guidance {
    pub schema_id: String,
    pub emission_order: Vec<String>,
    #[serde(default)]
    pub requested_echo: Option<String>,
    pub engine_derived_fields: Map<String, Value>,
    pub fields: Map<String, Value>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct ToolSpec {
    pub name: String,
    pub description: String,
    #[serde(default)]
    pub class: Option<String>,
    #[serde(default)]
    pub feature: Option<String>,
    pub annotations: Value,
    #[serde(rename = "inputSchema")]
    pub input_schema: Value,
}

impl Descriptor {
    /// Load and validate a descriptor file. Every failure is a
    /// `domain_invalid:<detail>` string; callers fail startup hard.
    pub fn load(path: &Path) -> Result<Descriptor, String> {
        let text = std::fs::read_to_string(path).map_err(|source| {
            format!(
                "domain_invalid:read_failed:{}:{}",
                path.display(),
                source
            )
        })?;
        Self::parse(&text)
    }

    /// Parse and validate descriptor JSON text.
    pub fn parse(text: &str) -> Result<Descriptor, String> {
        let value: Value = serde_json::from_str(text)
            .map_err(|source| format!("domain_invalid:json:{source}"))?;
        Self::from_value(value)
    }

    /// Validate a descriptor value against the compiled-in
    /// `narada.ledger-domain.v1` JSON Schema, then bind typed structs.
    pub fn from_value(value: Value) -> Result<Descriptor, String> {
        let schema_value: Value = serde_json::from_str(DESCRIPTOR_SCHEMA_JSON)
            .map_err(|source| format!("domain_invalid:descriptor_schema_compile:{source}"))?;
        let validator = jsonschema::validator_for(&schema_value).map_err(|source| {
            format!("domain_invalid:descriptor_schema_compile:{source}")
        })?;
        let failures = validator
            .iter_errors(&value)
            .take(5)
            .map(|failure| failure.to_string())
            .collect::<Vec<_>>();
        if !failures.is_empty() {
            return Err(format!("domain_invalid:schema:{}", failures.join("; ")));
        }
        let descriptor: Descriptor = serde_json::from_value(value)
            .map_err(|source| format!("domain_invalid:structure:{source}"))?;
        if descriptor.schema != DESCRIPTOR_SCHEMA_ID {
            return Err(format!(
                "domain_invalid:schema_id:{}",
                descriptor.schema
            ));
        }
        Ok(descriptor)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn epistemic_descriptor_path() -> std::path::PathBuf {
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../shared/ledger-domain-epistemic/domain.json")
    }

    #[test]
    fn checked_in_epistemic_descriptor_loads() {
        let descriptor =
            Descriptor::load(&epistemic_descriptor_path()).expect("epistemic descriptor loads");
        assert_eq!(descriptor.schema, DESCRIPTOR_SCHEMA_ID);
        assert_eq!(descriptor.identity.tool_prefix, "epistemic_graph");
        assert_eq!(descriptor.tools.len(), 22);
        assert_eq!(descriptor.entities.core_kinds.len(), 8);
        assert_eq!(descriptor.relations.core.len(), 14);
        assert_eq!(descriptor.operations.kinds.len(), 5);
        assert!(descriptor.features.proposals.enabled);
        assert!(descriptor.features.sequences.enabled);
        assert!(descriptor.features.source_inspect.enabled);
        assert!(descriptor.features.snapshot.enabled);
        assert!(descriptor.features.export.enabled);
    }

    #[test]
    fn malformed_descriptor_is_domain_invalid() {
        let failure = Descriptor::parse("{not json").expect_err("malformed JSON refuses");
        assert!(failure.starts_with("domain_invalid:"), "{failure}");

        let failure = Descriptor::parse("{\"schema\":\"narada.ledger-domain.v1\"}")
            .expect_err("incomplete descriptor refuses");
        assert!(failure.starts_with("domain_invalid:"), "{failure}");

        let mut value: Value = serde_json::from_str(
            &std::fs::read_to_string(epistemic_descriptor_path()).expect("descriptor text"),
        )
        .expect("descriptor json");
        value["identity"]["tool_prefix"] = Value::from(42);
        let failure = Descriptor::from_value(value).expect_err("wrong type refuses");
        assert!(failure.starts_with("domain_invalid:"), "{failure}");
    }

    #[test]
    fn unknown_top_level_section_is_refused() {
        let mut value: Value = serde_json::from_str(
            &std::fs::read_to_string(epistemic_descriptor_path()).expect("descriptor text"),
        )
        .expect("descriptor json");
        value["speculative_section"] = serde_json::json!({"invented": true});
        let failure =
            Descriptor::from_value(value).expect_err("unknown sections are refused by the schema");
        assert!(failure.starts_with("domain_invalid:"), "{failure}");
    }

    #[test]
    fn descriptor_schema_does_not_hardcode_epistemic_identity() {
        let mut value: Value = serde_json::from_str(
            &std::fs::read_to_string(epistemic_descriptor_path()).expect("descriptor text"),
        )
        .expect("descriptor json");
        value["identity"]["domain_id"] = Value::from("other-domain");
        value["identity"]["tool_prefix"] = Value::from("other_graph");
        value["identity"]["schema_namespace"] = Value::from("other.graph");
        value["identity"]["error_schema_id"] = Value::from("other.graph.error.v1");
        let descriptor = Descriptor::from_value(value).expect("generic descriptor identity loads");
        assert_eq!(descriptor.identity.tool_prefix, "other_graph");
        assert_eq!(descriptor.identity.schema_namespace, "other.graph");
    }
}
