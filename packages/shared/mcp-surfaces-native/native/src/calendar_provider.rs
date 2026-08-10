use serde_json::{json, Map, Value};

const MAX_TEXT_BYTES: usize = 512_000;

/// Transport-neutral calendar provider request.  The request describes the
/// Graph operation but does not carry credentials or perform I/O.  A native
/// authority can execute it only after its own explicit transport/policy gate.
#[derive(Clone, Debug, PartialEq)]
pub struct CalendarProviderRequest {
    pub method: &'static str,
    pub mailbox_id: Option<String>,
    pub suffix: String,
    pub query: Map<String, Value>,
    pub body: Option<Value>,
}

pub fn build_request(
    name: &str,
    args: &Map<String, Value>,
) -> Result<CalendarProviderRequest, Value> {
    match name {
        "calendar_list" => {
            let mut query = Map::new();
            query.insert("$top".into(), json!(graph_top(args.get("limit"), 20)));
            Ok(CalendarProviderRequest {
                method: "GET",
                mailbox_id: mailbox(args),
                suffix: "calendars".into(),
                query,
                body: None,
            })
        }
        "calendar_event_query" => {
            let mut query = Map::new();
            query.insert(
                "startDateTime".into(),
                json!(required_string(args, "start_datetime")?),
            );
            query.insert(
                "endDateTime".into(),
                json!(required_string(args, "end_datetime")?),
            );
            query.insert("$top".into(), json!(graph_top(args.get("limit"), 20)));
            query.insert(
                "$orderby".into(),
                json!(optional_string(args, "orderby").unwrap_or_else(|| "start/dateTime".into())),
            );
            for key in ["$select", "$filter"] {
                let source_key = key.trim_start_matches('$');
                if let Some(value) = optional_string(args, source_key) {
                    query.insert(key.into(), json!(value));
                }
            }
            let suffix = optional_string(args, "calendar_id")
                .map(|id| format!("calendars/{}/calendarView", encode_component(&id)))
                .unwrap_or_else(|| "calendarView".into());
            Ok(CalendarProviderRequest {
                method: "GET",
                mailbox_id: mailbox(args),
                suffix,
                query,
                body: None,
            })
        }
        "calendar_event_show" => {
            let event_id = required_string(args, "event_id")?;
            let mut query = Map::new();
            if let Some(value) = optional_string(args, "select") {
                query.insert("$select".into(), json!(value));
            }
            Ok(CalendarProviderRequest {
                method: "GET",
                mailbox_id: mailbox(args),
                suffix: format!("events/{}", encode_component(&event_id)),
                query,
                body: None,
            })
        }
        "calendar_event_create" => Ok(CalendarProviderRequest {
            method: "POST",
            mailbox_id: mailbox(args),
            suffix: optional_string(args, "calendar_id")
                .map(|id| format!("calendars/{}/events", encode_component(&id)))
                .unwrap_or_else(|| "events".into()),
            query: Map::new(),
            body: Some(event_body(args, true)?),
        }),
        "calendar_event_update" => Ok(CalendarProviderRequest {
            method: "PATCH",
            mailbox_id: mailbox(args),
            suffix: format!(
                "events/{}",
                encode_component(&required_string(args, "event_id")?)
            ),
            query: Map::new(),
            body: Some(event_body(args, false)?),
        }),
        "calendar_event_delete" => Ok(CalendarProviderRequest {
            method: "DELETE",
            mailbox_id: mailbox(args),
            suffix: format!(
                "events/{}",
                encode_component(&required_string(args, "event_id")?)
            ),
            query: Map::new(),
            body: None,
        }),
        _ => Err(error("unknown_provider_operation", name)),
    }
}

pub fn wrap_result(name: &str, request_url: String, response: Value) -> Result<Value, Value> {
    let result = match name {
        "calendar_list" => json!({
            "schema": "narada.calendar_mcp.calendars.v1",
            "status": "ok",
            "request_url": request_url,
            "calendars": response,
        }),
        "calendar_event_query" => json!({
            "schema": "narada.calendar_mcp.events.v1",
            "status": "ok",
            "request_url": request_url,
            "events": response,
        }),
        "calendar_event_show" => json!({
            "schema": "narada.calendar_mcp.event.v1",
            "status": "ok",
            "event": response,
        }),
        "calendar_event_create" => json!({
            "schema": "narada.calendar_mcp.event.v1",
            "status": "created",
            "event": response,
        }),
        "calendar_event_update" => json!({
            "schema": "narada.calendar_mcp.event.v1",
            "status": "updated",
            "event": response,
        }),
        "calendar_event_delete" => json!({
            "schema": "narada.calendar_mcp.event_delete.v1",
            "status": "deleted",
            "result": response,
        }),
        _ => return Err(error("unknown_provider_operation", name)),
    };
    Ok(result)
}

fn mailbox(args: &Map<String, Value>) -> Option<String> {
    optional_string(args, "mailbox_id")
}

fn graph_top(value: Option<&Value>, fallback: u64) -> u64 {
    let parsed = value.and_then(Value::as_f64).unwrap_or(fallback as f64);
    if !parsed.is_finite() {
        return fallback;
    }
    (parsed.trunc() as u64).clamp(1, 100)
}

fn required_string(args: &Map<String, Value>, key: &str) -> Result<String, Value> {
    optional_string(args, key).ok_or_else(|| error("required_argument_missing", key))
}

fn optional_string(args: &Map<String, Value>, key: &str) -> Option<String> {
    args.get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

fn event_body(args: &Map<String, Value>, require_times: bool) -> Result<Value, Value> {
    let mut body = Map::new();
    if let Some(value) = args.get("subject").and_then(Value::as_str) {
        body.insert("subject".into(), json!(value));
    }
    if let Some(value) = args.get("body_text").and_then(Value::as_str) {
        body.insert("body".into(), json!({"contentType":"Text","content":value}));
    }
    if let Some(value) = args.get("body_html").and_then(Value::as_str) {
        body.insert("body".into(), json!({"contentType":"HTML","content":value}));
    }
    let start = optional_string(args, "start_datetime");
    let end = optional_string(args, "end_datetime");
    let time_zone = optional_string(args, "time_zone");
    if require_times && (start.is_none() || end.is_none() || time_zone.is_none()) {
        return Err(error(
            "event_time_window_required",
            "event_time_window_required",
        ));
    }
    if (start.is_some() || end.is_some()) && time_zone.is_none() {
        return Err(error(
            "time_zone_required_for_event_time",
            "time_zone_required_for_event_time",
        ));
    }
    if let Some(value) = start {
        body.insert(
            "start".into(),
            json!({"dateTime":value,"timeZone":time_zone}),
        );
    }
    if let Some(value) = end {
        body.insert("end".into(), json!({"dateTime":value,"timeZone":time_zone}));
    }
    if let Some(value) = args.get("location").and_then(Value::as_str) {
        body.insert("location".into(), json!({"displayName":value}));
    }
    if let Some(values) = args.get("attendees").and_then(Value::as_array) {
        let attendees = values
            .iter()
            .map(|value| {
                value.as_str().map_or_else(
                    || value.clone(),
                    |address| json!({"emailAddress":{"address":address},"type":"required"}),
                )
            })
            .collect::<Vec<_>>();
        body.insert("attendees".into(), Value::Array(attendees));
    }
    if let Some(value) = args.get("is_online_meeting").and_then(Value::as_bool) {
        body.insert("isOnlineMeeting".into(), json!(value));
    }
    if let Some(value) = args.get("online_meeting_provider").and_then(Value::as_str) {
        body.insert("onlineMeetingProvider".into(), json!(value));
    }
    if let Some(value) = args.get("show_as").and_then(Value::as_str) {
        body.insert("showAs".into(), json!(value));
    }
    if let Some(value) = args.get("sensitivity").and_then(Value::as_str) {
        body.insert("sensitivity".into(), json!(value));
    }
    let encoded = serde_json::to_vec(&body)
        .map_err(|encode_error| error("event_body_encode_failed", &encode_error.to_string()))?;
    if encoded.len() > MAX_TEXT_BYTES {
        return Err(error(
            "event_body_too_large",
            "event body exceeds bounded size",
        ));
    }
    Ok(Value::Object(body))
}

fn encode_component(value: &str) -> String {
    value
        .bytes()
        .map(|byte| match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (byte as char).to_string()
            }
            _ => format!("%{byte:02X}"),
        })
        .collect()
}

fn error(code: &str, message: &str) -> Value {
    json!({"schema":"narada.calendar_mcp.error.v1","code":code,"message":message})
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_read_request_without_transport() {
        let mut args = Map::new();
        args.insert("mailbox_id".into(), json!("calendar@example.test"));
        args.insert("limit".into(), json!(3));
        let request = build_request("calendar_list", &args).expect("request");
        assert_eq!(request.method, "GET");
        assert_eq!(request.mailbox_id.as_deref(), Some("calendar@example.test"));
        assert_eq!(request.suffix, "calendars");
        assert_eq!(request.query.get("$top"), Some(&json!(3)));
        assert!(request.body.is_none());
    }

    #[test]
    fn builds_event_query_with_encoded_ids_and_bounds() {
        let mut args = Map::new();
        args.insert("calendar_id".into(), json!("calendar/one"));
        args.insert("start_datetime".into(), json!("2026-01-01T00:00:00Z"));
        args.insert("end_datetime".into(), json!("2026-01-02T00:00:00Z"));
        args.insert("limit".into(), json!(999));
        let request = build_request("calendar_event_query", &args).expect("request");
        assert_eq!(request.suffix, "calendars/calendar%2Fone/calendarView");
        assert_eq!(request.query.get("$top"), Some(&json!(100)));
    }

    #[test]
    fn builds_guarded_event_body_shape() {
        let mut args = Map::new();
        args.insert("subject".into(), json!("Planning"));
        args.insert("start_datetime".into(), json!("2026-01-01T10:00:00"));
        args.insert("end_datetime".into(), json!("2026-01-01T11:00:00"));
        args.insert("time_zone".into(), json!("UTC"));
        args.insert("attendees".into(), json!(["person@example.test"]));
        let request = build_request("calendar_event_create", &args).expect("request");
        assert_eq!(
            request.body.as_ref().and_then(|body| body.get("subject")),
            Some(&json!("Planning"))
        );
        assert_eq!(
            request.body.as_ref().and_then(|body| body.get("start")),
            Some(&json!({"dateTime":"2026-01-01T10:00:00","timeZone":"UTC"}))
        );
        assert_eq!(
            request.body.as_ref().and_then(|body| body.get("attendees")),
            Some(&json!([{"emailAddress":{"address":"person@example.test"},"type":"required"}]))
        );
    }
}
