use serde_json::json;
use std::env;
use std::fs;
use std::io::{Read, Write};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::path::PathBuf;
use std::time::Duration;

const MAX_REQUEST_BYTES: usize = 64 * 1024;

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
    let path = parts
        .next()
        .unwrap_or_default()
        .split('?')
        .next()
        .unwrap_or_default();
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
            "schema":"narada.operator_console_runtime.routes.v1","status":"partial_native_port","routes":["/","/health","/routes","/console"]
        }).to_string()),
        "/" | "/console" => ("text/html; charset=utf-8", "<!doctype html><html><head><meta charset=\"utf-8\"><title>Narada Operator Console</title></head><body><main><h1>Narada Operator Console</h1><p>Native Rust runtime is ready. Route authority migration is in progress.</p></main></body></html>".to_string()),
        _ => return response(stream, 404, "application/json", "{\"error\":\"route_not_found\"}"),
    };
    response(
        stream,
        200,
        content_type,
        if method == "HEAD" { "" } else { &body },
    )
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
