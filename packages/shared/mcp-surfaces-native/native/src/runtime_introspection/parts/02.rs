fn normalize_events(args: &Map<String, Value>, format: &str) -> Result<Vec<Event>, Value> {
    match format {
        "generic-events" => Ok(args
            .get("events")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .enumerate()
            .map(|(i, v)| {
                normalize_event(
                    v.as_object().unwrap_or(&Map::new()),
                    i,
                    "generic-events",
                    false,
                )
            })
            .collect()),
        "codex-transcript" => Ok(args
            .get("transcript")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .enumerate()
            .map(|(i, v)| normalize_event(v.as_object().unwrap_or(&Map::new()), i, "codex", true))
            .collect()),
        "codex-jsonl" => {
            let text = args.get("jsonl").and_then(Value::as_str).unwrap_or("");
            let mut result = Vec::new();
            for (i, line) in text
                .lines()
                .filter(|line| !line.trim().is_empty())
                .enumerate()
            {
                let value: Value = serde_json::from_str(line).map_err(|_| {
                    diagnostic(
                        "runtime_introspection_invalid_jsonl",
                        &format!("runtime_introspection_invalid_jsonl:{}", i + 1),
                    )
                })?;
                result.push(normalize_event(
                    value.as_object().unwrap_or(&Map::new()),
                    i,
                    "codex",
                    true,
                ));
            }
            Ok(result)
        }
        _ => Err(diagnostic(
            "runtime_introspection_format_unsupported",
            "runtime_introspection_format_unsupported",
        )),
    }
}

fn normalize_event(record: &Map<String, Value>, index: usize, adapter: &str, codex: bool) -> Event {
    let tool = if codex {
        first_string(record, &["tool_name", "name", "namespace"])
    } else {
        first_string(record, &["tool_name"])
    };
    let explicit_surface = first_string(record, &["surface_id"]);
    let kind_value = if codex {
        first_string(record, &["kind", "type", "event", "role"])
    } else {
        first_string(record, &["kind"])
    };
    let status_value = if codex {
        first_string(record, &["status", "outcome"])
    } else {
        first_string(record, &["status"])
    };
    Event {
        event_id: first_string(
            record,
            if codex {
                &["id", "event_id"]
            } else {
                &["event_id"]
            },
        )
        .unwrap_or_else(|| {
            if codex {
                format!("codex_event_{}", index + 1)
            } else {
                format!("event_{}", index + 1)
            }
        }),
        timestamp: first_string(record, &["timestamp"]),
        input_adapter: first_string(record, &["input_adapter", "source"])
            .unwrap_or_else(|| adapter.to_string()),
        kind: normalize_kind(kind_value.as_deref(), tool.as_deref()),
        status: normalize_status(status_value.as_deref()),
        surface_id: explicit_surface.or_else(|| tool.as_deref().and_then(surface_from_tool_name)),
        tool_name: tool,
        duration_ms: first_number(
            record,
            if codex {
                &["duration_ms", "elapsed_ms"]
            } else {
                &["duration_ms"]
            },
        ),
        message: first_string(
            record,
            if codex {
                &["content", "text", "message"]
            } else {
                &["message"]
            },
        ),
    }
}

fn first_string(record: &Map<String, Value>, keys: &[&str]) -> Option<String> {
    keys.iter().find_map(|key| {
        record
            .get(*key)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .map(str::to_string)
    })
}
fn first_number(record: &Map<String, Value>, keys: &[&str]) -> Option<f64> {
    keys.iter().find_map(|key| {
        record
            .get(*key)
            .and_then(|v| v.as_f64().or_else(|| v.as_str()?.parse::<f64>().ok()))
            .filter(|v| v.is_finite() && *v >= 0.0)
    })
}
fn normalize_kind(value: Option<&str>, tool: Option<&str>) -> String {
    let kind = value.unwrap_or("").to_ascii_lowercase();
    if kind.contains("error") {
        "error"
    } else if kind.contains("result") {
        "tool_result"
    } else if kind.contains("tool") || kind.contains("function") || tool.is_some() {
        "tool_call"
    } else if kind.contains("handoff") {
        "handoff"
    } else if kind.contains("user") || kind.contains("assistant") || kind.contains("message") {
        "message"
    } else {
        "observation"
    }
    .to_string()
}
fn normalize_status(value: Option<&str>) -> String {
    match value.unwrap_or("").to_ascii_lowercase().as_str() {
        "ok" | "passed" | "success" | "succeeded" | "complete" | "completed" => "ok",
        "fail" | "failed" | "failure" => "failed",
        "error" | "errored" => "error",
        "refused" | "denied" | "blocked" => "refused",
        "pending" | "running" => "pending",
        _ => "unknown",
    }
    .to_string()
}
fn surface_from_tool_name(tool: &str) -> Option<String> {
    let rest = tool
        .strip_prefix("mcp__narada_")
        .or_else(|| tool.strip_prefix("MCP__NARADA_"))?;
    let (_, namespace) = rest.split_once('_')?;
    let namespace = namespace.split(['.', '/']).next()?;
    let mapped = match namespace {
        "agent_context" => "agent-context",
        "cloudflare_carrier" => "cloudflare-carrier",
        "delegated_task" => "delegated-task",
        "graph_mail" => "graph-mail",
        "local_filesystem" => "local-filesystem",
        "mcp_registrar" => "mcp-registrar",
        "site_coherence" => "site-coherence",
        "site_inbox" => "site-inbox",
        "structured_command" => "structured-command",
        "surface_feedback" => "surface-feedback",
        "task_lifecycle" => "task-lifecycle",
        "worker_delegation" => "worker-delegation",
        value => return Some(value.replace('_', "-")),
    };
    Some(mapped.to_string())
}
fn compare_events(a: &Event, b: &Event) -> Ordering {
    match (&a.timestamp, &b.timestamp) {
        (Some(x), Some(y)) if x != y => x.cmp(y),
        _ => a.event_id.cmp(&b.event_id),
    }
}
fn event_value(event: &Event) -> Value {
    serde_json::to_value(event).unwrap_or(Value::Null)
}
fn event_size(event: &Event, index: usize) -> Value {
    let value = event_value(event);
    let serialized = serde_json::to_string(&value).unwrap_or_default();
    json!({"index":index,"event_id":event.event_id,"kind":event.kind,"status":event.status,"surface_id":event.surface_id,"tool_name":event.tool_name,"bytes":serialized.as_bytes().len(),"chars":serialized.chars().count(),"estimated_tokens":estimate_tokens(serialized.chars().count())})
}
fn estimate_tokens(chars: usize) -> usize {
    (chars + 3) / 4
}
fn is_error(event: &Event) -> bool {
    event.kind == "error" || matches!(event.status.as_str(), "error" | "failed" | "refused")
}
fn count_by<F>(events: &[Event], selector: F) -> Map<String, Value>
where
    F: Fn(&Event) -> Option<String>,
{
    let mut result = Map::new();
    for event in events {
        if let Some(value) = selector(event) {
            let count = result.get(&value).and_then(Value::as_u64).unwrap_or(0) + 1;
            result.insert(value, json!(count));
        }
    }
    result
}
fn ranked_counts(
    counts: &Map<String, Value>,
    events: &[Event],
    dimension: &str,
    sort: &str,
    limit: usize,
) -> Vec<Value> {
    let mut items: Vec<Value> = counts.iter().map(|(name,count)| { let related = events.iter().filter(|event| dimension_value(event, dimension).as_deref()==Some(name)); let duration: f64 = related.clone().filter_map(|event| event.duration_ms).sum(); let errors = related.clone().filter(|event| is_error(event)).count(); let refused = related.filter(|event| event.status=="refused").count(); json!({"name":name,"count":count,"duration_ms":duration,"errors":errors,"refused":refused}) }).collect();
    items.sort_by(|a, b| compare_ranked(a, b, sort));
    items.truncate(limit);
    items
}
fn compare_ranked(a: &Value, b: &Value, sort: &str) -> Ordering {
    let key = if matches!(sort, "duration_ms" | "errors") {
        sort
    } else {
        "count"
    };
    let av = a.get(key).and_then(Value::as_f64).unwrap_or(0.0);
    let bv = b.get(key).and_then(Value::as_f64).unwrap_or(0.0);
    bv.partial_cmp(&av)
        .unwrap_or(Ordering::Equal)
        .then_with(|| {
            a["name"]
                .as_str()
                .unwrap_or("")
                .cmp(b["name"].as_str().unwrap_or(""))
        })
}
fn dimension_value(event: &Event, dimension: &str) -> Option<String> {
    match dimension {
        "surface" => event.surface_id.clone(),
        "tool" => event.tool_name.clone(),
        "status" => Some(event.status.clone()),
        "kind" => Some(event.kind.clone()),
        "adapter" => Some(event.input_adapter.clone()),
        _ => None,
    }
}
fn token_categories(events: &[Event]) -> Map<String, Value> {
    let mut result = Map::new();
    for event in events {
        let category = if event.kind == "message" {
            event
                .tool_name
                .clone()
                .unwrap_or_else(|| "message".to_string())
        } else {
            event
                .surface_id
                .clone()
                .unwrap_or_else(|| event.kind.clone())
        };
        let bytes = serde_json::to_string(event)
            .unwrap_or_default()
            .chars()
            .count();
        let current = result.get(&category).and_then(Value::as_u64).unwrap_or(0);
        result.insert(category, json!(current + estimate_tokens(bytes) as u64));
    }
    result
}
