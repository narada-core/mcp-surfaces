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
        tool("runtime_introspection_top_events", "Analyze inline input and return the N largest normalized runtime trace events by serialized size.", trace_query_schema(json!({"limit":{"type":"integer","minimum":1,"maximum":200}}))),
        tool("runtime_introspection_analyze_trace", "Analyze saved or inline runtime trace/session JSONL composition into Narada runtime introspection metrics.", input_schema()),
        tool("runtime_introspection_analyze", "Analyze inline runtime events or Codex adapter records into Narada runtime composition metrics.", input_schema()),
        tool("runtime_introspection_top", "Analyze inline input and return ranked runtime metrics in one call.", trace_query_schema(json!({"dimension":{"type":"string","enum":DIMENSIONS},"limit":{"type":"integer","minimum":1,"maximum":50},"sort":{"type":"string","enum":["count","duration_ms","errors"]}}))),
        tool("runtime_introspection_show", "Analyze inline input and show one focused read-only view in one call.", trace_query_schema(json!({"view":{"type":"string","enum":VIEWS},"limit":{"type":"integer","minimum":1,"maximum":200}}))),
        tool("runtime_introspection_show_event", "Analyze inline input and show one normalized event by event_id or zero-based index.", trace_query_schema(json!({"event_id":{"type":"string","minLength":1,"maxLength":512},"index":{"type":"integer","minimum":0,"maximum":499}}))),
        memory_tool("runtime_introspection_memory_status", "Show freshness, coverage, and incident counts from the canonical server-bound Site runtime observer store.", json!({"type":"object","properties":{},"additionalProperties":false}), &[]),
        memory_tool("runtime_introspection_memory_owners", "List bounded runtime resource owners and their latest process/worker measurements.", json!({"type":"object","properties":{"active_only":{"type":"boolean"},"limit":{"type":"integer","minimum":1,"maximum":200}},"additionalProperties":false}), &[]),
        memory_tool("runtime_introspection_memory_timeline", "Read a bounded process and worker memory timeline for one exact runtime owner.", json!({"type":"object","properties":{"owner_id":{"type":"string"},"before_ms":{"type":"integer","minimum":0},"limit":{"type":"integer","minimum":1,"maximum":500}},"required":["owner_id"],"additionalProperties":false}), &["owner_id"]),
        memory_tool("runtime_introspection_memory_attribution", "Explain current worker-runtime-attributed and residual process memory for one exact owner without double-counting ArrayBuffers.", json!({"type":"object","properties":{"owner_id":{"type":"string"}},"required":["owner_id"],"additionalProperties":false}), &["owner_id"]),
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
        "first_use":["Select an explicit input format.","Use top, show, top_events, or show_event directly with the inline input; a separate analyze call is unnecessary.","Treat structuredContent as authoritative evidence."],
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
    analyze(args)
}

