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
