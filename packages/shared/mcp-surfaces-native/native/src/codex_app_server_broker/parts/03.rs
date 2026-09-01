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
