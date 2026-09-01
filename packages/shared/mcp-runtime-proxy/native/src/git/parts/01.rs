use crate::filesystem::{parse_site_extra_allowed_roots, read_message, write_message};
use crate::protocol;
use fs2::FileExt;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::env;
use std::fs::{self, OpenOptions};
use std::io::{self, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    mpsc, Arc, Mutex,
};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use sha2::{Digest, Sha256};
use time::{format_description::well_known::Rfc3339, Duration as TimeDuration, OffsetDateTime};

const PROTOCOL_VERSION: &str = "2024-11-05";
const DEFAULT_MAX_TIMEOUT_MS: u64 = 60_000;
const DEFAULT_MAX_OUTPUT_BYTES: usize = 2 * 1024 * 1024;
const MAX_OUTPUT_BYTES: usize = 20 * 1024 * 1024;
const PREVIEW_CHAR_LIMIT: usize = 1_000;
const WORK_SCOPE_TTL_MINUTES: i64 = 15;

#[derive(Clone)]
struct WorkScope {
    reference: String,
    repository_root: String,
    owner_id: String,
    authority: String,
    allowed_paths: Vec<String>,
    base_state: Value,
    created_at: String,
    expires_at: OffsetDateTime,
}

fn work_scope_error(error: impl std::fmt::Display) -> GitError {
    GitError::new(
        "git_work_scope_store_unavailable",
        "git_work_scope_store_unavailable",
        json!({"error": error.to_string()}),
    )
}

fn work_scope_value(scope: &WorkScope) -> Value {
    json!({
        "reference": scope.reference,
        "repository_root": scope.repository_root,
        "owner_id": scope.owner_id,
        "authority": scope.authority,
        "allowed_paths": scope.allowed_paths,
        "base_state": scope.base_state,
        "created_at": scope.created_at,
        "expires_at": scope.expires_at.format(&Rfc3339).unwrap_or_default(),
    })
}

fn work_scope_from_value(value: Value) -> Result<WorkScope, GitError> {
    let field = |name: &str| {
        value.get(name).and_then(Value::as_str).map(str::to_string).ok_or_else(|| {
            work_scope_error(format!("stored work scope is missing {name}"))
        })
    };
    let expires_at = OffsetDateTime::parse(&field("expires_at")?, &Rfc3339)
        .map_err(work_scope_error)?;
    let allowed_paths = value
        .get("allowed_paths")
        .and_then(Value::as_array)
        .ok_or_else(|| work_scope_error("stored work scope is missing allowed_paths"))?
        .iter()
        .map(|item| item.as_str().map(str::to_string).ok_or_else(|| work_scope_error("stored allowed path is not a string")))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(WorkScope {
        reference: field("reference")?,
        repository_root: field("repository_root")?,
        owner_id: field("owner_id")?,
        authority: field("authority")?,
        allowed_paths,
        base_state: value.get("base_state").cloned().unwrap_or(Value::Null),
        created_at: field("created_at")?,
        expires_at,
    })
}

fn with_work_scope_lock<T>(state: &State, action: impl FnOnce(&Path) -> Result<T, GitError>) -> Result<T, GitError> {
    fs::create_dir_all(&state.work_scope_store).map_err(work_scope_error)?;
    let lock_path = state.work_scope_store.join("store.lock");
    let lock = OpenOptions::new().create(true).read(true).write(true).open(lock_path).map_err(work_scope_error)?;
    lock.lock_exclusive().map_err(work_scope_error)?;
    let result = action(&state.work_scope_store);
    fs2::FileExt::unlock(&lock).map_err(work_scope_error)?;
    result
}

fn load_work_scopes_unlocked(store: &Path) -> Result<HashMap<String, WorkScope>, GitError> {
    let mut scopes = HashMap::new();
    for entry in fs::read_dir(store).map_err(work_scope_error)? {
        let path = entry.map_err(work_scope_error)?.path();
        if path.extension().and_then(|value| value.to_str()) != Some("json") {
            continue;
        }
        let value: Value = serde_json::from_slice(&fs::read(&path).map_err(work_scope_error)?)
            .map_err(work_scope_error)?;
        let scope = work_scope_from_value(value)?;
        scopes.insert(scope.reference.clone(), scope);
    }
    Ok(scopes)
}

fn persist_work_scope_unlocked(store: &Path, scope: &WorkScope) -> Result<(), GitError> {
    let target = store.join(format!("{}.json", scope.reference));
    let temporary = store.join(format!("{}.tmp", scope.reference));
    fs::write(&temporary, serde_json::to_vec(&work_scope_value(scope)).map_err(work_scope_error)?)
        .map_err(work_scope_error)?;
    fs::rename(&temporary, &target).map_err(work_scope_error)
}

fn remove_work_scope_unlocked(store: &Path, reference: &str) -> Result<(), GitError> {
    let path = store.join(format!("{reference}.json"));
    if path.exists() {
        fs::remove_file(path).map_err(work_scope_error)?;
    }
    Ok(())
}

#[derive(Clone)]
struct State {
    mode: String,
    allowed_roots: Vec<PathBuf>,
    max_timeout_ms: u64,
    max_output_bytes: usize,
    output_root: PathBuf,
    env: HashMap<String, String>,
    work_scope_store: PathBuf,
    git_write_lock: Arc<Mutex<()>>,
}

#[derive(Debug)]
struct GitError {
    code: String,
    message: String,
    details: Value,
}

impl GitError {
    fn new(code: impl Into<String>, message: impl Into<String>, details: Value) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
            details,
        }
    }
}

#[derive(Clone)]
struct GitResult {
    exit_code: Option<i32>,
    output_text: String,
    diagnostic_text: String,
    timed_out: bool,
    cancelled: bool,
    output_truncated: bool,
    diagnostic_truncated: bool,
}

enum Event {
    Request(Value, bool),
    Response(Value, bool, String),
    InputClosed,
}

pub fn run(args: &[String]) -> Result<(), String> {
    let state = parse_state(args)?;
    let (events_tx, events_rx) = mpsc::channel::<Event>();
    let reader_tx = events_tx.clone();
    thread::spawn(move || {
        let stdin = io::stdin();
        let mut reader = BufReader::new(stdin.lock());
        loop {
            match read_message(&mut reader) {
                Ok(Some((request, framed))) => {
                    if reader_tx.send(Event::Request(request, framed)).is_err() {
                        return;
                    }
                }
                Ok(None) | Err(_) => {
                    let _ = reader_tx.send(Event::InputClosed);
                    return;
                }
            }
        }
    });
    let stdout = io::stdout();
    let mut writer = stdout.lock();
    let mut active = HashMap::<String, Arc<AtomicBool>>::new();
    let mut input_closed = false;
    while let Ok(event) = events_rx.recv() {
        match event {
            Event::Request(request, framed) => {
                let method = request
                    .get("method")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                if method == "notifications/cancelled" {
                    let request_id = request
                        .get("params")
                        .and_then(|params| params.get("requestId"))
                        .map(value_key)
                        .unwrap_or_default();
                    if let Some(token) = active.get(&request_id) {
                        token.store(true, Ordering::Release);
                    }
                    continue;
                }
                if request.get("id").is_none() {
                    continue;
                }
                if let Some(response) = protocol::preflight_response(&request, "git-mcp") {
                    write_message(&mut writer, &response, framed)
                        .map_err(|error| error.to_string())?;
                    writer.flush().map_err(|error| error.to_string())?;
                    continue;
                }
                if method == "tools/call" {
                    let id = request.get("id").cloned().unwrap_or(Value::Null);
                    let key = value_key(&id);
                    let token = Arc::new(AtomicBool::new(false));
                    active.insert(key.clone(), token.clone());
                    let state_clone = state.clone();
                    let response_tx = events_tx.clone();
                    thread::spawn(move || {
                        let response = handle_request(&state_clone, &request, Some(token)).unwrap_or_else(|| json!({"jsonrpc": "2.0", "id": request.get("id").cloned().unwrap_or(Value::Null), "result": {}}));
                        let response = protocol::modernize_response(&request, response, "git-mcp");
                        let _ = response_tx.send(Event::Response(response, framed, key));
                    });
                } else if let Some(response) = handle_request(&state, &request, None)
                    .map(|response| protocol::modernize_response(&request, response, "git-mcp"))
                {
                    write_message(&mut writer, &response, framed)
                        .map_err(|error| error.to_string())?;
                    writer.flush().map_err(|error| error.to_string())?;
                }
            }
            Event::Response(response, framed, key) => {
                active.remove(&key);
                write_message(&mut writer, &response, framed).map_err(|error| error.to_string())?;
                writer.flush().map_err(|error| error.to_string())?;
            }
            Event::InputClosed => {
                input_closed = true;
                if active.is_empty() {
                    break;
                }
            }
        }
        if input_closed && active.is_empty() {
            break;
        }
    }
    Ok(())
}
