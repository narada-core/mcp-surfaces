use narada_mcp_materialization_contract::{
    describe_config, generation_fingerprint, pretty_json, CONTRACT_VERSION,
};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::env;
use std::fs;
use std::io::{self, BufRead, Read, Write};
use std::path::{Path, PathBuf};

const CONTRACT: &[u8] = include_bytes!("../../../tool-catalog.json.gz");
const LEGACY_PROTOCOL_VERSION: &str = "2024-11-05";
const MODERN_PROTOCOL_VERSION: &str = "2026-07-28";
const MAX_MESSAGE_BYTES: usize = 4 * 1024 * 1024;
const MAX_HEADER_BYTES: usize = 64 * 1024;

fn main() {
    if let Err(error) = run() {
        eprintln!("{error}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), String> {
    let stdin = io::stdin();
    let mut input = stdin.lock();
    let stdout = io::stdout();
    let mut output = stdout.lock();
    loop {
        let Some(request) = read_message(&mut input)? else {
            break;
        };
        if request
            .get("method")
            .and_then(Value::as_str)
            .is_some_and(|v| v.starts_with("notifications/"))
        {
            continue;
        }
        let response = dispatch(&request);
        let body = serde_json::to_vec(&response).map_err(|e| e.to_string())?;
        write!(output, "Content-Length: {}\r\n\r\n", body.len()).map_err(|e| e.to_string())?;
        output.write_all(&body).map_err(|e| e.to_string())?;
        output.flush().map_err(|e| e.to_string())?;
    }
    Ok(())
}

fn decode_contract() -> Result<Value, String> {
    let mut decoder = flate2::read::GzDecoder::new(CONTRACT);
    let mut bytes = Vec::new();
    decoder
        .read_to_end(&mut bytes)
        .map_err(|error| error.to_string())?;
    serde_json::from_slice(&bytes).map_err(|error| error.to_string())
}

