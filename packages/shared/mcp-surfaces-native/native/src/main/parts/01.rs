use jsonschema::validator_for;
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use std::env;
use std::fs::{create_dir_all, OpenOptions};
use std::io::{self, BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;
use uuid::Uuid;

const MAX_MCP_REQUEST_BYTES: usize = 4 * 1024 * 1024;
const MAX_MCP_HEADER_BYTES: usize = 64 * 1024;


const LEGACY_PROTOCOL_VERSION: &str = "2024-11-05";
const MODERN_PROTOCOL_VERSION: &str = "2026-07-28";

#[derive(Clone, Debug)]
struct Options {
    surface_id: String,
    site_root: PathBuf,
    allowed_roots: Vec<PathBuf>,
    log_root: Option<PathBuf>,
    registry_path: Option<PathBuf>,
    native_authority: bool,
    environment: Vec<(String, String)>,
}

fn main() {
    let arguments = env::args().skip(1).collect::<Vec<_>>();
    if quota_authority::is_query_mode(&arguments) {
        if let Err(error) = quota_authority::run_query_mode(&arguments) {
            let _ = writeln!(io::stderr(), "{error}");
            std::process::exit(1);
        }
        return;
    }
    if operator_console_runtime::is_host_mode(&arguments) {
        if let Err(error) = operator_console_runtime::run(&arguments) {
            let _ = writeln!(io::stderr(), "{error}");
            std::process::exit(1);
        }
        return;
    }
    if let Err(error) = run() {
        let _ = writeln!(io::stderr(), "{error}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), String> {
    let options = parse_options(env::args().skip(1).collect())?;
    let _speech_shutdown = (options.surface_id == "speech").then(speech_authority::shutdown_guard);
    let _browser_shutdown =
        (options.surface_id == "browser-control").then(browser_control_authority::shutdown_guard);
    for (key, value) in &options.environment {
        env::set_var(key, value);
    }
    if options.native_authority && options.surface_id == "calendar" {
        env::set_var("NARADA_NATIVE_GRAPH_AUTHORITY", "1");
    }
    if options.native_authority && options.surface_id == "graph-mail" {
        env::set_var("NARADA_NATIVE_GRAPH_MAIL_AUTHORITY", "1");
    }
    let stdin = io::stdin();
    let mut reader = BufReader::new(stdin.lock());
    let mut stdout = io::stdout().lock();
    loop {
        let Some(first) = read_line_bounded(&mut reader, MAX_MCP_REQUEST_BYTES)? else {
            break;
        };
        if first.trim().is_empty() {
            continue;
        }
        let (body, framed) = if first.to_ascii_lowercase().starts_with("content-length:") {
            let mut header = first;
            if header.len() > MAX_MCP_HEADER_BYTES {
                return Err("native_surface_header_too_large".to_string());
            }
            while !header.contains("\r\n\r\n") && !header.contains("\n\n") {
                let Some(line) = read_line_bounded(&mut reader, MAX_MCP_HEADER_BYTES)? else {
                    return Err("native_surface_incomplete_content_length_header".to_string());
                };
                header.push_str(&line);
                if header.len() > MAX_MCP_HEADER_BYTES {
                    return Err("native_surface_header_too_large".to_string());
                }
            }
            let length = header
                .lines()
                .find_map(|line| {
                    line.to_ascii_lowercase()
                        .strip_prefix("content-length:")
                        .map(|value| value.trim().to_string())
                })
                .ok_or("native_surface_content_length_missing")?
                .parse::<usize>()
                .map_err(|_| "native_surface_content_length_invalid".to_string())?;
            if length > MAX_MCP_REQUEST_BYTES {
                return Err("native_surface_request_too_large".to_string());
            }
            let mut body = vec![0_u8; length];
            reader
                .read_exact(&mut body)
                .map_err(|error| format!("native_surface_content_length_read_failed:{error}"))?;
            (body, true)
        } else {
            (first.into_bytes(), false)
        };
        let request: Value = serde_json::from_slice(&body)
            .map_err(|error| format!("native_surface_invalid_json:{error}"))?;
        if let Some(response) = handle_request(&request, &options) {
            let encoded = serde_json::to_string(&response)
                .map_err(|error| format!("native_surface_response_encode_failed:{error}"))?;
            if framed {
                write!(
                    stdout,
                    "Content-Length: {}\r\n\r\n{encoded}",
                    encoded.as_bytes().len()
                )
                .map_err(|error| format!("native_surface_stdout_write_failed:{error}"))?;
            } else {
                writeln!(stdout, "{encoded}")
                    .map_err(|error| format!("native_surface_stdout_write_failed:{error}"))?;
            }
            stdout
                .flush()
                .map_err(|error| format!("native_surface_stdout_flush_failed:{error}"))?;
        }
    }
    Ok(())
}

fn read_line_bounded<R: BufRead>(reader: &mut R, maximum: usize) -> Result<Option<String>, String> {
    let mut bytes = Vec::new();
    let count = reader
        .take((maximum + 1) as u64)
        .read_until(b'\n', &mut bytes)
        .map_err(|error| format!("native_surface_stdin_read_failed:{error}"))?;
    if count == 0 {
        return Ok(None);
    }
    if bytes.len() > maximum || !bytes.ends_with(b"\n") {
        return Err("native_surface_request_line_too_large".to_string());
    }
    String::from_utf8(bytes)
        .map(Some)
        .map_err(|_| "native_surface_request_invalid_utf8".to_string())
}
