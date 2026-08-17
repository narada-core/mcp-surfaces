use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex, OnceLock};
use std::thread;
use std::time::Duration;

const MAX_FRAME_BYTES: usize = 16 * 1024 * 1024;
const IO_TIMEOUT: Duration = Duration::from_secs(1_800);

#[derive(Clone, Debug)]
pub struct BrokerBinding {
    pub endpoint: String,
    pub capability: String,
    pub broker_generation: String,
}

struct BrokerState {
    binding: BrokerBinding,
}

static BROKER: OnceLock<Mutex<Option<BrokerState>>> = OnceLock::new();

pub fn binding(site_root: &Path) -> Result<BrokerBinding, String> {
    let broker = BROKER.get_or_init(|| Mutex::new(None));
    let mut state = broker
        .lock()
        .map_err(|_| "codex_app_server_broker_state_poisoned".to_string())?;
    if let Some(state) = state.as_ref() {
        return Ok(state.binding.clone());
    }
    let started = start_broker(site_root)?;
    let binding = started.binding.clone();
    *state = Some(started);
    Ok(binding)
}

pub fn current_generation() -> Option<String> {
    BROKER
        .get()
        .and_then(|broker| broker.lock().ok())
        .and_then(|state| state.as_ref().map(|state| state.binding.broker_generation.clone()))
}

fn start_broker(site_root: &Path) -> Result<BrokerState, String> {
    let listener = TcpListener::bind(("127.0.0.1", 0))
        .map_err(|error| format!("codex_app_server_broker_bind_failed:{error}"))?;
    let endpoint = listener
        .local_addr()
        .map_err(|error| format!("codex_app_server_broker_address_failed:{error}"))?
        .to_string();
    let capability = uuid::Uuid::new_v4().to_string();
    let broker_generation = uuid::Uuid::new_v4().to_string();
    let app_server = Arc::new(Mutex::new(AppServer::start(site_root)?));
    let server = Arc::clone(&app_server);
    let expected_capability = capability.clone();
    thread::Builder::new()
        .name("codex-app-server-broker".to_string())
        .spawn(move || {
            for connection in listener.incoming() {
                let Ok(stream) = connection else { continue };
                let app_server = Arc::clone(&server);
                let capability = expected_capability.clone();
                let _ = thread::Builder::new()
                    .name("codex-app-server-request".to_string())
                    .spawn(move || handle_connection(stream, &capability, app_server));
            }
        })
        .map_err(|error| format!("codex_app_server_broker_thread_failed:{error}"))?;
    Ok(BrokerState {
        binding: BrokerBinding {
            endpoint,
            capability,
            broker_generation,
        },
    })
}

fn handle_connection(
    mut stream: TcpStream,
    expected_capability: &str,
    app_server: Arc<Mutex<AppServer>>,
) {
    let _ = stream.set_read_timeout(Some(IO_TIMEOUT));
    let _ = stream.set_write_timeout(Some(IO_TIMEOUT));
    let response = read_frame(&mut stream)
        .and_then(|request| {
            if request.get("schema").and_then(Value::as_str)
                != Some("narada.codex_app_server.broker_request.v1")
            {
                return Err("codex_app_server_broker_schema_invalid".to_string());
            }
            if request.get("capability").and_then(Value::as_str) != Some(expected_capability) {
                return Err("codex_app_server_broker_capability_refused".to_string());
            }
            let cancelled = Arc::new(AtomicBool::new(false));
            let cancellation = Arc::clone(&cancelled);
            let mut disconnect = stream
                .try_clone()
                .map_err(|error| format!("codex_app_server_broker_clone_failed:{error}"))?;
            let _ = thread::Builder::new()
                .name("codex-app-server-disconnect".to_string())
                .spawn(move || {
                    let mut byte = [0_u8; 1];
                    if disconnect.read(&mut byte).unwrap_or(0) == 0 {
                        cancellation.store(true, Ordering::Release);
                    }
                });
            let mut server = app_server
                .lock()
                .map_err(|_| "codex_app_server_broker_lock_poisoned".to_string())?;
            server.perform_turn(&request, &cancelled)
        })
        .unwrap_or_else(|error| {
            json!({
                "schema":"narada.codex_app_server.broker_response.v1",
                "status":"failed",
                "error":error,
            })
        });
    let _ = serde_json::to_writer(&mut stream, &response);
    let _ = stream.write_all(b"\n");
    let _ = stream.flush();
}

fn read_frame(stream: &mut TcpStream) -> Result<Value, String> {
    let mut bytes = Vec::new();
    BufReader::new(stream)
        .take((MAX_FRAME_BYTES + 1) as u64)
        .read_until(b'\n', &mut bytes)
        .map_err(|error| format!("codex_app_server_broker_request_read_failed:{error}"))?;
    if bytes.len() > MAX_FRAME_BYTES {
        return Err("codex_app_server_broker_request_too_large".to_string());
    }
    serde_json::from_slice(&bytes)
        .map_err(|error| format!("codex_app_server_broker_request_invalid:{error}"))
}

struct AppServer {
    site_root: PathBuf,
    child: Child,
    input: ChildStdin,
    output: mpsc::Receiver<String>,
    next_id: u64,
    generation: String,
}

impl AppServer {
    fn start(site_root: &Path) -> Result<Self, String> {
        let site_root = site_root
            .canonicalize()
            .map_err(|error| format!("codex_app_server_site_root_invalid:{error}"))?;
        if !site_root.is_dir() {
            return Err("codex_app_server_site_root_not_directory".to_string());
        }
        let command =
            std::env::var_os("NARADA_NATIVE_CODEX_COMMAND").unwrap_or_else(|| "codex".into());
        let mut child = Command::new(command)
            .args(app_server_args())
            .current_dir(&site_root)
            .env_remove("CODEX_PERMISSION_PROFILE")
            .env_remove("CODEX_THREAD_ID")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|error| format!("codex_app_server_spawn_failed:{error}"))?;
        let input = child
            .stdin
            .take()
            .ok_or_else(|| "codex_app_server_stdin_missing".to_string())?;
        let output = child
            .stdout
            .take()
            .ok_or_else(|| "codex_app_server_stdout_missing".to_string())?;
        let (output_tx, output_rx) = mpsc::channel();
        thread::Builder::new()
            .name("codex-app-server-output".to_string())
            .spawn(move || {
                for line in BufReader::new(output).lines().map_while(Result::ok) {
                    if output_tx.send(line).is_err() {
                        break;
                    }
                }
            })
            .map_err(|error| format!("codex_app_server_output_thread_failed:{error}"))?;
        let mut server = Self {
            site_root,
            child,
            input,
            output: output_rx,
            next_id: 1,
            generation: uuid::Uuid::new_v4().to_string(),
        };
        let id = server.request(
            "initialize",
            json!({"clientInfo":{"name":"narada-native-provider","version":"1"},"capabilities":{"experimentalApi":true}}),
        )?;
        let _ = server.response(id)?;
        Ok(server)
    }

    fn perform_turn(&mut self, request: &Value, cancelled: &AtomicBool) -> Result<Value, String> {
        if self
            .child
            .try_wait()
            .map_err(|error| error.to_string())?
            .is_some()
        {
            *self = Self::start(&self.site_root)?;
        }
        let prompt = required_string(request, "prompt")?;
        let cwd = required_string(request, "cwd")?;
        let model = required_string(request, "model")?;
        let effort = required_string(request, "reasoning_effort")?;
        let sandbox = required_string(request, "sandbox")?;
        let writable_roots = request
            .get("writable_roots")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let thread_request = self.request(
            "thread/start",
            json!({
                "cwd":cwd,
                "model":model,
                "approvalPolicy":"never",
                "sandbox":sandbox,
                "runtimeWorkspaceRoots":writable_roots,
                "ephemeral":true,
                "config":{"mcp_servers":{},"features":{"apps":false}},
            }),
        )?;
        let thread = self.response(thread_request)?;
        let thread_id = thread
            .pointer("/thread/id")
            .and_then(Value::as_str)
            .ok_or_else(|| "codex_app_server_thread_id_missing".to_string())?
            .to_string();
        let sandbox_policy = if sandbox == "workspace-write" {
            json!({"type":"workspaceWrite","writableRoots":writable_roots,"networkAccess":false})
        } else {
            json!({"type":"readOnly"})
        };
        let turn_request = self.request(
            "turn/start",
            json!({
                "threadId":thread_id,
                "input":[{"type":"text","text":prompt}],
                "model":model,
                "effort":effort,
                "cwd":cwd,
                "approvalPolicy":"never",
                "sandboxPolicy":sandbox_policy,
            }),
        )?;
        let turn = self.response(turn_request)?;
        let turn_id = turn
            .pointer("/turn/id")
            .and_then(Value::as_str)
            .ok_or_else(|| "codex_app_server_turn_id_missing".to_string())?
            .to_string();
        let mut content = None;
        loop {
            if cancelled.load(Ordering::Acquire) {
                let interrupt = self.request(
                    "turn/interrupt",
                    json!({"threadId":thread_id,"turnId":turn_id}),
                )?;
                let _ = self.response(interrupt);
                return Err("codex_app_server_turn_interrupted".to_string());
            }
            let Some(message) = self.read_message_timeout(Duration::from_millis(100))? else {
                continue;
            };
            if message.get("method").and_then(Value::as_str) == Some("item/completed")
                && message.pointer("/params/item/type").and_then(Value::as_str)
                    == Some("agentMessage")
            {
                content = message
                    .pointer("/params/item/text")
                    .and_then(Value::as_str)
                    .map(str::to_string);
            }
            if message.get("method").and_then(Value::as_str) == Some("turn/completed")
                && message.pointer("/params/turn/id").and_then(Value::as_str)
                    == Some(turn_id.as_str())
            {
                let status = message
                    .pointer("/params/turn/status")
                    .and_then(Value::as_str)
                    .unwrap_or("failed");
                if status != "completed" {
                    return Err(format!("codex_app_server_turn_{status}"));
                }
                break;
            }
        }
        Ok(json!({
            "schema":"narada.codex_app_server.broker_response.v1",
            "status":"completed",
            "content":content.ok_or_else(|| "codex_app_server_content_missing".to_string())?,
            "thread_id":thread_id,
            "turn_id":turn_id,
            "host_generation":self.generation,
        }))
    }

    fn request(&mut self, method: &str, params: Value) -> Result<u64, String> {
        let id = self.next_id;
        self.next_id += 1;
        serde_json::to_writer(
            &mut self.input,
            &json!({"id":id,"method":method,"params":params}),
        )
        .map_err(|error| format!("codex_app_server_request_encode_failed:{error}"))?;
        self.input
            .write_all(b"\n")
            .and_then(|()| self.input.flush())
            .map_err(|error| format!("codex_app_server_request_write_failed:{error}"))?;
        Ok(id)
    }

    fn response(&mut self, id: u64) -> Result<Value, String> {
        loop {
            let message = self.read_message()?;
            if message.get("id").and_then(Value::as_u64) == Some(id) {
                if let Some(error) = message.get("error").filter(|value| !value.is_null()) {
                    return Err(format!("codex_app_server_response_error:{error}"));
                }
                return Ok(message.get("result").cloned().unwrap_or(Value::Null));
            }
        }
    }

    fn read_message(&mut self) -> Result<Value, String> {
        self.read_message_timeout(IO_TIMEOUT)?
            .ok_or_else(|| "codex_app_server_read_timed_out".to_string())
    }

    fn read_message_timeout(&mut self, timeout: Duration) -> Result<Option<Value>, String> {
        let line = match self.output.recv_timeout(timeout) {
            Ok(line) => line,
            Err(mpsc::RecvTimeoutError::Timeout) => return Ok(None),
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                return Err("codex_app_server_stdout_closed".to_string())
            }
        };
        if line.len() > MAX_FRAME_BYTES {
            return Err("codex_app_server_response_too_large".to_string());
        }
        serde_json::from_str(&line)
            .map(Some)
            .map_err(|error| format!("codex_app_server_response_invalid:{error}"))
    }
}

fn app_server_args() -> [&'static str; 7] {
    [
        "app-server",
        "--listen",
        "stdio://",
        "-c",
        "mcp_servers={}",
        "-c",
        "features.apps=false",
    ]
}

impl Drop for AppServer {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn required_string<'a>(value: &'a Value, key: &str) -> Result<&'a str, String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| format!("codex_app_server_{key}_required"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn broker_request_requires_explicit_provider_coordinates() {
        let request = json!({"prompt":"ok","cwd":"C:/repo","model":"model"});
        assert_eq!(required_string(&request, "prompt").unwrap(), "ok");
        assert_eq!(
            required_string(&request, "reasoning_effort").unwrap_err(),
            "codex_app_server_reasoning_effort_required"
        );
    }

    #[test]
    fn app_server_inherits_the_configured_windows_sandbox() {
        let args = app_server_args();
        assert!(
            args.iter().all(|arg| !arg.starts_with("windows.sandbox=")),
            "the broker must not replace the provisioned Windows sandbox mode"
        );
    }
}
