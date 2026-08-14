use serde_json::{json, Value};
use std::env;
use std::io::{self, BufRead, Write};
use std::path::PathBuf;

mod contract;
mod materialization;
mod orientation;
mod state;

const LEGACY_PROTOCOL_VERSION: &str = "2024-11-05";
const MODERN_PROTOCOL_VERSION: &str = "2026-07-28";
const MAX_REQUEST_BYTES: usize = 2 * 1024 * 1024;
const MAX_HEADER_BYTES: usize = 64 * 1024;

fn main() {
    if let Err(error) = run() {
        eprintln!("{error}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), String> {
    let options = options(env::args().skip(1).collect())?;
    let projection = options.projection.clone();
    let context = state::Context::new(options.site_root, options.site_id)?;
    context.prepare()?;
    let tools = contract::tools(&projection)?;
    let stdin = io::stdin();
    let mut reader = stdin.lock();
    let mut stdout = io::stdout().lock();
    while let Some(body) = read_message(&mut reader)? {
        let request: Value = serde_json::from_slice(&body)
            .map_err(|error| format!("agent_context_native_request_invalid:{error}"))?;
        let id = request.get("id").cloned().unwrap_or(Value::Null);
        let method = request.get("method").and_then(Value::as_str).unwrap_or("");
        let modern = request
            .pointer("/params/_meta/io.modelcontextprotocol~1protocolVersion")
            .and_then(Value::as_str)
            == Some(MODERN_PROTOCOL_VERSION);
        if let Some(version) = request
            .pointer("/params/_meta/io.modelcontextprotocol~1protocolVersion")
            .and_then(Value::as_str)
        {
            let metadata_error = if version != MODERN_PROTOCOL_VERSION {
                Some(format!(
                    "agent_context_protocol_version_unsupported:{version}"
                ))
            } else {
                validate_modern_metadata(&request).err()
            };
            if let Some(message) = metadata_error {
                write_message(
                    &mut stdout,
                    &json!({"jsonrpc":"2.0","id":id,"error":{"code":-32602,"message":message}}),
                )?;
                stdout
                    .flush()
                    .map_err(|error| format!("agent_context_native_flush_failed:{error}"))?;
                continue;
            }
        }
        let response = match method {
            "initialize" => {
                json!({"jsonrpc":"2.0","id":id,"result":{"protocolVersion":LEGACY_PROTOCOL_VERSION,"capabilities":capabilities(),"serverInfo":server_info()}})
            }
            "server/discover" if modern => {
                json!({"jsonrpc":"2.0","id":id,"result":modern_result(json!({
                    "supportedVersions":[MODERN_PROTOCOL_VERSION,LEGACY_PROTOCOL_VERSION],
                    "capabilities":capabilities(),"ttlMs":3_600_000,"cacheScope":"public"
                }))})
            }
            "server/discover" => {
                json!({"jsonrpc":"2.0","id":id,"error":{"code":-32602,"message":"agent_context_server_discover_requires_2026_request_metadata"}})
            }
            "tools/list" => {
                let result = json!({"tools":tools,"ttlMs":3_600_000,"cacheScope":"public"});
                json!({"jsonrpc":"2.0","id":id,"result":if modern { modern_result(result) } else { result }})
            }
            "tools/call" => {
                match contract::validate_call(&tools, request.get("params").unwrap_or(&Value::Null))
                    .and_then(|(name, arguments)| {
                        state::call_tool(&context, &projection, &name, &arguments)
                    }) {
                    Ok(value) => match state::bounded_tool_result(
                        &context,
                        request
                            .pointer("/params/name")
                            .and_then(Value::as_str)
                            .unwrap_or("unknown_tool"),
                        value,
                    ) {
                        Ok(result) => {
                            json!({"jsonrpc":"2.0","id":id,"result":if modern { modern_result(result) } else { result }})
                        }
                        Err(error) => {
                            json!({"jsonrpc":"2.0","id":id,"error":{"code":-32000,"message":error}})
                        }
                    },
                    Err(error) => {
                        json!({"jsonrpc":"2.0","id":id,"error":{"code":-32000,"message":error}})
                    }
                }
            }
            "resources/list"
            | "resources/read"
            | "prompts/list"
            | "prompts/get"
            | "completion/complete"
            | "logging/setLevel" => match state::protocol_request(
                &context,
                &projection,
                method,
                request.get("params").unwrap_or(&Value::Null),
            ) {
                Ok(result) => {
                    json!({"jsonrpc":"2.0","id":id,"result":if modern { modern_result(result) } else { result }})
                }
                Err(error) => {
                    json!({"jsonrpc":"2.0","id":id,"error":{"code":-32000,"message":error}})
                }
            },
            "notifications/initialized" | "notifications/cancelled" => continue,
            _ => {
                json!({"jsonrpc":"2.0","id":id,"error":{"code":-32601,"message":format!("agent_context_native_method_not_implemented:{method}")}})
            }
        };
        if request.get("id").is_none() {
            continue;
        }
        write_message(&mut stdout, &response)?;
        stdout
            .flush()
            .map_err(|error| format!("agent_context_native_flush_failed:{error}"))?;
    }
    Ok(())
}

fn capabilities() -> Value {
    json!({"tools":{},"resources":{},"prompts":{},"completions":{},"logging":{}})
}

fn server_info() -> Value {
    json!({"name":"agent-context-mcp","version":"0.1.0"})
}

fn modern_result(value: Value) -> Value {
    let mut result = value.as_object().cloned().unwrap_or_default();
    result.insert("resultType".into(), json!("complete"));
    let mut metadata = result
        .remove("_meta")
        .and_then(|value| value.as_object().cloned())
        .unwrap_or_default();
    metadata.insert("io.modelcontextprotocol/serverInfo".into(), server_info());
    result.insert("_meta".into(), Value::Object(metadata));
    Value::Object(result)
}

fn validate_modern_metadata(request: &Value) -> Result<(), String> {
    let metadata = request
        .pointer("/params/_meta")
        .and_then(Value::as_object)
        .ok_or("agent_context_modern_metadata_required")?;
    if metadata
        .get("io.modelcontextprotocol/clientInfo")
        .and_then(Value::as_object)
        .is_none()
    {
        return Err("agent_context_modern_client_info_required".into());
    }
    if metadata
        .get("io.modelcontextprotocol/clientCapabilities")
        .and_then(Value::as_object)
        .is_none()
    {
        return Err("agent_context_modern_client_capabilities_required".into());
    }
    Ok(())
}

fn read_message(reader: &mut impl BufRead) -> Result<Option<Vec<u8>>, String> {
    let Some(mut line) = read_bounded_line(reader, MAX_REQUEST_BYTES + 1)? else {
        return Ok(None);
    };
    if line.trim_start().starts_with('{') {
        if line.len() > MAX_REQUEST_BYTES {
            return Err("agent_context_native_request_too_large".into());
        }
        return Ok(Some(line.trim().as_bytes().to_vec()));
    }
    if line.len() > MAX_HEADER_BYTES {
        return Err("agent_context_native_headers_too_large".into());
    }
    let mut content_length = None;
    let mut header_bytes = line.len();
    loop {
        if line == "\r\n" || line == "\n" {
            break;
        }
        if let Some(value) = line
            .strip_prefix("Content-Length:")
            .or_else(|| line.strip_prefix("content-length:"))
        {
            content_length = Some(
                value
                    .trim()
                    .parse::<usize>()
                    .map_err(|_| "agent_context_native_content_length_invalid".to_string())?,
            );
        }
        let Some(next) = read_bounded_line(reader, MAX_HEADER_BYTES + 1)? else {
            return Err("agent_context_native_unexpected_eof".into());
        };
        line = next;
        header_bytes = header_bytes.saturating_add(line.len());
        if header_bytes > MAX_HEADER_BYTES {
            return Err("agent_context_native_headers_too_large".into());
        }
    }
    let length = content_length.ok_or("agent_context_native_content_length_required")?;
    if length > MAX_REQUEST_BYTES {
        return Err("agent_context_native_request_too_large".into());
    }
    let mut body = vec![0; length];
    reader
        .read_exact(&mut body)
        .map_err(|error| format!("agent_context_native_body_read_failed:{error}"))?;
    Ok(Some(body))
}

fn read_bounded_line(reader: &mut impl BufRead, maximum: usize) -> Result<Option<String>, String> {
    let mut line = String::new();
    let read = std::io::Read::take(&mut *reader, maximum as u64)
        .read_line(&mut line)
        .map_err(|error| format!("agent_context_native_read_failed:{error}"))?;
    if read == 0 {
        return Ok(None);
    }
    if read == maximum && !line.ends_with('\n') {
        return Err("agent_context_native_request_line_too_large".into());
    }
    Ok(Some(line))
}

fn write_message(writer: &mut impl Write, value: &Value) -> Result<(), String> {
    let body = serde_json::to_vec(value)
        .map_err(|error| format!("agent_context_native_response_invalid:{error}"))?;
    write!(writer, "Content-Length: {}\r\n\r\n", body.len())
        .map_err(|error| format!("agent_context_native_write_failed:{error}"))?;
    writer
        .write_all(&body)
        .map_err(|error| format!("agent_context_native_write_failed:{error}"))
}

struct Options {
    projection: String,
    site_root: PathBuf,
    site_id: Option<String>,
}

fn options(args: Vec<String>) -> Result<Options, String> {
    let mut projection = "occupant".to_string();
    let mut site_root =
        env::current_dir().map_err(|error| format!("agent_context_native_cwd_failed:{error}"))?;
    let mut site_id = None;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--tool-projection" => {
                projection = args
                    .get(index + 1)
                    .cloned()
                    .ok_or("agent_context_native_projection_required")?;
                index += 2;
            }
            "--site-root" => {
                site_root = PathBuf::from(
                    args.get(index + 1)
                        .ok_or("agent_context_native_site_root_required")?,
                );
                index += 2;
            }
            "--site-id" => {
                site_id = Some(
                    args.get(index + 1)
                        .cloned()
                        .ok_or("agent_context_native_site_id_required")?,
                );
                index += 2;
            }
            value => return Err(format!("agent_context_native_argument_unknown:{value}")),
        }
    }
    if projection != "occupant" && projection != "admin" {
        return Err(format!(
            "agent_context_native_projection_invalid:{projection}"
        ));
    }
    Ok(Options {
        projection,
        site_root,
        site_id,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    #[test]
    fn compiled_catalog_has_unique_tools_for_both_projections() {
        let occupant = contract::tools("occupant").expect("occupant tools");
        let admin = contract::tools("admin").expect("admin tools");
        assert!(!occupant.is_empty());
        assert!(admin.len() >= occupant.len());
        for tools in [&occupant, &admin] {
            let names = tools
                .iter()
                .map(|tool| {
                    assert!(
                        tool.get("inputSchema").is_some(),
                        "every tool must retain its schema"
                    );
                    tool.get("name").and_then(Value::as_str).expect("tool name")
                })
                .collect::<BTreeSet<_>>();
            assert_eq!(names.len(), tools.len(), "tool names must be unique");
        }
    }

    #[test]
    fn accepts_jsonl_and_content_length_requests() {
        let json = br#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#;
        let mut jsonl = std::io::Cursor::new([json.as_slice(), b"\n"].concat());
        assert_eq!(read_message(&mut jsonl).unwrap().unwrap(), json);

        let framed = format!("Content-Length: {}\r\n\r\n", json.len()).into_bytes();
        let mut content_length = std::io::Cursor::new([framed, json.to_vec()].concat());
        assert_eq!(read_message(&mut content_length).unwrap().unwrap(), json);
    }

    #[test]
    fn request_framing_has_hard_body_and_header_bounds() {
        let mut oversized_jsonl = std::io::Cursor::new(vec![b'{'; MAX_REQUEST_BYTES + 1]);
        assert_eq!(
            read_message(&mut oversized_jsonl).unwrap_err(),
            "agent_context_native_request_line_too_large"
        );
        let framed = format!("Content-Length: {}\r\n\r\n", MAX_REQUEST_BYTES + 1);
        assert_eq!(
            read_message(&mut std::io::Cursor::new(framed.into_bytes())).unwrap_err(),
            "agent_context_native_request_too_large"
        );
    }

    #[test]
    fn output_readback_has_one_named_required_reference_contract() {
        for projection in ["occupant", "admin"] {
            let tools = contract::tools(projection).unwrap();
            let tool = tools
                .iter()
                .find(|tool| tool["name"] == "mcp_output_show")
                .unwrap();
            assert_eq!(tool["inputSchema"]["required"], json!(["ref"]));
            assert!(tool["inputSchema"].get("anyOf").is_none());
            assert_eq!(tool["inputSchema"]["properties"]["limit"]["maximum"], 20000);
            assert_eq!(tool["inputSchema"]["additionalProperties"], false);
        }
    }
}
