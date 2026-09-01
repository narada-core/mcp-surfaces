use serde_json::{json, Map, Value};
use std::env;
use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;

const MAX_RESPONSE_BYTES: u64 = 512_000;
const DEFAULT_TIMEOUT_MS: u64 = 15_000;
const STATUS_STALE_MS: i128 = 2_000;

pub fn is_query_mode(args: &[String]) -> bool {
    args.first().map(String::as_str) == Some("--quota-meter-query")
}

pub fn run_query_mode(args: &[String]) -> Result<(), String> {
    if args.len() % 2 == 0
        || args
            .iter()
            .skip(1)
            .step_by(2)
            .any(|name| !matches!(name.as_str(), "--providers" | "--timeout-ms"))
    {
        return Err("quota_meter_query_arguments_invalid".into());
    }
    let providers = option(args, "--providers").unwrap_or_else(|| "all".into());
    let timeout = option(args, "--timeout-ms")
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(DEFAULT_TIMEOUT_MS)
        .clamp(100, 60_000);
    let result = glide(&providers, timeout).map_err(render_error)?;
    println!(
        "{}",
        serde_json::to_string(&result).map_err(|error| error.to_string())?
    );
    Ok(())
}

fn option(args: &[String], name: &str) -> Option<String> {
    args.windows(2)
        .find(|pair| pair[0] == name)
        .map(|pair| pair[1].clone())
}

pub fn guidance(root: &Path) -> Value {
    let executable = env::current_exe().ok();
    let script = overlay_script(root);
    json!({
        "schema":"narada.quota_meter.guidance.v1",
        "status":"ok",
        "surface_id":"quota-meter",
        "native_authority":true,
        "provider_credentials":"provider_owned_and_never_projected",
        "provider_reads":{"codex":"codex app-server stdio","kimi":"Kimi usage HTTPS"},
        "overlay":{"host":"PowerShell WPF","refresh_executable":executable,"script_path":script,"script_available":script.is_file()},
        "workflow":["Read glide status without interactive login.","Start or stop the overlay explicitly.","Inspect overlay status after lifecycle changes."],
        "recovery":["Run codex login or kimi login when a provider reports auth_required.","Set QUOTA_METER_ROOT only when the quota-meter checkout is not under the source root."],
    })
}

pub fn glide_status(args: &Map<String, Value>) -> Result<Value, Value> {
    let providers = provider_selection(args)?;
    let timeout = args
        .get("timeout_ms")
        .and_then(Value::as_u64)
        .unwrap_or(DEFAULT_TIMEOUT_MS);
    glide(&providers, timeout)
}

fn glide(selection: &str, timeout_ms: u64) -> Result<Value, Value> {
    let mut providers = Vec::new();
    for provider in selected_providers(selection)? {
        let result = match provider {
            "codex" => fetch_codex(timeout_ms),
            "kimi" => fetch_kimi(timeout_ms),
            _ => unreachable!(),
        };
        providers.push(attach_glide(result));
    }
    let status = if providers
        .iter()
        .all(|item| item.get("status").and_then(Value::as_str) == Some("ok"))
    {
        "ok"
    } else {
        "partial"
    };
    Ok(
        json!({"schema":"narada.quota_meter.glide_status.v1","status":status,"provider_selection":selection,"generated_at":now_iso(),"providers":providers}),
    )
}

fn provider_selection(args: &Map<String, Value>) -> Result<String, Value> {
    let value = args
        .get("providers")
        .and_then(Value::as_str)
        .unwrap_or("all")
        .trim()
        .to_ascii_lowercase();
    selected_providers(&value)?;
    Ok(value)
}

fn selected_providers(value: &str) -> Result<Vec<&'static str>, Value> {
    match value {
        "all" | "codex,kimi" | "kimi,codex" => Ok(if value == "kimi,codex" {
            vec!["kimi", "codex"]
        } else {
            vec!["codex", "kimi"]
        }),
        "codex" => Ok(vec!["codex"]),
        "kimi" => Ok(vec!["kimi"]),
        _ => Err(error(
            "quota_meter_invalid_provider_selection",
            json!({"allowed":["all","codex","kimi","codex,kimi","kimi,codex"]}),
        )),
    }
}

struct Rpc {
    child: Child,
    input: ChildStdin,
    responses: Receiver<Value>,
    next_id: u64,
    timeout: Duration,
}

impl Rpc {
    fn start(timeout_ms: u64) -> Result<Self, String> {
        let command = env::var("QUOTA_METER_CODEX_COMMAND")
            .ok()
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| "codex".into());
        let mut child = Command::new(command)
            .args(["app-server", "--listen", "stdio://"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .creation_flags_hidden()
            .spawn()
            .map_err(|error| format!("codex_app_server_start_failed:{error}"))?;
        let input = child
            .stdin
            .take()
            .ok_or("codex_app_server_stdin_unavailable")?;
        let output = child
            .stdout
            .take()
            .ok_or("codex_app_server_stdout_unavailable")?;
        let (send, responses) = mpsc::sync_channel(32);
        thread::spawn(move || {
            for line in BufReader::new(output).lines().map_while(Result::ok) {
                if line.len() > MAX_RESPONSE_BYTES as usize {
                    continue;
                }
                if let Ok(value) = serde_json::from_str::<Value>(&line) {
                    let _ = send.send(value);
                }
            }
        });
        Ok(Self {
            child,
            input,
            responses,
            next_id: 1,
            timeout: Duration::from_millis(timeout_ms),
        })
    }

    fn request(&mut self, method: &str, params: Option<Value>) -> Result<Value, String> {
        let id = self.next_id;
        self.next_id += 1;
        let mut value = json!({"method":method,"id":id});
        if let Some(params) = params {
            value["params"] = params;
        }
        writeln!(self.input, "{}", value)
            .map_err(|error| format!("codex_app_server_write_failed:{error}"))?;
        self.input
            .flush()
            .map_err(|error| format!("codex_app_server_flush_failed:{error}"))?;
        let deadline = Instant::now() + self.timeout;
        loop {
            let remaining = deadline
                .checked_duration_since(Instant::now())
                .ok_or_else(|| format!("codex_app_server_timeout:{method}"))?;
            let response = self
                .responses
                .recv_timeout(remaining)
                .map_err(|_| format!("codex_app_server_timeout:{method}"))?;
            if response.get("id").and_then(Value::as_u64) != Some(id) {
                continue;
            }
            if let Some(error) = response.get("error") {
                return Err(format!(
                    "codex_app_server_error:{method}:{}",
                    bounded_json(error)
                ));
            }
            return Ok(response.get("result").cloned().unwrap_or(response));
        }
    }

    fn notify(&mut self, method: &str) -> Result<(), String> {
        writeln!(self.input, "{}", json!({"method":method})).map_err(|error| error.to_string())
    }
}

impl Drop for Rpc {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn fetch_codex(timeout_ms: u64) -> Value {
    let fetched = now_iso();
    let outcome = (|| -> Result<Value, String> {
        let mut rpc = Rpc::start(timeout_ms)?;
        rpc.request("initialize", Some(json!({"clientInfo":{"name":"narada-quota-meter","title":"Narada quota meter","version":"0.1.0"}})))?;
        rpc.notify("initialized")?;
        let account = rpc.request("account/read", Some(json!({"refreshToken":false})))?;
        let account = account.get("account").ok_or("codex_auth_required")?;
        if account.get("type").and_then(Value::as_str) == Some("apiKey") {
            return Err("codex_subscription_login_required".into());
        }
        let rates = rpc.request("account/rateLimits/read", None)?;
        let usage = rpc.request("account/usage/read", None).ok();
        let source = rates.get("rateLimits").unwrap_or(&rates);
        let mut windows = Vec::new();
        if let Some(entries) = source.as_object() {
            for (key, value) in entries.iter().take(32) {
                let Some(object) = value.as_object() else {
                    continue;
                };
                let used = number(
                    object
                        .get("usedPercent")
                        .or_else(|| object.get("used_percent")),
                );
                let remaining = number(
                    object
                        .get("remainingPercent")
                        .or_else(|| object.get("remaining_percent")),
                )
                .or_else(|| used.map(|value| 100.0 - value));
                let used = used.or_else(|| remaining.map(|value| 100.0 - value));
                let reset = timestamp(object.get("resetsAt").or_else(|| object.get("resets_at")));
                let duration = number(
                    object
                        .get("windowDurationMins")
                        .or_else(|| object.get("window_duration_mins")),
                )
                .map(|value| value * 60.0);
                if used.is_none() && reset.is_none() {
                    continue;
                }
                windows.push(json!({"id":format!("codex:{key}"),"label":duration_label(duration,key),"usedPercent":used,"remainingPercent":remaining,"resetAt":reset,"durationSeconds":duration,"unit":"percent","source":"account/rateLimits/read","fetchedAt":fetched}));
            }
        }
        Ok(
            json!({"provider":"codex","displayName":"Codex","status":if windows.is_empty(){"unavailable"}else{"ok"},"auth":{"mode":account.get("type"),"plan":account.get("planType")},"plan":account.get("planType"),"windows":windows,"usage":usage,"metadata":{"rateLimitResetCredits":rates.get("rateLimitResetCredits")},"fetchedAt":fetched,"source":"codex app-server"}),
        )
    })();
    outcome.unwrap_or_else(|message| {
        provider_error("codex", "Codex", &message, &fetched, "codex login")
    })
}

