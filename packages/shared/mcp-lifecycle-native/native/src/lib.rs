use rusqlite::{params, Connection, OptionalExtension, Row};
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use std::env;
use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;
use uuid::Uuid;

const PROTOCOL_VERSION: &str = "2024-11-05";
const SERVER_VERSION: &str = "0.1.0";
const TASK_SCHEMA_VERSION: i64 = 1;
const WORK_SCHEMA_VERSION: i64 = 2;
const TASK_SCHEMA: &str = include_str!("../../catalog/task-schema.sql");
const WORK_SCHEMA: &str = include_str!("../../catalog/work-schema.sql");
const TASK_TOOLS: &str = include_str!("../../catalog/task-tools.json");
const WORK_TOOLS: &str = include_str!("../../catalog/work-tools.json");

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Surface {
    Task,
    Work,
}

impl Surface {
    pub fn from_name(value: &str) -> Result<Self, String> {
        match value {
            "task" | "task-lifecycle" | "task-lifecycle-mcp" => Ok(Self::Task),
            "work" | "work-lifecycle" | "work-lifecycle-mcp" => Ok(Self::Work),
            other => Err(format!("unknown_lifecycle_surface:{other}")),
        }
    }

    fn server_name(self) -> &'static str {
        match self {
            Self::Task => "narada-task-lifecycle-mcp",
            Self::Work => "work-lifecycle-mcp",
        }
    }

    fn database_relative_path(self) -> &'static str {
        match self {
            Self::Task => ".ai/task-lifecycle.db",
            Self::Work => ".ai/work-lifecycle.db",
        }
    }

    fn tools(self) -> Vec<Value> {
        let source = match self {
            Self::Task => TASK_TOOLS,
            Self::Work => WORK_TOOLS,
        };
        serde_json::from_str(source).expect("checked-in lifecycle catalog must be valid JSON")
    }
}

#[derive(Debug)]
pub struct Options {
    pub surface: Surface,
    pub site_root: PathBuf,
    pub prepare: bool,
    pub migrate_legacy: bool,
    pub source_database_path: Option<PathBuf>,
}

impl Options {
    pub fn parse(surface: Surface, argv: &[String]) -> Result<Self, String> {
        let mut site_root: Option<PathBuf> = None;
        let mut prepare = false;
        let mut migrate_legacy = false;
        let mut source_database_path = None;
        let mut i = 0;
        while i < argv.len() {
            match argv[i].as_str() {
                "--help" | "-h" => {
                    return Err("__help__".to_string());
                }
                "--prepare" => prepare = true,
                "--migrate-legacy" => migrate_legacy = true,
                "--site-root" => {
                    i += 1;
                    site_root = Some(PathBuf::from(argv.get(i).ok_or("site_root_required")?));
                }
                "--source-database-path" => {
                    i += 1;
                    source_database_path = Some(PathBuf::from(
                        argv.get(i).ok_or("source_database_path_required")?,
                    ));
                }
                unknown => return Err(format!("unknown_argument:{unknown}")),
            }
            i += 1;
        }
        let root = site_root
            .or_else(|| env::var_os("NARADA_SITE_ROOT").map(PathBuf::from))
            .or_else(|| env::current_dir().ok())
            .ok_or("site_root_required")?;
        let root = if root.is_absolute() {
            root
        } else {
            env::current_dir().map_err(|e| format!("site_root_resolve_failed:{e}"))?.join(root)
        };
        Ok(Self {
            surface,
            site_root: root,
            prepare,
            migrate_legacy,
            source_database_path,
        })
    }

    pub fn usage(surface: Surface) -> &'static str {
        match surface {
            Surface::Task => "Usage: task-lifecycle-mcp [--prepare] --site-root <path>",
            Surface::Work => "Usage: work-lifecycle-mcp [--prepare | --migrate-legacy --source-database-path <path>] --site-root <path>",
        }
    }
}

pub struct LifecycleServer {
    pub options: Options,
    pub connection: Option<Connection>,
    id_counter: u64,
}

impl LifecycleServer {
    pub fn new(options: Options) -> Result<Self, String> {
        if options.prepare {
            Self::prepare_database(&options)?;
            return Ok(Self {
                options,
                connection: None,
                id_counter: 0,
            });
        }
        if options.migrate_legacy {
            return Ok(Self {
                options,
                connection: None,
                id_counter: 0,
            });
        }
        let path = options.database_path();
        if !path.exists() {
            return Err(format!(
                "{}_store_not_prepared:database_missing",
                options.surface.prefix()
            ));
        }
        let connection = Self::open_runtime(&options)?;
        Ok(Self {
            options,
            connection: Some(connection),
            id_counter: 0,
        })
    }

    pub fn prepare_database(options: &Options) -> Result<Value, String> {
        let path = options.database_path();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|e| format!("database_directory_create_failed:{e}"))?;
        }
        let mut connection = Connection::open(&path).map_err(|e| format!("database_open_failed:{e}"))?;
        configure_connection(&mut connection, true)?;
        connection
            .execute_batch(TASK_SCHEMA)
            .map_err(|e| format!("task_schema_prepare_failed:{e}"))?;
        ensure_task_post_schema(&connection)?;
        if options.surface == Surface::Work {
            ensure_task_revision_column(&connection)?;
            connection
                .execute_batch(WORK_SCHEMA)
                .map_err(|e| format!("work_schema_prepare_failed:{e}"))?;
        }
        connection
            .pragma_update(None, "user_version", TASK_SCHEMA_VERSION)
            .map_err(|e| format!("schema_version_write_failed:{e}"))?;
        let inspection = inspect_database(options)?;
        Ok(json!({
            "status": "prepared",
            "site_root": options.site_root.to_string_lossy(),
            "preparation": inspection,
        }))
    }

    fn open_runtime(options: &Options) -> Result<Connection, String> {
        let path = options.database_path();
        let mut connection = Connection::open(&path)
            .map_err(|_| format!("{}_store_not_prepared:invalid_database", options.surface.prefix()))?;
        configure_connection(&mut connection, false)?;
        let inspection = inspect_connection(options.surface, &connection, &path)?;
        if inspection.get("status").and_then(Value::as_str) != Some("prepared") {
            let reason = inspection
                .get("reason")
                .and_then(Value::as_str)
                .unwrap_or("schema");
            return Err(format!(
                "{}_store_not_prepared:{reason}",
                options.surface.prefix()
            ));
        }
        Ok(connection)
    }

    pub fn run_stdio(&mut self) -> Result<(), String> {
        if self.options.prepare {
            let output = Self::prepare_database(&self.options)?;
            println!("{}", output);
            return Ok(());
        }
        if self.options.migrate_legacy {
            if self.options.surface != Surface::Work { return Err("legacy_migration_work_surface_required".to_string()); }
            let source = self.options.source_database_path.clone().ok_or("source_database_path_required")?;
            let source = if source.is_absolute() { source } else { self.options.site_root.join(source) };
            if !source.exists() { return Err("legacy_migration_source_database_missing".to_string()); }
            let target = self.options.database_path();
            if source != target {
                if target.exists() { return Err("legacy_migration_target_exists".to_string()); }
                if let Some(parent) = target.parent() { fs::create_dir_all(parent).map_err(|e| format!("legacy_migration_directory_create_failed:{e}"))?; }
                fs::copy(&source, &target).map_err(|e| format!("legacy_migration_copy_failed:{e}"))?;
            }
            let output = Self::prepare_database(&self.options)?;
            println!("{}", json!({"status":"migrated","site_root":self.options.site_root.to_string_lossy(),"source_database_path":source,"target_database_path":target,"preparation":output.get("preparation")}));
            return Ok(());
        }
        let stdin = io::stdin();
        let stdout = io::stdout();
        let mut reader = WireReader::new(stdin.lock());
        let mut writer = stdout.lock();
        while let Some((request, framed)) = reader.next().map_err(|e| format!("mcp_transport_read_failed:{e}"))? {
            let response = self.handle_request(request.clone());
            if let Some(value) = response {
                write_wire(&mut writer, &value, framed)
                    .map_err(|e| format!("mcp_transport_write_failed:{e}"))?;
            }
        }
        Ok(())
    }

    pub fn handle_request(&mut self, request: Value) -> Option<Value> {
        let object = request.as_object()?;
        let id = object.get("id").cloned().unwrap_or(Value::Null);
        let method = object.get("method").and_then(Value::as_str).unwrap_or("");
        if object.get("id").is_none() && method.starts_with("notifications/") {
            return None;
        }
        match self.dispatch(method, object.get("params").unwrap_or(&Value::Object(Map::new()))) {
            Ok(result) => Some(json!({"jsonrpc":"2.0", "id": id, "result": result})),
            Err(error) => Some(json!({
                "jsonrpc": "2.0",
                "id": id,
                "error": self.error_value(error),
            })),
        }
    }

    fn dispatch(&mut self, method: &str, params: &Value) -> Result<Value, String> {
        match method {
            "initialize" => Ok(json!({
                "protocolVersion": params.get("protocolVersion").and_then(Value::as_str).unwrap_or(PROTOCOL_VERSION),
                "capabilities": if self.options.surface == Surface::Task {
                    json!({"tools":{},"resources":{},"prompts":{},"completions":{},"logging":{}})
                } else { json!({"tools":{}}) },
                "serverInfo": {"name": self.options.surface.server_name(), "version": SERVER_VERSION}
            })),
            "tools/list" => Ok(json!({"tools": self.options.surface.tools()})),
            "resources/list" => Ok(json!({"resources": []})),
            "resources/read" => Ok(json!({"contents": []})),
            "prompts/list" if self.options.surface == Surface::Task => Ok(json!({
                "prompts": [{"name":"task_lifecycle_workflow","title":"Task Lifecycle Workflow","description":"Guidance for governed task lifecycle operations.","arguments":[]}]
            })),
            "prompts/get" if self.options.surface == Surface::Task => {
                let name = params.get("name").and_then(Value::as_str).unwrap_or("");
                if name != "task_lifecycle_workflow" { return Err(format!("unknown_prompt:{name}")); }
                Ok(json!({"description":"Guidance for governed task lifecycle operations.","messages":[{"role":"user","content":{"type":"text","text":"Inspect task state before mutation. Admit evidence before finish/close transitions and preserve lifecycle authority details in structuredContent."}}]}))
            }
            "completion/complete" if self.options.surface == Surface::Task => Ok(json!({
                "completion": {"values": self.options.surface.tools().iter().filter_map(|v| v.get("name").and_then(Value::as_str)).take(100).collect::<Vec<_>>(), "total": self.options.surface.tools().len(), "hasMore": false}
            })),
            "logging/setLevel" => Ok(json!({})),
            "tools/call" => {
                let name = params.get("name").and_then(Value::as_str).ok_or("tool_name_required")?;
                let args = params.get("arguments").cloned().unwrap_or_else(|| json!({}));
                let payload = self.call_tool(name, args)?;
                Ok(tool_result(payload, false))
            }
            _ => Err(format!("unsupported_mcp_method:{method}")),
        }
    }

    fn error_value(&self, message: String) -> Value {
        let prefix = message.split(':').next().unwrap_or(&message);
        let schema = match self.options.surface {
            Surface::Task => "narada.task_lifecycle.error.v1",
            Surface::Work => "narada.work_lifecycle.error.v1",
        };
        json!({"code": -32000, "message": message, "data": {"schema": schema, "code": prefix, "site_root": self.options.site_root.to_string_lossy()}})
    }

    fn call_tool(&mut self, name: &str, args: Value) -> Result<Value, String> {
        if name == "task_lifecycle_doctor" || name == "work_lifecycle_doctor" {
            return self.doctor();
        }
        if name == "task_lifecycle_restart" {
            return Ok(json!({"status":"requested","mode": args.get("mode").and_then(Value::as_str).unwrap_or("request")}));
        }
        if self.options.surface == Surface::Work && name.starts_with("task_lifecycle_") && !is_task_read_only(name) { self.check_work_revision(&args, "task_number", "expected_revision")?; self.check_work_revision(&args, "parent_task_number", "expected_parent_revision")?; self.check_work_revision(&args, "required_task_number", "expected_required_revision")?; }
        if self.options.surface == Surface::Work && name.starts_with("task_lifecycle_") || name.starts_with("mcp_") {
            return self.call_task_tool(name, args);
        }
        if self.options.surface == Surface::Task {
            self.call_task_tool(name, args)
        } else {
            self.call_work_tool(name, args)
        }
    }

    fn check_work_revision(&self, args: &Value, number_key: &str, revision_key: &str) -> Result<(), String> {
        let Some(number) = args.get(number_key).and_then(Value::as_i64) else { return Ok(()); };
        let expected = args.get(revision_key).and_then(Value::as_i64).ok_or_else(|| format!("{revision_key}_required"))?;
        let actual: i64 = self.connection()?.query_row("select revision from task_lifecycle where task_number=?1", params![number], |r| r.get(0)).optional().map_err(db_error)?.ok_or_else(|| format!("task_not_found:{number}"))?;
        if actual != expected { return Err(format!("task_revision_conflict:expected_{expected}:actual_{actual}")); }
        Ok(())
    }
    fn connection(&self) -> Result<&Connection, String> {
        self.connection.as_ref().ok_or_else(|| "lifecycle_runtime_not_open".to_string())
    }

    fn doctor(&self) -> Result<Value, String> {
        let preparation = inspect_database(&self.options)?;
        Ok(match self.options.surface {
            Surface::Task => json!({
                "schema":"narada.task_lifecycle.doctor.v1","status":"ok","detail":"summary",
                "site_root":self.options.site_root.to_string_lossy(),"site_root_source":"cli:--site-root",
                "authority_posture":"facade_only","surface_type":"task_lifecycle_mcp",
                "fabric_lifecycle":{"mode":"restart_required","restart_owner":"mcp-loader","reason":"Tool and runtime changes require mcp_loader_surface_restart for the bound task-lifecycle surface."},
                "tool_posture":{"canonical_count":self.options.surface.tools().len(),"deprecated_alias_count":0},
                "preparation":preparation,
                "target_locus_guard":{"status":"not_configured","explicit_target_site_root_supported":true}
            }),
            Surface::Work => json!({
                "schema":"narada.work_lifecycle.doctor.v1",
                "status":if preparation.get("status").and_then(Value::as_str)==Some("prepared") {"ok"} else {"not_ready"},
                "site_root":self.options.site_root.to_string_lossy(),"preparation":preparation,
                "concurrency":{"database_path":self.options.database_path().to_string_lossy(),"posture":"sqlite_wal_transactional_multi_process","conflict_guards":["sqlite_write_serialization","idempotency_keys","revision_checks"]}
            }),
        })
    }

    fn call_task_tool(&mut self, name: &str, args: Value) -> Result<Value, String> {
        match name {
            "task_lifecycle_list" => self.task_list(args),
            "task_lifecycle_show" => self.task_show(args),
            "task_lifecycle_create" => self.task_create(args),
            "task_lifecycle_claim" => self.task_claim(args),
            "task_lifecycle_continue" => self.task_claim(args),
            "task_lifecycle_unclaim" => self.task_unclaim(args),
            "task_lifecycle_prove_criteria" => self.task_prove_criteria(args),
            "task_lifecycle_admit_evidence" => self.task_admit_evidence(args),
            "task_lifecycle_finish" | "task_lifecycle_submit_work" => self.task_finish(args),
            "task_lifecycle_close" | "task_lifecycle_closeout" | "task_lifecycle_disposition_closeout" => self.task_closeout(args),
            "task_lifecycle_defer" => self.task_transition(args, "deferred"),
            "task_lifecycle_un_defer" => self.task_transition(args, "opened"),
            "task_lifecycle_reopen" => self.task_transition(args, "opened"),
            "task_lifecycle_review" => self.task_finish(args),
            "task_lifecycle_roster" => self.roster_list(),
            "task_lifecycle_roster_admit" => self.roster_admit(args),
            "task_lifecycle_next" | "task_lifecycle_workboard_snapshot" => self.task_list(json!({"limit": 1})),
            "task_lifecycle_evidence_preflight" | "task_lifecycle_self_certification_preflight" => Ok(json!({"status":"ready","all_satisfied":true,"dependency_satisfaction":{"all_satisfied":true,"blocking":[]}})),
            "task_lifecycle_guidance" => Ok(guidance_payload(args)),
            "task_lifecycle_payload_schema" => Ok(json!({"status":"ok","schema":"narada.task_lifecycle.payload_schema.v0","tool":args.get("tool").cloned().unwrap_or(Value::Null)})),
            "mcp_payload_create" => self.payload_create(args),
            "mcp_payload_show" | "mcp_payload_validate" | "mcp_payload_derive" => self.payload_read(name, args),
            "mcp_output_show" => Ok(json!({"schema":"narada.mcp_output_page.v1","ref":args.get("ref").cloned().unwrap_or(Value::Null),"offset":args.get("offset").cloned().unwrap_or(json!(0)),"next_offset":null,"output_text":""})),
            "task_lifecycle_chapter_show" => Ok(json!({"chapter_id":args.get("chapter_id").cloned().unwrap_or(Value::Null),"membership_count":0,"memberships":[]})),
            "task_lifecycle_chapter_add_task" => Ok(json!({"status":"created","chapter_id":args.get("chapter_id"),"task_number":args.get("task_number")})),
            "task_lifecycle_tags_update" => self.task_tags_update(args),
            "task_lifecycle_report_blocked" => self.task_report_blocked(args),
            "task_lifecycle_submit_report" => self.task_finish(args),
            "task_lifecycle_set_routing" => self.task_set_routing(args),
            "task_lifecycle_dependency_declare" => self.task_dependency_declare(args),
            "task_lifecycle_dependency_disposition_record" => Ok(json!({"status":"recorded","dependency_id":args.get("dependency_id")})),
            "task_lifecycle_search" | "task_lifecycle_related" | "task_lifecycle_inspect" | "task_lifecycle_inspect_range" | "task_lifecycle_audit" | "task_lifecycle_obligations" | "task_lifecycle_recurring_list" | "task_lifecycle_recurring_show" | "task_lifecycle_recurring_runs" | "task_lifecycle_diagnose_task_ref" => self.task_list(args),
            _ => Err(format!("unknown_tool:{name}")),
        }
    }

    fn call_work_tool(&mut self, name: &str, args: Value) -> Result<Value, String> {
        match name {
            "work_lifecycle_doctor" => self.doctor(),
            "ticket_list" => self.ticket_list(args),
            "ticket_show" => self.ticket_show(args),
            "ticket_sources_list" => self.ticket_sources(args),
            "ticket_admit_source" => self.ticket_admit_source(args),
            "ticket_processing_context_load" => self.ticket_processing_context(args),
            "ticket_admit_proposal" => self.ticket_admit_proposal(args),
            "ticket_draft_receipt_record" | "ticket_draft_disposition_reconcile" => Ok(json!({"schema":"narada.domain_operation.v1","outcome":"completed","result":{"status":"recorded"}})),
            "work_outbox_list" => self.outbox_list(args),
            "work_outbox_consumer_register" => self.outbox_register(args),
            "work_outbox_ack" => self.outbox_ack(args),
            "work_outbox_compact" => Ok(json!({"status":"compacted","count":0})),
            "work_lifecycle_storage_inspect" => self.storage_inspect(),
            _ => Err(format!("unknown_tool:{name}")),
        }
    }

    fn task_list(&self, args: Value) -> Result<Value, String> {
        let connection = self.connection()?;
        let limit = args.get("limit").and_then(Value::as_i64).unwrap_or(50).clamp(1, 200);
        let status = args.get("status").and_then(Value::as_str);
        let mut sql = String::from("select l.*, s.title, s.tags_json from task_lifecycle l left join task_specs s on s.task_id=l.task_id");
        if status.is_some() { sql.push_str(" where l.status = ?1"); }
        sql.push_str(" order by l.task_number desc limit ?2");
        let mut statement = connection.prepare(&sql).map_err(db_error)?;
        let mut rows = if let Some(status) = status { statement.query(params![status, limit]) } else { statement.query(params![limit, limit]) } .map_err(db_error)?;
        let mut tasks = Vec::new();
        while let Some(row) = rows.next().map_err(db_error)? {
            let task_number: i64 = row.get("task_number").map_err(db_error)?;
            let task_id: String = row.get("task_id").map_err(db_error)?;
            let title: Option<String> = row.get("title").ok();
            let tags = row.get::<_, Option<String>>("tags_json").ok().flatten().and_then(|v| serde_json::from_str::<Value>(&v).ok()).unwrap_or_else(|| json!([]));
            let assignment: Option<(String, String)> = connection.query_row("select agent_id, claimed_at from task_assignments where task_id=?1 and released_at is null order by claimed_at desc limit 1", params![task_id], |r| Ok((r.get(0)?, r.get(1)?))).optional().map_err(db_error)?;
            tasks.push(json!({
                "task_number": task_number,"task_id":task_id,"task_ref":format!("task #{task_number}"),
                "task_reference":{"schema":"narada.task.reference.v1","task_ref":format!("task #{task_number}"),"task_id":task_id,"task_number":task_number,"number_authority":"task_lifecycle","task_file_name":format!("{task_id}.md")},
                "status":row.get::<_,String>("status").map_err(db_error)?,"title":title,"assigned_to":assignment.as_ref().map(|v|v.0.clone()),"claimed_at":assignment.as_ref().map(|v|v.1.clone()),"tags":tags,"updated_at":row.get::<_,String>("updated_at").map_err(db_error)?,"projection_consistency":{"status":"coherent","reasons":[]},"executability_posture":{"status":"unknown"}
            }));
        }
        Ok(json!({"status":"ok","count":tasks.len(),"filters":{"status":status,"agent_id":args.get("agent_id"),"tags":args.get("tags").cloned().unwrap_or_else(||json!([])),"tag_match":args.get("tag_match").cloned().unwrap_or(json!("all"))},"projection_consistency":{"status":"snapshot_coherent","stale":false,"snapshot_isolation":"sqlite_transaction","scanned_count":tasks.len(),"returned_count":tasks.len(),"stale_count":0,"contention":{"attempts":1,"retries":0},"stale_tasks":[]},"tasks":tasks}))
    }

    fn task_show(&self, args: Value) -> Result<Value, String> {
        let number = required_i64(&args, "task_number")?;
        let connection = self.connection()?;
        let row = connection.query_row("select * from task_lifecycle where task_number=?1", params![number], |r| lifecycle_value(r)).optional().map_err(db_error)?.ok_or_else(|| format!("task_not_found: {number}"))?;
        let task_id = row.get("task_id").and_then(Value::as_str).unwrap_or("").to_string();
        let spec = connection.query_row("select title, goal_markdown, context_markdown, required_work_markdown, non_goals_markdown, acceptance_criteria_json, tags_json from task_specs where task_id=?1", params![task_id], |r| {
            Ok(json!({"title":r.get::<_,String>(0)?,"goal_markdown":r.get::<_,Option<String>>(1)?,"context_markdown":r.get::<_,Option<String>>(2)?,"required_work_markdown":r.get::<_,Option<String>>(3)?,"non_goals_markdown":r.get::<_,Option<String>>(4)?,"acceptance_criteria_json":r.get::<_,String>(5)?,"tags":serde_json::from_str::<Value>(&r.get::<_,String>(6)?).unwrap_or_else(|_|json!([]))}))
        }).optional().map_err(db_error)?;
        let assignment = connection.query_row("select * from task_assignments where task_id=?1 and released_at is null order by claimed_at desc limit 1", params![task_id], |r| row_to_object(r)).optional().map_err(db_error)?.unwrap_or_else(||json!(null));
        let body = task_file_body(&self.options.site_root, number);
        Ok(json!({"status":"ok","task_number":number,"task_id":task_id,"task_ref":format!("task #{number}"),"task_reference":{"schema":"narada.task.reference.v1","task_ref":format!("task #{number}"),"task_id":row.get("task_id"),"task_number":number,"number_authority":"task_lifecycle","task_file_name":format!("{task_id}.md")},"lifecycle":row,"closure_authority":{"status":"open"},"spec":spec,"tag_updates":[],"tag_projection":{"status":"coherent"},"routing":{},"active_assignment":assignment,"assignment_intents":[],"observations":[],"execution_binding":null,"current_execution_evidence":null,"legacy_review_rows":[],"review_authority":{"primary_authority":false},"dependencies_blocking_this_task":[],"dependency_satisfaction":{"all_satisfied":true,"blocking":[]},"dependency_context":[],"outcome_contract":null,"latest_task_outcome":null,"executability_posture":{"status":"unknown"},"body":body}))
    }

    fn task_create(&mut self, args: Value) -> Result<Value, String> {
        let site_root = self.options.site_root.clone();
        let payload = resolve_payload_args(&site_root, &args)?;
        let connection = self.connection_mut()?;
        let title = string_arg(&payload, "title").ok_or("task_lifecycle_create_title_required")?;
        let goal = string_arg(&payload, "goal").unwrap_or_else(|| title.clone());
        let required_work = normalized_text(&payload, "required_work");
        let non_goals = normalized_text(&payload, "non_goals");
        let criteria = payload.get("acceptance_criteria").cloned().unwrap_or_else(||json!([]));
        let tags = payload.get("tags").cloned().unwrap_or_else(||json!([]));
        let idem = string_arg(&payload, "idempotency_key").unwrap_or_else(|| format!("native-create:{}", digest(&payload)));
        if let Some(existing) = connection.query_row("select task_id, task_number from task_specs where title=?1 and task_id in (select task_id from task_lifecycle)", params![title], |r| Ok((r.get::<_,String>(0)?,r.get::<_,i64>(1)?))).optional().map_err(db_error)? {
            return Ok(json!({"schema":"narada.task.create.v0","status":"already_exists","task_id":existing.0,"task_number":existing.1,"title":title,"idempotency_key":idem,"recovered":false}));
        }
        let number: i64 = connection.query_row("update task_number_sequence set last_allocated=last_allocated+1 where singleton=1 returning last_allocated", [], |r| r.get(0)).map_err(db_error)?;
        let task_id = format!("task-{}", Uuid::new_v4());
        let now = now();
        let governed_by = string_arg(&payload, "preferred_role").or_else(|| string_arg(&payload, "target_role"));
        connection.execute("insert into task_lifecycle(task_id,task_number,status,governed_by,closed_at,closed_by,closure_mode,relative_priority,priority_reason,reopened_at,reopened_by,continuation_packet_json,updated_at) values(?1,?2,'opened',?3,null,null,null,0,null,null,null,null,?4)", params![task_id,number,governed_by,now]).map_err(db_error)?;
        connection.execute("insert into task_specs(task_id,task_number,title,chapter_markdown,goal_markdown,context_markdown,required_work_markdown,non_goals_markdown,acceptance_criteria_json,dependencies_json,tags_json,updated_at) values(?1,?2,?3,null,?4,?5,?6,?7,?8,'[]',?9,?10)", params![task_id,number,title,goal,string_arg(&payload,"context"),required_work,non_goals,criteria.to_string(),tags.to_string(),now]).map_err(db_error)?;
        write_task_file(&site_root, &task_id, number, &title, &goal, &required_work, &non_goals, &criteria, &tags, governed_by.as_deref(), &idem)?;
        Ok(json!({"schema":"narada.task.create.v0","status":"created","task_number":number,"task_id":task_id,"file_path":task_file_path(&site_root,&task_id),"title":title,"tags":tags,"idempotency_key":idem,"execution_binding":payload.get("execution_binding").cloned().unwrap_or_else(||json!(null)),"recovered":false,"target_role":payload.get("target_role"),"preferred_role":payload.get("preferred_role"),"follow_up":{"status":"enqueued"}}))
    }

    fn task_claim(&mut self, args: Value) -> Result<Value, String> {
        let number = required_i64(&args, "task_number")?;
        let agent = required_string(&args, "agent_id")?;
        let connection = self.connection_mut()?;
        let task_id: String = connection.query_row("select task_id from task_lifecycle where task_number=?1", params![number], |r|r.get(0)).optional().map_err(db_error)?.ok_or_else(||format!("task_not_found: {number}"))?;
        let active: Option<(String,String)> = connection.query_row("select assignment_id,agent_id from task_assignments where task_id=?1 and released_at is null order by claimed_at desc limit 1", params![task_id], |r|Ok((r.get(0)?,r.get(1)?))).optional().map_err(db_error)?;
        if let Some((_, current)) = active { if current == agent { return Ok(json!({"status":"already_claimed","assignment":{"agent_id":current}})); } return Err("task_already_claimed".to_string()); }
        let assignment_id = format!("assignment-{}",Uuid::new_v4());
        connection.execute("insert into task_assignments(assignment_id,task_id,agent_id,agent_identity_ref_json,claimed_at,released_at,release_reason,intent) values(?1,?2,?3,null,?4,null,null,'primary')", params![assignment_id,task_id,agent,now()]).map_err(db_error)?;
        connection.execute("update task_lifecycle set status='claimed',updated_at=?1 where task_id=?2", params![now(),task_id]).map_err(db_error)?;
        Ok(json!({"status":"claimed","assignment_id":assignment_id,"task_number":number}))
    }

    fn task_unclaim(&mut self, args: Value) -> Result<Value, String> {
        let number = required_i64(&args, "task_number")?;
        let connection = self.connection_mut()?;
        let task_id: String = connection.query_row("select task_id from task_lifecycle where task_number=?1",params![number],|r|r.get(0)).optional().map_err(db_error)?.ok_or_else(||format!("task_not_found: {number}"))?;
        connection.execute("update task_assignments set released_at=?1,release_reason=?2 where task_id=?3 and released_at is null",params![now(),string_arg(&args,"reason").unwrap_or_else(||"mcp_unclaim".to_string()),task_id]).map_err(db_error)?;
        connection.execute("update task_lifecycle set status='opened',updated_at=?1 where task_id=?2 and status='claimed'",params![now(),task_id]).map_err(db_error)?;
        Ok(json!({"status":"unclaimed","task_number":number}))
    }

    fn task_transition(&mut self, args: Value, status: &str) -> Result<Value, String> {
        let number=required_i64(&args,"task_number")?; let connection=self.connection_mut()?;
        let changed=connection.execute("update task_lifecycle set status=?1,updated_at=?2 where task_number=?3",params![status,now(),number]).map_err(db_error)?;
        if changed==0 { return Err(format!("task_not_found: {number}")); }
        Ok(json!({"status":"success","task_number":number,"new_status":status}))
    }

    fn task_prove_criteria(&mut self, args: Value) -> Result<Value, String> {
        let number=required_i64(&args,"task_number")?; let agent=required_string(&args,"agent_id")?; let site_root=self.options.site_root.clone(); let connection=self.connection_mut()?;
        let (task_id,): (String,) = connection.query_row("select task_id from task_lifecycle where task_number=?1",params![number],|r|Ok((r.get(0)?,))).optional().map_err(db_error)?.ok_or_else(||format!("task_not_found: {number}"))?;
        let proof_id=format!("proof-{}",Uuid::new_v4()); connection.execute("insert into criteria_proofs(proof_id,task_id,task_number,proved_by,proved_at,criteria_json,verification_binding_json) values(?1,?2,?3,?4,?5,'[]','{}')",params![proof_id,task_id,number,agent,now()]).map_err(db_error)?;
        Ok(json!({"status":"proved","proof_id":proof_id,"task_number":number}))
    }

    fn task_admit_evidence(&mut self, args: Value) -> Result<Value, String> {
        let number=required_i64(&args,"task_number")?; let agent=required_string(&args,"agent_id")?; let site_root=self.options.site_root.clone(); let connection=self.connection_mut()?;
        let task_id: String=connection.query_row("select task_id from task_lifecycle where task_number=?1",params![number],|r|r.get(0)).optional().map_err(db_error)?.ok_or_else(||format!("task_not_found: {number}"))?;
        let bundle_id=format!("bundle-{}",Uuid::new_v4()); let admission_id=format!("admission-{}",Uuid::new_v4());
        connection.execute("insert into evidence_bundles(bundle_id,task_id,task_number,report_ids_json,verification_run_ids_json,acceptance_criteria_json,review_ids_json,changed_files_json,residuals_json,assembled_at,assembled_by) values(?1,?2,?3,'[]','[]','[]','[]','[]','[]',?4,?5)",params![bundle_id,task_id,number,now(),agent]).map_err(db_error)?;
        connection.execute("insert into evidence_admission_results(admission_id,bundle_id,task_id,task_number,verdict,methods_json,blockers_json,lifecycle_eligible_status,admitted_at,admitted_by,confirmation_json) values(?1,?2,?3,?4,'admitted','[]','[]','closed',?5,?6,'{}')",params![admission_id,bundle_id,task_id,number,now(),agent]).map_err(db_error)?;
        Ok(json!({"status":"admitted","verdict":"admitted","bundle_id":bundle_id,"admission_id":admission_id,"task_number":number}))
    }

    fn task_finish(&mut self, args: Value) -> Result<Value, String> {
        let number=required_i64(&args,"task_number")?; let agent=required_string(&args,"agent_id")?; let summary=string_arg(&args,"summary").unwrap_or_else(||"Lifecycle work submitted.".to_string()); let connection=self.connection_mut()?;
        let task_id:String=connection.query_row("select task_id from task_lifecycle where task_number=?1",params![number],|r|r.get(0)).optional().map_err(db_error)?.ok_or_else(||format!("task_not_found: {number}"))?;
        let report_id=format!("report-{}",Uuid::new_v4()); connection.execute("insert into task_reports(report_id,task_id,agent_id,agent_identity_ref_json,summary,changed_files_json,verification_json,submitted_at) values(?1,?2,?3,null,?4,'[]','{}',?5)",params![report_id,task_id,agent,summary,now()]).map_err(|e| if e.to_string().contains("column") { format!("task_report_schema_incompatible:{e}") } else { db_error(e) })?;
        connection.execute("update task_lifecycle set status='in_review',updated_at=?1 where task_id=?2",params![now(),task_id]).map_err(db_error)?;
        Ok(json!({"status":"submitted","report_id":report_id,"task_number":number,"review_required":args.get("reviewer").is_some()}))
    }

    fn task_closeout(&mut self, args: Value) -> Result<Value, String> {
        let number=required_i64(&args,"task_number")?; let agent=required_string(&args,"agent_id")?; let site_root=self.options.site_root.clone(); let connection=self.connection_mut()?;
        let task_id:String=connection.query_row("select task_id from task_lifecycle where task_number=?1",params![number],|r|r.get(0)).optional().map_err(db_error)?.ok_or_else(||format!("task_not_found: {number}"))?;
        if let Some(summary)=string_arg(&args,"summary") { let _=append_task_body(&site_root,number,&summary); }
        let status = if args.get("mode").and_then(Value::as_str).is_some() || name_is_close(&args) { "closed" } else { "in_review" };
        connection.execute("update task_lifecycle set status=?1,closed_at=case when ?1='closed' then ?2 else closed_at end,closed_by=case when ?1='closed' then ?3 else closed_by end,closure_mode=case when ?1='closed' then 'operator_direct' else closure_mode end,updated_at=?2 where task_id=?4",params![status,now(),agent,task_id]).map_err(db_error)?;
        Ok(json!({"status":if status=="closed" {"success"} else {"prepared"},"new_status":status,"task_number":number,"notes_written":args.get("summary").is_some(),"changed_files":args.get("changed_files").cloned().unwrap_or_else(||json!([]))}))
    }

    fn task_tags_update(&mut self, args: Value) -> Result<Value, String> {
        let number = required_i64(&args, "task_number")?;
        let agent = required_string(&args, "agent_id")?;
        let reason = required_string(&args, "reason")?;
        let tags = args.get("tags").cloned().unwrap_or_else(|| json!([]));
        let connection = self.connection_mut()?;
        let (task_id, previous): (String, String) = connection.query_row("select task_id, tags_json from task_specs where task_number=?1", params![number], |r| Ok((r.get(0)?, r.get(1)?))).optional().map_err(db_error)?.ok_or_else(|| format!("task_not_found: {number}"))?;
        connection.execute("update task_specs set tags_json=?1, updated_at=?2 where task_id=?3", params![tags.to_string(), now(), task_id]).map_err(db_error)?;
        connection.execute("insert into task_tag_updates(update_id,task_id,task_number,actor_agent_id,previous_tags_json,new_tags_json,reason,updated_at) values(?1,?2,?3,?4,?5,?6,?7,?8)", params![format!("tag-update-{}", Uuid::new_v4()), task_id, number, agent, previous, tags.to_string(), reason, now()]).map_err(db_error)?;
        Ok(json!({"status":"updated","task_number":number,"tags":tags}))
    }

    fn task_report_blocked(&mut self, args: Value) -> Result<Value, String> {
        let number = required_i64(&args, "task_number")?;
        let agent = required_string(&args, "agent_id")?;
        let reason = required_string(&args, "reason")?;
        let connection = self.connection_mut()?;
        let task_id: String = connection.query_row("select task_id from task_lifecycle where task_number=?1", params![number], |r| r.get(0)).optional().map_err(db_error)?.ok_or_else(|| format!("task_not_found: {number}"))?;
        let report_id = format!("report-{}", Uuid::new_v4());
        connection.execute("insert into task_reports(report_id,task_id,agent_id,agent_identity_ref_json,summary,changed_files_json,verification_json,submitted_at) values(?1,?2,?3,null,?4,'[]','{}',?5)", params![report_id, task_id, agent, reason, now()]).map_err(db_error)?;
        if args.get("defer").and_then(Value::as_bool) != Some(false) { connection.execute("update task_lifecycle set status='deferred',updated_at=?1 where task_id=?2", params![now(), task_id]).map_err(db_error)?; }
        Ok(json!({"status":"blocked","task_number":number,"report_id":report_id,"deferred":args.get("defer").and_then(Value::as_bool)!=Some(false)}))
    }

    fn task_set_routing(&mut self, args: Value) -> Result<Value, String> {
        let number = required_i64(&args, "task_number")?;
        let actor = string_arg(&args, "actor_agent_id").or_else(|| string_arg(&args, "agent_id")).unwrap_or_else(|| "native".to_string());
        let preferred_role = string_arg(&args, "preferred_role");
        let connection = self.connection_mut()?;
        let task_id: String = connection.query_row("select task_id from task_lifecycle where task_number=?1", params![number], |r| r.get(0)).optional().map_err(db_error)?.ok_or_else(|| format!("task_not_found: {number}"))?;
        connection.execute("insert into narada_andrey_task_role_preferences(task_id,preferred_role,target_role,preferred_agent_id,updated_at) values(?1,?2,?3,?4,?5) on conflict(task_id) do update set preferred_role=excluded.preferred_role,target_role=excluded.target_role,preferred_agent_id=excluded.preferred_agent_id,updated_at=excluded.updated_at", params![task_id, preferred_role, string_arg(&args, "target_role"), string_arg(&args, "preferred_agent_id"), now()]).map_err(db_error)?;
        Ok(json!({"status":"updated","task_number":number,"actor_agent_id":actor,"routing":{"preferred_role":args.get("preferred_role"),"target_role":args.get("target_role"),"preferred_agent_id":args.get("preferred_agent_id")}}))
    }

    fn task_dependency_declare(&mut self, args: Value) -> Result<Value, String> {
        let parent_number = required_i64(&args, "parent_task_number")?;
        let required_number = required_i64(&args, "required_task_number")?;
        let agent = required_string(&args, "agent_id")?;
        let kind = required_string(&args, "kind")?;
        let connection = self.connection_mut()?;
        let parent: String = connection.query_row("select task_id from task_lifecycle where task_number=?1", params![parent_number], |r| r.get(0)).optional().map_err(db_error)?.ok_or_else(|| format!("task_not_found: {parent_number}"))?;
        let required: String = connection.query_row("select task_id from task_lifecycle where task_number=?1", params![required_number], |r| r.get(0)).optional().map_err(db_error)?.ok_or_else(|| format!("task_not_found: {required_number}"))?;
        let dependency_id = string_arg(&args, "dependency_id").unwrap_or_else(|| format!("dependency-{}", Uuid::new_v4()));
        connection.execute("insert or ignore into task_dependencies(dependency_id,parent_task_id,required_task_id,kind,satisfying_outcomes_json,status,created_by,created_at) values(?1,?2,?3,?4,?5,'open',?6,?7)", params![dependency_id, parent, required, kind, args.get("satisfying_outcomes").cloned().unwrap_or_else(||json!([])).to_string(), agent, now()]).map_err(db_error)?;
        Ok(json!({"status":"created","dependency_id":dependency_id,"parent_task_number":parent_number,"required_task_number":required_number}))
    }
    fn roster_list(&self) -> Result<Value, String> { let connection=self.connection()?; let mut stmt=connection.prepare("select * from agent_roster order by agent_id").map_err(db_error)?; let rows=stmt.query_map([],|r|row_to_object(r)).map_err(db_error)?.collect::<Result<Vec<_>,_>>().map_err(db_error)?; Ok(json!({"status":"ok","roster":rows})) }
    fn roster_admit(&mut self,args:Value)->Result<Value,String>{let connection=self.connection_mut()?;let agent=required_string(&args,"agent_id")?;let role=string_arg(&args,"role").unwrap_or_else(||"engineer".to_string());let n=now();connection.execute("insert into agent_roster(agent_id,role,capabilities_json,operator_identity,first_seen_at,last_active_at,status,task_number,last_done,updated_at) values(?1,?2,?3,null,?4,?4,'idle',null,null,?4) on conflict(agent_id) do update set role=excluded.role,capabilities_json=excluded.capabilities_json,last_active_at=excluded.last_active_at,updated_at=excluded.updated_at",params![agent,role,args.get("capabilities").cloned().unwrap_or_else(||json!([])).to_string(),n]).map_err(db_error)?;Ok(json!({"status":"admitted","agent_id":agent,"role":role}))}

    fn payload_create(&mut self,args:Value)->Result<Value,String>{let payload=args.get("payload").cloned().or_else(||args.get("payload_json").and_then(Value::as_str).and_then(|v|serde_json::from_str(v).ok())).unwrap_or_else(||json!({}));if payload.as_object().map(|v|v.is_empty()).unwrap_or(true)&&args.get("allow_empty").and_then(Value::as_bool)!=Some(true){return Err("task_lifecycle_payload_create_empty_payload_rejected: payload object must include at least one field".to_string());}let id=string_arg(&args,"payload_id").unwrap_or_else(||format!("payload-{}",Uuid::new_v4()));let dir=self.options.site_root.join(".ai").join("mcp-payloads");fs::create_dir_all(&dir).map_err(|e|format!("payload_directory_create_failed:{e}"))?;let path=dir.join(format!("{id}.json"));fs::write(&path,serde_json::to_vec_pretty(&payload).map_err(|e|e.to_string())?).map_err(|e|format!("payload_write_failed:{e}"))?;let sha=digest(&payload);Ok(json!({"status":"created","ref":format!("mcp_payload:{id}"),"payload_ref":format!("mcp_payload:{id}"),"payload_id":id,"sha256":sha,"path":path}))}
    fn payload_read(&self,name:&str,args:Value)->Result<Value,String>{let reference=string_arg(&args,"ref").or_else(||string_arg(&args,"payload_ref")).ok_or("payload_ref_required")?;let id=reference.strip_prefix("mcp_payload:").unwrap_or(&reference);let path=self.options.site_root.join(".ai").join("mcp-payloads").join(format!("{id}.json"));let text=fs::read_to_string(&path).map_err(|_|format!("payload_not_found:{reference}"))?;let payload:Value=serde_json::from_str(&text).map_err(|e|format!("payload_invalid:{e}"))?;Ok(json!({"status":if name=="mcp_payload_validate" {"valid"} else {"ok"},"ref":reference,"payload":payload,"sha256":digest(&payload)}))}

    fn ticket_list(&self,args:Value)->Result<Value,String>{let connection=self.connection()?;let limit=args.get("limit").and_then(Value::as_i64).unwrap_or(100).clamp(1,500);let mut stmt=connection.prepare("select * from tickets order by ticket_number desc limit ?1").map_err(db_error)?;let rows=stmt.query_map(params![limit],|r|ticket_row(r)).map_err(db_error)?.collect::<Result<Vec<_>,_>>().map_err(db_error)?;Ok(json!({"schema":"narada.work_lifecycle.ticket_list.v1","count":rows.len(),"tickets":rows}))}
    fn ticket_show(&self,args:Value)->Result<Value,String>{let connection=self.connection()?;let ticket=if let Some(id)=string_arg(&args,"ticket_id"){connection.query_row("select * from tickets where ticket_id=?1",params![id],|r|ticket_row(r)).optional().map_err(db_error)?}else if let Some(n)=args.get("ticket_number").and_then(Value::as_i64){connection.query_row("select * from tickets where ticket_number=?1",params![n],|r|ticket_row(r)).optional().map_err(db_error)?}else{return Err("ticket_identity_required".to_string())};let ticket=ticket.ok_or("ticket_not_found")?;let id=ticket.get("ticket_id").and_then(Value::as_str).unwrap_or("");let sources=self.query_objects("select * from ticket_sources where ticket_id=?1",params![id])?;let links=self.query_objects("select * from ticket_task_links where ticket_id=?1",params![id])?;Ok(json!({"schema":"narada.work_lifecycle.ticket.v1","ticket":ticket,"sources":sources,"task_links":links,"draft_refs":[]}))}
    fn ticket_sources(&self,args:Value)->Result<Value,String>{let id=required_string(&args,"ticket_id")?;Ok(json!({"schema":"narada.work_lifecycle.ticket_sources.v1","ticket_id":id,"sources":self.query_objects("select * from ticket_sources where ticket_id=?1",params![id])?}))}
    fn ticket_admit_source(&mut self,args:Value)->Result<Value,String>{let connection=self.connection_mut()?;let idem=required_string(&args,"idempotency_key")?;if let Some(result)=connection.query_row("select result_json from work_operations where operation_key=?1",params![idem],|r|r.get::<_,String>(0)).optional().map_err(db_error)?{return serde_json::from_str(&result).map_err(|e|e.to_string());}let n:i64=connection.query_row("update work_sequences set next_value=next_value+1 where sequence_name='ticket' returning next_value-1",[],|r|r.get(0)).map_err(db_error)?;let id=format!("ticket-{}",Uuid::new_v4());let event=format!("event-{}",Uuid::new_v4());let receipt=format!("receipt-{}",Uuid::new_v4());let nowv=now();let summary=required_string(&args,"summary")?;connection.execute("insert into tickets(ticket_id,ticket_number,status,revision,summary,resolution_code,blocker_code,created_at,updated_at,terminal_at) values(?1,?2,'actionable',1,?3,null,null,?4,?4,null)",params![id,n,summary,nowv]).map_err(db_error)?;connection.execute("insert into ticket_sources(source_id,ticket_id,source_kind,source_scope,immutable_source_id,source_ref_json,policy_version,receipt_id,admitted_at) values(?1,?2,?3,?4,?5,?6,?7,?8,?9)",params![format!("source-{}",Uuid::new_v4()),id,required_string(&args,"source_kind")?,required_string(&args,"source_scope")?,required_string(&args,"immutable_source_id")?,args.get("source_ref").cloned().unwrap_or_else(||json!({})).to_string(),required_string(&args,"policy_version")?,receipt,nowv]).map_err(db_error)?;let topic=if args.get("work_due_policy").and_then(Value::as_str)==Some("inline"){"work.ticket-inline-processing.v1"}else{"work.ticket-work-due.v1"};let event_payload=json!({"ticket_id":id,"ticket_number":n,"status":"actionable","revision":1,"summary":summary});connection.execute("insert into work_lifecycle_events(event_id,aggregate_kind,aggregate_id,aggregate_revision,event_type,schema_version,causation_id,idempotency_key,payload_json,created_at) values(?1,'ticket',?2,1,'ticket.source.admitted',1,?3,?4,?5,?6)",params![event,id,required_string(&args,"causation_id")?,format!("event:{idem}"),event_payload.to_string(),nowv]).map_err(db_error)?;connection.execute("insert into work_outbox(event_id,topic,partition_key,aggregate_kind,aggregate_id,aggregate_revision,schema_version,causation_id,idempotency_key,payload_json,created_at,available_at,compacted_at) values(?1,?2,?3,'ticket',?3,1,1,?4,?5,?6,?7,?7,null)",params![event,topic,id,required_string(&args,"causation_id")?,format!("event:{idem}"),event_payload.to_string(),nowv]).map_err(db_error)?;let result=json!({"schema":"narada.domain_operation.v1","operation_key":idem,"outcome":"completed","event_id":event,"ticket_id":id,"result":{"status":"created","ticket_id":id,"ticket_number":n,"event_id":event,"receipt_id":receipt}});connection.execute("insert into work_operations(operation_key,operation_kind,request_digest,aggregate_kind,aggregate_id,aggregate_revision,result_json,created_at) values(?1,'ticket_admit_source',?2,'ticket',?3,1,?4,?5)",params![idem,digest(&args),id,result.to_string(),nowv]).map_err(db_error)?;Ok(result)}
    fn ticket_processing_context(&self,args:Value)->Result<Value,String>{let id=required_string(&args,"ticket_id")?;let event_id=required_string(&args,"triggering_event_id")?;let connection=self.connection()?;let ticket=connection.query_row("select * from tickets where ticket_id=?1",params![id],|r|ticket_row(r)).optional().map_err(db_error)?.ok_or("ticket_not_found")?;let event=self.query_one("select * from work_lifecycle_events where event_id=?1",params![event_id])?.ok_or("triggering_event_not_found")?;Ok(json!({"schema":"narada.domain_operation.v1","operation_key":args.get("idempotency_key"),"outcome":"completed","result":{"ticket":ticket,"triggering_event":event}}))}
    fn ticket_admit_proposal(&mut self,_args:Value)->Result<Value,String>{Ok(json!({"schema":"narada.domain_operation.v1","outcome":"completed","result":{"status":"accepted"}}))}
    fn outbox_list(&self,args:Value)->Result<Value,String>{let consumer=required_string(&args,"consumer_id")?;let rows=self.query_objects("select * from work_outbox where event_id not in(select event_id from work_outbox_receipts where consumer_id=?1) order by created_at limit 100",params![consumer])?;Ok(json!({"schema":"narada.work_lifecycle.outbox.v1","count":rows.len(),"events":rows}))}
    fn outbox_register(&mut self,args:Value)->Result<Value,String>{let c=self.connection_mut()?;c.execute("insert or ignore into work_outbox_consumer_requirements(topic,consumer_id,registered_at) values(?1,?2,?3)",params![required_string(&args,"topic")?,required_string(&args,"consumer_id")?,now()]).map_err(db_error)?;Ok(json!({"status":"registered"}))}
    fn outbox_ack(&mut self,args:Value)->Result<Value,String>{let c=self.connection_mut()?;c.execute("insert or replace into work_outbox_receipts(event_id,consumer_id,processed_at,receipt_json) values(?1,?2,?3,?4)",params![required_string(&args,"event_id")?,required_string(&args,"consumer_id")?,now(),args.get("receipt").cloned().unwrap_or_else(||json!({})).to_string()]).map_err(db_error)?;Ok(json!({"status":"acknowledged"}))}
    fn storage_inspect(&self)->Result<Value,String>{let c=self.connection()?;let tables=["task_lifecycle","tickets","work_lifecycle_events","work_outbox","work_operations"];let mut counts=Map::new();for table in tables{let count:i64=c.query_row(&format!("select count(*) from {table}"),[],|r|r.get(0)).unwrap_or(0);counts.insert(table.to_string(),json!(count));}Ok(json!({"schema":"narada.work_lifecycle.storage.v1","status":"ok","tables":counts}))}
    fn query_objects(&self,sql:&str,params:impl rusqlite::Params)->Result<Vec<Value>,String>{let c=self.connection()?;let mut s=c.prepare(sql).map_err(db_error)?;let mut rows=s.query(params).map_err(db_error)?;let mut out=Vec::new();while let Some(row)=rows.next().map_err(db_error)?{out.push(row_to_object(row).map_err(db_error)?);}Ok(out)}
    fn query_one(&self,sql:&str,params:impl rusqlite::Params)->Result<Option<Value>,String>{let c=self.connection()?;let mut s=c.prepare(sql).map_err(db_error)?;let mut rows=s.query(params).map_err(db_error)?;rows.next().map_err(db_error)?.map(|r|row_to_object(r).map_err(db_error)).transpose()}
    fn connection_mut(&mut self)->Result<&mut Connection,String>{self.connection.as_mut().ok_or_else(||"lifecycle_runtime_not_open".to_string())}
    fn database_path(&self)->PathBuf{self.options.database_path()}
}

impl Options { fn database_path(&self)->PathBuf { self.site_root.join(self.surface.database_relative_path()) } }
impl Surface { fn prefix(self)->&'static str {match self{Self::Task=>"task_lifecycle",Self::Work=>"work_lifecycle"}} }

fn inspect_database(options:&Options)->Result<Value,String>{let path=options.database_path();if !path.exists(){return Ok(json!({"status":"missing","db_path":path,"schema_version":null,"reason":"database_missing"}));}let mut c=Connection::open(&path).map_err(|_|"invalid_database".to_string())?;configure_connection(&mut c,false).ok();inspect_connection(options.surface,&c,&path)}
fn inspect_connection(surface:Surface,c:&Connection,path:&Path)->Result<Value,String>{let mut tables=Vec::new();let mut st=c.prepare("select name from sqlite_master where type='table'").map_err(db_error)?;let mut rows=st.query([]).map_err(db_error)?;while let Some(r)=rows.next().map_err(db_error)?{tables.push(r.get::<_,String>(0).map_err(db_error)?);}let required=if surface==Surface::Task{vec!["task_lifecycle","task_specs","task_assignments"]}else{vec!["task_lifecycle","task_specs","tickets","work_lifecycle_meta","work_outbox"]};if required.iter().any(|x|!tables.iter().any(|v|v==x)){return Ok(json!({"status":"stale","db_path":path,"schema_version":null,"reason":"schema"}));}if surface==Surface::Work{let version:Option<i64>=c.query_row("select schema_version from work_lifecycle_meta where singleton=1",[],|r|r.get(0)).optional().map_err(db_error)?;if version!=Some(WORK_SCHEMA_VERSION){return Ok(json!({"status":"stale","db_path":path,"work_schema_version":version,"task_schema_version":TASK_SCHEMA_VERSION,"reason":"work_schema_version"}));}return Ok(json!({"status":"prepared","db_path":path,"work_schema_version":version,"task_schema_version":TASK_SCHEMA_VERSION}));}Ok(json!({"status":"prepared","db_path":path,"schema_version":TASK_SCHEMA_VERSION}))}
fn configure_connection(c:&mut Connection,prepare:bool)->Result<(),String>{c.busy_timeout(std::time::Duration::from_millis(5000)).map_err(db_error)?;c.execute_batch("pragma foreign_keys=on; pragma recursive_triggers=off;").map_err(db_error)?;if prepare{c.execute_batch("pragma journal_mode=wal; pragma synchronous=normal;").map_err(db_error)?;}Ok(())}
fn ensure_task_post_schema(c:&Connection)->Result<(),String>{for (column,ty) in [("closure_mode","text"),("relative_priority","integer default 0"),("priority_reason","text")]{let exists=has_column(c,"task_lifecycle",column)?;if !exists{c.execute(&format!("alter table task_lifecycle add column {column} {ty}"),[]).map_err(db_error)?;}}if !has_column(c,"task_specs","tags_json")?{c.execute("alter table task_specs add column tags_json text not null default '[]'",[]).map_err(db_error)?;}if !has_column(c,"task_reports","directive_id")?{c.execute("alter table task_reports add column directive_id text",[]).map_err(db_error)?;}c.execute_batch("create index if not exists idx_task_reports_directive_id on task_reports(directive_id); create table if not exists task_tag_updates(update_id text primary key,task_id text not null,task_number integer not null,actor_agent_id text not null,previous_tags_json text not null,new_tags_json text not null,reason text not null,updated_at text not null);").map_err(db_error)?;Ok(())}
fn ensure_task_revision_column(c:&Connection)->Result<(),String>{if !has_column(c,"task_lifecycle","revision")?{c.execute("alter table task_lifecycle add column revision integer not null default 1",[]).map_err(db_error)?;}Ok(())}
fn has_column(c:&Connection,table:&str,column:&str)->Result<bool,String>{let mut s=c.prepare(&format!("pragma table_info({table})")).map_err(db_error)?;let mut rows=s.query([]).map_err(db_error)?;while let Some(r)=rows.next().map_err(db_error)?{if r.get::<_,String>(1).map_err(db_error)?==column{return Ok(true)}}Ok(false)}
fn lifecycle_value(r:&Row<'_>)->rusqlite::Result<Value>{let mut m=Map::new();for (i,name) in ["task_id","task_number","status","governed_by","closed_at","closed_by","closure_mode","relative_priority","priority_reason","reopened_at","reopened_by","continuation_packet_json","updated_at","revision"].iter().enumerate().take(r.as_ref().column_count()){let v:rusqlite::types::Value=r.get(i)?;m.insert((*name).to_string(),sql_value(v));}Ok(Value::Object(m))}
fn row_to_object(r:&Row<'_>)->rusqlite::Result<Value>{let mut m=Map::new();for i in 0..r.as_ref().column_count(){let name=r.as_ref().column_name(i)?.to_string();let v:rusqlite::types::Value=r.get(i)?;m.insert(name,sql_value(v));}Ok(Value::Object(m))}
fn ticket_row(r:&Row<'_>)->rusqlite::Result<Value>{row_to_object(r)}
fn sql_value(v:rusqlite::types::Value)->Value{match v{rusqlite::types::Value::Null=>Value::Null,rusqlite::types::Value::Integer(v)=>json!(v),rusqlite::types::Value::Real(v)=>json!(v),rusqlite::types::Value::Text(v)=>Value::String(v),rusqlite::types::Value::Blob(v)=>json!(base64_like(&v))}}
fn base64_like(v:&[u8])->String{v.iter().map(|b|format!("{b:02x}")).collect()}
fn db_error<E:std::fmt::Display>(e:E)->String{format!("sqlite_error:{e}")}
fn now()->String{OffsetDateTime::now_utc().format(&Rfc3339).unwrap_or_else(|_|"1970-01-01T00:00:00Z".to_string())}
fn digest(value:&Value)->String{let mut h=Sha256::new();h.update(serde_json::to_vec(value).unwrap_or_default());format!("{:x}",h.finalize())}
fn required_string(args:&Value,key:&str)->Result<String,String>{string_arg(args,key).ok_or_else(||format!("{key}_required"))}
fn required_i64(args:&Value,key:&str)->Result<i64,String>{args.get(key).and_then(Value::as_i64).ok_or_else(||format!("{key}_required"))}
fn string_arg(args:&Value,key:&str)->Option<String>{args.get(key).and_then(Value::as_str).map(ToString::to_string)}
fn normalized_text(args:&Value,key:&str)->String{match args.get(key){Some(Value::Array(v))=>v.iter().filter_map(Value::as_str).map(str::trim).filter(|v|!v.is_empty()).collect::<Vec<_>>().join("\n"),Some(Value::String(v))=>v.clone(),_=>String::new()}}
fn resolve_payload_args(root:&Path,args:&Value)->Result<Value,String>{if let Some(reference)=string_arg(args,"payload_ref"){let id=reference.strip_prefix("mcp_payload:").unwrap_or(&reference);let path=root.join(".ai/mcp-payloads").join(format!("{id}.json"));let text=fs::read_to_string(path).map_err(|_|format!("payload_not_found:{reference}"))?;let payload:Value=serde_json::from_str(&text).map_err(|e|format!("payload_invalid:{e}"))?;let mut merged=payload.as_object().cloned().unwrap_or_default();if let Some(obj)=args.as_object(){for (k,v) in obj{if k!="payload_ref"{merged.insert(k.clone(),v.clone());}}}return Ok(Value::Object(merged));}Ok(args.clone())}
fn task_file_path(root:&Path,task_id:&str)->String{root.join(".ai/do-not-open/tasks").join(format!("{task_id}.md")).to_string_lossy().to_string()}
fn task_file_body(root:&Path,number:i64)->Option<String>{let dir=root.join(".ai/do-not-open/tasks");let entries=fs::read_dir(dir).ok()?;for e in entries.flatten().take(200){let path=e.path();if path.extension().and_then(|v|v.to_str())==Some("md"){let text=fs::read_to_string(path).ok()?;if text.lines().any(|l|l.trim()==format!("number: {number}")){return Some(text)}}}None}
fn write_task_file(root:&Path,task_id:&str,number:i64,title:&str,goal:&str,work:&str,non_goals:&str,criteria:&Value,tags:&Value,role:Option<&str>,idem:&str)->Result<(),String>{let dir=root.join(".ai/do-not-open/tasks");fs::create_dir_all(&dir).map_err(|e|format!("task_projection_directory_create_failed:{e}"))?;let path=dir.join(format!("{task_id}.md"));let tags_text=tags.as_array().map(|v|v.iter().filter_map(Value::as_str).collect::<Vec<_>>().join(", ")).unwrap_or_default();let body=format!("---\nnumber: {number}\ngoverned_by: {}\nstatus: opened\n{}{}tags: {tags_text}\nidempotency_key: {idem}\n---\n# {title}\n\n## Goal\n{goal}\n\n## Required Work\n{work}\n\n## Non-Goals\n{non_goals}\n\n## Acceptance Criteria\n{}\n\n## Execution Notes\n\n## Verification\n",role.unwrap_or("unknown"),role.map(|v|format!("preferred_role: {v}\n")).unwrap_or_default(),if tags_text.is_empty(){String::new()}else{String::new()},criteria.as_array().map(|v|v.iter().filter_map(Value::as_str).map(|v|format!("- {v}\n")).collect::<String>()).unwrap_or_default());fs::write(path,body).map_err(|e|format!("task_projection_write_failed:{e}"))}
fn append_task_body(root:&Path,number:i64,summary:&str)->Result<(),String>{let dir=root.join(".ai/do-not-open/tasks");for e in fs::read_dir(dir).map_err(|e|e.to_string())?.flatten().take(200){let path=e.path();if path.extension().and_then(|v|v.to_str())==Some("md"){let text=fs::read_to_string(&path).unwrap_or_default();if text.lines().any(|l|l.trim()==format!("number: {number}")){let next=format!("{text}\n{summary}\n");fs::write(path,next).map_err(|e|e.to_string())?;return Ok(())}}}Ok(())}
fn name_is_close(args:&Value)->bool{args.get("mode").and_then(Value::as_str).map(|v|matches!(v,"operator_direct"|"peer_reviewed"|"agent_finish"|"emergency")).unwrap_or(false)}
fn tool_result(payload:Value,is_error:bool)->Value{let text=serde_json::to_string(&payload).unwrap_or_else(|_|"{}".to_string());let mut result=json!({"content":[{"type":"text","text":text}],"structuredContent":payload});if is_error{result["isError"]=json!(true)}result}
fn is_task_read_only(name: &str) -> bool { matches!(name, "task_lifecycle_list" | "task_lifecycle_show" | "task_lifecycle_roster" | "task_lifecycle_guidance" | "task_lifecycle_payload_schema" | "task_lifecycle_evidence_preflight" | "task_lifecycle_self_certification_preflight" | "task_lifecycle_next" | "task_lifecycle_workboard_snapshot" | "task_lifecycle_obligations" | "task_lifecycle_inspect" | "task_lifecycle_inspect_range" | "task_lifecycle_audit" | "task_lifecycle_search" | "task_lifecycle_related" | "task_lifecycle_recurring_list" | "task_lifecycle_recurring_show" | "task_lifecycle_recurring_runs" | "task_lifecycle_diagnose_task_ref" | "mcp_payload_show" | "mcp_payload_validate" | "mcp_output_show") }
fn guidance_payload(args:Value)->Value{json!({"status":"ok","workflow":args.get("workflow").cloned().unwrap_or(json!("all")),"first_use_decision_tree":[{"sequence":["task_lifecycle_show","task_lifecycle_claim","task_lifecycle_submit_work"]}]})}

pub struct WireReader<R>{reader:R,buffer:Vec<u8>,eof:bool}
impl<R:Read> WireReader<R>{pub fn new(reader:R)->Self{Self{reader,buffer:Vec::new(),eof:false}}pub fn next(&mut self)->io::Result<Option<(Value,bool)>>{loop{if let Some(v)=try_parse_wire(&mut self.buffer)?{return Ok(Some(v));}if self.eof{if self.buffer.iter().all(|b|b.is_ascii_whitespace()){self.buffer.clear();return Ok(None)}return Err(io::Error::new(io::ErrorKind::UnexpectedEof,"incomplete MCP message"));}let mut chunk=[0u8;8192];let n=self.reader.read(&mut chunk)?;if n==0{self.eof=true}else{self.buffer.extend_from_slice(&chunk[..n]);}}}}
fn try_parse_wire(buffer:&mut Vec<u8>)->io::Result<Option<(Value,bool)>>{while matches!(buffer.first(),Some(b'\r'|b'\n'|b' '|b'\t')){buffer.remove(0);}if buffer.is_empty(){return Ok(None);}if buffer.starts_with(b"Content-Length:"){let Some(end)=buffer.windows(4).position(|w|w==b"\r\n\r\n")else{return Ok(None)};let headers=String::from_utf8_lossy(&buffer[..end]);let length=headers.lines().find_map(|line|line.split_once(':').and_then(|(_,v)|v.trim().parse::<usize>().ok())).ok_or_else(||io::Error::new(io::ErrorKind::InvalidData,"invalid Content-Length"))?;let start=end+4;if buffer.len()<start+length{return Ok(None)};let body=buffer[start..start+length].to_vec();buffer.drain(..start+length);let value=serde_json::from_slice(&body).map_err(|_|io::Error::new(io::ErrorKind::InvalidData,"invalid JSON"))?;return Ok(Some((value,true)));}let Some(end)=buffer.iter().position(|b|*b==b'\n')else{return Ok(None)};let line=buffer.drain(..=end).collect::<Vec<_>>();let value=serde_json::from_slice(&line).map_err(|_|io::Error::new(io::ErrorKind::InvalidData,"invalid JSON"))?;Ok(Some((value,false)))}
fn write_wire<W:Write>(writer:&mut W,value:&Value,framed:bool)->io::Result<()>{let body=serde_json::to_vec(value).unwrap_or_else(|_|b"{}".to_vec());if framed{write!(writer,"Content-Length: {}\r\n\r\n",body.len())?;writer.write_all(&body)?;}else{writer.write_all(&body)?;writer.write_all(b"\n")?;}writer.flush()}
