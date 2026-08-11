use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::env;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use time::{format_description::well_known::Rfc3339, OffsetDateTime};

mod derive;

const INPUT_SCHEMA: &str = "narada.carrier_materialization_input.v1";
const GENERATION_SCHEMA: &str = "narada.mcp_materialization_generation.v1";

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct MaterializationInput {
    schema: String,
    workspace_root: PathBuf,
    artifact_manifest_path: PathBuf,
    artifact_manifest_fingerprint: Option<String>,
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

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CarrierInput {
    carrier_id: String,
    carrier_kind: CarrierKind,
    config_path: PathBuf,
    #[serde(default)]
    codex_plugin_overrides: BTreeMap<String, bool>,
    #[serde(default)]
    trust_projects: Vec<String>,
    servers: Vec<ServerInput>,
}

#[derive(Debug, Deserialize, Serialize, Clone, Copy)]
#[serde(rename_all = "lowercase")]
enum CarrierKind {
    Codex,
    Kimi,
    Opencode,
}

#[derive(Debug, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
struct ServerInput {
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
    #[serde(default)]
    tools: Vec<ToolInput>,
}

#[derive(Debug, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
struct ToolInput {
    name: String,
    approval_mode: String,
}

#[derive(Debug, Serialize)]
struct Generation {
    schema: &'static str,
    contract_version: u32,
    carrier_id: String,
    carrier_kind: CarrierKind,
    config_path: String,
    config_sha256: String,
    artifact_manifest_path: String,
    artifact_manifest_fingerprint: Option<String>,
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

#[derive(Clone)]
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
        let result = materialize(input)?;
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
    if command == "materialize-site" {
        let options = derive::DeriveOptions::parse(args)?;
        let result = materialize(derive::derive_input(options)?)?;
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
        let materialization = materialize(input)?;
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
    let result = materialize(input)?;
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
    let fields = [
        "schema",
        "contract_version",
        "carrier_id",
        "carrier_kind",
        "config_path",
        "config_sha256",
        "artifact_manifest_path",
        "artifact_manifest_fingerprint",
        "runtime_profile_kind",
        "runtime_materialization_plan_path",
        "runtime_materialization_plan_fingerprint",
        "runtime_implementation_matrix_path",
        "runtime_implementation_matrix_fingerprint",
        "registrar_entrypoint",
        "registrar_fingerprint",
        "proxy_implementation",
        "proxy_entrypoint",
        "proxy_fingerprint",
        "server_count",
        "proxy_count",
        "generated_at",
    ];
    let mut unsigned = Map::new();
    for field in fields {
        unsigned.insert(
            field.to_string(),
            generation.get(field).cloned().unwrap_or(Value::Null),
        );
    }
    if sha256(&serde_json::to_vec(&Value::Object(unsigned)).map_err(json_failure)?) != expected {
        return Err(Failure::new(
            "materializer_generation_fingerprint_mismatch",
            path_text(sidecar_path),
        ));
    }
    if generation.get("contract_version").and_then(Value::as_u64) != Some(6) {
        return Err(Failure::new(
            "materializer_generation_contract_obsolete",
            path_text(sidecar_path),
        ));
    }
    verify_file_fingerprint(generation, "registrar_entrypoint", "registrar_fingerprint")?;
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
    let kind = match json_field_string(generation, "carrier_kind")? {
        "codex" => CarrierKind::Codex,
        "kimi" => CarrierKind::Kimi,
        "opencode" => CarrierKind::Opencode,
        value => return Err(Failure::new("materializer_carrier_kind_unsupported", value)),
    };
    let config = fs::read(&config_path)
        .map_err(|error| Failure::new("materializer_config_read_failed", error.to_string()))?;
    if materialization_config_fingerprint(kind, &config)?
        != json_field_string(generation, "config_sha256")?
    {
        return Err(Failure::new(
            "materializer_config_fingerprint_mismatch",
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
        || sha256(&serde_json::to_vec(&unsigned_plan).map_err(json_failure)?) != expected_plan
    {
        return Err(Failure::new(
            "materializer_runtime_plan_fingerprint_mismatch",
            path_text(&plan_path),
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

fn materialize(input: MaterializationInput) -> Result<Value, Failure> {
    validate_input(&input)?;
    let generated_at = OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .map_err(|error| Failure::new("materializer_clock_failed", error.to_string()))?;
    let mut publications = Vec::new();
    let mut index_carriers = Vec::new();
    for carrier in &input.carriers {
        let config = emit_carrier(carrier)?;
        let config_hash = materialization_config_fingerprint(carrier.carrier_kind, &config)?;
        let plan_path = suffix_path(&carrier.config_path, ".narada-runtime-plan.json");
        let sidecar_path = suffix_path(&carrier.config_path, ".narada-generation.json");
        let plan_unsigned = json!({
            "schema": "narada.runtime_materialization_plan.v1",
            "status": "accepted",
            "runtime_profile_kind": input.runtime_profile_kind,
            "source": {
                "authority": "narada.runtime_implementation_matrix",
                "matrix_fingerprint": input.runtime_implementation_matrix_fingerprint,
            },
            "carrier_id": carrier.carrier_id,
            "servers": carrier.servers.iter().map(|server| json!({"name":server.name,"command":server.command,"args":server.args})).collect::<Vec<_>>(),
        });
        let plan_hash = sha256(&serde_json::to_vec(&plan_unsigned).map_err(json_failure)?);
        let mut plan = plan_unsigned;
        plan.as_object_mut().expect("plan is an object").insert(
            "plan_fingerprint".to_string(),
            Value::String(plan_hash.clone()),
        );
        let mut generation = Generation {
            schema: GENERATION_SCHEMA,
            contract_version: 6,
            carrier_id: carrier.carrier_id.clone(),
            carrier_kind: carrier.carrier_kind,
            config_path: path_text(&carrier.config_path),
            config_sha256: config_hash,
            artifact_manifest_path: path_text(&input.artifact_manifest_path),
            artifact_manifest_fingerprint: input.artifact_manifest_fingerprint.clone(),
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
        let generation_fingerprint =
            sha256(&serde_json::to_vec(&unsigned_generation).map_err(json_failure)?);
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
        index_carriers.push(json!({
            "carrier_id": carrier.carrier_id,
            "carrier_kind": carrier.carrier_kind,
            "config_path": path_text(&carrier.config_path),
            "generation_sidecar_path": path_text(&sidecar_path),
            "materialization_generation_fingerprint": generation_fingerprint,
        }));
    }
    publications.push(Publication {
        path: input.installed_carrier_index_path.clone(),
        content: pretty_json(&json!({
            "schema": "narada.installed_carrier_index.v1",
            "workspace_root": path_text(&input.workspace_root),
            "artifact_manifest_path": path_text(&input.artifact_manifest_path),
            "carriers": index_carriers,
        }))?,
    });
    transactional_publish(&publications)?;
    Ok(json!({
        "schema": "narada.mcp_materializer.result.v1",
        "status": "materialized_all",
        "carrier_count": input.carriers.len(),
        "installed_carrier_index_path": path_text(&input.installed_carrier_index_path),
    }))
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
        for server in &carrier.servers {
            validate_identifier(&server.name, "server_name")?;
            if !server_names.insert(&server.name) {
                return Err(Failure::new(
                    "materializer_server_name_duplicate",
                    server.name.clone(),
                ));
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
            validate_proxy_launch(input, carrier, server)?;
        }
    }
    Ok(())
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
        ("--runtime-contract-version", "6".to_string()),
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
    }
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
            b"// Generated by narada-mcp-materializer. Do not hand-edit; changes will be overwritten on next materialize.\n".iter().copied(),
        );
    }
    Ok(output)
}

fn emit_codex(carrier: &CarrierInput) -> Result<Vec<u8>, Failure> {
    let mut out = String::from("# Generated by narada-mcp-materializer. Do not hand-edit; changes will be overwritten on next materialize.\n\n# Codex Apps/connectors are opt-in for profile-less launches.\n[features]\napps = false\n\n");
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
            json_string(&project.replace('\\', "\\\\"))?
        ));
    }
    for server in &carrier.servers {
        out.push_str(&format!("[mcp_servers.{}]\n", toml_key(&server.name)));
        out.push_str(&format!("command = {}\n", json_string(&server.command)?));
        out.push_str(&format!("args = {}\n", string_array(&server.args)?));
        out.push_str(&format!(
            "approval_mode = {}\n",
            json_string(server.approval_mode.as_deref().unwrap_or("approve"))?
        ));
        if let Some(timeout) = server.startup_timeout_sec {
            out.push_str(&format!("startup_timeout_sec = {timeout}\n"));
        }
        if !server.env_vars.is_empty() {
            out.push_str(&format!("env_vars = {}\n", string_array(&server.env_vars)?));
        }
        out.push('\n');
        for tool in &server.tools {
            out.push_str(&format!(
                "[mcp_servers.{}.tools.{}]\napproval_mode = {}\n\n",
                toml_key(&server.name),
                toml_key(&tool.name),
                json_string(&tool.approval_mode)?
            ));
        }
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
    let temporary = parent.join(format!(".{name}.narada-{}.tmp", std::process::id()));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary)?;
    file.write_all(content)?;
    file.sync_all()?;
    drop(file);
    if path.exists() {
        fs::remove_file(path)?;
    }
    fs::rename(&temporary, path)
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

fn pretty_json(value: &Value) -> Result<Vec<u8>, Failure> {
    let mut bytes = serde_json::to_vec_pretty(value).map_err(json_failure)?;
    bytes.push(b'\n');
    Ok(bytes)
}
fn json_failure(error: serde_json::Error) -> Failure {
    Failure::new("materializer_json_failed", error.to_string())
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

fn materialization_config_fingerprint(
    carrier_kind: CarrierKind,
    content: &[u8],
) -> Result<String, Failure> {
    let normalized = String::from_utf8(content.to_vec())
        .map_err(|error| Failure::new("materializer_config_not_utf8", error.to_string()))?
        .replace("\r\n", "\n")
        .replace('\r', "\n");
    let canonical = if matches!(carrier_kind, CarrierKind::Codex) {
        let mut lines = Vec::new();
        let mut in_mcp_table = false;
        let mut saw_mcp_table = false;
        for line in normalized.lines() {
            let trimmed = line.trim();
            if trimmed.starts_with("[mcp_servers.") && trimmed.ends_with(']') {
                in_mcp_table = true;
                saw_mcp_table = true;
                lines.push(line);
                continue;
            }
            if trimmed.starts_with('[') && trimmed.ends_with(']') {
                in_mcp_table = false;
            }
            if in_mcp_table {
                lines.push(line);
            }
        }
        if saw_mcp_table {
            lines.join("\n")
        } else {
            normalized.clone()
        }
    } else {
        normalized
    };
    Ok(sha256(canonical.trim_end_matches('\n').as_bytes()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn fixture(root: &Path) -> MaterializationInput {
        MaterializationInput {
            schema: INPUT_SCHEMA.into(),
            workspace_root: root.into(),
            artifact_manifest_path: root.join("manifest.json"),
            artifact_manifest_fingerprint: Some("a".repeat(64)),
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
                servers: vec![ServerInput {
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
                        "6".into(),
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
    fn materializes_config_sidecars_and_index() {
        let root = tempdir().unwrap();
        let input = fixture(root.path());
        let result = materialize(input).unwrap();
        assert_eq!(result["status"], "materialized_all");
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
    fn invalid_input_writes_nothing() {
        let root = tempdir().unwrap();
        let mut input = fixture(root.path());
        input.carriers[0].servers[0].name = "bad name".into();
        assert_eq!(
            materialize(input).unwrap_err().code,
            "materializer_identifier_invalid"
        );
        assert_eq!(fs::read_dir(root.path()).unwrap().count(), 0);
    }

    #[test]
    fn publication_failure_restores_every_previous_file() {
        let root = tempdir().unwrap();
        let mut input = fixture(root.path());
        fs::write(root.path().join("config.toml"), b"operator-owned-before\n").unwrap();
        fs::write(root.path().join("blocked-parent"), b"not-a-directory\n").unwrap();
        input.carriers.push(CarrierInput {
            carrier_id: "kimi-test".into(),
            carrier_kind: CarrierKind::Kimi,
            config_path: root.path().join("blocked-parent").join("mcp.json"),
            codex_plugin_overrides: BTreeMap::new(),
            trust_projects: vec![],
            servers: vec![],
        });

        let failure = materialize(input).unwrap_err();

        assert_eq!(failure.code, "materializer_transaction_failed");
        assert_eq!(
            fs::read(root.path().join("config.toml")).unwrap(),
            b"operator-owned-before\n"
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
}
