use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::net::{IpAddr, TcpStream};
use std::path::Path;
use std::sync::{Mutex, OnceLock};
use std::thread;
use std::time::{Duration, Instant};
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;
use tungstenite::stream::MaybeTlsStream;
use tungstenite::{connect, Message, WebSocket};
use url::Url;
use uuid::Uuid;

const MAX_SELECTOR: usize = 512;
const MAX_TEXT: usize = 4_000;
const MAX_NODES: usize = 500;
const MAX_WAIT_MS: u64 = 15_000;
const MAX_SCREENSHOT_BASE64: usize = 10 * 1024 * 1024;
const MAX_TARGET_RESPONSE: u64 = 2 * 1024 * 1024;

struct Session {
    profile_id: String,
    session_id: String,
    cdp_endpoint: String,
    allowed_origins: Vec<String>,
    target: Value,
    attached_at: String,
    last_action: Option<String>,
    next_id: u64,
    socket: WebSocket<MaybeTlsStream<TcpStream>>,
}

static SESSIONS: OnceLock<Mutex<HashMap<String, Session>>> = OnceLock::new();
fn sessions() -> &'static Mutex<HashMap<String, Session>> {
    SESSIONS.get_or_init(|| Mutex::new(HashMap::new()))
}
fn key(profile: &str, session: &str) -> String {
    format!("{profile}\0{session}")
}

pub struct ShutdownGuard;
impl Drop for ShutdownGuard {
    fn drop(&mut self) {
        shutdown();
    }
}
pub fn shutdown_guard() -> ShutdownGuard {
    ShutdownGuard
}
fn shutdown() {
    if let Ok(mut guard) = sessions().lock() {
        for (_, mut session) in guard.drain() {
            let _ = session.socket.close(None);
        }
    }
}

pub fn list_tools() -> Vec<Value> {
    let session = || {
        json!({
            "profile_id":{"type":"string","minLength":1,"maxLength":200},
            "session_id":{"type":"string","minLength":1,"maxLength":300}
        })
    };
    let mut tools = vec![
        tool(
            "browser_control_guidance",
            "Show the native browser-control workflow and safety boundaries.",
            json!({"type":"object","properties":{"workflow":{"type":"string","maxLength":256},"tool":{"type":"string","maxLength":256}},"additionalProperties":false}),
            true,
        ),
        tool(
            "browser_control_session_inventory",
            "List explicitly attached browser sessions without discovering or launching browsers.",
            empty(),
            true,
        ),
    ];
    let mut attach = Map::new();
    attach.extend(session().as_object().cloned().unwrap_or_default());
    attach.insert(
        "cdp_endpoint".into(),
        json!({"type":"string","minLength":1,"maxLength":2048}),
    );
    attach.insert("allowed_origins".into(), json!({"type":"array","minItems":1,"maxItems":32,"items":{"type":"string","minLength":1,"maxLength":2048}}));
    tools.push(tool(
        "browser_control_attach",
        "Attach to one explicitly selected page target through a loopback CDP endpoint.",
        object(
            attach,
            &[
                "profile_id",
                "session_id",
                "cdp_endpoint",
                "allowed_origins",
            ],
        ),
        false,
    ));
    tools.push(tool(
        "browser_control_status",
        "Refresh one attached session from its loopback CDP target list.",
        object(
            session().as_object().cloned().unwrap_or_default(),
            &["profile_id", "session_id"],
        ),
        true,
    ));
    let action = |extra: Map<String, Value>, required: &[&str]| {
        let mut p = session().as_object().cloned().unwrap_or_default();
        p.extend(extra);
        object(p, required)
    };
    tools.push(tool(
        "browser_control_navigate",
        "Navigate within the session's exact origin allowlist.",
        action(
            Map::from_iter([(
                "url".into(),
                json!({"type":"string","minLength":1,"maxLength":4000}),
            )]),
            &["profile_id", "session_id", "url"],
        ),
        false,
    ));
    tools.push(tool(
        "browser_control_accessibility_snapshot",
        "Return a bounded redacted accessibility tree.",
        action(
            Map::from_iter([(
                "max_nodes".into(),
                json!({"type":"integer","minimum":1,"maximum":500,"default":200}),
            )]),
            &["profile_id", "session_id"],
        ),
        true,
    ));
    tools.push(tool("browser_control_screenshot", "Capture a bounded screenshot; transport output references remain page-readable with mcp_output_show.", action(Map::from_iter([("format".into(),json!({"type":"string","enum":["png","jpeg"],"default":"png"})),("quality".into(),json!({"type":"integer","minimum":0,"maximum":100}))]), &["profile_id","session_id"]), true));
    let intent = || {
        Map::from_iter([
            (
                "selector".into(),
                json!({"type":"string","minLength":1,"maxLength":512}),
            ),
            (
                "intent".into(),
                json!({"type":"string","enum":["verify","login","submit","destructive"],"default":"verify"}),
            ),
            (
                "confirmed".into(),
                json!({"type":"boolean","default":false}),
            ),
        ])
    };
    tools.push(tool("browser_control_click", "Click one non-sensitive CSS-selected element with explicit confirmation for consequential intent.", action(intent(), &["profile_id","session_id","selector"]), false));
    let mut fill = intent();
    fill.insert(
        "value".into(),
        json!({"type":"string","minLength":1,"maxLength":4000}),
    );
    tools.push(tool("browser_control_fill", "Fill a non-sensitive editable element; secret and authentication fields are always refused.", action(fill, &["profile_id","session_id","selector","value"]), false));
    tools.push(tool(
        "browser_control_wait",
        "Wait for a bounded duration or CSS selector.",
        action(
            Map::from_iter([
                (
                    "selector".into(),
                    json!({"type":"string","minLength":1,"maxLength":512}),
                ),
                (
                    "sleep_ms".into(),
                    json!({"type":"integer","minimum":0,"maximum":15000,"default":250}),
                ),
                (
                    "timeout_ms".into(),
                    json!({"type":"integer","minimum":1,"maximum":15000,"default":5000}),
                ),
            ]),
            &["profile_id", "session_id"],
        ),
        false,
    ));
    tools.push(tool("browser_control_assert", "Assert bounded element existence and optional text containment without returning page markup.", action(Map::from_iter([("selector".into(),json!({"type":"string","minLength":1,"maxLength":512})),("contains_text".into(),json!({"type":"string","minLength":1,"maxLength":4000}))]), &["profile_id","session_id","selector"]), true));
    tools.push(tool(
        "browser_control_detach",
        "Close one native CDP attachment without changing browser lifecycle.",
        object(
            session().as_object().cloned().unwrap_or_default(),
            &["profile_id", "session_id"],
        ),
        false,
    ));
    tools.push(tool("mcp_output_show", "Read a bounded page from a materialized browser-control output.", json!({"type":"object","properties":{"ref":{"type":"string","minLength":1,"maxLength":4096},"output_ref":{"type":"string","minLength":1,"maxLength":4096},"offset":{"type":"integer","minimum":0,"maximum":1073741824,"default":0},"limit":{"type":"integer","minimum":1,"maximum":20000,"default":10000}},"additionalProperties":false}), true));
    tools
}

pub fn call_tool(name: &str, args: &Map<String, Value>, root: &Path) -> Result<Value, Value> {
    match name {
        "browser_control_guidance" => Ok(
            json!({"schema":"narada.browser_control.guidance.v1","status":"ok","workflow":["Start the browser with a loopback CDP endpoint outside this surface.","Attach by exact profile_id and page target id with exact allowed origins.","Use snapshots/assertions for verification; consequential click/fill requires confirmed:true.","Detach explicitly; EOF also drops all in-process attachments."],"boundaries":{"launches_browser":false,"discovers_non_loopback":false,"accepts_secrets":false,"persists_sessions":false},"requested":args}),
        ),
        "browser_control_session_inventory" => inventory(),
        "browser_control_attach" => attach(args, root),
        "browser_control_status" => with_session(args, root, "status", |session| {
            refresh(session)?;
            Ok(result("status", session.info()))
        }),
        "browser_control_navigate" => {
            with_session(args, root, "navigate", |session| navigate(session, args))
        }
        "browser_control_accessibility_snapshot" => {
            with_session(args, root, "accessibility_snapshot", |session| {
                snapshot(session, args, root)
            })
        }
        "browser_control_screenshot" => with_session(args, root, "screenshot", |session| {
            screenshot(session, args, root)
        }),
        "browser_control_click" => {
            with_session(args, root, "click", |session| click(session, args))
        }
        "browser_control_fill" => with_session(args, root, "fill", |session| fill(session, args)),
        "browser_control_wait" => with_session(args, root, "wait", |session| wait(session, args)),
        "browser_control_assert" => {
            with_session(args, root, "assert", |session| assertion(session, args))
        }
        "browser_control_detach" => detach(args, root),
        "mcp_output_show" => super::host_contracts::output_show(args, root),
        _ => Err(error(
            "unknown_tool",
            &format!("unknown_tool:{name}"),
            json!({"tool_name":name}),
        )),
    }
}

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

fn validate_http_endpoint(value: &str) -> Result<String, Value> {
    let mut url = Url::parse(value).map_err(|_| {
        error(
            "cdp_endpoint_invalid",
            "CDP endpoint must be an absolute loopback HTTP(S) URL.",
            json!({}),
        )
    })?;
    if !matches!(url.scheme(), "http" | "https")
        || !loopback(&url)
        || url.path() != "/"
        || url.query().is_some()
        || url.fragment().is_some()
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return Err(error(
            "cdp_endpoint_refused",
            "CDP endpoint must be a credential-free loopback HTTP(S) origin with root path.",
            json!({}),
        ));
    }
    url.set_path("");
    Ok(url.to_string().trim_end_matches('/').to_string())
}
fn validate_ws_endpoint(value: &str) -> Result<(), Value> {
    let url = Url::parse(value).map_err(|_| {
        error(
            "browser_websocket_url_invalid",
            "Debugger URL is invalid.",
            json!({}),
        )
    })?;
    if !matches!(url.scheme(), "ws" | "wss")
        || !loopback(&url)
        || url.query().is_some()
        || url.fragment().is_some()
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return Err(error(
            "browser_websocket_url_refused",
            "Debugger WebSocket must be credential-free and loopback-only.",
            json!({}),
        ));
    }
    Ok(())
}
fn loopback(url: &Url) -> bool {
    url.host_str().is_some_and(|h| {
        h.eq_ignore_ascii_case("localhost") || h.parse::<IpAddr>().is_ok_and(|ip| ip.is_loopback())
    })
}
fn list_targets(endpoint: &str) -> Result<Vec<Value>, Value> {
    let response = ureq::get(&format!("{endpoint}/json/list"))
        .timeout(Duration::from_secs(5))
        .call()
        .map_err(|c| {
            error(
                "cdp_target_list_failed",
                &c.to_string(),
                json!({"endpoint":endpoint}),
            )
        })?;
    if response
        .header("content-length")
        .and_then(|v| v.parse::<u64>().ok())
        .is_some_and(|n| n > MAX_TARGET_RESPONSE)
    {
        return Err(error(
            "cdp_target_list_too_large",
            "CDP target response exceeds 2 MiB.",
            json!({}),
        ));
    }
    let mut reader = response.into_reader();
    let mut reader = (&mut reader).take(MAX_TARGET_RESPONSE + 1);
    let mut bytes = Vec::new();
    std::io::Read::read_to_end(&mut reader, &mut bytes)
        .map_err(|c| error("cdp_target_list_read_failed", &c.to_string(), json!({})))?;
    if bytes.len() as u64 > MAX_TARGET_RESPONSE {
        return Err(error(
            "cdp_target_list_too_large",
            "CDP target response exceeds 2 MiB.",
            json!({}),
        ));
    }
    serde_json::from_slice::<Vec<Value>>(&bytes).map_err(|_| {
        error(
            "cdp_target_list_invalid",
            "CDP target response is not a JSON array.",
            json!({}),
        )
    })
}
fn normalize_origins(value: Option<&Value>) -> Result<Vec<String>, Value> {
    let arr = value.and_then(Value::as_array).ok_or_else(|| {
        error(
            "allowed_origins_required",
            "allowed_origins must contain 1 to 32 exact origins.",
            json!({}),
        )
    })?;
    if arr.is_empty() || arr.len() > 32 {
        return Err(error(
            "allowed_origins_bounded",
            "allowed_origins must contain 1 to 32 exact origins.",
            json!({"count":arr.len()}),
        ));
    }
    let mut out = Vec::new();
    for item in arr {
        let raw = item.as_str().ok_or_else(|| {
            error(
                "allowed_origin_invalid",
                "Each allowed origin must be a string.",
                json!({}),
            )
        })?;
        let url = Url::parse(raw).map_err(|_| {
            error(
                "allowed_origin_invalid",
                "Allowed origins must be absolute HTTP(S) origins.",
                json!({}),
            )
        })?;
        let normalized = origin(&url)?;
        if raw.trim_end_matches('/') != normalized {
            return Err(error("allowed_origin_not_exact","Allowed entries must be exact origins without paths, queries, credentials, or fragments.",json!({"value":redact_url(raw),"expected":normalized})));
        }
        if !out.contains(&normalized) {
            out.push(normalized);
        }
    }
    Ok(out)
}
fn origin(url: &Url) -> Result<String, Value> {
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return Err(error(
            "http_origin_invalid",
            "Expected a credential-free HTTP(S) URL.",
            json!({}),
        ));
    }
    let host = url.host_str().unwrap();
    let port = url.port().map(|p| format!(":{p}")).unwrap_or_default();
    Ok(format!("{}://{}{}", url.scheme(), host, port))
}
fn confirm(args: &Map<String, Value>) -> Result<&str, Value> {
    let intent = args
        .get("intent")
        .and_then(Value::as_str)
        .unwrap_or("verify");
    if intent != "verify" && args.get("confirmed").and_then(Value::as_bool) != Some(true) {
        return Err(error(
            "confirmation_required",
            &format!("confirmed:true is required for {intent} intent."),
            json!({"intent":intent,"required":"confirmed:true"}),
        ));
    }
    Ok(intent)
}
fn refuse_sensitive(selector: &str, node: &Value) -> Result<(), Value> {
    let mut hay = selector.to_string();
    hay.push(' ');
    hay.push_str(node.get("nodeName").and_then(Value::as_str).unwrap_or(""));
    if let Some(attrs) = node.get("attributes").and_then(Value::as_array) {
        for v in attrs {
            hay.push(' ');
            hay.push_str(v.as_str().unwrap_or(""));
        }
    }
    let lower = hay.to_ascii_lowercase();
    if [
        "password",
        "passcode",
        "token",
        "secret",
        "api-key",
        "api_key",
        "api key",
        "cookie",
        "authorization",
        "credential",
        "private-key",
        "private_key",
        "client-secret",
        "client_secret",
        "client secret",
    ]
    .iter()
    .any(|v| lower.contains(v))
    {
        Err(error(
            "sensitive_field_refused",
            "Password, token, secret, cookie, and authentication fields are never accepted.",
            json!({"selector":selector}),
        ))
    } else {
        Ok(())
    }
}
fn required(args: &Map<String, Value>, name: &str, max: usize) -> Result<String, Value> {
    let value = args
        .get(name)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .ok_or_else(|| {
            error(
                "argument_required",
                &format!("{name} is required."),
                json!({"field":name}),
            )
        })?;
    if value.len() > max {
        return Err(error(
            "argument_too_long",
            &format!("{name} exceeds its bounded length."),
            json!({"field":name,"max_length":max}),
        ));
    }
    Ok(value.to_string())
}
fn property_value(v: Option<&Value>) -> String {
    v.and_then(|x| x.get("value").or(Some(x)))
        .map(|x| safe(Some(x), 600))
        .unwrap_or_default()
}
fn safe(v: Option<&Value>, max: usize) -> String {
    let text = v
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect::<String>();
    text.trim().chars().take(max).collect()
}
fn redact_url(value: &str) -> String {
    Url::parse(value)
        .map(|mut u| {
            let _ = u.set_username("");
            let _ = u.set_password(None);
            let keys = u
                .query_pairs()
                .filter(|(k, _)| is_sensitive(k))
                .map(|(k, _)| k.into_owned())
                .collect::<Vec<_>>();
            for k in keys {
                u.query_pairs_mut().clear().append_pair(&k, "[redacted]");
            }
            if u.fragment().is_some() {
                u.set_fragment(Some("[redacted]"));
            }
            u.to_string()
        })
        .unwrap_or_else(|_| value.chars().take(2000).collect())
}
fn is_sensitive(v: &str) -> bool {
    let s = v.to_ascii_lowercase();
    [
        "password",
        "passcode",
        "token",
        "secret",
        "api_key",
        "api-key",
        "cookie",
        "authorization",
        "credential",
    ]
    .iter()
    .any(|k| s.contains(k))
}
fn materialize_output(root: &Path, tool_name: &str, full_output: Value) -> Result<Value, Value> {
    let id = Uuid::new_v4().simple().to_string();
    let reference = format!("mcp_output:{id}");
    let directory = root.join(".ai/tmp/mcp-outputs/workspace");
    fs::create_dir_all(&directory).map_err(|cause| {
        error(
            "output_directory_create_failed",
            &cause.to_string(),
            json!({"path":directory}),
        )
    })?;
    let presentation =
        serde_json::to_string_pretty(&full_output).unwrap_or_else(|_| full_output.to_string());
    let record = json!({"schema":"narada.mcp_output_ref.v1","ref":reference,"output_id":id,"tool_name":tool_name,"full_output_char_length":presentation.chars().count(),"truncated":false,"full_output":full_output});
    let encoded = serde_json::to_vec(&record)
        .map_err(|cause| error("output_encode_failed", &cause.to_string(), json!({})))?;
    if encoded.len() > 10 * 1024 * 1024 {
        return Err(error(
            "output_ref_too_large",
            "Materialized output exceeds the 10 MiB store limit.",
            json!({"byte_length":encoded.len()}),
        ));
    }
    let path = directory.join(format!("{id}.json"));
    fs::write(&path, encoded).map_err(|cause| {
        error(
            "output_write_failed",
            &cause.to_string(),
            json!({"path":path}),
        )
    })?;
    Ok(
        json!({"schema":"narada.mcp_output_preview.v1","status":"ok","tool_name":tool_name,"ref":reference,"output_ref":reference,"full_output_char_length":presentation.chars().count(),"output_truncated":true,"remediation":"Call mcp_output_show with output_ref and bounded offset/limit."}),
    )
}
fn receipt(root: &Path, operation: &str, details: &Value) -> Result<(), Value> {
    let path = root.join(".ai/tmp/browser-control/action-receipts.jsonl");
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|c| {
            error(
                "browser_receipt_directory_failed",
                &c.to_string(),
                json!({"path":parent}),
            )
        })?;
    }
    let line=serde_json::to_string(&json!({"schema":"narada.browser_control.action_receipt.v1","receipt_id":format!("browser-receipt-{}",Uuid::new_v4()),"operation":operation,"recorded_at":now(),"details":details})).unwrap_or_default();
    if line.len() > 65_536 {
        return Err(error(
            "browser_receipt_too_large",
            "Browser action receipt exceeds 64 KiB.",
            json!({}),
        ));
    }
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .map_err(|c| {
            error(
                "browser_receipt_open_failed",
                &c.to_string(),
                json!({"path":path}),
            )
        })?;
    writeln!(file, "{line}").map_err(|c| {
        error(
            "browser_receipt_write_failed",
            &c.to_string(),
            json!({"path":path}),
        )
    })
}
fn now() -> String {
    OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .unwrap_or_else(|_| "1970-01-01T00:00:00Z".into())
}
fn empty() -> Value {
    json!({"type":"object","properties":{},"additionalProperties":false})
}
fn object(properties: Map<String, Value>, required: &[&str]) -> Value {
    json!({"type":"object","properties":properties,"required":required,"additionalProperties":false})
}
fn tool(name: &str, description: &str, input: Value, read_only: bool) -> Value {
    json!({"name":name,"description":description,"inputSchema":input,"annotations":{"title":name,"readOnlyHint":read_only,"destructiveHint":!read_only,"idempotentHint":name!="browser_control_attach","openWorldHint":false},"outputSchema":{"type":"object","additionalProperties":true}})
}
fn result(operation: &str, value: Value) -> Value {
    json!({"schema":"narada.browser_control.result.v1","status":"ok","operation":operation,"result":value})
}
fn error(code: &str, message: &str, details: Value) -> Value {
    json!({"schema":"narada.browser_control.error.v1","status":"unavailable","code":code,"message":message,"details":details})
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn catalog_is_exact_closed_and_bounded() {
        let tools = list_tools();
        assert_eq!(tools.len(), 13);
        for t in tools {
            let s = &t["inputSchema"];
            assert!(s.get("title").is_none());
            assert_eq!(s["additionalProperties"], false);
        }
    }
    #[test]
    fn origins_are_exact_and_credentials_are_refused() {
        assert_eq!(
            normalize_origins(Some(&json!(["https://example.com"]))).unwrap(),
            vec!["https://example.com"]
        );
        assert!(normalize_origins(Some(&json!(["https://example.com/path"]))).is_err());
        assert!(validate_http_endpoint("http://user@127.0.0.1:9222").is_err());
        assert!(validate_http_endpoint("http://example.com:9222").is_err());
    }
    #[test]
    fn sensitive_fields_are_refused() {
        assert!(refuse_sensitive(
            "#password",
            &json!({"nodeName":"INPUT","attributes":["type","text"]})
        )
        .is_err());
        assert!(refuse_sensitive(
            "#name",
            &json!({"nodeName":"INPUT","attributes":["type","text"]})
        )
        .is_ok());
    }
    #[test]
    fn oversized_output_is_materialized_and_pageable() {
        let root = std::env::temp_dir().join(format!("narada-browser-output-{}", Uuid::new_v4()));
        let preview = materialize_output(
            &root,
            "browser_control_screenshot",
            json!({"data_base64":"a".repeat(70_000)}),
        )
        .unwrap();
        let page = super::super::host_contracts::output_show(
            &Map::from_iter([
                ("output_ref".to_string(), preview["output_ref"].clone()),
                ("limit".to_string(), json!(100)),
            ]),
            &root,
        )
        .unwrap();
        assert_eq!(page["output_truncated"], true);
        assert_eq!(page["limit"], 100);
        let _ = fs::remove_dir_all(root);
    }
}
