use serde_json::{json, Map, Value};
use std::env;
use std::fs;
use std::net::{SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

const DEFAULT_TIMEOUT_MS: u64 = 30_000;
const MAX_TIMEOUT_MS: u64 = 120_000;

pub fn list_tools() -> Vec<Value> {
    vec![
        tool("operator_console_overlay_guidance", json!({}), true),
        tool("operator_console_overlay_status", json!({}), true),
        tool(
            "operator_console_overlay_open",
            json!({
                "url":{"type":"string","minLength":1,"maxLength":2048,"pattern":"^https?://"},
                "title":{"type":"string","minLength":1,"maxLength":200},
                "visibility":{"type":"string","enum":["always","terminal-group","hidden","windows-terminal"],"default":"windows-terminal"},
                "refresh_seconds":{"type":"integer","minimum":1,"maximum":3600,"default":2},
                "timeout_ms":{"type":"integer","minimum":100,"maximum":MAX_TIMEOUT_MS,"default":DEFAULT_TIMEOUT_MS}
            }),
            false,
        ),
        tool(
            "operator_console_overlay_refresh",
            json!({
                "timeout_ms":{"type":"integer","minimum":100,"maximum":MAX_TIMEOUT_MS,"default":DEFAULT_TIMEOUT_MS}
            }),
            false,
        ),
        tool(
            "operator_console_overlay_close",
            json!({
                "timeout_ms":{"type":"integer","minimum":100,"maximum":MAX_TIMEOUT_MS,"default":DEFAULT_TIMEOUT_MS}
            }),
            false,
        ),
    ]
}

fn tool(name: &str, properties: Value, read_only: bool) -> Value {
    json!({
        "name":name,
        "description":format!("Rust-owned Operator Console overlay operation: {name}."),
        "inputSchema":{"type":"object","properties":properties,"additionalProperties":false},
        "annotations":{"title":name,"readOnlyHint":read_only,"destructiveHint":false,"idempotentHint":true,"openWorldHint":false},
        "outputSchema":{"type":"object","additionalProperties":true}
    })
}

pub fn call(name: &str, args: &Map<String, Value>, site_root: &Path) -> Result<Value, Value> {
    match name {
        "operator_console_overlay_guidance" => Ok(json!({
            "schema":"narada.mcp_surface.guidance.v0","status":"ok","surface_id":"operator-console-overlay",
            "purpose":"Native lifecycle authority for the dedicated Operator Console Windows overlay.",
            "first_use":["Call operator_console_overlay_status before mutation.","Open accepts an explicit HTTP(S) console URL; the local native console runtime is used when URL is omitted."],
            "boundaries":["Rust owns document persistence and lifecycle commands.","PowerShell remains the admitted Windows WPF host, not application authority."]
        })),
        "operator_console_overlay_status" => Ok(wrap("status", "inspect", inspect(site_root))),
        "operator_console_overlay_open" => open(args, site_root),
        "operator_console_overlay_refresh" => refresh(site_root),
        "operator_console_overlay_close" => close(args, site_root),
        _ => Err(error("unknown_tool", &format!("unknown_tool:{name}"))),
    }
}

fn open(args: &Map<String, Value>, site_root: &Path) -> Result<Value, Value> {
    let url = args
        .get("url")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .map(Ok)
        .unwrap_or_else(|| ensure_local_runtime(site_root))?;
    if !(url.starts_with("http://") || url.starts_with("https://")) || url.len() > 2048 {
        return Err(error(
            "operator_console_overlay_url_invalid",
            "operator_console_overlay_url_invalid",
        ));
    }
    let title = args
        .get("title")
        .and_then(Value::as_str)
        .unwrap_or("Narada Operator Console");
    let visibility = args
        .get("visibility")
        .and_then(Value::as_str)
        .unwrap_or("windows-terminal");
    let refresh_seconds = args
        .get("refresh_seconds")
        .and_then(Value::as_u64)
        .unwrap_or(2);
    let timeout_ms = timeout(args)?;
    let state_directory = state_directory();
    fs::create_dir_all(&state_directory)
        .map_err(|cause| error("operator_console_state_create_failed", &cause.to_string()))?;
    let document = json!({
        "schema":"narada.window_surface_overlay.document.v1","id":"operator-console","title":title,
        "title_tone":"accent","subtitle":url,
        "rows":[
            {"label":"Workspace","value":url,"kind":"open_url","target":url},
            {"label":"Console routes","value":"/console","kind":"open_url","target":format!("{}/console",url.trim_end_matches('/'))}
        ],
        "actions":[
            {"id":"open-console","label":"Open console","icon":"↗","tooltip":"Open console","kind":"open_url","tone":"accent","target":url},
            {"id":"refresh","label":"Refresh","icon":"⟳","tooltip":"Refresh overlay","kind":"refresh"}
        ]
    });
    write_atomic(&state_directory.join("document.json"), &document)?;
    let output = run_script(
        "Start-WindowSurfaceOverlay.ps1",
        &[
            "-Id",
            "operator-console",
            "-StateRoot",
            &state_directory.to_string_lossy(),
            "-VisibilityPolicy",
            visibility,
            "-RefreshSeconds",
            &refresh_seconds.to_string(),
            "-StartupTimeoutSeconds",
            &(timeout_ms / 1000).clamp(1, 120).to_string(),
        ],
        timeout_ms,
        site_root,
    )?;
    Ok(wrap("open", "start", output))
}

fn refresh(site_root: &Path) -> Result<Value, Value> {
    let directory = state_directory();
    fs::create_dir_all(&directory)
        .map_err(|cause| error("operator_console_state_create_failed", &cause.to_string()))?;
    fs::write(
        directory.join("refresh.signal"),
        time::OffsetDateTime::now_utc()
            .unix_timestamp_nanos()
            .to_string(),
    )
    .map_err(|cause| error("operator_console_refresh_failed", &cause.to_string()))?;
    Ok(wrap("refresh", "refresh", inspect(site_root)))
}

fn close(args: &Map<String, Value>, site_root: &Path) -> Result<Value, Value> {
    let timeout_ms = timeout(args)?;
    let directory = state_directory();
    let output = run_script(
        "Stop-WindowSurfaceOverlay.ps1",
        &[
            "-Id",
            "operator-console",
            "-StateRoot",
            &directory.to_string_lossy(),
        ],
        timeout_ms,
        site_root,
    )?;
    Ok(wrap("close", "stop", output))
}

fn inspect(site_root: &Path) -> Value {
    let directory = state_directory();
    let pid = fs::read_to_string(directory.join("overlay.pid"))
        .ok()
        .and_then(|value| value.trim().parse::<u32>().ok());
    json!({
        "schema":"narada.window_surface_overlay.result.v1","id":"operator-console",
        "state":if pid.is_some(){"running"}else{"stopped"},"pid":pid,
        "state_directory":directory,"document":read_json(&directory.join("document.json")),
        "narada_root":narada_root(site_root)
    })
}

fn run_script(
    name: &str,
    args: &[&str],
    timeout_ms: u64,
    site_root: &Path,
) -> Result<Value, Value> {
    let script = narada_root(site_root)
        .join("packages/window-overlay-core/src")
        .join(name);
    if !script.is_file() {
        return Err(error(
            "operator_console_overlay_host_script_missing",
            &script.to_string_lossy(),
        ));
    }
    let shell = env::var("NARADA_POWERSHELL").unwrap_or_else(|_| "pwsh".to_string());
    let mut child = Command::new(shell)
        .args(["-NoLogo", "-NoProfile", "-NonInteractive", "-File"])
        .arg(&script)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|cause| {
            error(
                "operator_console_overlay_host_spawn_failed",
                &cause.to_string(),
            )
        })?;
    let deadline = Instant::now() + Duration::from_millis(timeout_ms);
    loop {
        if child
            .try_wait()
            .map_err(|cause| {
                error(
                    "operator_console_overlay_host_wait_failed",
                    &cause.to_string(),
                )
            })?
            .is_some()
        {
            break;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            return Err(error(
                "operator_console_overlay_command_timeout",
                &format!("operator_console_overlay_command_timeout:{timeout_ms}"),
            ));
        }
        thread::sleep(Duration::from_millis(25));
    }
    let output = child.wait_with_output().map_err(|cause| {
        error(
            "operator_console_overlay_host_output_failed",
            &cause.to_string(),
        )
    })?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(error(
            "operator_console_overlay_host_failed",
            &stderr.chars().take(2048).collect::<String>(),
        ));
    }
    serde_json::from_slice(&output.stdout).or_else(|_| Ok(inspect(site_root)))
}

fn timeout(args: &Map<String, Value>) -> Result<u64, Value> {
    let value = args
        .get("timeout_ms")
        .and_then(Value::as_u64)
        .unwrap_or(DEFAULT_TIMEOUT_MS);
    if !(100..=MAX_TIMEOUT_MS).contains(&value) {
        return Err(error(
            "operator_console_overlay_timeout_invalid",
            "operator_console_overlay_timeout_invalid",
        ));
    }
    Ok(value)
}

fn state_directory() -> PathBuf {
    let root = env::var_os("NARADA_WINDOW_SURFACE_OVERLAY_STATE_ROOT")
        .map(PathBuf::from)
        .or_else(|| {
            env::var_os("LOCALAPPDATA")
                .map(|value| PathBuf::from(value).join("Narada/window-surface-overlays"))
        })
        .unwrap_or_else(|| PathBuf::from("AppData/Local/Narada/window-surface-overlays"));
    root.join("operator-console")
}

fn narada_root(site_root: &Path) -> PathBuf {
    env::var_os("NARADA_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|| site_root.parent().unwrap_or(site_root).join("src/narada"))
}

fn ensure_local_runtime(site_root: &Path) -> Result<String, Value> {
    if let Some(url) = env::var("NARADA_OPERATOR_CONSOLE_URL")
        .ok()
        .or_else(|| env::var("NARADA_OPERATOR_ROUTER_URL").ok())
    {
        return Ok(url);
    }
    let port = env::var("NARADA_OPERATOR_CONSOLE_PORT")
        .ok()
        .and_then(|value| value.parse::<u16>().ok())
        .unwrap_or(43117);
    let address = SocketAddr::from(([127, 0, 0, 1], port));
    if TcpStream::connect_timeout(&address, Duration::from_millis(250)).is_err() {
        let executable = env::current_exe().map_err(|cause| {
            error(
                "operator_console_runtime_executable_unavailable",
                &cause.to_string(),
            )
        })?;
        let mut command = Command::new(executable);
        command
            .args([
                "--operator-console-runtime-host",
                "--host",
                "127.0.0.1",
                "--port",
                &port.to_string(),
            ])
            .current_dir(site_root)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            command.creation_flags(0x0800_0000 | 0x0000_0200);
        }
        command
            .spawn()
            .map_err(|cause| error("operator_console_runtime_spawn_failed", &cause.to_string()))?;
        let deadline = Instant::now() + Duration::from_secs(10);
        while TcpStream::connect_timeout(&address, Duration::from_millis(100)).is_err() {
            if Instant::now() >= deadline {
                return Err(error(
                    "operator_console_runtime_readiness_timeout",
                    "operator_console_runtime_readiness_timeout",
                ));
            }
            thread::sleep(Duration::from_millis(50));
        }
    }
    Ok(format!("http://127.0.0.1:{port}"))
}

fn read_json(path: &Path) -> Value {
    fs::metadata(path)
        .ok()
        .filter(|meta| meta.len() <= 256_000)
        .and_then(|_| fs::read(path).ok())
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or(Value::Null)
}

fn write_atomic(path: &Path, value: &Value) -> Result<(), Value> {
    let temporary = path.with_extension(format!("{}.tmp", uuid::Uuid::new_v4()));
    fs::write(
        &temporary,
        serde_json::to_vec_pretty(value).map_err(|cause| {
            error(
                "operator_console_document_encode_failed",
                &cause.to_string(),
            )
        })?,
    )
    .map_err(|cause| error("operator_console_document_write_failed", &cause.to_string()))?;
    if path.exists() {
        fs::remove_file(path).map_err(|cause| {
            error(
                "operator_console_document_replace_failed",
                &cause.to_string(),
            )
        })?;
    }
    fs::rename(temporary, path).map_err(|cause| {
        error(
            "operator_console_document_promote_failed",
            &cause.to_string(),
        )
    })
}

fn wrap(operation: &str, command: &str, overlay: Value) -> Value {
    json!({"schema":"narada.operator_console_overlay.mcp_result.v1","status":"ok","operation":operation,"command":command,"overlay_id":"operator-console","overlay":overlay})
}

fn error(code: &str, message: &str) -> Value {
    json!({"code":code,"message":message})
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn publishes_closed_bounded_native_overlay_contracts() {
        let tools = list_tools();
        assert_eq!(tools.len(), 5);
        for tool in tools {
            assert_eq!(tool.pointer("/inputSchema/type"), Some(&json!("object")));
            assert_eq!(
                tool.pointer("/inputSchema/additionalProperties"),
                Some(&json!(false))
            );
        }
        let open = list_tools()
            .into_iter()
            .find(|tool| tool["name"] == "operator_console_overlay_open")
            .expect("open");
        assert_eq!(
            open.pointer("/inputSchema/properties/url/maxLength"),
            Some(&json!(2048))
        );
        assert_eq!(
            open.pointer("/inputSchema/properties/timeout_ms/maximum"),
            Some(&json!(MAX_TIMEOUT_MS))
        );
    }

    #[test]
    fn status_and_refresh_are_native_state_operations() {
        let root =
            std::env::temp_dir().join(format!("narada-operator-console-{}", uuid::Uuid::new_v4()));
        std::env::set_var("NARADA_WINDOW_SURFACE_OVERLAY_STATE_ROOT", &root);
        let status = call("operator_console_overlay_status", &Map::new(), &root).expect("status");
        assert_eq!(status["operation"], "status");
        let refresh =
            call("operator_console_overlay_refresh", &Map::new(), &root).expect("refresh");
        assert_eq!(refresh["operation"], "refresh");
        assert!(root.join("operator-console/refresh.signal").is_file());
        std::env::remove_var("NARADA_WINDOW_SURFACE_OVERLAY_STATE_ROOT");
        fs::remove_dir_all(root).expect("cleanup");
    }
}
