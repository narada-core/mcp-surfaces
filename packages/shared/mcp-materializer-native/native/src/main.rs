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

mod derive;

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

fn run() -> Result<(), Failure> {
    let mut args = env::args_os().skip(1);
    let mut command = args
        .next()
        .and_then(|value| value.into_string().ok())
        .ok_or_else(|| {
            Failure::new(
                "materializer_command_required",
                "Expected `materialize-all --input <path>`.",
            )
        })?;
    let current_executable = env::current_exe().ok();
    if current_executable
        .as_ref()
        .is_some_and(|path| path_eq(path, Path::new(&command)))
    {
        command = args
            .next()
            .and_then(|value| value.into_string().ok())
            .ok_or_else(|| {
                Failure::new(
                    "materializer_command_required",
                    "Expected a command after the compatibility entrypoint.",
                )
            })?;
    }
    if command == "--materialize-all" {
        if args.next().is_some() {
            return Err(Failure::new(
                "materializer_argument_unknown",
                "Unexpected trailing argument.",
            ));
        }
        let user_profile = env::var_os("USERPROFILE").ok_or_else(|| {
            Failure::new(
                "materializer_user_profile_required",
                "USERPROFILE is required for installed-carrier recovery.",
            )
        })?;
        let index = PathBuf::from(user_profile).join(".narada/carriers/installed-carriers.json");
        let input = derive::derive_input(derive::options_from_installed_index(&index)?)?;
        let result = materialize(input, true)?;
        println!("{result}");
        return Ok(());
    }
    if command == "publish" {
        let flag = args.next().and_then(|value| value.into_string().ok());
        if flag.as_deref() != Some("--artifact-root") {
            return Err(Failure::new(
                "materializer_artifact_root_required",
                "Expected publish --artifact-root <path>.",
            ));
        }
        let artifact_root = args.next().map(PathBuf::from).ok_or_else(|| {
            Failure::new(
                "materializer_artifact_root_required",
                "Expected an artifact root.",
            )
        })?;
        if args.next().is_some() {
            return Err(Failure::new(
                "materializer_argument_unknown",
                "Unexpected trailing argument.",
            ));
        }
        println!("{}", publish_self(&artifact_root)?);
        return Ok(());
    }
    if matches!(
        command.as_str(),
        "contract-describe"
            | "contract-fingerprint-generation"
            | "contract-merge-codex"
            | "contract-format-json"
    ) {
        let flag = args.next().and_then(|value| value.into_string().ok());
        if flag.as_deref() != Some("--input") {
            return Err(Failure::new(
                "materializer_contract_input_required",
                format!("Expected {command} --input <path>."),
            ));
        }
        let input_path = args.next().map(PathBuf::from).ok_or_else(|| {
            Failure::new(
                "materializer_contract_input_required",
                "Expected an input path.",
            )
        })?;
        if args.next().is_some() {
            return Err(Failure::new(
                "materializer_argument_unknown",
                "Unexpected trailing argument.",
            ));
        }
        if command == "contract-describe" {
            let input: ContractDescribeInput =
                serde_json::from_slice(&fs::read(&input_path).map_err(|error| {
                    Failure::new("materializer_contract_input_read_failed", error.to_string())
                })?)
                .map_err(|error| {
                    Failure::new("materializer_contract_input_invalid", error.to_string())
                })?;
            let content = fs::read(&input.config_path).map_err(|error| {
                Failure::new("materializer_config_read_failed", error.to_string())
            })?;
            let selectors = if input.carrier_kind == "codex" && input.selectors.is_empty() {
                codex_managed_selectors(&input.server_ids, &input.plugin_ids, &input.project_paths)
            } else {
                input.selectors
            };
            let description = describe_config(&input.carrier_kind, &content, &selectors)
                .map_err(|error| Failure::new("materializer_contract_describe_failed", error))?;
            println!(
                "{}",
                serde_json::to_value(description).map_err(json_failure)?
            );
        } else if command == "contract-fingerprint-generation" {
            let generation = read_json(&input_path, "materializer_contract_input_invalid")?;
            println!(
                "{}",
                json!({"generation_fingerprint": generation_fingerprint(&generation).map_err(|error| Failure::new("materializer_generation_fingerprint_failed", error))?})
            );
        } else if command == "contract-merge-codex" {
            let input: ContractMergeCodexInput =
                serde_json::from_slice(&fs::read(&input_path).map_err(|error| {
                    Failure::new("materializer_contract_input_read_failed", error.to_string())
                })?)
                .map_err(|error| {
                    Failure::new("materializer_contract_input_invalid", error.to_string())
                })?;
            let desired = fs::read(&input.desired_path).map_err(|error| {
                Failure::new("materializer_config_read_failed", error.to_string())
            })?;
            let existing = input
                .existing_path
                .as_ref()
                .map(|path| {
                    fs::read(path).map_err(|error| {
                        Failure::new("materializer_config_read_failed", error.to_string())
                    })
                })
                .transpose()?;
            let selectors =
                codex_managed_selectors(&input.server_ids, &input.plugin_ids, &input.project_paths);
            let merged = merge_codex_configuration(
                existing.as_deref(),
                &desired,
                &input.previous_selectors,
                &selectors,
            )
            .map_err(|error| Failure::new("materializer_codex_merge_failed", error))?;
            fs::write(&input.output_path, merged).map_err(|error| {
                Failure::new(
                    "materializer_contract_output_write_failed",
                    error.to_string(),
                )
            })?;
            println!("{}", json!({"status":"merged","selectors":selectors}));
        } else {
            let input: ContractFormatJsonInput =
                serde_json::from_slice(&fs::read(&input_path).map_err(|error| {
                    Failure::new("materializer_contract_input_read_failed", error.to_string())
                })?)
                .map_err(|error| {
                    Failure::new("materializer_contract_input_invalid", error.to_string())
                })?;
            let value = read_json(&input.source_path, "materializer_json_source_invalid")?;
            let mut output = pretty_json(&value)?;
            if let Some(header) = input.header {
                let mut prefix = header
                    .trim_end_matches(['\r', '\n'])
                    .replace("\r\n", "\n")
                    .replace('\r', "\n")
                    .into_bytes();
                prefix.push(b'\n');
                prefix.extend(output);
                output = prefix;
            }
            fs::write(&input.output_path, output).map_err(|error| {
                Failure::new(
                    "materializer_contract_output_write_failed",
                    error.to_string(),
                )
            })?;
            println!("{}", json!({"status":"formatted"}));
        }
        return Ok(());
    }
    if matches!(command.as_str(), "materialize-site" | "promote-site") {
        let require_fresh_validation = command == "promote-site";
        let options = derive::DeriveOptions::parse(args)?;
        let result = materialize(derive::derive_input(options)?, require_fresh_validation)?;
        println!("{result}");
        return Ok(());
    }
    if command == "recover-generation" {
        let flag = args.next().and_then(|value| value.into_string().ok());
        if flag.as_deref() != Some("--generation") {
            return Err(Failure::new(
                "materializer_generation_required",
                "Expected recover-generation --generation <path>.",
            ));
        }
        let generation = args.next().map(PathBuf::from).ok_or_else(|| {
            Failure::new(
                "materializer_generation_required",
                "Expected a generation path.",
            )
        })?;
        if args.next().is_some() {
            return Err(Failure::new(
                "materializer_argument_unknown",
                "Unexpected trailing argument.",
            ));
        }
        let options = derive::options_from_generation(&generation)?;
        let input = derive::derive_input(options)?;
        let installed_index = input.installed_carrier_index_path.clone();
        let workspace_root = input.workspace_root.clone();
        let carrier_ids = input
            .carriers
            .iter()
            .map(|carrier| carrier.carrier_id.clone())
            .collect::<Vec<_>>();
        let materialization = materialize(input, true)?;
        let verification = verify_all(&installed_index)?;
        let recovered_at = OffsetDateTime::now_utc()
            .format(&Rfc3339)
            .map_err(|error| Failure::new("materializer_clock_failed", error.to_string()))?;
        let evidence_unsigned = json!({
            "schema": "narada.mcp_materializer.recovery_evidence.v1",
            "status": "recovered",
            "recovered_at": recovered_at,
            "trigger_generation_path": path_text(&generation),
            "materialization": materialization,
            "verification": verification,
        });
        let evidence_fingerprint =
            sha256(&serde_json::to_vec(&evidence_unsigned).map_err(json_failure)?);
        let evidence_ref = format!("sha256:{evidence_fingerprint}");
        let mut evidence = evidence_unsigned;
        evidence
            .as_object_mut()
            .expect("recovery evidence is an object")
            .insert("ref".to_string(), Value::String(evidence_ref.clone()));
        let recovery_root = workspace_root.join(".ai/runtime/carrier-materialization-recovery");
        let evidence_path = recovery_root.join("latest-materialization.json");
        let pressure_path = workspace_root.join(".ai/runtime/carrier-restart-pressure.json");
        let pressure_carriers = carrier_ids
            .iter()
            .map(|carrier_id| {
                (
                    carrier_id.clone(),
                    json!({
                        "carrier_id": carrier_id,
                        "materialized_at": recovered_at,
                        "evidence_ref": evidence_ref,
                    }),
                )
            })
            .collect::<Map<String, Value>>();
        let pressure = json!({
            "schema": "narada.carrier_restart_pressure.v1",
            "updated_at": recovered_at,
            "carriers": pressure_carriers,
        });
        transactional_publish(&[
            Publication {
                path: evidence_path.clone(),
                content: pretty_json(&evidence)?,
            },
            Publication {
                path: pressure_path.clone(),
                content: pretty_json(&pressure)?,
            },
        ])?;
        println!(
            "{}",
            json!({
                "schema": evidence.get("schema"),
                "status": evidence.get("status"),
                "ref": evidence_ref,
                "recovered_at": recovered_at,
                "trigger_generation_path": path_text(&generation),
                "materialization": evidence.get("materialization"),
                "verification": evidence.get("verification"),
                "evidence_path": path_text(&evidence_path),
                "restart_pressure_path": path_text(&pressure_path),
                "restart_pressure": pressure.get("carriers"),
            })
        );
        return Ok(());
    }
    if command == "verify-all" {
        let flag = args.next().and_then(|value| value.into_string().ok());
        if flag.as_deref() != Some("--installed-index") {
            return Err(Failure::new(
                "materializer_installed_index_required",
                "Expected verify-all --installed-index <path>.",
            ));
        }
        let index = args.next().map(PathBuf::from).ok_or_else(|| {
            Failure::new(
                "materializer_installed_index_required",
                "Expected an installed carrier index path.",
            )
        })?;
        if args.next().is_some() {
            return Err(Failure::new(
                "materializer_argument_unknown",
                "Unexpected trailing argument.",
            ));
        }
        println!("{}", verify_all(&index)?);
        return Ok(());
    }
    if command == "acknowledge-restart" {
        let mut values = BTreeMap::<String, String>::new();
        while let Some(flag) = args.next() {
            let flag = flag.into_string().map_err(|_| {
                Failure::new("materializer_argument_invalid", "Argument is not UTF-8.")
            })?;
            if !matches!(
                flag.as_str(),
                "--installed-index" | "--carrier-id" | "--expected-evidence-ref"
            ) {
                return Err(Failure::new(
                    "materializer_argument_unknown",
                    format!("Unknown argument: {flag}"),
                ));
            }
            let value = args
                .next()
                .and_then(|value| value.into_string().ok())
                .ok_or_else(|| {
                    Failure::new(
                        "materializer_argument_value_required",
                        format!("{flag} requires a value."),
                    )
                })?;
            if values.insert(flag.clone(), value).is_some() {
                return Err(Failure::new(
                    "materializer_argument_duplicate",
                    format!("Duplicate argument: {flag}"),
                ));
            }
        }
        let required = |flag: &str| {
            values.get(flag).cloned().ok_or_else(|| {
                Failure::new("materializer_argument_required", format!("Missing {flag}."))
            })
        };
        let result = acknowledge_restart(
            Path::new(&required("--installed-index")?),
            &required("--carrier-id")?,
            &required("--expected-evidence-ref")?,
        )?;
        println!("{result}");
        if result.get("status").and_then(Value::as_str) == Some("stale_ack_refused") {
            std::process::exit(2);
        }
        return Ok(());
    }
    if command != "materialize-all" {
        return Err(Failure::new(
            "materializer_command_unknown",
            format!("Unknown command: {command}"),
        ));
    }
    let flag = args.next().and_then(|value| value.into_string().ok());
    if flag.as_deref() != Some("--input") {
        return Err(Failure::new(
            "materializer_input_required",
            "Expected --input <path>.",
        ));
    }
    let input_path = args
        .next()
        .map(PathBuf::from)
        .ok_or_else(|| Failure::new("materializer_input_required", "Expected --input <path>."))?;
    if args.next().is_some() {
        return Err(Failure::new(
            "materializer_argument_unknown",
            "Unexpected trailing argument.",
        ));
    }
    let raw = fs::read(&input_path).map_err(|error| {
        Failure::new("materializer_input_read_failed", error.to_string())
            .with_details(json!({"input_path": path_text(&input_path)}))
    })?;
    let input: MaterializationInput = serde_json::from_slice(&raw).map_err(|error| {
        Failure::new("materializer_input_invalid", error.to_string())
            .with_details(json!({"input_path": path_text(&input_path)}))
    })?;
    let result = materialize(input, false)?;
    println!("{result}");
    Ok(())
}

fn acknowledge_restart(
    installed_index_path: &Path,
    carrier_id: &str,
    expected_evidence_ref: &str,
) -> Result<Value, Failure> {
    validate_identifier(carrier_id, "carrier_id")?;
    if expected_evidence_ref.trim().is_empty() {
        return Err(Failure::new(
            "materializer_expected_evidence_ref_required",
            "Expected evidence reference must not be empty.",
        ));
    }
    let index = read_json(installed_index_path, "materializer_installed_index_invalid")?;
    if index.get("schema").and_then(Value::as_str) != Some("narada.installed_carrier_index.v1") {
        return Err(Failure::new(
            "materializer_installed_index_schema_unsupported",
            path_text(installed_index_path),
        ));
    }
    let workspace_root = PathBuf::from(json_field_string(&index, "workspace_root")?);
    let pressure_path = workspace_root.join(".ai/runtime/carrier-restart-pressure.json");
    let mut pressure = if pressure_path.exists() {
        read_json(&pressure_path, "materializer_restart_pressure_invalid")?
    } else {
        json!({
            "schema": "narada.carrier_restart_pressure.v1",
            "carriers": {},
        })
    };
    if pressure.get("schema").and_then(Value::as_str) != Some("narada.carrier_restart_pressure.v1")
    {
        return Err(Failure::new(
            "materializer_restart_pressure_schema_unsupported",
            path_text(&pressure_path),
        ));
    }
    let carriers = pressure
        .get_mut("carriers")
        .and_then(Value::as_object_mut)
        .ok_or_else(|| {
            Failure::new(
                "materializer_restart_pressure_carriers_required",
                path_text(&pressure_path),
            )
        })?;
    let current = carriers.get(carrier_id).cloned();
    let current_ref = current
        .as_ref()
        .and_then(|value| value.get("evidence_ref"))
        .and_then(Value::as_str);
    if current.is_some() && current_ref != Some(expected_evidence_ref) {
        return Ok(json!({
            "schema": "narada.carrier_restart_acknowledgement.v1",
            "status": "stale_ack_refused",
            "carrier_id": carrier_id,
            "expected_pressure_ref": expected_evidence_ref,
            "current_pressure": current,
            "remaining_carrier_ids": carriers.keys().cloned().collect::<Vec<_>>(),
        }));
    }
    let acknowledged = carriers.remove(carrier_id);
    let updated_at = OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .map_err(|error| Failure::new("materializer_clock_failed", error.to_string()))?;
    pressure
        .as_object_mut()
        .expect("pressure is an object")
        .insert("updated_at".to_string(), Value::String(updated_at));
    atomic_write(&pressure_path, &pretty_json(&pressure)?).map_err(|error| {
        Failure::new(
            "materializer_restart_pressure_publish_failed",
            error.to_string(),
        )
        .with_details(json!({"path": path_text(&pressure_path)}))
    })?;
    let remaining = pressure
        .get("carriers")
        .and_then(Value::as_object)
        .map(|value| value.keys().cloned().collect::<Vec<_>>())
        .unwrap_or_default();
    Ok(json!({
        "schema": "narada.carrier_restart_acknowledgement.v1",
        "status": if acknowledged.is_some() { "acknowledged" } else { "already_current" },
        "carrier_id": carrier_id,
        "acknowledged_pressure": acknowledged,
        "remaining_carrier_ids": remaining,
        "restart_pressure_path": path_text(&pressure_path),
    }))
}

fn verify_all(index_path: &Path) -> Result<Value, Failure> {
    let index = read_json(index_path, "materializer_installed_index_invalid")?;
    if index.get("schema").and_then(Value::as_str) != Some("narada.installed_carrier_index.v1") {
        return Err(Failure::new(
            "materializer_installed_index_schema_unsupported",
            path_text(index_path),
        ));
    }
    let carriers = index
        .get("carriers")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            Failure::new(
                "materializer_installed_index_carriers_required",
                path_text(index_path),
            )
        })?;
    if carriers.is_empty() {
        return Err(Failure::new(
            "materializer_installed_index_empty",
            path_text(index_path),
        ));
    }
    let mut verified = Vec::new();
    for carrier in carriers {
        let carrier_id = json_field_string(carrier, "carrier_id")?;
        let sidecar_path = PathBuf::from(json_field_string(carrier, "generation_sidecar_path")?);
        let generation = read_json(&sidecar_path, "materializer_generation_invalid")?;
        verify_generation(
            &generation,
            &sidecar_path,
            carrier
                .get("materialization_generation_fingerprint")
                .and_then(Value::as_str),
        )?;
        verified.push(carrier_id.to_string());
    }
    Ok(json!({
        "schema": "narada.mcp_materializer.verification.v1",
        "status": "current",
        "installed_carrier_index_path": path_text(index_path),
        "verified_carrier_ids": verified,
        "verified_carrier_count": verified.len(),
    }))
}

fn verify_generation(
    generation: &Value,
    sidecar_path: &Path,
    indexed_fingerprint: Option<&str>,
) -> Result<(), Failure> {
    if generation.get("schema").and_then(Value::as_str) != Some(GENERATION_SCHEMA) {
        return Err(Failure::new(
            "materializer_generation_schema_unsupported",
            path_text(sidecar_path),
        ));
    }
    let expected = json_field_string(generation, "generation_fingerprint")?;
    if indexed_fingerprint != Some(expected) {
        return Err(Failure::new(
            "materializer_index_generation_mismatch",
            path_text(sidecar_path),
        ));
    }
    if generation_fingerprint(generation)
        .map_err(|error| Failure::new("materializer_generation_fingerprint_failed", error))?
        != expected
    {
        return Err(Failure::new(
            "materializer_generation_fingerprint_mismatch",
            path_text(sidecar_path),
        ));
    }
    if generation.get("contract_version").and_then(Value::as_u64) != Some(CONTRACT_VERSION.into()) {
        return Err(Failure::new(
            "materializer_generation_contract_obsolete",
            path_text(sidecar_path),
        ));
    }
    verify_generation_bundle(generation, sidecar_path)?;
    verify_file_fingerprint(generation, "registrar_entrypoint", "registrar_fingerprint")?;
    verify_file_fingerprint(
        generation,
        "materialization_contract_entrypoint",
        "materialization_contract_fingerprint",
    )?;
    verify_file_fingerprint(generation, "proxy_entrypoint", "proxy_fingerprint")?;
    verify_file_fingerprint(
        generation,
        "runtime_implementation_matrix_path",
        "runtime_implementation_matrix_fingerprint",
    )?;
    let manifest_path = PathBuf::from(json_field_string(generation, "artifact_manifest_path")?);
    let manifest = read_json(&manifest_path, "materializer_artifact_manifest_invalid")?;
    if manifest.get("manifest_fingerprint").and_then(Value::as_str)
        != generation
            .get("artifact_manifest_fingerprint")
            .and_then(Value::as_str)
    {
        return Err(Failure::new(
            "materializer_artifact_manifest_fingerprint_mismatch",
            path_text(&manifest_path),
        ));
    }
    let config_path = PathBuf::from(json_field_string(generation, "config_path")?);
    let expected_config_path = sidecar_path
        .to_string_lossy()
        .strip_suffix(".narada-generation.json")
        .map(PathBuf::from)
        .ok_or_else(|| {
            Failure::new(
                "materializer_generation_sidecar_pair_invalid",
                path_text(sidecar_path),
            )
        })?;
    if !path_eq(&config_path, &expected_config_path) {
        return Err(Failure::new(
            "materializer_generation_config_pair_mismatch",
            path_text(sidecar_path),
        ));
    }
    let kind = json_field_string(generation, "carrier_kind")?;
    let config = fs::read(&config_path)
        .map_err(|error| Failure::new("materializer_config_read_failed", error.to_string()))?;
    let selectors = generation
        .pointer("/managed_projection/selectors")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            Failure::new(
                "materializer_managed_selectors_missing",
                path_text(sidecar_path),
            )
        })?
        .iter()
        .map(|value| {
            value.as_str().map(str::to_string).ok_or_else(|| {
                Failure::new(
                    "materializer_managed_selector_invalid",
                    path_text(sidecar_path),
                )
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    let current = describe_config(kind, &config, &selectors)
        .map_err(|error| Failure::new("materializer_contract_describe_failed", error))?;
    if current.managed_projection.sha256
        != generation
            .pointer("/managed_projection/sha256")
            .and_then(Value::as_str)
            .unwrap_or("")
    {
        return Err(Failure::new(
            "materializer_managed_projection_fingerprint_mismatch",
            path_text(&config_path),
        ));
    }
    let plan_path = PathBuf::from(json_field_string(
        generation,
        "runtime_materialization_plan_path",
    )?);
    let plan = read_json(&plan_path, "materializer_runtime_plan_invalid")?;
    let expected_plan = json_field_string(generation, "runtime_materialization_plan_fingerprint")?;
    let mut unsigned_plan = plan.clone();
    let embedded_plan = unsigned_plan
        .as_object_mut()
        .and_then(|object| object.remove("plan_fingerprint"))
        .and_then(|value| value.as_str().map(str::to_string));
    if embedded_plan.as_deref() != Some(expected_plan)
        || generation
            .get("launch_catalog_fingerprint")
            .and_then(Value::as_str)
            != Some(expected_plan)
        || sha256(&serde_json::to_vec(&unsigned_plan).map_err(json_failure)?) != expected_plan
    {
        return Err(Failure::new(
            "materializer_runtime_plan_fingerprint_mismatch",
            path_text(&plan_path),
        ));
    }
    let source = plan.get("source").ok_or_else(|| {
        Failure::new(
            "materializer_runtime_plan_source_missing",
            path_text(&plan_path),
        )
    })?;
    let contract_path = PathBuf::from(json_field_string(source, "carrier_contract_path")?);
    let contract = fs::read(&contract_path).map_err(|error| {
        Failure::new(
            "materializer_carrier_contract_read_failed",
            error.to_string(),
        )
    })?;
    if source
        .get("carrier_contract_fingerprint")
        .and_then(Value::as_str)
        != Some(sha256(&contract).as_str())
    {
        return Err(Failure::new(
            "materializer_carrier_contract_fingerprint_mismatch",
            path_text(&contract_path),
        ));
    }
    Ok(())
}

fn verify_generation_bundle(generation: &Value, sidecar_path: &Path) -> Result<(), Failure> {
    let bundle_id = json_field_string(generation, "bundle_id")?;
    let expected_bundle_fingerprint = json_field_string(generation, "bundle_fingerprint")?;
    let bundle_path = PathBuf::from(json_field_string(generation, "bundle_path")?);
    let mut bundle = read_json(&bundle_path, "materializer_bundle_invalid")?;
    if bundle.get("schema").and_then(Value::as_str) != Some("narada.carrier_generation_bundle.v1")
        || bundle.get("bundle_id").and_then(Value::as_str) != Some(bundle_id)
        || bundle.get("bundle_fingerprint").and_then(Value::as_str)
            != Some(expected_bundle_fingerprint)
    {
        return Err(Failure::new(
            "materializer_bundle_identity_mismatch",
            path_text(&bundle_path),
        ));
    }
    let object = bundle
        .as_object_mut()
        .ok_or_else(|| Failure::new("materializer_bundle_invalid", path_text(&bundle_path)))?;
    object.remove("bundle_id");
    object.remove("bundle_fingerprint");
    let actual = canonical_json_sha256(&bundle)
        .map_err(|error| Failure::new("materializer_bundle_fingerprint_failed", error))?;
    if actual != expected_bundle_fingerprint {
        return Err(Failure::new(
            "materializer_bundle_fingerprint_mismatch",
            path_text(&bundle_path),
        ));
    }
    let carrier_id = json_field_string(generation, "carrier_id")?;
    let member = bundle
        .get("carriers")
        .and_then(Value::as_array)
        .and_then(|carriers| {
            carriers.iter().find(|carrier| {
                carrier.get("carrier_id").and_then(Value::as_str) == Some(carrier_id)
            })
        })
        .ok_or_else(|| {
            Failure::new(
                "materializer_bundle_carrier_missing",
                format!("{bundle_id}:{carrier_id}"),
            )
        })?;
    if member
        .get("generation_sidecar_path")
        .and_then(Value::as_str)
        .is_none_or(|path| !path_eq(Path::new(path), sidecar_path))
    {
        return Err(Failure::new(
            "materializer_bundle_sidecar_mismatch",
            path_text(sidecar_path),
        ));
    }
    let pointer_path = PathBuf::from(json_field_string(generation, "bundle_commit_pointer_path")?);
    let pointer = read_json(&pointer_path, "materializer_bundle_pointer_invalid")?;
    if pointer.get("schema").and_then(Value::as_str)
        != Some("narada.carrier_generation_bundle_pointer.v1")
        || pointer.get("bundle_id").and_then(Value::as_str) != Some(bundle_id)
        || pointer.get("bundle_fingerprint").and_then(Value::as_str)
            != Some(expected_bundle_fingerprint)
        || pointer
            .get("bundle_path")
            .and_then(Value::as_str)
            .is_none_or(|path| !path_eq(Path::new(path), &bundle_path))
    {
        return Err(Failure::new(
            "materializer_bundle_not_committed",
            path_text(&pointer_path),
        ));
    }
    let build_set_path = PathBuf::from(json_field_string(generation, "artifact_build_set_path")?);
    let build_set = read_json(&build_set_path, "materializer_artifact_build_set_invalid")?;
    if build_set.get("build_set_digest").and_then(Value::as_str)
        != generation
            .get("artifact_build_set_fingerprint")
            .and_then(Value::as_str)
    {
        return Err(Failure::new(
            "materializer_artifact_build_set_identity_mismatch",
            path_text(&build_set_path),
        ));
    }
    Ok(())
}

fn verify_file_fingerprint(
    generation: &Value,
    path_field: &'static str,
    fingerprint_field: &'static str,
) -> Result<(), Failure> {
    let path = PathBuf::from(json_field_string(generation, path_field)?);
    let bytes = fs::read(&path).map_err(|error| {
        Failure::new("materializer_authority_file_read_failed", error.to_string())
    })?;
    if generation.get(fingerprint_field).and_then(Value::as_str) != Some(sha256(&bytes).as_str()) {
        return Err(Failure::new(
            "materializer_authority_file_fingerprint_mismatch",
            path_text(&path),
        ));
    }
    Ok(())
}

fn read_json(path: &Path, code: &'static str) -> Result<Value, Failure> {
    let bytes = fs::read(path).map_err(|error| Failure::new(code, error.to_string()))?;
    serde_json::from_slice(&bytes).map_err(|error| Failure::new(code, error.to_string()))
}

fn json_field_string<'a>(value: &'a Value, field: &'static str) -> Result<&'a str, Failure> {
    value
        .get(field)
        .and_then(Value::as_str)
        .ok_or_else(|| Failure::new("materializer_json_field_required", field))
}

fn publish_self(artifact_root: &Path) -> Result<Value, Failure> {
    let artifact_root = if artifact_root.is_absolute() {
        artifact_root.to_path_buf()
    } else {
        env::current_dir()
            .map_err(|error| {
                Failure::new("materializer_working_directory_failed", error.to_string())
            })?
            .join(artifact_root)
    };
    let executable = env::current_exe()
        .map_err(|error| Failure::new("materializer_executable_unresolved", error.to_string()))?;
    let bytes = fs::read(&executable)
        .map_err(|error| Failure::new("materializer_executable_read_failed", error.to_string()))?;
    let fingerprint = sha256(&bytes);
    let name = if cfg!(windows) {
        "narada-mcp-materializer.exe"
    } else {
        "narada-mcp-materializer"
    };
    let destination = artifact_root.join("versions").join(&fingerprint).join(name);
    if let Some(parent) = destination.parent() {
        fs::create_dir_all(parent).map_err(|error| {
            Failure::new("materializer_artifact_directory_failed", error.to_string())
        })?;
    }
    if destination.exists() {
        let existing = fs::read(&destination).map_err(|error| {
            Failure::new("materializer_artifact_read_failed", error.to_string())
        })?;
        if existing != bytes {
            return Err(Failure::new(
                "materializer_artifact_collision",
                path_text(&destination),
            ));
        }
    } else {
        atomic_write(&destination, &bytes).map_err(|error| {
            Failure::new("materializer_artifact_publish_failed", error.to_string())
        })?;
    }
    let generated_at = OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .map_err(|error| Failure::new("materializer_clock_failed", error.to_string()))?;
    let relative = format!("versions/{fingerprint}/{name}");
    let pointer = json!({
        "schema": "narada.mcp_materializer.native_artifact_pointer.v1",
        "generated_at": generated_at,
        "build_fingerprint": fingerprint,
        "artifacts": { name: relative },
    });
    atomic_write(&artifact_root.join("current.json"), &pretty_json(&pointer)?).map_err(
        |error| {
            Failure::new(
                "materializer_artifact_pointer_publish_failed",
                error.to_string(),
            )
        },
    )?;
    Ok(json!({
        "schema": "narada.mcp_materializer.publish_result.v1",
        "status": "published",
        "executable": path_text(&destination),
        "pointer_path": path_text(&artifact_root.join("current.json")),
        "build_fingerprint": fingerprint,
    }))
}

fn previous_managed_selectors(sidecar_path: &Path) -> Result<Vec<String>, Failure> {
    if !sidecar_path.exists() {
        return Ok(vec![]);
    }
    let generation = read_json(sidecar_path, "materializer_generation_invalid")?;
    match generation.get("schema").and_then(Value::as_str) {
        Some(GENERATION_SCHEMA) | Some(LEGACY_GENERATION_SCHEMA) => generation
            .pointer("/managed_projection/selectors")
            .and_then(Value::as_array)
            .ok_or_else(|| {
                Failure::new(
                    "materializer_managed_selectors_missing",
                    path_text(sidecar_path),
                )
            })?
            .iter()
            .map(|selector| {
                selector.as_str().map(str::to_string).ok_or_else(|| {
                    Failure::new(
                        "materializer_managed_selector_invalid",
                        path_text(sidecar_path),
                    )
                })
            })
            .collect(),
        Some(AMBIGUOUS_GENERATION_SCHEMA) => Ok(vec!["/mcp_servers".to_string()]),
        Some(schema) => Err(Failure::new(
            "materializer_generation_schema_unsupported",
            schema,
        )),
        None => Err(Failure::new(
            "materializer_generation_schema_missing",
            path_text(sidecar_path),
        )),
    }
}

fn materialize(
    input: MaterializationInput,
    require_fresh_validation: bool,
) -> Result<Value, Failure> {
    validate_input(&input)?;
    verify_artifact_build_set(&input)?;
    let generated_at = OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .map_err(|error| Failure::new("materializer_clock_failed", error.to_string()))?;
    let materialization_input_digest =
        canonical_json_sha256(&serde_json::to_value(&input).map_err(json_failure)?)
            .map_err(|error| Failure::new("materializer_input_fingerprint_failed", error))?;
    let validation_policy_identity =
        sha256(b"narada.fresh_process_validation.v1:initialize+tools/list+deterministic_readiness");
    let carrier_root = input
        .installed_carrier_index_path
        .parent()
        .ok_or_else(|| {
            Failure::new(
                "materializer_carrier_root_unresolved",
                path_text(&input.installed_carrier_index_path),
            )
        })?
        .to_path_buf();
    let bundle_commit_pointer_path = carrier_root.join("current-bundle.json");
    let bundle_carriers = input
        .carriers
        .iter()
        .map(|carrier| {
            let sidecar_path = suffix_path(&carrier.config_path, ".narada-generation.json");
            json!({
                "carrier_id": carrier.carrier_id,
                "carrier_kind": carrier.carrier_kind,
                "config_path": path_text(&carrier.config_path),
                "generation_sidecar_path": path_text(&sidecar_path),
            })
        })
        .collect::<Vec<_>>();
    let bundle_unsigned = json!({
        "schema": "narada.carrier_generation_bundle.v1",
        "consistency_domain": "selected_carrier_bundle",
        "materialization_input_digest": materialization_input_digest,
        "artifact_build_set_path": path_text(&input.artifact_build_set_path),
        "artifact_build_set_fingerprint": input.artifact_build_set_fingerprint,
        "artifact_manifest_path": path_text(&input.artifact_manifest_path),
        "artifact_manifest_fingerprint": input.artifact_manifest_fingerprint,
        "validation_policy_identity": validation_policy_identity,
        "carriers": bundle_carriers,
    });
    let bundle_fingerprint = canonical_json_sha256(&bundle_unsigned)
        .map_err(|error| Failure::new("materializer_bundle_fingerprint_failed", error))?;
    let bundle_id = bundle_fingerprint.clone();
    let bundle_path = carrier_root
        .join("bundles")
        .join(&bundle_id)
        .join("bundle.json");
    let mut bundle = bundle_unsigned;
    bundle
        .as_object_mut()
        .expect("bundle is an object")
        .insert("bundle_id".to_string(), Value::String(bundle_id.clone()));
    bundle.as_object_mut().expect("bundle is an object").insert(
        "bundle_fingerprint".to_string(),
        Value::String(bundle_fingerprint.clone()),
    );
    let migration_provenance = if input.carriers.iter().any(|carrier| {
        let sidecar = suffix_path(&carrier.config_path, ".narada-generation.json");
        read_json(&sidecar, "materializer_generation_invalid")
            .ok()
            .and_then(|value| {
                value
                    .get("schema")
                    .and_then(Value::as_str)
                    .map(str::to_string)
            })
            .is_some_and(|schema| {
                schema == LEGACY_GENERATION_SCHEMA || schema == AMBIGUOUS_GENERATION_SCHEMA
            })
    }) {
        "legacy_baseline_untrusted"
    } else {
        "native_v3"
    };
    let mut publications = Vec::new();
    let mut index_carriers = Vec::new();
    for carrier in &input.carriers {
        let plan_path = suffix_path(&carrier.config_path, ".narada-runtime-plan.json");
        let sidecar_path = suffix_path(&carrier.config_path, ".narada-generation.json");
        let desired = emit_carrier(carrier)?;
        let selectors = if matches!(carrier.carrier_kind, CarrierKind::Codex) {
            codex_managed_selectors(
                carrier.servers.iter().map(|server| &server.name),
                carrier.codex_plugin_overrides.keys(),
                carrier.trust_projects.iter(),
            )
        } else {
            vec![]
        };
        let config = if matches!(carrier.carrier_kind, CarrierKind::Codex) {
            let previous_selectors = previous_managed_selectors(&sidecar_path)?;
            let existing = fs::read(&carrier.config_path).ok();
            merge_codex_configuration(
                existing.as_deref(),
                &desired,
                &previous_selectors,
                &selectors,
            )
            .map_err(|error| {
                Failure::new("materializer_codex_merge_failed", error).with_details(json!({
                    "config_path": path_text(&carrier.config_path),
                    "mutation_status": "not_started",
                }))
            })?
        } else {
            desired
        };
        let carrier_kind = match carrier.carrier_kind {
            CarrierKind::Codex => "codex",
            CarrierKind::Kimi => "kimi",
            CarrierKind::Opencode => "opencode",
            CarrierKind::Pi => "pi",
        };
        let description = describe_config(carrier_kind, &config, &selectors)
            .map_err(|error| Failure::new("materializer_contract_describe_failed", error))?;
        let plan_unsigned = json!({
            "schema": "narada.runtime_materialization_plan.v1",
            "status": "accepted",
            "runtime_profile_kind": input.runtime_profile_kind,
            "source": {
                "authority": "narada.runtime_implementation_matrix",
                "matrix_fingerprint": input.runtime_implementation_matrix_fingerprint,
                "carrier_contract_path": path_text(&input.carrier_contract_path),
                "carrier_contract_fingerprint": input.carrier_contract_fingerprint,
                "artifact_build_set_path": path_text(&input.artifact_build_set_path),
                "artifact_build_set_fingerprint": input.artifact_build_set_fingerprint,
            },
            "carrier_id": carrier.carrier_id,
            "servers": carrier.servers.iter().map(|server| json!({
                "binding_id":server.binding_id.as_deref().unwrap_or(&server.name),
                "name":server.name,
                "source_server_key":server.source_server_key,
                "command":server.command,
                "args":server.args,
            })).collect::<Vec<_>>(),
        });
        let plan_hash = sha256(&serde_json::to_vec(&plan_unsigned).map_err(json_failure)?);
        let mut plan = plan_unsigned;
        plan.as_object_mut().expect("plan is an object").insert(
            "plan_fingerprint".to_string(),
            Value::String(plan_hash.clone()),
        );
        let mut generation = Generation {
            schema: GENERATION_SCHEMA,
            contract_version: CONTRACT_VERSION,
            carrier_id: carrier.carrier_id.clone(),
            carrier_kind: carrier.carrier_kind,
            config_path: path_text(&carrier.config_path),
            config_artifact: description.config_artifact,
            managed_projection: description.managed_projection,
            materialization_contract_entrypoint: path_text(&input.registrar_entrypoint),
            materialization_contract_fingerprint: input.registrar_fingerprint.clone(),
            artifact_manifest_path: path_text(&input.artifact_manifest_path),
            artifact_manifest_fingerprint: input.artifact_manifest_fingerprint.clone(),
            artifact_build_set_path: path_text(&input.artifact_build_set_path),
            artifact_build_set_fingerprint: input.artifact_build_set_fingerprint.clone(),
            materialization_input_digest: materialization_input_digest.clone(),
            bundle_id: bundle_id.clone(),
            bundle_path: path_text(&bundle_path),
            bundle_commit_pointer_path: path_text(&bundle_commit_pointer_path),
            bundle_fingerprint: bundle_fingerprint.clone(),
            launch_catalog_fingerprint: plan_hash.clone(),
            validation_policy_identity: validation_policy_identity.clone(),
            migration_provenance: migration_provenance.to_string(),
            runtime_profile_kind: input.runtime_profile_kind.clone(),
            runtime_materialization_plan_path: path_text(&plan_path),
            runtime_materialization_plan_fingerprint: plan_hash,
            runtime_implementation_matrix_path: path_text(
                &input.runtime_implementation_matrix_path,
            ),
            runtime_implementation_matrix_fingerprint: input
                .runtime_implementation_matrix_fingerprint
                .clone(),
            registrar_entrypoint: path_text(&input.registrar_entrypoint),
            registrar_fingerprint: input.registrar_fingerprint.clone(),
            proxy_implementation: input.proxy_implementation.clone(),
            proxy_entrypoint: path_text(&input.proxy_entrypoint),
            proxy_fingerprint: input.proxy_fingerprint.clone(),
            server_count: carrier.servers.len(),
            proxy_count: carrier.servers.len(),
            generated_at: generated_at.clone(),
            generation_fingerprint: String::new(),
        };
        let mut unsigned_generation = serde_json::to_value(&generation).map_err(json_failure)?;
        unsigned_generation
            .as_object_mut()
            .expect("generation is an object")
            .remove("generation_fingerprint");
        let generation_fingerprint = generation_fingerprint(&unsigned_generation)
            .map_err(|error| Failure::new("materializer_generation_fingerprint_failed", error))?;
        generation.generation_fingerprint = generation_fingerprint.clone();
        publications.push(Publication {
            path: carrier.config_path.clone(),
            content: config,
        });
        publications.push(Publication {
            path: plan_path,
            content: pretty_json(&plan)?,
        });
        publications.push(Publication {
            path: sidecar_path.clone(),
            content: pretty_json(&serde_json::to_value(&generation).map_err(json_failure)?)?,
        });
        match (
            &carrier.binding_admission_path,
            &carrier.binding_admission_envelope,
        ) {
            (Some(path), Some(envelope)) => publications.push(Publication {
                path: path.clone(),
                content: pretty_json(envelope)?,
            }),
            (None, None) => {}
            _ => {
                return Err(Failure::new(
                    "materializer_binding_admission_incomplete",
                    carrier.carrier_id.clone(),
                ))
            }
        }
        index_carriers.push(json!({
            "carrier_id": carrier.carrier_id,
            "carrier_kind": carrier.carrier_kind,
            "config_path": path_text(&carrier.config_path),
            "generation_sidecar_path": path_text(&sidecar_path),
            "materialization_generation_fingerprint": generation_fingerprint,
            "bundle_id": bundle_id,
        }));
    }
    publications.push(Publication {
        path: bundle_path.clone(),
        content: pretty_json(&bundle)?,
    });
    publications.push(Publication {
        path: input.installed_carrier_index_path.clone(),
        content: pretty_json(&json!({
            "schema": "narada.installed_carrier_index.v1",
            "workspace_root": path_text(&input.workspace_root),
            "carrier_contract_path": path_text(&input.carrier_contract_path),
            "carrier_contract_fingerprint": input.carrier_contract_fingerprint,
            "artifact_manifest_path": path_text(&input.artifact_manifest_path),
            "artifact_build_set_path": path_text(&input.artifact_build_set_path),
            "artifact_build_set_fingerprint": input.artifact_build_set_fingerprint,
            "bundle_id": bundle_id,
            "bundle_path": path_text(&bundle_path),
            "bundle_fingerprint": bundle_fingerprint,
            "bundle_commit_pointer_path": path_text(&bundle_commit_pointer_path),
            "carriers": index_carriers,
        }))?,
    });
    if require_fresh_validation {
        fresh_process_validate(&input)?;
    }
    let commit_pointer = json!({
        "schema": "narada.carrier_generation_bundle_pointer.v1",
        "bundle_id": bundle_id,
        "bundle_path": path_text(&bundle_path),
        "bundle_fingerprint": bundle_fingerprint,
        "committed_at": generated_at,
    });
    publications.push(Publication {
        path: bundle_commit_pointer_path.clone(),
        content: pretty_json(&commit_pointer)?,
    });
    let transaction = durable_bundle_publish(
        &publications,
        &carrier_root,
        &bundle_commit_pointer_path,
        &bundle_id,
    )?;
    Ok(json!({
        "schema": "narada.materialization_operation_result.v1",
        "status": "committed",
        "bundle_id": bundle_id,
        "bundle_path": path_text(&bundle_path),
        "bundle_commit_pointer_path": path_text(&bundle_commit_pointer_path),
        "carrier_count": input.carriers.len(),
        "installed_carrier_index_path": path_text(&input.installed_carrier_index_path),
        "transaction": transaction,
        "restart_required": true,
        "restart_scope": "selected_carrier_bundle",
    }))
}

fn verify_artifact_build_set(input: &MaterializationInput) -> Result<(), Failure> {
    let mut build_set = read_json(
        &input.artifact_build_set_path,
        "materializer_artifact_build_set_invalid",
    )?;
    if build_set.get("schema").and_then(Value::as_str) != Some("narada.artifact_build_set.v1")
        || build_set.get("assurance").and_then(Value::as_str) != Some("declared_isolated_closure")
    {
        return Err(Failure::new(
            "materializer_artifact_build_set_schema_unsupported",
            path_text(&input.artifact_build_set_path),
        ));
    }
    let expected_digest = json_field_string(&build_set, "build_set_digest")?.to_string();
    if expected_digest != input.artifact_build_set_fingerprint {
        return Err(Failure::new(
            "materializer_artifact_build_set_identity_mismatch",
            path_text(&input.artifact_build_set_path),
        ));
    }
    let unsigned = build_set
        .as_object_mut()
        .ok_or_else(|| Failure::new("materializer_artifact_build_set_invalid", "not_object"))?;
    unsigned.remove("build_set_digest");
    unsigned.remove("generated_at");
    let actual_digest = format!(
        "sha256:{}",
        canonical_json_sha256(&build_set)
            .map_err(|error| Failure::new("materializer_artifact_build_set_invalid", error))?
    );
    if actual_digest != expected_digest {
        return Err(Failure::new(
            "materializer_artifact_build_set_fingerprint_mismatch",
            path_text(&input.artifact_build_set_path),
        ));
    }
    let manifest_path = json_field_string(&build_set, "workspace_manifest_path")?;
    if !path_eq(Path::new(manifest_path), &input.artifact_manifest_path)
        || build_set
            .get("workspace_manifest_fingerprint")
            .and_then(Value::as_str)
            != input.artifact_manifest_fingerprint.as_deref()
    {
        return Err(Failure::new(
            "materializer_artifact_build_set_manifest_mismatch",
            manifest_path,
        ));
    }
    let manifest_bytes = fs::read(&input.artifact_manifest_path).map_err(|error| {
        Failure::new(
            "materializer_artifact_manifest_read_failed",
            error.to_string(),
        )
    })?;
    if build_set
        .get("workspace_manifest_bytes_digest")
        .and_then(Value::as_str)
        != Some(format!("sha256:{}", sha256(&manifest_bytes)).as_str())
    {
        return Err(Failure::new(
            "materializer_artifact_build_set_manifest_bytes_mismatch",
            path_text(&input.artifact_manifest_path),
        ));
    }
    let artifacts = build_set
        .get("artifacts")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            Failure::new(
                "materializer_artifact_build_set_artifacts_required",
                path_text(&input.artifact_build_set_path),
            )
        })?;
    let mut declared = BTreeMap::<String, String>::new();
    for artifact in artifacts {
        let path = PathBuf::from(json_field_string(artifact, "path")?);
        let expected = json_field_string(artifact, "sha256")?;
        let size = artifact
            .get("size")
            .and_then(Value::as_u64)
            .ok_or_else(|| {
                Failure::new(
                    "materializer_artifact_build_set_size_required",
                    path_text(&path),
                )
            })?;
        let bytes = fs::read(&path).map_err(|error| {
            Failure::new(
                "materializer_artifact_build_set_artifact_missing",
                error.to_string(),
            )
            .with_details(json!({"path":path_text(&path)}))
        })?;
        if bytes.len() as u64 != size || format!("sha256:{}", sha256(&bytes)) != expected {
            return Err(Failure::new(
                "materializer_artifact_build_set_artifact_stale",
                path_text(&path),
            ));
        }
        declared.insert(path_text(&path).to_lowercase(), expected.to_string());
    }
    let mut references = BTreeSet::<PathBuf>::new();
    references.insert(input.registrar_entrypoint.clone());
    references.insert(input.proxy_entrypoint.clone());
    for carrier in &input.carriers {
        for server in &carrier.servers {
            let command = PathBuf::from(&server.command);
            if command.is_absolute() {
                references.insert(command);
            }
            for index in 0..server.args.len().saturating_sub(1) {
                if matches!(
                    server.args[index].as_str(),
                    "--child-command"
                        | "--entrypoint"
                        | "--registrar-command"
                        | "--registrar-entrypoint"
                ) {
                    let reference = PathBuf::from(&server.args[index + 1]);
                    if reference.is_absolute() {
                        references.insert(reference);
                    }
                }
            }
        }
    }
    let missing = references
        .iter()
        .filter(|path| !declared.contains_key(&path_text(path).to_lowercase()))
        .map(|path| path_text(path))
        .collect::<Vec<_>>();
    if !missing.is_empty() {
        return Err(Failure::new(
            "materializer_artifact_build_set_reference_missing",
            "A launch reference is absent from the sealed build set.",
        )
        .with_details(json!({"missing_references":missing})));
    }
    Ok(())
}

fn fresh_process_validate(input: &MaterializationInput) -> Result<(), Failure> {
    let mut validated = BTreeSet::<String>::new();
    for carrier in &input.carriers {
        for server in &carrier.servers {
            let descriptor = serde_json::to_vec(&json!({
                "command": server.command,
                "args": server.args,
            }))
            .map_err(json_failure)?;
            let descriptor_digest = sha256(&descriptor);
            if !validated.insert(descriptor_digest.clone()) {
                continue;
            }
            validate_launch_descriptor(server).map_err(|failure| {
                failure.with_details(json!({
                    "carrier_id": carrier.carrier_id,
                    "server_name": server.name,
                    "descriptor_digest": descriptor_digest,
                    "scope": "fresh_process_validation",
                }))
            })?;
        }
    }
    Ok(())
}

fn validate_launch_descriptor(server: &ServerInput) -> Result<(), Failure> {
    let mut validation_args = Vec::<String>::new();
    let mut arguments = server.args.iter();
    while let Some(argument) = arguments.next() {
        if matches!(
            argument.as_str(),
            "--materialization-sidecar" | "--binding-admission-path" | "--binding-admission-digest"
        ) {
            arguments.next();
            continue;
        }
        validation_args.push(argument.clone());
    }
    let mut child = Command::new(&server.command)
        .args(&validation_args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|error| {
            Failure::new("materializer_fresh_process_spawn_failed", error.to_string())
        })?;
    let modern_meta = json!({
        "io.modelcontextprotocol/protocolVersion":"2026-07-28",
        "io.modelcontextprotocol/clientInfo":{"name":"narada-mcp-materializer","version":"0.1.0"},
        "io.modelcontextprotocol/clientCapabilities":{}
    });
    let requests = vec![
        json!({"jsonrpc":"2.0","id":1,"method":"server/discover","params":{"_meta":modern_meta.clone()}}),
        json!({"jsonrpc":"2.0","id":2,"method":"tools/list","params":{"_meta":modern_meta}}),
    ];
    {
        let stdin = child.stdin.as_mut().ok_or_else(|| {
            Failure::new(
                "materializer_fresh_process_stdin_missing",
                server.name.clone(),
            )
        })?;
        for request in requests {
            let bytes = serde_json::to_vec(&request).map_err(json_failure)?;
            stdin.write_all(&bytes).map_err(|error| {
                Failure::new("materializer_fresh_process_write_failed", error.to_string())
            })?;
            stdin.write_all(b"\n").map_err(|error| {
                Failure::new("materializer_fresh_process_write_failed", error.to_string())
            })?;
        }
        stdin.flush().map_err(|error| {
            Failure::new("materializer_fresh_process_write_failed", error.to_string())
        })?;
    }
    let stdout = child.stdout.take().ok_or_else(|| {
        Failure::new(
            "materializer_fresh_process_stdout_missing",
            server.name.clone(),
        )
    })?;
    let (sender, receiver) = mpsc::channel::<String>();
    thread::spawn(move || {
        let mut reader = BufReader::new(stdout);
        loop {
            let mut line = String::new();
            match reader.read_line(&mut line) {
                Ok(0) | Err(_) => break,
                Ok(_) => {
                    if sender.send(line).is_err() {
                        break;
                    }
                }
            }
        }
    });
    let timeout = StdDuration::from_secs(server.startup_timeout_sec.unwrap_or(30).clamp(5, 120));
    let deadline = Instant::now() + timeout;
    let mut initialized = false;
    let mut tools_listed = false;
    while Instant::now() < deadline && !(initialized && tools_listed) {
        if child
            .try_wait()
            .map_err(|error| {
                Failure::new("materializer_fresh_process_wait_failed", error.to_string())
            })?
            .is_some()
        {
            break;
        }
        match receiver.recv_timeout(StdDuration::from_millis(100)) {
            Ok(line) => {
                let trimmed = line.trim();
                if !trimmed.starts_with('{') {
                    continue;
                }
                let Ok(response) = serde_json::from_str::<Value>(trimmed) else {
                    continue;
                };
                if response.get("id").and_then(Value::as_i64) == Some(1) {
                    initialized = response
                        .pointer("/result/resultType")
                        .and_then(Value::as_str)
                        == Some("complete")
                        && response
                            .pointer("/result/supportedVersions")
                            .and_then(Value::as_array)
                            .is_some_and(|versions| {
                                versions
                                    .iter()
                                    .any(|version| version.as_str() == Some("2026-07-28"))
                            });
                }
                if response.get("id").and_then(Value::as_i64) == Some(2)
                    && response
                        .pointer("/result/tools")
                        .and_then(Value::as_array)
                        .is_some()
                {
                    tools_listed = true;
                }
                if response.get("error").is_some() {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(Failure::new(
                        "materializer_fresh_process_protocol_refused",
                        server.name.clone(),
                    )
                    .with_details(json!({"response":response})));
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
    let _ = child.kill();
    let _ = child.wait();
    if !initialized || !tools_listed {
        return Err(Failure::new(
            "materializer_fresh_process_validation_failed",
            server.name.clone(),
        )
        .with_details(json!({
            "protocol_discovery_succeeded":initialized,
            "protocol_mode":"2026-07-28",
            "tools_list_succeeded":tools_listed,
            "timeout_seconds":timeout.as_secs(),
        })));
    }
    Ok(())
}

fn acquire_publication_lock(carrier_root: &Path) -> Result<fs::File, Failure> {
    let lock_root = carrier_root.join("locks");
    fs::create_dir_all(&lock_root).map_err(|error| {
        Failure::new(
            "materializer_publication_lock_directory_failed",
            error.to_string(),
        )
    })?;
    let lock_path = lock_root.join("carrier-publication.lock");
    let mut lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .open(&lock_path)
        .map_err(|error| {
            Failure::new(
                "materializer_publication_lock_open_failed",
                error.to_string(),
            )
        })?;
    lock.try_lock_exclusive().map_err(|error| {
        Failure::new("materializer_publication_locked", error.to_string())
            .with_details(json!({"lock_path": path_text(&lock_path)}))
    })?;
    lock.set_len(0).map_err(|error| {
        Failure::new(
            "materializer_publication_lock_metadata_failed",
            error.to_string(),
        )
    })?;
    lock.write_all(
        format!(
            "{{\"schema\":\"narada.carrier_publication_lock.v1\",\"pid\":{},\"acquired_at\":{}}}\n",
            std::process::id(),
            serde_json::to_string(
                &OffsetDateTime::now_utc()
                    .format(&Rfc3339)
                    .unwrap_or_default()
            )
            .unwrap_or_else(|_| "\"\"".to_string())
        )
        .as_bytes(),
    )
    .map_err(|error| {
        Failure::new(
            "materializer_publication_lock_metadata_failed",
            error.to_string(),
        )
    })?;
    lock.sync_all().map_err(|error| {
        Failure::new(
            "materializer_publication_lock_metadata_failed",
            error.to_string(),
        )
    })?;
    Ok(lock)
}

fn durable_bundle_publish(
    publications: &[Publication],
    carrier_root: &Path,
    commit_pointer_path: &Path,
    bundle_id: &str,
) -> Result<Value, Failure> {
    if publications
        .last()
        .is_none_or(|publication| !path_eq(&publication.path, commit_pointer_path))
    {
        return Err(Failure::new(
            "materializer_commit_pointer_not_last",
            path_text(commit_pointer_path),
        ));
    }
    let _publication_lock = acquire_publication_lock(carrier_root)?;
    recover_pending_transactions(carrier_root)?;
    let current_pointer_hash = fs::read(commit_pointer_path)
        .ok()
        .map(|bytes| sha256(&bytes))
        .unwrap_or_else(|| "absent".to_string());
    let transaction_id = sha256(
        format!(
            "{bundle_id}:{current_pointer_hash}:{}:{}",
            std::process::id(),
            OffsetDateTime::now_utc().unix_timestamp_nanos()
        )
        .as_bytes(),
    );
    let transaction_root = carrier_root.join("transactions").join(&transaction_id);
    fs::create_dir_all(&transaction_root).map_err(|error| {
        Failure::new(
            "materializer_transaction_directory_failed",
            error.to_string(),
        )
    })?;
    for publication in publications {
        if !same_volume(&transaction_root, &publication.path) {
            return Err(Failure::new(
                "materializer_transaction_cross_volume_refused",
                path_text(&publication.path),
            )
            .with_details(json!({"transaction_root":path_text(&transaction_root)})));
        }
    }
    let candidate_root = transaction_root.join("candidates");
    let preimage_root = transaction_root.join("preimages");
    fs::create_dir_all(&candidate_root).map_err(|error| {
        Failure::new(
            "materializer_transaction_directory_failed",
            error.to_string(),
        )
    })?;
    fs::create_dir_all(&preimage_root).map_err(|error| {
        Failure::new(
            "materializer_transaction_directory_failed",
            error.to_string(),
        )
    })?;
    let mut items = Vec::new();
    let mut snapshots = Vec::new();
    for (index, publication) in publications.iter().enumerate() {
        let preimage = fs::read(&publication.path).ok();
        let candidate_path = candidate_root.join(format!("{index}.bin"));
        atomic_write(&candidate_path, &publication.content).map_err(|error| {
            Failure::new(
                "materializer_transaction_candidate_write_failed",
                error.to_string(),
            )
        })?;
        let preimage_path = preimage
            .as_ref()
            .map(|content| {
                let path = preimage_root.join(format!("{index}.bin"));
                atomic_write(&path, content).map(|_| path).map_err(|error| {
                    Failure::new(
                        "materializer_transaction_preimage_write_failed",
                        error.to_string(),
                    )
                })
            })
            .transpose()?;
        items.push(json!({
            "order": index,
            "path": path_text(&publication.path),
            "candidate_path": path_text(&candidate_path),
            "candidate_sha256": sha256(&publication.content),
            "preimage_path": preimage_path.as_ref().map(|path|path_text(path)),
            "preimage_sha256": preimage.as_ref().map(|content|sha256(content)),
            "state": "prepared",
        }));
        snapshots.push(Snapshot {
            path: publication.path.clone(),
            content: preimage,
        });
    }
    let journal_path = transaction_root.join("journal.json");
    let mut journal = json!({
        "schema": "narada.carrier_generation_transaction.v1",
        "transaction_id": transaction_id,
        "bundle_id": bundle_id,
        "state": "prepared",
        "commit_pointer_path": path_text(commit_pointer_path),
        "items": items,
        "threat_model": "cooperating_same_user_processes_and_crash_recovery",
    });
    write_transaction_journal(&journal_path, &journal)?;
    journal["state"] = json!("promoting");
    write_transaction_journal(&journal_path, &journal)?;
    for (index, publication) in publications.iter().enumerate() {
        let current = fs::read(&publication.path).ok();
        let preimage = &snapshots[index].content;
        if current.as_deref() != preimage.as_deref()
            && current.as_deref() != Some(publication.content.as_slice())
        {
            journal["state"] = json!("blocked_recovery");
            write_transaction_journal(&journal_path, &journal)?;
            return Err(Failure::new(
                "materializer_transaction_cas_conflict",
                path_text(&publication.path),
            )
            .with_details(json!({"transaction_id":transaction_id})));
        }
        if current.as_deref() != Some(publication.content.as_slice()) {
            if let Err(error) = atomic_write(&publication.path, &publication.content) {
                journal["state"] = json!("recovery_required");
                write_transaction_journal(&journal_path, &journal)?;
                if index + 1 < publications.len() {
                    let rollback_errors = rollback(&snapshots);
                    if rollback_errors.is_empty() {
                        journal["state"] = json!("aborted");
                        write_transaction_journal(&journal_path, &journal)?;
                    }
                }
                return Err(
                    Failure::new("materializer_transaction_failed", error.to_string())
                        .with_details(json!({
                            "failed_path":path_text(&publication.path),
                            "transaction_id":transaction_id,
                        })),
                );
            }
        }
        let installed = fs::read(&publication.path).map_err(|error| {
            Failure::new("materializer_transaction_verify_failed", error.to_string())
        })?;
        if sha256(&installed) != sha256(&publication.content) {
            journal["state"] = json!("recovery_required");
            write_transaction_journal(&journal_path, &journal)?;
            return Err(Failure::new(
                "materializer_transaction_verify_failed",
                path_text(&publication.path),
            ));
        }
        journal["items"][index]["state"] = json!("published");
        write_transaction_journal(&journal_path, &journal)?;
        if env::var("NARADA_MATERIALIZER_CRASH_AFTER_WRITE")
            .ok()
            .and_then(|value| value.parse::<usize>().ok())
            == Some(index + 1)
        {
            std::process::exit(86);
        }
    }
    journal["state"] = json!("committed");
    write_transaction_journal(&journal_path, &journal)?;
    Ok(json!({
        "schema":"narada.carrier_generation_transaction_result.v1",
        "status":"committed",
        "transaction_id":transaction_id,
        "journal_path":path_text(&journal_path),
        "publication_count":publications.len(),
    }))
}

fn write_transaction_journal(path: &Path, journal: &Value) -> Result<(), Failure> {
    let content = pretty_json(journal)?;
    atomic_write(path, &content).map_err(|error| {
        Failure::new(
            "materializer_transaction_journal_write_failed",
            error.to_string(),
        )
    })?;
    let installed = fs::read(path).map_err(|error| {
        Failure::new(
            "materializer_transaction_journal_verify_failed",
            error.to_string(),
        )
    })?;
    if installed != content {
        return Err(Failure::new(
            "materializer_transaction_journal_verify_failed",
            path_text(path),
        ));
    }
    Ok(())
}

fn recover_pending_transactions(carrier_root: &Path) -> Result<Value, Failure> {
    let transactions_root = carrier_root.join("transactions");
    if !transactions_root.exists() {
        return Ok(json!({"status":"nothing_to_recover","recovered":[]}));
    }
    let mut recovered = Vec::new();
    for entry in fs::read_dir(&transactions_root).map_err(|error| {
        Failure::new(
            "materializer_transaction_inventory_failed",
            error.to_string(),
        )
    })? {
        let entry = entry.map_err(|error| {
            Failure::new(
                "materializer_transaction_inventory_failed",
                error.to_string(),
            )
        })?;
        if !entry.file_type().map(|kind| kind.is_dir()).unwrap_or(false) {
            continue;
        }
        let journal_path = entry.path().join("journal.json");
        if !journal_path.exists() {
            continue;
        }
        let mut journal = read_json(&journal_path, "materializer_transaction_journal_invalid")?;
        let state = journal.get("state").and_then(Value::as_str).unwrap_or("");
        if matches!(state, "committed" | "aborted") {
            continue;
        }
        let commit_pointer_path =
            PathBuf::from(json_field_string(&journal, "commit_pointer_path")?);
        let items = journal
            .get("items")
            .and_then(Value::as_array)
            .cloned()
            .ok_or_else(|| {
                Failure::new(
                    "materializer_transaction_journal_invalid",
                    path_text(&journal_path),
                )
            })?;
        let commit_item = items
            .iter()
            .find(|item| {
                item.get("path")
                    .and_then(Value::as_str)
                    .is_some_and(|path| path_eq(Path::new(path), &commit_pointer_path))
            })
            .ok_or_else(|| {
                Failure::new(
                    "materializer_transaction_commit_item_missing",
                    path_text(&journal_path),
                )
            })?;
        let pointer_committed = fs::read(&commit_pointer_path).ok().is_some_and(|bytes| {
            commit_item.get("candidate_sha256").and_then(Value::as_str)
                == Some(sha256(&bytes).as_str())
        });
        let ordered: Box<dyn Iterator<Item = &Value>> = if pointer_committed {
            Box::new(items.iter())
        } else {
            Box::new(items.iter().rev())
        };
        for item in ordered {
            let target = PathBuf::from(json_field_string(item, "path")?);
            let candidate_path = PathBuf::from(json_field_string(item, "candidate_path")?);
            let candidate = fs::read(&candidate_path).map_err(|error| {
                Failure::new(
                    "materializer_transaction_candidate_missing",
                    error.to_string(),
                )
            })?;
            let preimage_path = item.get("preimage_path").and_then(Value::as_str);
            let preimage = preimage_path
                .map(|path| {
                    fs::read(path).map_err(|error| {
                        Failure::new(
                            "materializer_transaction_preimage_missing",
                            error.to_string(),
                        )
                    })
                })
                .transpose()?;
            let current = fs::read(&target).ok();
            if current.as_deref() != Some(candidate.as_slice())
                && current.as_deref() != preimage.as_deref()
            {
                journal["state"] = json!("blocked_recovery");
                write_transaction_journal(&journal_path, &journal)?;
                return Err(Failure::new(
                    "materializer_transaction_recovery_cas_conflict",
                    path_text(&target),
                )
                .with_details(json!({"journal_path":path_text(&journal_path)})));
            }
            let desired = if pointer_committed {
                Some(candidate.as_slice())
            } else {
                preimage.as_deref()
            };
            match desired {
                Some(content) if current.as_deref() != Some(content) => {
                    atomic_write(&target, content).map_err(|error| {
                        Failure::new(
                            "materializer_transaction_recovery_write_failed",
                            error.to_string(),
                        )
                    })?;
                }
                None if current.is_some() => {
                    fs::remove_file(&target).map_err(|error| {
                        Failure::new(
                            "materializer_transaction_recovery_remove_failed",
                            error.to_string(),
                        )
                    })?;
                }
                _ => {}
            }
        }
        journal["state"] = json!(if pointer_committed {
            "committed"
        } else {
            "aborted"
        });
        write_transaction_journal(&journal_path, &journal)?;
        recovered.push(json!({
            "transaction_id":journal.get("transaction_id"),
            "resolution":journal.get("state"),
        }));
    }
    Ok(json!({"status":"recovered","recovered":recovered}))
}

fn same_volume(left: &Path, right: &Path) -> bool {
    #[cfg(windows)]
    {
        use std::path::Component;
        let prefix = |path: &Path| {
            path.components()
                .next()
                .and_then(|component| match component {
                    Component::Prefix(prefix) => {
                        Some(prefix.as_os_str().to_string_lossy().to_lowercase())
                    }
                    _ => None,
                })
        };
        prefix(left) == prefix(right)
    }
    #[cfg(not(windows))]
    {
        left.is_absolute() == right.is_absolute()
    }
}

fn validate_input(input: &MaterializationInput) -> Result<(), Failure> {
    if input.schema != INPUT_SCHEMA {
        return Err(Failure::new(
            "materializer_input_schema_unsupported",
            format!("Unsupported schema: {}", input.schema),
        ));
    }
    if input.carriers.is_empty() {
        return Err(Failure::new(
            "materializer_carriers_required",
            "At least one carrier is required.",
        ));
    }
    if !input.carrier_contract_path.is_absolute()
        || input.carrier_contract_fingerprint.len() != 64
        || !input
            .carrier_contract_fingerprint
            .chars()
            .all(|value| value.is_ascii_hexdigit())
    {
        return Err(Failure::new(
            "materializer_carrier_contract_source_invalid",
            path_text(&input.carrier_contract_path),
        ));
    }
    if !matches!(
        input.proxy_implementation.as_str(),
        "native" | "bun" | "node"
    ) {
        return Err(Failure::new(
            "materializer_proxy_implementation_invalid",
            "proxy_implementation must be native, bun, or node.",
        ));
    }
    let mut ids = BTreeSet::new();
    let mut paths = BTreeSet::new();
    for carrier in &input.carriers {
        validate_identifier(&carrier.carrier_id, "carrier_id")?;
        if !ids.insert(&carrier.carrier_id) {
            return Err(Failure::new(
                "materializer_carrier_id_duplicate",
                carrier.carrier_id.clone(),
            ));
        }
        if !carrier.config_path.is_absolute() {
            return Err(Failure::new(
                "materializer_config_path_not_absolute",
                path_text(&carrier.config_path),
            ));
        }
        if !paths.insert(&carrier.config_path) {
            return Err(Failure::new(
                "materializer_config_path_duplicate",
                path_text(&carrier.config_path),
            ));
        }
        let mut server_names = BTreeSet::new();
        let mut binding_ids = BTreeSet::new();
        for server in &carrier.servers {
            validate_identifier(&server.name, "server_name")?;
            if !server_names.insert(&server.name) {
                return Err(Failure::new(
                    "materializer_server_name_duplicate",
                    server.name.clone(),
                ));
            }
            if let Some(binding_id) = &server.binding_id {
                validate_identifier(binding_id, "binding_id")?;
                if !binding_ids.insert(binding_id) {
                    return Err(Failure::new(
                        "materializer_binding_id_duplicate",
                        binding_id.clone(),
                    ));
                }
            }
            if server.command.trim().is_empty() {
                return Err(Failure::new(
                    "materializer_server_command_required",
                    server.name.clone(),
                ));
            }
            if server.command.contains('\0') || server.args.iter().any(|arg| arg.contains('\0')) {
                return Err(Failure::new(
                    "materializer_nul_refused",
                    server.name.clone(),
                ));
            }
            validate_protocol_route(carrier, server)?;
            validate_proxy_launch(input, carrier, server)?;
        }
    }
    Ok(())
}

fn validate_protocol_route(carrier: &CarrierInput, server: &ServerInput) -> Result<(), Failure> {
    let carrier_protocol = "2026-07-28";
    let proxy_accepted_client_protocols = ["2026-07-28"];
    let proxy_emitted_server_protocol = carrier_protocol;
    let server_accepted_protocols: &[&str] = &["2026-07-28"];
    let translation_adapter: Option<&str> = None;
    let valid = proxy_accepted_client_protocols.contains(&carrier_protocol)
        && server_accepted_protocols.contains(&proxy_emitted_server_protocol);
    if valid {
        return Ok(());
    }
    Err(Failure::new(
        "materializer_protocol_route_incompatible",
        format!("{}:{}", carrier.carrier_id, server.name),
    )
    .with_details(json!({
        "carrier_id":carrier.carrier_id,
        "carrier_kind":carrier.carrier_kind,
        "server_name":server.name,
        "carrier_protocol":carrier_protocol,
        "proxy_accepted_client_protocols":proxy_accepted_client_protocols,
        "proxy_emitted_server_protocol":proxy_emitted_server_protocol,
        "server_accepted_protocols":server_accepted_protocols,
        "translation_adapter":translation_adapter,
        "invariant":"carrier_protocol must be accepted by proxy; proxy-emitted protocol must be accepted by server; a version-changing edge requires an explicitly admitted translation adapter"
    })))
}

fn validate_proxy_launch(
    input: &MaterializationInput,
    carrier: &CarrierInput,
    server: &ServerInput,
) -> Result<(), Failure> {
    let command = PathBuf::from(&server.command);
    if !command.is_absolute() || !path_eq(&command, &input.proxy_entrypoint) {
        return Err(Failure::new(
            "materializer_proxy_command_mismatch",
            server.name.clone(),
        ));
    }
    let required = [
        ("--runtime-contract-version", CONTRACT_VERSION.to_string()),
        (
            "--artifact-manifest",
            path_text(&input.artifact_manifest_path),
        ),
        ("--carrier-id", carrier.carrier_id.clone()),
        (
            "--carrier-kind",
            match carrier.carrier_kind {
                CarrierKind::Codex => "codex",
                CarrierKind::Kimi => "kimi",
                CarrierKind::Opencode => "opencode",
                CarrierKind::Pi => "pi",
            }
            .to_string(),
        ),
        (
            "--registrar-command",
            path_text(&input.registrar_entrypoint),
        ),
        (
            "--registrar-entrypoint",
            path_text(&input.registrar_entrypoint),
        ),
        (
            "--materialization-sidecar",
            path_text(&suffix_path(
                &carrier.config_path,
                ".narada-generation.json",
            )),
        ),
    ];
    for (flag, expected) in required {
        let actual = arg_value(&server.args, flag);
        let equal = if flag.contains("manifest")
            || flag.contains("registrar")
            || flag.contains("sidecar")
        {
            actual
                .map(PathBuf::from)
                .is_some_and(|value| path_eq(&value, Path::new(&expected)))
        } else {
            actual == Some(expected.as_str())
        };
        if !equal {
            return Err(Failure::new(
                "materializer_proxy_argument_mismatch",
                format!("{}:{flag}", server.name),
            )
            .with_details(json!({"expected":expected,"actual":actual})));
        }
    }
    if arg_value(&server.args, "--child-command").is_none()
        || arg_value(&server.args, "--entrypoint").is_none()
    {
        return Err(Failure::new(
            "materializer_proxy_child_contract_incomplete",
            server.name.clone(),
        ));
    }
    Ok(())
}

fn arg_value<'a>(args: &'a [String], flag: &str) -> Option<&'a str> {
    args.iter()
        .position(|arg| arg == flag)
        .and_then(|index| args.get(index + 1))
        .map(String::as_str)
}

fn path_eq(left: &Path, right: &Path) -> bool {
    path_text(left).eq_ignore_ascii_case(&path_text(right))
}

fn validate_identifier(value: &str, field: &'static str) -> Result<(), Failure> {
    let valid = !value.is_empty()
        && value.len() <= 200
        && value
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-'));
    if valid {
        Ok(())
    } else {
        Err(Failure::new(
            "materializer_identifier_invalid",
            format!("{field}:{value}"),
        ))
    }
}

fn emit_carrier(carrier: &CarrierInput) -> Result<Vec<u8>, Failure> {
    match carrier.carrier_kind {
        CarrierKind::Codex => emit_codex(carrier),
        CarrierKind::Kimi => emit_json_carrier(carrier, "mcpServers"),
        CarrierKind::Opencode => emit_json_carrier(carrier, "mcp"),
        CarrierKind::Pi => emit_pi(carrier),
    }
}

fn emit_pi(carrier: &CarrierInput) -> Result<Vec<u8>, Failure> {
    const TEMPLATE: &str = include_str!("../../assets/pi-mcp-extension.ts");
    const PRESENTATION: &str = include_str!("../../assets/mcp-result-presentation.ts");
    const PLACEHOLDER: &str = "__NARADA_PI_MCP_SERVERS__";
    const PRESENTATION_PLACEHOLDER: &str = "__NARADA_MCP_RESULT_PRESENTATION__";
    if TEMPLATE.matches(PLACEHOLDER).count() != 1
        || TEMPLATE.matches(PRESENTATION_PLACEHOLDER).count() != 1
    {
        return Err(Failure::new(
            "materializer_pi_template_invalid",
            "Pi extension template must contain exactly one server and presentation placeholder",
        ));
    }
    let servers = carrier
        .servers
        .iter()
        .filter(|server| {
            matches!(
                server.name.as_str(),
                "agent-context" | "local-filesystem" | "mcp-loader" | "task-lifecycle"
            )
        })
        .map(|server| {
            json!({
                "name": server.name,
                "command": server.command,
                "args": server.args,
                "enabled": server.enabled,
                "startupTimeoutMs": server.startup_timeout_sec.unwrap_or(60) * 1000,
            })
        })
        .collect::<Vec<_>>();
    let encoded = serde_json::to_string(&servers).map_err(json_failure)?;
    Ok(TEMPLATE
        .replace(PRESENTATION_PLACEHOLDER, PRESENTATION)
        .replace(PLACEHOLDER, &encoded)
        .into_bytes())
}

fn emit_json_carrier(carrier: &CarrierInput, field: &str) -> Result<Vec<u8>, Failure> {
    let mut servers = Map::new();
    for server in &carrier.servers {
        let value = match carrier.carrier_kind {
            CarrierKind::Kimi => {
                let mut value = json!({
                    "transport": "stdio",
                    "command": server.command,
                    "args": server.args,
                    "protocolVersion": "2026-07-28",
                });
                if let Some(mode) = &server.approval_mode {
                    value["approval_mode"] = Value::String(mode.clone());
                }
                if !server.env_vars.is_empty() {
                    value["env_vars"] = json!(server.env_vars);
                }
                value
            }
            CarrierKind::Opencode => json!({
                "type": "local",
                "command": std::iter::once(&server.command).chain(server.args.iter()).collect::<Vec<_>>(),
                "enabled": server.enabled,
            }),
            CarrierKind::Codex => unreachable!("Codex uses TOML"),
            CarrierKind::Pi => unreachable!("Pi uses its extension projection"),
        };
        servers.insert(server.name.clone(), value);
    }
    let mut root = Map::new();
    if matches!(carrier.carrier_kind, CarrierKind::Opencode) {
        root.insert(
            "$schema".to_string(),
            Value::String("https://opencode.ai/config.json".to_string()),
        );
    }
    root.insert(field.to_string(), Value::Object(servers));
    let mut output = pretty_json(&Value::Object(root))?;
    if matches!(carrier.carrier_kind, CarrierKind::Opencode) {
        output.splice(
            0..0,
            b"// Narada owns this entire OpenCode carrier document; use materialization to change it.\n".iter().copied(),
        );
    }
    Ok(output)
}

fn emit_codex(carrier: &CarrierInput) -> Result<Vec<u8>, Failure> {
    let mut out = String::from("# Narada manages only recorded MCP and carrier policy settings; other Codex settings are preserved.\n\n# Codex Apps/connectors are opt-in for profile-less launches.\n[features]\napps = false\n\n");
    for (plugin, enabled) in &carrier.codex_plugin_overrides {
        out.push_str(&format!(
            "[plugins.{}]\nenabled = {}\n\n",
            toml_key(plugin),
            enabled
        ));
    }
    for project in &carrier.trust_projects {
        out.push_str(&format!(
            "[projects.{}]\ntrust_level = \"trusted\"\n\n",
            json_string(project)?
        ));
    }
    for server in &carrier.servers {
        out.push_str(&format!("[mcp_servers.{}]\n", toml_key(&server.name)));
        out.push_str(&format!("command = {}\n", json_string(&server.command)?));
        out.push_str(&format!("args = {}\n", string_array(&server.args)?));
        out.push_str(&format!(
            "default_tools_approval_mode = {}\n",
            json_string(server.approval_mode.as_deref().unwrap_or("approve"))?
        ));
        if let Some(timeout) = server.startup_timeout_sec {
            out.push_str(&format!("startup_timeout_sec = {timeout}\n"));
        }
        if !server.env_vars.is_empty() {
            out.push_str(&format!("env_vars = {}\n", string_array(&server.env_vars)?));
        }
        out.push('\n');
    }
    Ok(out.into_bytes())
}

fn toml_key(value: &str) -> String {
    if value
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-'))
    {
        value.to_string()
    } else {
        serde_json::to_string(value).expect("string serialization cannot fail")
    }
}
fn json_string(value: &str) -> Result<String, Failure> {
    serde_json::to_string(value).map_err(json_failure)
}
fn string_array(values: &[String]) -> Result<String, Failure> {
    Ok(format!(
        "[{}]",
        values
            .iter()
            .map(|value| json_string(value))
            .collect::<Result<Vec<_>, _>>()?
            .join(",")
    ))
}
fn transactional_publish(publications: &[Publication]) -> Result<(), Failure> {
    let snapshots = publications
        .iter()
        .map(|item| Snapshot {
            path: item.path.clone(),
            content: fs::read(&item.path).ok(),
        })
        .collect::<Vec<_>>();
    for publication in publications {
        if let Err(error) = atomic_write(&publication.path, &publication.content) {
            let rollback_errors = rollback(&snapshots);
            return Err(
                Failure::new("materializer_transaction_failed", error.to_string()).with_details(
                    json!({
                        "failed_path": path_text(&publication.path),
                        "rollback_errors": rollback_errors,
                    }),
                ),
            );
        }
    }
    Ok(())
}

fn atomic_write(path: &Path, content: &[u8]) -> std::io::Result<()> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent)?;
    let name = path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("materialized");
    let nonce = OffsetDateTime::now_utc().unix_timestamp_nanos();
    let temporary = parent.join(format!(".{name}.narada-{}-{nonce}.tmp", std::process::id()));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary)?;
    file.write_all(content)?;
    file.sync_all()?;
    drop(file);
    replace_file(&temporary, path)?;
    let installed = fs::read(path)?;
    if installed != content {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "durable_replace_verification_failed",
        ));
    }
    Ok(())
}

#[cfg(windows)]
fn replace_file(temporary: &Path, destination: &Path) -> std::io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use std::ptr;
    use windows_sys::Win32::Storage::FileSystem::{
        MoveFileExW, ReplaceFileW, MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH,
        REPLACEFILE_WRITE_THROUGH,
    };
    let wide = |path: &Path| {
        path.as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect::<Vec<_>>()
    };
    let temporary_wide = wide(temporary);
    let destination_wide = wide(destination);
    let mut result = unsafe {
        if destination.exists() {
            ReplaceFileW(
                destination_wide.as_ptr(),
                temporary_wide.as_ptr(),
                ptr::null(),
                REPLACEFILE_WRITE_THROUGH,
                ptr::null_mut(),
                ptr::null_mut(),
            )
        } else {
            MoveFileExW(
                temporary_wide.as_ptr(),
                destination_wide.as_ptr(),
                MOVEFILE_WRITE_THROUGH,
            )
        }
    };
    if result == 0 && destination.exists() {
        result = unsafe {
            MoveFileExW(
                temporary_wide.as_ptr(),
                destination_wide.as_ptr(),
                MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
            )
        };
    }
    if result == 0 {
        let error = std::io::Error::last_os_error();
        let _ = fs::remove_file(temporary);
        return Err(error);
    }
    Ok(())
}

#[cfg(not(windows))]
fn replace_file(temporary: &Path, destination: &Path) -> std::io::Result<()> {
    fs::rename(temporary, destination)
}

fn rollback(snapshots: &[Snapshot]) -> Vec<String> {
    let mut errors = Vec::new();
    for snapshot in snapshots.iter().rev() {
        let result = match &snapshot.content {
            Some(content) => atomic_write(&snapshot.path, content),
            None if snapshot.path.exists() => fs::remove_file(&snapshot.path),
            None => Ok(()),
        };
        if let Err(error) = result {
            errors.push(format!("{}:{error}", path_text(&snapshot.path)));
        }
    }
    errors
}

fn json_failure(error: serde_json::Error) -> Failure {
    Failure::new("materializer_json_failed", error.to_string())
}
fn pretty_json(value: &Value) -> Result<Vec<u8>, Failure> {
    contract_pretty_json(value).map_err(|error| Failure::new("materializer_json_failed", error))
}
fn sha256(value: &[u8]) -> String {
    hex::encode(Sha256::digest(value))
}
fn suffix_path(path: &Path, suffix: &str) -> PathBuf {
    PathBuf::from(format!("{}{suffix}", path.to_string_lossy()))
}
fn path_text(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn fixture(root: &Path) -> MaterializationInput {
        fs::write(root.join("narada-mcp-materializer.exe"), b"materializer").unwrap();
        fs::write(root.join("narada-mcp-runtime.exe"), b"runtime").unwrap();
        fs::write(root.join("child.exe"), b"child").unwrap();
        fs::write(root.join("matrix.json"), b"matrix").unwrap();
        fs::write(root.join("carrier-contract.json"), b"contract").unwrap();
        let manifest = json!({
            "schema":"narada.workspace_artifact_manifest.v1",
            "workspace_root":path_text(root),
            "manifest_fingerprint":"a".repeat(64),
            "packages":[],
            "artifacts":[]
        });
        let manifest_bytes = pretty_json(&manifest).unwrap();
        fs::write(root.join("manifest.json"), &manifest_bytes).unwrap();
        let artifacts = [
            ("narada-mcp-materializer.exe", b"materializer".as_slice()),
            ("narada-mcp-runtime.exe", b"runtime".as_slice()),
            ("child.exe", b"child".as_slice()),
        ]
        .into_iter()
        .map(|(name, bytes)| {
            json!({
                "path":path_text(&root.join(name)),
                "size":bytes.len(),
                "sha256":format!("sha256:{}",sha256(bytes)),
            })
        })
        .collect::<Vec<_>>();
        let build_set_unsigned = json!({
            "schema":"narada.artifact_build_set.v1",
            "assurance":"declared_isolated_closure",
            "workspace_root":path_text(root),
            "workspace_manifest_path":path_text(&root.join("manifest.json")),
            "workspace_manifest_fingerprint":"a".repeat(64),
            "workspace_manifest_bytes_digest":format!("sha256:{}",sha256(&manifest_bytes)),
            "source_closure_digest":format!("sha256:{}", "0".repeat(64)),
            "toolchain":{},
            "ambient_input_classes":[],
            "required_references":[],
            "artifacts":artifacts,
        });
        let build_set_digest = format!(
            "sha256:{}",
            canonical_json_sha256(&build_set_unsigned).unwrap()
        );
        let mut build_set = build_set_unsigned;
        build_set["build_set_digest"] = json!(build_set_digest);
        build_set["generated_at"] = json!("2026-08-12T00:00:00Z");
        fs::write(
            root.join("artifact-build-set.json"),
            pretty_json(&build_set).unwrap(),
        )
        .unwrap();
        MaterializationInput {
            schema: INPUT_SCHEMA.into(),
            workspace_root: root.into(),
            carrier_contract_path: root.join("carrier-contract.json"),
            carrier_contract_fingerprint: "e".repeat(64),
            artifact_manifest_path: root.join("manifest.json"),
            artifact_manifest_fingerprint: Some("a".repeat(64)),
            artifact_build_set_path: root.join("artifact-build-set.json"),
            artifact_build_set_fingerprint: build_set_digest,
            runtime_profile_kind: "native".into(),
            runtime_implementation_matrix_path: root.join("matrix.json"),
            runtime_implementation_matrix_fingerprint: "b".repeat(64),
            registrar_entrypoint: root.join("narada-mcp-materializer.exe"),
            registrar_fingerprint: Some("c".repeat(64)),
            proxy_implementation: "native".into(),
            proxy_entrypoint: root.join("narada-mcp-runtime.exe"),
            proxy_fingerprint: Some("d".repeat(64)),
            installed_carrier_index_path: root.join("installed-carriers.json"),
            carriers: vec![CarrierInput {
                carrier_id: "codex-test".into(),
                carrier_kind: CarrierKind::Codex,
                config_path: root.join("config.toml"),
                codex_plugin_overrides: BTreeMap::new(),
                trust_projects: vec![],
                binding_admission_path: None,
                binding_admission_envelope: None,
                servers: vec![ServerInput {
                    binding_id: Some("fixture-binding".into()),
                    source_server_key: Some("narada-site-fixture".into()),
                    name: "narada-test".into(),
                    command: path_text(&root.join("narada-mcp-runtime.exe")),
                    args: vec![
                        "proxy".into(),
                        "--surface-id".into(),
                        "fixture".into(),
                        "--child-command".into(),
                        path_text(&root.join("child.exe")),
                        "--artifact-manifest".into(),
                        path_text(&root.join("manifest.json")),
                        "--runtime-contract-version".into(),
                        CONTRACT_VERSION.to_string(),
                        "--entrypoint".into(),
                        path_text(&root.join("child.exe")),
                        "--carrier-id".into(),
                        "codex-test".into(),
                        "--carrier-kind".into(),
                        "codex".into(),
                        "--registrar-command".into(),
                        path_text(&root.join("narada-mcp-materializer.exe")),
                        "--registrar-entrypoint".into(),
                        path_text(&root.join("narada-mcp-materializer.exe")),
                        "--materialization-sidecar".into(),
                        path_text(&root.join("config.toml.narada-generation.json")),
                        "--".into(),
                    ],
                    env_vars: vec![],
                    enabled: true,
                    approval_mode: Some("approve".into()),
                    startup_timeout_sec: Some(60),
                    tools: vec![ToolInput {
                        name: "test_show".into(),
                        approval_mode: "approve".into(),
                    }],
                }],
            }],
        }
    }

    #[test]
    fn accepts_modern_kimi_route_to_modern_only_registrar() {
        let root = tempdir().unwrap();
        let mut input = fixture(root.path());
        input.carriers[0].carrier_id = "kimi-test".into();
        input.carriers[0].carrier_kind = CarrierKind::Kimi;
        input.carriers[0].servers[0].name = "mcp-registrar".into();

        let carrier = &input.carriers[0];
        validate_protocol_route(carrier, &carrier.servers[0])
            .expect("modern Kimi route must reach the modern-only Registrar");
    }

    #[test]
    fn emits_pi_extension_with_only_bootstrap_servers() {
        let root = tempdir().unwrap();
        let mut input = fixture(root.path());
        let carrier = &mut input.carriers[0];
        carrier.carrier_kind = CarrierKind::Pi;
        carrier.servers[0].name = "local-filesystem".into();
        carrier.servers[0].startup_timeout_sec = Some(7);
        let mut lazy = carrier.servers[0].clone();
        lazy.name = "narada-marici-git".into();
        carrier.servers.push(lazy);
        let source = String::from_utf8(emit_carrier(carrier).unwrap()).unwrap();
        assert!(source.contains("export default function naradaMcpCarrier"));
        assert!(source.contains("\"name\":\"local-filesystem\""));
        assert!(source.contains("\"startupTimeoutMs\":7000"));
        assert!(!source.contains("narada-marici-git"));
        assert!(source.contains("tools/list"));
        assert!(source.contains("pi.registerTool"));
        assert!(source.contains("Array.isArray(result?.content) && result.content.length > 0"));
        assert!(source.contains("result?.structuredContent !== undefined"));
        assert!(source.contains("MAX_BOOTSTRAP_SCHEMA_CHARS"));
        assert!(!source.contains("__NARADA_PI_MCP_SERVERS__"));
    }

    #[test]
    fn pi_projection_is_whole_document_managed() {
        let root = tempdir().unwrap();
        let mut input = fixture(root.path());
        input.carriers[0].carrier_kind = CarrierKind::Pi;
        let emitted = emit_carrier(&input.carriers[0]).unwrap();
        let description = describe_config("pi", &emitted, &[]).unwrap();
        assert_eq!(description.managed_projection.scope, "whole_document");
        assert_eq!(description.managed_projection.sha256, sha256(&emitted));
    }

    #[test]
    fn materializes_config_sidecars_and_index() {
        let root = tempdir().unwrap();
        let input = fixture(root.path());
        let result = materialize(input, false).unwrap();
        assert_eq!(result["status"], "committed");
        assert!(root.path().join("config.toml").exists());
        assert!(root
            .path()
            .join("config.toml.narada-generation.json")
            .exists());
        assert!(root
            .path()
            .join("config.toml.narada-runtime-plan.json")
            .exists());
        let index: Value =
            serde_json::from_slice(&fs::read(root.path().join("installed-carriers.json")).unwrap())
                .unwrap();
        assert_eq!(index["carriers"][0]["carrier_id"], "codex-test");
    }

    #[test]
    fn recovery_rolls_back_when_the_commit_pointer_was_not_published() {
        let root = tempdir().unwrap();
        let carrier_root = root.path().join("carrier-state");
        let transaction_root = carrier_root.join("transactions").join("crashed");
        let candidates = transaction_root.join("candidates");
        let preimages = transaction_root.join("preimages");
        fs::create_dir_all(&candidates).unwrap();
        fs::create_dir_all(&preimages).unwrap();
        let target = root.path().join("config.toml");
        let pointer = carrier_root.join("current-bundle.json");
        fs::write(&target, b"candidate").unwrap();
        fs::write(candidates.join("0.bin"), b"candidate").unwrap();
        fs::write(preimages.join("0.bin"), b"preimage").unwrap();
        fs::write(candidates.join("1.bin"), b"pointer").unwrap();
        let journal = json!({
            "schema":"narada.carrier_generation_transaction.v1",
            "transaction_id":"crashed",
            "bundle_id":"bundle",
            "state":"promoting",
            "commit_pointer_path":path_text(&pointer),
            "items":[
                {
                    "order":0,
                    "path":path_text(&target),
                    "candidate_path":path_text(&candidates.join("0.bin")),
                    "candidate_sha256":sha256(b"candidate"),
                    "preimage_path":path_text(&preimages.join("0.bin")),
                    "preimage_sha256":sha256(b"preimage"),
                    "state":"published"
                },
                {
                    "order":1,
                    "path":path_text(&pointer),
                    "candidate_path":path_text(&candidates.join("1.bin")),
                    "candidate_sha256":sha256(b"pointer"),
                    "preimage_path":Value::Null,
                    "preimage_sha256":Value::Null,
                    "state":"prepared"
                }
            ]
        });
        fs::write(
            transaction_root.join("journal.json"),
            pretty_json(&journal).unwrap(),
        )
        .unwrap();
        let recovered = recover_pending_transactions(&carrier_root).unwrap();
        assert_eq!(recovered["recovered"][0]["resolution"], "aborted");
        assert_eq!(fs::read(&target).unwrap(), b"preimage");
        assert!(!pointer.exists());
    }

    #[test]
    fn recovery_rolls_forward_when_the_commit_pointer_was_published() {
        let root = tempdir().unwrap();
        let carrier_root = root.path().join("carrier-state");
        let transaction_root = carrier_root.join("transactions").join("crashed");
        let candidates = transaction_root.join("candidates");
        let preimages = transaction_root.join("preimages");
        fs::create_dir_all(&candidates).unwrap();
        fs::create_dir_all(&preimages).unwrap();
        let target = root.path().join("config.toml");
        let pointer = carrier_root.join("current-bundle.json");
        fs::write(&target, b"preimage").unwrap();
        fs::write(candidates.join("0.bin"), b"candidate").unwrap();
        fs::write(preimages.join("0.bin"), b"preimage").unwrap();
        fs::write(candidates.join("1.bin"), b"pointer").unwrap();
        fs::create_dir_all(pointer.parent().unwrap()).unwrap();
        fs::write(&pointer, b"pointer").unwrap();
        let journal = json!({
            "schema":"narada.carrier_generation_transaction.v1",
            "transaction_id":"crashed",
            "bundle_id":"bundle",
            "state":"promoting",
            "commit_pointer_path":path_text(&pointer),
            "items":[
                {
                    "order":0,
                    "path":path_text(&target),
                    "candidate_path":path_text(&candidates.join("0.bin")),
                    "candidate_sha256":sha256(b"candidate"),
                    "preimage_path":path_text(&preimages.join("0.bin")),
                    "preimage_sha256":sha256(b"preimage"),
                    "state":"prepared"
                },
                {
                    "order":1,
                    "path":path_text(&pointer),
                    "candidate_path":path_text(&candidates.join("1.bin")),
                    "candidate_sha256":sha256(b"pointer"),
                    "preimage_path":Value::Null,
                    "preimage_sha256":Value::Null,
                    "state":"published"
                }
            ]
        });
        fs::write(
            transaction_root.join("journal.json"),
            pretty_json(&journal).unwrap(),
        )
        .unwrap();
        let recovered = recover_pending_transactions(&carrier_root).unwrap();
        assert_eq!(recovered["recovered"][0]["resolution"], "committed");
        assert_eq!(fs::read(&target).unwrap(), b"candidate");
        assert_eq!(fs::read(&pointer).unwrap(), b"pointer");
    }

    #[test]
    fn identical_materialization_inputs_reuse_the_semantic_bundle_identity() {
        let root = tempdir().unwrap();
        let first = materialize(fixture(root.path()), false).unwrap();
        let second = materialize(fixture(root.path()), false).unwrap();
        assert_eq!(first["bundle_id"], second["bundle_id"]);
    }

    #[test]
    fn publication_lock_refuses_a_concurrent_writer() {
        let root = tempdir().unwrap();
        let first = acquire_publication_lock(root.path()).unwrap();
        let second = acquire_publication_lock(root.path()).unwrap_err();
        assert_eq!(second.code, "materializer_publication_locked");
        drop(first);
        acquire_publication_lock(root.path()).unwrap();
    }

    #[test]
    fn invalid_input_writes_nothing() {
        let root = tempdir().unwrap();
        let mut input = fixture(root.path());
        input.carriers[0].servers[0].name = "bad name".into();
        assert_eq!(
            materialize(input, false).unwrap_err().code,
            "materializer_identifier_invalid"
        );
        assert!(!root.path().join("config.toml").exists());
        assert!(!root.path().join("installed-carriers.json").exists());
        assert!(!root
            .path()
            .join(".narada")
            .join("carrier-transactions")
            .exists());
    }

    #[test]
    fn publication_failure_restores_every_previous_file() {
        let root = tempdir().unwrap();
        let mut input = fixture(root.path());
        fs::write(
            root.path().join("config.toml"),
            b"model = \"operator-owned-before\"\n",
        )
        .unwrap();
        fs::write(root.path().join("blocked-parent"), b"not-a-directory\n").unwrap();
        input.carriers.push(CarrierInput {
            carrier_id: "kimi-test".into(),
            carrier_kind: CarrierKind::Kimi,
            config_path: root.path().join("blocked-parent").join("mcp.json"),
            codex_plugin_overrides: BTreeMap::new(),
            trust_projects: vec![],
            binding_admission_path: None,
            binding_admission_envelope: None,
            servers: vec![],
        });

        let failure = materialize(input, false).unwrap_err();

        assert_eq!(failure.code, "materializer_transaction_failed");
        assert_eq!(
            fs::read(root.path().join("config.toml")).unwrap(),
            b"model = \"operator-owned-before\"\n"
        );
        assert!(!root
            .path()
            .join("config.toml.narada-generation.json")
            .exists());
        assert!(!root
            .path()
            .join("config.toml.narada-runtime-plan.json")
            .exists());
        assert!(!root.path().join("installed-carriers.json").exists());
    }

    #[test]
    fn malformed_existing_codex_toml_fails_before_writes() {
        let root = tempdir().unwrap();
        let input = fixture(root.path());
        fs::write(root.path().join("config.toml"), b"[broken\n").unwrap();

        let failure = materialize(input, false).unwrap_err();

        assert_eq!(failure.code, "materializer_codex_merge_failed");
        assert_eq!(
            fs::read(root.path().join("config.toml")).unwrap(),
            b"[broken\n"
        );
        assert!(!root
            .path()
            .join("config.toml.narada-generation.json")
            .exists());
    }
}
