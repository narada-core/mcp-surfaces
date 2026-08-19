//! `narada-ledger-domain`: generic ledger-domain MCP server.
//!
//! Hosts one `narada.ledger-domain.v1` descriptor (`--domain <path>`) as a
//! complete event-ledger MCP surface rooted at `--site-root <path>`. Both
//! flags are required. The process speaks newline-delimited JSON-RPC and
//! Content-Length framed MCP, with the legacy 2024-11-05 and modern
//! 2026-07-28 protocol shapes, matching the monolithic native shell.

use jsonschema::validator_for;
use narada_ledger_domain::descriptor::Descriptor;
use narada_ledger_domain::engine::Engine;
use serde_json::{json, Map, Value};
use std::env;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::path::PathBuf;

const MAX_MCP_REQUEST_BYTES: usize = 4 * 1024 * 1024;
const MAX_MCP_HEADER_BYTES: usize = 64 * 1024;

const LEGACY_PROTOCOL_VERSION: &str = "2024-11-05";
const MODERN_PROTOCOL_VERSION: &str = "2026-07-28";

struct Server {
    engine: Engine,
    site_root: PathBuf,
    server_name: String,
}

fn main() {
    if let Err(error) = run() {
        let _ = writeln!(io::stderr(), "{error}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), String> {
    let arguments = env::args().skip(1).collect::<Vec<_>>();
    let (domain_path, site_root) = parse_options(&arguments)?;
    let descriptor = Descriptor::load(&domain_path)?;
    let server_name = format!("{}-mcp", descriptor.identity.domain_id);
    let engine = Engine::new(descriptor)?;
    let server = Server {
        engine,
        site_root,
        server_name,
    };
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
        if let Some(response) = handle_request(&request, &server) {
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

fn parse_options(arguments: &[String]) -> Result<(PathBuf, PathBuf), String> {
    let mut domain = None;
    let mut site_root = None;
    let mut index = 0;
    while index < arguments.len() {
        match arguments[index].as_str() {
            "--domain" => {
                index += 1;
                domain = arguments.get(index).map(PathBuf::from);
            }
            "--site-root" => {
                index += 1;
                site_root = arguments.get(index).map(PathBuf::from);
            }
            other => return Err(format!("native_surface_unknown_argument:{other}")),
        }
        index += 1;
    }
    let domain = domain.ok_or("native_surface_missing_domain")?;
    let site_root = site_root.ok_or("native_surface_missing_site_root")?;
    Ok((domain, site_root))
}

fn handle_request(request: &Value, server: &Server) -> Option<Value> {
    let object = request.as_object()?;
    let method = object
        .get("method")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if method.starts_with("notifications/") {
        return None;
    }
    let id = object.get("id").cloned().unwrap_or(Value::Null);
    let params = object
        .get("params")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    let modern = is_modern_request(&params);
    let result = if modern {
        validate_modern_request(&params).and_then(|_| match method {
            "server/discover" => Ok(server_discover_result(server)),
            "tools/list" => Ok(modern_result(
                json!({
                    "tools": list_tools(&server.engine),
                    "ttlMs": 300_000,
                    "cacheScope": "public"
                }),
                server,
            )),
            "tools/call" => call_tool(server, &params).map(|value| modern_result(value, server)),
            "initialize" => Err(diagnostic(
                "initialize_removed",
                "The 2026-07-28 protocol has no initialize handshake.",
                json!({ "protocolVersion": MODERN_PROTOCOL_VERSION }),
            )),
            _ => Err(diagnostic(
                "unsupported_mcp_method",
                &format!("unsupported_mcp_method:{method}"),
                json!({ "method": method }),
            )),
        })
    } else {
        match method {
            "initialize" => Ok(initialize_result(server)),
            "tools/list" => Ok(json!({ "tools": list_tools(&server.engine) })),
            "tools/call" => call_tool(server, &params),
            "server/discover" => Err(diagnostic(
                "modern_metadata_required",
                "server/discover requires 2026-07-28 request metadata.",
                json!({ "protocolVersion": MODERN_PROTOCOL_VERSION }),
            )),
            _ => Err(diagnostic(
                "unsupported_mcp_method",
                &format!("unsupported_mcp_method:{method}"),
                json!({ "method": method }),
            )),
        }
    };
    Some(match result {
        Ok(result) => json!({ "jsonrpc": "2.0", "id": id, "result": result }),
        Err(error) => {
            json!({ "jsonrpc": "2.0", "id": id, "error": { "code": -32000, "message": error["message"], "data": error } })
        }
    })
}

fn is_modern_request(params: &Map<String, Value>) -> bool {
    params
        .get("_meta")
        .and_then(Value::as_object)
        .and_then(|meta| meta.get("io.modelcontextprotocol/protocolVersion"))
        .and_then(Value::as_str)
        == Some(MODERN_PROTOCOL_VERSION)
}

fn validate_modern_request(params: &Map<String, Value>) -> Result<(), Value> {
    let meta = params
        .get("_meta")
        .and_then(Value::as_object)
        .ok_or_else(|| {
            diagnostic(
                "modern_metadata_required",
                "Modern MCP requests require _meta.",
                Value::Null,
            )
        })?;
    if meta
        .get("io.modelcontextprotocol/clientInfo")
        .and_then(Value::as_object)
        .is_none()
    {
        return Err(diagnostic(
            "modern_metadata_required",
            "Modern MCP requests require clientInfo metadata.",
            json!({ "key": "io.modelcontextprotocol/clientInfo" }),
        ));
    }
    if meta
        .get("io.modelcontextprotocol/clientCapabilities")
        .and_then(Value::as_object)
        .is_none()
    {
        return Err(diagnostic(
            "modern_metadata_required",
            "Modern MCP requests require clientCapabilities metadata.",
            json!({ "key": "io.modelcontextprotocol/clientCapabilities" }),
        ));
    }
    Ok(())
}

fn capabilities() -> Value {
    json!({"tools":{}})
}

fn initialize_result(server: &Server) -> Value {
    json!({
        "protocolVersion": LEGACY_PROTOCOL_VERSION,
        "capabilities": capabilities(),
        "serverInfo": { "name": server.server_name, "version": "0.1.0" }
    })
}

fn server_discover_result(server: &Server) -> Value {
    modern_result(
        json!({
            "supportedVersions": [MODERN_PROTOCOL_VERSION, LEGACY_PROTOCOL_VERSION],
            "capabilities": capabilities(),
            "ttlMs": 3_600_000,
            "cacheScope": "public"
        }),
        server,
    )
}

fn modern_result(value: Value, server: &Server) -> Value {
    let mut result = value.as_object().cloned().unwrap_or_default();
    result.insert("resultType".to_string(), json!("complete"));
    let mut meta = result
        .remove("_meta")
        .and_then(|value| value.as_object().cloned())
        .unwrap_or_default();
    meta.insert(
        "io.modelcontextprotocol/serverInfo".to_string(),
        json!({ "name": server.server_name, "version": "0.1.0" }),
    );
    result.insert("_meta".to_string(), Value::Object(meta));
    Value::Object(result)
}

fn list_tools(engine: &Engine) -> Vec<Value> {
    let mut tools = engine.list_tools();
    for tool in &mut tools {
        let name = tool
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or("mcp_tool")
            .to_string();
        if let Some(schema) = tool.get_mut("inputSchema") {
            normalize_input_schema(schema, Some(&name));
            if let Some(object) = schema.as_object_mut() {
                object.insert("title".to_string(), json!(format!("{name}.input")));
                object.insert("additionalProperties".to_string(), Value::Bool(false));
            }
        }
    }
    tools
}

fn normalize_input_schema(schema: &mut Value, field: Option<&str>) {
    let Some(object) = schema.as_object_mut() else {
        return;
    };
    match object.get("type").and_then(Value::as_str) {
        Some("string") if !object.contains_key("maxLength") && !object.contains_key("enum") => {
            let name = field.unwrap_or_default().to_ascii_lowercase();
            let maximum = if name.contains("path") || name.contains("root") || name.contains("file")
            {
                4096
            } else if name.contains("summary")
                || name.contains("body")
                || name.contains("context")
                || name.contains("output")
            {
                32768
            } else {
                8192
            };
            object.insert("maxLength".to_string(), json!(maximum));
        }
        Some("array") if !object.contains_key("maxItems") => {
            object.insert("maxItems".to_string(), json!(500));
        }
        Some("object") if !object.contains_key("maxProperties") => {
            object.insert("maxProperties".to_string(), json!(256));
        }
        _ => {}
    }
    if let Some(properties) = object.get_mut("properties").and_then(Value::as_object_mut) {
        for (name, child) in properties {
            normalize_input_schema(child, Some(name));
        }
    }
    if let Some(items) = object.get_mut("items") {
        normalize_input_schema(items, field);
    }
    for keyword in ["allOf", "anyOf", "oneOf"] {
        if let Some(branches) = object.get_mut(keyword).and_then(Value::as_array_mut) {
            for branch in branches {
                normalize_input_schema(branch, field);
            }
        }
    }
}

fn call_tool(server: &Server, params: &Map<String, Value>) -> Result<Value, Value> {
    let name = params.get("name").and_then(Value::as_str).ok_or_else(|| {
        diagnostic(
            "invalid_request",
            "tools/call requires a tool name.",
            Value::Null,
        )
    })?;
    let args = params
        .get("arguments")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    let tool = list_tools(&server.engine)
        .into_iter()
        .find(|tool| tool.get("name").and_then(Value::as_str) == Some(name))
        .ok_or_else(|| diagnostic("unknown_tool", &format!("unknown_tool:{name}"), Value::Null))?;
    validate_input_schema(
        tool.get("inputSchema").unwrap_or(&Value::Null),
        &Value::Object(args.clone()),
        "/arguments",
    )?;
    let result = server
        .engine
        .call_tool(name, &args, &server.site_root)
        .map_err(|error| error)?;
    let is_error = result.get("status").and_then(Value::as_str) == Some("unavailable");
    let mut response = json!({ "content": [{ "type": "text", "text": serde_json::to_string_pretty(&result).unwrap_or_else(|_| result.to_string()) }], "structuredContent": result });
    if is_error {
        response["isError"] = json!(true);
    }
    Ok(response)
}

fn validate_input_schema(schema: &Value, value: &Value, path: &str) -> Result<(), Value> {
    let validator = validator_for(schema).map_err(|error| {
        diagnostic(
            "input_schema_invalid",
            "input_schema_invalid",
            json!({"path":path,"message":error.to_string()}),
        )
    })?;
    let error = validator.iter_errors(value).next();
    match error {
        None => Ok(()),
        Some(error) => Err(diagnostic(
            "input_schema_validation_failed",
            &format!("input_schema_validation_failed:{path}"),
            json!({"path":path,"message":error.to_string()}),
        )),
    }
}

fn diagnostic(code: &str, message: &str, details: Value) -> Value {
    let mut object = Map::new();
    object.insert("code".to_string(), Value::String(code.to_string()));
    object.insert("message".to_string(), Value::String(message.to_string()));
    if !details.is_null() {
        object.insert("details".to_string(), details);
    }
    Value::Object(object)
}
