
impl Options {
    fn database_path(&self) -> PathBuf {
        self.site_root.join(self.surface.database_relative_path())
    }
}
impl Surface {
    fn prefix(self) -> &'static str {
        match self {
            Self::Task => "task_lifecycle",
            Self::Work => "work_lifecycle",
        }
    }
}

fn inspect_database(options: &Options) -> Result<Value, String> {
    let path = options.database_path();
    if !path.exists() {
        return Ok(
            json!({"status":"missing","db_path":path,"schema_version":null,"reason":"database_missing"}),
        );
    }
    let mut c = Connection::open(&path).map_err(|_| "invalid_database".to_string())?;
    configure_connection(&mut c, false).ok();
    inspect_connection(options.surface, &c, &path)
}
fn inspect_connection(surface: Surface, c: &Connection, path: &Path) -> Result<Value, String> {
    let mut tables = Vec::new();
    let mut st = c
        .prepare("select name from sqlite_master where type='table'")
        .map_err(db_error)?;
    let mut rows = st.query([]).map_err(db_error)?;
    while let Some(r) = rows.next().map_err(db_error)? {
        tables.push(r.get::<_, String>(0).map_err(db_error)?);
    }
    let required = if surface == Surface::Task {
        vec!["task_lifecycle", "task_specs", "task_assignments"]
    } else {
        vec![
            "task_lifecycle",
            "task_specs",
            "tickets",
            "work_lifecycle_meta",
            "work_outbox",
        ]
    };
    if required.iter().any(|x| !tables.iter().any(|v| v == x)) {
        return Ok(
            json!({"status":"stale","db_path":path,"schema_version":null,"reason":"schema"}),
        );
    }
    if surface == Surface::Work {
        let version: Option<i64> = c
            .query_row(
                "select schema_version from work_lifecycle_meta where singleton=1",
                [],
                |r| r.get(0),
            )
            .optional()
            .map_err(db_error)?;
        if version != Some(WORK_SCHEMA_VERSION) {
            return Ok(
                json!({"status":"stale","db_path":path,"work_schema_version":version,"task_schema_version":TASK_SCHEMA_VERSION,"reason":"work_schema_version"}),
            );
        }
        return Ok(
            json!({"status":"prepared","db_path":path,"work_schema_version":version,"task_schema_version":TASK_SCHEMA_VERSION}),
        );
    }
    Ok(json!({"status":"prepared","db_path":path,"schema_version":TASK_SCHEMA_VERSION}))
}
fn configure_connection(c: &mut Connection, prepare: bool) -> Result<(), String> {
    c.busy_timeout(std::time::Duration::from_millis(5000))
        .map_err(db_error)?;
    c.execute_batch("pragma foreign_keys=on; pragma recursive_triggers=off;")
        .map_err(db_error)?;
    if prepare {
        c.execute_batch("pragma journal_mode=wal; pragma synchronous=normal;")
            .map_err(db_error)?;
    }
    Ok(())
}
fn ensure_work_task_revision_triggers(c: &Connection) -> Result<(), String> {
    let mut statement = c.prepare("select name from sqlite_master where type=?1").map_err(db_error)?;
    let tables = statement.query_map(params!["table"], |row| row.get::<_, String>(0)).map_err(db_error)?.collect::<Result<Vec<_>, _>>().map_err(db_error)?;
    for table in tables {
        if table == "task_lifecycle" || table.starts_with("sqlite_") || table.starts_with("ticket_") || table.starts_with("work_") || !table.chars().all(|value| value.is_ascii_alphanumeric() || value == '_') { continue; }
        if !has_column(c, &table, "task_id")? { continue; }
        for (operation, reference) in [("insert", "new"), ("update", "new"), ("delete", "old")] {
            let trigger = format!("work_task_revision_{table}_{operation}");
            let sql = format!("drop trigger if exists {trigger}; create trigger {trigger} after {operation} on {table} when {reference}.task_id is not null begin update task_lifecycle set updated_at=strftime(\"%Y-%m-%dT%H:%M:%fZ\",\"now\") where task_id={reference}.task_id; end;");
            c.execute_batch(&sql).map_err(db_error)?;
        }
    }
    Ok(())
}
fn ensure_task_post_schema(c: &Connection) -> Result<(), String> {
    for (column, ty) in [
        ("closure_mode", "text"),
        ("relative_priority", "integer default 0"),
        ("priority_reason", "text"),
    ] {
        let exists = has_column(c, "task_lifecycle", column)?;
        if !exists {
            c.execute(
                &format!("alter table task_lifecycle add column {column} {ty}"),
                [],
            )
            .map_err(db_error)?;
        }
    }
    if !has_column(c, "task_specs", "tags_json")? {
        c.execute(
            "alter table task_specs add column tags_json text not null default '[]'",
            [],
        )
        .map_err(db_error)?;
    }
    if !has_column(c, "task_reports", "directive_id")? {
        c.execute("alter table task_reports add column directive_id text", [])
            .map_err(db_error)?;
    }
    for (table, column) in [("task_reports", "agent_identity_ref_json"), ("task_report_records", "agent_identity_ref_json")] {
        if !has_column(c, table, column)? {
            c.execute(&format!("alter table {table} add column {column} text"), []).map_err(db_error)?;
        }
    }
    c.execute_batch("create index if not exists idx_task_reports_directive_id on task_reports(directive_id); create table if not exists task_tag_updates(update_id text primary key,task_id text not null,task_number integer not null,actor_agent_id text not null,previous_tags_json text not null,new_tags_json text not null,reason text not null,updated_at text not null);").map_err(db_error)?;
    c.execute_batch("create table if not exists narada_task_creation_requests(idempotency_key text primary key,payload_sha256 text not null,task_id text not null unique,task_number integer not null unique,file_path text not null,execution_binding_json text not null,status text not null check(status in ('reserved','created','failed')),created_at text not null,updated_at text not null);
        create index if not exists idx_narada_task_creation_requests_status on narada_task_creation_requests(status);
        create table if not exists narada_task_execution_bindings(task_id text primary key,task_number integer not null unique,binding_json text not null,correlation_key text not null unique,created_at text not null,updated_at text not null);
        create table if not exists narada_andrey_task_role_preferences(task_id text primary key,preferred_role text,target_role text,preferred_agent_id text,updated_at text not null);
        create table if not exists task_routing_events(event_id text primary key,task_id text not null,task_number integer not null,actor_agent_id text not null,actor_role text,reason text not null,changed_fields_json text not null,previous_routing_json text not null,new_routing_json text not null,created_at text not null);
        create index if not exists idx_task_routing_events_task_id on task_routing_events(task_id);
        create table if not exists agent_roster_events(event_id text primary key,event_type text not null,agent_id text not null,role text,capabilities_json text,operator_identity text,requested_by text not null,requested_at text not null,authority_basis_json text not null,admission_status text not null,admitted_by text,admitted_at text,reason text,payload_json text,supersedes_event_id text);
        create index if not exists idx_agent_roster_events_agent_id on agent_roster_events(agent_id,requested_at);
        create index if not exists idx_agent_roster_events_status on agent_roster_events(admission_status,requested_at);").map_err(db_error)?;
    c.execute_batch("create table if not exists task_result_contracts (
        task_id text primary key,
        schema_id text not null,
        schema_digest text not null,
        schema_json text not null,
        created_at text not null,
        foreign key (task_id) references task_lifecycle(task_id)
    );
    create table if not exists task_structured_results (
        result_id text primary key,
        task_id text not null,
        report_id text not null,
        schema_id text not null,
        schema_digest text not null,
        result_json text not null,
        evidence_refs_json text not null,
        validation_json text not null,
        admitted_at text not null,
        foreign key (task_id) references task_lifecycle(task_id)
    );
    create unique index if not exists idx_task_structured_results_task
        on task_structured_results(task_id);").map_err(db_error)?;
    Ok(())
}
fn ensure_native_auxiliary_schema(c: &Connection) -> Result<(), String> {
    c.execute_batch(
        "create table if not exists native_task_operations (
            operation_key text primary key,
            operation_kind text not null,
            request_digest text not null,
            result_json text not null,
            created_at text not null
        );
        create table if not exists task_lifecycle_events (
            event_id text primary key,
            task_id text,
            task_number integer,
            event_type text not null,
            payload_json text not null,
            created_at text not null
        );
        create table if not exists task_chapter_memberships (
            chapter_id text not null,
            task_number integer not null,
            order_index integer not null,
            note text,
            actor_agent_id text,
            updated_at text not null,
            primary key (chapter_id, task_number)
        );
        create table if not exists recurring_task_definitions (
            recurrence_id text primary key,
            status text not null,
            definition_json text not null,
            last_due_key text,
            last_auto_triggered_at text,
            updated_at text not null
        );
        create table if not exists recurring_task_events (
            event_id text primary key,
            recurrence_id text not null,
            event_type text not null,
            actor_agent_id text not null,
            authority_basis_json text not null,
            event_json text not null,
            created_at text not null
        );
        create table if not exists recurring_task_runs (
            run_id text primary key,
            recurrence_id text not null,
            task_id text,
            task_number integer,
            due_key text,
            trigger_mode text not null,
            reason text not null,
            created_at text not null,
            run_json text not null
        );
        create index if not exists idx_recurring_task_definitions_status
            on recurring_task_definitions(status);
        create index if not exists idx_recurring_task_runs_recurrence
            on recurring_task_runs(recurrence_id, created_at desc);
        create table if not exists recurring_task_run_claims (
            recurrence_id text not null,
            due_key text not null,
            run_id text not null,
            claimed_at text not null,
            primary key(recurrence_id, due_key)
        );",
    )
    .map_err(db_error)?;
    migrate_legacy_recurring_schema(c)
}
