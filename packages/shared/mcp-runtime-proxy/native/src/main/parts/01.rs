use narada_mcp_materialization_contract::{
    canonical_json_sha256, describe_config, generation_fingerprint, AMBIGUOUS_GENERATION_SCHEMA,
    CONTRACT_VERSION, GENERATION_SCHEMA, LEGACY_GENERATION_SCHEMA,
};
use serde_json::{json, Value};
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
#[path = "../../filesystem.rs"]
mod filesystem;
#[allow(dead_code)]
#[path = "../../git.rs"]
mod git;
#[path = "../../protocol.rs"]
mod protocol;
#[allow(dead_code)]
#[path = "../../structured_command.rs"]
mod structured_command;

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
    include_str!("../../../../src/orientation-entry-enforcement-contract.json");
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
