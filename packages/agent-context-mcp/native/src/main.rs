use serde_json::{json, Value};
use std::env;
use std::io::{self, BufRead, Write};
use std::path::PathBuf;

mod materialization;
mod orientation;
mod state;

const CATALOG: &str = include_str!("../tool-catalog.json");

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
    let catalog: Value = serde_json::from_str(CATALOG)
        .map_err(|error| format!("agent_context_native_catalog_invalid:{error}"))?;
    let tools = catalog
        .pointer(&format!("/projections/{projection}"))
        .and_then(Value::as_array)
        .cloned()
        .ok_or_else(|| format!("agent_context_native_projection_invalid:{projection}"))?;
    let stdin = io::stdin();
    let mut reader = stdin.lock();
    let mut stdout = io::stdout().lock();
    while let Some(body) = read_message(&mut reader)? {
        let request: Value = serde_json::from_slice(&body)
            .map_err(|error| format!("agent_context_native_request_invalid:{error}"))?;
        let id = request.get("id").cloned().unwrap_or(Value::Null);
        let method = request.get("method").and_then(Value::as_str).unwrap_or("");
        let response = match method {
            "initialize" => {
                json!({"jsonrpc":"2.0","id":id,"result":{"protocolVersion":request.pointer("/params/protocolVersion").and_then(Value::as_str).unwrap_or("2026-04-18"),"capabilities":{"tools":{},"resources":{},"prompts":{},"completions":{},"logging":{}},"serverInfo":{"name":"agent-context-mcp","version":"0.1.0"}}})
            }
            "tools/list" => json!({"jsonrpc":"2.0","id":id,"result":{"tools":tools}}),
            "tools/call" => match state::call_tool(
                &context,
                &projection,
                request.get("params").unwrap_or(&Value::Null),
            ) {
                Ok(value) => match state::bounded_tool_result(
                    &context,
                    request
                        .pointer("/params/name")
                        .and_then(Value::as_str)
                        .unwrap_or("unknown_tool"),
                    value,
                ) {
                    Ok(result) => json!({"jsonrpc":"2.0","id":id,"result":result}),
                    Err(error) => {
                        json!({"jsonrpc":"2.0","id":id,"error":{"code":-32000,"message":error}})
                    }
                },
                Err(error) => {
                    json!({"jsonrpc":"2.0","id":id,"error":{"code":-32000,"message":error}})
                }
            },
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
                Ok(result) => json!({"jsonrpc":"2.0","id":id,"result":result}),
                Err(error) => {
                    json!({"jsonrpc":"2.0","id":id,"error":{"code":-32000,"message":error}})
                }
            },
            "notifications/initialized" => continue,
            _ => {
                json!({"jsonrpc":"2.0","id":id,"error":{"code":-32601,"message":format!("agent_context_native_method_not_implemented:{method}")}})
            }
        };
        write_message(&mut stdout, &response)?;
        stdout
            .flush()
            .map_err(|error| format!("agent_context_native_flush_failed:{error}"))?;
    }
    Ok(())
}

fn read_message(reader: &mut impl BufRead) -> Result<Option<Vec<u8>>, String> {
    let mut line = String::new();
    if reader
        .read_line(&mut line)
        .map_err(|error| format!("agent_context_native_read_failed:{error}"))?
        == 0
    {
        return Ok(None);
    }
    if line.trim_start().starts_with('{') {
        return Ok(Some(line.trim().as_bytes().to_vec()));
    }
    let mut content_length = None;
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
        line.clear();
        if reader
            .read_line(&mut line)
            .map_err(|error| format!("agent_context_native_read_failed:{error}"))?
            == 0
        {
            return Err("agent_context_native_unexpected_eof".into());
        }
    }
    let length = content_length.ok_or("agent_context_native_content_length_required")?;
    let mut body = vec![0; length];
    reader
        .read_exact(&mut body)
        .map_err(|error| format!("agent_context_native_body_read_failed:{error}"))?;
    Ok(Some(body))
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
        let catalog: Value = serde_json::from_str(CATALOG).expect("catalog must parse");
        let occupant = catalog
            .pointer("/projections/occupant")
            .and_then(Value::as_array)
            .expect("occupant tools");
        let admin = catalog
            .pointer("/projections/admin")
            .and_then(Value::as_array)
            .expect("admin tools");
        assert!(!occupant.is_empty());
        assert!(admin.len() >= occupant.len());
        for tools in [occupant, admin] {
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
}
