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

