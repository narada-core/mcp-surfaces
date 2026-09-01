use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use time::OffsetDateTime;
use uuid::Uuid;

const SERVER_NAME: &str = "narada-site-inbox-mcp";
const MAX_ENVELOPE_BYTES: u64 = 512_000;
const MAX_OUTPUT_BYTES: u64 = 512_000;
const MAX_LOG_BYTES: u64 = 10 * 1024 * 1024;
const STATUSES: &[&str] = &["received", "acknowledged", "dismissed", "promoted"];
const ACTIONS: &[&str] = &[
    "acknowledge",
    "acknowledge_duplicate",
    "archive",
    "materialize",
    "review",
    "review_capa_request",
    "triage",
];
const ROLES: &[&str] = &["architect", "builder", "operator"];
const KINDS: &[&str] = &[
    "proposal",
    "observation",
    "command_request",
    "question",
    "knowledge_candidate",
    "task_candidate",
    "incident",
    "upstream_task_candidate",
];

pub fn list_tools() -> Vec<Value> {
    vec![
        guidance_tool(),
        tool(
            "inbox_doctor",
            "Inspect site-local inbox MCP readiness.",
            schema(json!({"properties":{}}), &[]),
            true,
        ),
        tool(
            "inbox_list",
            "List site-local inbox envelopes ordered by actionability.",
            schema(
                json!({"properties":{
                    "status":{"type":"string","enum":STATUSES,"default":"received"},
                    "kind":{"type":"string","enum":KINDS},
                    "target_role":{"type":"string","enum":ROLES},
                    "action":{"type":"string","enum":ACTIONS},
                    "limit":{"type":"integer","minimum":1,"maximum":100,"default":20}
                }}),
                &[],
            ),
            true,
        ),
        tool(
            "inbox_show",
            "Show one site-local inbox envelope by envelope_id.",
            schema(
                json!({"properties":{"envelope_id":{"type":"string","minLength":5,"maxLength":512}}}),
                &["envelope_id"],
            ),
            true,
        ),
        tool(
            "inbox_submit",
            "Submit one site-local inbox envelope and admit it to the local inbox log.",
            schema(
                json!({"properties":{
                    "kind":{"type":"string","enum":KINDS},"title":{"type":"string","minLength":1,"maxLength":1024},"summary":{"type":"string","maxLength":8192},
                    "principal":{"type":"string","minLength":1,"maxLength":512},"target_role":{"type":"string","enum":ROLES},
                    "severity":{"type":"integer","minimum":0,"maximum":100},
                    "authority_level":{"type":"string","enum":["agent_reported","operator_confirmed","operator_directed"],"default":"agent_reported"},
                    "payload":{"type":"object","maxProperties":256,"default":{}},
                    "idempotency_key":{"type":"string","minLength":1,"maxLength":512,"description":"Stable retry key; identical replay returns the original envelope and conflicting reuse is refused."}
                }}),
                &["kind", "title", "principal"],
            ),
            false,
        ),
        tool(
            "inbox_acknowledge",
            "Acknowledge an envelope.",
            schema(
                json!({"properties":{"envelope_id":{"type":"string","minLength":5,"maxLength":512},"principal":{"type":"string","minLength":1,"maxLength":512},"reason":{"type":"string","maxLength":4096}}}),
                &["envelope_id", "principal"],
            ),
            false,
        ),
        tool(
            "inbox_dismiss",
            "Dismiss an envelope.",
            schema(
                json!({"properties":{"envelope_id":{"type":"string","minLength":5,"maxLength":512},"principal":{"type":"string","minLength":1,"maxLength":512},"reason":{"type":"string","minLength":1,"maxLength":4096}}}),
                &["envelope_id", "principal", "reason"],
            ),
            false,
        ),
        tool(
            "inbox_promote_capa",
            "Promote an envelope to CAPA review status.",
            schema(
                json!({"properties":{"envelope_id":{"type":"string","minLength":5,"maxLength":512},"principal":{"type":"string","minLength":1,"maxLength":512},"reason":{"type":"string","maxLength":4096}}}),
                &["envelope_id", "principal"],
            ),
            false,
        ),
        tool(
            "inbox_audit",
            "Read recent admission log entries.",
            schema(
                json!({"properties":{"limit":{"type":"integer","minimum":1,"maximum":200,"default":50},"envelope_id":{"type":"string","minLength":5,"maxLength":512}}}),
                &[],
            ),
            true,
        ),
        tool(
            "inbox_next",
            "Return the next site-local inbox envelope for triage.",
            schema(
                json!({"properties":{"target_role":{"type":"string","enum":ROLES}}}),
                &[],
            ),
            true,
        ),
        tool(
            "capa_queue",
            "List inbox envelopes classified as CAPA review candidates.",
            schema(
                json!({"properties":{"limit":{"type":"integer","minimum":1,"maximum":100,"default":20}}}),
                &[],
            ),
            true,
        ),
        tool(
            "inbox_output_show",
            "Read a materialized Inbox MCP output ref with offset/limit paging.",
            schema(
                json!({"properties":{"ref":{"type":"string","minLength":12,"maxLength":91},"output_ref":{"type":"string","minLength":12,"maxLength":91},"offset":{"type":"integer","minimum":0,"maximum":10000000},"limit":{"type":"integer","minimum":1,"maximum":10000,"default":4000}}}),
                &[],
            ),
            true,
        ),
    ]
}

pub fn auxiliary(method: &str, params: &Map<String, Value>) -> Result<Value, Value> {
    match method {
        "prompts/list" => Ok(
            json!({"prompts":[{"name":"inbox_triage_workflow","title":"Inbox Triage Workflow","description":"Guidance for inbox intake and triage.","arguments":[]}]}),
        ),
        "prompts/get" => {
            if params.get("name").and_then(Value::as_str) != Some("inbox_triage_workflow") {
                return Err(error("unknown_prompt", "unknown_prompt"));
            }
            Ok(
                json!({"description":"Guidance for inbox intake and triage.","messages":[{"role":"user","content":{"type":"text","text":"Use inbox_list or inbox_next to inspect actionable envelopes, then use inbox_show before disposition or follow-up workflows."}}]}),
            )
        }
        "completion/complete" => {
            let is_name = params
                .get("argument")
                .and_then(Value::as_object)
                .and_then(|v| v.get("name"))
                .and_then(Value::as_str)
                == Some("name");
            let values = if is_name {
                list_tools()
                    .iter()
                    .filter_map(|v| v.get("name").cloned())
                    .take(100)
                    .collect::<Vec<_>>()
            } else {
                Vec::new()
            };
            Ok(json!({"completion":{"values":values,"total":values.len(),"hasMore":false}}))
        }
        "logging/setLevel" => Ok(json!({})),
        _ => Err(error(
            "unsupported_mcp_method",
            &format!("unsupported_mcp_method:{method}"),
        )),
    }
}

pub fn call_tool(name: &str, args: &Map<String, Value>, root: &Path) -> Result<Value, Value> {
    match name {
        "inbox_guidance" => Ok(guidance(args)),
        "inbox_doctor" => doctor(root),
        "inbox_list" => list(args, root),
        "inbox_show" => show(args, root),
        "inbox_submit" => submit(args, root),
        "inbox_acknowledge" => disposition(args, root, "acknowledged"),
        "inbox_dismiss" => disposition(args, root, "dismissed"),
        "inbox_promote_capa" => disposition(args, root, "promoted"),
        "inbox_audit" => audit(args, root),
        "inbox_next" => next(args, root),
        "capa_queue" => capa(args, root),
        "inbox_output_show" => output_show(args, root),
        _ => Err(error("unknown_tool", &format!("unknown_tool:{name}"))),
    }
}

fn schema(properties: Value, required: &[&str]) -> Value {
    let mut result = properties.as_object().cloned().unwrap_or_default();
    result.insert("type".into(), Value::String("object".into()));
    result.insert("additionalProperties".into(), Value::Bool(false));
    if !required.is_empty() {
        result.insert("required".into(), json!(required));
    }
    Value::Object(result)
}

fn guidance_tool() -> Value {
    tool(
        "inbox_guidance",
        "Show model-facing operating guidance for site-inbox MCP workflows.",
        schema(
            json!({"properties":{"workflow":{"type":"string","maxLength":256},"tool":{"type":"string","maxLength":256}}}),
            &[],
        ),
        true,
    )
}

fn tool(name: &str, description: &str, schema: Value, read_only: bool) -> Value {
    json!({
        "name": name, "description": description,
        "annotations": {
            "title": name, "readOnlyHint": read_only, "destructiveHint": name == "inbox_dismiss",
            "idempotentHint": name == "inbox_guidance" || name.contains("doctor") || name.contains("list") || name.contains("show") || name.contains("queue") || name.contains("audit"),
            "openWorldHint": false
        },
        "inputSchema": schema, "outputSchema": {"type":"object","additionalProperties":true}
    })
}

fn guidance(args: &Map<String, Value>) -> Value {
    json!({
        "schema":"narada.mcp_surface.guidance.v0","status":"ok","surface_id":"site-inbox",
        "guidance_tool":"inbox_guidance","purpose":"Governed site inbox intake and triage.",
        "requested":{"workflow":optional_string(args,"workflow"),"tool":optional_string(args,"tool")},
        "first_use":[
            "Call this guidance command when the surface is unfamiliar, when a refusal/error is unclear, or before composing a multi-step workflow.",
            "Inspect policy/doctor/status tools before mutation or open-world operations.",
            "Use bounded list/search/query tools for discovery, then show/read/detail tools before acting on a specific object.",
            "Preserve structuredContent as authoritative evidence; text content is for assistant readability."
        ],
        "tool_preference":[
            {"step":"orient","guidance":"Use *_guidance first when uncertain, then policy/doctor/status tools."},
            {"step":"discover","guidance":"Use bounded list/search/query commands with explicit limits and filters."},
            {"step":"inspect","guidance":"Use show/read/detail commands for exact targets before mutation."},
            {"step":"mutate","guidance":"Only call mutation tools after policy allows it and intent, target, and expected result are explicit."},
            {"step":"verify","guidance":"Read back state with the owning surface after any mutation."}
        ],
        "boundaries":[
            "Guidance is read-only model-facing operating advice.",
            "Guidance does not weaken policy, authorize mutation, or replace tool schemas.",
            "The owning MCP surface remains authoritative for state and enforcement."
        ]
    })
}

