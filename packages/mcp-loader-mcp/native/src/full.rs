use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::env;
use std::fs::{
    self, create_dir_all, metadata, read_dir, read_to_string, remove_dir_all, OpenOptions,
};
use std::io::{self, BufReader, Read, Write};
use std::path::{Component, Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;

#[cfg(windows)]
use std::os::windows::process::CommandExt;

type JsonObject = Map<String, Value>;

macro_rules! json_object {
    ($value:tt) => {
        json!($value).as_object().cloned().unwrap_or_default()
    };
}

mod child;
mod config;
mod connections;
mod fabric;
mod protocol;
mod results;
mod runtime;
mod tools;
mod wire;

pub(crate) use config::*;
pub(crate) use connections::*;
pub(crate) use fabric::*;
pub(crate) use protocol::*;
pub(crate) use results::*;
pub(crate) use runtime::*;
pub(crate) use tools::*;
pub(crate) use wire::*;

const PROTOCOL_VERSION: &str = "2024-11-05";
const MODERN_PROTOCOL_VERSION: &str = "2026-07-28";
const SERVER_NAME: &str = "mcp-loader-mcp";
const SERVER_VERSION: &str = "0.1.0";
const DEFAULT_MAX_CONNECTIONS: usize = 8;
const DEFAULT_MAX_REQUEST_BYTES: usize = 1024 * 1024;
const MAX_WIRE_HEADER_BYTES: usize = 64 * 1024;
const DEFAULT_MAX_RESPONSE_BYTES: usize = 4 * 1024 * 1024;
const DEFAULT_ATTACH_TIMEOUT_MS: u64 = 30_000;
const DEFAULT_RUNTIME_LEASE_MS: u64 = 30_000;
const DEFAULT_TOOL_CALL_TIMEOUT_MS: u64 = 120_000;
const DEFAULT_TOOL_TIMEOUT_GRACE_MS: u64 = 1_000;
const MAX_TOOL_TIMEOUT_MS: u64 = 900_000;
const MAX_TOOL_TIMEOUT_GRACE_MS: u64 = 60_000;
const DEFAULT_LOADER_RESULT_INLINE_LIMIT: usize = 4_000;
const DEFAULT_OUTPUT_SHOW_CHAR_LIMIT: usize = 4_000;
const MAX_OUTPUT_SHOW_CHAR_LIMIT: usize = 4_000;
const MAX_OUTPUT_PAGE_BYTES: usize = 8 * 1024;
const MAX_INLINE_RESPONSE_BYTES: usize = 32 * 1024;
const STDERR_TAIL_LIMIT: usize = 8_000;
const RUNTIME_PROXY_STATUS_TOOL_NAME: &str = "mcp_runtime_proxy_status";
const SITE_TOOL_OBSERVATION_MAX_ENTRIES: usize = 32;
const SITE_TOOL_OBSERVATION_MAX_AGE_MS: u128 = 7 * 24 * 60 * 60 * 1000;
const SITE_TOOL_OBSERVATION_PAYLOAD_PREFIX: &str = "site-tools-";
const SURFACE_HANDLE_PREFIX: &str = "msh_";

static ID_COUNTER: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Debug)]
pub(crate) struct Diagnostic {
    code: String,
    message: String,
    details: Value,
}

impl Diagnostic {
    fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
            details: json!({}),
        }
    }

    fn with_details(mut self, details: Value) -> Self {
        self.details = details;
        self
    }

    fn value(&self) -> Value {
        json!({
            "schema": "narada.mcp_loader.error.v1",
            "code": self.code,
            "message": self.message,
            "details": self.details,
        })
    }
}

#[derive(Clone, Debug)]
pub(crate) struct ChildSpec {
    command: String,
    args: Vec<String>,
}

type PendingRequests = Arc<Mutex<HashMap<u64, mpsc::Sender<Result<Value, Diagnostic>>>>>;
pub(crate) type RuntimeMetadata = (
    String,
    String,
    Value,
    Value,
    Option<Value>,
    Option<String>,
    Option<String>,
    Vec<String>,
);

pub(crate) struct ChildSession {
    spec: ChildSpec,
    child: Arc<Mutex<Child>>,
    stdin: Arc<Mutex<ChildStdin>>,
    pending: PendingRequests,
    next_id: AtomicU64,
    closed: Arc<AtomicBool>,
    killed: Arc<AtomicBool>,
    stderr_tail: Arc<Mutex<String>>,
    pid: u32,
}

pub(crate) struct Connection {
    session: ChildSession,
    connection_id: String,
    owner_run_id: String,
    owner_pid: u32,
    parent_pid: u32,
    ownership_marker: String,
    logical_connection_id: String,
    generation_id: String,
    server_name: String,
    projection_id: String,
    execution: Value,
    lifecycle: Value,
    descriptor: Option<Value>,
    descriptor_digest: Option<String>,
    declared_tool_contract_digest: Option<String>,
    tool_contract_digest: Option<String>,
    heartbeat_ms: u128,
    lease_expires_ms: u128,
    site_root: String,
    surface_id: String,
    binding_id: Option<String>,
    admission_envelope_id: Option<String>,
    admitted_binding_digest: Option<String>,
    authority_epoch: Option<u64>,
    runtime_kind: Option<String>,
    runtime_requirements: Vec<String>,
    runtime_command: String,
    entrypoint: String,
    args: Vec<String>,
    child_invocation_kind: String,
    requested_entrypoint: Option<String>,
    extra_args: Vec<String>,
    initialized: bool,
    server_info: Value,
    tools: Vec<Value>,
    detached: bool,
    attached_ms: u128,
    detached_ms: Option<u128>,
}

pub(crate) struct SurfaceHandle {
    handle: String,
    logical_connection_id: String,
    binding_id: Option<String>,
    site_root: String,
    surface_id: String,
    runtime_kind: Option<String>,
    created_at: String,
}

#[derive(Clone, Debug)]
pub(crate) struct Options {
    allowed_site_roots: Option<Vec<String>>,
    allowed_entrypoint_prefixes: Option<Vec<String>>,
    allowed_surface_ids: Option<Vec<String>>,
    allowed_env_vars: Option<Vec<String>>,
    max_connections: usize,
    max_request_bytes: usize,
    max_response_bytes: usize,
    attach_timeout_ms: u64,
    tool_call_timeout_ms: u64,
    tool_call_grace_ms: u64,
    child_command: Option<String>,
    child_entrypoint: Option<String>,
    child_args: Vec<String>,
    binding_admission_path: Option<String>,
    binding_admission_digest: Option<String>,
    standalone_ambient_attachment: bool,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            allowed_site_roots: None,
            allowed_entrypoint_prefixes: None,
            allowed_surface_ids: None,
            allowed_env_vars: None,
            max_connections: DEFAULT_MAX_CONNECTIONS,
            max_request_bytes: DEFAULT_MAX_REQUEST_BYTES,
            max_response_bytes: DEFAULT_MAX_RESPONSE_BYTES,
            attach_timeout_ms: DEFAULT_ATTACH_TIMEOUT_MS,
            tool_call_timeout_ms: DEFAULT_TOOL_CALL_TIMEOUT_MS,
            tool_call_grace_ms: DEFAULT_TOOL_TIMEOUT_GRACE_MS,
            child_command: None,
            child_entrypoint: None,
            child_args: Vec::new(),
            binding_admission_path: None,
            binding_admission_digest: None,
            standalone_ambient_attachment: false,
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct Policy {
    allowed_site_roots: Vec<String>,
    allowed_entrypoint_prefixes: Vec<String>,
    allowed_surface_ids: Option<Vec<String>>,
    allowed_env_vars: Vec<String>,
    max_connections: usize,
    max_request_bytes: usize,
    max_response_bytes: usize,
    attach_timeout_ms: u64,
    tool_call_timeout_ms: u64,
    tool_call_grace_ms: u64,
}

pub(crate) struct LoaderState {
    policy: Policy,
    surface_root: String,
    workspace_root: String,
    started_ms: u128,
    run_id: String,
    owner_pid: u32,
    ownership_marker: String,
    schema_lease_secret: String,
    connections: HashMap<String, Connection>,
    handles: HashMap<String, SurfaceHandle>,
    binding_admission: Option<Value>,
    standalone_ambient_attachment: bool,
}

pub(crate) struct WireReader<R> {
    reader: R,
    buffer: Vec<u8>,
    eof: bool,
    max_message_bytes: usize,
}
fn child_error_diagnostic(error: &Value) -> Diagnostic {
    let data = error.get("data").unwrap_or(&Value::Null);
    let code = data
        .get("code")
        .and_then(Value::as_str)
        .unwrap_or("child_error");
    let message = error
        .get("message")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .unwrap_or(code);
    let mut details = json!({
        "child_jsonrpc_code": error.get("code").cloned().unwrap_or(Value::Null),
        "child_code": code,
    });
    if let Some(domain_details) = data.get("details") {
        details["child_details"] = domain_details.clone();
    }
    Diagnostic::new(code, message).with_details(details)
}

fn value_u64(value: &Value) -> Option<u64> {
    value
        .as_u64()
        .or_else(|| value.as_str().and_then(|text| text.parse::<u64>().ok()))
}

pub fn main_entry() {
    let options = match parse_options(env::args().skip(1).collect()) {
        Ok(options) => options,
        Err(error) => {
            eprintln!("{}", error.message);
            std::process::exit(2);
        }
    };
    if let Err(error) = run_server(options) {
        eprintln!("{}", error.message);
        std::process::exit(1);
    }
}

fn call_tool(name: &str, arguments: Value, state: &mut LoaderState) -> Result<Value, Diagnostic> {
    let object = arguments.as_object().cloned().unwrap_or_default();
    match name {
        "mcp_loader_guidance" => Ok(guidance_result(&object, state)),
        "mcp_loader_runtime_status" => Ok(runtime_freshness(state)),
        "mcp_loader_policy_inspect" => Ok(policy_inspect(state)),
        "mcp_loader_connection_inventory" => Ok(connection_inventory(&object, state)),
        "mcp_loader_process_ownership" => Ok(process_ownership(state)),
        "mcp_loader_owned_port_lookup" => owned_port_lookup(&object, state),
        "mcp_loader_runtime_observation" => runtime_observation(&object, state),
        "mcp_loader_list_site_surfaces" => list_site_surfaces(&object, state),
        "mcp_loader_site_fabric_diagnostics" => site_fabric_diagnostics(&object, state),
        "mcp_loader_site_tool_inventory_check" => site_tool_inventory(&object, state),
        "mcp_loader_attach_surface" => attach_surface(&object, state),
        "mcp_loader_open_surface" => open_surface(&object, state),
        "mcp_loader_resume_or_open_surface" => resume_or_open_surface(&object, state),
        "mcp_loader_surface_handle_inventory" => Ok(surface_handle_inventory(state)),
        "mcp_loader_list_tools" => list_attached_tools(&object, state),
        "mcp_loader_inspect_tool" => inspect_attached_tool(&object, state),
        "mcp_loader_inspect_binding_tool" => inspect_binding_tool(&object, state),
        "mcp_loader_inspect_binding_tools" => inspect_binding_tools(&object, state),
        "mcp_loader_surface_status" => surface_status(&object, state),
        "mcp_loader_tool_discovery_manifest" => tool_discovery_manifest(&object, state),
        "mcp_loader_call_tool" => call_attached_tool(&object, state),
        "mcp_loader_call_surface_tool" => call_surface_handle_tool(&object, state),
        "mcp_loader_call_binding_tool" => call_binding_tool(&object, state),
        "mcp_loader_read_result" => read_loader_result(&object, state),
        "mcp_loader_detach" => detach_connection(&object, state),
        "mcp_loader_surface_restart" => restart_connection(&object, state),
        _ => Err(Diagnostic::new(
            "unknown_tool",
            format!("unknown_tool:{}", name),
        )),
    }
}
#[cfg(test)]
mod modern_protocol_tests;
#[cfg(test)]
mod tests;
