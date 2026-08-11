use serde_json::{json, Value};
use std::io::{self, BufRead, Read, Write};

const CONTRACT: &[u8] = include_bytes!("../tool-catalog.json.gz");

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
    let contract: Value = match flate2::read::GzDecoder::new(CONTRACT)
        .bytes()
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())
        .and_then(|bytes| serde_json::from_slice(&bytes).map_err(|e| e.to_string()))
    {
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
            if matches!(
                name,
                "registrar_guidance" | "registrar_surface_list" | "registrar_carrier_list"
            ) {
                let mut guidance = if name == "registrar_guidance" {
                    contract["guidance"].clone()
                } else {
                    contract["read_models"][name].clone()
                };
                let args = request
                    .pointer("/params/arguments")
                    .cloned()
                    .unwrap_or_else(|| json!({}));
                if name == "registrar_guidance" {
                    guidance.as_object_mut().unwrap().insert("requested".into(),json!({"workflow":args.get("workflow").and_then(Value::as_str).filter(|v|!v.trim().is_empty()).map(str::trim),"tool":args.get("tool").and_then(Value::as_str).filter(|v|!v.trim().is_empty()).map(str::trim)}));
                }
                json!({"jsonrpc":"2.0","id":id,"result":{"content":[{"type":"text","text":serde_json::to_string_pretty(&guidance).unwrap()}],"structuredContent":guidance}})
            } else if name == "registrar_surface_tool_inventory_check" {
                let args = request
                    .pointer("/params/arguments")
                    .cloned()
                    .unwrap_or_else(|| json!({}));
                let result = surface_tool_inventory(&contract, &args);
                json!({"jsonrpc":"2.0","id":id,"result":{"content":[{"type":"text","text":serde_json::to_string_pretty(&result).unwrap()}],"structuredContent":result}})
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
fn surface_tool_inventory(contract: &Value, args: &Value) -> Value {
    let observed = args.get("observed_tools").and_then(Value::as_object);
    let include_ok = args.get("include_ok") == Some(&Value::Bool(true));
    let items = contract
        .pointer("/read_models/registrar_surface_list/items")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut findings = vec![];
    let mut checked = 0;
    for surface in &items {
        let id = surface["id"].as_str().unwrap_or("");
        let Some(input) = observed.and_then(|value| value.get(id)) else {
            continue;
        };
        checked += 1;
        let registered = unique(
            surface["tools"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str),
        );
        let actual = unique(
            input
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str),
        );
        let missing = actual
            .iter()
            .filter(|value| !registered.contains(value))
            .cloned()
            .collect::<Vec<_>>();
        let extra = registered
            .iter()
            .filter(|value| !actual.contains(value))
            .cloned()
            .collect::<Vec<_>>();
        let status = if missing.is_empty() && extra.is_empty() {
            "ok"
        } else {
            "drift"
        };
        if status != "ok" || include_ok {
            findings.push(json!({"surface_id":id,"package":surface["package"],"status":status,"registered_count":registered.len(),"observed_count":actual.len(),"missing_from_registrar":missing,"extra_in_registrar":extra}));
        }
    }
    let without = items
        .iter()
        .filter_map(|value| value["id"].as_str())
        .filter(|id| observed.is_none_or(|value| !value.contains_key(*id)))
        .collect::<Vec<_>>();
    json!({"schema":"narada.registrar.surface_tool_inventory_check.v1","status":if findings.iter().any(|value|value["status"]=="drift"){"drift"}else{"ok"},"checked_count":checked,"surfaces_without_observations":without,"findings":findings})
}
fn unique<'a>(values: impl Iterator<Item = &'a str>) -> Vec<String> {
    let mut result = vec![];
    for value in values {
        if !result.iter().any(|existing| existing == value) {
            result.push(value.to_string());
        }
    }
    result
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
