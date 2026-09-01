fn planned_files(config: &Value, descriptors: &[Value], root: &str) -> Vec<Value> {
    let mut files = vec![
        json!({"path":format!("{root}\\config.json"),"purpose":"Compatibility projection of Site governance coordinates; .narada/site.json is authority seed","mutation":"planned_only_projection"}),
        json!({"path":format!("{root}\\AGENTS.md"),"purpose":"Site-local agent execution contract","mutation":"planned_only"}),
        json!({"path":format!("{root}\\.narada\\site.json"),"purpose":"Site authority seed coordinates","mutation":"planned_only"}),
        json!({"path":format!("{root}\\.narada\\lineage\\events\\site-created.json"),"purpose":"Append-only Site origin/build lineage event","mutation":"planned_only"}),
        json!({"path":format!("{root}\\.narada\\README.md"),"purpose":"Site-local Narada substrate orientation","mutation":"planned_only"}),
        json!({"path":format!("{root}\\.narada\\admission\\admission-ledger.jsonl"),"purpose":"Site-local admission ledger","mutation":"planned_only"}),
        json!({"path":format!("{root}\\.narada\\inbox\\README.md"),"purpose":"Site-local intake placeholder","mutation":"planned_only"}),
    ];
    if config
        .pointer("/task_lifecycle/enable")
        .is_some_and(|v| v != false)
    {
        files.push(json!({"path":format!("{root}\\.ai\\site-task-lifecycle-admission.json"),"purpose":"Task lifecycle local admission manifest","mutation":"requires_separate_admission"}));
    }
    if config
        .pointer("/agent_context/enable")
        .is_some_and(|v| v != false)
    {
        files.push(json!({"path":format!("{root}\\.ai\\agent-context-memory-admission.json"),"purpose":"Agent context local admission manifest","mutation":"requires_separate_admission"}));
    }
    if config.pointer("/mcp/intent") == Some(&Value::String("descriptor_only".to_string())) {
        if let Some(surfaces) = config.pointer("/mcp/surfaces").and_then(Value::as_array) {
            for surface in surfaces.iter().filter_map(Value::as_str) {
                files.push(json!({"path":format!("{root}\\.narada\\mcp\\descriptors\\{surface}.json"),"purpose":format!("{surface} MCP descriptor"),"mutation":"descriptor_materialization_only"}));
            }
        }
    }
    for descriptor in descriptors
        .iter()
        .filter(|v| v["posture"] == "descriptor_only")
    {
        let safe = descriptor["package_name"]
            .as_str()
            .unwrap_or_default()
            .trim_start_matches("@narada-core/");
        files.push(json!({"path":format!("{root}\\.narada\\admission\\package-slices\\{safe}.json"),"purpose":format!("{} descriptor package slice",descriptor["package_name"].as_str().unwrap_or_default()),"mutation":"descriptor_materialization_only"}));
    }
    files
}
fn required_admissions(config: &Value, descriptors: &[Value]) -> Vec<Value> {
    let mut values =
        vec![json!({"admission":"filesystem_creation","status":"not_admitted_in_dry_run"})];
    if config
        .pointer("/storage/intent")
        .and_then(Value::as_str)
        .is_some_and(|v| v != "none")
    {
        values.push(
            json!({"admission":"local_storage_adapter","status":"separate_admission_required"}),
        );
    }
    if config
        .pointer("/task_lifecycle/enable")
        .is_some_and(|v| v != false)
    {
        values.push(json!({"admission":"task_lifecycle_db_init_and_mutation","status":"separate_admission_required"}));
    }
    if config
        .pointer("/agent_context/enable")
        .is_some_and(|v| v != false)
    {
        values.push(json!({"admission":"agent_context_storage_and_hydration","status":"separate_admission_required"}));
    }
    if config
        .pointer("/mcp/intent")
        .and_then(Value::as_str)
        .is_some_and(|v| v != "none")
    {
        values.push(
            json!({"admission":"live_mcp_registration","status":"separate_admission_required"}),
        );
    }
    for (package, admission) in [
        (
            "@narada-core/site-inbox",
            "site_inbox_local_substrate_and_publication",
        ),
        (
            "@narada-core/site-config",
            "site_config_registry_probe_execution",
        ),
        (
            "@narada-core/site-lift",
            "site_lift_adoption_materialization",
        ),
    ] {
        if descriptors.iter().any(|v| v["package_name"] == package) {
            values.push(json!({"admission":admission,"status":"separate_admission_required"}));
        }
    }
    if !descriptors.is_empty() {
        values.push(
            json!({"admission":"package_descriptor_selection","status":"included_in_dry_run"}),
        );
    }
    values
}
fn collect_strings(value: &Value, depth: usize, out: &mut Vec<String>) {
    if depth > 32 || out.len() >= 10_000 {
        return;
    }
    match value {
        Value::String(v) => out.push(v.clone()),
        Value::Array(values) => {
            for value in values.iter().take(10_000 - out.len()) {
                collect_strings(value, depth + 1, out)
            }
        }
        Value::Object(values) => {
            for value in values.values().take(10_000 - out.len()) {
                collect_strings(value, depth + 1, out)
            }
        }
        _ => {}
    }
}

fn packages_for_preset(preset: &str) -> Vec<&'static str> {
    match preset {
        "agent-site-core" => vec![
            "@narada-core/site-task-lifecycle",
            "@narada-core/agent-context-memory",
            "@narada-core/site-inbox",
        ],
        "task-lifecycle" => vec!["@narada-core/site-task-lifecycle"],
        "agent-memory" => vec!["@narada-core/agent-context-memory"],
        "site-machinery" => vec![
            "@narada-core/site-inbox",
            "@narada-core/site-config",
            "@narada-core/site-lift",
        ],
        _ => vec![],
    }
}
fn package_descriptor(name: &str) -> Value {
    let (descriptors, denied) = match name {
        "@narada-core/site-task-lifecycle" => (
            vec![
                "receiving_site_setup_plan",
                "task_db_schema_init_plan",
                "task_db_adapter_conformance_contract",
                "task_admission_write_request",
                "mcp_registration_descriptor",
            ],
            vec![
                "package-owned SQLite",
                "SQLite mutation",
                "source task DB/history import",
                "live MCP registration",
            ],
        ),
        "@narada-core/agent-context-memory" => (
            vec![
                "named_agent_registry_fragment",
                "session_start_contract",
                "checkpoint_descriptor",
                "hydration_request_descriptor",
                "agent_context_schema_init_plan",
                "mcp_registration_descriptor",
                "capability_registry_fragment",
            ],
            vec![
                "package-owned SQLite",
                "runtime hydration execution",
                "source checkpoint/agent-context DB import",
                "live MCP registration",
            ],
        ),
        "@narada-core/site-inbox" => (
            vec![
                "envelope_admission_request",
                "admission_decision",
                "portable_artifact_plan",
                "crossing_coordinates",
                "inbox_refusal_guard",
            ],
            vec![
                "inbox DB mutation",
                "portable envelope file write",
                "source inbox DB/history import",
                "task promotion",
                "live MCP registration",
            ],
        ),
        "@narada-core/site-config" => (
            vec![
                "known_site_registry_entry",
                "capability_edge",
                "capability_denial",
                "registered_site_probe_request",
                "registered_site_probe_report",
            ],
            vec![
                "target Site config mutation",
                "target task/inbox DB import",
                "trust record mutation",
                "live probe execution",
                "arbitrary client/project scan",
            ],
        ),
        "@narada-core/site-lift" => (
            vec![
                "artifact_descriptor",
                "adoption_plan",
                "adoption_command_packet",
                "nonportable_state_refusal",
                "receiver_admission_summary",
            ],
            vec![
                "file copy/install/bootstrap",
                "source runtime state import",
                "receiving Site mutation authority",
                "live MCP registration",
                "catalog publication",
            ],
        ),
        _ => (vec![], vec!["unknown package cannot grant live capability"]),
    };
    json!({"package_name":name,"posture":if descriptors.is_empty(){"unknown_package_refused"}else{"descriptor_only"},"descriptors":descriptors,"denied_live_effects":denied})
}

pub(crate) fn kinds() -> Value {
    json!({"status":"success","mutation_performed":false,"kinds":KINDS.iter().map(kind_json).collect::<Vec<_>>()})
}

