use rusqlite::{params, Connection, OpenFlags, TransactionBehavior};
use serde_json::{json, Map, Value};
use std::collections::{BTreeMap, HashSet};
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;

const MAX_REGISTRY_ROWS: i64 = 10_000;
const MAX_DISCOVERY_ENTRIES: usize = 1_000;

pub(crate) fn call(name: &str, args: &Map<String, Value>) -> Result<Value, Value> {
    match name {
        "site_registry_list" => list(args),
        "site_registry_show" => show(args),
        "site_registry_discover_plan" => discover(args),
        _ => Err(error("unknown_tool", &format!("unknown_tool:{name}"))),
    }
}

pub(crate) fn apply_discovery(args: &Map<String, Value>) -> Result<Value, Value> {
    let plan = discover(args)?;
    let entries = plan["entries"].as_array().cloned().unwrap_or_default();
    if entries.len() > MAX_DISCOVERY_ENTRIES {
        return Err(error(
            "site_registry_discovery_bound_exceeded",
            "discovery plan exceeds 1000 entries",
        ));
    }
    let path = registry_path();
    let mut db = Connection::open_with_flags(&path, OpenFlags::SQLITE_OPEN_READ_WRITE)
        .map_err(db_error("site_registry_open_failed"))?;
    verify_schema(&db)?;
    let tx = db
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(db_error("site_registry_transaction_failed"))?;
    let now = OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .unwrap_or_default();
    let actor = args
        .get("actor")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("operator");
    let mut applied = Vec::new();
    let mut audit_refs = Vec::new();
    for entry in &entries {
        if entry["status"] != "planned" {
            continue;
        }
        let operation = entry["operation"].as_str().unwrap_or_default();
        let site_id = entry["site_id"].as_str().unwrap_or_default();
        let after = &entry["after"];
        let changed = if operation == "add" {
            tx.execute("INSERT INTO site_registry(site_id,variant,site_root,substrate,aim_json,control_endpoint,last_seen_at,created_at,lifecycle_status,observation_status,sources_json,aliases_json,revision,updated_at,retired_at,retire_reason) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,1,?13,?14,?15) ON CONFLICT(site_id) DO NOTHING",params![site_id,after["variant"].as_str().unwrap_or("native"),after["site_root"].as_str().unwrap_or_default(),after["substrate"].as_str().unwrap_or("windows"),after["aim_json"].as_str(),after["control_endpoint"].as_str(),after["last_seen_at"].as_str(),after["created_at"].as_str().unwrap_or(&now),after["lifecycle_status"].as_str().unwrap_or("active"),after["observation_status"].as_str().unwrap_or("present"),storage_sources(&after["sources"]),after["aliases"].to_string(),&now,after["retired_at"].as_str(),after["retire_reason"].as_str()]).map_err(db_error("site_registry_discovery_insert_failed"))?
        } else {
            let expected = entry["before"]["revision"].as_i64().unwrap_or(0);
            tx.execute("UPDATE site_registry SET sources_json=?1,aliases_json=?2,last_seen_at=?3,observation_status=?4,revision=revision+1,updated_at=?5 WHERE site_id=?6 AND revision=?7",params![storage_sources(&after["sources"]),after["aliases"].to_string(),after["last_seen_at"].as_str(),after["observation_status"].as_str().unwrap_or("present"),&now,site_id,expected]).map_err(db_error("site_registry_discovery_update_failed"))?
        };
        if changed != 1 {
            return Err(
                json!({"code":"site_registry_discovery_revision_conflict","message":"registry changed after discovery planning; no discovery changes were committed","site_id":site_id,"operation":operation}),
            );
        }
        let audit_id = format!("registry-management-{}", uuid::Uuid::new_v4());
        tx.execute("INSERT INTO registry_management_audit(event_id,site_id,operation,actor,reason,occurred_at,before_json,after_json,status) VALUES(?1,?2,?3,?4,'native_site_discovery',?5,?6,?7,'applied')",params![audit_id,site_id,operation,actor,&now,if entry["before"].is_null(){None}else{Some(entry["before"].to_string())},after.to_string()]).map_err(db_error("site_registry_discovery_audit_failed"))?;
        audit_refs.push(audit_id);
        applied.push(site_id.to_string());
    }
    tx.commit()
        .map_err(db_error("site_registry_transaction_commit_failed"))?;
    let mut result = plan;
    result["status"] = Value::String(
        if applied.is_empty() {
            "unchanged"
        } else {
            "applied"
        }
        .to_string(),
    );
    result["mutation_performed"] = Value::Bool(!applied.is_empty());
    result["dry_run"] = Value::Bool(false);
    result["applied"] = json!(applied);
    result["audit_refs"] = json!(audit_refs);
    result["audit_ref"] = if audit_refs.len() == 1 {
        Value::String(audit_refs[0].clone())
    } else {
        Value::Null
    };
    Ok(result)
}

fn storage_sources(value: &Value) -> String {
    Value::Array(value.as_array().map(|items|items.iter().map(|source|json!({"kind":source["kind"],"ref":source["ref"],"observedAt":source.get("observed_at").or_else(||source.get("observedAt")).cloned().unwrap_or(Value::Null)})).collect()).unwrap_or_default()).to_string()
}

pub(crate) fn doctor() -> Value {
    let path = registry_path();
    let (status, schema_ready, record_count, diagnostic) = match open_registry(&path) {
        Ok(db) => match verify_schema(&db) {
            Ok(()) => match db.query_row("SELECT COUNT(*) FROM site_registry", [], |row| {
                row.get::<_, i64>(0)
            }) {
                Ok(count) => ("ok", true, Some(count), Value::Null),
                Err(cause) => (
                    "attention",
                    true,
                    None,
                    json!({"code":"site_registry_count_failed","message":cause.to_string()}),
                ),
            },
            Err(problem) => ("attention", false, None, problem),
        },
        Err(problem) => ("attention", false, None, problem),
    };
    json!({
        "schema":"narada.site_registry.doctor.v1","status":status,"server_name":"site-registry-mcp",
        "implementation":"rust-native","runtime_dependency":"none","registry_path":path.to_string_lossy(),
        "registry_exists":path.is_file(),"schema_ready":schema_ready,"record_count":record_count,
        "catalog_source":"user_site_site_registry","read_only":true,"diagnostic":diagnostic
    })
}

fn list(args: &Map<String, Value>) -> Result<Value, Value> {
    list_at(args, &registry_path())
}
fn list_at(args: &Map<String, Value>, path: &Path) -> Result<Value, Value> {
    let limit = integer(args, "limit", 100, 1, 500)?;
    let offset = integer(args, "offset", 0, 0, MAX_REGISTRY_ROWS)?;
    let db = ready_registry(path)?;
    let total = db
        .query_row("SELECT COUNT(*) FROM site_registry", [], |row| {
            row.get::<_, i64>(0)
        })
        .map_err(db_error("site_registry_count_failed"))?;
    if total > MAX_REGISTRY_ROWS {
        return Err(error(
            "site_registry_row_bound_exceeded",
            "site registry exceeds the 10000-record safety bound",
        ));
    }
    let mut statement = db.prepare(
        "SELECT site_id,variant,site_root,substrate,aim_json,control_endpoint,last_seen_at,created_at,lifecycle_status,observation_status,sources_json,aliases_json,revision,updated_at,retired_at,retire_reason FROM site_registry ORDER BY created_at ASC,site_id ASC LIMIT ?1 OFFSET ?2"
    ).map_err(db_error("site_registry_query_prepare_failed"))?;
    let rows = statement
        .query_map(params![limit + 1, offset], site_from_row)
        .map_err(db_error("site_registry_query_failed"))?;
    let mut sites = rows
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(db_error("site_registry_row_failed"))?;
    let has_more = sites.len() > limit as usize;
    sites.truncate(limit as usize);
    let returned = sites.len();
    Ok(json!({
        "schema":"narada.site_registry.management.v0","status":"success","operation":"list",
        "mutation_performed":false,"registry_path":path.to_string_lossy(),"catalog_source":"user_site_site_registry",
        "count":total,"offset":offset,"limit":limit,"returned":returned,"has_more":has_more,
        "next_offset":if has_more {Value::from(offset + returned as i64)} else {Value::Null},"sites":sites
    }))
}

fn show(args: &Map<String, Value>) -> Result<Value, Value> {
    show_at(args, &registry_path())
}
fn show_at(args: &Map<String, Value>, path: &Path) -> Result<Value, Value> {
    let reference = required_text(args, "reference")?;
    let db = ready_registry(path)?;
    let total = db
        .query_row("SELECT COUNT(*) FROM site_registry", [], |row| {
            row.get::<_, i64>(0)
        })
        .map_err(db_error("site_registry_count_failed"))?;
    if total > MAX_REGISTRY_ROWS {
        return Err(error(
            "site_registry_row_bound_exceeded",
            "site registry exceeds the 10000-record safety bound",
        ));
    }
    let mut statement = db.prepare("SELECT site_id,variant,site_root,substrate,aim_json,control_endpoint,last_seen_at,created_at,lifecycle_status,observation_status,sources_json,aliases_json,revision,updated_at,retired_at,retire_reason FROM site_registry ORDER BY created_at ASC,site_id ASC LIMIT 10001").map_err(db_error("site_registry_query_prepare_failed"))?;
    let rows = statement
        .query_map([], site_from_row)
        .map_err(db_error("site_registry_query_failed"))?;
    let sites = rows
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(db_error("site_registry_row_failed"))?;
    let normalized = reference.to_ascii_lowercase();
    let site = sites.into_iter().find(|site| {
        site["site_id"]
            .as_str()
            .is_some_and(|id| id.eq_ignore_ascii_case(&reference))
            || site["aliases"].as_array().is_some_and(|aliases| {
                aliases.iter().any(|alias| {
                    alias["value"]
                        .as_str()
                        .is_some_and(|value| value.to_ascii_lowercase() == normalized)
                })
            })
    });
    let Some(site) = site else {
        return Ok(
            json!({"schema":"narada.site_registry.management.v0","status":"refused","operation":"show","mutation_performed":false,"site_id":reference,"registry_path":path.to_string_lossy(),"catalog_source":"user_site_site_registry","before":null,"after":null,"changes":[],"conflicts":[],"refusals":["site_not_found"],"audit_ref":null}),
        );
    };
    let site_id = site["site_id"].as_str().unwrap_or_default();
    let mut audit_statement = db.prepare("SELECT event_id,site_id,operation,actor,reason,occurred_at,before_json,after_json,status FROM registry_management_audit WHERE site_id=?1 ORDER BY occurred_at DESC LIMIT 20").map_err(db_error("site_registry_audit_prepare_failed"))?;
    let audit = audit_statement.query_map(params![site_id], |row| Ok(json!({"event_id":row.get::<_,String>(0)?,"site_id":row.get::<_,String>(1)?,"operation":row.get::<_,String>(2)?,"actor":row.get::<_,String>(3)?,"reason":row.get::<_,Option<String>>(4)?,"occurred_at":row.get::<_,String>(5)?,"before_json":row.get::<_,Option<String>>(6)?,"after_json":row.get::<_,Option<String>>(7)?,"status":row.get::<_,String>(8)?}))).map_err(db_error("site_registry_audit_query_failed"))?.collect::<rusqlite::Result<Vec<_>>>().map_err(db_error("site_registry_audit_row_failed"))?;
    let next = if site["lifecycle_status"] == "retired" {
        json!(["restore", "purge"])
    } else {
        json!(["edit", "retire"])
    };
    Ok(
        json!({"schema":"narada.site_registry.management.v0","status":"success","operation":"show","mutation_performed":false,"site_id":site_id,"registry_path":path.to_string_lossy(),"catalog_source":"user_site_site_registry","site":site,"management_audit":audit,"next_actions":next}),
    )
}

fn discover(args: &Map<String, Value>) -> Result<Value, Value> {
    let source = args.get("source").and_then(Value::as_str).unwrap_or("all");
    if !matches!(source, "filesystem" | "launch_registry" | "all") {
        return Err(error(
            "site_registry_source_invalid",
            "source must be filesystem, launch_registry, or all",
        ));
    }
    let actor = args
        .get("actor")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .unwrap_or("operator");
    let root_filter = args
        .get("root")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|v| !v.is_empty());
    let path = registry_path();
    let db = ready_registry(&path)?;
    let existing = load_all_sites(&db)?;
    let mut candidates = Vec::new();
    let mut ignored = 0usize;
    if matches!(source, "filesystem" | "all") {
        let (found, skipped) = filesystem_candidates(root_filter)?;
        candidates.extend(found);
        ignored += skipped;
    }
    if matches!(source, "launch_registry" | "all") {
        let (found, skipped) = launch_candidates(root_filter)?;
        candidates.extend(found);
        ignored += skipped;
    }
    let mut by_root: BTreeMap<String, Value> = BTreeMap::new();
    for candidate in candidates {
        let key = path_key(candidate["site_root"].as_str().unwrap_or_default());
        by_root
            .entry(key)
            .and_modify(|current| merge_candidate(current, &candidate))
            .or_insert(candidate);
    }
    let mut entries = Vec::new();
    let mut added = Vec::new();
    let mut updated = Vec::new();
    let mut unchanged = Vec::new();
    let mut retired = 0usize;
    for candidate in by_root.into_values() {
        let root = candidate["site_root"].as_str().unwrap_or_default();
        let candidate_id = candidate["site_id"].as_str().unwrap_or_default();
        let matched = existing.iter().find(|site| {
            path_key(site["site_root"].as_str().unwrap_or_default()) == path_key(root)
                || site["site_id"]
                    .as_str()
                    .is_some_and(|id| id.eq_ignore_ascii_case(candidate_id))
        });
        let (status, operation, changes, refusals, after) = match matched {
            None => {
                added.push(candidate_id.to_string());
                (
                    "planned",
                    "add",
                    json!(["site_record"]),
                    json!([]),
                    candidate.clone(),
                )
            }
            Some(site) if site["lifecycle_status"] == "retired" => {
                retired += 1;
                (
                    "advisory",
                    "add",
                    json!([]),
                    json!(["retired_record_requires_restore_or_re_admit"]),
                    site.clone(),
                )
            }
            Some(site) => {
                let missing_alias = site["site_id"]
                    .as_str()
                    .is_some_and(|id| !id.eq_ignore_ascii_case(candidate_id))
                    && !candidate_id.is_empty();
                let missing_source = candidate["sources"]
                    .as_array()
                    .unwrap_or(&Vec::new())
                    .iter()
                    .any(|incoming| {
                        !site["sources"]
                            .as_array()
                            .unwrap_or(&Vec::new())
                            .iter()
                            .any(|stored| {
                                stored["kind"] == incoming["kind"]
                                    && stored["ref"] == incoming["ref"]
                            })
                    });
                if missing_alias || missing_source {
                    updated.push(site["site_id"].as_str().unwrap_or_default().to_string());
                    let mut merged = site.clone();
                    merge_candidate(&mut merged, &candidate);
                    (
                        "planned",
                        "edit",
                        json!(["discovery_evidence"]),
                        json!([]),
                        merged,
                    )
                } else {
                    unchanged.push(site["site_id"].as_str().unwrap_or_default().to_string());
                    ("unchanged", "add", json!([]), json!([]), site.clone())
                }
            }
        };
        entries.push(json!({"schema":"narada.site_registry.management.v0","status":status,"operation":operation,"mutation_performed":false,"site_id":matched.and_then(|v|v["site_id"].as_str()).unwrap_or(candidate_id),"registry_path":path.to_string_lossy(),"catalog_source":"user_site_site_registry","before":matched,"after":after,"changes":changes,"conflicts":[],"refusals":refusals,"audit_ref":null,"actor":actor}));
    }
    let status = if retired > 0 { "advisory" } else { "planned" };
    Ok(
        json!({"schema":"narada.site_registry.management.v0","status":status,"operation":"discover","mutation_performed":false,"dry_run":true,"registry_path":path.to_string_lossy(),"catalog_source":"user_site_site_registry","source":source,"root_filter":root_filter,"counts":{"added":added.len(),"updated":updated.len(),"unchanged":unchanged.len(),"ignored":ignored,"retired_source_present":retired,"conflicted":0},"added":added,"updated":updated,"unchanged":unchanged,"entries":entries,"conflicts":[],"refusals":[],"audit_ref":null,"audit_refs":[]}),
    )
}

fn load_all_sites(db: &Connection) -> Result<Vec<Value>, Value> {
    let count = db
        .query_row("SELECT COUNT(*) FROM site_registry", [], |row| {
            row.get::<_, i64>(0)
        })
        .map_err(db_error("site_registry_count_failed"))?;
    if count > MAX_REGISTRY_ROWS {
        return Err(error(
            "site_registry_row_bound_exceeded",
            "site registry exceeds the 10000-record safety bound",
        ));
    }
    let mut stmt = db.prepare("SELECT site_id,variant,site_root,substrate,aim_json,control_endpoint,last_seen_at,created_at,lifecycle_status,observation_status,sources_json,aliases_json,revision,updated_at,retired_at,retire_reason FROM site_registry ORDER BY created_at ASC,site_id ASC LIMIT 10001").map_err(db_error("site_registry_query_prepare_failed"))?;
    let result = stmt
        .query_map([], site_from_row)
        .map_err(db_error("site_registry_query_failed"))?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(db_error("site_registry_row_failed"));
    result
}

fn filesystem_candidates(root_filter: Option<&str>) -> Result<(Vec<Value>, usize), Value> {
    let base = env::var_os("NARADA_NATIVE_SITES_BASE_DIR")
        .map(PathBuf::from)
        .or_else(|| env::var_os("LOCALAPPDATA").map(|v| PathBuf::from(v).join("Narada")))
        .or_else(|| {
            env::var_os("USERPROFILE").map(|v| PathBuf::from(v).join("AppData/Local/Narada"))
        });
    let Some(base) = base else {
        return Ok((Vec::new(), 0));
    };
    if !base.is_dir() {
        return Ok((Vec::new(), 0));
    }
    let mut entries = fs::read_dir(&base)
        .map_err(io_error("site_registry_filesystem_scan_failed"))?
        .take(MAX_DISCOVERY_ENTRIES + 1)
        .collect::<Result<Vec<_>, _>>()
        .map_err(io_error("site_registry_filesystem_scan_failed"))?;
    if entries.len() > MAX_DISCOVERY_ENTRIES {
        return Err(error(
            "site_registry_discovery_bound_exceeded",
            "filesystem discovery exceeds 1000 entries",
        ));
    }
    entries.sort_by_key(|entry| entry.file_name());
    let mut found = Vec::new();
    let mut ignored = 0;
    for entry in entries {
        let root = entry.path();
        if !root.is_dir()
            || entry.file_name().to_string_lossy().starts_with('.')
            || entry.file_name() == "node_modules"
        {
            ignored += 1;
            continue;
        }
        let config = root.join("config.json");
        if !config.is_file() {
            ignored += 1;
            continue;
        }
        if root_filter.is_some_and(|filter| path_key(filter) != path_key(&root.to_string_lossy())) {
            continue;
        }
        let (aim, substrate) = read_candidate_config(&config);
        found.push(candidate(
            entry.file_name().to_string_lossy().as_ref(),
            &root,
            "native",
            &substrate,
            aim,
            "filesystem",
        ));
    }
    Ok((found, ignored))
}

fn launch_candidates(root_filter: Option<&str>) -> Result<(Vec<Value>, usize), Value> {
    let launch = user_site_root().join("config/launch/agents.psd1");
    if !launch.is_file() {
        return Ok((Vec::new(), 0));
    }
    let content = fs::read_to_string(&launch)
        .map_err(io_error("site_registry_launch_registry_read_failed"))?;
    if content.len() > 4 * 1024 * 1024 {
        return Err(error(
            "site_registry_launch_registry_too_large",
            "launch registry exceeds 4 MiB",
        ));
    }
    let mut records = Vec::<Map<String, Value>>::new();
    let mut current: Option<Map<String, Value>> = None;
    let mut ignored = 0;
    for line in content.lines().take(50_001) {
        let trimmed = line.trim();
        if trimmed == "@{" {
            current = Some(Map::new());
            continue;
        }
        if trimmed == "}" {
            if let Some(record) = current.take() {
                records.push(record);
            }
            continue;
        }
        if let Some(record) = current.as_mut() {
            if let Some((key, value)) = parse_ps_assignment(trimmed) {
                record.insert(key.to_string(), Value::String(value));
            }
        }
    }
    if content.lines().count() > 50_000 {
        return Err(error(
            "site_registry_launch_registry_bound_exceeded",
            "launch registry exceeds 50000 lines",
        ));
    }
    let mut found = Vec::new();
    for record in records.into_iter().take(MAX_DISCOVERY_ENTRIES + 1) {
        let Some(root) = record.get("SiteRoot").and_then(Value::as_str) else {
            ignored += 1;
            continue;
        };
        if root_filter.is_some_and(|filter| path_key(filter) != path_key(root)) {
            continue;
        }
        let id = record
            .get("Site")
            .and_then(Value::as_str)
            .map(ToString::to_string)
            .or_else(|| site_id_from_root(Path::new(root)))
            .or_else(|| {
                Path::new(root)
                    .file_name()
                    .map(|v| v.to_string_lossy().to_ascii_lowercase())
            });
        let Some(id) = id.filter(|v| !v.is_empty() && v != ".narada") else {
            ignored += 1;
            continue;
        };
        found.push(candidate(
            &id,
            Path::new(root),
            "native",
            "windows-launch-registry",
            None,
            "launch_registry",
        ));
    }
    if found.len() > MAX_DISCOVERY_ENTRIES {
        return Err(error(
            "site_registry_discovery_bound_exceeded",
            "launch registry discovery exceeds 1000 entries",
        ));
    }
    Ok((found, ignored))
}

fn candidate(
    id: &str,
    root: &Path,
    variant: &str,
    substrate: &str,
    aim: Option<Value>,
    source: &str,
) -> Value {
    let observed = OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .unwrap_or_default();
    json!({"site_id":id,"site_root":root.to_string_lossy(),"variant":variant,"substrate":substrate,"aim_json":aim,"control_endpoint":null,"last_seen_at":observed,"created_at":observed,"updated_at":observed,"lifecycle_status":"active","observation_status":"present","sources":[{"kind":source,"ref":root.to_string_lossy(),"observed_at":observed}],"aliases":[],"revision":1,"retired_at":null,"retire_reason":null})
}
fn merge_candidate(target: &mut Value, incoming: &Value) {
    if let (Some(existing), Some(additions)) = (
        target["sources"].as_array_mut(),
        incoming["sources"].as_array(),
    ) {
        for source in additions {
            if !existing
                .iter()
                .any(|v| v["kind"] == source["kind"] && v["ref"] == source["ref"])
            {
                existing.push(source.clone());
            }
        }
        if target["site_id"] != incoming["site_id"] {
            if let Some(id) = incoming["site_id"].as_str() {
                target["aliases"]
                    .as_array_mut()
                    .unwrap()
                    .push(json!({"value":id,"source":"discovery"}));
            }
        }
    }
}

fn site_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Value> {
    let aim: Option<String> = row.get(4)?;
    let sources: String = row.get(10)?;
    let aliases: String = row.get(11)?;
    Ok(
        json!({"site_id":row.get::<_,String>(0)?,"variant":row.get::<_,String>(1)?,"site_root":row.get::<_,String>(2)?,"substrate":row.get::<_,String>(3)?,"aim_json":aim,"control_endpoint":row.get::<_,Option<String>>(5)?,"last_seen_at":row.get::<_,Option<String>>(6)?,"created_at":row.get::<_,String>(7)?,"lifecycle_status":row.get::<_,String>(8)?,"observation_status":row.get::<_,String>(9)?,"sources":public_sources(&sources),"aliases":serde_json::from_str::<Value>(&aliases).unwrap_or(json!([])),"revision":row.get::<_,i64>(12)?,"updated_at":row.get::<_,String>(13)?,"retired_at":row.get::<_,Option<String>>(14)?,"retire_reason":row.get::<_,Option<String>>(15)?}),
    )
}
fn ready_registry(path: &Path) -> Result<Connection, Value> {
    let db = open_registry(path)?;
    verify_schema(&db)?;
    Ok(db)
}
fn public_sources(raw: &str) -> Value {
    let parsed = serde_json::from_str::<Value>(raw).unwrap_or(json!([]));
    Value::Array(parsed.as_array().map(|items| items.iter().map(|source| json!({"kind":source.get("kind").and_then(Value::as_str).unwrap_or_default(),"ref":source.get("ref").and_then(Value::as_str).unwrap_or_default(),"observed_at":source.get("observed_at").or_else(||source.get("observedAt")).and_then(Value::as_str).unwrap_or_default()})).collect()).unwrap_or_default())
}
fn open_registry(path: &Path) -> Result<Connection, Value> {
    if !path.is_file() {
        return Err(error(
            "site_registry_missing",
            &format!("site registry does not exist: {}", path.display()),
        ));
    }
    Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .map_err(db_error("site_registry_open_failed"))
}
fn verify_schema(db: &Connection) -> Result<(), Value> {
    let mut stmt = db
        .prepare("PRAGMA table_info(site_registry)")
        .map_err(db_error("site_registry_schema_probe_failed"))?;
    let columns = stmt
        .query_map([], |row| row.get::<_, String>(1))
        .map_err(db_error("site_registry_schema_probe_failed"))?
        .collect::<rusqlite::Result<HashSet<_>>>()
        .map_err(db_error("site_registry_schema_probe_failed"))?;
    let required = [
        "site_id",
        "variant",
        "site_root",
        "substrate",
        "aim_json",
        "control_endpoint",
        "last_seen_at",
        "created_at",
        "lifecycle_status",
        "observation_status",
        "sources_json",
        "aliases_json",
        "revision",
        "updated_at",
        "retired_at",
        "retire_reason",
    ];
    let missing = required
        .iter()
        .filter(|name| !columns.contains(**name))
        .copied()
        .collect::<Vec<_>>();
    if missing.is_empty() {
        Ok(())
    } else {
        Err(
            json!({"code":"site_registry_schema_unprepared","message":"site registry schema is incomplete","missing_columns":missing}),
        )
    }
}
fn registry_path() -> PathBuf {
    user_site_root().join("registry.db")
}
fn user_site_root() -> PathBuf {
    env::var_os("NARADA_USER_SITE_ROOT")
        .map(PathBuf::from)
        .or_else(|| env::var_os("USERPROFILE").map(|v| PathBuf::from(v).join("Narada")))
        .or_else(|| env::var_os("HOME").map(|v| PathBuf::from(v).join("Narada")))
        .unwrap_or_else(|| PathBuf::from("Narada"))
}
fn read_candidate_config(path: &Path) -> (Option<Value>, String) {
    let parsed = fs::read_to_string(path)
        .ok()
        .and_then(|v| serde_json::from_str::<Value>(&v).ok());
    let aim = parsed
        .as_ref()
        .and_then(|v| v.get("aim"))
        .map(Value::to_string)
        .map(Value::String);
    let substrate = parsed
        .as_ref()
        .and_then(|v| v.get("substrate"))
        .and_then(Value::as_str)
        .unwrap_or("windows")
        .to_string();
    (aim, substrate)
}
fn site_id_from_root(root: &Path) -> Option<String> {
    for path in [
        root.join("site.json"),
        root.join(".narada/site.json"),
        root.join("config.json"),
        root.join(".narada/config.json"),
    ] {
        if let Ok(content) = fs::read_to_string(path) {
            if let Ok(v) = serde_json::from_str::<Value>(&content) {
                if let Some(id) = v
                    .get("site_id")
                    .and_then(Value::as_str)
                    .or_else(|| v.pointer("/site/site_id").and_then(Value::as_str))
                    .or_else(|| v.pointer("/static_config/site_id").and_then(Value::as_str))
                {
                    if !id.trim().is_empty() {
                        return Some(id.trim().to_string());
                    }
                }
            }
        }
    }
    None
}
fn parse_ps_assignment(line: &str) -> Option<(&str, String)> {
    let (key, value) = line.split_once('=')?;
    let key = key.trim();
    if !matches!(
        key,
        "Agent"
            | "Site"
            | "Role"
            | "NaradaRoot"
            | "WorkspaceRoot"
            | "SiteRoot"
            | "Launcher"
            | "Carrier"
            | "Runtime"
    ) {
        return None;
    }
    let value = value.trim();
    if value.len() < 2 {
        return None;
    }
    let quote = value.chars().next()?;
    if !matches!(quote, '\'' | '"') || !value.ends_with(quote) {
        return None;
    }
    Some((key, value[1..value.len() - 1].to_string()))
}
fn path_key(value: &str) -> String {
    value
        .replace('\\', "/")
        .trim_end_matches('/')
        .to_ascii_lowercase()
}
fn required_text(args: &Map<String, Value>, key: &str) -> Result<String, Value> {
    args.get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(ToString::to_string)
        .ok_or_else(|| {
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
    if !(min..=max).contains(&value) {
        Err(error(
            "argument_out_of_bounds",
            &format!("{key} must be between {min} and {max}"),
        ))
    } else {
        Ok(value)
    }
}
fn error(code: &str, message: &str) -> Value {
    json!({"code":code,"message":message})
}
fn db_error(code: &'static str) -> impl FnOnce(rusqlite::Error) -> Value {
    move |cause| error(code, &cause.to_string())
}
fn io_error(code: &'static str) -> impl FnOnce(std::io::Error) -> Value {
    move |cause| error(code, &cause.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;
    fn fixture() -> (PathBuf, Connection) {
        let root = env::temp_dir().join(format!("narada-site-registry-{}", Uuid::new_v4()));
        fs::create_dir_all(&root).unwrap();
        let db = Connection::open(root.join("registry.db")).unwrap();
        db.execute_batch("CREATE TABLE site_registry(site_id TEXT PRIMARY KEY,variant TEXT NOT NULL,site_root TEXT NOT NULL,substrate TEXT NOT NULL,aim_json TEXT,control_endpoint TEXT,last_seen_at TEXT,created_at TEXT NOT NULL,lifecycle_status TEXT NOT NULL,observation_status TEXT NOT NULL,sources_json TEXT NOT NULL,aliases_json TEXT NOT NULL,revision INTEGER NOT NULL,updated_at TEXT NOT NULL,retired_at TEXT,retire_reason TEXT);CREATE TABLE registry_management_audit(event_id TEXT PRIMARY KEY,site_id TEXT NOT NULL,operation TEXT NOT NULL,actor TEXT NOT NULL,reason TEXT,occurred_at TEXT NOT NULL,before_json TEXT,after_json TEXT,status TEXT NOT NULL);").unwrap();
        (root, db)
    }
    #[test]
    fn reads_registry_and_resolves_alias() {
        let (root, db) = fixture();
        db.execute("INSERT INTO site_registry VALUES(?1,'native',?2,'windows',NULL,NULL,NULL,'2026-01-01','active','present','[]','[{\"value\":\"alias-a\",\"source\":\"test\"}]',1,'2026-01-01',NULL,NULL)",params!["site-a",root.join("site-a").to_string_lossy()]).unwrap();
        drop(db);
        let listed = list_at(&Map::new(), &root.join("registry.db")).unwrap();
        assert_eq!(listed["returned"], 1);
        let shown = show_at(
            &serde_json::from_value(json!({"reference":"alias-a"})).unwrap(),
            &root.join("registry.db"),
        )
        .unwrap();
        assert_eq!(shown["site_id"], "site-a");
        fs::remove_dir_all(root).unwrap();
    }
}
