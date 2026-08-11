use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::env;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use time::{format_description::well_known::Rfc3339, OffsetDateTime};

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

#[derive(Debug, Deserialize)]
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

#[derive(Debug, Deserialize)]
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
    let command = args
        .next()
        .and_then(|value| value.into_string().ok())
        .ok_or_else(|| {
            Failure::new(
                "materializer_command_required",
                "Expected `materialize-all --input <path>`.",
            )
        })?;
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

fn materialize(input: MaterializationInput) -> Result<Value, Failure> {
    validate_input(&input)?;
    let generated_at = OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .map_err(|error| Failure::new("materializer_clock_failed", error.to_string()))?;
    let mut publications = Vec::new();
    let mut index_carriers = Vec::new();
    for carrier in &input.carriers {
        let config = emit_carrier(carrier)?;
        let config_hash = sha256(&config);
        let plan_path = suffix_path(&carrier.config_path, ".narada-runtime-plan.json");
        let sidecar_path = suffix_path(&carrier.config_path, ".narada-generation.json");
        let plan = serde_json::to_vec_pretty(&json!({
            "schema": "narada.runtime_materialization_plan.v1",
            "runtime_profile_kind": input.runtime_profile_kind,
            "runtime_implementation_matrix_path": path_text(&input.runtime_implementation_matrix_path),
            "runtime_implementation_matrix_fingerprint": input.runtime_implementation_matrix_fingerprint,
            "carrier_id": carrier.carrier_id,
            "servers": carrier.servers.iter().map(|server| json!({"name":server.name,"command":server.command,"args":server.args})).collect::<Vec<_>>(),
        })).map_err(json_failure)?;
        let plan_hash = sha256(&plan);
        let fingerprint_source = json!({
            "carrier_id": carrier.carrier_id,
            "carrier_kind": carrier.carrier_kind,
            "config_sha256": config_hash,
            "artifact_manifest_fingerprint": input.artifact_manifest_fingerprint,
            "runtime_materialization_plan_fingerprint": plan_hash,
            "runtime_implementation_matrix_fingerprint": input.runtime_implementation_matrix_fingerprint,
            "registrar_fingerprint": input.registrar_fingerprint,
            "proxy_fingerprint": input.proxy_fingerprint,
        });
        let generation_fingerprint =
            sha256(&serde_json::to_vec(&fingerprint_source).map_err(json_failure)?);
        let generation = Generation {
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
            generation_fingerprint: generation_fingerprint.clone(),
            generated_at: generated_at.clone(),
        };
        publications.push(Publication {
            path: carrier.config_path.clone(),
            content: config,
        });
        publications.push(Publication {
            path: plan_path,
            content: with_newline(plan),
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
        }
    }
    Ok(())
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
fn with_newline(mut value: Vec<u8>) -> Vec<u8> {
    value.push(b'\n');
    value
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
                    command: "server.exe".into(),
                    args: vec!["--stdio".into()],
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
