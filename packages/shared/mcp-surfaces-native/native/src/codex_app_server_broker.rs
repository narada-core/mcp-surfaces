use serde_json::{json, Value};
use std::collections::VecDeque;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Condvar, Mutex, OnceLock};
use std::thread;
use std::time::{Duration, Instant};

const MAX_FRAME_BYTES: usize = 16 * 1024 * 1024;
const IO_TIMEOUT: Duration = Duration::from_secs(1_800);
const QUEUE_HEARTBEAT: Duration = Duration::from_secs(5);
const MAX_QUEUED_JOBS: usize = 64;

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
static PROCESS_GENERATION: OnceLock<String> = OnceLock::new();

fn process_generation() -> &'static str {
    PROCESS_GENERATION
        .get_or_init(|| uuid::Uuid::new_v4().to_string())
        .as_str()
}

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
    Some(process_generation().to_string())
}

fn start_broker(site_root: &Path) -> Result<BrokerState, String> {
    let listener = TcpListener::bind(("127.0.0.1", 0))
        .map_err(|error| format!("codex_app_server_broker_bind_failed:{error}"))?;
    let endpoint = listener
        .local_addr()
        .map_err(|error| format!("codex_app_server_broker_address_failed:{error}"))?
        .to_string();
    let capability = uuid::Uuid::new_v4().to_string();
    let broker_generation = process_generation().to_string();
    let scheduler = Arc::new(BrokerScheduler::new(AppServer::start(site_root)?));
    BrokerScheduler::start(Arc::clone(&scheduler))?;
    let server = Arc::clone(&scheduler);
    let expected_capability = capability.clone();
    thread::Builder::new()
        .name("codex-app-server-broker".to_string())
        .spawn(move || {
            for connection in listener.incoming() {
                let Ok(stream) = connection else { continue };
                let scheduler = Arc::clone(&server);
                let capability = expected_capability.clone();
                let _ = thread::Builder::new()
                    .name("codex-app-server-request".to_string())
                    .spawn(move || handle_connection(stream, &capability, scheduler));
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

struct BrokerJob {
    id: String,
    request: Value,
    cancelled: Arc<AtomicBool>,
    events: mpsc::Sender<Value>,
}

struct BrokerQueue {
    jobs: VecDeque<BrokerJob>,
}

struct BrokerScheduler {
    queue: Mutex<BrokerQueue>,
    wake: Condvar,
    app_server: Mutex<Option<AppServer>>,
}

impl BrokerScheduler {
    fn new(app_server: AppServer) -> Self {
        Self {
            queue: Mutex::new(BrokerQueue {
                jobs: VecDeque::new(),
            }),
            wake: Condvar::new(),
            app_server: Mutex::new(Some(app_server)),
        }
    }

    fn start(scheduler: Arc<Self>) -> Result<(), String> {
        thread::Builder::new()
            .name("codex-app-server-lane-1".to_string())
            .spawn(move || scheduler.run())
            .map(|_| ())
            .map_err(|error| format!("codex_app_server_scheduler_thread_failed:{error}"))
    }

    fn enqueue(&self, job: BrokerJob) -> Result<usize, String> {
        let mut queue = self
            .queue
            .lock()
            .map_err(|_| "codex_app_server_queue_poisoned".to_string())?;
        if queue.jobs.len() >= MAX_QUEUED_JOBS {
            return Err("codex_app_server_provider_queue_full".to_string());
        }
        queue.jobs.push_back(job);
        let position = queue.jobs.len();
        self.wake.notify_one();
        Ok(position)
    }

    fn position(&self, id: &str) -> Option<usize> {
        self.queue
            .lock()
            .ok()?
            .jobs
            .iter()
            .position(|job| job.id == id)
            .map(|index| index + 1)
    }

    fn run(&self) {
        loop {
            let job = {
                let mut queue = match self.queue.lock() {
                    Ok(queue) => queue,
                    Err(_) => return,
                };
                while queue.jobs.is_empty() {
                    queue = match self.wake.wait(queue) {
                        Ok(queue) => queue,
                        Err(_) => return,
                    };
                }
                queue.jobs.pop_front().expect("non-empty broker queue")
            };
            if job.cancelled.load(Ordering::Acquire) {
                let _ = job.events.send(broker_event(
                    &job.id,
                    "cancelled",
                    json!({"error":"codex_app_server_queued_job_cancelled"}),
                ));
                continue;
            }
            let admitted_at = Instant::now();
            let _ = job.events.send(broker_event(
                &job.id,
                "admitted",
                json!({"capacity":{"lanes":1,"queue_limit":MAX_QUEUED_JOBS,"scheduling":"fifo"}}),
            ));
            let response = self
                .app_server
                .lock()
                .map_err(|_| "codex_app_server_state_poisoned".to_string())
                .and_then(|mut slot| {
                    slot.as_mut()
                        .ok_or_else(|| "codex_app_server_state_missing".to_string())?
                        .perform_turn(&job.request, &job.cancelled)
                })
                .unwrap_or_else(|error| {
                    let state = if error == "codex_app_server_turn_interrupted" {
                        "cancelled"
                    } else {
                        "failed"
                    };
                    broker_event(
                        &job.id,
                        state,
                        json!({"error":error,"execution_ms":admitted_at.elapsed().as_millis()}),
                    )
                });
            let _ = job.events.send(response);
        }
    }
}

fn handle_connection(
    mut stream: TcpStream,
    expected_capability: &str,
    scheduler: Arc<BrokerScheduler>,
) {
    let _ = stream.set_read_timeout(Some(IO_TIMEOUT));
    let _ = stream.set_write_timeout(Some(IO_TIMEOUT));
    let result = read_frame(&mut stream).and_then(|request| {
            if request.get("schema").and_then(Value::as_str)
                != Some("narada.codex_app_server.broker_request.v2")
            {
                return Err("codex_app_server_broker_schema_invalid".to_string());
            }
            if request.get("capability").and_then(Value::as_str) != Some(expected_capability) {
                return Err("codex_app_server_broker_capability_refused".to_string());
            }
            let request_id = required_string(&request, "request_id")?.to_string();
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
            let (events, responses) = mpsc::channel();
            let position = scheduler.enqueue(BrokerJob {
                id: request_id.clone(),
                request,
                cancelled: Arc::clone(&cancelled),
                events,
            })?;
            write_frame(&mut stream, &broker_event(
                &request_id,
                "queued",
                json!({"queue_position":position,"capacity":{"lanes":1,"queue_limit":MAX_QUEUED_JOBS,"scheduling":"fifo"}}),
            ))?;
            let mut admitted = false;
            loop {
                match responses.recv_timeout(QUEUE_HEARTBEAT) {
                    Ok(event) => {
                        let state = event.get("state").and_then(Value::as_str).unwrap_or("failed");
                        admitted |= state == "admitted";
                        write_frame(&mut stream, &event)?;
                        if matches!(state, "completed" | "failed" | "cancelled") {
                            return Ok(());
                        }
                    }
                    Err(mpsc::RecvTimeoutError::Timeout) if !admitted => {
                        write_frame(&mut stream, &broker_event(
                            &request_id,
                            "heartbeat",
                            json!({"queue_position":scheduler.position(&request_id)}),
                        ))?;
                    }
                    Err(mpsc::RecvTimeoutError::Timeout) => {}
                    Err(mpsc::RecvTimeoutError::Disconnected) => {
                        return Err("codex_app_server_scheduler_disconnected".to_string());
                    }
                }
            }
        });
    if let Err(error) = result {
        let response = broker_event("unknown", "failed", json!({"error":error}));
        let _ = write_frame(&mut stream, &response);
    }
}

fn broker_event(request_id: &str, state: &str, extra: Value) -> Value {
    let mut event = json!({
        "schema":"narada.codex_app_server.broker_event.v2",
        "request_id":request_id,
        "state":state,
    });
    if let (Some(target), Some(values)) = (event.as_object_mut(), extra.as_object()) {
        for (key, value) in values {
            target.insert(key.clone(), value.clone());
        }
    }
    event
}

fn write_frame(stream: &mut TcpStream, value: &Value) -> Result<(), String> {
    serde_json::to_writer(&mut *stream, value)
        .map_err(|error| format!("codex_app_server_broker_response_encode_failed:{error}"))?;
    stream
        .write_all(b"\n")
        .and_then(|()| stream.flush())
        .map_err(|error| format!("codex_app_server_broker_response_write_failed:{error}"))
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
        Ok(broker_event(
            required_string(request, "request_id")?,
            "completed",
            json!({
                "content":content.ok_or_else(|| "codex_app_server_content_missing".to_string())?,
                "thread_id":thread_id,
                "turn_id":turn_id,
                "host_generation":self.generation,
            }),
        ))
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

fn app_server_args() -> [&'static str; 9] {
    [
        "app-server",
        "--listen",
        "stdio://",
        "-c",
        "mcp_servers={}",
        "-c",
        "features.apps=false",
        "-c",
        "windows.sandbox=\"unelevated\"",
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
    fn app_server_uses_the_narrow_root_compatible_windows_sandbox() {
        let args = app_server_args();
        assert!(
            args.contains(&"windows.sandbox=\"unelevated\""),
            "the broker must avoid the elevated setup payload transport limit"
        );
    }

    #[test]
    fn broker_events_use_only_the_admission_aware_v2_contract() {
        let event = broker_event("request-1", "queued", json!({"queue_position":2}));
        assert_eq!(event["schema"], "narada.codex_app_server.broker_event.v2");
        assert_eq!(event["state"], "queued");
        assert_eq!(event["queue_position"], 2);
        assert_eq!(MAX_QUEUED_JOBS, 64);
    }
}
