fn narada_root(site_root: &Path) -> PathBuf {
    env::var_os("NARADA_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|| site_root.to_path_buf())
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
    let narada_root = overlay.get("narada_root").cloned().unwrap_or(Value::Null);
    let mut overlay = overlay;
    if let Some(object) = overlay.as_object_mut() {
        object.remove("narada_root");
    }
    json!({"schema":"narada.operator_console_overlay.mcp_result.v1","status":"ok","operation":operation,"command":command,"overlay_id":"operator-console","narada_root":narada_root,"overlay":overlay})
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
