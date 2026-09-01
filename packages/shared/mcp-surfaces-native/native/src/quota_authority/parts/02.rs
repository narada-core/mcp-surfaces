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
    if let Ok(value) = env::var("KIMI_CODE_API_KEY").or_else(|_| env::var("KIMI_API_KEY")) {
        if !value.trim().is_empty() {
            return Some((value, "api_key"));
        }
    }
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

