use narada_mcp_materialization_contract::{canonical_json_sha256, pretty_json, sha256};
use serde_json::{json, Map, Value};
use std::collections::BTreeMap;
use std::env;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};
use time::OffsetDateTime;

const RUNTIME_PACKAGES: &[&str] = &[
    "narada-agent-context-mcp",
    "narada-mcp-loader",
    "narada-mcp-registrar",
    "narada-mcp-lifecycle",
    "narada-mcp-materializer",
    "narada-mcp-runtime",
    "narada-mcp-surfaces-native",
];

#[derive(Clone, Copy)]
struct DistributionArtifact {
    binary: &'static str,
    artifact_root: &'static str,
    pointer_schema: &'static str,
}

const ARTIFACTS: &[DistributionArtifact] = &[
    artifact(
        "narada-agent-context-mcp",
        "narada-agent-context-mcp",
        "packages/agent-context-mcp/dist/native",
        "narada.mcp_runtime_proxy.native_artifact_pointer.v1",
    ),
    artifact(
        "narada-mcp-loader",
        "narada-mcp-loader",
        "packages/mcp-loader-mcp/dist/native",
        "narada.mcp_runtime_proxy.native_artifact_pointer.v1",
    ),
    artifact(
        "narada-mcp-registrar",
        "narada-mcp-registrar",
        "packages/mcp-registrar/dist/native",
        "narada.mcp_runtime_proxy.native_artifact_pointer.v1",
    ),
    artifact(
        "narada-mcp-lifecycle",
        "narada-task-lifecycle-mcp",
        "packages/shared/mcp-lifecycle-native/dist/native",
        "narada.mcp_runtime_proxy.native_artifact_pointer.v1",
    ),
    artifact(
        "narada-mcp-lifecycle",
        "narada-work-lifecycle-mcp",
        "packages/shared/mcp-lifecycle-native/dist/native",
        "narada.mcp_runtime_proxy.native_artifact_pointer.v1",
    ),
    artifact(
        "narada-mcp-materializer",
        "narada-mcp-materializer",
        "packages/shared/mcp-materializer-native/dist/native",
        "narada.mcp_materializer.native_artifact_pointer.v1",
    ),
    artifact(
        "narada-mcp-runtime",
        "narada-mcp-runtime",
        "packages/shared/mcp-runtime-proxy/dist/native",
        "narada.mcp_runtime_proxy.native_artifact_pointer.v1",
    ),
    artifact(
        "narada-mcp-runtime",
        "narada-mcp-rhai-filesystem",
        "packages/shared/mcp-runtime-proxy/dist/native",
        "narada.mcp_runtime_proxy.native_artifact_pointer.v1",
    ),
    artifact(
        "narada-mcp-surfaces-native",
        "narada-mcp-surfaces",
        "packages/shared/mcp-surfaces-native/dist/native",
        "narada.mcp_runtime_proxy.native_artifact_pointer.v1",
    ),
];

const fn artifact(
    _package: &'static str,
    binary: &'static str,
    artifact_root: &'static str,
    pointer_schema: &'static str,
) -> DistributionArtifact {
    DistributionArtifact {
        binary,
        artifact_root,
        pointer_schema,
    }
}

fn main() -> ExitCode {
    match run() {
        Ok(value) => {
            println!("{}", serde_json::to_string(&value).unwrap());
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!(
                "{}",
                serde_json::to_string(&json!({
                    "schema":"narada.native_distribution.error.v1",
                    "status":"failed",
                    "error":error,
                }))
                .unwrap()
            );
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<Value, String> {
    let mut args = env::args().skip(1);
    let command = args.next().ok_or("native_distribution_command_required")?;
    let root = workspace_root()?;
    match command.as_str() {
        "build" => build(&root),
        "test" => test(&root),
        "package" => package(&root),
        "materialize" => {
            let options = MaterializeOptions::parse(args.collect())?;
            materialize(&root, &options)
        }
        "release" => {
            let options = MaterializeOptions::parse(args.collect())?;
            test(&root)?;
            package(&root)?;
            materialize(&root, &options)
        }
        "verify" => verify_native_distribution(&root),
        _ => Err(format!("native_distribution_command_unknown:{command}")),
    }
}

fn workspace_root() -> Result<PathBuf, String> {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .map(Path::to_path_buf)
        .ok_or_else(|| "native_distribution_workspace_unresolved".into())
}

fn cargo(root: &Path, arguments: &[&str], phase: &str) -> Result<(), String> {
    let status = Command::new(env::var_os("CARGO").unwrap_or_else(|| "cargo".into()))
        .args(arguments)
        .current_dir(root)
        .status()
        .map_err(|error| format!("{phase}:cargo_launch_failed:{error}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("{phase}:cargo_exit:{:?}", status.code()))
    }
}

fn build(root: &Path) -> Result<Value, String> {
    let mut arguments = vec!["build", "--release", "--locked"];
    for package in RUNTIME_PACKAGES {
        arguments.extend(["--package", package]);
    }
    cargo(root, &arguments, "native_build")?;
    Ok(
        json!({"schema":"narada.native_distribution.build.v1","status":"built","package_count":RUNTIME_PACKAGES.len()}),
    )
}

fn test(root: &Path) -> Result<Value, String> {
    // Several surface tests temporarily override process-wide environment variables.
    // Serialize them so the workspace authority is deterministic.
    cargo(
        root,
        &["test", "--workspace", "--locked", "--", "--test-threads=1"],
        "native_test",
    )?;
    Ok(json!({"schema":"narada.native_distribution.test.v1","status":"passed"}))
}

fn package(root: &Path) -> Result<Value, String> {
    build(root)?;
    let target = root.join("target/release");
    let mut grouped = BTreeMap::<&str, Vec<(DistributionArtifact, PathBuf, Vec<u8>)>>::new();
    for artifact in ARTIFACTS {
        let name = executable_name(artifact.binary);
        let source = target.join(&name);
        let bytes = fs::read(&source)
            .map_err(|error| format!("native_artifact_missing:{}:{error}", source.display()))?;
        grouped
            .entry(artifact.artifact_root)
            .or_default()
            .push((*artifact, source, bytes));
    }
    let generated_at = now()?;
    let mut published = Vec::<PathBuf>::new();
    for (relative_root, artifacts) in grouped {
        let mut fingerprint_input = Vec::new();
        for (artifact, _, bytes) in &artifacts {
            fingerprint_input.extend_from_slice(executable_name(artifact.binary).as_bytes());
            fingerprint_input.push(0);
            fingerprint_input.extend_from_slice(bytes);
        }
        let fingerprint = sha256(&fingerprint_input);
        let artifact_root = root.join(relative_root);
        let version_root = artifact_root.join("versions").join(&fingerprint);
        fs::create_dir_all(&version_root).map_err(|error| error.to_string())?;
        let mut entries = Map::new();
        for (artifact, _, bytes) in artifacts {
            let name = executable_name(artifact.binary);
            let destination = version_root.join(&name);
            publish_immutable(&destination, &bytes)?;
            entries.insert(
                name.clone(),
                json!(format!("versions/{fingerprint}/{name}")),
            );
            published.push(destination);
        }
        let pointer = json!({
            "schema": ARTIFACTS.iter().find(|candidate| candidate.artifact_root == relative_root).unwrap().pointer_schema,
            "generated_at": generated_at,
            "build_fingerprint": fingerprint,
            "artifacts": entries,
        });
        atomic_write(&artifact_root.join("current.json"), &pretty_json(&pointer)?)?;
    }
    published.sort();
    let runtime = root.join(".ai/runtime");
    fs::create_dir_all(&runtime).map_err(|error| error.to_string())?;
    let manifest_path = runtime.join("workspace-artifact-manifest.json");
    let manifest = native_manifest(root, &published, &generated_at)?;
    atomic_write(&manifest_path, &pretty_json(&manifest)?)?;
    let build_set_path = runtime.join("artifact-build-set.json");
    let build_set = native_build_set(root, &manifest_path, &manifest, &published, &generated_at)?;
    atomic_write(&build_set_path, &pretty_json(&build_set)?)?;
    Ok(json!({
        "schema":"narada.native_distribution.package.v1",
        "status":"packaged",
        "artifact_count":published.len(),
        "manifest_path":path_text(&manifest_path),
        "build_set_path":path_text(&build_set_path),
    }))
}

fn native_manifest(
    root: &Path,
    artifacts: &[PathBuf],
    generated_at: &str,
) -> Result<Value, String> {
    let records = artifacts
        .iter()
        .map(file_record)
        .collect::<Result<Vec<_>, _>>()?;
    let mut value = json!({
        "schema":"narada.workspace_artifact_manifest.v1",
        "generated_at":generated_at,
        "workspace_root":path_text(root),
        "packages":[],
        "artifacts":records,
        "distribution_kind":"native",
        "build_authority":"cargo",
    });
    let fingerprint = sha256(
        &serde_json::to_vec(&strip_volatile_manifest_metadata(&value))
            .map_err(|error| error.to_string())?,
    );
    value
        .as_object_mut()
        .unwrap()
        .insert("manifest_fingerprint".into(), json!(fingerprint));
    Ok(value)
}

fn native_build_set(
    root: &Path,
    manifest_path: &Path,
    manifest: &Value,
    artifacts: &[PathBuf],
    generated_at: &str,
) -> Result<Value, String> {
    let records = artifacts.iter().map(|path| {
        let bytes = fs::read(path).map_err(|error| error.to_string())?;
        Ok(json!({"path":path_text(path),"sha256":format!("sha256:{}",sha256(&bytes)),"size":bytes.len()}))
    }).collect::<Result<Vec<Value>, String>>()?;
    let manifest_bytes = fs::read(manifest_path).map_err(|error| error.to_string())?;
    let required = artifacts
        .iter()
        .map(|path| path_text(path))
        .collect::<Vec<_>>();
    let mut value = json!({
        "schema":"narada.artifact_build_set.v1",
        "assurance":"declared_isolated_closure",
        "generated_at":generated_at,
        "workspace_root":path_text(root),
        "workspace_manifest_path":path_text(manifest_path),
        "workspace_manifest_fingerprint":manifest["manifest_fingerprint"],
        "workspace_manifest_bytes_digest":format!("sha256:{}",sha256(&manifest_bytes)),
        "source_closure_digest":format!("sha256:{}",sha256(include_bytes!("../../Cargo.lock"))),
        "ambient_input_classes":["operating_system","rust_toolchain","cargo_dependency_store"],
        "toolchain":{"schema":"narada.artifact.toolchain.native.v1","build_authority":"cargo"},
        "artifacts":records,
        "required_references":required,
    });
    let mut unsigned = value.clone();
    unsigned.as_object_mut().unwrap().remove("generated_at");
    let digest = format!("sha256:{}", canonical_json_sha256(&unsigned)?);
    value
        .as_object_mut()
        .unwrap()
        .insert("build_set_digest".into(), json!(digest));
    Ok(value)
}

#[derive(Default)]
struct MaterializeOptions {
    contract: Option<PathBuf>,
    matrix: Option<PathBuf>,
    home: Option<PathBuf>,
    installed_index: Option<PathBuf>,
}

impl MaterializeOptions {
    fn parse(args: Vec<String>) -> Result<Self, String> {
        let mut options = Self::default();
        let mut index = 0;
        while index < args.len() {
            if args[index] == "--" {
                index += 1;
                continue;
            }
            let value = args.get(index + 1).ok_or_else(|| {
                format!(
                    "native_distribution_argument_value_required:{}",
                    args[index]
                )
            })?;
            match args[index].as_str() {
                "--contract" => options.contract = Some(value.into()),
                "--matrix" => options.matrix = Some(value.into()),
                "--home" => options.home = Some(value.into()),
                "--installed-index" => options.installed_index = Some(value.into()),
                flag => return Err(format!("native_distribution_argument_unknown:{flag}")),
            }
            index += 2;
        }
        Ok(options)
    }
}

fn materialize(root: &Path, options: &MaterializeOptions) -> Result<Value, String> {
    // Publish first so registry synchronization can resolve the current immutable
    // artifact pointers. Synchronization changes launch references, therefore seal
    // the graph again before asking the materializer to validate it.
    package(root)?;
    let home = options
        .home
        .clone()
        .or_else(|| env::var_os("USERPROFILE").map(PathBuf::from))
        .ok_or("native_distribution_home_required")?;
    let source_root = root
        .parent()
        .ok_or("native_distribution_source_root_unresolved")?;
    let contract = options
        .contract
        .clone()
        .unwrap_or_else(|| home.join("Narada/.narada/capabilities/carrier-materialization.json"));
    let matrix = options.matrix.clone().unwrap_or_else(|| source_root.join("narada/packages/operator-surface-runtime-contract/contracts/runtime-implementation-matrix.json"));
    let installed = options
        .installed_index
        .clone()
        .unwrap_or_else(|| home.join(".narada/carriers/installed-carriers.json"));
    sync_site_registries(root, &contract)?;
    package(root)?;
    let materializer = current_artifact(
        root,
        "packages/shared/mcp-materializer-native/dist/native",
        &executable_name("narada-mcp-materializer"),
    )?;
    let status = Command::new(&materializer)
        .args([
            "promote-site",
            "--contract",
            &path_text(&contract),
            "--workspace-root",
            &path_text(root),
            "--home",
            &path_text(&home),
            "--matrix",
            &path_text(&matrix),
            "--installed-index",
            &path_text(&installed),
        ])
        .current_dir(root)
        .status()
        .map_err(|error| format!("native_materializer_launch_failed:{error}"))?;
    if !status.success() {
        return Err(format!("native_materializer_exit:{:?}", status.code()));
    }
    Ok(
        json!({"schema":"narada.native_distribution.materialize.v1","status":"materialized","authority":path_text(&materializer)}),
    )
}

fn sync_site_registries(root: &Path, contract_path: &Path) -> Result<(), String> {
    let contract: Value = serde_json::from_slice(
        &fs::read(contract_path).map_err(|error| format!("native_contract_read_failed:{error}"))?,
    )
    .map_err(|error| format!("native_contract_parse_failed:{error}"))?;
    let site_ids = contract["sites"]
        .as_array()
        .ok_or("native_contract_sites_required")?
        .iter()
        .map(|site| {
            site["site_id"]
                .as_str()
                .filter(|value| !value.is_empty())
                .ok_or("native_contract_site_id_required")
        })
        .collect::<Result<Vec<_>, _>>()?;
    let registrar = current_artifact(
        root,
        "packages/mcp-registrar/dist/native",
        &executable_name("narada-mcp-registrar"),
    )?;
    for (index, site_id) in site_ids.iter().enumerate() {
        let request = json!({
            "jsonrpc":"2.0",
            "id":index + 1,
            "method":"tools/call",
            "params":{
                "name":"registrar_site_surface_registry_sync",
                "arguments":{"site_id":site_id}
            }
        });
        let body = serde_json::to_vec(&request).map_err(|error| error.to_string())?;
        let mut child = Command::new(&registrar)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|error| format!("native_registrar_launch_failed:{error}"))?;
        let mut input = child.stdin.take().ok_or("native_registrar_stdin_missing")?;
        write!(input, "Content-Length: {}\r\n\r\n", body.len())
            .and_then(|_| input.write_all(&body))
            .map_err(|error| format!("native_registrar_request_failed:{error}"))?;
        drop(input);
        let output = child
            .wait_with_output()
            .map_err(|error| format!("native_registrar_wait_failed:{error}"))?;
        if !output.status.success() {
            return Err(format!(
                "native_registrar_exit:{site_id}:{:?}:{}",
                output.status.code(),
                String::from_utf8_lossy(&output.stderr).trim()
            ));
        }
        let separator = output
            .stdout
            .windows(4)
            .position(|window| window == b"\r\n\r\n")
            .ok_or_else(|| format!("native_registrar_response_invalid:{site_id}"))?;
        let response: Value = serde_json::from_slice(&output.stdout[separator + 4..])
            .map_err(|error| format!("native_registrar_response_parse_failed:{site_id}:{error}"))?;
        if let Some(error) = response.get("error") {
            return Err(format!("native_registrar_sync_failed:{site_id}:{error}"));
        }
    }
    Ok(())
}

fn verify_native_distribution(root: &Path) -> Result<Value, String> {
    let forbidden = ["node", "bun", "pnpm", "tsx", "powershell", "pwsh"];
    let source = fs::read_to_string(root.join("native-distribution/src/main.rs"))
        .map_err(|error| error.to_string())?;
    let violations = forbidden
        .iter()
        .filter(|word| source.contains(&format!("Command::new(\"{word}\"")))
        .collect::<Vec<_>>();
    if !violations.is_empty() {
        return Err(format!(
            "native_distribution_forbidden_subprocess:{violations:?}"
        ));
    }
    let manifest_path = root.join(".ai/runtime/workspace-artifact-manifest.json");
    let manifest: Value = serde_json::from_slice(
        &fs::read(&manifest_path)
            .map_err(|error| format!("native_manifest_read_failed:{error}"))?,
    )
    .map_err(|error| format!("native_manifest_parse_failed:{error}"))?;
    let expected = manifest["manifest_fingerprint"]
        .as_str()
        .ok_or("native_manifest_fingerprint_required")?;
    let mut unsigned = manifest.clone();
    unsigned
        .as_object_mut()
        .ok_or("native_manifest_object_required")?
        .remove("manifest_fingerprint");
    let actual = sha256(
        &serde_json::to_vec(&strip_volatile_manifest_metadata(&unsigned))
            .map_err(|error| error.to_string())?,
    );
    if actual != expected {
        return Err(format!(
            "native_manifest_fingerprint_mismatch:{expected}:{actual}"
        ));
    }
    for artifact in ARTIFACTS {
        current_artifact(
            root,
            artifact.artifact_root,
            &executable_name(artifact.binary),
        )?;
    }
    Ok(
        json!({"schema":"narada.native_distribution.verification.v1","status":"passed","node_required":false,"bun_required":false,"pnpm_required":false,"artifact_count":ARTIFACTS.len()}),
    )
}

fn strip_volatile_manifest_metadata(value: &Value) -> Value {
    match value {
        Value::Array(values) => Value::Array(
            values
                .iter()
                .map(strip_volatile_manifest_metadata)
                .collect(),
        ),
        Value::Object(object) => Value::Object(
            object
                .iter()
                .filter_map(|(key, child)| {
                    if key == "generated_at" || key == "mtime_ms" {
                        None
                    } else {
                        Some((key.clone(), strip_volatile_manifest_metadata(child)))
                    }
                })
                .collect(),
        ),
        _ => value.clone(),
    }
}

fn current_artifact(root: &Path, relative_root: &str, name: &str) -> Result<PathBuf, String> {
    let artifact_root = root.join(relative_root);
    let pointer: Value = serde_json::from_slice(
        &fs::read(artifact_root.join("current.json")).map_err(|error| error.to_string())?,
    )
    .map_err(|error| error.to_string())?;
    let relative = pointer
        .pointer(&format!("/artifacts/{name}"))
        .and_then(Value::as_str)
        .ok_or_else(|| format!("native_pointer_artifact_missing:{name}"))?;
    let path = artifact_root.join(relative);
    if !path.is_file() {
        return Err(format!("native_artifact_missing:{}", path.display()));
    }
    Ok(path)
}

fn publish_immutable(path: &Path, bytes: &[u8]) -> Result<(), String> {
    if path.exists() {
        let existing = fs::read(path).map_err(|error| error.to_string())?;
        if existing != bytes {
            return Err(format!("native_artifact_collision:{}", path.display()));
        }
        return Ok(());
    }
    atomic_write(path, bytes)
}

fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let parent = path.parent().ok_or("native_distribution_parent_missing")?;
    fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    let temporary = parent.join(format!(
        ".{}.{}.tmp",
        path.file_name().unwrap().to_string_lossy(),
        std::process::id()
    ));
    fs::write(&temporary, bytes).map_err(|error| error.to_string())?;
    if path.exists() {
        fs::remove_file(path).map_err(|error| error.to_string())?;
    }
    fs::rename(&temporary, path).map_err(|error| error.to_string())
}

fn file_record(path: &PathBuf) -> Result<Value, String> {
    let bytes = fs::read(path).map_err(|error| error.to_string())?;
    let modified = fs::metadata(path)
        .and_then(|metadata| metadata.modified())
        .ok()
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|duration| duration.as_secs_f64() * 1000.0)
        .unwrap_or(0.0);
    Ok(
        json!({"path":path_text(path),"sha256":sha256(&bytes),"size":bytes.len(),"mtime_ms":modified}),
    )
}

fn executable_name(stem: &str) -> String {
    if cfg!(windows) {
        format!("{stem}.exe")
    } else {
        stem.into()
    }
}
fn path_text(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}
fn now() -> Result<String, String> {
    OffsetDateTime::now_utc()
        .format(&time::format_description::well_known::Rfc3339)
        .map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    #[test]
    fn native_distribution_has_no_javascript_subprocess() {
        verify_native_distribution_source(include_str!("main.rs")).unwrap();
    }
    fn verify_native_distribution_source(source: &str) -> Result<(), String> {
        for command in ["node", "bun", "pnpm", "tsx", "powershell", "pwsh"] {
            if source.contains(&format!("Command::new(\"{command}\"")) {
                return Err(command.into());
            }
        }
        Ok(())
    }
}
