fn output_show(args: &Map<String, Value>, root: &Path) -> Result<Value, Value> {
    let reference = optional_string(args, "ref")
        .or_else(|| optional_string(args, "output_ref"))
        .ok_or_else(|| error("output_ref_required", "output_ref_required"))?;
    let id = reference.strip_prefix("mcp_output:").ok_or_else(|| {
        error(
            "output_ref_invalid",
            &format!("output_ref_invalid:{reference}"),
        )
    })?;
    if id.is_empty()
        || id.len() > 80
        || !id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        return Err(error(
            "output_ref_invalid",
            &format!("output_ref_invalid:{reference}"),
        ));
    }
    let path = root
        .join(".ai/tmp/mcp-outputs/workspace")
        .join(format!("{id}.json"));
    if fs::metadata(&path).map_err(|_| error("output_ref_not_found", &format!("output_ref_not_found:{reference}")))?.len() > MAX_OUTPUT_BYTES { return Err(error("output_ref_too_large", "output_ref_too_large")); }
    let text = fs::read_to_string(&path).map_err(|_| {
        error(
            "output_ref_not_found",
            &format!("output_ref_not_found:{reference}"),
        )
    })?;
    let record: Value = serde_json::from_str(&text)
        .map_err(|e| error("output_ref_invalid_json", &e.to_string()))?;
    if record.get("schema").and_then(Value::as_str) != Some("narada.mcp_output_ref.v1") {
        return Err(error(
            "output_ref_schema_unsupported",
            "output_ref_schema_unsupported",
        ));
    }
    let full = record.get("full_output").cloned().unwrap_or(Value::Null);
    let presentation = serde_json::to_string_pretty(&full).unwrap_or_else(|_| full.to_string());
    let offset = args.get("offset").and_then(Value::as_u64).unwrap_or(0) as usize;
    let limit = args
        .get("limit")
        .and_then(Value::as_u64)
        .unwrap_or(4000)
        .min(10000) as usize;
    let chars = presentation.chars().collect::<Vec<_>>();
    let start = offset.min(chars.len());
    let chunk = chars.iter().skip(start).take(limit).collect::<String>();
    let end = start + chunk.chars().count();
    Ok(json!({
        "schema":"narada.mcp_output_page.v1","status":"ok","ref":reference,
        "tool_name":record.get("tool_name"),"full_output_char_length":chars.len(),
        "byte_size":text.len(),"original_truncated":record.get("truncated").and_then(Value::as_bool).unwrap_or(false),
        "path":path.to_string_lossy(),"offset":start,"limit":limit,
        "next_offset":if end<chars.len(){json!(end)}else{Value::Null},
        "output_limit":limit,"output_truncated":end<chars.len(),"output_text":chunk
    }))
}

fn refresh(root: &Path) -> Result<(i64, usize), Value> {
    let mut db = open_db(root)?;
    let now = now_iso();
    let latest = latest(root);
    let files = envelope_files(root);
    let tx = db
        .transaction()
        .map_err(|e| db_err("inbox_index_transaction_failed", e))?;
    tx.execute("DELETE FROM inbox_envelopes", [])
        .map_err(|e| db_err("inbox_index_clear_failed", e))?;
    let mut invalid = 0usize;
    for path in files {
        let text = match fs::metadata(&path).ok().filter(|metadata| metadata.len() <= MAX_ENVELOPE_BYTES).and_then(|_| fs::read_to_string(&path).ok()) {
            Some(v) => v.trim_start_matches('\u{feff}').to_string(),
            None => {
                invalid += 1;
                continue;
            }
        };
        let value: Value = match serde_json::from_str(&text) {
            Ok(v) => v,
            Err(_) => {
                invalid += 1;
                continue;
            }
        };
        let Some(envelope) = value.as_object() else {
            invalid += 1;
            continue;
        };
        let Some(id) = envelope.get("envelope_id").and_then(Value::as_str) else {
            invalid += 1;
            continue;
        };
        if !valid_id(id) {
            invalid += 1;
            continue;
        }
        let sev = severity(envelope);
        let auth = envelope.get("authority").and_then(Value::as_object);
        let payload = envelope.get("payload").and_then(Value::as_object);
        let source = envelope.get("source").and_then(Value::as_object);
        let status = effective(envelope, latest.get(id));
        tx.execute(
            "INSERT INTO inbox_envelopes(envelope_id,file_path,status,kind,authority_level,title,summary,principal,source_ref,received_at,target_role,severity,severity_reason,action,payload_json,indexed_at) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16)",
            params![
                id, path.to_string_lossy().to_string(), status,
                envelope.get("kind").and_then(Value::as_str).unwrap_or("observation"),
                auth.and_then(|a| a.get("level")).and_then(Value::as_str).unwrap_or("agent_reported"),
                envelope.get("title").and_then(Value::as_str).or_else(|| payload.and_then(|p| p.get("title")).and_then(Value::as_str)).unwrap_or("(untitled)"),
                envelope.get("summary").and_then(Value::as_str).or_else(|| payload.and_then(|p| p.get("summary")).and_then(Value::as_str)),
                envelope.get("principal").and_then(Value::as_str).or_else(|| auth.and_then(|a| a.get("principal")).and_then(Value::as_str)).or_else(|| payload.and_then(|p| p.get("principal")).and_then(Value::as_str)),
                source.and_then(|s| s.get("ref")).and_then(Value::as_str),
                envelope.get("received_at").and_then(Value::as_str),
                sev.role, sev.value, sev.reason, sev.action, text, now
            ],
        ).map_err(|e| db_err("inbox_index_insert_failed", e))?;
    }
    tx.execute(
        "INSERT INTO inbox_index_meta(key,value,updated_at) VALUES('last_refreshed_at',?1,?1) ON CONFLICT(key) DO UPDATE SET value=excluded.value,updated_at=excluded.updated_at",
        params![now.clone()],
    ).map_err(|e| db_err("inbox_index_meta_failed", e))?;
    tx.commit()
        .map_err(|e| db_err("inbox_index_commit_failed", e))?;
    let count = db
        .query_row("SELECT COUNT(*) FROM inbox_envelopes", [], |r| {
            r.get::<_, i64>(0)
        })
        .map_err(|e| db_err("inbox_index_count_failed", e))?;
    Ok((count, invalid))
}

fn open_db(root: &Path) -> Result<Connection, Value> {
    let path = root.join(".ai/state/inbox-index.sqlite");
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|e| error("inbox_index_directory_failed", &e.to_string()))?;
    }
    let db = Connection::open(path).map_err(|e| db_err("inbox_index_open_failed", e))?;
    db.execute_batch(
        "PRAGMA journal_mode=WAL; PRAGMA user_version=1;
         CREATE TABLE IF NOT EXISTS inbox_index_meta(key TEXT PRIMARY KEY,value TEXT NOT NULL,updated_at TEXT NOT NULL);
         CREATE TABLE IF NOT EXISTS inbox_envelopes(
           envelope_id TEXT PRIMARY KEY,file_path TEXT NOT NULL,status TEXT NOT NULL,kind TEXT NOT NULL,
           authority_level TEXT,title TEXT,summary TEXT,principal TEXT,source_ref TEXT,received_at TEXT,
           target_role TEXT,severity INTEGER,severity_reason TEXT,action TEXT,payload_json TEXT NOT NULL,indexed_at TEXT NOT NULL
         );
         CREATE INDEX IF NOT EXISTS idx_inbox_envelopes_status_received ON inbox_envelopes(status,received_at);
         CREATE INDEX IF NOT EXISTS idx_inbox_envelopes_severity ON inbox_envelopes(status,severity DESC,received_at);",
    ).map_err(|e| db_err("inbox_index_schema_failed", e))?;
    db.execute(
        "INSERT INTO inbox_index_meta(key,value,updated_at) VALUES('schema_version','1',?1) ON CONFLICT(key) DO UPDATE SET value=excluded.value,updated_at=excluded.updated_at",
        params![now_iso()],
    ).map_err(|e| db_err("inbox_index_meta_failed", e))?;
    Ok(db)
}

fn rows(root: &Path) -> Result<Vec<Map<String, Value>>, Value> {
    refresh(root)?;
    rows_after_refresh(root)
}

fn rows_after_refresh(root: &Path) -> Result<Vec<Map<String, Value>>, Value> {
    let db = open_db(root)?;
    let mut statement = db.prepare(
        "SELECT envelope_id,file_path,status,kind,authority_level,title,summary,principal,source_ref,
         received_at,target_role,severity,severity_reason,action,payload_json,indexed_at
         FROM inbox_envelopes ORDER BY COALESCE(severity,0) DESC,COALESCE(received_at,'') ASC",
    ).map_err(|e| db_err("inbox_index_query_failed", e))?;
    let result = statement
        .query_map([], row_record)
        .map_err(|e| db_err("inbox_index_query_failed", e))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| db_err("inbox_index_query_failed", e));
    result
}

fn read_row(root: &Path, id: &str) -> Result<Option<Map<String, Value>>, Value> {
    refresh(root)?;
    let db = open_db(root)?;
    db.query_row(
        "SELECT envelope_id,file_path,status,kind,authority_level,title,summary,principal,source_ref,
         received_at,target_role,severity,severity_reason,action,payload_json,indexed_at
         FROM inbox_envelopes WHERE envelope_id=?1",
        params![id], row_record,
    ).optional().map_err(|e| db_err("inbox_index_query_failed", e))
}

fn row_record(row: &rusqlite::Row<'_>) -> rusqlite::Result<Map<String, Value>> {
    let fields = [
        (0, "envelope_id"),
        (1, "file_path"),
        (2, "status"),
        (3, "kind"),
        (4, "authority_level"),
        (5, "title"),
        (6, "summary"),
        (7, "principal"),
        (8, "source_ref"),
        (9, "received_at"),
        (10, "target_role"),
        (12, "severity_reason"),
        (13, "action"),
        (14, "payload_json"),
        (15, "indexed_at"),
    ];
    let mut out = Map::new();
    for (index, key) in fields {
        let value: Option<String> = row.get(index)?;
        out.insert(key.into(), value.map(Value::String).unwrap_or(Value::Null));
    }
    let severity: Option<i64> = row.get(11)?;
    out.insert(
        "severity".into(),
        severity.map(Value::from).unwrap_or(Value::Null),
    );
    Ok(out)
}

#[derive(Clone)]
struct Severity {
    role: Option<String>,
    value: Option<i64>,
    reason: Option<String>,
    action: Option<String>,
}

