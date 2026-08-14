use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use serde_json::{json, Map, Value};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

const MAX_RELATION_FILE_BYTES: u64 = 8 * 1024 * 1024;
const MAX_RELATIONS: usize = 10_000;

struct LifecycleKind {
    kind: &'static str,
    purpose: &'static str,
    source_required: bool,
    target_required: bool,
    authority_modes: &'static [&'static str],
    artifacts: &'static [&'static str],
}

const KINDS: &[LifecycleKind] = &[
    LifecycleKind { kind:"clone", purpose:"Create another Site embodiment from an existing Site while declaring whether mutation authority stays, migrates, or forwards.", source_required:true, target_required:true, authority_modes:&["read_only","forwarding","authority_migration"], artifacts:&["source_site_ref","provenance_record","authority_map","trace_handoff","read_back_confirmation","embodiment_policy"] },
    LifecycleKind { kind:"fork", purpose:"Create a divergent Site lineage with explicit provenance and independent future authority.", source_required:true, target_required:true, authority_modes:&["new_authority"], artifacts:&["source_site_ref","provenance_record","authority_map","trace_handoff","read_back_confirmation","lineage_boundary"] },
    LifecycleKind { kind:"split", purpose:"Extract a sub-locus from a Site with traceable provenance and explicit authority transfer or residual linkage.", source_required:true, target_required:true, authority_modes:&["partial_transfer","residual_linkage"], artifacts:&["source_site_ref","provenance_record","authority_map","trace_handoff","read_back_confirmation","extraction_manifest","residual_linkage"] },
    LifecycleKind { kind:"absorb", purpose:"Admit sidecar or local Site knowledge, machinery, or trace into a broader Site or Narada proper.", source_required:true, target_required:true, authority_modes:&["admission_review"], artifacts:&["source_site_ref","provenance_record","authority_map","trace_handoff","read_back_confirmation","admission_bundle","re_instantiation_evidence"] },
    LifecycleKind { kind:"migrate", purpose:"Move Site authority or substrate while preserving identity, provenance, config, trace, and read-back confirmation.", source_required:true, target_required:true, authority_modes:&["authority_migration"], artifacts:&["source_site_ref","provenance_record","authority_map","trace_handoff","read_back_confirmation","migration_plan","cutover_confirmation"] },
    LifecycleKind { kind:"re-instantiate", purpose:"Rebuild a Site from template, durable trace, config, and evidence, then prove the originating case still runs.", source_required:true, target_required:true, authority_modes:&["reconstruction_proof"], artifacts:&["source_site_ref","provenance_record","authority_map","trace_handoff","read_back_confirmation","template_ref","reconstruction_proof"] },
    LifecycleKind { kind:"archive", purpose:"Retire a Site from active operation while preserving trace, provenance, and explicit non-authority posture.", source_required:true, target_required:false, authority_modes:&["retired_non_authority"], artifacts:&["source_site_ref","archive_manifest","authority_retirement_record","trace_preservation_record"] },
];

pub(crate) fn create_presets() -> Value {
    let presets = ["minimal","agent-site-core","agent-memory","task-lifecycle","site-machinery"].into_iter().map(|preset| {
        let (label,recommended,use_when,includes,does_not_include)=match preset {
            "agent-site-core"=>("Agent Site core",true,"Create a useful agent-facing Site baseline with task lifecycle, agent memory, and canonical inbox descriptors.",vec!["task_lifecycle","agent_context_memory","canonical_inbox"],vec!["site_config_awareness","site_lift_adoption","live capability grants","source runtime import"]),
            "task-lifecycle"=>("Task lifecycle only",false,"Create only the task lifecycle descriptor slice.",vec!["task_lifecycle"],vec!["agent_context_memory","canonical_inbox","site_config_awareness","site_lift_adoption","live capability grants"]),
            "agent-memory"=>("Agent memory only",false,"Create only the agent context memory descriptor slice.",vec!["agent_context_memory"],vec!["task_lifecycle","canonical_inbox","site_config_awareness","site_lift_adoption","live capability grants"]),
            "site-machinery"=>("Inbox/config/lift",false,"Create the narrower inbox, known-Site config, and lift/adoption descriptor bundle.",vec!["canonical_inbox","site_config_awareness","site_lift_adoption"],vec!["task_lifecycle","agent_context_memory","live capability grants","source runtime import"]),
            _=>("Minimal skeleton",false,"Create a bare Site skeleton without descriptor packages.",vec![],vec!["task_lifecycle","agent_context_memory","canonical_inbox","site_config_awareness","site_lift_adoption","live capability grants"]),
        };
        let packages=packages_for_preset(preset);
        json!({"preset":preset,"label":label,"recommended":recommended,"use_when":use_when,"includes":includes,"does_not_include":does_not_include,"template_id":format!("narada-proper.templates.site.{preset}.v0"),"exposure_class":if preset=="minimal"{"mutating_guarded"}else{"descriptor_only"},"package_components":packages,"descriptor_components":packages.iter().map(|name|package_descriptor(name)).collect::<Vec<_>>(),"operational_commands":{"dry_run":format!("narada sites create --preset {preset} --site-id <id> --root <path> --dry-run --format json"),"skeleton":format!("narada sites create --preset {preset} --site-id <id> --root <path> --format json"),"live":if preset=="minimal"{Value::Null}else{Value::String(format!("narada sites create --preset {preset} --site-id <id> --root <path> --execute-live --live-authority-basis <basis> --format json"))}},"admission_boundary":{"package_selection_grants_live_capability":false,"source_state_imported":false,"live_execution_requires_explicit_authority":true}})
    }).collect::<Vec<_>>();
    json!({"schema":"narada.create_site.presets.v0","status":"ok","recommended_preset":"agent-site-core","default_interactive_preset":"agent-site-core","presets":presets,"non_claims":["source Site import/migration/lift","implicit capability grants","private MCP client config mutation","real Windows profile mutation outside target Site artifacts","PC/operator-surface mutation"]})
}

pub(crate) fn create_plan(args: &Map<String, Value>, bound_root: &Path) -> Result<Value, Value> {
    if args.contains_key("output_plan") {
        return Err(error("site_create_plan_output_refused","site_create_plan is read-only; persist a returned plan through an explicit filesystem write authority"));
    }
    let (config, config_path) = if let Some(path) = text(args, "config") {
        let requested = PathBuf::from(path);
        let absolute = if requested.is_absolute() {
            requested
        } else {
            bound_root.join(requested)
        };
        let canonical = absolute
            .canonicalize()
            .map_err(io_error("site_create_config_unavailable"))?;
        let bound = bound_root
            .canonicalize()
            .map_err(io_error("site_lifecycle_bound_root_unavailable"))?;
        if !canonical.starts_with(&bound) {
            return Err(error(
                "site_create_config_outside_bound_site",
                "config must remain inside the bound Site root",
            ));
        }
        let metadata =
            fs::metadata(&canonical).map_err(io_error("site_create_config_stat_failed"))?;
        if metadata.len() > 4 * 1024 * 1024 {
            return Err(error(
                "site_create_config_too_large",
                "create-site config exceeds 4 MiB",
            ));
        }
        let value = serde_json::from_slice::<Value>(
            &fs::read(&canonical).map_err(io_error("site_create_config_read_failed"))?,
        )
        .map_err(|cause| error("site_create_config_invalid", &cause.to_string()))?;
        (value, canonical.to_string_lossy().to_string())
    } else {
        let preset = text(args, "preset").unwrap_or_else(|| "agent-site-core".to_string());
        let site_id = text(args, "site_id");
        let root = text(args, "root");
        if site_id.is_none() || root.is_none() {
            return Ok(
                json!({"status":"error","error":"missing_config_or_shorthand","message":"site_create_plan requires config or site_id and root."}),
            );
        }
        (
            preset_config(
                &preset,
                &site_id.unwrap(),
                &root.unwrap(),
                text(args, "site_kind").as_deref().unwrap_or("project"),
                text(args, "authority_locus")
                    .as_deref()
                    .unwrap_or("project"),
            ),
            "<inline:create-site-options>".to_string(),
        )
    };
    Ok(build_plan(&config, &config_path))
}

fn preset_config(
    preset: &str,
    site_id: &str,
    root: &str,
    site_kind: &str,
    authority_locus: &str,
) -> Value {
    let packages = packages_for_preset(preset);
    let components = packages
        .iter()
        .map(|v| Value::String((*v).to_string()))
        .collect::<Vec<_>>();
    let (mut storage, mut mcp, mut capabilities, mut inbox, mut task, mut context) = (
        json!({"intent":"none"}),
        json!({"intent":"none","surfaces":[]}),
        json!({"policy":"none","required":[],"denied":[]}),
        json!({"enable":"drop_only"}),
        json!({"enable":false}),
        json!({"enable":false}),
    );
    match preset {
        "agent-site-core" => {
            storage = json!({"intent":"descriptor_only","driver_preference":"sqlite3-cli","mutation_mode":"none"});
            mcp = json!({"intent":"descriptor_only","surfaces":["site_task_lifecycle","agent_context_memory"]});
            capabilities = json!({"policy":"declare_required","required":["task_lifecycle","agent_context_memory","canonical_inbox"],"denied":["source_task_db_import","source_checkpoint_import","source_inbox_history_import"]});
            inbox = json!({"enable":"canonical_envelope_intake"});
            task = json!({"enable":"descriptor_only","package":"@narada-core/site-task-lifecycle"});
            context =
                json!({"enable":"descriptor_only","package":"@narada-core/agent-context-memory"});
        }
        "task-lifecycle" => {
            storage = json!({"intent":"descriptor_only","driver_preference":"sqlite3-cli","mutation_mode":"none"});
            mcp = json!({"intent":"descriptor_only","surfaces":["site_task_lifecycle"]});
            capabilities = json!({"policy":"declare_required","required":["task_lifecycle"],"denied":["source_task_db_import"]});
            inbox = json!({"enable":"canonical_envelope_intake"});
            task = json!({"enable":"descriptor_only","package":"@narada-core/site-task-lifecycle"});
        }
        "agent-memory" => {
            storage = json!({"intent":"descriptor_only","driver_preference":"sqlite3-cli","mutation_mode":"none"});
            mcp = json!({"intent":"descriptor_only","surfaces":["agent_context_memory"]});
            capabilities = json!({"policy":"declare_required","required":["agent_context_memory"],"denied":["source_checkpoint_import"]});
            context =
                json!({"enable":"descriptor_only","package":"@narada-core/agent-context-memory"});
        }
        "site-machinery" => {
            capabilities = json!({"policy":"declare_required","required":["canonical_inbox","site_config_awareness","site_lift_adoption"],"denied":["source_site_runtime_import","cross_site_mutation"]});
            inbox = json!({"enable":"canonical_envelope_intake"});
        }
        _ => {}
    }
    json!({"schema":"narada.create_site.options.v0","mode":"dry_run","preset":preset,"template_catalog":{"template_id":format!("narada-proper.templates.site.{preset}.v0"),"template_components":components},"site":{"site_id":site_id,"site_kind":site_kind,"authority_locus":authority_locus,"site_root":root,"workspace_root":root,"substrate":"windows-native","execution_surface":"windows_native","sync_posture":"hybrid_capable_plain_folder"},"packages":packages.iter().map(|name|json!({"name":name})).collect::<Vec<_>>(),"identity":{"named_agents":[],"role_assignments":[],"role_compatibility_identities":[],"claimed_identity_evidence":[],"mechanical_verification_basis":[]},"storage":storage,"mcp":mcp,"capabilities":capabilities,"inbox":inbox,"task_lifecycle":task,"agent_context":context,"operator_surface":{"intent":"none"},"windows_pwsh":{"profile":"emit_example","path_style":"windows"},"evidence":{"template_refs":[format!("narada-proper.templates.site.{preset}.v0")],"refused_imports":[]}})
}

fn build_plan(config: &Value, config_path: &str) -> Value {
    let preset = config["preset"].as_str().unwrap_or("minimal");
    let packages = config["packages"].as_array().cloned().unwrap_or_default();
    let descriptors = packages
        .iter()
        .map(|pkg| package_descriptor(pkg["name"].as_str().unwrap_or_default()))
        .collect::<Vec<_>>();
    let mut refusals = Vec::new();
    if config["schema"] != "narada.create_site.options.v0" {
        refusals.push(json!({"code":"invalid_config_schema","message":"Expected schema narada.create_site.options.v0.","evidence":config["schema"]}));
    }
    if ![
        "minimal",
        "agent-site-core",
        "agent-memory",
        "task-lifecycle",
        "site-machinery",
    ]
    .contains(&preset)
    {
        refusals.push(json!({"code":if preset=="full-operator-surface-aware-user-site"{"preset_requires_unadmitted_operator_surface"}else{"unsupported_preset"},"message":format!("Unsupported descriptor-only preset: {preset}"),"evidence":preset}));
    }
    for coordinate in ["site_id", "site_kind", "authority_locus", "site_root"] {
        if config["site"][coordinate]
            .as_str()
            .map(str::trim)
            .unwrap_or_default()
            .is_empty()
        {
            refusals.push(json!({"code":"missing_site_coordinate","message":format!("Missing site.{coordinate}.")}));
        }
    }
    for descriptor in &descriptors {
        if descriptor["posture"] == "unknown_package_refused" {
            refusals.push(json!({"code":"unknown_package_refused","message":"Only Narada proper create-site template components can be expanded by this dry-run command.","evidence":descriptor["package_name"]}));
        }
    }
    if config.pointer("/operator_surface/pc_locus_required") == Some(&Value::Bool(true)) {
        refusals.push(json!({"code":"pc_locus_authority_missing","message":"create site does not assume PC-locus authority; PC setup requires separate admission."}));
    }
    for(value,code,reason)in[(config.pointer("/storage/intent"),"live_adapter_admission_missing","create-site dry-run cannot admit or execute a storage adapter."),(config.pointer("/mcp/intent"),"live_mcp_registration_admission_missing","create-site dry-run cannot perform live MCP registration."),(config.pointer("/capabilities/policy"),"package_selection_does_not_grant_live_capability","Capability grants require separate admission; package/template selection is descriptor-only."),(config.pointer("/windows_pwsh/profile"),"live_profile_write_admission_missing","Windows PowerShell profile writes require separate local admission and execute posture.")]{let denied=matches!((value.and_then(Value::as_str),code),(Some("local_adapter_admitted"),_)|(Some("local_registration_admitted"),_)|(Some("admit_local"),_)|(Some("admit_profile_write"),_));if denied{refusals.push(json!({"code":code,"message":reason}));}}
    let mut strings = Vec::new();
    collect_strings(config, 0, &mut strings);
    for value in strings {
        let lower = value.replace('/', "\\").to_ascii_lowercase();
        if lower.contains("task-lifecycle.db")
            || lower.contains("agent-context.sqlite")
            || lower.contains("agent-context.db")
            || lower.contains("\\.narada\\checkpoints")
            || lower.contains("\\.ai\\checkpoints")
            || lower.contains("\\.ai\\do-not-open\\tasks")
            || lower.contains("\\.ai\\inbox")
            || lower.contains("\\operator-surfaces\\")
            || lower.contains("\\secrets\\")
            || lower.contains("\\tokens\\")
            || lower.contains("\\credentials\\")
        {
            refusals.push(json!({"code":"source_runtime_state_import_refused","message":"source runtime or secret state is not a valid create-site input; use a separate migration/lift/import path.","path":value}));
        }
    }
    let root = config
        .pointer("/site/site_root")
        .and_then(Value::as_str)
        .unwrap_or("<site-root>");
    let template_id = config
        .pointer("/template_catalog/template_id")
        .and_then(Value::as_str)
        .map(ToString::to_string)
        .unwrap_or_else(|| format!("narada-proper.templates.site.{preset}.v0"));
    let components = config
        .pointer("/template_catalog/template_components")
        .cloned()
        .unwrap_or_else(|| {
            Value::Array(
                descriptors
                    .iter()
                    .map(|v| v["package_name"].clone())
                    .collect(),
            )
        });
    let planned = planned_files(config, &descriptors, root);
    let admissions = required_admissions(config, &descriptors);
    json!({"schema":"narada.create_site.dry_run_plan.v0","status":if refusals.is_empty(){"planned"}else{"refused"},"command":"narada sites create","mode":"dry_run","config_path":config_path,"selected_preset":preset,"selected_template":{"template_id":template_id,"template_components":components},"site":config["site"],"package_descriptors":descriptors,"required_local_admissions":admissions,"planned_files":planned,"refusals":refusals,"warnings":[],"evidence":{"template_refs":config.pointer("/evidence/template_refs").cloned().unwrap_or(json!([])),"source_refs_rejected_as_normal_inputs":config.pointer("/evidence/invalid_source_site_inputs").cloned().unwrap_or(json!([])),"dry_run_only":true,"package_selection_grants_live_capability":false,"source_state_imported":false},"non_claims":["filesystem Site creation","local adapter admission","DB init execution","MCP registration execution","runtime hydration execution","capability or secret grants","operator-surface or PC-locus runtime mutation","migration/lift/import from existing Sites"]})
}
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

pub(crate) fn preflight(args: &Map<String, Value>) -> Result<Value, Value> {
    let name = required(args, "kind")?;
    let Some(kind) = KINDS.iter().find(|entry| entry.kind == name) else {
        return Ok(
            json!({"status":"error","mutation_performed":false,"error":format!("Unsupported Site lifecycle transformation: \"{name}\""),"allowed_kinds":KINDS.iter().map(|entry|entry.kind).collect::<Vec<_>>() }),
        );
    };
    let source = text(args, "source_site");
    let target = text(args, "target_site");
    let mode = text(args, "authority_mode");
    let checks = vec![
        check(
            "source_site_declared",
            source.is_some() || !kind.source_required,
            source
                .as_deref()
                .map(|v| format!("Source Site: {v}"))
                .unwrap_or_else(|| "Source Site is required".to_string()),
            format!("Provide source_site for {}", kind.kind),
        ),
        check(
            "target_site_declared",
            target.is_some() || !kind.target_required,
            target
                .as_deref()
                .map(|v| format!("Target Site: {v}"))
                .unwrap_or_else(|| {
                    if kind.target_required {
                        "Target Site is required".to_string()
                    } else {
                        "Target Site is not required".to_string()
                    }
                }),
            format!("Provide target_site for {}", kind.kind),
        ),
        check(
            "authority_mode_declared",
            mode.is_some(),
            mode.as_deref()
                .map(|v| format!("Authority mode: {v}"))
                .unwrap_or_else(|| "Authority mode is required".to_string()),
            format!("Choose one of: {}", kind.authority_modes.join(", ")),
        ),
        check(
            "authority_mode_supported",
            mode.as_deref()
                .is_some_and(|v| kind.authority_modes.contains(&v)),
            mode.as_deref()
                .map(|v| {
                    format!(
                        "Authority mode {v} {} supported",
                        if kind.authority_modes.contains(&v) {
                            "is"
                        } else {
                            "is not"
                        }
                    )
                })
                .unwrap_or_else(|| "Authority mode was not provided".to_string()),
            format!("Choose one of: {}", kind.authority_modes.join(", ")),
        ),
    ];
    let ready = checks.iter().all(|entry| entry["status"] == "pass");
    Ok(
        json!({"status":if ready{"ready"}else{"blocked"},"mutation_performed":false,"kind":kind.kind,"purpose":kind.purpose,"source_site":source,"target_site":target,"authority_mode":mode,"required_artifacts":kind.artifacts,"checks":checks,"next_step":if ready{"Create a governed transformation plan artifact before any Site filesystem, registry, config, inbox, task, or authority mutation."}else{"Resolve failed checks before creating a transformation plan."}}),
    )
}

pub(crate) fn relation_list(args: &Map<String, Value>, bound_root: &Path) -> Result<Value, Value> {
    let root = requested_root(args, bound_root)?;
    let registry_path = root.join(".ai/site-relation-registry.json");
    let relations = read_relations(&registry_path)?;
    let limit = integer(args, "limit", 20, 1, 500)? as usize;
    let filtered = relations
        .into_iter()
        .filter(|relation| {
            matches_filter(relation, args, "relation_kind", "kind")
                && matches_filter(relation, args, "source_site_ref", "source_site")
                && matches_filter(relation, args, "target_site_ref", "target_site")
                && matches_filter(relation, args, "status", "status")
        })
        .take(limit)
        .collect::<Vec<_>>();
    Ok(
        json!({"status":"success","mutation_performed":false,"registry_path":registry_path.to_string_lossy(),"count":filtered.len(),"limit":limit,"relations":filtered}),
    )
}

pub(crate) fn relation_validate(
    args: &Map<String, Value>,
    bound_root: &Path,
) -> Result<Value, Value> {
    let root = requested_root(args, bound_root)?;
    let registry_path = root.join(".ai/site-relation-registry.json");
    let relations = read_relations(&registry_path)?;
    let active = relations
        .iter()
        .filter(|relation| relation["status"] == "active")
        .collect::<Vec<_>>();
    let mut issues = Vec::new();
    for relation in &active {
        let id = relation["relation_id"].as_str().unwrap_or_default();
        for (field, code, message) in [
            (
                "source_site_ref",
                "missing_source_site",
                "Relation source_site_ref is required.",
            ),
            (
                "target_site_ref",
                "missing_target_site",
                "Relation target_site_ref is required.",
            ),
            (
                "authority_effect",
                "missing_authority_effect",
                "Relation authority_effect is required.",
            ),
        ] {
            if relation[field]
                .as_str()
                .map(str::trim)
                .unwrap_or_default()
                .is_empty()
            {
                issues.push(
                    json!({"relation_id":id,"severity":"error","code":code,"message":message}),
                );
            }
        }
        if let Some(reciprocal) = relation["reciprocal_relation_id"].as_str() {
            if !reciprocal.is_empty()
                && !active
                    .iter()
                    .any(|candidate| candidate["relation_id"] == reciprocal)
            {
                issues.push(json!({"relation_id":id,"severity":"error","code":"missing_named_reciprocal","message":format!("Reciprocal relation is not active: {reciprocal}")}));
            }
        }
        if relation["reciprocal_required"] == true && !has_reciprocal(relation, &active) {
            issues.push(json!({"relation_id":id,"severity":"error","code":"missing_required_reciprocal","message":format!("Missing active reciprocal relation {} -> {}.",relation["target_site_ref"].as_str().unwrap_or_default(),relation["source_site_ref"].as_str().unwrap_or_default())}));
        }
    }
    let valid = issues.is_empty();
    Ok(
        json!({"status":if valid{"success"}else{"error"},"mutation_performed":false,"registry_path":registry_path.to_string_lossy(),"relation_count":relations.len(),"valid":valid,"issues":issues}),
    )
}

pub(crate) fn authority_preflight(
    args: &Map<String, Value>,
    bound_root: &Path,
) -> Result<Value, Value> {
    let root = requested_root(args, bound_root)?;
    let family = text(args, "mutation_family").unwrap_or_else(|| "task_lifecycle".to_string());
    let supported =
        ["task_lifecycle", "inbox", "publication", "secret", "site"].contains(&family.as_str());
    let files = json!({"task_lifecycle_db":root.join(".ai/task-lifecycle.db").is_file(),"task_snapshot":root.join(".ai/task-lifecycle-snapshot.json").is_file(),"tasks_dir":root.join(".ai/do-not-open/tasks").is_dir(),"inbox_db":root.join(".ai/inbox.db").is_file(),"inbox_exports":root.join(".ai/inbox-envelopes").is_dir(),"publication_dir":root.join(".ai/repo-publications").is_dir(),"site_config":root.join("config.json").is_file()||root.join(".narada-site.json").is_file(),"read_only_marker":root.join(".ai/read-only-embodiment.json").is_file()});
    let read_only = files["read_only_marker"] == true;
    let has_authority = [
        "task_lifecycle_db",
        "task_snapshot",
        "tasks_dir",
        "inbox_db",
        "inbox_exports",
        "publication_dir",
        "site_config",
    ]
    .iter()
    .any(|key| files[*key] == true);
    let repo = git_posture(&root);
    let behind = repo
        .as_ref()
        .and_then(|v| v["behind"].as_i64())
        .unwrap_or(0)
        > 0;
    let (locus, safety, next, reason) = if !supported {
        (
            "unsupported",
            "inspect_only",
            "Use a supported mutation_family.",
            format!("Unsupported mutation family: {family}."),
        )
    } else if read_only {
        ("read_only_embodiment","refuse","Run this mutation at the declared authority locus, or submit an inbox observation from this embodiment.","This checkout declares itself as a read-only embodiment.".to_string())
    } else if behind {
        ("stale_clone","inspect_only","git pull --ff-only && narada mutation-evidence reconcile --apply","The local branch is behind its upstream; mutation would risk writing against stale authority.".to_string())
    } else if has_authority {
        (
            "authority_locus",
            "allowed_with_command",
            recommended(&family),
            "Authority-bearing Narada state surfaces are present at this locus.".to_string(),
        )
    } else {
        (
            "unknown",
            "refuse",
            "Run authority preflight at the authority Site.",
            "No authority-bearing Narada state surface was found.".to_string(),
        )
    };
    Ok(
        json!({"status":"success","cwd":root.to_string_lossy(),"mutation_family":family,"locus_state":locus,"mutation_safety":safety,"next_safe_command":next,"reason":reason,"repo":repo,"authority_files":files,"embodiments":[],"embodiment_warnings":[],"integration_hooks":integration_hooks()}),
    )
}

pub(crate) fn dependency_posture(bound_root: &Path) -> Result<Value, Value> {
    let executable =
        std::env::current_exe().map_err(io_error("native_executable_resolution_failed"))?;
    let packages = [
        "agent-cli",
        "mcp-transport",
        "task-governance-core",
        "task-lifecycle-mcp",
    ];
    let legacy_links = packages.iter().filter_map(|name| {
        let path = bound_root.join("node_modules/@narada-core").join(name);
        path.symlink_metadata().ok().map(|metadata| json!({"package_name":format!("@narada-core/{name}"),"path":path.to_string_lossy(),"is_symlink":metadata.file_type().is_symlink(),"is_directory":metadata.is_dir()}))
    }).collect::<Vec<_>>();
    Ok(
        json!({"schema":"narada.site_native_dependency_posture.v1","status":if legacy_links.is_empty(){"native_self_contained"}else{"legacy_links_present"},"implementation":"rust-native","runtime_dependencies":[],"node_required":false,"bun_required":false,"typescript_required":false,"native_executable":executable.to_string_lossy(),"site_root":bound_root.to_string_lossy(),"legacy_package_links":legacy_links,"legacy_package_link_count":legacy_links.len(),"mutation_performed":false,"next_action":if legacy_links.is_empty(){Value::Null}else{Value::String("Review and remove legacy node_modules links through an explicit filesystem authority after confirming no legacy runtime consumes them.".to_string())}}),
    )
}

pub(crate) fn retired_dependency_sync(bound_root: &Path) -> Value {
    json!({"schema":"narada.site_deps_sync.retired.v1","status":"retired","implementation":"rust-native","mutation_attempted":false,"mutation_performed":false,"site_root":bound_root.to_string_lossy(),"reason":"legacy_node_package_link_synchronization_removed_from_native_runtime","replacement_tool":"site_dependency_posture","node_modules_modified":false,"remediation":"Call site_dependency_posture. Native MCP surfaces are self-contained and do not synchronize JavaScript package links."})
}

pub(crate) fn init_site(args: &Map<String, Value>) -> Result<Value, Value> {
    let site_id = required(args, "site_id")?;
    if site_id.contains(['\\', '/']) {
        return Err(error(
            "site_id_invalid",
            "site_id must be an identifier, not a path",
        ));
    }
    let substrate = required(args, "substrate")?;
    if ![
        "windows-native",
        "windows-wsl",
        "macos",
        "linux-user",
        "linux-system",
    ]
    .contains(&substrate.as_str())
    {
        return Ok(
            json!({"status":"error","error":format!("Unsupported substrate: \"{substrate}\". Valid substrates: windows-native, windows-wsl, macos, linux-user, linux-system"),"remediation":"Choose a supported substrate."}),
        );
    }
    let execute = args.get("execute").and_then(Value::as_bool) == Some(true);
    let dry = args
        .get("dry_run")
        .and_then(Value::as_bool)
        .unwrap_or(!execute);
    if !execute || dry {
        return Ok(init_plan(&site_id, &substrate, args, true)?);
    }
    if args
        .get("authority_basis")
        .and_then(Value::as_object)
        .is_none_or(Map::is_empty)
    {
        return Err(error(
            "authority_basis_required",
            "site_init requires a non-empty authority_basis",
        ));
    }
    let plan = init_plan(&site_id, &substrate, args, false)?;
    let root = PathBuf::from(plan["siteRoot"].as_str().unwrap_or_default());
    let config_path = root.join("config.json");
    if config_path.is_file() {
        let existing =
            read_bounded_json(&config_path, 4 * 1024 * 1024, "site_init_existing_config")?;
        if existing != plan["config"] {
            return Err(
                json!({"code":"site_init_conflict","message":"target Site already has a different config","path":config_path.to_string_lossy()}),
            );
        }
        let repaired = register_site(
            &site_id,
            &substrate,
            &root,
            args.get("operation").and_then(Value::as_str),
        )?;
        let mut replay = plan;
        replay["status"] = Value::String(
            if repaired {
                "repaired_registry"
            } else {
                "reused"
            }
            .to_string(),
        );
        replay["dryRun"] = Value::Bool(false);
        replay["mutation_performed"] = Value::Bool(repaired);
        replay["idempotency_replay"] = Value::Bool(!repaired);
        return Ok(replay);
    }
    ensure_registry_compatible(&site_id, &substrate, &root)?;
    if root.exists()
        && fs::read_dir(&root)
            .map_err(io_error("site_init_target_read_failed"))?
            .take(1)
            .next()
            .is_some()
    {
        return Err(
            json!({"code":"site_init_collision_refused","message":"site_init refuses a non-empty target without an identical config","path":root.to_string_lossy()}),
        );
    }
    for directory in [
        "state",
        "messages",
        "tombstones",
        "views",
        "blobs",
        "tmp",
        "db",
        "logs",
        "traces",
        ".ai",
    ] {
        fs::create_dir_all(root.join(directory))
            .map_err(io_error("site_init_directory_create_failed"))?;
    }
    write_new_json(&config_path, &plan["config"])?;
    write_new_text(
        &root.join("AGENTS.md"),
        &site_agents_contract(&site_id, &substrate, &root),
    )?;
    register_site(
        &site_id,
        &substrate,
        &root,
        args.get("operation").and_then(Value::as_str),
    )?;
    let mut result = plan;
    result["status"] = Value::String("success".to_string());
    result["dryRun"] = Value::Bool(false);
    result["mutation_performed"] = Value::Bool(true);
    result["idempotency_replay"] = Value::Bool(false);
    Ok(result)
}

fn init_plan(
    site_id: &str,
    substrate: &str,
    args: &Map<String, Value>,
    dry: bool,
) -> Result<Value, Value> {
    let authority = text(args, "authority_locus").unwrap_or_else(|| "user".to_string());
    if matches!(substrate, "windows-native" | "windows-wsl")
        && !["user", "pc"].contains(&authority.as_str())
    {
        return Err(error(
            "authority_locus_invalid",
            "Windows authority_locus must be user or pc",
        ));
    }
    let sync = text(args, "sync")
        .or_else(|| (authority == "user").then_some("hybrid_capable_plain_folder".to_string()));
    if sync.as_deref().is_some_and(|value| {
        ![
            "local_only",
            "cloud_synced_folder",
            "git_backed",
            "hybrid",
            "hybrid_capable_plain_folder",
        ]
        .contains(&value)
    }) {
        return Err(error(
            "sync_posture_invalid",
            "unsupported Site sync posture",
        ));
    }
    let root = text(args, "root")
        .map(PathBuf::from)
        .unwrap_or_else(|| default_site_root(site_id, substrate, &authority));
    let execution = text(args, "execution_surface").unwrap_or_else(|| {
        match substrate {
            "windows-native" | "windows-wsl" => "windows_native",
            "linux-user" => "linux_user",
            "linux-system" => "linux_system",
            _ => "macos_native",
        }
        .to_string()
    });
    if ![
        "windows_native",
        "wsl_assisted",
        "wsl_native",
        "linux_user",
        "linux_system",
        "macos_native",
    ]
    .contains(&execution.as_str())
    {
        return Err(error(
            "execution_surface_invalid",
            "unsupported execution_surface",
        ));
    }
    let variant = match substrate {
        "windows-native" => "native",
        "windows-wsl" => "wsl",
        "linux-user" => "linux-user",
        "linux-system" => "linux-system",
        _ => "macos",
    };
    let config = json!({"site_id":site_id,"variant":variant,"substrate":substrate,"site_root":root.to_string_lossy(),"config_path":root.join("config.json").to_string_lossy(),"locus":{"authority_locus":authority},"sync":sync.as_ref().map(|posture|json!({"posture":posture,"git_initialized":false,"cloud_sync":"external_if_configured"})),"execution":{"surface":execution,"inferred":!args.contains_key("execution_surface"),"executor_runtime":if cfg!(windows){"windows"}else if cfg!(target_os="macos"){"macos"}else{"linux"},"target_authority_locus":if substrate.starts_with("windows-"){format!("windows_{authority}")}else{substrate.to_string()},"target_root":root.to_string_lossy(),"permission_posture":if authority=="pc"{"pc_locus_programdata_write_required"}else{"site_locus_write_required"}},"cycle_interval_minutes":5,"lock_ttl_ms":310000,"ceiling_ms":300000});
    Ok(
        json!({"status":if dry{"planned"}else{"initializing"},"siteId":site_id,"substrate":substrate,"siteRoot":root.to_string_lossy(),"configPath":root.join("config.json").to_string_lossy(),"dryRun":dry,"mutation_performed":false,"config":config,"planned_directories":["state","messages","tombstones","views","blobs","tmp","db","logs","traces",".ai"],"planned_files":[root.join("config.json").to_string_lossy(),root.join("AGENTS.md").to_string_lossy()],"nextSteps":[format!("narada doctor --site {site_id}"),format!("narada cycle --site {site_id}"),format!("narada sites enable {site_id}")]}),
    )
}
fn default_site_root(site_id: &str, substrate: &str, authority: &str) -> PathBuf {
    if let Some(root) = std::env::var_os("NARADA_SITE_ROOT") {
        return if authority == "user" && substrate.starts_with("windows-") {
            PathBuf::from(root)
        } else {
            PathBuf::from(root).join(site_id)
        };
    }
    match substrate {
        "windows-native" if authority == "user" => std::env::var_os("NARADA_USER_SITE_ROOT")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("USERPROFILE").map(|v| PathBuf::from(v).join("Narada")))
            .unwrap_or_else(|| PathBuf::from("Narada")),
        "windows-native" => std::env::var_os("NARADA_PC_SITE_ROOT")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                std::env::var_os("ProgramData")
                    .map(PathBuf::from)
                    .unwrap_or_else(|| PathBuf::from("C:/ProgramData"))
                    .join("Narada/sites/pc")
            })
            .join(site_id),
        "windows-wsl" if authority == "user" => home_dir().join(".narada"),
        "windows-wsl" => PathBuf::from("/var/lib/narada/sites/pc").join(site_id),
        "linux-system" => PathBuf::from("/var/lib/narada").join(site_id),
        "linux-user" => std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| home_dir().join(".local/share"))
            .join("narada")
            .join(site_id),
        _ => home_dir()
            .join("Library/Application Support/Narada")
            .join(site_id),
    }
}
fn home_dir() -> PathBuf {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
}
fn path_key(value: &str) -> String {
    value
        .replace('\\', "/")
        .trim_end_matches('/')
        .to_ascii_lowercase()
}
fn site_agents_contract(site_id: &str, substrate: &str, root: &Path) -> String {
    format!("# {site_id} Site Agent Contract\n\nThis is the Site-local execution contract for `{}`.\n\n- Authority is local to `{}`.\n- Architect specifies governed work; Builder executes admitted work; Observer reports without mutation.\n- Runtime presence does not grant Operator or Site authority.\n- Use canonical inbox, lifecycle, evidence, and publication surfaces.\n- Incoming material is inert until admitted by this Site.\n",substrate,root.display())
}
fn write_new_json(path: &Path, value: &Value) -> Result<(), Value> {
    write_new_text(
        path,
        &format!(
            "{}\n",
            serde_json::to_string_pretty(value)
                .map_err(|cause| error("site_init_config_encode_failed", &cause.to_string()))?
        ),
    )
}
fn write_new_text(path: &Path, content: &str) -> Result<(), Value> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(io_error("site_init_parent_create_failed"))?;
    }
    use std::io::Write;
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(io_error("site_init_file_create_failed"))?;
    file.write_all(content.as_bytes())
        .map_err(io_error("site_init_file_write_failed"))?;
    file.sync_all()
        .map_err(io_error("site_init_file_sync_failed"))
}
fn read_bounded_json(path: &Path, max: u64, code: &'static str) -> Result<Value, Value> {
    if fs::metadata(path).map_err(io_error(code))?.len() > max {
        return Err(error(code, "JSON artifact exceeds its size bound"));
    }
    serde_json::from_slice(&fs::read(path).map_err(io_error(code))?)
        .map_err(|cause| error(code, &cause.to_string()))
}
fn registry_connection() -> Result<Connection, Value> {
    let registry_root = std::env::var_os("NARADA_USER_SITE_ROOT")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("USERPROFILE").map(|v| PathBuf::from(v).join("Narada")))
        .unwrap_or_else(|| home_dir().join("Narada"));
    fs::create_dir_all(&registry_root).map_err(io_error("site_registry_root_create_failed"))?;
    let db = Connection::open(registry_root.join("registry.db"))
        .map_err(|cause| error("site_registry_open_failed", &cause.to_string()))?;
    db.execute_batch("CREATE TABLE IF NOT EXISTS site_registry(site_id TEXT PRIMARY KEY,variant TEXT NOT NULL,site_root TEXT NOT NULL,substrate TEXT NOT NULL DEFAULT 'windows',aim_json TEXT,control_endpoint TEXT,last_seen_at TEXT,created_at TEXT NOT NULL DEFAULT (datetime('now')),lifecycle_status TEXT NOT NULL DEFAULT 'active',observation_status TEXT NOT NULL DEFAULT 'unverified',sources_json TEXT NOT NULL DEFAULT '[]',aliases_json TEXT NOT NULL DEFAULT '[]',revision INTEGER NOT NULL DEFAULT 1,updated_at TEXT NOT NULL DEFAULT (datetime('now')),retired_at TEXT,retire_reason TEXT);CREATE TABLE IF NOT EXISTS registry_management_audit(event_id TEXT PRIMARY KEY,site_id TEXT NOT NULL,operation TEXT NOT NULL,actor TEXT NOT NULL,reason TEXT,occurred_at TEXT NOT NULL,before_json TEXT,after_json TEXT,status TEXT NOT NULL);").map_err(|cause|error("site_registry_schema_prepare_failed",&cause.to_string()))?;
    Ok(db)
}
fn ensure_registry_compatible(site_id: &str, substrate: &str, root: &Path) -> Result<(), Value> {
    let db = registry_connection()?;
    let existing = db
        .query_row(
            "SELECT site_root,substrate FROM site_registry WHERE site_id=?1",
            params![site_id],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
        )
        .optional()
        .map_err(|cause| error("site_registry_lookup_failed", &cause.to_string()))?;
    if let Some((existing_root, existing_substrate)) = existing {
        if path_key(&existing_root) != path_key(&root.to_string_lossy())
            || existing_substrate != substrate
        {
            return Err(error(
                "site_registry_conflict",
                "site_id is already registered to a different root or substrate",
            ));
        }
    }
    Ok(())
}
fn register_site(
    site_id: &str,
    substrate: &str,
    root: &Path,
    operation: Option<&str>,
) -> Result<bool, Value> {
    let mut db = registry_connection()?;
    let existing = db
        .query_row(
            "SELECT site_root,substrate FROM site_registry WHERE site_id=?1",
            params![site_id],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
        )
        .optional()
        .map_err(|cause| error("site_registry_lookup_failed", &cause.to_string()))?;
    if let Some((existing_root, existing_substrate)) = existing {
        if path_key(&existing_root) != path_key(&root.to_string_lossy())
            || existing_substrate != substrate
        {
            return Err(error(
                "site_registry_conflict",
                "site_id is already registered to a different root or substrate",
            ));
        }
        return Ok(false);
    }
    let tx = db
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|cause| error("site_registry_transaction_failed", &cause.to_string()))?;
    let now = time::OffsetDateTime::now_utc()
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_default();
    let variant = match substrate {
        "windows-native" => "native",
        "windows-wsl" => "wsl",
        "linux-user" => "linux-user",
        "linux-system" => "linux-system",
        _ => "macos",
    };
    tx.execute("INSERT INTO site_registry(site_id,variant,site_root,substrate,aim_json,created_at,lifecycle_status,observation_status,sources_json,aliases_json,revision,updated_at) VALUES(?1,?2,?3,?4,?5,?6,'active','present',?7,'[]',1,?6)",params![site_id,variant,root.to_string_lossy(),substrate,operation,&now,json!([{"kind":"site_init","ref":root.to_string_lossy(),"observedAt":now}]).to_string()]).map_err(|cause|error("site_registry_insert_failed",&cause.to_string()))?;
    tx.commit()
        .map_err(|cause| error("site_registry_commit_failed", &cause.to_string()))?;
    Ok(true)
}

fn kind_json(entry: &LifecycleKind) -> Value {
    json!({"kind":entry.kind,"purpose":entry.purpose,"source_required":entry.source_required,"target_required":entry.target_required,"authority_modes":entry.authority_modes,"artifacts":entry.artifacts})
}
fn check(name: &str, pass: bool, detail: String, remediation: String) -> Value {
    json!({"check":name,"status":if pass{"pass"}else{"fail"},"detail":detail,"remediation":remediation})
}
fn relation_registry(path: &Path) -> Result<Value, Value> {
    if !path.is_file() {
        return Ok(
            json!({"registry_kind":"site_relation_registry","registry_version":1,"relations":[]}),
        );
    }
    let metadata = fs::metadata(path).map_err(io_error("site_relation_registry_stat_failed"))?;
    if metadata.len() > MAX_RELATION_FILE_BYTES {
        return Err(error(
            "site_relation_registry_too_large",
            "site relation registry exceeds 8 MiB",
        ));
    }
    let parsed: Value = serde_json::from_slice(
        &fs::read(path).map_err(io_error("site_relation_registry_read_failed"))?,
    )
    .map_err(|cause| error("site_relation_registry_invalid", &cause.to_string()))?;
    Ok(parsed)
}
fn read_relations(path: &Path) -> Result<Vec<Value>, Value> {
    let registry = relation_registry(path)?;
    let relations = registry
        .get("relations")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    if relations.len() > MAX_RELATIONS {
        Err(error(
            "site_relation_registry_bound_exceeded",
            "site relation registry exceeds 10000 records",
        ))
    } else {
        Ok(relations)
    }
}
fn matches_filter(relation: &Value, args: &Map<String, Value>, field: &str, arg: &str) -> bool {
    text(args, arg).is_none_or(|expected| relation[field].as_str() == Some(expected.as_str()))
}
fn has_reciprocal(relation: &Value, active: &[&Value]) -> bool {
    if let Some(id) = relation["reciprocal_relation_id"]
        .as_str()
        .filter(|v| !v.is_empty())
    {
        return active
            .iter()
            .any(|candidate| candidate["relation_id"] == id);
    }
    let expected = match relation["relation_kind"].as_str() {
        Some("absorbed") => Some("absorbed_by"),
        Some("absorbed_by") => Some("absorbed"),
        Some("subscribes_to") => Some("publishes_to"),
        Some("publishes_to") => Some("subscribes_to"),
        _ => None,
    };
    active.iter().any(|candidate| {
        candidate["source_site_ref"] == relation["target_site_ref"]
            && candidate["target_site_ref"] == relation["source_site_ref"]
            && expected.is_none_or(|kind| candidate["relation_kind"] == kind)
    })
}
fn requested_root(args: &Map<String, Value>, bound: &Path) -> Result<PathBuf, Value> {
    let requested = text(args, "cwd")
        .map(PathBuf::from)
        .unwrap_or_else(|| bound.to_path_buf());
    let canonical = requested
        .canonicalize()
        .map_err(io_error("site_lifecycle_root_unavailable"))?;
    let bound = bound
        .canonicalize()
        .map_err(io_error("site_lifecycle_bound_root_unavailable"))?;
    if !canonical.starts_with(&bound) {
        return Err(error(
            "site_lifecycle_root_outside_bound_site",
            "cwd must remain inside the bound Site root",
        ));
    }
    Ok(canonical)
}
fn git_posture(root: &Path) -> Option<Value> {
    let top = git(root, &["rev-parse", "--show-toplevel"])?;
    let repo = PathBuf::from(&top);
    let branch = git(&repo, &["branch", "--show-current"]);
    let upstream = git(
        &repo,
        &["rev-parse", "--abbrev-ref", "--symbolic-full-name", "@{u}"],
    );
    let head = git(&repo, &["rev-parse", "HEAD"]);
    let upstream_head = git(&repo, &["rev-parse", "@{u}"]);
    let ahead =
        git(&repo, &["rev-list", "--count", "@{u}..HEAD"]).and_then(|v| v.parse::<i64>().ok());
    let behind =
        git(&repo, &["rev-list", "--count", "HEAD..@{u}"]).and_then(|v| v.parse::<i64>().ok());
    let dirty = git(&repo, &["status", "--porcelain"])
        .map(|v| v.lines().take(10001).count() as i64)
        .unwrap_or(0);
    Some(
        json!({"root":top,"branch":branch,"upstream":upstream,"head":head,"upstream_head":upstream_head,"ahead":ahead,"behind":behind,"dirty_count":dirty}),
    )
}
fn git(root: &Path, args: &[&str]) -> Option<String> {
    let mut command = Command::new("git");
    command.args(args).current_dir(root);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000);
    }
    let output = command.output().ok()?;
    if !output.status.success() || output.stdout.len() > 1024 * 1024 {
        return None;
    }
    let value = String::from_utf8_lossy(&output.stdout).trim().to_string();
    (!value.is_empty()).then_some(value)
}
fn recommended(family: &str) -> &'static str {
    match family{"task_lifecycle"=>"narada work-next --agent <agent> --claim","inbox"=>"narada inbox work-next --claim --by <principal>","publication"=>"narada publication prepare --by <principal> --message <message>","secret"=>"narada sites authority preflight --mutation-family secret && <sanctioned secret command>",_=>"narada sites lifecycle preflight <kind> --source-site <ref> --target-site <ref>"}
}
fn integration_hooks() -> Value {
    json!({"task_lifecycle":["task-lifecycle"],"inbox":["site-inbox"],"publication":["git"],"secret":[],"site":["site-lifecycle","site-registry"]})
}
fn text(args: &Map<String, Value>, key: &str) -> Option<String> {
    args.get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(ToString::to_string)
}
fn required(args: &Map<String, Value>, key: &str) -> Result<String, Value> {
    text(args, key).ok_or_else(|| {
        error(
            "required_argument_missing",
            &format!("required_argument_missing:{key}"),
        )
    })
}
fn integer(
    args: &Map<String, Value>,
    key: &str,
    default: i64,
    min: i64,
    max: i64,
) -> Result<i64, Value> {
    let value = args
        .get(key)
        .map(Value::as_i64)
        .unwrap_or(Some(default))
        .ok_or_else(|| error("argument_invalid", &format!("{key} must be an integer")))?;
    if (min..=max).contains(&value) {
        Ok(value)
    } else {
        Err(error(
            "argument_out_of_bounds",
            &format!("{key} must be between {min} and {max}"),
        ))
    }
}
fn error(code: &str, message: &str) -> Value {
    json!({"code":code,"message":message})
}
fn io_error(code: &'static str) -> impl FnOnce(std::io::Error) -> Value {
    move |cause| error(code, &cause.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn lifecycle_catalog_and_preflight_are_coherent() {
        assert_eq!(create_presets()["presets"].as_array().unwrap().len(), 5);
        assert_eq!(kinds()["kinds"].as_array().unwrap().len(), 7);
        let ready=preflight(&serde_json::from_value(json!({"kind":"archive","source_site":"a","authority_mode":"retired_non_authority"})).unwrap()).unwrap();
        assert_eq!(ready["status"], "ready");
        let blocked = preflight(&serde_json::from_value(json!({"kind":"clone"})).unwrap()).unwrap();
        assert_eq!(blocked["status"], "blocked");
    }
}
