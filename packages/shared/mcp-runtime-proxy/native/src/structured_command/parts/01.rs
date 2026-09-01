use crate::filesystem::{parse_site_extra_allowed_roots, read_message, write_message};
use crate::protocol;
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use std::env;
use std::fs::{self, OpenOptions};
use std::io::{self, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
#[cfg(windows)]
use std::os::windows::process::CommandExt;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    mpsc, Arc,
};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;

const PROTOCOL_VERSION: &str = "2024-11-05";
const DEFAULT_MAX_TIMEOUT_MS: u64 = 900_000;
const MAX_SYNCHRONOUS_TIMEOUT_MS: u64 = 240_000;
const DEFAULT_MAX_OUTPUT_BYTES: usize = 1024 * 1024;
const MAX_OUTPUT_BYTES: usize = 20 * 1024 * 1024;
const DEFAULT_ALLOWED_COMMANDS: &[&str] = &["railway", "wrangler"];
const DEFAULT_ALLOWED_PREFIXES: &[&[&str]] = &[
    &["pnpm", "test"],
    &["pnpm", "build"],
    &["pnpm", "typecheck"],
    &["pnpm", "--filter"],
    &["cargo", "fmt"],
    &["cargo", "check"],
    &["cargo", "test"],
    &["cargo", "build"],
    &["cargo", "run"],
    &["cargo", "native-build"],
    &["cargo", "native-test"],
    &["cargo", "native-package"],
    &["cargo", "native-materialize"],
    &["cargo", "native-release"],
    &["cargo", "native-verify"],
    &["pwsh", "-file"],
    &["pwsh", "-noprofile", "-file"],
    &["pwsh", "-noprofile", "-executionpolicy", "bypass", "-file"],
];
const DEFAULT_BLOCKED_COMMANDS: &[&str] = &[
    "cmd",
    "cmd.exe",
    "powershell",
    "powershell.exe",
    "wsl",
    "wsl.exe",
    "wt",
    "wt.exe",
    "windowsterminal",
    "windowsterminal.exe",
    "openconsole",
    "openconsole.exe",
];
const TERMINAL_INTEGRATION_ENVIRONMENT: &[&str] = &[
    "WT_SESSION",
    "WT_PROFILE_ID",
    "TERM_PROGRAM",
    "TERM_PROGRAM_VERSION",
];
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;
const TRANSIENT_EXTENSIONS: &[&str] = &[".ps1", ".psm1", ".js", ".mjs", ".cjs", ".ts"];

#[derive(Clone)]
struct State {
    allowed_roots: Vec<PathBuf>,
    allowed_commands: Vec<String>,
    allowed_prefixes: Vec<Vec<String>>,
    blocked_commands: Vec<String>,
    max_timeout_ms: u64,
    max_output_bytes: usize,
    audit_log_dir: Option<PathBuf>,
    site_root: PathBuf,
    storage_root: PathBuf,
    env: std::collections::HashMap<String, String>,
}

#[derive(Debug)]
struct StructuredError {
    code: String,
    message: String,
    details: Value,
}

impl StructuredError {
    fn new(code: impl Into<String>, message: impl Into<String>, details: Value) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
            details,
        }
    }
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
    let mut active = std::collections::HashMap::<String, Arc<AtomicBool>>::new();
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
                if let Some(response) =
                    protocol::preflight_response(&request, "structured-command-mcp")
                {
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
                        let response = protocol::modernize_response(
                            &request,
                            response,
                            "structured-command-mcp",
                        );
                        let _ = response_tx.send(Event::Response(response, framed, key));
                    });
                } else if let Some(response) =
                    handle_request(&state, &request, None).map(|response| {
                        protocol::modernize_response(
                            &request,
                            response,
                            "structured-command-mcp",
                        )
                    })
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
