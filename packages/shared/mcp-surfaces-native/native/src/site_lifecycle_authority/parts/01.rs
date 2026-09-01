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
