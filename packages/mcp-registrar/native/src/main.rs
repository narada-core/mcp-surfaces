use serde_json::{json, Value};
use std::io::{self, BufRead, Write};

const CONTRACT: &str = include_str!("../tool-catalog.json");

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

fn dispatch(request: &Value) -> Value {
    let id = request.get("id").cloned().unwrap_or(Value::Null);
    let contract: Value = match serde_json::from_str(CONTRACT) {
        Ok(v) => v,
        Err(e) => return error(id, format!("mcp_registrar_native_contract_invalid:{e}")),
    };
    match request.get("method").and_then(Value::as_str).unwrap_or("") {
        "initialize" => {
            json!({"jsonrpc":"2.0","id":id,"result":{"protocolVersion":request.pointer("/params/protocolVersion").cloned().unwrap_or_else(||json!("2024-11-05")),"capabilities":{"tools":{}},"serverInfo":{"name":"mcp-registrar","version":"0.1.0"}}})
        }
        "tools/list" => json!({"jsonrpc":"2.0","id":id,"result":{"tools":contract["tools"]}}),
        "tools/call" => {
            let name = request
                .pointer("/params/name")
                .and_then(Value::as_str)
                .unwrap_or("");
            if name == "registrar_guidance" {
                let mut guidance = contract["guidance"].clone();
                let args = request
                    .pointer("/params/arguments")
                    .cloned()
                    .unwrap_or_else(|| json!({}));
                guidance.as_object_mut().unwrap().insert("requested".into(),json!({"workflow":args.get("workflow").and_then(Value::as_str).filter(|v|!v.trim().is_empty()).map(str::trim),"tool":args.get("tool").and_then(Value::as_str).filter(|v|!v.trim().is_empty()).map(str::trim)}));
                json!({"jsonrpc":"2.0","id":id,"result":{"content":[{"type":"text","text":serde_json::to_string_pretty(&guidance).unwrap()}],"structuredContent":guidance}})
            } else {
                error(
                    id,
                    format!("mcp_registrar_native_tool_not_implemented:{name}"),
                )
            }
        }
        method => error(id, format!("unsupported_mcp_method:{method}")),
    }
}
fn error(id: Value, message: String) -> Value {
    json!({"jsonrpc":"2.0","id":id,"error":{"code":-32000,"message":message}})
}

fn read_message<R: BufRead>(input: &mut R) -> Result<Option<Value>, String> {
    let mut first = String::new();
    if input.read_line(&mut first).map_err(|e| e.to_string())? == 0 {
        return Ok(None);
    }
    if first.to_ascii_lowercase().starts_with("content-length:") {
        let length = first
            .split_once(':')
            .and_then(|(_, v)| v.trim().parse::<usize>().ok())
            .ok_or("invalid_content_length")?;
        loop {
            let mut line = String::new();
            input.read_line(&mut line).map_err(|e| e.to_string())?;
            if line == "\r\n" || line == "\n" {
                break;
            }
            if line.is_empty() {
                return Err("unexpected_eof_in_headers".into());
            }
        }
        let mut body = vec![0; length];
        input.read_exact(&mut body).map_err(|e| e.to_string())?;
        serde_json::from_slice(&body)
            .map(Some)
            .map_err(|e| e.to_string())
    } else {
        serde_json::from_str(first.trim())
            .map(Some)
            .map_err(|e| e.to_string())
    }
}
