use super::{
    canonical_json_sha256, path_text, sha256, suffix_path, CarrierInput, CarrierKind, Failure,
    MaterializationInput, ServerInput, ToolInput,
};
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};

const REGISTRY_SCHEMA: &str = "narada.site.capabilities.mcp_surfaces.v1";

#[derive(Debug)]
pub(crate) struct DeriveOptions {
    contract: PathBuf,
    workspace_root: PathBuf,
    home: PathBuf,
    matrix: PathBuf,
    installed_index: PathBuf,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Contract {
    schema: String,
    sites: Vec<ContractSite>,
    carriers: Vec<ContractCarrier>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ContractSite {
    site_id: String,
    registry_path: PathBuf,
    surface_ids: Vec<String>,
    #[serde(default)]
    admit_local_bindings: bool,
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
                "--contract" | "--workspace-root" | "--home" | "--matrix" | "--installed-index"
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
            contract: take("--contract")?,
            workspace_root: take("--workspace-root")?,
            home: take("--home")?,
            matrix: take("--matrix")?,
            installed_index: take("--installed-index")?,
        })
    }
}

pub(crate) fn derive_input(options: DeriveOptions) -> Result<MaterializationInput, Failure> {
    require_absolute(&options.contract, "contract")?;
    require_absolute(&options.workspace_root, "workspace_root")?;
    require_absolute(&options.home, "home")?;
    require_absolute(&options.matrix, "matrix")?;
    require_absolute(&options.installed_index, "installed_index")?;

    let contract_bytes = read_required(
        &options.contract,
        "materializer_carrier_contract_read_failed",
    )?;
    let contract_fingerprint = sha256(&contract_bytes);
    let contract: Contract = serde_json::from_slice(&contract_bytes).map_err(|error| {
        Failure::new("materializer_carrier_contract_invalid", error.to_string())
    })?;
    if contract.schema != "narada.native_carrier_contract.v2" {
        return Err(Failure::new(
            "materializer_carrier_contract_schema_unsupported",
            contract.schema,
        ));
    }
    if contract.sites.is_empty() || contract.carriers.is_empty() {
        return Err(Failure::new(
            "materializer_carrier_contract_empty",
            "The carrier contract must declare at least one Site and one carrier.",
        ));
    }
    let mut servers = Vec::new();
    let mut ambient_bindings = Vec::new();
    let mut ambient_fabric_digests = Vec::new();
    let mut admitted_site_roots = BTreeSet::new();
    let mut proxy_commands = BTreeSet::new();
    let mut binding_by_surface = BTreeMap::<String, (String, String)>::new();
    let mut site_ids = BTreeSet::new();
    for site in &contract.sites {
        if site.site_id.is_empty() {
            return Err(Failure::new(
                "materializer_carrier_contract_site_empty",
                site.site_id.clone(),
            ));
        }
        if !site_ids.insert(site.site_id.clone()) {
            return Err(Failure::new(
                "materializer_carrier_contract_site_duplicate",
                site.site_id.clone(),
            ));
        }
        require_absolute(&site.registry_path, "registry_path")?;
        if site.admit_local_bindings {
            admitted_site_roots.insert(registry_site_root(&site.registry_path)?);
        }
        let registry_bytes =
            read_required(&site.registry_path, "materializer_registry_read_failed")?;
        let registry: Value = serde_json::from_slice(&registry_bytes)
            .map_err(|error| Failure::new("materializer_registry_invalid", error.to_string()))?;
        ambient_fabric_digests.push(sha256(&registry_bytes));
        if registry.get("schema").and_then(Value::as_str) != Some(REGISTRY_SCHEMA) {
            return Err(Failure::new(
                "materializer_registry_schema_unsupported",
                path_text(&site.registry_path),
            ));
        }
        if registry.get("site_id").and_then(Value::as_str) != Some(site.site_id.as_str()) {
            return Err(Failure::new(
                "materializer_registry_site_mismatch",
                site.site_id.clone(),
            ));
        }
        let surfaces = registry
            .get("surfaces")
            .and_then(Value::as_array)
            .ok_or_else(|| {
                Failure::new(
                    "materializer_registry_surfaces_required",
                    path_text(&site.registry_path),
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
                        path_text(&site.registry_path),
                    )
                })?;
            if by_id.insert(id.to_string(), surface).is_some() {
                return Err(Failure::new("materializer_registry_surface_duplicate", id));
            }
        }
        for surface in surfaces {
            if let Some(entry) =
                ambient_binding_entry(&site.site_id, surface, site.admit_local_bindings)?
            {
                ambient_bindings.push(entry);
            }
        }
        let explicitly_selected = site.surface_ids.iter().cloned().collect::<BTreeSet<_>>();
        let mut selected = explicitly_selected.clone();
        for surface in surfaces {
            if surface
                .pointer("/surface_projection/default_injection")
                .and_then(Value::as_str)
                == Some("enabled")
            {
                selected.insert(required_string(surface, "catalog_surface_id")?);
            }
        }
        for surface_id in &selected {
            let surface = by_id.get(surface_id).ok_or_else(|| {
                Failure::new(
                    "materializer_declared_surface_missing",
                    format!("{}:{surface_id}", site.site_id),
                )
            })?;
            let source_server_key = required_string(surface, "server_name")?;
            let server_name = if explicitly_selected.contains(surface_id) {
                surface_id.clone()
            } else {
                source_server_key.clone()
            };
            let binding_id = surface
                .get("binding_id")
                .and_then(Value::as_str)
                .map(str::to_owned)
                .unwrap_or_else(|| format!("{}-{}", site.site_id, surface_id));
            if let Some((predecessor_binding_id, predecessor_server_key)) =
                binding_by_surface.get(&server_name)
            {
                return Err(Failure::new(
                    "materializer_surface_binding_conflict",
                    format!("More than one binding was selected for surface '{server_name}'."),
                ).with_details(json!({
                    "surface_id": server_name,
                    "bindings": [
                        {"binding_id": predecessor_binding_id, "server_key": predecessor_server_key},
                        {"binding_id": binding_id, "server_key": source_server_key},
                    ]
                })));
            }
            binding_by_surface.insert(
                server_name.clone(),
                (binding_id.clone(), source_server_key.clone()),
            );
            let binding = surface.get("runtime_binding").ok_or_else(|| {
                Failure::new("materializer_runtime_binding_required", surface_id.clone())
            })?;
            if binding.get("proxy_implementation").and_then(Value::as_str) != Some("native") {
                return Err(Failure::new(
                    "materializer_native_proxy_required",
                    surface_id.clone(),
                ));
            }
            let transport = binding.get("transport").ok_or_else(|| {
                Failure::new("materializer_transport_required", surface_id.clone())
            })?;
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
                binding_id: Some(binding_id),
                source_server_key: Some(source_server_key),
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
    let artifact_build_set_path = options
        .workspace_root
        .join(".ai/runtime/artifact-build-set.json");
    let artifact_build_set_bytes = read_required(
        &artifact_build_set_path,
        "materializer_artifact_build_set_read_failed",
    )?;
    let artifact_build_set: Value =
        serde_json::from_slice(&artifact_build_set_bytes).map_err(|error| {
            Failure::new("materializer_artifact_build_set_invalid", error.to_string())
        })?;
    if artifact_build_set.get("schema").and_then(Value::as_str)
        != Some("narada.artifact_build_set.v1")
    {
        return Err(Failure::new(
            "materializer_artifact_build_set_schema_unsupported",
            path_text(&artifact_build_set_path),
        ));
    }
    let artifact_build_set_fingerprint = required_string(&artifact_build_set, "build_set_digest")?;
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
    ambient_bindings.sort_by(|left, right| {
        left.get("binding_id")
            .and_then(Value::as_str)
            .cmp(&right.get("binding_id").and_then(Value::as_str))
    });
    // Ambient carrier authority is durable installation state, not a session
    // lease. Keep its signed content reproducible; session envelopes carry
    // actual issuance/expiry timestamps in the Narada launch path.
    let ambient_issued_at = "1970-01-01T00:00:00Z".to_string();
    let ambient_fabric_digest = stable_digest(&json!(ambient_fabric_digests));
    let ambient_site_id = site_ids
        .iter()
        .next()
        .cloned()
        .unwrap_or_else(|| "ambient".to_string());
    let carriers = contract
        .carriers
        .into_iter()
        .map(|carrier| {
            let config_path = options.home.join(carrier.config_relative_path);
            let sidecar_path = suffix_path(&config_path, ".narada-generation.json");
            let admission_path = suffix_path(&config_path, ".narada-binding-admission.json");
            let carrier_kind = match carrier.carrier_kind {
                CarrierKind::Codex => "codex",
                CarrierKind::Kimi => "kimi",
                CarrierKind::Opencode => "opencode",
                CarrierKind::Pi => "pi",
            };
            let mut admission_envelope = json!({
                "schema": "narada.mcp.binding_admission_envelope.v1",
                "envelope_id": format!("ambient-{}", carrier.carrier_id),
                "decision": "admitted",
                "issued_at": ambient_issued_at.clone(),
                "valid_until": Value::Null,
                "principal_key": format!("ambient:{}", carrier.carrier_id),
                "site_id": ambient_site_id.clone(),
                "carrier_session_id": format!("ambient-{}", carrier.carrier_id),
                "carrier_kind": carrier_kind,
                "runtime_kind": "native",
                "authority_epoch": 0,
                "authority_context": {
                    "schema": "narada.carrier_authority_context.v1",
                    "carrier_materialization": {
                        "status": "valid",
                        "authority_epoch": 0,
                        "fabric_digest": ambient_fabric_digest.clone()
                    },
                    "session_attestation": { "status": "absent" },
                    "identity": { "status": "anonymous" },
                    "site_authority": {
                        "status": "materialized",
                        "site_ids": site_ids.iter().cloned().collect::<Vec<_>>()
                    },
                    "binding_activation": "capability_governed",
                    "lifecycle_participation": "carrier_user_scope"
                },
                "carrier_session_admission_receipt_ref": format!("materialized:{}", carrier.carrier_id),
                "authority_readback_ref": format!("materialized:{}", carrier.carrier_id),
                "fabric_digest": ambient_fabric_digest.clone(),
                "bindings": ambient_bindings.clone(),
                "envelope_digest": "",
            });
            let mut unsigned_envelope = admission_envelope.clone();
            unsigned_envelope.as_object_mut().expect("envelope is an object").remove("envelope_digest");
            let admission_digest = stable_digest(&unsigned_envelope);
            admission_envelope["envelope_digest"] = Value::String(admission_digest.clone());
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
                    if server.name == "mcp-loader" {
                        server.args.extend([
                            "--binding-admission-path".to_string(),
                            path_text(&admission_path),
                            "--binding-admission-digest".to_string(),
                            admission_digest.clone(),
                        ]);
                        for site_root in &admitted_site_roots {
                            server.args.extend([
                                "--allowed-site-root".to_string(),
                                path_text(site_root),
                            ]);
                        }
                    }
                    server
                })
                .collect();
            CarrierInput {
                carrier_id: carrier.carrier_id,
                carrier_kind: carrier.carrier_kind,
                config_path,
                codex_plugin_overrides: carrier.codex_plugin_overrides,
                trust_projects: vec![trust_project.clone()],
                binding_admission_path: Some(admission_path),
                binding_admission_envelope: Some(admission_envelope),
                servers: derived_servers,
            }
        })
        .collect();

    Ok(MaterializationInput {
        schema: "narada.carrier_materialization_input.v1".to_string(),
        workspace_root: options.workspace_root,
        carrier_contract_path: options.contract,
        carrier_contract_fingerprint: contract_fingerprint,
        artifact_manifest_path,
        artifact_manifest_fingerprint: Some(artifact_manifest_fingerprint),
        artifact_build_set_path,
        artifact_build_set_fingerprint,
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

fn stable_digest(value: &Value) -> String {
    canonical_json_sha256(value).expect("JSON values must canonicalize")
}

fn registry_site_root(registry_path: &Path) -> Result<PathBuf, Failure> {
    let capabilities = registry_path.parent().ok_or_else(|| {
        Failure::new(
            "materializer_registry_site_root_unresolved",
            path_text(registry_path),
        )
    })?;
    let narada = capabilities.parent().ok_or_else(|| {
        Failure::new(
            "materializer_registry_site_root_unresolved",
            path_text(registry_path),
        )
    })?;
    let site_root = narada.parent().ok_or_else(|| {
        Failure::new(
            "materializer_registry_site_root_unresolved",
            path_text(registry_path),
        )
    })?;
    if capabilities.file_name().and_then(|value| value.to_str()) != Some("capabilities")
        || narada.file_name().and_then(|value| value.to_str()) != Some(".narada")
    {
        return Err(Failure::new(
            "materializer_registry_site_root_unresolved",
            path_text(registry_path),
        ));
    }
    Ok(site_root.to_path_buf())
}

fn binding_admission_entry_digest_v1(entry: &Value) -> String {
    let mut unsigned = entry.clone();
    let object = unsigned
        .as_object_mut()
        .expect("binding admission entry must be an object");
    object.remove("binding_digest");
    let identity = object
        .remove("binding_identity")
        .expect("binding admission entry must carry binding_identity");
    object.insert("launch_identity".to_string(), identity);
    stable_digest(&unsigned)
}
fn ambient_binding_entry(
    site_id: &str,
    surface: &Value,
    admit_local: bool,
) -> Result<Option<Value>, Failure> {
    let injection_scope = surface
        .get("injection_scope")
        .and_then(Value::as_str)
        .or_else(|| {
            surface
                .get("narada_scope")
                .and_then(|value| value.get("injection_scope"))
                .and_then(Value::as_str)
        })
        .or_else(|| {
            surface
                .get("surface_projection")
                .and_then(|value| value.get("injection_scope"))
                .and_then(Value::as_str)
        })
        .filter(|value| !value.trim().is_empty())
        .map(str::to_string)
        .ok_or_else(|| Failure::new("materializer_registry_field_required", "injection_scope"))?;
    if injection_scope == "local_site" && !admit_local {
        return Ok(None);
    }
    if injection_scope != "host"
        && injection_scope != "user_site"
        && injection_scope != "local_site"
    {
        return Err(Failure::new(
            "materializer_injection_scope_unsupported",
            injection_scope,
        ));
    }
    let surface_id = required_string(surface, "catalog_surface_id")?;
    let binding_id = surface
        .get("binding_id")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .unwrap_or_else(|| format!("{site_id}-{surface_id}"));
    let projection = surface
        .get("surface_projection")
        .cloned()
        .unwrap_or(Value::Null);
    let projection_id = projection
        .get("projection_id")
        .and_then(Value::as_str)
        .unwrap_or("default")
        .to_string();
    let authority_locus = surface
        .get("authority_locus")
        .or_else(|| {
            surface
                .get("narada_scope")
                .and_then(|value| value.get("authority_locus"))
        })
        .cloned()
        .unwrap_or_else(|| match injection_scope.as_str() {
            "host" => json!({"kind": "host"}),
            "user_site" => json!({"kind": "user_site", "site_id": site_id}),
            "local_site" => json!({"kind": "local_site", "site_id": site_id}),
            _ => Value::Null,
        });
    if authority_locus.is_null() {
        return Err(Failure::new(
            "materializer_authority_locus_required",
            surface_id,
        ));
    }
    let runtime_binding = surface
        .get("runtime_binding")
        .ok_or_else(|| Failure::new("materializer_runtime_binding_required", surface_id.clone()))?;
    let transport = runtime_binding
        .get("transport")
        .ok_or_else(|| Failure::new("materializer_transport_required", surface_id.clone()))?;
    if transport.get("type").and_then(Value::as_str) != Some("stdio") {
        return Err(Failure::new(
            "materializer_transport_unsupported",
            surface_id,
        ));
    }
    let command = required_string(transport, "command")?;
    let args = string_array(transport, "args")?;
    let descriptor = projection.get("surface_descriptor").unwrap_or(&Value::Null);
    let env_vars = descriptor
        .get("projections")
        .and_then(Value::as_array)
        .and_then(|items| {
            items
                .iter()
                .find(|item| item.get("id").and_then(Value::as_str) == Some(projection_id.as_str()))
        })
        .and_then(|item| item.get("transport"))
        .map(|transport| string_array(transport, "env"))
        .transpose()?
        .unwrap_or_default();
    let target_site_root = authority_locus
        .get("site_root")
        .cloned()
        .unwrap_or(Value::Null);
    let binding_identity = json!({
        "schema": "narada.mcp.binding_identity.v1",
        "binding_id": binding_id.clone(),
        "surface_id": surface_id.clone(),
        "projection_id": projection_id.clone(),
        "injection_scope": injection_scope.clone(),
        "authority_locus": authority_locus,
        "transport": "stdio",
        "command": command,
        "args": args,
        "env": {},
        "env_vars": env_vars,
        "target_site_root": target_site_root,
        "surface_projection": projection,
    });
    let mut entry = json!({
        "binding_id": binding_id,
        "surface_id": surface_id,
        "projection_id": projection_id,
        "authority_locus": binding_identity["authority_locus"].clone(),
        "injection_scope": injection_scope,
        "operations": ["discover", "attach", "restart"],
        "binding_identity": binding_identity,
        "binding_digest": "",
    });
    let digest = binding_admission_entry_digest_v1(&entry);
    entry["binding_digest"] = Value::String(digest);
    Ok(Some(entry))
}

pub(crate) fn options_from_generation(path: &Path) -> Result<DeriveOptions, Failure> {
    require_absolute(path, "generation")?;
    let bytes = read_required(path, "materializer_generation_read_failed")?;
    let generation: Value = serde_json::from_slice(&bytes)
        .map_err(|error| Failure::new("materializer_generation_invalid", error.to_string()))?;
    if !matches!(
        generation.get("schema").and_then(Value::as_str),
        Some("narada.mcp_materialization_generation.v1")
            | Some("narada.mcp_materialization_generation.v2")
            | Some("narada.mcp_materialization_generation.v3")
    ) {
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
    let plan_path = PathBuf::from(required_string(
        &generation,
        "runtime_materialization_plan_path",
    )?);
    let plan_bytes = read_required(&plan_path, "materializer_runtime_plan_read_failed")?;
    let plan: Value = serde_json::from_slice(&plan_bytes)
        .map_err(|error| Failure::new("materializer_runtime_plan_invalid", error.to_string()))?;
    let source = plan.get("source").ok_or_else(|| {
        Failure::new(
            "materializer_runtime_plan_source_missing",
            path_text(&plan_path),
        )
    })?;
    let contract_path = PathBuf::from(required_string(source, "carrier_contract_path")?);
    require_absolute(&contract_path, "contract")?;
    let contract_bytes =
        read_required(&contract_path, "materializer_carrier_contract_read_failed")?;
    let expected_contract_fingerprint = required_string(source, "carrier_contract_fingerprint")?;
    if sha256(&contract_bytes) != expected_contract_fingerprint {
        return Err(Failure::new(
            "materializer_carrier_contract_fingerprint_mismatch",
            path_text(&contract_path),
        ));
    }
    let contract: Contract = serde_json::from_slice(&contract_bytes).map_err(|error| {
        Failure::new("materializer_carrier_contract_invalid", error.to_string())
    })?;
    if contract.schema != "narada.native_carrier_contract.v2" {
        return Err(Failure::new(
            "materializer_carrier_contract_schema_unsupported",
            contract.schema,
        ));
    }
    let declared = contract
        .carriers
        .iter()
        .find(|carrier| match carrier.carrier_kind {
            CarrierKind::Codex => carrier_kind == "codex",
            CarrierKind::Kimi => carrier_kind == "kimi",
            CarrierKind::Opencode => carrier_kind == "opencode",
            CarrierKind::Pi => carrier_kind == "pi",
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
        contract: contract_path,
        workspace_root,
        matrix,
        installed_index: home.join(".narada/carriers/installed-carriers.json"),
        home,
    })
}

pub(crate) fn options_from_installed_index(path: &Path) -> Result<DeriveOptions, Failure> {
    require_absolute(path, "installed_index")?;
    let bytes = read_required(path, "materializer_installed_index_read_failed")?;
    let index: Value = serde_json::from_slice(&bytes)
        .map_err(|error| Failure::new("materializer_installed_index_invalid", error.to_string()))?;
    if index.get("schema").and_then(Value::as_str) != Some("narada.installed_carrier_index.v1") {
        return Err(Failure::new(
            "materializer_installed_index_schema_unsupported",
            path_text(path),
        ));
    }
    let generation = index
        .get("carriers")
        .and_then(Value::as_array)
        .and_then(|carriers| carriers.first())
        .and_then(|carrier| carrier.get("generation_sidecar_path"))
        .and_then(Value::as_str)
        .map(PathBuf::from)
        .ok_or_else(|| {
            Failure::new(
                "materializer_installed_index_generation_required",
                path_text(path),
            )
        })?;
    options_from_generation(&generation)
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

#[cfg(test)]
mod tests {
    use super::*;

    fn surface(scope: &str) -> Value {
        json!({
            "catalog_surface_id": "fixture",
            "binding_id": "fixture-binding",
            "injection_scope": scope,
            "authority_locus": {"kind": scope},
            "runtime_binding": {
                "transport": {
                    "type": "stdio",
                    "command": "fixture.exe",
                    "args": ["serve"]
                }
            },
            "surface_projection": {
                "projection_id": "default",
                "surface_descriptor": {
                    "projections": [{
                        "id": "default",
                        "transport": {"env": ["PATH"]}
                    }]
                }
            }
        })
    }

    #[test]
    fn ambient_admission_excludes_local_site_bindings() {
        assert!(ambient_binding_entry("site", &surface("local_site"), false)
            .unwrap()
            .is_none());
    }

    #[test]
    fn ambient_admission_carries_exact_host_and_user_bindings() {
        for scope in ["host", "user_site"] {
            let entry = ambient_binding_entry("site", &surface(scope), false)
                .unwrap()
                .expect("ambient binding");
            assert_eq!(entry["injection_scope"], scope);
            assert_eq!(entry["binding_id"], "fixture-binding");
            assert_eq!(
                entry["binding_digest"],
                binding_admission_entry_digest_v1(&entry)
            );
            assert_eq!(entry["binding_identity"]["command"], "fixture.exe");
        }
    }
}
