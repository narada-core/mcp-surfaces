
    use super::*;

    fn server(surface: Surface) -> LifecycleServer {
        LifecycleServer {
            options: Options {
                surface,
                site_root: PathBuf::from("."),
                site_root_source: "test".to_string(),
                prepare: false,
                migrate_legacy: false,
                source_database_path: None,
            },
            connection: None,
            booted_at: "2026-01-01T00:00:00Z".to_string(),
        }
    }

    fn modern_params() -> Value {
        json!({
            "_meta": {
                "io.modelcontextprotocol/protocolVersion": MODERN_PROTOCOL_VERSION,
                "io.modelcontextprotocol/clientInfo": {"name": "test", "version": "1"},
                "io.modelcontextprotocol/clientCapabilities": {}
            }
        })
    }

    #[test]
    fn modern_discovery_and_tools_list_are_self_describing() {
        let mut server = server(Surface::Task);
        let discover = server.handle_request(json!({"jsonrpc":"2.0","id":1,"method":"server/discover","params":modern_params()})).expect("discover response");
        assert_eq!(discover["result"]["resultType"], "complete");
        assert_eq!(discover["result"]["supportedVersions"][0], MODERN_PROTOCOL_VERSION);
        assert_eq!(discover["result"]["_meta"]["io.modelcontextprotocol/serverInfo"]["name"], "narada-task-lifecycle-mcp");

        let list = server.handle_request(json!({"jsonrpc":"2.0","id":2,"method":"tools/list","params":modern_params()})).expect("list response");
        assert_eq!(list["result"]["resultType"], "complete");
        assert_eq!(list["result"]["cacheScope"], "public");
        assert!(list["result"]["ttlMs"].as_u64().unwrap_or_default() > 0);
    }

    #[test]
    fn lifecycle_tool_schemas_are_named_closed_and_bounded() {
        fn assert_bounded(schema: &Value, path: &str) {
            if schema.get("type").and_then(Value::as_str) == Some("string")
                && schema.get("enum").is_none()
            {
                assert!(schema.get("maxLength").and_then(Value::as_u64).is_some(), "unbounded string: {path}");
            }
            if schema.get("type").and_then(Value::as_str) == Some("array") {
                assert!(schema.get("maxItems").and_then(Value::as_u64).is_some(), "unbounded array: {path}");
            }
            if let Some(properties) = schema.get("properties").and_then(Value::as_object) {
                for (name, child) in properties {
                    assert_bounded(child, &format!("{path}/{name}"));
                }
            }
            if let Some(items) = schema.get("items") {
                assert_bounded(items, &format!("{path}/*"));
            }
            for keyword in ["allOf", "anyOf", "oneOf"] {
                if let Some(branches) = schema.get(keyword).and_then(Value::as_array) {
                    for (index, branch) in branches.iter().enumerate() {
                        assert_bounded(branch, &format!("{path}/{keyword}/{index}"));
                    }
                }
            }
        }
        for surface in [Surface::Task, Surface::Work] {
            for tool in surface.tools() {
                let name = tool["name"].as_str().expect("tool name");
                let schema = &tool["inputSchema"];
                assert_eq!(schema["title"], format!("{name}.input"));
                assert_eq!(schema["additionalProperties"], false);
                assert_bounded(schema, name);
            }
        }
    }

    #[test]
    fn lifecycle_input_contract_is_enforced_before_authority_dispatch() {
        let mut server = server(Surface::Task);
        let response = server.handle_request(json!({
            "jsonrpc":"2.0","id":20,"method":"tools/call",
            "params":{"name":"task_lifecycle_doctor","arguments":{"detail":"x".repeat(9000)}}
        })).expect("error response");
        assert!(response["error"]["message"].as_str().is_some_and(|message| message.contains("input_schema_validation_failed:/arguments/detail:maxLength")));

        let unknown = server.handle_request(json!({
            "jsonrpc":"2.0","id":21,"method":"tools/call",
            "params":{"name":"task_lifecycle_doctor","arguments":{"unexpected":true}}
        })).expect("error response");
        assert!(unknown["error"]["message"].as_str().is_some_and(|message| message.contains("additionalProperties")));
    }

    #[test]
    fn modern_requests_require_metadata_and_remove_initialize() {
        let mut server = server(Surface::Work);
        let missing = server.handle_request(json!({"jsonrpc":"2.0","id":3,"method":"tools/list","params":{"_meta":{"io.modelcontextprotocol/protocolVersion":MODERN_PROTOCOL_VERSION}}})).expect("error response");
        assert_eq!(missing["error"]["data"]["code"], "modern_metadata_required");

        let mut params = modern_params();
        params["protocolVersion"] = json!(MODERN_PROTOCOL_VERSION);
        let initialize = server.handle_request(json!({"jsonrpc":"2.0","id":4,"method":"initialize","params":params})).expect("error response");
        assert_eq!(initialize["error"]["data"]["code"], "initialize_removed");
    }

    #[test]
    fn task_errors_preserve_structured_diagnostics() {
        let error = server(Surface::Task).error_value("task_not_found:2469".to_string());
        assert_eq!(error["data"]["schema"], "narada.task_lifecycle.error.v1");
        assert_eq!(error["data"]["code"], "task_not_found");
        assert_eq!(error["data"]["site_root"], ".");
    }

    #[test]
    fn guidance_is_compact_by_default_and_full_on_request() {
        let compact = guidance_payload(Path::new("."), json!({"workflow":"ordinary_task"}));
        let full = guidance_payload(Path::new("."), json!({"workflow":"ordinary_task","detail":"full"}));
        assert_eq!(compact["detail"], "compact");
        assert!(compact.get("state_truth_table").is_none());
        assert_eq!(full["detail"], "full");
        assert!(full.get("state_truth_table").is_some());
        assert!(serde_json::to_string(&compact).unwrap().len() < 4_000);
        let full_text = serde_json::to_string(&full).unwrap();
        assert!(full_text.contains("canonical opening and closing inbox checks"));
        assert!(full_text.contains("not a turn-boundary inbox check"));
        let bridge = Surface::Task.tools().into_iter().find(|tool| tool["name"] == "task_lifecycle_bridge_poll").expect("bridge tool");
        assert!(bridge["description"].as_str().unwrap().contains("not the canonical opening or closing turn-boundary inbox check"));
    }

    #[test]
    fn materialized_output_round_trips_through_output_show() {
        let root = std::env::temp_dir().join(format!("narada-native-output-roundtrip-{}", Uuid::new_v4()));
        let mut server = server(Surface::Task);
        server.options.site_root = root.clone();
        let body = "x".repeat(5_000);
        let result = server.tool_result("task_lifecycle_guidance", json!({"status":"ok","body":body}), false).expect("materialize output");
        let output_ref = result["structuredContent"]["output_ref"].as_str().expect("output ref");
        let page = server.output_show(json!({"ref":output_ref,"offset":0,"limit":10_000})).expect("read materialized output");
        assert!(page["output_text"].as_str().unwrap().contains(&body));
        assert_eq!(page["output_truncated"], false);
        fs::remove_dir_all(root).expect("remove output fixture");
    }

    #[test]
    fn status_projection_uses_the_authoritative_task_id() {
        let root = std::env::temp_dir().join(format!("narada-native-projection-{}", Uuid::new_v4()));
        let task_dir = root.join(".ai/do-not-open/tasks");
        fs::create_dir_all(&task_dir).expect("create task projection directory");
        let task_id = "task-authoritative-id";
        let path = task_dir.join(format!("{task_id}.md"));
        fs::write(&path, "---\nnumber: 2469\nstatus: claimed\n---\n# Task\n").expect("write task projection");

        project_task_status(&root, task_id, 2469, "closed").expect("project status");

        let projected = fs::read_to_string(&path).expect("read task projection");
        assert!(projected.lines().any(|line| line == "status: closed"));
        fs::remove_dir_all(root).expect("remove task projection fixture");
    }

    #[test]
    fn anonymous_bridge_poll_is_read_only_and_identity_neutral() {
        let root = std::env::temp_dir().join(format!(
            "narada-native-bridge-poll-{}",
            Uuid::new_v4()
        ));
        let options = Options {
            surface: Surface::Task,
            site_root: root.clone(),
            site_root_source: "test".to_string(),
            prepare: false,
            migrate_legacy: false,
            source_database_path: None,
        };
        LifecycleServer::prepare_database(&options).expect("prepare task database");
        let server = LifecycleServer::new(options).expect("open task server");
        let result = server
            .task_bridge_poll(json!({"dry_run":true,"limit":20}))
            .expect("poll");
        assert_eq!(result["status"], "planned");
        assert_eq!(result["participation_scope"], "site");
        assert_eq!(result["site_root"], root.to_string_lossy().as_ref());
        assert_eq!(result["site_root_source"], "test");
        assert_eq!(result["identity_effect"]["identity_inferred"], false);
        assert_eq!(
            result["identity_effect"]["poll_authorizes_named_reply"],
            false
        );
        drop(server);
        fs::remove_dir_all(root).expect("remove bridge fixture");
    }

    #[test]
    fn legacy_recurring_schema_migrates_without_losing_history() {
        let connection = Connection::open_in_memory().expect("open database");
        connection.execute_batch("create table recurring_task_definitions(recurrence_id text primary key,title text not null,status text not null,trigger_mode text not null,trigger_description text,target_role text,preferred_role text,goal_markdown text,context_markdown text,required_work_markdown text,non_goals_markdown text,acceptance_criteria_json text not null,evidence_requirements_json text not null,created_by text not null,created_at text not null,updated_at text not null,suspended_at text,retired_at text,schedule_kind text,schedule_interval integer,schedule_timezone text,last_due_key text,last_auto_triggered_at text);
            create table recurring_task_events(event_id text primary key,recurrence_id text not null,event_type text not null,state_after text not null,actor_agent_id text not null,authority_basis_json text not null,event_json text not null,created_at text not null);
            create table recurring_task_runs(run_id text primary key,recurrence_id text not null,task_id text not null,task_number integer not null,trigger_mode text not null,run_reason text not null,actor_agent_id text not null,authority_basis_json text not null,created_at text not null);
            insert into recurring_task_definitions values('rec-1','Daily watcher','active','schedule','daily','resident','resident','Watch','Context','Sweep','None','[\"verified\"]','[\"primary\"]','agent','2026-08-01T00:00:00Z','2026-08-02T00:00:00Z',null,null,'daily',1,'America/Chicago','2026-08-02','2026-08-02T00:00:00Z');
            insert into recurring_task_events values('event-1','rec-1','created','active','agent','{}','{}','2026-08-01T00:00:00Z');
            insert into recurring_task_runs values('run-1','rec-1','task-1',1,'schedule','due','agent','{}','2026-08-02T00:00:00Z');").expect("legacy schema");

        ensure_native_auxiliary_schema(&connection).expect("migrate legacy recurrence");
        ensure_native_auxiliary_schema(&connection).expect("migration is idempotent");

        let definition: String = connection
            .query_row(
                "select definition_json from recurring_task_definitions where recurrence_id='rec-1'",
                [],
                |row| row.get(0),
            )
            .expect("definition");
        let definition: Value = serde_json::from_str(&definition).expect("definition json");
        assert_eq!(definition["title"], "Daily watcher");
        assert_eq!(definition["acceptance_criteria"][0], "verified");
        assert_eq!(definition["schedule_timezone"], "America/Chicago");
        assert!(!has_column(&connection, "recurring_task_events", "state_after").unwrap());
        assert!(has_column(&connection, "recurring_task_runs", "run_json").unwrap());
        let run_json: String = connection
            .query_row("select run_json from recurring_task_runs where run_id='run-1'", [], |row| row.get(0))
            .expect("run history");
        assert_eq!(serde_json::from_str::<Value>(&run_json).unwrap()["reason"], "due");
        connection.execute("insert into recurring_task_definitions(recurrence_id,status,definition_json,last_due_key,last_auto_triggered_at,updated_at) values('rec-2','active','{}',null,null,'2026-08-03T00:00:00Z')", []).expect("native definition insert");
        connection.execute("insert into recurring_task_events(event_id,recurrence_id,event_type,actor_agent_id,authority_basis_json,event_json,created_at) values('event-2','rec-2','created','agent','{}','{}','2026-08-03T00:00:00Z')", []).expect("native event insert");
    }

    #[test]
    fn recurring_definition_projects_and_trigger_uses_immutable_payload() {
        let root = std::env::temp_dir().join(format!(
            "narada-native-recurring-trigger-{}",
            Uuid::new_v4()
        ));
        let options = Options {
            surface: Surface::Task,
            site_root: root.clone(),
            site_root_source: "test".to_string(),
            prepare: false,
            migrate_legacy: false,
            source_database_path: None,
        };
        LifecycleServer::prepare_database(&options).expect("prepare task database");
        let mut server = LifecycleServer::new(options).expect("open task server");
        let created = server
            .task_recurring_create(json!({
                "title":"Daily watcher",
                "goal":"Detect changes",
                "context":"Stored context",
                "required_work":"Inspect primary sources",
                "non_goals":"Do not infer from citations",
                "acceptance_criteria":["Evidence recorded"],
                "tags":["watcher"],
                "preferred_role":"resident",
                "target_role":"resident",
                "actor_agent_id":"test-agent",
                "authority_basis":{"kind":"test","summary":"end-to-end recurrence test"}
            }))
            .expect("create recurrence");
        let recurrence_id = created["recurrence_id"].as_str().unwrap().to_string();

        let shown = server
            .task_recurring_read(
                "task_lifecycle_recurring_show",
                json!({"recurrence_id":recurrence_id}),
            )
            .expect("show recurrence");
        assert_eq!(shown["definition"]["title"], "Daily watcher");
        assert_eq!(shown["definition"]["goal"], "Detect changes");
        assert!(shown["definition"].get("definition_json").is_none());

        let trigger_args = json!({
            "recurrence_id":recurrence_id,
            "actor_agent_id":"test-agent",
            "authority_basis":{"kind":"test","summary":"trigger recurrence"},
            "run_reason":"test",
            "due_key":"2026-08-13"
        });
        let first = server
            .task_recurring_trigger(trigger_args.clone())
            .expect("trigger recurrence through immutable payload");
        let second = server
            .task_recurring_trigger(trigger_args)
            .expect("idempotent replay through same immutable payload");
        assert_eq!(first["task"]["task_id"], second["task"]["task_id"]);
        assert_eq!(first["task"]["title"], "Daily watcher");
        assert_eq!(second["status"], "already_triggered");
        assert_eq!(second["task"]["status"], "created");
        let run_count: i64 = server
            .connection()
            .unwrap()
            .query_row(
                "select count(*) from recurring_task_runs where recurrence_id=?1 and due_key='2026-08-13'",
                params![recurrence_id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(run_count, 1);
        let shown_with_runs = server.task_recurring_read(
            "task_lifecycle_recurring_show",
            json!({"recurrence_id":recurrence_id,"include_runs":true}),
        ).unwrap();
        assert_eq!(shown_with_runs["runs"][0]["due_key"], "2026-08-13");
        assert!(shown_with_runs["runs"][0].get("run_json").is_none());
        let payload_ref = format!(
            "mcp_payload:recurring_{}@v1",
            &digest(&json!({"recurrence_id":recurrence_id,"due_key":"2026-08-13"}))[..48]
        );
        assert_eq!(
            read_payload_revision_payload(&root, &payload_ref).unwrap()["required_work"],
            "Inspect primary sources"
        );
        drop(server);
        fs::remove_dir_all(root).expect("remove recurring fixture");
    }

    #[test]
    fn recurring_list_is_compact_and_paginated_by_default() {
        let root = std::env::temp_dir().join(format!("narada-native-recurring-list-{}", Uuid::new_v4()));
        let options = Options { surface: Surface::Task, site_root: root.clone(), site_root_source: "test".to_string(), prepare: false, migrate_legacy: false, source_database_path: None };
        LifecycleServer::prepare_database(&options).expect("prepare task database");
        let mut server = LifecycleServer::new(options).expect("open task server");
        let authority = json!({"kind":"test","summary":"pagination test"});
        server.task_recurring_create(json!({"title":"First","actor_agent_id":"test-agent","authority_basis":authority})).unwrap();
        server.task_recurring_create(json!({"title":"Second","actor_agent_id":"test-agent","authority_basis":authority})).unwrap();
        let first = server.task_recurring_read("task_lifecycle_recurring_list", json!({"limit":1})).unwrap();
        let second = server.task_recurring_read("task_lifecycle_recurring_list", json!({"limit":1,"offset":1})).unwrap();
        assert_eq!(first["compact"], true);
        assert_eq!(first["has_more"], true);
        assert_eq!(first["next_offset"], 1);
        assert!(first["definitions"][0].get("goal").is_none());
        assert_ne!(first["definitions"][0]["recurrence_id"], second["definitions"][0]["recurrence_id"]);
        drop(server);
        fs::remove_dir_all(root).expect("remove recurring list fixture");
    }

    #[test]
    fn recurring_run_due_uses_one_utc_daily_key_and_skips_manual_definitions() {
        let root = std::env::temp_dir().join(format!(
            "narada-native-recurring-due-{}",
            Uuid::new_v4()
        ));
        let options = Options {
            surface: Surface::Task,
            site_root: root.clone(),
            site_root_source: "test".to_string(),
            prepare: false,
            migrate_legacy: false,
            source_database_path: None,
        };
        LifecycleServer::prepare_database(&options).expect("prepare task database");
        let mut server = LifecycleServer::new(options).expect("open task server");
        let authority = json!({"kind":"test","summary":"daily recurrence test"});
        let manual = server.task_recurring_create(json!({"title":"Manual","actor_agent_id":"test-agent","authority_basis":authority})).unwrap();
        let scheduled = server.task_recurring_create(json!({"title":"Scheduled","actor_agent_id":"test-agent","authority_basis":authority,"trigger_mode":"schedule","schedule_kind":"daily","schedule_timezone":"UTC"})).unwrap();
        let args = json!({"actor_agent_id":"test-agent","authority_basis":authority,"current_time":"2026-08-13T23:59:59-05:00"});
        let first = server.task_recurring_run_due(args.clone()).unwrap();
        let second = server.task_recurring_run_due(args).unwrap();
        assert_eq!(first["due_key"], "2026-08-14");
        assert_eq!(first["count"], 1);
        assert_eq!(first["runs"][0]["recurrence_id"], scheduled["recurrence_id"]);
        assert_eq!(second["count"], 0);
        let manual_run_count: i64 = server.connection().unwrap().query_row("select count(*) from recurring_task_runs where recurrence_id=?1",params![manual["recurrence_id"].as_str()],|row|row.get(0)).unwrap();
        assert_eq!(manual_run_count,0);
        assert!(second["skipped"].as_array().unwrap().is_empty());
        drop(server);
        fs::remove_dir_all(root).expect("remove recurring due fixture");
    }

    #[test]
    fn internal_task_creation_is_confined_to_the_payload_adapter() {
        let production = concat!(
            include_str!("parts/01.rs"),
            include_str!("parts/02.rs"),
            include_str!("parts/03.rs"),
            include_str!("parts/04.rs"),
            include_str!("parts/05.rs"),
            include_str!("parts/06.rs"),
            include_str!("parts/07.rs"),
            include_str!("parts/08.rs"),
            include_str!("parts/09.rs"),
            include_str!("parts/10.rs"),
            include_str!("parts/11.rs"),
            include_str!("parts/12.rs"),
            include_str!("parts/13.rs"),
            include_str!("parts/14.rs"),
            include_str!("parts/15.rs"),
            include_str!("parts/16.rs"),
            include_str!("parts/17.rs"),
            include_str!("parts/18.rs"),
            include_str!("parts/19.rs"),
            include_str!("parts/20.rs"),
            include_str!("parts/21.rs"),
        );
        assert_eq!(production.matches("self.task_create(").count(), 2);
        assert!(production.contains("self.task_create(json!({\"payload_ref\":payload_ref}))"));
        assert!(production.contains("\"task_lifecycle_create\" => self.task_create(args)"));
    }
