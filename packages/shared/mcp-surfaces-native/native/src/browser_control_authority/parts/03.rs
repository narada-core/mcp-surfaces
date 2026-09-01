fn click(s: &mut Session, args: &Map<String, Value>) -> Result<Value, Value> {
    let selector = required(args, "selector", MAX_SELECTOR)?;
    let intent = confirm(args)?;
    let node = find_node(s, &selector)?;
    let desc = describe(s, node)?;
    refuse_sensitive(&selector, &desc)?;
    let center = center(s, node)?;
    for (kind, button) in [
        ("mouseMoved", None),
        ("mousePressed", Some("left")),
        ("mouseReleased", Some("left")),
    ] {
        let mut p = json!({"type":kind,"x":center.0,"y":center.1});
        if let Some(b) = button {
            p["button"] = json!(b);
            p["clickCount"] = json!(1);
        }
        cdp(s, "Input.dispatchMouseEvent", p, 8_000)?;
    }
    s.last_action = Some("click".into());
    Ok(result(
        "click",
        json!({"session":s.info(),"selector":selector,"intent":intent,"confirmed":intent!="verify","clicked":true}),
    ))
}
fn fill(s: &mut Session, args: &Map<String, Value>) -> Result<Value, Value> {
    let selector = required(args, "selector", MAX_SELECTOR)?;
    let value = required(args, "value", MAX_TEXT)?;
    let intent = confirm(args)?;
    let node = find_node(s, &selector)?;
    let desc = describe(s, node)?;
    refuse_sensitive(&selector, &desc)?;
    let name = desc
        .get("nodeName")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_ascii_uppercase();
    let editable = desc
        .get("isContentEditable")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if !matches!(name.as_str(), "INPUT" | "TEXTAREA") && !editable {
        return Err(error(
            "fill_target_not_editable",
            "fill is limited to input, textarea, and contenteditable elements.",
            json!({"selector":selector}),
        ));
    }
    cdp(s, "DOM.focus", json!({"nodeId":node}), 8_000)?;
    cdp(
        s,
        "Input.dispatchKeyEvent",
        json!({"type":"keyDown","key":"a","code":"KeyA","modifiers":2}),
        8_000,
    )?;
    cdp(
        s,
        "Input.dispatchKeyEvent",
        json!({"type":"keyUp","key":"a","code":"KeyA","modifiers":2}),
        8_000,
    )?;
    cdp(s, "Input.insertText", json!({"text":value}), 8_000)?;
    s.last_action = Some("fill".into());
    let digest = format!("{:x}", Sha256::digest(value.as_bytes()));
    Ok(result(
        "fill",
        json!({"session":s.info(),"selector":selector,"intent":intent,"confirmed":intent!="verify","filled":true,"value_length":value.chars().count(),"value_sha256":digest}),
    ))
}
fn wait(s: &mut Session, args: &Map<String, Value>) -> Result<Value, Value> {
    let timeout = args
        .get("timeout_ms")
        .and_then(Value::as_u64)
        .unwrap_or(5000)
        .clamp(1, MAX_WAIT_MS);
    let selector = args
        .get("selector")
        .and_then(Value::as_str)
        .map(str::to_string);
    let sleep = args
        .get("sleep_ms")
        .and_then(Value::as_u64)
        .unwrap_or(if selector.is_some() { 0 } else { 250 })
        .min(MAX_WAIT_MS);
    let started = Instant::now();
    if sleep > 0 {
        thread::sleep(Duration::from_millis(sleep.min(timeout)));
    }
    let mut found = selector.is_none();
    while !found && started.elapsed() < Duration::from_millis(timeout) {
        match find_node(s, selector.as_deref().unwrap_or("")) {
            Ok(_) => found = true,
            Err(v) if v.get("code").and_then(Value::as_str) == Some("selector_not_found") => {
                thread::sleep(Duration::from_millis(100).min(Duration::from_millis(timeout)))
            }
            Err(v) => return Err(v),
        }
    }
    s.last_action = Some("wait".into());
    Ok(result(
        "wait",
        json!({"session":s.info(),"selector":selector,"found":found,"elapsed_ms":started.elapsed().as_millis(),"timed_out":!found}),
    ))
}
fn assertion(s: &mut Session, args: &Map<String, Value>) -> Result<Value, Value> {
    let selector = required(args, "selector", MAX_SELECTOR)?;
    let node = find_node(s, &selector)?;
    let out = cdp(s, "DOM.getOuterHTML", json!({"nodeId":node}), 8_000)?;
    let html = out.get("outerHTML").and_then(Value::as_str).unwrap_or("");
    let expected = args.get("contains_text").and_then(Value::as_str);
    Ok(
        json!({"schema":"narada.browser_control.assertion.v1","status":"ok","session":s.info(),"selector":selector,"matched":expected.map(|v|html.contains(v)).unwrap_or(true),"contains_text_requested":expected.is_some(),"contains_text_length":expected.map(str::len).unwrap_or(0)}),
    )
}

fn find_node(s: &mut Session, selector: &str) -> Result<u64, Value> {
    let doc = cdp(
        s,
        "DOM.getDocument",
        json!({"depth":1,"pierce":false}),
        8_000,
    )?;
    let root = doc
        .pointer("/root/nodeId")
        .and_then(Value::as_u64)
        .ok_or_else(|| {
            error(
                "dom_document_missing",
                "The browser did not return a DOM document.",
                json!({}),
            )
        })?;
    let out = cdp(
        s,
        "DOM.querySelector",
        json!({"nodeId":root,"selector":selector}),
        8_000,
    )?;
    let node = out.get("nodeId").and_then(Value::as_u64).unwrap_or(0);
    if node == 0 {
        Err(error(
            "selector_not_found",
            "The selector did not match an element.",
            json!({"selector":selector}),
        ))
    } else {
        Ok(node)
    }
}
fn describe(s: &mut Session, node: u64) -> Result<Value, Value> {
    Ok(cdp(s, "DOM.describeNode", json!({"nodeId":node}), 8_000)?
        .get("node")
        .cloned()
        .unwrap_or_else(|| json!({})))
}
fn center(s: &mut Session, node: u64) -> Result<(f64, f64), Value> {
    cdp(
        s,
        "DOM.scrollIntoViewIfNeeded",
        json!({"nodeId":node}),
        8_000,
    )?;
    let out = cdp(s, "DOM.getBoxModel", json!({"nodeId":node}), 8_000)?;
    let q = out
        .pointer("/model/content")
        .or_else(|| out.pointer("/model/border"))
        .and_then(Value::as_array)
        .ok_or_else(|| {
            error(
                "element_box_missing",
                "The selected element has no usable layout box.",
                json!({}),
            )
        })?;
    if q.len() < 8 {
        return Err(error(
            "element_box_missing",
            "The selected element has no usable layout box.",
            json!({}),
        ));
    }
    let n = |i: usize| q[i].as_f64().unwrap_or(0.0);
    Ok((
        (n(0) + n(2) + n(4) + n(6)) / 4.0,
        (n(1) + n(3) + n(5) + n(7)) / 4.0,
    ))
}
fn cdp(s: &mut Session, method: &str, params: Value, timeout_ms: u64) -> Result<Value, Value> {
    let timeout_ms = std::env::var("NARADA_BROWSER_CONTROL_CDP_TIMEOUT_MS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .map(|configured| timeout_ms.min(configured.clamp(50, 30_000)))
        .unwrap_or(timeout_ms);
    let id = s.next_id;
    s.next_id += 1;
    match s.socket.get_mut() {
        MaybeTlsStream::Plain(stream) => {
            stream
                .set_read_timeout(Some(Duration::from_millis(timeout_ms)))
                .ok();
        }
        MaybeTlsStream::Rustls(stream) => {
            stream
                .sock
                .set_read_timeout(Some(Duration::from_millis(timeout_ms)))
                .ok();
        }
        _ => {}
    }
    s.socket
        .send(Message::Text(
            json!({"id":id,"method":method,"params":params})
                .to_string()
                .into(),
        ))
        .map_err(|c| error("cdp_write_failed", &c.to_string(), json!({"method":method})))?;
    loop {
        match s.socket.read() {
            Ok(Message::Text(text)) => {
                let value: Value = serde_json::from_str(&text).map_err(|_| {
                    error(
                        "cdp_response_invalid",
                        "CDP returned invalid JSON.",
                        json!({"method":method}),
                    )
                })?;
                if value.get("id").and_then(Value::as_u64) != Some(id) {
                    continue;
                }
                if let Some(e) = value.get("error") {
                    return Err(error(
                        "cdp_command_failed",
                        "The browser rejected the CDP command.",
                        json!({"method":method,"cdp_error":e}),
                    ));
                }
                return Ok(value.get("result").cloned().unwrap_or_else(|| json!({})));
            }
            Ok(Message::Ping(v)) => {
                s.socket.send(Message::Pong(v)).ok();
            }
            Ok(Message::Close(_)) => {
                return Err(error(
                    "cdp_connection_closed",
                    "The browser closed the CDP connection.",
                    json!({"method":method}),
                ))
            }
            Ok(_) => {}
            Err(c) => {
                return Err(error(
                    "cdp_response_timeout_or_read_failed",
                    &c.to_string(),
                    json!({"method":method,"timeout_ms":timeout_ms}),
                ))
            }
        }
    }
}

