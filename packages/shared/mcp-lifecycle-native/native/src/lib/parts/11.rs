impl LifecycleServer {
    fn task_finish(&mut self, args: Value) -> Result<Value, String> {
        let number = required_i64(&args, "task_number")?;
        let agent = required_string(&args, "agent_id")?;
        let summary =
            string_arg(&args, "summary").ok_or("task_lifecycle_finish_summary_required")?;
        if summary.trim().is_empty() {
            return Err("task_lifecycle_finish_summary_required".to_string());
        }
        let no_files = args.get("no_files_changed").and_then(Value::as_bool) == Some(true);
        let changed_files = args
            .get("changed_files")
            .cloned()
            .unwrap_or_else(|| json!([]));
        let verification = args
            .get("verification")
            .cloned()
            .or_else(|| args.get("verification_summary").cloned())
            .unwrap_or_else(|| json!({}));
        if !no_files
            && changed_files
                .as_array()
                .map(|v| v.is_empty())
                .unwrap_or(true)
            && verification
                .as_object()
                .map(|v| v.is_empty())
                .unwrap_or(true)
        {
            return Err("task_lifecycle_finish_evidence_required".to_string());
        }
        let (task_id, status, assignment_id, result_contract) = {
            let connection = self.connection()?;
            let task = connection
                .query_row(
                    "select task_id,status from task_lifecycle where task_number=?1",
                    params![number],
                    |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)),
                )
                .optional()
                .map_err(db_error)?
                .ok_or_else(|| format!("task_not_found: {number}"))?;
            let assignment=connection.query_row("select assignment_id from task_assignments where task_id=?1 and agent_id=?2 and released_at is null order by claimed_at desc limit 1",params![&task.0,&agent],|r|r.get::<_,String>(0)).optional().map_err(db_error)?;
            let contract=connection.query_row("select schema_id,schema_digest,schema_json from task_result_contracts where task_id=?1",params![&task.0],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?))).optional().map_err(db_error)?;
            (task.0, task.1, assignment, contract)
        };
        if !matches!(
            status.as_str(),
            "claimed" | "in_progress" | "opened" | "needs_continuation" | "in_review"
        ) {
            return Err(format!("task_lifecycle_finish_state_refused:{status}"));
        }
        if assignment_id.is_none() {
            return Err("task_lifecycle_finish_claim_required".to_string());
        }
        let structured_result = args.get("result").cloned();
        let delegated_schema_digest = string_arg(&args, "result_schema_digest");
        let result_validation = if let Some((schema_id, schema_digest, schema_json)) = result_contract.as_ref() {
            if delegated_schema_digest.as_deref().is_some_and(|value| value != schema_digest) {
                return Err(format!("task_result_schema_digest_mismatch:expected={schema_digest}:observed={}", delegated_schema_digest.unwrap_or_default()));
            }
            let value = structured_result.as_ref().ok_or("task_result_required:/result")?;
            let schema: Value = serde_json::from_str(schema_json).map_err(|error| format!("task_result_contract_invalid:{error}"))?;
            let mut errors = Vec::new();
            validate_result_schema(&schema, value, "/result", &mut errors);
            if !errors.is_empty() { return Err(format!("task_result_validation_failed:{}", Value::Array(errors))); }
            Some(json!({"status":"valid","schema_id":schema_id,"schema_digest":schema_digest,"errors":[]}))
        } else { None };
        let report_id = format!("report-{}", Uuid::new_v4());
        let timestamp = now();
        let operation_key = string_arg(&args, "idempotency_key")
            .unwrap_or_else(|| format!("task-finish:{number}:{agent}:{}", digest(&args)));
        let outcome = string_arg(&args, "outcome");
        let report_json = json!({"report_id":report_id,"task_number":number,"task_id":task_id,"agent_id":agent,"summary":summary,"changed_files":changed_files,"verification":verification,"outcome":outcome.clone().map(Value::String).unwrap_or(Value::Null),"findings":args.get("findings"),"evidence_refs":args.get("evidence_refs"),"result":structured_result.clone(),"result_validation":result_validation.clone()});
        let connection = self.connection_mut()?;
        if let Some(stored) = connection
            .query_row(
                "select result_json from native_task_operations where operation_key=?1",
                params![&operation_key],
                |r| r.get::<_, String>(0),
            )
            .optional()
            .map_err(db_error)?
        {
            return serde_json::from_str(&stored).map_err(|e| format!("stored_result_invalid:{e}"));
        }
        let tx = connection.transaction().map_err(db_error)?;
        tx.execute("insert into task_reports(report_id,task_id,agent_id,agent_identity_ref_json,summary,changed_files_json,verification_json,directive_id,submitted_at) values(?1,?2,?3,?4,?5,?6,?7,?8,?9)",params![&report_id,&task_id,&agent,json!({"agent_id":agent}).to_string(),&summary,changed_files.to_string(),verification.to_string(),string_arg(&args,"directive_id"),timestamp]).map_err(db_error)?;
        tx.execute("insert into task_report_records(report_id,task_id,assignment_id,agent_id,agent_identity_ref_json,reported_at,report_json) values(?1,?2,?3,?4,?5,?6,?7)",params![&report_id,&task_id,assignment_id, &agent,json!({"agent_id":agent}).to_string(),timestamp,report_json.to_string()]).map_err(db_error)?;
        if let (Some((schema_id, schema_digest, _)), Some(value), Some(validation)) = (result_contract.as_ref(), structured_result.as_ref(), result_validation.as_ref()) {
            tx.execute("insert into task_structured_results(result_id,task_id,report_id,schema_id,schema_digest,result_json,evidence_refs_json,validation_json,admitted_at) values(?1,?2,?3,?4,?5,?6,?7,?8,?9)",params![format!("result-{}",Uuid::new_v4()),&task_id,&report_id,schema_id,schema_digest,value.to_string(),args.get("evidence_refs").cloned().unwrap_or_else(||json!([])).to_string(),validation.to_string(),&timestamp]).map_err(db_error)?;
        }
        let existing_contract: Option<(String,String,String,String,String,String,Option<String>,String,String)> = tx.query_row("select contract_id,outcome_type,allowed_outcomes_json,satisfying_outcomes_json,blocking_outcomes_json,required_fields_json,capability_requirement,created_by,created_at from task_outcome_contracts where task_id=?1 order by created_at desc limit 1", params![&task_id], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?,r.get(6)?,r.get(7)?,r.get(8)?))).optional().map_err(db_error)?;
        let (contract_id, contract_json, allowed_outcomes, created_contract) = if let Some(row) = existing_contract {
            let allowed = serde_json::from_str::<Value>(&row.2).ok().and_then(|value| value.as_array().cloned()).unwrap_or_default().into_iter().filter_map(|value| value.as_str().map(ToString::to_string)).collect::<Vec<_>>();
            let value = json!({"contract_id":row.0,"task_id":task_id,"outcome_type":row.1,"allowed_outcomes_json":row.2,"satisfying_outcomes_json":row.3,"blocking_outcomes_json":row.4,"required_fields_json":row.5,"capability_requirement":row.6,"created_by":row.7,"created_at":row.8});
            (row.0, value, allowed, false)
        } else {
            let id = format!("contract-completion-{task_id}");
            let allowed_json = json!(["completed"]).to_string();
            let required_json = json!(["summary"]).to_string();
            tx.execute("insert into task_outcome_contracts(contract_id,task_id,outcome_type,allowed_outcomes_json,satisfying_outcomes_json,blocking_outcomes_json,required_fields_json,capability_requirement,created_by,created_at) values(?1,?2,?3,?4,?5,?6,?7,null,?8,?9)", params![&id,&task_id,"completion",&allowed_json,&allowed_json,"[]",&required_json,&agent,&timestamp]).map_err(db_error)?;
            (id.clone(),json!({"contract_id":id,"task_id":task_id,"outcome_type":"completion","allowed_outcomes_json":allowed_json,"satisfying_outcomes_json":allowed_json,"blocking_outcomes_json":"[]","required_fields_json":required_json,"capability_requirement":Value::Null,"created_by":agent,"created_at":timestamp}),vec!["completed".to_string()],true)
        };
        let reviewer = string_arg(&args, "reviewer");
        let mut task_outcome: Option<Value> = None;
        if let Some(outcome_value) = outcome.as_deref() {
            if !allowed_outcomes.iter().any(|allowed| allowed == outcome_value) {
                return Err(format!("outcome_not_allowed:{outcome_value}"));
            }
        }
        if outcome.is_some() || (created_contract && reviewer.is_some()) {
            let outcome_value = outcome.clone().unwrap_or_else(|| "completed".to_string());
            let outcome_id = format!("outcome_{}", Uuid::new_v4());
            let findings_json = args.get("findings").cloned().unwrap_or_else(||json!([])).to_string();
            let evidence_refs_json = args.get("evidence_refs").cloned().unwrap_or_else(||json!([])).to_string();
            tx.execute("insert into task_outcomes(outcome_id,task_id,contract_id,agent_id,outcome,summary,findings_json,evidence_refs_json,admitted_at) values(?1,?2,?3,?4,?5,?6,?7,?8,?9)", params![&outcome_id,&task_id,&contract_id,&agent,&outcome_value,&summary,&findings_json,&evidence_refs_json,&timestamp]).map_err(db_error)?;
            task_outcome = Some(json!({"outcome_id":outcome_id,"task_id":task_id,"contract_id":contract_id,"agent_id":agent,"outcome":outcome_value,"summary":summary,"findings_json":findings_json,"evidence_refs_json":evidence_refs_json,"admitted_at":timestamp}));
        }

        let mut review_dependency: Option<Value> = None;
        let mut review_file: Option<(String, i64)> = None;
        if let Some(reviewer_id) = reviewer.as_deref() {
            let review_number: i64 = tx.query_row("update task_number_sequence set last_allocated=last_allocated+1 where singleton=1 returning last_allocated",[],|r|r.get(0)).map_err(db_error)?;
            let review_task_id = format!("review-{}", Uuid::new_v4());
            let review_contract_id = format!("contract-review-{review_task_id}");
            let dependency_id = format!("dep-review-{task_id}-{review_task_id}");
            let review_title = format!("Review task #{number}");
            tx.execute("insert into task_lifecycle(task_id,task_number,status,governed_by,closed_at,closed_by,closure_mode,relative_priority,priority_reason,reopened_at,reopened_by,continuation_packet_json,updated_at) values(?1,?2,?3,?4,null,null,null,0,null,null,null,null,?5)",params![&review_task_id,review_number,"opened",reviewer_id,timestamp]).map_err(db_error)?;
            tx.execute("insert into task_specs(task_id,task_number,title,chapter_markdown,goal_markdown,context_markdown,required_work_markdown,non_goals_markdown,acceptance_criteria_json,dependencies_json,tags_json,updated_at) values(?1,?2,?3,null,?4,?5,?6,?7,?8,?9,?10,?11)",params![&review_task_id,review_number,&review_title,format!("Review the submitted work for task #{number}."),format!("Review outcome for task #{number}."),"Admit an accepted or rejected review outcome.","Do not mutate the reviewed work.",json!(["A structured review outcome is admitted."]).to_string(),json!([]).to_string(),json!([]).to_string(),timestamp]).map_err(db_error)?;
            tx.execute("insert into task_outcome_contracts(contract_id,task_id,outcome_type,allowed_outcomes_json,satisfying_outcomes_json,blocking_outcomes_json,required_fields_json,capability_requirement,created_by,created_at) values(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",params![&review_contract_id,&review_task_id,"review",json!(["accepted","accepted_with_notes","rejected"]).to_string(),json!(["accepted","accepted_with_notes"]).to_string(),json!(["rejected"]).to_string(),json!(["summary"]).to_string(),"architect_as_reviewer",&agent,timestamp]).map_err(db_error)?;
            tx.execute("insert into task_dependencies(dependency_id,parent_task_id,required_task_id,kind,satisfying_outcomes_json,status,created_by,created_at) values(?1,?2,?3,?4,?5,?6,?7,?8)",params![&dependency_id,&task_id,&review_task_id,"review",json!(["accepted","accepted_with_notes"]).to_string(),"open",&agent,timestamp]).map_err(db_error)?;
            tx.execute("update task_lifecycle set status=?1,revision=revision+1,updated_at=?2 where task_id=?3",params!["awaiting_dependencies",timestamp,&task_id]).map_err(db_error)?;
            review_dependency = Some(json!({"status":"admitted","dependency_id":dependency_id,"parent_task_id":task_id.clone(),"parent_task_number":number,"required_task_id":review_task_id.clone(),"required_task_number":review_number,"dependency_kind":"review","reviewer":reviewer_id,"outcome_contract":{"contract_id":review_contract_id,"allowed_outcomes":["accepted","accepted_with_notes","rejected"],"satisfying_outcomes":["accepted","accepted_with_notes"]}}));
            review_file = Some((review_task_id, review_number));
        } else if reviewer.is_none() && task_outcome.is_some() {
            tx.execute("update task_lifecycle set status=?1,closed_at=?2,closed_by=?3,closure_mode=?4,revision=revision+1,updated_at=?2 where task_id=?5",params!["closed",timestamp,&agent,"agent_finish",&task_id]).map_err(db_error)?;
        } else {
            tx.execute("update task_lifecycle set status=?1,revision=revision+1,updated_at=?2 where task_id=?3",params!["in_review",timestamp,&task_id]).map_err(db_error)?;
        }
        tx.execute("insert into task_lifecycle_events(event_id,task_id,task_number,event_type,payload_json,created_at) values(?1,?2,?3,?4,?5,?6)",params![format!("task-event-{}",Uuid::new_v4()),&task_id,number,"task.report.submitted",report_json.to_string(),timestamp]).map_err(db_error)?;
        let result = if let Some(dependency) = review_dependency.as_ref() {
            json!({"status":"success","completion_mode":"report","task_number":number,"task_id":task_id,"report_id":report_id,"review_required":true,"new_status":"awaiting_dependencies","close_action":"submitted_for_review","review_action":"dependency_requested","blocked_by":"dependencies","review_dependency":dependency,"report":report_json,"outcome_contract":contract_json,"task_outcome":task_outcome,"outcome_admission":if task_outcome.is_some(){"created"}else{"not_recorded"},"evidence_state":{"admission_state":"not_recorded"}})
        } else if task_outcome.is_some() {
            json!({"status":"success","completion_mode":"report","task_number":number,"task_id":task_id,"report_id":report_id,"review_required":false,"new_status":"closed","close_action":"closed","report":report_json,"outcome_contract":contract_json,"task_outcome":task_outcome,"outcome_admission":"created","evidence_state":{"admission_state":"not_recorded"}})
        } else {
            json!({"status":"submitted","completion_mode":"report","task_number":number,"task_id":task_id,"report_id":report_id,"review_required":true,"new_status":"in_review","close_action":"submitted_for_review","review_action":"reviewer_required","blocked_by":"review","report":report_json,"outcome_contract":contract_json,"task_outcome":Value::Null,"outcome_admission":"not_recorded","evidence_state":{"admission_state":"not_recorded"},"remediation":"Provide an admitted distinct reviewer or an explicit outcome contract outcome before closing this task."})
        };
        tx.execute("insert into native_task_operations(operation_key,operation_kind,request_digest,result_json,created_at) values(?1,'task_finish',?2,?3,?4)",params![&operation_key,digest(&args),result.to_string(),timestamp]).map_err(db_error)?;
        tx.commit().map_err(db_error)?;
        if let Some((review_task_id, review_number)) = review_file {
            write_task_file(
                &self.options.site_root,
                &review_task_id,
                review_number,
                &format!("Review task #{number}"),
                &format!("Review the submitted work for task #{number}."),
                "Admit an accepted or rejected review outcome.",
                "Do not mutate the reviewed work.",
                &json!(["A structured review outcome is admitted."]),
                &json!([]),
                reviewer.as_deref(),
                &format!("review:{task_id}:{review_task_id}"),
            )?;
        }
        Ok(result)
    }
    fn task_submit_work(&mut self, args: Value) -> Result<Value, String> {
        let number = required_i64(&args, "task_number")?;
        let agent = required_string(&args, "agent_id")?;
        let resume = args.get("resume_existing_work").and_then(Value::as_bool) == Some(true);
        let execution_notes = string_arg(&args, "execution_notes");
        let verification = string_arg(&args, "verification");
        if resume && (execution_notes.is_some() || verification.is_some()) {
            return Err("task_lifecycle_submit_work_resume_existing_work_conflicts_with_replacement_notes".to_string());
        }
        let mut summary = string_arg(&args, "summary");
        if !resume {
            if summary.as_deref().is_none_or(|value| value.trim().is_empty()) {
                return Err("task_lifecycle_submit_work_summary_required".to_string());
            }
            for (field, value) in [("execution_notes", execution_notes.as_deref()), ("verification", verification.as_deref())] {
                if value.is_none_or(|text| text.trim().chars().count() < 20) {
                    return Err(format!("task_lifecycle_submit_work_{field}_not_substantive"));
                }
            }
        }
        if args.get("changed_files").is_some()
            && args.get("no_files_changed").and_then(Value::as_bool) == Some(true)
        {
            return Err("changed_files_conflicts_with_no_files_changed".to_string());
        }
        let (task_id, lifecycle_status) = self.connection()?.query_row(
            "select task_id,status from task_lifecycle where task_number=?1",
            params![number],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
        ).optional().map_err(db_error)?.ok_or_else(|| format!("task_not_found: {number}"))?;
        let rostered: bool = self.connection()?.query_row(
            "select count(*) from agent_roster where agent_id=?1 and status not in ('retired','revoked')",
            params![&agent],
            |row| row.get::<_, i64>(0),
        ).map_err(db_error)? > 0;
        if !rostered {
            return Ok(json!({
                "schema":"narada.task.submit_work.v1","status":"blocked",
                "blocked_at":"task_lifecycle_submit_work.roster_preflight",
                "task_number":number,"agent_id":agent,
                "error":"submit_work_agent_not_in_roster",
                "remediation":"Admit the agent into the task lifecycle roster before submit_work."
            }));
        }
        let mut primitive_results = Vec::new();
        let mut payload_source = Value::Null;
        if args.get("auto_materialize_payload").and_then(Value::as_bool) == Some(true) {
            let payload_id = format!("submit-work-{number}-{}", Uuid::new_v4());
            let payload = json!({
                "summary":summary,"execution_notes":execution_notes,"verification":verification,
                "changed_files":args.get("changed_files"),"no_files_changed":args.get("no_files_changed"),
                "recovery_truthfulness":args.get("recovery_truthfulness"),"self_certification":args.get("self_certification")
            });
            let created = self.payload_create(json!({"payload_id":payload_id,"payload":payload,"created_by":agent}))?;
            payload_source = json!({"kind":"auto_materialized_payload","ref":created.get("ref")});
        }
        if resume {
            let previous = self.query_one(
                "select report_id,summary,changed_files_json from task_reports where task_id=?1 and agent_id=?2 order by submitted_at desc,rowid desc limit 1",
                params![&task_id,&agent],
            )?.ok_or("task_lifecycle_submit_work_resume_existing_work_report_not_found")?;
            if summary.is_none() { summary = previous.get("summary").and_then(Value::as_str).map(ToString::to_string); }
            primitive_results.push(json!({"tool":"task_lifecycle_submit_work.reuse_existing_task_notes","result":{"status":"reused","report_id":previous.get("report_id")},"is_error":false}));
        }
        let should_claim = args.get("claim").and_then(Value::as_bool).unwrap_or(lifecycle_status == "opened");
        if should_claim {
            let mut claim_args = json!({"task_number":number,"agent_id":agent});
            if let Some(basis) = args.get("authority_basis") { claim_args["authority_basis"] = basis.clone(); }
            let result = self.task_claim(claim_args)?;
            primitive_results.push(json!({"tool":"task_lifecycle_claim","result":result,"is_error":false}));
        }
        if !resume {
            replace_task_markdown_section(&self.options.site_root, &task_id, number, "Execution Notes", execution_notes.as_deref().unwrap_or_default())?;
            replace_task_markdown_section(&self.options.site_root, &task_id, number, "Verification", verification.as_deref().unwrap_or_default())?;
            primitive_results.push(json!({"tool":"task_lifecycle_submit_work.write_task_notes","result":{"status":"written","task_number":number,"sections":["Execution Notes","Verification"]},"is_error":false}));
        }
        let should_prove = args.get("prove_criteria").and_then(Value::as_bool).unwrap_or(!resume);
        if should_prove {
            let result = self.task_prove_criteria(json!({"task_number":number,"agent_id":agent}))?;
            primitive_results.push(json!({"tool":"task_lifecycle_prove_criteria","result":result,"is_error":false}));
        }
        let should_admit = args.get("admit_evidence").and_then(Value::as_bool).unwrap_or(!resume);
        if should_admit {
            let mut admit_args = json!({"task_number":number,"agent_id":agent});
            if let Some(packet) = args.get("self_certification") { admit_args["self_certification"] = packet.clone(); }
            let result = self.task_admit_evidence(admit_args)?;
            primitive_results.push(json!({"tool":"task_lifecycle_admit_evidence","result":result,"is_error":false}));
        }
        let mut final_status = lifecycle_status;
        if args.get("finish").and_then(Value::as_bool) != Some(false) {
            let mut finish_args = json!({"task_number":number,"agent_id":agent,"summary":summary.ok_or("task_lifecycle_submit_work_summary_required")?});
            for field in ["reviewer","changed_files","no_files_changed","authority_basis","recovery_truthfulness","self_certification"] {
                if let Some(value) = args.get(field) { finish_args[field] = value.clone(); }
            }
            let result = self.task_finish(finish_args)?;
            final_status = result.get("new_status").and_then(Value::as_str).unwrap_or("in_review").to_string();
            primitive_results.push(json!({"tool":"task_lifecycle_finish","result":result,"is_error":false}));
        }
        Ok(json!({
            "schema":"narada.task.submit_work.v1","status":"submitted","task_number":number,
            "agent_id":agent,"final_lifecycle_status":final_status,
            "closure_status":if final_status == "closed" {"closed"} else {"submitted_for_review_not_closed"},
            "submitted_for_review_not_closed":final_status != "closed",
            "primitive_results":primitive_results,"blocked_at":Value::Null,"payload_source":payload_source
        }))
    }
}
