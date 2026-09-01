// Typed `narada.ledger-domain.v1` descriptor loading and validation.
//
// A descriptor is validated at startup against the compiled-in generic
// descriptor schema (`packages/shared/ledger-domain-mcp/domain.schema.json`)
// and then deserialized into typed structs for every engine section. Domain
// vocabularies and tool schemas remain descriptor data. Every refusal carries
// the `domain_invalid:<detail>` shape; startup fails hard on any violation.

use serde::Deserialize;
use serde_json::{Map, Value};
use std::collections::BTreeMap;
use std::path::Path;

const DESCRIPTOR_SCHEMA_JSON: &str =
    include_str!("../../../../../shared/ledger-domain-mcp/domain.schema.json");

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
    pub communication: CommunicationConfig,
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
pub struct CommunicationConfig {
    pub canonical_kind: String,
    #[serde(default)]
    pub legacy_read_aliases: Vec<String>,
    pub legacy_read_policy: String,
    pub legacy_write_policy: String,
    pub contract_version: u64,
    pub canonicalization_operation: String,
    pub legacy_write_refusal_code: String,
    pub collision_refusal_code: String,
    pub required_fields: Vec<String>,
    pub content_any_of: Vec<String>,
}

