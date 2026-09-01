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
    pub max_timeout_ms: u64,
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
            format!("domain_invalid:read_failed:{}:{}", path.display(), source)
        })?;
        Self::parse(&text)
    }

    /// Parse and validate descriptor JSON text.
    pub fn parse(text: &str) -> Result<Descriptor, String> {
        let value: Value =
            serde_json::from_str(text).map_err(|source| format!("domain_invalid:json:{source}"))?;
        Self::from_value(value)
    }

    /// Validate a descriptor value against the compiled-in
    /// `narada.ledger-domain.v1` JSON Schema, then bind typed structs.
    pub fn from_value(value: Value) -> Result<Descriptor, String> {
        let schema_value: Value = serde_json::from_str(DESCRIPTOR_SCHEMA_JSON)
            .map_err(|source| format!("domain_invalid:descriptor_schema_compile:{source}"))?;
        let validator = jsonschema::validator_for(&schema_value)
            .map_err(|source| format!("domain_invalid:descriptor_schema_compile:{source}"))?;
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
            return Err(format!("domain_invalid:schema_id:{}", descriptor.schema));
        }
        Ok(descriptor)
    }
}

