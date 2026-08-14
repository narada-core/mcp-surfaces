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
        assert_eq!(kinds()["kinds"].as_array().unwrap().len(), 7);
        let ready=preflight(&serde_json::from_value(json!({"kind":"archive","source_site":"a","authority_mode":"retired_non_authority"})).unwrap()).unwrap();
        assert_eq!(ready["status"], "ready");
        let blocked = preflight(&serde_json::from_value(json!({"kind":"clone"})).unwrap()).unwrap();
        assert_eq!(blocked["status"], "blocked");
    }
}
