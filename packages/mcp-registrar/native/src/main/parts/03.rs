fn admit_structured_command_python(contract: &mut Value) {
    let Some(surface) = contract
        .pointer_mut("/read_models/registrar_surface_list/items")
        .and_then(Value::as_array_mut)
        .and_then(|items| {
            items
                .iter_mut()
                .find(|item| item["id"] == "structured-command")
        })
    else {
        return;
    };
    ensure_python_command_admission(&mut surface["args"]);
    if let Some(projections) = surface["projections"].as_array_mut() {
        for projection in projections {
            ensure_python_command_admission(&mut projection["args"]);
        }
    }
    if let Some(projections) = surface["descriptor"]["projections"].as_array_mut() {
        for projection in projections {
            ensure_python_command_admission(&mut projection["transport"]["args"]);
        }
    }
    surface["descriptor_digest"] = json!(sha256_text(&canonical_json(&surface["descriptor"])));
}

fn extend_epistemic_catalog(contract: &mut Value) {
    let tools = [
        ("epistemic_graph_guidance", true),
        ("epistemic_graph_status", true),
        ("epistemic_graph_query", true),
        ("epistemic_graph_query_batch", true),
        ("epistemic_graph_source_inspect", true),
        ("epistemic_graph_neighborhood", true),
        ("epistemic_graph_snapshot", true),
        ("epistemic_graph_sequence_create", false),
        ("epistemic_graph_sequence_status", true),
        ("epistemic_graph_sequence_list", true),
        ("epistemic_graph_sequence_claim_next", false),
        ("epistemic_graph_sequence_claims", true),
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
        "id":"default","transport":{"kind":"stdio","command":"narada-ledger-domain","args":["--domain","{mcp_surfaces_root}/shared/ledger-domain-epistemic/domain.json","--site-root","{site_root}"],"env":[]},
        "injection_scope":"local_site","default_injection":"enabled","runtime_requirements":[],"authority_requirements":["scope.local_site"],
        "lifecycle":{"mode":"replayable","reason":"Canonical events are immutable and the query projection is rebuildable."}
    });
    let descriptor = json!({
        "schema_version":"2.0","source":"native","surface_id":"epistemic-graph","surface_version":"0.1.0",
        "package":"@narada-core/ledger-domain-mcp","guidance_tool":"epistemic_graph_guidance","tools":descriptor_tools,
        "projections":[projection.clone()],"metadata":{"authority":"tracked_event_ledger","truth_certification":false}
    });
    let descriptor_digest = sha256_text(&canonical_json(&descriptor));
    let tool_contract_digest = sha256_text(&canonical_json(&descriptor["tools"]));
    let names = tools
        .iter()
        .map(|(name, _)| json!(name))
        .collect::<Vec<_>>();
    let item = json!({
        "id":"epistemic-graph","package":"ledger-domain-mcp","entrypoint":"{mcp_surfaces_root}/ledger-domain-mcp/dist/native/narada-ledger-domain.exe",
        "kind":"mcp_surface","args":["--domain","{mcp_surfaces_root}/shared/ledger-domain-epistemic/domain.json","--site-root","{site_root}"],"tools":names,
        "projections":[{"id":"default","injection_scope":"local_site","execution":{"adapter":"stdio","tenancy":"session_isolated","replacement":"manual"},"restart_owner":"local_site","runtime_requirements":[],"env_vars":[],"command":"narada-ledger-domain","entrypoint":"{mcp_surfaces_root}/ledger-domain-mcp/dist/native/narada-ledger-domain.exe","args":["--domain","{mcp_surfaces_root}/shared/ledger-domain-epistemic/domain.json","--site-root","{site_root}"]}],
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
    let intent = || json!({"type":"object","properties":{"instruction":{"type":"string","minLength":1,"maxLength":65536},"task":{"type":"string","minLength":1,"maxLength":65536},"goal":{"type":"string","minLength":1,"maxLength":65536},"summary":{"type":"string","minLength":1,"maxLength":65536},"mode":{"type":"string","minLength":1,"maxLength":512}},"additionalProperties":false,"anyOf":[{"required":["instruction"]},{"required":["task"]},{"required":["goal"]},{"required":["summary"]}],"maxProperties":256});
    let constraints = || json!({"type":"object","properties":{"authority":{"type":"string","enum":["read","write","command"]},"cognition":{"type":"string","enum":["low","medium","high"],"default":"low"},"cwd":{"type":"string","minLength":1,"maxLength":4096},"preflight_paths":{"type":"array","maxItems":64,"items":{"type":"object","properties":{"path":{"type":"string","minLength":1,"maxLength":4096},"access":{"type":"string","enum":["read","write","create"],"default":"read"}},"required":["path"],"additionalProperties":false,"maxProperties":256}},"invocation_plan_ref":{"type":"string","minLength":6,"maxLength":512,"pattern":"^plan:[A-Za-z0-9._:-]+$"},"max_run_ms":{"type":"integer","minimum":1,"maximum":1800000,"default":300000,"description":"Hard worker runtime deadline enforced by the native authority."},"wait_for_completion":{"type":"boolean","default":false,"description":"Return after bounded child completion polling when true; false returns the accepted running record immediately."},"wait_timeout_ms":{"type":"integer","minimum":0,"maximum":300000,"default":30000,"description":"Maximum inline completion wait when wait_for_completion is true."}},"additionalProperties":false,"maxProperties":256});
    for item in items {
        let id = item.get("id").and_then(Value::as_str).unwrap_or_default().to_owned();
        if id == "surface-feedback" { ensure_feedback_site_reporter_projection(item); }
        let Some(tools) = item.pointer_mut("/descriptor/tools").and_then(Value::as_array_mut) else { continue; };
        for tool in tools {
            let name = tool.get("name").and_then(Value::as_str).unwrap_or_default();
            let schema = match (id.as_str(), name) {
                ("epistemic-graph", "epistemic_graph_guidance") => Some(json!({"type":"object","properties":{"workflow":{"type":"string","maxLength":256},"tool":{"type":"string","maxLength":256}},"additionalProperties":false})),
                ("epistemic-graph", "epistemic_graph_sequence_create") => Some(json!({"type":"object","properties":{"sequence_name":{"type":"string","minLength":1,"maxLength":120},"actor":{"type":"string","minLength":1,"maxLength":256},"authority_basis":{"type":"object","minProperties":1,"maxProperties":32},"start_at":{"type":"integer","minimum":1,"default":1},"idempotency_key":{"type":"string","minLength":1,"maxLength":256}},"required":["sequence_name","actor","authority_basis"],"additionalProperties":false})),
                ("epistemic-graph", "epistemic_graph_sequence_status") => Some(json!({"type":"object","properties":{"sequence_name":{"type":"string","minLength":1,"maxLength":120}},"required":["sequence_name"],"additionalProperties":false})),
                ("epistemic-graph", "epistemic_graph_sequence_list") => Some(json!({"type":"object","properties":{"limit":{"type":"integer","minimum":1,"maximum":100,"default":100},"offset":{"type":"integer","minimum":0,"default":0}},"additionalProperties":false})),
                ("epistemic-graph", "epistemic_graph_sequence_claim_next") => Some(json!({"type":"object","properties":{"sequence_name":{"type":"string","minLength":1,"maxLength":120},"actor":{"type":"string","minLength":1,"maxLength":256},"authority_basis":{"type":"object","minProperties":1,"maxProperties":32},"idempotency_key":{"type":"string","minLength":1,"maxLength":256}},"required":["sequence_name","actor","authority_basis","idempotency_key"],"additionalProperties":false})),
                ("epistemic-graph", "epistemic_graph_sequence_claims") => Some(json!({"type":"object","properties":{"sequence_name":{"type":"string","minLength":1,"maxLength":120},"limit":{"type":"integer","minimum":1,"maximum":100,"default":100},"offset":{"type":"integer","minimum":0,"default":0}},"required":["sequence_name"],"additionalProperties":false})),
                ("worker-delegation", "worker_run") => Some(json!({"type":"object","properties":{"intent":intent(),"constraints":constraints()},"required":["intent"],"additionalProperties":false})),
                ("worker-delegation", "worker_run_batch") => Some(json!({"type":"object","properties":{"requests":{"type":"array","minItems":1,"maxItems":50,"items":{"type":"object","properties":{"intent":intent(),"constraints":constraints()},"required":["intent"],"additionalProperties":false,"maxProperties":256}}},"required":["requests"],"additionalProperties":false,"maxProperties":256})),
                ("worker-delegation", "worker_run_status") => Some(json!({"type":"object","properties":{"run_id":{"type":"string","minLength":1,"maxLength":256},"compact":{"type":"boolean","default":true}},"required":["run_id"],"additionalProperties":false})),
                ("worker-delegation", "worker_runs_list") => Some(json!({"type":"object","properties":{"limit":{"type":"integer","minimum":1,"maximum":200},"compact":{"type":"boolean","default":true},"site_scope":{"type":"string","enum":["current_site"],"default":"current_site","description":"Runs are filtered by the server-bound Site root; caller-supplied cross-site identity is not accepted."},"include_running":{"type":"boolean"},"include_completed":{"type":"boolean"}},"additionalProperties":false})),
                ("worker-delegation", "worker_run_wait") => Some(json!({"type":"object","properties":{"run_id":{"type":"string","minLength":1,"maxLength":256},"compact":{"type":"boolean","default":true},"timeout_ms":{"type":"integer","minimum":0,"maximum":300000,"default":30000,"description":"Maximum bounded state-file polling interval."}},"required":["run_id"],"additionalProperties":false})),
                ("worker-delegation", "worker_run_wait_batch") => Some(json!({"type":"object","properties":{"run_ids":{"type":"array","minItems":1,"maxItems":50,"items":{"type":"string","minLength":1,"maxLength":256}},"compact":{"type":"boolean","default":true},"timeout_ms":{"type":"integer","minimum":0,"maximum":180000,"default":30000},"poll_ms":{"type":"integer","minimum":100,"maximum":30000,"default":5000}},"required":["run_ids"],"additionalProperties":false})),
                ("worker-delegation", "worker_config_resolve") => Some(json!({"type":"object","properties":{"cwd":{"type":"string","minLength":1,"maxLength":4096},"constraints":constraints()},"additionalProperties":false})),
                ("worker-delegation", "worker_result_show") => Some(json!({"type":"object","properties":{"run_id":{"type":"string","minLength":1,"maxLength":256},"offset":{"type":"integer","minimum":0,"maximum":256000},"limit":{"type":"integer","minimum":1,"maximum":256000}},"required":["run_id"],"additionalProperties":false})),
                ("worker-delegation", "worker_command_run") => Some(json!({"type":"object","properties":{"authority":{"type":"string","const":"command"},"command":{"type":"string","minLength":1,"maxLength":512},"args":{"type":"array","maxItems":64,"items":{"type":"string","maxLength":4096}},"cwd":{"type":"string","minLength":1,"maxLength":4096},"timeout_ms":{"type":"integer","minimum":1,"maximum":60000,"default":10000},"stdout_limit":{"type":"integer","minimum":1,"maximum":65536,"default":4096},"stderr_limit":{"type":"integer","minimum":1,"maximum":65536,"default":4096}},"required":["authority","command"],"additionalProperties":false})),
                ("surface-feedback", "surface_feedback_submit") => Some(json!({
                    "type":"object","properties":{
                        "surface_id":{"type":"string","minLength":1},
                        "submitter_site_id":{"type":"string","minLength":1,"description":"Optional assertion that must equal the server-bound Site identity. Omit for ordinary submission."},
                        "submitter_principal":{"type":"string","minLength":1,"description":"Optional assertion that must equal the server-bound principal. Omit for ordinary submission."},
                        "kind":{"type":"string","enum":["bug","improvement","gap","observation"]},
                        "summary":{"type":"string","minLength":1},"details":{"type":"string"},
                        "idempotency_key":{"type":"string","minLength":1,"description":"Stable retry key; reuse with different content is refused."}
                    },"required":["surface_id","kind","summary"],"additionalProperties":false
                })),
                _ => None,
            };
            if let Some(schema) = schema { tool["input_schema"] = schema; }
        }
    }
}

fn ensure_feedback_site_reporter_projection(item: &mut Value) {
    // The TypeScript rollback package (packages/surface-feedback-mcp) was removed;
    // the default projection is native-only and resolves to the shared native surface.
    let native_entrypoint = "{mcp_surfaces_root}/shared/mcp-surfaces-native/dist/native/narada-mcp-surfaces.exe";
    let default_args = json!(["--feedback-root","{site_control_root}/feedback","--canonical-feedback-root","{site_control_root}/feedback","--task-lifecycle-root","{site_root}","--site-id","{site_id}"]);
    item["package"] = json!("mcp-surfaces-native");
    item["entrypoint"] = json!(native_entrypoint);
    if let Some(descriptor) = item.get_mut("descriptor") {
        descriptor["package"] = json!("@narada-core/mcp-surfaces-native");
        descriptor["surface_version"] = json!("0.3.0");
        if let Some(projections) = descriptor.get_mut("projections").and_then(Value::as_array_mut) {
            for projection in projections.iter_mut() {
                if projection["id"] == "default" {
                    projection["transport"] = json!({"kind":"stdio","command":"narada-mcp-surfaces","args":default_args,"env":["NARADA_SURFACE_FEEDBACK_ROOT"]});
                }
            }
        }
    }
    if let Some(projections) = item.get_mut("projections").and_then(Value::as_array_mut) {
        for projection in projections.iter_mut() {
            if projection["id"] == "default" {
                projection["command"] = json!("narada-mcp-surfaces");
                projection["entrypoint"] = json!(native_entrypoint);
                projection["args"] = default_args.clone();
            }
        }
    }
    let args = json!(["--feedback-root","{user_site_control_root}/feedback","--canonical-feedback-root","{user_site_control_root}/feedback","--task-lifecycle-root","{site_root}","--site-id","{site_id}"]);
    let descriptor_projection = json!({
        "id":"site-reporter","transport":{"kind":"stdio","command":"narada-mcp-surfaces","args":args,"env":["NARADA_SURFACE_FEEDBACK_ROOT"]},
        "injection_scope":"local_site","default_injection":"disabled","runtime_requirements":[],"authority_requirements":["scope.local_site"],
        "lifecycle":{"mode":"replayable","reason":"Site-bound reporters write to the canonical User Site feedback store while retaining mechanically bound Site identity."}
    });
    if let Some(projections) = item.pointer_mut("/descriptor/projections").and_then(Value::as_array_mut) {
        if !projections.iter().any(|projection| projection["id"] == "site-reporter") { projections.push(descriptor_projection); }
    }
    let entrypoint = item.get("entrypoint").cloned().unwrap_or(Value::Null);
    if let Some(projections) = item.get_mut("projections").and_then(Value::as_array_mut) {
        if !projections.iter().any(|projection| projection["id"] == "site-reporter") {
            projections.push(json!({
                "id":"site-reporter","injection_scope":"local_site","execution":{"adapter":"stdio","tenancy":"session_isolated","replacement":"manual"},
                "restart_owner":"local_site","runtime_requirements":[],"env_vars":["NARADA_SURFACE_FEEDBACK_ROOT"],"command":"narada-mcp-surfaces",
                "entrypoint":entrypoint,"args":args
            }));
        }
    }
    if let Some(descriptor) = item.get("descriptor").cloned() {
        item["descriptor_digest"] = json!(sha256_text(&canonical_json(&descriptor)));
        item["tool_contract_digest"] = json!(sha256_text(&canonical_json(&descriptor["tools"])));
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
