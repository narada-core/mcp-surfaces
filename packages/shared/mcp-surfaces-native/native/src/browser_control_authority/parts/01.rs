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

