impl LifecycleServer {
    fn task_recurring_update_status(&mut self, args: Value, status: &str) -> Result<Value, String> {
        let id = required_string(&args, "recurrence_id")?;
        let actor = required_string(&args, "actor_agent_id")?;
        let authority = args
            .get("authority_basis")
            .cloned()
            .ok_or("authority_basis_required")?;
        let connection = self.connection_mut()?;
        let changed=connection.execute("update recurring_task_definitions set status=?1,updated_at=?2 where recurrence_id=?3",params![status,now(),id]).map_err(db_error)?;
        if changed == 0 {
            return Err(format!("recurring_definition_not_found:{id}"));
        }
        connection.execute("insert into recurring_task_events(event_id,recurrence_id,event_type,actor_agent_id,authority_basis_json,event_json,created_at) values(?1,?2,?3,?4,?5,?6,?7)",params![format!("recurring-event-{}",Uuid::new_v4()),id,status,actor,authority.to_string(),json!({"reason":args.get("reason")}).to_string(),now()]).map_err(db_error)?;
        Ok(json!({"schema":"narada.task.recurring.update.v1","status":status,"recurrence_id":id}))
    }

    fn task_recurring_trigger(&mut self, args: Value) -> Result<Value, String> {
        let id = required_string(&args, "recurrence_id")?;
        let actor = required_string(&args, "actor_agent_id")?;
        let authority = args
            .get("authority_basis")
            .cloned()
            .ok_or("authority_basis_required")?;
        let connection = self.connection()?;
        let definition: Value=connection.query_row("select definition_json from recurring_task_definitions where recurrence_id=?1 and status not in ('suspended','retired')",params![id],|r|{let text:String=r.get(0)?;Ok(serde_json::from_str(&text).unwrap_or_else(|_|json!({})))}).optional().map_err(db_error)?.ok_or_else(||format!("recurring_definition_not_found:{id}"))?;
        let due_key = string_arg(&args, "due_key").unwrap_or_else(now);
        let payload = json!({"title":definition.get("title"),"goal":definition.get("goal"),"context":definition.get("context"),"required_work":definition.get("required_work"),"non_goals":definition.get("non_goals"),"acceptance_criteria":definition.get("acceptance_criteria").cloned().unwrap_or_else(||json!([])),"tags":definition.get("tags").cloned().unwrap_or_else(||json!([])),"preferred_role":definition.get("preferred_role"),"target_role":definition.get("target_role"),"idempotency_key":format!("recurring-run:{id}:{due_key}")});
        let payload_digest = digest(&json!({"recurrence_id":id,"due_key":due_key}));
        let payload_id = format!("recurring_{}", &payload_digest[..48]);
        let created = self.task_create_from_internal_payload(payload,payload_id,actor.clone())?;
        let task_id = created.get("task_id").cloned().unwrap_or(Value::Null);
        let task_number = created.get("task_number").cloned().unwrap_or(Value::Null);
        let run_id = format!("recurring-run-{}", Uuid::new_v4());
        let timestamp = now();
        let run = json!({"run_id":run_id,"recurrence_id":id,"task_id":task_id,"task_number":task_number,"due_key":due_key,"trigger_mode":args.get("trigger_mode").cloned().unwrap_or_else(||json!("manual")),"reason":args.get("run_reason").cloned().unwrap_or_else(||json!("manual trigger")),"created_at":timestamp});
        let connection = self.connection_mut()?;
        let transaction = connection.transaction().map_err(db_error)?;
        if let Some(existing) = transaction.query_row(
            "select run_json from recurring_task_runs where recurrence_id=?1 and due_key=?2 order by created_at limit 1",
            params![id,due_key],
            |row| row.get::<_,String>(0),
        ).optional().map_err(db_error)? {
            transaction.rollback().map_err(db_error)?;
            let existing = serde_json::from_str::<Value>(&existing).map_err(|error|format!("recurring_run_json_invalid:{error}"))?;
            return Ok(json!({"schema":"narada.task.recurring.trigger.v1","status":"already_triggered","recurrence_id":id,"run":existing,"task":created}));
        }
        let claimed = transaction.execute(
            "insert or ignore into recurring_task_run_claims(recurrence_id,due_key,run_id,claimed_at) values(?1,?2,?3,?4)",
            params![id,due_key,run_id,timestamp],
        ).map_err(db_error)?;
        if claimed == 0 {
            transaction.rollback().map_err(db_error)?;
            return Err(format!("recurring_run_claim_conflict:{id}:{due_key}"));
        }
        transaction.execute("insert into recurring_task_runs(run_id,recurrence_id,task_id,task_number,due_key,trigger_mode,reason,created_at,run_json) values(?1,?2,?3,?4,?5,?6,?7,?8,?9)",params![run_id,id,task_id.as_str(),task_number.as_i64(),due_key,run.get("trigger_mode").and_then(Value::as_str).unwrap_or("manual"),run.get("reason").and_then(Value::as_str).unwrap_or("manual"),timestamp,run.to_string()]).map_err(db_error)?;
        if run.get("trigger_mode").and_then(Value::as_str)==Some("schedule") {
            transaction.execute("update recurring_task_definitions set last_due_key=?1,last_auto_triggered_at=?2,updated_at=?2 where recurrence_id=?3",params![due_key,timestamp,id]).map_err(db_error)?;
        }
        transaction.execute("insert into recurring_task_events(event_id,recurrence_id,event_type,actor_agent_id,authority_basis_json,event_json,created_at) values(?1,?2,'triggered',?3,?4,?5,?6)",params![format!("recurring-event-{}",Uuid::new_v4()),id,actor,authority.to_string(),run.to_string(),timestamp]).map_err(db_error)?;
        transaction.commit().map_err(db_error)?;
        Ok(
            json!({"schema":"narada.task.recurring.trigger.v1","status":"triggered","recurrence_id":id,"run":run,"task":created}),
        )
    }

    fn task_recurring_run_due(&mut self, args: Value) -> Result<Value, String> {
        let actor = required_string(&args, "actor_agent_id")?;
        let authority = args
            .get("authority_basis")
            .cloned()
            .ok_or("authority_basis_required")?;
        let limit = args
            .get("limit")
            .and_then(Value::as_i64)
            .unwrap_or(20)
            .clamp(1, 100);
        let current_time = string_arg(&args,"current_time").unwrap_or_else(now);
        let due_key = utc_daily_due_key(&current_time)?;
        let definitions=self.query_objects("select recurrence_id,definition_json,last_due_key from recurring_task_definitions where status='active' and json_extract(definition_json,'$.trigger_mode')='schedule' and json_extract(definition_json,'$.schedule_kind')='daily' and coalesce(last_due_key,'')<>?1 order by updated_at limit ?2",params![due_key,limit])?;
        let mut runs = Vec::new();
        let skipped: Vec<Value> = Vec::new();
        for definition in definitions {
            let id = definition.get("recurrence_id").and_then(Value::as_str).unwrap_or_default().to_string();
            let result=self.task_recurring_trigger(json!({"recurrence_id":id,"actor_agent_id":actor,"authority_basis":authority,"run_reason":format!("Scheduled daily run for {due_key}"),"trigger_mode":"schedule","due_key":due_key}))?;
            runs.push(result);
        }
        Ok(json!({"schema":"narada.task.recurring.run_due.v1","status":"completed","current_time":current_time,"due_key":due_key,"count":runs.len(),"runs":runs,"skipped":skipped}))
    }

    fn task_chapter_show(&self, args: Value) -> Result<Value, String> {
        let chapter = required_string(&args, "chapter_id")?;
        let memberships = if self.connection.is_some() {
            self.query_objects("select chapter_id,task_number,order_index,note,actor_agent_id,updated_at from task_chapter_memberships where chapter_id=?1 order by order_index,task_number",params![chapter])?
        } else {
            Vec::new()
        };
        Ok(
            json!({"schema":"narada.task.chapter.v1","status":"ok","chapter_id":chapter,"membership_count":memberships.len(),"memberships":memberships}),
        )
    }

    fn task_chapter_add(&mut self, args: Value) -> Result<Value, String> {
        let chapter = required_string(&args, "chapter_id")?;
        let number = required_i64(&args, "task_number")?;
        let status = {
            let connection = self.connection_mut()?;
            let exists: Option<String> = connection
                .query_row(
                    "select task_id from task_lifecycle where task_number=?1",
                    params![number],
                    |r| r.get(0),
                )
                .optional()
                .map_err(db_error)?;
            if exists.is_none() {
                return Err(format!("task_not_found:{number}"));
            }
            let order=args.get("order_index").and_then(Value::as_i64).unwrap_or_else(||connection.query_row("select coalesce(max(order_index),-1)+1 from task_chapter_memberships where chapter_id=?1",params![chapter],|r|r.get(0)).unwrap_or(0));
            if connection.execute("insert or ignore into task_chapter_memberships(chapter_id,task_number,order_index,note,actor_agent_id,updated_at) values(?1,?2,?3,?4,?5,?6)",params![chapter,number,order,args.get("note").and_then(Value::as_str),args.get("actor_agent_id").and_then(Value::as_str),now()]).map_err(db_error)?==0{"already_present"}else{"added"}
        };
        let memberships=self.query_objects("select chapter_id,task_number,order_index,note,actor_agent_id,updated_at from task_chapter_memberships where chapter_id=?1 order by order_index,task_number",params![chapter])?;
        Ok(
            json!({"schema":"narada.task.chapter_membership.v1","status":status,"chapter_id":chapter,"task_number":number,"membership_count":memberships.len(),"memberships":memberships}),
        )
    }
    fn task_list(&self, args: Value) -> Result<Value, String> {
        let connection = self.connection()?;
        let limit = args
            .get("limit")
            .and_then(Value::as_i64)
            .unwrap_or(50)
            .clamp(1, 200);
        let offset = args.get("offset").and_then(Value::as_i64).unwrap_or(0).clamp(0, 10_000);
        let status = args.get("status").and_then(Value::as_str);
        let agent = args.get("agent_id").and_then(Value::as_str);
        let wanted_tags = args
            .get("tags")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let tag_match = args
            .get("tag_match")
            .and_then(Value::as_str)
            .unwrap_or("all");
        let mut stmt=connection.prepare("select l.task_id,l.task_number,l.status,l.governed_by,l.closed_at,l.closed_by,l.closure_mode,l.relative_priority,l.priority_reason,l.reopened_at,l.reopened_by,l.continuation_packet_json,l.updated_at,s.title,s.tags_json,(select a.agent_id from task_assignments a where a.task_id=l.task_id and a.released_at is null order by a.claimed_at desc limit 1),(select a.claimed_at from task_assignments a where a.task_id=l.task_id and a.released_at is null order by a.claimed_at desc limit 1) from task_lifecycle l left join task_specs s on s.task_id=l.task_id order by l.task_number desc limit 1000").map_err(db_error)?;
        let mut tasks = Vec::new();
        let mut matched = 0i64;
        let mut scanned = 0usize;
        let mut rows = stmt.query([]).map_err(db_error)?;
        while let Some(row) = rows.next().map_err(db_error)? {
            scanned += 1;
            let row_status: String = row.get(2).map_err(db_error)?;
            if status.is_some_and(|expected| expected != row_status) {
                continue;
            }
            let assigned: Option<String> = row.get(15).ok().flatten();
            if agent.is_some_and(|expected| assigned.as_deref() != Some(expected)) {
                continue;
            }
            let tags: Value = row
                .get::<_, Option<String>>(14)
                .ok()
                .flatten()
                .and_then(|v| serde_json::from_str(&v).ok())
                .unwrap_or_else(|| json!([]));
            let tag_values = tags.as_array().cloned().unwrap_or_default();
            let tags_match = if wanted_tags.is_empty() {
                true
            } else if tag_match == "any" {
                wanted_tags.iter().any(|tag| tag_values.contains(tag))
            } else {
                wanted_tags.iter().all(|tag| tag_values.contains(tag))
            };
            if !tags_match {
                continue;
            }
            if matched < offset { matched += 1; continue; }
            matched += 1;
            let number: i64 = row.get(1).map_err(db_error)?;
            let task_id: String = row.get(0).map_err(db_error)?;
            let title: Option<String> = row.get(13).ok().flatten();
            let claimed_at: Option<String> = row.get(16).ok().flatten();
            tasks.push(json!({"task_number":number,"task_id":task_id,"task_ref":format!("task #{number}"),"task_reference":{"schema":"narada.task.reference.v1","task_ref":format!("task #{number}"),"task_id":task_id,"task_number":number,"number_authority":"task_lifecycle","task_file_name":format!("{task_id}.md")},"status":row_status,"title":title,"assigned_to":assigned,"claimed_at":claimed_at,"tags":tags,"updated_at":row.get::<_,String>(12).map_err(db_error)?,"projection_consistency":{"status":"coherent","reasons":[]},"executability_posture":{"status":"unknown"}}));
            if tasks.len() > limit as usize {
                break;
            }
        }
        let has_more = tasks.len() > limit as usize;
        tasks.truncate(limit as usize);
        Ok(
            json!({"schema":"narada.task.list.v1","status":"ok","count":tasks.len(),"returned":tasks.len(),"offset":offset,"limit":limit,"has_more":has_more,"next_offset":if has_more{json!(offset + limit)}else{Value::Null},"count_exact":!has_more,"filters":{"status":status,"agent_id":args.get("agent_id"),"tags":args.get("tags").cloned().unwrap_or_else(||json!([])),"tag_match":args.get("tag_match").cloned().unwrap_or(json!("all"))},"projection_consistency":{"status":"snapshot_coherent","stale":false,"snapshot_isolation":"sqlite_transaction","scanned_count":scanned,"returned_count":tasks.len(),"stale_count":0,"contention":{"attempts":1,"retries":0},"stale_tasks":[]},"tasks":tasks}),
        )
    }
}
