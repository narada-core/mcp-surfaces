use serde_json::{json, Map, Value};
use std::env;
use std::fs;
use std::io::{Read, Write};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::path::PathBuf;
use std::time::Duration;

const MAX_REQUEST_BYTES: usize = 64 * 1024;
const KNOWN_ROUTES: &[&str] = &[
    "/",
    "/health",
    "/routes",
    "/console",
    "/console/surfaces",
    "/console/agents",
    "/console/agents/api/overview",
    "/console/agents/api/admission-options",
    "/console/agents/api/admission",
    "/console/agents/api/stop",
    "/console/agents/api/delete",
    "/console/agents/api/launch",
    "/console/agents/api/pending",
    "/console/agents/api/session-route",
    "/console/sessions",
    "/console/sessions/api/sessions",
    "/console/registry",
    "/console/registry/add",
    "/console/registry/manage",
    "/console/registry/api/sites",
    "/console/registry/api/operations/plan",
    "/console/registry/api/operations/apply",
    "/console/registry/api/discover-plan",
    "/console/launch",
    "/console/onboarding",
    "/console/onboarding/api/status",
    "/console/onboarding/api/start",
    "/console/fleet",
    "/console/fleet/api/hosts",
    "/console/fleet/api/observations",
];

pub fn is_host_mode(args: &[String]) -> bool {
    args.first().map(String::as_str) == Some("--operator-console-runtime-host")
}

pub fn run(args: &[String]) -> Result<(), String> {
    let host = optional(args, "--host").unwrap_or_else(|| "127.0.0.1".to_string());
    if !matches!(host.as_str(), "127.0.0.1" | "localhost") {
        return Err("operator_console_runtime_loopback_required".to_string());
    }
    let port = optional(args, "--port")
        .unwrap_or_else(|| "43117".to_string())
        .parse::<u16>()
        .map_err(|_| "operator_console_runtime_port_invalid".to_string())?;
    if port == 0 {
        return Err("operator_console_runtime_port_invalid".to_string());
    }
    let address = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port);
    let listener = TcpListener::bind(address)
        .map_err(|cause| format!("operator_console_runtime_bind_failed:{cause}"))?;
    let state_root = state_root();
    fs::create_dir_all(&state_root)
        .map_err(|cause| format!("operator_console_runtime_state_create_failed:{cause}"))?;
    let state_path = state_root.join("runtime.json");
    write_state(&state_path, "ready", port)?;
    for incoming in listener.incoming() {
        match incoming {
            Ok(mut stream) => {
                stream.set_read_timeout(Some(Duration::from_secs(5))).ok();
                stream.set_write_timeout(Some(Duration::from_secs(5))).ok();
                if let Err(cause) = serve(&mut stream, port) {
                    let _ = response(
                        &mut stream,
                        400,
                        "application/json",
                        &json!({"error":cause}).to_string(),
                    );
                }
            }
            Err(cause) => {
                write_state(&state_path, "failed", port)?;
                return Err(format!("operator_console_runtime_accept_failed:{cause}"));
            }
        }
    }
    Ok(())
}

fn serve(stream: &mut TcpStream, port: u16) -> Result<(), String> {
    let mut request = Vec::new();
    let mut byte = [0_u8; 1];
    while request.len() <= MAX_REQUEST_BYTES {
        let count = stream
            .read(&mut byte)
            .map_err(|cause| format!("request_read_failed:{cause}"))?;
        if count == 0 {
            break;
        }
        request.push(byte[0]);
        if request.ends_with(b"\r\n\r\n") || request.ends_with(b"\n\n") {
            break;
        }
    }
    if request.len() > MAX_REQUEST_BYTES {
        return Err("request_too_large".to_string());
    }
    let first = String::from_utf8(request)
        .map_err(|_| "request_encoding_invalid".to_string())?
        .lines()
        .next()
        .unwrap_or_default()
        .to_string();
    let mut parts = first.split_whitespace();
    let method = parts.next().unwrap_or_default();
    let target = parts.next().unwrap_or_default();
    let (path, query) = target.split_once('?').unwrap_or((target, ""));
    if !matches!(method, "GET" | "HEAD") {
        return response(
            stream,
            405,
            "application/json",
            "{\"error\":\"method_not_allowed\"}",
        );
    }
    let (content_type, body) = match path {
        "/health" => ("application/json", json!({
            "schema":"narada.operator_console_runtime.health.v1","status":"ready","runtime":"rust","pid":std::process::id(),"port":port
        }).to_string()),
        "/routes" => ("application/json", json!({
            "schema":"narada.operator_console_runtime.routes.v1","status":"partial_native_port","routes":KNOWN_ROUTES
        }).to_string()),
        "/" | "/console" => ("text/html; charset=utf-8", "<!doctype html><html><head><meta charset=\"utf-8\"><title>Narada Operator Console</title></head><body><main><h1>Narada Operator Console</h1><p>Native Rust runtime is ready. Route authority migration is in progress.</p></main></body></html>".to_string()),
        "/console/registry/api/sites" => {
            return serve_registry_list(stream, method, query);
        }
        _ if path.starts_with("/console/registry/api/sites/") => {
            return serve_registry_show(stream, method, path);
        }
        "/console/registry/api/discover-plan" => {
            return serve_registry_discover_plan(stream, method, query);
        }
        _ if KNOWN_ROUTES.iter().any(|known| *known == path) => {
            let body = json!({"schema":"narada.operator_console_runtime.route_result.v1","status":"unavailable","code":"native_route_not_yet_ported","route":path,"migration":"rust_authority_port_in_progress"}).to_string();
            return response(stream, 501, "application/json", &body);
        }
        _ => return response(stream, 404, "application/json", "{\"error\":\"route_not_found\"}"),
    };
    response(
        stream,
        200,
        content_type,
        if method == "HEAD" { "" } else { &body },
    )
}

fn serve_registry_list(stream: &mut TcpStream, method: &str, query: &str) -> Result<(), String> {
    if method != "GET" && method != "HEAD" {
        return response(
            stream,
            405,
            "application/json",
            "{\"error\":\"method_not_allowed\"}",
        );
    }
    let args = query_args(query, &["limit", "offset"])?;
    let body = native_registry_call("site_registry_list", &args)?;
    respond_native_result(stream, method, body)
}

fn serve_registry_show(stream: &mut TcpStream, method: &str, path: &str) -> Result<(), String> {
    if method != "GET" && method != "HEAD" {
        return response(
            stream,
            405,
            "application/json",
            "{\"error\":\"method_not_allowed\"}",
        );
    }
    let reference = path
        .strip_prefix("/console/registry/api/sites/")
        .filter(|value| !value.is_empty())
        .ok_or_else(|| "registry_site_reference_missing".to_string())?;
    let reference = percent_decode(reference)?;
    if reference.len() > 512 || reference.is_empty() || reference.contains('/') {
        return response(
            stream,
            400,
            "application/json",
            "{\"error\":\"registry_site_reference_invalid\"}",
        );
    }
    let mut args = Map::new();
    args.insert("reference".to_string(), Value::String(reference));
    let body = native_registry_call("site_registry_show", &args)?;
    respond_native_result(stream, method, body)
}

fn serve_registry_discover_plan(
    stream: &mut TcpStream,
    method: &str,
    query: &str,
) -> Result<(), String> {
    if method != "GET" && method != "HEAD" {
        return response(
            stream,
            405,
            "application/json",
            "{\"error\":\"method_not_allowed\"}",
        );
    }
    let args = query_args(query, &["source", "root", "actor"])?;
    let body = native_registry_call("site_registry_discover_plan", &args)?;
    respond_native_result(stream, method, body)
}

fn native_registry_call(name: &str, args: &Map<String, Value>) -> Result<String, String> {
    let value = crate::site_registry_authority::call(name, args).map_err(|error| {
        serde_json::to_string(&error)
            .unwrap_or_else(|_| "{\"error\":\"registry_authority_failed\"}".to_string())
    })?;
    serde_json::to_string(&value).map_err(|cause| format!("registry_result_encode_failed:{cause}"))
}

fn respond_native_result(stream: &mut TcpStream, method: &str, body: String) -> Result<(), String> {
    let status = serde_json::from_str::<Value>(&body).ok().and_then(|value| {
        value
            .get("status")
            .and_then(Value::as_str)
            .map(str::to_string)
    });
    let code = match status.as_deref() {
        Some("success") | Some("planned") | Some("unchanged") | Some("advisory") => 200,
        Some("refused") => {
            if body.contains("site_not_found") {
                404
            } else {
                400
            }
        }
        _ => 503,
    };
    response(
        stream,
        code,
        "application/json",
        if method == "HEAD" { "" } else { &body },
    )
}

fn query_args(query: &str, allowed: &[&str]) -> Result<Map<String, Value>, String> {
    let mut args = Map::new();
    for pair in query.split('&').filter(|pair| !pair.is_empty()).take(16) {
        let (raw_key, raw_value) = pair.split_once('=').unwrap_or((pair, ""));
        let key = percent_decode(raw_key)?;
        if !allowed.iter().any(|allowed_key| *allowed_key == key) {
            return Err(format!("registry_query_parameter_invalid:{key}"));
        }
        let value = percent_decode(raw_value)?;
        if value.len() > 4096 {
            return Err("registry_query_value_too_large".to_string());
        }
        match key.as_str() {
            "limit" | "offset" => {
                let number = value
                    .parse::<i64>()
                    .map_err(|_| format!("registry_query_integer_invalid:{key}"))?;
                args.insert(key, Value::from(number));
            }
            _ => {
                args.insert(key, Value::String(value));
            }
        }
    }
    Ok(args)
}

fn percent_decode(value: &str) -> Result<String, String> {
    let bytes = value.as_bytes();
    let mut output = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            if index + 2 >= bytes.len() {
                return Err("request_percent_encoding_invalid".to_string());
            }
            let high = hex_digit(bytes[index + 1])
                .ok_or_else(|| "request_percent_encoding_invalid".to_string())?;
            let low = hex_digit(bytes[index + 2])
                .ok_or_else(|| "request_percent_encoding_invalid".to_string())?;
            output.push((high << 4) | low);
            index += 3;
        } else {
            output.push(if bytes[index] == b'+' {
                b' '
            } else {
                bytes[index]
            });
            index += 1;
        }
    }
    String::from_utf8(output).map_err(|_| "request_encoding_invalid".to_string())
}

fn hex_digit(value: u8) -> Option<u8> {
    match value {
        b'0'..=b'9' => Some(value - b'0'),
        b'a'..=b'f' => Some(value - b'a' + 10),
        b'A'..=b'F' => Some(value - b'A' + 10),
        _ => None,
    }
}

fn response(
    stream: &mut TcpStream,
    status: u16,
    content_type: &str,
    body: &str,
) -> Result<(), String> {
    let reason = match status {
        200 => "OK",
        400 => "Bad Request",
        404 => "Not Found",
        405 => "Method Not Allowed",
        _ => "Error",
    };
    write!(stream, "HTTP/1.1 {status} {reason}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len())
        .and_then(|()| stream.flush()).map_err(|cause| format!("response_write_failed:{cause}"))
}

fn write_state(path: &std::path::Path, status: &str, port: u16) -> Result<(), String> {
    let value = json!({"schema":"narada.operator_console_runtime.state.v1","status":status,"runtime":"rust","pid":std::process::id(),"url":format!("http://127.0.0.1:{port}")});
    let temporary = path.with_extension(format!("{}.tmp", uuid::Uuid::new_v4()));
    fs::write(
        &temporary,
        serde_json::to_vec_pretty(&value).map_err(|cause| cause.to_string())?,
    )
    .map_err(|cause| cause.to_string())?;
    if path.exists() {
        fs::remove_file(path).map_err(|cause| cause.to_string())?;
    }
    fs::rename(temporary, path).map_err(|cause| cause.to_string())
}

fn state_root() -> PathBuf {
    env::var_os("NARADA_OPERATOR_CONSOLE_RUNTIME_STATE_ROOT")
        .map(PathBuf::from)
        .or_else(|| {
            env::var_os("LOCALAPPDATA")
                .map(|value| PathBuf::from(value).join("Narada/operator-console-runtime"))
        })
        .unwrap_or_else(|| PathBuf::from("AppData/Local/Narada/operator-console-runtime"))
}

fn optional(args: &[String], key: &str) -> Option<String> {
    let index = args.iter().position(|value| value == key)?;
    args.get(index + 1)
        .cloned()
        .filter(|value| !value.trim().is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn refuses_non_loopback_runtime_host() {
        let error = run(&[
            "--operator-console-runtime-host".into(),
            "--host".into(),
            "0.0.0.0".into(),
            "--port".into(),
            "43117".into(),
        ])
        .expect_err("refusal");
        assert_eq!(error, "operator_console_runtime_loopback_required");
    }
}
