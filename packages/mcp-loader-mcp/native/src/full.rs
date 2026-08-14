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

const PROTOCOL_VERSION: &str = "2024-11-05";
const MODERN_PROTOCOL_VERSION: &str = "2026-07-28";
const SERVER_NAME: &str = "mcp-loader-mcp";
const SERVER_VERSION: &str = "0.1.0";
const DEFAULT_MAX_CONNECTIONS: usize = 8;
const DEFAULT_MAX_REQUEST_BYTES: usize = 1024 * 1024;
const DEFAULT_MAX_RESPONSE_BYTES: usize = 4 * 1024 * 1024;
const DEFAULT_ATTACH_TIMEOUT_MS: u64 = 30_000;
const DEFAULT_RUNTIME_LEASE_MS: u64 = 30_000;
const DEFAULT_TOOL_CALL_TIMEOUT_MS: u64 = 120_000;
const DEFAULT_TOOL_TIMEOUT_GRACE_MS: u64 = 1_000;
const MAX_TOOL_TIMEOUT_MS: u64 = 900_000;
const MAX_TOOL_TIMEOUT_GRACE_MS: u64 = 60_000;
const DEFAULT_LOADER_RESULT_INLINE_LIMIT: usize = 12_000;
const DEFAULT_OUTPUT_SHOW_CHAR_LIMIT: usize = 10_000;
const MAX_OUTPUT_SHOW_CHAR_LIMIT: usize = 20_000;
const MAX_OUTPUT_PAGE_BYTES: usize = 12 * 1024;
const MAX_INLINE_RESPONSE_BYTES: usize = 32 * 1024;
const STDERR_TAIL_LIMIT: usize = 8_000;
const RUNTIME_PROXY_STATUS_TOOL_NAME: &str = "mcp_runtime_proxy_status";
const SITE_TOOL_OBSERVATION_MAX_ENTRIES: usize = 32;
const SITE_TOOL_OBSERVATION_MAX_AGE_MS: u128 = 7 * 24 * 60 * 60 * 1000;
const SITE_TOOL_OBSERVATION_PAYLOAD_PREFIX: &str = "site-tools-";
const SURFACE_HANDLE_PREFIX: &str = "msh_";
const FILE_MTIME_CLOCK_SKEW_MS: u128 = 1_000;

static ID_COUNTER: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Debug)]
struct Diagnostic {
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
struct ChildSpec {
    command: String,
    args: Vec<String>,
}

struct ChildSession {
    spec: ChildSpec,
    child: Arc<Mutex<Child>>,
    stdin: Arc<Mutex<ChildStdin>>,
    pending: Arc<Mutex<HashMap<u64, mpsc::Sender<Result<Value, Diagnostic>>>>>,
    next_id: AtomicU64,
    closed: Arc<AtomicBool>,
    killed: Arc<AtomicBool>,
    stderr_tail: Arc<Mutex<String>>,
    pid: u32,
}

struct Connection {
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

struct SurfaceHandle {
    handle: String,
    logical_connection_id: String,
    site_root: String,
    surface_id: String,
    runtime_kind: Option<String>,
    created_at: String,
}

#[derive(Clone, Debug)]
struct Options {
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
struct Policy {
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

struct LoaderState {
    options: Options,
    policy: Policy,
    surface_root: String,
    workspace_root: String,
    started_ms: u128,
    run_id: String,
    owner_pid: u32,
    ownership_marker: String,
    connections: HashMap<String, Connection>,
    handles: HashMap<String, SurfaceHandle>,
    binding_admission: Option<Value>,
    standalone_ambient_attachment: bool,
}

struct WireReader<R> {
    reader: R,
    buffer: Vec<u8>,
    eof: bool,
}

impl<R: Read> WireReader<R> {
    fn new(reader: R) -> Self {
        Self {
            reader,
            buffer: Vec::new(),
            eof: false,
        }
    }

    fn next(&mut self) -> io::Result<Option<(Value, bool)>> {
        loop {
            if let Some(message) = try_parse_wire(&mut self.buffer)? {
                return Ok(Some(message));
            }
            if self.eof {
                if self.buffer.iter().all(|byte| byte.is_ascii_whitespace()) {
                    self.buffer.clear();
                    return Ok(None);
                }
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "incomplete MCP message",
                ));
            }
            let mut chunk = [0_u8; 8192];
            let count = self.reader.read(&mut chunk)?;
            if count == 0 {
                self.eof = true;
            } else {
                self.buffer.extend_from_slice(&chunk[..count]);
            }
        }
    }
}

fn try_parse_wire(buffer: &mut Vec<u8>) -> io::Result<Option<(Value, bool)>> {
    while matches!(buffer.first(), Some(b'\r' | b'\n' | b' ' | b'\t')) {
        buffer.remove(0);
    }
    if buffer.is_empty() {
        return Ok(None);
    }
    if buffer.len() >= 15 && buffer[..15].eq_ignore_ascii_case(b"content-length:") {
        let (header_end, separator_len) = match find_header_end(buffer) {
            Some(found) => found,
            None => return Ok(None),
        };
        let header = String::from_utf8_lossy(&buffer[..header_end]);
        let length = header
            .lines()
            .find_map(|line| {
                let (name, value) = line.split_once(':')?;
                if name.trim().eq_ignore_ascii_case("content-length") {
                    value.trim().parse::<usize>().ok()
                } else {
                    None
                }
            })
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "missing Content-Length"))?;
        let body_start = header_end + separator_len;
        let body_end = body_start.saturating_add(length);
        if buffer.len() < body_end {
            return Ok(None);
        }
        let value = serde_json::from_slice::<Value>(&buffer[body_start..body_end])
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error.to_string()))?;
        buffer.drain(..body_end);
        return Ok(Some((value, true)));
    }
    let newline = match buffer.iter().position(|byte| *byte == b'\n') {
        Some(position) => position,
        None => return Ok(None),
    };
    let mut line = buffer.drain(..=newline).collect::<Vec<_>>();
    if line.last() == Some(&b'\n') {
        line.pop();
    }
    if line.last() == Some(&b'\r') {
        line.pop();
    }
    if line.iter().all(|byte| byte.is_ascii_whitespace()) {
        return Ok(None);
    }
    let value = serde_json::from_slice::<Value>(&line)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error.to_string()))?;
    Ok(Some((value, false)))
}

fn find_header_end(buffer: &[u8]) -> Option<(usize, usize)> {
    if let Some(position) = buffer.windows(4).position(|window| window == b"\r\n\r\n") {
        return Some((position, 4));
    }
    buffer
        .windows(2)
        .position(|window| window == b"\n\n")
        .map(|position| (position, 2))
}

fn write_wire<W: Write>(writer: &mut W, value: &Value, framed: bool) -> io::Result<()> {
    let body = serde_json::to_vec(value)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error.to_string()))?;
    if framed {
        write!(writer, "Content-Length: {}\r\n\r\n", body.len())?;
        writer.write_all(&body)?;
    } else {
        writer.write_all(&body)?;
        writer.write_all(b"\n")?;
    }
    writer.flush()
}

impl ChildSession {
    fn spawn(spec: ChildSpec, env_map: &HashMap<String, String>) -> Result<Self, Diagnostic> {
        let mut command = Command::new(&spec.command);
        command
            .args(&spec.args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        command.env_clear();
        for (key, value) in env_map {
            command.env(key, value);
        }
        #[cfg(windows)]
        command.creation_flags(0x0800_0000);
        let mut child = command.spawn().map_err(|error| {
            Diagnostic::new(
                "child_spawn_failed",
                format!("child_spawn_failed:{}", error),
            )
            .with_details(json!({"command": spec.command, "args": spec.args}))
        })?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| Diagnostic::new("child_stdin_unavailable", "child_stdin_unavailable"))?;
        let stdout = child.stdout.take().ok_or_else(|| {
            Diagnostic::new("child_stdout_unavailable", "child_stdout_unavailable")
        })?;
        let stderr = child.stderr.take().ok_or_else(|| {
            Diagnostic::new("child_stderr_unavailable", "child_stderr_unavailable")
        })?;
        let pid = child.id();
        let child = Arc::new(Mutex::new(child));
        let stdin = Arc::new(Mutex::new(stdin));
        let pending: Arc<Mutex<HashMap<u64, mpsc::Sender<Result<Value, Diagnostic>>>>> =
            Arc::new(Mutex::new(HashMap::new()));
        let closed = Arc::new(AtomicBool::new(false));
        let killed = Arc::new(AtomicBool::new(false));
        let reader_pending = Arc::clone(&pending);
        let reader_closed = Arc::clone(&closed);
        thread::spawn(move || {
            let mut reader = WireReader::new(stdout);
            loop {
                match reader.next() {
                    Ok(Some((message, _))) => {
                        let Some(object) = message.as_object() else {
                            continue;
                        };
                        let Some(id) = object.get("id").and_then(value_u64) else {
                            continue;
                        };
                        let sender = reader_pending
                            .lock()
                            .ok()
                            .and_then(|mut pending| pending.remove(&id));
                        let Some(sender) = sender else {
                            continue;
                        };
                        let result = if let Some(error) = object.get("error") {
                            Err(child_error_diagnostic(error))
                        } else {
                            Ok(object.get("result").cloned().unwrap_or(Value::Null))
                        };
                        let _ = sender.send(result);
                    }
                    Ok(None) | Err(_) => break,
                }
            }
            reader_closed.store(true, Ordering::SeqCst);
            let pending = reader_pending
                .lock()
                .ok()
                .map(|mut pending| {
                    pending
                        .drain()
                        .map(|(_, sender)| sender)
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            for sender in pending {
                let _ = sender.send(Err(Diagnostic::new("child_exited", "child_exited")));
            }
        });
        let tail = Arc::new(Mutex::new(String::new()));
        let reader_tail = Arc::clone(&tail);
        thread::spawn(move || {
            let mut reader = BufReader::new(stderr);
            let mut chunk = [0_u8; 2048];
            loop {
                match reader.read(&mut chunk) {
                    Ok(0) | Err(_) => break,
                    Ok(count) => {
                        if let Ok(mut value) = reader_tail.lock() {
                            value.push_str(&String::from_utf8_lossy(&chunk[..count]));
                            if value.len() > STDERR_TAIL_LIMIT {
                                let start = value.len() - STDERR_TAIL_LIMIT;
                                *value = value[start..].to_string();
                            }
                        }
                    }
                }
            }
        });
        Ok(Self {
            spec,
            child,
            stdin,
            pending,
            next_id: AtomicU64::new(1),
            closed,
            killed,
            stderr_tail: tail,
            pid,
        })
    }

    fn request(&self, method: &str, params: Value, timeout_ms: u64) -> Result<Value, Diagnostic> {
        if self.closed.load(Ordering::SeqCst) {
            return Err(Diagnostic::new(
                "connection_detached",
                "connection_detached",
            ));
        }
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let (sender, receiver) = mpsc::channel();
        self.pending
            .lock()
            .map_err(|_| Diagnostic::new("pending_lock_failed", "pending_lock_failed"))?
            .insert(id, sender);
        let request = json!({"jsonrpc":"2.0","id":id,"method":method,"params":params});
        let write_result = self
            .stdin
            .lock()
            .map_err(|_| Diagnostic::new("child_stdin_lock_failed", "child_stdin_lock_failed"))
            .and_then(|mut stdin| {
                write_wire(&mut *stdin, &request, false).map_err(|error| {
                    Diagnostic::new(
                        "child_write_failed",
                        format!("child_write_failed:{}", error),
                    )
                })
            });
        if let Err(error) = write_result {
            let _ = self.pending.lock().map(|mut pending| pending.remove(&id));
            return Err(error);
        }
        match receiver.recv_timeout(Duration::from_millis(timeout_ms)) {
            Ok(result) => result,
            Err(mpsc::RecvTimeoutError::Timeout) => {
                let _ = self.pending.lock().map(|mut pending| pending.remove(&id));
                Err(Diagnostic::new(
                    "child_timeout",
                    format!("child_timeout:{}:{}ms", method, timeout_ms),
                ))
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                Err(Diagnostic::new("child_exited", "child_exited"))
            }
        }
    }

    fn notify(&self, method: &str, params: Value) -> Result<(), Diagnostic> {
        if self.closed.load(Ordering::SeqCst) {
            return Ok(());
        }
        let request = json!({"jsonrpc":"2.0","method":method,"params":params});
        self.stdin
            .lock()
            .map_err(|_| Diagnostic::new("child_stdin_lock_failed", "child_stdin_lock_failed"))
            .and_then(|mut stdin| {
                write_wire(&mut *stdin, &request, false).map_err(|error| {
                    Diagnostic::new(
                        "child_write_failed",
                        format!("child_write_failed:{}", error),
                    )
                })
            })
    }

    fn alive(&self) -> bool {
        if self.closed.load(Ordering::SeqCst) {
            return false;
        }
        let alive = self
            .child
            .lock()
            .ok()
            .and_then(|mut child| child.try_wait().ok())
            .is_some_and(|status| status.is_none());
        if !alive {
            self.closed.store(true, Ordering::SeqCst);
        }
        alive
    }

    fn terminate(&self) -> Value {
        self.closed.store(true, Ordering::SeqCst);
        let mut child = match self.child.lock() {
            Ok(child) => child,
            Err(_) => return json!({"status":"termination_lock_failed"}),
        };
        if let Ok(Some(status)) = child.try_wait() {
            #[cfg(unix)]
            {
                use std::os::unix::process::ExitStatusExt;
                return json!({"status":"already_exited","exit_code":status.code(),"signal":status.signal(),"forced":false});
            }
            #[cfg(not(unix))]
            return json!({"status":"already_exited","exit_code":status.code(),"signal":Value::Null,"forced":false});
        }
        let killed = child.kill().is_ok();
        self.killed.store(killed, Ordering::SeqCst);
        let waited = child.wait().ok();
        #[cfg(unix)]
        {
            use std::os::unix::process::ExitStatusExt;
            return json!({"status":if killed {"terminated"} else {"termination_failed"},"exit_code":waited.as_ref().and_then(|status| status.code()),"signal":waited.as_ref().and_then(|status| status.signal()),"forced":killed});
        }
        #[cfg(not(unix))]
        json!({"status":if killed {"terminated"} else {"termination_failed"},"exit_code":waited.as_ref().and_then(|status| status.code()),"signal":Value::Null,"forced":killed})
    }

    fn stderr_tail(&self) -> String {
        self.stderr_tail
            .lock()
            .map(|value| value.clone())
            .unwrap_or_default()
    }
    fn exit_code(&self) -> Option<i32> {
        self.child
            .lock()
            .ok()
            .and_then(|mut child| child.try_wait().ok().flatten())
            .and_then(|status| status.code())
    }
    #[cfg(unix)]
    fn signal_code(&self) -> Option<i32> {
        use std::os::unix::process::ExitStatusExt;
        self.child
            .lock()
            .ok()
            .and_then(|mut child| child.try_wait().ok().flatten())
            .and_then(|status| status.signal())
    }
    #[cfg(not(unix))]
    fn signal_code(&self) -> Option<i32> {
        None
    }
    fn killed(&self) -> bool {
        self.killed.load(Ordering::SeqCst)
    }
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

fn parse_options(args: Vec<String>) -> Result<Options, Diagnostic> {
    let mut options = Options::default();
    let mut allowed_roots = Vec::new();
    let mut allowed_prefixes = Vec::new();
    let mut allowed_surfaces = Vec::new();
    let mut allowed_env = Vec::new();
    let mut index = 0;
    while index < args.len() {
        let arg = &args[index];
        let mut next = || -> Result<String, Diagnostic> {
            index += 1;
            args.get(index).cloned().ok_or_else(|| {
                Diagnostic::new("argument_required", format!("argument_required:{}", arg))
            })
        };
        match arg.as_str() {
            "--allowed-site-root" => allowed_roots.push(next()?),
            "--allowed-entrypoint-prefix" => allowed_prefixes.push(next()?),
            "--allowed-surface-id" => allowed_surfaces.push(next()?),
            "--allowed-env-var" => allowed_env.push(next()?),
            "--max-connections" => {
                options.max_connections = bounded_usize(&next()?, "--max-connections", 1, 64)?
            }
            "--max-request-bytes" => {
                options.max_request_bytes =
                    bounded_usize(&next()?, "--max-request-bytes", 4096, 16 * 1024 * 1024)?
            }
            "--max-response-bytes" => {
                options.max_response_bytes =
                    bounded_usize(&next()?, "--max-response-bytes", 4096, 64 * 1024 * 1024)?
            }
            "--attach-timeout-ms" => {
                options.attach_timeout_ms =
                    bounded_u64(&next()?, "--attach-timeout-ms", 1000, 300000)?
            }
            "--tool-call-timeout-ms" => {
                options.tool_call_timeout_ms =
                    bounded_u64(&next()?, "--tool-call-timeout-ms", 1000, 900000)?
            }
            "--tool-timeout-grace-ms" => {
                options.tool_call_grace_ms = bounded_u64(
                    &next()?,
                    "--tool-timeout-grace-ms",
                    0,
                    MAX_TOOL_TIMEOUT_GRACE_MS,
                )?
            }
            "--child-command" => options.child_command = Some(next()?),
            "--child-entrypoint" => options.child_entrypoint = Some(next()?),
            "--child-arg" => options.child_args.push(next()?),
            "--binding-admission-path" => options.binding_admission_path = Some(next()?),
            "--binding-admission-digest" => options.binding_admission_digest = Some(next()?),
            "--standalone-ambient-attachment" => options.standalone_ambient_attachment = true,
            "--" => {
                options
                    .child_args
                    .extend(args.iter().skip(index + 1).cloned());
                break;
            }
            _ => {
                return Err(Diagnostic::new(
                    "unknown_argument",
                    format!("unknown_argument:{}", arg),
                ))
            }
        }
        index += 1;
    }
    if !allowed_roots.is_empty() {
        options.allowed_site_roots = Some(allowed_roots);
    }
    if !allowed_prefixes.is_empty() {
        options.allowed_entrypoint_prefixes = Some(allowed_prefixes);
    }
    if !allowed_surfaces.is_empty() {
        options.allowed_surface_ids = Some(allowed_surfaces);
    }
    if !allowed_env.is_empty() {
        options.allowed_env_vars = Some(allowed_env);
    }
    Ok(options)
}

fn bounded_usize(value: &str, flag: &str, min: usize, max: usize) -> Result<usize, Diagnostic> {
    let parsed = value
        .parse::<usize>()
        .map_err(|_| Diagnostic::new("invalid_argument", format!("invalid_argument:{}", flag)))?;
    Ok(parsed.clamp(min, max))
}

fn bounded_u64(value: &str, flag: &str, min: u64, max: u64) -> Result<u64, Diagnostic> {
    let parsed = value
        .parse::<u64>()
        .map_err(|_| Diagnostic::new("invalid_argument", format!("invalid_argument:{}", flag)))?;
    Ok(parsed.clamp(min, max))
}

fn load_binding_admission(options: &Options) -> Result<Option<Value>, Diagnostic> {
    let required = env::var("NARADA_MCP_BINDING_ADMISSION_REQUIRED")
        .ok()
        .as_deref()
        == Some("1");
    let path = options
        .binding_admission_path
        .clone()
        .or_else(|| env::var("NARADA_MCP_BINDING_ADMISSION_PATH").ok())
        .filter(|value| !value.trim().is_empty());
    let Some(path) = path else {
        if required {
            return Err(Diagnostic::new(
                "mcp_binding_admission_required",
                "mcp_binding_admission_required",
            ));
        }
        return Ok(None);
    };
    let text = read_to_string(&path).map_err(|error| {
        Diagnostic::new(
            "mcp_binding_admission_unreadable",
            format!("mcp_binding_admission_unreadable:{error}"),
        )
    })?;
    let envelope: Value = serde_json::from_str(&text).map_err(|error| {
        Diagnostic::new(
            "mcp_binding_admission_invalid",
            format!("mcp_binding_admission_invalid:{error}"),
        )
    })?;
    if envelope.get("schema").and_then(Value::as_str)
        != Some("narada.mcp.binding_admission_envelope.v1")
    {
        return Err(Diagnostic::new(
            "mcp_binding_admission_schema_invalid",
            "mcp_binding_admission_schema_invalid",
        ));
    }
    let digest = envelope
        .get("envelope_digest")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let mut unsigned = envelope.clone();
    if let Some(object) = unsigned.as_object_mut() {
        object.remove("envelope_digest");
    }
    if sha256(&stable_json(&unsigned)) != digest {
        return Err(Diagnostic::new(
            "mcp_binding_admission_envelope_digest_mismatch",
            "mcp_binding_admission_envelope_digest_mismatch",
        ));
    }
    let expected_digest = options
        .binding_admission_digest
        .clone()
        .or_else(|| env::var("NARADA_MCP_BINDING_ADMISSION_DIGEST").ok());
    if expected_digest
        .as_deref()
        .is_some_and(|expected| expected != digest)
    {
        return Err(Diagnostic::new(
            "mcp_binding_admission_digest_mismatch",
            "mcp_binding_admission_digest_mismatch",
        ));
    }
    let expected_session = env::var("NARADA_NARS_SESSION_ID")
        .or_else(|_| env::var("NARADA_RUNTIME_SESSION_ID"))
        .or_else(|_| env::var("NARADA_CARRIER_SESSION_ID"))
        .ok();
    if expected_session.as_deref().is_some_and(|expected| {
        envelope.get("carrier_session_id").and_then(Value::as_str) != Some(expected)
    }) {
        return Err(Diagnostic::new(
            "mcp_binding_admission_session_mismatch",
            "mcp_binding_admission_session_mismatch",
        ));
    }
    if let Ok(expected) = env::var("NARADA_SESSION_AUTHORITY_PRINCIPAL_KEY") {
        if envelope.get("principal_key").and_then(Value::as_str) != Some(expected.as_str()) {
            return Err(Diagnostic::new(
                "mcp_binding_admission_principal_mismatch",
                "mcp_binding_admission_principal_mismatch",
            ));
        }
    }
    if let Some(expected) = env::var("NARADA_SESSION_AUTHORITY_EPOCH")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
    {
        if envelope.get("authority_epoch").and_then(Value::as_u64) != Some(expected) {
            return Err(Diagnostic::new(
                "mcp_binding_admission_epoch_mismatch",
                "mcp_binding_admission_epoch_mismatch",
            ));
        }
    }
    let now = OffsetDateTime::now_utc();
    if envelope
        .get("issued_at")
        .and_then(Value::as_str)
        .and_then(|value| OffsetDateTime::parse(value, &Rfc3339).ok())
        .is_some_and(|issued| issued > now)
    {
        return Err(Diagnostic::new(
            "mcp_binding_admission_not_yet_issued",
            "mcp_binding_admission_not_yet_issued",
        ));
    }
    if envelope
        .get("valid_until")
        .and_then(Value::as_str)
        .and_then(|value| OffsetDateTime::parse(value, &Rfc3339).ok())
        .is_some_and(|expiry| expiry <= now)
    {
        return Err(Diagnostic::new(
            "mcp_binding_admission_expired",
            "mcp_binding_admission_expired",
        ));
    }
    Ok(Some(envelope))
}

fn run_server(options: Options) -> Result<(), Diagnostic> {
    let binding_admission = load_binding_admission(&options)?;
    let executable = env::current_exe()
        .map_err(|error| Diagnostic::new("runtime_path_unavailable", error.to_string()))?;
    let native_dir = executable
        .parent()
        .and_then(Path::parent)
        .and_then(Path::parent)
        .ok_or_else(|| Diagnostic::new("runtime_path_unavailable", "runtime_path_unavailable"))?;
    let package_root = native_dir
        .parent()
        .and_then(Path::parent)
        .unwrap_or(native_dir);
    let surface_root = normalize_path(
        &package_root
            .parent()
            .unwrap_or(package_root)
            .to_string_lossy(),
    );
    let workspace_root = normalize_path(
        &Path::new(&surface_root)
            .parent()
            .unwrap_or(Path::new(&surface_root))
            .to_string_lossy(),
    );
    let started_ms = now_ms();
    let run_id = new_id("loader");
    let owner_pid = std::process::id();
    let ownership_marker = format!("narada.mcp.loader/{}", run_id);
    let policy = build_policy(&options, &surface_root, &workspace_root);
    let standalone_ambient_attachment = options.standalone_ambient_attachment;
    let mut state = LoaderState {
        options,
        policy,
        surface_root,
        workspace_root,
        started_ms,
        run_id,
        owner_pid,
        ownership_marker,
        connections: HashMap::new(),
        handles: HashMap::new(),
        binding_admission,
        standalone_ambient_attachment,
    };
    let stdin = io::stdin();
    let mut reader = WireReader::new(stdin.lock());
    let stdout = io::stdout();
    let mut writer = stdout.lock();
    while let Some((request, framed)) = reader.next().map_err(|error| {
        Diagnostic::new(
            "parent_read_failed",
            format!("parent_read_failed:{}", error),
        )
    })? {
        let method = request
            .get("method")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if request.get("id").is_none() && method.starts_with("notifications/") {
            continue;
        }
        let id = request.get("id").cloned().unwrap_or(Value::Null);
        let response = match dispatch(&request, &mut state) {
            Ok(result) => json!({"jsonrpc":"2.0","id":id,"result":result}),
            Err(error) => {
                json!({"jsonrpc":"2.0","id":id,"error":{"code":-32000,"message":error.message,"data":error.value()}})
            }
        };
        write_wire(&mut writer, &response, framed).map_err(|error| {
            Diagnostic::new(
                "parent_write_failed",
                format!("parent_write_failed:{}", error),
            )
        })?;
    }
    let connections = std::mem::take(&mut state.connections);
    for (_, connection) in connections {
        connection.session.terminate();
    }
    Ok(())
}

fn build_policy(options: &Options, surface_root: &str, workspace_root: &str) -> Policy {
    let user_profile = env::var("USERPROFILE").or_else(|_| env::var("HOME")).ok();
    let mut site_roots = options
        .allowed_site_roots
        .clone()
        .unwrap_or_else(|| vec![workspace_root.to_string()]);
    if let Ok(configured) = env::var("NARADA_MCP_ALLOWED_SITE_ROOTS") {
        site_roots.extend(configured.split(',').filter_map(|item| optional_str(item)));
    }
    if let Some(profile) = user_profile.as_deref() {
        site_roots.push(normalize_path(&join_path(profile, "Narada")));
    }
    let mut prefixes = options
        .allowed_entrypoint_prefixes
        .clone()
        .unwrap_or_else(|| vec![surface_root.to_string(), "{site_root}/tools/".to_string()]);
    if let Ok(configured) = env::var("NARADA_MCP_ALLOWED_ENTRYPOINT_PREFIXES") {
        prefixes.extend(configured.split(',').filter_map(|item| optional_str(item)));
    }
    if let Some(profile) = user_profile.as_deref() {
        prefixes.push(normalize_path(&join_path(profile, "Narada/tools")));
    }
    let mut allowed_prefixes: Vec<String> = prefixes
        .into_iter()
        .map(|prefix| normalize_policy_prefix(&prefix))
        .collect();
    allowed_prefixes.sort_by_key(|value| std::cmp::Reverse(value.len()));
    allowed_prefixes.dedup();
    let mut allowed_site_roots: Vec<String> = site_roots
        .into_iter()
        .map(|value| normalize_path(&value))
        .collect();
    allowed_site_roots.sort_by_key(|value| {
        if cfg!(windows) {
            value.to_lowercase()
        } else {
            value.clone()
        }
    });
    allowed_site_roots.dedup_by(|left, right| {
        if cfg!(windows) {
            left.to_lowercase() == right.to_lowercase()
        } else {
            left == right
        }
    });
    Policy {
        allowed_site_roots,
        allowed_entrypoint_prefixes: allowed_prefixes,
        allowed_surface_ids: options.allowed_surface_ids.clone(),
        allowed_env_vars: options.allowed_env_vars.clone().unwrap_or_else(|| {
            vec![
                "USERPROFILE",
                "HOME",
                "NODE_OPTIONS",
                "PATH",
                "PROCESSOR_ARCHITECTURE",
                "SystemRoot",
                "NARADA_AGENT_ID",
                "NARADA_OPERATOR_ID",
                "NARADA_NARS_SESSION_SOURCE_KIND",
                "NARADA_CARRIER_SESSION_ID",
                "NARADA_SITE_ID",
                "NARADA_ROOT",
                "NARADA_SRC_ROOT",
            ]
            .into_iter()
            .map(String::from)
            .collect()
        }),
        max_connections: options.max_connections,
        max_request_bytes: options.max_request_bytes,
        max_response_bytes: options.max_response_bytes,
        attach_timeout_ms: options.attach_timeout_ms,
        tool_call_timeout_ms: options.tool_call_timeout_ms,
        tool_call_grace_ms: options.tool_call_grace_ms,
    }
}

fn is_modern_request(params: &Value) -> bool {
    params
        .get("_meta")
        .and_then(Value::as_object)
        .and_then(|meta| meta.get("io.modelcontextprotocol/protocolVersion"))
        .and_then(Value::as_str)
        == Some(MODERN_PROTOCOL_VERSION)
}

fn validate_modern_request(params: &Value) -> Result<(), Diagnostic> {
    let meta = params
        .get("_meta")
        .and_then(Value::as_object)
        .ok_or_else(|| Diagnostic::new("modern_metadata_required", "modern_metadata_required"))?;
    if meta
        .get("io.modelcontextprotocol/clientInfo")
        .and_then(Value::as_object)
        .is_none()
    {
        return Err(Diagnostic::new(
            "modern_metadata_required",
            "modern_metadata_required:clientInfo",
        ));
    }
    if meta
        .get("io.modelcontextprotocol/clientCapabilities")
        .and_then(Value::as_object)
        .is_none()
    {
        return Err(Diagnostic::new(
            "modern_metadata_required",
            "modern_metadata_required:clientCapabilities",
        ));
    }
    Ok(())
}

fn modern_request_params() -> Value {
    json!({
        "_meta": {
            "io.modelcontextprotocol/protocolVersion": MODERN_PROTOCOL_VERSION,
            "io.modelcontextprotocol/clientInfo": {"name": SERVER_NAME, "version": SERVER_VERSION},
            "io.modelcontextprotocol/clientCapabilities": {}
        }
    })
}

fn modernize_result(value: Value, method: &str) -> Value {
    let mut result = value.as_object().cloned().unwrap_or_default();
    result.insert("resultType".to_string(), json!("complete"));
    if matches!(method, "tools/list" | "resources/list" | "resources/read") {
        result.entry("ttlMs".to_string()).or_insert(json!(300_000));
        result
            .entry("cacheScope".to_string())
            .or_insert(json!("public"));
    }
    let mut meta = result
        .remove("_meta")
        .and_then(|entry| entry.as_object().cloned())
        .unwrap_or_default();
    meta.insert(
        "io.modelcontextprotocol/serverInfo".to_string(),
        json!({"name": SERVER_NAME, "version": SERVER_VERSION}),
    );
    result.insert("_meta".to_string(), Value::Object(meta));
    Value::Object(result)
}

fn modern_discover_result() -> Value {
    json!({
        "supportedVersions": [MODERN_PROTOCOL_VERSION, PROTOCOL_VERSION],
        "capabilities": {"tools": {}},
        "ttlMs": 3_600_000,
        "cacheScope": "public"
    })
}

fn modern_discovery_is_valid(value: &Value) -> bool {
    value.get("resultType").and_then(Value::as_str) == Some("complete")
        && value
            .get("supportedVersions")
            .and_then(Value::as_array)
            .is_some_and(|versions| {
                versions
                    .iter()
                    .any(|version| version.as_str() == Some(MODERN_PROTOCOL_VERSION))
            })
}
fn dispatch(request: &Value, state: &mut LoaderState) -> Result<Value, Diagnostic> {
    let method = request
        .get("method")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let params = request.get("params").cloned().unwrap_or_else(|| json!({}));
    if is_modern_request(&params) {
        validate_modern_request(&params)?;
        return match method {
            "server/discover" => Ok(modernize_result(modern_discover_result(), method)),
            "initialize" => Err(Diagnostic::new(
                "initialize_removed",
                "The 2026-07-28 protocol has no initialize handshake.",
            )),
            _ => {
                dispatch_legacy(method, &params, state).map(|value| modernize_result(value, method))
            }
        };
    }
    dispatch_legacy(method, &params, state)
}

fn dispatch_legacy(
    method: &str,
    params: &Value,
    state: &mut LoaderState,
) -> Result<Value, Diagnostic> {
    match method {
        "initialize" => Ok(json!({
            "protocolVersion": PROTOCOL_VERSION,
            "capabilities": {"tools": {}},
            "serverInfo": {"name": SERVER_NAME, "version": SERVER_VERSION}
        })),
        "notifications/initialized" | "ping" => Ok(json!({})),
        "tools/list" => Ok(json!({"tools": list_tools()})),
        "tools/call" => {
            let object = params
                .as_object()
                .cloned()
                .ok_or_else(|| Diagnostic::new("invalid_tool_call", "invalid_tool_call"))?;
            let name = required_string(&object, "name", "missing_tool_name")?;
            let args = object
                .get("arguments")
                .cloned()
                .unwrap_or_else(|| json!({}));
            let result = call_tool(&name, args, state)?;
            Ok(call_tool_result(result))
        }
        _ => Err(Diagnostic::new(
            "unsupported_mcp_method",
            format!("unsupported_mcp_method:{}", method),
        )),
    }
}
fn call_tool_result(result: Value) -> Value {
    let text = render_result(&result);
    json!({"content":[{"type":"text","text":text,"annotations":{"audience":["assistant"]}}],"structuredContent":result})
}

fn required_string(object: &JsonObject, key: &str, code: &str) -> Result<String, Diagnostic> {
    object
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .ok_or_else(|| Diagnostic::new(code, code))
}

fn optional_str(value: impl AsRef<str>) -> Option<String> {
    let text = value.as_ref().trim();
    if text.is_empty() {
        None
    } else {
        Some(text.to_string())
    }
}

fn value_string(value: Option<&Value>) -> Option<String> {
    value.and_then(Value::as_str).and_then(optional_str)
}

fn string_array(value: Option<&Value>) -> Result<Option<Vec<String>>, Diagnostic> {
    let Some(value) = value else {
        return Ok(None);
    };
    let array = value
        .as_array()
        .ok_or_else(|| Diagnostic::new("invalid_string_array", "invalid_string_array"))?;
    Ok(Some(
        array
            .iter()
            .filter_map(|item| item.as_str().map(ToOwned::to_owned))
            .collect(),
    ))
}

fn normalize_path(raw: &str) -> String {
    let input = PathBuf::from(raw.replace('\\', "/"));
    let absolute = if input.is_absolute() {
        input
    } else {
        env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join(input)
    };
    let mut parts: Vec<String> = Vec::new();
    for component in absolute.components() {
        match component {
            Component::Prefix(prefix) => {
                parts.push(prefix.as_os_str().to_string_lossy().to_string())
            }
            Component::RootDir => {}
            Component::CurDir => {}
            Component::ParentDir => {
                let _ = parts.pop();
            }
            Component::Normal(part) => parts.push(part.to_string_lossy().to_string()),
        }
    }
    let mut output = if raw.contains(':') && raw.as_bytes().get(1) == Some(&b':') {
        String::new()
    } else {
        String::new()
    };
    if raw.len() >= 2 && raw.as_bytes()[1] == b':' {
        output.push_str(&raw[..2]);
    }
    if output.is_empty() {
        if raw.starts_with('/') {
            output.push('/');
        }
    } else if !output.ends_with('/') {
        output.push('/');
    }
    output.push_str(
        &parts
            .iter()
            .skip(if raw.len() >= 2 && raw.as_bytes()[1] == b':' {
                1
            } else {
                0
            })
            .cloned()
            .collect::<Vec<_>>()
            .join("/"),
    );
    if output.is_empty() {
        ".".to_string()
    } else {
        output.trim_end_matches('/').to_string()
    }
}

fn join_path(root: &str, child: &str) -> String {
    normalize_path(&format!(
        "{}/{}",
        root.trim_end_matches(['\\', '/']),
        child.trim_start_matches(['\\', '/'])
    ))
}

fn normalize_policy_prefix(prefix: &str) -> String {
    let normalized = prefix.replace('\\', "/").trim_end_matches('/').to_string();
    if normalized == "{site_root}" || normalized.starts_with("{site_root}/") {
        normalized
    } else {
        normalize_path(&normalized)
    }
}

fn is_under_path(child: &str, parent: &str) -> bool {
    let c = normalize_path(child);
    let p = normalize_path(parent);
    c == p || c.starts_with(&(p + "/"))
}

fn derive_site_id(site_root: &str) -> Result<String, Diagnostic> {
    let normalized = site_root.replace('\\', "/");
    let value = normalized
        .rsplit('/')
        .find(|part| !part.is_empty())
        .unwrap_or("site");
    let id = value
        .strip_prefix("narada.")
        .or_else(|| value.strip_prefix("narada-"))
        .unwrap_or(value)
        .to_string();
    if id == "andrey"
        || id == "user-site"
        || value == "narada-andrey"
        || value == "narada-user-site"
    {
        return Err(Diagnostic::new(
            "site_fabric_legacy_site_id_rejected",
            format!("site_fabric_legacy_site_id_rejected:{}:site_root", value),
        ));
    }
    Ok(id)
}

fn interpolate_site_arg(value: &str, site_root: &str) -> Result<String, Diagnostic> {
    let site_control_root = if site_root.ends_with("/.narada") {
        site_root.to_string()
    } else {
        join_path(site_root, ".narada")
    };
    let site_id = derive_site_id(site_root)?;
    Ok(value
        .replace("{site_root}", site_root)
        .replace("{site_control_root}", &site_control_root)
        .replace(
            "{site_runtime_root}",
            &join_path(&site_control_root, "runtime"),
        )
        .replace("{site_id}", &site_id))
}

fn now_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or_default()
}

fn now_iso() -> String {
    OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .unwrap_or_else(|_| "1970-01-01T00:00:00Z".to_string())
}

fn new_id(prefix: &str) -> String {
    format!(
        "{}-{}-{}",
        prefix,
        now_ms(),
        ID_COUNTER.fetch_add(1, Ordering::SeqCst)
    )
}

struct FabricBundle {
    fabric: JsonObject,
    paths: Vec<String>,
    source_by_surface: HashMap<String, String>,
}

fn read_site_fabric(site_root: &str) -> Result<FabricBundle, Diagnostic> {
    let paths = resolve_site_fabric_paths(site_root)?;
    let mut servers = Map::new();
    let mut source_by_surface = HashMap::new();
    let mut site_id: Option<String> = None;
    for path in &paths {
        let text = read_to_string(path).map_err(|error| {
            Diagnostic::new(
                "site_fabric_parse_error",
                format!("site_fabric_parse_error:{}:{}", path, error),
            )
        })?;
        let fragment: Value = serde_json::from_str(&text).map_err(|error| {
            Diagnostic::new(
                "site_fabric_parse_error",
                format!("site_fabric_parse_error:{}:{}", path, error),
            )
        })?;
        let fragment_obj = fragment.as_object().cloned().ok_or_else(|| {
            Diagnostic::new(
                "site_fabric_parse_error",
                format!("site_fabric_parse_error:{}:object_required", path),
            )
        })?;
        if let Some(fragment_site_id) = value_string(fragment_obj.get("site_id")) {
            if fragment_site_id == "narada-andrey" || fragment_site_id == "narada-user-site" {
                return Err(Diagnostic::new(
                    "site_fabric_legacy_site_id_rejected",
                    format!(
                        "site_fabric_legacy_site_id_rejected:{}:{}",
                        fragment_site_id, path
                    ),
                ));
            }
            if let Some(existing) = &site_id {
                if existing != &fragment_site_id {
                    return Err(Diagnostic::new(
                        "site_fabric_site_id_mismatch",
                        format!(
                            "site_fabric_site_id_mismatch:{}:{}:{}",
                            existing, fragment_site_id, path
                        ),
                    ));
                }
            }
            site_id = Some(fragment_site_id);
        }
        let fragment_servers = fragment_obj
            .get("mcpServers")
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();
        for (surface_id, server) in fragment_servers {
            if let Some(previous) = source_by_surface.get(&surface_id) {
                return Err(Diagnostic::new(
                    "site_fabric_duplicate_surface",
                    format!(
                        "site_fabric_duplicate_surface:{}:{}:{}",
                        surface_id, previous, path
                    ),
                ));
            }
            servers.insert(surface_id.clone(), server);
            source_by_surface.insert(surface_id, path.clone());
        }
    }
    let schema = if paths.len() == 1 {
        "narada.mcp_loader.site_fabric.v1"
    } else {
        "narada.mcp_loader.fragmented_site_fabric.v1"
    };
    let mut fabric = Map::new();
    fabric.insert("schema".to_string(), json!(schema));
    fabric.insert(
        "site_id".to_string(),
        site_id.map(Value::String).unwrap_or(Value::Null),
    );
    fabric.insert("mcpServers".to_string(), Value::Object(servers));
    Ok(FabricBundle {
        fabric,
        paths,
        source_by_surface,
    })
}

fn resolve_site_fabric_paths(site_root: &str) -> Result<Vec<String>, Diagnostic> {
    let mcp_dir = join_path(site_root, ".ai/mcp");
    let canonical = join_path(&mcp_dir, "config.json");
    let canonical_exists = Path::new(&canonical).exists();
    let canonical_has_servers = if canonical_exists {
        site_fabric_has_declared_servers(&canonical)
    } else {
        None
    };
    if canonical_exists && canonical_has_servers != Some(false) {
        return Ok(vec![canonical]);
    }
    let site_base = Path::new(site_root)
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("site")
        .replace('.', "-");
    let aggregate = join_path(&mcp_dir, &format!("{}-mcp.json", site_base));
    if Path::new(&aggregate).exists() {
        return Ok(vec![aggregate]);
    }
    if !Path::new(&mcp_dir).exists() {
        return Err(Diagnostic::new(
            "site_fabric_not_found",
            format!("site_fabric_not_found:{}", canonical),
        ));
    }
    let mut candidates = Vec::new();
    if let Ok(entries) = read_dir(&mcp_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            let name = path
                .file_name()
                .and_then(|value| value.to_str())
                .unwrap_or_default();
            if !name.ends_with("-mcp.json") {
                continue;
            }
            let Some(path_string) = path.to_str().map(normalize_path) else {
                continue;
            };
            let Ok(text) = read_to_string(&path_string) else {
                continue;
            };
            let Ok(value) = serde_json::from_str::<Value>(&text) else {
                continue;
            };
            if value.get("mcpServers").and_then(Value::as_object).is_some() {
                candidates.push(path_string);
            }
        }
    }
    candidates.sort();
    if !candidates.is_empty() {
        return Ok(candidates);
    }
    if canonical_exists {
        return Ok(vec![canonical]);
    }
    Err(Diagnostic::new(
        "site_fabric_not_found",
        format!("site_fabric_not_found:{}", canonical),
    ))
}

fn site_fabric_has_declared_servers(path: &str) -> Option<bool> {
    let text = read_to_string(path).ok()?;
    let value = serde_json::from_str::<Value>(&text).ok()?;
    Some(
        value
            .get("mcpServers")
            .and_then(Value::as_object)
            .map(|servers| !servers.is_empty())
            .unwrap_or(false),
    )
}

fn find_site_server(
    servers: &JsonObject,
    requested: &str,
) -> Result<Option<(String, Value)>, Diagnostic> {
    if let Some(server) = servers.get(requested) {
        return Ok(Some((requested.to_string(), server.clone())));
    }
    let mut matches = Vec::new();
    for (server_name, server) in servers {
        if server.get("surface_id").and_then(Value::as_str) == Some(requested) {
            matches.push((server_name.clone(), server.clone()));
        }
    }
    if matches.len() > 1 {
        let names = matches
            .iter()
            .map(|(name, _)| name.clone())
            .collect::<Vec<_>>();
        return Err(Diagnostic::new(
            "surface_id_ambiguous",
            format!("surface_id_ambiguous:{}", requested),
        )
        .with_details(json!({"surface_id":requested,"server_names":names})));
    }
    Ok(matches.into_iter().next())
}

fn shared_surface_registry(surface_id: &str, surface_root: &str) -> Option<(String, Vec<String>)> {
    let entrypoint = |package: &str, file: &str| {
        join_path(surface_root, &format!("{}/dist/src/{}", package, file))
    };
    let result = match surface_id {
        "operator-console-overlay" => (
            entrypoint("operator-console-overlay-mcp", "main.js"),
            vec![],
        ),
        "local-filesystem" => (
            entrypoint("local-filesystem-mcp", "main.js"),
            vec![
                "--mode",
                "write",
                "--allowed-root",
                "{site_root}",
                "--anchored-allowed-root",
                "user_home:.codex",
                "--output-root",
                "{site_root}",
            ],
        ),
        "structured-command" => (
            entrypoint("structured-command-mcp", "main.js"),
            vec![
                "--allowed-root",
                "{site_root}",
                "--allow-command",
                "node",
                "--allow-command",
                "pnpm",
                "--allow-command",
                "npm",
            ],
        ),
        "git" => (
            entrypoint("git-mcp", "main.js"),
            vec!["--allowed-root", "{site_root}", "--mode", "write"],
        ),
        "site-inbox" => (
            entrypoint("site-inbox-mcp", "main.js"),
            vec!["--site-root", "{site_root}"],
        ),
        "mailbox" => (
            entrypoint("mailbox-mcp", "main.js"),
            vec!["--site-root", "{site_root}"],
        ),
        "graph-mail" => (
            entrypoint("graph-mail-mcp", "main.js"),
            vec!["--site-root", "{site_root}"],
        ),
        "calendar" => (
            entrypoint("calendar-mcp", "main.js"),
            vec!["--site-root", "{site_root}"],
        ),
        "task-lifecycle" => (
            entrypoint("task-lifecycle-mcp", "task-lifecycle/task-mcp-server.js"),
            vec!["--site-root", "{site_root}"],
        ),
        "site-loop" => (
            entrypoint("site-loop-mcp", "site-loop-mcp-server.js"),
            vec!["--site-root", "{site_root}"],
        ),
        "agent-context" => (
            entrypoint("agent-context-mcp", "main.js"),
            vec!["--site-root", "{site_root}"],
        ),
        "catalog-observation" => (entrypoint("catalog-observation-mcp", "main.js"), vec![]),
        "runtime-introspection" => (entrypoint("runtime-introspection-mcp", "main.js"), vec![]),
        "worker-delegation" => (
            entrypoint("worker-delegation-mcp", "main.js"),
            vec![
                "--allowed-root",
                "{site_root}",
                "--run-root",
                "{site_runtime_root}/worker-delegation",
            ],
        ),
        "delegated-task" => (
            entrypoint("delegated-task-mcp", "main.js"),
            vec![
                "--task-root",
                "{site_root}",
                "--allowed-root",
                "{site_root}",
            ],
        ),
        "sop" => (
            entrypoint("sop-mcp", "main.js"),
            vec![
                "--sop-root",
                "{site_root}",
                "--server-name",
                "{site_id}-sop",
            ],
        ),
        "scheduler" => (
            entrypoint("scheduler-mcp", "main.js"),
            vec!["--allowed-root", "{site_root}"],
        ),
        "mcp-registrar" => (entrypoint("mcp-registrar", "main.js"), vec![]),
        "surface-feedback" => (
            entrypoint("surface-feedback-mcp", "main.js"),
            vec![
                "--feedback-root",
                "{site_control_root}/feedback",
                "--canonical-feedback-root",
                "{site_control_root}/feedback",
                "--task-lifecycle-root",
                "{site_root}",
                "--site-id",
                "{site_id}",
            ],
        ),
        "speech" => (entrypoint("speech-mcp", "main.js"), vec![]),
        "cloudflare-carrier" => (
            entrypoint("cloudflare-carrier-mcp", "main.js"),
            vec!["--site-root", "{site_root}"],
        ),
        "site-coherence" => (
            entrypoint("site-coherence-mcp", "main.js"),
            vec!["--site-root", "{site_root}"],
        ),
        "site-lifecycle" => (
            entrypoint("site-lifecycle-mcp", "main.js"),
            vec!["--narada-root", "{site_root}"],
        ),
        "artifacts" => (entrypoint("artifacts-mcp", "main.js"), vec![]),
        "epistemic-graph" => (
            entrypoint("shared/mcp-surfaces-native", "narada-mcp-surfaces.exe"),
            vec![
                "--surface-id",
                "epistemic-graph",
                "--site-root",
                "{site_root}",
            ],
        ),
        "nars-session" => (entrypoint("nars-session-mcp", "main.js"), vec![]),
        "quota-meter" => (entrypoint("quota-meter-mcp", "main.js"), vec![]),
        _ => return None,
    };
    Some((result.0, result.1.into_iter().map(String::from).collect()))
}

fn extract_runtime_entrypoint(command: &str, args: &[String]) -> Option<String> {
    if is_runtime_proxy_command(command) && args.first().map(String::as_str) == Some("proxy") {
        return extract_proxy_child_entrypoint(args);
    }
    let normalized = command.trim().replace('\\', "/");
    let base = normalized
        .rsplit('/')
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase();
    if [
        "node", "node.exe", "node.cmd", "bun", "bun.exe", "deno", "deno.exe",
    ]
    .contains(&base.as_str())
    {
        return args
            .iter()
            .find(|arg| arg.ends_with(".mjs") || arg.ends_with(".js") || arg.ends_with(".cjs"))
            .cloned();
    }
    let stripped = command
        .trim()
        .strip_prefix("node --import tsx ")
        .or_else(|| command.trim().strip_prefix("node "));
    if let Some(value) = stripped {
        if !value.trim().is_empty() && value.trim() != "node" {
            return Some(value.trim().to_string());
        }
    }
    args.iter()
        .find(|arg| arg.ends_with(".mjs") || arg.ends_with(".js") || arg.ends_with(".cjs"))
        .cloned()
}

fn remove_entrypoint_arg(args: &[String], entrypoint: &str) -> Vec<String> {
    let normalized = normalize_path(entrypoint);
    let mut removed = false;
    args.iter()
        .filter_map(|arg| {
            if !removed && normalize_path(arg) == normalized {
                removed = true;
                None
            } else {
                Some(arg.clone())
            }
        })
        .collect()
}

fn surface_requirements(server: Option<&Value>) -> Vec<String> {
    let Some(server) = server else {
        return Vec::new();
    };
    let projection = server.get("surface_projection").and_then(Value::as_object);
    let values = projection
        .and_then(|object| object.get("runtime_requirements"))
        .or_else(|| server.get("runtime_requirements"));
    values
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.as_str().and_then(optional_str))
                .collect()
        })
        .unwrap_or_default()
}

fn runtime_matches(requirements: &[String], runtime_kind: Option<&str>) -> bool {
    requirements.is_empty()
        || runtime_kind
            .is_some_and(|kind| requirements.iter().any(|requirement| requirement == kind))
}

fn runtime_metadata(
    site_root: &str,
    surface_id: &str,
) -> Result<
    (
        String,
        String,
        Value,
        Value,
        Option<Value>,
        Option<String>,
        Option<String>,
        Vec<String>,
    ),
    Diagnostic,
> {
    let bundle = read_site_fabric(site_root)?;
    let matched = find_site_server(
        bundle
            .fabric
            .get("mcpServers")
            .and_then(Value::as_object)
            .unwrap_or(&Map::new()),
        surface_id,
    )?;
    let server_name = matched
        .as_ref()
        .map(|(name, _)| name.clone())
        .unwrap_or_else(|| surface_id.to_string());
    let server = matched
        .as_ref()
        .map(|(_, server)| server.clone())
        .unwrap_or_else(|| json!({}));
    let projection = server.get("surface_projection").and_then(Value::as_object);
    let projection_id = projection
        .and_then(|object| object.get("id").or_else(|| object.get("projection_id")))
        .and_then(Value::as_str)
        .or_else(|| server.get("projection_id").and_then(Value::as_str))
        .unwrap_or("default")
        .to_string();
    let execution = projection
        .and_then(|object| object.get("execution"))
        .cloned()
        .unwrap_or_else(
            || json!({"adapter":"stdio","tenancy":"session_isolated","replacement":"manual"}),
        );
    let execution = if execution.is_object() {
        execution
    } else {
        json!({"adapter":"stdio","tenancy":"session_isolated","replacement":"manual"})
    };
    let lifecycle = projection
        .and_then(|object| object.get("lifecycle"))
        .or_else(|| server.get("lifecycle"))
        .filter(|value| value.get("mode").and_then(Value::as_str).is_some())
        .cloned()
        .unwrap_or_else(|| json!({"mode":"replayable"}));
    let descriptor = projection
        .and_then(|object| {
            object
                .get("descriptor")
                .or_else(|| object.get("surface_descriptor"))
        })
        .or_else(|| {
            server
                .get("descriptor")
                .or_else(|| server.get("surface_descriptor"))
        })
        .cloned();
    let descriptor_digest = projection
        .and_then(|object| {
            object
                .get("descriptor_digest")
                .or_else(|| object.get("surface_descriptor_digest"))
        })
        .or_else(|| {
            server
                .get("descriptor_digest")
                .or_else(|| server.get("surface_descriptor_digest"))
        })
        .and_then(Value::as_str)
        .map(String::from);
    let declared_digest = projection
        .and_then(|object| {
            object
                .get("tool_contract_digest")
                .or_else(|| object.get("surface_tool_contract_digest"))
        })
        .or_else(|| {
            server
                .get("tool_contract_digest")
                .or_else(|| server.get("surface_tool_contract_digest"))
        })
        .and_then(Value::as_str)
        .map(String::from);
    Ok((
        server_name,
        projection_id,
        execution,
        lifecycle,
        descriptor,
        descriptor_digest,
        declared_digest,
        surface_requirements(Some(&server)),
    ))
}

fn ensure_site_root_allowed(site_root: &str, policy: &Policy) -> Result<(), Diagnostic> {
    let normalized = normalize_path(site_root);
    let candidate = if cfg!(windows) {
        normalized.to_lowercase()
    } else {
        normalized.clone()
    };
    if policy.allowed_site_roots.iter().any(|allowed| {
        let boundary = if cfg!(windows) {
            allowed.to_lowercase()
        } else {
            allowed.clone()
        };
        candidate == boundary || candidate.starts_with(&(boundary + "/"))
    }) {
        Ok(())
    } else {
        Err(Diagnostic::new(
            "site_root_not_allowed",
            format!("site_root_not_allowed:{}", site_root),
        ))
    }
}

fn ensure_surface_allowed(
    surface_id: &str,
    site_root: &str,
    policy: &Policy,
    state: &LoaderState,
) -> Result<(), Diagnostic> {
    let bundle = read_site_fabric(site_root)?;
    let servers = bundle
        .fabric
        .get("mcpServers")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    let allowed = match &policy.allowed_surface_ids {
        None => {
            find_site_server(&servers, surface_id)?.is_some()
                || shared_surface_registry(surface_id, &state.surface_root).is_some()
        }
        Some(ids) => {
            find_site_server(&servers, surface_id)?.is_some_and(|(key, _)| ids.contains(&key))
                || ids.contains(&surface_id.to_string())
        }
    };
    if allowed {
        Ok(())
    } else {
        Err(Diagnostic::new(
            "surface_not_allowed",
            format!("surface_not_allowed:{}", surface_id),
        ))
    }
}

fn ensure_entrypoint_allowed(
    site_root: &str,
    entrypoint: &str,
    policy: &Policy,
) -> Result<(), Diagnostic> {
    let normalized = normalize_path(entrypoint);
    for prefix in &policy.allowed_entrypoint_prefixes {
        let expanded = prefix.replace("{site_root}", &normalize_path(site_root));
        if normalized == expanded || normalized.starts_with(&(expanded + "/")) {
            return Ok(());
        }
    }
    Err(Diagnostic::new(
        "entrypoint_not_allowed",
        format!("entrypoint_not_allowed:{}", entrypoint),
    ))
}

fn assert_binding_admission_available(state: &LoaderState) -> Result<(), Diagnostic> {
    if state.binding_admission.is_some() || state.standalone_ambient_attachment {
        Ok(())
    } else {
        Err(Diagnostic::new("mcp_binding_admission_required", "mcp_binding_admission_required")
            .with_details(json!({"child_spawned":false,"remediation":"Launch through an admitted Narada carrier session or use --standalone-ambient-attachment only for explicit development fixtures."})))
    }
}

fn admitted_binding(
    state: &LoaderState,
    _site_root: &str,
    binding_id: &str,
    operation: &str,
) -> Result<Option<(Value, Value)>, Diagnostic> {
    assert_binding_admission_available(state)?;
    let Some(envelope) = &state.binding_admission else {
        return Ok(None);
    };
    let entry = envelope
        .get("bindings")
        .and_then(Value::as_array)
        .and_then(|bindings| {
            bindings.iter().find(|binding| {
                binding.get("binding_id").and_then(Value::as_str) == Some(binding_id)
            })
        })
        .cloned()
        .ok_or_else(|| {
            let candidates = envelope
                .get("bindings")
                .and_then(Value::as_array)
                .map(|bindings| {
                    bindings
                        .iter()
                        .filter(|binding| {
                            let candidate = binding.get("binding_id").and_then(Value::as_str).unwrap_or_default();
                            let surface = binding.get("surface_id").and_then(Value::as_str).unwrap_or_default();
                            !surface.is_empty()
                                && (binding_id.ends_with(surface)
                                    || candidate.ends_with(binding_id)
                                    || candidate == binding_id)
                        })
                        .filter_map(|binding| binding.get("binding_id").cloned())
                        .take(10)
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            Diagnostic::new(
                "mcp_binding_not_admitted",
                format!("mcp_binding_not_admitted:{binding_id}:{operation}"),
            ).with_details(json!({
                "requested_binding_id":binding_id,
                "operation":operation,
                "candidate_binding_ids":candidates,
                "remediation":"Use the canonical binding_id returned by mcp_loader_list_site_surfaces or registrar_site_bind; server_name/server_key is not binding identity."
            }))
        })?;
    let operation_allowed = entry
        .get("operations")
        .and_then(Value::as_array)
        .is_some_and(|operations| {
            operations
                .iter()
                .any(|value| value.as_str() == Some(operation))
        });
    if !operation_allowed {
        return Err(Diagnostic::new(
            "mcp_binding_not_admitted",
            format!("mcp_binding_not_admitted:{binding_id}:{operation}"),
        ));
    }
    let server = entry.get("binding_identity").cloned().ok_or_else(|| {
        Diagnostic::new(
            "mcp_binding_identity_required",
            format!("mcp_binding_identity_required:{binding_id}"),
        )
    })?;
    let actual = narada_mcp_fabric_contracts::binding_admission_entry_digest_v1(&entry);
    let expected = entry
        .get("binding_digest")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if actual != expected {
        return Err(Diagnostic::new("mcp_binding_digest_mismatch", format!("mcp_binding_digest_mismatch:{binding_id}"))
            .with_details(json!({"child_spawned":false,"expected_binding_digest":expected,"actual_binding_digest":actual})));
    }
    Ok(Some((entry, server)))
}

fn supervisor_restart_action() -> Value {
    json!({
        "schema":"narada.mcp_loader.supervisor_restart_action.v1",
        "kind":"restart_loader_process",
        "target":"mcp-loader-process",
        "owner":"carrier_or_runtime_supervisor",
        "operation":"restart",
        "capability":"restart_mcp_loader_process",
        "tool_name":"restart_mcp_loader_process",
        "arguments":{},
        "actuator_scope":"external_supervisor_capability",
        "agent_callable":false,
        "availability":"external_supervisor_only",
        "invocation_note":"This is a carrier/runtime-supervisor capability name, not a tool exposed by mcp-loader. The agent must call the carrier supervisor only when that capability is separately present.",
        "next_call":{"tool_name":"restart_mcp_loader_process","arguments":{}},
        "connection_id_required":false,
        "session_restart_required":false
    })
}

fn runtime_lifecycle(connection_id: Option<&str>, lifecycle: Option<&Value>) -> Value {
    let attached = connection_id.is_some();
    let mode = lifecycle
        .and_then(|value| value.get("mode"))
        .and_then(Value::as_str);
    let non_replayable = attached && mode.is_some_and(|value| value != "replayable");
    let restartable = if non_replayable {
        Value::Bool(false)
    } else if attached {
        Value::Bool(true)
    } else {
        Value::Null
    };
    let mut result = json!({
        "schema":"narada.mcp_loader.runtime_lifecycle.v1",
        "managed_by":"mcp-loader",
        "restartable":restartable,
        "restartability_status":if non_replayable {"unavailable_for_lifecycle"} else if attached {"available"} else {"available_after_successful_attach"},
        "restart_scope":if non_replayable {"carrier_supervisor"} else {"attached_child_process"},
        "session_restart_required":false,
        "connection_id_required":true,
        "inventory_tool":"mcp_loader_connection_inventory",
        "status_tool":"mcp_loader_surface_status",
        "restart_tool":if non_replayable {Value::Null} else {json!("mcp_loader_surface_restart")},
        "loader_restart_action":supervisor_restart_action(),
        "guidance":if non_replayable {
            format!("This projection declares lifecycle mode {}; mcp-loader must not replace its child. Ask the carrier or runtime supervisor to invoke restart_mcp_loader_process, then reconnect the surface.", mode.unwrap_or("unknown"))
        } else {
            "Restart replaces only the attached child surface process; it does not restart the agent session or reload the mcp-loader process.".to_string()
        }
    });
    if attached && !non_replayable {
        let id = connection_id.unwrap_or_default();
        result["actions"] = json!({
            "inspect":{"tool_name":"mcp_loader_surface_status","arguments":{"connection_id":id}},
            "restart":{"tool_name":"mcp_loader_surface_restart","arguments":{"connection_id":id}}
        });
    }
    result
}

fn guidance_definition() -> Value {
    json!({
        "name":"mcp_loader_guidance",
        "description":"Show model-facing operating guidance for mcp-loader MCP workflows.",
        "inputSchema":{"type":"object","properties":{
            "workflow":{"type":"string","description":"Optional workflow name or area to focus guidance on."},
            "tool":{"type":"string","description":"Optional tool name for tool-specific guidance."}
        },"additionalProperties":false},
        "annotations":{"title":"mcp_loader_guidance","readOnlyHint":true,"destructiveHint":false,"idempotentHint":true,"openWorldHint":false},
        "outputSchema":{"type":"object","additionalProperties":true}
    })
}

fn tool_definition(
    name: &str,
    description: &str,
    properties: Value,
    required: &[&str],
    read_only: bool,
    destructive: bool,
) -> Value {
    json!({
        "name":name,
        "description":description,
        "annotations":{"title":name,"readOnlyHint":read_only,"destructiveHint":destructive,"idempotentHint":false,"openWorldHint":true},
        "inputSchema":{"type":"object","properties":properties,"additionalProperties":false,"required":required},
        "outputSchema":{"type":"object","additionalProperties":true}
    })
}

fn list_tools() -> Vec<Value> {
    vec![
        guidance_definition(),
        tool_definition("mcp_loader_runtime_status","Inspect whether this loader process is current relative to its runtime, source, dependency, and build-configuration evidence and whether the loader process itself must be restarted.",json!({}),&[],true,false),
        tool_definition("mcp_loader_policy_inspect","Inspect the policy governing runtime MCP surface loading.",json!({}),&[],true,false),
        tool_definition("mcp_loader_connection_inventory","List attached loader connections, including liveness, age, explicit loader-managed restartability, capacity, and bounded recovery actions for stale children.",json!({}),&[],true,false),
        tool_definition("mcp_loader_process_ownership","Inspect process ownership for children spawned by this loader run. This is a read-only reconciliation view: it reports loader-owned direct children and safe cleanup actions, but never enumerates or terminates unrelated host processes or conhost descendants.",json!({}),&[],true,false),
        tool_definition("mcp_loader_runtime_observation","Return the normalized V2 runtime observation for one attached surface, including stable logical identity, generation state, lifecycle eligibility, contract digests, and bounded actuator guidance.",json!({"connection_id":{"type":"string"},"carrier_kind":{"type":"string"},"manifest_digest":{"type":"string"}}),&["connection_id","carrier_kind"],true,false),
        tool_definition("mcp_loader_list_site_surfaces","List resolvable MCP surfaces declared in a site's local fabric.",json!({"site_root":{"type":"string"}}),&["site_root"],true,false),
        tool_definition("mcp_loader_site_fabric_diagnostics","Inspect site MCP fabric provenance and classify shared-registry drift or intentional entrypoint overrides.",json!({"site_root":{"type":"string"}}),&["site_root"],true,false),
        tool_definition("mcp_loader_site_tool_inventory_check","Compare site fabric declarations with fresh child tools/list responses; compact output includes per-finding status and tool-name deltas, runtime-skipped surfaces produce partial coverage, and an immutable observation_ref is materialized for Registrar conformance checks.",json!({"site_root":{"type":"string"},"surface_ids":{"type":"array","items":{"type":"string"}},"runtime_kind":{"type":"string"},"include_ok":{"type":"boolean"}}),&["site_root"],true,false),
        tool_definition("mcp_loader_attach_surface","Spawn and initialize an exactly admitted stdio MCP binding, return a connection id, and report loader-managed restartability.",json!({"site_root":{"type":"string"},"binding_id":{"type":"string"},"surface_id":{"type":"string"},"runtime_kind":{"type":"string"},"entrypoint":{"type":"string"},"args":{"type":"array","items":{"type":"string"}}}),&["site_root","binding_id"],false,false),
        tool_definition("mcp_loader_open_surface","Open an exactly admitted binding and return a stable logical handle for calls across loader-managed child generations.",json!({"site_root":{"type":"string"},"binding_id":{"type":"string"},"surface_id":{"type":"string"},"runtime_kind":{"type":"string"},"entrypoint":{"type":"string"},"args":{"type":"array","items":{"type":"string"}}}),&["site_root","binding_id"],false,false),
        tool_definition("mcp_loader_surface_handle_inventory","List stable logical surface handles and the current child generation, without spawning or replacing a surface.",json!({}),&[],true,false),
        tool_definition("mcp_loader_list_tools","List tools exposed by an attached MCP surface.",json!({"connection_id":{"type":"string"}}),&["connection_id"],true,false),
        tool_definition("mcp_loader_surface_status","Inspect the runtime status and loader-managed restartability of an attached MCP surface child process.",json!({"connection_id":{"type":"string"}}),&["connection_id"],true,false),
        tool_definition("mcp_loader_tool_discovery_manifest","Return canonical semantic tool names for an attached surface and flag generated aliases as non-authoritative.",json!({"connection_id":{"type":"string"}}),&["connection_id"],true,false),
        tool_definition("mcp_loader_call_tool","Call a tool on an attached MCP surface. Results are bounded by default and include a typed summary; set include_runtime_metadata=true when lifecycle/freshness evidence is needed on this call.",json!({"connection_id":{"type":"string"},"tool_name":{"type":"string"},"arguments":{"type":"object"},"include_runtime_metadata":{"type":"boolean"}}),&["connection_id","tool_name"],false,false),
        tool_definition("mcp_loader_call_surface_tool","Call a tool through a stable logical surface handle. Results are bounded by default and include a typed summary; set include_runtime_metadata=true for lifecycle/freshness evidence.",json!({"surface_handle":{"type":"string"},"tool_name":{"type":"string"},"arguments":{"type":"object"},"include_runtime_metadata":{"type":"boolean"}}),&["surface_handle","tool_name"],false,false),
        tool_definition("mcp_loader_read_result","Read a bounded page from a materialized proxied child result. The ref is bound to the same Site authority as the connection.",json!({"connection_id":{"type":"string"},"ref":{"type":"string"},"output_ref":{"type":"string"},"offset":{"type":"integer","minimum":0},"limit":{"type":"integer","minimum":1},"timeout_ms":{"type":"integer","minimum":1,"maximum":15000}}),&["connection_id"],true,false),
        tool_definition("mcp_loader_detach","Detach and terminate an attached MCP surface.",json!({"connection_id":{"type":"string"}}),&["connection_id"],false,true),
        tool_definition("mcp_loader_surface_restart","Replace an attached MCP surface child process with a freshly initialized connection using the same site, surface, entrypoint, and args; this does not restart the agent session.",json!({"connection_id":{"type":"string"},"reason":{"type":"string"}}),&["connection_id"],false,true),
    ]
}

fn guidance_result(arguments: &JsonObject, state: &LoaderState) -> Value {
    let workflow = value_string(arguments.get("workflow"));
    let tool = value_string(arguments.get("tool"));
    json!({
        "schema":"narada.mcp_surface.guidance.v0",
        "status":"ok",
        "surface_id":"mcp-loader",
        "guidance_tool":"mcp_loader_guidance",
        "purpose":"Policy-gated runtime attachment and proxying for MCP surfaces admitted by a Site fabric.",
        "requested":{"workflow":workflow,"tool":tool},
        "runtime_lifecycle":runtime_lifecycle(None,None),
        "runtime_freshness":runtime_freshness(state),
        "tool_call_timeout":{
            "tool":"mcp_loader_call_tool","nested_argument":"arguments.timeout_ms",
            "policy_default_ms":DEFAULT_TOOL_CALL_TIMEOUT_MS,"request_max_ms":MAX_TOOL_TIMEOUT_MS,
            "grace_flag":"--tool-timeout-grace-ms","default_grace_ms":DEFAULT_TOOL_TIMEOUT_GRACE_MS,
            "grace_max_ms":MAX_TOOL_TIMEOUT_GRACE_MS,
            "semantics":"When nested timeout_ms is present, it is forwarded to the child and the loader waits timeout_ms plus bounded grace for the child timeout result. When absent, the loader policy default is the outer deadline and no grace is added."
        },
        "first_use":[
            "Call mcp_loader_policy_inspect before relying on loader capabilities or allowed roots.",
            "Call mcp_loader_connection_inventory before attachment when recovering from capacity errors or an earlier interrupted session.",
            "Call mcp_loader_process_ownership when reconciling child processes after an interrupted attach; it reports only this loader run's direct children and safe known-connection cleanup actions.",
            "Call mcp_loader_list_site_surfaces and mcp_loader_site_fabric_diagnostics for the explicit Site root.",
            "Use mcp_loader_attach_surface with an explicit surface_id and runtime_kind when the projection requires one.",
            "Inspect surface_projection.execution before attachment. mcp-loader accepts stdio projections only; surface_factory projections belong to the PC Site surface runtime.",
            "Use mcp_loader_list_tools or mcp_loader_tool_discovery_manifest after attachment; the child tools/list response owns exact tool schemas.",
            "Call mcp_loader_runtime_observation with connection_id and carrier_kind to obtain the V2 normalized observation.",
            "For mcp_loader_call_tool, place timeout_ms inside the nested arguments object.",
            "Call mcp_loader_runtime_status when the loader process may have out-of-date source, dependency, or build-configuration evidence.",
            "Preserve structuredContent as authoritative evidence; text content is for assistant readability."
        ],
        "tool_preference":[
            {"step":"orient","guidance":"Use mcp_loader_guidance, mcp_loader_runtime_status, and mcp_loader_policy_inspect before attachment or proxy calls."},
            {"step":"recover","guidance":"For a stale or transport-closed child, inspect inventory or status, then call mcp_loader_surface_restart."},
            {"step":"reconcile_processes","guidance":"Use mcp_loader_process_ownership to distinguish loader-owned direct children from unobserved host processes."},
            {"step":"resolve_site","guidance":"Use mcp_loader_list_site_surfaces and mcp_loader_site_fabric_diagnostics against the same explicit Site root."},
            {"step":"attach","guidance":"Use mcp_loader_open_surface when repeated calls should survive a loader-managed child restart."},
            {"step":"discover","guidance":"Use mcp_loader_list_tools or mcp_loader_tool_discovery_manifest."},
            {"step":"observe_live","guidance":"Use mcp_loader_site_tool_inventory_check to compare declared tools with fresh child tools/list responses."},
            {"step":"observe_runtime","guidance":"Call mcp_loader_runtime_observation after attachment."},
            {"step":"operate","guidance":"Call a child tool only after selecting the intended connection and honoring the child surface policy."},
            {"step":"finish","guidance":"Use mcp_loader_detach or mcp_loader_surface_restart deliberately and inspect returned evidence."}
        ],
        "examples":[
            {"intent":"First use","call":"mcp_loader_guidance({})"},
            {"intent":"Inspect a workflow","call":"mcp_loader_guidance({ workflow: \"discover\", tool: \"mcp_loader_list_tools\" })"},
            {"intent":"Recover capacity","call":"mcp_loader_connection_inventory({})"},
            {"intent":"Inspect a Site","call":"mcp_loader_list_site_surfaces({ site_root: \"<site_root>\" })"},
            {"intent":"Inspect loader freshness","call":"mcp_loader_runtime_status({})"},
            {"intent":"Observe live tools","call":"mcp_loader_site_tool_inventory_check({ site_root: \"<site_root>\" })"},
            {"intent":"Observe a generation","call":"mcp_loader_runtime_observation({ connection_id: \"<connection_id>\", carrier_kind: \"codex\" })"}
        ],
        "anti_patterns":[
            "Do not infer a Site or runtime from the current directory, process name, server name, or entrypoint path.",
            "Do not attach an undeclared surface or use an entrypoint outside the allowed policy prefixes.",
            "Do not reinterpret a surface_factory projection as stdio.",
            "Do not copy child inputSchema or outputSchema into loader guidance.",
            "Do not treat loader attachment as authorization for the child surface domain.",
            "Do not enumerate or terminate arbitrary host processes, conhost descendants, or processes lacking this loader run's ownership marker."
        ],
        "recovery":[
            "For unknown_tool, call tools/list and mcp_loader_guidance again after restart.",
            "For surface_runtime_required or surface_runtime_not_supported, inspect the declared projection and retry only with an explicit compatible runtime_kind.",
            "For surface_execution_adapter_not_supported_by_loader, route the admitted binding through the PC Site surface runtime.",
            "For child failures, inspect mcp_loader_surface_status and stderr evidence, then use mcp_loader_surface_restart when eligible.",
            "For max_connections_reached, inspect inventory and detach stale or closed connections.",
            "For stale loader runtime, use runtime_freshness.reload_action as a carrier/runtime-supervisor request."
        ],
        "feedback":{"surface_id":"mcp-loader","tool":"surface_feedback_submit","when":["guidance is missing, stale, or contradicted by live loader behavior","schema shape makes correct usage hard","errors hide the actionable refusal or recovery path"]},
        "boundaries":[
            "MCP Loader owns child attachment, initialization, tool discovery, call proxying, and detachment.",
            "MCP Loader does not own attached-surface domain policy, action admission, or child tool semantics.",
            "MCP Loader is the stdio compatibility adapter. It does not host surface factories or own authority-shared instances.",
            "The loader binds children to the requested Site root and does not let an ambient caller Site root override it.",
            "Process ownership is limited to direct children spawned by this loader run."
        ]
    })
}

fn observe_file(path: &str) -> Value {
    match metadata(path) {
        Ok(stat) => {
            let modified = stat
                .modified()
                .ok()
                .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
                .map(|duration| duration.as_millis());
            json!({"path":path,"exists":true,"mtime_ms":modified,"mtime":modified.map(ms_to_iso)})
        }
        Err(_) => json!({"path":path,"exists":false,"mtime_ms":Value::Null,"mtime":Value::Null}),
    }
}

fn ms_to_iso(milliseconds: u128) -> String {
    OffsetDateTime::from_unix_timestamp_nanos((milliseconds.saturating_mul(1_000_000)) as i128)
        .ok()
        .and_then(|date| date.format(&Rfc3339).ok())
        .unwrap_or_else(|| "1970-01-01T00:00:00Z".to_string())
}

fn runtime_freshness(state: &LoaderState) -> Value {
    let mut reload_action = supervisor_restart_action();
    reload_action["guidance"] = json!("Restart the mcp-loader process through its carrier or runtime supervisor to load rebuilt loader code. mcp_loader_surface_restart replaces only an attached child and does not reload the mcp-loader process.");
    let loader_source = join_path(&state.workspace_root, "packages/mcp-loader-mcp/src/main.ts");

    let runtime_entrypoint = env::current_exe()
        .ok()
        .map(|path| normalize_path(&path.to_string_lossy()))
        .unwrap_or_else(|| "narada-mcp-loader".to_string());
    let pairs = vec![
        (
            "loader_entrypoint",
            loader_source.clone(),
            runtime_entrypoint.clone(),
        ),
        (
            "loader_guidance",
            join_path(
                &state.workspace_root,
                "packages/mcp-loader-mcp/src/guidance.ts",
            ),
            join_path(
                &state.workspace_root,
                "packages/mcp-loader-mcp/dist/src/guidance.js",
            ),
        ),
        (
            "loader_runtime_lifecycle",
            join_path(
                &state.workspace_root,
                "packages/mcp-loader-mcp/src/runtime-lifecycle.ts",
            ),
            join_path(
                &state.workspace_root,
                "packages/mcp-loader-mcp/dist/src/runtime-lifecycle.js",
            ),
        ),
        (
            "loader_tool_timeout",
            join_path(
                &state.workspace_root,
                "packages/mcp-loader-mcp/src/tool-timeout.ts",
            ),
            join_path(
                &state.workspace_root,
                "packages/mcp-loader-mcp/dist/src/tool-timeout.js",
            ),
        ),
        (
            "mcp_transport",
            join_path(
                &state.workspace_root,
                "packages/shared/mcp-transport/src/mcp-payload-file.ts",
            ),
            join_path(
                &state.workspace_root,
                "packages/shared/mcp-transport/dist/src/mcp-payload-file.js",
            ),
        ),
    ];
    let config_files = vec![
        (
            "workspace_package",
            join_path(&state.workspace_root, "package.json"),
        ),
        (
            "workspace_lockfile",
            join_path(&state.workspace_root, "pnpm-lock.yaml"),
        ),
        (
            "workspace_typescript_config",
            join_path(&state.workspace_root, "tsconfig.base.json"),
        ),
        (
            "loader_package",
            join_path(
                &state.workspace_root,
                "packages/mcp-loader-mcp/package.json",
            ),
        ),
        (
            "loader_typescript_config",
            join_path(
                &state.workspace_root,
                "packages/mcp-loader-mcp/tsconfig.json",
            ),
        ),
        (
            "mcp_transport_package",
            join_path(
                &state.workspace_root,
                "packages/shared/mcp-transport/package.json",
            ),
        ),
    ];
    let mut reasons = Vec::new();
    let cutoff = state.started_ms + FILE_MTIME_CLOCK_SKEW_MS;
    let mut file_pairs = Vec::new();
    let mut newest_runtime = 0_u128;
    for (name, source, runtime) in &pairs {
        let source_obs = observe_file(source);
        let runtime_obs = observe_file(runtime);
        let source_exists = source_obs
            .get("exists")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let runtime_exists = runtime_obs
            .get("exists")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        if !runtime_exists {
            reasons.push(format!("runtime_file_unavailable:{}", name));
        }
        if !source_exists {
            reasons.push(format!("source_file_unavailable:{}", name));
        }
        let source_mtime = source_obs
            .get("mtime_ms")
            .and_then(Value::as_u64)
            .map(u128::from);
        let runtime_mtime = runtime_obs
            .get("mtime_ms")
            .and_then(Value::as_u64)
            .map(u128::from);
        if runtime_mtime.is_some_and(|mtime| mtime > cutoff) {
            reasons.push(format!("runtime_file_changed_after_process_start:{}", name));
        }
        if source_mtime.is_some_and(|mtime| mtime > cutoff) {
            reasons.push(format!("source_file_changed_after_process_start:{}", name));
        }
        if source_mtime
            .zip(runtime_mtime)
            .is_some_and(|(source, runtime)| source > runtime)
        {
            reasons.push(format!("source_file_newer_than_runtime_file:{}", name));
        }
        if let Some(mtime) = runtime_mtime {
            newest_runtime = newest_runtime.max(mtime);
        }
        file_pairs.push(json!({"name":name,"source":source_obs,"runtime":runtime_obs}));
    }
    let mut config_observations = Vec::new();
    for (name, path) in &config_files {
        let observation = observe_file(path);
        if !observation
            .get("exists")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        {
            reasons.push(format!("config_file_unavailable:{}", name));
        } else if let Some(mtime) = observation
            .get("mtime_ms")
            .and_then(Value::as_u64)
            .map(u128::from)
        {
            if mtime > cutoff {
                reasons.push(format!("config_file_changed_after_process_start:{}", name));
            }
            if mtime > newest_runtime {
                reasons.push(format!("config_file_newer_than_runtime_files:{}", name));
            }
        }
        config_observations.push(json!({"name":name,"observation":observation}));
    }
    let status = if reasons.iter().any(|reason| reason.contains("unavailable")) {
        "unknown"
    } else if reasons.is_empty() {
        "current"
    } else {
        "stale"
    };
    let entrypoint = file_pairs
        .iter()
        .find(|pair| pair.get("name").and_then(Value::as_str) == Some("loader_entrypoint"))
        .cloned()
        .unwrap_or_else(|| json!({"source":null,"runtime":null}));
    let source_files: Vec<Value> = file_pairs
        .iter()
        .map(|pair| {
            let mut value = json!({"name":pair["name"]});
            value["observation"] = pair["source"].clone();
            value
        })
        .collect();
    let runtime_files: Vec<Value> = file_pairs
        .iter()
        .map(|pair| {
            let mut value = json!({"name":pair["name"]});
            value["observation"] = pair["runtime"].clone();
            value
        })
        .collect();
    let dependencies: Vec<Value> = file_pairs
        .iter()
        .filter(|pair| pair.get("name").and_then(Value::as_str) != Some("loader_entrypoint"))
        .map(|pair| json!({"name":pair["name"],"source":pair["source"],"runtime":pair["runtime"]}))
        .collect();
    json!({
        "schema":"narada.mcp_loader.runtime_freshness.v1",
        "status":status,
        "reload_required":if status=="stale" {Value::Bool(true)} else if status=="current" {Value::Bool(false)} else {Value::Null},
        "process_started_at":ms_to_iso(state.started_ms),
        "process_started_at_ms":state.started_ms,
        "freshness_scope":"loader_source_runtime_dependencies_and_build_configuration",
        "runtime_entrypoint":entrypoint.get("runtime").cloned().unwrap_or(Value::Null),
        "source_entrypoint":entrypoint.get("source").cloned().unwrap_or(Value::Null),
        "source_files":source_files,
        "runtime_files":runtime_files,
        "dependency_files":dependencies,
        "config_files":config_observations,
        "tracked_file_count":file_pairs.len()*2+config_files.len(),
        "reasons":reasons,
        "reload_action":reload_action
    })
}

fn stable_value(value: &Value) -> Value {
    match value {
        Value::Object(object) => {
            let mut entries: Vec<_> = object.iter().collect();
            entries.sort_by(|left, right| left.0.cmp(right.0));
            let mut output = Map::new();
            for (key, value) in entries {
                output.insert(key.clone(), stable_value(value));
            }
            Value::Object(output)
        }
        Value::Array(values) => Value::Array(values.iter().map(stable_value).collect()),
        _ => value.clone(),
    }
}

fn stable_json(value: &Value) -> String {
    serde_json::to_string(&stable_value(value)).unwrap_or_else(|_| "null".to_string())
}

fn pretty_json(value: &Value) -> String {
    serde_json::to_string_pretty(value).unwrap_or_else(|_| "null".to_string())
}

fn sha256(value: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(value.as_bytes());
    hex::encode(hasher.finalize())
}

fn utf16_len(value: &str) -> usize {
    value.encode_utf16().count()
}

fn json_byte_len(value: &Value) -> usize {
    serde_json::to_vec(value)
        .map(|bytes| bytes.len())
        .unwrap_or(usize::MAX)
}

fn connection_ownership(connection: &Connection) -> Value {
    json!({
        "owner":"mcp-loader","owner_run_id":connection.owner_run_id,"owner_pid":connection.owner_pid,
        "parent_pid":connection.parent_pid,"ownership_marker":connection.ownership_marker,
        "cleanup_scope":"loader_owned_child_only"
    })
}

fn connection_live(connection: &Connection) -> bool {
    !connection.detached && connection.session.alive()
}

fn touch_connection(connection: &mut Connection) {
    connection.heartbeat_ms = now_ms();
    connection.lease_expires_ms = connection.heartbeat_ms + DEFAULT_RUNTIME_LEASE_MS as u128;
}

fn lifecycle_mode(connection: &Connection) -> Option<&str> {
    connection.lifecycle.get("mode").and_then(Value::as_str)
}

fn connection_recovery_actions(connection: &Connection) -> Vec<Value> {
    if lifecycle_mode(connection) != Some("replayable") {
        vec![json!({
            "actuator":"carrier-supervisor","tool_name":Value::Null,
            "arguments":{"connection_id":connection.connection_id,"logical_connection_id":connection.logical_connection_id,"capability":"restart_mcp_loader_process"},
            "guidance":"This projection is not loader-replayable. Ask the carrier supervisor to invoke restart_mcp_loader_process for the attached MCP loader before reconnecting the session."
        })]
    } else {
        vec![json!({
            "actuator":"mcp-loader","tool_name":"mcp_loader_surface_restart",
            "arguments":{"connection_id":connection.connection_id},
            "guidance":"Invoke mcp_loader_surface_restart with the connection_id to replace this child generation; this does not restart the agent session or loader process."
        })]
    }
}

fn runtime_generation(connection: &Connection, observed_at_ms: u128) -> Value {
    let fresh = connection.lease_expires_ms > observed_at_ms;
    json!({
        "generation_id":connection.generation_id,"state":"active","started_at":ms_to_iso(connection.attached_ms),
        "activated_at":ms_to_iso(connection.attached_ms),"heartbeat_at":ms_to_iso(connection.heartbeat_ms),
        "lease_expires_at":ms_to_iso(connection.lease_expires_ms),
        "freshness":if fresh {"current"} else {"stale"},
        "health":if connection_live(connection) {"healthy"} else {"unreachable"},
        "descriptor_digest":connection.descriptor_digest,
        "tool_contract_digest":connection.tool_contract_digest,
        "inflight":connection.session.pending.lock().map(|pending| pending.len()).unwrap_or(0)
    })
}

fn connection_status(connection: &Connection, state: &LoaderState) -> Value {
    let live = connection_live(connection);
    let value = json!({
        "connection_id":connection.connection_id,"ownership":connection_ownership(connection),
        "logical_connection_id":connection.logical_connection_id,"generation_id":connection.generation_id,
        "site_root":connection.site_root,"surface_id":connection.surface_id,"server_name":connection.server_name,
        "projection_id":connection.projection_id,"execution":connection.execution,
        "runtime_kind":connection.runtime_kind,"runtime_requirements":connection.runtime_requirements,
        "lifecycle":connection.lifecycle,
        "runtime_lifecycle":runtime_lifecycle(Some(&connection.connection_id),Some(&connection.lifecycle)),
        "runtime_freshness":runtime_freshness(state),"runtime_command":connection.runtime_command,"entrypoint":connection.entrypoint,"args":connection.args,"child_invocation_kind":connection.child_invocation_kind,
        "status":if live {"live"} else {"closed"},"detached":connection.detached,"initialized":connection.initialized,
        "pid":connection.session.pid,"exit_code":connection.session.exit_code(),"signal_code":connection.session.signal_code(),
        "killed":connection.session.killed(),"pending_count":connection.session.pending.lock().map(|pending| pending.len()).unwrap_or(0),
        "attached_at":ms_to_iso(connection.attached_ms),"detached_at":connection.detached_ms.map(ms_to_iso),
        "stderr_tail":connection.session.stderr_tail(),"server_info":connection.server_info,
        "tool_count":connection.tools.len(),"descriptor_digest":connection.descriptor_digest,
        "declared_tool_contract_digest":connection.declared_tool_contract_digest,"tool_contract_digest":connection.tool_contract_digest,
        "heartbeat_at":ms_to_iso(connection.heartbeat_ms),"lease_expires_at":ms_to_iso(connection.lease_expires_ms),
        "active_generation":if live {runtime_generation(connection,now_ms())} else {Value::Null},
        "draining_generations":[],"recovery_actions":connection_recovery_actions(connection)
    });
    value
}

fn observed_tool_digest(tools: &[Value], _descriptor: Option<&Value>) -> Option<String> {
    let mut canonical = Vec::new();
    for tool in tools {
        let Some(object) = tool.as_object() else {
            continue;
        };
        if object.get("name").and_then(Value::as_str) == Some(RUNTIME_PROXY_STATUS_TOOL_NAME) {
            continue;
        }
        let mut entry = Map::new();
        entry.insert(
            "name".to_string(),
            object.get("name").cloned().unwrap_or(Value::Null),
        );
        entry.insert(
            "description".to_string(),
            object.get("description").cloned().unwrap_or(Value::Null),
        );
        entry.insert(
            "input_schema".to_string(),
            object
                .get("inputSchema")
                .or_else(|| object.get("input_schema"))
                .cloned()
                .unwrap_or_else(|| json!({})),
        );
        entry.insert(
            "output_schema".to_string(),
            object
                .get("outputSchema")
                .or_else(|| object.get("output_schema"))
                .cloned()
                .unwrap_or(Value::Null),
        );
        entry.insert(
            "annotations".to_string(),
            object.get("annotations").cloned().unwrap_or(Value::Null),
        );
        canonical.push(Value::Object(entry));
    }
    if canonical.is_empty() {
        None
    } else {
        canonical.sort_by(|left, right| {
            left.get("name")
                .and_then(Value::as_str)
                .cmp(&right.get("name").and_then(Value::as_str))
        });
        Some(sha256(&stable_json(&Value::Array(canonical))))
    }
}

fn attach_surface(arguments: &JsonObject, state: &mut LoaderState) -> Result<Value, Diagnostic> {
    let explicit_entrypoint = value_string(arguments.get("entrypoint"));
    let site_root = value_string(arguments.get("site_root")).map(|value| normalize_path(&value));
    let standalone =
        state.standalone_ambient_attachment && site_root.is_none() && explicit_entrypoint.is_some();
    let site_root = site_root.unwrap_or_else(|| normalize_path("."));
    let binding_id = value_string(arguments.get("binding_id"));
    let admitted = if state.binding_admission.is_some() {
        let id = binding_id
            .as_deref()
            .ok_or_else(|| Diagnostic::new("missing_binding_id", "missing_binding_id"))?;
        admitted_binding(state, &site_root, id, "attach")?
    } else {
        assert_binding_admission_available(state)?;
        None
    };
    let surface_id = admitted
        .as_ref()
        .and_then(|(entry, _)| {
            entry
                .get("surface_id")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned)
        })
        .or_else(|| value_string(arguments.get("surface_id")))
        .unwrap_or_else(|| "native-loader-child".to_string());
    if admitted.is_some()
        && value_string(arguments.get("surface_id")).is_some_and(|asserted| asserted != surface_id)
    {
        return Err(Diagnostic::new(
            "mcp_binding_surface_assertion_mismatch",
            format!(
                "mcp_binding_surface_assertion_mismatch:{}",
                binding_id.clone().unwrap_or_default()
            ),
        ));
    }
    if !standalone {
        ensure_site_root_allowed(&site_root, &state.policy)?;
        ensure_surface_allowed(&surface_id, &site_root, &state.policy, state)?;
    }
    let runtime_kind = value_string(arguments.get("runtime_kind"));
    let (
        server_name,
        projection_id,
        execution,
        lifecycle,
        descriptor,
        descriptor_digest,
        declared_digest,
        runtime_requirements,
    ) = if standalone {
        (
            surface_id.clone(),
            "default".to_string(),
            json!({"adapter":"stdio","tenancy":"session_isolated","replacement":"manual"}),
            json!({"mode":"replayable"}),
            None,
            None,
            None,
            Vec::new(),
        )
    } else {
        runtime_metadata(&site_root, &surface_id)?
    };
    if !runtime_matches(&runtime_requirements, runtime_kind.as_deref()) {
        if runtime_kind.is_none() {
            return Err(Diagnostic::new(
                "surface_runtime_required",
                format!("surface_runtime_required:{}", surface_id),
            )
            .with_details(
                json!({"surface_id":surface_id,"runtime_requirements":runtime_requirements}),
            ));
        }
        return Err(Diagnostic::new("surface_runtime_not_supported", format!("surface_runtime_not_supported:{}:{}", surface_id, runtime_kind.clone().unwrap_or_default()))
            .with_details(json!({"surface_id":surface_id,"runtime_kind":runtime_kind,"runtime_requirements":runtime_requirements})));
    }
    if execution
        .get("adapter")
        .and_then(Value::as_str)
        .unwrap_or("stdio")
        != "stdio"
    {
        return Err(Diagnostic::new("surface_execution_adapter_not_supported_by_loader", format!("surface_execution_adapter_not_supported_by_loader:{}:{}", surface_id, execution.get("adapter").and_then(Value::as_str).unwrap_or_default()))
            .with_details(json!({"surface_id":surface_id,"projection_id":projection_id,"execution":execution,"responsible_actuator":"pc_site_surface_runtime","remediation":"Route this admitted binding through the PC Site surface runtime; mcp-loader remains the stdio compatibility adapter."})));
    }
    if state.connections.len() >= state.policy.max_connections {
        let inventory = connection_inventory(state);
        return Err(Diagnostic::new("max_connections_reached", format!("max_connections_reached:{}", state.connections.len()))
            .with_details(json!({"max_connections":inventory["max_connections"],"connection_count":inventory["connection_count"],"available_slots":inventory["available_slots"],"closed_connection_ids":inventory["closed_connection_ids"],"recovery":inventory["recovery"]})));
    }
    let extra_args = string_array(arguments.get("args"))?.unwrap_or_default();
    if admitted.is_some() && (explicit_entrypoint.is_some() || !extra_args.is_empty()) {
        return Err(Diagnostic::new(
            "mcp_binding_invocation_override_not_allowed",
            format!(
                "mcp_binding_invocation_override_not_allowed:{}",
                binding_id.clone().unwrap_or_default()
            ),
        )
        .with_details(json!({"child_spawned":false})));
    }
    if explicit_entrypoint.is_none() && !extra_args.is_empty() {
        return Err(Diagnostic::new(
            "site_fabric_invocation_override_not_allowed",
            format!("site_fabric_invocation_override_not_allowed:{}", surface_id),
        )
        .with_details(json!({
            "surface_id": surface_id,
            "remediation": "Change and rematerialize the authoritative Site fabric instead of supplying per-call arguments."
        })));
    }
    let (entrypoint, resolved_args, command, child_invocation_kind) = if let Some(explicit) =
        explicit_entrypoint.clone()
    {
        (
            normalize_path(&explicit),
            extra_args.clone(),
            value_string(arguments.get("child_command"))
                .or_else(|| state.options.child_command.clone())
                .unwrap_or_else(|| default_runtime_command()),
            "entrypoint".to_string(),
        )
    } else {
        let bundle = read_site_fabric(&site_root)?;
        let servers = bundle
            .fabric
            .get("mcpServers")
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();
        if let Some((_, server)) = find_site_server(&servers, &surface_id)? {
            let command = server
                .get("command")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            let raw_args = server
                .get("args")
                .and_then(Value::as_array)
                .map(|values| {
                    values
                        .iter()
                        .filter_map(|value| value.as_str().map(ToOwned::to_owned))
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            let is_proxy_wrapped = is_runtime_proxy_command(&command)
                && raw_args.first().map(String::as_str) == Some("proxy");
            if is_proxy_wrapped {
                let child_command = extract_proxy_child_command(&raw_args)
                    .map(|cmd| resolve_child_command(&cmd))
                    .ok_or_else(|| {
                        Diagnostic::new(
                            "surface_command_unsupported",
                            format!("surface_command_unsupported:runtime-proxy:{}", command),
                        )
                    })?;
                let child_entrypoint =
                    extract_proxy_child_entrypoint(&raw_args).ok_or_else(|| {
                        Diagnostic::new(
                            "surface_command_unsupported",
                            format!("surface_command_unsupported:runtime-proxy:{}", command),
                        )
                    })?;
                let child_invocation_kind = extract_proxy_child_invocation_kind(&raw_args);
                let child_args = extract_proxy_child_args(&raw_args).ok_or_else(|| {
                    Diagnostic::new(
                        "surface_command_unsupported",
                        format!("surface_command_unsupported:runtime-proxy:{}", command),
                    )
                })?;
                match child_invocation_kind.as_str() {
                    "entrypoint" => (
                        normalize_path(&child_entrypoint),
                        child_args.into_iter().chain(extra_args.clone()).collect(),
                        child_command,
                        child_invocation_kind,
                    ),
                    "native_entrypoint" => {
                        let native_entrypoint = normalize_path(&child_command);
                        (
                            native_entrypoint,
                            child_args.into_iter().chain(extra_args.clone()).collect(),
                            child_command,
                            child_invocation_kind,
                        )
                    }
                    "native_applet" => {
                        let child_applet =
                            extract_proxy_child_applet(&raw_args).ok_or_else(|| {
                                Diagnostic::new(
                                    "surface_native_child_unsupported",
                                    format!(
                                        "surface_native_child_unsupported:{}",
                                        child_invocation_kind
                                    ),
                                )
                            })?;
                        let native_entrypoint = normalize_path(&child_command);
                        let mut native_args = vec![child_applet];
                        native_args.extend(child_args);
                        native_args.extend(extra_args.clone());
                        (
                            native_entrypoint,
                            native_args,
                            child_command,
                            child_invocation_kind,
                        )
                    }
                    _ => {
                        return Err(Diagnostic::new(
                            "surface_native_child_unsupported",
                            format!("surface_native_child_unsupported:{}", child_invocation_kind),
                        )
                        .with_details(json!({
                            "surface_id": surface_id,
                            "child_command": child_command,
                            "child_entrypoint": child_entrypoint,
                        })));
                    }
                }
            } else {
                let declared =
                    extract_runtime_entrypoint(&command, &raw_args).ok_or_else(|| {
                        Diagnostic::new(
                            "surface_command_unsupported",
                            format!("surface_command_unsupported:{}:{}", surface_id, command),
                        )
                    })?;
                (
                    normalize_path(&declared),
                    remove_entrypoint_arg(&raw_args, &declared)
                        .into_iter()
                        .chain(extra_args.clone())
                        .collect(),
                    command,
                    "entrypoint".to_string(),
                )
            }
        } else if let Some((entrypoint, args)) =
            shared_surface_registry(&surface_id, &state.surface_root)
        {
            let args = args
                .into_iter()
                .map(|value| interpolate_site_arg(&value, &site_root))
                .collect::<Result<Vec<_>, _>>()?;
            (
                normalize_path(&entrypoint),
                args.into_iter().chain(extra_args.clone()).collect(),
                default_runtime_command(),
                "entrypoint".to_string(),
            )
        } else {
            return Err(Diagnostic::new(
                "surface_not_found",
                format!("surface_not_found:{}", surface_id),
            ));
        }
    };
    // A Site-fabric launch is admitted by its exact materialized declaration.
    // Prefix policy remains the authority for caller-supplied entrypoints only.
    if explicit_entrypoint.is_some() {
        ensure_entrypoint_allowed(&site_root, &entrypoint, &state.policy)?;
    }
    if !Path::new(&entrypoint).exists() {
        return Err(Diagnostic::new(
            "entrypoint_not_found",
            format!("entrypoint_not_found:{}", entrypoint),
        ));
    }
    let connection = open_connection(
        state,
        site_root,
        surface_id,
        runtime_kind,
        runtime_requirements,
        entrypoint,
        resolved_args,
        command,
        child_invocation_kind,
        explicit_entrypoint,
        extra_args,
        server_name,
        projection_id,
        execution,
        lifecycle,
        descriptor,
        descriptor_digest,
        declared_digest,
        admitted.as_ref().map(|(entry, _)| entry.clone()),
    )?;
    let id = connection.connection_id.clone();
    let response = attached_response(&connection, state);
    state.connections.insert(id, connection);
    Ok(response)
}

fn default_runtime_command() -> String {
    resolve_javascript_runtime()
}

fn resolve_javascript_runtime() -> String {
    let exec = env::current_exe().unwrap_or_default();
    let exec_base = exec
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    if ["node.exe", "node", "bun.exe", "bun", "deno.exe", "deno"].contains(&exec_base.as_str()) {
        return exec.to_string_lossy().to_string();
    }
    let home = env::var("USERPROFILE")
        .or_else(|_| env::var("HOME"))
        .unwrap_or_default();
    let bun_candidates = if cfg!(windows) {
        vec![
            format!("{}\\.bun\\bin\\bun.exe", home),
            format!("{}\\.bun\\bin\\bun", home),
        ]
    } else {
        vec![format!("{}/.bun/bin/bun", home)]
    };
    for candidate in &bun_candidates {
        if Path::new(candidate).is_file() {
            return candidate.clone();
        }
    }
    if cfg!(windows) {
        let program_files =
            env::var("PROGRAMFILES").unwrap_or_else(|_| "C:\\Program Files".to_string());
        let program_files_x86 =
            env::var("PROGRAMFILES(X86)").unwrap_or_else(|_| "C:\\Program Files (x86)".to_string());
        let node_candidates = [
            format!("{}\\nodejs\\node.exe", program_files),
            format!("{}\\nodejs\\node.exe", program_files_x86),
        ];
        for candidate in &node_candidates {
            if Path::new(candidate).is_file() {
                return candidate.clone();
            }
        }
    }
    let path_var = env::var("PATH")
        .or_else(|_| env::var("Path"))
        .or_else(|_| env::var("path"))
        .unwrap_or_default();
    let separator = if cfg!(windows) { ';' } else { ':' };
    let names = if cfg!(windows) {
        vec!["bun.exe", "bun", "node.exe", "node"]
    } else {
        vec!["bun", "node"]
    };
    for dir in path_var.split(separator) {
        let dir = dir.trim();
        if dir.is_empty() {
            continue;
        }
        for name in &names {
            let candidate = Path::new(dir).join(name);
            if candidate.is_file() {
                return candidate.to_string_lossy().to_string();
            }
        }
    }
    if cfg!(windows) {
        "node.exe".to_string()
    } else {
        "node".to_string()
    }
}

fn is_runtime_proxy_command(command: &str) -> bool {
    let base = command
        .replace('\\', "/")
        .rsplit('/')
        .next()
        .unwrap_or(command)
        .to_ascii_lowercase();
    base.contains("narada-mcp-runtime") || base.contains("mcp-runtime-proxy")
}

fn extract_proxy_child_command(args: &[String]) -> Option<String> {
    args.iter()
        .position(|arg| arg == "--child-command")
        .and_then(|idx| args.get(idx + 1))
        .cloned()
}

fn extract_proxy_child_entrypoint(args: &[String]) -> Option<String> {
    args.iter()
        .position(|arg| arg == "--entrypoint")
        .and_then(|idx| args.get(idx + 1))
        .cloned()
}

fn extract_proxy_child_invocation_kind(args: &[String]) -> String {
    args.iter()
        .position(|arg| arg == "--child-invocation-kind")
        .and_then(|idx| args.get(idx + 1))
        .cloned()
        .unwrap_or_else(|| "entrypoint".to_string())
}

fn extract_proxy_child_applet(args: &[String]) -> Option<String> {
    args.iter()
        .position(|arg| arg == "--child-applet")
        .and_then(|idx| args.get(idx + 1))
        .cloned()
}

fn extract_proxy_child_args(args: &[String]) -> Option<Vec<String>> {
    let mut child_args = Vec::new();
    let mut found_separator = false;
    for arg in args {
        if found_separator {
            child_args.push(arg.clone());
        } else if arg == "--" {
            found_separator = true;
        }
    }
    found_separator.then_some(child_args)
}

fn resolve_child_command(command: &str) -> String {
    let base = command
        .replace('\\', "/")
        .rsplit('/')
        .next()
        .unwrap_or(command)
        .to_ascii_lowercase();
    if ["bun", "bun.exe", "node", "node.exe"].contains(&base.as_str())
        && !Path::new(command).is_absolute()
    {
        resolve_javascript_runtime()
    } else {
        command.to_string()
    }
}

fn open_connection(
    state: &LoaderState,
    site_root: String,
    surface_id: String,
    runtime_kind: Option<String>,
    runtime_requirements: Vec<String>,
    entrypoint: String,
    resolved_args: Vec<String>,
    command: String,
    child_invocation_kind: String,
    requested_entrypoint: Option<String>,
    extra_args: Vec<String>,
    server_name: String,
    projection_id: String,
    execution: Value,
    lifecycle: Value,
    descriptor: Option<Value>,
    descriptor_digest: Option<String>,
    declared_digest: Option<String>,
    admitted_binding: Option<Value>,
) -> Result<Connection, Diagnostic> {
    let connection_id = new_id("connection");
    let logical_connection_id = connection_id.clone();
    let generation_id = new_id("generation");
    let owner_run_id = state.run_id.clone();
    let owner_pid = state.owner_pid;
    let ownership_marker = state.ownership_marker.clone();
    let child_spec = build_child_spec(
        &command,
        &entrypoint,
        &resolved_args,
        &child_invocation_kind,
    );
    let env_map = build_child_env(
        &site_root,
        &state.policy,
        &connection_id,
        &logical_connection_id,
        &generation_id,
        &ownership_marker,
    );
    let session = match ChildSession::spawn(child_spec, &env_map) {
        Ok(session) => session,
        Err(error) => return Err(error),
    };
    let (server_info, tools_result) = match session.request(
        "server/discover",
        modern_request_params(),
        state.policy.attach_timeout_ms,
    ) {
        Ok(discovery) if modern_discovery_is_valid(&discovery) => {
            let server_info = discovery
                .get("_meta")
                .and_then(|meta| meta.get("io.modelcontextprotocol/serverInfo"))
                .cloned()
                .unwrap_or_else(|| json!({}));
            let tools_result = match session.request(
                "tools/list",
                modern_request_params(),
                state.policy.attach_timeout_ms,
            ) {
                Ok(value) => value,
                Err(error) => {
                    session.terminate();
                    return Err(error.with_details(json!({
                        "connection_id":connection_id,
                        "surface_id":surface_id,
                        "entrypoint":entrypoint,
                        "args":resolved_args,
                        "exit_code":session.exit_code(),
                        "signal_code":session.signal_code(),
                        "stderr_tail":session.stderr_tail(),
                        "runtime_lifecycle":runtime_lifecycle(Some(&connection_id),Some(&lifecycle))
                    })));
                }
            };
            (server_info, tools_result)
        }
        Ok(_) | Err(_) => {
            let init = match session.request(
                    "initialize",
                    json!({"protocolVersion":PROTOCOL_VERSION,"capabilities":{},"clientInfo":{"name":SERVER_NAME,"version":SERVER_VERSION}}),
                    state.policy.attach_timeout_ms,
                ) {
                    Ok(value) => value,
                    Err(error) => {
                        session.terminate();
                        return Err(error.with_details(json!({
                            "connection_id":connection_id,
                            "surface_id":surface_id,
                            "entrypoint":entrypoint,
                            "args":resolved_args,
                            "exit_code":session.exit_code(),
                            "signal_code":session.signal_code(),
                            "stderr_tail":session.stderr_tail(),
                            "runtime_lifecycle":runtime_lifecycle(Some(&connection_id),Some(&lifecycle))
                        })));
                    }
                };
            if let Err(error) = session.notify("notifications/initialized", json!({})) {
                session.terminate();
                return Err(error);
            }
            let tools_result =
                match session.request("tools/list", json!({}), state.policy.attach_timeout_ms) {
                    Ok(value) => value,
                    Err(error) => {
                        session.terminate();
                        return Err(error.with_details(json!({
                        "connection_id":connection_id,
                        "surface_id":surface_id,
                        "entrypoint":entrypoint,
                        "args":resolved_args,
                        "exit_code":session.exit_code(),
                        "signal_code":session.signal_code(),
                        "stderr_tail":session.stderr_tail(),
                        "runtime_lifecycle":runtime_lifecycle(Some(&connection_id),Some(&lifecycle))
                    })));
                    }
                };
            (
                init.get("serverInfo").cloned().unwrap_or_else(|| json!({})),
                tools_result,
            )
        }
    };
    let tools = tools_result
        .get("tools")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let attached_ms = now_ms();
    let mut connection = Connection {
        session,
        connection_id,
        owner_run_id,
        owner_pid,
        parent_pid: owner_pid,
        ownership_marker,
        logical_connection_id,
        generation_id,
        server_name,
        projection_id,
        execution,
        lifecycle,
        descriptor,
        descriptor_digest,
        declared_tool_contract_digest: declared_digest,
        tool_contract_digest: observed_tool_digest(&tools, None),
        heartbeat_ms: attached_ms,
        lease_expires_ms: attached_ms + DEFAULT_RUNTIME_LEASE_MS as u128,
        site_root,
        surface_id,
        binding_id: admitted_binding
            .as_ref()
            .and_then(|entry| entry.get("binding_id"))
            .and_then(Value::as_str)
            .map(ToOwned::to_owned),
        admission_envelope_id: state
            .binding_admission
            .as_ref()
            .and_then(|value| value.get("envelope_id"))
            .and_then(Value::as_str)
            .map(ToOwned::to_owned),
        admitted_binding_digest: admitted_binding
            .as_ref()
            .and_then(|entry| entry.get("binding_digest"))
            .and_then(Value::as_str)
            .map(ToOwned::to_owned),
        authority_epoch: state
            .binding_admission
            .as_ref()
            .and_then(|value| value.get("authority_epoch"))
            .and_then(Value::as_u64),
        runtime_kind,
        runtime_requirements,
        runtime_command: command,
        entrypoint,
        args: resolved_args,
        child_invocation_kind,
        requested_entrypoint,
        extra_args,
        initialized: true,
        server_info,
        tools,
        detached: false,
        attached_ms,
        detached_ms: None,
    };
    touch_connection(&mut connection);
    Ok(connection)
}

fn build_child_spec(
    command: &str,
    entrypoint: &str,
    args: &[String],
    child_invocation_kind: &str,
) -> ChildSpec {
    let base = command
        .replace('\\', "/")
        .rsplit('/')
        .next()
        .unwrap_or(command)
        .to_ascii_lowercase();
    let mut child_args = Vec::new();
    if child_invocation_kind == "native_entrypoint" || child_invocation_kind == "native_applet" {
        child_args.extend(args.iter().cloned());
    } else if [
        "node", "node.exe", "node.cmd", "bun", "bun.exe", "deno", "deno.exe",
    ]
    .contains(&base.as_str())
    {
        child_args.push(entrypoint.to_string());
        child_args.extend(args.iter().cloned());
    } else if normalize_path(command) == normalize_path(entrypoint) {
        child_args.extend(args.iter().cloned());
    } else {
        child_args.push(entrypoint.to_string());
        child_args.extend(args.iter().cloned());
    }
    ChildSpec {
        command: command.to_string(),
        args: child_args,
    }
}

fn build_child_env(
    site_root: &str,
    policy: &Policy,
    connection_id: &str,
    logical_id: &str,
    generation_id: &str,
    marker: &str,
) -> HashMap<String, String> {
    let mut env_map = HashMap::new();
    for key in &policy.allowed_env_vars {
        if let Ok(value) = env::var(key) {
            env_map.insert(key.clone(), value);
        }
    }
    env_map.insert("NARADA_SITE_ROOT".to_string(), site_root.to_string());
    env_map.insert(
        "NARADA_MCP_LOADER_RUN_ID".to_string(),
        marker
            .strip_prefix("narada.mcp.loader/")
            .unwrap_or(marker)
            .to_string(),
    );
    env_map.insert(
        "NARADA_MCP_LOADER_CONNECTION_ID".to_string(),
        connection_id.to_string(),
    );
    env_map.insert(
        "NARADA_MCP_LOADER_LOGICAL_CONNECTION_ID".to_string(),
        logical_id.to_string(),
    );
    env_map.insert(
        "NARADA_MCP_LOADER_GENERATION_ID".to_string(),
        generation_id.to_string(),
    );
    env_map.insert(
        "NARADA_MCP_LOADER_OWNER_PID".to_string(),
        std::process::id().to_string(),
    );
    env_map.insert(
        "NARADA_MCP_LOADER_PARENT_PID".to_string(),
        std::process::id().to_string(),
    );
    env_map.insert(
        "NARADA_MCP_LOADER_OWNERSHIP_MARKER".to_string(),
        marker.to_string(),
    );
    env_map
}

fn attached_response(connection: &Connection, state: &LoaderState) -> Value {
    json!({
        "schema":"narada.mcp_loader.surface_attached.v1",
        "connection_id":connection.connection_id,"logical_connection_id":connection.logical_connection_id,
        "generation_id":connection.generation_id,"site_root":connection.site_root,"surface_id":connection.surface_id,
        "binding_id":connection.binding_id,"admission_envelope_id":connection.admission_envelope_id,
        "binding_digest":connection.admitted_binding_digest,"authority_epoch":connection.authority_epoch,
        "runtime_kind":connection.runtime_kind,"runtime_requirements":connection.runtime_requirements,
        "runtime_lifecycle":runtime_lifecycle(Some(&connection.connection_id),Some(&connection.lifecycle)),
        "runtime_freshness":runtime_freshness(state),"runtime_command":connection.runtime_command,"entrypoint":connection.entrypoint,"args":connection.args,"child_invocation_kind":connection.child_invocation_kind,
        "server_info":connection.server_info,"tools":connection.tools,"descriptor_digest":connection.descriptor_digest,
        "tool_contract_digest":connection.tool_contract_digest,"declared_tool_contract_digest":connection.declared_tool_contract_digest,
        "lifecycle":connection.lifecycle,"ownership":connection_ownership(connection)
    })
}

fn open_surface(arguments: &JsonObject, state: &mut LoaderState) -> Result<Value, Diagnostic> {
    let attached = attach_surface(arguments, state)?;
    let connection_id = attached
        .get("connection_id")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            Diagnostic::new(
                "surface_attach_missing_connection_id",
                "surface_attach_missing_connection_id",
            )
        })?
        .to_string();
    let connection = state
        .connections
        .get(&connection_id)
        .ok_or_else(|| Diagnostic::new("connection_not_found", &connection_id))?;
    let handle = format!("{}{}", SURFACE_HANDLE_PREFIX, new_id("h").replace('-', ""));
    let created_ms = now_ms();
    let record = SurfaceHandle {
        handle: handle.clone(),
        logical_connection_id: connection.logical_connection_id.clone(),
        site_root: connection.site_root.clone(),
        surface_id: connection.surface_id.clone(),
        runtime_kind: connection.runtime_kind.clone(),
        created_at: ms_to_iso(created_ms),
    };
    let created_at = record.created_at.clone();
    state.handles.insert(handle.clone(), record);
    Ok(json!({
        "schema":"narada.mcp_loader.surface_handle_opened.v1","status":"opened","surface_handle":handle,
        "handle_scope":"loader_process","handle_survives_child_restart":true,"handle_survives_loader_restart":false,
        "logical_connection_id":connection.logical_connection_id,"connection_id":connection.connection_id,
        "ownership":connection_ownership(connection),"generation_id":connection.generation_id,"site_root":connection.site_root,
        "surface_id":connection.surface_id,"runtime_kind":connection.runtime_kind,"runtime_requirements":connection.runtime_requirements,
        "runtime_lifecycle":runtime_lifecycle(Some(&connection.connection_id),Some(&connection.lifecycle)),
        "runtime_freshness":runtime_freshness(state),"tool_count":connection.tools.len(),"created_at":created_at,
        "call":{"tool_name":"mcp_loader_call_surface_tool","arguments":{"surface_handle":handle,"tool_name":"<child_tool>","arguments":{}}}
    }))
}

fn policy_inspect(state: &LoaderState) -> Value {
    let admission = state.binding_admission.as_ref().map(|envelope| json!({
        "status":"admitted","envelope_id":envelope.get("envelope_id"),"envelope_digest":envelope.get("envelope_digest"),
        "binding_count":envelope.get("bindings").and_then(Value::as_array).map(Vec::len).unwrap_or(0),
        "authority_epoch":envelope.get("authority_epoch"),"carrier_session_id":envelope.get("carrier_session_id")
    })).unwrap_or_else(|| json!({"status":if state.standalone_ambient_attachment{"standalone_ambient"}else{"required_missing"}}));
    json!({"schema":"narada.mcp_loader.policy.v1","binding_admission":admission,"policy":{
        "allowedSiteRoots":state.policy.allowed_site_roots,"allowedEntrypointPrefixes":state.policy.allowed_entrypoint_prefixes,
        "allowedSurfaceIds":state.policy.allowed_surface_ids.as_ref().map(|ids| json!(ids)).unwrap_or_else(|| json!("site_fabric")),
        "allowedEnvVars":state.policy.allowed_env_vars,"maxConnections":state.policy.max_connections,
        "maxRequestBytes":state.policy.max_request_bytes,"maxResponseBytes":state.policy.max_response_bytes,
        "attachTimeoutMs":state.policy.attach_timeout_ms,"toolCallTimeoutMs":state.policy.tool_call_timeout_ms,
        "toolCallGraceMs":state.policy.tool_call_grace_ms
    }})
}

fn connection_inventory(state: &LoaderState) -> Value {
    let mut connections = state.connections.values().collect::<Vec<_>>();
    connections.sort_by(|left, right| left.connection_id.cmp(&right.connection_id));
    let entries: Vec<Value> = connections.iter().map(|connection| {
        let status = connection_status(connection, state);
        let live = connection_live(connection);
        let mut entry = status.as_object().cloned().unwrap_or_default();
        entry.insert("connection_id".to_string(), json!(connection.connection_id));
        entry.insert("liveness".to_string(), json!(if live {"live"} else {"closed"}));
        entry.insert("age_ms".to_string(), json!(now_ms().saturating_sub(connection.attached_ms)));
        entry.insert("runtime_lifecycle".to_string(), runtime_lifecycle(Some(&connection.connection_id), Some(&connection.lifecycle)));
        entry.insert("recovery_actions".to_string(), json!({
            "inspect":{"tool_name":"mcp_loader_surface_status","arguments":{"connection_id":connection.connection_id}},
            "detach":{"tool_name":"mcp_loader_detach","arguments":{"connection_id":connection.connection_id}},
            "restart":connection_recovery_actions(connection).first().cloned().unwrap_or(Value::Null),
            "ownership":{"tool_name":"mcp_loader_process_ownership","arguments":{}}
        }));
        Value::Object(entry)
    }).collect();
    let live_ids: Vec<String> = connections
        .iter()
        .filter(|connection| connection_live(connection))
        .map(|connection| connection.connection_id.clone())
        .collect();
    let closed_ids: Vec<String> = connections
        .iter()
        .filter(|connection| !connection_live(connection))
        .map(|connection| connection.connection_id.clone())
        .collect();
    json!({
        "schema":"narada.mcp_loader.connection_inventory.v1","status":"ok","runtime_freshness":runtime_freshness(state),
        "max_connections":state.policy.max_connections,"connection_count":entries.len(),
        "available_slots":state.policy.max_connections.saturating_sub(entries.len()),"live_count":live_ids.len(),"closed_count":closed_ids.len(),
        "live_connection_ids":live_ids,"closed_connection_ids":closed_ids,"connections":entries,
        "recovery":{
            "when_full":"Inspect this inventory, then detach closed or no-longer-needed connections. Use surface restart only for an intentionally live replacement.",
            "inspect_tool":"mcp_loader_surface_status","detach_tool":"mcp_loader_detach","restart_tool":"mcp_loader_surface_restart",
            "ownership_tool":"mcp_loader_process_ownership","note":"The inventory is read-only and does not reap children or free slots automatically."
        }
    })
}

fn process_ownership(state: &LoaderState) -> Value {
    let mut processes = Vec::new();
    for connection in state.connections.values() {
        let live = connection_live(connection);
        let mut entry = connection_ownership(connection)
            .as_object()
            .cloned()
            .unwrap_or_default();
        entry.insert("connection_id".to_string(), json!(connection.connection_id));
        entry.insert(
            "logical_connection_id".to_string(),
            json!(connection.logical_connection_id),
        );
        entry.insert("generation_id".to_string(), json!(connection.generation_id));
        entry.insert("pid".to_string(), json!(connection.session.pid));
        entry.insert(
            "status".to_string(),
            json!(if live { "live" } else { "closed" }),
        );
        entry.insert("ownership_status".to_string(), json!("loader_owned"));
        entry.insert(
            "descendant_scope".to_string(),
            json!("direct_child_process_only"),
        );
        entry.insert("cleanup".to_string(), if live {
            json!({"status":"not_eligible","action":{"tool_name":"mcp_loader_detach","arguments":{"connection_id":connection.connection_id}}})
        } else {
            json!({"status":"safe_to_reconcile","action":{"tool_name":"mcp_loader_detach","arguments":{"connection_id":connection.connection_id}}})
        });
        processes.push(Value::Object(entry));
    }
    processes.sort_by(|left, right| {
        left.get("connection_id")
            .and_then(Value::as_str)
            .cmp(&right.get("connection_id").and_then(Value::as_str))
    });
    let safe_closed: Vec<Value> = processes
        .iter()
        .filter(|process| process.get("status").and_then(Value::as_str) == Some("closed"))
        .filter_map(|process| process.get("connection_id").cloned())
        .collect();
    json!({
        "schema":"narada.mcp_loader.process_ownership.v1","status":"ok",
        "loader":{"run_id":state.run_id,"pid":state.owner_pid,"ownership_marker":state.ownership_marker,"started_at":ms_to_iso(state.started_ms)},
        "scope":"known_direct_children_spawned_by_this_loader_run","processes":processes,
        "safe_reconciliation_connection_ids":safe_closed,
        "external_process_policy":"unowned_or_unobserved_processes_are_not_enumerated_or_terminated",
        "host_process_reconciliation":{"status":"not_available","reason":"mcp-loader has no authority to enumerate arbitrary host processes or conhost descendants","conhost_descendants":"not_enumerated","remediation":"Use the host/runtime supervisor for external process inspection; use mcp_loader_detach for a known loader-owned connection."}
    })
}

fn list_site_surfaces(arguments: &JsonObject, state: &LoaderState) -> Result<Value, Diagnostic> {
    let site_root = normalize_path(&required_string(
        arguments,
        "site_root",
        "missing_site_root",
    )?);
    ensure_site_root_allowed(&site_root, &state.policy)?;
    assert_binding_admission_available(state)?;
    let bundle = read_site_fabric(&site_root)?;
    let servers = bundle
        .fabric
        .get("mcpServers")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    let mut surfaces = Vec::new();
    for (server_id, server) in servers {
        let binding_id = server
            .get("binding_id")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if let Some(envelope) = &state.binding_admission {
            let discoverable = envelope
                .get("bindings")
                .and_then(Value::as_array)
                .is_some_and(|bindings| {
                    bindings.iter().any(|binding| {
                        binding.get("binding_id").and_then(Value::as_str) == Some(binding_id)
                            && binding
                                .get("operations")
                                .and_then(Value::as_array)
                                .is_some_and(|ops| {
                                    ops.iter().any(|op| op.as_str() == Some("discover"))
                                })
                    })
                });
            if !discoverable {
                continue;
            }
        }
        let surface_id = server
            .get("surface_id")
            .and_then(Value::as_str)
            .unwrap_or(&server_id)
            .to_string();
        let env_vars: Vec<String> = server
            .get("env")
            .and_then(Value::as_object)
            .map(|object| object.keys().cloned().collect())
            .unwrap_or_default();
        let requirements = surface_requirements(Some(&server));
        surfaces.push(json!({
            "binding_id":if binding_id.is_empty(){Value::Null}else{json!(binding_id)},"surface_id":surface_id,"server_name":server_id,"command":server.get("command").cloned().unwrap_or(Value::Null),
            "args":server.get("args").cloned().unwrap_or_else(|| json!([])),"env_vars":env_vars,
            "runtime_requirements":requirements,"runtime_lifecycle":runtime_lifecycle(None,None)
        }));
    }
    surfaces.sort_by(|left, right| {
        left.get("surface_id")
            .and_then(Value::as_str)
            .cmp(&right.get("surface_id").and_then(Value::as_str))
    });
    Ok(
        json!({"schema":"narada.mcp_loader.site_surfaces.v1","site_root":site_root,"runtime_freshness":runtime_freshness(state),"surfaces":surfaces}),
    )
}

fn classify_fabric_entrypoint(
    site_root: &str,
    declared: Option<&str>,
    expected: Option<&str>,
    exists: bool,
) -> (&'static str, Vec<String>) {
    let Some(declared) = declared else {
        return ("entrypoint_unresolved", vec!["Inspect the site fabric command and args; mcp-loader could not determine the JavaScript entrypoint.".to_string()]);
    };
    if !exists {
        return ("stale_entrypoint", vec!["Repair or regenerate the site MCP fabric so the declared entrypoint exists before attach.".to_string()]);
    }
    if expected.is_some_and(|value| value == declared) {
        return ("matches_shared_registry", Vec::new());
    }
    if is_under_path(declared, site_root) {
        return (if expected.is_some() {"site_local_override"} else {"site_local_surface"}, vec!["Treat this as site-local authority; compare expected tools before replacing it with the shared registry entrypoint.".to_string()]);
    }
    if expected.is_some() {
        return ("external_entrypoint_override", vec!["Classify as intentional override or drift at the fabric generator/registrar layer before local repair. Compare tool counts and authority implications against the shared registry entrypoint.".to_string()]);
    }
    (
        "external_site_declared_surface",
        vec![
            "Verify the external entrypoint authority and allowed-entrypoint policy before attach."
                .to_string(),
        ],
    )
}

fn site_fabric_diagnostics(
    arguments: &JsonObject,
    state: &LoaderState,
) -> Result<Value, Diagnostic> {
    let site_root = normalize_path(&required_string(
        arguments,
        "site_root",
        "missing_site_root",
    )?);
    ensure_site_root_allowed(&site_root, &state.policy)?;
    let bundle = read_site_fabric(&site_root)?;
    let servers = bundle
        .fabric
        .get("mcpServers")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    let mut diagnostics = Vec::new();
    for (surface_id, server) in &servers {
        let config_path = bundle
            .source_by_surface
            .get(surface_id)
            .cloned()
            .or_else(|| bundle.paths.first().cloned())
            .unwrap_or_default();
        let command = server
            .get("command")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let raw_args: Vec<String> = server
            .get("args")
            .and_then(Value::as_array)
            .map(|values| {
                values
                    .iter()
                    .filter_map(|value| value.as_str().map(ToOwned::to_owned))
                    .collect()
            })
            .unwrap_or_default();
        let declared =
            extract_runtime_entrypoint(&command, &raw_args).map(|value| normalize_path(&value));
        let expected = shared_surface_registry(surface_id, &state.surface_root)
            .map(|(entrypoint, _)| normalize_path(&entrypoint));
        let exists = declared
            .as_ref()
            .is_some_and(|value| Path::new(value).exists());
        let (classification, remediation) = classify_fabric_entrypoint(
            &site_root,
            declared.as_deref(),
            expected.as_deref(),
            exists,
        );
        diagnostics.push(json!({
            "surface_id":surface_id,"source":"site_fabric","config_path":config_path,"command":command,"args":raw_args,
            "declared_entrypoint":declared,"shared_registry_entrypoint":expected,"entrypoint_exists":exists,
            "classification":classification,
            "durability":{"local_repair_durable":"unknown","reason":"mcp-loader reads site fabric but does not own the generator or VCS ignore rules for this config."},
            "provenance":{"config_source":config_path,"shared_registry_source":if expected.is_some() {json!("@narada-core/mcp-loader-mcp embedded registry")} else {Value::Null},"generator":server.get("generated_by").cloned().unwrap_or(Value::Null),"generated_at":server.get("generated_at").cloned().unwrap_or(Value::Null),"tracking_state":"unknown","tracking_state_reason":"VCS tracking and ignore state are outside mcp-loader authority."},
            "remediation":remediation
        }));
    }
    let mut fallbacks = Vec::new();
    for known in [
        "operator-console-overlay",
        "local-filesystem",
        "structured-command",
        "git",
        "site-inbox",
        "mailbox",
        "graph-mail",
        "calendar",
        "task-lifecycle",
        "site-loop",
        "agent-context",
        "catalog-observation",
        "runtime-introspection",
        "worker-delegation",
        "delegated-task",
        "sop",
        "scheduler",
        "mcp-registrar",
        "surface-feedback",
        "speech",
        "cloudflare-carrier",
        "site-coherence",
        "site-lifecycle",
        "artifacts",
        "epistemic-graph",
        "nars-session",
        "quota-meter",
    ] {
        if !servers.contains_key(known) {
            if let Some((entrypoint, _)) = shared_surface_registry(known, &state.surface_root) {
                fallbacks.push(json!({"surface_id":known,"source":"shared_registry_fallback","shared_registry_entrypoint":normalize_path(&entrypoint),"classification":"registry_fallback_available","provenance":{"shared_registry_source":"@narada-core/mcp-loader-mcp embedded registry"}}));
            }
        }
    }
    Ok(
        json!({"schema":"narada.mcp_loader.site_fabric_diagnostics.v1","site_root":site_root,"config_path":if bundle.paths.len()==1 {bundle.paths.first().cloned().map(Value::String).unwrap_or(Value::Null)} else {Value::Null},"config_paths":bundle.paths,"config_exists":true,"diagnostics":diagnostics,"shared_registry_fallbacks":fallbacks}),
    )
}

fn surface_handle_inventory(state: &LoaderState) -> Value {
    let mut handles = Vec::new();
    for handle in state.handles.values() {
        let connection = find_connection_for_handle(handle, state);
        handles.push(json!({
            "surface_handle":handle.handle,"handle_scope":"loader_process","logical_connection_id":handle.logical_connection_id,
            "site_root":handle.site_root,"surface_id":handle.surface_id,"runtime_kind":handle.runtime_kind,
            "created_at":handle.created_at,"connection_id":connection.as_ref().map(|value| value.connection_id.clone()),
            "generation_id":connection.as_ref().map(|value| value.generation_id.clone()),
            "status":if connection.as_ref().is_some_and(|value| connection_live(value)) {"live"} else {"unavailable"},
            "recovery":if let Some(connection) = connection {
                json!({"tool_name":"mcp_loader_surface_restart","arguments":{"connection_id":connection.connection_id}})
            } else {
                json!({"tool_name":"mcp_loader_open_surface","arguments":{"site_root":handle.site_root,"surface_id":handle.surface_id,"runtime_kind":handle.runtime_kind}})
            }
        }));
    }
    handles.sort_by(|left, right| {
        left.get("surface_handle")
            .and_then(Value::as_str)
            .cmp(&right.get("surface_handle").and_then(Value::as_str))
    });
    json!({"schema":"narada.mcp_loader.surface_handle_inventory.v1","status":"ok","handle_scope":"loader_process","handle_count":handles.len(),"handles":handles})
}

fn find_connection_for_handle<'a>(
    handle: &SurfaceHandle,
    state: &'a LoaderState,
) -> Option<&'a Connection> {
    let mut matches: Vec<&Connection> = state
        .connections
        .values()
        .filter(|connection| connection.logical_connection_id == handle.logical_connection_id)
        .collect();
    matches.sort_by(|left, right| right.attached_ms.cmp(&left.attached_ms));
    matches
        .iter()
        .find(|connection| connection_live(connection))
        .copied()
        .or_else(|| matches.first().copied())
}

fn list_attached_tools(arguments: &JsonObject, state: &LoaderState) -> Result<Value, Diagnostic> {
    let connection = get_connection(arguments, state)?;
    Ok(
        json!({"schema":"narada.mcp_loader.tools.v1","connection_id":connection.connection_id,"surface_id":connection.surface_id,
        "runtime_lifecycle":runtime_lifecycle(Some(&connection.connection_id),Some(&connection.lifecycle)),
        "runtime_freshness":runtime_freshness(state),"tools":connection.tools}),
    )
}

fn surface_status(arguments: &JsonObject, state: &LoaderState) -> Result<Value, Diagnostic> {
    let connection = get_connection(arguments, state)?;
    let mut result = connection_status(connection, state)
        .as_object()
        .cloned()
        .unwrap_or_default();
    result.insert(
        "schema".to_string(),
        json!("narada.mcp_loader.surface_status.v1"),
    );
    Ok(Value::Object(result))
}
fn tool_discovery_manifest(
    arguments: &JsonObject,
    state: &LoaderState,
) -> Result<Value, Diagnostic> {
    let connection = get_connection(arguments, state)?;
    let tools: Vec<Value> = connection.tools.iter().map(|tool| {
        json!({
            "canonical_name":tool.get("name").and_then(Value::as_str).unwrap_or_default(),
            "callable_name":tool.get("name").and_then(Value::as_str).unwrap_or_default(),
            "generated_aliases":[],
            "description":tool.get("description").cloned().unwrap_or(Value::Null),
            "inputSchema":tool.get("inputSchema").or_else(|| tool.get("input_schema")).cloned().unwrap_or(Value::Null)
        })
    }).collect();
    Ok(
        json!({"schema":"narada.mcp_loader.tool_discovery_manifest.v1","connection_id":connection.connection_id,"surface_id":connection.surface_id,
        "runtime_lifecycle":runtime_lifecycle(Some(&connection.connection_id),Some(&connection.lifecycle)),
        "runtime_freshness":runtime_freshness(state),
        "alias_policy":{"canonical_name_source":"tools/list.name","generated_aliases_authoritative":false,"guidance":"Use canonical_name/callable_name for directives and tool calls. Client-generated aliases should be treated as compatibility UI labels only."},
        "tools":tools}),
    )
}

fn get_connection<'a>(
    arguments: &JsonObject,
    state: &'a LoaderState,
) -> Result<&'a Connection, Diagnostic> {
    let id = required_string(arguments, "connection_id", "missing_connection_id")?;
    state.connections.get(&id).ok_or_else(|| {
        Diagnostic::new(
            "connection_not_found",
            format!("connection_not_found:{}", id),
        )
    })
}

fn resolve_timeout(arguments: &Value, policy: &Policy) -> Result<(u64, bool, u64), Diagnostic> {
    let requested = arguments.get("timeout_ms");
    let Some(requested) = requested else {
        return Ok((policy.tool_call_timeout_ms, false, 0));
    };
    let requested = requested
        .as_u64()
        .or_else(|| {
            requested
                .as_i64()
                .and_then(|value| u64::try_from(value).ok())
        })
        .ok_or_else(|| Diagnostic::new("invalid_tool_call_timeout", "invalid_tool_call_timeout"))?;
    if requested == 0 || requested > MAX_TOOL_TIMEOUT_MS {
        return Err(Diagnostic::new(
            "tool_call_timeout_exceeds_loader_max",
            format!("tool_call_timeout_exceeds_loader_max:{}", requested),
        )
        .with_details(
            json!({"requested_timeout_ms":requested,"max_timeout_ms":MAX_TOOL_TIMEOUT_MS}),
        ));
    }
    Ok((
        requested.saturating_add(policy.tool_call_grace_ms),
        true,
        requested,
    ))
}

fn call_attached_tool(
    arguments: &JsonObject,
    state: &mut LoaderState,
) -> Result<Value, Diagnostic> {
    let connection_id = required_string(arguments, "connection_id", "missing_connection_id")?;
    let tool_name = required_string(arguments, "tool_name", "missing_tool_name")?;
    if let Some(connection) = state.connections.get(&connection_id) {
        if let Some(binding_id) = connection.binding_id.as_deref() {
            admitted_binding(state, &connection.site_root, binding_id, "attach")?;
        } else {
            assert_binding_admission_available(state)?;
        }
    }
    let tool_arguments = arguments
        .get("arguments")
        .cloned()
        .unwrap_or_else(|| json!({}));
    let tool_object = tool_arguments.as_object().cloned().unwrap_or_default();
    if json_byte_len(&tool_arguments) > state.policy.max_request_bytes {
        return Err(Diagnostic::new(
            "request_too_large",
            format!(
                "request_too_large:{}:{}",
                json_byte_len(&tool_arguments),
                state.policy.max_request_bytes
            ),
        ));
    }
    let (outer_timeout, explicit_timeout, request_timeout) =
        resolve_timeout(&tool_arguments, &state.policy)?;
    let request_params = {
        let mut object = Map::new();
        object.insert("name".to_string(), Value::String(tool_name.clone()));
        object.insert("arguments".to_string(), Value::Object(tool_object));
        if explicit_timeout {
            object.insert(
                "_meta".to_string(),
                json!({"narada_request_timeout_ms":request_timeout}),
            );
        }
        Value::Object(object)
    };
    let child_result = {
        let connection = state.connections.get_mut(&connection_id).ok_or_else(|| {
            Diagnostic::new(
                "connection_not_found",
                format!("connection_not_found:{}", connection_id),
            )
        })?;
        if connection.detached {
            return Err(Diagnostic::new(
                "connection_detached",
                format!("connection_detached:{}", connection_id),
            ));
        }
        match connection
            .session
            .request("tools/call", request_params, outer_timeout)
        {
            Ok(result) => {
                touch_connection(connection);
                result
            }
            Err(mut error) => {
                let domain_details =
                    request_error_details(&error.details, "tools/call", outer_timeout);
                error.details = child_runtime_diagnostic(connection, domain_details);
                return Err(error);
            }
        }
    };
    let include_runtime = arguments
        .get("include_runtime_metadata")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let enriched = if include_runtime
        && (tool_name.ends_with("_guidance") || tool_name == "guidance")
    {
        let mut result = child_result.clone();
        if let Some(object) = result.as_object_mut() {
            let mut structured = object
                .get("structuredContent")
                .cloned()
                .unwrap_or_else(|| json!({}));
            if let Some(structured_object) = structured.as_object_mut() {
                let connection = state.connections.get(&connection_id).ok_or_else(|| {
                    Diagnostic::new("connection_not_found", "connection_not_found")
                })?;
                structured_object.insert(
                    "loader_runtime_lifecycle".to_string(),
                    runtime_lifecycle(Some(&connection.connection_id), Some(&connection.lifecycle)),
                );
                structured_object.insert(
                    "loader_runtime_freshness".to_string(),
                    runtime_freshness(state),
                );
            }
            object.insert("structuredContent".to_string(), structured);
        }
        result
    } else {
        child_result
    };
    let is_error = enriched
        .get("isError")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let compacted = compact_child_result(&enriched);
    let bounded = build_bounded_result(
        state,
        &connection_id,
        &format!(
            "mcp_loader_call_tool:{}:{}",
            state
                .connections
                .get(&connection_id)
                .map(|value| value.surface_id.as_str())
                .unwrap_or("surface"),
            tool_name
        ),
        &compacted,
        is_error,
    )?;
    let connection = state
        .connections
        .get(&connection_id)
        .ok_or_else(|| Diagnostic::new("connection_not_found", "connection_not_found"))?;
    let bounded_object = bounded
        .get("structuredContent")
        .cloned()
        .unwrap_or(Value::Null);
    let result_bounded = bounded_object.get("schema").and_then(Value::as_str)
        == Some("narada.producer_output_page.v1");
    let mut response = json!({
        "schema":"narada.mcp_loader.tool_result.v1","connection_id":connection.connection_id,"surface_id":connection.surface_id,
        "result":bounded_object,"result_summary":typed_result_summary(&enriched),"result_bounded":result_bounded
    });
    if let Some(output_ref) = bounded
        .get("structuredContent")
        .and_then(|value| value.get("output_ref"))
        .and_then(Value::as_str)
    {
        response["details_ref"] = json!(output_ref);
        response["details_reader"] = json!("mcp_loader_read_result");
    }
    if include_runtime {
        response["runtime_lifecycle"] =
            runtime_lifecycle(Some(&connection.connection_id), Some(&connection.lifecycle));
        response["runtime_freshness"] = runtime_freshness(state);
    }
    if json_byte_len(&response) > state.policy.max_response_bytes {
        return Err(Diagnostic::new(
            "response_too_large",
            format!(
                "response_too_large:{}:{}",
                json_byte_len(&response),
                state.policy.max_response_bytes
            ),
        ));
    }
    Ok(response)
}

fn typed_result_summary(result: &Value) -> Value {
    let structured = result
        .get("structuredContent")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    let mut summary = json!({
        "schema":structured.get("schema").and_then(Value::as_str).unwrap_or("narada.mcp_loader.child_result.v1"),
        "status":structured.get("status").and_then(Value::as_str).unwrap_or(if result.get("isError").and_then(Value::as_bool).unwrap_or(false) {"error"} else {"ok"}),
        "is_error":result.get("isError").and_then(Value::as_bool).unwrap_or(false)
    });
    if let Some(summary_object) = summary.as_object_mut() {
        for key in [
            "code",
            "message",
            "summary",
            "surface_id",
            "task_id",
            "task_number",
            "ref",
            "output_ref",
            "next_offset",
            "truncated",
        ] {
            if let Some(value) = structured.get(key) {
                if value.is_string() || value.is_number() || value.is_boolean() || value.is_null() {
                    summary_object.insert(key.to_string(), value.clone());
                }
            }
        }
        for key in ["count", "total", "checked_surface_count", "violation_count"] {
            if let Some(value) = structured.get(key).filter(|value| value.is_number()) {
                summary_object.insert(key.to_string(), value.clone());
            }
        }
        if let Some(items) = structured.get("items").and_then(Value::as_array) {
            summary_object.insert("item_count".to_string(), json!(items.len()));
        }
        if let Some(findings) = structured.get("findings").and_then(Value::as_array) {
            summary_object.insert("finding_count".to_string(), json!(findings.len()));
        }
    }
    summary
}

fn request_error_details(details: &Value, method: &str, timeout_ms: u64) -> Value {
    let mut result = details.as_object().cloned().unwrap_or_default();
    result.insert("method".to_string(), json!(method));
    result.insert("timeout_ms".to_string(), json!(timeout_ms));
    Value::Object(result)
}

fn child_runtime_diagnostic(connection: &Connection, extra: Value) -> Value {
    let mut result = json!({
        "connection_id":connection.connection_id,"surface_id":connection.surface_id,"entrypoint":connection.entrypoint,
        "args":connection.args,"exit_code":connection.session.exit_code(),"signal_code":connection.session.signal_code(),
        "stderr_tail":connection.session.stderr_tail(),
        "runtime_lifecycle":runtime_lifecycle(Some(&connection.connection_id),Some(&connection.lifecycle))
    });
    if let (Some(target), Some(source)) = (result.as_object_mut(), extra.as_object()) {
        for (key, value) in source {
            target.insert(key.clone(), value.clone());
        }
    }
    result
}

fn detach_connection(arguments: &JsonObject, state: &mut LoaderState) -> Result<Value, Diagnostic> {
    let id = required_string(arguments, "connection_id", "missing_connection_id")?;
    let mut connection = state.connections.remove(&id).ok_or_else(|| {
        Diagnostic::new(
            "connection_not_found",
            format!("connection_not_found:{}", id),
        )
    })?;
    connection.detached = true;
    connection.detached_ms = Some(now_ms());
    let termination = connection.session.terminate();
    Ok(
        json!({"schema":"narada.mcp_loader.detached.v1","connection_id":connection.connection_id,"surface_id":connection.surface_id,"status":"detached","termination":termination}),
    )
}

fn restart_connection(
    arguments: &JsonObject,
    state: &mut LoaderState,
) -> Result<Value, Diagnostic> {
    let id = required_string(arguments, "connection_id", "missing_connection_id")?;
    let admitted = if let Some(existing) = state.connections.get(&id) {
        if let Some(binding_id) = existing.binding_id.as_deref() {
            admitted_binding(state, &existing.site_root, binding_id, "restart")?
        } else {
            assert_binding_admission_available(state)?;
            None
        }
    } else {
        None
    };
    let mut previous = state.connections.remove(&id).ok_or_else(|| {
        Diagnostic::new(
            "connection_not_found",
            format!("connection_not_found:{}", id),
        )
    })?;
    if lifecycle_mode(&previous) != Some("replayable") {
        let runtime = runtime_lifecycle(Some(&previous.connection_id), Some(&previous.lifecycle));
        let recovery = connection_recovery_actions(&previous);
        state.connections.insert(id.clone(), previous);
        return Err(Diagnostic::new("surface_restart_not_loader_replayable", format!("surface_restart_not_loader_replayable:{}", id))
            .with_details(json!({"connection_id":id,"surface_id":state.connections.get(&id).map(|value| value.surface_id.clone()),"lifecycle":state.connections.get(&id).map(|value| value.lifecycle.clone()),"runtime_lifecycle":runtime,"recovery_actions":recovery})));
    }
    let previous_status = connection_status(&previous, state);
    let termination = previous.session.terminate();
    previous.detached = true;
    previous.detached_ms = Some(now_ms());
    let replacement = match open_connection(
        state,
        previous.site_root.clone(),
        previous.surface_id.clone(),
        previous.runtime_kind.clone(),
        previous.runtime_requirements.clone(),
        previous.entrypoint.clone(),
        previous.args.clone(),
        previous.session.spec.command.clone(),
        previous.child_invocation_kind.clone(),
        previous.requested_entrypoint.clone(),
        previous.extra_args.clone(),
        previous.server_name.clone(),
        previous.projection_id.clone(),
        previous.execution.clone(),
        previous.lifecycle.clone(),
        previous.descriptor.clone(),
        previous.descriptor_digest.clone(),
        previous.declared_tool_contract_digest.clone(),
        admitted.map(|(entry, _)| entry),
    ) {
        Ok(mut connection) => {
            connection.logical_connection_id = previous.logical_connection_id.clone();
            connection
        }
        Err(error) => {
            state.connections.insert(id.clone(), previous);
            return Err(error);
        }
    };
    let response = json!({
        "schema":"narada.mcp_loader.surface_restarted.v1","status":"restarted","reason":value_string(arguments.get("reason")),
        "previous_connection":previous_status,"replacement_connection":connection_status(&replacement,state),
        "connection_id":replacement.connection_id,"previous_connection_id":id,"surface_id":replacement.surface_id,
        "runtime_lifecycle":runtime_lifecycle(Some(&replacement.connection_id),Some(&replacement.lifecycle)),
        "entrypoint":replacement.entrypoint,"args":replacement.args,"termination":termination,
        "server_info":replacement.server_info,"tools":replacement.tools
    });
    state
        .connections
        .insert(replacement.connection_id.clone(), replacement);
    Ok(response)
}

fn runtime_observation(
    arguments: &JsonObject,
    state: &mut LoaderState,
) -> Result<Value, Diagnostic> {
    let id = required_string(arguments, "connection_id", "missing_connection_id")?;
    let carrier_kind = required_string(arguments, "carrier_kind", "missing_carrier_kind")?;
    let connection = state.connections.get_mut(&id).ok_or_else(|| {
        Diagnostic::new(
            "connection_not_found",
            format!("connection_not_found:{}", id),
        )
    })?;
    let live = connection_live(connection);
    if live {
        touch_connection(connection);
    }
    let site_id = derive_site_id(&connection.site_root)?;
    let manifest_digest = value_string(arguments.get("manifest_digest"));
    Ok(json!({
        "schema_version":"2.0","observation_id":format!("observation-{}-{}",now_ms(),&connection.logical_connection_id[..connection.logical_connection_id.len().min(12)]),
        "observed_at":now_iso(),"site_id":site_id,"carrier_kind":carrier_kind,"runtime_state_root":Value::Null,
        "manifest_digest":manifest_digest,
        "servers":[{
            "server_name":connection.server_name,"surface_id":connection.surface_id,"projection_id":connection.projection_id,
            "logical_connection_id":connection.logical_connection_id,"lifecycle":connection.lifecycle,
            "active_generation":if live {runtime_generation(connection,now_ms())} else {Value::Null},
            "draining_generations":[],
            "recovery_actions":connection_recovery_actions(connection),
            "detail":if live {"mcp-loader owns this active generation; use the bounded loader restart action for replacement."} else {"The loader child is no longer live; inspect the status and use the bounded loader restart action if lifecycle permits."}
        }]
    }))
}

fn call_surface_handle_tool(
    arguments: &JsonObject,
    state: &mut LoaderState,
) -> Result<Value, Diagnostic> {
    let handle_name = required_string(arguments, "surface_handle", "missing_surface_handle")?;
    let handle = state.handles.get(&handle_name).ok_or_else(|| {
        Diagnostic::new(
            "surface_handle_not_found",
            format!("surface_handle_not_found:{}", handle_name),
        )
    })?;
    let connection_id = find_connection_for_handle(handle, state)
        .filter(|connection| connection_live(connection))
        .map(|connection| connection.connection_id.clone());
    let Some(connection_id) = connection_id else {
        return Err(Diagnostic::new("surface_handle_connection_unavailable", format!("surface_handle_connection_unavailable:{}", handle_name))
            .with_details(json!({"surface_handle":handle_name,"logical_connection_id":handle.logical_connection_id,"site_root":handle.site_root,"surface_id":handle.surface_id,"recovery":{"tool_name":"mcp_loader_open_surface","arguments":{"site_root":handle.site_root,"surface_id":handle.surface_id,"runtime_kind":handle.runtime_kind}}})));
    };
    let mut delegated = arguments.clone();
    delegated.insert("connection_id".to_string(), json!(connection_id));
    call_attached_tool(&delegated, state)
}

fn render_result(result: &Value) -> String {
    let schema = result
        .get("schema")
        .and_then(Value::as_str)
        .unwrap_or("mcp_loader.result");
    let status = result.get("status").and_then(Value::as_str).unwrap_or("ok");
    if schema == "narada.mcp_loader.site_tool_inventory_check.v1" {
        let mut lines = vec![
            format!("{}: {}", schema, status),
            format!(
                "checked_surface_count: {}",
                result
                    .get("checked_surface_count")
                    .and_then(Value::as_u64)
                    .unwrap_or(0)
            ),
            format!(
                "violation_count: {}",
                result
                    .get("violation_count")
                    .and_then(Value::as_u64)
                    .unwrap_or(0)
            ),
            format!(
                "finding_status_counts: {}",
                serde_json::to_string(result.get("finding_status_counts").unwrap_or(&json!({})))
                    .unwrap_or_default()
            ),
        ];
        if let Some(findings) = result.get("findings").and_then(Value::as_array) {
            if !findings.is_empty() {
                lines.push("findings:".to_string());
            }
            for finding in findings.iter().take(50) {
                let surface = finding
                    .get("surface_id")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown-surface");
                let status = finding
                    .get("status")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown");
                lines.push(format!("- {} [{}]", surface, status));
                for key in [
                    "missing_from_fabric",
                    "extra_in_fabric",
                    "duplicate_declared_tools",
                    "duplicate_observed_tools",
                    "unclassified_observed_tools",
                ] {
                    if let Some(values) = finding.get(key).and_then(Value::as_array) {
                        let visible: Vec<String> = values
                            .iter()
                            .filter_map(|value| value.as_str().map(String::from))
                            .take(20)
                            .collect();
                        if !visible.is_empty() {
                            lines.push(format!("  {}: {}", key, visible.join(", ")));
                        }
                    }
                }
                if let Some(error) = finding.get("error") {
                    let code = error.get("code").and_then(Value::as_str).unwrap_or("");
                    let message = error.get("message").and_then(Value::as_str).unwrap_or("");
                    if !code.is_empty() || !message.is_empty() {
                        lines.push(format!("  error: {} - {}", code, message));
                    }
                }
            }
        }
        if let Some(reference) = result.get("observation_ref").and_then(Value::as_str) {
            lines.push(format!("observation_ref: {}", reference));
        }
        return lines.join("\n");
    }
    let connection = result
        .get("connection_id")
        .and_then(Value::as_str)
        .map(|id| format!("\nconnection_id: {}", id))
        .unwrap_or_default();
    let surface = result
        .get("surface_id")
        .and_then(Value::as_str)
        .map(|id| format!("\nsurface_id: {}", id))
        .unwrap_or_default();
    format!("{}: {}{}{}", schema, status, connection, surface)
}

fn output_root(site_root: &str) -> String {
    join_path(site_root, ".ai/tmp/mcp-outputs/workspace")
}

fn payload_root(site_root: &str) -> String {
    join_path(site_root, ".ai/tmp/mcp-payloads/workspace")
}

fn write_immutable(path: &str, content: &str) -> Result<bool, Diagnostic> {
    if let Some(parent) = Path::new(path).parent() {
        create_dir_all(parent)
            .map_err(|error| Diagnostic::new("output_directory_failed", error.to_string()))?;
    }
    match OpenOptions::new().write(true).create_new(true).open(path) {
        Ok(mut file) => {
            file.write_all(content.as_bytes())
                .map_err(|error| Diagnostic::new("output_write_failed", error.to_string()))?;
            file.sync_all().ok();
            Ok(true)
        }
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => Ok(false),
        Err(error) => Err(Diagnostic::new("output_write_failed", error.to_string())),
    }
}

fn output_id() -> String {
    format!(
        "o_{}{}",
        now_ms(),
        ID_COUNTER.fetch_add(1, Ordering::SeqCst)
    )
}

fn bounded_page(text: &str, offset: usize, limit: usize, max_bytes: usize) -> (String, usize) {
    let chars: Vec<char> = text.chars().collect();
    if offset >= chars.len() {
        return (String::new(), chars.len());
    }
    let mut end = (offset + limit).min(chars.len());
    while end > offset {
        let chunk: String = chars[offset..end].iter().collect();
        if chunk.as_bytes().len() <= max_bytes {
            return (chunk, end);
        }
        end -= 1;
    }
    (String::new(), offset)
}

fn compact_child_result(value: &Value) -> Value {
    let Some(object) = value.as_object() else {
        return value.clone();
    };
    if !object.contains_key("structuredContent") {
        return value.clone();
    }
    let mut compacted = object.clone();
    compacted.remove("content");
    Value::Object(compacted)
}

fn build_bounded_result(
    state: &LoaderState,
    connection_id: &str,
    tool_name: &str,
    value: &Value,
    is_error: bool,
) -> Result<Value, Diagnostic> {
    let full_text = pretty_json(value);
    let inline_limit = DEFAULT_LOADER_RESULT_INLINE_LIMIT;
    if utf16_len(&full_text) <= inline_limit
        && full_text.as_bytes().len() + json_byte_len(value) <= MAX_INLINE_RESPONSE_BYTES
    {
        return Ok(
            json!({"content":[{"type":"text","text":full_text,"annotations":{"audience":["assistant"]}}],"structuredContent":value,"isError":if is_error {Value::Bool(true)} else {Value::Null}}),
        );
    }
    let connection = state.connections.get(connection_id).ok_or_else(|| {
        Diagnostic::new(
            "connection_not_found",
            format!("connection_not_found:{}", connection_id),
        )
    })?;
    let id = output_id();
    let reference = format!("mcp_output:{}", id);
    let record = json!({
        "schema":"narada.mcp_output_ref.v1","ref":reference,"output_id":id,"tool_name":tool_name,
        "created_at":now_iso(),"created_by":env::var("NARADA_AGENT_ID").ok(),"content_type":"application/json",
        "inline_char_limit":inline_limit,"full_output_char_length":utf16_len(&full_text),"truncated":true,
        "sha256":sha256(&stable_json(value)),"max_bytes":state.policy.max_response_bytes,"full_output":value
    });
    let serialized = format!("{}\n", stable_json(&record));
    let path = join_path(&output_root(&connection.site_root), &format!("{}.json", id));
    if !write_immutable(&path, &serialized)? {
        return Err(Diagnostic::new(
            "mcp_output_ref_collision",
            format!("mcp_output_ref_collision:{}", reference),
        ));
    }
    let mut preview_limit = inline_limit.min(MAX_OUTPUT_SHOW_CHAR_LIMIT);
    let envelope = loop {
        let (preview, end) = bounded_page(&full_text, 0, preview_limit, MAX_OUTPUT_PAGE_BYTES);
        let next = if end < full_text.chars().count() {
            Some(end)
        } else {
            None
        };
        let envelope = json!({
            "schema":"narada.producer_output_page.v1","status":output_status(value,is_error),"truncated":true,
            "output_ref":reference,"ref":reference,"result_materialized":true,"tool_name":tool_name,
            "offset":0,"limit":inline_limit,"next_offset":next,"transport_offset":0,"transport_limit":inline_limit,
            "transport_next_offset":next,"output_text":preview,"output_truncated":next.is_some(),"reader_tool":"mcp_loader_read_result",
            "site_root":connection.site_root,
            "read_command":format!("mcp_loader_read_result({{ \"ref\": \"{}\", \"offset\": 0, \"limit\": {} }})",reference,DEFAULT_OUTPUT_SHOW_CHAR_LIMIT),
            "remediation":format!("Use mcp_loader_read_result with output_ref/ref={} to read the bounded produced JSON pages; continue with the returned next_offset.",reference),
            "inline_limit":inline_limit,"full_output_char_length":utf16_len(&full_text)
        });
        if json_byte_len(&Value::String(
            serde_json::to_string(&envelope).unwrap_or_default(),
        )) <= inline_limit
            && json_byte_len(&envelope) <= MAX_INLINE_RESPONSE_BYTES
        {
            break envelope;
        }
        if preview_limit == 0 {
            return Err(Diagnostic::new(
                "inline_output_envelope_limit_too_small",
                "inline_output_envelope_limit_too_small",
            ));
        }
        preview_limit = preview_limit.saturating_mul(3) / 4;
    };
    Ok(
        json!({"content":[{"type":"text","text":serde_json::to_string(&envelope).unwrap_or_default(),"annotations":{"audience":["assistant"]}}],"structuredContent":envelope,"isError":if is_error {Value::Bool(true)} else {Value::Null}}),
    )
}

fn output_status(value: &Value, is_error: bool) -> String {
    value
        .get("status")
        .and_then(Value::as_str)
        .filter(|text| text.len() <= 32)
        .map(String::from)
        .unwrap_or_else(|| {
            if is_error {
                "error".to_string()
            } else {
                "ok".to_string()
            }
        })
}

fn read_loader_result(arguments: &JsonObject, state: &LoaderState) -> Result<Value, Diagnostic> {
    let connection = get_connection(arguments, state)?;
    let reference = value_string(arguments.get("ref"))
        .or_else(|| value_string(arguments.get("output_ref")))
        .ok_or_else(|| Diagnostic::new("missing_output_ref", "missing_output_ref"))?;
    if !reference.starts_with("mcp_output:") {
        return Err(Diagnostic::new(
            "output_ref_invalid",
            format!("output_ref_invalid:{}", reference),
        ));
    }
    let id = reference.trim_start_matches("mcp_output:");
    if id.is_empty() || id.contains('/') || id.contains('\\') {
        return Err(Diagnostic::new(
            "output_ref_invalid",
            format!("output_ref_invalid:{}", reference),
        ));
    }
    let path = join_path(&output_root(&connection.site_root), &format!("{}.json", id));
    let bytes = fs::read(&path).map_err(|_| {
        Diagnostic::new(
            "output_ref_not_found",
            format!("output_ref_not_found:{}", reference),
        )
    })?;
    if bytes.len() > state.policy.max_response_bytes {
        return Err(Diagnostic::new(
            "output_ref_too_large",
            format!("output_ref_too_large:{}", reference),
        ));
    }
    let record: Value = serde_json::from_slice(&bytes)
        .map_err(|error| Diagnostic::new("output_ref_invalid_json", error.to_string()))?;
    if record.get("schema").and_then(Value::as_str) != Some("narada.mcp_output_ref.v1")
        || record.get("ref").and_then(Value::as_str) != Some(reference.as_str())
    {
        return Err(Diagnostic::new(
            "output_ref_metadata_mismatch",
            format!("output_ref_metadata_mismatch:{}", reference),
        ));
    }
    let full_output = record.get("full_output").cloned().unwrap_or(Value::Null);
    let full_text = pretty_json(&full_output);
    if record
        .get("full_output_char_length")
        .and_then(Value::as_u64)
        .map(usize::try_from)
        .transpose()
        .ok()
        .flatten()
        != Some(utf16_len(&full_text))
    {
        return Err(Diagnostic::new(
            "output_ref_length_mismatch",
            format!("output_ref_length_mismatch:{}", reference),
        ));
    }
    if record.get("sha256").and_then(Value::as_str)
        != Some(sha256(&stable_json(&full_output)).as_str())
    {
        return Err(Diagnostic::new(
            "output_ref_sha256_mismatch",
            format!("output_ref_sha256_mismatch:{}", reference),
        ));
    }
    let offset = arguments.get("offset").and_then(Value::as_u64).unwrap_or(0) as usize;
    let limit = arguments
        .get("limit")
        .and_then(Value::as_u64)
        .unwrap_or(DEFAULT_OUTPUT_SHOW_CHAR_LIMIT as u64) as usize;
    if limit == 0 || limit > MAX_OUTPUT_SHOW_CHAR_LIMIT {
        return Err(Diagnostic::new(
            "output_limit_exceeds_transport_maximum",
            format!("output_limit_exceeds_transport_maximum:{}", limit),
        ));
    }
    let (chunk, end) = bounded_page(&full_text, offset, limit, MAX_OUTPUT_PAGE_BYTES);
    let page = json!({
        "schema":"narada.mcp_output_page.v1","status":"ok","ref":reference,
        "tool_name":record.get("tool_name").cloned().unwrap_or(Value::Null),
        "full_output_char_length":utf16_len(&full_text),"byte_size":bytes.len(),"original_truncated":true,
        "path":format!(".ai/tmp/mcp-outputs/workspace/{}.json",id),
        "offset":offset,"limit":limit,"next_offset":if end < full_text.chars().count() {Value::from(end as u64)} else {Value::Null},
        "output_limit":limit,"output_truncated":end < full_text.chars().count(),"output_text":chunk
    });
    Ok(
        json!({"schema":"narada.mcp_loader.result_page.v1","connection_id":connection.connection_id,"surface_id":connection.surface_id,"result":page}),
    )
}

fn payload_observation(
    site_root: &str,
    observation: &Value,
    state: &LoaderState,
) -> Result<(String, String, usize, Value), Diagnostic> {
    let id = format!(
        "{}{}",
        SITE_TOOL_OBSERVATION_PAYLOAD_PREFIX,
        output_id().trim_start_matches("o_")
    );
    let reference = format!("mcp_payload:{}@v1", id);
    let payload_json = stable_json(observation);
    let payload_size = payload_json.as_bytes().len();
    if payload_size > state.policy.max_response_bytes {
        return Err(Diagnostic::new(
            "payload_too_large",
            format!("payload_too_large:{}", payload_size),
        ));
    }
    let record = json!({
        "schema":"narada.mcp_payload.revision.v1","ref":reference,"payload_id":id,"revision":1,
        "created_at":now_iso(),"created_by":SERVER_NAME,"source":{"kind":"create"},
        "sha256":sha256(&payload_json),"byte_size":payload_size,"max_bytes":state.policy.max_response_bytes,
        "transient_not_authority":true,"immutable_revision":true,"payload":observation
    });
    let path = join_path(
        &join_path(site_root, ".ai/tmp/mcp-payloads/workspace"),
        &format!("{}/v1.json", id),
    );
    let serialized = format!("{}\n", stable_json(&record));
    let written = write_immutable(&path, &serialized)?;
    if !written {
        let existing = read_to_string(&path)
            .map_err(|error| Diagnostic::new("payload_revision_conflict", error.to_string()))?;
        let existing_value: Value = serde_json::from_str(&existing)
            .map_err(|error| Diagnostic::new("payload_revision_conflict", error.to_string()))?;
        if existing_value.get("sha256") != record.get("sha256") {
            return Err(Diagnostic::new("payload_revision_conflict", reference));
        }
    }
    let retention = prune_payloads(site_root)?;
    Ok((reference, sha256(&payload_json), payload_size, retention))
}

fn prune_payloads(site_root: &str) -> Result<Value, Diagnostic> {
    let root = payload_root(site_root);
    if !Path::new(&root).exists() {
        return Ok(
            json!({"status":"ok","payload_id_prefix":SITE_TOOL_OBSERVATION_PAYLOAD_PREFIX,"max_entries":SITE_TOOL_OBSERVATION_MAX_ENTRIES,"max_age_ms":SITE_TOOL_OBSERVATION_MAX_AGE_MS,"considered_count":0,"retained_count":0,"removed_count":0,"retained_payload_ids":[],"removed_payload_ids":[]}),
        );
    }
    let mut entries: Vec<(String, u128, String)> = read_dir(&root)
        .ok()
        .into_iter()
        .flatten()
        .filter_map(|entry| {
            let entry = entry.ok()?;
            let name = entry.file_name().to_string_lossy().to_string();
            if !name.starts_with(SITE_TOOL_OBSERVATION_PAYLOAD_PREFIX) || !entry.path().is_dir() {
                return None;
            }
            let modified = entry
                .metadata()
                .ok()?
                .modified()
                .ok()?
                .duration_since(UNIX_EPOCH)
                .ok()?
                .as_millis();
            Some((name, modified, entry.path().to_string_lossy().to_string()))
        })
        .collect();
    entries.sort_by(|left, right| right.1.cmp(&left.1).then_with(|| right.0.cmp(&left.0)));
    let now = now_ms();
    let mut retained = Vec::new();
    let mut removed = Vec::new();
    for (index, (name, modified, path)) in entries.iter().enumerate() {
        if index >= SITE_TOOL_OBSERVATION_MAX_ENTRIES
            || now.saturating_sub(*modified) > SITE_TOOL_OBSERVATION_MAX_AGE_MS
        {
            remove_dir_all(path).ok();
            removed.push(name.clone());
        } else {
            retained.push(name.clone());
        }
    }
    Ok(
        json!({"status":"ok","payload_id_prefix":SITE_TOOL_OBSERVATION_PAYLOAD_PREFIX,"max_entries":SITE_TOOL_OBSERVATION_MAX_ENTRIES,"max_age_ms":SITE_TOOL_OBSERVATION_MAX_AGE_MS,"considered_count":entries.len(),"retained_count":retained.len(),"removed_count":removed.len(),"retained_payload_ids":retained,"removed_payload_ids":removed}),
    )
}

fn site_tool_inventory(
    arguments: &JsonObject,
    state: &mut LoaderState,
) -> Result<Value, Diagnostic> {
    let site_root = normalize_path(&required_string(
        arguments,
        "site_root",
        "missing_site_root",
    )?);
    ensure_site_root_allowed(&site_root, &state.policy)?;
    let bundle = read_site_fabric(&site_root)?;
    let servers = bundle
        .fabric
        .get("mcpServers")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    let requested = string_array(arguments.get("surface_ids"))?;
    let mut surface_ids = requested.clone().unwrap_or_else(|| {
        let mut values: Vec<String> = servers.keys().cloned().collect();
        values.sort();
        values
    });
    let runtime_kind = value_string(arguments.get("runtime_kind"));
    let include_ok = arguments
        .get("include_ok")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let mut findings = Vec::new();
    let mut observed_tools = Map::new();
    let mut observed_read_only = Map::new();
    let mut observed_mutating = Map::new();
    let mut observed_unclassified = Map::new();
    for surface_id in &surface_ids {
        let matched = find_site_server(&servers, surface_id)?;
        let Some((_, server)) = matched else {
            findings.push(json!({"surface_id":surface_id,"status":"surface_not_declared","declared_tools":[],"observed_tools":[]}));
            continue;
        };
        let raw_declared: Vec<String> = server
            .get("tools")
            .and_then(Value::as_array)
            .map(|values| {
                values
                    .iter()
                    .filter_map(|value| value.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default();
        let declared = sorted_unique(&raw_declared);
        let duplicate_declared = duplicate_strings(&raw_declared);
        let requirements = surface_requirements(Some(&server));
        if !runtime_matches(&requirements, runtime_kind.as_deref()) {
            findings.push(json!({"surface_id":surface_id,"status":"runtime_not_selected","declared_count":declared.len(),"observed_count":0,"declared_tools":declared,"observed_tools":[],"runtime_kind":runtime_kind,"runtime_requirements":requirements}));
            continue;
        }
        let mut connection_id: Option<String> = None;
        let probe = attach_surface(
            &json_object!({"site_root":site_root.clone(),"surface_id":surface_id.clone(),"runtime_kind":runtime_kind.clone()}),
            state,
        );
        match probe {
            Ok(attached) => {
                let id = attached
                    .get("connection_id")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                connection_id = Some(id.clone());
                if let Some(connection) = state.connections.get(&id) {
                    let observed_definitions: Vec<&Value> = connection
                        .tools
                        .iter()
                        .filter(|tool| {
                            tool.get("name").and_then(Value::as_str)
                                != Some(RUNTIME_PROXY_STATUS_TOOL_NAME)
                        })
                        .collect();
                    let raw_observed: Vec<String> = observed_definitions
                        .iter()
                        .filter_map(|tool| {
                            tool.get("name").and_then(Value::as_str).map(String::from)
                        })
                        .filter(|name| !name.is_empty())
                        .collect();
                    let observed = sorted_unique(&raw_observed);
                    let duplicate_observed = duplicate_strings(&raw_observed);
                    let read_only: Vec<String> = sorted_unique(
                        &observed_definitions
                            .iter()
                            .filter(|tool| {
                                tool.get("annotations")
                                    .and_then(|value| value.get("readOnlyHint"))
                                    .and_then(Value::as_bool)
                                    == Some(true)
                            })
                            .filter_map(|tool| {
                                tool.get("name").and_then(Value::as_str).map(String::from)
                            })
                            .collect::<Vec<_>>()
                            .as_slice(),
                    );
                    let mutating: Vec<String> = sorted_unique(
                        &observed_definitions
                            .iter()
                            .filter(|tool| {
                                tool.get("annotations")
                                    .and_then(|value| value.get("readOnlyHint"))
                                    .and_then(Value::as_bool)
                                    == Some(false)
                            })
                            .filter_map(|tool| {
                                tool.get("name").and_then(Value::as_str).map(String::from)
                            })
                            .collect::<Vec<_>>()
                            .as_slice(),
                    );
                    let unclassified: Vec<String> = sorted_unique(
                        &observed_definitions
                            .iter()
                            .filter(|tool| {
                                tool.get("annotations")
                                    .and_then(|value| value.get("readOnlyHint"))
                                    .and_then(Value::as_bool)
                                    .is_none()
                            })
                            .filter_map(|tool| {
                                tool.get("name").and_then(Value::as_str).map(String::from)
                            })
                            .collect::<Vec<_>>()
                            .as_slice(),
                    );
                    observed_tools.insert(surface_id.clone(), json!(observed));
                    observed_read_only.insert(surface_id.clone(), json!(read_only));
                    observed_mutating.insert(surface_id.clone(), json!(mutating));
                    observed_unclassified.insert(surface_id.clone(), json!(unclassified));
                    let missing: Vec<String> = observed
                        .iter()
                        .filter(|name| !declared.contains(name))
                        .cloned()
                        .collect();
                    let extra: Vec<String> = declared
                        .iter()
                        .filter(|name| !observed.contains(name))
                        .cloned()
                        .collect();
                    let status = if missing.is_empty()
                        && extra.is_empty()
                        && duplicate_declared.is_empty()
                        && duplicate_observed.is_empty()
                        && unclassified.is_empty()
                    {
                        "ok"
                    } else {
                        "drift"
                    };
                    if include_ok || status != "ok" {
                        findings.push(json!({"surface_id":surface_id,"status":status,"declared_count":declared.len(),"observed_count":observed.len(),"missing_from_fabric":missing,"extra_in_fabric":extra,"duplicate_declared_tools":duplicate_declared,"duplicate_observed_tools":duplicate_observed,"unclassified_observed_tools":unclassified}));
                    }
                }
            }
            Err(error) => findings.push(
                json!({"surface_id":surface_id,"status":"probe_failed","error":error.value()}),
            ),
        }
        if let Some(id) = connection_id {
            let _ = detach_connection(&json_object!({"connection_id":id}), state);
        }
    }
    surface_ids.sort();
    let violation_count = findings
        .iter()
        .filter(|finding| {
            !matches!(
                finding.get("status").and_then(Value::as_str),
                Some("ok") | Some("runtime_not_selected")
            )
        })
        .count();
    let skipped: Vec<String> = findings
        .iter()
        .filter(|finding| {
            finding.get("status").and_then(Value::as_str) == Some("runtime_not_selected")
        })
        .filter_map(|finding| {
            finding
                .get("surface_id")
                .and_then(Value::as_str)
                .map(String::from)
        })
        .collect();
    let mut status_counts = Map::new();
    for finding in &findings {
        let status = finding
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or("unknown")
            .to_string();
        let count = status_counts
            .get(&status)
            .and_then(Value::as_u64)
            .unwrap_or(0)
            + 1;
        status_counts.insert(status, json!(count));
    }
    let observed_ids: Vec<String> = observed_tools.keys().cloned().collect();
    let unobserved: Vec<String> = surface_ids
        .iter()
        .filter(|id| !observed_tools.contains_key(*id))
        .cloned()
        .collect();
    let observation = json!({
        "schema":"narada.mcp_loader.site_tool_inventory_check.v1","status":if violation_count>0 {"drift"} else if !skipped.is_empty() {"partial"} else {"ok"},
        "site_root":site_root,"observed_at":now_iso(),"requested_surface_ids":requested,"runtime_kind":runtime_kind,
        "attempted_surface_ids":surface_ids,"observed_surface_ids":observed_ids,"unobserved_surface_ids":unobserved,
        "runtime_skipped_surface_ids":skipped,"runtime_skipped_count":skipped.len(),
        "observation_coverage":if requested.is_some() || !skipped.is_empty() {"partial"} else {"complete"},
        "checked_surface_count":surface_ids.len(),"violation_count":violation_count,
        "observed_tools":observed_tools,"observed_read_only_tools":observed_read_only,
        "observed_mutating_tools":observed_mutating,"observed_unclassified_tools":observed_unclassified,
        "finding_status_counts":status_counts,"findings":findings
    });
    let (reference, digest, byte_size, retention) =
        payload_observation(&site_root, &observation, state)?;
    let mut result = observation;
    if let Some(object) = result.as_object_mut() {
        object.insert("observation_ref".to_string(), json!(reference));
        object.insert("observation_sha256".to_string(), json!(digest));
        object.insert("observation_byte_size".to_string(), json!(byte_size));
        object.insert("observation_retention".to_string(), retention);
    }
    Ok(result)
}

fn sorted_unique(values: &[String]) -> Vec<String> {
    let mut result = values.to_vec();
    result.sort();
    result.dedup();
    result
}

fn duplicate_strings(values: &[String]) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut duplicates = HashSet::new();
    for value in values {
        if !seen.insert(value) {
            duplicates.insert(value.clone());
        }
    }
    let mut result: Vec<String> = duplicates.into_iter().collect();
    result.sort();
    result
}

fn call_tool(name: &str, arguments: Value, state: &mut LoaderState) -> Result<Value, Diagnostic> {
    let object = arguments.as_object().cloned().unwrap_or_default();
    match name {
        "mcp_loader_guidance" => Ok(guidance_result(&object, state)),
        "mcp_loader_runtime_status" => Ok(runtime_freshness(state)),
        "mcp_loader_policy_inspect" => Ok(policy_inspect(state)),
        "mcp_loader_connection_inventory" => Ok(connection_inventory(state)),
        "mcp_loader_process_ownership" => Ok(process_ownership(state)),
        "mcp_loader_runtime_observation" => runtime_observation(&object, state),
        "mcp_loader_list_site_surfaces" => list_site_surfaces(&object, state),
        "mcp_loader_site_fabric_diagnostics" => site_fabric_diagnostics(&object, state),
        "mcp_loader_site_tool_inventory_check" => site_tool_inventory(&object, state),
        "mcp_loader_attach_surface" => attach_surface(&object, state),
        "mcp_loader_open_surface" => open_surface(&object, state),
        "mcp_loader_surface_handle_inventory" => Ok(surface_handle_inventory(state)),
        "mcp_loader_list_tools" => list_attached_tools(&object, state),
        "mcp_loader_surface_status" => surface_status(&object, state),
        "mcp_loader_tool_discovery_manifest" => tool_discovery_manifest(&object, state),
        "mcp_loader_call_tool" => call_attached_tool(&object, state),
        "mcp_loader_call_surface_tool" => call_surface_handle_tool(&object, state),
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
mod tests {
    use super::{
        child_error_diagnostic, compact_child_result, extract_proxy_child_args,
        request_error_details,
    };
    use serde_json::json;

    #[test]
    fn compact_child_result_removes_duplicate_text_when_structured_data_exists() {
        let child = json!({
            "content":[{"type":"text","text":"duplicate"}],
            "structuredContent":{"schema":"example.v1","status":"ok"},
            "isError":false
        });
        let compacted = compact_child_result(&child);
        assert!(compacted.get("content").is_none());
        assert_eq!(compacted["structuredContent"]["schema"], "example.v1");
        assert_eq!(compacted["isError"], false);
        let text_only = json!({"content":[{"type":"text","text":"only"}]});
        assert_eq!(compact_child_result(&text_only), text_only);
    }

    #[test]
    fn child_error_is_projected_once_as_domain_diagnostic() {
        let diagnostic = child_error_diagnostic(&json!({
            "code": -32000,
            "message": "git_push_head_mismatch",
            "data": {
                "code": "git_push_head_mismatch",
                "details": {"expected_commit": "a", "actual_head": "b"}
            }
        }));
        assert_eq!(diagnostic.code, "git_push_head_mismatch");
        assert_eq!(diagnostic.message, "git_push_head_mismatch");
        assert_eq!(diagnostic.details["child_jsonrpc_code"], -32000);
        assert_eq!(diagnostic.details["child_details"]["actual_head"], "b");
        assert!(diagnostic.details.get("child_error").is_none());
        let merged = request_error_details(&diagnostic.details, "tools/call", 120_000);
        assert_eq!(merged["child_details"]["expected_commit"], "a");
        assert_eq!(merged["method"], "tools/call");
        assert_eq!(merged["timeout_ms"], 120_000);
    }

    #[test]
    fn proxy_child_args_require_separator() {
        assert_eq!(extract_proxy_child_args(&["proxy".to_string()]), None);
        assert_eq!(
            extract_proxy_child_args(&[
                "proxy".to_string(),
                "--".to_string(),
                "--site-root".to_string()
            ]),
            Some(vec!["--site-root".to_string()]),
        );
    }
}

#[cfg(test)]
mod modern_protocol_tests {
    use super::*;

    #[test]
    fn modern_loader_results_are_self_describing() {
        let params = modern_request_params();
        assert!(is_modern_request(&params));
        assert!(validate_modern_request(&params).is_ok());
        let result = modernize_result(json!({"tools": []}), "tools/list");
        assert_eq!(result["resultType"], "complete");
        assert_eq!(result["cacheScope"], "public");
        assert!(result["ttlMs"].as_u64().unwrap_or_default() > 0);
        assert_eq!(
            result["_meta"]["io.modelcontextprotocol/serverInfo"]["name"],
            SERVER_NAME
        );
        let discovery = modernize_result(modern_discover_result(), "server/discover");
        assert_eq!(discovery["supportedVersions"][0], MODERN_PROTOCOL_VERSION);
        assert!(modern_discovery_is_valid(&discovery));
    }

    #[test]
    fn modern_loader_requests_require_client_metadata() {
        let missing =
            json!({"_meta": {"io.modelcontextprotocol/protocolVersion": MODERN_PROTOCOL_VERSION}});
        let error = validate_modern_request(&missing).expect_err("missing metadata must refuse");
        assert_eq!(error.code, "modern_metadata_required");
    }
}
