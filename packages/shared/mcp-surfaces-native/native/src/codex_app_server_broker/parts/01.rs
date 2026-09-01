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

