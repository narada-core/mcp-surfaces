impl LifecycleServer {
    fn task_show(&self, args: Value) -> Result<Value, String> {
        let number = required_i64(&args, "task_number")?;
        let connection = self.connection()?;
        let lifecycle = connection
            .query_row(
                "select * from task_lifecycle where task_number=?1",
                params![number],
                |r| lifecycle_value(r),
            )
            .optional()
            .map_err(db_error)?
            .ok_or_else(|| format!("task_not_found: {number}"))?;
        let task_id = lifecycle
            .get("task_id")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let spec=connection.query_row("select title,goal_markdown,context_markdown,required_work_markdown,non_goals_markdown,acceptance_criteria_json,tags_json from task_specs where task_id=?1",params![&task_id],|r|Ok(json!({"title":r.get::<_,String>(0)?,"goal_markdown":r.get::<_,Option<String>>(1)?,"context_markdown":r.get::<_,Option<String>>(2)?,"required_work_markdown":r.get::<_,Option<String>>(3)?,"non_goals_markdown":r.get::<_,Option<String>>(4)?,"acceptance_criteria":serde_json::from_str::<Value>(&r.get::<_,String>(5)?).unwrap_or_else(|_|json!([])),"tags":serde_json::from_str::<Value>(&r.get::<_,String>(6)?).unwrap_or_else(|_|json!([]))}))).optional().map_err(db_error)?;
        let assignment=connection.query_row("select * from task_assignments where task_id=?1 and released_at is null order by claimed_at desc limit 1",params![&task_id],|r|row_to_object(r)).optional().map_err(db_error)?.unwrap_or(Value::Null);
        let tag_updates = self.query_objects(
            "select * from task_tag_updates where task_id=?1 order by updated_at desc limit 100",
            params![&task_id],
        )?;
        let observations=self.query_objects("select * from observation_artifacts where task_id=?1 order by created_at desc limit 100",params![&task_id])?;
        let dependencies=self.query_objects("select d.*,p.task_number as parent_task_number,r.task_number as required_task_number,r.status as required_status from task_dependencies d join task_lifecycle p on p.task_id=d.parent_task_id join task_lifecycle r on r.task_id=d.required_task_id where d.parent_task_id=?1 or d.required_task_id=?1 order by d.created_at",params![&task_id])?;
        let reports = self.query_objects(
            "select * from task_reports where task_id=?1 order by submitted_at desc limit 20",
            params![&task_id],
        )?;
        let legacy_review_rows = self.query_objects(
            "select review_id,reviewer_agent_id,verdict,findings_json,reviewed_at from task_reviews where task_id=?1 order by reviewed_at desc limit 100",
            params![&task_id],
        )?.into_iter().map(|row| json!({
            "review_id":row.get("review_id"),
            "reviewer_agent_id":row.get("reviewer_agent_id"),
            "verdict":row.get("verdict"),
            "reviewed_at":row.get("reviewed_at"),
            "single_operator_meta":Value::Null,
            "authority_role":"legacy_compatibility_projection",
            "primary_authority":false,
            "migration_target":"task_dependencies.task_outcomes",
            "findings":row.get("findings_json").and_then(Value::as_str).and_then(|text|serde_json::from_str::<Value>(text).ok()).unwrap_or_else(||json!([]))
        })).collect::<Vec<_>>();        let routing=connection.query_row("select preferred_role,target_role,preferred_agent_id,updated_at from narada_andrey_task_role_preferences where task_id=?1",params![&task_id],|r|Ok(json!({"preferred_role":r.get::<_,Option<String>>(0)?,"target_role":r.get::<_,Option<String>>(1)?,"preferred_agent_id":r.get::<_,Option<String>>(2)?,"updated_at":r.get::<_,String>(3)?}))).optional().map_err(db_error)?.unwrap_or(Value::Null);
        let dependency_satisfaction = self.task_dependency_satisfaction(&task_id)?;
        let result_contract = connection.query_row("select schema_id,schema_digest,schema_json,created_at from task_result_contracts where task_id=?1",params![&task_id],|r|{let schema:String=r.get(2)?;Ok(json!({"schema_id":r.get::<_,String>(0)?,"schema_digest":r.get::<_,String>(1)?,"schema":serde_json::from_str::<Value>(&schema).unwrap_or(Value::Null),"created_at":r.get::<_,String>(3)?}))}).optional().map_err(db_error)?.unwrap_or(Value::Null);
        let structured_result = connection.query_row("select result_id,report_id,schema_id,schema_digest,result_json,evidence_refs_json,validation_json,admitted_at from task_structured_results where task_id=?1",params![&task_id],|r|{let result:String=r.get(4)?;let evidence:String=r.get(5)?;let validation:String=r.get(6)?;Ok(json!({"result_id":r.get::<_,String>(0)?,"report_id":r.get::<_,String>(1)?,"schema_id":r.get::<_,String>(2)?,"schema_digest":r.get::<_,String>(3)?,"result":serde_json::from_str::<Value>(&result).unwrap_or(Value::Null),"evidence_refs":serde_json::from_str::<Value>(&evidence).unwrap_or_else(|_|json!([])),"validation":serde_json::from_str::<Value>(&validation).unwrap_or(Value::Null),"admitted_at":r.get::<_,String>(7)?}))}).optional().map_err(db_error)?.unwrap_or(Value::Null);
        let outcome_contract = connection
            .query_row(
                "select contract_id,outcome_type,allowed_outcomes_json,satisfying_outcomes_json,blocking_outcomes_json,required_fields_json,capability_requirement,created_by,created_at from task_outcome_contracts where task_id=?1 order by created_at desc limit 1",
                params![&task_id],
                |r| {
                    let allowed: String = r.get(2)?;
                    let satisfying: String = r.get(3)?;
                    let blocking: String = r.get(4)?;
                    let required: String = r.get(5)?;
                    Ok(json!({"contract_id":r.get::<_,String>(0)?,"task_id":task_id,"outcome_type":r.get::<_,String>(1)?,"allowed_outcomes":serde_json::from_str::<Value>(&allowed).unwrap_or_else(|_|json!([])),"satisfying_outcomes":serde_json::from_str::<Value>(&satisfying).unwrap_or_else(|_|json!([])),"blocking_outcomes":serde_json::from_str::<Value>(&blocking).unwrap_or_else(|_|json!([])),"required_fields":serde_json::from_str::<Value>(&required).unwrap_or_else(|_|json!([])),"capability_requirement":r.get::<_,Option<String>>(6)?,"created_by":r.get::<_,String>(7)?,"created_at":r.get::<_,String>(8)?}))
                },
            )
            .optional()
            .map_err(db_error)?
            .unwrap_or(Value::Null);
        let latest_task_outcome = connection
            .query_row(
                "select outcome_id,task_id,contract_id,agent_id,outcome,summary,findings_json,evidence_refs_json,admitted_at from task_outcomes where task_id=?1 order by admitted_at desc limit 1",
                params![&task_id],
                |r| {
                    let findings: String = r.get(6)?;
                    let evidence_refs: String = r.get(7)?;
                    Ok(json!({"outcome_id":r.get::<_,String>(0)?,"task_id":r.get::<_,String>(1)?,"contract_id":r.get::<_,String>(2)?,"agent_id":r.get::<_,String>(3)?,"outcome":r.get::<_,String>(4)?,"summary":r.get::<_,String>(5)?,"findings":serde_json::from_str::<Value>(&findings).unwrap_or_else(|_|json!([])),"evidence_refs":serde_json::from_str::<Value>(&evidence_refs).unwrap_or_else(|_|json!([])),"admitted_at":r.get::<_,String>(8)?}))
                },
            )
            .optional()
            .map_err(db_error)?
            .unwrap_or(Value::Null);
        let closure_status = if lifecycle.get("status").and_then(Value::as_str) == Some("closed") {
            "closed"
        } else {
            "open"
        };
        let body = task_file_body(&self.options.site_root, &task_id, number);
        let execution_binding = connection.query_row(
            "select binding_json,created_at,updated_at from narada_task_execution_bindings where task_id=?1",
            params![&task_id],
            |r| Ok(json!({"status":"bound","binding":serde_json::from_str::<Value>(&r.get::<_,String>(0)?).unwrap_or_else(|_|json!(null)),"created_at":r.get::<_,String>(1)?,"updated_at":r.get::<_,String>(2)?})),
        ).optional().map_err(db_error)?.unwrap_or_else(|| json!({"status":"unbound","binding":null}));
        Ok(
            json!({"status":"ok","task_number":number,"task_id":task_id,"task_ref":format!("task #{number}"),"task_reference":{"schema":"narada.task.reference.v1","task_ref":format!("task #{number}"),"task_id":lifecycle.get("task_id"),"task_number":number,"number_authority":"task_lifecycle","task_file_name":format!("{task_id}.md")},"result_contract":result_contract,"structured_result":structured_result,"lifecycle":lifecycle,"closure_authority":{"status":closure_status,"has_closure_evidence":lifecycle.get("closed_at").is_some_and(|v|!v.is_null()),"closed_at":lifecycle.get("closed_at"),"closed_by":lifecycle.get("closed_by"),"closure_mode":lifecycle.get("closure_mode")},"spec":spec,"tag_updates":tag_updates,"tag_projection":{"status":"coherent"},"routing":routing,"active_assignment":assignment,"assignment_intents":[],"observations":observations,"execution_binding":execution_binding,"current_execution_evidence":reports.first().cloned(),"legacy_review_rows":legacy_review_rows,"review_authority":{"primary_authority":"task_dependencies.task_outcomes","legacy_review_rows_authority":"compatibility_projection_only","legacy_review_row_count":legacy_review_rows.len(),"dependency_review_count":dependencies.iter().filter(|dependency|dependency.get("kind").and_then(Value::as_str)==Some("review")).count(),"compatibility_note":if legacy_review_rows.is_empty(){"No legacy review rows are present; review authority, if any, is dependency/outcome native."}else{"Legacy review rows are retained for historical readback only. Parent closure and review dependency satisfaction must use task dependency outcomes."}},"dependencies_blocking_this_task":dependencies.iter().filter(|d|d.get("required_status").and_then(Value::as_str)!=Some("closed")).cloned().collect::<Vec<_>>(),"dependency_satisfaction":dependency_satisfaction,"dependency_context":dependencies,"outcome_contract":outcome_contract,"latest_task_outcome":latest_task_outcome,"executability_posture":{"status":"unknown"},"body":body}),
        )
    }
    fn task_create_from_internal_payload(&mut self, payload: Value, payload_id: String, created_by: String) -> Result<Value,String> {
        let payload_result = self.payload_create(json!({"payload_id":payload_id,"payload":payload,"created_by":created_by}))?;
        let payload_ref = payload_result.get("ref").and_then(Value::as_str).ok_or("internal_task_payload_ref_missing")?;
        self.task_create(json!({"payload_ref":payload_ref}))
    }
    fn task_create(&mut self, args: Value) -> Result<Value, String> {
        enforce_task_create_payload_contract(&args)?;
        let site_root = self.options.site_root.clone();
        let payload = resolve_payload_args(&site_root, &args)?;
        let title = string_arg(&payload, "title").ok_or("task_lifecycle_create_title_required")?;
        let goal = string_arg(&payload, "goal").unwrap_or_else(|| title.clone());
        let required_work = normalized_text(&payload, "required_work");
        let non_goals = normalized_text(&payload, "non_goals");
        let criteria = payload
            .get("acceptance_criteria")
            .cloned()
            .unwrap_or_else(|| json!([]));
        let tags = payload.get("tags").cloned().unwrap_or_else(|| json!([]));
        let idem = string_arg(&payload, "idempotency_key")
            .unwrap_or_else(|| format!("native-create:{}", digest(&payload)));
        let request_digest = digest(&payload);
        let result = {
            let connection = self.connection_mut()?;
            if let Some(existing)=connection.query_row("select result_json,request_digest from native_task_operations where operation_key=?1",params![&idem],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?))).optional().map_err(db_error)? {
                if existing.1!=request_digest{return Err("task_operation_idempotency_conflict".to_string());}
                return serde_json::from_str(&existing.0).map_err(|e|format!("stored_result_invalid:{e}"));
            }
            let tx = connection.transaction().map_err(db_error)?;
            if let Some(existing)=tx.query_row("select task_id,task_number from task_specs where title=?1 and task_id in(select task_id from task_lifecycle)",params![&title],|r|Ok((r.get::<_,String>(0)?,r.get::<_,i64>(1)?))).optional().map_err(db_error)? {
                let value=json!({"schema":"narada.task.create.v0","status":"already_exists","task_id":existing.0,"task_number":existing.1,"title":title,"idempotency_key":idem,"recovered":false});
                tx.execute("insert or ignore into native_task_operations(operation_key,operation_kind,request_digest,result_json,created_at) values(?1,'task_create',?2,?3,?4)",params![&idem,request_digest,value.to_string(),now()]).map_err(db_error)?;
                tx.commit().map_err(db_error)?;
                return Ok(value);
            }
            let number:i64=tx.query_row("update task_number_sequence set last_allocated=last_allocated+1 where singleton=1 returning last_allocated",[],|r|r.get(0)).map_err(db_error)?;
            let task_id = format!("task-{}", Uuid::new_v4());
            let timestamp = now();
            let governed_by = string_arg(&payload, "preferred_role")
                .or_else(|| string_arg(&payload, "target_role"));
            tx.execute("insert into task_lifecycle(task_id,task_number,status,governed_by,closed_at,closed_by,closure_mode,relative_priority,priority_reason,reopened_at,reopened_by,continuation_packet_json,updated_at) values(?1,?2,'opened',?3,null,null,null,0,null,null,null,null,?4)",params![&task_id,number,governed_by,timestamp]).map_err(db_error)?;
            tx.execute("insert into task_specs(task_id,task_number,title,chapter_markdown,goal_markdown,context_markdown,required_work_markdown,non_goals_markdown,acceptance_criteria_json,dependencies_json,tags_json,updated_at) values(?1,?2,?3,null,?4,?5,?6,?7,?8,'[]',?9,?10)",params![&task_id,number,&title,&goal,string_arg(&payload,"context"),required_work,non_goals,criteria.to_string(),tags.to_string(),timestamp]).map_err(db_error)?;
            if let Some(contract) = payload.get("result_contract") {
                let schema = contract.get("schema").ok_or("task_result_contract_schema_required")?;
                if !schema.is_object() { return Err("task_result_contract_schema_object_required".to_string()); }
                let schema_id = contract.get("schema_id").and_then(Value::as_str).filter(|value| !value.trim().is_empty()).ok_or("task_result_contract_schema_id_required")?;
                let schema_digest = digest(schema);
                if contract.get("schema_digest").and_then(Value::as_str).is_some_and(|value| value != schema_digest) {
                    return Err("task_result_contract_digest_mismatch".to_string());
                }
                tx.execute("insert into task_result_contracts(task_id,schema_id,schema_digest,schema_json,created_at) values(?1,?2,?3,?4,?5)", params![&task_id,schema_id,&schema_digest,schema.to_string(),&timestamp]).map_err(db_error)?;
            }
            let execution_binding = normalize_execution_binding(&site_root, payload.get("execution_binding"), &idem)?;
            validate_execution_binding_scope(&execution_binding, &site_root)?;
            let binding_json = execution_binding.to_string();
            let correlation_key = execution_binding.get("correlation_key").and_then(Value::as_str).unwrap_or(&idem).to_string();
            tx.execute("insert into narada_task_creation_requests(idempotency_key,payload_sha256,task_id,task_number,file_path,execution_binding_json,status,created_at,updated_at) values(?1,?2,?3,?4,?5,?6,'created',?7,?7)", params![&idem,&request_digest,&task_id,number,task_file_path(&site_root,&task_id),&binding_json,&timestamp]).map_err(db_error)?;
            if execution_binding.as_object().is_some() && !execution_binding.as_object().is_some_and(|object| object.is_empty()) {
                tx.execute("insert into narada_task_execution_bindings(task_id,task_number,binding_json,correlation_key,created_at,updated_at) values(?1,?2,?3,?4,?5,?5)", params![&task_id,number,&binding_json,&correlation_key,&timestamp]).map_err(db_error)?;
            }
            if payload.get("preferred_role").is_some() || payload.get("target_role").is_some() || payload.get("preferred_agent_id").is_some() {
                tx.execute("insert into narada_andrey_task_role_preferences(task_id,preferred_role,target_role,preferred_agent_id,updated_at) values(?1,?2,?3,?4,?5)", params![&task_id,string_arg(&payload,"preferred_role"),string_arg(&payload,"target_role"),string_arg(&payload,"preferred_agent_id"),&timestamp]).map_err(db_error)?;
            }
            let event_id = format!("task-event-{}", Uuid::new_v4());
            tx.execute("insert into task_lifecycle_events(event_id,task_id,task_number,event_type,payload_json,created_at) values(?1,?2,?3,'task.created',?4,?5)",params![event_id,&task_id,number,json!({"status":"opened","revision":1,"idempotency_key":idem}).to_string(),timestamp]).map_err(db_error)?;
            let value = json!({"schema":"narada.task.create.v0","status":"created","task_number":number,"task_id":task_id,"file_path":task_file_path(&site_root,&task_id),"title":title,"tags":tags,"idempotency_key":idem,"execution_binding":execution_binding,"recovered":false,"target_role":payload.get("target_role"),"preferred_role":payload.get("preferred_role"),"follow_up":{"status":"enqueued"}});
            tx.execute("insert into native_task_operations(operation_key,operation_kind,request_digest,result_json,created_at) values(?1,'task_create',?2,?3,?4)",params![&idem,request_digest,value.to_string(),timestamp]).map_err(db_error)?;
            tx.commit().map_err(db_error)?;
            write_task_file(
                &site_root,
                &task_id,
                number,
                &title,
                &goal,
                &required_work,
                &non_goals,
                &criteria,
                &tags,
                governed_by.as_deref(),
                &idem,
            )?;
            value
        };
        Ok(result)
    }
    fn task_claim_guard(&self, task_id: &str, agent: &str) -> Result<(), String> {
        let c = self.connection()?;
        let routing: Option<(Option<String>, Option<String>, Option<String>)> = c.query_row(
            "select preferred_role,target_role,preferred_agent_id from narada_andrey_task_role_preferences where task_id=?1",
            params![task_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        ).optional().map_err(db_error)?;
        if let Some((preferred_role, target_role, preferred_agent)) = routing {
            if preferred_agent.as_deref().is_some_and(|value| value != agent) {
                return Err(format!("task_preferred_agent_mismatch:{agent}"));
            }
            if let Some(required_role) = target_role.or(preferred_role) {
                let actual_role: Option<String> = c.query_row("select role from agent_roster where agent_id=?1 and status not in ('retired','revoked')", params![agent], |r| r.get(0)).optional().map_err(db_error)?;
                if actual_role.as_deref().is_some_and(|value| value != required_role) {
                    return Err(format!("task_role_mismatch:expected_{required_role}:actual_{}", actual_role.unwrap_or_default()));
                }
            }
        }
        Ok(())
    }
    fn task_claim(&mut self, args: Value) -> Result<Value, String> {
        let number = required_i64(&args, "task_number")?;
        let agent = required_string(&args, "agent_id")?;
        let (task_id, status): (String, String) = {
            let connection = self.connection()?;
            connection
                .query_row(
                    "select task_id,status from task_lifecycle where task_number=?1",
                    params![number],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .optional()
                .map_err(db_error)?
                .ok_or_else(|| format!("task_not_found: {number}"))?
        };
        if matches!(status.as_str(), "closed" | "confirmed") {
            return Err(format!("task_not_claimable:{status}"));
        }
        self.task_claim_guard(&task_id, &agent)?;
        let connection = self.connection_mut()?;
        let active:Option<(String,String)>=connection.query_row("select assignment_id,agent_id from task_assignments where task_id=?1 and released_at is null order by claimed_at desc limit 1",params![&task_id],|r|Ok((r.get(0)?,r.get(1)?))).optional().map_err(db_error)?;
        if let Some((assignment_id, current)) = active {
            if current == agent {
                return Ok(
                    json!({"status":"already_claimed","assignment_id":assignment_id,"task_number":number,"assignment":{"agent_id":current}}),
                );
            }
            return Err("task_already_claimed".to_string());
        }
        let assignment_id = format!("assignment-{}", Uuid::new_v4());
        let timestamp = now();
        let tx = connection.transaction().map_err(db_error)?;
        tx.execute("insert into task_assignments(assignment_id,task_id,agent_id,agent_identity_ref_json,claimed_at,released_at,release_reason,intent) values(?1,?2,?3,?4,?5,null,null,'primary')",params![&assignment_id,&task_id,&agent,json!({"agent_id":agent}).to_string(),timestamp]).map_err(db_error)?;
        tx.execute(
            "update task_lifecycle set status='claimed',revision=revision+1,updated_at=?1 where task_id=?2",
            params![timestamp, &task_id],
        )
        .map_err(db_error)?;
        tx.execute("insert into task_lifecycle_events(event_id,task_id,task_number,event_type,payload_json,created_at) values(?1,?2,?3,'task.claimed',?4,?5)",params![format!("task-event-{}",Uuid::new_v4()),&task_id,number,json!({"agent_id":agent,"assignment_id":assignment_id}).to_string(),timestamp]).map_err(db_error)?;
        tx.commit().map_err(db_error)?;
        project_task_status(&self.options.site_root, &task_id, number, "claimed")?;
        Ok(
            json!({"status":"claimed","assignment_id":assignment_id,"task_number":number,"agent_id":agent}),
        )
    }
    fn task_unclaim(&mut self, args: Value) -> Result<Value, String> {
        let number = required_i64(&args, "task_number")?;
        let connection = self.connection_mut()?;
        let task_id: String = connection
            .query_row(
                "select task_id from task_lifecycle where task_number=?1",
                params![number],
                |r| r.get(0),
            )
            .optional()
            .map_err(db_error)?
            .ok_or_else(|| format!("task_not_found: {number}"))?;
        let current: Option<(String, String, String)> = connection.query_row("select assignment_id,agent_id,claimed_at from task_assignments where task_id=?1 and released_at is null order by claimed_at desc limit 1", params![&task_id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))).optional().map_err(db_error)?;
        let Some((assignment_id, current_agent, claimed_at)) = current else {
            return Ok(json!({"status":"not_claimed","task_number":number,"released":0}));
        };
        if let Some(agent) = string_arg(&args, "agent_id") {
            if agent != current_agent {
                return Ok(json!({"status":"claimed_by_other","task_number":number,"assigned_agent":current_agent,"requested_agent":agent,"assignment_id":assignment_id}));
            }
        }
        let timestamp = now();
        let reason = string_arg(&args, "reason").unwrap_or_else(|| "mcp_unclaim".to_string());
        let changed=connection.execute("update task_assignments set released_at=?1,release_reason=?2 where assignment_id=?3 and released_at is null",params![timestamp,&reason,&assignment_id]).map_err(db_error)?;
        connection.execute("update task_lifecycle set status='opened',revision=revision+1,updated_at=?1 where task_id=?2 and status='claimed'",params![timestamp,&task_id]).map_err(db_error)?;
        connection.execute("insert into task_lifecycle_events(event_id,task_id,task_number,event_type,payload_json,created_at) values(?1,?2,?3,'task.unclaimed',?4,?5)",params![format!("task-event-{}",Uuid::new_v4()),&task_id,number,json!({"reason":reason,"released":changed}).to_string(),timestamp]).map_err(db_error)?;
        project_task_status(&self.options.site_root, &task_id, number, "opened")?;
        Ok(json!({"status":"unclaimed","task_number":number,"released":changed,"assignment_id":assignment_id,"agent_id":current_agent,"claimed_at":claimed_at}))
    }
}
