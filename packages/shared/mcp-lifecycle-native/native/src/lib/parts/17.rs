
fn migrate_legacy_recurring_schema(c: &Connection) -> Result<(), String> {
    let migration_required = !has_column(c, "recurring_task_definitions", "definition_json")?
        || has_column(c, "recurring_task_events", "state_after")?
        || has_column(c, "recurring_task_runs", "run_reason")?;
    if !migration_required {
        return Ok(());
    }
    let foreign_keys_enabled: i64 = c
        .pragma_query_value(None, "foreign_keys", |row| row.get(0))
        .map_err(db_error)?;
    c.execute_batch("pragma foreign_keys=off; begin immediate;")
        .map_err(db_error)?;
    let result = migrate_legacy_recurring_schema_in_transaction(c).and_then(|_| {
        let violation = c
            .query_row("pragma foreign_key_check", [], |_| Ok(()))
            .optional()
            .map_err(db_error)?;
        if violation.is_some() {
            Err("recurring_schema_migration_foreign_key_violation".to_string())
        } else {
            Ok(())
        }
    });
    if result.is_ok() {
        c.execute_batch("commit;").map_err(db_error)?;
    } else {
        let _ = c.execute_batch("rollback;");
    }
    if foreign_keys_enabled != 0 {
        c.pragma_update(None, "foreign_keys", true).map_err(db_error)?;
    }
    result
}

fn migrate_legacy_recurring_schema_in_transaction(c: &Connection) -> Result<(), String> {
    if !has_column(c, "recurring_task_definitions", "definition_json")? {
        let mut statement = c.prepare("select recurrence_id,title,status,trigger_mode,trigger_description,target_role,preferred_role,goal_markdown,context_markdown,required_work_markdown,non_goals_markdown,acceptance_criteria_json,evidence_requirements_json,created_by,created_at,updated_at,schedule_kind,schedule_interval,schedule_timezone,last_due_key,last_auto_triggered_at from recurring_task_definitions").map_err(db_error)?;
        let rows = statement.query_map([], |row| {
            let array = |index| -> rusqlite::Result<Value> {
                let text: String = row.get(index)?;
                Ok(serde_json::from_str(&text).unwrap_or_else(|_| json!([])))
            };
            Ok((
                row.get::<_, String>(0)?,
                json!({
                    "recurrence_id": row.get::<_, String>(0)?,
                    "title": row.get::<_, String>(1)?,
                    "status": row.get::<_, String>(2)?,
                    "trigger_mode": row.get::<_, String>(3)?,
                    "trigger_description": row.get::<_, Option<String>>(4)?,
                    "target_role": row.get::<_, Option<String>>(5)?,
                    "preferred_role": row.get::<_, Option<String>>(6)?,
                    "goal": row.get::<_, Option<String>>(7)?,
                    "context": row.get::<_, Option<String>>(8)?,
                    "required_work": row.get::<_, Option<String>>(9)?,
                    "non_goals": row.get::<_, Option<String>>(10)?,
                    "acceptance_criteria": array(11)?,
                    "evidence_requirements": array(12)?,
                    "created_by": row.get::<_, String>(13)?,
                    "created_at": row.get::<_, String>(14)?,
                    "updated_at": row.get::<_, String>(15)?,
                    "schedule_kind": row.get::<_, Option<String>>(16)?,
                    "schedule_interval": row.get::<_, Option<i64>>(17)?,
                    "schedule_timezone": row.get::<_, Option<String>>(18)?,
                    "last_due_key": row.get::<_, Option<String>>(19)?,
                    "last_auto_triggered_at": row.get::<_, Option<String>>(20)?
                }),
            ))
        }).map_err(db_error)?.collect::<Result<Vec<_>, _>>().map_err(db_error)?;
        drop(statement);
        c.execute_batch(
            "create table recurring_task_definitions_native(
                recurrence_id text primary key,
                status text not null,
                definition_json text not null,
                last_due_key text,
                last_auto_triggered_at text,
                updated_at text not null
             );",
        )
        .map_err(db_error)?;
        for (id, definition) in rows {
            c.execute(
                "insert into recurring_task_definitions_native(recurrence_id,status,definition_json,last_due_key,last_auto_triggered_at,updated_at)
                 values(?1,?2,?3,?4,?5,?6)",
                params![
                    id,
                    definition["status"].as_str().unwrap_or("active"),
                    definition.to_string(),
                    definition["last_due_key"].as_str(),
                    definition["last_auto_triggered_at"].as_str(),
                    definition["updated_at"].as_str().unwrap_or("")
                ],
            )
            .map_err(db_error)?;
        }
        c.execute_batch(
            "drop table recurring_task_definitions;
             alter table recurring_task_definitions_native rename to recurring_task_definitions;
             create index if not exists idx_recurring_task_definitions_status
                on recurring_task_definitions(status);",
        )
        .map_err(db_error)?;
    }
    if has_column(c, "recurring_task_events", "state_after")? {
        c.execute_batch("create table recurring_task_events_native(event_id text primary key,recurrence_id text not null,event_type text not null,actor_agent_id text not null,authority_basis_json text not null,event_json text not null,created_at text not null);
            insert into recurring_task_events_native(event_id,recurrence_id,event_type,actor_agent_id,authority_basis_json,event_json,created_at) select event_id,recurrence_id,event_type,actor_agent_id,authority_basis_json,event_json,created_at from recurring_task_events;
            drop table recurring_task_events;
            alter table recurring_task_events_native rename to recurring_task_events;").map_err(db_error)?;
    }
    if has_column(c, "recurring_task_runs", "run_reason")? {
        c.execute_batch("create table recurring_task_runs_native(run_id text primary key,recurrence_id text not null,task_id text,task_number integer,due_key text,trigger_mode text not null,reason text not null,created_at text not null,run_json text not null);
            insert into recurring_task_runs_native(run_id,recurrence_id,task_id,task_number,due_key,trigger_mode,reason,created_at,run_json) select run_id,recurrence_id,task_id,task_number,null,trigger_mode,run_reason,created_at,json_object('run_id',run_id,'recurrence_id',recurrence_id,'task_id',task_id,'task_number',task_number,'trigger_mode',trigger_mode,'reason',run_reason,'actor_agent_id',actor_agent_id,'authority_basis',json(authority_basis_json),'created_at',created_at) from recurring_task_runs;
            drop table recurring_task_runs;
            alter table recurring_task_runs_native rename to recurring_task_runs;
            create index if not exists idx_recurring_task_runs_recurrence on recurring_task_runs(recurrence_id,created_at desc);").map_err(db_error)?;
    }
    Ok(())
}
fn ensure_downstream_dependency_contracts(c: &Connection) -> Result<(), String> {
    let mut statement = c
        .prepare("select required_task_id,satisfying_outcomes_json,created_by,created_at from task_dependencies where kind='downstream_work'")
        .map_err(db_error)?;
    let dependencies = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
            ))
        })
        .map_err(db_error)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(db_error)?;
    for (task_id, satisfying_json, created_by, created_at) in dependencies {
        let exists: bool = c
            .query_row("select count(*) from task_outcome_contracts where task_id=?1 and outcome_type='completion'", params![&task_id], |row| row.get::<_, i64>(0))
            .map_err(db_error)? > 0;
        if exists { continue; }
        let satisfying = serde_json::from_str::<Value>(&satisfying_json).unwrap_or_else(|_| json!(["completed"]));
        let satisfying = if satisfying.as_array().is_some_and(|items| !items.is_empty()) { satisfying } else { json!(["completed"]) };
        let allowed = json!(["completed","blocked","failed"]);
        c.execute("insert or ignore into task_outcome_contracts(contract_id,task_id,outcome_type,allowed_outcomes_json,satisfying_outcomes_json,blocking_outcomes_json,required_fields_json,capability_requirement,created_by,created_at) values(?1,?2,'completion',?3,?4,?5,?6,null,?7,?8)", params![format!("contract-downstream_work-{task_id}"), &task_id, allowed.to_string(), satisfying.to_string(), json!(["blocked","failed"]).to_string(), json!(["summary"]).to_string(), &created_by, &created_at]).map_err(db_error)?;
    }
    Ok(())
}
fn ensure_task_revision_column(c: &Connection) -> Result<(), String> {
    if !has_column(c, "task_lifecycle", "revision")? {
        c.execute(
            "alter table task_lifecycle add column revision integer not null default 1",
            [],
        )
        .map_err(db_error)?;
    }
    Ok(())
}
fn has_column(c: &Connection, table: &str, column: &str) -> Result<bool, String> {
    let mut s = c
        .prepare(&format!("pragma table_info({table})"))
        .map_err(db_error)?;
    let mut rows = s.query([]).map_err(db_error)?;
    while let Some(r) = rows.next().map_err(db_error)? {
        if r.get::<_, String>(1).map_err(db_error)? == column {
            return Ok(true);
        }
    }
    Ok(false)
}
fn lifecycle_value(r: &Row<'_>) -> rusqlite::Result<Value> {
    let mut m = Map::new();
    for (i, name) in [
        "task_id",
        "task_number",
        "status",
        "governed_by",
        "closed_at",
        "closed_by",
        "closure_mode",
        "relative_priority",
        "priority_reason",
        "reopened_at",
        "reopened_by",
        "continuation_packet_json",
        "updated_at",
        "revision",
    ]
    .iter()
    .enumerate()
    .take(r.as_ref().column_count())
    {
        let v: rusqlite::types::Value = r.get(i)?;
        m.insert((*name).to_string(), sql_value(v));
    }
    Ok(Value::Object(m))
}
fn row_to_object(r: &Row<'_>) -> rusqlite::Result<Value> {
    let mut m = Map::new();
    for i in 0..r.as_ref().column_count() {
        let name = r.as_ref().column_name(i)?.to_string();
        let v: rusqlite::types::Value = r.get(i)?;
        m.insert(name, sql_value(v));
    }
    Ok(Value::Object(m))
}
