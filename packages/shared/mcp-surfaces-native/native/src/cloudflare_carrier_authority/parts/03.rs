fn request_json(
    method: &str,
    url: &str,
    cookie_value: Option<&str>,
    body: Option<&Value>,
) -> Result<(u16, Value), Value> {
    let parsed = validate_request_url(url)?;
    let timeout = Duration::from_millis(
        env::var("NARADA_CLOUDFLARE_REQUEST_TIMEOUT_MS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(5_000)
            .clamp(100, 30_000),
    );
    let mut request = if method == "POST" {
        ureq::post(parsed.as_str())
    } else {
        ureq::get(parsed.as_str())
    }
    .timeout(timeout)
    .set("content-type", "application/json");
    if let Some(cookie) = cookie_value {
        request = request.set("cookie", &format!("narada_operator_session={cookie}"));
    }
    let response = match if let Some(value) = body {
        request.send_string(&value.to_string())
    } else {
        request.call()
    } {
        Ok(v) => v,
        Err(ureq::Error::Status(_, v)) => v,
        Err(cause) => {
            return Err(error(
                "cloudflare_transport_failed",
                &cause.to_string(),
                json!({"url":redacted(&parsed),"timeout_ms":timeout.as_millis()}),
            ))
        }
    };
    read_response(response)
}
fn get_json(url: &str, header: Option<(&str, &str)>) -> Result<(u16, Value), Value> {
    let parsed = validate_request_url(url)?;
    let timeout = Duration::from_millis(
        env::var("NARADA_CLOUDFLARE_REQUEST_TIMEOUT_MS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(5_000)
            .clamp(100, 30_000),
    );
    let mut request = ureq::get(parsed.as_str()).timeout(timeout);
    if let Some((k, v)) = header {
        request = request.set(k, v);
    }
    let response = match request.call() {
        Ok(v) => v,
        Err(ureq::Error::Status(_, v)) => v,
        Err(cause) => return Ok((0, json!({"transport_error":cause.to_string()}))),
    };
    read_response(response)
}
fn read_response(response: ureq::Response) -> Result<(u16, Value), Value> {
    let status = response.status();
    if response
        .header("content-length")
        .and_then(|v| v.parse::<u64>().ok())
        .is_some_and(|v| v > MAX_HTTP_BYTES)
    {
        return Err(error(
            "cloudflare_response_too_large",
            "Provider response exceeds 2 MiB.",
            json!({"status":status}),
        ));
    }
    let mut reader = response.into_reader();
    let mut limited = (&mut reader).take(MAX_HTTP_BYTES + 1);
    let mut bytes = Vec::new();
    limited.read_to_end(&mut bytes).map_err(|cause| {
        error(
            "cloudflare_response_read_failed",
            &cause.to_string(),
            json!({}),
        )
    })?;
    if bytes.len() as u64 > MAX_HTTP_BYTES {
        return Err(error(
            "cloudflare_response_too_large",
            "Provider response exceeds 2 MiB.",
            json!({"status":status}),
        ));
    }
    let value = serde_json::from_slice(&bytes).unwrap_or_else(|_| json!({}));
    Ok((status, value))
}
fn validate_request_url(value: &str) -> Result<Url, Value> {
    let url = Url::parse(value).map_err(|_| {
        error(
            "cloudflare_url_invalid",
            "Configured URL is invalid.",
            json!({}),
        )
    })?;
    if !matches!(url.scheme(), "http" | "https")
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return Err(error(
            "cloudflare_url_refused",
            "Configured URL must be credential-free HTTP(S).",
            json!({}),
        ));
    }
    if url.scheme() == "http" && !allow_insecure(&url) {
        return Err(error(
            "cloudflare_insecure_url_refused",
            "Plain HTTP is permitted only for an explicit loopback test fixture.",
            json!({"url":redacted(&url)}),
        ));
    }
    Ok(url)
}
fn validate_base_url(value: &str, worker: bool) -> Result<String, Value> {
    let url = validate_request_url(value.trim_end_matches('/'))?;
    if worker && url.path() != "/" {
        return Err(error(
            "cloudflare_worker_url_invalid",
            "Worker URL must be an origin without a path.",
            json!({"url":redacted(&url)}),
        ));
    }
    Ok(url.to_string().trim_end_matches('/').to_string())
}
fn allow_insecure(url: &Url) -> bool {
    env::var("NARADA_CLOUDFLARE_ALLOW_INSECURE_TEST")
        .ok()
        .as_deref()
        == Some("1")
        && url
            .host_str()
            .is_some_and(|v| matches!(v, "127.0.0.1" | "localhost" | "::1"))
}
fn cookie(state: &State) -> Option<String> {
    let v = bounded_json(&state.session_file).ok()?;
    let raw = v.get("cookie")?.as_str()?;
    let value = raw
        .split(';')
        .find_map(|part| part.trim().strip_prefix("narada_operator_session="))
        .unwrap_or(raw);
    (!value.is_empty()).then(|| value.to_string())
}
fn bounded_json(path: &Path) -> Result<Value, &'static str> {
    let meta = fs::symlink_metadata(path).map_err(|_| "missing")?;
    if !meta.is_file() || meta.file_type().is_symlink() {
        return Err("not_regular_file");
    }
    if meta.len() > MAX_FILE_BYTES {
        return Err("too_large");
    }
    serde_json::from_slice(&fs::read(path).map_err(|_| "read_failed")?).map_err(|_| "invalid_json")
}
fn optional_json(path: &Path) -> Option<Value> {
    bounded_json(path).ok()
}
fn bounded_metadata(path: &Path) -> Option<fs::Metadata> {
    let m = fs::symlink_metadata(path).ok()?;
    if !m.is_file() || m.file_type().is_symlink() || m.len() > MAX_FILE_BYTES {
        None
    } else {
        Some(m)
    }
}
fn summarize(operation: &str, body: &Value, continuation: bool) -> Value {
    match operation {
        "site.list" => {
            json!({"operation":operation,"site_count":body.pointer("/site_product_overview/site_count").cloned().unwrap_or(json!(0)),"next_health":body.pointer("/site_product_overview/next_health"),"next_action":body.pointer("/site_product_overview/next_action"),"next_reason":body.pointer("/site_product_overview/next_reason"),"health_counts":body.pointer("/site_product_overview/health_counts")})
        }
        "site.read" => {
            json!({"operation":operation,"site_id":body.pointer("/site/site_id").or_else(||body.get("site_id")),"health":body.pointer("/site_product_status/health").or_else(||body.pointer("/product_status/health")),"next_action":body.pointer("/site_product_status/next_action").or_else(||body.pointer("/product_status/next_action")),"continuity_state":body.pointer("/site_product_status/continuity_state")})
        }
        "operation.list" => {
            let ops = body
                .get("operations")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            let next = continuation
                .then(|| {
                    ops.iter()
                        .find(|v| {
                            v.get("status").and_then(Value::as_str) == Some("needs_continuation")
                        })
                        .and_then(|v| v.get("operation_id"))
                        .cloned()
                })
                .flatten();
            json!({"operation":operation,"operation_count":ops.len(),"needs_continuation_count":ops.iter().filter(|v|v.get("status").and_then(Value::as_str)==Some("needs_continuation")).count(),"next_continuation_id":next})
        }
        "operation.read" => {
            json!({"operation":operation,"operation_id":body.pointer("/operation/operation_id"),"current_status":body.pointer("/operation/status"),"phase":body.pointer("/operation_lifecycle_status/phase"),"health":body.pointer("/operation_lifecycle_status/health"),"next_action":body.pointer("/operation_lifecycle_status/next_action")})
        }
        _ => json!({"operation":operation}),
    }
}
fn joined(
    status: &str,
    code: Option<&str>,
    id: &str,
    carrier: Value,
    projection: Value,
    next: &str,
) -> Value {
    json!({"schema":"narada.cloudflare_carrier_mcp.carrier_health.v1","status":status,"code":code,"carrier_api":carrier,"projection":projection,"next_action":if next.is_empty(){Value::Null}else{json!(next)},"projection_id":id})
}
fn projection_unavailable(status: u16) -> String {
    if matches!(status, 401 | 403) {
        "projection_browser_access_refused".into()
    } else if status == 0 {
        "projection_unavailable".into()
    } else {
        format!("projection_http_{status}")
    }
}
fn legacy_projection_base(value: &Value) -> Option<String> {
    let endpoint = value
        .get("remote_registration")
        .and_then(|registration| registration.get("endpoint"))
        .and_then(Value::as_str)?;
    let suffix = "/api/nars/projections/register";
    let base = endpoint.strip_suffix(suffix)?.trim_end_matches('/');
    validate_base_url(base, false).ok()
}
fn operator_action(session: &Value) -> Value {
    match session.get("status").and_then(Value::as_str) {
        Some("missing") => json!("refresh_cloudflare_operator_session"),
        Some("present") if session.get("is_fresh").and_then(Value::as_bool) == Some(false) => {
            json!("refresh_cloudflare_operator_session_then_retry")
        }
        Some("incomplete") => json!("capture_cloudflare_operator_session_cookie"),
        _ => Value::Null,
    }
}
fn id(args: &Map<String, Value>, key: &str) -> Option<String> {
    args.get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(str::to_string)
}
fn string(v: Option<&Value>) -> Option<String> {
    v.and_then(Value::as_str)
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(str::to_string)
}
fn obj(v: Option<&Value>) -> Map<String, Value> {
    v.and_then(Value::as_object).cloned().unwrap_or_default()
}
fn env_path(names: &[&str]) -> Option<PathBuf> {
    names.iter().find_map(|name| {
        env::var(name)
            .ok()
            .filter(|v| !v.trim().is_empty())
            .map(PathBuf::from)
    })
}
fn confined(path: &Path, root: &Path) -> bool {
    let root = fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
    let parent = fs::canonicalize(path.parent().unwrap_or(path))
        .unwrap_or_else(|_| path.parent().unwrap_or(path).to_path_buf());
    parent.starts_with(root)
}
fn normalized(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}
