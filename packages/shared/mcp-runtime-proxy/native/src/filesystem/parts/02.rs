
pub(crate) fn parse_state_for_rhai(args: &[String]) -> Result<State, String> {
    parse_state(args)
}

pub(crate) fn mode_for_rhai(state: &State) -> &str {
    &state.mode
}

pub(crate) fn initialize_for_rhai(request: &Value, mode: &str) -> Value {
    initialize(request, mode)
}

pub(crate) fn tools_list_for_rhai(mode: &str) -> Value {
    json!({"tools": list_tools(mode)})
}

pub(crate) fn tool_call_for_rhai(state: &mut State, params: &Value) -> Value {
    match call_tool(state, params) {
        Ok(result) => json!({"ok": true, "result": result}),
        Err(error) => json!({
            "ok": false,
            "error": {
                "code": -32000,
                "message": error.message,
                "data": diagnostic(&error)
            }
        }),
    }
}

fn parse_roots_config(path: &Path) -> Vec<String> {
    let Ok(text) = fs::read_to_string(path) else {
        return Vec::new();
    };
    let Ok(value) = serde_json::from_str::<Value>(&text) else {
        return Vec::new();
    };
    value
        .get("allowed_roots")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

fn parse_trust_config(path: &Path) -> Vec<String> {
    let Ok(text) = fs::read_to_string(path) else {
        return Vec::new();
    };
    let mut current: Option<String> = None;
    let mut roots = Vec::new();
    for raw in text.lines() {
        let line = raw.trim();
        if let Some(value) = line
            .strip_prefix("[projects.'")
            .and_then(|value| value.strip_suffix("']"))
        {
            current = Some(value.to_string());
        } else if line.starts_with('[') {
            current = None;
        } else if line.eq_ignore_ascii_case("trust_level = \"trusted\"") {
            if let Some(value) = current.clone() {
                roots.push(value);
            }
        }
    }
    roots
}

fn user_home_anchor() -> Option<PathBuf> {
    user_home_anchor_from(|key| env::var_os(key))
}

fn user_home_anchor_from<F>(get: F) -> Option<PathBuf>
where
    F: Fn(&str) -> Option<std::ffi::OsString>,
{
    for key in ["USERPROFILE", "HOME"] {
        if let Some(value) = get(key) {
            if !value.to_string_lossy().trim().is_empty() {
                return Some(PathBuf::from(value));
            }
        }
    }

    #[cfg(windows)]
    {
        if let (Some(drive), Some(path)) = (get("HOMEDRIVE"), get("HOMEPATH")) {
            if !drive.to_string_lossy().trim().is_empty()
                && !path.to_string_lossy().trim().is_empty()
            {
                return Some(PathBuf::from(format!(
                    "{}{}",
                    drive.to_string_lossy(),
                    path.to_string_lossy()
                )));
            }
        }

        for key in ["APPDATA", "LOCALAPPDATA"] {
            if let Some(value) = get(key) {
                if let Some(parent) = Path::new(&value).parent().and_then(Path::parent) {
                    return Some(parent.to_path_buf());
                }
            }
        }
    }

    None
}

fn resolve_anchor(spec: &str) -> Result<String, String> {
    let Some((anchor, relative)) = spec.split_once(':') else {
        return Err(format!("anchored_allowed_root_requires_anchor:{spec}"));
    };
    if relative.is_empty() || Path::new(relative).is_absolute() {
        return Err(format!(
            "anchored_allowed_root_path_must_be_relative:{spec}"
        ));
    }
    let base = match anchor {
        "user_home" => {
            user_home_anchor().ok_or_else(|| "user_home_anchor_unavailable".to_string())?
        }
        _ => return Err(format!("anchored_allowed_root_unknown_anchor:{anchor}")),
    };
    Ok(base.join(relative).to_string_lossy().to_string())
}

fn handle_request(state: &mut State, request: &Value) -> Option<Value> {
    let method = request
        .get("method")
        .and_then(Value::as_str)
        .unwrap_or_default();
    request.get("id")?;
    let id = request.get("id").cloned().unwrap_or(Value::Null);
    let result = match method {
        "initialize" => Ok(initialize(request, &state.mode)),
        "tools/list" => Ok(json!({"tools": list_tools(&state.mode)})),
        "tools/call" => call_tool(state, request.get("params").unwrap_or(&Value::Null)),
        "resources/list" => Ok(json!({"resources": []})),
        "resources/read" => Err(FsError::new(
            "resource_not_found",
            "resource_not_found",
            json!({}),
        )),
        "prompts/list" => Ok(
            json!({"prompts": [{"name": "local_filesystem_tool_usage", "title": "Local Filesystem Tool Usage", "description": format!("Guidance for using local-filesystem-{} tools safely.", state.mode), "arguments": []}]}),
        ),
        "prompts/get" => prompt_get(state, request.get("params").unwrap_or(&Value::Null)),
        "completion/complete" => Ok(completion(
            state,
            request.get("params").unwrap_or(&Value::Null),
        )),
        "logging/setLevel" => Ok(json!({})),
        _ => Err(FsError::new(
            "unsupported_mcp_method",
            format!("unsupported_mcp_method: {method}"),
            json!({"method": method}),
        )),
    };
    Some(match result {
        Ok(value) => json!({"jsonrpc": "2.0", "id": id, "result": value}),
        Err(error) => {
            json!({"jsonrpc": "2.0", "id": id, "error": {"code": -32000, "message": error.message, "data": diagnostic(&error)}})
        }
    })
}

fn initialize(_request: &Value, mode: &str) -> Value {
    json!({
        "protocolVersion": PROTOCOL_VERSION,
        "capabilities": {"tools": {}, "resources": {}, "prompts": {}, "completions": {}, "logging": {}},
        "serverInfo": {"name": format!("local-filesystem-{mode}-native"), "version": "0.1.0"}
    })
}

fn prompt_get(state: &State, params: &Value) -> Result<Value, FsError> {
    let name = params
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if name != "local_filesystem_tool_usage" {
        return Err(FsError::new(
            "unknown_prompt",
            format!("unknown_prompt: {name}"),
            json!({"name": name}),
        ));
    }
    Ok(
        json!({"description": format!("Guidance for using local-filesystem-{} tools safely.", state.mode), "messages": [{"role": "user", "content": {"type": "text", "text": format!("Use local-filesystem-{} tools only within allowed roots. Prefer read/search tools before mutation and preserve structuredContent as authoritative.", state.mode)}}]}),
    )
}

fn completion(state: &State, params: &Value) -> Value {
    let prefix = params
        .pointer("/argument/value")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_ascii_lowercase();
    let values: Vec<String> = state
        .allowed_roots
        .iter()
        .map(|path| normalize_path(path))
        .filter(|value| value.to_ascii_lowercase().starts_with(&prefix))
        .take(100)
        .collect();
    json!({"completion":{"total":values.len(),"hasMore":false,"values":values}})
}
