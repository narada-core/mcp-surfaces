use crate::protocol;
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::env;
use std::fs;
use std::fs::OpenOptions;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;

const PROTOCOL_VERSION: &str = "2024-11-05";
const READ_TIMEOUT_MS: u64 = 5_000;
const WRITE_TIMEOUT_MS: u64 = 10_000;
const SEARCH_TIMEOUT_MS: u64 = 60_000;
const MAX_READ_LINES: i64 = 1_000;
const MAX_READ_LINE_BYTES: usize = 1024 * 1024;
const MAX_READ_WINDOW_BYTES: usize = 4 * 1024 * 1024;
const MAX_SEARCH_CAPTURE_ENTRIES: usize = 10_000;
const MAX_SEARCH_CAPTURE_BYTES: usize = 32 * 1024 * 1024;
const MAX_SEARCH_LINE_BYTES: usize = 256 * 1024;
const MAX_TEXT_MUTATION_BYTES: u64 = 8 * 1024 * 1024;
const DEFAULT_GLOB_IGNORES: &[&str] = &[
    "**/.git/**",
    "**/node_modules/**",
    "**/dist/**",
    "**/build/**",
    "**/coverage/**",
    "**/.next/**",
    "**/.turbo/**",
    "**/.cache/**",
    "**/.venv/**",
    "**/__pycache__/**",
    "**/target/**",
];
const DEFAULT_GREP_IGNORES: &[&str] = &[
    "**/.git/**",
    "**/node_modules/**",
    "**/dist/**",
    "**/build/**",
    "**/coverage/**",
    "**/.next/**",
    "**/.turbo/**",
    "**/.cache/**",
    "**/.venv/**",
    "**/__pycache__/**",
    "**/target/**",
    "**/.ai/runtime/**",
    "**/.ai/tmp/**",
    "**/.ai/output/**",
    "**/.narada/runtime/**",
    "**/.narada/tmp/**",
    "**/.tmp-tests/**",
];
const GENERATED_MARKERS: &[&str] = &[
    "/.ai/runtime/",
    "/.ai/tmp/",
    "/.ai/output/",
    "/.narada/runtime/",
    "/.narada/tmp/",
    "/.narada/local-filesystem-mcp/patch-outcomes/",
    "/.tmp-tests/",
];
const TRANSIENT_EXECUTABLE_EXTENSIONS: &[&str] = &[
    ".cmd", ".bat", ".ps1", ".psm1", ".js", ".mjs", ".cjs", ".ts",
];

#[derive(Clone)]
pub(crate) struct State {
    mode: String,
    allowed_roots: Vec<PathBuf>,
    root_entries: Vec<Value>,
    output_root: PathBuf,
    audit_log_dir: Option<PathBuf>,
    cache: HashMap<String, (String, Vec<String>, bool)>,
    snapshots: HashMap<String, (Vec<String>, bool)>,
    snapshot_order: Vec<String>,
}

pub(crate) fn site_allowed_roots_config_path(output_root: &Path) -> PathBuf {
    let control_root = if output_root
        .file_name()
        .map(|name| name.to_string_lossy().eq_ignore_ascii_case(".narada"))
        .unwrap_or(false)
    {
        output_root.to_path_buf()
    } else {
        output_root.join(".narada")
    };
    control_root.join("allowed-roots.json")
}

fn parse_site_root_config(output_root: &Path, keys: &[&str]) -> Vec<String> {
    let path = site_allowed_roots_config_path(output_root);
    let Ok(text) = fs::read_to_string(path) else {
        return Vec::new();
    };
    let Ok(value) = serde_json::from_str::<Value>(&text) else {
        return Vec::new();
    };
    keys.iter()
        .flat_map(|key| {
            value
                .get(*key)
                .and_then(Value::as_array)
                .into_iter()
                .flat_map(|items| items.iter().filter_map(Value::as_str).map(str::to_string))
        })
        .collect()
}

pub(crate) fn parse_site_allowed_roots(output_root: &Path) -> Vec<String> {
    parse_site_root_config(output_root, &["extra_allowed_roots", "temp_allowed_roots"])
}

pub(crate) fn parse_site_extra_allowed_roots(output_root: &Path) -> Vec<String> {
    parse_site_root_config(output_root, &["extra_allowed_roots"])
}

#[derive(Debug)]
struct FsError {
    code: String,
    message: String,
    details: Value,
}

impl FsError {
    fn new(code: impl Into<String>, message: impl Into<String>, details: Value) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
            details,
        }
    }
}

pub fn run(args: &[String]) -> Result<(), String> {
    let mut state = parse_state(args)?;
    let server_name = format!("local-filesystem-{}", state.mode);
    let stdin = io::stdin();
    let mut reader = BufReader::new(stdin.lock());
    let stdout = io::stdout();
    let mut writer = stdout.lock();
    loop {
        let Some((request, framed)) = read_message(&mut reader).map_err(|e| e.to_string())? else {
            break;
        };
        if request.get("id").is_none() {
            continue;
        }
        if let Some(response) = protocol::preflight_response(&request, &server_name) {
            write_message(&mut writer, &response, framed).map_err(|e| e.to_string())?;
            writer.flush().map_err(|e| e.to_string())?;
            continue;
        }
        if let Some(response) = handle_request(&mut state, &request) {
            let response = protocol::modernize_response(&request, response, &server_name);
            write_message(&mut writer, &response, framed).map_err(|e| e.to_string())?;
            writer.flush().map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}

fn parse_state(args: &[String]) -> Result<State, String> {
    let mut mode = "read".to_string();
    let mut roots = Vec::<String>::new();
    let mut anchored = Vec::<String>::new();
    let mut roots_config: Option<String> = None;
    let mut output_root: Option<String> = None;
    let mut audit_log_dir: Option<String> = None;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--mode" => {
                index += 1;
                mode = args.get(index).cloned().ok_or("filesystem_mode_required")?;
            }
            "--allowed-root" => {
                index += 1;
                roots.push(
                    args.get(index)
                        .cloned()
                        .ok_or("filesystem_allowed_root_required")?,
                );
            }
            "--anchored-allowed-root" => {
                index += 1;
                anchored.push(
                    args.get(index)
                        .cloned()
                        .ok_or("filesystem_anchored_root_required")?,
                );
            }
            "--roots-config" => {
                index += 1;
                roots_config = Some(
                    args.get(index)
                        .cloned()
                        .ok_or("filesystem_roots_config_required")?,
                );
            }
            "--output-root" => {
                index += 1;
                output_root = Some(
                    args.get(index)
                        .cloned()
                        .ok_or("filesystem_output_root_required")?,
                );
            }
            "--audit-log-dir" => {
                index += 1;
                audit_log_dir = Some(
                    args.get(index)
                        .cloned()
                        .ok_or("filesystem_audit_log_dir_required")?,
                );
            }
            "--roots-from-trust-config" | "--roots-from-codex-config" => {
                index += 1;
                let path = args
                    .get(index)
                    .cloned()
                    .ok_or("filesystem_trust_config_required")?;
                roots.extend(parse_trust_config(Path::new(&path)));
            }
            "--help" => return Err("filesystem_help".to_string()),
            other => return Err(format!("filesystem_unknown_argument:{other}")),
        }
        index += 1;
    }
    let output_root = absolute(
        output_root
            .map(PathBuf::from)
            .unwrap_or_else(|| env::current_dir().unwrap_or_else(|_| PathBuf::from("."))),
    );
    let mut root_specs = roots
        .into_iter()
        .map(|root| {
            (
                root,
                json!({"source": "explicit_flag", "flag": "--allowed-root"}),
            )
        })
        .collect::<Vec<_>>();
    if let Some(path) = roots_config {
        let config_path = PathBuf::from(path);
        let config_path_text = config_path.to_string_lossy().to_string();
        root_specs.extend(parse_roots_config(&config_path).into_iter().map(|root| {
            (
                root,
                json!({"source": "roots_config", "config_path": config_path_text.clone()}),
            )
        }));
    }
    let site_config_path = site_allowed_roots_config_path(&output_root);
    root_specs.extend(parse_site_allowed_roots(&output_root).into_iter().map(|root| {
        (
            root,
            json!({"source": "site_allowed_roots_config", "config_path": site_config_path.to_string_lossy()}),
        )
    }));
    for spec in anchored {
        root_specs.push((
            resolve_anchor(&spec)?,
            json!({"source": "anchored_allowed_root", "flag": "--anchored-allowed-root", "spec": spec}),
        ));
    }
    if mode != "read" && mode != "write" {
        return Err("filesystem_mode_must_be_read_or_write".to_string());
    }
    let mut entries = Vec::new();
    let mut allowed_roots = Vec::new();
    for (root, provenance) in root_specs {
        let path = absolute(PathBuf::from(root));
        let key = normalize_path(&path);
        if allowed_roots
            .iter()
            .any(|value: &PathBuf| normalize_path(value) == key)
        {
            continue;
        }
        entries.push(json!({"root": path.to_string_lossy(), "provenance": provenance}));
        allowed_roots.push(path);
    }
    if allowed_roots.is_empty() {
        return Err("filesystem_mcp_requires_at_least_one_allowed_root".to_string());
    }
    Ok(State {
        mode,
        allowed_roots,
        root_entries: entries,
        output_root,
        audit_log_dir: audit_log_dir.map(|value| absolute(PathBuf::from(value))),
        cache: HashMap::new(),
        snapshots: HashMap::new(),
        snapshot_order: Vec::new(),
    })
}
