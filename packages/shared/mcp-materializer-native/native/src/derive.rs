use super::{
    path_text, sha256, suffix_path, CarrierInput, CarrierKind, Failure, MaterializationInput,
    ServerInput, ToolInput,
};
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};

const CONTRACT: &str = include_str!("../../contracts/default-carriers.json");
const REGISTRY_SCHEMA: &str = "narada.site.capabilities.mcp_surfaces.v1";

#[derive(Debug)]
pub(crate) struct DeriveOptions {
    registry: PathBuf,
    workspace_root: PathBuf,
    home: PathBuf,
    matrix: PathBuf,
    installed_index: PathBuf,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Contract {
    schema: String,
    site_id: String,
    surface_ids: Vec<String>,
    carriers: Vec<ContractCarrier>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ContractCarrier {
    carrier_id: String,
    carrier_kind: CarrierKind,
    config_relative_path: String,
    #[serde(default)]
    codex_plugin_overrides: BTreeMap<String, bool>,
}

impl DeriveOptions {
    pub(crate) fn parse(mut args: impl Iterator<Item = OsString>) -> Result<Self, Failure> {
        let mut values = BTreeMap::<String, PathBuf>::new();
        while let Some(flag) = args.next() {
            let flag = flag.into_string().map_err(|_| {
                Failure::new("materializer_argument_invalid", "Argument is not UTF-8.")
            })?;
            if !matches!(
                flag.as_str(),
                "--registry" | "--workspace-root" | "--home" | "--matrix" | "--installed-index"
            ) {
                return Err(Failure::new(
                    "materializer_argument_unknown",
                    format!("Unknown argument: {flag}"),
                ));
            }
            let value = args.next().map(PathBuf::from).ok_or_else(|| {
                Failure::new(
                    "materializer_argument_value_required",
                    format!("{flag} requires a path."),
                )
            })?;
            if values.insert(flag.clone(), value).is_some() {
                return Err(Failure::new(
                    "materializer_argument_duplicate",
                    format!("Duplicate argument: {flag}"),
                ));
            }
        }
        let take = |flag: &str| {
            values.get(flag).cloned().ok_or_else(|| {
                Failure::new("materializer_argument_required", format!("Missing {flag}."))
            })
        };
        Ok(Self {
            registry: take("--registry")?,
            workspace_root: take("--workspace-root")?,
            home: take("--home")?,
            matrix: take("--matrix")?,
            installed_index: take("--installed-index")?,
        })
    }
}

pub(crate) fn derive_input(options: DeriveOptions) -> Result<MaterializationInput, Failure> {
    require_absolute(&options.registry, "registry")?;
    require_absolute(&options.workspace_root, "workspace_root")?;
    require_absolute(&options.home, "home")?;
    require_absolute(&options.matrix, "matrix")?;
    require_absolute(&options.installed_index, "installed_index")?;

    let contract: Contract = serde_json::from_str(CONTRACT).map_err(|error| {
        Failure::new("materializer_carrier_contract_invalid", error.to_string())
    })?;
    if contract.schema != "narada.native_carrier_contract.v1" {
        return Err(Failure::new(
            "materializer_carrier_contract_schema_unsupported",
            contract.schema,
        ));
    }
    let registry_bytes = read_required(&options.registry, "materializer_registry_read_failed")?;
    let registry: Value = serde_json::from_slice(&registry_bytes)
        .map_err(|error| Failure::new("materializer_registry_invalid", error.to_string()))?;
    if registry.get("schema").and_then(Value::as_str) != Some(REGISTRY_SCHEMA) {
        return Err(Failure::new(
            "materializer_registry_schema_unsupported",
            registry
                .get("schema")
                .and_then(Value::as_str)
                .unwrap_or("missing"),
        ));
    }
    if registry.get("site_id").and_then(Value::as_str) != Some(contract.site_id.as_str()) {
        return Err(Failure::new(
            "materializer_registry_site_mismatch",
            "The capability registry does not belong to the declared carrier Site.",
        ));
    }

    let surfaces = registry
        .get("surfaces")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            Failure::new(
                "materializer_registry_surfaces_required",
                "surfaces must be an array.",
            )
        })?;
    let mut by_id = BTreeMap::<String, &Value>::new();
    for surface in surfaces {
        let id = surface
            .get("catalog_surface_id")
            .and_then(Value::as_str)
            .ok_or_else(|| {
                Failure::new(
                    "materializer_registry_surface_id_required",
                    "catalog_surface_id is required.",
                )
            })?;
        if by_id.insert(id.to_string(), surface).is_some() {
            return Err(Failure::new("materializer_registry_surface_duplicate", id));
        }
    }

    let mut servers = Vec::new();
    let mut proxy_commands = BTreeSet::new();
    for surface_id in &contract.surface_ids {
        let surface = by_id.get(surface_id).ok_or_else(|| {
            Failure::new("materializer_declared_surface_missing", surface_id.clone())
        })?;
        let server_name = required_string(surface, "server_name")?;
        let binding = surface.get("runtime_binding").ok_or_else(|| {
            Failure::new("materializer_runtime_binding_required", surface_id.clone())
        })?;
        if binding.get("proxy_implementation").and_then(Value::as_str) != Some("native") {
            return Err(Failure::new(
                "materializer_native_proxy_required",
                surface_id.clone(),
            ));
        }
        let transport = binding
            .get("transport")
            .ok_or_else(|| Failure::new("materializer_transport_required", surface_id.clone()))?;
        if transport.get("type").and_then(Value::as_str) != Some("stdio") {
            return Err(Failure::new(
                "materializer_transport_unsupported",
                surface_id.clone(),
            ));
        }
        let command = required_string(transport, "command")?;
        proxy_commands.insert(command.clone());
        let args = string_array(transport, "args")?;
        let tools = string_array(surface, "registered_live_tools")?
            .into_iter()
            .map(|name| ToolInput {
                name,
                approval_mode: "approve".to_string(),
            })
            .collect();
        let projection = surface.get("surface_projection").unwrap_or(&Value::Null);
        let descriptor = projection.get("surface_descriptor").unwrap_or(&Value::Null);
        let projection_id = projection.get("projection_id").and_then(Value::as_str);
        let env_vars = descriptor
            .get("projections")
            .and_then(Value::as_array)
            .and_then(|items| {
                items
                    .iter()
                    .find(|item| item.get("id").and_then(Value::as_str) == projection_id)
            })
            .and_then(|item| item.get("transport"))
            .map(|transport| string_array(transport, "env"))
            .transpose()?
            .unwrap_or_default();
        let startup_timeout_sec = descriptor
            .get("metadata")
            .and_then(|value| value.get("codex_startup_timeout_sec"))
            .and_then(Value::as_u64);
        servers.push(ServerInput {
            name: server_name,
            command,
            args,
            env_vars,
            enabled: true,
            approval_mode: Some("approve".to_string()),
            startup_timeout_sec,
            tools,
        });
    }
    if proxy_commands.len() != 1 {
        return Err(Failure::new(
            "materializer_proxy_entrypoint_ambiguous",
            format!(
                "Expected one native proxy command, found {}.",
                proxy_commands.len()
            ),
        ));
    }
    let proxy_entrypoint = PathBuf::from(proxy_commands.into_iter().next().unwrap());
    require_absolute(&proxy_entrypoint, "proxy_entrypoint")?;
    let proxy_fingerprint = Some(sha256(&read_required(
        &proxy_entrypoint,
        "materializer_proxy_read_failed",
    )?));

    let artifact_manifest_path = options
        .workspace_root
        .join(".ai/runtime/workspace-artifact-manifest.json");
    let artifact_manifest_bytes = read_required(
        &artifact_manifest_path,
        "materializer_artifact_manifest_read_failed",
    )?;
    let artifact_manifest: Value =
        serde_json::from_slice(&artifact_manifest_bytes).map_err(|error| {
            Failure::new("materializer_artifact_manifest_invalid", error.to_string())
        })?;
    let artifact_manifest_fingerprint = artifact_manifest
        .get("manifest_fingerprint")
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| {
            Failure::new(
                "materializer_artifact_manifest_fingerprint_required",
                path_text(&artifact_manifest_path),
            )
        })?;
    let matrix_fingerprint = sha256(&read_required(
        &options.matrix,
        "materializer_matrix_read_failed",
    )?);
    let executable = std::env::current_exe()
        .map_err(|error| Failure::new("materializer_executable_unresolved", error.to_string()))?;
    let registrar_fingerprint = Some(sha256(&read_required(
        &executable,
        "materializer_executable_read_failed",
    )?));
    let trust_project = options
        .workspace_root
        .parent()
        .unwrap_or(&options.workspace_root)
        .to_string_lossy()
        .to_string();
    let carriers = contract
        .carriers
        .into_iter()
        .map(|carrier| {
            let config_path = options.home.join(carrier.config_relative_path);
            let sidecar_path = suffix_path(&config_path, ".narada-generation.json");
            let carrier_kind = match carrier.carrier_kind {
                CarrierKind::Codex => "codex",
                CarrierKind::Kimi => "kimi",
                CarrierKind::Opencode => "opencode",
            };
            let derived_servers = servers
                .iter()
                .cloned()
                .map(|mut server| {
                    let delimiter = server
                        .args
                        .iter()
                        .position(|arg| arg == "--")
                        .unwrap_or(server.args.len());
                    server.args.splice(
                        delimiter..delimiter,
                        [
                            "--carrier-id".to_string(),
                            carrier.carrier_id.clone(),
                            "--carrier-kind".to_string(),
                            carrier_kind.to_string(),
                            "--registrar-command".to_string(),
                            path_text(&executable),
                            "--registrar-entrypoint".to_string(),
                            path_text(&executable),
                            "--materialization-sidecar".to_string(),
                            path_text(&sidecar_path),
                        ],
                    );
                    server
                })
                .collect();
            CarrierInput {
                carrier_id: carrier.carrier_id,
                carrier_kind: carrier.carrier_kind,
                config_path,
                codex_plugin_overrides: carrier.codex_plugin_overrides,
                trust_projects: vec![trust_project.clone()],
                servers: derived_servers,
            }
        })
        .collect();

    Ok(MaterializationInput {
        schema: "narada.carrier_materialization_input.v1".to_string(),
        workspace_root: options.workspace_root,
        artifact_manifest_path,
        artifact_manifest_fingerprint: Some(artifact_manifest_fingerprint),
        runtime_profile_kind: "native".to_string(),
        runtime_implementation_matrix_path: options.matrix,
        runtime_implementation_matrix_fingerprint: matrix_fingerprint,
        registrar_entrypoint: executable,
        registrar_fingerprint,
        proxy_implementation: "native".to_string(),
        proxy_entrypoint,
        proxy_fingerprint,
        installed_carrier_index_path: options.installed_index,
        carriers,
    })
}

pub(crate) fn options_from_generation(path: &Path) -> Result<DeriveOptions, Failure> {
    require_absolute(path, "generation")?;
    let bytes = read_required(path, "materializer_generation_read_failed")?;
    let generation: Value = serde_json::from_slice(&bytes)
        .map_err(|error| Failure::new("materializer_generation_invalid", error.to_string()))?;
    if generation.get("schema").and_then(Value::as_str)
        != Some("narada.mcp_materialization_generation.v1")
    {
        return Err(Failure::new(
            "materializer_generation_schema_unsupported",
            path_text(path),
        ));
    }
    let config_path = PathBuf::from(required_string(&generation, "config_path")?);
    let carrier_kind = required_string(&generation, "carrier_kind")?;
    let artifact_manifest = PathBuf::from(required_string(&generation, "artifact_manifest_path")?);
    let matrix = PathBuf::from(required_string(
        &generation,
        "runtime_implementation_matrix_path",
    )?);
    let workspace_root = artifact_manifest
        .parent()
        .and_then(Path::parent)
        .and_then(Path::parent)
        .map(Path::to_path_buf)
        .ok_or_else(|| {
            Failure::new(
                "materializer_workspace_root_unresolved",
                path_text(&artifact_manifest),
            )
        })?;
    let contract: Contract = serde_json::from_str(CONTRACT).map_err(|error| {
        Failure::new("materializer_carrier_contract_invalid", error.to_string())
    })?;
    let declared = contract
        .carriers
        .iter()
        .find(|carrier| match carrier.carrier_kind {
            CarrierKind::Codex => carrier_kind == "codex",
            CarrierKind::Kimi => carrier_kind == "kimi",
            CarrierKind::Opencode => carrier_kind == "opencode",
        })
        .ok_or_else(|| Failure::new("materializer_carrier_kind_unsupported", carrier_kind))?;
    let relative = PathBuf::from(&declared.config_relative_path);
    if !config_path.ends_with(&relative) {
        return Err(Failure::new(
            "materializer_config_path_contract_mismatch",
            path_text(&config_path),
        ));
    }
    let mut home = config_path.clone();
    for _ in relative.components() {
        home.pop();
    }
    Ok(DeriveOptions {
        registry: home.join("Narada/.narada/capabilities/mcp-surfaces.json"),
        workspace_root,
        matrix,
        installed_index: home.join(".narada/carriers/installed-carriers.json"),
        home,
    })
}

fn read_required(path: &Path, code: &'static str) -> Result<Vec<u8>, Failure> {
    fs::read(path).map_err(|error| {
        Failure::new(code, error.to_string()).with_details(json!({"path": path_text(path)}))
    })
}

fn require_absolute(path: &Path, field: &'static str) -> Result<(), Failure> {
    if path.is_absolute() {
        Ok(())
    } else {
        Err(Failure::new(
            "materializer_derived_path_not_absolute",
            format!("{field}:{}", path_text(path)),
        ))
    }
}

fn required_string(value: &Value, field: &'static str) -> Result<String, Failure> {
    value
        .get(field)
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .map(str::to_string)
        .ok_or_else(|| Failure::new("materializer_registry_field_required", field))
}

fn string_array(value: &Value, field: &'static str) -> Result<Vec<String>, Failure> {
    value
        .get(field)
        .and_then(Value::as_array)
        .ok_or_else(|| Failure::new("materializer_registry_array_required", field))?
        .iter()
        .map(|item| {
            item.as_str()
                .map(str::to_string)
                .ok_or_else(|| Failure::new("materializer_registry_string_required", field))
        })
        .collect()
}
