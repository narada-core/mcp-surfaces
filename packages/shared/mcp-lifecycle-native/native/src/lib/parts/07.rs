impl LifecycleServer {
    fn task_inspect_range(&self, args: Value) -> Result<Value, String> {
        let start = args
            .get("start_task_number")
            .and_then(Value::as_i64)
            .unwrap_or(1);
        let end = args
            .get("end_task_number")
            .and_then(Value::as_i64)
            .unwrap_or(start);
        if end < start {
            return Err("task_range_invalid".to_string());
        }
        let limit = args
            .get("limit")
            .and_then(Value::as_i64)
            .unwrap_or(50)
            .clamp(1, 200);
        let rows = self.query_objects("select l.*,s.title,s.tags_json from task_lifecycle l left join task_specs s on s.task_id=l.task_id where l.task_number between ?1 and ?2 order by l.task_number limit ?3", params![start,end,limit])?;
        Ok(
            json!({"schema":"narada.task.inspect_range.v1","status":"ok","start_task_number":start,"end_task_number":end,"count":rows.len(),"tasks":rows}),
        )
    }

    fn task_evidence_preflight(&self, args: Value) -> Result<Value, String> {
        let number = required_i64(&args, "task_number")?;
        let connection = self.connection()?;
        let task = connection
            .query_row(
                "select task_id,status from task_lifecycle where task_number=?1",
                params![number],
                |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)),
            )
            .optional()
            .map_err(db_error)?
            .ok_or_else(|| format!("task_not_found:{number}"))?;
        let reports: i64 = connection
            .query_row(
                "select count(*) from task_reports where task_id=?1",
                params![task.0],
                |r| r.get(0),
            )
            .map_err(db_error)?;
        let admissions: i64 = connection.query_row("select count(*) from evidence_admission_results where task_id=?1 and verdict='admitted'",params![task.0],|r|r.get(0)).map_err(db_error)?;
        let dependency_satisfaction = self.task_dependency_satisfaction(&task.0)?;
        let mut blockers = if reports == 0 {
            vec![json!({"code":"report_required","message":"Submit a report before closeout."})]
        } else if admissions == 0 {
            vec![
                json!({"code":"evidence_admission_required","message":"Admit evidence before closure."}),
            ]
        } else {
            Vec::new()
        };
        if dependency_satisfaction.get("all_satisfied").and_then(Value::as_bool) == Some(false) {
            blockers.push(json!({"code":"dependencies_unsatisfied","message":"Complete required dependencies before closure.","dependency_satisfaction":dependency_satisfaction.clone()}));
        }
        Ok(
            json!({"schema":"narada.task.mcp.evidence_preflight.v0","status":if blockers.is_empty(){"ready"}else{"blocked"},"task_number":number,"task_id":task.0,"lifecycle_status":task.1,"blockers":blockers,"dependency_satisfaction":dependency_satisfaction,"evidence":{"report_count":reports,"admission_count":admissions},"next_action":if blockers.is_empty(){"task_lifecycle_close"}else{"task_lifecycle_finish"}}),
        )
    }

    fn task_self_certification_preflight(&self, args: Value) -> Result<Value, String> {
        let value = args
            .get("self_certification")
            .cloned()
            .unwrap_or_else(|| json!(null));
        let valid = value.as_object().map(|o| !o.is_empty()).unwrap_or(false);
        Ok(
            json!({"schema":"narada.task.mcp.self_certification_preflight.v0","status":if valid{"ready"}else{"blocked"},"valid":valid,"self_certification":value,"blockers":if valid{json!([])}else{json!([{"code":"self_certification_required"}])}}),
        )
    }

    fn task_record_observation(&mut self, args: Value) -> Result<Value, String> {
        let artifact_uri = required_string(&args, "artifact_uri")?;
        let agent = string_arg(&args, "agent_id")
            .or_else(|| string_arg(&args, "source_operator"))
            .unwrap_or_else(|| "native".to_string());
        let number = args.get("task_number").and_then(Value::as_i64);
        let connection = self.connection_mut()?;
        let (task_id, task_number) = if let Some(n) = number {
            connection
                .query_row(
                    "select task_id,task_number from task_lifecycle where task_number=?1",
                    params![n],
                    |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)),
                )
                .optional()
                .map_err(db_error)?
                .unwrap_or((String::new(), n))
        } else {
            (String::new(), 0)
        };
        let id = format!("artifact-{}", Uuid::new_v4());
        let admitted = json!({"artifact_uri":artifact_uri,"content":args.get("content"),"source_operator":args.get("source_operator"),"agent_id":agent});
        connection.execute("insert into observation_artifacts(artifact_id,artifact_type,source_operator,task_id,task_number,agent_id,artifact_uri,digest,admitted_view_json,created_at) values(?1,'observation',?2,?3,?4,?5,?6,?7,?8,?9)",params![id,agent,if task_id.is_empty(){None::<String>}else{Some(task_id)},task_number,agent,artifact_uri,digest(&admitted),admitted.to_string(),now()]).map_err(db_error)?;
        Ok(
            json!({"schema":"narada.task.observation.v1","status":"admitted","artifact_id":id,"artifact_uri":artifact_uri,"task_number":number}),
        )
    }

    fn task_bridge_poll(&self, args: Value) -> Result<Value, String> {
        let limit = args
            .get("limit")
            .and_then(Value::as_i64)
            .unwrap_or(25)
            .clamp(1, 100);
        let inbox = self.options.site_root.join(".ai").join("inbox");
        let mut envelopes = Vec::new();
        if let Ok(entries) = fs::read_dir(inbox) {
            for entry in entries.flatten().take(limit as usize) {
                let path = entry.path();
                if path.is_file() {
                    envelopes.push(json!({"envelope_id":path.file_stem().and_then(|v|v.to_str()),"path":path.to_string_lossy()}));
                }
            }
        }
        Ok(
            json!({
                "schema":"narada.task.inbox.bridge.v1",
                "status":if args.get("dry_run").and_then(Value::as_bool)==Some(true){"planned"}else{"ok"},
                "participation_scope":"site",
                "site_root":self.options.site_root.to_string_lossy(),
                "site_root_source":&self.options.site_root_source,
                "identity_effect":{
                    "identity_inferred":false,
                    "poll_authorizes_named_claim":false,
                    "poll_authorizes_named_reply":false
                },
                "count":envelopes.len(),
                "envelopes":envelopes
            }),
        )
    }

    fn task_inbox_target(&mut self, args: Value) -> Result<Value, String> {
        let envelope = required_string(&args, "envelope_id")?;
        let status = string_arg(&args, "disposition").unwrap_or_else(|| "targeted".to_string());
        if args.get("dry_run").and_then(Value::as_bool) == Some(true) {
            return Ok(
                json!({"schema":"narada.task.inbox.target.v1","status":"planned","envelope_id":envelope,"disposition":status}),
            );
        }
        let connection = self.connection_mut()?;
        connection.execute("insert or replace into envelope_task_mappings(envelope_id,task_id,task_number,materialized_at) values(?1,'',null,?2)",params![envelope,now()]).map_err(db_error)?;
        Ok(
            json!({"schema":"narada.task.inbox.target.v1","status":"targeted","envelope_id":envelope,"disposition":status}),
        )
    }

    fn task_dependency_disposition(&mut self, args: Value) -> Result<Value, String> {
        let dependency_id = required_string(&args, "dependency_id")?;
        let agent = required_string(&args, "agent_id")?;
        let kind = required_string(&args, "kind")?;
        let summary = required_string(&args, "summary")?;
        let connection = self.connection_mut()?;
        let required_task_id: Option<String> = connection
            .query_row(
                "select required_task_id from task_dependencies where dependency_id=?1",
                params![dependency_id],
                |r| r.get(0),
            )
            .optional()
            .map_err(db_error)?;
        let Some(required_task_id) = required_task_id else {
            return Err(format!("dependency_not_found:{dependency_id}"));
        };
        let required_outcome_id = if let Some(outcome_id) = string_arg(&args, "required_outcome_id") {
            outcome_id
        } else {
            connection
                .query_row(
                    "select outcome_id from task_outcomes where task_id=?1 order by admitted_at desc,rowid desc limit 1",
                    params![required_task_id],
                    |r| r.get(0),
                )
                .optional()
                .map_err(db_error)?
                .ok_or_else(|| format!("dependency_required_outcome_not_found:{dependency_id}"))?
        };
        let id = format!("disposition-{}", Uuid::new_v4());
        let status = string_arg(&args, "status").unwrap_or_else(|| "recorded".to_string());
        connection.execute(
            "insert into task_dependency_dispositions(disposition_id,dependency_id,required_outcome_id,kind,status,target_task_id,routed_obligation_id,authority_basis_json,summary,created_by,created_at) values(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)",
            params![id,dependency_id,required_outcome_id,kind,status,string_arg(&args,"target_task_id"),string_arg(&args,"routed_obligation_id"),args.get("authority_basis").cloned().unwrap_or_else(||json!(null)).to_string(),summary,agent,now()],
        ).map_err(db_error)?;
        connection
            .execute(
                "update task_dependencies set status=?1 where dependency_id=?2",
                params![status, dependency_id],
            )
            .map_err(db_error)?;
        Ok(
            json!({"schema":"narada.task.dependency_disposition.v1","status":"recorded","disposition_id":id,"dependency_id":dependency_id,"kind":kind,"summary":summary}),
        )
    }

    fn task_recurring_read(&self, name: &str, args: Value) -> Result<Value, String> {
        let connection = self.connection()?;
        let limit = args
            .get("limit")
            .and_then(Value::as_i64)
            .unwrap_or(20)
            .clamp(1, 100);
        if name == "task_lifecycle_recurring_list" {
            let offset = args.get("offset").and_then(Value::as_i64).unwrap_or(0).clamp(0, 10_000);
            let compact = args.get("compact").and_then(Value::as_bool).unwrap_or(true);
            let rows = self.query_objects(
                "select recurrence_id,status,definition_json,last_due_key,last_auto_triggered_at,updated_at from recurring_task_definitions where (?1 is null or status=?1) order by updated_at desc limit ?2 offset ?3",
                params![args.get("status").and_then(Value::as_str),limit + 1,offset],
            )?;
            let mut definitions = rows
                .into_iter()
                .map(project_recurring_definition)
                .collect::<Result<Vec<_>, _>>()?;
            let has_more = definitions.len() > limit as usize;
            definitions.truncate(limit as usize);
            if compact { definitions = definitions.iter().map(compact_recurring_definition).collect(); }
            return Ok(
                json!({"schema":"narada.task.recurring.list.v1","status":"ok","count":definitions.len(),"returned":definitions.len(),"offset":offset,"limit":limit,"has_more":has_more,"next_offset":if has_more{json!(offset + limit)}else{Value::Null},"compact":compact,"definitions":definitions}),
            );
        }
        let recurrence_id = required_string(&args, "recurrence_id")?;
        let row = connection.query_row(
            "select recurrence_id,status,definition_json,last_due_key,last_auto_triggered_at,updated_at from recurring_task_definitions where recurrence_id=?1",
            params![recurrence_id],
            |r| Ok(json!({"recurrence_id":r.get::<_,String>(0)?,"status":r.get::<_,String>(1)?,"definition_json":r.get::<_,String>(2)?,"last_due_key":r.get::<_,Option<String>>(3)?,"last_auto_triggered_at":r.get::<_,Option<String>>(4)?,"updated_at":r.get::<_,String>(5)?})),
        ).optional().map_err(db_error)?.ok_or_else(||format!("recurring_definition_not_found:{recurrence_id}"))?;
        if name == "task_lifecycle_recurring_runs" {
            let runs = self.query_objects("select run_json from recurring_task_runs where recurrence_id=?1 order by created_at desc limit ?2",params![recurrence_id,limit])?.into_iter().map(project_recurring_run).collect::<Result<Vec<_>,_>>()?;
            return Ok(
                json!({"schema":"narada.task.recurring.runs.v1","status":"ok","recurrence_id":recurrence_id,"count":runs.len(),"runs":runs}),
            );
        }
        let definition = project_recurring_definition(row)?;
        let runs = if args.get("include_runs").and_then(Value::as_bool)==Some(true) {
            self.query_objects("select run_json from recurring_task_runs where recurrence_id=?1 order by created_at desc limit ?2",params![recurrence_id,limit])?.into_iter().map(project_recurring_run).collect::<Result<Vec<_>,_>>()?
        } else { Vec::new() };
        Ok(json!({"schema":"narada.task.recurring.v1","status":"ok","definition":definition,"runs":runs}))
    }

    fn task_recurring_create(&mut self, args: Value) -> Result<Value, String> {
        let title = required_string(&args, "title")?;
        let actor = required_string(&args, "actor_agent_id")?;
        let authority = args
            .get("authority_basis")
            .cloned()
            .ok_or("authority_basis_required")?;
        let recurrence_id = format!("recurrence-{}", Uuid::new_v4());
        let status = string_arg(&args, "initial_status").unwrap_or_else(|| "active".to_string());
        let definition = json!({
            "recurrence_id":recurrence_id,
            "status":status,
            "title":title,
            "goal":args.get("goal"),
            "context":args.get("context"),
            "required_work":args.get("required_work"),
            "non_goals":args.get("non_goals"),
            "acceptance_criteria":args.get("acceptance_criteria").cloned().unwrap_or_else(||json!([])),
            "evidence_requirements":args.get("evidence_requirements").cloned().unwrap_or_else(||json!([])),
            "tags":args.get("tags").cloned().unwrap_or_else(||json!([])),
            "target_role":args.get("target_role"),
            "preferred_role":args.get("preferred_role"),
            "trigger_description":args.get("trigger_description"),
            "trigger_mode":args.get("trigger_mode").cloned().unwrap_or_else(||json!("manual")),
            "schedule_kind":args.get("schedule_kind"),
            "schedule_timezone":args.get("schedule_timezone"),
            "created_by":actor,
            "authority_basis":authority
        });
        let connection = self.connection_mut()?;
        connection.execute("insert into recurring_task_definitions(recurrence_id,status,definition_json,last_due_key,last_auto_triggered_at,updated_at) values(?1,?2,?3,null,null,?4)",params![recurrence_id,status,definition.to_string(),now()]).map_err(db_error)?;
        connection.execute("insert into recurring_task_events(event_id,recurrence_id,event_type,actor_agent_id,authority_basis_json,event_json,created_at) values(?1,?2,'created',?3,?4,?5,?6)",params![format!("recurring-event-{}",Uuid::new_v4()),recurrence_id,actor,args.get("authority_basis").cloned().unwrap_or_else(||json!(null)).to_string(),json!({"definition":definition}).to_string(),now()]).map_err(db_error)?;
        Ok(
            json!({"schema":"narada.task.recurring.create.v1","status":"created","recurrence_id":recurrence_id,"definition":definition}),
        )
    }

}
