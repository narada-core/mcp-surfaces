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

fn fetch_kimi(timeout_ms: u64) -> Value {
    let fetched = now_iso();
    let Some((token, mode)) = kimi_credential() else {
        return provider_error(
            "kimi",
            "Kimi Code",
            "kimi_auth_required",
            &fetched,
            "kimi login",
        );
    };
    let url = env::var("KIMI_USAGE_URL")
        .unwrap_or_else(|_| "https://api.kimi.com/coding/v1/usages".into());
    let response = ureq::get(&url)
        .set("Accept", "application/json")
        .set("Authorization", &format!("Bearer {token}"))
        .set("User-Agent", "narada-quota-meter/0.1.0")
        .timeout(Duration::from_millis(timeout_ms))
        .call();
    let response = match response {
        Ok(value) => value,
        Err(ureq::Error::Status(401, _)) => {
            return provider_error(
                "kimi",
                "Kimi Code",
                "kimi_auth_rejected",
                &fetched,
                "kimi login",
            )
        }
        Err(error_value) => {
            return provider_error(
                "kimi",
                "Kimi Code",
                &format!("kimi_usage_unavailable:{error_value}"),
                &fetched,
                "kimi login",
            )
        }
    };
    let mut bytes = Vec::new();
    if response
        .into_reader()
        .take(MAX_RESPONSE_BYTES + 1)
        .read_to_end(&mut bytes)
        .is_err()
        || bytes.len() as u64 > MAX_RESPONSE_BYTES
    {
        return provider_error(
            "kimi",
            "Kimi Code",
            "kimi_usage_response_too_large",
            &fetched,
            "kimi login",
        );
    }
    let body: Value = match serde_json::from_slice(&bytes) {
        Ok(value) => value,
        Err(_) => {
            return provider_error(
                "kimi",
                "Kimi Code",
                "kimi_usage_invalid_json",
                &fetched,
                "kimi login",
            )
        }
    };
    let mut windows = Vec::new();
    if let Some(usage) = body.get("usage") {
        windows.push(kimi_window(
            usage,
            None,
            "kimi:weekly",
            "7d",
            Some(604800.0),
            &fetched,
        ));
    }
    if let Some(limits) = body.get("limits").and_then(Value::as_array) {
        for (index, item) in limits.iter().take(32).enumerate() {
            let detail = item.get("detail").unwrap_or(item);
            windows.push(kimi_window(
                detail,
                item.get("window"),
                &format!("kimi:window:{index}"),
                &format!("window-{}", index + 1),
                None,
                &fetched,
            ));
        }
    }
    json!({"provider":"kimi","displayName":"Kimi Code","status":if windows.is_empty(){"unavailable"}else{"ok"},"auth":{"mode":mode},"plan":body.get("subType"),"windows":windows,"usage":Value::Null,"metadata":{"parallel":body.get("parallel"),"totalQuota":body.get("totalQuota"),"boosterWallet":body.get("boosterWallet")},"fetchedAt":fetched,"source":"GET /coding/v1/usages"})
}

fn kimi_credential() -> Option<(String, &'static str)> {
    let home = env::var("USERPROFILE").or_else(|_| env::var("HOME")).ok()?;
    let kimi_home = env::var("KIMI_CODE_HOME")
        .ok()
        .filter(|v| !v.trim().is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(&home).join(".kimi-code"));
    let mut paths = Vec::new();
    if let Ok(path) = env::var("KIMI_CODE_CREDENTIALS") {
        paths.push(PathBuf::from(path));
    }
    paths.push(kimi_home.join("credentials/kimi-code.json"));
    paths.push(PathBuf::from(home).join(".kimi/credentials/kimi-code.json"));
    for path in paths.into_iter().take(3) {
        let Ok(meta) = fs::metadata(&path) else {
            continue;
        };
        if meta.len() > 64_000 {
            continue;
        }
        let Ok(text) = fs::read_to_string(path) else {
            continue;
        };
        let Ok(value) = serde_json::from_str::<Value>(&text) else {
            continue;
        };
        let Some(token) = value
            .get("access_token")
            .and_then(Value::as_str)
            .filter(|v| !v.is_empty())
        else {
            continue;
        };
        let expires = value
            .get("expires_at")
            .and_then(expiry_epoch_seconds)
            .map(|v| if v > 100_000_000_000.0 { v / 1000.0 } else { v });
        if expires.is_some_and(|value| value <= epoch_now() / 1000.0 + 30.0) {
            continue;
        }
        return Some((token.into(), "native_oauth"));
    }
    if let Ok(value) = env::var("KIMI_CODE_API_KEY").or_else(|_| env::var("KIMI_API_KEY")) {
        if !value.trim().is_empty() {
            return Some((value, "api_key"));
        }
    }
    None
}

fn expiry_epoch_seconds(value: &Value) -> Option<f64> {
    value
        .as_f64()
        .or_else(|| value.as_str().and_then(|text| text.parse().ok()))
        .or_else(|| {
            value
                .as_str()
                .and_then(|text| OffsetDateTime::parse(text, &Rfc3339).ok())
                .map(|time| time.unix_timestamp() as f64)
        })
}

fn kimi_window(
    detail: &Value,
    window: Option<&Value>,
    id: &str,
    fallback: &str,
    fallback_duration: Option<f64>,
    fetched: &str,
) -> Value {
    let used = number(
        detail
            .get("usedPercent")
            .or_else(|| detail.get("used_percent")),
    )
    .or_else(|| {
        let used = number(detail.get("used"))?;
        let limit = number(detail.get("limit"))?;
        (limit > 0.0).then_some(used / limit * 100.0)
    });
    let remaining = number(
        detail
            .get("remainingPercent")
            .or_else(|| detail.get("remaining_percent")),
    )
    .or_else(|| used.map(|v| 100.0 - v));
    let used = used.or_else(|| remaining.map(|v| 100.0 - v));
    let duration = window
        .and_then(|value| number(value.get("duration")))
        .map(|value| {
            let unit = window
                .and_then(|v| v.get("timeUnit").or_else(|| v.get("time_unit")))
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_ascii_uppercase();
            if unit.contains("HOUR") {
                value * 3600.0
            } else if unit.contains("DAY") {
                value * 86400.0
            } else if unit.contains("SECOND") {
                value
            } else {
                value * 60.0
            }
        })
        .or(fallback_duration);
    json!({"id":id,"label":duration_label(duration,fallback),"usedPercent":used,"remainingPercent":remaining,"resetAt":timestamp(detail.get("resetTime").or_else(||detail.get("reset_time"))),"durationSeconds":duration,"unit":"quota","amount":{"limit":detail.get("limit"),"used":detail.get("used"),"remaining":detail.get("remaining")},"source":"GET /coding/v1/usages","fetchedAt":fetched})
}

fn attach_glide(mut provider: Value) -> Value {
    if let Some(windows) = provider.get_mut("windows").and_then(Value::as_array_mut) {
        for window in windows.iter_mut() {
            let used = number(window.get("usedPercent"));
            let remaining = number(window.get("remainingPercent"));
            let reset_ms = epoch_ms(window.get("resetAt"));
            let duration = number(window.get("durationSeconds"));
            let now = epoch_now();
            let start = match (reset_ms, duration) {
                (Some(reset), Some(seconds)) => Some(reset - seconds * 1000.0),
                _ => None,
            };
            let elapsed = match (start, duration) {
                (Some(start), Some(seconds)) if seconds > 0.0 => {
                    Some(((now - start) / (seconds * 1000.0) * 100.0).clamp(0.0, 100.0))
                }
                _ => None,
            };
            let factor = match (used, elapsed) {
                (Some(used), Some(elapsed)) if elapsed > 0.0 => Some(used / elapsed),
                _ => None,
            };
            let status = if used.map(|v| v >= 100.0).unwrap_or(false)
                || remaining.map(|v| v <= 0.0).unwrap_or(false)
            {
                "exhausted"
            } else if used.is_none() {
                "usage-unknown"
            } else if let Some(factor) = factor {
                if factor < 0.98 {
                    "under"
                } else if factor > 1.03 {
                    "over"
                } else {
                    "in-range"
                }
            } else {
                "window-duration-unknown"
            };
            window["glidePath"] = json!({"status":status,"formula":"usedPercent / elapsedTimePercent","glidePathFactor":factor,"usedPercent":used,"elapsedTimePercent":elapsed,"hoursUntilReset":reset_ms.map(|v|((v-now)/3600000.0).max(0.0)),"exhaustsBeforeReset":factor.map(|v|v>1.0),"resetAt":window.get("resetAt")});
        }
    }
    provider
}

pub fn overlay_status(root: &Path) -> Value {
    let base = state_root(root);
    let pid_path = base.join("overlay.pid");
    let position_path = base.join("overlay-position.json");
    let status_path = base.join("overlay-status.json");
    let pid = fs::read_to_string(&pid_path)
        .ok()
        .and_then(|v| v.trim().parse::<u32>().ok())
        .filter(|v| *v > 0);
    let process_live = pid.map(process_alive).unwrap_or(false);
    let position = bounded_file_json(&position_path);
    let telemetry = bounded_file_json(&status_path);
    let updated = telemetry
        .as_ref()
        .filter(|v| v.get("schemaVersion").and_then(Value::as_u64) == Some(1))
        .and_then(|v| v.get("updatedAt"))
        .and_then(Value::as_str)
        .and_then(|v| OffsetDateTime::parse(v, &Rfc3339).ok());
    let age = updated.map(|v| (OffsetDateTime::now_utc() - v).whole_milliseconds());
    let stale = age.map(|value| value > STATUS_STALE_MS).unwrap_or(true);
    let identity_verified =
        process_live && age.is_some_and(|value| (-5000..=10000).contains(&value));
    let running = identity_verified;
    json!({"schema":"narada.quota_meter.overlay_status.v1","status":if running{"running"}else if pid.is_some(){"stale"}else{"stopped"},"running":running,"pid":pid,"process_live":process_live,"identity_verified":identity_verified,"stale_pid_file":pid.is_some()&&!running,"position":position,"telemetry":telemetry,"telemetry_stale":stale})
}

pub fn overlay_start(args: &Map<String, Value>, root: &Path) -> Result<Value, Value> {
    let providers = provider_selection(args)?;
    let refresh = args
        .get("refresh_seconds")
        .and_then(Value::as_u64)
        .unwrap_or(60);
    let current = overlay_status(root);
    if current.get("running").and_then(Value::as_bool) == Some(true) {
        return Ok(
            json!({"schema":"narada.quota_meter.overlay_lifecycle.v1","status":"already_running","provider_selection":providers,"refresh_seconds":refresh,"overlay":current}),
        );
    }
    let script = overlay_script(root);
    if !script.is_file() {
        return Err(error(
            "quota_meter_overlay_script_not_found",
            json!({"path":script,"remediation":"Set QUOTA_METER_ROOT to the quota-meter checkout containing src/overlay.ps1."}),
        ));
    }
    let exe = env::current_exe().map_err(|e| {
        error(
            "quota_meter_native_executable_unavailable",
            json!({"message":e.to_string()}),
        )
    })?;
    let base = state_root(root);
    fs::create_dir_all(&base).map_err(|e| {
        error(
            "quota_meter_state_root_create_failed",
            json!({"message":e.to_string()}),
        )
    })?;
    let shell = env::var("QUOTA_METER_POWERSHELL").unwrap_or_else(|_| "pwsh".into());
    let mut command = Command::new(shell);
    command
        .args(["-NoProfile", "-ExecutionPolicy", "Bypass", "-File"])
        .arg(&script)
        .args(["-Action", "start", "-NativePath"])
        .arg(&exe)
        .args([
            "-ProviderSelection",
            &providers,
            "-RefreshSeconds",
            &refresh.to_string(),
            "-PidPath",
        ])
        .arg(base.join("overlay.pid"))
        .args(["-PositionPath"])
        .arg(base.join("overlay-position.json"))
        .args(["-RefreshPath"])
        .arg(base.join("overlay-refresh.signal"))
        .args(["-StatusPath"])
        .arg(base.join("overlay-status.json"))
        .args(["-LoginStatePath"])
        .arg(base.join("overlay-login-state.json"))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .creation_flags_hidden();
    command.spawn().map_err(|e| {
        error(
            "quota_meter_overlay_start_failed",
            json!({"message":e.to_string()}),
        )
    })?;
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        let status = overlay_status(root);
        if status.get("running").and_then(Value::as_bool) == Some(true) {
            return Ok(
                json!({"schema":"narada.quota_meter.overlay_lifecycle.v1","status":"started","provider_selection":providers,"refresh_seconds":refresh,"overlay":status}),
            );
        }
        thread::sleep(Duration::from_millis(50));
    }
    Err(error(
        "quota_meter_overlay_start_timeout",
        json!({"timeout_ms":5000}),
    ))
}

pub fn overlay_stop(root: &Path) -> Result<Value, Value> {
    let before = overlay_status(root);
    if before.get("running").and_then(Value::as_bool) != Some(true) {
        return Ok(
            json!({"schema":"narada.quota_meter.overlay_lifecycle.v1","status":"already_stopped","overlay":before}),
        );
    }
    let script = overlay_script(root);
    let base = state_root(root);
    let shell = env::var("QUOTA_METER_POWERSHELL").unwrap_or_else(|_| "pwsh".into());
    let output = Command::new(shell)
        .args(["-NoProfile", "-ExecutionPolicy", "Bypass", "-File"])
        .arg(script)
        .args(["-Action", "stop", "-NativePath"])
        .arg(env::current_exe().unwrap_or_default())
        .args(["-PidPath"])
        .arg(base.join("overlay.pid"))
        .args(["-PositionPath"])
        .arg(base.join("overlay-position.json"))
        .args(["-RefreshPath"])
        .arg(base.join("overlay-refresh.signal"))
        .args(["-StatusPath"])
        .arg(base.join("overlay-status.json"))
        .args(["-LoginStatePath"])
        .arg(base.join("overlay-login-state.json"))
        .creation_flags_hidden()
        .output()
        .map_err(|e| {
            error(
                "quota_meter_overlay_stop_failed",
                json!({"message":e.to_string()}),
            )
        })?;
    if !output.status.success() {
        return Err(error(
            "quota_meter_overlay_stop_failed",
            json!({"exit_code":output.status.code(),"stderr":String::from_utf8_lossy(&output.stderr).chars().take(2000).collect::<String>()}),
        ));
    }
    Ok(
        json!({"schema":"narada.quota_meter.overlay_lifecycle.v1","status":"stopped","overlay":overlay_status(root)}),
    )
}

fn overlay_script(root: &Path) -> PathBuf {
    if let Ok(path) = env::var("QUOTA_METER_OVERLAY_SCRIPT") {
        if !path.trim().is_empty() {
            return PathBuf::from(path);
        }
    }
    let source = env::var("NARADA_SRC_ROOT")
        .ok()
        .map(PathBuf::from)
        .or_else(|| {
            env::var("USERPROFILE")
                .ok()
                .map(|v| PathBuf::from(v).join("src"))
        })
        .unwrap_or_else(|| root.to_path_buf());
    let quota_root = env::var("QUOTA_METER_ROOT")
        .ok()
        .filter(|v| !v.trim().is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| source.join("quota-meter"));
    quota_root.join("src/overlay.ps1")
}
fn state_root(root: &Path) -> PathBuf {
    if let Ok(path) = env::var("QUOTA_METER_STATE_ROOT") {
        if !path.trim().is_empty() {
            return PathBuf::from(path);
        }
    }
    env::var("LOCALAPPDATA")
        .or_else(|_| env::var("TEMP"))
        .or_else(|_| env::var("TMP"))
        .map(PathBuf::from)
        .unwrap_or_else(|_| root.to_path_buf())
        .join("quota-meter")
}
fn bounded_file_json(path: &Path) -> Option<Value> {
    let meta = fs::metadata(path).ok()?;
    if meta.len() > 64_000 {
        return None;
    }
    serde_json::from_str(&fs::read_to_string(path).ok()?).ok()
}
fn process_alive(pid: u32) -> bool {
    #[cfg(windows)]
    {
        let output = Command::new("tasklist")
            .args(["/FI", &format!("PID eq {pid}"), "/FO", "CSV", "/NH"])
            .creation_flags_hidden()
            .output();
        return output
            .ok()
            .filter(|v| v.status.success())
            .map(|v| String::from_utf8_lossy(&v.stdout).contains(&format!("\"{pid}\"")))
            .unwrap_or(false);
    }
    #[cfg(not(windows))]
    {
        Command::new("kill")
            .args(["-0", &pid.to_string()])
            .status()
            .map(|v| v.success())
            .unwrap_or(false)
    }
}

fn provider_error(id: &str, name: &str, message: &str, fetched: &str, login: &str) -> Value {
    let auth =
        message.contains("auth") || message.contains("login") || message.contains("subscription");
    json!({"provider":id,"displayName":name,"status":if auth{"auth_required"}else{"unavailable"},"auth":{"mode":"unknown"},"windows":[],"usage":Value::Null,"metadata":{},"loginCommand":login,"error":{"code":if auth{"AUTH_REQUIRED"}else{"PROVIDER_UNAVAILABLE"},"message":message},"fetchedAt":fetched})
}
fn number(value: Option<&Value>) -> Option<f64> {
    value
        .and_then(|v| {
            v.as_f64()
                .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
        })
        .filter(|v| v.is_finite())
}
fn timestamp(value: Option<&Value>) -> Option<String> {
    let value = value?;
    if let Some(text) = value.as_str() {
        return OffsetDateTime::parse(text, &Rfc3339)
            .ok()
            .and_then(|v| v.format(&Rfc3339).ok());
    }
    let number = value.as_f64()?;
    let seconds = if number < 100_000_000_000.0 {
        number
    } else {
        number / 1000.0
    };
    OffsetDateTime::from_unix_timestamp(seconds as i64)
        .ok()
        .and_then(|v| v.format(&Rfc3339).ok())
}
fn duration_label(value: Option<f64>, fallback: &str) -> String {
    let Some(seconds) = value else {
        return fallback.into();
    };
    let seconds = seconds as u64;
    if seconds % 86400 == 0 {
        format!("{}d", seconds / 86400)
    } else if seconds % 3600 == 0 {
        format!("{}h", seconds / 3600)
    } else if seconds % 60 == 0 {
        format!("{}m", seconds / 60)
    } else {
        format!("{seconds}s")
    }
}
fn epoch_ms(value: Option<&Value>) -> Option<f64> {
    let text = value?.as_str()?;
    OffsetDateTime::parse(text, &Rfc3339)
        .ok()
        .map(|v| v.unix_timestamp_nanos() as f64 / 1_000_000.0)
}
fn epoch_now() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64()
        * 1000.0
}
fn now_iso() -> String {
    OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .unwrap_or_default()
}
fn bounded_json(value: &Value) -> String {
    serde_json::to_string(value)
        .unwrap_or_default()
        .chars()
        .take(2000)
        .collect()
}
fn error(code: &str, details: Value) -> Value {
    json!({"schema":"narada.quota_meter.error.v1","code":code,"message":code,"details":details})
}
fn render_error(value: Value) -> String {
    value
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or("quota_meter_error")
        .into()
}

trait HiddenCommand {
    fn creation_flags_hidden(&mut self) -> &mut Self;
}
impl HiddenCommand for Command {
    fn creation_flags_hidden(&mut self) -> &mut Self {
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            self.creation_flags(0x08000000);
        }
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn provider_selection_and_glide_are_bounded() {
        assert_eq!(selected_providers("all").unwrap(), vec!["codex", "kimi"]);
        assert!(selected_providers("other").is_err());
        let value = attach_glide(
            json!({"windows":[{"usedPercent":50.0,"remainingPercent":50.0,"resetAt":"2026-08-15T00:00:00Z","durationSeconds":86400.0}]}),
        );
        assert!(value["windows"][0]["glidePath"]["status"].is_string());
        assert!(expiry_epoch_seconds(&json!("2026-08-15T00:00:00Z")).is_some());
    }
}
