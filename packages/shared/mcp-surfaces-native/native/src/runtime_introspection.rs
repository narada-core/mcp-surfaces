use rusqlite::{params, types::ValueRef, Connection, OpenFlags};
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use std::cmp::Ordering;
use std::path::Path;
use time::OffsetDateTime;

const FORMATS: &[&str] = &["generic-events", "codex-jsonl", "codex-transcript"];
const DIMENSIONS: &[&str] = &["surface", "tool", "status", "kind", "adapter"];
const VIEWS: &[&str] = &[
    "summary", "timeline", "surfaces", "tools", "errors", "adapters",
];

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
struct Event {
    event_id: String,
    timestamp: Option<String>,
    input_adapter: String,
    kind: String,
    status: String,
    surface_id: Option<String>,
    tool_name: Option<String>,
    duration_ms: Option<f64>,
    message: Option<String>,
}

pub fn list_tools() -> Vec<Value> {
    vec![
        guidance_tool(),
        tool("runtime_introspection_formats", "List the read-only inline input formats accepted by the runtime introspection surface.", json!({"type":"object","properties":{},"additionalProperties":false})),
        tool("runtime_introspection_top_events", "Return the N largest normalized runtime trace events by serialized size.", json!({"type":"object","properties":{"analysis":{"type":"object","additionalProperties":true},"limit":{"type":"integer","minimum":1,"maximum":200}},"additionalProperties":false})),
        tool("runtime_introspection_analyze_trace", "Analyze saved or inline runtime trace/session JSONL composition into Narada runtime introspection metrics.", input_schema()),
        tool("runtime_introspection_analyze", "Analyze inline runtime events or Codex adapter records into Narada runtime composition metrics.", input_schema()),
        tool("runtime_introspection_top", "Return ranked runtime metrics from an existing analysis or inline input events.", json!({"type":"object","properties":{"analysis":{"type":"object","additionalProperties":true},"format":{"type":"string","enum":FORMATS},"events":{"type":"array","items":{"type":"object","additionalProperties":true}},"jsonl":{"type":"string"},"transcript":{"type":"array","items":{"type":"object","additionalProperties":true}},"dimension":{"type":"string","enum":DIMENSIONS},"limit":{"type":"integer","minimum":1,"maximum":50},"sort":{"type":"string","enum":["count","duration_ms","errors"]}},"additionalProperties":false})),
        tool("runtime_introspection_show", "Show a focused read-only view from an existing analysis or inline input events.", json!({"type":"object","properties":{"analysis":{"type":"object","additionalProperties":true},"format":{"type":"string","enum":FORMATS},"events":{"type":"array","items":{"type":"object","additionalProperties":true}},"jsonl":{"type":"string"},"transcript":{"type":"array","items":{"type":"object","additionalProperties":true}},"view":{"type":"string","enum":VIEWS},"limit":{"type":"integer","minimum":1,"maximum":200}},"additionalProperties":false})),
        tool("runtime_introspection_show_event", "Show one normalized runtime trace event by event_id or zero-based index.", json!({"type":"object","properties":{"analysis":{"type":"object","additionalProperties":true},"format":{"type":"string","enum":FORMATS},"events":{"type":"array","items":{"type":"object","additionalProperties":true}},"jsonl":{"type":"string"},"transcript":{"type":"array","items":{"type":"object","additionalProperties":true}},"event_id":{"type":"string"},"index":{"type":"integer","minimum":0}},"additionalProperties":false})),
        memory_tool("runtime_introspection_memory_status", "Show freshness, coverage, and incident counts from the canonical server-bound Site runtime observer store.", json!({"type":"object","properties":{},"additionalProperties":false}), &[]),
        memory_tool("runtime_introspection_memory_owners", "List bounded runtime resource owners and their latest process/worker measurements.", json!({"type":"object","properties":{"active_only":{"type":"boolean"},"limit":{"type":"integer","minimum":1,"maximum":200}},"additionalProperties":false}), &[]),
        memory_tool("runtime_introspection_memory_timeline", "Read a bounded process and worker memory timeline for one exact runtime owner.", json!({"type":"object","properties":{"owner_id":{"type":"string"},"before_ms":{"type":"integer","minimum":0},"limit":{"type":"integer","minimum":1,"maximum":500}},"required":["owner_id"],"additionalProperties":false}), &["owner_id"]),
        memory_tool("runtime_introspection_memory_attribution", "Explain current V8-attributed and residual process memory for one exact owner without double-counting ArrayBuffers.", json!({"type":"object","properties":{"owner_id":{"type":"string"}},"required":["owner_id"],"additionalProperties":false}), &["owner_id"]),
        memory_tool("runtime_introspection_memory_incidents", "List bounded memory incidents from the canonical observer store.", json!({"type":"object","properties":{"status":{"type":"string","enum":["open","reviewed","dismissed","all"]},"limit":{"type":"integer","minimum":1,"maximum":200}},"additionalProperties":false}), &[]),
        memory_tool("runtime_introspection_memory_incident_show", "Show one memory incident with sanitized evidence and artifact metadata.", json!({"type":"object","properties":{"incident_id":{"type":"string"}},"required":["incident_id"],"additionalProperties":false}), &["incident_id"]),
    ]
}

pub fn call_tool(name: &str, args: &Map<String, Value>, root: &Path) -> Result<Value, Value> {
    match name {
        "runtime_introspection_guidance" => Ok(guidance(args)),
        "runtime_introspection_formats" => Ok(formats()),
        "runtime_introspection_analyze" | "runtime_introspection_analyze_trace" => analyze(args),
        "runtime_introspection_top_events" => top_events(args),
        "runtime_introspection_top" => top(args),
        "runtime_introspection_show" => show(args),
        "runtime_introspection_show_event" => show_event(args),
        "runtime_introspection_memory_status" => memory_status(root),
        "runtime_introspection_memory_owners" => memory_owners(args, root),
        "runtime_introspection_memory_timeline" => memory_timeline(args, root),
        "runtime_introspection_memory_attribution" => memory_attribution(args, root),
        "runtime_introspection_memory_incidents" => memory_incidents(args, root),
        "runtime_introspection_memory_incident_show" => memory_incident_show(args, root),
        _ => Err(diagnostic("unknown_tool", &format!("unknown_tool:{name}"))),
    }
}

fn guidance(args: &Map<String, Value>) -> Value {
    json!({
        "schema":"narada.mcp_surface.guidance.v0","status":"ok","surface_id":"runtime-introspection",
        "guidance_tool":"runtime_introspection_guidance",
        "purpose":"Analyze bounded runtime events and observer evidence without actuation.",
        "requested":{"workflow":args.get("workflow").cloned().unwrap_or(Value::Null),"tool":args.get("tool").cloned().unwrap_or(Value::Null)},
        "first_use":["Select an explicit input format.","Analyze before ranking or showing a view.","Treat structuredContent as authoritative evidence."],
        "boundaries":["The surface is read-only.","Inputs are inline or server-bound observer evidence; no arbitrary commands are executed.","All list and timeline results are bounded."]
    })
}

fn formats() -> Value {
    json!({
        "schema":"narada.runtime_introspection.formats.v1","status":"ok",
        "formats":[
            {"format":"generic-events","description":"Array of normalized runtime events supplied inline.","input_field":"events"},
            {"format":"codex-jsonl","description":"JSON Lines records from a Codex transcript or tool event stream, consumed as an input adapter.","input_field":"jsonl"},
            {"format":"codex-transcript","description":"Array of Codex transcript-like records, consumed as an input adapter.","input_field":"transcript"}
        ],
        "adapter_model":{"codex":"input_adapter_only","narada_surface_identity":"derived_from_mcp_tool_names_or_explicit_surface_id"}
    })
}

fn analyze(args: &Map<String, Value>) -> Result<Value, Value> {
    let format = normalize_enum(
        args.get("format"),
        "generic-events",
        FORMATS,
        "runtime_introspection_format_unsupported",
    )?;
    let mut events = normalize_events(args, &format)?;
    events.sort_by(compare_events);
    let by_surface = count_by(&events, |e| e.surface_id.clone());
    let by_tool = count_by(&events, |e| e.tool_name.clone());
    let by_status = count_by(&events, |e| Some(e.status.clone()));
    let by_kind = count_by(&events, |e| Some(e.kind.clone()));
    let by_adapter = count_by(&events, |e| Some(e.input_adapter.clone()));
    let errors: Vec<Event> = events.iter().filter(|e| is_error(e)).cloned().collect();
    let refused = events.iter().filter(|e| e.status == "refused").count();
    let total_duration: f64 = events.iter().filter_map(|e| e.duration_ms).sum();
    let sizes: Vec<Value> = events
        .iter()
        .enumerate()
        .map(|(i, e)| event_size(e, i))
        .collect();
    let total_bytes: usize = sizes
        .iter()
        .map(|v| v["bytes"].as_u64().unwrap_or(0) as usize)
        .sum();
    let total_chars: usize = sizes
        .iter()
        .map(|v| v["chars"].as_u64().unwrap_or(0) as usize)
        .sum();
    let analysis_id = args
        .get("analysis_id")
        .and_then(Value::as_str)
        .filter(|v| !v.trim().is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| stable_id(&format, &events));
    let mut notes =
        vec!["codex_records_are_treated_as_input_adapter_not_narada_surface".to_string()];
    if events.iter().any(|e| e.timestamp.is_none()) {
        notes.push("some_events_missing_timestamp".to_string());
    }
    if events
        .iter()
        .any(|e| e.surface_id.is_none() && e.tool_name.is_some())
    {
        notes.push("some_tools_do_not_map_to_narada_surface".to_string());
    }
    let mut largest = sizes.clone();
    largest.sort_by(|a, b| {
        b["bytes"]
            .as_u64()
            .unwrap_or(0)
            .cmp(&a["bytes"].as_u64().unwrap_or(0))
    });
    let timeline: Vec<Value> = events.iter().map(event_value).collect();
    let top_surfaces = ranked_counts(&by_surface, &events, "surface", "count", 10);
    let top_tools = ranked_counts(&by_tool, &events, "tool", "count", 10);
    let top_errors: Vec<Value> = errors.iter().take(10).map(event_value).collect();
    Ok(json!({
        "schema":"narada.runtime_introspection.analysis.v0","status":"analyzed","analysis_id":analysis_id,
        "generated_at":now_iso(),"format":format,
        "summary":{"event_count":events.len(),"tool_call_count":events.iter().filter(|e| e.kind=="tool_call").count(),"error_count":errors.len(),"refused_count":refused,"surface_count":by_surface.len(),"tool_count":by_tool.len(),"input_adapters":by_adapter.keys().cloned().collect::<Vec<_>>(),"total_duration_ms":total_duration,"total_bytes":total_bytes,"total_chars":total_chars,"estimated_tokens":estimate_tokens(total_chars)},
        "counts":{"by_surface":by_surface,"by_tool":by_tool,"by_status":by_status,"by_kind":by_kind,"by_adapter":by_adapter},
        "top":{"surfaces":top_surfaces,"tools":top_tools,"errors":top_errors},"timeline":timeline,
        "largest_events":largest.into_iter().take(10).collect::<Vec<_>>(),"token_estimate_by_category":token_categories(&events),"notes":notes
    }))
}

fn top_events(args: &Map<String, Value>) -> Result<Value, Value> {
    let analysis = analysis_from_args(args)?;
    let limit = bounded(args.get("limit"), 10, 1, 200);
    let mut events: Vec<Value> = timeline(&analysis)
        .iter()
        .enumerate()
        .map(|(i, e)| event_size(e, i))
        .collect();
    events.sort_by(|a, b| {
        b["bytes"]
            .as_u64()
            .unwrap_or(0)
            .cmp(&a["bytes"].as_u64().unwrap_or(0))
    });
    Ok(
        json!({"schema":"narada.runtime_introspection.top_events.v0","status":"ok","analysis_id":analysis["analysis_id"],"limit":limit,"events":events.into_iter().take(limit).collect::<Vec<_>>() }),
    )
}

fn top(args: &Map<String, Value>) -> Result<Value, Value> {
    let analysis = analysis_from_args(args)?;
    let dimension = normalize_enum(
        args.get("dimension"),
        "surface",
        DIMENSIONS,
        "runtime_introspection_top_dimension_unsupported",
    )?;
    let limit = bounded(args.get("limit"), 10, 1, 50);
    let sort = args.get("sort").and_then(Value::as_str).unwrap_or("count");
    let events = timeline(&analysis);
    let key = format!("by_{dimension}");
    let counts = analysis["counts"]
        .get(&key)
        .cloned()
        .unwrap_or_else(|| json!({}));
    let mut items = ranked_counts(
        counts.as_object().unwrap_or(&Map::new()),
        &events,
        &dimension,
        sort,
        limit.max(200),
    );
    items.sort_by(|a, b| compare_ranked(a, b, sort));
    items.truncate(limit);
    Ok(
        json!({"schema":"narada.runtime_introspection.top.v1","status":"ok","analysis_id":analysis["analysis_id"],"dimension":dimension,"sort":sort,"limit":limit,"items":items}),
    )
}

fn show(args: &Map<String, Value>) -> Result<Value, Value> {
    let analysis = analysis_from_args(args)?;
    let view = normalize_enum(
        args.get("view"),
        "summary",
        VIEWS,
        "runtime_introspection_show_view_unsupported",
    )?;
    let limit = bounded(args.get("limit"), 50, 1, 200);
    let events = timeline(&analysis);
    let data = match view.as_str() {
        "summary" => analysis["summary"].clone(),
        "timeline" => json!(events
            .iter()
            .take(limit)
            .map(event_value)
            .collect::<Vec<_>>()),
        "errors" => json!(events
            .iter()
            .filter(|e| is_error(e))
            .take(limit)
            .map(event_value)
            .collect::<Vec<_>>()),
        "surfaces" | "tools" | "adapters" => {
            let dimension = if view == "surfaces" {
                "surface"
            } else if view == "tools" {
                "tool"
            } else {
                "adapter"
            };
            let key = format!("by_{dimension}");
            let counts = analysis["counts"]
                .get(&key)
                .and_then(Value::as_object)
                .cloned()
                .unwrap_or_default();
            json!(ranked_counts(&counts, &events, dimension, "count", limit))
        }
        _ => Value::Null,
    };
    Ok(
        json!({"schema":"narada.runtime_introspection.show.v1","status":"ok","analysis_id":analysis["analysis_id"],"view":view,"limit":limit,"data":data}),
    )
}

fn show_event(args: &Map<String, Value>) -> Result<Value, Value> {
    let analysis = analysis_from_args(args)?;
    let events = timeline(&analysis);
    let selected = if let Some(id) = args.get("event_id").and_then(Value::as_str) {
        events.iter().find(|e| e.event_id == id).cloned()
    } else if let Some(index) = args.get("index").and_then(Value::as_i64) {
        events.get(index.max(0) as usize).cloned()
    } else {
        None
    };
    let event = selected.ok_or_else(|| {
        diagnostic(
            "runtime_introspection_event_not_found",
            "runtime_introspection_event_not_found",
        )
    })?;
    let index = events
        .iter()
        .position(|e| e.event_id == event.event_id)
        .unwrap_or(0);
    Ok(
        json!({"schema":"narada.runtime_introspection.event.v0","status":"ok","analysis_id":analysis["analysis_id"],"event":event_value(&event),"size":event_size(&event,index)}),
    )
}

fn analysis_from_args(args: &Map<String, Value>) -> Result<Value, Value> {
    if let Some(analysis) = args.get("analysis").filter(|v| v.is_object()) {
        let schema = analysis.get("schema").and_then(Value::as_str).unwrap_or("");
        if schema == "narada.runtime_introspection.analysis.v0"
            || schema == "narada.runtime_introspection.analysis.v1"
        {
            return Ok(analysis.clone());
        }
    }
    analyze(args)
}

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
fn stable_id(format: &str, events: &[Event]) -> String {
    let bytes = serde_json::to_vec(&json!({"format":format,"events":events})).unwrap_or_default();
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    let digest = hasher.finalize();
    format!(
        "analysis_{}",
        digest
            .iter()
            .take(6)
            .map(|b| format!("{b:02x}"))
            .collect::<String>()
    )
}
fn timeline(analysis: &Value) -> Vec<Event> {
    analysis
        .get("timeline")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|value| serde_json::from_value(value.clone()).ok())
        .collect()
}
fn normalize_enum(
    value: Option<&Value>,
    fallback: &str,
    allowed: &[&str],
    code: &str,
) -> Result<String, Value> {
    let selected = value.and_then(Value::as_str).unwrap_or(fallback);
    if allowed.contains(&selected) {
        Ok(selected.to_string())
    } else {
        Err(diagnostic(code, &format!("{code}:{selected}")))
    }
}
fn bounded(value: Option<&Value>, fallback: usize, min: usize, max: usize) -> usize {
    let candidate = value
        .and_then(Value::as_i64)
        .map(|n| n as usize)
        .unwrap_or(fallback);
    candidate.clamp(min, max)
}
fn now_iso() -> String {
    OffsetDateTime::now_utc()
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_else(|_| "1970-01-01T00:00:00Z".to_string())
}
fn input_schema() -> Value {
    json!({"type":"object","properties":{"analysis_id":{"type":"string"},"format":{"type":"string","enum":FORMATS},"events":{"type":"array","items":{"type":"object","additionalProperties":false}},"jsonl":{"type":"string"},"transcript":{"type":"array","items":{"type":"object","additionalProperties":false}}},"additionalProperties":false})
}
fn tool(name: &str, description: &str, schema: Value) -> Value {
    json!({"name":name,"description":description,"inputSchema":schema,"annotations":{"title":name,"readOnlyHint":true,"destructiveHint":false,"idempotentHint":true,"openWorldHint":false},"outputSchema":{"type":"object","additionalProperties":true}})
}
fn memory_tool(name: &str, description: &str, schema: Value, _required: &[&str]) -> Value {
    tool(name, description, schema)
}
fn guidance_tool() -> Value {
    tool(
        "runtime_introspection_guidance",
        "Show model-facing operating guidance for runtime introspection workflows.",
        json!({"type":"object","properties":{"workflow":{"type":"string"},"tool":{"type":"string"}},"additionalProperties":false}),
    )
}
fn diagnostic(code: &str, message: &str) -> Value {
    json!({"code":code,"message":message})
}

fn database_path(root: &Path) -> std::path::PathBuf {
    root.join(".narada")
        .join("runtime")
        .join("mcp-runtime-observer")
        .join("observations.db")
}
fn open_database(root: &Path) -> Result<Connection, Value> {
    let path = database_path(root);
    if !path.exists() {
        return Err(diagnostic(
            "runtime_introspection_memory_store_unavailable",
            "runtime_introspection_memory_store_unavailable",
        ));
    }
    Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY).map_err(|_| {
        diagnostic(
            "runtime_introspection_memory_store_unavailable",
            "runtime_introspection_memory_store_unavailable",
        )
    })
}
fn memory_status(root: &Path) -> Result<Value, Value> {
    let db = open_database(root)?;
    let process=query_one(&db,"SELECT COUNT(*) samples,MAX(sampled_at_ms) last_sample_at_ms,COUNT(DISTINCT owner_id) sampled_owners FROM process_samples",params![])?;
    let workers=query_one(&db,"SELECT COUNT(*) samples,MAX(sampled_at_ms) last_sample_at_ms,COUNT(DISTINCT owner_id) sampled_owners FROM worker_samples",params![])?;
    let owners=query_one(&db,"SELECT COUNT(*) owners,SUM(CASE WHEN active=1 THEN 1 ELSE 0 END) active_owners FROM owners",params![])?;
    let incidents=query_one(&db,"SELECT COUNT(*) incidents,SUM(CASE WHEN status='open' THEN 1 ELSE 0 END) open_incidents FROM incidents",params![])?;
    let last = process["last_sample_at_ms"]
        .as_i64()
        .unwrap_or(0)
        .max(workers["last_sample_at_ms"].as_i64().unwrap_or(0));
    let mut result = json!({"schema":"narada.runtime_introspection.memory_status.v1","status":if last==0 {"empty"} else {"ready"},"observed_at":now_iso(),"last_sample_at":if last>0 {json!(last)} else {Value::Null},"process":process,"workers":workers,"authority":"server_bound_site","response":"evidence_only_no_automatic_actuation"});
    if let Some(obj) = result.as_object_mut() {
        obj.extend(owners);
        obj.extend(incidents);
    }
    Ok(result)
}
fn memory_owners(args: &Map<String, Value>, root: &Path) -> Result<Value, Value> {
    let db = open_database(root)?;
    let limit = bounded(args.get("limit"), 50, 1, 200) as i64;
    let active = if args.get("active_only").and_then(Value::as_bool) == Some(false) {
        0
    } else {
        1
    };
    let items=query_rows(&db,"SELECT o.owner_id,o.site_id,o.authority_ref,o.owner_kind,o.pid,o.process_started_at,o.parent_owner_id,o.surface_id,o.instance_id,o.generation_id,o.carrier_session_id,o.executable_name,o.observed_at,o.active FROM owners o WHERE (?1=0 OR active=1) ORDER BY active DESC,observed_at DESC LIMIT ?2",params![active,limit])?;
    Ok(
        json!({"schema":"narada.runtime_introspection.memory_owners.v1","items":items,"count":items.len(),"limit":limit}),
    )
}
fn memory_timeline(args: &Map<String, Value>, root: &Path) -> Result<Value, Value> {
    let db = open_database(root)?;
    let owner = require_string(args, "owner_id")?;
    let limit = bounded(args.get("limit"), 100, 1, 500) as i64;
    let before = args
        .get("before_ms")
        .and_then(Value::as_i64)
        .unwrap_or(i64::MAX);
    let items=query_rows(&db,"SELECT sampled_at_ms,'process' sample_kind,private_bytes primary_bytes,working_set_bytes,commit_bytes,handle_count,thread_count,NULL heap_used_bytes,NULL external_bytes,NULL array_buffers_bytes FROM process_samples WHERE owner_id=?1 AND sampled_at_ms<?2 UNION ALL SELECT sampled_at_ms,'worker',heap_used_bytes,NULL,NULL,NULL,NULL,heap_used_bytes,external_bytes,array_buffers_bytes FROM worker_samples WHERE owner_id=?1 AND sampled_at_ms<?2 ORDER BY sampled_at_ms DESC LIMIT ?3",params![owner,before,limit])?;
    Ok(
        json!({"schema":"narada.runtime_introspection.memory_timeline.v1","owner_id":owner,"items":items,"count":items.len(),"next_before_ms":if items.len()==limit as usize {items.last().and_then(|v|v["sampled_at_ms"].clone().as_i64())} else {None}}),
    )
}
fn memory_attribution(args: &Map<String, Value>, root: &Path) -> Result<Value, Value> {
    let db = open_database(root)?;
    let owner = require_string(args, "owner_id")?;
    let process=query_one(&db,"SELECT * FROM process_samples WHERE owner_id IN (?1,COALESCE((SELECT parent_owner_id FROM owners WHERE owner_id=?1),?1)) ORDER BY sampled_at_ms DESC LIMIT 1",params![owner])?;
    let worker = query_one(
        &db,
        "SELECT * FROM worker_samples WHERE owner_id=?1 ORDER BY sampled_at_ms DESC LIMIT 1",
        params![owner],
    )?;
    let private_bytes = number_field(&process, "private_bytes");
    let heap = number_field(&worker, "heap_used_bytes");
    let external = number_field(&worker, "external_bytes");
    let buffers = number_field(&worker, "array_buffers_bytes");
    let attributed = private_bytes.min(heap + external);
    let ratio = if private_bytes > 0 {
        attributed as f64 / private_bytes as f64
    } else {
        0.0
    };
    let (classification, confidence) = if ratio >= 0.7 {
        ("direct", 0.92)
    } else if ratio >= 0.4 {
        ("partial", 0.7)
    } else {
        ("residual", 0.45)
    };
    Ok(
        json!({"schema":"narada.runtime_introspection.memory_attribution.v1","owner_id":owner,"attribution":classification,"confidence":confidence,"process_private_bytes":if private_bytes>0{json!(private_bytes)}else{Value::Null},"worker_heap_used_bytes":if heap>0{json!(heap)}else{Value::Null},"worker_external_bytes":if external>0{json!(external)}else{Value::Null},"worker_array_buffers_bytes":if buffers>0{json!(buffers)}else{Value::Null},"attributed_v8_bytes":if attributed>0{json!(attributed)}else{Value::Null},"non_v8_residual_bytes":if private_bytes>0{json!(private_bytes-attributed)}else{Value::Null},"note":"array_buffers_are_reported_as_evidence_but_not_added_to_external_to_avoid_double_counting"}),
    )
}
fn memory_incidents(args: &Map<String, Value>, root: &Path) -> Result<Value, Value> {
    let db = open_database(root)?;
    let limit = bounded(args.get("limit"), 50, 1, 200) as i64;
    let status = args.get("status").and_then(Value::as_str).unwrap_or("open");
    let items=query_rows(&db,"SELECT * FROM incidents WHERE (?1='all' OR status=?1) ORDER BY updated_at_ms DESC LIMIT ?2",params![status,limit])?;
    Ok(
        json!({"schema":"narada.runtime_introspection.memory_incidents.v1","status":status,"items":items,"count":items.len()}),
    )
}
fn memory_incident_show(args: &Map<String, Value>, root: &Path) -> Result<Value, Value> {
    let db = open_database(root)?;
    let id = require_string(args, "incident_id")?;
    let incident = query_one(
        &db,
        "SELECT * FROM incidents WHERE incident_id=?1",
        params![id],
    )?;
    if incident.is_empty() {
        return Err(diagnostic(
            "runtime_introspection_memory_incident_not_found",
            "runtime_introspection_memory_incident_not_found",
        ));
    }
    let mut evidence=query_rows(&db,"SELECT evidence_id,created_at_ms,evidence_type,payload_json FROM evidence WHERE incident_id=?1 ORDER BY created_at_ms",params![id])?;
    for item in &mut evidence { project_evidence_payload(item)?; }
    let artifacts=query_rows(&db,"SELECT artifact_id,created_at_ms,path,kind,bytes FROM artifacts WHERE incident_id=?1 ORDER BY created_at_ms",params![id])?;
    Ok(
        json!({"schema":"narada.runtime_introspection.memory_incident.v1","incident":incident,"evidence":evidence,"artifacts":artifacts}),
    )
}
fn project_evidence_payload(item: &mut Value) -> Result<(), Value> {
    let object = item.as_object_mut().ok_or_else(|| diagnostic("runtime_introspection_memory_evidence_corrupt","runtime_introspection_memory_evidence_corrupt"))?;
    let text = object.remove("payload_json").and_then(|value|value.as_str().map(ToString::to_string)).ok_or_else(|| diagnostic("runtime_introspection_memory_evidence_corrupt","runtime_introspection_memory_evidence_corrupt"))?;
    let payload = serde_json::from_str::<Value>(&text).map_err(|_| diagnostic("runtime_introspection_memory_evidence_corrupt","runtime_introspection_memory_evidence_corrupt"))?;
    object.insert("payload".to_string(),payload);
    Ok(())
}
fn number_field(value: &Map<String, Value>, key: &str) -> i64 {
    value
        .get(key)
        .and_then(|v| v.as_i64().or_else(|| v.as_f64().map(|n| n as i64)))
        .unwrap_or(0)
}
fn require_string(args: &Map<String, Value>, key: &str) -> Result<String, Value> {
    args.get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(str::to_string)
        .ok_or_else(|| {
            diagnostic(
                "runtime_introspection_memory_argument_required",
                &format!("runtime_introspection_memory_argument_required:{key}"),
            )
        })
}
fn query_one<P: rusqlite::Params>(
    db: &Connection,
    sql: &str,
    params: P,
) -> Result<Map<String, Value>, Value> {
    let mut statement = db.prepare(sql).map_err(|_| {
        diagnostic(
            "runtime_introspection_memory_store_unavailable",
            "runtime_introspection_memory_store_unavailable",
        )
    })?;
    let mut rows = statement.query(params).map_err(|_| {
        diagnostic(
            "runtime_introspection_memory_store_unavailable",
            "runtime_introspection_memory_store_unavailable",
        )
    })?;
    if let Some(row) = rows.next().map_err(|_| {
        diagnostic(
            "runtime_introspection_memory_store_unavailable",
            "runtime_introspection_memory_store_unavailable",
        )
    })? {
        row_to_map(row).map_err(|_| {
            diagnostic(
                "runtime_introspection_memory_store_unavailable",
                "runtime_introspection_memory_store_unavailable",
            )
        })
    } else {
        Ok(Map::new())
    }
}
fn query_rows<P: rusqlite::Params>(
    db: &Connection,
    sql: &str,
    params: P,
) -> Result<Vec<Value>, Value> {
    let mut statement = db.prepare(sql).map_err(|_| {
        diagnostic(
            "runtime_introspection_memory_store_unavailable",
            "runtime_introspection_memory_store_unavailable",
        )
    })?;
    let mut rows = statement.query(params).map_err(|_| {
        diagnostic(
            "runtime_introspection_memory_store_unavailable",
            "runtime_introspection_memory_store_unavailable",
        )
    })?;
    let mut result = Vec::new();
    while let Some(row) = rows.next().map_err(|_| {
        diagnostic(
            "runtime_introspection_memory_store_unavailable",
            "runtime_introspection_memory_store_unavailable",
        )
    })? {
        result.push(Value::Object(row_to_map(row).map_err(|_| {
            diagnostic(
                "runtime_introspection_memory_store_unavailable",
                "runtime_introspection_memory_store_unavailable",
            )
        })?));
    }
    Ok(result)
}
fn row_to_map(row: &rusqlite::Row<'_>) -> rusqlite::Result<Map<String, Value>> {
    let mut result = Map::new();
    let count = row.as_ref().column_count();
    for index in 0..count {
        let name = row.as_ref().column_name(index)?.to_string();
        let value = match row.get_ref(index)? {
            ValueRef::Null => Value::Null,
            ValueRef::Integer(v) => json!(v),
            ValueRef::Real(v) => json!(v),
            ValueRef::Text(v) => Value::String(String::from_utf8_lossy(v).to_string()),
            ValueRef::Blob(v) => Value::String(format!("blob:{}", v.len())),
        };
        result.insert(name, value);
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_tool_contract_is_read_only_and_bounded() {
        let tools = list_tools();
        assert_eq!(tools.len(), 14);
        assert_eq!(tools[0]["name"], "runtime_introspection_guidance");
        assert_eq!(tools[7]["name"], "runtime_introspection_show_event");
        assert!(tools
            .iter()
            .all(|tool| tool["annotations"]["readOnlyHint"] == true));
        assert!(tools
            .iter()
            .all(|tool| tool["inputSchema"]["additionalProperties"] == false));
    }

    #[test]
    fn native_analysis_matches_surface_and_refusal_counts() {
        let mut args = Map::new();
        args.insert("format".to_string(), json!("codex-transcript"));
        args.insert(
            "transcript".to_string(),
            json!([
                {"id":"1","timestamp":"2026-06-20T14:00:00.000Z","type":"tool_call","tool_name":"mcp__narada_andrey_local_filesystem.fs_read_file","status":"ok","duration_ms":12},
                {"id":"2","type":"tool_call","tool_name":"mcp__narada_andrey_structured_command.structured_c","status":"refused","duration_ms":3}
            ]),
        );
        let analysis = analyze(&args).unwrap();
        assert_eq!(analysis["summary"]["event_count"], 2);
        assert_eq!(analysis["summary"]["refused_count"], 1);
        assert_eq!(analysis["counts"]["by_surface"]["local-filesystem"], 1);
        assert_eq!(analysis["counts"]["by_surface"]["structured-command"], 1);
        assert_eq!(analysis["summary"]["input_adapters"][0], "codex");
    }

    #[test]
    fn invalid_jsonl_is_refused_with_bounded_diagnostic() {
        let mut args = Map::new();
        args.insert("format".to_string(), json!("codex-jsonl"));
        args.insert("jsonl".to_string(), json!("{\"id\":\"ok\"}\nnot-json"));
        let error = analyze(&args).unwrap_err();
        assert_eq!(error["code"], "runtime_introspection_invalid_jsonl");
    }

    #[test]
    fn incident_evidence_projects_payload_json_as_domain_payload() {
        let mut evidence = json!({"evidence_id":"e1","payload_json":"{\"rss_bytes\":42}"});
        project_evidence_payload(&mut evidence).unwrap();
        assert_eq!(evidence["payload"]["rss_bytes"], 42);
        assert!(evidence.get("payload_json").is_none());
    }
}
