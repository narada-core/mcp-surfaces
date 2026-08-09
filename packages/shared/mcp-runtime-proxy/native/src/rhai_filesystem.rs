use crate::filesystem;
use crate::protocol;
use rhai::{Engine, Scope};
use serde_json::{json, Value};
use std::io::{self, BufReader, Write};
use std::sync::{Arc, Mutex};

const ROUTER_SCRIPT: &str = r#"
fn route(method, tool, args_json) {
    if method == "initialize" { return "__initialize__"; }
    if method == "tools/list" { return "__tools_list__"; }
    if method == "resources/list" { return "__resources_list__"; }
    if method == "resources/read" { return "__resources_read__"; }
    if method == "prompts/list" { return "__prompts_list__"; }
    if method == "completion/complete" { return "__completion__"; }
    if method == "logging/setLevel" { return "__logging__"; }
    if method == "tools/call" { return filesystem_tool(tool, args_json); }
    return "__unsupported__";
}
"#;

pub fn run(args: &[String]) -> Result<(), String> {
    let state = filesystem::parse_state_for_rhai(args)?;
    let mode = filesystem::mode_for_rhai(&state).to_string();
    let server_name = format!("local-filesystem-{mode}-rhai");
    let shared_state = Arc::new(Mutex::new(state));
    let mut engine = Engine::new();
    let host_state = Arc::clone(&shared_state);
    engine.register_fn("filesystem_tool", move |name: String, args_json: String| -> String {
        let arguments = serde_json::from_str::<Value>(&args_json).unwrap_or(Value::Null);
        let params = json!({"name": name, "arguments": arguments});
        let result = match host_state.lock() {
            Ok(mut state) => filesystem::tool_call_for_rhai(&mut state, &params),
            Err(_) => json!({
                "ok": false,
                "error": {
                    "code": -32000,
                    "message": "rhai_filesystem_state_lock_failed",
                    "data": {"error_code": "rhai_filesystem_state_lock_failed"}
                }
            }),
        };
        serde_json::to_string(&result).unwrap_or_else(|_| {
            "{\"ok\":false,\"error\":{\"code\":-32000,\"message\":\"rhai_filesystem_result_serialization_failed\"}}".to_string()
        })
    });
    let ast = engine
        .compile(ROUTER_SCRIPT)
        .map_err(|error| format!("rhai_filesystem_script_compile_failed:{error}"))?;
    let mut scope = Scope::new();
    let stdin = io::stdin();
    let mut reader = BufReader::new(stdin.lock());
    let stdout = io::stdout();
    let mut writer = stdout.lock();

    loop {
        let Some((request, framed)) =
            filesystem::read_message(&mut reader).map_err(|error| error.to_string())?
        else {
            break;
        };
        if request.get("id").is_none() {
            continue;
        }
        if let Some(response) = protocol::preflight_response(&request, &server_name) {
            filesystem::write_message(&mut writer, &response, framed)
                .map_err(|error| error.to_string())?;
            writer.flush().map_err(|error| error.to_string())?;
            continue;
        }
        let response = dispatch(&engine, &mut scope, &ast, &request, &mode)?;
        let response = protocol::modernize_response(&request, response, &server_name);
        filesystem::write_message(&mut writer, &response, framed)
            .map_err(|error| error.to_string())?;
        writer.flush().map_err(|error| error.to_string())?;
    }
    Ok(())
}

fn dispatch(
    engine: &Engine,
    scope: &mut Scope<'static>,
    ast: &rhai::AST,
    request: &Value,
    mode: &str,
) -> Result<Value, String> {
    let id = request.get("id").cloned().unwrap_or(Value::Null);
    let method = request
        .get("method")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let tool = request
        .get("params")
        .and_then(|value| value.get("name"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    let arguments = request
        .get("params")
        .and_then(|value| value.get("arguments"))
        .cloned()
        .unwrap_or_else(|| json!({}));
    let routed = engine
        .call_fn::<String>(
            scope,
            ast,
            "route",
            (
                method.to_string(),
                tool.to_string(),
                serde_json::to_string(&arguments).unwrap_or_else(|_| "{}".to_string()),
            ),
        )
        .map_err(|error| format!("rhai_filesystem_route_failed:{error}"))?;

    let result = match routed.as_str() {
        "__initialize__" => {
            let mut initialize = filesystem::initialize_for_rhai(request, mode);
            if let Some(server_info) = initialize
                .get_mut("serverInfo")
                .and_then(Value::as_object_mut)
            {
                server_info.insert(
                    "name".to_string(),
                    json!(format!("local-filesystem-{mode}-rhai")),
                );
            }
            json!({"jsonrpc": "2.0", "id": id, "result": initialize})
        }
        "__tools_list__" => {
            json!({"jsonrpc": "2.0", "id": id, "result": filesystem::tools_list_for_rhai(mode)})
        }
        "__resources_list__" => json!({"jsonrpc": "2.0", "id": id, "result": {"resources": []}}),
        "__resources_read__" => json!({
            "jsonrpc": "2.0",
            "id": id,
            "error": {
                "code": -32000,
                "message": "resource_not_found",
                "data": {"error_code": "resource_not_found"}
            }
        }),
        "__prompts_list__" => json!({"jsonrpc": "2.0", "id": id, "result": {"prompts": []}}),
        "__completion__" => {
            json!({"jsonrpc": "2.0", "id": id, "result": {"completion": {"values": [], "total": 0, "hasMore": false}}})
        }
        "__logging__" => json!({"jsonrpc": "2.0", "id": id, "result": {}}),
        "__unsupported__" => json!({
            "jsonrpc": "2.0",
            "id": id,
            "error": {
                "code": -32601,
                "message": format!("unsupported_mcp_method: {method}"),
                "data": {"error_code": "unsupported_mcp_method"}
            }
        }),
        encoded => {
            let host_result = serde_json::from_str::<Value>(encoded)
                .map_err(|error| format!("rhai_filesystem_host_result_invalid:{error}"))?;
            if host_result.get("ok").and_then(Value::as_bool) == Some(true) {
                json!({"jsonrpc": "2.0", "id": id, "result": host_result.get("result").cloned().unwrap_or(Value::Null)})
            } else {
                json!({"jsonrpc": "2.0", "id": id, "error": host_result.get("error").cloned().unwrap_or_else(|| json!({
                    "code": -32000,
                    "message": "rhai_filesystem_tool_failed"
                }))})
            }
        }
    };
    Ok(result)
}
