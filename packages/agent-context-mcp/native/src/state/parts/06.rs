fn value_text(v: &Value) -> String {
    v.as_str().map(str::to_string).unwrap_or_else(|| {
        if v.is_null() {
            "".into()
        } else {
            v.to_string()
        }
    })
}

fn ensure_checkpoint_tables(db: &Connection) -> Result<(), String> {
    db.execute_batch("CREATE TABLE IF NOT EXISTS agent_checkpoints (checkpoint_id TEXT PRIMARY KEY,agent_id TEXT NOT NULL,session_id TEXT,checkpoint_at TEXT NOT NULL,active_task_json TEXT,files_touched_json TEXT,key_decisions_json TEXT,open_questions_json TEXT,git_head TEXT,payload_json TEXT); CREATE INDEX IF NOT EXISTS idx_agent_checkpoints_agent ON agent_checkpoints(agent_id,checkpoint_at DESC); CREATE TABLE IF NOT EXISTS agent_checkpoint_history (history_id TEXT PRIMARY KEY,checkpoint_id TEXT NOT NULL,agent_id TEXT NOT NULL,session_id TEXT,checkpoint_at TEXT NOT NULL,active_task_json TEXT,files_touched_json TEXT,key_decisions_json TEXT,open_questions_json TEXT,git_head TEXT,payload_json TEXT,archived_at TEXT NOT NULL); CREATE INDEX IF NOT EXISTS idx_checkpoint_history_agent ON agent_checkpoint_history(agent_id,archived_at DESC); CREATE TABLE IF NOT EXISTS identity_state_records (record_id TEXT PRIMARY KEY,event_id TEXT,session_id TEXT,claimed_identity_json TEXT NOT NULL,authentication_json TEXT NOT NULL,authority_json TEXT NOT NULL,recorded_at TEXT NOT NULL); CREATE TRIGGER IF NOT EXISTS identity_state_records_no_update BEFORE UPDATE ON identity_state_records BEGIN SELECT RAISE(ABORT, 'identity_state_records_append_only_no_update'); END; CREATE TRIGGER IF NOT EXISTS identity_state_records_no_delete BEFORE DELETE ON identity_state_records BEGIN SELECT RAISE(ABORT, 'identity_state_records_append_only_no_delete'); END;").map_err(db_error)
}

fn checkpoint_by_id(db: &Connection, agent: &str, id: &str) -> Result<Option<Value>, String> {
    let current = db
        .query_row(
            "SELECT * FROM agent_checkpoints WHERE agent_id=?1 AND checkpoint_id=?2 LIMIT 1",
            params![agent, id],
            row_to_checkpoint,
        )
        .optional()
        .map_err(db_error)?;
    if current.is_some() {
        return Ok(current);
    }
    db.query_row("SELECT checkpoint_id,agent_id,session_id,checkpoint_at,active_task_json,files_touched_json,key_decisions_json,open_questions_json,git_head,payload_json FROM agent_checkpoint_history WHERE agent_id=?1 AND checkpoint_id=?2 ORDER BY archived_at DESC LIMIT 1",params![agent,id],row_to_checkpoint).optional().map_err(db_error)
}

fn row_to_checkpoint(row: &Row) -> rusqlite::Result<Value> {
    let payload: Value = parse_json(row.get::<_, Option<String>>(9)?.as_deref(), json!({}));
    let identity_state = payload.get("identity_state").cloned().unwrap_or(Value::Null);
    let mut projected_payload = payload.clone();
    if let Some(object) = projected_payload.as_object_mut() { object.remove("identity_state"); }
    Ok(
        json!({"checkpoint_id":row.get::<_,String>(0)?,"agent_id":row.get::<_,String>(1)?,"session_id":row.get::<_,Option<String>>(2)?,"checkpoint_at":row.get::<_,String>(3)?,"active_task":parse_json(row.get::<_,Option<String>>(4)?.as_deref(),Value::Null),"files_touched":parse_json(row.get::<_,Option<String>>(5)?.as_deref(),json!([])),"key_decisions":parse_json(row.get::<_,Option<String>>(6)?.as_deref(),json!([])),"open_questions":parse_json(row.get::<_,Option<String>>(7)?.as_deref(),json!([])),"git_head":row.get::<_,Option<String>>(8)?,"last_workboard_check_at":payload.get("last_workboard_check_at").cloned().unwrap_or(Value::Null),"next_intended_action":payload.get("next_intended_action").cloned().unwrap_or(Value::Null),"authority_basis":payload.get("authority_basis").cloned().unwrap_or(Value::Null),"continuation_blockers":payload.get("continuation_blockers").cloned().unwrap_or_else(||json!([])),"evidence_refs":payload.get("evidence_refs").cloned().unwrap_or_else(||json!([])),"worktree_state":payload.get("worktree_state").cloned().unwrap_or(Value::Null),"tactical_resume_notes":payload.get("tactical_resume_notes").cloned().unwrap_or_else(||json!([])),"continuation":payload.get("continuation").cloned().unwrap_or(Value::Null),"continuation_ref":payload.get("continuation_ref").cloned().unwrap_or(Value::Null),"continuation_projection":payload.get("continuation_projection").cloned().unwrap_or(Value::Null),"identity_state":identity_state,"payload":projected_payload}),
    )
}

fn validate_identity(context: &Context, agent: &str) -> Result<(), String> {
    if agent.trim().is_empty() {
        return Err(format!("agent_context_identity_invalid: {agent}"));
    }
    let roster = context.site_root.join(".ai/agents/roster.json");
    if !roster.exists() {
        return Ok(());
    }
    let value: Value =
        serde_json::from_slice(&fs::read(&roster).map_err(|e| format!("roster_read_error: {e}"))?)
            .map_err(|e| format!("roster_parse_error: {e}"))?;
    let found = value
        .get("agents")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .any(|v| v.get("agent_id").and_then(Value::as_str) == Some(agent))
        })
        .unwrap_or(false);
    if found || value.get("enforce_session_roster") != Some(&Value::Bool(true)) {
        Ok(())
    } else {
        Err(format!("identity_not_in_roster: {agent}"))
    }
}

fn normalize_continuation(
    value: Option<&Value>,
    checkpoint_id: &str,
    checkpoint_at: &str,
) -> Result<Option<Value>, String> {
    let Some(value) = value.filter(|v| !v.is_null()) else {
        return Ok(None);
    };
    let object = value
        .as_object()
        .ok_or("continuation_invalid: expected an object")?;
    let allowed = [
        "schema",
        "continuation_id",
        "objective",
        "current_state",
        "completed_work",
        "decisions",
        "evidence_refs",
        "open_blockers",
        "next_action",
        "canonical_sources",
        "constraints",
        "resume_mode",
        "created_at",
    ];
    if let Some(key) = object.keys().find(|k| !allowed.contains(&k.as_str())) {
        return Err(format!("continuation_field_unknown: {key}"));
    }
    if value.get("schema").and_then(Value::as_str) != Some("narada.continuation.v1") {
        return Err("continuation_schema_invalid".into());
    }
    let objective =
        required_string(value, "objective").map_err(|_| "continuation_objective_invalid")?;
    let current_state = required_string(value, "current_state")
        .map_err(|_| "continuation_current_state_invalid")?;
    let resume = value
        .get("resume_mode")
        .and_then(Value::as_str)
        .unwrap_or("fresh_session");
    if !matches!(resume, "fresh_session" | "same_session") {
        return Err("continuation_resume_mode_invalid".into());
    }
    let mut canonical = json!({"schema":"narada.continuation.v1","continuation_id":value.get("continuation_id").and_then(Value::as_str).map(str::to_string).unwrap_or_else(||id("cont")),"objective":objective,"current_state":current_state,"completed_work":array(value,"completed_work"),"decisions":array(value,"decisions"),"evidence_refs":array(value,"evidence_refs"),"open_blockers":array(value,"open_blockers"),"next_action":field_or_null(value,"next_action"),"canonical_sources":array(value,"canonical_sources"),"constraints":array(value,"constraints"),"resume_mode":resume,"source_checkpoint_ref":format!("agent_context_checkpoint:{checkpoint_id}"),"created_at":value.get("created_at").cloned().unwrap_or_else(||Value::String(checkpoint_at.into()))});
    if serde_json::to_vec(&canonical)
        .map_err(|error| error.to_string())?
        .len()
        > 64 * 1024
    {
        return Err("continuation_too_large".into());
    }
    let mut content = canonical.clone();
    content
        .as_object_mut()
        .unwrap()
        .remove("source_checkpoint_ref");
    use sha2::{Digest, Sha256};
    canonical.as_object_mut().unwrap().insert(
        "content_hash".into(),
        Value::String(format!(
            "{:x}",
            Sha256::digest(serde_json::to_vec(&content).unwrap())
        )),
    );
    Ok(Some(canonical))
}

fn continuation_projection(
    agent: &str,
    reference: Option<&Value>,
    previous: Option<&Value>,
) -> Value {
    if let Some(reference) = reference {
        return json!({"status":"linked","reason":null,"continuation_ref":reference,"previous_checkpoint_id":null,"next_action":null});
    }
    let previous_ref = previous
        .and_then(|v| v.get("continuation_ref"))
        .filter(|v| !v.is_null());
    let mut arguments = json!({"agent_id":agent});
    if let Some(path) = previous_ref
        .and_then(|v| v.get("path"))
        .and_then(Value::as_str)
    {
        arguments
            .as_object_mut()
            .unwrap()
            .insert("path".into(), json!(path));
        arguments
            .as_object_mut()
            .unwrap()
            .insert("overwrite".into(), json!(true));
    }
    json!({"status":if previous_ref.is_some(){"stale"}else{"unlinked"},"reason":if previous_ref.is_some(){"checkpoint_supersedes_linked_projection"}else{"continuation_projection_not_exported"},"continuation_ref":previous_ref.cloned().unwrap_or(Value::Null),"previous_checkpoint_id":if previous_ref.is_some(){previous.and_then(|v|v.get("checkpoint_id")).cloned().unwrap_or(Value::Null)}else{Value::Null},"next_action":{"tool":"agent_context_continuation_export","arguments":arguments}})
}

fn required_string(v: &Value, key: &str) -> Result<String, String> {
    v.get(key)
        .and_then(Value::as_str)
        .filter(|s| !s.trim().is_empty())
        .map(str::to_string)
        .ok_or_else(|| format!("{key}_required"))
}
fn optional_string(v: &Value, key: &str) -> Result<Option<String>, String> {
    match v.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) if !s.trim().is_empty() => Ok(Some(s.trim().into())),
        _ => Err(format!("{key}_invalid")),
    }
}
fn field_or_null(v: &Value, key: &str) -> Value {
    v.get(key).cloned().unwrap_or(Value::Null)
}
fn array(v: &Value, key: &str) -> Value {
    v.get(key)
        .filter(|v| v.is_array())
        .cloned()
        .unwrap_or_else(|| json!([]))
}
fn json_text(v: Value) -> String {
    serde_json::to_string(&v).unwrap()
}
fn json_db(v: Option<&Value>) -> Option<String> {
    v.filter(|v| !v.is_null()).map(|v| json_text(v.clone()))
}
fn parse_json(v: Option<&str>, fallback: Value) -> Value {
    v.and_then(|s| serde_json::from_str(s).ok())
        .unwrap_or(fallback)
}
fn merge(mut a: Value, b: Value) -> Value {
    a.as_object_mut()
        .unwrap()
        .extend(b.as_object().unwrap().clone());
    a
}
fn timestamp() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true)
}
fn id(prefix: &str) -> String {
    format!("{prefix}_{}", Uuid::new_v4().simple())
}
fn path_text(path: &Path) -> String {
    let text = path.to_string_lossy();
    text.strip_prefix(r"\\?\").unwrap_or(&text).to_string()
}
fn sanitize_site_id(v: &str) -> String {
    v.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || "_.-".contains(c) {
                c
            } else {
                '-'
            }
        })
        .collect()
}
fn db_error(e: rusqlite::Error) -> String {
    format!("agent_context_db_error:{e}")
}
