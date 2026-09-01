pub fn auxiliary(method: &str, params: &Map<String, Value>) -> Result<Value, Value> {
    match method {
        "prompts/list" => Ok(
            json!({"prompts":[{"name":"browser_control_workflow","title":"Browser Control Workflow","description":"Attach explicitly to a loopback CDP page, act within exact origins, and detach.","arguments":[]}]}),
        ),
        "prompts/get" => {
            if params.get("name").and_then(Value::as_str) != Some("browser_control_workflow") {
                return Err(error(
                    "unknown_prompt",
                    "Unknown browser-control prompt.",
                    json!({}),
                ));
            }
            Ok(
                json!({"description":"Native browser-control workflow","messages":[{"role":"user","content":{"type":"text","text":"List attached sessions; attach only to an operator-selected loopback CDP target; use exact allowed origins; never put secrets into fill; confirm consequential actions; detach when finished."}}]}),
            )
        }
        "completion/complete" => {
            let values = if params
                .get("argument")
                .and_then(Value::as_object)
                .and_then(|argument| argument.get("name"))
                .and_then(Value::as_str)
                == Some("name")
            {
                list_tools()
                    .into_iter()
                    .filter_map(|value| value.get("name").cloned())
                    .collect::<Vec<_>>()
            } else {
                Vec::new()
            };
            Ok(json!({"completion":{"total":values.len(),"hasMore":false,"values":values}}))
        }
        "logging/setLevel" => Ok(json!({})),
        _ => Err(error(
            "unsupported_mcp_method",
            &format!("unsupported_mcp_method:{method}"),
            json!({"method":method}),
        )),
    }
}

fn attach(args: &Map<String, Value>, root: &Path) -> Result<Value, Value> {
    let profile = required(args, "profile_id", 200)?;
    let sid = required(args, "session_id", 300)?;
    let k = key(&profile, &sid);
    if sessions()
        .lock()
        .map_err(|_| {
            error(
                "browser_session_state_poisoned",
                "Session state is unavailable.",
                json!({}),
            )
        })?
        .contains_key(&k)
    {
        return Err(error(
            "browser_session_already_attached",
            "The selected session is already attached; use status or detach first.",
            json!({"profile_id":profile,"session_id":sid}),
        ));
    }
    let endpoint = validate_http_endpoint(&required(args, "cdp_endpoint", 2048)?)?;
    let origins = normalize_origins(args.get("allowed_origins"))?;
    let targets = list_targets(&endpoint)?;
    let target=targets.iter().find(|v| v.get("id").and_then(Value::as_str)==Some(&sid) && v.get("type").and_then(Value::as_str)==Some("page")).cloned().ok_or_else(||error("browser_session_not_found","The explicitly selected page target was not found.",json!({"profile_id":profile,"session_id":sid,"available_session_ids":targets.iter().filter(|v|v.get("type").and_then(Value::as_str)==Some("page")).filter_map(|v|v.get("id").and_then(Value::as_str)).take(50).collect::<Vec<_>>() })))?;
    let ws = target
        .get("webSocketDebuggerUrl")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            error(
                "browser_target_websocket_missing",
                "The selected target has no debugger WebSocket URL.",
                json!({}),
            )
        })?;
    validate_ws_endpoint(ws)?;
    let (socket, _) = connect(ws).map_err(|cause| {
        error(
            "browser_websocket_connect_failed",
            &cause.to_string(),
            json!({"endpoint":redact_url(ws)}),
        )
    })?;
    let mut guard = sessions().lock().map_err(|_| {
        error(
            "browser_session_state_poisoned",
            "Session state is unavailable.",
            json!({}),
        )
    })?;
    let mut session = Session {
        profile_id: profile,
        session_id: sid,
        cdp_endpoint: endpoint,
        allowed_origins: origins,
        target,
        attached_at: now(),
        last_action: None,
        next_id: 1,
        socket,
    };
    for method in ["Page.enable", "DOM.enable", "Accessibility.enable"] {
        cdp(&mut session, method, json!({}), 8_000)?;
    }
    let info = session.info();
    receipt(root, "attach", &info)?;
    guard.insert(k, session);
    Ok(result("attach", info))
}

fn inventory() -> Result<Value, Value> {
    let guard = sessions().lock().map_err(|_| {
        error(
            "browser_session_state_poisoned",
            "Session state is unavailable.",
            json!({}),
        )
    })?;
    let values = guard.values().map(Session::info).collect::<Vec<_>>();
    Ok(
        json!({"schema":"narada.browser_control.result.v1","status":"ok","operation":"session_inventory","count":values.len(),"sessions":values,"persistence":"process_local"}),
    )
}
fn with_session<F>(
    args: &Map<String, Value>,
    root: &Path,
    operation: &str,
    f: F,
) -> Result<Value, Value>
where
    F: FnOnce(&mut Session) -> Result<Value, Value>,
{
    let profile = required(args, "profile_id", 200)?;
    let sid = required(args, "session_id", 300)?;
    let k = key(&profile, &sid);
    let mut guard = sessions().lock().map_err(|_| {
        error(
            "browser_session_state_poisoned",
            "Session state is unavailable.",
            json!({}),
        )
    })?;
    let session = guard.get_mut(&k).ok_or_else(|| {
        error(
            "browser_session_not_attached",
            "Attach this exact profile/session first.",
            json!({"profile_id":profile,"session_id":sid,"recovery":"browser_control_attach"}),
        )
    })?;
    let value = f(session)?;
    receipt(
        root,
        operation,
        &json!({"profile_id":profile,"session_id":sid,"status":"ok"}),
    )?;
    Ok(value)
}
fn detach(args: &Map<String, Value>, root: &Path) -> Result<Value, Value> {
    let profile = required(args, "profile_id", 200)?;
    let sid = required(args, "session_id", 300)?;
    let removed = sessions()
        .lock()
        .map_err(|_| {
            error(
                "browser_session_state_poisoned",
                "Session state is unavailable.",
                json!({}),
            )
        })?
        .remove(&key(&profile, &sid));
    let Some(mut session) = removed else {
        return Err(error(
            "browser_session_not_attached",
            "The selected session is not attached.",
            json!({"profile_id":profile,"session_id":sid}),
        ));
    };
    let _ = session.socket.close(None);
    let value = json!({"profile_id":profile,"session_id":sid,"detached":true});
    receipt(root, "detach", &value)?;
    Ok(result("detach", value))
}

impl Session {
    fn info(&self) -> Value {
        json!({"profile_id":self.profile_id,"session_id":self.session_id,"cdp_endpoint":self.cdp_endpoint,"allowed_origins":self.allowed_origins,"target":{"id":self.target.get("id").cloned().unwrap_or(Value::Null),"type":self.target.get("type").cloned().unwrap_or(Value::Null),"title":safe(self.target.get("title"),300),"url":redact_url(self.target.get("url").and_then(Value::as_str).unwrap_or(""))},"attached_at":self.attached_at,"last_action":self.last_action,"persistence":"process_local"})
    }
}
fn refresh(s: &mut Session) -> Result<(), Value> {
    if let Some(target) = list_targets(&s.cdp_endpoint)?
        .into_iter()
        .find(|v| v.get("id").and_then(Value::as_str) == Some(&s.session_id))
    {
        s.target = target;
    }
    Ok(())
}
fn navigate(s: &mut Session, args: &Map<String, Value>) -> Result<Value, Value> {
    let url = required(args, "url", 4000)?;
    let parsed = Url::parse(&url).map_err(|_| {
        error(
            "navigation_url_invalid",
            "url must be an absolute HTTP(S) URL.",
            json!({}),
        )
    })?;
    let origin = origin(&parsed)?;
    if !s.allowed_origins.contains(&origin) {
        return Err(error(
            "navigation_origin_refused",
            "The URL origin is not in this session's exact allowlist.",
            json!({"origin":origin,"allowed_origins":s.allowed_origins}),
        ));
    }
    let out = cdp(s, "Page.navigate", json!({"url":url}), 8_000)?;
    s.last_action = Some("navigate".into());
    Ok(result(
        "navigate",
        json!({"session":s.info(),"url":redact_url(&url),"frame_id":out.get("frameId"),"navigation_error":out.get("errorText")}),
    ))
}
fn snapshot(s: &mut Session, args: &Map<String, Value>, root: &Path) -> Result<Value, Value> {
    let max = args
        .get("max_nodes")
        .and_then(Value::as_u64)
        .unwrap_or(200)
        .clamp(1, MAX_NODES as u64) as usize;
    let out = cdp(s, "Accessibility.getFullAXTree", json!({}), 8_000)?;
    let all = out
        .get("nodes")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let nodes=all.iter().take(max).map(|n|json!({"node_id":safe(n.get("nodeId"),100),"ignored":n.get("ignored").and_then(Value::as_bool).unwrap_or(false),"role":property_value(n.get("role")),"name":property_value(n.get("name")),"description":property_value(n.get("description")),"value_available":!property_value(n.get("value")).is_empty(),"properties":n.get("properties").and_then(Value::as_array).map(|p|p.iter().filter(|v|matches!(v.get("name").and_then(Value::as_str),Some("checked"|"disabled"|"expanded"|"focused"|"selected"|"level"))).map(|v|json!({"name":safe(v.get("name"),80),"value":property_value(v.get("value"))})).collect::<Vec<_>>()).unwrap_or_default()})).collect::<Vec<_>>();
    let value = json!({"schema":"narada.browser_control.accessibility_snapshot.v1","status":"ok","session":s.info(),"node_count":nodes.len(),"truncated":all.len()>nodes.len(),"nodes":nodes});
    if serde_json::to_vec(&value).map(|encoded| encoded.len()).unwrap_or(usize::MAX) > 64 * 1024 {
        materialize_output(root, "browser_control_accessibility_snapshot", value)
    } else {
        Ok(value)
    }
}
fn screenshot(s: &mut Session, args: &Map<String, Value>, root: &Path) -> Result<Value, Value> {
    let format = args.get("format").and_then(Value::as_str).unwrap_or("png");
    let mut p = json!({"format":format,"fromSurface":true});
    if let Some(q) = args.get("quality") {
        p["quality"] = q.clone();
    }
    let out = cdp(s, "Page.captureScreenshot", p, 15_000)?;
    let data = out.get("data").and_then(Value::as_str).unwrap_or("");
    if data.is_empty() {
        return Err(error(
            "screenshot_empty",
            "The browser returned an empty screenshot.",
            json!({}),
        ));
    }
    if data.len() > MAX_SCREENSHOT_BASE64 {
        return Err(error(
            "screenshot_too_large",
            "The screenshot exceeds the bounded 10 MiB base64 limit.",
            json!({"base64_length":data.len()}),
        ));
    }
    let value = json!({"schema":"narada.browser_control.screenshot.v1","status":"ok","session":s.info(),"content_type":if format=="jpeg"{"image/jpeg"}else{"image/png"},"encoding":"base64","byte_length":data.len()*3/4,"data_base64":data});
    if data.len() > 64 * 1024 {
        materialize_output(root, "browser_control_screenshot", value)
    } else {
        Ok(value)
    }
}
