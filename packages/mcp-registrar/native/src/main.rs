use narada_mcp_materialization_contract::{
    describe_config, generation_fingerprint, pretty_json, CONTRACT_VERSION,
};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::env;
use std::fs;
use std::io::{self, BufRead, Read, Write};
use std::path::{Path, PathBuf};

const CONTRACT: &[u8] = include_bytes!("../tool-catalog.json.gz");
const MODERN_PROTOCOL_VERSION: &str = "2026-07-28";
const MAX_MESSAGE_BYTES: usize = 4 * 1024 * 1024;
const MAX_HEADER_BYTES: usize = 64 * 1024;

fn main() {
    if let Err(error) = run() {
        eprintln!("{error}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), String> {
    let stdin = io::stdin();
    let mut input = stdin.lock();
    let stdout = io::stdout();
    let mut output = stdout.lock();
    loop {
        let Some(request) = read_message(&mut input)? else {
            break;
        };
        if request
            .get("method")
            .and_then(Value::as_str)
            .is_some_and(|v| v.starts_with("notifications/"))
        {
            continue;
        }
        let response = dispatch(&request);
        let body = serde_json::to_vec(&response).map_err(|e| e.to_string())?;
        write!(output, "Content-Length: {}\r\n\r\n", body.len()).map_err(|e| e.to_string())?;
        output.write_all(&body).map_err(|e| e.to_string())?;
        output.flush().map_err(|e| e.to_string())?;
    }
    Ok(())
}

fn decode_contract() -> Result<Value, String> {
    let mut decoder = flate2::read::GzDecoder::new(CONTRACT);
    let mut bytes = Vec::new();
    decoder
        .read_to_end(&mut bytes)
        .map_err(|error| error.to_string())?;
    serde_json::from_slice(&bytes).map_err(|error| error.to_string())
}

fn dispatch(request: &Value) -> Value {
    let id = request.get("id").cloned().unwrap_or(Value::Null);
    let method = request.get("method").and_then(Value::as_str).unwrap_or("");
    let params = request.get("params").cloned().unwrap_or_else(|| json!({}));
    let protocol_marker = params.pointer("/_meta/io.modelcontextprotocol~1protocolVersion");
    if protocol_marker.is_some_and(|value| value.as_str() != Some(MODERN_PROTOCOL_VERSION)) {
        return error(id, "protocol_version_unsupported".into());
    }
    let modern = protocol_marker.is_some();
    if modern {
        let meta = params.get("_meta").and_then(Value::as_object);
        if meta
            .and_then(|value| value.get("io.modelcontextprotocol/clientInfo"))
            .and_then(Value::as_object)
            .is_none()
            || meta
                .and_then(|value| value.get("io.modelcontextprotocol/clientCapabilities"))
                .and_then(Value::as_object)
                .is_none()
        {
            return error(id, "modern_metadata_required".into());
        }
    }
    if method == "initialize" {
        return error(id, "initialize_removed".into());
    }
    if modern && method == "server/discover" {
        return json!({"jsonrpc":"2.0","id":id,"result":{"resultType":"complete","supportedVersions":[MODERN_PROTOCOL_VERSION],"capabilities":{"tools":{}},"serverInfo":{"name":"mcp-registrar","version":"0.1.0"},"_meta":{"io.modelcontextprotocol/serverInfo":{"name":"mcp-registrar","version":"0.1.0"}}}});
    }
    let mut contract: Value = match decode_contract() {
        Ok(v) => v,
        Err(e) => return error(id, format!("mcp_registrar_native_contract_invalid:{e}")),
    };
    extend_epistemic_catalog(&mut contract);
    align_native_surface_descriptor_schemas(&mut contract);
    if let Err(e) = validate_contract(&contract) {
        return error(id, format!("mcp_registrar_native_contract_invalid:{e}"));
    }
    if let Err(e) = rebind_native_registrar(&mut contract) {
        return error(id, format!("mcp_registrar_native_contract_invalid:{e}"));
    }
    normalize_tool_schemas(&mut contract);
    if method == "tools/call" {
        if let Err(message) = validate_tool_call(&contract, &params) {
            return error(id, message);
        }
    }
    let mut response = match method {
        "tools/list" => json!({"jsonrpc":"2.0","id":id,"result":{"tools":contract["tools"]}}),
        "tools/call" => {
            let name = request
                .pointer("/params/name")
                .and_then(Value::as_str)
                .unwrap_or("");
            if matches!(
                name,
                "registrar_guidance"
                    | "registrar_surface_list"
                    | "registrar_carrier_list"
                    | "registrar_site_list"
            ) {
                let mut guidance = if name == "registrar_guidance" {
                    contract["guidance"].clone()
                } else if name == "registrar_surface_list" {
                    let args = request
                        .pointer("/params/arguments")
                        .cloned()
                        .unwrap_or_else(|| json!({}));
                    surface_list(&contract, &args)
                } else if name == "registrar_site_list" {
                    let args = request
                        .pointer("/params/arguments")
                        .cloned()
                        .unwrap_or_else(|| json!({}));
                    site_list(&contract, &args)
                } else if name == "registrar_carrier_list" {
                    let args = request
                        .pointer("/params/arguments")
                        .cloned()
                        .unwrap_or_else(|| json!({}));
                    carrier_list(&contract, &args)
                } else {
                    contract["read_models"][name].clone()
                };
                let args = request
                    .pointer("/params/arguments")
                    .cloned()
                    .unwrap_or_else(|| json!({}));
                if name == "registrar_guidance" {
                    guidance.as_object_mut().unwrap().insert("requested".into(),json!({"workflow":args.get("workflow").and_then(Value::as_str).filter(|v|!v.trim().is_empty()).map(str::trim),"tool":args.get("tool").and_then(Value::as_str).filter(|v|!v.trim().is_empty()).map(str::trim)}));
                }
                json!({"jsonrpc":"2.0","id":id,"result":{"content":[{"type":"text","text":serde_json::to_string_pretty(&guidance).unwrap()}],"structuredContent":guidance}})
            } else if name == "registrar_surface_tool_inventory_check" {
                let args = request
                    .pointer("/params/arguments")
                    .cloned()
                    .unwrap_or_else(|| json!({}));
                let result = surface_tool_inventory(&contract, &args);
                json!({"jsonrpc":"2.0","id":id,"result":{"content":[{"type":"text","text":serde_json::to_string_pretty(&result).unwrap()}],"structuredContent":result}})
            } else if name == "registrar_site_surfaces" {
                let args = request
                    .pointer("/params/arguments")
                    .cloned()
                    .unwrap_or_else(|| json!({}));
                match site_surfaces(&contract, &args) {
                    Ok(result) => {
                        json!({"jsonrpc":"2.0","id":id,"result":{"content":[{"type":"text","text":serde_json::to_string_pretty(&result).unwrap()}],"structuredContent":result}})
                    }
                    Err(message) => error(id, message),
                }
            } else if name == "registrar_site_output_reader_closure_check" {
                let args = request
                    .pointer("/params/arguments")
                    .cloned()
                    .unwrap_or_else(|| json!({}));
                match site_output_reader_closure_check(&contract, &args) {
                    Ok(result) => {
                        json!({"jsonrpc":"2.0","id":id,"result":{"content":[{"type":"text","text":serde_json::to_string_pretty(&result).unwrap()}],"structuredContent":result}})
                    }
                    Err(message) => error(id, message),
                }
            } else if name == "registrar_surface_usage" {
                let args = request
                    .pointer("/params/arguments")
                    .cloned()
                    .unwrap_or_else(|| json!({}));
                match surface_usage(&contract, &args) {
                    Ok(result) => {
                        json!({"jsonrpc":"2.0","id":id,"result":{"content":[{"type":"text","text":serde_json::to_string_pretty(&result).unwrap()}],"structuredContent":result}})
                    }
                    Err(message) => error(id, message),
                }
            } else if name == "registrar_site_surface_registry_sync" {
                let args = request
                    .pointer("/params/arguments")
                    .cloned()
                    .unwrap_or_else(|| json!({}));
                match site_surface_registry_sync(&contract, &args) {
                    Ok(result) => {
                        json!({"jsonrpc":"2.0","id":id,"result":{"content":[{"type":"text","text":serde_json::to_string_pretty(&result).unwrap()}],"structuredContent":result}})
                    }
                    Err(message) => error(id, message),
                }
            } else if name == "registrar_site_bind" || name == "registrar_site_unbind" {
                let args = request
                    .pointer("/params/arguments")
                    .cloned()
                    .unwrap_or_else(|| json!({}));
                let result = if name == "registrar_site_bind" {
                    site_bind(&contract, &args)
                } else {
                    site_unbind(&contract, &args)
                };
                match result {
                    Ok(value) => {
                        json!({"jsonrpc":"2.0","id":id,"result":{"content":[{"type":"text","text":serde_json::to_string_pretty(&value).unwrap()}],"structuredContent":value}})
                    }
                    Err(message) => error(id, message),
                }
            } else if name == "registrar_site_mcp_fabric_validate" {
                let args = request
                    .pointer("/params/arguments")
                    .cloned()
                    .unwrap_or_else(|| json!({}));
                match site_mcp_fabric_validate(&contract, &args) {
                    Ok(value) => {
                        json!({"jsonrpc":"2.0","id":id,"result":{"content":[{"type":"text","text":serde_json::to_string_pretty(&value).unwrap()}],"structuredContent":value}})
                    }
                    Err(message) => error(id, message),
                }
            } else if name == "registrar_carrier_validate" {
                let args = request
                    .pointer("/params/arguments")
                    .cloned()
                    .unwrap_or_else(|| json!({}));
                match carrier_validate(&contract, &args) {
                    Ok(value) => {
                        json!({"jsonrpc":"2.0","id":id,"result":{"content":[{"type":"text","text":serde_json::to_string_pretty(&value).unwrap()}],"structuredContent":value}})
                    }
                    Err(message) => error(id, message),
                }
            } else if name == "registrar_carrier_diff" {
                let args = request
                    .pointer("/params/arguments")
                    .cloned()
                    .unwrap_or_else(|| json!({}));
                match carrier_diff(&contract, &args) {
                    Ok(value) => {
                        json!({"jsonrpc":"2.0","id":id,"result":{"content":[{"type":"text","text":serde_json::to_string_pretty(&value).unwrap()}],"structuredContent":value}})
                    }
                    Err(message) => error(id, message),
                }
            } else if name == "registrar_site_registry_conformance_check" {
                let args = request
                    .pointer("/params/arguments")
                    .cloned()
                    .unwrap_or_else(|| json!({}));
                match site_registry_conformance_check(&contract, &args) {
                    Ok(value) => {
                        json!({"jsonrpc":"2.0","id":id,"result":{"content":[{"type":"text","text":serde_json::to_string_pretty(&value).unwrap()}],"structuredContent":value}})
                    }
                    Err(message) => error(id, message),
                }
            } else if name == "registrar_carrier_bind" || name == "registrar_carrier_unbind" {
                let args = request
                    .pointer("/params/arguments")
                    .cloned()
                    .unwrap_or_else(|| json!({}));
                let result = if name == "registrar_carrier_bind" {
                    carrier_bind(&contract, &args)
                } else {
                    carrier_unbind(&contract, &args)
                };
                match result {
                    Ok(value) => {
                        json!({"jsonrpc":"2.0","id":id,"result":{"content":[{"type":"text","text":serde_json::to_string_pretty(&value).unwrap()}],"structuredContent":value}})
                    }
                    Err(failure) => carrier_mutation_error(id, failure),
                }
            } else if name == "registrar_sync" {
                let args = request
                    .pointer("/params/arguments")
                    .cloned()
                    .unwrap_or_else(|| json!({}));
                match registrar_sync(&contract, &args) {
                    Ok(value) => {
                        json!({"jsonrpc":"2.0","id":id,"result":{"content":[{"type":"text","text":serde_json::to_string_pretty(&value).unwrap()}],"structuredContent":value}})
                    }
                    Err(message) => sync_error(id, message),
                }
            } else {
                error(
                    id,
                    format!("mcp_registrar_native_tool_not_implemented:{name}"),
                )
            }
        }
        method => error(id, format!("unsupported_mcp_method:{method}")),
    };
    if modern {
        if let Some(result) = response.get_mut("result").and_then(Value::as_object_mut) {
            result.insert("resultType".into(), json!("complete"));
            result.insert("cacheScope".into(), json!("private"));
            result.insert("ttlMs".into(), json!(0));
        }
    }
    response
}

fn extend_epistemic_catalog(contract: &mut Value) {
    let tools = [
        ("epistemic_graph_guidance", true),
        ("epistemic_graph_status", true),
        ("epistemic_graph_query", true),
        ("epistemic_graph_query_batch", true),
        ("epistemic_graph_source_inspect", true),
        ("epistemic_graph_neighborhood", true),
        ("epistemic_graph_proposal_submit", false),
        ("epistemic_graph_submit_review_admit", false),
        ("epistemic_graph_capture_sources", false),
        ("epistemic_graph_proposal_read", true),
        ("epistemic_graph_proposal_resubmit", false),
        ("epistemic_graph_proposal_review", false),
        ("epistemic_graph_proposal_admit", false),
        ("epistemic_graph_proposal_reject", false),
        ("epistemic_graph_export", true),
    ];
    let descriptor_tools = tools
        .iter()
        .map(|(name, read_only)| {
            json!({
                "name":name,
                "description":format!("Native epistemic graph operation: {name}."),
                "input_schema":{"type":"object","additionalProperties":true},
                "output_schema":{"type":"object","additionalProperties":true},
                "annotations":{"title":name,"readOnlyHint":read_only,"destructiveHint":false,"idempotentHint":read_only,"openWorldHint":false},
                "effect":{"class":if *read_only{"read"}else{"local_write"},"idempotency":if *read_only{"replayable"}else{"idempotent_with_key"},"confirmation":"policy"}
            })
        })
        .collect::<Vec<_>>();
    let projection = json!({
        "id":"default","transport":{"kind":"stdio","command":"narada-mcp-surfaces","args":["--surface-id","epistemic-graph","--site-root","{site_root}"],"env":[]},
        "injection_scope":"local_site","default_injection":"enabled","runtime_requirements":[],"authority_requirements":["scope.local_site"],
        "lifecycle":{"mode":"replayable","reason":"Canonical events are immutable and the query projection is rebuildable."}
    });
    let descriptor = json!({
        "schema_version":"2.0","source":"native","surface_id":"epistemic-graph","surface_version":"0.1.0",
        "package":"@narada-core/mcp-surfaces-native","guidance_tool":"epistemic_graph_guidance","tools":descriptor_tools,
        "projections":[projection.clone()],"metadata":{"authority":"tracked_event_ledger","truth_certification":false}
    });
    let descriptor_digest = sha256_text(&canonical_json(&descriptor));
    let tool_contract_digest = sha256_text(&canonical_json(&descriptor["tools"]));
    let names = tools
        .iter()
        .map(|(name, _)| json!(name))
        .collect::<Vec<_>>();
    let item = json!({
        "id":"epistemic-graph","package":"mcp-surfaces-native","entrypoint":"{mcp_surfaces_root}/shared/mcp-surfaces-native/dist/native/narada-mcp-surfaces.exe",
        "kind":"mcp_surface","args":["--surface-id","epistemic-graph","--site-root","{site_root}"],"tools":names,
        "projections":[{"id":"default","injection_scope":"local_site","execution":{"adapter":"stdio","tenancy":"session_isolated","replacement":"manual"},"restart_owner":"local_site","runtime_requirements":[],"env_vars":[],"command":"narada-mcp-surfaces","entrypoint":"{mcp_surfaces_root}/shared/mcp-surfaces-native/dist/native/narada-mcp-surfaces.exe","args":["--surface-id","epistemic-graph","--site-root","{site_root}"]}],
        "injection_scope":"local_site","restart_owner":"local_site","env_vars":[],"descriptor_source":"native","descriptor_digest":descriptor_digest,"tool_contract_digest":tool_contract_digest,"descriptor":descriptor,
        "authority_locus":{"kind":"local_site"},"mutation_locus":{"kind":"local_site"},
        "narada_scope":{"injection_scope":"local_site","authority_locus":{"kind":"local_site"},"mutation_locus":{"kind":"local_site"},"restart_owner":"local_site","scope_source":"registrar_surface_catalog"}
    });
    let count = if let Some(items) = contract
        .pointer_mut("/read_models/registrar_surface_list/items")
        .and_then(Value::as_array_mut)
    {
        if !items
            .iter()
            .any(|candidate| candidate["id"] == "epistemic-graph")
        {
            items.push(item);
        }
        Some(items.len())
    } else {
        None
    };
    if let (Some(count), Some(slot)) = (
        count,
        contract.pointer_mut("/read_models/registrar_surface_list/count"),
    ) {
        *slot = json!(count);
    }
}

fn align_native_surface_descriptor_schemas(contract: &mut Value) {
    let Some(items) = contract.pointer_mut("/read_models/registrar_surface_list/items").and_then(Value::as_array_mut) else { return; };
    let intent = || json!({"type":"object","properties":{"instruction":{"type":"string","minLength":1,"maxLength":65536},"task":{"type":"string","minLength":1,"maxLength":65536},"goal":{"type":"string","minLength":1,"maxLength":65536},"summary":{"type":"string","minLength":1,"maxLength":65536},"mode":{"type":"string","maxLength":256}},"additionalProperties":false,"anyOf":[{"required":["instruction"]},{"required":["task"]},{"required":["goal"]},{"required":["summary"]}]});
    let constraints = || json!({"type":"object","properties":{"authority":{"type":"string","enum":["read","write","command"]},"cognition":{"type":"string","enum":["low","medium","high"]},"cwd":{"type":"string","minLength":1,"maxLength":4096},"invocation_plan_ref":{"type":"string","minLength":6,"maxLength":512,"pattern":"^plan:[A-Za-z0-9._:-]+$"},"max_run_ms":{"type":"integer","minimum":1,"maximum":1800000,"default":300000,"description":"Hard worker runtime deadline enforced by the native authority."},"wait_for_completion":{"type":"boolean","default":false,"description":"Return after bounded child completion polling when true; false returns the accepted running record immediately."},"wait_timeout_ms":{"type":"integer","minimum":0,"maximum":300000,"default":30000,"description":"Maximum inline completion wait when wait_for_completion is true."}},"additionalProperties":false});
    for item in items {
        let id = item.get("id").and_then(Value::as_str).unwrap_or_default().to_owned();
        let Some(tools) = item.pointer_mut("/descriptor/tools").and_then(Value::as_array_mut) else { continue; };
        for tool in tools {
            let name = tool.get("name").and_then(Value::as_str).unwrap_or_default();
            let schema = match (id.as_str(), name) {
                ("epistemic-graph", "epistemic_graph_guidance") => Some(json!({"type":"object","properties":{"workflow":{"type":"string","maxLength":256},"tool":{"type":"string","maxLength":256}},"additionalProperties":false})),
                ("worker-delegation", "worker_run") => Some(json!({"type":"object","properties":{"intent":intent(),"constraints":constraints()},"required":["intent"],"additionalProperties":false})),
                ("worker-delegation", "worker_config_resolve") => Some(json!({"type":"object","properties":{"cwd":{"type":"string","minLength":1,"maxLength":4096},"constraints":constraints()},"additionalProperties":false})),
                _ => None,
            };
            if let Some(schema) = schema { tool["input_schema"] = schema; }
        }
    }
}

fn validate_contract(contract: &Value) -> Result<(), String> {
    if contract["schema"] != "narada.mcp_registrar.native_tool_catalog.v1" {
        return Err("unsupported_schema".into());
    }
    validate_unique_records(contract["tools"].as_array(), "name", "tools")?;
    validate_unique_records(
        contract
            .pointer("/read_models/registrar_surface_list/items")
            .and_then(Value::as_array),
        "id",
        "surfaces",
    )?;
    validate_unique_records(
        contract
            .pointer("/read_models/registrar_carrier_list/items")
            .and_then(Value::as_array),
        "carrier_id",
        "carriers",
    )?;
    Ok(())
}

fn validate_unique_records(
    items: Option<&Vec<Value>>,
    key: &str,
    label: &str,
) -> Result<(), String> {
    let items = items
        .filter(|items| !items.is_empty())
        .ok_or_else(|| format!("{label}_missing"))?;
    let mut seen = std::collections::BTreeSet::new();
    for item in items {
        let value = item
            .get(key)
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| format!("{label}_{key}_missing"))?;
        if !seen.insert(value) {
            return Err(format!("{label}_{key}_duplicate:{value}"));
        }
    }
    Ok(())
}

fn carrier_record<'a>(contract: &'a Value, carrier_id: &str) -> Result<&'a Value, String> {
    contract
        .pointer("/read_models/registrar_carrier_list/items")
        .and_then(Value::as_array)
        .and_then(|items| {
            items
                .iter()
                .find(|carrier| carrier["carrier_id"] == carrier_id)
        })
        .ok_or_else(|| format!("registrar_unknown_carrier:{carrier_id}"))
}
fn ensure_surface<'a>(contract: &'a Value, surface_id: &str) -> Result<&'a Value, String> {
    contract
        .pointer("/read_models/registrar_surface_list/items")
        .and_then(Value::as_array)
        .and_then(|items| items.iter().find(|surface| surface["id"] == surface_id))
        .ok_or_else(|| format!("registrar_unknown_surface:{surface_id}"))
}
fn carrier_surface_keys(contract: &Value, carrier_id: &str, surface_id: &str) -> Vec<String> {
    contract
        .pointer(&format!(
            "/read_models/registrar_carrier_validation_plans/{carrier_id}/servers"
        ))
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|server| server["surface_id"] == surface_id)
        .filter_map(|server| server["server_key"].as_str().map(str::to_string))
        .collect()
}
struct MutationFailure {
    code: String,
    message: String,
    details: Value,
}
fn mutation_failure(code: &str, message: String, details: Value) -> MutationFailure {
    MutationFailure {
        code: code.into(),
        message,
        details,
    }
}
fn carrier_mutation_error(id: Value, failure: MutationFailure) -> Value {
    let child_data = json!({"schema":"narada.registrar.error.v1","code":failure.code,"message":failure.message,"details":failure.details});
    let child_error = json!({"code":-32000,"message":failure.message,"data":child_data});
    let entrypoint = env::current_exe()
        .map(|path| path_text(&path).replace('\\', "/"))
        .unwrap_or_else(|_| "narada-mcp-registrar".into());
    json!({"jsonrpc":"2.0","id":id,"error":{"code":-32000,"message":failure.message,"data":{"schema":"narada.registrar.error.v1","code":"registrar_fresh_materialization_failed","message":failure.message,"details":{"entrypoint":entrypoint,"stderr_tail":"","exit_code":0,"signal":null,"child_error":child_error}}}})
}
fn sync_error(id: Value, message: String) -> Value {
    if let Some(suffix) = message.strip_prefix("registrar_progressive_bulk_bind_refused:") {
        let remediation = if suffix == "all_carriers" {
            "Progressive carriers expose only their explicit bootstrap allowlists; use mcp-loader for runtime attachment or switch the bindings to static loading."
        } else {
            "Progressive carriers expose only their explicit bootstrap allowlist; use mcp-loader for runtime attachment or switch the binding to static loading."
        };
        let mut details = json!({"remediation":remediation});
        if suffix != "all_carriers" {
            details
                .as_object_mut()
                .unwrap()
                .insert("carrier_id".into(), json!(suffix));
        }
        return json!({"jsonrpc":"2.0","id":id,"error":{"code":-32000,"message":message,"data":{"schema":"narada.registrar.error.v1","code":"registrar_progressive_bulk_bind_refused","message":message,"details":details}}});
    }
    error(id, message)
}
fn carrier_bind(contract: &Value, args: &Value) -> Result<Value, MutationFailure> {
    let carrier_id = required_argument(args, "carrier_id", "registrar_requires_carrier_id")
        .map_err(|message| mutation_failure("registrar_requires_carrier_id", message, json!({})))?;
    let surface_id = required_argument(args, "surface_id", "registrar_requires_surface_id")
        .map_err(|message| mutation_failure("registrar_requires_surface_id", message, json!({})))?;
    let carrier = carrier_record(contract, &carrier_id)
        .map_err(|message| mutation_failure("registrar_unknown_carrier", message, json!({})))?;
    let surface = ensure_surface(contract, &surface_id)
        .map_err(|message| mutation_failure("registrar_unknown_surface", message, json!({})))?;
    if let Some(projection_id) = args
        .get("projection_id")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
    {
        if !surface["projections"].as_array().is_some_and(|items| {
            items
                .iter()
                .any(|projection| projection["id"] == projection_id)
        }) {
            return Err(mutation_failure(
                "registrar_unknown_surface_projection",
                format!("registrar_unknown_surface_projection:{surface_id}:{projection_id}"),
                json!({"surface_id":surface_id,"projection_id":projection_id}),
            ));
        }
    }
    let site_id = args
        .get("site_id")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .unwrap_or("andrey-user");
    let sites = site_catalog(contract)["items"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let site = lookup_site_value(&sites,site_id).map_err(|message|mutation_failure("registrar_unknown_site",message,json!({"known":sites.iter().filter_map(|site|site["site_id"].as_str()).collect::<Vec<_>>() })))?;
    let site_declares_surface = site["surfaces"]
        .as_array()
        .into_iter()
        .flatten()
        .any(|value| value == &surface_id)
        || site_fabric_surface_ids(contract, &site)
            .iter()
            .any(|value| value == &surface_id)
        || site_local_surface_ids(contract, &site)
            .iter()
            .any(|value| value == &surface_id);
    let binding = carrier["site_bindings"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|binding| binding["site_id"] == site_id);
    if binding.is_none() {
        let next_route = if site_declares_surface {
            "mcp-loader"
        } else {
            "registrar_site_bind"
        };
        let remediation = if site_declares_surface {
            "The surface is declared by the Site but this carrier has no Site binding for it. Use mcp-loader with the Site root for runtime attachment; do not add a carrier binding for a site-scoped surface."
        } else {
            "The requested Site does not declare this surface. Bind the surface to the Site first, then use mcp-loader for a site-scoped runtime attachment or add it to the native carrier contract for static materialization."
        };
        return Err(mutation_failure(
            "registrar_carrier_site_binding_missing",
            format!("registrar_carrier_site_binding_missing:{carrier_id}:{site_id}:{surface_id}"),
            json!({
                "carrier_id":carrier_id,
                "site_id":site_id,
                "surface_id":surface_id,
                "carrier_site_binding":"absent",
                "site_surface_declared":site_declares_surface,
                "site_root":site["root"],
                "next_route":next_route,
                "remediation":remediation
            }),
        ));
    }
    let keys = carrier_surface_keys(contract, &carrier_id, &surface_id);
    if binding.is_some_and(|value| value["loading_mode"] == "progressive") && keys.is_empty() {
        return Err(mutation_failure(
            "registrar_progressive_surface_bind_refused",
            format!("registrar_progressive_surface_bind_refused:{carrier_id}:{surface_id}"),
            json!({"carrier_id":carrier_id,"site_id":site_id,"surface_id":surface_id,"loading_mode":"progressive","remediation":"Use mcp-loader to attach this surface at runtime, or explicitly add it to the progressive bootstrap allowlist before materializing the carrier."}),
        ));
    }
    if !keys.is_empty() {
        return Err(mutation_failure(
            "registrar_carrier_config_owned_by_native_materializer",
            format!(
                "registrar_carrier_config_owned_by_native_materializer:{carrier_id}:{surface_id}"
            ),
            json!({"carrier_id":carrier_id,"surface_id":surface_id,"server_keys":keys,"remediation":"Edit the external native carrier contract or the owning Site registry, then run cargo native-materialize."}),
        ));
    }
    Err(mutation_failure(
        "registrar_carrier_bind_requires_native_materializer",
        format!("registrar_carrier_bind_requires_native_materializer:{carrier_id}:{site_id}:{surface_id}"),
        json!({
            "carrier_id":carrier_id,
            "site_id":site_id,
            "surface_id":surface_id,
            "route":"native-materializer",
            "remediation":"Change the owning native carrier contract or Site registry, then run cargo native-materialize; this registrar does not emit single-carrier configuration."
        }),
    ))
}
fn carrier_unbind(contract: &Value, args: &Value) -> Result<Value, MutationFailure> {
    let carrier_id = required_argument(args, "carrier_id", "registrar_requires_carrier_id")
        .map_err(|message| mutation_failure("registrar_requires_carrier_id", message, json!({})))?;
    let surface_id = required_argument(args, "surface_id", "registrar_requires_surface_id")
        .map_err(|message| mutation_failure("registrar_requires_surface_id", message, json!({})))?;
    let carrier = carrier_record(contract, &carrier_id)
        .map_err(|message| mutation_failure("registrar_unknown_carrier", message, json!({})))?;
    let keys = carrier_surface_keys(contract, &carrier_id, &surface_id);
    if !keys.is_empty() {
        return Err(mutation_failure(
            "registrar_carrier_unbind_refused_aggregate_surface",
            format!("registrar_carrier_unbind_refused_aggregate_surface:{surface_id}"),
            json!({"carrier_id":carrier_id,"surface_id":surface_id,"server_keys":keys,"remediation":"This surface is produced by the external native carrier contract. Remove it from that contract or the owning Site registry, then run cargo native-materialize."}),
        ));
    }
    let kind = carrier["kind"].as_str().unwrap_or("");
    if kind == "opencode" {
        return Err(mutation_failure(
            "registrar_single_surface_unbind_unsupported_for_opencode_aggregate",
            "registrar_single_surface_unbind_unsupported_for_opencode_aggregate".into(),
            json!({}),
        ));
    }
    let declared_path = carrier["config_path"].as_str().unwrap_or("");
    let path_value = effective_carrier_config_path(kind, declared_path);
    let path = path_value.as_str();
    let content = fs::read_to_string(path).map_err(|_| {
        mutation_failure(
            "registrar_config_not_found",
            format!("registrar_config_not_found:{path}"),
            json!({}),
        )
    })?;
    let bound = if kind == "kimi" {
        parse_jsonc(&content)
            .and_then(|value| value["mcpServers"].as_object().cloned())
            .is_some_and(|servers| {
                servers.contains_key(&format!("narada-site-andrey-user-{surface_id}"))
            })
    } else {
        content.contains(&format!("[mcp_servers.{surface_id}]"))
    };
    if !bound {
        return Ok(json!({"status":"not_bound","carrier_id":carrier_id,"surface_id":surface_id}));
    }
    let (next_content, server_key) = if kind == "kimi" {
        let mut parsed = parse_jsonc(&content).unwrap();
        let key = format!("narada-site-andrey-user-{surface_id}");
        parsed["mcpServers"].as_object_mut().unwrap().remove(&key);
        (
            String::from_utf8(pretty_json(&parsed).map_err(|error| {
                mutation_failure(
                    "registrar_json_emit_failed",
                    error,
                    json!({"config_path":path}),
                )
            })?)
            .map_err(|error| {
                mutation_failure(
                    "registrar_json_emit_failed",
                    error.to_string(),
                    json!({"config_path":path}),
                )
            })?,
            key,
        )
    } else {
        let section = format!("[mcp_servers.{surface_id}]");
        let index = content.find(&section).unwrap();
        let next = content[index + section.len()..]
            .find("\n[")
            .map(|offset| index + section.len() + offset);
        (
            if let Some(next) = next {
                format!("{}{}", &content[..index], &content[next..])
            } else {
                content[..index].trim_end().to_string()
            },
            surface_id.clone(),
        )
    };
    let template=contract.pointer(&format!("/read_models/registrar_carrier_projection_plans/{carrier_id}/recovery_unbind/{surface_id}")).ok_or_else(||mutation_failure("registrar_native_carrier_unbind_template_missing",format!("registrar_native_carrier_unbind_template_missing:{carrier_id}:{surface_id}"),json!({})))?;
    let mut runtime_plan = template["runtime_materialization_plan"].clone();
    let validation = template["materialization_validation"].clone();
    let mut generation = template["generation_unsigned"].clone();
    let current_sidecar_path = format!("{path}.narada-generation.json");
    let current_generation: Value =
        serde_json::from_slice(&fs::read(&current_sidecar_path).map_err(|error| {
            mutation_failure(
                "registrar_generation_sidecar_read_failed",
                error.to_string(),
                json!({"path":current_sidecar_path}),
            )
        })?)
        .map_err(|error| {
            mutation_failure(
                "registrar_generation_sidecar_invalid",
                error.to_string(),
                json!({"path":current_sidecar_path}),
            )
        })?;
    let artifact_manifest_fingerprint = current_generation
        .get("artifact_manifest_fingerprint")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            mutation_failure(
                "registrar_artifact_manifest_fingerprint_missing",
                "registrar_artifact_manifest_fingerprint_missing".into(),
                json!({"path":current_sidecar_path}),
            )
        })?;
    generation.as_object_mut().unwrap().insert(
        "artifact_manifest_fingerprint".into(),
        json!(artifact_manifest_fingerprint),
    );
    if path != declared_path {
        replace_value_string(&mut runtime_plan, declared_path, path);
        replace_value_string(&mut generation, declared_path, path);
    }
    let object = runtime_plan.as_object_mut().unwrap();
    object.remove("plan_fingerprint");
    let fingerprint = sha256_text(&serde_json::to_string(object).unwrap());
    object.insert("plan_fingerprint".into(), json!(fingerprint.clone()));
    generation.as_object_mut().unwrap().insert(
        "runtime_materialization_plan_fingerprint".into(),
        json!(fingerprint),
    );
    let selectors = generation
        .pointer("/managed_projection/selectors")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            mutation_failure(
                "registrar_managed_selectors_missing",
                "registrar_managed_selectors_missing".into(),
                json!({"config_path":path}),
            )
        })?
        .iter()
        .map(|value| {
            value.as_str().map(str::to_string).ok_or_else(|| {
                mutation_failure(
                    "registrar_managed_selector_invalid",
                    "registrar_managed_selector_invalid".into(),
                    json!({"config_path":path}),
                )
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    let description =
        describe_config(kind, next_content.as_bytes(), &selectors).map_err(|error| {
            mutation_failure(
                "registrar_materialization_contract_failed",
                format!("registrar_materialization_contract_failed:{error}"),
                json!({"config_path":path}),
            )
        })?;
    generation.as_object_mut().unwrap().insert(
        "config_artifact".into(),
        serde_json::to_value(description.config_artifact).map_err(|error| {
            mutation_failure(
                "registrar_materialization_contract_failed",
                error.to_string(),
                json!({"config_path":path}),
            )
        })?,
    );
    generation.as_object_mut().unwrap().insert(
        "managed_projection".into(),
        serde_json::to_value(description.managed_projection).map_err(|error| {
            mutation_failure(
                "registrar_materialization_contract_failed",
                error.to_string(),
                json!({"config_path":path}),
            )
        })?,
    );
    let generated_at = time::OffsetDateTime::now_utc()
        .format(&time::macros::format_description!(
            "[year]-[month]-[day]T[hour]:[minute]:[second].[subsecond digits:3]Z"
        ))
        .map_err(|error| {
            mutation_failure("registrar_clock_failed", error.to_string(), json!({}))
        })?;
    generation
        .as_object_mut()
        .unwrap()
        .insert("generated_at".into(), json!(generated_at));
    let fingerprint = generation_fingerprint(&generation).map_err(|error| {
        mutation_failure(
            "registrar_generation_fingerprint_failed",
            format!("registrar_generation_fingerprint_failed:{error}"),
            json!({"config_path":path}),
        )
    })?;
    generation
        .as_object_mut()
        .unwrap()
        .insert("generation_fingerprint".into(), json!(fingerprint));
    fs::write(path, &next_content).map_err(|error| {
        mutation_failure(
            "registrar_config_write_failed",
            error.to_string(),
            json!({"config_path":path}),
        )
    })?;
    let plan_path = format!("{path}.narada-runtime-plan.json");
    let sidecar_path = format!("{path}.narada-generation.json");
    write_pretty_json(&plan_path, &runtime_plan).map_err(|message| {
        mutation_failure(
            "registrar_runtime_plan_write_failed",
            message,
            json!({"path":plan_path}),
        )
    })?;
    write_pretty_json(&sidecar_path, &generation).map_err(|message| {
        mutation_failure(
            "registrar_generation_write_failed",
            message,
            json!({"path":sidecar_path}),
        )
    })?;
    Ok(
        json!({"status":"unbound","carrier_id":carrier_id,"surface_id":surface_id,"server_key":server_key,"runtime_contract_version":CONTRACT_VERSION,"materialization_validation":validation,"materialization_generation":generation,"generation_sidecar_path":sidecar_path,"runtime_materialization_plan":runtime_plan,"runtime_materialization_plan_path":plan_path,"recovery_escape_hatch":true}),
    )
}
fn write_pretty_json(path: &str, value: &Value) -> Result<(), String> {
    let target = PathBuf::from(path);
    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent).map_err(|error| error.to_string())?
    }
    let temporary = PathBuf::from(format!("{path}.tmp-{}", std::process::id()));
    fs::write(
        &temporary,
        serde_json::to_string_pretty(value).map_err(|error| error.to_string())? + "\n",
    )
    .map_err(|error| error.to_string())?;
    fs::rename(temporary, target).map_err(|error| error.to_string())
}
fn effective_carrier_config_path(kind: &str, fallback: &str) -> String {
    let name = match kind {
        "opencode" => "NARADA_OPENCODE_CONFIG_PATH",
        "kimi" => "NARADA_KIMI_CONFIG_PATH",
        "codex" => "NARADA_CODEX_CONFIG_PATH",
        _ => "",
    };
    env::var(name)
        .ok()
        .filter(|value| !value.trim().is_empty())
        .map(|value| path_text(&canonical_root(PathBuf::from(value))))
        .unwrap_or_else(|| fallback.into())
}
fn replace_value_string(value: &mut Value, old: &str, new: &str) {
    match value {
        Value::String(text) => *text = text.replace(old, new),
        Value::Array(items) => {
            for item in items {
                replace_value_string(item, old, new)
            }
        }
        Value::Object(items) => {
            for item in items.values_mut() {
                replace_value_string(item, old, new)
            }
        }
        _ => {}
    }
}
fn rebind_native_registrar(contract: &mut Value) -> Result<(), String> {
    let declared = contract
        .pointer("/runtime_bindings/registrar_entrypoint")
        .and_then(Value::as_str)
        .ok_or("native_registrar_binding_missing")?
        .to_string();
    let current = native_artifact_entrypoint(
        "mcp-registrar",
        if cfg!(windows) {
            "narada-mcp-registrar.exe"
        } else {
            "narada-mcp-registrar"
        },
    )
    .ok_or("native_registrar_artifact_unavailable")?;
    repair_native_contract(contract, &declared, &current);
    Ok(())
}

fn repair_native_contract(contract: &mut Value, declared: &str, current: &str) {
    if declared != current {
        replace_value_string(contract, declared, current);
        replace_value_string(
            contract,
            &declared.replace('/', "\\"),
            &current.replace('/', "\\"),
        );
    }
    replace_value_string(
        &mut contract["guidance"],
        "pnpm materialize:carrier",
        "cargo native-release",
    );
    if let Some(tools) = contract.get_mut("tools").and_then(Value::as_array_mut) {
        if let Some(tool) = tools
            .iter_mut()
            .find(|tool| tool["name"] == "registrar_surface_list")
        {
            tool["inputSchema"] = json!({"type":"object","properties":{"compact":{"type":"boolean","default":true,"description":"Return identity and summary fields; set false for full descriptors."},"limit":{"type":"integer","minimum":1,"maximum":200,"default":50},"offset":{"type":"integer","minimum":0,"maximum":10000,"default":0}},"additionalProperties":false});
        }
        for name in ["registrar_site_list", "registrar_carrier_list"] {
            if let Some(tool) = tools.iter_mut().find(|tool| tool["name"] == name) {
                tool["inputSchema"] = json!({"type":"object","properties":{"compact":{"type":"boolean","default":true,"description":"Return identity and summary fields; set false for full records."},"limit":{"type":"integer","minimum":1,"maximum":100,"default":20},"offset":{"type":"integer","minimum":0,"maximum":10000,"default":0}},"additionalProperties":false});
            }
        }
        if let Some(tool) = tools
            .iter_mut()
            .find(|tool| tool["name"] == "registrar_site_surface_registry_sync")
        {
            tool["inputSchema"]["properties"]["include_registry"] = json!({"type":"boolean","default":false,"description":"Include the complete generated registry in a dry-run response; the default returns only its bounded summary."});
        }
    }
    if let Some(plans) = contract
        .pointer_mut("/read_models/registrar_carrier_validation_plans")
        .and_then(Value::as_object_mut)
    {
        for plan in plans.values_mut() {
            for server in plan
                .get_mut("servers")
                .and_then(Value::as_array_mut)
                .into_iter()
                .flatten()
            {
                if server["surface_id"] == "mcp-registrar" {
                    server["entrypoint"] = json!(current);
                }
            }
        }
    }
}

fn surface_list(contract: &Value, args: &Value) -> Value {
    let items = contract
        .pointer("/read_models/registrar_surface_list/items")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let total = items.len();
    let offset = args
        .get("offset")
        .and_then(Value::as_u64)
        .unwrap_or(0)
        .min(10_000) as usize;
    let limit = args
        .get("limit")
        .and_then(Value::as_u64)
        .unwrap_or(50)
        .clamp(1, 200) as usize;
    let compact = args.get("compact").and_then(Value::as_bool).unwrap_or(true);
    let page = items
        .into_iter()
        .skip(offset)
        .take(limit + 1)
        .collect::<Vec<_>>();
    let has_more = page.len() > limit;
    let projected = page.into_iter().take(limit).map(|surface| {
        if !compact { return surface; }
        json!({
            "id":surface["id"],"package":surface["package"],"kind":surface["kind"],
            "injection_scope":surface["injection_scope"],"restart_owner":surface["restart_owner"],
            "descriptor_source":surface["descriptor_source"],
            "tool_count":surface["tools"].as_array().map_or(0, Vec::len),
            "projection_count":surface["projections"].as_array().map_or(0, Vec::len)
        })
    }).collect::<Vec<_>>();
    json!({"schema":"narada.registrar.surface_list.v1","status":"ok","items":projected,"returned":projected.len(),"total":total,"offset":offset,"limit":limit,"has_more":has_more,"next_offset":if has_more{json!(offset + limit)}else{Value::Null},"compact":compact})
}
fn carrier_list(contract: &Value, args: &Value) -> Value {
    let items = contract
        .pointer("/read_models/registrar_carrier_list/items")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    paginated_catalog("narada.registrar.carrier_list.v1", items, args, |carrier| {
        json!({
            "carrier_id":carrier["carrier_id"],
            "kind":carrier["kind"],
            "config_path":carrier["config_path"],
            "site_binding_count":carrier["site_bindings"].as_array().map_or(0, Vec::len),
            "surface_count":carrier["surfaces"].as_array().map_or(0, Vec::len)
        })
    })
}

fn paginated_catalog(
    schema: &str,
    items: Vec<Value>,
    args: &Value,
    compact_projection: impl Fn(&Value) -> Value,
) -> Value {
    let total = items.len();
    let offset = args
        .get("offset")
        .and_then(Value::as_u64)
        .unwrap_or(0)
        .min(10_000) as usize;
    let limit = args
        .get("limit")
        .and_then(Value::as_u64)
        .unwrap_or(20)
        .clamp(1, 100) as usize;
    let compact = args.get("compact").and_then(Value::as_bool).unwrap_or(true);
    let page = items
        .into_iter()
        .skip(offset)
        .take(limit + 1)
        .collect::<Vec<_>>();
    let has_more = page.len() > limit;
    let projected = page
        .into_iter()
        .take(limit)
        .map(|item| {
            if compact {
                compact_projection(&item)
            } else {
                item
            }
        })
        .collect::<Vec<_>>();
    json!({"schema":schema,"status":"ok","items":projected,"returned":projected.len(),"total":total,"offset":offset,"limit":limit,"has_more":has_more,"next_offset":if has_more{json!(offset + limit)}else{Value::Null},"compact":compact})
}
fn registrar_sync(contract: &Value, args: &Value) -> Result<Value, String> {
    let target = required_argument(args, "target", "registrar_requires_target")?;
    let carriers = contract
        .pointer("/read_models/registrar_carrier_list/items")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    if target == "all_surfaces_to_carriers" {
        let carrier_id = required_argument(
            args,
            "carrier_id",
            "registrar_requires_carrier_id_for_target",
        )?;
        let carrier = carrier_record(contract, &carrier_id)?;
        if carrier["site_bindings"]
            .as_array()
            .into_iter()
            .flatten()
            .any(|binding| binding["loading_mode"] == "progressive")
        {
            return Err(format!(
                "registrar_progressive_bulk_bind_refused:{carrier_id}"
            ));
        }
    }
    if target == "all_surfaces_to_all_carriers"
        && carriers.iter().any(|carrier| {
            carrier["site_bindings"]
                .as_array()
                .into_iter()
                .flatten()
                .any(|binding| binding["loading_mode"] == "progressive")
        })
    {
        return Err("registrar_progressive_bulk_bind_refused:all_carriers".into());
    }
    if target == "all_surfaces_to_carriers" || target == "all_surfaces_to_all_carriers" {
        return Err(format!("registrar_native_sync_unreachable:{target}"));
    }
    let surface_id = required_argument(args, "surface_id", "registrar_requires_surface_id")?;
    ensure_surface(contract, &surface_id)?;
    let mut results = vec![];
    if target == "all_sites" || target == "all" {
        for site in site_catalog(contract)["items"]
            .as_array()
            .into_iter()
            .flatten()
        {
            let site_id = site["site_id"].as_str().unwrap_or("");
            let call = json!({"site_id":site_id,"surface_id":surface_id,"projection_id":args.get("projection_id"),"runtime_kind":args.get("runtime_kind"),"allow_sidecar":args["allow_sidecar"]==true});
            match site_bind(contract, &call) {
                Ok(value) => results.push(value),
                Err(message) => {
                    results.push(json!({"site_id":site_id,"surface_id":surface_id,"error":message}))
                }
            }
        }
    }
    if target == "all_carriers" || target == "all" {
        for carrier in &carriers {
            let carrier_id = carrier["carrier_id"].as_str().unwrap_or("");
            match carrier_bind(
                contract,
                &json!({"carrier_id":carrier_id,"surface_id":surface_id,"projection_id":args.get("projection_id")}),
            ) {
                Ok(value) => results.push(value),
                Err(failure) => results
                    .push(json!({"carrier_id":carrier_id,"surface_id":surface_id,"error":failure.message})),
            }
        }
    }
    Ok(json!({"surface_id":surface_id,"target":target,"count":results.len(),"results":results}))
}

fn site_registry_conformance_check(contract: &Value, args: &Value) -> Result<Value, String> {
    let requested = required_argument(args, "site_id", "registrar_requires_site_id")?;
    let observation_ref = required_argument(
        args,
        "observation_ref",
        "registrar_requires_observation_ref",
    )?;
    let include_ok = args.get("include_ok").and_then(Value::as_bool) == Some(true);
    let sites = site_catalog(contract)["items"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let site = lookup_site_value(&sites, &requested)?;
    let root = PathBuf::from(site["root"].as_str().unwrap_or(""));
    let registry_path = capability_registry_path(&root);
    if !registry_path.exists() {
        return Err(format!(
            "registrar_site_surface_registry_not_found:{}",
            path_text(&registry_path)
        ));
    }
    let registry: Value = serde_json::from_str(
        &fs::read_to_string(&registry_path)
            .map_err(|error| format!("registrar_site_surface_registry_parse_failed:{error}"))?,
    )
    .map_err(|error| format!("registrar_site_surface_registry_parse_failed:{error}"))?;
    let shown = read_payload_revision(&root, &observation_ref)?;
    if shown["created_by"] != "mcp-loader-mcp"
        || !shown["payload_id"]
            .as_str()
            .unwrap_or("")
            .starts_with("site-tools-")
    {
        return Err("registrar_inventory_observation_lineage_mismatch".into());
    }
    let observation = &shown["payload"];
    if observation["schema"] != "narada.mcp_loader.site_tool_inventory_check.v1" {
        return Err("registrar_inventory_observation_schema_mismatch".into());
    }
    if comparable_path(observation["site_root"].as_str().unwrap_or(""))
        != comparable_path(site["root"].as_str().unwrap_or(""))
    {
        return Err("registrar_inventory_observation_site_mismatch".into());
    }
    for field in [
        "observed_tools",
        "observed_read_only_tools",
        "observed_mutating_tools",
    ] {
        if !observation[field].is_object() {
            return Err(format!(
                "registrar_inventory_observation_field_missing:{field}"
            ));
        }
    }
    let mut result = check_registry_conformance(
        contract,
        &site,
        &registry,
        &observation["observed_tools"],
        &observation["observed_read_only_tools"],
        &observation["observed_mutating_tools"],
        include_ok,
    )?;
    let object = result.as_object_mut().unwrap();
    object.insert("observation_ref".into(), json!(observation_ref));
    object.insert("observation_sha256".into(), shown["sha256"].clone());
    object.insert("observation_created_at".into(), shown["created_at"].clone());
    object.insert("observation_status".into(), observation["status"].clone());
    object.insert(
        "observation_observed_at".into(),
        observation["observed_at"].clone(),
    );
    object.insert(
        "observation_lineage".into(),
        json!({"declared_creator":shown["created_by"],"payload_id":shown["payload_id"],"assurance":"declarative_lineage_guard_not_cryptographic_provenance","authority_effect":"none"}),
    );
    Ok(result)
}

fn read_payload_revision(root: &Path, reference: &str) -> Result<Value, String> {
    let Some(rest) = reference.strip_prefix("mcp_payload:") else {
        return Err(format!("payload_ref_invalid: {reference}"));
    };
    let Some((payload_id, revision_text)) = rest.rsplit_once("@v") else {
        return Err(format!("payload_ref_invalid: {reference}"));
    };
    let revision = revision_text
        .parse::<u64>()
        .map_err(|_| format!("payload_ref_invalid: {reference}"))?;
    let path = root
        .join(".ai/tmp/mcp-payloads/workspace")
        .join(payload_id)
        .join(format!("v{revision}.json"));
    let content =
        fs::read_to_string(&path).map_err(|_| format!("payload_ref_not_found: {reference}"))?;
    let record: Value = serde_json::from_str(&content)
        .map_err(|error| format!("payload_ref_invalid_json: {error}"))?;
    if record["schema"] != "narada.mcp_payload.revision.v1" {
        return Err(format!(
            "payload_ref_schema_unsupported: {}",
            record["schema"].as_str().unwrap_or("")
        ));
    }
    if record["ref"] != reference
        || record["payload_id"] != payload_id
        || record["revision"] != revision
    {
        return Err(format!("payload_ref_metadata_mismatch: {reference}"));
    }
    let payload_text = canonical_json(&record["payload"]);
    if record["byte_size"].as_u64() != Some(payload_text.len() as u64) {
        return Err(format!("payload_ref_byte_size_mismatch: {reference}"));
    }
    if record["sha256"] != sha256_text(&payload_text) {
        return Err(format!("payload_ref_sha256_mismatch: {reference}"));
    }
    Ok(record)
}

#[allow(clippy::drop_non_drop)]
fn check_registry_conformance(
    contract: &Value,
    site: &Value,
    registry: &Value,
    observed: &Value,
    observed_read_only: &Value,
    observed_mutating: &Value,
    include_ok: bool,
) -> Result<Value, String> {
    let site_id = site["site_id"].as_str().unwrap_or("");
    let root = PathBuf::from(site["root"].as_str().unwrap_or(""));
    let registry_path = capability_registry_path(&root);
    let expected = build_site_surface_registry(contract, site)?;
    let expected_surfaces = expected["surfaces"].as_array().cloned().unwrap_or_default();
    let actual_surfaces = registry["surfaces"].as_array().cloned().unwrap_or_default();
    let mut actual_by_server = std::collections::HashMap::<String, Value>::new();
    let mut violations = vec![];
    let mut add_global = |code: &str, detail: Value| {
        violations.push(merge_value(json!({"layer":"materialized_registry","code":code,"surface_id":null,"server_name":null}), detail));
    };
    if registry["schema"] != "narada.site.capabilities.mcp_surfaces.v1" {
        add_global(
            "registry_schema_mismatch",
            json!({"expected":"narada.site.capabilities.mcp_surfaces.v1","actual":registry.get("schema")}),
        );
    }
    if registry["site_id"] != site_id {
        add_global(
            "registry_site_id_mismatch",
            json!({"expected":site_id,"actual":registry.get("site_id")}),
        );
    }
    if registry["generated_by"] != "mcp-registrar" {
        add_global(
            "registry_generator_mismatch",
            json!({"expected":"mcp-registrar","actual":registry.get("generated_by")}),
        );
    }
    if registry.pointer("/generation_policy/mode") != Some(&json!("enabled_surface_tool_authority"))
    {
        add_global(
            "registry_generation_policy_mismatch",
            json!({"expected":"enabled_surface_tool_authority","actual":registry.pointer("/generation_policy/mode")}),
        );
    }
    if registry.pointer("/generation_policy/source")
        != Some(&json!(".ai/mcp + registrar surface catalog"))
    {
        add_global(
            "registry_generation_source_mismatch",
            json!({"expected":".ai/mcp + registrar surface catalog","actual":registry.pointer("/generation_policy/source")}),
        );
    }
    if registry.pointer("/generation_policy/note") != expected.pointer("/generation_policy/note") {
        add_global("registry_generation_note_mismatch", json!({}));
    }
    if registry["generated_at"]
        .as_str()
        .and_then(|value| {
            time::OffsetDateTime::parse(value, &time::format_description::well_known::Rfc3339).ok()
        })
        .is_none()
    {
        add_global(
            "registry_generated_at_invalid",
            json!({"actual":registry.get("generated_at")}),
        );
    }
    if !registry["surfaces"].is_array() {
        add_global(
            "registry_surfaces_invalid",
            json!({"actual_type":json_type_name(registry.get("surfaces"))}),
        );
    }
    for surface in &actual_surfaces {
        let name = surface["server_name"].as_str().unwrap_or("");
        if name.is_empty() {
            add_global(
                "registry_surface_server_name_missing",
                json!({"surface_id":surface.get("surface_id")}),
            );
            continue;
        }
        if actual_by_server
            .insert(name.into(), surface.clone())
            .is_some()
        {
            add_global(
                "registry_surface_server_name_duplicate",
                json!({"server_name":name}),
            );
        }
    }
    let expected_names = expected_surfaces
        .iter()
        .filter_map(|surface| surface["server_name"].as_str())
        .collect::<std::collections::HashSet<_>>();
    for name in actual_by_server.keys() {
        if !expected_names.contains(name.as_str()) {
            add_global(
                "registry_surface_not_in_fabric",
                json!({"server_name":name}),
            );
        }
    }
    drop(add_global);
    let observed_keys = observed
        .as_object()
        .map(|value| value.keys().cloned().collect::<Vec<_>>())
        .unwrap_or_default();
    let mut unobserved = vec![];
    let mut surface_results = vec![];
    for expected_surface in &expected_surfaces {
        let server_name = expected_surface["server_name"].as_str().unwrap_or("");
        let actual = actual_by_server.get(server_name);
        let surface_id = actual
            .map(|v| v["surface_id"].clone())
            .unwrap_or_else(|| json!(format!("{server_name}.local")));
        let catalog_id = expected_surface["catalog_surface_id"].clone();
        let mut local = vec![];
        let mut add = |layer: &str, code: &str, detail: Value| {
            let violation = merge_value(
                json!({"layer":layer,"code":code,"surface_id":surface_id,"server_name":server_name,"catalog_surface_id":catalog_id}),
                detail,
            );
            local.push(violation.clone());
            violations.push(violation);
        };
        if actual.is_none() {
            add(
                "materialized_registry",
                "registry_surface_missing",
                json!({}),
            );
        }
        let keys = [
            server_name,
            &format!("{server_name}.local"),
            expected_surface["surface_id"].as_str().unwrap_or(""),
            expected_surface["catalog_surface_id"]
                .as_str()
                .unwrap_or(""),
        ];
        let live = observed_array(observed, &keys);
        let live_ro = observed_array(observed_read_only, &keys);
        let live_mut = observed_array(observed_mutating, &keys);
        let requested = observed_keys.is_empty()
            || keys
                .iter()
                .any(|key| observed_keys.iter().any(|value| value == key));
        if !requested {
            unobserved.push(server_name.to_string());
        } else {
            if live.is_none() {
                add("live_surface", "live_tool_observation_missing", json!({}));
            }
            if live_ro.is_none() {
                add(
                    "live_surface",
                    "live_read_only_observation_missing",
                    json!({}),
                );
            }
            if live_mut.is_none() {
                add(
                    "live_surface",
                    "live_mutating_observation_missing",
                    json!({}),
                );
            }
        }
        let expected_tools = strings(&expected_surface["registered_live_tools"]);
        if let Some(values) = live.as_ref() {
            let duplicates = duplicate_strings(values);
            if !duplicates.is_empty() {
                add(
                    "live_surface",
                    "live_tools_duplicate",
                    json!({"duplicate_tools":duplicates}),
                );
            }
        }
        if let Some(values) = live_ro.as_ref() {
            let duplicates = duplicate_strings(values);
            if !duplicates.is_empty() {
                add(
                    "live_surface",
                    "live_read_only_tools_duplicate",
                    json!({"duplicate_tools":duplicates}),
                );
            }
        }
        if let Some(values) = live_mut.as_ref() {
            let duplicates = duplicate_strings(values);
            if !duplicates.is_empty() {
                add(
                    "live_surface",
                    "live_mutating_tools_duplicate",
                    json!({"duplicate_tools":duplicates}),
                );
            }
        }
        if let (Some(all), Some(read_only), Some(mutating)) = (&live, &live_ro, &live_mut) {
            let mut semantic_union = read_only.clone();
            semantic_union.extend(mutating.clone());
            compare_sets(
                &mut add,
                "live_surface",
                "live_tool_semantics_partition_incomplete",
                all,
                &semantic_union,
            );
            let overlaps = read_only
                .iter()
                .filter(|tool| mutating.contains(tool))
                .cloned()
                .collect::<Vec<_>>();
            if !overlaps.is_empty() {
                let mut overlaps = overlaps;
                overlaps.sort();
                overlaps.dedup();
                add(
                    "live_surface",
                    "live_tool_semantics_partition_overlap",
                    json!({"overlapping_tools":overlaps}),
                );
            }
        }
        if let Some(values) = live.as_ref() {
            compare_sets(
                &mut add,
                "site_fabric",
                "fabric_tools_differ_from_live",
                values,
                &expected_tools,
            );
            compare_sets(
                &mut add,
                "registrar_catalog",
                "catalog_tools_differ_from_live",
                values,
                &expected_tools,
            );
        }
        if let Some(actual) = actual {
            let raw_registered = strings(&actual["registered_live_tools"]);
            let registered = unique_strings(&raw_registered);
            let read_only = strings(
                &actual
                    .pointer("/tool_contract/read_only_tools")
                    .cloned()
                    .unwrap_or_else(|| json!([])),
            );
            let mutating = strings(
                &actual
                    .pointer("/tool_contract/mutating_tools")
                    .cloned()
                    .unwrap_or_else(|| json!([])),
            );
            let refused = strings(
                &actual
                    .pointer("/tool_contract/refused_tools")
                    .cloned()
                    .unwrap_or_else(|| json!([])),
            );
            let duplicate_contract_tools = json!({"registered_live_tools":duplicate_strings(&raw_registered),"read_only_tools":duplicate_strings(&read_only),"mutating_tools":duplicate_strings(&mutating),"refused_tools":duplicate_strings(&refused)});
            if duplicate_contract_tools
                .as_object()
                .unwrap()
                .values()
                .any(|value| value.as_array().is_some_and(|items| !items.is_empty()))
            {
                add(
                    "tool_contract",
                    "tool_contract_contains_duplicates",
                    duplicate_contract_tools,
                );
            }
            if let Some(values) = live.as_ref() {
                compare_sets(
                    &mut add,
                    "materialized_registry",
                    "registered_tools_differ_from_live",
                    values,
                    &registered,
                );
            }
            compare_sets(
                &mut add,
                "materialized_registry",
                "registered_tools_differ_from_fabric",
                &expected_tools,
                &registered,
            );
            compare_sets(
                &mut add,
                "materialized_registry",
                "registered_tools_differ_from_catalog",
                &expected_tools,
                &registered,
            );
            let mut contract_union = read_only.clone();
            contract_union.extend(mutating.clone());
            contract_union.extend(refused.clone());
            compare_sets(
                &mut add,
                "tool_contract",
                "tool_contract_partition_incomplete",
                &registered,
                &contract_union,
            );
            let overlaps = read_only
                .iter()
                .filter(|tool| mutating.contains(tool) || refused.contains(tool))
                .chain(mutating.iter().filter(|tool| refused.contains(tool)))
                .cloned()
                .collect::<Vec<_>>();
            if !overlaps.is_empty() {
                let overlaps = unique_strings(&overlaps);
                add(
                    "tool_contract",
                    "tool_contract_partition_overlap",
                    json!({"overlapping_tools":overlaps}),
                );
            }
            if !refused.is_empty() {
                add(
                    "tool_contract",
                    "tool_contract_contains_external_refusals",
                    json!({"refused_tools":refused}),
                );
            }
            if let Some(values) = live_ro.as_ref() {
                compare_sets(
                    &mut add,
                    "tool_contract",
                    "read_only_classification_differ_from_live",
                    values,
                    &read_only,
                );
            }
            if let Some(values) = live_mut.as_ref() {
                compare_sets(
                    &mut add,
                    "tool_contract",
                    "mutating_classification_differ_from_live",
                    values,
                    &mutating,
                );
            }
            for field in [
                "surface_id",
                "display_name",
                "server_name",
                "authority_boundary",
                "client_config",
                "catalog_surface_id",
                "descriptor_provenance",
                "surface_descriptor",
            ] {
                if canonical_json(&actual[field]) != canonical_json(&expected_surface[field]) {
                    add(
                        "materialized_registry",
                        "registry_surface_projection_drift",
                        json!({"field":field}),
                    );
                }
            }
        }
        if include_ok || !local.is_empty() {
            surface_results.push(json!({"surface_id":surface_id,"server_name":server_name,"catalog_surface_id":catalog_id,"status":if local.is_empty(){"ok"}else{"drift"},"violation_count":local.len(),"violations":local}));
        }
    }
    let output_reader = output_reader_closure_for_registry(
        contract,
        registry,
        Some(site_id),
        Some(&root),
        Some(&registry_path),
    );
    for raw in output_reader["violations"].as_array().into_iter().flatten() {
        violations.push(merge_value(
            json!({"layer":"output_reader_closure","code":"output_reader_closure_violation"}),
            raw.clone(),
        ));
    }
    unobserved.sort();
    let mut observed_sorted = observed_keys;
    observed_sorted.sort();
    Ok(
        json!({"schema":"narada.registrar.site_registry_conformance_check.v1","status":if !violations.is_empty(){"drift"}else if !unobserved.is_empty(){"incomplete"}else{"ok"},"site_id":site_id,"site_root":site["root"],"registry_path":path_text(&registry_path),"checked_surface_count":expected_surfaces.len(),"observed_surface_count":observed.as_object().map_or(0,|v|v.len()),"observation_coverage":{"status":if observed_sorted.is_empty(){"missing"}else if !unobserved.is_empty(){"partial"}else{"complete"},"observed_server_names":observed_sorted,"unobserved_server_names":unobserved},"violation_count":violations.len(),"violations":violations,"surfaces":surface_results,"output_reader_closure":output_reader}),
    )
}

fn observed_array(input: &Value, keys: &[&str]) -> Option<Vec<String>> {
    keys.iter().find_map(|key| {
        input.get(key).and_then(Value::as_array).map(|values| {
            values
                .iter()
                .map(|value| value.as_str().unwrap_or("").to_string())
                .collect()
        })
    })
}
fn strings(value: &Value) -> Vec<String> {
    value
        .as_array()
        .into_iter()
        .flatten()
        .map(|value| value.as_str().unwrap_or("").to_string())
        .collect()
}
fn unique_strings(values: &[String]) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    values
        .iter()
        .filter(|value| seen.insert((*value).clone()))
        .cloned()
        .collect()
}
fn duplicate_strings(values: &[String]) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    let mut duplicates = vec![];
    for value in values {
        if !seen.insert(value.clone()) && !duplicates.contains(value) {
            duplicates.push(value.clone());
        }
    }
    duplicates.sort();
    duplicates
}
fn compare_sets(
    add: &mut impl FnMut(&str, &str, Value),
    layer: &str,
    code: &str,
    expected: &[String],
    actual: &[String],
) {
    let mut left = expected.to_vec();
    left.sort();
    left.dedup();
    let mut right = actual.to_vec();
    right.sort();
    right.dedup();
    if left == right {
        return;
    }
    let missing = left
        .iter()
        .filter(|value| !right.contains(value))
        .cloned()
        .collect::<Vec<_>>();
    let extra = right
        .iter()
        .filter(|value| !left.contains(value))
        .cloned()
        .collect::<Vec<_>>();
    add(
        layer,
        code,
        json!({"missing":missing,"extra":extra,"expected_count":left.len(),"actual_count":right.len()}),
    );
}
fn comparable_path(value: &str) -> String {
    value.replace('\\', "/").to_lowercase()
}
fn json_type_name(value: Option<&Value>) -> &'static str {
    match value {
        Some(Value::Array(_)) => "object",
        Some(Value::Object(_)) => "object",
        Some(Value::String(_)) => "string",
        Some(Value::Bool(_)) => "boolean",
        Some(Value::Number(_)) => "number",
        _ => "undefined",
    }
}

fn carrier_diff(contract: &Value, args: &Value) -> Result<Value, String> {
    let carrier_id = required_argument(args, "carrier_id", "registrar_requires_carrier_id")?;
    let plan = contract
        .pointer(&format!(
            "/read_models/registrar_carrier_projection_plans/{carrier_id}"
        ))
        .ok_or_else(|| format!("registrar_unknown_carrier:{carrier_id}"))?;
    let config_path = plan["config_path"].as_str().unwrap_or("");
    let generated_content = plan["generated_content"].as_str().unwrap_or("");
    let generated_structured = &plan["generated_structured"];
    let current_content = fs::read_to_string(config_path).ok();
    let current_structured = current_content
        .as_deref()
        .and_then(|content| parse_carrier_config(plan["kind"].as_str().unwrap_or(""), content));
    let generated_servers = carrier_servers(generated_structured);
    let current_servers = current_structured
        .as_ref()
        .map(carrier_servers)
        .unwrap_or_default();
    if let (Some(current_content), Some(receipt)) = (
        current_content.as_deref(),
        native_materialization_receipt(config_path, &carrier_id),
    ) {
        let current_sha256 = sha256_text(current_content);
        if receipt.matches(
            plan["kind"].as_str().unwrap_or(""),
            current_content.as_bytes(),
        ) {
            let mut unchanged = current_servers.keys().cloned().collect::<Vec<_>>();
            unchanged.sort();
            return Ok(json!({
                "schema":"narada.registrar.carrier_projection_diff.v1",
                "status":"clean",
                "carrier_id":carrier_id,
                "config_path":config_path,
                "current_exists":true,
                "projection_changed":false,
                "server_projection_changed":false,
                "carrier_metadata_or_format_only":false,
                "change_scopes":[],
                "explanation_code":"carrier_projection_matches_native_materialization_receipt",
                "comparison_authority":"native_materialization_receipt",
                "comparison_scope":receipt.scope,
                "generation_sidecar_path":receipt.sidecar_path,
                "generated_sha256":receipt.expected_sha256,
                "current_sha256":current_sha256,
                "generated_byte_size":current_content.len(),
                "current_byte_size":current_content.len(),
                "added":[],
                "removed":[],
                "changed":[],
                "unchanged":unchanged.clone(),
                "added_count":0,
                "removed_count":0,
                "changed_count":0,
                "server_changed_count":0,
                "count_semantics":"added_removed_changed_counts_cover_server_definitions_only",
                "server_changes":{"added":[],"removed":[],"changed":[],"unchanged":unchanged,"added_count":0,"removed_count":0,"changed_count":0},
                "runtime_contract_version":plan["runtime_contract_version"],
                "materialization_validation":plan["materialization_validation"]
            }));
        }
    }
    let mut added = vec![];
    let mut removed = vec![];
    let mut changed = vec![];
    let mut unchanged = vec![];
    for (key, generated) in &generated_servers {
        match current_servers.get(key) {
            None => added.push(key.clone()),
            Some(current) if canonical_json(generated) != canonical_json(current) => {
                changed.push(key.clone())
            }
            Some(_) => unchanged.push(key.clone()),
        }
    }
    for key in current_servers.keys() {
        if !generated_servers.contains_key(key) {
            removed.push(key.clone())
        }
    }
    let current_exists = current_content.is_some();
    let projection_changed = current_content.as_deref() != Some(generated_content);
    let server_projection_changed = !added.is_empty() || !removed.is_empty() || !changed.is_empty();
    let metadata_only = current_exists && projection_changed && !server_projection_changed;
    let change_scopes = if !current_exists {
        json!(["full_projection_missing"])
    } else if !projection_changed {
        json!([])
    } else if server_projection_changed {
        json!(["full_projection", "server_definitions"])
    } else {
        json!(["full_projection", "carrier_metadata_or_format"])
    };
    let result = json!({
        "schema":"narada.registrar.carrier_projection_diff.v1",
        "status":if !current_exists{"missing"}else if projection_changed{"diff"}else{"clean"},
        "carrier_id":carrier_id,
        "config_path":config_path,
        "current_exists":current_exists,
        "projection_changed":projection_changed,
        "server_projection_changed":server_projection_changed,
        "carrier_metadata_or_format_only":metadata_only,
        "change_scopes":change_scopes,
        "explanation_code":if !current_exists{"carrier_projection_missing"}else if !projection_changed{"carrier_projection_exact_match"}else if metadata_only{"carrier_metadata_or_format_changed_without_server_definition_change"}else{"carrier_server_definition_change"},
        "generated_sha256":sha256_text(generated_content),
        "current_sha256":current_content.as_deref().map(sha256_text),
        "generated_byte_size":generated_content.len(),
        "current_byte_size":current_content.as_ref().map(|value|value.len()),
        "added":added,
        "removed":removed,
        "changed":changed,
        "unchanged":unchanged,
        "added_count":added.len(),
        "removed_count":removed.len(),
        "changed_count":changed.len(),
        "server_changed_count":changed.len(),
        "count_semantics":"added_removed_changed_counts_cover_server_definitions_only",
        "server_changes":{"added":added,"removed":removed,"changed":changed,"unchanged":unchanged,"added_count":added.len(),"removed_count":removed.len(),"changed_count":changed.len()},
        "runtime_contract_version":plan["runtime_contract_version"],
        "materialization_validation":plan["materialization_validation"]
    });
    Ok(result)
}

struct NativeMaterializationReceipt {
    sidecar_path: String,
    expected_sha256: String,
    scope: String,
    selectors: Vec<String>,
}

impl NativeMaterializationReceipt {
    fn matches(&self, kind: &str, content: &[u8]) -> bool {
        if self.scope == "whole_document" {
            return format!("{:x}", Sha256::digest(content)) == self.expected_sha256;
        }
        describe_config(kind, content, &self.selectors)
            .ok()
            .is_some_and(|description| {
                description.managed_projection.sha256 == self.expected_sha256
            })
    }
}

fn native_materialization_receipt(
    config_path: &str,
    carrier_id: &str,
) -> Option<NativeMaterializationReceipt> {
    let sidecar_path = format!("{config_path}.narada-generation.json");
    let sidecar: Value = serde_json::from_str(&fs::read_to_string(&sidecar_path).ok()?).ok()?;
    if sidecar.get("carrier_id").and_then(Value::as_str) != Some(carrier_id)
        || sidecar
            .get("config_path")
            .and_then(Value::as_str)
            .is_none_or(|declared| comparable_path(declared) != comparable_path(config_path))
    {
        return None;
    }
    let scope = sidecar
        .pointer("/managed_projection/scope")
        .and_then(Value::as_str)?
        .to_string();
    let expected_sha256 = sidecar
        .pointer(if scope == "whole_document" {
            "/config_artifact/bytes_sha256"
        } else {
            "/managed_projection/sha256"
        })
        .and_then(Value::as_str)?
        .to_string();
    let selectors = sidecar
        .pointer("/managed_projection/selectors")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .map(|selector| selector.as_str().map(str::to_string))
        .collect::<Option<Vec<_>>>()?;
    Some(NativeMaterializationReceipt {
        sidecar_path,
        expected_sha256,
        scope,
        selectors,
    })
}

fn sha256_text(text: &str) -> String {
    format!("{:x}", Sha256::digest(text.as_bytes()))
}

fn carrier_servers(value: &Value) -> serde_json::Map<String, Value> {
    value
        .get("mcpServers")
        .or_else(|| value.get("mcp"))
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default()
}

fn canonical_json(value: &Value) -> String {
    match value {
        Value::Array(values) => format!(
            "[{}]",
            values
                .iter()
                .map(canonical_json)
                .collect::<Vec<_>>()
                .join(",")
        ),
        Value::Object(values) => {
            let mut keys = values.keys().collect::<Vec<_>>();
            keys.sort();
            format!(
                "{{{}}}",
                keys.into_iter()
                    .map(|key| format!(
                        "{}:{}",
                        serde_json::to_string(key).unwrap(),
                        canonical_json(&values[key])
                    ))
                    .collect::<Vec<_>>()
                    .join(",")
            )
        }
        _ => serde_json::to_string(value).unwrap(),
    }
}

fn parse_carrier_config(kind: &str, content: &str) -> Option<Value> {
    match kind {
        "opencode" | "kimi" => parse_jsonc(content),
        "codex" => Some(parse_codex_toml(content)),
        _ => None,
    }
}

fn parse_codex_toml(content: &str) -> Value {
    let mut servers = serde_json::Map::new();
    let mut current: Option<String> = None;
    for raw in content.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if line.starts_with("[mcp_servers.") && line.ends_with(']') {
            let key = &line[13..line.len() - 1];
            if key.contains(".tools.") {
                current = None;
            } else {
                current = Some(key.to_string());
                servers.insert(key.to_string(), json!({}));
            }
            continue;
        }
        let Some(key) = current.as_ref() else {
            continue;
        };
        let Some((field, raw_value)) = line.split_once('=') else {
            continue;
        };
        let field = field.trim();
        let raw_value = raw_value.trim();
        let value =
            serde_json::from_str(raw_value).unwrap_or_else(|_| json!(raw_value.trim_matches('"')));
        servers
            .get_mut(key)
            .and_then(Value::as_object_mut)
            .unwrap()
            .insert(field.to_string(), value);
    }
    json!({"mcpServers":servers})
}

fn carrier_validate(contract: &Value, args: &Value) -> Result<Value, String> {
    let carrier_id = required_argument(args, "carrier_id", "registrar_requires_carrier_id")?;
    let include_ok = args.get("include_ok").and_then(Value::as_bool) == Some(true);
    let carriers = contract
        .pointer("/read_models/registrar_carrier_list/items")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    if !carriers
        .iter()
        .any(|candidate| candidate["carrier_id"] == carrier_id)
    {
        return Err(format!("registrar_unknown_carrier:{carrier_id}"));
    }
    let surface_catalog = contract
        .pointer("/read_models/registrar_surface_list/items")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let servers = contract
        .pointer(&format!(
            "/read_models/registrar_carrier_validation_plans/{carrier_id}/servers"
        ))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut findings = vec![];
    let mut add = |severity: &str, code: &str, message: String, detail: Value| {
        let mut finding = json!({"severity":severity,"code":code,"message":message});
        if let Some(values) = detail.as_object() {
            finding.as_object_mut().unwrap().extend(values.clone())
        }
        findings.push(finding)
    };
    let mut seen = std::collections::HashMap::<String, String>::new();
    for server in &servers {
        let key = server["server_key"].as_str().unwrap_or("");
        let surface_id = server["surface_id"].as_str().unwrap_or(key);
        let detail = merge_value(
            json!({"server_key":key,"surface_id":surface_id}),
            scope_finding_detail(server["narada_scope"].clone()),
        );
        if let Some(previous) = seen.insert(key.to_string(), surface_id.to_string()) {
            add(
                "error",
                "registrar_duplicate_server_key",
                format!("Server key '{key}' is produced by both '{previous}' and '{surface_id}'"),
                detail.clone(),
            );
        } else if include_ok {
            add(
                "info",
                "registrar_server_key_ok",
                format!("Server key '{key}' resolved for surface '{surface_id}'"),
                detail.clone(),
            );
        }
    }
    for server in &servers {
        let key = server["server_key"].as_str().unwrap_or("");
        let surface_id = server["surface_id"].as_str().unwrap_or(key);
        let detail = merge_value(
            json!({"server_key":key,"surface_id":surface_id}),
            scope_finding_detail(server["narada_scope"].clone()),
        );
        let entrypoint = canonical_root(PathBuf::from(server["entrypoint"].as_str().unwrap_or("")));
        if !entrypoint.exists() {
            add(
                "error",
                "registrar_missing_entrypoint",
                format!(
                    "Entrypoint for '{key}' does not exist: {}",
                    path_text(&entrypoint)
                ),
                merge_value(detail.clone(), json!({"entrypoint":path_text(&entrypoint)})),
            );
        } else if include_ok {
            add(
                "info",
                "registrar_entrypoint_exists",
                format!("Entrypoint for '{key}' exists: {}", path_text(&entrypoint)),
                merge_value(detail.clone(), json!({"entrypoint":path_text(&entrypoint)})),
            );
        }
        let known = surface_catalog
            .iter()
            .find(|surface| surface["id"] == surface_id);
        add_runtime_preflight(
            &mut add,
            include_ok,
            merge_value(detail.clone(), json!({"entrypoint":path_text(&entrypoint)})),
            known,
            server["uses_runtime_proxy"].as_bool() == Some(true),
        );
        let child_args = server["args"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .collect::<Vec<_>>();
        if [
            "local-filesystem",
            "git",
            "structured-command",
            "delegated-task",
            "worker-delegation",
        ]
        .contains(&surface_id)
        {
            let roots = flag_values(&child_args, "--allowed-root");
            if roots.is_empty() {
                add("error", "registrar_missing_allowed_root", format!("Surface '{surface_id}' requires at least one --allowed-root but '{key}' has none"), detail.clone());
            } else if include_ok {
                add(
                    "info",
                    "registrar_allowed_root_ok",
                    format!(
                        "Surface '{surface_id}' on '{key}' has {} allowed root(s)",
                        roots.len()
                    ),
                    merge_value(detail.clone(), json!({"allowed_roots":roots})),
                );
            }
        }
        if surface_id == "local-filesystem" || surface_id == "local-filesystem-mcp.local" {
            if !child_args.contains(&"--output-root") {
                add(
                    "warning",
                    "registrar_missing_output_root",
                    format!("Filesystem surface '{key}' is missing --output-root"),
                    detail.clone(),
                );
            } else if include_ok {
                add(
                    "info",
                    "registrar_output_root_ok",
                    format!("Filesystem surface '{key}' has --output-root"),
                    detail.clone(),
                );
            }
        }
    }
    let errors = findings
        .iter()
        .filter(|finding| finding["severity"] == "error")
        .count();
    let warnings = findings
        .iter()
        .filter(|finding| finding["severity"] == "warning")
        .count();
    Ok(
        json!({"schema":"narada.registrar.carrier_validation.v1","status":if errors>0{"invalid"}else if warnings>0{"valid_with_warnings"}else{"valid"},"carrier_id":carrier_id,"server_count":servers.len(),"errors":errors,"warnings":warnings,"findings":findings,"bounded":true}),
    )
}
fn site_catalog(contract: &Value) -> Value {
    let fallback = &contract["read_models"]["registrar_site_list_fallback"];
    let registry_path = env::var_os("NARADA_SITE_REGISTRY_DB")
        .map(PathBuf::from)
        .unwrap_or_else(|| user_site_root().join("registry.db"));
    if !registry_path.exists() {
        return fallback_site_list(fallback, &registry_path, "registry_file_missing");
    }
    match read_site_registry(&registry_path, fallback) {
        Ok(mut items) => {
            for site in &mut items {
                let root = PathBuf::from(site["root"].as_str().unwrap_or(""));
                let fallback = site["site_id"].as_str().unwrap_or("");
                let site_id = canonical_site_id(&root, fallback);
                match scan_site_surfaces(contract, &root, &site_id) {
                    Ok(surfaces) => {
                        let surface_count = surfaces.len();
                        site["surfaces"] = json!(surfaces);
                        site["surface_count"] = json!(surface_count);
                        site["surfaces_status"] = json!("current");
                    }
                    Err(message) => {
                        site["surfaces"] = json!([]);
                        site["surface_count"] = json!(0);
                        site["surfaces_status"] = json!("unavailable");
                        site["surfaces_error"] = json!(message);
                    }
                }
            }
            json!({
                "items": items,
                "count": items.len(),
                "catalog_source": "user_site_site_registry",
                "registry_path": path_text(&registry_path),
                "compatibility_fallback_used": false
            })
        }
        Err(message) => fallback_site_list(fallback, &registry_path, &message),
    }
}

fn site_list(contract: &Value, args: &Value) -> Value {
    let catalog = site_catalog(contract);
    let items = catalog["items"].as_array().cloned().unwrap_or_default();
    let mut result = paginated_catalog("narada.registrar.site_list.v1", items, args, |site| {
        json!({
            "site_id":site["site_id"],
            "root":site["root"],
            "surface_count":site["surface_count"],
            "surfaces_status":site["surfaces_status"]
        })
    });
    result["catalog_source"] = catalog["catalog_source"].clone();
    result["registry_path"] = catalog["registry_path"].clone();
    result["compatibility_fallback_used"] = catalog["compatibility_fallback_used"].clone();
    result
}

fn fallback_site_list(fallback: &Value, path: &Path, error_message: &str) -> Value {
    json!({
        "items": fallback["items"],
        "count": fallback["count"],
        "catalog_source": "legacy_compatibility_catalog",
        "registry_path": path_text(path),
        "compatibility_fallback_used": true,
        "catalog_error": error_message
    })
}

#[allow(clippy::let_and_return)]
fn read_site_registry(path: &Path, fallback: &Value) -> Result<Vec<Value>, String> {
    let connection = rusqlite::Connection::open_with_flags(
        path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|error| error.to_string())?;
    let has_lifecycle = {
        let mut statement = connection
            .prepare("PRAGMA table_info(site_registry)")
            .map_err(|error| error.to_string())?;
        let columns = statement
            .query_map([], |row| row.get::<_, String>(1))
            .map_err(|error| error.to_string())?;
        let found = columns
            .filter_map(Result::ok)
            .any(|name| name == "lifecycle_status");
        found
    };
    let sql = if has_lifecycle {
        "SELECT site_id, site_root, lifecycle_status FROM site_registry ORDER BY created_at ASC, site_id ASC"
    } else {
        "SELECT site_id, site_root, NULL FROM site_registry ORDER BY created_at ASC, site_id ASC"
    };
    let mut statement = connection.prepare(sql).map_err(|error| error.to_string())?;
    let rows = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, Option<String>>(0)?,
                row.get::<_, Option<String>>(1)?,
                row.get::<_, Option<String>>(2)?,
            ))
        })
        .map_err(|error| error.to_string())?;
    let known = fallback["items"].as_array().cloned().unwrap_or_default();
    let mut items = vec![];
    for row in rows {
        let (site_id, root, lifecycle) = row.map_err(|error| error.to_string())?;
        let site_id = site_id.unwrap_or_default().trim().to_string();
        let root = root.unwrap_or_default().trim().to_string();
        let lifecycle = lifecycle
            .unwrap_or_else(|| "active".into())
            .trim()
            .to_ascii_lowercase();
        if site_id.is_empty() || root.is_empty() || lifecycle != "active" {
            continue;
        }
        let root = canonical_root(PathBuf::from(root));
        let template = known.iter().find(|site| {
            site["root"].as_str().is_some_and(|known_root| {
                comparable_root(Path::new(known_root)) == comparable_root(&root)
            })
        });
        let config_path = site_config_path(&root);
        let fallback_overrides = template
            .and_then(|site| site.get("surface_overrides"))
            .cloned()
            .unwrap_or_else(|| json!({}));
        let overrides = read_surface_overrides(&config_path, fallback_overrides)?;
        items.push(json!({
            "site_id": site_id,
            "root": path_text(&root),
            "config_path": path_text(&config_path),
            "surfaces": template.and_then(|site| site.get("surfaces")).cloned().unwrap_or_else(||json!([])),
            "surface_overrides": overrides
        }));
        if let Some(allowlist) = template.and_then(|site| site.get("local_surface_allowlist")) {
            if !allowlist.is_null() {
                items
                    .last_mut()
                    .unwrap()
                    .as_object_mut()
                    .unwrap()
                    .insert("local_surface_allowlist".into(), allowlist.clone());
            }
        }
    }
    Ok(items)
}

fn read_surface_overrides(config_path: &Path, fallback: Value) -> Result<Value, String> {
    if !config_path.exists() {
        return Ok(fallback);
    }
    let text = fs::read_to_string(config_path).map_err(|error| error.to_string())?;
    let parsed: Value =
        serde_json::from_str(text.trim_start_matches('\u{feff}')).map_err(|error| {
            format!(
                "registrar_site_config_parse_failed:{}:{error}",
                path_text(config_path)
            )
        })?;
    let mut overrides = fallback.as_object().cloned().unwrap_or_default();
    if let Some(entries) = parsed.get("surface_overrides").and_then(Value::as_object) {
        for (surface_id, value) in entries {
            let enabled = value
                .get("enabled")
                .and_then(Value::as_bool)
                .ok_or_else(|| format!("registrar_site_surface_override_invalid:{surface_id}"))?;
            let mut item = json!({"enabled": enabled});
            if let Some(implementation) =
                value.get("surface_implementation").and_then(Value::as_str)
            {
                if implementation != "js" && implementation != "native" {
                    return Err(format!(
                        "registrar_site_surface_override_invalid:{surface_id}"
                    ));
                }
                item.as_object_mut()
                    .unwrap()
                    .insert("surface_implementation".into(), json!(implementation));
            }
            overrides.insert(surface_id.clone(), item);
        }
    }
    Ok(Value::Object(overrides))
}

fn user_site_root() -> PathBuf {
    env::var_os("NARADA_USER_SITE_ROOT")
        .or_else(|| {
            env::var_os("USERPROFILE")
                .map(|home| PathBuf::from(home).join("Narada").into_os_string())
        })
        .or_else(|| {
            env::var_os("HOME").map(|home| PathBuf::from(home).join("Narada").into_os_string())
        })
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(".narada/user-site"))
}

fn canonical_root(path: PathBuf) -> PathBuf {
    let absolute = if path.is_absolute() {
        path
    } else {
        env::current_dir().unwrap_or_default().join(path)
    };
    if absolute
        .file_name()
        .is_some_and(|name| name.to_string_lossy().eq_ignore_ascii_case(".narada"))
    {
        absolute.parent().unwrap_or(&absolute).to_path_buf()
    } else {
        absolute
    }
}

fn site_config_path(root: &Path) -> PathBuf {
    let nested = root.join(".narada").join("config.json");
    if nested.exists() {
        nested
    } else {
        root.join("config.json")
    }
}

fn comparable_root(path: &Path) -> String {
    path_text(&canonical_root(path.to_path_buf()))
        .trim_end_matches('/')
        .to_ascii_lowercase()
}

fn path_text(path: &Path) -> String {
    path.to_string_lossy().replace('/', "\\")
}
fn site_surfaces(contract: &Value, args: &Value) -> Result<Value, String> {
    let requested = args
        .get("site_id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| "registrar_requires_site_id".to_string())?;
    if requested == "narada-andrey" || requested == "narada-user-site" {
        return Err("registrar_legacy_site_id_rejected:site_id".into());
    }
    let catalog = site_catalog(contract);
    let candidates = catalog["items"].as_array().cloned().unwrap_or_default();
    let mut site = None;
    for candidate in candidates {
        let root = candidate["root"].as_str().unwrap_or("");
        let fallback_id = candidate["site_id"].as_str().unwrap_or("");
        let canonical_id = canonical_site_id(Path::new(root), fallback_id);
        if fallback_id == requested
            || canonical_id == requested
            || format!("narada-{canonical_id}") == requested
        {
            site = Some((candidate, canonical_id));
            break;
        }
    }
    let Some((site, site_id)) = site else {
        return Err(format!("registrar_unknown_site:{requested}"));
    };
    let root = PathBuf::from(site["root"].as_str().unwrap_or(""));
    let found = scan_site_surfaces(contract, &root, &site_id)?;
    let count = found.len();
    Ok(
        json!({"schema":"narada.registrar.site_surfaces.v1","status":"ok","site_id":site_id,"surfaces":found,"count":count,"bounded":true}),
    )
}

fn scan_site_surfaces(contract: &Value, root: &Path, site_id: &str) -> Result<Vec<String>, String> {
    let control_root = site_mcp_control_root(root);
    let config_dir = control_root.join(".ai").join("mcp");
    if !config_dir.exists() {
        return Ok(Vec::new());
    }
    let surface_ids = contract["read_models"]["registrar_surface_list"]["items"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|surface| surface["id"].as_str())
        .collect::<Vec<_>>();
    let prefix = if site_id == "andrey-user" {
        "narada-site-andrey-user".to_string()
    } else if site_id.starts_with("narada-") {
        site_id.to_string()
    } else {
        format!("narada-{site_id}")
    };
    let mut found: Vec<String> = vec![];
    let entries = fs::read_dir(&config_dir).map_err(|error| error.to_string())?;
    for entry in entries.filter_map(Result::ok) {
        if entry.path().extension().and_then(|value| value.to_str()) != Some("json") {
            continue;
        }
        let Ok(text) = fs::read_to_string(entry.path()) else {
            continue;
        };
        let Ok(config) = serde_json::from_str::<Value>(&text) else {
            continue;
        };
        let servers = config.get("mcpServers").and_then(Value::as_object);
        for surface_id in &surface_ids {
            let key = format!("{prefix}-{surface_id}");
            if servers.is_some_and(|value| value.contains_key(&key))
                && !found.iter().any(|value| value == surface_id)
            {
                found.push((*surface_id).to_string());
            }
        }
    }
    Ok(found)
}

fn canonical_site_id(root: &Path, fallback: &str) -> String {
    for path in [
        root.join(".narada").join("site.json"),
        root.join("site.json"),
    ] {
        if let Ok(text) = fs::read_to_string(path) {
            if let Ok(value) = serde_json::from_str::<Value>(&text) {
                if let Some(site_id) = value
                    .get("site_id")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                {
                    return site_id.to_string();
                }
            }
        }
    }
    fallback.to_string()
}

fn site_mcp_control_root(root: &Path) -> PathBuf {
    if root
        .file_name()
        .is_some_and(|name| name.to_string_lossy().eq_ignore_ascii_case(".narada"))
        || root.join(".ai").join("mcp").exists()
    {
        return root.to_path_buf();
    }
    let nested = root.join(".narada");
    if nested.join(".ai").join("mcp").exists() {
        nested
    } else {
        root.to_path_buf()
    }
}
fn site_output_reader_closure_check(contract: &Value, args: &Value) -> Result<Value, String> {
    let include_ok = args.get("include_ok").and_then(Value::as_bool) == Some(true);
    let mut requested: Vec<(Option<String>, PathBuf)> = vec![];
    let mut add = |site_id: Option<String>, root: PathBuf| {
        let registry = capability_registry_path(&root);
        if !requested.iter().any(|(_, existing)| {
            comparable_root(&capability_registry_path(existing)) == comparable_root(&registry)
        }) {
            requested.push((site_id, canonical_root(root)));
        }
    };
    let catalog = site_catalog(contract);
    let sites = catalog["items"].as_array().cloned().unwrap_or_default();
    for site_id in argument_strings(args, "site_id", "site_ids") {
        let site = lookup_site_value(&sites, &site_id)?;
        add(
            Some(site["site_id"].as_str().unwrap_or(&site_id).to_string()),
            PathBuf::from(site["root"].as_str().unwrap_or("")),
        );
    }
    for root in argument_strings(args, "site_root", "site_roots") {
        let path = canonical_root(PathBuf::from(&root));
        let known = sites
            .iter()
            .find(|site| {
                let candidate = PathBuf::from(site["root"].as_str().unwrap_or(""));
                comparable_root(&path) == comparable_root(&candidate)
                    || comparable_root(&path) == comparable_root(&candidate.join(".narada"))
            })
            .and_then(|site| site["site_id"].as_str())
            .map(str::to_string);
        add(known, path);
    }
    if requested.is_empty() {
        return Err("registrar_requires_site_for_output_reader_closure_check".into());
    }
    let mut site_results = vec![];
    let mut violations = vec![];
    let mut missing_count = 0;
    let mut drift_count = 0;
    let mut checked_surface_count = 0;
    for (site_id, root) in &requested {
        let registry_path = capability_registry_path(root);
        if !registry_path.exists() {
            missing_count += 1;
            site_results.push(json!({"status":"missing","site_id":site_id,"site_root":path_text(root),"registry_path":path_text(&registry_path),"violation":"missing_registry"}));
            continue;
        }
        let registry = fs::read_to_string(&registry_path)
            .ok()
            .and_then(|text| serde_json::from_str::<Value>(&text).ok());
        let Some(registry) = registry else {
            drift_count += 1;
            let invalid = json!({"status":"drift","site_id":site_id,"site_root":path_text(root),"registry_path":path_text(&registry_path),"violation":"invalid_registry_json"});
            site_results.push(invalid.clone());
            violations.push(invalid);
            continue;
        };
        let check = output_reader_closure_for_registry(
            contract,
            &registry,
            site_id.as_deref(),
            Some(root),
            Some(&registry_path),
        );
        checked_surface_count += check["checked_surface_count"].as_u64().unwrap_or(0);
        if check["status"] == "drift" {
            drift_count += 1;
        }
        if let Some(items) = check["violations"].as_array() {
            violations.extend(items.iter().cloned());
        }
        if check["status"] != "ok" || include_ok {
            site_results.push(check);
        }
    }
    Ok(json!({
        "schema":"narada.registrar.site_output_reader_closure_check.v1",
        "status":if drift_count>0{"drift"}else if missing_count>0{"missing"}else{"ok"},
        "checked_site_count":requested.len(),
        "checked_surface_count":checked_surface_count,
        "missing_count":missing_count,
        "drift_count":drift_count,
        "violation_count":violations.len(),
        "violations":violations,
        "sites":site_results
    }))
}

fn output_reader_closure_for_registry(
    contract: &Value,
    registry: &Value,
    site_id: Option<&str>,
    site_root: Option<&Path>,
    registry_path: Option<&Path>,
) -> Value {
    let raw_surfaces = registry.get("surfaces");
    let mut violations = vec![];
    let mut producer_rule_count = 0;
    let context = |surface: Option<&Value>, producer: Option<&str>, reader: Option<&str>| {
        json!({
            "site_id":site_id,
            "site_root":site_root.map(path_text),
            "registry_path":registry_path.map(path_text),
            "surface_id":surface.and_then(|v|v["surface_id"].as_str()),
            "server_name":surface.and_then(|v|v["server_name"].as_str()),
            "catalog_surface_id":surface.and_then(|v|v["catalog_surface_id"].as_str()),
            "producer_tool":producer,
            "required_reader_tool":reader
        })
    };
    if !raw_surfaces.is_some_and(Value::is_array) {
        let mut violation = context(None, None, None);
        violation
            .as_object_mut()
            .unwrap()
            .insert("violation".into(), json!("invalid_registry_surfaces"));
        violations.push(violation);
    } else if let Some(surfaces) = raw_surfaces.and_then(Value::as_array) {
        for surface in surfaces {
            let registered = unique(
                surface["registered_live_tools"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_str),
            );
            let read_only = unique(
                surface
                    .pointer("/tool_contract/read_only_tools")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_str),
            );
            let closure = output_reader_closure(contract, surface, &registered);
            producer_rule_count += closure.len();
            for (producer, reader_value) in closure {
                let Some(reader) = reader_value.as_str() else {
                    continue;
                };
                if !registered.iter().any(|value| value == &producer) {
                    continue;
                }
                let base = context(Some(surface), Some(&producer), Some(reader));
                if !registered.iter().any(|value| value == reader) {
                    let mut violation = base.clone();
                    violation
                        .as_object_mut()
                        .unwrap()
                        .insert("violation".into(), json!("missing_registered_live_tool"));
                    violations.push(violation);
                }
                if !read_only.iter().any(|value| value == reader) {
                    let mut violation = base;
                    violation
                        .as_object_mut()
                        .unwrap()
                        .insert("violation".into(), json!("missing_read_only_admission"));
                    violations.push(violation);
                }
            }
        }
    }
    json!({"schema":"narada.registrar.output_reader_closure_check.v1","status":if violations.is_empty(){"ok"}else{"drift"},"site_id":site_id,"site_root":site_root.map(path_text),"registry_path":registry_path.map(path_text),"checked_surface_count":raw_surfaces.and_then(Value::as_array).map_or(0,Vec::len),"producer_rule_count":producer_rule_count,"violation_count":violations.len(),"violations":violations})
}

fn output_reader_closure(
    contract: &Value,
    surface: &Value,
    registered: &[String],
) -> serde_json::Map<String, Value> {
    let catalog_id = surface["catalog_surface_id"].as_str().unwrap_or("");
    let items = contract
        .pointer("/read_models/registrar_surface_list/items")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    if let Some(closure) = items
        .iter()
        .find(|item| item["id"] == catalog_id)
        .and_then(|item| item["output_reader_closure"].as_object())
    {
        return closure.clone();
    }
    items
        .iter()
        .filter_map(|item| item["output_reader_closure"].as_object())
        .find(|closure| closure.keys().any(|producer| registered.contains(producer)))
        .cloned()
        .unwrap_or_default()
}

fn argument_strings(args: &Value, singular: &str, plural: &str) -> Vec<String> {
    let values = args
        .get(singular)
        .and_then(Value::as_str)
        .into_iter()
        .chain(
            args.get(plural)
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(Value::as_str),
        );
    unique(
        values
            .filter(|value| !value.trim().is_empty())
            .map(str::trim),
    )
}

fn lookup_site_value(sites: &[Value], requested: &str) -> Result<Value, String> {
    if requested == "narada-andrey" || requested == "narada-user-site" {
        return Err("registrar_legacy_site_id_rejected:site_id".into());
    }
    for site in sites {
        let fallback = site["site_id"].as_str().unwrap_or("");
        let canonical = canonical_site_id(Path::new(site["root"].as_str().unwrap_or("")), fallback);
        if requested == fallback
            || requested == canonical
            || requested == format!("narada-{canonical}")
        {
            let mut found = site.clone();
            found
                .as_object_mut()
                .unwrap()
                .insert("site_id".into(), json!(canonical));
            return Ok(found);
        }
    }
    Err(format!("registrar_unknown_site:{requested}"))
}

fn capability_registry_path(root: &Path) -> PathBuf {
    let root = canonical_root(root.to_path_buf());
    root.join(".narada")
        .join("capabilities")
        .join("mcp-surfaces.json")
}
fn surface_usage(contract: &Value, args: &Value) -> Result<Value, String> {
    let surface_id = args
        .get("surface_id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| "registrar_requires_surface_id".to_string())?;
    let is_local = surface_id.ends_with(".local");
    let catalog = site_catalog(contract);
    let sites = catalog["items"].as_array().cloned().unwrap_or_default();
    let mut matching_sites = vec![];
    for site in &sites {
        let site_id = site["site_id"].as_str().unwrap_or("");
        if !is_local
            && (site["surfaces"]
                .as_array()
                .into_iter()
                .flatten()
                .any(|value| value == surface_id)
                || site_fabric_surface_ids(contract, site)
                    .iter()
                    .any(|value| value == surface_id))
        {
            matching_sites.push(json!({"site_id":site_id,"via":"shared"}));
        }
        if site_local_surface_ids(contract, site)
            .iter()
            .any(|value| value == surface_id)
        {
            matching_sites.push(json!({"site_id":site_id,"via":"local"}));
        }
    }
    let carriers = contract
        .pointer("/read_models/registrar_carrier_list/items")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut matching_carriers = vec![];
    for carrier in carriers {
        let carrier_id = carrier["carrier_id"].as_str().unwrap_or("");
        let kind = carrier["kind"].as_str().unwrap_or("");
        for binding in carrier["site_bindings"].as_array().into_iter().flatten() {
            let site_id = binding["site_id"].as_str().unwrap_or("");
            let Some(site) = sites.iter().find(|site| site["site_id"] == site_id) else {
                continue;
            };
            let shared = shared_surface_ids_for_binding(contract, binding, site);
            if !is_local && shared.iter().any(|value| value == surface_id) {
                matching_carriers.push(
                    json!({"carrier_id":carrier_id,"kind":kind,"via":"shared","site_id":site_id}),
                );
            }
            if is_local || binding["surfaces"] == "all" {
                for local in site_local_surface_ids(contract, site) {
                    if local != surface_id {
                        continue;
                    }
                    let includes = binding["surfaces"] == "all"
                        || binding["surfaces"]
                            .as_array()
                            .into_iter()
                            .flatten()
                            .any(|value| value == &local);
                    if includes {
                        matching_carriers.push(json!({"carrier_id":carrier_id,"kind":kind,"via":"local","site_id":site_id}));
                    }
                }
            }
        }
    }
    let mut deduped = vec![];
    for item in matching_carriers {
        if !deduped.iter().any(|existing: &Value| existing == &item) {
            deduped.push(item);
        }
    }
    let runtime_access = json!({
        "available": !matching_sites.is_empty(),
        "owner": "mcp-loader",
        "mode": "site-scoped",
        "carrier_binding_required": matching_sites.is_empty()
    });
    Ok(
        json!({"surface_id":surface_id,"is_local":is_local,"sites":matching_sites,"carriers":deduped,"site_count":matching_sites.len(),"carrier_count":deduped.len(),"runtime_access":runtime_access}),
    )
}

fn site_fabric_surface_ids(contract: &Value, site: &Value) -> Vec<String> {
    let root = PathBuf::from(site["root"].as_str().unwrap_or(""));
    let directory = site_mcp_control_root(&root).join(".ai").join("mcp");
    let known = contract
        .pointer("/read_models/registrar_surface_list/items")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let prefix = if site["site_id"] == "andrey-user" {
        "narada-site-andrey-user".to_string()
    } else {
        let id = site["site_id"].as_str().unwrap_or("");
        if id.starts_with("narada-") {
            id.to_string()
        } else {
            format!("narada-{id}")
        }
    };
    let mut found = vec![];
    let Ok(entries) = fs::read_dir(directory) else {
        return found;
    };
    for entry in entries.filter_map(Result::ok) {
        if entry.path().extension().and_then(|value| value.to_str()) != Some("json") {
            continue;
        }
        let Some(config) = fs::read_to_string(entry.path())
            .ok()
            .and_then(|text| serde_json::from_str::<Value>(&text).ok())
        else {
            continue;
        };
        for (key, server) in config["mcpServers"].as_object().into_iter().flatten() {
            let explicit = server["surface_id"].as_str().map(str::to_string);
            let inferred = key
                .strip_prefix(&(prefix.clone() + "-"))
                .map(str::to_string)
                .unwrap_or_else(|| key.clone());
            let id = explicit.unwrap_or(inferred);
            let canonical = known
                .iter()
                .find(|surface| surface["id"] == id)
                .and_then(|surface| surface["id"].as_str())
                .unwrap_or(&id)
                .to_string();
            if !found.contains(&canonical) {
                found.push(canonical)
            }
        }
    }
    found
}

fn site_local_surface_ids(contract: &Value, site: &Value) -> Vec<String> {
    let Some(path) = site["config_path"].as_str() else {
        return vec![];
    };
    let Ok(text) = fs::read_to_string(path) else {
        return vec![];
    };
    let Some(config) = parse_jsonc(&text) else {
        return vec![];
    };
    let entries = config
        .pointer("/structural_config/agent_execution_policy/allowed_mcp_entrypoints")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let allowlist = site["local_surface_allowlist"].as_array();
    let catalog = contract
        .pointer("/read_models/registrar_surface_list/items")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut result = vec![];
    for entry in entries {
        let Some(id) = entry["surface_id"]
            .as_str()
            .filter(|value| !value.is_empty())
        else {
            continue;
        };
        if allowlist.is_some_and(|values| !values.iter().any(|value| value == id)) {
            continue;
        }
        let canonical = id.trim_end_matches(".local").trim_end_matches("-mcp");
        if let Some(surface) = catalog.iter().find(|surface| surface["id"] == canonical) {
            let local = surface["projections"]
                .as_array()
                .into_iter()
                .flatten()
                .any(|projection| projection["injection_scope"] == "local_site");
            if !local {
                continue;
            }
        }
        if !result.iter().any(|value| value == id) {
            result.push(id.to_string())
        }
    }
    result
}

fn shared_surface_ids_for_binding(contract: &Value, binding: &Value, site: &Value) -> Vec<String> {
    let items = contract
        .pointer("/read_models/registrar_surface_list/items")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let enabled = |id: &str| {
        site.pointer(&format!("/surface_overrides/{id}/enabled"))
            .and_then(Value::as_bool)
            != Some(false)
    };
    let mut ids: Vec<String> = if binding["surfaces"] == "all" {
        items
            .iter()
            .filter_map(|surface| surface["id"].as_str())
            .filter(|id| enabled(id))
            .map(str::to_string)
            .collect()
    } else {
        binding["surfaces"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .filter(|id| !id.ends_with(".local") && enabled(id))
            .map(str::to_string)
            .collect()
    };
    if binding["loading_mode"] == "progressive" {
        for id in ["task-lifecycle", "surface-feedback"] {
            if enabled(id) && !ids.iter().any(|value| value == id) {
                ids.push(id.into())
            }
        }
    } else {
        for surface in &items {
            let Some(id) = surface["id"].as_str() else {
                continue;
            };
            if !enabled(id) {
                continue;
            }
            let automatic =
                surface["projections"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .any(|projection| {
                        projection["injection_scope"] == "local_site"
                            && projection["default_injection"] == "enabled"
                    });
            if automatic && !ids.iter().any(|value| value == id) {
                ids.push(id.into())
            }
        }
    }
    ids
}

fn parse_jsonc(text: &str) -> Option<Value> {
    let mut output = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    let mut quoted = false;
    let mut escaped = false;
    while let Some(ch) = chars.next() {
        if quoted {
            output.push(ch);
            if escaped {
                escaped = false
            } else if ch == '\\' {
                escaped = true
            } else if ch == '"' {
                quoted = false
            };
            continue;
        }
        if ch == '"' {
            quoted = true;
            output.push(ch);
            continue;
        }
        if ch == '/' && chars.peek() == Some(&'/') {
            chars.next();
            for next in chars.by_ref() {
                if next == '\n' {
                    output.push('\n');
                    break;
                }
            }
            continue;
        }
        if ch == '/' && chars.peek() == Some(&'*') {
            chars.next();
            let mut previous = '\0';
            for next in chars.by_ref() {
                if previous == '*' && next == '/' {
                    break;
                }
                previous = next
            }
            continue;
        }
        output.push(ch)
    }
    serde_json::from_str(&output).ok()
}
fn site_mcp_fabric_validate(contract: &Value, args: &Value) -> Result<Value, String> {
    let requested = required_argument(args, "site_id", "registrar_requires_site_id")?;
    let include_ok = args.get("include_ok").and_then(Value::as_bool) == Some(true);
    let catalog = site_catalog(contract);
    let sites = catalog["items"].as_array().cloned().unwrap_or_default();
    let site = lookup_site_value(&sites, &requested)?;
    let site_id = site["site_id"].as_str().unwrap_or(&requested);
    let root = PathBuf::from(site["root"].as_str().unwrap_or(""));
    let directory = site_mcp_control_root(&root).join(".ai").join("mcp");
    let surface_catalog = contract
        .pointer("/read_models/registrar_surface_list/items")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let servers = discover_fabric_servers(&directory, "site_fabric");
    let carrier_servers =
        discover_fabric_servers(&directory.join("carriers"), "carrier_projection");
    let mut findings = vec![];
    let mut add = |severity: &str, code: &str, message: String, detail: Value| {
        let mut finding = json!({"severity":severity,"code":code,"message":message});
        if let Some(values) = detail.as_object() {
            finding.as_object_mut().unwrap().extend(values.clone())
        }
        findings.push(finding)
    };
    if servers.is_empty() {
        add(
            "warning",
            "registrar_site_fabric_empty",
            format!(
                "No MCP servers found in {}",
                path_text(&root.join(".ai").join("mcp"))
            ),
            json!({"site_id":site_id}),
        );
    }
    let mut seen_keys = std::collections::HashSet::new();
    let mut seen_surfaces = std::collections::HashMap::<String, (String, String)>::new();
    let mut present = std::collections::HashSet::new();
    for server in &servers {
        let key = server["server_key"].as_str().unwrap_or("");
        let surface_id = server["surface_id"].as_str().unwrap_or(key);
        let file = server["source_file"].as_str().unwrap_or("");
        present.insert(surface_id.to_string());
        let detail = merge_value(
            json!({"site_id":site_id,"server_key":key,"source_file":file,"surface_id":surface_id}),
            server_scope_detail(&surface_catalog, server, surface_id, site_id, &root),
        );
        let known = surface_catalog
            .iter()
            .find(|surface| surface["id"] == surface_id);
        let has_embedded_descriptor =
            embedded_site_local_catalog(server, surface_id).is_some();
        if known.is_none()
            && server["surface_descriptor_path"].is_null()
            && !has_embedded_descriptor
        {
            add(
                "error",
                "registrar_site_local_descriptor_missing",
                format!("Site-local surface '{surface_id}' has no governed descriptor"),
                merge_value(
                    detail.clone(),
                    json!({"remediation":"Declare a Site-relative surface_descriptor_path on the Site-local MCP server entry."}),
                ),
            );
        }
        if !seen_keys.insert(key.to_string()) {
            add(
                "error",
                "registrar_site_fabric_duplicate_server_key",
                format!("Duplicate server key '{key}' in site fabric"),
                detail.clone(),
            );
        } else if include_ok {
            add(
                "info",
                "registrar_site_fabric_server_key_ok",
                format!("Server key '{key}' found"),
                detail.clone(),
            );
        }
        if known.is_some() {
            if let Some((old_key, old_file)) = seen_surfaces.get(surface_id) {
                add(
                    "error",
                    "registrar_site_fabric_duplicate_canonical_surface",
                    format!("Multiple Site fabric entries claim canonical surface '{surface_id}'"),
                    merge_value(
                        detail.clone(),
                        json!({"canonical_surface_id":surface_id,"conflicting_server_key":old_key,"conflicting_source_file":old_file,"remediation":format!("Remove the superseded projection from {} and rematerialize from authoritative Site registration.",path_text(&site_mcp_control_root(&root).join(".ai").join("mcp")))}),
                    ),
                );
            } else {
                seen_surfaces.insert(surface_id.to_string(), (key.to_string(), file.to_string()));
            }
        }
        let child_args = server["args"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .collect::<Vec<_>>();
        let entrypoint = server["entrypoint"].as_str().unwrap_or("");
        let unresolved = std::iter::once(entrypoint)
            .chain(child_args.iter().copied())
            .filter(|value| value.contains('{') && value.contains('}'))
            .collect::<Vec<_>>();
        if !unresolved.is_empty() {
            add(
                "error",
                "registrar_site_fabric_unresolved_template",
                format!("Surface {key} contains unresolved materialization tokens"),
                merge_value(
                    detail.clone(),
                    json!({"unresolved_values":unresolved,"remediation":"Regenerate the Site fabric from registrar materialization; do not defer placeholder expansion to the loader."}),
                ),
            );
        }
        let resolved = canonical_root(PathBuf::from(entrypoint));
        if !resolved.exists() {
            add(
                "error",
                "registrar_site_fabric_missing_entrypoint",
                format!(
                    "Entrypoint for '{key}' does not exist: {}",
                    path_text(&resolved)
                ),
                merge_value(detail.clone(), json!({"entrypoint":path_text(&resolved)})),
            );
        } else if include_ok {
            add(
                "info",
                "registrar_site_fabric_entrypoint_exists",
                format!("Entrypoint for '{key}' exists: {}", path_text(&resolved)),
                merge_value(detail.clone(), json!({"entrypoint":path_text(&resolved)})),
            );
        }
        add_runtime_preflight(
            &mut add,
            include_ok,
            merge_value(detail.clone(), json!({"entrypoint":path_text(&resolved)})),
            known,
            server["uses_runtime_proxy"].as_bool() == Some(true),
        );
        if [
            "local-filesystem",
            "git",
            "structured-command",
            "delegated-task",
            "worker-delegation",
        ]
        .contains(&surface_id)
        {
            let roots = flag_values(&child_args, "--allowed-root");
            if roots.is_empty() {
                add("error","registrar_site_fabric_missing_allowed_root",format!("Surface '{surface_id}' requires at least one --allowed-root but '{key}' has none"),detail.clone());
            } else if include_ok {
                add(
                    "info",
                    "registrar_site_fabric_allowed_root_ok",
                    format!(
                        "Surface '{surface_id}' on '{key}' has {} allowed root(s)",
                        roots.len()
                    ),
                    merge_value(detail.clone(), json!({"allowed_roots":roots})),
                );
            }
        }
        if surface_id == "local-filesystem" || surface_id == "local-filesystem-mcp.local" {
            if !child_args.contains(&"--output-root") {
                add(
                    "warning",
                    "registrar_site_fabric_missing_output_root",
                    format!("Filesystem surface '{key}' is missing --output-root"),
                    detail.clone(),
                );
            } else if include_ok {
                add(
                    "info",
                    "registrar_site_fabric_output_root_ok",
                    format!("Filesystem surface '{key}' has --output-root"),
                    detail.clone(),
                );
            }
        }
        if [
            "agent-context",
            "task-lifecycle",
            "site-inbox",
            "mailbox",
            "graph-mail",
            "delegated-task",
        ]
        .contains(&surface_id)
        {
            if !child_args.contains(&"--site-root") {
                add(
                    "error",
                    "registrar_site_fabric_missing_site_root",
                    format!("Surface '{surface_id}' on '{key}' is missing --site-root"),
                    detail.clone(),
                );
            } else if include_ok {
                add(
                    "info",
                    "registrar_site_fabric_site_root_ok",
                    format!("Surface '{surface_id}' on '{key}' has --site-root"),
                    detail.clone(),
                );
            }
        }
    }
    for server in &carrier_servers {
        let surface_id = server["surface_id"].as_str().unwrap_or("");
        let key = server["server_key"].as_str().unwrap_or("");
        let detail = json!({"site_id":site_id,"server_key":key,"surface_id":surface_id,"source_file":server["source_file"],"projection_kind":"carrier_projection"});
        let Some(authority) = surface_catalog
            .iter()
            .find(|surface| surface["id"] == surface_id)
        else {
            add(
                "error",
                "registrar_carrier_projection_unknown_surface",
                format!("Carrier projection '{key}' has no authoritative surface definition"),
                detail,
            );
            continue;
        };
        let actual = server["entrypoint"]
            .as_str()
            .unwrap_or("")
            .replace('\\', "/");
        let expected = authority["entrypoint"]
            .as_str()
            .unwrap_or("")
            .replace(
                "{mcp_surfaces_root}",
                &workspace_repo_root()
                    .map(|root| path_text(&root.join("packages")))
                    .unwrap_or_default(),
            )
            .replace('\\', "/");
        if actual != expected {
            add("error","registrar_carrier_projection_entrypoint_drift",format!("Carrier projection '{key}' does not use the authoritative '{surface_id}' entrypoint"),merge_value(detail.clone(),json!({"entrypoint":actual,"expected_entrypoint":expected,"authoritative_package":authority["package"]})));
        } else if include_ok {
            add(
                "info",
                "registrar_carrier_projection_entrypoint_ok",
                format!(
                    "Carrier projection '{key}' uses the authoritative '{surface_id}' entrypoint"
                ),
                detail.clone(),
            );
        }
        let values = server["args"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .collect::<Vec<_>>();
        if [
            "agent-context",
            "task-lifecycle",
            "site-inbox",
            "mailbox",
            "graph-mail",
            "delegated-task",
        ]
        .contains(&surface_id)
            && !values.contains(&"--site-root")
        {
            add(
                "error",
                "registrar_carrier_projection_missing_site_root",
                format!("Carrier projection '{key}' is missing required --site-root"),
                detail,
            );
        }
    }
    for surface in &surface_catalog {
        let Some(id) = surface["id"].as_str() else {
            continue;
        };
        if site
            .pointer(&format!("/surface_overrides/{id}/enabled"))
            .and_then(Value::as_bool)
            == Some(false)
        {
            continue;
        }
        let required = surface["projections"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|projection| {
                projection["injection_scope"] == "local_site"
                    && projection["default_injection"] == "enabled"
            });
        let Some(projection) = required else { continue };
        if present.contains(id) || (id == "task-lifecycle" && present.contains("work-lifecycle")) {
            continue;
        }
        add("error","registrar_site_fabric_missing_default_surface",format!("Default local Site surface '{id}' is missing from runtime-authoritative Site MCP fabric"),json!({"site_id":site_id,"surface_id":id,"projection_id":projection["id"],"default_injection":projection["default_injection"],"injection_scope":projection["injection_scope"],"expected_server_key":format!("{}-{id}",site_prefix(site_id)),"required_repair_locus":{"kind":"local_site","site_root":site["root"]},"remediation":format!("Materialize '{id}' with projection '{}' into {} before launching Site-bound sessions.",projection["id"].as_str().unwrap_or(""),path_text(&root.join(".ai").join("mcp")))}));
    }
    let errors = findings
        .iter()
        .filter(|finding| finding["severity"] == "error")
        .count();
    let warnings = findings
        .iter()
        .filter(|finding| finding["severity"] == "warning")
        .count();
    Ok(
        json!({"schema":"narada.registrar.site_fabric_validation.v1","status":if errors>0{"invalid"}else if warnings>0{"valid_with_warnings"}else{"valid"},"site_id":site_id,"server_count":servers.len(),"carrier_projection_count":carrier_servers.len(),"errors":errors,"warnings":warnings,"findings":findings,"bounded":true}),
    )
}

fn discover_fabric_servers(directory: &Path, projection_kind: &str) -> Vec<Value> {
    let mut result = vec![];
    let Ok(entries) = fs::read_dir(directory) else {
        return result;
    };
    for entry in entries.filter_map(Result::ok) {
        let path = entry.path();
        if path.extension().and_then(|value| value.to_str()) != Some("json") {
            continue;
        }
        let Some(file) = path.file_name().and_then(|value| value.to_str()) else {
            continue;
        };
        let Some(config) = fs::read_to_string(&path)
            .ok()
            .and_then(|text| serde_json::from_str::<Value>(&text).ok())
        else {
            continue;
        };
        for (key, server) in config["mcpServers"].as_object().into_iter().flatten() {
            let raw_command = server["command"].as_str().unwrap_or("node");
            let raw_args = server["args"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect::<Vec<_>>();
            let launch = unwrap_launch(raw_command, &raw_args);
            let args = if launch.proxied {
                let separator = raw_args.iter().position(|value| value == "--");
                separator
                    .map(|index| raw_args[index + 1..].to_vec())
                    .unwrap_or_default()
            } else {
                raw_args.iter().skip(1).cloned().collect()
            };
            let surface_id = server["surface_id"].as_str().unwrap_or(key);
            result.push(json!({"server_key":key,"surface_id":surface_id,"entrypoint":launch.entrypoint,"args":args,"uses_runtime_proxy":launch.proxied,"surface_descriptor_path":server.get("surface_descriptor_path"),"narada_scope":server.get("narada_scope"),"surface_projection":server.get("surface_projection"),"source_file":if projection_kind=="carrier_projection"{format!("carriers/{file}")}else{file.to_string()},"projection_kind":projection_kind}));
        }
    }
    result
}
fn server_scope_detail(
    catalog: &[Value],
    server: &Value,
    surface_id: &str,
    site_id: &str,
    root: &Path,
) -> Value {
    let projection_id = server
        .pointer("/surface_projection/projection_id")
        .and_then(Value::as_str)
        .unwrap_or("default");
    let projection = catalog
        .iter()
        .find(|surface| surface["id"] == surface_id)
        .and_then(|surface| surface["projections"].as_array())
        .and_then(|items| {
            items
                .iter()
                .find(|projection| projection["id"] == projection_id)
        });
    let computed = projection.map(|value| scope_metadata(value, root)).unwrap_or_else(|| {
        json!({"injection_scope":"local_site","authority_locus":{"kind":"local_site","site_root":path_text(root)},"mutation_locus":{"kind":"local_site","site_root":path_text(root)},"restart_owner":"local_site"})
    });
    let raw = server["narada_scope"].as_object();
    let injection = raw
        .and_then(|value| value.get("injection_scope"))
        .cloned()
        .unwrap_or_else(|| computed["injection_scope"].clone());
    let authority = raw
        .and_then(|value| value.get("authority_locus"))
        .cloned()
        .unwrap_or_else(|| computed["authority_locus"].clone());
    let mutation = raw
        .and_then(|value| value.get("mutation_locus"))
        .cloned()
        .unwrap_or_else(|| computed["mutation_locus"].clone());
    let restart = raw
        .and_then(|value| value.get("restart_owner"))
        .cloned()
        .unwrap_or_else(|| computed["restart_owner"].clone());
    let bound = raw
        .and_then(|value| value.get("bound_into_site"))
        .cloned()
        .unwrap_or_else(|| json!(site_id));
    let source = if raw.is_some() {
        json!("site_config_narada_scope")
    } else {
        json!("registrar_surface_catalog")
    };
    let narada = json!({"injection_scope":injection,"authority_locus":authority,"mutation_locus":mutation,"restart_owner":restart,"bound_into_site":bound,"scope_source":source});
    json!({"injection_scope":injection,"authority_locus":authority,"mutation_locus":mutation,"restart_owner":restart,"bound_into_site":bound,"scope_source":source,"narada_scope":narada,"diagnostic_class":if injection=="host"{"host_injected_surface_missing_or_misconfigured_in_session"}else if injection=="user_site"{"user_site_injected_surface_missing_or_misconfigured_in_session"}else{"local_site_surface_missing_or_misconfigured"},"required_repair_locus":mutation})
}

fn scope_finding_detail(scope: Value) -> Value {
    let injection = scope["injection_scope"].as_str().unwrap_or("local_site");
    let diagnostic_class = if injection == "host" {
        "host_injected_surface_missing_or_misconfigured_in_session"
    } else if injection == "user_site" {
        "user_site_injected_surface_missing_or_misconfigured_in_session"
    } else {
        "local_site_surface_missing_or_misconfigured"
    };
    let mut detail = scope.clone();
    if let Some(object) = detail.as_object_mut() {
        object.insert("narada_scope".into(), scope.clone());
        object.insert("diagnostic_class".into(), json!(diagnostic_class));
        object.insert(
            "required_repair_locus".into(),
            scope["mutation_locus"].clone(),
        );
    }
    detail
}
fn add_runtime_preflight(
    add: &mut impl FnMut(&str, &str, String, Value),
    include_ok: bool,
    detail: Value,
    surface: Option<&Value>,
    proxied: bool,
) {
    let Some(workspace) = workspace_repo_root() else {
        return;
    };
    if proxied {
        let manifest = workspace
            .join(".ai")
            .join("runtime")
            .join("workspace-artifact-manifest.json");
        let manifest_text = manifest.to_string_lossy().replace('\\', "/");
        if manifest.exists() {
            if include_ok {
                add(
                    "info",
                    "registrar_workspace_artifact_manifest_exists",
                    format!("Workspace artifact manifest exists: {manifest_text}"),
                    merge_value(
                        detail.clone(),
                        json!({"artifact_manifest_path":manifest_text}),
                    ),
                );
            }
        } else {
            add(
                "error",
                "registrar_workspace_artifact_manifest_missing",
                format!("Workspace artifact manifest does not exist: {manifest_text}"),
                merge_value(
                    detail.clone(),
                    json!({"artifact_manifest_path":manifest_text,"remediation":"Run cargo native-package from mcp-surfaces before launching carrier MCPs."}),
                ),
            );
        }
        let proxy = native_proxy_entrypoint().unwrap_or_default();
        if Path::new(&proxy).exists() {
            if include_ok {
                add(
                    "info",
                    "registrar_runtime_proxy_exists",
                    format!("Runtime proxy exists: {proxy}"),
                    merge_value(
                        detail.clone(),
                        json!({"runtime_proxy_entrypoint":proxy,"runtime_proxy_implementation":"native"}),
                    ),
                );
            }
        } else {
            add(
                "error",
                "registrar_runtime_proxy_missing",
                format!("Runtime proxy does not exist: {proxy}"),
                merge_value(
                    detail.clone(),
                    json!({"runtime_proxy_entrypoint":proxy,"runtime_proxy_implementation":"native","remediation":"Run cargo native-package from mcp-surfaces before launching carrier MCPs."}),
                ),
            );
        }
    }
    let Some(surface) = surface else { return };
    for check in runtime_dependency_checks(&workspace, surface) {
        let dependency = check["dependency"].as_str().unwrap_or("").to_string();
        let export = check["export_path"].as_str().unwrap_or("").to_string();
        let mut finding_detail = check.clone();
        finding_detail.as_object_mut().unwrap().remove("exists");
        if check["exists"].as_bool() == Some(true) {
            if include_ok {
                add(
                    "info",
                    "registrar_runtime_dependency_exists",
                    format!("Runtime dependency export for '{dependency}' exists: {export}"),
                    merge_value(detail.clone(), finding_detail),
                );
            }
        } else {
            add(
                "error",
                "registrar_runtime_dependency_missing",
                format!("Runtime dependency export for '{dependency}' does not exist: {export}"),
                merge_value(
                    detail.clone(),
                    merge_value(
                        finding_detail,
                        json!({"remediation":format!("Run cargo native-package from mcp-surfaces before launching carrier MCPs; missing native dependency: {dependency}.")}),
                    ),
                ),
            );
        }
    }
}
fn runtime_dependency_checks(workspace: &Path, surface: &Value) -> Vec<Value> {
    let package = surface["package"].as_str().unwrap_or("");
    let package_root = workspace.join("packages").join(package);
    let Some(manifest) = fs::read_to_string(package_root.join("package.json"))
        .ok()
        .and_then(|text| serde_json::from_str::<Value>(&text).ok())
    else {
        return vec![];
    };
    let mut result = vec![];
    for dependency in manifest["dependencies"]
        .as_object()
        .into_iter()
        .flatten()
        .map(|(name, _)| name)
        .filter(|name| name.starts_with("@narada-core/mcp-"))
    {
        let name = dependency.trim_start_matches("@narada-core/");
        let shared = workspace.join("packages").join("shared").join(name);
        let dependency_root = if shared.join("package.json").exists() {
            shared
        } else {
            workspace.join("packages").join(name)
        };
        let package_path = dependency_root.join("package.json");
        let Some(package_json) = fs::read_to_string(&package_path)
            .ok()
            .and_then(|text| serde_json::from_str::<Value>(&text).ok())
        else {
            result.push(json!({"dependency":dependency,"package_root":dependency_root.to_string_lossy().replace('\\',"/"),"export_path":package_path.to_string_lossy().replace('\\',"/"),"exists":false}));
            continue;
        };
        for target in export_targets(&package_json) {
            let export = dependency_root.join(target.trim_start_matches("./"));
            let export_text = export.to_string_lossy().replace('\\', "/");
            result.push(json!({"dependency":dependency,"package_root":dependency_root.to_string_lossy().replace('\\',"/"),"export_path":export_text,"exists":export_target_exists(&export)}));
        }
    }
    result
}
fn export_targets(package: &Value) -> Vec<String> {
    let mut result = vec![];
    match &package["exports"] {
        Value::String(value) => result.push(value.clone()),
        Value::Object(values) => {
            for value in values.values() {
                if let Some(target) = value.as_str().or_else(|| value["default"].as_str()) {
                    if !result.iter().any(|item| item == target) {
                        result.push(target.into())
                    }
                }
            }
        }
        _ => {}
    }
    result
}
fn export_target_exists(path: &Path) -> bool {
    let text = path.to_string_lossy();
    let Some(index) = text.find(['*', '?']) else {
        return path.exists();
    };
    let prefix = &text[..index];
    let directory = if prefix.ends_with(['/', '\\']) {
        PathBuf::from(&prefix[..prefix.len() - 1])
    } else {
        PathBuf::from(prefix)
            .parent()
            .unwrap_or(Path::new(""))
            .to_path_buf()
    };
    fs::read_dir(directory)
        .ok()
        .is_some_and(|mut entries| entries.next().is_some())
}
fn flag_values<'a>(args: &[&'a str], flag: &str) -> Vec<&'a str> {
    let mut result = vec![];
    let mut index = 0;
    while index < args.len() {
        if args[index] == flag && index + 1 < args.len() {
            result.push(args[index + 1]);
            index += 2
        } else {
            index += 1
        }
    }
    result
}
fn merge_value(mut left: Value, right: Value) -> Value {
    if let (Some(target), Some(source)) = (left.as_object_mut(), right.as_object()) {
        target.extend(source.clone())
    }
    left
}

fn site_bind(contract: &Value, args: &Value) -> Result<Value, String> {
    let site_id = required_argument(args, "site_id", "registrar_requires_site_id")?;
    let surface_id = required_argument(args, "surface_id", "registrar_requires_surface_id")?;
    let catalog = site_catalog(contract);
    let sites = catalog["items"].as_array().cloned().unwrap_or_default();
    let site = lookup_site_value(&sites, &site_id)?;
    let surfaces = contract
        .pointer("/read_models/registrar_surface_list/items")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let surface = surfaces
        .iter()
        .find(|surface| surface["id"] == surface_id)
        .ok_or_else(|| format!("registrar_unknown_surface:{surface_id}"))?;
    if site
        .pointer(&format!("/surface_overrides/{surface_id}/enabled"))
        .and_then(Value::as_bool)
        == Some(false)
        && args.get("allow_disabled_sidecar").and_then(Value::as_bool) != Some(true)
    {
        return Ok(
            json!({"status":"refused","reason_code":"registrar_site_bind_refused_surface_disabled","site_id":site_id,"surface_id":surface_id,"sidecar_state":"disabled_by_site_override","reason":"This Site explicitly disables the requested surface, so registrar_site_bind will not materialize a sidecar for it.","required_next_step":"Enable the surface in the Site override or pass allow_disabled_sidecar=true only for an intentional compatibility sidecar."}),
        );
    }
    let config_dir = site_mcp_control_root(Path::new(site["root"].as_str().unwrap_or("")))
        .join(".ai")
        .join("mcp");
    let aggregate = format!("{site_id}-mcp.json");
    let aggregate_exists = config_dir.join(&aggregate).exists();
    if aggregate_exists && args.get("allow_sidecar").and_then(Value::as_bool) != Some(true) {
        return Ok(
            json!({"status":"refused","reason_code":"registrar_site_bind_refused_aggregate_fabric_exists","site_id":site_id,"surface_id":surface_id,"aggregate_file":aggregate,"reason":"This Site has an authoritative aggregate MCP fabric. registrar_site_bind would create a sidecar snippet, so it is refused unless allow_sidecar is explicitly true.","required_next_step":"Update the aggregate MCP fabric through the Site materialization path, or pass allow_sidecar=true only for an intentional compatibility sidecar."}),
        );
    }
    let projection_id = args
        .get("projection_id")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty());
    let runtime_kind = args
        .get("runtime_kind")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty());
    let projection = select_projection(surface, projection_id, runtime_kind)?;
    fs::create_dir_all(&config_dir).map_err(|error| error.to_string())?;
    let prefix = site_prefix(&site_id);
    let server_key = format!("{prefix}-{surface_id}");
    let binding_id = format!("{site_id}-{surface_id}");
    let file_name = format!("{prefix}-{surface_id}-mcp.json");
    let config = build_bind_config(
        contract,
        &site,
        surface,
        projection,
        runtime_kind,
        &server_key,
    )?;
    let config_path = config_dir.join(&file_name);
    let rendered = serde_json::to_string_pretty(&config).map_err(|error| error.to_string())? + "\n";
    let binding_changed =
        fs::read_to_string(&config_path).ok().as_deref() != Some(rendered.as_str());
    fs::write(&config_path, rendered).map_err(|error| error.to_string())?;
    let registry_result = write_site_registry(contract, &site)?;
    Ok(json!({
        "status":"bound","site_id":site_id,"surface_id":surface_id,"projection_id":projection["id"],"file":file_name,"server_key":server_key,"binding_id":binding_id,"registry":registry_result,
        "activation":{
            "status":if binding_changed {"carrier_rematerialization_required"} else {"binding_unchanged_verify_carrier_admission"},
            "reason":if binding_changed {"The Site binding changed, while already materialized carrier admission envelopes are immutable snapshots."} else {"The Site binding is unchanged. A current carrier may use it only if its admission envelope already contains this binding."},
            "site_binding_ready":true,"binding_changed":binding_changed,"carrier_rematerialization_required":binding_changed,"carrier_restart_required":binding_changed,
            "next_steps":[
                {"order":1,"action":"rematerialize_carriers","owner":"narada-mcp-materializer","instruction":"Run the authoritative all-carrier materialization or recover-generation command."},
                {"order":2,"action":"restart_carrier","owner":"carrier","instruction":"Restart the carrier after successful materialization."},
                {"order":3,"action":"open_surface","owner":"mcp-loader","instruction":"Open the binding by canonical binding_id after restart.","tool":"mcp_loader_open_surface","arguments":{"site_root":site["root"],"binding_id":binding_id,"surface_id":surface_id}}
            ]
        }
    }))
}

fn site_unbind(contract: &Value, args: &Value) -> Result<Value, String> {
    let site_id = required_argument(args, "site_id", "registrar_requires_site_id")?;
    let surface_id = required_argument(args, "surface_id", "registrar_requires_surface_id")?;
    let catalog = site_catalog(contract);
    let sites = catalog["items"].as_array().cloned().unwrap_or_default();
    let site = lookup_site_value(&sites, &site_id)?;
    let directory = site_mcp_control_root(Path::new(site["root"].as_str().unwrap_or("")))
        .join(".ai")
        .join("mcp");
    if !directory.exists() {
        return Ok(json!({"status":"not_found","site_id":site_id,"surface_id":surface_id}));
    }
    let key = format!("{}-{surface_id}", site_prefix(&site_id));
    if let Ok(entries) = fs::read_dir(&directory) {
        for entry in entries.filter_map(Result::ok) {
            if entry.path().extension().and_then(|value| value.to_str()) != Some("json") {
                continue;
            }
            let Some(config) = fs::read_to_string(entry.path())
                .ok()
                .and_then(|text| serde_json::from_str::<Value>(&text).ok())
            else {
                continue;
            };
            if config["mcpServers"].get(&key).is_some() {
                let file = entry.file_name().to_string_lossy().to_string();
                fs::remove_file(entry.path()).map_err(|error| error.to_string())?;
                let registry = write_site_registry(contract, &site)?;
                return Ok(
                    json!({"status":"unbound","site_id":site_id,"surface_id":surface_id,"file":file,"registry":registry}),
                );
            }
        }
    }
    Ok(json!({"status":"not_bound","site_id":site_id,"surface_id":surface_id}))
}

fn write_site_registry(contract: &Value, site: &Value) -> Result<Value, String> {
    let registry = build_site_surface_registry(contract, site)?;
    let path = capability_registry_path(Path::new(site["root"].as_str().unwrap_or("")));
    fs::create_dir_all(
        path.parent()
            .ok_or("registrar_site_registry_path_invalid")?,
    )
    .map_err(|error| error.to_string())?;
    fs::write(
        &path,
        serde_json::to_string_pretty(&registry).map_err(|error| error.to_string())? + "\n",
    )
    .map_err(|error| error.to_string())?;
    let surfaces = registry["surfaces"].as_array().cloned().unwrap_or_default();
    let tools = surfaces
        .iter()
        .map(|surface| {
            surface["registered_live_tools"]
                .as_array()
                .map_or(0, Vec::len)
        })
        .sum::<usize>();
    Ok(
        json!({"status":"synced","site_id":site["site_id"],"path":path_text(&path),"surface_count":surfaces.len(),"tool_count":tools}),
    )
}

fn required_argument(args: &Value, name: &str, code: &str) -> Result<String, String> {
    args.get(name)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .ok_or_else(|| code.to_string())
}
fn site_prefix(site_id: &str) -> String {
    if site_id == "andrey-user" {
        "narada-site-andrey-user".into()
    } else if site_id.starts_with("narada-") {
        site_id.into()
    } else {
        format!("narada-{site_id}")
    }
}

fn select_projection<'a>(
    surface: &'a Value,
    projection_id: Option<&str>,
    runtime_kind: Option<&str>,
) -> Result<&'a Value, String> {
    let projections = surface["projections"]
        .as_array()
        .ok_or("registrar_surface_projection_required")?;
    if let Some(id) = projection_id {
        return projections
            .iter()
            .find(|projection| projection["id"] == id)
            .ok_or_else(|| {
                format!(
                    "registrar_unknown_surface_projection:{}:{id}",
                    surface["id"].as_str().unwrap_or("")
                )
            });
    }
    if let Some(kind) = runtime_kind {
        let matches = projections
            .iter()
            .filter(|projection| {
                projection["runtime_requirements"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .any(|value| value == kind)
            })
            .collect::<Vec<_>>();
        if matches.len() == 1 {
            return Ok(matches[0]);
        }
        let neutral = projections
            .iter()
            .filter(|projection| {
                projection["runtime_requirements"]
                    .as_array()
                    .is_none_or(Vec::is_empty)
            })
            .collect::<Vec<_>>();
        if neutral.len() == 1 {
            return Ok(neutral[0]);
        }
    }
    if projections.len() == 1 {
        return Ok(&projections[0]);
    }
    Err(format!(
        "registrar_surface_projection_required:{}",
        surface["id"].as_str().unwrap_or("")
    ))
}

fn build_bind_config(
    _contract: &Value,
    site: &Value,
    surface: &Value,
    projection: &Value,
    runtime_kind: Option<&str>,
    server_key: &str,
) -> Result<Value, String> {
    let site_id = site["site_id"].as_str().unwrap_or("");
    let surface_id = surface["id"].as_str().unwrap_or("");
    let root = canonical_root(PathBuf::from(site["root"].as_str().unwrap_or("")));
    let workspace = site_workspace_root(site);
    let source_args = projection
        .get("args")
        .and_then(Value::as_array)
        .or_else(|| surface["args"].as_array())
        .cloned()
        .unwrap_or_default();
    let mut child_args = source_args
        .iter()
        .filter_map(Value::as_str)
        .map(|value| interpolate(value, site_id, &root, &workspace))
        .collect::<Vec<_>>();
    append_durable_worker_allowed_roots(surface_id, &root, &mut child_args)?;
    if projection["id"] == "user-site-operator" {
        child_args.extend(
            [
                "--projection",
                "user-site-operator",
                "--user-site-root",
                &path_text(&user_site_root()),
                "--source-kind",
                "operator",
                "--operator-id",
                &default_operator_id(),
            ]
            .map(str::to_string),
        );
    }
    let entrypoint_template = projection["entrypoint"]
        .as_str()
        .or_else(|| surface["entrypoint"].as_str())
        .unwrap_or("");
    let child_entrypoint = canonical_root(PathBuf::from(interpolate(
        entrypoint_template,
        site_id,
        &root,
        &workspace,
    )));
    let implementation = site
        .pointer(&format!(
            "/surface_overrides/{surface_id}/surface_implementation"
        ))
        .and_then(Value::as_str);
    let launch = site_launch(
        surface_id,
        projection,
        implementation,
        &path_text(&child_entrypoint),
        &child_args,
    )?;
    let exposed = projection["exposed_tools"]
        .as_array()
        .cloned()
        .unwrap_or_else(|| surface["tools"].as_array().cloned().unwrap_or_default());
    let scope = scope_metadata(projection, &root);
    let mut envs = surface["env_vars"].as_array().cloned().unwrap_or_default();
    envs.extend(
        projection["env_vars"]
            .as_array()
            .cloned()
            .unwrap_or_default(),
    );
    let envs = unique(envs.iter().filter_map(Value::as_str));
    let projection_metadata = projection_metadata(surface, projection, runtime_kind);
    Ok(
        json!({"schema":"narada.mcp.client_config.v0","site_id":site_id,"description":format!("{} MCP surface bound by registrar.",surface["package"].as_str().unwrap_or("")),"mcpServers":{server_key:{"transport":"stdio","command":launch.0,"args":launch.1,"tools":exposed,"env_vars":envs,"surface_id":surface_id,"projection_id":projection["id"],"surface_projection":projection_metadata,"authority_posture":if scope["injection_scope"]=="local_site"{"site_local_mcp_surface"}else{"injected_mcp_surface"},"injection_scope":scope["injection_scope"],"authority_locus":scope["authority_locus"],"mutation_locus":scope["mutation_locus"],"restart_owner":scope["restart_owner"],"bound_into_site":site_id,"narada_scope":{"injection_scope":scope["injection_scope"],"authority_locus":scope["authority_locus"],"mutation_locus":scope["mutation_locus"],"restart_owner":scope["restart_owner"],"bound_into_site":site_id,"scope_source":"registrar_surface_catalog"}}}}),
    )
}

fn site_workspace_root(site: &Value) -> PathBuf {
    let config = site["config_path"]
        .as_str()
        .and_then(|path| fs::read_to_string(path).ok())
        .and_then(|text| serde_json::from_str::<Value>(&text).ok());
    let configured = config.as_ref().and_then(|value| {
        value["workspace_root"].as_str().or_else(|| {
            value
                .pointer("/site/workspace_root")
                .and_then(Value::as_str)
        })
    });
    canonical_root(PathBuf::from(
        configured.unwrap_or_else(|| site["root"].as_str().unwrap_or("")),
    ))
}

fn append_durable_worker_allowed_roots(
    surface_id: &str,
    site_root: &Path,
    child_args: &mut Vec<String>,
) -> Result<(), String> {
    if surface_id != "worker-delegation" {
        return Ok(());
    }
    let extras = durable_extra_allowed_roots(site_root)?;
    if extras.is_empty() {
        return Ok(());
    }
    let site_root_key = comparable_root(site_root);
    let mut filtered = Vec::with_capacity(child_args.len());
    let mut index = 0;
    while index < child_args.len() {
        if index + 1 < child_args.len()
            && child_args[index] == "--allowed-root"
            && comparable_root(Path::new(&child_args[index + 1])) == site_root_key
        {
            index += 2;
            continue;
        }
        filtered.push(child_args[index].clone());
        index += 1;
    }
    *child_args = filtered;
    let existing = child_args
        .windows(2)
        .filter(|pair| pair[0] == "--allowed-root")
        .map(|pair| comparable_root(Path::new(&pair[1])))
        .collect::<std::collections::BTreeSet<_>>();
    for extra in extras {
        if existing.contains(&comparable_root(&extra)) {
            continue;
        }
        child_args.push("--allowed-root".to_string());
        child_args.push(path_text(&extra));
    }
    Ok(())
}

fn durable_extra_allowed_roots(site_root: &Path) -> Result<Vec<PathBuf>, String> {
    let path = site_root.join(".narada").join("allowed-roots.json");
    if !path.exists() {
        return Ok(Vec::new());
    }
    let bytes = fs::read(&path).map_err(|error| {
        format!(
            "registrar_allowed_roots_read_failed:{}:{error}",
            path_text(&path)
        )
    })?;
    let document: Value = serde_json::from_slice(&bytes).map_err(|error| {
        format!(
            "registrar_allowed_roots_invalid_json:{}:{error}",
            path_text(&path)
        )
    })?;
    let Some(entries) = document.get("extra_allowed_roots") else {
        return Ok(Vec::new());
    };
    let entries = entries.as_array().ok_or_else(|| {
        format!(
            "registrar_allowed_roots_invalid_extra_allowed_roots:{}",
            path_text(&path)
        )
    })?;
    let mut roots = Vec::new();
    let mut seen = std::collections::BTreeSet::new();
    for (index, entry) in entries.iter().enumerate() {
        let value = entry
            .as_str()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| {
                format!(
                    "registrar_allowed_roots_invalid_entry:{}:{index}",
                    path_text(&path)
                )
            })?;
        let root = PathBuf::from(value);
        if !root.is_absolute() {
            return Err(format!(
                "registrar_allowed_roots_entry_not_absolute:{}:{index}",
                path_text(&path)
            ));
        }
        let root = canonical_root(root);
        if seen.insert(comparable_root(&root)) {
            roots.push(root);
        }
    }
    Ok(roots)
}

fn interpolate(value: &str, site_id: &str, root: &Path, workspace: &Path) -> String {
    let control = if root
        .file_name()
        .is_some_and(|name| name.to_string_lossy().eq_ignore_ascii_case(".narada"))
    {
        root.to_path_buf()
    } else {
        root.join(".narada")
    };
    value
        .replace(
            "{mcp_surfaces_root}",
            &workspace_repo_root()
                .map(|root| path_text(&root.join("packages")))
                .unwrap_or_default(),
        )
        .replace("{site_root}", &path_text(root))
        .replace("{site_control_root}", &path_text(&control))
        .replace("{site_runtime_root}", &path_text(&control.join("runtime")))
        .replace("{workspace_root}", &path_text(workspace))
        .replace("{site_id}", site_id)
}
fn default_operator_id() -> String {
    user_site_root()
        .parent()
        .and_then(Path::file_name)
        .and_then(|value| value.to_str())
        .unwrap_or("operator")
        .to_ascii_lowercase()
}
fn workspace_repo_root() -> Option<PathBuf> {
    let executable = env::current_exe().ok()?;
    executable
        .ancestors()
        .find(|root| root.join("packages").join("mcp-registrar").exists())
        .map(Path::to_path_buf)
}

fn runtime_implementation_matrix_path(workspace: &Path) -> Result<PathBuf, String> {
    const MATRIX_RELATIVE_PATH: &str =
        "narada/packages/operator-surface-runtime-contract/contracts/runtime-implementation-matrix.json";
    workspace
        .ancestors()
        .find(|candidate| candidate.join(MATRIX_RELATIVE_PATH).is_file())
        .map(|candidate| candidate.join(MATRIX_RELATIVE_PATH))
        .ok_or_else(|| {
            format!(
                "registrar_runtime_matrix_unavailable:{}",
                path_text(workspace)
            )
        })
}

fn scope_metadata(projection: &Value, root: &Path) -> Value {
    let injection = projection["injection_scope"]
        .as_str()
        .unwrap_or("local_site");
    let locus = if injection == "host" {
        json!({"kind":"host"})
    } else if injection == "user_site" {
        json!({"kind":"user_site","site_root":path_text(&user_site_root())})
    } else {
        json!({"kind":"local_site","site_root":path_text(root)})
    };
    json!({"injection_scope":injection,"authority_locus":locus,"mutation_locus":locus,"restart_owner":projection["restart_owner"].as_str().unwrap_or(injection)})
}
fn projection_metadata(surface: &Value, projection: &Value, runtime_kind: Option<&str>) -> Value {
    let tools = projection["exposed_tools"]
        .as_array()
        .cloned()
        .unwrap_or_else(|| surface["tools"].as_array().cloned().unwrap_or_default());
    let descriptor = &surface["descriptor"];
    let mut value = json!({"surface_id":surface["id"],"projection_id":projection["id"],"injection_scope":projection["injection_scope"],"runtime_requirements":projection.get("runtime_requirements").cloned().unwrap_or_else(||json!([])),"exposed_tools":tools,"execution":projection["execution"],"descriptor_digest":surface["descriptor_digest"],"tool_contract_digest":surface["tool_contract_digest"],"surface_descriptor":descriptor});
    for key in ["default_injection"] {
        if let Some(item) = projection.get(key) {
            value
                .as_object_mut()
                .unwrap()
                .insert(key.into(), item.clone());
        }
    }
    if let Some(kind) = runtime_kind {
        value
            .as_object_mut()
            .unwrap()
            .insert("runtime_kind".into(), json!(kind));
    }
    if let Some(lifecycle) = descriptor["projections"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|candidate| candidate["id"] == projection["id"])
        .and_then(|candidate| candidate.get("lifecycle"))
    {
        value
            .as_object_mut()
            .unwrap()
            .insert("lifecycle".into(), lifecycle.clone());
    }
    value
}

fn site_launch(
    surface_id: &str,
    projection: &Value,
    implementation: Option<&str>,
    entrypoint: &str,
    args: &[String],
) -> Result<(String, Vec<String>), String> {
    let component = component_kind(surface_id);
    let engine = runtime_engine(&component, implementation)?;
    let proxy = native_proxy_entrypoint().ok_or("registrar_native_runtime_proxy_missing")?;
    let mut effective_command = if engine == "rust" {
        projection["command"]
            .as_str()
            .filter(|value| !value.trim().is_empty())
            .ok_or("registrar_native_projection_command_missing")?
            .to_string()
    } else {
        return Err(format!(
            "registrar_non_native_runtime_retired:{engine}"
        ));
    };
    let mut effective_entrypoint = entrypoint.to_string();
    let mut effective_args = args.to_vec();
    let mut invocation = None;
    let mut applet = None;
    let shared = [
        "catalog-observation",
        "operator-routing",
        "site-inbox",
        "site-lifecycle",
        "site-registry",
        "project-state",
        "epistemic-graph",
        "runtime-introspection",
        "site-coherence",
        "launcher",
        "mailbox",
        "graph-mail",
        "calendar",
        "worker-delegation",
        "delegated-task",
        "sop",
        "scheduler",
        "surface-feedback",
        "speech",
        "artifacts",
        "nars-session",
        "quota-meter",
        "operator-console-overlay",
        "browser-control",
        "cloudflare-carrier",
    ]
    .contains(&surface_id);
    if engine == "rust" {
        if ["local-filesystem", "structured-command", "git"].contains(&surface_id) {
            effective_command = proxy.clone();
            effective_entrypoint = proxy.clone();
            invocation = Some("native_applet");
            applet = Some(if surface_id == "local-filesystem" {
                "filesystem"
            } else {
                surface_id
            });
        } else if surface_id == "mcp-loader" {
            let path = native_artifact_entrypoint("mcp-loader-mcp", "narada-mcp-loader.exe")
                .ok_or("registrar_native_mcp_loader_missing")?;
            effective_command = path.clone();
            effective_entrypoint = path;
            invocation = Some("native_entrypoint");
        } else if surface_id == "task-lifecycle" || surface_id == "work-lifecycle" {
            let artifact = if surface_id == "task-lifecycle" {
                "narada-task-lifecycle-mcp.exe"
            } else {
                "narada-work-lifecycle-mcp.exe"
            };
            let path = native_artifact_entrypoint("shared/mcp-lifecycle-native", artifact)
                .ok_or("registrar_native_lifecycle_missing")?;
            effective_command = path.clone();
            effective_entrypoint = path;
            invocation = Some("native_entrypoint");
        } else if surface_id == "agent-context" && projection["id"] == "default" {
            let path =
                native_artifact_entrypoint("agent-context-mcp", "narada-agent-context-mcp.exe")
                    .ok_or("registrar_native_agent_context_missing")?;
            effective_command = path.clone();
            effective_entrypoint = path;
            invocation = Some("native_entrypoint");
        } else if shared {
            let path =
                native_artifact_entrypoint("shared/mcp-surfaces-native", "narada-mcp-surfaces.exe")
                    .ok_or("registrar_native_shared_surface_missing")?;
            effective_command = path.clone();
            effective_entrypoint = path;
            effective_args = native_shared_args(surface_id, args);
            invocation = Some("native_entrypoint");
        } else if surface_id == "mcp-registrar" {
            let path = native_artifact_entrypoint("mcp-registrar", "narada-mcp-registrar.exe")
                .ok_or("registrar_native_registrar_missing")?;
            effective_command = path.clone();
            effective_entrypoint = path;
            invocation = Some("native_entrypoint");
        }
    }
    let mut proxy_args = vec![
        "proxy".into(),
        "--surface-id".into(),
        surface_id.into(),
        "--child-command".into(),
        effective_command,
        "--artifact-manifest".into(),
        workspace_repo_root()
            .map(|root| {
                root.join(".ai/runtime/workspace-artifact-manifest.json")
                    .to_string_lossy()
                    .replace('\\', "/")
            })
            .unwrap_or_default(),
        "--runtime-contract-version".into(),
        CONTRACT_VERSION.to_string(),
        "--entrypoint".into(),
        effective_entrypoint,
    ];
    if let Some(kind) = invocation {
        proxy_args.extend(["--child-invocation-kind", kind].map(str::to_string));
        if kind == "native_applet" {
            proxy_args
                .extend(["--child-applet", applet.unwrap_or("filesystem")].map(str::to_string));
        }
    }
    proxy_args.push("--".into());
    proxy_args.extend(effective_args);
    Ok((proxy, proxy_args))
}

fn native_shared_args(surface_id: &str, args: &[String]) -> Vec<String> {
    let mut result = vec!["--surface-id".into(), surface_id.into()];
    if surface_id == "calendar" || surface_id == "graph-mail" {
        result.push("--native-authority".into())
    }
    let roots = [
        "--site-root",
        "--narada-root",
        "--feedback-root",
        "--output-root",
        "--user-site-root",
        "--repo-root",
        "--sop-root",
        "--task-root",
        "--allowed-root",
    ];
    let forwarded = [
        "--log-root",
        "--registry-path",
        "--projection-id",
        "--canonical-feedback-root",
        "--task-lifecycle-root",
        "--feedback-discovery-root",
        "--site-id",
        "--owned-surface-id",
        "--projection",
        "--source-kind",
        "--operator-id",
        "--run-root",
        "--sops-dir",
        "--provider-registry-path",
        "--server-name",
    ];
    let mut index = 0;
    while index < args.len() {
        let key = &args[index];
        if (roots.contains(&key.as_str()) || forwarded.contains(&key.as_str()))
            && index + 1 < args.len()
            && !args[index + 1].starts_with("--")
        {
            result.push(key.clone());
            result.push(args[index + 1].clone());
            index += 2
        } else {
            index += 1
        }
    }
    result
}
fn component_kind(surface: &str) -> String {
    match surface {
        "mcp-loader" => "mcp-loader-mcp",
        "local-filesystem" => "filesystem-mcp",
        "structured-command" => "structured-command-mcp",
        "git" => "git-mcp",
        "agent-context" => "agent-context-mcp",
        "mcp-registrar" => "mcp-registrar",
        "task-lifecycle" => "task-lifecycle-mcp",
        "work-lifecycle" => "work-lifecycle-mcp",
        value => return format!("{value}-mcp"),
    }
    .into()
}
fn runtime_engine(component: &str, implementation: Option<&str>) -> Result<String, String> {
    if implementation == Some("js") {
        return Err("registrar_legacy_javascript_runtime_retired".into());
    }
    let workspace = workspace_repo_root().ok_or("registrar_workspace_root_unavailable")?;
    let path = runtime_implementation_matrix_path(&workspace)?;
    let matrix: Value = serde_json::from_str(
        &fs::read_to_string(&path)
            .map_err(|error| format!("registrar_runtime_matrix_read_failed:{}:{error}", path_text(&path)))?,
    )
    .map_err(|error| format!("registrar_runtime_matrix_invalid:{}:{error}", path_text(&path)))?;
    let row = matrix["rows"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|row| row["component_kind"] == component)
        .ok_or_else(|| format!("registrar_runtime_implementation_unavailable:{component}"))?;
    let engine = if implementation == Some("native") {
        "rust"
    } else {
        row.pointer("/profile_runtime_engine_kinds/native")
            .and_then(Value::as_str)
            .ok_or_else(|| format!("registrar_runtime_profile_engine_missing:{component}"))?
    };
    if engine != "rust" {
        return Err(format!("registrar_non_native_runtime_retired:{engine}"));
    }
    if row
        .pointer(&format!("/implementations/{engine}/status"))
        .and_then(Value::as_str)
        != Some("admitted")
    {
        return Err(format!(
            "registrar_runtime_implementation_unavailable:{component}"
        ));
    }
    Ok(engine.into())
}

fn refresh_site_sidecar_bindings(contract: &Value, site: &Value) -> Result<Value, String> {
    let site_id = site["site_id"]
        .as_str()
        .filter(|value| !value.is_empty())
        .ok_or("registrar_site_id_missing")?;
    let config_dir = site_mcp_control_root(Path::new(site["root"].as_str().unwrap_or("")))
        .join(".ai")
        .join("mcp");
    if !config_dir.exists() {
        return Ok(json!({"inspected":0,"refreshed":0,"changed":0}));
    }
    let aggregate_name = format!("{site_id}-mcp.json");
    let mut paths = fs::read_dir(&config_dir)
        .map_err(|error| error.to_string())?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            let file_name = path
                .file_name()
                .and_then(|value| value.to_str())
                .unwrap_or("");
            path.extension().and_then(|value| value.to_str()) == Some("json")
                && file_name.starts_with("narada-")
                && file_name != aggregate_name
        })
        .collect::<Vec<_>>();
    paths.sort();
    if paths.len() > 256 {
        return Err(format!(
            "registrar_site_binding_refresh_limit_exceeded:{site_id}:{}",
            paths.len()
        ));
    }
    let mut seen = BTreeSet::new();
    let mut inspected = 0usize;
    let mut refreshed = 0usize;
    let mut changed = 0usize;
    for path in paths {
        let config: Value = serde_json::from_slice(
            &fs::read(&path)
                .map_err(|error| format!("registrar_site_binding_read_failed:{error}"))?,
        )
        .map_err(|error| {
            format!(
                "registrar_site_binding_parse_failed:{}:{error}",
                path_text(&path)
            )
        })?;
        let Some(servers) = config.get("mcpServers").and_then(Value::as_object) else {
            continue;
        };
        if servers.len() > 64 {
            return Err(format!(
                "registrar_site_binding_server_limit_exceeded:{}:{}",
                path_text(&path),
                servers.len()
            ));
        }
        for server in servers.values() {
            inspected += 1;
            let Some(surface_id) = server
                .get("surface_id")
                .and_then(Value::as_str)
                .filter(|value| !value.is_empty())
            else {
                continue;
            };
            let projection_id = server
                .get("projection_id")
                .and_then(Value::as_str)
                .filter(|value| !value.is_empty())
                .unwrap_or("default");
            if !seen.insert((surface_id.to_string(), projection_id.to_string())) {
                continue;
            }
            let result = site_bind(
                contract,
                &json!({
                    "site_id":site_id,
                    "surface_id":surface_id,
                    "projection_id":projection_id,
                    "allow_sidecar":true
                }),
            )
            .map_err(|error| {
                format!(
                    "registrar_site_binding_refresh_surface_failed:{site_id}:{surface_id}:{projection_id}:{error}"
                )
            })?;
            if result["status"] != "bound" {
                return Err(format!(
                    "registrar_site_binding_refresh_refused:{site_id}:{surface_id}:{}",
                    result
                ));
            }
            refreshed += 1;
            if result
                .pointer("/activation/binding_changed")
                .and_then(Value::as_bool)
                == Some(true)
            {
                changed += 1;
            }
        }
    }
    Ok(json!({"inspected":inspected,"refreshed":refreshed,"changed":changed}))
}

fn site_surface_registry_sync(contract: &Value, args: &Value) -> Result<Value, String> {
    let requested = args
        .get("site_id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| "registrar_requires_site_id".to_string())?;
    let catalog = site_catalog(contract);
    let sites = catalog["items"].as_array().cloned().unwrap_or_default();
    let site = lookup_site_value(&sites, requested)?;
    let path = capability_registry_path(Path::new(site["root"].as_str().unwrap_or("")));
    if args.get("dry_run").and_then(Value::as_bool) == Some(true) {
        let registry = build_site_surface_registry(contract, &site)?;
        let surfaces = registry["surfaces"].as_array().cloned().unwrap_or_default();
        let tool_count = surfaces
            .iter()
            .map(|surface| {
                surface["registered_live_tools"]
                    .as_array()
                    .map_or(0, Vec::len)
            })
            .sum::<usize>();
        let mut result = json!({"schema":"narada.registrar.site_surface_registry_sync.v1","status":"dry_run","site_id":requested,"path":path_text(&path),"surface_count":surfaces.len(),"tool_count":tool_count,"registry_included":false,"bounded":true});
        if args.get("include_registry").and_then(Value::as_bool) == Some(true) {
            result["registry"] = registry;
            result["registry_included"] = json!(true);
        }
        return Ok(result);
    }
    let binding_refresh = refresh_site_sidecar_bindings(contract, &site).map_err(|error| {
        format!("registrar_site_binding_refresh_failed:{requested}:{error}")
    })?;
    let registry = build_site_surface_registry(contract, &site)
        .map_err(|error| format!("registrar_site_registry_build_failed:{requested}:{error}"))?;
    let parent = path
        .parent()
        .ok_or("registrar_site_registry_path_invalid")?;
    fs::create_dir_all(parent).map_err(|error| {
        format!(
            "registrar_site_registry_parent_create_failed:{}:{error}",
            path_text(parent)
        )
    })?;
    let temporary = path.with_extension(format!("json.tmp-{}", std::process::id()));
    fs::write(
        &temporary,
        serde_json::to_string_pretty(&registry).map_err(|error| error.to_string())? + "\n",
    )
    .map_err(|error| {
        format!(
            "registrar_site_registry_write_failed:{}:{error}",
            path_text(&temporary)
        )
    })?;
    fs::rename(&temporary, &path).map_err(|error| {
        format!(
            "registrar_site_registry_rename_failed:{}:{}:{error}",
            path_text(&temporary),
            path_text(&path)
        )
    })?;
    let surfaces = registry["surfaces"].as_array().cloned().unwrap_or_default();
    let tool_count = surfaces
        .iter()
        .map(|surface| {
            surface["registered_live_tools"]
                .as_array()
                .map_or(0, Vec::len)
        })
        .sum::<usize>();
    Ok(
        json!({"schema":"narada.registrar.site_surface_registry_sync.v1","status":"synced","site_id":site["site_id"],"path":path_text(&path),"surface_count":surfaces.len(),"tool_count":tool_count,"binding_refresh":binding_refresh,"bounded":true}),
    )
}

fn build_site_surface_registry(contract: &Value, site: &Value) -> Result<Value, String> {
    let root = PathBuf::from(site["root"].as_str().unwrap_or(""));
    let directory = site_mcp_control_root(&root).join(".ai").join("mcp");
    let mut surfaces = vec![];
    if let Ok(entries) = fs::read_dir(&directory) {
        for entry in entries.filter_map(Result::ok) {
            let path = entry.path();
            if path.extension().and_then(|value| value.to_str()) != Some("json") {
                continue;
            }
            let Some(file) = path.file_name().and_then(|value| value.to_str()) else {
                continue;
            };
            let Some(config) = fs::read_to_string(&path)
                .ok()
                .and_then(|text| serde_json::from_str::<Value>(&text).ok())
            else {
                continue;
            };
            for (server_name, server) in config["mcpServers"].as_object().into_iter().flatten() {
                surfaces.push(registry_surface(contract, site, server_name, server, file)?);
            }
        }
    }
    surfaces.sort_by(|left: &Value, right: &Value| {
        left["server_name"]
            .as_str()
            .unwrap_or("")
            .cmp(right["server_name"].as_str().unwrap_or(""))
    });
    let generated_at = time::OffsetDateTime::now_utc()
        .format(&time::format_description::well_known::Rfc3339)
        .map_err(|error| error.to_string())?;
    Ok(
        json!({"schema":"narada.site.capabilities.mcp_surfaces.v1","artifact_role":"site_capability_surface_registry_not_mcp_client_config","site_id":site["site_id"],"generated_by":"mcp-registrar","generated_at":generated_at,"generation_policy":{"source":".ai/mcp + registrar surface catalog","mode":"enabled_surface_tool_authority","note":"Every tool exposed by an enabled MCP surface is declared for action admission. The MCP surface remains responsible for command policy and mutation enforcement."},"surfaces":surfaces}),
    )
}

fn embedded_site_local_catalog(server: &Value, surface_id: &str) -> Option<Value> {
    let projection = server.get("surface_projection")?;
    let descriptor = projection.get("surface_descriptor")?;
    if descriptor.get("surface_id").and_then(Value::as_str) != Some(surface_id) {
        return None;
    }
    let mut local_projection = projection.clone();
    if let Some(object) = local_projection.as_object_mut() {
        object
            .entry("id".to_string())
            .or_insert_with(|| json!(projection["projection_id"].as_str().unwrap_or("default")));
    }
    Some(json!({
        "id": surface_id,
        "tools": projection.get("exposed_tools").cloned().unwrap_or_else(|| json!([])),
        "projections": [local_projection],
        "descriptor": descriptor
    }))
}

fn registry_surface(
    contract: &Value,
    site: &Value,
    server_name: &str,
    server: &Value,
    file: &str,
) -> Result<Value, String> {
    let site_id = site["site_id"].as_str().unwrap_or("");
    let prefix = if site_id == "andrey-user" {
        "narada-site-andrey-user".to_string()
    } else if site_id.starts_with("narada-") {
        site_id.to_string()
    } else {
        format!("narada-{site_id}")
    };
    let inferred = server_name
        .strip_prefix(&(prefix + "-"))
        .unwrap_or(server_name);
    let surface_id = server["surface_id"].as_str().unwrap_or(inferred);
    let items = contract
        .pointer("/read_models/registrar_surface_list/items")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let catalog = items
        .iter()
        .find(|surface| surface["id"] == surface_id)
        .cloned()
        .or_else(|| embedded_site_local_catalog(server, surface_id))
        .ok_or_else(|| format!("registrar_site_local_descriptor_missing:{surface_id}"))?;
    let projection_id = server["projection_id"]
        .as_str()
        .or_else(|| {
            server
                .pointer("/surface_projection/projection_id")
                .and_then(Value::as_str)
        })
        .unwrap_or("default");
    let projection = catalog["projections"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|projection| projection["id"] == projection_id)
        .ok_or_else(|| {
            format!("registrar_unknown_surface_projection:{surface_id}:{projection_id}")
        })?;
    let tool_source = projection
        .get("exposed_tools")
        .filter(|value| value.is_array())
        .unwrap_or(&catalog["tools"]);
    let registered = unique(
        tool_source
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str),
    );
    let descriptor = &catalog["descriptor"];
    let mut read_only = vec![];
    let mut refused = vec![];
    for tool in descriptor["tools"].as_array().into_iter().flatten() {
        let Some(name) = tool["name"].as_str() else {
            continue;
        };
        if !registered.iter().any(|value| value == name) {
            continue;
        }
        if tool.pointer("/effect/class").and_then(Value::as_str) == Some("read")
            || tool
                .pointer("/annotations/readOnlyHint")
                .and_then(Value::as_bool)
                == Some(true)
        {
            read_only.push(name.to_string());
        }
        if tool
            .pointer("/annotations/legacy_policy")
            .and_then(Value::as_str)
            == Some("refused")
        {
            refused.push(name.to_string());
        }
    }
    let mut classified = read_only.clone();
    classified.extend(refused.clone());
    let mutating = registered
        .iter()
        .filter(|name| !classified.contains(name))
        .cloned()
        .collect::<Vec<_>>();
    let raw_command = server["command"].as_str().unwrap_or("node").to_string();
    let mut raw_args = server["args"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(str::to_string)
        .collect::<Vec<_>>();
    let mut launch = unwrap_launch(&raw_command, &raw_args);
    if matches!(
        launch.invocation.as_deref(),
        Some("native_applet" | "native_entrypoint")
    ) {
        if let Some(canonical) = canonical_native_surface_entrypoint(surface_id, projection_id) {
            for flag in ["--child-command", "--entrypoint"] {
                if let Some(index) = raw_args.iter().position(|value| value == flag) {
                    if let Some(value) = raw_args.get_mut(index + 1) {
                        *value = canonical.clone();
                    }
                }
            }
            launch.entrypoint = canonical.clone();
            launch.child_command = canonical;
        }
    }
    let runtime_kind = if matches!(
        launch.invocation.as_deref(),
        Some("native_applet" | "native_entrypoint")
    ) {
        "rust-stdio"
    } else if executable_name(&launch.child_command) == "bun" {
        "bun-stdio"
    } else {
        "node-stdio"
    };
    let mut surface_projection = json!({"surface_id":surface_id,"projection_id":projection_id,"injection_scope":projection["injection_scope"],"runtime_requirements":projection.get("runtime_requirements").cloned().unwrap_or_else(||json!([])),"exposed_tools":registered,"execution":projection["execution"],"descriptor_digest":catalog["descriptor_digest"],"tool_contract_digest":catalog["tool_contract_digest"],"surface_descriptor":descriptor});
    if let Some(value) = projection.get("default_injection") {
        surface_projection
            .as_object_mut()
            .unwrap()
            .insert("default_injection".into(), value.clone());
    }
    if let Some(value) = server.pointer("/surface_projection/runtime_kind") {
        surface_projection
            .as_object_mut()
            .unwrap()
            .insert("runtime_kind".into(), value.clone());
    }
    if let Some(value) = descriptor["projections"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|candidate| candidate["id"] == projection_id)
        .and_then(|candidate| candidate.get("lifecycle"))
    {
        surface_projection
            .as_object_mut()
            .unwrap()
            .insert("lifecycle".into(), value.clone());
    }
    let transport_command = if launch.proxied {
        native_proxy_entrypoint().unwrap_or(raw_command.clone())
    } else {
        raw_command.clone()
    };
    let transport_args = if !launch.proxied && raw_args.is_empty() {
        vec![String::new()]
    } else {
        raw_args
    };
    Ok(
        json!({"surface_id":format!("{server_name}.local"),"surface_projection":surface_projection,"surface_type":catalog["kind"],"display_name":server_name,"server_name":server_name,"runtime_binding":{"runtime_kind":runtime_kind,"proxy_implementation":if launch.proxied{json!("native")}else{Value::Null},"entrypoint":launch.entrypoint,"owner_site_id":site_id,"transport":{"type":"stdio","command":transport_command,"args":transport_args}},"authority_boundary":{"posture":"registrar_generated_runtime_surface_registry","grants_tool_authority":true,"granted_tool_authority_kind":"declared_enabled_mcp_surface_tools","source":"site_mcp_fabric_and_registrar_catalog"},"client_config":{"generated_path":format!(".ai/mcp/{file}"),"generated_file":file},"tool_contract":{"exposed_tools":registered,"semantic_operations":[],"deprecated_aliases":{},"read_only_tools":read_only,"mutating_tools":mutating,"refused_tools":refused},"registered_live_tools":registered,"catalog_surface_id":descriptor["surface_id"],"evidence":{"source":"site_mcp_fabric","path":format!(".ai/mcp/{file}"),"projection_kind":"site_fabric"}}),
    )
}

fn canonical_native_surface_entrypoint(surface_id: &str, projection_id: &str) -> Option<String> {
    let (package, artifact) = native_surface_artifact(surface_id, projection_id)?;
    native_artifact_entrypoint(package, artifact)
}

fn native_surface_artifact(
    surface_id: &str,
    projection_id: &str,
) -> Option<(&'static str, &'static str)> {
    if ["local-filesystem", "structured-command", "git"].contains(&surface_id) {
        return Some(("shared/mcp-runtime-proxy", "narada-mcp-runtime.exe"));
    }
    match surface_id {
        "mcp-loader" => return Some(("mcp-loader-mcp", "narada-mcp-loader.exe")),
        "task-lifecycle" => {
            return Some((
                "shared/mcp-lifecycle-native",
                "narada-task-lifecycle-mcp.exe",
            ))
        }
        "work-lifecycle" => {
            return Some((
                "shared/mcp-lifecycle-native",
                "narada-work-lifecycle-mcp.exe",
            ))
        }
        "agent-context" if projection_id == "default" => {
            return Some(("agent-context-mcp", "narada-agent-context-mcp.exe"))
        }
        "mcp-registrar" => return Some(("mcp-registrar", "narada-mcp-registrar.exe")),
        _ => {}
    }
    if [
        "catalog-observation",
        "operator-routing",
        "site-inbox",
        "site-lifecycle",
        "site-registry",
        "project-state",
        "epistemic-graph",
        "runtime-introspection",
        "site-coherence",
        "launcher",
        "mailbox",
        "graph-mail",
        "calendar",
        "worker-delegation",
        "delegated-task",
        "sop",
        "scheduler",
        "surface-feedback",
        "speech",
        "artifacts",
        "nars-session",
        "quota-meter",
        "operator-console-overlay",
        "browser-control",
        "cloudflare-carrier",
    ]
    .contains(&surface_id)
    {
        return Some(("shared/mcp-surfaces-native", "narada-mcp-surfaces.exe"));
    }
    None
}

struct Launch {
    entrypoint: String,
    child_command: String,
    proxied: bool,
    invocation: Option<String>,
}
fn unwrap_launch(command: &str, args: &[String]) -> Launch {
    if args.first().map(String::as_str) == Some("proxy") {
        let value = |flag: &str| {
            args.iter()
                .position(|item| item == flag)
                .and_then(|index| args.get(index + 1))
                .cloned()
                .unwrap_or_default()
        };
        return Launch {
            entrypoint: value("--entrypoint"),
            child_command: value("--child-command"),
            proxied: true,
            invocation: Some(value("--child-invocation-kind")).filter(|value| !value.is_empty()),
        };
    }
    Launch {
        entrypoint: args.first().cloned().unwrap_or_default(),
        child_command: command.to_string(),
        proxied: false,
        invocation: None,
    }
}
fn executable_name(command: &str) -> String {
    Path::new(command)
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or(command)
        .to_ascii_lowercase()
}
fn native_proxy_entrypoint() -> Option<String> {
    native_artifact_entrypoint("shared/mcp-runtime-proxy", "narada-mcp-runtime.exe")
}
fn native_artifact_entrypoint(package: &str, artifact: &str) -> Option<String> {
    let executable = env::current_exe().ok()?;
    let workspace = executable.ancestors().find(|root| {
        root.join("packages")
            .join("shared")
            .join("mcp-runtime-proxy")
            .exists()
    })?;
    let native_root = package
        .split('/')
        .fold(workspace.join("packages"), |root, part| root.join(part))
        .join("dist")
        .join("native");
    let pointer: Value =
        serde_json::from_str(&fs::read_to_string(native_root.join("current.json")).ok()?).ok()?;
    let relative = pointer.get("artifacts")?.get(artifact)?.as_str()?;
    Some(
        native_root
            .join(relative)
            .to_string_lossy()
            .replace('\\', "/"),
    )
}

fn surface_tool_inventory(contract: &Value, args: &Value) -> Value {
    let observed = args.get("observed_tools").and_then(Value::as_object);
    let include_ok = args.get("include_ok") == Some(&Value::Bool(true));
    let items = contract
        .pointer("/read_models/registrar_surface_list/items")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut findings = vec![];
    let mut checked = 0;
    for surface in &items {
        let id = surface["id"].as_str().unwrap_or("");
        let Some(input) = observed.and_then(|value| value.get(id)) else {
            continue;
        };
        checked += 1;
        let registered = unique(
            surface["tools"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str),
        );
        let actual = unique(
            input
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str),
        );
        let missing = actual
            .iter()
            .filter(|value| !registered.contains(value))
            .cloned()
            .collect::<Vec<_>>();
        let extra = registered
            .iter()
            .filter(|value| !actual.contains(value))
            .cloned()
            .collect::<Vec<_>>();
        let status = if missing.is_empty() && extra.is_empty() {
            "ok"
        } else {
            "drift"
        };
        if status != "ok" || include_ok {
            findings.push(json!({"surface_id":id,"package":surface["package"],"status":status,"registered_count":registered.len(),"observed_count":actual.len(),"missing_from_registrar":missing,"extra_in_registrar":extra}));
        }
    }
    let without = items
        .iter()
        .filter_map(|value| value["id"].as_str())
        .filter(|id| observed.is_none_or(|value| !value.contains_key(*id)))
        .collect::<Vec<_>>();
    json!({"schema":"narada.registrar.surface_tool_inventory_check.v1","status":if findings.iter().any(|value|value["status"]=="drift"){"drift"}else{"ok"},"checked_count":checked,"surfaces_without_observations":without,"findings":findings})
}
fn unique<'a>(values: impl Iterator<Item = &'a str>) -> Vec<String> {
    let mut result = vec![];
    for value in values {
        if !result.iter().any(|existing| existing == value) {
            result.push(value.to_string());
        }
    }
    result
}

fn normalize_tool_schemas(contract: &mut Value) {
    let Some(tools) = contract.get_mut("tools").and_then(Value::as_array_mut) else {
        return;
    };
    for tool in tools {
        let name = tool
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or("registrar_tool")
            .to_string();
        let Some(schema) = tool.get_mut("inputSchema") else {
            continue;
        };
        normalize_schema(schema, Some(&name));
        if let Some(object) = schema.as_object_mut() {
            object.insert("title".into(), json!(format!("{name}.input")));
            object.insert("additionalProperties".into(), json!(false));
            object.entry("maxProperties").or_insert(json!(64));
        }
    }
}

fn normalize_schema(schema: &mut Value, field: Option<&str>) {
    let Some(object) = schema.as_object_mut() else {
        return;
    };
    match object.get("type").and_then(Value::as_str) {
        Some("string") if !object.contains_key("maxLength") && !object.contains_key("enum") => {
            let field = field.unwrap_or_default().to_ascii_lowercase();
            let maximum = if field.contains("path") || field.contains("root") {
                4096
            } else {
                8192
            };
            object.insert("maxLength".into(), json!(maximum));
        }
        Some("array") if !object.contains_key("maxItems") => {
            object.insert("maxItems".into(), json!(256));
        }
        Some("object") if !object.contains_key("maxProperties") => {
            object.insert("maxProperties".into(), json!(256));
        }
        _ => {}
    }
    if let Some(properties) = object.get_mut("properties").and_then(Value::as_object_mut) {
        for (name, child) in properties {
            normalize_schema(child, Some(name));
        }
    }
    if let Some(items) = object.get_mut("items") {
        normalize_schema(items, field);
    }
}

fn validate_tool_call(contract: &Value, params: &Value) -> Result<(), String> {
    let object = params
        .as_object()
        .ok_or("invalid_tool_call_params:expected_object")?;
    for key in object.keys() {
        if !matches!(key.as_str(), "name" | "arguments" | "_meta") {
            return Err(format!("invalid_tool_call_params:unknown_field:{key}"));
        }
    }
    let name = object
        .get("name")
        .and_then(Value::as_str)
        .ok_or("invalid_tool_call_params:name_required")?;
    let tool = contract["tools"]
        .as_array()
        .and_then(|tools| tools.iter().find(|tool| tool["name"] == name))
        .ok_or_else(|| format!("unknown_tool:{name}"))?;
    let arguments = object
        .get("arguments")
        .cloned()
        .unwrap_or_else(|| json!({}));
    validate_schema(&tool["inputSchema"], &arguments, "$args")
}

fn validate_schema(schema: &Value, value: &Value, path: &str) -> Result<(), String> {
    if let Some(expected) = schema.get("type").and_then(Value::as_str) {
        let matches = match expected {
            "object" => value.is_object(),
            "array" => value.is_array(),
            "string" => value.is_string(),
            "integer" => value.as_i64().is_some() || value.as_u64().is_some(),
            "number" => value.is_number(),
            "boolean" => value.is_boolean(),
            _ => true,
        };
        if !matches {
            return Err(format!("invalid_tool_arguments:{path}:expected_{expected}"));
        }
    }
    if let Some(text) = value.as_str() {
        if schema
            .get("maxLength")
            .and_then(Value::as_u64)
            .is_some_and(|max| text.len() > max as usize)
        {
            return Err(format!("invalid_tool_arguments:{path}:maxLength"));
        }
        if schema
            .get("enum")
            .and_then(Value::as_array)
            .is_some_and(|values| !values.iter().any(|candidate| candidate == value))
        {
            return Err(format!("invalid_tool_arguments:{path}:enum"));
        }
    }
    if let Some(array) = value.as_array() {
        if schema
            .get("maxItems")
            .and_then(Value::as_u64)
            .is_some_and(|max| array.len() > max as usize)
        {
            return Err(format!("invalid_tool_arguments:{path}:maxItems"));
        }
        if let Some(items) = schema.get("items") {
            for (index, item) in array.iter().enumerate() {
                validate_schema(items, item, &format!("{path}[{index}]"))?;
            }
        }
    }
    if let Some(object) = value.as_object() {
        if schema
            .get("maxProperties")
            .and_then(Value::as_u64)
            .is_some_and(|max| object.len() > max as usize)
        {
            return Err(format!("invalid_tool_arguments:{path}:maxProperties"));
        }
        let properties = schema.get("properties").and_then(Value::as_object);
        if schema.get("additionalProperties") == Some(&json!(false)) {
            for key in object.keys() {
                if !properties.is_some_and(|known| known.contains_key(key)) {
                    return Err(format!("invalid_tool_arguments:{path}:unknown_field:{key}"));
                }
            }
        }
        for required in schema
            .get("required")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
        {
            if !object.contains_key(required) {
                return Err(format!("invalid_tool_arguments:{path}:required:{required}"));
            }
        }
        if let Some(properties) = properties {
            for (key, child) in object {
                if let Some(child_schema) = properties.get(key) {
                    validate_schema(child_schema, child, &format!("{path}.{key}"))?;
                }
            }
        }
    }
    Ok(())
}

fn error(id: Value, message: String) -> Value {
    json!({"jsonrpc":"2.0","id":id,"error":{"code":-32000,"message":message}})
}

fn read_line_bounded<R: BufRead>(input: &mut R, maximum: usize) -> Result<Option<String>, String> {
    let mut bytes = Vec::new();
    let count = input
        .take((maximum + 1) as u64)
        .read_until(b'\n', &mut bytes)
        .map_err(|error| error.to_string())?;
    if count == 0 {
        return Ok(None);
    }
    if bytes.len() > maximum || !bytes.ends_with(b"\n") {
        return Err("mcp_line_exceeds_byte_limit".into());
    }
    String::from_utf8(bytes)
        .map(Some)
        .map_err(|_| "mcp_line_invalid_utf8".into())
}

fn read_message<R: BufRead>(input: &mut R) -> Result<Option<Value>, String> {
    let Some(first) = read_line_bounded(input, MAX_MESSAGE_BYTES)? else {
        return Ok(None);
    };
    if first.to_ascii_lowercase().starts_with("content-length:") {
        let length = first
            .split_once(':')
            .and_then(|(_, v)| v.trim().parse::<usize>().ok())
            .ok_or("invalid_content_length")?;
        if length > MAX_MESSAGE_BYTES {
            return Err("mcp_body_exceeds_byte_limit".into());
        }
        let mut header_bytes = first.len();
        loop {
            let Some(line) = read_line_bounded(input, MAX_HEADER_BYTES)? else {
                return Err("unexpected_eof_in_headers".into());
            };
            header_bytes += line.len();
            if header_bytes > MAX_HEADER_BYTES {
                return Err("mcp_headers_exceed_byte_limit".into());
            }
            if line == "\r\n" || line == "\n" {
                break;
            }
        }
        let mut body = vec![0; length];
        input.read_exact(&mut body).map_err(|e| e.to_string())?;
        serde_json::from_slice(&body)
            .map(Some)
            .map_err(|e| e.to_string())
    } else {
        if first.len() > MAX_MESSAGE_BYTES {
            return Err("mcp_body_exceeds_byte_limit".into());
        }
        serde_json::from_str(first.trim())
            .map(Some)
            .map_err(|e| e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn embedded_contract() -> Value {
        decode_contract().unwrap()
    }

    #[test]
    fn embedded_native_contract_is_valid() {
        validate_contract(&embedded_contract()).unwrap();
    }

    #[test]
    fn runtime_matrix_path_resolves_from_nested_worktree() {
        let root = env::temp_dir().join(format!(
            "narada-registrar-runtime-matrix-{}",
            std::process::id()
        ));
        let source_root = root.join("src");
        let worktree_root = source_root.join("mcp-surfaces/.worktrees/worker");
        let matrix = source_root.join(
            "narada/packages/operator-surface-runtime-contract/contracts/runtime-implementation-matrix.json",
        );
        fs::create_dir_all(&worktree_root).expect("worktree");
        fs::create_dir_all(matrix.parent().expect("matrix parent")).expect("narada source");
        fs::write(&matrix, b"{}").expect("matrix file");

        assert_eq!(
            runtime_implementation_matrix_path(&worktree_root).expect("matrix path"),
            matrix
        );

        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn embedded_site_local_descriptor_normalizes_into_catalog_shape() {
        let server = json!({
            "surface_projection": {
                "projection_id": "default",
                "exposed_tools": ["local_read", "local_write"],
                "surface_descriptor": {
                    "surface_id": "local-domain",
                    "tools": [
                        {"name": "local_read", "effect": {"class": "read"}},
                        {"name": "local_write", "effect": {"class": "local_write"}}
                    ]
                }
            }
        });
        let catalog =
            embedded_site_local_catalog(&server, "local-domain").expect("site-local catalog");
        assert_eq!(catalog["id"], "local-domain");
        assert_eq!(catalog["projections"][0]["id"], "default");
        assert_eq!(catalog["tools"], json!(["local_read", "local_write"]));
        assert_eq!(catalog["descriptor"]["surface_id"], "local-domain");
    }

    #[test]
    fn embedded_site_local_descriptor_cannot_claim_another_surface() {
        let server = json!({
            "surface_projection": {
                "projection_id": "default",
                "surface_descriptor": {"surface_id": "other-domain"}
            }
        });
        assert!(embedded_site_local_catalog(&server, "local-domain").is_none());
    }

    #[test]
    fn embedded_git_surface_is_injected_into_site_bound_sessions() {
        let contract = embedded_contract();
        let git = contract
            .pointer("/read_models/registrar_surface_list/items")
            .and_then(Value::as_array)
            .and_then(|items| items.iter().find(|item| item["id"] == "git"))
            .expect("git surface");
        let projection = git["projections"]
            .as_array()
            .and_then(|items| items.iter().find(|item| item["id"] == "default"))
            .expect("git runtime projection");
        let descriptor_projection = git["descriptor"]["projections"]
            .as_array()
            .and_then(|items| items.iter().find(|item| item["id"] == "default"))
            .expect("git descriptor projection");
        assert_eq!(projection["default_injection"], "enabled");
        assert_eq!(descriptor_projection["default_injection"], "enabled");
    }

    #[test]
    fn duplicate_native_contract_records_are_rejected() {
        let mut contract = embedded_contract();
        let duplicate = contract["tools"][0].clone();
        contract["tools"].as_array_mut().unwrap().push(duplicate);
        assert!(validate_contract(&contract)
            .unwrap_err()
            .starts_with("tools_name_duplicate:"));
    }

    #[test]
    fn unsupported_native_contract_schema_is_rejected() {
        let mut contract = embedded_contract();
        contract["schema"] = json!("legacy");
        assert_eq!(
            validate_contract(&contract).unwrap_err(),
            "unsupported_schema"
        );
    }

    #[test]
    fn native_epistemic_catalog_matches_live_surface_tools() {
        let mut contract = embedded_contract();
        extend_epistemic_catalog(&mut contract);
        let surface = contract
            .pointer("/read_models/registrar_surface_list/items")
            .and_then(Value::as_array)
            .and_then(|items| items.iter().find(|item| item["id"] == "epistemic-graph"))
            .expect("epistemic catalog");
        let names = surface["tools"].as_array().expect("tool names");
        assert_eq!(names.len(), 15);
        for required in [
            "epistemic_graph_query_batch",
            "epistemic_graph_source_inspect",
            "epistemic_graph_capture_sources",
            "epistemic_graph_submit_review_admit",
            "epistemic_graph_proposal_read",
            "epistemic_graph_proposal_resubmit",
        ] {
            assert!(names.iter().any(|name| name == required), "{required}");
        }
    }

    #[test]
    fn native_registry_rebinding_covers_every_distribution_artifact_class() {
        for (surface_id, projection_id, executable) in [
            ("local-filesystem", "default", "narada-mcp-runtime.exe"),
            ("mcp-loader", "default", "narada-mcp-loader.exe"),
            ("agent-context", "default", "narada-agent-context-mcp.exe"),
            ("task-lifecycle", "stdio", "narada-task-lifecycle-mcp.exe"),
            ("mcp-registrar", "default", "narada-mcp-registrar.exe"),
            ("surface-feedback", "default", "narada-mcp-surfaces.exe"),
            ("epistemic-graph", "default", "narada-mcp-surfaces.exe"),
        ] {
            let (_, artifact) = native_surface_artifact(surface_id, projection_id)
                .unwrap_or_else(|| panic!("missing native mapping for {surface_id}"));
            assert_eq!(artifact, executable, "{surface_id}");
        }
    }

    #[test]
    fn native_contract_repairs_guidance_schema_and_validation_entrypoint() {
        let mut contract = embedded_contract();
        let declared = contract["runtime_bindings"]["registrar_entrypoint"]
            .as_str()
            .unwrap()
            .to_string();
        let current = "C:/native/narada-mcp-registrar.exe";
        repair_native_contract(&mut contract, &declared, current);
        assert!(!contract["guidance"]
            .to_string()
            .contains("pnpm materialize:carrier"));
        assert!(contract["guidance"]
            .to_string()
            .contains("cargo native-release"));
        let list_tool = contract["tools"]
            .as_array()
            .unwrap()
            .iter()
            .find(|tool| tool["name"] == "registrar_surface_list")
            .unwrap();
        assert_eq!(
            list_tool["inputSchema"]["properties"]["compact"]["default"],
            true
        );
        for plan in contract["read_models"]["registrar_carrier_validation_plans"]
            .as_object()
            .unwrap()
            .values()
        {
            for server in plan["servers"].as_array().into_iter().flatten() {
                if server["surface_id"] == "mcp-registrar" {
                    assert_eq!(server["entrypoint"], current);
                }
            }
        }
    }

    #[test]
    fn surface_inventory_is_compact_and_paginated_by_default() {
        let contract = embedded_contract();
        let first = surface_list(&contract, &json!({"limit":1}));
        let second = surface_list(&contract, &json!({"limit":1,"offset":1}));
        assert_eq!(first["compact"], true);
        assert_eq!(first["returned"], 1);
        assert_eq!(first["has_more"], true);
        assert_eq!(first["next_offset"], 1);
        assert!(first["items"][0].get("descriptor").is_none());
        assert_ne!(first["items"][0]["id"], second["items"][0]["id"]);
        let full = surface_list(&contract, &json!({"limit":1,"compact":false}));
        assert!(full["items"][0].get("descriptor").is_some());
    }

    #[test]
    fn carrier_inventory_is_compact_self_describing_and_paginated() {
        let contract = embedded_contract();
        let first = carrier_list(&contract, &json!({"limit":1}));
        let second = carrier_list(&contract, &json!({"limit":1,"offset":1}));
        assert_eq!(first["schema"], "narada.registrar.carrier_list.v1");
        assert_eq!(first["status"], "ok");
        assert_eq!(first["compact"], true);
        assert_eq!(first["returned"], 1);
        assert_eq!(first["has_more"], true);
        assert!(first["items"][0].get("site_bindings").is_none());
        assert_ne!(
            first["items"][0]["carrier_id"],
            second["items"][0]["carrier_id"]
        );
        let full = carrier_list(&contract, &json!({"limit":1,"compact":false}));
        assert!(full["items"][0].get("site_bindings").is_some());
    }

    #[test]
    fn every_public_schema_is_named_closed_bounded_and_enforced() {
        let mut contract = embedded_contract();
        extend_epistemic_catalog(&mut contract);
        let declared = contract["runtime_bindings"]["registrar_entrypoint"]
            .as_str()
            .unwrap()
            .to_string();
        repair_native_contract(
            &mut contract,
            &declared,
            "C:/native/narada-mcp-registrar.exe",
        );
        normalize_tool_schemas(&mut contract);
        for tool in contract["tools"].as_array().unwrap() {
            let name = tool["name"].as_str().unwrap();
            let schema = &tool["inputSchema"];
            assert_eq!(schema["title"], format!("{name}.input"));
            assert_eq!(schema["additionalProperties"], false);
            assert!(schema["maxProperties"].as_u64().is_some());
            let failure = validate_tool_call(
                &contract,
                &json!({"name":name,"arguments":{"unexpected":true}}),
            )
            .unwrap_err();
            assert!(
                failure.contains("unknown_field:unexpected"),
                "{name}: {failure}"
            );
        }
    }

    #[test]
    fn native_descriptor_schemas_match_live_worker_and_epistemic_contracts() {
        let mut contract = embedded_contract();
        extend_epistemic_catalog(&mut contract);
        align_native_surface_descriptor_schemas(&mut contract);
        let items = contract.pointer("/read_models/registrar_surface_list/items").and_then(Value::as_array).unwrap();
        let schema = |surface: &str, name: &str| {
            items.iter().find(|item| item["id"] == surface).unwrap()
                .pointer("/descriptor/tools").and_then(Value::as_array).unwrap()
                .iter().find(|tool| tool["name"] == name).unwrap()["input_schema"].clone()
        };
        assert!(schema("worker-delegation", "worker_run").pointer("/properties/constraints/properties/site_root").is_none());
        assert_eq!(
            schema("worker-delegation", "worker_run")
                .pointer("/properties/constraints/properties/wait_for_completion/default")
                .and_then(Value::as_bool),
            Some(false)
        );
        assert_eq!(
            schema("worker-delegation", "worker_run")
                .pointer("/properties/constraints/properties/wait_timeout_ms/maximum")
                .and_then(Value::as_u64),
            Some(300_000)
        );
        assert_eq!(schema("worker-delegation", "worker_config_resolve")["additionalProperties"], false);
        assert_eq!(schema("epistemic-graph", "epistemic_graph_guidance")["properties"]["workflow"]["type"], "string");
    }

    #[test]
    fn protocol_versions_are_honest_and_modern_requests_are_self_describing() {
        for version in ["2024-11-05", "2025-03-26", "2099-01-01"] {
            let removed = dispatch(
                &json!({"id":1,"method":"initialize","params":{"protocolVersion":version}}),
            );
            assert_eq!(removed["error"]["message"], "initialize_removed");
        }
        let incomplete = dispatch(
            &json!({"id":3,"method":"server/discover","params":{"_meta":{"io.modelcontextprotocol/protocolVersion":MODERN_PROTOCOL_VERSION}}}),
        );
        assert_eq!(incomplete["error"]["message"], "modern_metadata_required");
        let modern = dispatch(
            &json!({"id":4,"method":"server/discover","params":{"_meta":{"io.modelcontextprotocol/protocolVersion":MODERN_PROTOCOL_VERSION,"io.modelcontextprotocol/clientInfo":{"name":"test","version":"1"},"io.modelcontextprotocol/clientCapabilities":{}}}}),
        );
        assert_eq!(
            modern["result"]["supportedVersions"][0],
            MODERN_PROTOCOL_VERSION
        );
        assert_eq!(modern["result"]["resultType"], "complete");
    }

    #[test]
    fn wire_reader_refuses_oversized_messages_before_allocation() {
        let framed = format!("Content-Length: {}\r\n\r\n", MAX_MESSAGE_BYTES + 1);
        assert_eq!(
            read_message(&mut std::io::Cursor::new(framed)).unwrap_err(),
            "mcp_body_exceeds_byte_limit"
        );
        let jsonl = format!("{}\n", "x".repeat(MAX_MESSAGE_BYTES + 1));
        assert_eq!(
            read_message(&mut std::io::Cursor::new(jsonl)).unwrap_err(),
            "mcp_line_exceeds_byte_limit"
        );
    }

    #[test]
    fn repaired_contract_makes_expanding_reads_bounded_by_default() {
        let mut contract = embedded_contract();
        let declared = contract
            .pointer("/read_models/registrar_surface_list/items/0/entrypoint")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        repair_native_contract(&mut contract, &declared, &declared);
        let find = |name: &str| {
            contract["tools"]
                .as_array()
                .unwrap()
                .iter()
                .find(|tool| tool["name"] == name)
                .unwrap()
        };
        for name in ["registrar_site_list", "registrar_carrier_list"] {
            assert_eq!(
                find(name)["inputSchema"]["properties"]["compact"]["default"],
                true
            );
            assert_eq!(
                find(name)["inputSchema"]["properties"]["limit"]["default"],
                20
            );
        }
        assert_eq!(
            find("registrar_site_surface_registry_sync")["inputSchema"]["properties"]
                ["include_registry"]["default"],
            false
        );
    }

    #[test]
    fn site_scoped_surface_reports_loader_route_when_carrier_binding_is_absent() {
        let contract = embedded_contract();
        let failure = carrier_bind(
            &contract,
            &json!({
                "carrier_id":"codex-andrey",
                "site_id":"marici",
                "surface_id":"scheduler"
            }),
        )
        .expect_err("site-scoped scheduler must not be emitted as a carrier mutation");
        assert_eq!(failure.code, "registrar_carrier_site_binding_missing");
        assert_eq!(failure.details["site_surface_declared"], true);
        assert_eq!(failure.details["next_route"], "mcp-loader");
        assert!(failure.details["site_root"]
            .as_str()
            .unwrap_or_default()
            .replace('\\', "/")
            .ends_with("/marici"));
    }

    #[test]
    fn surface_usage_exposes_site_runtime_access_without_carrier_projection() {
        let contract = embedded_contract();
        let usage = surface_usage(&contract, &json!({"surface_id":"scheduler"})).unwrap();
        assert_eq!(usage["runtime_access"]["owner"], "mcp-loader");
        assert_eq!(usage["runtime_access"]["mode"], "site-scoped");
        assert_eq!(usage["runtime_access"]["available"], true);
    }

    #[test]
    fn worker_binding_consumes_durable_extra_allowed_roots() {
        let suffix = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = env::temp_dir().join(format!(
            "narada-registrar-extra-roots-{}-{suffix}",
            std::process::id()
        ));
        fs::create_dir_all(root.join(".narada")).unwrap();
        let src = PathBuf::from("C:/Users/andrey/src");
        let wt = PathBuf::from("C:/Users/andrey/wt");
        fs::write(
            root.join(".narada/allowed-roots.json"),
            serde_json::to_vec(&json!({
                "extra_allowed_roots": [
                    src.to_string_lossy(),
                    wt.to_string_lossy(),
                    src.to_string_lossy()
                ],
                "temp_allowed_roots": ["C:/Users/andrey/tmp"]
            }))
            .unwrap(),
        )
        .unwrap();

        let mut args = vec![
            "--allowed-root".to_string(),
            path_text(&root),
        ];
        append_durable_worker_allowed_roots("worker-delegation", &root, &mut args).unwrap();
        let roots = args
            .windows(2)
            .filter(|pair| pair[0] == "--allowed-root")
            .map(|pair| comparable_root(Path::new(&pair[1])))
            .collect::<BTreeSet<_>>();
        assert!(!roots.contains(&comparable_root(&root)));
        assert!(roots.contains(&comparable_root(&src)));
        assert!(roots.contains(&comparable_root(&wt)));
        assert_eq!(roots.len(), 2);
        assert!(!roots.contains(&comparable_root(Path::new("C:/Users/andrey/tmp"))));

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn native_materialization_receipt_is_accepted_only_for_its_carrier_and_config() {
        let nonce = format!(
            "narada-registrar-receipt-{}-{}",
            std::process::id(),
            time::OffsetDateTime::now_utc().unix_timestamp_nanos()
        );
        let config_path = env::temp_dir().join(format!("{nonce}.json"));
        let config_path_text = config_path.to_string_lossy().to_string();
        let sidecar_path = format!("{config_path_text}.narada-generation.json");
        fs::write(
            &sidecar_path,
            serde_json::to_vec(&json!({
                "carrier_id":"kimi-test",
                "config_path":config_path_text,
                "config_artifact":{"bytes_sha256":"abc123"},
                "managed_projection":{"scope":"whole_document","selectors":[]}
            }))
            .unwrap(),
        )
        .unwrap();

        let receipt = native_materialization_receipt(&config_path_text, "kimi-test").unwrap();
        assert_eq!(receipt.expected_sha256, "abc123");
        assert_eq!(receipt.scope, "whole_document");
        assert!(native_materialization_receipt(&config_path_text, "other-carrier").is_none());

        fs::remove_file(sidecar_path).unwrap();
    }
}
