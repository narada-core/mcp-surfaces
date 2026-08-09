use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::env;
use std::fs;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::path::{Component, Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::{mpsc, Arc, Mutex, OnceLock};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;

#[allow(dead_code)]
mod filesystem;
#[allow(dead_code)]
mod git;
mod protocol;
#[allow(dead_code)]
mod structured_command;

const CONTRACT_VERSION: u64 = 6;
const DEFAULT_REQUEST_TIMEOUT_MS: u64 = 240_000;
const DEFAULT_TOOL_TIMEOUT_GRACE_MS: u64 = 15_000;
const MAX_TRANSPORT_TIMEOUT_MS: u64 = 900_000;
const DEFAULT_LIVENESS_CHECK_MS: u64 = 5_000;
const DEFAULT_ORPHAN_GRACE_MS: u64 = 15_000;
const STATUS_TOOL: &str = "mcp_runtime_proxy_status";
const TAIL_LIMIT: usize = 8_000;

#[derive(Clone)]
struct Options {
    child_command: String,
    entrypoint: PathBuf,
    child_invocation_kind: String,
    child_applet: Option<String>,
    child_prefix_args: Vec<String>,
    child_args: Vec<String>,
    carrier_id: Option<String>,
    carrier_kind: Option<String>,
    registrar_command: Option<String>,
    registrar_entrypoint: Option<PathBuf>,
    artifact_manifest: Option<PathBuf>,
    materialization_sidecar: Option<PathBuf>,
    surface_id: Option<String>,
    request_timeout_ms: u64,
    tool_timeout_grace_ms: u64,
    diagnostics_dir: PathBuf,
    liveness_check_ms: u64,
    orphan_grace_ms: u64,
    runtime_contract_version: Option<u64>,
}

fn lexically_normalize_path(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                if !normalized.pop() {
                    normalized.push(component.as_os_str());
                }
            }
            _ => normalized.push(component.as_os_str()),
        }
    }
    normalized
}

const ORIENTATION_CONTRACT_SCHEMA: &str =
    "narada.mcp_runtime_proxy.orientation_entry_enforcement_contract.v1";
const ORIENTATION_CONTRACT_JSON: &str =
    include_str!("../../src/orientation-entry-enforcement-contract.json");
static ORIENTATION_CONTRACT: OnceLock<Value> = OnceLock::new();

fn orientation_contract() -> &'static Value {
    ORIENTATION_CONTRACT.get_or_init(|| {
        let value: Value = serde_json::from_str(ORIENTATION_CONTRACT_JSON)
            .expect("orientation_entry_enforcement_contract_json_invalid");
        assert_eq!(
            value.get("schema").and_then(Value::as_str),
            Some(ORIENTATION_CONTRACT_SCHEMA),
            "orientation_entry_enforcement_contract_schema_invalid"
        );
        value
    })
}

fn contract_value(path: &str) -> &'static Value {
    orientation_contract()
        .pointer(path)
        .unwrap_or_else(|| panic!("orientation_entry_enforcement_contract_path_missing:{path}"))
}

fn contract_string(path: &str) -> &'static str {
    contract_value(path)
        .as_str()
        .unwrap_or_else(|| panic!("orientation_entry_enforcement_contract_string_invalid:{path}"))
}

fn contract_string_array(path: &str) -> Vec<&'static str> {
    contract_value(path)
        .as_array()
        .unwrap_or_else(|| panic!("orientation_entry_enforcement_contract_array_invalid:{path}"))
        .iter()
        .map(|value| {
            value.as_str().unwrap_or_else(|| {
                panic!("orientation_entry_enforcement_contract_array_item_invalid:{path}")
            })
        })
        .collect()
}

fn contract_reason(field: &str) -> &'static str {
    contract_string(&format!("/state/reasons/{field}"))
}

fn has_duplicate_json_object_keys(source: &str) -> bool {
    enum Frame {
        Object(HashSet<String>),
        Array,
    }

    let bytes = source.as_bytes();
    let mut stack = Vec::<Frame>::new();
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'{' => stack.push(Frame::Object(HashSet::new())),
            b'[' => stack.push(Frame::Array),
            b'}' | b']' => {
                stack.pop();
            }
            b'"' => {
                let start = index;
                index += 1;
                let mut escaped = false;
                while index < bytes.len() {
                    if escaped {
                        escaped = false;
                    } else if bytes[index] == b'\\' {
                        escaped = true;
                    } else if bytes[index] == b'"' {
                        break;
                    }
                    index += 1;
                }
                if index >= bytes.len() {
                    return false;
                }
                let mut lookahead = index + 1;
                while lookahead < bytes.len() && bytes[lookahead].is_ascii_whitespace() {
                    lookahead += 1;
                }
                if bytes.get(lookahead) == Some(&b':') {
                    if let Some(Frame::Object(keys)) = stack.last_mut() {
                        let Ok(key) = serde_json::from_str::<String>(&source[start..=index]) else {
                            return false;
                        };
                        if !keys.insert(key) {
                            return true;
                        }
                    }
                }
            }
            _ => {}
        }
        index += 1;
    }
    false
}

fn json_file(path: &Path) -> Option<Value> {
    let source = fs::read_to_string(path).ok()?;
    if contract_string("/raw_json/duplicate_keys") == "reject"
        && has_duplicate_json_object_keys(&source)
    {
        return None;
    }
    serde_json::from_str(&source).ok()
}

fn non_empty_json_string(value: Option<&Value>) -> bool {
    value
        .and_then(Value::as_str)
        .is_some_and(|text| !text.trim().is_empty())
}

fn json_equivalent(left: &Value, right: &Value) -> bool {
    if left.is_number() && right.is_number() {
        return left
            .as_f64()
            .zip(right.as_f64())
            .is_some_and(|(left, right)| left == right);
    }
    left == right
}

fn safe_positive_integer(value: Option<&Value>, maximum: u64) -> bool {
    value.is_some_and(|value| {
        value
            .as_u64()
            .is_some_and(|number| number > 0 && number <= maximum)
            || value.as_f64().is_some_and(|number| {
                number.is_finite()
                    && number.fract() == 0.0
                    && number >= 1.0
                    && number <= maximum as f64
            })
    })
}

fn valid_orientation_coordinate(value: Option<&Value>) -> bool {
    let Some(coordinate) = value.and_then(Value::as_object) else {
        return false;
    };
    let integer_field = contract_string("/coordinate/positive_safe_integer_field");
    let integer_maximum = contract_value("/coordinate/positive_safe_integer_max")
        .as_u64()
        .expect("orientation_entry_enforcement_contract_integer_maximum_invalid");
    contract_string_array("/coordinate/non_empty_string_fields")
        .into_iter()
        .all(|field| non_empty_json_string(coordinate.get(field)))
        && safe_positive_integer(coordinate.get(integer_field), integer_maximum)
}

fn same_orientation_coordinate(left: Option<&Value>, right: Option<&Value>) -> bool {
    let (Some(left), Some(right)) = (
        left.and_then(Value::as_object),
        right.and_then(Value::as_object),
    ) else {
        return false;
    };
    valid_orientation_coordinate(Some(&Value::Object(left.clone())))
        && valid_orientation_coordinate(Some(&Value::Object(right.clone())))
        && contract_string_array("/coordinate/identity_fields")
            .into_iter()
            .all(|field| {
                left.get(field)
                    .zip(right.get(field))
                    .is_some_and(|(left, right)| json_equivalent(left, right))
            })
}

fn rule_set(name: &str) -> &'static Value {
    contract_value("/rule_sets")
        .get(name)
        .unwrap_or_else(|| panic!("orientation_entry_enforcement_contract_rule_set_missing:{name}"))
}

fn rule_pair<'a>(candidate: &'a Value, field: &str) -> (&'a str, &'a str) {
    let pair = candidate.as_array().unwrap_or_else(|| {
        panic!("orientation_entry_enforcement_contract_rule_pair_invalid:{field}")
    });
    assert_eq!(
        pair.len(),
        2,
        "orientation_entry_enforcement_contract_rule_pair_invalid:{field}"
    );
    (
        pair[0].as_str().unwrap_or_else(|| {
            panic!("orientation_entry_enforcement_contract_rule_pair_invalid:{field}")
        }),
        pair[1].as_str().unwrap_or_else(|| {
            panic!("orientation_entry_enforcement_contract_rule_pair_invalid:{field}")
        }),
    )
}

fn validate_rule_set(document: &Value, name: &str) -> bool {
    let rules = rule_set(name);
    let equals = rules
        .get("equals")
        .and_then(Value::as_array)
        .unwrap_or_else(|| panic!("orientation_entry_enforcement_contract_equals_invalid:{name}"));
    for candidate in equals {
        let rule = candidate.as_object().unwrap_or_else(|| {
            panic!("orientation_entry_enforcement_contract_equals_rule_invalid:{name}")
        });
        let path = rule.get("path").and_then(Value::as_str).unwrap_or_else(|| {
            panic!("orientation_entry_enforcement_contract_equals_path_invalid:{name}")
        });
        let Some(actual) = document.pointer(path) else {
            return false;
        };
        let expected = rule.get("value").unwrap_or_else(|| {
            panic!("orientation_entry_enforcement_contract_equals_value_invalid:{name}")
        });
        if !json_equivalent(actual, expected) {
            return false;
        }
    }
    if let Some(paths) = rules.get("non_empty_strings") {
        for path in paths.as_array().unwrap_or_else(|| {
            panic!("orientation_entry_enforcement_contract_string_rules_invalid:{name}")
        }) {
            let path = path.as_str().unwrap_or_else(|| {
                panic!("orientation_entry_enforcement_contract_string_path_invalid:{name}")
            });
            if !non_empty_json_string(document.pointer(path)) {
                return false;
            }
        }
    }
    if let Some(paths) = rules.get("coordinate_paths") {
        for path in paths.as_array().unwrap_or_else(|| {
            panic!("orientation_entry_enforcement_contract_coordinate_rules_invalid:{name}")
        }) {
            let path = path.as_str().unwrap_or_else(|| {
                panic!("orientation_entry_enforcement_contract_coordinate_path_invalid:{name}")
            });
            if !valid_orientation_coordinate(document.pointer(path)) {
                return false;
            }
        }
    }
    if let Some(pairs) = rules.get("equal_paths") {
        for candidate in pairs.as_array().unwrap_or_else(|| {
            panic!("orientation_entry_enforcement_contract_equal_path_rules_invalid:{name}")
        }) {
            let (left_path, right_path) = rule_pair(candidate, "equal_paths");
            let Some((left, right)) = document
                .pointer(left_path)
                .zip(document.pointer(right_path))
            else {
                return false;
            };
            if !json_equivalent(left, right) {
                return false;
            }
        }
    }
    if let Some(pairs) = rules.get("equal_coordinates") {
        for candidate in pairs.as_array().unwrap_or_else(|| {
            panic!("orientation_entry_enforcement_contract_equal_coordinate_rules_invalid:{name}")
        }) {
            let (left_path, right_path) = rule_pair(candidate, "equal_coordinates");
            if !same_orientation_coordinate(
                document.pointer(left_path),
                document.pointer(right_path),
            ) {
                return false;
            }
        }
    }
    true
}

fn blocked_orientation_state(
    entry_file: Option<&Path>,
    reason: &str,
    delivery_receipt_ref: Option<&str>,
) -> Value {
    json!({
        "schema": contract_string("/state/schema"),
        "required": true,
        "status": "blocked",
        "ordinary_work_gate": "acknowledgement_required",
        "reason": reason,
        "delivery_receipt_ref": delivery_receipt_ref,
        "acknowledgement_ref": Value::Null,
        "entry_file": entry_file,
        "next_call": contract_value("/state/next_call"),
    })
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum OrientationRequiredSignal {
    Absent,
    Required,
    NotRequired,
    Invalid,
}

fn orientation_required_signal() -> OrientationRequiredSignal {
    let variable = contract_string("/environment/required_signal");
    let value = match env::var(variable) {
        Ok(value) => value.trim().to_ascii_lowercase(),
        Err(env::VarError::NotPresent) => return OrientationRequiredSignal::Absent,
        Err(env::VarError::NotUnicode(_)) => return OrientationRequiredSignal::Invalid,
    };
    if value.is_empty() {
        return OrientationRequiredSignal::Absent;
    }
    if contract_string_array("/environment/required_values").contains(&value.as_str()) {
        return OrientationRequiredSignal::Required;
    }
    if contract_string_array("/environment/not_required_values").contains(&value.as_str()) {
        return OrientationRequiredSignal::NotRequired;
    }
    OrientationRequiredSignal::Invalid
}

fn orientation_entry_state() -> Value {
    let entry_variable = contract_string("/environment/entry_file");
    let configured = env::var(entry_variable)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());
    let configured_path = configured.as_ref().map(PathBuf::from);
    let entry_file = configured_path.as_ref().map(|path| {
        lexically_normalize_path(&if path.is_absolute() {
            path.clone()
        } else {
            env::current_dir()
                .unwrap_or_else(|_| PathBuf::from("."))
                .join(path)
        })
    });
    let signal = orientation_required_signal();
    if signal == OrientationRequiredSignal::Invalid {
        return blocked_orientation_state(
            entry_file.as_deref(),
            contract_reason("required_signal_invalid"),
            None,
        );
    }
    if signal == OrientationRequiredSignal::NotRequired && configured.is_some() {
        return blocked_orientation_state(
            entry_file.as_deref(),
            contract_reason("required_signal_conflict"),
            None,
        );
    }
    if signal == OrientationRequiredSignal::Required && configured.is_none() {
        return blocked_orientation_state(None, contract_reason("required_packet_missing"), None);
    }
    let Some(configured_path) = configured_path else {
        return json!({
            "schema": contract_string("/state/schema"),
            "required": false,
            "status": "not_required",
            "ordinary_work_gate": "open",
            "reason": contract_reason("not_supplied"),
            "delivery_receipt_ref": Value::Null,
            "acknowledgement_ref": Value::Null,
            "entry_file": Value::Null,
            "next_call": Value::Null,
        });
    };
    let entry_file = entry_file.expect("orientation_entry_file_resolution_missing");
    if !configured_path.is_absolute() {
        return blocked_orientation_state(
            Some(&entry_file),
            contract_reason("entry_path_invalid"),
            None,
        );
    }
    if !entry_file.exists() {
        return blocked_orientation_state(
            Some(&entry_file),
            contract_reason("entry_unavailable"),
            None,
        );
    }
    let Some(packet) = json_file(&entry_file) else {
        return blocked_orientation_state(
            Some(&entry_file),
            contract_reason("entry_invalid"),
            None,
        );
    };
    if !validate_rule_set(&packet, "packet_header") {
        return blocked_orientation_state(
            Some(&entry_file),
            contract_reason("entry_invalid"),
            None,
        );
    }
    let delivery_ref = packet
        .pointer(contract_string("/readback_paths/delivery_receipt_ref"))
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty());
    if !validate_rule_set(&packet, "delivery_binding") {
        return blocked_orientation_state(
            Some(&entry_file),
            contract_reason("delivery_binding_invalid"),
            delivery_ref,
        );
    }
    if !validate_rule_set(&packet, "acknowledgement_projection_ref") {
        return blocked_orientation_state(
            Some(&entry_file),
            contract_reason("acknowledgement_ref_invalid"),
            delivery_ref,
        );
    }
    let Some(relative_path) = packet
        .pointer(contract_string(
            "/readback_paths/acknowledgement_projection_path",
        ))
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
    else {
        return blocked_orientation_state(
            Some(&entry_file),
            contract_reason("acknowledgement_ref_invalid"),
            delivery_ref,
        );
    };
    let Some(parent) = entry_file.parent() else {
        return blocked_orientation_state(
            Some(&entry_file),
            contract_reason("acknowledgement_ref_invalid"),
            delivery_ref,
        );
    };
    let acknowledgement_path = lexically_normalize_path(&parent.join(relative_path));
    if acknowledgement_path.parent() != Some(parent) {
        return blocked_orientation_state(
            Some(&entry_file),
            contract_reason("acknowledgement_ref_invalid"),
            delivery_ref,
        );
    }
    if !acknowledgement_path.exists() {
        return blocked_orientation_state(
            Some(&entry_file),
            contract_reason("acknowledgement_required"),
            delivery_ref,
        );
    }
    let Some(acknowledgement) = json_file(&acknowledgement_path) else {
        return blocked_orientation_state(
            Some(&entry_file),
            contract_reason("acknowledgement_invalid"),
            delivery_ref,
        );
    };
    let combined = json!({
        "packet": packet,
        "acknowledgement": acknowledgement,
    });
    if !validate_rule_set(&combined, "acknowledgement_projection") {
        return blocked_orientation_state(
            Some(&entry_file),
            contract_reason("acknowledgement_invalid"),
            delivery_ref,
        );
    }
    let acknowledgement_ref = combined
        .pointer("/acknowledgement")
        .and_then(|value| value.pointer(contract_string("/readback_paths/acknowledgement_ref")))
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty());
    let Some(acknowledgement_ref) = acknowledgement_ref else {
        return blocked_orientation_state(
            Some(&entry_file),
            contract_reason("acknowledgement_invalid"),
            delivery_ref,
        );
    };
    json!({
        "schema": contract_string("/state/schema"),
        "required": true,
        "status": "open",
        "ordinary_work_gate": "open",
        "reason": contract_reason("acknowledged"),
        "delivery_receipt_ref": delivery_ref,
        "acknowledgement_ref": acknowledgement_ref,
        "entry_file": entry_file,
        "next_call": Value::Null,
    })
}

fn contract_method_admitted(field: &str, method: &str) -> bool {
    contract_string_array(&format!("/request_admission/{field}")).contains(&method)
}

fn contract_tool_admitted(options: &Options, request: &Value) -> bool {
    let Some(tool_name) = request.pointer("/params/name").and_then(Value::as_str) else {
        return false;
    };
    if contract_string_array("/request_admission/proxy_tool_calls").contains(&tool_name) {
        return true;
    }
    contract_value("/request_admission/allowed_tool_calls")
        .as_array()
        .expect("orientation_entry_enforcement_contract_allowed_tool_calls_invalid")
        .iter()
        .any(|candidate| {
            candidate.get("surface_id").and_then(Value::as_str) == options.surface_id.as_deref()
                && candidate
                    .get("tool_names")
                    .and_then(Value::as_array)
                    .is_some_and(|names| {
                        names
                            .iter()
                            .filter_map(Value::as_str)
                            .any(|name| name == tool_name)
                    })
        })
}

fn orientation_request_refusal(options: &Options, request: &Value) -> Option<Value> {
    let state = orientation_entry_state();
    if state.get("ordinary_work_gate").and_then(Value::as_str) == Some("open") {
        return None;
    }
    let method = request.get("method").and_then(Value::as_str).unwrap_or("");
    let is_request = request.get("id").is_some();
    let admitted = if is_request {
        contract_method_admitted("allowed_request_methods", method)
            || (method == "tools/call" && contract_tool_admitted(options, request))
    } else {
        contract_method_admitted("allowed_notification_methods", method)
    };
    if admitted {
        return None;
    }
    Some(state)
}

#[derive(Clone)]
struct WireMessage {
    value: Value,
    framed: bool,
}

enum Event {
    Carrier(WireMessage),
    CarrierClosed,
    Child(WireMessage),
    ChildOutputClosed,
    ChildStderr(Vec<u8>),
}

struct Pending {
    method: String,
    framed: bool,
    deadline: Instant,
    effective_timeout_ms: u64,
    requested_transport_timeout_ms: Option<u64>,
}

#[derive(Clone)]
struct FileSnapshot {
    path: PathBuf,
    exists: bool,
    modified_ms: Option<u128>,
    size: Option<u64>,
    sha256: Option<String>,
}

#[derive(Clone)]
struct FreshnessTracker {
    started_at: String,
    proxy_runtime: FileSnapshot,
    child_runtime: FileSnapshot,
}

struct NativeStartupTrace {
    started_at: String,
    started_clock: Instant,
    path: PathBuf,
    events: Vec<Value>,
    completed: bool,
}

fn main() {
    let args = env::args().skip(1).collect::<Vec<_>>();
    let result = match args.first().map(String::as_str) {
        Some("proxy") => run_proxy(&args[1..]),
        Some("filesystem") => filesystem::run(&args[1..]),
        Some("git") => git::run(&args[1..]),
        Some("structured-command") => structured_command::run(&args[1..]),
        Some(other) => Err(format!("narada_mcp_runtime_unknown_applet:{other}")),
        None => Err("narada_mcp_runtime_applet_required".to_string()),
    };
    if let Err(error) = result {
        eprintln!("{error}");
        std::process::exit(1);
    }
}

fn parse_options(args: &[String]) -> Result<Options, String> {
    if args.iter().any(|arg| arg == "--list-runtime-instances") {
        return Err("list_runtime_instances_dispatched_separately".to_string());
    }
    let split = args
        .iter()
        .position(|arg| arg == "--")
        .unwrap_or(args.len());
    let mut values = HashMap::<String, String>::new();
    let mut index = 0;
    while index < split {
        let key = &args[index];
        if !key.starts_with("--") {
            return Err(format!("mcp_runtime_proxy_unknown_argument:{key}"));
        }
        let value = args
            .get(index + 1)
            .ok_or_else(|| format!("mcp_runtime_proxy_argument_value_required:{key}"))?;
        values.insert(key.clone(), value.clone());
        index += 2;
    }
    let contract = values
        .get("--runtime-contract-version")
        .map(|value| positive(value, "runtime_contract_version"))
        .transpose()?;
    let child_command = values
        .get("--child-command")
        .cloned()
        .ok_or("mcp_runtime_proxy_missing_child_command")?;
    let child_invocation_kind = values
        .get("--child-invocation-kind")
        .cloned()
        .unwrap_or_else(|| "entrypoint".to_string());
    if child_invocation_kind != "entrypoint"
        && child_invocation_kind != "native_applet"
        && child_invocation_kind != "native_entrypoint"
    {
        return Err("mcp_runtime_proxy_invalid_child_invocation_kind".to_string());
    }
    let child_applet = values.get("--child-applet").cloned();
    if child_invocation_kind == "native_applet" && child_applet.is_none() {
        return Err("mcp_runtime_proxy_missing_child_applet".to_string());
    }
    let child_prefix_args = values
        .get("--child-prefix-args")
        .map(|value| {
            serde_json::from_str::<Vec<String>>(value)
                .map_err(|_| "mcp_runtime_proxy_invalid_child_prefix_args".to_string())
        })
        .transpose()?
        .unwrap_or_default();
    let entrypoint = values
        .get("--entrypoint")
        .map(PathBuf::from)
        .ok_or("mcp_runtime_proxy_missing_entrypoint")?;
    let registrar_entrypoint = values.get("--registrar-entrypoint").map(PathBuf::from);
    let registrar_command = values.get("--registrar-command").cloned();
    if contract.unwrap_or(0) >= 3 && registrar_entrypoint.is_some() && registrar_command.is_none() {
        return Err("mcp_runtime_proxy_missing_registrar_command".to_string());
    }
    let diagnostics_dir = values
        .get("--diagnostics-dir")
        .map(PathBuf::from)
        .unwrap_or_else(default_diagnostics_dir);
    Ok(Options {
        child_command,
        entrypoint: absolute(entrypoint),
        child_invocation_kind,
        child_applet,
        child_prefix_args,
        child_args: if split < args.len() {
            args[split + 1..].to_vec()
        } else {
            Vec::new()
        },
        carrier_id: values.get("--carrier-id").cloned(),
        carrier_kind: values.get("--carrier-kind").cloned(),
        registrar_command,
        registrar_entrypoint: registrar_entrypoint.map(absolute),
        artifact_manifest: values
            .get("--artifact-manifest")
            .map(|value| absolute(PathBuf::from(value))),
        materialization_sidecar: values
            .get("--materialization-sidecar")
            .map(|value| absolute(PathBuf::from(value))),
        surface_id: values.get("--surface-id").cloned(),
        request_timeout_ms: values
            .get("--request-timeout-ms")
            .map(|value| positive(value, "request_timeout_ms"))
            .transpose()?
            .unwrap_or(DEFAULT_REQUEST_TIMEOUT_MS),
        tool_timeout_grace_ms: values
            .get("--tool-timeout-grace-ms")
            .map(|value| positive(value, "tool_timeout_grace_ms"))
            .transpose()?
            .unwrap_or(DEFAULT_TOOL_TIMEOUT_GRACE_MS),
        diagnostics_dir: absolute(diagnostics_dir),
        liveness_check_ms: values
            .get("--liveness-check-ms")
            .map(|value| positive(value, "liveness_check_ms"))
            .transpose()?
            .unwrap_or(DEFAULT_LIVENESS_CHECK_MS),
        orphan_grace_ms: values
            .get("--orphan-grace-ms")
            .map(|value| positive(value, "orphan_grace_ms"))
            .transpose()?
            .unwrap_or(DEFAULT_ORPHAN_GRACE_MS),
        runtime_contract_version: contract,
    })
}

fn run_proxy(args: &[String]) -> Result<(), String> {
    if args.iter().any(|arg| arg == "--list-runtime-instances") {
        return list_instances(args);
    }
    let options = parse_options(args)?;
    let startup_clock = Instant::now();
    let manifest_fingerprint = match preflight_workspace(&options) {
        Ok(value) => value,
        Err(refusal) => return preflight_refusal(&options, refusal),
    };
    if options.runtime_contract_version != Some(CONTRACT_VERSION) {
        let (code, reason) = if options.runtime_contract_version.is_none() {
            (
                "runtime_contract_version_missing",
                "The launch did not declare the MCP runtime contract version.",
            )
        } else {
            (
                "runtime_contract_version_mismatch",
                "The launch declares an obsolete MCP runtime contract version.",
            )
        };
        return preflight_refusal(
            &options,
            refusal(
                code,
                reason,
                json!({
                    "actual_runtime_contract_version": options.runtime_contract_version,
                    "expected_runtime_contract_version": CONTRACT_VERSION,
                    "remediation": "Regenerate the carrier configuration with the current registrar before launching this surface."
                }),
            ),
        );
    }
    if let Some(sidecar) = &options.materialization_sidecar {
        if let Err(refusal) =
            preflight_materialization(&options, sidecar, manifest_fingerprint.as_deref())
        {
            return preflight_refusal(&options, refusal);
        }
    }
    if !options.entrypoint.is_file() {
        return Err(format!(
            "mcp_runtime_proxy_entrypoint_not_found:{}",
            options.entrypoint.display()
        ));
    }

    fs::create_dir_all(&options.diagnostics_dir)
        .map_err(|error| format!("mcp_runtime_proxy_diagnostics_create_failed:{error}"))?;
    write_startup_phase_trace(&options, startup_clock.elapsed().as_secs_f64() * 1000.0);
    let mut startup_trace = NativeStartupTrace {
        started_at: now_iso(),
        started_clock: startup_clock,
        path: options.diagnostics_dir.join(format!(
            "startup-{}.json",
            safe_segment(options.surface_id.as_deref().unwrap_or("surface"))
        )),
        events: Vec::new(),
        completed: false,
    };
    record_startup_event(
        &mut startup_trace,
        &options,
        "preflight_ok",
        json!({
            "runtime_contract_version": options.runtime_contract_version,
            "artifact_manifest_fingerprint": manifest_fingerprint,
        }),
        false,
    );
    let resolved_child_command = resolve_child_command(&options.child_command);
    let mut command = Command::new(&resolved_child_command);
    let child_entry = if options.child_invocation_kind == "native_applet" {
        Some(Path::new(
            options.child_applet.as_deref().unwrap_or_default(),
        ))
    } else if options.child_invocation_kind == "native_entrypoint" {
        None
    } else {
        Some(options.entrypoint.as_path())
    };
    command.args(&options.child_prefix_args);
    if let Some(child_entry) = child_entry {
        command.arg(child_entry);
    }
    command.args(&options.child_args);
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000 | 0x00000004);
    }
    let child = command
        .spawn()
        .map_err(|error| format!("mcp_runtime_proxy_child_spawn_failed:{error}"))?;
    let child_pid = child.id();
    let child = Arc::new(Mutex::new(child));
    let _job = assign_kill_job(&child)?;
    resume_main_thread(child_pid)?;
    record_startup_event(
        &mut startup_trace,
        &options,
        "child_spawned",
        json!({
            "child_pid": child_pid,
            "child_command": options.child_command,
            "child_prefix_args": options.child_prefix_args,
            "child_invocation_kind": options.child_invocation_kind,
            "child_applet": options.child_applet,
        }),
        false,
    );
    let child_stdin = Arc::new(Mutex::new(child.lock().map_err(lock_error)?.stdin.take()));
    let child_stdout = child
        .lock()
        .map_err(lock_error)?
        .stdout
        .take()
        .ok_or("mcp_runtime_proxy_child_stdout_missing")?;
    let child_stderr = child
        .lock()
        .map_err(lock_error)?
        .stderr
        .take()
        .ok_or("mcp_runtime_proxy_child_stderr_missing")?;
    let proxy_pid = std::process::id();
    let started_at = now_iso();
    let freshness = FreshnessTracker {
        started_at: started_at.clone(),
        proxy_runtime: file_snapshot(
            &env::current_exe().unwrap_or_else(|_| PathBuf::from("narada-mcp-runtime")),
        ),
        child_runtime: file_snapshot(&options.entrypoint),
    };
    write_instance(
        &options,
        proxy_pid,
        child_pid,
        &started_at,
        "live",
        None,
        &freshness,
    )?;
    emit_runtime_start(&options, proxy_pid, child_pid);

    let (sender, receiver) = mpsc::channel::<Event>();
    spawn_reader(io::stdin(), sender.clone(), true);
    spawn_reader(child_stdout, sender.clone(), false);
    {
        let sender = sender.clone();
        thread::spawn(move || {
            let mut reader = BufReader::new(child_stderr);
            let mut buffer = [0_u8; 4096];
            loop {
                match reader.read(&mut buffer) {
                    Ok(0) | Err(_) => break,
                    Ok(count) => {
                        let _ = sender.send(Event::ChildStderr(buffer[..count].to_vec()));
                    }
                }
            }
        });
    }

    let mut stdout = io::stdout().lock();
    let mut pending = HashMap::<String, Pending>::new();
    let mut stderr_tail = Vec::<u8>::new();
    let mut carrier_closed_at: Option<Instant> = None;
    let mut child_output_closed = false;
    let mut last_heartbeat = Instant::now();
    loop {
        match receiver.recv_timeout(Duration::from_millis(25)) {
            Ok(Event::Carrier(message)) => {
                if is_status_call(&message.value) {
                    let response = status_response(
                        &message.value,
                        &options,
                        proxy_pid,
                        child_pid,
                        manifest_fingerprint.as_deref(),
                        &freshness,
                    );
                    write_wire(&mut stdout, &response, message.framed)?;
                    continue;
                }
                if let Some(state) = orientation_request_refusal(&options, &message.value) {
                    if let Some(id) = message
                        .value
                        .get("id")
                        .filter(|id| id.is_string() || id.is_number())
                        .cloned()
                    {
                        let reason = state
                            .get("reason")
                            .and_then(Value::as_str)
                            .unwrap_or("orientation_acknowledgement_required");
                        let response = json!({
                            "jsonrpc": "2.0",
                            "id": id,
                            "error": {
                                "code": -32000,
                                "message": format!("orientation_required:{reason}"),
                                "data": state,
                            },
                        });
                        write_wire(&mut stdout, &response, message.framed)?;
                    }
                    record_startup_event(
                        &mut startup_trace,
                        &options,
                        "request_refused",
                        json!({
                            "method": message.value.get("method"),
                            "request_id": message.value.get("id"),
                            "reason": state.get("reason"),
                            "ordinary_work_gate": state.get("ordinary_work_gate"),
                            "delivery_receipt_ref": state.get("delivery_receipt_ref"),
                        }),
                        false,
                    );
                    continue;
                }
                if let Some((id, method)) = request_identity(&message.value) {
                    if method == "initialize" || method == "tools/list" {
                        record_startup_event(
                            &mut startup_trace,
                            &options,
                            "request_forwarded",
                            json!({
                                "method": method,
                                "request_id": id,
                            }),
                            false,
                        );
                    }
                    let requested = requested_transport_timeout(&message.value);
                    let effective = effective_timeout(
                        options.request_timeout_ms,
                        requested,
                        options.tool_timeout_grace_ms,
                    );
                    pending.insert(
                        id,
                        Pending {
                            method,
                            framed: message.framed,
                            deadline: Instant::now() + Duration::from_millis(effective),
                            effective_timeout_ms: effective,
                            requested_transport_timeout_ms: requested,
                        },
                    );
                }
                write_child(&child_stdin, &message.value)?;
            }
            Ok(Event::CarrierClosed) => {
                carrier_closed_at.get_or_insert_with(Instant::now);
                let _ = child_stdin.lock().map_err(lock_error)?.take();
            }
            Ok(Event::Child(mut message)) => {
                let id = json_id(&message.value);
                let framed = id
                    .as_ref()
                    .and_then(|value| pending.get(value).map(|entry| entry.framed))
                    .unwrap_or(message.framed);
                if let Some(id) = id {
                    let method = pending.get(&id).map(|entry| entry.method.clone());
                    if method.as_deref() == Some("tools/list") {
                        inject_status_tool(&mut message.value);
                    }
                    if matches!(method.as_deref(), Some("initialize" | "tools/list")) {
                        record_startup_event(
                            &mut startup_trace,
                            &options,
                            "child_response",
                            json!({
                                "method": method,
                                "request_id": id,
                            }),
                            method.as_deref() == Some("tools/list"),
                        );
                    }
                    pending.remove(&id);
                }
                write_wire(&mut stdout, &message.value, framed)?;
            }
            Ok(Event::ChildOutputClosed) => child_output_closed = true,
            Ok(Event::ChildStderr(bytes)) => {
                io::stderr()
                    .write_all(&bytes)
                    .map_err(|error| format!("mcp_runtime_proxy_stderr_forward_failed:{error}"))?;
                append_tail(&mut stderr_tail, &bytes);
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => child_output_closed = true,
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }

        let now = Instant::now();
        let timed_out = pending
            .iter()
            .filter(|(_, request)| request.deadline <= now)
            .map(|(id, _)| id.clone())
            .collect::<Vec<_>>();
        for id in timed_out {
            if let Some(request) = pending.remove(&id) {
                send_cancel(&child_stdin, &id)?;
                let error = proxy_error(
                    &id,
                    &request,
                    &options,
                    "child_request_timeout",
                    format!(
                        "child_request_timeout:{}:{}ms",
                        request.method, request.effective_timeout_ms
                    ),
                    None,
                    &stderr_tail,
                );
                write_wire(&mut stdout, &error, request.framed)?;
                write_forensic(
                    &options,
                    "child_request_timeout",
                    &id,
                    &request.method,
                    child_pid,
                    &stderr_tail,
                )?;
                child.lock().map_err(lock_error)?.kill().ok();
            }
        }

        let exit = child
            .lock()
            .map_err(lock_error)?
            .try_wait()
            .map_err(|error| format!("mcp_runtime_proxy_child_wait_failed:{error}"))?;
        if let Some(status) = exit {
            let code = status.code();
            for _ in 0..20 {
                match receiver.recv_timeout(Duration::from_millis(5)) {
                    Ok(Event::ChildStderr(bytes)) => {
                        io::stderr().write_all(&bytes).map_err(io_string)?;
                        append_tail(&mut stderr_tail, &bytes);
                    }
                    Ok(Event::Child(mut message)) => {
                        let id = json_id(&message.value);
                        let framed = id
                            .as_ref()
                            .and_then(|value| pending.get(value).map(|entry| entry.framed))
                            .unwrap_or(message.framed);
                        if let Some(id) = id {
                            if pending.get(&id).map(|entry| entry.method.as_str())
                                == Some("tools/list")
                            {
                                inject_status_tool(&mut message.value);
                            }
                            pending.remove(&id);
                        }
                        write_wire(&mut stdout, &message.value, framed)?;
                    }
                    Ok(_) => {}
                    Err(_) => break,
                }
            }
            for (id, request) in pending.drain() {
                let error = proxy_error(
                    &id,
                    &request,
                    &options,
                    "child_exited_before_response",
                    format!(
                        "child_exited_before_response:{}",
                        code.map(|value| value.to_string())
                            .unwrap_or_else(|| "signal".to_string())
                    ),
                    code,
                    &stderr_tail,
                );
                write_wire(&mut stdout, &error, request.framed)?;
                write_forensic(
                    &options,
                    "child_exited_before_response",
                    &id,
                    &request.method,
                    child_pid,
                    &stderr_tail,
                )?;
            }
            write_instance(
                &options,
                proxy_pid,
                child_pid,
                &started_at,
                "closed",
                code,
                &freshness,
            )?;
            emit_runtime_exit(
                &options,
                child_pid,
                if status.success() { "ok" } else { "failed" },
            );
            stdout.flush().ok();
            if status.success() {
                return Ok(());
            }
            return Err(format!(
                "mcp_runtime_proxy_child_exit:{}",
                code.unwrap_or(1)
            ));
        }
        if let Some(closed_at) = carrier_closed_at {
            if now.duration_since(closed_at) >= Duration::from_millis(options.orphan_grace_ms) {
                child.lock().map_err(lock_error)?.kill().ok();
            }
        }
        if carrier_closed_at.is_none()
            && last_heartbeat.elapsed() >= Duration::from_millis(options.liveness_check_ms)
        {
            write_instance(
                &options,
                proxy_pid,
                child_pid,
                &started_at,
                "live",
                None,
                &freshness,
            )?;
            last_heartbeat = Instant::now();
        }
        let _ = child_output_closed;
    }
}

fn spawn_reader<R: Read + Send + 'static>(reader: R, sender: mpsc::Sender<Event>, carrier: bool) {
    thread::spawn(move || {
        let mut reader = BufReader::new(reader);
        loop {
            match read_wire(&mut reader) {
                Ok(Some(message)) => {
                    let event = if carrier {
                        Event::Carrier(message)
                    } else {
                        Event::Child(message)
                    };
                    if sender.send(event).is_err() {
                        break;
                    }
                }
                Ok(None) | Err(_) => {
                    let _ = sender.send(if carrier {
                        Event::CarrierClosed
                    } else {
                        Event::ChildOutputClosed
                    });
                    break;
                }
            }
        }
    });
}

fn read_wire<R: BufRead>(reader: &mut R) -> io::Result<Option<WireMessage>> {
    let mut first = String::new();
    if reader.read_line(&mut first)? == 0 {
        return Ok(None);
    }
    if first.trim().is_empty() {
        return read_wire(reader);
    }
    if first.to_ascii_lowercase().starts_with("content-length:") {
        let length = first
            .split_once(':')
            .and_then(|(_, value)| value.trim().parse::<usize>().ok())
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "invalid Content-Length"))?;
        loop {
            let mut header = String::new();
            reader.read_line(&mut header)?;
            if header == "\r\n" || header == "\n" || header.is_empty() {
                break;
            }
        }
        let mut body = vec![0_u8; length];
        reader.read_exact(&mut body)?;
        let value = serde_json::from_slice(&body).map_err(json_io)?;
        return Ok(Some(WireMessage {
            value,
            framed: true,
        }));
    }
    let value = parse_json_prefix(first.trim())
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "invalid JSON-RPC line"))?;
    Ok(Some(WireMessage {
        value,
        framed: false,
    }))
}

fn parse_json_prefix(text: &str) -> Option<Value> {
    serde_json::Deserializer::from_str(text)
        .into_iter::<Value>()
        .next()?
        .ok()
}

fn write_wire<W: Write>(writer: &mut W, value: &Value, framed: bool) -> Result<(), String> {
    let body = serde_json::to_vec(value)
        .map_err(|error| format!("mcp_runtime_proxy_json_encode_failed:{error}"))?;
    if framed {
        write!(writer, "Content-Length: {}\r\n\r\n", body.len()).map_err(io_string)?;
    }
    writer.write_all(&body).map_err(io_string)?;
    if !framed {
        writer.write_all(b"\n").map_err(io_string)?;
    }
    writer.flush().map_err(io_string)
}

fn write_child(stdin: &Arc<Mutex<Option<ChildStdin>>>, value: &Value) -> Result<(), String> {
    let mut guard = stdin.lock().map_err(lock_error)?;
    let stream = guard
        .as_mut()
        .ok_or("mcp_runtime_proxy_child_stdin_closed")?;
    write_wire(stream, value, false)
}

fn request_identity(value: &Value) -> Option<(String, String)> {
    let id = json_id(value)?;
    let method = value.get("method")?.as_str()?.to_string();
    Some((id, method))
}

fn json_id(value: &Value) -> Option<String> {
    match value.get("id")? {
        Value::String(value) => Some(format!("s:{value}")),
        Value::Number(value) => Some(format!("n:{value}")),
        _ => None,
    }
}

fn id_value(id: &str) -> Value {
    if let Some(value) = id.strip_prefix("s:") {
        Value::String(value.to_string())
    } else if let Some(value) = id.strip_prefix("n:") {
        serde_json::from_str(value).unwrap_or(Value::Null)
    } else {
        Value::Null
    }
}

fn requested_transport_timeout(value: &Value) -> Option<u64> {
    value
        .pointer("/params/_meta/narada_request_timeout_ms")?
        .as_u64()
        .filter(|value| *value > 0)
}

fn effective_timeout(proxy: u64, requested: Option<u64>, grace: u64) -> u64 {
    requested
        .map(|value| proxy.max(value.min(MAX_TRANSPORT_TIMEOUT_MS).saturating_add(grace)))
        .unwrap_or(proxy)
}

fn is_status_call(value: &Value) -> bool {
    value.get("method").and_then(Value::as_str) == Some("tools/call")
        && value.pointer("/params/name").and_then(Value::as_str) == Some(STATUS_TOOL)
        && value
            .get("id")
            .is_some_and(|id| id.is_string() || id.is_number())
}

fn inject_status_tool(value: &mut Value) {
    let Some(tools) = value
        .pointer_mut("/result/tools")
        .and_then(Value::as_array_mut)
    else {
        return;
    };
    if tools
        .iter()
        .any(|tool| tool.get("name").and_then(Value::as_str) == Some(STATUS_TOOL))
    {
        return;
    }
    tools.push(status_tool_definition());
}

fn status_tool_definition() -> Value {
    json!({
        "name": STATUS_TOOL,
        "description": "Inspect carrier-bound proxy/server liveness and build/runtime freshness, including the machine-readable supervisor restart action.",
        "inputSchema": { "type": "object", "properties": {}, "additionalProperties": false },
        "annotations": { "title": STATUS_TOOL, "readOnlyHint": true, "destructiveHint": false, "idempotentHint": true, "openWorldHint": false }
    })
}

fn status_response(
    request: &Value,
    options: &Options,
    proxy_pid: u32,
    child_pid: u32,
    manifest_fingerprint: Option<&str>,
    freshness: &FreshnessTracker,
) -> Value {
    let id = request.get("id").cloned().unwrap_or(Value::Null);
    let runtime_freshness = evaluate_freshness(
        options,
        proxy_pid,
        child_pid,
        manifest_fingerprint,
        freshness,
    );
    let freshness_status = runtime_freshness
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let heartbeat_at = now_iso();
    let lease_expires_at = OffsetDateTime::now_utc()
        .saturating_add(time::Duration::milliseconds(
            (options.liveness_check_ms.saturating_mul(3)) as i64,
        ))
        .format(&Rfc3339)
        .unwrap_or_else(|_| heartbeat_at.clone());
    let payload = json!({
        "schema": "narada.mcp_runtime_proxy.status.v1",
        "status": "ok",
        "surface_id": options.surface_id,
        "liveness": {
            "schema": "narada.mcp_runtime_proxy.instance.v2",
            "surface_id": options.surface_id,
            "proxy_pid": proxy_pid,
            "parent_pid": parent_pid(),
            "child_pid": child_pid,
            "supervisor_pid": Value::Null,
            "managed_child_pid": child_pid,
            "server_pid": child_pid,
            "entrypoint": options.entrypoint,
            "child_invocation_kind": options.child_invocation_kind,
            "child_applet": options.child_applet,
            "started_at": freshness.started_at,
            "heartbeat_at": heartbeat_at,
            "lease_expires_at": lease_expires_at,
            "state": "live",
            "liveness_evidence": { "proxy_implementation": "native", "carrier_id": options.carrier_id },
            "artifact_manifest_path": options.artifact_manifest,
            "artifact_manifest_fingerprint": manifest_fingerprint,
            "generation_id": format!("{}:{}", options.surface_id.as_deref().unwrap_or("surface"), freshness.started_at),
            "supervisor_identity_path": Value::Null,
            "closed_at": Value::Null,
            "observed_state": "live",
            "stale_reasons": [],
        },
        "runtime_freshness": runtime_freshness
    });
    json!({ "jsonrpc": "2.0", "id": id, "result": {
        "content": [{ "type": "text", "text": format!("mcp_runtime_proxy_status: {freshness_status}\nproxy_pid: {proxy_pid}\nchild_pid: {child_pid}\nchild_pid_role: server\nserver_pid: {child_pid}\nrestart_owner: carrier_or_runtime_supervisor") }],
        "structuredContent": payload
    }})
}

fn file_snapshot(path: &Path) -> FileSnapshot {
    match fs::metadata(path) {
        Ok(metadata) => FileSnapshot {
            path: absolute(path.to_path_buf()),
            exists: true,
            modified_ms: metadata
                .modified()
                .ok()
                .and_then(|value| value.duration_since(UNIX_EPOCH).ok())
                .map(|value| value.as_millis()),
            size: Some(metadata.len()),
            sha256: fs::read(path).ok().map(|bytes| sha256_bytes(&bytes)),
        },
        Err(_) => FileSnapshot {
            path: absolute(path.to_path_buf()),
            exists: false,
            modified_ms: None,
            size: None,
            sha256: None,
        },
    }
}

fn snapshot_json(snapshot: &FileSnapshot) -> Value {
    json!({
        "path": snapshot.path,
        "exists": snapshot.exists,
        "mtime_ms": snapshot.modified_ms,
        "size": snapshot.size,
        "sha256": snapshot.sha256,
    })
}

fn evaluate_freshness(
    options: &Options,
    proxy_pid: u32,
    child_pid: u32,
    manifest_fingerprint: Option<&str>,
    tracker: &FreshnessTracker,
) -> Value {
    let current_proxy = file_snapshot(&tracker.proxy_runtime.path);
    let current_child = file_snapshot(&tracker.child_runtime.path);
    let mut reasons = Vec::<Value>::new();
    let mut evidence_unknown = false;
    for (name, started, current) in [
        ("proxy_runtime", &tracker.proxy_runtime, &current_proxy),
        ("child_runtime", &tracker.child_runtime, &current_child),
    ] {
        if !current.exists {
            evidence_unknown = true;
            reasons.push(json!({ "code": "runtime_file_missing", "evidence": "unknown", "name": name, "path": current.path }));
        } else if started.sha256 != current.sha256 {
            reasons.push(json!({
                "code": "runtime_changed_since_process_start",
                "name": name,
                "path": current.path,
                "started_sha256": started.sha256,
                "current_sha256": current.sha256,
                "started_size": started.size,
                "current_size": current.size,
            }));
        }
    }
    let stale = reasons
        .iter()
        .any(|reason| reason.get("evidence").and_then(Value::as_str) != Some("unknown"));
    let status = if stale {
        "stale"
    } else if evidence_unknown {
        "unknown"
    } else {
        "current"
    };
    json!({
        "schema": "narada.mcp_runtime_proxy.runtime_freshness.v2",
        "status": status,
        "observed_at": now_iso(),
        "process_started_at": tracker.started_at,
        "proxy_pid": proxy_pid,
        "child_pid": child_pid,
        "surface_id": options.surface_id,
        "proxy_implementation": "native",
        "artifact_manifest_fingerprint": manifest_fingerprint,
        "runtime_files": [
            { "name": "proxy_runtime", "started": snapshot_json(&tracker.proxy_runtime), "current": snapshot_json(&current_proxy) },
            { "name": "child_runtime", "started": snapshot_json(&tracker.child_runtime), "current": snapshot_json(&current_child) },
        ],
        "source_files": [],
        "reasons": reasons,
        "reload_action": {
            "schema": "narada.mcp_runtime_proxy.supervisor_restart_action.v1",
            "kind": "restart_carrier_bound_surface",
            "operation": "restart",
            "owner": "carrier_or_runtime_supervisor",
            "target": { "scope": "carrier_bound_surface", "surface_id": options.surface_id, "proxy_pid": proxy_pid, "child_pid": child_pid },
            "automatic": false,
            "guidance": "Restart this carrier-bound proxy/server pair through the carrier or runtime supervisor. Restarting an mcp-loader child does not replace this process.",
        },
    })
}

fn send_cancel(stdin: &Arc<Mutex<Option<ChildStdin>>>, id: &str) -> Result<(), String> {
    let value = json!({ "jsonrpc": "2.0", "method": "notifications/cancelled", "params": { "requestId": id_value(id), "reason": "request timed out in mcp runtime proxy" } });
    if stdin.lock().map_err(lock_error)?.is_none() {
        return Ok(());
    }
    write_child(stdin, &value)
}

fn proxy_error(
    id: &str,
    request: &Pending,
    options: &Options,
    code: &str,
    message: String,
    exit_code: Option<i32>,
    stderr_tail: &[u8],
) -> Value {
    json!({ "jsonrpc": "2.0", "id": id_value(id), "error": {
        "code": -32000,
        "message": message,
        "data": {
            "schema": "narada.mcp_runtime_proxy.error.v1",
            "code": code,
            "method": request.method,
            "surface_id": options.surface_id,
            "entrypoint": options.entrypoint,
            "exit_code": exit_code,
            "signal": Value::Null,
            "stderr_tail": String::from_utf8_lossy(stderr_tail),
            "stdout_tail": "",
            "proxy_request_timeout_ms": options.request_timeout_ms,
            "effective_request_timeout_ms": request.effective_timeout_ms,
            "requested_transport_timeout_ms": request.requested_transport_timeout_ms,
            "tool_timeout_grace_ms": options.tool_timeout_grace_ms
        }
    }})
}

#[derive(Clone)]
struct Refusal {
    code: String,
    reason: String,
    details: Value,
}

fn refusal(code: &str, reason: &str, details: Value) -> Refusal {
    Refusal {
        code: code.to_string(),
        reason: reason.to_string(),
        details,
    }
}

fn preflight_workspace(options: &Options) -> Result<Option<String>, Refusal> {
    let Some(path) = &options.artifact_manifest else {
        return Err(refusal(
            "workspace_manifest_missing",
            "The launch did not provide an existing workspace artifact manifest.",
            json!({}),
        ));
    };
    let parsed = read_json(path).map_err(|error| {
        refusal(
            "workspace_manifest_stale",
            "The workspace artifact manifest is unreadable.",
            json!({ "error": error }),
        )
    })?;
    if parsed.get("schema").and_then(Value::as_str) != Some("narada.workspace_artifact_manifest.v1")
    {
        return Err(refusal(
            "workspace_manifest_stale",
            "The workspace artifact manifest has an unsupported schema or missing fingerprint.",
            json!({}),
        ));
    }
    let expected = parsed
        .get("manifest_fingerprint")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            refusal(
                "workspace_manifest_stale",
                "The workspace artifact manifest has an unsupported schema or missing fingerprint.",
                json!({}),
            )
        })?;
    let mut unsigned = parsed.clone();
    unsigned
        .as_object_mut()
        .map(|object| object.shift_remove("manifest_fingerprint"));
    let actual = sha256_bytes(
        &serde_json::to_vec(&strip_volatile_manifest_metadata(&unsigned)).unwrap_or_default(),
    );
    if actual != expected {
        return Err(refusal(
            "workspace_manifest_stale",
            "The workspace artifact manifest fingerprint does not match its contents.",
            json!({ "expected_fingerprint": expected, "actual_fingerprint": actual }),
        ));
    }
    let entrypoint = normalized_path(&options.entrypoint);
    let package = parsed
        .get("packages")
        .and_then(Value::as_array)
        .and_then(|packages| {
            packages.iter().find(|package| {
                package
                    .get("root")
                    .and_then(Value::as_str)
                    .map(|root| is_path_inside(root, &entrypoint))
                    .unwrap_or(false)
            })
        });
    if let Some(package) = package {
        verify_fingerprint(
            package.get("package_json"),
            "workspace_manifest_stale",
            "The package manifest changed after artifact generation.",
        )?;
        for (field, code, reason) in [
            (
                "build_configs",
                "workspace_manifest_stale",
                "The package build configuration changed after artifact generation.",
            ),
            (
                "source_files",
                "workspace_manifest_stale",
                "A source file changed after artifact generation.",
            ),
        ] {
            for fingerprint in package
                .get(field)
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                verify_fingerprint(Some(fingerprint), code, reason)?;
            }
        }
        let targets = package
            .get("export_targets")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        for target in &targets {
            let Some(fingerprint) = target.get("fingerprint") else {
                return Err(refusal(
                    "workspace_export_target_missing",
                    "A declared package export target is missing.",
                    json!({ "path": target.get("path") }),
                ));
            };
            verify_fingerprint(
                Some(fingerprint),
                "workspace_manifest_stale",
                "A declared package export target changed after artifact generation.",
            )?;
        }
        if !targets.iter().any(|target| {
            target
                .get("path")
                .and_then(Value::as_str)
                .map(|path| same_path(path, &entrypoint))
                .unwrap_or(false)
        }) {
            return Err(refusal(
                "workspace_artifact_missing",
                "The requested entrypoint is not a declared runtime artifact.",
                json!({ "path": entrypoint }),
            ));
        }
        for dependency in package
            .get("dependency_fingerprints")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            verify_fingerprint(
                dependency.get("package_json"),
                "workspace_dependency_unverified",
                "A local workspace dependency changed after artifact generation.",
            )?;
        }
    } else {
        let artifact = parsed
            .get("artifacts")
            .and_then(Value::as_array)
            .and_then(|items| {
                items.iter().find(|item| {
                    item.get("path")
                        .and_then(Value::as_str)
                        .map(|path| same_path(path, &entrypoint))
                        .unwrap_or(false)
                })
            })
            .ok_or_else(|| {
                refusal(
                    "workspace_artifact_missing",
                    "The entrypoint is not present in the workspace artifact manifest.",
                    json!({ "path": entrypoint }),
                )
            })?;
        verify_fingerprint(
            Some(artifact),
            "workspace_manifest_stale",
            "The manifest entrypoint changed after manifest generation.",
        )?;
    }
    Ok(Some(expected.to_string()))
}

fn verify_fingerprint(value: Option<&Value>, code: &str, reason: &str) -> Result<(), Refusal> {
    let value = value.ok_or_else(|| refusal(code, reason, json!({})))?;
    let path = value
        .get("path")
        .and_then(Value::as_str)
        .ok_or_else(|| refusal(code, reason, json!({})))?;
    let expected_hash = value
        .get("sha256")
        .and_then(Value::as_str)
        .ok_or_else(|| refusal(code, reason, json!({ "path": path })))?;
    let expected_size = value
        .get("size")
        .and_then(Value::as_u64)
        .ok_or_else(|| refusal(code, reason, json!({ "path": path })))?;
    let bytes = fs::read(path).map_err(|_| refusal(code, reason, json!({ "path": path })))?;
    if bytes.len() as u64 != expected_size || sha256_bytes(&bytes) != expected_hash {
        return Err(refusal(code, reason, json!({ "path": path })));
    }
    Ok(())
}

fn preflight_materialization(
    options: &Options,
    sidecar: &Path,
    manifest_fingerprint: Option<&str>,
) -> Result<(), Refusal> {
    let generation = read_json(sidecar).map_err(|error| {
        refusal(
            "materialization_generation_missing",
            "The materialization generation sidecar is missing or unreadable.",
            json!({ "error": error }),
        )
    })?;
    if generation.get("schema").and_then(Value::as_str)
        != Some("narada.mcp_materialization_generation.v1")
    {
        return Err(refusal(
            "materialization_generation_stale",
            "The materialization generation sidecar has an unsupported schema.",
            json!({}),
        ));
    }
    let expected = generation
        .get("generation_fingerprint")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            refusal(
                "materialization_generation_stale",
                "The materialization generation sidecar is structurally incomplete.",
                json!({}),
            )
        })?;
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
    if sha256_bytes(&serde_json::to_vec(&Value::Object(unsigned)).unwrap_or_default()) != expected {
        return Err(refusal(
            "materialization_generation_stale",
            "The materialization generation fingerprint does not match its contents.",
            generation_context(&generation),
        ));
    }
    if generation.get("contract_version").and_then(Value::as_u64) != Some(CONTRACT_VERSION) {
        return Err(refusal(
            "materialization_generation_stale",
            "The materialization contract version is obsolete.",
            generation_context(&generation),
        ));
    }
    let manifest_path = options
        .artifact_manifest
        .as_ref()
        .map(|path| normalized_path(path))
        .unwrap_or_default();
    if generation
        .get("artifact_manifest_path")
        .and_then(Value::as_str)
        .map(normalize_text_path)
        .as_deref()
        != Some(&manifest_path)
        || generation
            .get("artifact_manifest_fingerprint")
            .and_then(Value::as_str)
            != manifest_fingerprint
    {
        return Err(refusal(
            "materialization_generation_stale",
            "The materialization generation references a different workspace artifact manifest.",
            generation_context(&generation),
        ));
    }
    let registrar = generation
        .get("registrar_entrypoint")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            refusal(
                "materialization_generation_stale",
                "The materialization generation sidecar is structurally incomplete.",
                generation_context(&generation),
            )
        })?;
    if sha256_file(Path::new(registrar)).as_deref()
        != generation
            .get("registrar_fingerprint")
            .and_then(Value::as_str)
    {
        return Err(refusal(
            "materialization_generation_stale",
            "The registrar build changed after configuration generation.",
            generation_context(&generation),
        ));
    }
    let proxy_entrypoint = generation
        .get("proxy_entrypoint")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            refusal(
                "materialization_generation_stale",
                "The materialization generation sidecar is structurally incomplete.",
                generation_context(&generation),
            )
        })?;
    if sha256_file(Path::new(proxy_entrypoint)).as_deref()
        != generation.get("proxy_fingerprint").and_then(Value::as_str)
    {
        return Err(refusal(
            "materialization_generation_stale",
            "The selected runtime proxy changed after configuration generation.",
            generation_context(&generation),
        ));
    }
    let config_path = generation
        .get("config_path")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            refusal(
                "materialization_generation_stale",
                "The materialization generation sidecar is structurally incomplete.",
                generation_context(&generation),
            )
        })?;
    let expected_config = sidecar
        .to_string_lossy()
        .strip_suffix(".narada-generation.json")
        .map(str::to_string)
        .unwrap_or_default();
    if !same_path(config_path, &expected_config) {
        return Err(refusal(
            "materialization_generation_stale",
            "The materialization generation sidecar is not paired with its carrier configuration.",
            generation_context(&generation),
        ));
    }
    let plan_path = generation
        .get("runtime_materialization_plan_path")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            refusal(
                "materialization_generation_stale",
                "The materialization generation sidecar is structurally incomplete.",
                generation_context(&generation),
            )
        })?;
    let expected_plan = format!("{expected_config}.narada-runtime-plan.json");
    if !same_path(plan_path, &expected_plan) {
        return Err(refusal(
            "materialization_generation_stale",
            "The materialization generation sidecar is not paired with its runtime materialization plan.",
            generation_context(&generation),
        ));
    }
    let plan = read_json(Path::new(plan_path)).map_err(|error| {
        refusal(
            "materialization_generation_stale",
            "The runtime materialization plan is missing or unreadable.",
            json!({ "error": error, "runtime_materialization_plan_path": plan_path }),
        )
    })?;
    let expected_plan_fingerprint = generation
        .get("runtime_materialization_plan_fingerprint")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            refusal(
                "materialization_generation_stale",
                "The materialization generation sidecar is structurally incomplete.",
                generation_context(&generation),
            )
        })?;
    if plan.get("schema").and_then(Value::as_str) != Some("narada.runtime_materialization_plan.v1")
        || plan.get("status").and_then(Value::as_str) != Some("accepted")
        || plan.get("runtime_profile_kind").and_then(Value::as_str)
            != generation
                .get("runtime_profile_kind")
                .and_then(Value::as_str)
        || plan.get("plan_fingerprint").and_then(Value::as_str) != Some(expected_plan_fingerprint)
        || runtime_plan_fingerprint(&plan).as_deref() != Some(expected_plan_fingerprint)
    {
        return Err(refusal(
            "materialization_generation_stale",
            "The runtime materialization plan changed after generation.",
            generation_context(&generation),
        ));
    }
    let expected_matrix_fingerprint = generation
        .get("runtime_implementation_matrix_fingerprint")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            refusal(
                "materialization_generation_stale",
                "The materialization generation sidecar is structurally incomplete.",
                generation_context(&generation),
            )
        })?;
    let matrix_path = generation
        .get("runtime_implementation_matrix_path")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            refusal(
                "materialization_generation_stale",
                "The materialization generation sidecar is structurally incomplete.",
                generation_context(&generation),
            )
        })?;
    let plan_matrix_fingerprint = plan
        .get("source")
        .and_then(|source| source.get("matrix_fingerprint"))
        .and_then(Value::as_str);
    if plan_matrix_fingerprint != Some(expected_matrix_fingerprint)
        || sha256_file(Path::new(matrix_path)).as_deref() != Some(expected_matrix_fingerprint)
    {
        return Err(refusal(
            "materialization_generation_stale",
            "The runtime implementation matrix changed after generation.",
            generation_context(&generation),
        ));
    }
    let config = fs::read_to_string(config_path).map_err(|_| {
        refusal(
            "materialization_generation_stale",
            "The materialized configuration changed after generation.",
            generation_context(&generation),
        )
    })?;
    let kind = generation
        .get("carrier_kind")
        .and_then(Value::as_str)
        .unwrap_or("");
    if sha256_bytes(canonical_config(kind, &config).as_bytes())
        != generation
            .get("config_sha256")
            .and_then(Value::as_str)
            .unwrap_or("")
    {
        return Err(refusal(
            "materialization_generation_stale",
            "The materialized configuration changed after generation.",
            generation_context(&generation),
        ));
    }
    Ok(())
}

fn canonical_config(kind: &str, content: &str) -> String {
    let normalized = content.replace("\r\n", "\n").replace('\r', "\n");
    if kind != "codex" {
        return normalized;
    }
    let mut canonical = Vec::new();
    let mut in_mcp = false;
    let mut saw_mcp = false;
    for line in normalized.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("[mcp_servers.") && trimmed.ends_with(']') {
            in_mcp = true;
            saw_mcp = true;
            canonical.push(line);
            continue;
        }
        if trimmed.starts_with('[') && trimmed.ends_with(']') {
            in_mcp = false;
        }
        if in_mcp {
            canonical.push(line);
        }
    }
    if saw_mcp {
        canonical.join("\n").trim_end_matches('\n').to_string()
    } else {
        normalized.trim_end_matches('\n').to_string()
    }
}

fn preflight_refusal(options: &Options, mut refusal: Refusal) -> Result<(), String> {
    let mut details = refusal.details.as_object().cloned().unwrap_or_default();
    details.insert("remediation".to_string(), Value::String("Run pnpm build, materialize all carriers with the current registrar, and restart the carrier session.".to_string()));
    details.insert("recovery".to_string(), recovery(options, &refusal));
    refusal.details = Value::Object(details);
    eprintln!(
        "mcp_runtime_proxy_preflight_refused:{}:{}",
        refusal.code, refusal.reason
    );
    let mut reader = BufReader::new(io::stdin());
    if let Ok(Some(request)) = read_wire(&mut reader) {
        let id = request.value.get("id").cloned().unwrap_or(Value::Null);
        let response = json!({ "jsonrpc": "2.0", "id": id, "error": {
            "code": -32000,
            "message": format!("mcp_runtime_proxy_preflight_refused:{}", refusal.code),
            "data": {
                "schema": "narada.mcp_runtime_proxy.error.v1",
                "code": refusal.code,
                "method": request.value.get("method").cloned().unwrap_or(Value::Null),
                "surface_id": options.surface_id,
                "entrypoint": options.entrypoint,
                "artifact_manifest_path": options.artifact_manifest,
                "details": refusal.details
            }
        }});
        write_wire(&mut io::stdout().lock(), &response, request.framed)?;
    }
    Err(format!(
        "mcp_runtime_proxy_preflight_refused:{}",
        refusal.code
    ))
}

fn recovery(options: &Options, refusal: &Refusal) -> Value {
    let args = options.registrar_entrypoint.as_ref().map(|path| {
        vec![
            path.to_string_lossy().to_string(),
            "--materialize-all".to_string(),
        ]
    });
    let command = match (&options.registrar_command, args) {
        (Some(executable), Some(args)) => {
            json!({ "executable": executable, "args": args, "display": format!("\"{}\" \"{}\" \"--materialize-all\"", executable, options.registrar_entrypoint.as_ref().unwrap().display()) })
        }
        _ => Value::Null,
    };
    let materialization = refusal.code.starts_with("materialization_")
        || refusal.code.starts_with("runtime_contract_");
    let prefix = if materialization {
        "materialization"
    } else {
        "workspace-materialization"
    };
    let group_id = format!(
        "{prefix}-{}",
        &sha256_bytes(
            format!(
                "{}:{:?}:{:?}",
                refusal.code, options.artifact_manifest, options.materialization_sidecar
            )
            .as_bytes()
        )[..20]
    );
    if materialization {
        let config_path = options.materialization_sidecar.as_ref().map(|path| {
            let text = path.to_string_lossy();
            PathBuf::from(
                text.strip_suffix(".narada-generation.json")
                    .unwrap_or(&text),
            )
        });
        return json!({
            "schema": "narada.mcp_runtime_proxy.materialization_recovery.v1",
            "recovery_group_id": group_id,
            "deduplication": { "scope": "carrier_materialization", "key": group_id, "guidance": "Report one recovery action for this group; bootstrap surfaces sharing this id describe the same carrier failure." },
            "carrier": { "carrier_id": options.carrier_id, "carrier_kind": options.carrier_kind, "config_path": config_path },
            "regeneration": { "required": true, "available": !command.is_null(), "owner": "mcp-registrar", "command": command, "unavailable_reason": if options.registrar_entrypoint.is_none() { Value::String("The materialization record does not identify the registrar entrypoint.".to_string()) } else { Value::Null } },
            "restart_required": true,
            "restart": { "owner": options.carrier_kind.as_deref().unwrap_or("carrier"), "automatic": false, "instruction": carrier_restart(options.carrier_kind.as_deref()) }
        });
    }
    let workspace_root = options
        .artifact_manifest
        .as_ref()
        .and_then(|path| path.parent())
        .and_then(Path::parent)
        .and_then(Path::parent);
    json!({
        "schema": "narada.mcp_runtime_proxy.workspace_recovery.v1",
        "recovery_group_id": group_id,
        "deduplication": { "scope": "carrier_materialization", "key": group_id, "guidance": "Report one build/materialization action for this group; bootstrap surfaces sharing this id describe the same carrier failure." },
        "cause": { "code": refusal.code, "reason": refusal.reason, "details": refusal.details },
        "steps": [
            { "order": 1, "action": "build_workspace", "command": { "executable": "pnpm", "args": ["build"], "cwd": workspace_root, "display": "pnpm build" } },
            { "order": 2, "action": "materialize_all_carriers", "required": true, "owner": "mcp-registrar", "available": !command.is_null(), "command": command, "unavailable_reason": if options.registrar_entrypoint.is_none() { Value::String("The carrier launch does not identify the registrar entrypoint.".to_string()) } else { Value::Null } },
            { "order": 3, "action": "restart_carrier", "required": true, "automatic": false, "instruction": carrier_restart(options.carrier_kind.as_deref()) }
        ],
        "restart_required": true
    })
}

fn carrier_restart(kind: Option<&str>) -> &'static str {
    match kind {
        Some("codex") => "Restart Codex or start a new Codex session after materialization.",
        Some("kimi") => "Restart Kimi or start a new Kimi session after materialization.",
        Some("opencode") => {
            "Restart OpenCode or start a new OpenCode session after materialization."
        }
        _ => "Restart the carrier or start a new carrier session after materialization.",
    }
}

fn write_instance(
    options: &Options,
    proxy_pid: u32,
    child_pid: u32,
    started_at: &str,
    state: &str,
    exit_code: Option<i32>,
    freshness: &FreshnessTracker,
) -> Result<(), String> {
    let now = now_iso();
    let lease_expires_at = OffsetDateTime::now_utc()
        .saturating_add(time::Duration::milliseconds(
            (options.liveness_check_ms.saturating_mul(3)) as i64,
        ))
        .format(&Rfc3339)
        .unwrap_or_else(|_| now.clone());
    let record = json!({
        "schema": "narada.mcp_runtime_proxy.instance.v2",
        "surface_id": options.surface_id,
        "proxy_pid": proxy_pid,
        "parent_pid": parent_pid(),
        "child_pid": child_pid,
        "supervisor_pid": Value::Null,
        "managed_child_pid": child_pid,
        "server_pid": child_pid,
        "entrypoint": options.entrypoint,
        "started_at": started_at,
        "heartbeat_at": now,
        "lease_expires_at": lease_expires_at,
        "state": state,
        "liveness_evidence": { "proxy_implementation": "native", "carrier_id": options.carrier_id, "exit_code": exit_code },
        "runtime_freshness": evaluate_freshness(options, proxy_pid, child_pid, None, freshness),
        "artifact_manifest_path": options.artifact_manifest,
        "generation_id": format!("{}:{}", options.surface_id.as_deref().unwrap_or("surface"), started_at),
        "closed_at": if state == "closed" { Value::String(now) } else { Value::Null }
    });
    atomic_json(
        &options
            .diagnostics_dir
            .join(format!("instance-{proxy_pid}.json")),
        &record,
    )
}

fn list_instances(args: &[String]) -> Result<(), String> {
    let root = args
        .iter()
        .position(|value| value == "--diagnostics-dir")
        .and_then(|index| args.get(index + 1))
        .map(PathBuf::from)
        .unwrap_or_else(default_diagnostics_dir);
    let mut instances = Vec::new();
    if let Ok(entries) = fs::read_dir(&root) {
        for entry in entries.flatten().take(10_000) {
            let name = entry.file_name().to_string_lossy().to_string();
            if name.starts_with("instance-") && name.ends_with(".json") {
                if let Ok(value) = read_json(&entry.path()) {
                    instances.push(classify_instance(value));
                }
            }
        }
    }
    let count = |state: &str| {
        instances
            .iter()
            .filter(|value| value.get("observed_state").and_then(Value::as_str) == Some(state))
            .count()
    };
    let output = json!({
        "schema": "narada.mcp_runtime_proxy.instance_list.v1",
        "status": "ok",
        "diagnostics_dir": absolute(root),
        "observed_at": now_iso(),
        "counts": { "total": instances.len(), "live": count("live"), "stale": count("stale"), "reclaimed": count("reclaimed"), "closed": count("closed") },
        "instances": instances
    });
    println!(
        "{}",
        serde_json::to_string_pretty(&output).unwrap_or_else(|_| "{}".to_string())
    );
    Ok(())
}

fn classify_instance(mut value: Value) -> Value {
    let state = value
        .get("state")
        .and_then(Value::as_str)
        .unwrap_or("stale")
        .to_string();
    let mut reasons = Vec::<Value>::new();
    let observed = if state == "reclaimed" || state == "closed" {
        state
    } else {
        for (field, reason) in [
            ("proxy_pid", "proxy_pid_not_alive"),
            ("parent_pid", "parent_carrier_pid_not_alive"),
            ("child_pid", "child_pid_not_alive"),
        ] {
            if let Some(pid) = value.get(field).and_then(Value::as_u64) {
                if !process_alive(pid as u32) {
                    reasons.push(Value::String(reason.to_string()));
                }
            }
        }
        let lease_expired = value
            .get("lease_expires_at")
            .and_then(Value::as_str)
            .and_then(|value| OffsetDateTime::parse(value, &Rfc3339).ok())
            .is_some_and(|value| value < OffsetDateTime::now_utc());
        if lease_expired {
            reasons.push(Value::String("heartbeat_lease_expired".to_string()));
        }
        if reasons.is_empty() && state == "live" {
            "live".to_string()
        } else {
            "stale".to_string()
        }
    };
    if let Some(object) = value.as_object_mut() {
        object.insert("observed_state".to_string(), Value::String(observed));
        object.insert("stale_reasons".to_string(), Value::Array(reasons));
    }
    value
}

#[cfg(windows)]
fn process_alive(pid: u32) -> bool {
    use windows_sys::Win32::Foundation::{CloseHandle, WAIT_TIMEOUT};
    use windows_sys::Win32::System::Threading::{OpenProcess, WaitForSingleObject};
    const SYNCHRONIZE_ACCESS: u32 = 0x0010_0000;
    let handle = unsafe { OpenProcess(SYNCHRONIZE_ACCESS, 0, pid) };
    if handle.is_null() {
        return false;
    }
    let result = unsafe { WaitForSingleObject(handle, 0) } == WAIT_TIMEOUT;
    unsafe { CloseHandle(handle) };
    result
}

#[cfg(not(windows))]
fn process_alive(_pid: u32) -> bool {
    false
}

fn write_forensic(
    options: &Options,
    event: &str,
    id: &str,
    method: &str,
    child_pid: u32,
    stderr_tail: &[u8],
) -> Result<(), String> {
    let artifact = json!({
        "schema": "narada.mcp_runtime_proxy.forensic_artifact.v1",
        "event": event,
        "recorded_at": now_iso(),
        "proxy": { "pid": std::process::id(), "implementation": "native", "surface_id": options.surface_id },
        "child_process": { "pid": child_pid, "command": options.child_command, "child_prefix_args": options.child_prefix_args, "entrypoint": options.entrypoint },
        "request": { "id": id_value(id), "method": method },
        "stream_tails": { "stderr_tail": String::from_utf8_lossy(stderr_tail), "stdout_tail": "" }
    });
    atomic_json(
        &options.diagnostics_dir.join(format!(
            "{}-{}-{}.json",
            timestamp_ms(),
            safe_segment(options.surface_id.as_deref().unwrap_or("surface")),
            safe_segment(id)
        )),
        &artifact,
    )
}

fn generation_context(generation: &Value) -> Value {
    json!({
        "carrier_id": generation.get("carrier_id"),
        "carrier_kind": generation.get("carrier_kind"),
        "config_path": generation.get("config_path"),
        "registrar_entrypoint": generation.get("registrar_entrypoint"),
        "registrar_fingerprint": generation.get("registrar_fingerprint"),
        "proxy_implementation": generation.get("proxy_implementation"),
        "proxy_entrypoint": generation.get("proxy_entrypoint"),
        "proxy_fingerprint": generation.get("proxy_fingerprint"),
        "runtime_profile_kind": generation.get("runtime_profile_kind"),
        "runtime_materialization_plan_path": generation.get("runtime_materialization_plan_path"),
        "runtime_materialization_plan_fingerprint": generation.get("runtime_materialization_plan_fingerprint"),
        "runtime_implementation_matrix_path": generation.get("runtime_implementation_matrix_path"),
        "runtime_implementation_matrix_fingerprint": generation.get("runtime_implementation_matrix_fingerprint"),
        "materialization_generated_at": generation.get("generated_at")
    })
}

fn emit_runtime_start(options: &Options, proxy_pid: u32, child_pid: u32) {
    let site_id = env::var("NARADA_SITE_ID").unwrap_or_else(|_| "unknown-site".to_string());
    let authority_ref =
        env::var("NARADA_AUTHORITY_REF").unwrap_or_else(|_| format!("site:{site_id}:mcp-surfaces"));
    let carrier_session = env::var("NARADA_CARRIER_SESSION_ID")
        .ok()
        .filter(|value| !value.trim().is_empty());
    let observed_at = now_iso();
    let proxy_owner = format!("carrier-proxy-{proxy_pid}");
    let executable = env::current_exe()
        .ok()
        .map(|path| path.to_string_lossy().to_string());
    append_observation(
        options,
        &json!({
            "schema": "narada.mcp_runtime.resource_owner.v1", "owner_id": proxy_owner,
            "site_id": site_id, "authority_ref": authority_ref, "owner_kind": "carrier_proxy",
            "pid": proxy_pid, "process_started_at": Value::Null, "parent_owner_id": Value::Null,
            "surface_id": options.surface_id, "instance_id": Value::Null, "generation_id": Value::Null,
            "carrier_session_id": carrier_session, "executable_name": executable, "observed_at": observed_at
        }),
    );
    append_observation(
        options,
        &json!({
            "schema": "narada.mcp_runtime.resource_owner.v1", "owner_id": format!("proxy-child-{child_pid}"),
            "site_id": site_id, "authority_ref": authority_ref, "owner_kind": "nars_stdio_child",
            "pid": child_pid, "process_started_at": Value::Null, "parent_owner_id": proxy_owner,
            "surface_id": options.surface_id, "instance_id": Value::Null, "generation_id": Value::Null,
            "carrier_session_id": carrier_session, "executable_name": options.child_command, "observed_at": observed_at
        }),
    );
    append_observation(
        options,
        &json!({
            "schema": "narada.mcp_runtime.lifecycle_event.v1", "event_id": format!("event-native-{proxy_pid}-{}", timestamp_ms()),
            "occurred_at": observed_at, "site_id": site_id, "authority_ref": authority_ref,
            "owner_id": format!("proxy-child-{child_pid}"), "event_type": "process_started",
            "surface_id": options.surface_id, "instance_id": Value::Null, "generation_id": Value::Null,
            "request_id": Value::Null, "status": "ok", "inflight": Value::Null
        }),
    );
}

fn emit_runtime_exit(options: &Options, child_pid: u32, status: &str) {
    let site_id = env::var("NARADA_SITE_ID").unwrap_or_else(|_| "unknown-site".to_string());
    let authority_ref =
        env::var("NARADA_AUTHORITY_REF").unwrap_or_else(|_| format!("site:{site_id}:mcp-surfaces"));
    append_observation(
        options,
        &json!({
            "schema": "narada.mcp_runtime.lifecycle_event.v1", "event_id": format!("event-native-{}-{}", std::process::id(), timestamp_ms()),
            "occurred_at": now_iso(), "site_id": site_id, "authority_ref": authority_ref,
            "owner_id": format!("proxy-child-{child_pid}"), "event_type": "process_exited",
            "surface_id": options.surface_id, "instance_id": Value::Null, "generation_id": Value::Null,
            "request_id": Value::Null, "status": status, "inflight": Value::Null
        }),
    );
}

fn append_observation(options: &Options, record: &Value) {
    let Some(site_root) = env::var_os("NARADA_SITE_ROOT") else {
        return;
    };
    let source_id = safe_segment(&format!(
        "carrier-proxy-{}",
        options.surface_id.as_deref().unwrap_or("surface")
    ))
    .to_ascii_lowercase();
    let root = PathBuf::from(site_root)
        .join(".narada")
        .join("runtime")
        .join("mcp-runtime-observer")
        .join("sources");
    let path = root.join(format!("{source_id}.current.jsonl"));
    let line = match serde_json::to_string(record) {
        Ok(value) => format!("{value}\n"),
        Err(_) => return,
    };
    if fs::create_dir_all(&root).is_err() {
        return;
    }
    if fs::metadata(&path)
        .map(|value| value.len().saturating_add(line.len() as u64) > 8 * 1024 * 1024)
        .unwrap_or(false)
    {
        let rotated = root.join(format!(
            "{source_id}.{}.{}.jsonl",
            timestamp_ms(),
            std::process::id()
        ));
        let _ = fs::rename(&path, rotated);
    }
    if let Ok(mut file) = fs::OpenOptions::new().create(true).append(true).open(path) {
        let _ = file.write_all(line.as_bytes());
    }
}

fn atomic_json(path: &Path, value: &Value) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(io_string)?;
    }
    let temporary = path.with_extension(format!("tmp-{}", std::process::id()));
    fs::write(
        &temporary,
        format!(
            "{}\n",
            serde_json::to_string_pretty(value).map_err(|error| error.to_string())?
        ),
    )
    .map_err(io_string)?;
    if path.exists() {
        fs::remove_file(path).ok();
    }
    fs::rename(&temporary, path).map_err(io_string)
}

fn write_startup_phase_trace(options: &Options, preflight_ms: f64) {
    let path = options.diagnostics_dir.join(format!(
        "startup-phases-{}.json",
        safe_segment(options.surface_id.as_deref().unwrap_or("surface"))
    ));
    let _ = atomic_json(
        &path,
        &json!({
            "schema": "narada.mcp_runtime_proxy.startup_phases.v1",
            "surface_id": options.surface_id,
            "observed_at": now_iso(),
            "preflight_ms": preflight_ms,
            "child_invocation_kind": options.child_invocation_kind,
            "child_applet": options.child_applet,
        }),
    );
}

fn record_startup_event(
    trace: &mut NativeStartupTrace,
    options: &Options,
    event: &str,
    detail: Value,
    completed: bool,
) {
    trace.events.push(json!({
        "at": now_iso(),
        "elapsed_ms": trace.started_clock.elapsed().as_secs_f64() * 1000.0,
        "event": event,
        "detail": detail,
    }));
    trace.completed = trace.completed || completed;
    let value = json!({
        "schema": "narada.mcp_runtime_proxy.startup_trace.v1",
        "surface_id": options.surface_id.clone(),
        "entrypoint": options.entrypoint.clone(),
        "started_at": trace.started_at.clone(),
        "updated_at": now_iso(),
        "completed": trace.completed,
        "runtime_contract_version": options.runtime_contract_version,
        "proxy_implementation": "native",
        "proxy_pid": std::process::id(),
        "events": trace.events.clone(),
    });
    let _ = atomic_json(&trace.path, &value);
}

fn read_json(path: &Path) -> Result<Value, String> {
    let bytes = fs::read(path).map_err(|error| format!("{}:{error}", path.display()))?;
    serde_json::from_slice(&bytes).map_err(|error| format!("{}:{error}", path.display()))
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

fn sha256_file(path: &Path) -> Option<String> {
    fs::read(path).ok().map(|bytes| sha256_bytes(&bytes))
}
fn runtime_plan_fingerprint(plan: &Value) -> Option<String> {
    let mut unsigned = plan.as_object()?.clone();
    unsigned.remove("plan_fingerprint");
    serde_json::to_vec(&Value::Object(unsigned))
        .ok()
        .map(|bytes| sha256_bytes(&bytes))
}
fn sha256_bytes(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}
fn same_path(left: &str, right: &str) -> bool {
    normalize_text_path(left) == normalize_text_path(right)
}
fn normalized_path(path: &Path) -> String {
    normalize_text_path(&absolute(path.to_path_buf()).to_string_lossy())
}
fn normalize_text_path(path: &str) -> String {
    path.replace('/', "\\")
        .trim_end_matches('\\')
        .to_ascii_lowercase()
}
fn is_path_inside(root: &str, path: &str) -> bool {
    let root = normalize_text_path(root);
    path == root || path.starts_with(&(root + "\\"))
}
fn absolute(path: PathBuf) -> PathBuf {
    if path.is_absolute() {
        path
    } else {
        env::current_dir().unwrap_or_default().join(path)
    }
}

fn resolve_child_command(child_command: &str) -> PathBuf {
    let path = PathBuf::from(child_command);
    if path.is_absolute() {
        return path;
    }
    if path.exists() {
        return absolute(path);
    }

    let base = path
        .file_name()
        .map(|value| value.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    let is_bun = base == "bun" || base == "bun.exe";
    let is_node = base == "node" || base == "node.exe";

    if is_bun {
        for candidate in known_bun_paths() {
            if candidate.is_file() {
                return candidate;
            }
        }
    }

    if is_node {
        for candidate in known_node_paths() {
            if candidate.is_file() {
                return candidate;
            }
        }
    }

    if let Some(found) = executable_on_path(&base) {
        return found;
    }

    // Fall back to the original command and let Command::new report the failure.
    path
}

fn known_bun_paths() -> Vec<PathBuf> {
    let mut paths = Vec::new();
    if let Some(home) = env::var_os("USERPROFILE").or_else(|| env::var_os("HOME")) {
        paths.push(
            PathBuf::from(&home)
                .join(".bun")
                .join("bin")
                .join("bun.exe"),
        );
        paths.push(PathBuf::from(&home).join(".bun").join("bin").join("bun"));
    }
    if let Some(bun_install) = env::var_os("BUN_INSTALL") {
        let root = PathBuf::from(&bun_install);
        paths.push(root.join("bun.exe"));
        paths.push(root.join("bun"));
        paths.push(root.join("bin").join("bun.exe"));
        paths.push(root.join("bin").join("bun"));
    }
    paths
}

fn known_node_paths() -> Vec<PathBuf> {
    let mut paths = Vec::new();
    let exec = env::current_exe().unwrap_or_default();
    let exec_base = exec
        .file_name()
        .map(|value| value.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    if exec_base == "node.exe" || exec_base == "node" {
        paths.push(exec);
    }
    if cfg!(windows) {
        let program_files =
            env::var_os("PROGRAMFILES").unwrap_or_else(|| "C:\\Program Files".into());
        let program_files_x86 =
            env::var_os("PROGRAMFILES(X86)").unwrap_or_else(|| "C:\\Program Files (x86)".into());
        paths.push(
            PathBuf::from(&program_files)
                .join("nodejs")
                .join("node.exe"),
        );
        paths.push(
            PathBuf::from(&program_files_x86)
                .join("nodejs")
                .join("node.exe"),
        );
    }
    paths
}

fn executable_on_path(command: &str) -> Option<PathBuf> {
    let path_var = env::var_os("PATH")
        .or_else(|| env::var_os("Path"))
        .or_else(|| env::var_os("path"))?;
    let path_str = path_var.to_string_lossy();
    let separator = if cfg!(windows) { ';' } else { ':' };
    let names: Vec<String> = if cfg!(windows) {
        let base = command.strip_suffix(".exe").unwrap_or(command);
        vec![command.to_string(), base.to_string(), format!("{base}.exe")]
    } else {
        vec![command.to_string()]
    };
    let extensions: Vec<&str> = if cfg!(windows) {
        vec![".exe", ".cmd", ".bat", ""]
    } else {
        vec![""]
    };

    for dir in path_str.split(separator) {
        if dir.is_empty() {
            continue;
        }
        let dir_path = PathBuf::from(dir);
        for name in &names {
            for ext in &extensions {
                let candidate = dir_path.join(format!("{name}{ext}"));
                if candidate.is_file() {
                    return Some(candidate);
                }
            }
        }
    }
    None
}

fn positive(value: &str, name: &str) -> Result<u64, String> {
    value
        .parse::<u64>()
        .ok()
        .filter(|value| *value > 0)
        .ok_or_else(|| format!("mcp_runtime_proxy_invalid_{name}:{value}"))
}
fn append_tail(target: &mut Vec<u8>, bytes: &[u8]) {
    target.extend_from_slice(bytes);
    if target.len() > TAIL_LIMIT {
        target.drain(..target.len() - TAIL_LIMIT);
    }
}
fn safe_segment(value: &str) -> String {
    value
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || "._-".contains(character) {
                character
            } else {
                '_'
            }
        })
        .take(80)
        .collect()
}
fn now_iso() -> String {
    OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .unwrap_or_else(|_| "1970-01-01T00:00:00Z".to_string())
}
fn timestamp_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}
fn json_io(error: serde_json::Error) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, error)
}
fn io_string(error: io::Error) -> String {
    error.to_string()
}
fn lock_error<T>(error: std::sync::PoisonError<T>) -> String {
    format!("mcp_runtime_proxy_lock_poisoned:{error}")
}
fn default_diagnostics_dir() -> PathBuf {
    env::var_os("NARADA_SITE_ROOT")
        .or_else(|| env::var_os("NARADA_WORKSPACE_ROOT"))
        .map(PathBuf::from)
        .unwrap_or_else(|| env::current_dir().unwrap_or_default())
        .join(".ai")
        .join("runtime")
        .join("mcp-runtime-proxy")
}

#[cfg(windows)]
struct KillJob(windows_sys::Win32::Foundation::HANDLE);
#[cfg(windows)]
impl Drop for KillJob {
    fn drop(&mut self) {
        unsafe {
            windows_sys::Win32::Foundation::CloseHandle(self.0);
        }
    }
}

#[cfg(windows)]
fn assign_kill_job(child: &Arc<Mutex<Child>>) -> Result<KillJob, String> {
    use std::ffi::c_void;
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
        SetInformationJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
        JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    };
    let job = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
    if job.is_null() {
        return Err(format!(
            "mcp_runtime_proxy_job_create_failed:{}",
            io::Error::last_os_error()
        ));
    }
    let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { std::mem::zeroed() };
    limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
    let configured = unsafe {
        SetInformationJobObject(
            job,
            JobObjectExtendedLimitInformation,
            &mut limits as *mut _ as *mut c_void,
            std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
        )
    };
    if configured == 0 {
        unsafe {
            CloseHandle(job);
        }
        return Err(format!(
            "mcp_runtime_proxy_job_configure_failed:{}",
            io::Error::last_os_error()
        ));
    }
    let handle =
        child.lock().map_err(lock_error)?.as_raw_handle() as windows_sys::Win32::Foundation::HANDLE;
    if unsafe { AssignProcessToJobObject(job, handle) } == 0 {
        unsafe {
            CloseHandle(job);
        }
        return Err(format!(
            "mcp_runtime_proxy_job_assign_failed:{}",
            io::Error::last_os_error()
        ));
    }
    Ok(KillJob(job))
}

#[cfg(not(windows))]
struct KillJob;
#[cfg(not(windows))]
fn assign_kill_job(_child: &Arc<Mutex<Child>>) -> Result<KillJob, String> {
    Ok(KillJob)
}

#[cfg(windows)]
fn resume_main_thread(process_id: u32) -> Result<(), String> {
    use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, Thread32First, Thread32Next, TH32CS_SNAPTHREAD, THREADENTRY32,
    };
    use windows_sys::Win32::System::Threading::{OpenThread, ResumeThread, THREAD_SUSPEND_RESUME};
    unsafe {
        let snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0);
        if snapshot == INVALID_HANDLE_VALUE {
            return Err(format!(
                "mcp_runtime_proxy_thread_snapshot_failed:{}",
                io::Error::last_os_error()
            ));
        }
        let mut entry: THREADENTRY32 = std::mem::zeroed();
        entry.dwSize = std::mem::size_of::<THREADENTRY32>() as u32;
        let mut thread_id = 0;
        if Thread32First(snapshot, &mut entry) != 0 {
            loop {
                if entry.th32OwnerProcessID == process_id {
                    thread_id = entry.th32ThreadID;
                    break;
                }
                if Thread32Next(snapshot, &mut entry) == 0 {
                    break;
                }
            }
        }
        CloseHandle(snapshot);
        if thread_id == 0 {
            return Err("mcp_runtime_proxy_suspended_child_thread_missing".to_string());
        }
        let thread = OpenThread(THREAD_SUSPEND_RESUME, 0, thread_id);
        if thread.is_null() {
            return Err(format!(
                "mcp_runtime_proxy_thread_open_failed:{}",
                io::Error::last_os_error()
            ));
        }
        let resumed = ResumeThread(thread);
        CloseHandle(thread);
        if resumed == u32::MAX {
            return Err(format!(
                "mcp_runtime_proxy_thread_resume_failed:{}",
                io::Error::last_os_error()
            ));
        }
    }
    Ok(())
}

#[cfg(not(windows))]
fn resume_main_thread(_process_id: u32) -> Result<(), String> {
    Ok(())
}

#[cfg(windows)]
fn parent_pid() -> u32 {
    use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W,
        TH32CS_SNAPPROCESS,
    };
    let current = std::process::id();
    unsafe {
        let snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
        if snapshot == INVALID_HANDLE_VALUE {
            return 0;
        }
        let mut entry: PROCESSENTRY32W = std::mem::zeroed();
        entry.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;
        let mut found = 0;
        if Process32FirstW(snapshot, &mut entry) != 0 {
            loop {
                if entry.th32ProcessID == current {
                    found = entry.th32ParentProcessID;
                    break;
                }
                if Process32NextW(snapshot, &mut entry) == 0 {
                    break;
                }
            }
        }
        CloseHandle(snapshot);
        found
    }
}

#[cfg(not(windows))]
fn parent_pid() -> u32 {
    0
}
