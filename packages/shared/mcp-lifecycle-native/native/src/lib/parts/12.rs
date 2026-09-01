impl LifecycleServer {
    fn task_dependency_satisfaction(&self, parent_task_id: &str) -> Result<Value, String> {
        let evaluated_at = now();
        let rows = self.query_objects("select dependency_id,parent_task_id,required_task_id,kind,satisfying_outcomes_json from task_dependencies where parent_task_id=?1 order by created_at", params![parent_task_id])?;
        let connection = self.connection()?;
        let parse_list = |value: Option<&str>| -> Vec<String> {
            value.and_then(|text| serde_json::from_str::<Value>(text).ok()).and_then(|parsed| parsed.as_array().cloned()).unwrap_or_default().into_iter().filter_map(|item| item.as_str().map(ToString::to_string)).collect()
        };
        let mut dependencies = Vec::new();
        for row in rows {
            let dependency_id = row.get("dependency_id").and_then(Value::as_str).unwrap_or_default().to_string();
            let parent_id = row.get("parent_task_id").and_then(Value::as_str).unwrap_or_default().to_string();
            let required_id = row.get("required_task_id").and_then(Value::as_str).unwrap_or_default().to_string();
            let kind = row.get("kind").and_then(Value::as_str).unwrap_or_default().to_string();
            let satisfying = parse_list(row.get("satisfying_outcomes_json").and_then(Value::as_str));
            let latest: Option<(String,String)> = connection.query_row("select outcome_id,outcome from task_outcomes where task_id=?1 order by admitted_at desc limit 1", params![&required_id], |r| Ok((r.get(0)?,r.get(1)?))).optional().map_err(db_error)?;
            let latest_outcome_id = latest.as_ref().map(|value| value.0.clone());
            let latest_outcome = latest.as_ref().map(|value| value.1.clone());
            let blocking_json: Option<String> = connection.query_row("select blocking_outcomes_json from task_outcome_contracts where task_id=?1 order by created_at desc limit 1", params![&required_id], |r| r.get(0)).optional().map_err(db_error)?;
            let blocking = parse_list(blocking_json.as_deref());
            let state = match latest_outcome.as_deref() {
                None => "missing_outcome",
                Some(value) if satisfying.iter().any(|item| item == value) => "satisfied",
                Some(value) if blocking.iter().any(|item| item == value) => "blocking_outcome",
                Some(_) => "unsatisfying_outcome",
            };
            let disposition = if let Some(outcome_id) = latest_outcome_id.as_deref() {
                connection.query_row("select disposition_id,kind,status,target_task_id,routed_obligation_id,summary,created_by,created_at from task_dependency_dispositions where dependency_id=?1 and required_outcome_id=?2 order by created_at desc limit 1", params![&dependency_id,outcome_id], |r| Ok(json!({"disposition_id":r.get::<_,String>(0)?,"kind":r.get::<_,String>(1)?,"status":r.get::<_,String>(2)?,"target_task_id":r.get::<_,Option<String>>(3)?,"routed_obligation_id":r.get::<_,Option<String>>(4)?,"summary":r.get::<_,String>(5)?,"created_by":r.get::<_,String>(6)?,"created_at":r.get::<_,String>(7)?}))).optional().map_err(db_error)?
            } else { None };
            let disposition_accepted = disposition.as_ref().map(|item| {
                let disposition_kind = item.get("kind").and_then(Value::as_str).unwrap_or_default();
                let disposition_status = item.get("status").and_then(Value::as_str).unwrap_or_default();
                disposition_status != "superseded" && match disposition_kind {
                    "operator_deferred" | "out_of_scope_or_rejected" => matches!(disposition_status,"deferred" | "resolved"),
                    _ => matches!(disposition_status,"open" | "resolved"),
                }
            }).unwrap_or(false);
            let outcome_satisfied = latest_outcome.as_ref().is_some_and(|value| satisfying.iter().any(|item| item == value));
            let satisfied = outcome_satisfied || (state == "blocking_outcome" && disposition_accepted);
            let blocking_reason = if satisfied {Value::Null} else if state == "missing_outcome" {json!(format!("dependency {dependency_id} has no admitted outcome"))} else if state == "blocking_outcome" {json!(format!("latest outcome {} blocks dependency {dependency_id} and requires explicit disposition",latest_outcome.as_deref().unwrap_or("unknown")))} else {json!(format!("latest outcome {} does not satisfy dependency {dependency_id}",latest_outcome.as_deref().unwrap_or("unknown")))};
            dependencies.push(json!({"dependency_id":dependency_id,"parent_task_id":parent_id,"required_task_id":required_id,"required_outcome_id":latest_outcome_id,"dependency_kind":kind,"satisfying_outcomes":satisfying,"blocking_outcomes":blocking,"latest_outcome":latest_outcome,"satisfied":satisfied,"state":state,"disposition_required":state == "blocking_outcome" && !disposition_accepted,"latest_disposition":disposition,"conflict_policy_evidence":Value::Null,"blocking_reason":blocking_reason,"remediation_options":json!([]),"evaluated_at":evaluated_at.clone()}));
        }
        let satisfied_count = dependencies.iter().filter(|item| item.get("satisfied").and_then(Value::as_bool) == Some(true)).count();
        let dependency_count = dependencies.len();
        Ok(json!({"schema":"narada.task.dependency_satisfaction.v0","parent_task_id":parent_task_id,"evaluated_at":evaluated_at,"dependency_count":dependency_count,"satisfied_count":satisfied_count,"unsatisfied_count":dependency_count.saturating_sub(satisfied_count),"all_satisfied":satisfied_count == dependency_count,"dependencies":dependencies}))
    }

    fn task_closeout(&mut self, args: Value) -> Result<Value, String> {
        let number = required_i64(&args, "task_number")?;
        let agent = required_string(&args, "agent_id")?;
        let site_root = self.options.site_root.clone();
        let summary = string_arg(&args, "summary");
        let disposition_only = args.get("disposition").is_some()
            && args.get("finish").and_then(Value::as_bool) != Some(true);
        if args.get("dry_run").and_then(Value::as_bool) == Some(true) {
            return Ok(
                json!({"status":"planned","task_number":number,"agent_id":agent,"notes_written":false,"changed_files":args.get("changed_files").cloned().unwrap_or_else(||json!([]))}),
            );
        }
        let (task_id, status) = {
            let connection = self.connection()?;
            connection
                .query_row(
                    "select task_id,status from task_lifecycle where task_number=?1",
                    params![number],
                    |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)),
                )
                .optional()
                .map_err(db_error)?
                .ok_or_else(|| format!("task_not_found: {number}"))?
        };
        let wants_close = args.get("mode").and_then(Value::as_str).is_some();
        let admission_count: i64 = {
            let connection = self.connection()?;
            connection.query_row("select count(*) from evidence_admission_results where task_id=?1 and verdict='admitted'",params![&task_id],|r|r.get(0)).map_err(db_error)?
        };
        if wants_close && admission_count == 0 {
            return Ok(
                json!({"status":"blocked","task_number":number,"new_status":status,"close_action":"blocked","close_blockers":["evidence_admission_required"],"evidence_preflight":{"status":"blocked","next_action":"task_lifecycle_admit_evidence"}}),
            );
        }
        if wants_close {
            let dependency_satisfaction = self.task_dependency_satisfaction(&task_id)?;
            if dependency_satisfaction.get("all_satisfied").and_then(Value::as_bool) == Some(false) {
                return Ok(json!({
                    "status":"blocked",
                    "error":"task_close_dependencies_unsatisfied",
                    "close_action":"blocked",
                    "close_blocked":true,
                    "close_blockers":dependency_satisfaction.get("dependencies").cloned().unwrap_or_else(||json!([])),
                    "task_number":number,
                    "task_id":task_id,
                    "schema":"narada.task.mcp.close.dependency_satisfaction_gate.v0",
                    "dependency_satisfaction":dependency_satisfaction,
                    "remediation":"Complete each required dependency task with an admitted satisfying outcome before closing the parent task.",
                    "next_action":"Complete each required dependency task with an admitted satisfying outcome before closing the parent task."
                }));
            }
        }
        if let Some(text) = summary.as_deref() {
            append_task_body(&site_root, &task_id, number, text)?;
        }
        if disposition_only {
            return Ok(json!({
                "status":"prepared",
                "schema":"narada.task.mcp.disposition_closeout.v0",
                "task_number":number,
                "task_id":task_id,
                "notes_written":summary.is_some(),
                "changed_files":args.get("changed_files").cloned().unwrap_or_else(||json!([])),
                "disposition":args.get("disposition"),
                "finish_result":Value::Null,
            }));
        }
        let new_status = if wants_close { "closed" } else { "in_review" };
        let mode = args
            .get("mode")
            .and_then(Value::as_str)
            .unwrap_or("prepared");
        let timestamp = now();
        let connection = self.connection_mut()?;
        connection.execute("update task_lifecycle set status=?1,closed_at=case when ?1='closed' then ?2 else closed_at end,closed_by=case when ?1='closed' then ?3 else closed_by end,closure_mode=case when ?1='closed' then ?4 else closure_mode end,updated_at=?2 where task_id=?5",params![new_status,timestamp,&agent,mode,&task_id]).map_err(db_error)?;
        connection.execute("insert into task_lifecycle_events(event_id,task_id,task_number,event_type,payload_json,created_at) values(?1,?2,?3,?4,?5,?6)",params![format!("task-event-{}",Uuid::new_v4()),&task_id,number,if new_status=="closed"{"task.closed"}else{"task.closeout.prepared"},json!({"agent_id":agent,"summary":summary,"mode":mode,"previous_status":status}).to_string(),timestamp]).map_err(db_error)?;
        Ok(
            json!({"status":if new_status=="closed"{"success"}else{"prepared"},"new_status":new_status,"task_number":number,"task_id":task_id,"notes_written":summary.is_some(),"changed_files":args.get("changed_files").cloned().unwrap_or_else(||json!([])),"closure_mode":if new_status=="closed"{json!(mode)}else{Value::Null},"close_action":if new_status=="closed"{"closed"}else{"prepared"}}),
        )
    }

    fn task_review(&mut self, args: Value) -> Result<Value, String> {
        let number = required_i64(&args, "task_number")?;
        let agent = required_string(&args, "agent_id")?;
        let verdict = required_string(&args, "verdict")?;
        let (task_id, status) = {
            let c = self.connection()?;
            c.query_row(
                "select task_id,status from task_lifecycle where task_number=?1",
                params![number],
                |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)),
            )
            .optional()
            .map_err(db_error)?
            .ok_or_else(|| format!("task_not_found:{number}"))?
        };
        let review_id = format!("review-{}", Uuid::new_v4());
        let timestamp = now();
        let findings = args.get("findings").cloned().unwrap_or_else(|| json!([]));
        let c = self.connection_mut()?;
        c.execute("insert into task_reviews(review_id,task_id,reviewer_agent_id,verdict,findings_json,reviewed_at) values(?1,?2,?3,?4,?5,?6)",params![&review_id,&task_id,&agent,&verdict,findings.to_string(),timestamp]).map_err(db_error)?;
        c.execute("insert into task_lifecycle_events(event_id,task_id,task_number,event_type,payload_json,created_at) values(?1,?2,?3,'task.review.recorded',?4,?5)",params![format!("task-event-{}",Uuid::new_v4()),&task_id,number,json!({"review_id":review_id,"verdict":verdict,"findings":findings}).to_string(),timestamp]).map_err(db_error)?;
        Ok(
            json!({"schema":"narada.task.review.v1","status":"recorded","review_id":review_id,"task_number":number,"task_id":task_id,"verdict":verdict,"findings":findings,"previous_status":status,"completion_mode":"review"}),
        )
    }

    fn task_evidence_supersede(&mut self, args: Value) -> Result<Value, String> {
        let number = required_i64(&args, "task_number")?;
        let agent = required_string(&args, "agent_id")?;
        let supersedes = required_string(&args, "supersedes_report_id")?;
        let artifact = required_string(&args, "artifact_uri")?;
        let summary = required_string(&args, "summary")?;
        let verification = required_string(&args, "verification_summary")?;
        let (task_id, exists) = {
            let c = self.connection()?;
            let task_id: String = c
                .query_row(
                    "select task_id from task_lifecycle where task_number=?1",
                    params![number],
                    |r| r.get(0),
                )
                .optional()
                .map_err(db_error)?
                .ok_or_else(|| format!("task_not_found:{number}"))?;
            let exists: bool = c
                .query_row(
                    "select count(*) from task_reports where report_id=?1 and task_id=?2",
                    params![supersedes, &task_id],
                    |r| r.get::<_, i64>(0),
                )
                .map_err(db_error)?
                > 0;
            (task_id, exists)
        };
        if !exists {
            return Err(format!("report_not_found:{supersedes}"));
        }
        let id = format!("artifact-{}", Uuid::new_v4());
        let admitted = json!({"artifact_uri":artifact,"supersedes_report_id":supersedes,"summary":summary,"verification_summary":verification});
        let timestamp = now();
        let c = self.connection_mut()?;
        c.execute("insert into observation_artifacts(artifact_id,artifact_type,source_operator,task_id,task_number,agent_id,artifact_uri,digest,admitted_view_json,created_at) values(?1,'evidence_supersession',?2,?3,?4,?5,?6,?7,?8,?9)",params![&id,&agent,&task_id,number,&agent,&artifact,digest(&admitted),admitted.to_string(),timestamp]).map_err(db_error)?;
        c.execute("insert into task_lifecycle_events(event_id,task_id,task_number,event_type,payload_json,created_at) values(?1,?2,?3,'task.evidence.superseded',?4,?5)",params![format!("task-event-{}",Uuid::new_v4()),&task_id,number,admitted.to_string(),timestamp]).map_err(db_error)?;
        Ok(
            json!({"schema":"narada.task.evidence_supersede.v1","status":"admitted","artifact_id":id,"task_number":number,"supersedes_report_id":supersedes,"artifact_uri":artifact}),
        )
    }

    fn task_compatibility_reconcile(&mut self, args: Value) -> Result<Value, String> {
        let agent = required_string(&args, "agent_id")?;
        let dry_run = args.get("dry_run").and_then(Value::as_bool) == Some(true);
        let limit = args
            .get("limit")
            .and_then(Value::as_i64)
            .unwrap_or(25)
            .clamp(1, 100);
        let numbers = args
            .get("task_numbers")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let candidates = if numbers.is_empty() {
            self.query_objects("select task_number,status from task_lifecycle where status in ('in_review','closed','confirmed') order by task_number desc limit ?1",params![limit])?
        } else {
            numbers
                .into_iter()
                .filter_map(|value| value.as_i64())
                .map(|n| json!({"task_number":n}))
                .collect::<Vec<_>>()
        };
        if !dry_run {
            return Ok(
                json!({"schema":"narada.task.compatibility_reconcile.v1","status":"refused","code":"native_compatibility_reconcile_requires_explicit_repair_policy","agent_id":agent,"dry_run":false,"scanned":candidates.len(),"repaired":0,"candidates":candidates}),
            );
        }
        Ok(
            json!({"schema":"narada.task.compatibility_reconcile.v1","status":"planned","agent_id":agent,"dry_run":true,"scanned":candidates.len(),"repaired":0,"candidates":candidates}),
        )
    }
    fn task_tags_update(&mut self, args: Value) -> Result<Value, String> {
        let number = required_i64(&args, "task_number")?;
        let agent = required_string(&args, "agent_id")?;
        let reason = required_string(&args, "reason")?;
        let tags = args.get("tags").cloned().unwrap_or_else(|| json!([]));
        let connection = self.connection_mut()?;
        let (task_id, previous): (String, String) = connection
            .query_row(
                "select task_id, tags_json from task_specs where task_number=?1",
                params![number],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()
            .map_err(db_error)?
            .ok_or_else(|| format!("task_not_found: {number}"))?;
        connection
            .execute(
                "update task_specs set tags_json=?1, updated_at=?2 where task_id=?3",
                params![tags.to_string(), now(), task_id],
            )
            .map_err(db_error)?;
        connection.execute("insert into task_tag_updates(update_id,task_id,task_number,actor_agent_id,previous_tags_json,new_tags_json,reason,updated_at) values(?1,?2,?3,?4,?5,?6,?7,?8)", params![format!("tag-update-{}", Uuid::new_v4()), task_id, number, agent, previous, tags.to_string(), reason, now()]).map_err(db_error)?;
        Ok(json!({"status":"updated","task_number":number,"tags":tags}))
    }

}
