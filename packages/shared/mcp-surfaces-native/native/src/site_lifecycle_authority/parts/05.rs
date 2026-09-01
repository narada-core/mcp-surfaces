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
