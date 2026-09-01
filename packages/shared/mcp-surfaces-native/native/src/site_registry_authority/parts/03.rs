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
