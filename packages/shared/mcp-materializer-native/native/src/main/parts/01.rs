use fs2::FileExt;
use narada_mcp_materialization_contract::{
    canonical_json_sha256, codex_managed_selectors, describe_config, generation_fingerprint,
    merge_codex_configuration, pretty_json as contract_pretty_json, ConfigArtifact,
    ManagedProjection, AMBIGUOUS_GENERATION_SCHEMA, CONTRACT_VERSION, GENERATION_SCHEMA,
    LEGACY_GENERATION_SCHEMA,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::env;
use std::fs::{self, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration as StdDuration, Instant};
use time::{format_description::well_known::Rfc3339, OffsetDateTime};

const INPUT_SCHEMA: &str = "narada.carrier_materialization_input.v1";

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct MaterializationInput {
    schema: String,
    workspace_root: PathBuf,
    carrier_contract_path: PathBuf,
    carrier_contract_fingerprint: String,
    artifact_manifest_path: PathBuf,
    artifact_manifest_fingerprint: Option<String>,
    artifact_build_set_path: PathBuf,
    artifact_build_set_fingerprint: String,
    runtime_profile_kind: String,
    runtime_implementation_matrix_path: PathBuf,
    runtime_implementation_matrix_fingerprint: String,
    registrar_entrypoint: PathBuf,
    registrar_fingerprint: Option<String>,
    proxy_implementation: String,
    proxy_entrypoint: PathBuf,
    proxy_fingerprint: Option<String>,
    installed_carrier_index_path: PathBuf,
    carriers: Vec<CarrierInput>,
}
#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct CarrierInput {
    carrier_id: String,
    carrier_kind: CarrierKind,
    config_path: PathBuf,
    #[serde(default)]
    codex_plugin_overrides: BTreeMap<String, bool>,
    #[serde(default)]
    trust_projects: Vec<String>,
    #[serde(default)]
    binding_admission_path: Option<PathBuf>,
    #[serde(default)]
    binding_admission_envelope: Option<Value>,
    servers: Vec<ServerInput>,
}

#[derive(Debug, Deserialize, Serialize, Clone, Copy)]
#[serde(rename_all = "lowercase")]
enum CarrierKind {
    Codex,
    Kimi,
    Opencode,
    Pi,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
#[serde(deny_unknown_fields)]
struct ServerInput {
    /// Stable machine identity. Legacy plans may omit it and fall back to the alias.
    #[serde(default)]
    binding_id: Option<String>,
    /// Private registrar/materialization identity; never used as the carrier-visible name.
    #[serde(default)]
    source_server_key: Option<String>,
    name: String,
    command: String,
    #[serde(default)]
    args: Vec<String>,
    #[serde(default)]
    env_vars: Vec<String>,
    #[serde(default = "default_true")]
    enabled: bool,
    #[serde(default)]
    approval_mode: Option<String>,
    #[serde(default)]
    startup_timeout_sec: Option<u64>,
    // Retained in the strict input contract; Codex policy is server-scoped.
    #[allow(dead_code)]
    #[serde(default)]
    tools: Vec<ToolInput>,
}

#[allow(dead_code)]
#[derive(Debug, Deserialize, Serialize, Clone)]
#[serde(deny_unknown_fields)]
struct ToolInput {
    name: String,
    approval_mode: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ContractDescribeInput {
    carrier_kind: String,
    config_path: PathBuf,
    #[serde(default)]
    selectors: Vec<String>,
    #[serde(default)]
    server_ids: Vec<String>,
    #[serde(default)]
    plugin_ids: Vec<String>,
    #[serde(default)]
    project_paths: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ContractMergeCodexInput {
    existing_path: Option<PathBuf>,
    desired_path: PathBuf,
    output_path: PathBuf,
    #[serde(default)]
    previous_selectors: Vec<String>,
    #[serde(default)]
    server_ids: Vec<String>,
    #[serde(default)]
    plugin_ids: Vec<String>,
    #[serde(default)]
    project_paths: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ContractFormatJsonInput {
    source_path: PathBuf,
    output_path: PathBuf,
    header: Option<String>,
}

#[derive(Debug, Serialize)]
struct Generation {
    schema: &'static str,
    contract_version: u32,
    carrier_id: String,
    carrier_kind: CarrierKind,
    config_path: String,
    config_artifact: ConfigArtifact,
    managed_projection: ManagedProjection,
    materialization_contract_entrypoint: String,
    materialization_contract_fingerprint: Option<String>,
    artifact_manifest_path: String,
    artifact_manifest_fingerprint: Option<String>,
    artifact_build_set_path: String,
    artifact_build_set_fingerprint: String,
    materialization_input_digest: String,
    bundle_id: String,
    bundle_path: String,
    bundle_commit_pointer_path: String,
    bundle_fingerprint: String,
    launch_catalog_fingerprint: String,
    validation_policy_identity: String,
    migration_provenance: String,
    runtime_profile_kind: String,
    runtime_materialization_plan_path: String,
    runtime_materialization_plan_fingerprint: String,
    runtime_implementation_matrix_path: String,
    runtime_implementation_matrix_fingerprint: String,
    registrar_entrypoint: String,
    registrar_fingerprint: Option<String>,
    proxy_implementation: String,
    proxy_entrypoint: String,
    proxy_fingerprint: Option<String>,
    server_count: usize,
    proxy_count: usize,
    generation_fingerprint: String,
    generated_at: String,
}

struct Publication {
    path: PathBuf,
    content: Vec<u8>,
}

#[derive(Clone)]
struct Snapshot {
    path: PathBuf,
    content: Option<Vec<u8>>,
}

fn default_true() -> bool {
    true
}

fn main() {
    if let Err(error) = run() {
        eprintln!(
            "{}",
            json!({
                "schema": "narada.mcp_materializer.error.v1",
                "status": "failed",
                "code": error.code,
                "message": error.message,
                "details": error.details,
            })
        );
        std::process::exit(1);
    }
}

#[derive(Debug)]
struct Failure {
    code: &'static str,
    message: String,
    details: Value,
}

impl Failure {
    fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            details: json!({}),
        }
    }
    fn with_details(mut self, details: Value) -> Self {
        self.details = details;
        self
    }
}

