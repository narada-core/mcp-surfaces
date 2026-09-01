impl LifecycleServer {
    fn task_transition(&mut self, args: Value, status: &str) -> Result<Value, String> {
        let number = required_i64(&args, "task_number")?;
        let connection = self.connection_mut()?;
        let (task_id, current_status): (String, String) = connection
            .query_row(
                "select task_id,status from task_lifecycle where task_number=?1",
                params![number],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()
            .map_err(db_error)?
            .ok_or_else(|| format!("task_not_found: {number}"))?;
        let valid = match current_status.as_str() {
            "draft" => matches!(status, "opened"),
            "opened" => matches!(status, "claimed" | "closed" | "deferred"),
            "claimed" => matches!(status, "in_review" | "awaiting_dependencies" | "opened" | "needs_continuation" | "deferred" | "closed"),
            "needs_continuation" => matches!(status, "claimed" | "opened" | "deferred"),
            "in_review" => matches!(status, "closed" | "opened" | "needs_continuation" | "awaiting_dependencies" | "deferred"),
            "awaiting_dependencies" => matches!(status, "closed" | "opened" | "needs_continuation" | "deferred"),
            "deferred" => status == "opened",
            "closed" | "confirmed" => matches!(status, "confirmed" | "opened" | "in_review"),
            _ => false,
        };
        if !valid {
            return Ok(json!({"status":"invalid_transition","error":"invalid_transition","task_number":number,"task_id":task_id,"from_status":current_status,"to_status":status,"message":format!("Cannot transition from '{current_status}' to '{status}'.")}));
        }
        let timestamp = now();
        let changed=connection.execute("update task_lifecycle set status=?1,revision=revision+1,reopened_at=case when ?1='opened' and status in ('closed','confirmed') then ?2 else reopened_at end,reopened_by=case when ?1='opened' and status in ('closed','confirmed') then ?3 else reopened_by end,updated_at=?2 where task_number=?4",params![status,timestamp,string_arg(&args,"agent_id"),number]).map_err(db_error)?;
        if changed == 0 {
            return Err(format!("task_not_found: {number}"));
        }
        connection.execute("insert into task_lifecycle_events(event_id,task_id,task_number,event_type,payload_json,created_at) values(?1,?2,?3,?4,?5,?6)",params![format!("task-event-{}",Uuid::new_v4()),&task_id,number,format!("task.status.{status}"),json!({"new_status":status,"reason":args.get("reason")}).to_string(),timestamp]).map_err(db_error)?;
        project_task_status(&self.options.site_root, &task_id, number, status)?;
        Ok(json!({"status":"success","task_number":number,"new_status":status}))
    }
    fn task_prove_criteria(&mut self, args: Value) -> Result<Value, String> {
        let number = required_i64(&args, "task_number")?;
        let agent = required_string(&args, "agent_id")?;
        let (task_id, criteria): (String, String) = self.connection()?
            .query_row(
                "select task_id,acceptance_criteria_json from task_specs where task_number=?1",
                params![number],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()
            .map_err(db_error)?
            .ok_or_else(|| format!("task_not_found: {number}"))?;
        let path = self.options.site_root.join(".ai/do-not-open/tasks").join(format!("{task_id}.md"));
        let original = fs::read_to_string(&path).map_err(|e| format!("task_file_read_failed:{e}"))?;
        let mut changed = false;
        let mut updated = String::with_capacity(original.len());
        for line in original.lines() {
            if line.trim_start().starts_with("- [ ]") {
                updated.push_str(&line.replacen("[ ]", "[x]", 1));
                changed = true;
            } else { updated.push_str(line); }
            updated.push('\n');
        }
        if !changed {
            return Ok(json!({"status":"no_changes","task_number":number,"message":"No unchecked acceptance criteria found."}));
        }
        let timestamp = now();
        if updated.starts_with("---\n") || updated.starts_with("---\r\n") {
            if let Some(end) = updated[3..].find("\n---") {
                let insertion = 3 + end;
                updated.insert_str(insertion, &format!("\ncriteria_proved_by: {agent}\ncriteria_proved_at: {timestamp}"));
            }
        }
        fs::write(&path, &updated).map_err(|e| format!("task_file_write_failed:{e}"))?;
        let proof_id = format!("proof-{}", Uuid::new_v4());
        let criteria_value: Value = serde_json::from_str(&criteria).unwrap_or_else(|_| json!([]));
        let connection = self.connection_mut()?;
        connection.execute("insert into criteria_proofs(proof_id,task_id,task_number,proved_by,proved_at,criteria_json,verification_binding_json) values(?1,?2,?3,?4,?5,?6,?7)",params![&proof_id,&task_id,number,&agent,timestamp,criteria_value.to_string(),json!({"source":"native","tool":"task_lifecycle_prove_criteria"}).to_string()]).map_err(db_error)?;
        connection.execute("insert into task_lifecycle_events(event_id,task_id,task_number,event_type,payload_json,created_at) values(?1,?2,?3,'task.criteria.proved',?4,?5)",params![format!("task-event-{}",Uuid::new_v4()),&task_id,number,json!({"proof_id":proof_id,"criteria":criteria_value}).to_string(),timestamp]).map_err(db_error)?;
        let admission = match self.task_admit_evidence(json!({"task_number":number,"agent_id":agent,"methods":["criteria_proof"],"acceptance_criteria":criteria_value})) {
            Ok(value) => value,
            Err(error) => { let _ = fs::write(&path, original); return Err(error); }
        };
        Ok(json!({"schema":"narada.task.mcp.prove_criteria.v0","status":"proved","proof_id":proof_id,"task_number":number,"criteria":criteria_value,"proved_by":agent,"admission":admission,"criteria_projection_rolled_back":false}))
    }
    fn task_admit_evidence(&mut self, args: Value) -> Result<Value, String> {
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
        let report_ids=self.query_objects("select report_id from task_reports where task_id=?1 order by submitted_at desc limit 50",params![&task_id])?.into_iter().filter_map(|v|v.get("report_id").cloned()).collect::<Vec<_>>();
        let proof_ids=self.query_objects("select proof_id from criteria_proofs where task_id=?1 order by proved_at desc limit 50",params![&task_id])?.into_iter().filter_map(|v|v.get("proof_id").cloned()).collect::<Vec<_>>();
        let bundle_id = format!("bundle-{}", Uuid::new_v4());
        let admission_id = format!("admission-{}", Uuid::new_v4());
        let timestamp = now();
        let methods = args
            .get("methods")
            .cloned()
            .unwrap_or_else(|| json!(["admission"]));
        if !methods.is_array() { return Err("evidence_methods_must_be_array".to_string()); }
        if methods.as_array().is_some_and(|items| items.is_empty()) { return Err("evidence_methods_required".to_string()); }
        if methods.as_array().is_some_and(|items| items.iter().any(|item| item.as_str() == Some("criteria_proof"))) && proof_ids.is_empty() {
            return Ok(json!({"schema":"narada.task.mcp.admit_evidence.v0","status":"blocked","verdict":"blocked","task_number":number,"blockers":["criteria_proof_required"],"methods":methods}));
        }
        let connection = self.connection_mut()?;
        let tx = connection.transaction().map_err(db_error)?;
        tx.execute("insert into evidence_bundles(bundle_id,task_id,task_number,report_ids_json,verification_run_ids_json,acceptance_criteria_json,review_ids_json,changed_files_json,residuals_json,assembled_at,assembled_by) values(?1,?2,?3,?4,'[]',?5,'[]',?6,?7,?8,?9)",params![&bundle_id,&task_id,number,Value::Array(report_ids.clone()).to_string(),args.get("acceptance_criteria").cloned().unwrap_or_else(||json!([])).to_string(),args.get("changed_files").cloned().unwrap_or_else(||json!([])).to_string(),json!([]).to_string(),timestamp,&agent]).map_err(db_error)?;
        tx.execute("insert into evidence_admission_results(admission_id,bundle_id,task_id,task_number,verdict,methods_json,blockers_json,lifecycle_eligible_status,admitted_at,admitted_by,confirmation_json) values(?1,?2,?3,?4,'admitted',?5,'[]',?6,?7,?8,?9)",params![&admission_id,&bundle_id,&task_id,number,methods.to_string(),status,timestamp,&agent,json!({"proof_ids":proof_ids,"report_ids":report_ids}).to_string()]).map_err(db_error)?;
        tx.execute("insert into task_lifecycle_events(event_id,task_id,task_number,event_type,payload_json,created_at) values(?1,?2,?3,'task.evidence.admitted',?4,?5)",params![format!("task-event-{}",Uuid::new_v4()),&task_id,number,json!({"bundle_id":bundle_id,"admission_id":admission_id,"methods":methods}).to_string(),timestamp]).map_err(db_error)?;
        tx.commit().map_err(db_error)?;
        Ok(
            json!({"schema":"narada.task.mcp.admit_evidence.v0","status":"admitted","verdict":"admitted","bundle_id":bundle_id,"admission_id":admission_id,"task_number":number,"methods":methods,"report_ids":report_ids,"proof_ids":proof_ids}),
        )
    }
}
